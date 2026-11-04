"""Tests for the tts helper: worker logic with fakes, process behaviour in a subprocess."""

import os
import subprocess
import sys
import textwrap
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
from typing import Any

import numpy as np

from nova_helpers.audio import AudioDeviceError
from nova_helpers.tts import TtsWorker

HELPERS_DIR = Path(__file__).resolve().parent.parent
TIMEOUT_S = 5.0


class Events:
    """Collects emitted events as dicts."""

    def __init__(self) -> None:
        self.items: list[dict[str, Any]] = []

    def __call__(self, event: str, **fields: Any) -> None:
        self.items.append({"event": event, **fields})

    def names(self) -> list[tuple[str, str | None]]:
        return [(item["event"], item.get("id")) for item in self.items]

    def wait_for(self, event: str) -> None:
        deadline = time.monotonic() + TIMEOUT_S
        while not any(item["event"] == event for item in self.items):
            assert time.monotonic() < deadline, f"no {event} event in {self.items}"
            time.sleep(0.005)


class FakeSpeaker:
    def __init__(self) -> None:
        self.written: list[np.ndarray] = []
        self.aborted = 0

    def write(self, pcm: np.ndarray) -> None:
        self.written.append(pcm)

    def abort(self) -> None:
        self.aborted += 1


def chunk(value: int) -> np.ndarray:
    return np.full(10, value, dtype=np.int16)


def values(speaker: FakeSpeaker) -> list[int]:
    return [int(pcm[0]) for pcm in speaker.written]


def speak(worker: TtsWorker, utterance: str, text: str = "x") -> None:
    worker.handle({"cmd": "speak", "id": utterance, "text": text})


def make_worker(
    synthesize: Callable[[str], Iterator[np.ndarray]] | None = None,
    speaker: FakeSpeaker | None = None,
    on_fatal: Callable[[], None] | None = None,
) -> tuple[TtsWorker, Events, FakeSpeaker]:
    events, speaker = Events(), speaker or FakeSpeaker()
    worker = TtsWorker(events, synthesize or (lambda _text: iter([chunk(1)])), speaker, on_fatal=on_fatal)
    return worker, events, speaker


def test_speak_emits_started_then_done_in_order() -> None:
    worker, events, speaker = make_worker(lambda _text: iter([chunk(1), chunk(2)]))

    speak(worker, "1")
    events.wait_for("done")
    worker.close()

    assert events.names() == [("started", "1"), ("done", "1")]
    assert values(speaker) == [1, 2]


def test_started_comes_before_the_first_audio_block() -> None:
    events = Events()
    seen_at_write: list[list[str]] = []

    class RecordingSpeaker(FakeSpeaker):
        def write(self, pcm: np.ndarray) -> None:
            seen_at_write.append([item["event"] for item in events.items])

    worker = TtsWorker(events, lambda _text: iter([chunk(1)]), RecordingSpeaker())
    speak(worker, "1")
    events.wait_for("done")
    worker.close()

    assert seen_at_write == [["started"]]


def test_queue_keeps_order_of_two_utterances() -> None:
    worker, events, speaker = make_worker(lambda text: iter([chunk(int(text))]))

    speak(worker, "a", "1")
    speak(worker, "b", "2")
    deadline = time.monotonic() + TIMEOUT_S
    while ("done", "b") not in events.names():
        assert time.monotonic() < deadline
        time.sleep(0.005)
    worker.close()

    assert events.names() == [("started", "a"), ("done", "a"), ("started", "b"), ("done", "b")]
    assert values(speaker) == [1, 2]


def test_stop_aborts_clears_queue_and_sends_no_done() -> None:
    inside, release = threading.Event(), threading.Event()
    synthesized: list[str] = []

    def synthesize(text: str) -> Iterator[np.ndarray]:
        synthesized.append(text)
        inside.set()
        release.wait(TIMEOUT_S)
        yield chunk(1)

    worker, events, speaker = make_worker(synthesize)
    speak(worker, "first", "one")
    speak(worker, "second", "two")
    assert inside.wait(TIMEOUT_S)

    worker.handle({"cmd": "stop"})
    release.set()
    worker.close()

    assert speaker.aborted >= 1
    assert synthesized == ["one"]
    assert events.names() == [("started", "first")]
    assert speaker.written == []


def test_stop_during_synthesis_writes_no_later_chunk() -> None:
    in_second, release = threading.Event(), threading.Event()

    def synthesize(_text: str) -> Iterator[np.ndarray]:
        yield chunk(1)
        in_second.set()
        release.wait(TIMEOUT_S)
        yield chunk(2)
        yield chunk(3)

    worker, events, speaker = make_worker(synthesize)
    speak(worker, "1")
    assert in_second.wait(TIMEOUT_S)

    worker.handle({"cmd": "stop"})
    release.set()
    worker.close()

    assert values(speaker) == [1]
    assert "done" not in [item["event"] for item in events.items]


