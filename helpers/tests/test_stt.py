"""Tests for the stt helper: segmenter logic with fakes, process behaviour in a subprocess."""

import logging
import os
import subprocess
import sys
import textwrap
import threading
import time
from collections.abc import Iterator
from pathlib import Path
from types import SimpleNamespace
from typing import Any, ClassVar

import numpy as np
import pytest

from nova_helpers import stt
from nova_helpers.audio import BLOCK_SAMPLES
from nova_helpers.stt import Segmenter, SileroVad, load_model, text_of

HELPERS_DIR = Path(__file__).resolve().parent.parent
TIMEOUT_S = 5.0
SETTLE_S = 0.05
PREROLL = 6
SILENCE = 10
LIMIT = 375
PARTIAL_EVERY = 12


class Rig:
    """A segmenter with a scripted VAD, gated fake transcription and recorded events."""

    def __init__(self) -> None:
        self.changed = threading.Condition()
        self.events: list[dict[str, Any]] = []
        self.finals: list[np.ndarray] = []
        self.partials: list[np.ndarray] = []
        self.gate = threading.Event()
        self.gate.set()
        self.vad_resets = 0
        self.fatal = threading.Event()
        self.failure: Exception | None = None
        self.next_value = 1
        self.segmenter = Segmenter(
            self._emit,
            self._is_speech,
            self._transcribe_final,
            transcribe_partial=self._transcribe_partial,
            reset_vad=self._reset_vad,
            on_fatal=self.fatal.set,
        )

    def _emit(self, event: str, **fields: Any) -> None:
        with self.changed:
            self.events.append({"event": event, **fields})
            self.changed.notify_all()

    def _is_speech(self, block: np.ndarray) -> bool:
        return bool(block[0] > 0)

    def _reset_vad(self) -> None:
        self.vad_resets += 1

    def _transcribe_final(self, audio: np.ndarray) -> str:
        return self._transcribe(self.finals, audio, "satz")

    def _transcribe_partial(self, audio: np.ndarray) -> str:
        return self._transcribe(self.partials, audio, "teil")

    def _transcribe(self, calls: list[np.ndarray], audio: np.ndarray, word: str) -> str:
        with self.changed:
            calls.append(audio)
            self.changed.notify_all()
        assert self.gate.wait(TIMEOUT_S)
        if self.failure is not None:
            raise self.failure
        return f"{word} {len(audio) // BLOCK_SAMPLES}"

    def listen(self) -> None:
        self.segmenter.handle({"cmd": "listen"})

    def cancel(self) -> None:
        self.segmenter.handle({"cmd": "cancel"})

    def speech(self, blocks: int = 1) -> None:
        """Feeds speech blocks; each carries its own positive sample value."""
        for _ in range(blocks):
            self.segmenter.process_block(np.full(BLOCK_SAMPLES, self.next_value, dtype=np.int16))
            self.next_value += 1

    def silence(self, blocks: int = 1) -> None:
        for _ in range(blocks):
            self.segmenter.process_block(np.zeros(BLOCK_SAMPLES, dtype=np.int16))

    def names(self) -> list[str]:
        with self.changed:
            return [event["event"] for event in self.events]

    def count(self, name: str) -> int:
        return self.names().count(name)

    def wait_for(self, name: str, count: int = 1) -> None:
        with self.changed:
            assert self.changed.wait_for(lambda: self._names_unlocked().count(name) >= count, TIMEOUT_S), self.events

    def _names_unlocked(self) -> list[str]:
        return [event["event"] for event in self.events]

    def wait_for_transcription_start(self, calls: list[np.ndarray], count: int = 1) -> None:
        with self.changed:
            assert self.changed.wait_for(lambda: len(calls) >= count, TIMEOUT_S)


def settle() -> None:
    """Gives the worker time to do what it must not do; only used before negative asserts."""
    time.sleep(SETTLE_S)


def blocks_of(audio: np.ndarray) -> list[int]:
    return audio[::BLOCK_SAMPLES].tolist()


