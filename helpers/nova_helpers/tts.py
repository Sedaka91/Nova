"""Text-to-speech helper: Piper synthesis played on a PipeWire sink, driven by JSON lines."""

import argparse
import collections
import logging
import os
import sys
import threading
import time
from collections.abc import Callable, Iterable
from typing import Any

import numpy as np

from nova_helpers.audio import AudioDeviceError, Speaker, data_dir, missing_model_message
from nova_helpers.protocol import Emitter, claim_stdout, read_commands, setup_logging

log = logging.getLogger(__name__)

_STOP_SETTLE_TIMEOUT_S = 1.0
_CLOSE_JOIN_TIMEOUT_S = 1.0


class TtsWorker:
    """Speaks queued utterances on a worker thread; `handle` runs on the stdin thread.

    Every `stop` starts a new generation. The worker checks its generation before each
    event and each chunk, so nothing of a stopped utterance is written or reported later.
    """

    def __init__(
        self,
        emit: Callable[..., None],
        synthesize: Callable[[str], Iterable[np.ndarray]],
        speaker: Any,
        on_fatal: Callable[[], None] | None = None,
    ) -> None:
        self._emit = emit
        self._synthesize = synthesize
        self._speaker = speaker
        self._on_fatal = on_fatal
        self._changed = threading.Condition()
        self._queue: collections.deque[tuple[str, str]] = collections.deque()
        self._generation = 0
        self._writing = False
        self._closed = False
        self._thread = threading.Thread(target=self._run, name="tts-worker", daemon=True)
        self._thread.start()

    def handle(self, command: dict[str, Any]) -> None:
        match command.get("cmd"):
            case "speak":
                with self._changed:
                    self._queue.append((str(command.get("id", "")), str(command.get("text", ""))))
                    self._changed.notify()
            case "stop":
                self._stop()
            case other:
                log.warning("unknown command: %r", other)

    def close(self) -> None:
        """Stops speaking and ends the worker thread, waiting briefly for it."""
        with self._changed:
            self._closed = True
            self._changed.notify()
        self._stop()
        self._thread.join(timeout=_CLOSE_JOIN_TIMEOUT_S)

    def _stop(self) -> None:
        with self._changed:
            self._generation += 1
            self._queue.clear()
        # `abort` only clears what is queued. A write that began before the generation changed
        # may queue its chunk after the first abort, so abort until no write is in flight.
        self._speaker.abort()
        deadline = time.monotonic() + _STOP_SETTLE_TIMEOUT_S
        while self._is_writing() and time.monotonic() < deadline:
            time.sleep(0.005)
            self._speaker.abort()

    def _is_writing(self) -> bool:
        with self._changed:
            return self._writing

    def _run(self) -> None:
        while True:
            with self._changed:
                while not self._queue and not self._closed:
                    self._changed.wait()
                if self._closed:
                    return
                utterance, text = self._queue.popleft()
                generation = self._generation
            try:
                self._speak(utterance, text, generation)
            except AudioDeviceError as error:
                self._emit("error", message=str(error))
                if self._on_fatal is not None:
                    self._on_fatal()
                return

    def _speak(self, utterance: str, text: str, generation: int) -> None:
        if not self._emit_if_current(generation, "started", id=utterance):
            return
        try:
            for chunk in self._synthesize(text):
                with self._changed:
                    if generation != self._generation:
                        return
                    self._writing = True
                try:
                    self._speaker.write(chunk)
                finally:
                    with self._changed:
                        self._writing = False
        except AudioDeviceError:
            raise
        except Exception as error:  # a broken utterance must not leave Nova waiting for `done`
            log.exception("synthesis failed")
            self._emit_if_current(generation, "error", message=f"Sprachausgabe fehlgeschlagen: {error}")
        self._emit_if_current(generation, "done", id=utterance)

    def _emit_if_current(self, generation: int, event: str, **fields: Any) -> bool:
        with self._changed:
            if generation != self._generation:
                return False
            self._emit(event, **fields)
            return True


def _load_voice(voice: str) -> tuple[Callable[[str], Iterable[np.ndarray]], int]:
    """Loads a Piper voice; returns its synthesize function and sample rate.

    Raises:
        FileNotFoundError: If the voice has not been downloaded.
    """
    path = data_dir() / "piper" / f"{voice}.onnx"
    if not path.is_file():
        raise FileNotFoundError(missing_model_message(path))
    from piper import PiperVoice  # imported late: it is slow and may print

    piper_voice = PiperVoice.load(path)

    def synthesize(text: str) -> Iterable[np.ndarray]:
        return (chunk.audio_int16_array for chunk in piper_voice.synthesize(text))

    return synthesize, piper_voice.config.sample_rate


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="nova_helpers.tts")
    parser.add_argument("--voice", required=True, help="Piper voice name, e.g. de_DE-thorsten-medium")
    parser.add_argument("--output", default="nova_sink", help="PipeWire sink node")
    args = parser.parse_args(argv)

    setup_logging("tts")
    emit = Emitter(claim_stdout())
    try:
        synthesize, rate = _load_voice(args.voice)
        speaker = Speaker(args.output, rate)
    except (FileNotFoundError, AudioDeviceError) as error:
        emit("error", message=str(error))
        return 1

    worker = TtsWorker(emit, synthesize, speaker, on_fatal=lambda: os._exit(1))
    emit("ready")
    try:
        for command in read_commands(sys.stdin):
            worker.handle(command)
    except KeyboardInterrupt:
        pass
    worker.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
