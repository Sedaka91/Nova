"""Speech recognition helper: Silero VAD and faster-whisper on the microphone, driven by JSON lines."""

import argparse
import collections
import logging
import os
import sys
import threading
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Self

import numpy as np

from nova_helpers.audio import SAMPLE_RATE, AudioDeviceError, Microphone
from nova_helpers.protocol import Emitter, claim_stdout, read_commands, setup_logging

if TYPE_CHECKING:
    from faster_whisper import WhisperModel

log = logging.getLogger(__name__)

# All counted in 80 ms blocks.
_PREROLL_BLOCKS = 6
_SILENCE_BLOCKS = 10
_MAX_UTTERANCE_BLOCKS = 375
_PARTIAL_INTERVAL_BLOCKS = 12

_LANGUAGE = "de"
_PARTIAL_BEAM_SIZE = 1
_FINAL_BEAM_SIZE = 5
_MAX_NO_SPEECH_PROB = 0.6
_CPU_THREADS = 4  # measured: 4 beats 8 to 20, more threads spill onto the E-cores and wait for them

_VAD_THRESHOLD = 0.5
_VAD_FRAME_SAMPLES = 512
_VAD_CONTEXT_SAMPLES = 64
_VAD_STATE_SHAPE = (1, 1, 128)

_INT16_SCALE = 32768.0


def _failure_message(error: Exception) -> str:
    return f"Spracherkennung fehlgeschlagen: {error}"


@dataclass(frozen=True)
class _Job:
    generation: int
    audio: np.ndarray
    final: bool


class Segmenter:
    """Cuts the block stream into utterances; `handle` and `process_block` run on different threads.

    Decides from blocks and VAD values only. Transcription runs on a worker thread, so the
    audio thread never waits for it: a `speech_start` goes out while an older utterance is
    still being transcribed. Each utterance has its own generation, which the worker checks
    before it reports, so nothing arrives after `cancel` and a partial never outlives its
    utterance. Only the newest partial waits; finals all run, in order.
    """

    def __init__(
        self,
        emit: Callable[..., None],
        is_speech: Callable[[np.ndarray], bool],
        transcribe: Callable[[np.ndarray], str],
        *,
        transcribe_partial: Callable[[np.ndarray], str] | None = None,
        reset_vad: Callable[[], None] | None = None,
        on_fatal: Callable[[], None] | None = None,
    ) -> None:
        self._emit = emit
        self._is_speech = is_speech
        self._transcribe = transcribe
        self._transcribe_partial = transcribe_partial or transcribe
        self._reset_vad = reset_vad
        self._on_fatal = on_fatal
        self._changed = threading.Condition()
        self._active = False
        self._preroll: collections.deque[np.ndarray] = collections.deque(maxlen=_PREROLL_BLOCKS)
        self._utterance: list[np.ndarray] | None = None
        self._blocks_since_start = 0
        self._blocks_since_partial = 0
        self._silent_blocks = 0
        self._generation = 0
        self._cancelled_generation = 0
        self._finals: collections.deque[_Job] = collections.deque()
        self._partial: _Job | None = None
        threading.Thread(target=self._run, name="stt-worker", daemon=True).start()

    def handle(self, command: dict[str, Any]) -> None:
        match command.get("cmd"):
            case "listen":
                with self._changed:
                    if self._active:
                        return
                    if self._reset_vad is not None:
                        self._reset_vad()  # it last heard audio from before the pause
                    self._active = True
            case "cancel":
                with self._changed:
                    self._active = False
                    self._utterance = None
                    self._cancelled_generation = self._generation
                    self._finals.clear()
                    self._partial = None
            case other:
                log.warning("unknown command: %r", other)

    def process_block(self, block: np.ndarray) -> None:
        with self._changed:
            if not self._active:
                self._preroll.append(block)
                return
            speech = self._is_speech(block)
            if self._utterance is None:
                if speech:
                    self._start_utterance(block)
                else:
                    self._preroll.append(block)
                return
            self._continue_utterance(block, speech)

    def _start_utterance(self, block: np.ndarray) -> None:
        self._utterance = [*self._preroll, block]
        self._preroll.clear()
        self._blocks_since_start = 1
        self._blocks_since_partial = 0
        self._silent_blocks = 0
        self._generation += 1
        self._emit("speech_start")

    def _continue_utterance(self, block: np.ndarray, speech: bool) -> None:
        self._utterance.append(block)
        self._blocks_since_start += 1
        self._blocks_since_partial += 1
        self._silent_blocks = 0 if speech else self._silent_blocks + 1
        if not speech:
            self._partial = None  # nothing new to read, and it would hold up the final
        if self._silent_blocks >= _SILENCE_BLOCKS or self._blocks_since_start >= _MAX_UTTERANCE_BLOCKS:
            self._finals.append(_Job(self._generation, np.concatenate(self._utterance), final=True))
            self._partial = None
            self._utterance = None
            self._changed.notify()
        elif speech and self._blocks_since_partial >= _PARTIAL_INTERVAL_BLOCKS:
            self._partial = _Job(self._generation, np.concatenate(self._utterance), final=False)
            self._blocks_since_partial = 0
            self._changed.notify()

    def _is_current(self, job: _Job) -> bool:
        if job.generation <= self._cancelled_generation:
            return False
        return job.final or (job.generation == self._generation and self._utterance is not None)

    def _next_job(self) -> _Job:
        with self._changed:
            while not self._finals and self._partial is None:
                self._changed.wait()
            if self._finals:
                return self._finals.popleft()
            job, self._partial = self._partial, None
            return job

    def _run(self) -> None:
        while True:
            job = self._next_job()
            try:
                text = (self._transcribe if job.final else self._transcribe_partial)(job.audio)
            except Exception as error:
                log.exception("transcription failed")
                self._emit("error", message=_failure_message(error))
                if self._on_fatal is not None:
                    self._on_fatal()
                return
            with self._changed:
                if self._is_current(job):
                    self._emit("final" if job.final else "partial", text=text)