@pytest.fixture
def rig() -> Rig:
    return Rig()


def test_inactive_until_listen(rig: Rig) -> None:
    rig.speech(20)
    rig.silence(20)
    settle()

    assert rig.events == []
    assert rig.finals == []


def test_speech_start_once_per_utterance(rig: Rig) -> None:
    rig.listen()

    rig.speech(5)
    rig.silence(3)
    rig.speech(5)

    assert rig.names() == ["speech_start"]


def test_preroll_is_part_of_transcribed_audio(rig: Rig) -> None:
    rig.listen()
    for value in range(-20, 0):
        rig.segmenter.process_block(np.full(BLOCK_SAMPLES, value, dtype=np.int16))

    rig.speech(1)
    rig.silence(SILENCE)
    rig.wait_for("final")

    assert blocks_of(rig.finals[0]) == [*range(-PREROLL, 0), 1, *[0] * SILENCE]


def test_blocks_seen_while_inactive_do_not_reach_the_first_utterance(rig: Rig) -> None:
    for value in range(-20, 0):
        rig.segmenter.process_block(np.full(BLOCK_SAMPLES, value, dtype=np.int16))

    rig.listen()
    rig.speech(1)
    rig.silence(SILENCE)
    rig.wait_for("final")

    assert blocks_of(rig.finals[0]) == [1, *[0] * SILENCE]


def test_repeated_listen_keeps_audio_of_a_running_utterance(rig: Rig) -> None:
    rig.listen()
    rig.speech(3)

    rig.segmenter.handle({"cmd": "listen"})
    rig.silence(SILENCE)
    rig.wait_for("final")

    assert blocks_of(rig.finals[0]) == [1, 2, 3, *[0] * SILENCE]


def test_final_after_800ms_silence(rig: Rig) -> None:
    rig.listen()
    rig.speech(5)

    rig.silence(SILENCE)
    rig.wait_for("final")

    assert rig.events[-1] == {"event": "final", "text": f"satz {5 + SILENCE}"}


def test_silence_shorter_than_limit_keeps_utterance(rig: Rig) -> None:
    rig.listen()
    rig.speech(5)

    rig.silence(SILENCE - 1)
    rig.speech(1)
    rig.silence(SILENCE - 1)
    settle()

    assert rig.count("final") == 0


def test_partial_about_every_second(rig: Rig) -> None:
    rig.listen()

    rig.speech(PARTIAL_EVERY)
    settle()
    assert rig.count("partial") == 0
    rig.speech(1)
    rig.wait_for("partial")
    rig.speech(PARTIAL_EVERY)
    rig.wait_for("partial", 2)

    assert [event["text"] for event in rig.events if event["event"] == "partial"] == ["teil 13", "teil 25"]


def test_no_partial_in_trailing_silence(rig: Rig) -> None:
    rig.listen()
    rig.speech(PARTIAL_EVERY - 3)

    rig.silence(SILENCE - 1)
    settle()

    assert rig.partials == []