def test_stop_during_a_blocked_write_releases_it_and_ends_the_utterance() -> None:
    writing, aborted = threading.Event(), threading.Event()

    class BlockingSpeaker(FakeSpeaker):
        def write(self, pcm: np.ndarray) -> None:
            super().write(pcm)
            writing.set()
            aborted.wait(TIMEOUT_S)

        def abort(self) -> None:
            super().abort()
            aborted.set()

    worker, events, speaker = make_worker(lambda _text: iter([chunk(1), chunk(2)]), BlockingSpeaker())
    speak(worker, "1")
    assert writing.wait(TIMEOUT_S)

    worker.handle({"cmd": "stop"})
    worker.close()

    assert values(speaker) == [1]
    assert events.names() == [("started", "1")]


def test_stop_between_last_write_and_done_sends_no_done() -> None:
    holder: list[TtsWorker] = []

    def synthesize(_text: str) -> Iterator[np.ndarray]:
        yield chunk(1)
        holder[0].handle({"cmd": "stop"})  # runs after the last write returned, before done

    worker, events, _speaker = make_worker(synthesize)
    holder.append(worker)
    speak(worker, "1")
    events.wait_for("started")
    worker.close()

    assert events.names() == [("started", "1")]


def test_speaking_works_again_after_stop() -> None:
    worker, events, speaker = make_worker()
    worker.handle({"cmd": "stop"})

    speak(worker, "1")
    events.wait_for("done")
    worker.close()

    assert events.names() == [("started", "1"), ("done", "1")]
    assert values(speaker) == [1]


def test_stop_when_idle_is_harmless() -> None:
    worker, events, speaker = make_worker()

    worker.handle({"cmd": "stop"})
    worker.close()

    assert events.items == []
    assert speaker.written == []


def test_unknown_command_is_ignored() -> None:
    worker, events, _speaker = make_worker()

    worker.handle({"cmd": "dance"})
    worker.close()

    assert events.items == []


def test_device_error_is_reported_and_fatal() -> None:
    class BrokenSpeaker(FakeSpeaker):
        def write(self, pcm: np.ndarray) -> None:
            raise AudioDeviceError("Audiogerät nova_sink ist nicht verfügbar")

    fatal = threading.Event()
    worker, events, _speaker = make_worker(speaker=BrokenSpeaker(), on_fatal=fatal.set)

    speak(worker, "1")

    assert fatal.wait(TIMEOUT_S)
    worker.close()
    assert {"event": "error", "message": "Audiogerät nova_sink ist nicht verfügbar"} in events.items
    assert "done" not in [item["event"] for item in events.items]


def test_synthesis_failure_is_reported_and_the_utterance_still_ends() -> None:
    def synthesize(_text: str) -> Iterator[np.ndarray]:
        raise RuntimeError("boom")
        yield chunk(0)

    worker, events, _speaker = make_worker(synthesize)

    speak(worker, "1")
    events.wait_for("done")
    worker.close()

    assert [item["event"] for item in events.items] == ["started", "error", "done"]


# --- process behaviour ---

PATCHED_MAIN = textwrap.dedent(
    """
    import sys
    import numpy as np
    from nova_helpers import tts
    from nova_helpers.audio import AudioDeviceError

    {setup}
    sys.exit(tts.main(["--voice", "v", "--output", "nova_sink"]))
    """
)


def start_tts(setup: str, env: dict[str, str] | None = None) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [sys.executable, "-c", PATCHED_MAIN.format(setup=textwrap.dedent(setup))],
        cwd=HELPERS_DIR,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "PYTHONPATH": str(HELPERS_DIR), **(env or {})},
    )


def test_exits_zero_promptly_on_stdin_eof_even_while_speaking() -> None:
    process = start_tts(
        """
        import threading
        class Speaker:
            def __init__(self, node, rate): pass
            def write(self, pcm): threading.Event().wait(60)
            def abort(self): pass
        def load_voice(name):
            return (lambda text: iter([np.zeros(10, dtype=np.int16)])), 22050
        tts.Speaker = Speaker
        tts._load_voice = load_voice
        """
    )
    assert process.stdin is not None and process.stdout is not None
    assert process.stdout.readline() == b'{"event":"ready"}\n'
    process.stdin.write(b'{"cmd":"speak","id":"1","text":"x"}\n')
    process.stdin.flush()
    assert process.stdout.readline() == b'{"event":"started","id":"1"}\n'

    started = time.monotonic()
    process.stdin.close()

    assert process.wait(timeout=TIMEOUT_S) == 0
    assert time.monotonic() - started < 3
    process.stdout.close()
    process.stderr and process.stderr.close()


def test_device_error_on_open_exits_one_with_error_event() -> None:
    process = start_tts(
        """
        class Speaker:
            def __init__(self, node, rate): raise AudioDeviceError("kein Gerät")
        tts.Speaker = Speaker
        tts._load_voice = lambda name: ((lambda text: iter([])), 22050)
        """
    )

    out, _err = process.communicate(timeout=TIMEOUT_S)

    assert process.returncode == 1
    assert out == '{"event":"error","message":"kein Gerät"}\n'.encode()


def test_missing_voice_file_exits_one_with_setup_hint(tmp_path: Path) -> None:
    process = start_tts("", env={"XDG_DATA_HOME": str(tmp_path)})

    out, _err = process.communicate(timeout=30)

    assert process.returncode == 1
    assert b'"event":"error"' in out
    assert b"python -m nova_helpers.setup" in out
    assert str(tmp_path / "nova" / "piper" / "v.onnx").encode() in out
    assert b"ready" not in out