class SileroVad:
    """Streams blocks through the Silero model, deciding per block whether it contains speech.

    `get_vad_model()(audio)` starts from zeroed LSTM state on every call, which is useless for
    single blocks, so this drives its session the same way, frame by frame, and keeps the state.
    Blocks and frames do not align; samples left over from a block wait for the next one.
    """

    def __init__(self, session: Any) -> None:
        self._session = session
        self.reset()

    @classmethod
    def load(cls) -> Self:
        from faster_whisper.vad import get_vad_model  # imported late: it is slow

        return cls(get_vad_model().session)

    def reset(self) -> None:
        self._h = np.zeros(_VAD_STATE_SHAPE, dtype=np.float32)
        self._c = np.zeros(_VAD_STATE_SHAPE, dtype=np.float32)
        self._context = np.zeros(_VAD_CONTEXT_SAMPLES, dtype=np.float32)
        self._pending = np.empty(0, dtype=np.float32)

    def __call__(self, block: np.ndarray) -> bool:
        self._pending = np.concatenate((self._pending, block.astype(np.float32) / _INT16_SCALE))
        speech = False
        while len(self._pending) >= _VAD_FRAME_SAMPLES:
            frame = self._pending[:_VAD_FRAME_SAMPLES]
            self._pending = self._pending[_VAD_FRAME_SAMPLES:]
            batch = np.concatenate((self._context, frame))[np.newaxis]
            output, self._h, self._c = self._session.run(None, {"input": batch, "h": self._h, "c": self._c})
            self._context = frame[-_VAD_CONTEXT_SAMPLES:]
            speech = speech or bool(output[0] >= _VAD_THRESHOLD)
        return speech


def text_of(segments: Iterable[Any]) -> str:
    """Joins the text of `segments`, dropping those Whisper invented from silence or noise."""
    return "".join(s.text for s in segments if s.no_speech_prob <= _MAX_NO_SPEECH_PROB).strip()