def test_waiting_partial_is_dropped_when_silence_begins(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(PARTIAL_EVERY + 1)
    rig.wait_for_transcription_start(rig.partials)
    rig.speech(PARTIAL_EVERY)

    rig.silence(1)
    rig.gate.set()
    settle()

    assert len(rig.partials) == 1


def test_forced_final_after_30s(rig: Rig) -> None:
    rig.listen()

    rig.speech(LIMIT - 1)
    settle()
    assert rig.count("final") == 0
    rig.speech(1)
    rig.wait_for("final")

    assert rig.events[-1] == {"event": "final", "text": f"satz {LIMIT}"}


def test_speech_after_forced_final_is_a_new_utterance_without_old_audio(rig: Rig) -> None:
    rig.listen()
    rig.speech(LIMIT)
    rig.wait_for("final")

    rig.speech(1)
    rig.silence(SILENCE)
    rig.wait_for("final", 2)

    assert rig.count("speech_start") == 2
    assert blocks_of(rig.finals[1]) == [LIMIT + 1, *[0] * SILENCE]


def test_keeps_listening_after_final(rig: Rig) -> None:
    rig.listen()
    rig.speech(2)
    rig.silence(SILENCE)
    rig.wait_for("final")

    rig.speech(3)
    rig.silence(SILENCE)
    rig.wait_for("final", 2)

    assert rig.names() == ["speech_start", "final", "speech_start", "final"]
    assert rig.events[-1] == {"event": "final", "text": f"satz {3 + SILENCE}"}


def test_cancel_drops_buffer_and_stops(rig: Rig) -> None:
    rig.listen()
    rig.speech(5)

    rig.cancel()
    rig.silence(SILENCE)
    rig.speech(5)
    settle()

    assert rig.names() == ["speech_start"]
    assert rig.finals == []


def test_listen_after_cancel_starts_without_the_cancelled_audio(rig: Rig) -> None:
    rig.listen()
    rig.speech(5)
    rig.cancel()

    rig.listen()
    rig.speech(1)
    rig.silence(SILENCE)
    rig.wait_for("final")

    assert blocks_of(rig.finals[0]) == [6, *[0] * SILENCE]


def test_listen_twice_is_harmless(rig: Rig) -> None:
    rig.listen()
    rig.speech(5)

    rig.listen()
    rig.silence(SILENCE)
    rig.wait_for("final")

    assert rig.names() == ["speech_start", "final"]
    assert rig.events[-1]["text"] == f"satz {5 + SILENCE}"


def test_listen_resets_the_vad_only_when_activating(rig: Rig) -> None:
    rig.listen()
    rig.listen()
    assert rig.vad_resets == 1

    rig.cancel()
    rig.listen()
    assert rig.vad_resets == 2


def test_cancel_during_final_transcription_drops_its_result(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(2)
    rig.silence(SILENCE)
    rig.wait_for_transcription_start(rig.finals)

    rig.cancel()
    rig.gate.set()
    settle()

    assert rig.names() == ["speech_start"]


def test_cancel_during_partial_transcription_drops_its_result(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(PARTIAL_EVERY + 1)
    rig.wait_for_transcription_start(rig.partials)

    rig.cancel()
    rig.gate.set()
    settle()

    assert rig.names() == ["speech_start"]


def test_cancel_drops_queued_final(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(2)
    rig.silence(SILENCE)
    rig.wait_for_transcription_start(rig.finals)
    rig.speech(2)
    rig.silence(SILENCE)

    rig.cancel()
    rig.gate.set()
    settle()

    assert len(rig.finals) == 1
    assert rig.count("final") == 0


def test_final_of_previous_utterance_survives_next_speech_start_in_order(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(2)
    rig.silence(SILENCE)
    rig.wait_for_transcription_start(rig.finals)
    rig.speech(3)
    rig.silence(SILENCE)
    rig.gate.set()
    rig.wait_for("final", 2)

    finals = [event["text"] for event in rig.events if event["event"] == "final"]
    assert finals == [f"satz {2 + SILENCE}", f"satz {3 + SILENCE}"]


def test_partial_of_finished_utterance_is_dropped(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(PARTIAL_EVERY + 1)
    rig.wait_for_transcription_start(rig.partials)
    rig.silence(SILENCE)

    rig.gate.set()
    rig.wait_for("final")
    settle()

    assert rig.names() == ["speech_start", "final"]


def test_speech_start_does_not_wait_for_a_running_transcription(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(2)
    rig.silence(SILENCE)
    rig.wait_for_transcription_start(rig.finals)

    rig.speech(1)

    assert rig.names() == ["speech_start", "speech_start"]
    rig.gate.set()


def test_stale_partials_are_skipped_but_finals_are_not(rig: Rig) -> None:
    rig.gate.clear()
    rig.listen()
    rig.speech(PARTIAL_EVERY + 1)
    rig.wait_for_transcription_start(rig.partials)
    rig.speech(PARTIAL_EVERY * 3)

    rig.gate.set()
    rig.wait_for("partial", 2)
    settle()

    assert [len(audio) // BLOCK_SAMPLES for audio in rig.partials] == [13, 13 + PARTIAL_EVERY * 3]


def test_partial_falls_back_to_the_final_transcriber() -> None:
    events: list[tuple[str, dict[str, Any]]] = []
    segmenter = Segmenter(lambda event, **fields: events.append((event, fields)), lambda _: True, lambda _: "text")
    segmenter.handle({"cmd": "listen"})

    for _ in range(PARTIAL_EVERY + 1):
        segmenter.process_block(np.ones(BLOCK_SAMPLES, dtype=np.int16))

    deadline = time.monotonic() + TIMEOUT_S
    while len(events) < 2 and time.monotonic() < deadline:
        time.sleep(0.001)
    assert events == [("speech_start", {}), ("partial", {"text": "text"})]


def test_unknown_command_is_ignored(rig: Rig) -> None:
    rig.segmenter.handle({"cmd": "dance"})
    rig.speech(5)

    assert rig.events == []


def test_transcription_failure_reports_error_and_is_fatal(rig: Rig) -> None:
    rig.failure = RuntimeError("Inferenz kaputt")
    rig.listen()
    rig.speech(1)
    rig.silence(SILENCE)

    assert rig.fatal.wait(TIMEOUT_S)
    assert rig.events[-1]["event"] == "error"
    assert "Inferenz kaputt" in rig.events[-1]["message"]


# --- text_of ---


def segment(text: str, no_speech_prob: float) -> SimpleNamespace:
    return SimpleNamespace(text=text, no_speech_prob=no_speech_prob)


def test_no_speech_segments_are_dropped() -> None:
    assert text_of([segment(" Untertitel im Auftrag des ZDF", 0.9)]) == ""


def test_segment_at_the_limit_is_kept() -> None:
    assert text_of([segment(" Hallo", 0.6)]) == "Hallo"


def test_text_of_joins_kept_segments_and_trims() -> None:
    assert text_of([segment(" Guten", 0.1), segment(" Quatsch", 0.95), segment(" Morgen", 0.2)]) == "Guten Morgen"


# --- load_model ---


class FakeWhisper:
    """Stands in for WhisperModel; like the real one, it fails only once segments are consumed."""

    created: ClassVar[list[dict[str, Any]]] = []
    fail_on_create = False
    fail_on_inference = False

    def __init__(self, name: str, **options: Any) -> None:
        type(self).created.append({"name": name, **options})
        self.device = options["device"]
        if self.device == "cuda" and self.fail_on_create:
            raise RuntimeError("no CUDA device")

    def transcribe(self, _audio: np.ndarray, **_options: Any) -> tuple[Iterator[Any], None]:
        def segments() -> Iterator[Any]:
            if self.device == "cuda" and self.fail_on_inference:
                raise RuntimeError("Library libcublas.so.12 is not found or cannot be loaded")
            yield from ()

        return segments(), None


@pytest.fixture
def fake_whisper(monkeypatch: pytest.MonkeyPatch) -> type[FakeWhisper]:
    monkeypatch.setattr(FakeWhisper, "created", [])
    monkeypatch.setattr("faster_whisper.WhisperModel", FakeWhisper)
    return FakeWhisper


def test_auto_falls_back_to_cpu(
    fake_whisper: type[FakeWhisper], monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    monkeypatch.setattr(fake_whisper, "fail_on_create", True)

    with caplog.at_level(logging.INFO):
        model = load_model("small", "auto")

    assert model.device == "cpu"
    assert fake_whisper.created[-1]["compute_type"] == "int8"
    assert "cpu" in caplog.text and "int8" in caplog.text


def test_auto_falls_back_to_cpu_when_only_inference_fails(
    fake_whisper: type[FakeWhisper], monkeypatch: pytest.MonkeyPatch, caplog: pytest.LogCaptureFixture
) -> None:
    monkeypatch.setattr(fake_whisper, "fail_on_inference", True)

    with caplog.at_level(logging.INFO):
        model = load_model("small", "auto")

    assert model.device == "cpu"
    assert fake_whisper.created[-1]["compute_type"] == "int8"
    assert "libcublas" in caplog.text


def test_auto_keeps_a_working_cuda(fake_whisper: type[FakeWhisper], caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.INFO):
        model = load_model("small", "auto")

    assert model.device == "cuda"
    assert len(fake_whisper.created) == 1
    assert "cuda" in caplog.text


def test_cuda_failure_is_hard(fake_whisper: type[FakeWhisper], monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(fake_whisper, "fail_on_create", True)

    with pytest.raises(RuntimeError, match="no CUDA"):
        load_model("small", "cuda")

    assert [created["device"] for created in fake_whisper.created] == ["cuda"]


def test_cuda_failure_on_first_inference_is_hard(
    fake_whisper: type[FakeWhisper], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(fake_whisper, "fail_on_inference", True)

    with pytest.raises(RuntimeError, match="libcublas"):
        load_model("small", "cuda")

    assert [created["device"] for created in fake_whisper.created] == ["cuda"]


def test_cpu_never_touches_cuda(fake_whisper: type[FakeWhisper]) -> None:
    model = load_model("small", "cpu")

    assert model.device == "cpu"
    assert [created["device"] for created in fake_whisper.created] == ["cpu"]


def test_models_are_never_downloaded(fake_whisper: type[FakeWhisper]) -> None:
    load_model("small", "cpu")
    load_model("small", "cuda")

    assert all(created["local_files_only"] for created in fake_whisper.created)


def test_transcriber_converts_audio_and_uses_german_with_given_beam() -> None:
    calls: list[tuple[np.ndarray, dict[str, Any]]] = []

    class Model:
        def transcribe(self, audio: np.ndarray, **options: Any) -> tuple[Iterator[Any], None]:
            calls.append((audio, options))
            return iter([segment(" Hallo", 0.1)]), None

    transcribe = stt.make_transcriber(Model(), beam_size=5)

    text = transcribe(np.array([-32768, 0, 16384], dtype=np.int16))

    assert text == "Hallo"
    audio, options = calls[0]
    assert audio.dtype == np.float32
    assert audio.tolist() == [-1.0, 0.0, 0.5]
    assert options["language"] == "de"
    assert options["beam_size"] == 5


# --- SileroVad ---


class FakeSession:
    """Records model inputs; speech probability is the mean magnitude of the 512 new samples."""

    def __init__(self) -> None:
        self.inputs: list[dict[str, np.ndarray]] = []

    def run(self, _outputs: Any, feed: dict[str, np.ndarray]) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
        self.inputs.append(feed)
        probability = np.abs(feed["input"][0, 64:]).mean()
        return np.array([probability]), feed["h"] + 1, feed["c"] + 2


def fake_vad() -> tuple[SileroVad, FakeSession]:
    session = FakeSession()
    return SileroVad(session), session


def test_vad_rebuffers_blocks_into_512_sample_frames() -> None:
    vad, session = fake_vad()

    vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16))
    assert len(session.inputs) == 2  # 256 samples remain
    vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16))
    assert len(session.inputs) == 5  # 2560 samples are exactly 5 frames


def test_vad_frames_carry_64_samples_of_context_and_no_gaps() -> None:
    vad, session = fake_vad()
    ramp = np.arange(BLOCK_SAMPLES * 2, dtype=np.int16)

    vad(ramp[:BLOCK_SAMPLES])
    vad(ramp[BLOCK_SAMPLES:])

    frames = [np.rint(feed["input"][0] * 32768).astype(np.int64) for feed in session.inputs]
    assert all(frame.shape == (576,) for frame in frames)
    assert frames[0].tolist() == [0] * 64 + list(range(512))
    for index, frame in enumerate(frames[1:], start=1):
        assert frame.tolist() == list(range(index * 512 - 64, (index + 1) * 512))


def test_vad_keeps_model_state_between_calls() -> None:
    vad, session = fake_vad()

    vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16))
    vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16))

    assert [feed["h"].max() for feed in session.inputs] == [0, 1, 2, 3, 4]
    assert [feed["c"].max() for feed in session.inputs] == [0, 2, 4, 6, 8]


def test_vad_is_speech_when_any_frame_reaches_the_threshold() -> None:
    vad, _session = fake_vad()
    block = np.zeros(BLOCK_SAMPLES, dtype=np.int16)
    block[600:1024] = 32767  # second frame only, mean magnitude above 0.5

    assert vad(block) is True
    assert vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16)) is False


def test_vad_reset_clears_state_context_and_leftover() -> None:
    vad, session = fake_vad()
    vad(np.full(BLOCK_SAMPLES, 1000, dtype=np.int16))

    vad.reset()
    session.inputs.clear()
    vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16))

    assert len(session.inputs) == 2
    first = session.inputs[0]
    assert first["h"].max() == 0 and first["c"].max() == 0
    assert np.count_nonzero(first["input"]) == 0


