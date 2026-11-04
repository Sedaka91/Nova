"""Wake word helper: openWakeWord on the microphone, driven by JSON lines."""

import argparse
import logging
import os
import sys
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import numpy as np

from nova_helpers.audio import AudioDeviceError, Microphone, data_dir, missing_model_message
from nova_helpers.protocol import Emitter, claim_stdout, read_commands, setup_logging

log = logging.getLogger(__name__)

LOCKOUT_S = 1.0


class WakeDetector:
    """Turns model scores into `wake` events; `handle` and `process_block` run on different threads."""

    def __init__(
        self,
        emit: Callable[..., None],
        predict: Callable[[np.ndarray], float],
        reset: Callable[[], None],
        threshold: float,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self._emit = emit
        self._predict = predict
        self._reset = reset
        self._threshold = threshold
        self._clock = clock
        self._lock = threading.Lock()
        self._paused = False
        self._last_wake: float | None = None

    def handle(self, command: dict[str, Any]) -> None:
        match command.get("cmd"):
            case "pause":
                with self._lock:
                    self._paused = True
            case "resume":
                with self._lock:
                    self._reset()
                    self._paused = False
            case other:
                log.warning("unknown command: %r", other)

    def process_block(self, block: np.ndarray) -> None:
        with self._lock:
            if self._paused:
                return
            score = self._predict(block)
            if score < self._threshold:
                return
            now = self._clock()
            if self._last_wake is not None and now - self._last_wake < LOCKOUT_S:
                return
            self._last_wake = now
            self._emit("wake", score=score)


def _resolve_model(model: str) -> Path:
    if "/" in model:
        return Path(model)
    matches = sorted((data_dir() / "openwakeword").glob(f"{model}*.onnx"))
    if not matches:
        raise FileNotFoundError(missing_model_message(data_dir() / "openwakeword" / f"{model}*.onnx"))
    return matches[0]


def _load_model(model: str) -> tuple[Callable[[np.ndarray], float], Callable[[], None]]:
    """Loads the wake word model; returns its predict and reset functions.

    Raises:
        FileNotFoundError: If a model file has not been downloaded.
    """
    directory = data_dir() / "openwakeword"
    files = [_resolve_model(model), directory / "melspectrogram.onnx", directory / "embedding_model.onnx"]
    for file in files:
        if not file.is_file():
            raise FileNotFoundError(missing_model_message(file))
    from openwakeword.model import Model  # imported late: it is slow and may print

    wake_model = Model(
        wakeword_models=[str(files[0])],
        inference_framework="onnx",
        melspec_model_path=str(files[1]),
        embedding_model_path=str(files[2]),
    )

    def predict(block: np.ndarray) -> float:
        return float(max(wake_model.predict(block).values()))

    return predict, wake_model.reset


def _listen(microphone: Microphone, detector: WakeDetector, emit: Callable[..., None]) -> None:
    try:
        for block in microphone:
            detector.process_block(block)
    except AudioDeviceError as error:
        emit("error", message=str(error))
        os._exit(1)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="nova_helpers.wakeword")
    parser.add_argument("--model", required=True, help="model name (hey_jarvis) or path containing a /")
    parser.add_argument("--threshold", type=float, required=True)
    parser.add_argument("--input", default="nova_mic", help="PipeWire source node")
    args = parser.parse_args(argv)

    setup_logging("wakeword")
    emit = Emitter(claim_stdout())
    try:
        predict, reset = _load_model(args.model)
        detector = WakeDetector(emit, predict, reset, args.threshold)
        with Microphone(args.input) as microphone:
            emit("ready")
            audio = threading.Thread(
                target=_listen, args=(microphone, detector, emit), name="wakeword-audio", daemon=True
            )
            audio.start()
            for command in read_commands(sys.stdin):
                detector.handle(command)
    except (FileNotFoundError, AudioDeviceError) as error:
        emit("error", message=str(error))
        return 1
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