def _probe(model: "WhisperModel") -> None:
    """Runs one inference: a broken CUDA setup loads fine and fails only here, in the lazy segments."""
    segments, _ = model.transcribe(np.zeros(SAMPLE_RATE, dtype=np.float32), language=_LANGUAGE, beam_size=1)
    list(segments)


def _load_cuda(name: str) -> "WhisperModel":
    from faster_whisper import WhisperModel  # imported late: it is slow and may print

    model = WhisperModel(name, device="cuda", compute_type="int8_float16", local_files_only=True)
    _probe(model)
    log.info("whisper %s on cuda int8_float16", name)
    return model


def _load_cpu(name: str) -> "WhisperModel":
    from faster_whisper import WhisperModel  # imported late: it is slow and may print

    model = WhisperModel(name, device="cpu", compute_type="int8", cpu_threads=_CPU_THREADS, local_files_only=True)
    log.info("whisper %s on cpu int8 with %d threads", name, _CPU_THREADS)
    return model


def load_model(name: str, device: str) -> "WhisperModel":
    """Loads a Whisper model from the local cache.

    Args:
        name: Model name, e.g. `small`.
        device: `cpu`, `cuda`, or `auto` for CUDA with a fallback to CPU `int8`.

    Raises:
        RuntimeError: If `device` is `cuda` and CUDA cannot run inference.
    """
    if device == "cuda":
        return _load_cuda(name)
    if device == "auto":
        try:
            return _load_cuda(name)
        except (RuntimeError, ValueError, OSError) as error:
            log.warning("cuda unusable, falling back to cpu: %s", error)
    return _load_cpu(name)


def make_transcriber(model: "WhisperModel", beam_size: int) -> Callable[[np.ndarray], str]:
    """Returns a function that turns int16 samples into text."""

    def transcribe(audio: np.ndarray) -> str:
        segments, _ = model.transcribe(
            audio.astype(np.float32) / _INT16_SCALE,
            language=_LANGUAGE,
            beam_size=beam_size,
            condition_on_previous_text=False,
        )
        return text_of(segments)

    return transcribe


def _listen(microphone: Microphone, segmenter: Segmenter, emit: Callable[..., None]) -> None:
    try:
        for block in microphone:
            segmenter.process_block(block)
    except AudioDeviceError as error:
        emit("error", message=str(error))
        os._exit(1)
    except Exception as error:
        log.exception("audio thread failed")
        emit("error", message=_failure_message(error))
        os._exit(1)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="nova_helpers.stt")
    parser.add_argument("--model", required=True, help="Whisper model name, e.g. small")
    parser.add_argument("--device", required=True, choices=["cpu", "cuda", "auto"])
    parser.add_argument("--input", default="nova_mic", help="PipeWire source node")
    args = parser.parse_args(argv)

    setup_logging("stt")
    logging.getLogger("faster_whisper").setLevel(logging.WARNING)  # logs every transcription, once a second
    emit = Emitter(claim_stdout())
    try:
        model = load_model(args.model, args.device)
        vad = SileroVad.load()
        segmenter = Segmenter(
            emit,
            vad,
            make_transcriber(model, _FINAL_BEAM_SIZE),
            transcribe_partial=make_transcriber(model, _PARTIAL_BEAM_SIZE),
            reset_vad=vad.reset,
            on_fatal=lambda: os._exit(1),
        )
        with Microphone(args.input) as microphone:
            emit("ready")
            audio = threading.Thread(target=_listen, args=(microphone, segmenter, emit), name="stt-audio", daemon=True)
            audio.start()
            for command in read_commands(sys.stdin):
                segmenter.handle(command)
    except AudioDeviceError as error:
        emit("error", message=str(error))
        return 1
    except Exception as error:
        log.exception("stt failed")
        emit("error", message=_failure_message(error))
        return 1
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