def test_real_silero_model_stays_silent_on_zeros() -> None:
    vad = SileroVad.load()

    assert not any(vad(np.zeros(BLOCK_SAMPLES, dtype=np.int16)) for _ in range(5))


# --- process behaviour ---

PATCHED_MAIN = textwrap.dedent(
    """
    import sys
    import threading
    import time
    import numpy as np
    from nova_helpers import stt
    from nova_helpers.audio import AudioDeviceError

    stt.load_model = lambda name, device: None
    stt.make_transcriber = lambda model, beam_size: lambda audio: "text"
    class Vad:
        @classmethod
        def load(cls): return cls()
        def __call__(self, block): return bool(block[0])
        def reset(self): pass
    stt.SileroVad = Vad
    {setup}
    sys.exit(stt.main(["--model", "small", "--device", "cpu", "--input", "nova_mic"]))
    """
)

ENDLESS_SILENCE = """
class Microphone:
    def __init__(self, node): self.closed = threading.Event()
    def __enter__(self): return self
    def __exit__(self, *exc): self.closed.set()
    def __iter__(self):
        while not self.closed.wait(0.001):
            yield np.zeros(1280, dtype=np.int16)
stt.Microphone = Microphone
"""

ONE_UTTERANCE = """
class Microphone:
    def __init__(self, node): self.closed = threading.Event()
    def __enter__(self): return self
    def __exit__(self, *exc): self.closed.set()
    def __iter__(self):
        while not self.listening.wait(0.001):
            yield np.zeros(1280, dtype=np.int16)
        yield np.ones(1280, dtype=np.int16)
        while not self.closed.wait(0.001):
            yield np.zeros(1280, dtype=np.int16)
    listening = threading.Event()
stt.Microphone = Microphone
handle = stt.Segmenter.handle
def spy(self, command):
    handle(self, command)
    Microphone.listening.set()
stt.Segmenter.handle = spy
"""


def start_stt(setup: str) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [sys.executable, "-c", PATCHED_MAIN.format(setup=textwrap.dedent(setup))],
        cwd=HELPERS_DIR,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "PYTHONPATH": str(HELPERS_DIR)},
    )


def output_after_exit(process: subprocess.Popen[bytes]) -> bytes:
    """Waits with stdin still open: EOF would end the helper before the failure under test."""
    assert process.stdout is not None
    process.wait(TIMEOUT_S)
    return process.stdout.read()


def test_ready_then_exits_zero_promptly_on_stdin_eof() -> None:
    process = start_stt(ENDLESS_SILENCE)
    assert process.stdin is not None and process.stdout is not None
    assert process.stdout.readline() == b'{"event":"ready"}\n'

    process.stdin.close()

    assert process.wait(TIMEOUT_S) == 0


def test_exits_promptly_on_stdin_eof_while_transcribing() -> None:
    process = start_stt(
        ONE_UTTERANCE
        + """
stt.make_transcriber = lambda model, beam_size: lambda audio: time.sleep(60)
"""
    )
    assert process.stdin is not None and process.stdout is not None
    assert process.stdout.readline() == b'{"event":"ready"}\n'
    process.stdin.write(b'{"cmd":"listen"}\n')
    process.stdin.flush()
    assert process.stdout.readline() == b'{"event":"speech_start"}\n'

    time.sleep(0.2)  # ten silent blocks hand the utterance to the worker
    process.stdin.close()

    assert process.wait(TIMEOUT_S) == 0


def test_listen_runs_the_whole_loop_in_the_process() -> None:
    process = start_stt(ONE_UTTERANCE)
    assert process.stdin is not None and process.stdout is not None
    assert process.stdout.readline() == b'{"event":"ready"}\n'

    process.stdin.write(b'{"cmd":"listen"}\n')
    process.stdin.flush()

    assert process.stdout.readline() == b'{"event":"speech_start"}\n'
    assert process.stdout.readline() == b'{"event":"final","text":"text"}\n'
    process.stdin.close()
    assert process.wait(TIMEOUT_S) == 0


def test_device_error_on_open_exits_nonzero_without_ready() -> None:
    process = start_stt(
        """
class Microphone:
    def __init__(self, node): pass
    def __enter__(self): raise AudioDeviceError("kein Gerät")
    def __exit__(self, *exc): pass
stt.Microphone = Microphone
"""
    )

    out, _err = process.communicate(timeout=TIMEOUT_S)

    assert process.returncode == 1
    assert out == '{"event":"error","message":"kein Gerät"}\n'.encode()


def test_load_failure_exits_nonzero_without_ready() -> None:
    process = start_stt(
        ENDLESS_SILENCE
        + """
def fail(name, device): raise RuntimeError("Library libcublas.so.12 is not found")
stt.load_model = fail
"""
    )

    out, _err = process.communicate(timeout=TIMEOUT_S)

    assert process.returncode == 1
    assert out.startswith(b'{"event":"error","message":"')
    assert b"libcublas" in out
    assert b"ready" not in out


@pytest.mark.parametrize(
    ("failure", "expected"),
    [("AudioDeviceError('Gerät weg')", "Gerät weg"), ("ValueError('kaputt')", "kaputt")],
)
def test_failure_in_the_audio_thread_exits_nonzero(failure: str, expected: str) -> None:
    process = start_stt(
        f"""
class Microphone:
    def __init__(self, node): pass
    def __enter__(self): return self
    def __exit__(self, *exc): pass
    def __iter__(self):
        time.sleep(0.05)
        raise {failure}
stt.Microphone = Microphone
"""
    )

    out = output_after_exit(process)

    lines = out.decode().splitlines()
    assert process.returncode == 1
    assert lines[0] == '{"event":"ready"}'
    assert '"event":"error"' in lines[1] and expected in lines[1]


def test_failure_in_the_vad_exits_nonzero() -> None:
    process = start_stt(
        ENDLESS_SILENCE
        + """
def fail(self, block): raise ValueError("VAD kaputt")
Vad.__call__ = fail
"""
    )
    assert process.stdin is not None
    process.stdin.write(b'{"cmd":"listen"}\n')
    process.stdin.flush()

    out = output_after_exit(process)

    assert process.returncode == 1
    assert b"VAD kaputt" in out


def test_failure_in_the_worker_exits_nonzero() -> None:
    process = start_stt(
        ONE_UTTERANCE
        + """
def fail(audio): raise ValueError("Whisper kaputt")
stt.make_transcriber = lambda model, beam_size: fail
"""
    )
    assert process.stdin is not None
    process.stdin.write(b'{"cmd":"listen"}\n')
    process.stdin.flush()

    out = output_after_exit(process)

    assert process.returncode == 1
    assert b"Whisper kaputt" in out
