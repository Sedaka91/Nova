"""Tests for the wakeword helper: detector logic with fakes, process behaviour in a subprocess."""

import os
import subprocess
import sys
import textwrap
import time
from pathlib import Path
from typing import Any

import numpy as np

from nova_helpers.audio import BLOCK_SAMPLES
from nova_helpers.wakeword import WakeDetector

HELPERS_DIR = Path(__file__).resolve().parent.parent
TIMEOUT_S = 5.0


class Rig:
    """A detector with scripted scores, a fake clock and recorded events."""

    def __init__(self, threshold: float = 0.5) -> None:
        self.events: list[dict[str, Any]] = []
        self.score = 0.0
        self.now = 100.0
        self.predicted = 0
        self.resets = 0
        self.detector = WakeDetector(self._emit, self._predict, self._reset, threshold, clock=lambda: self.now)

    def _emit(self, event: str, **fields: Any) -> None:
        self.events.append({"event": event, **fields})

    def _predict(self, _block: np.ndarray) -> float:
        self.predicted += 1
        return self.score

    def _reset(self) -> None:
        self.resets += 1

    def feed(self, score: float) -> None:
        self.score = score
        self.detector.process_block(np.zeros(BLOCK_SAMPLES, dtype=np.int16))


def test_score_above_threshold_emits_wake() -> None:
    rig = Rig()

    rig.feed(0.93)

    assert rig.events == [{"event": "wake", "score": 0.93}]


def test_score_at_threshold_wakes_and_below_does_not() -> None:
    rig = Rig(threshold=0.5)

    rig.feed(0.49)
    assert rig.events == []
    rig.feed(0.5)
    assert len(rig.events) == 1


def test_second_wake_within_one_second_is_blocked() -> None:
    rig = Rig()

    rig.feed(0.9)
    rig.now += 0.99
    rig.feed(0.9)
    assert len(rig.events) == 1
    rig.now += 0.01
    rig.feed(0.9)
    assert len(rig.events) == 2


def test_blocked_wake_does_not_extend_the_lockout() -> None:
    rig = Rig()

    rig.feed(0.9)
    rig.now += 0.6
    rig.feed(0.9)
    rig.now += 0.4
    rig.feed(0.9)

    assert len(rig.events) == 2


def test_pause_drops_scores() -> None:
    rig = Rig()

    rig.detector.handle({"cmd": "pause"})
    rig.feed(0.99)

    assert rig.events == []
    assert rig.predicted == 0


def test_resume_resets_model_buffer_and_detects_again() -> None:
    rig = Rig()
    rig.detector.handle({"cmd": "pause"})

    rig.detector.handle({"cmd": "resume"})
    rig.feed(0.9)

    assert rig.resets == 1
    assert len(rig.events) == 1


def test_unknown_command_is_ignored() -> None:
    rig = Rig()

    rig.detector.handle({"cmd": "dance"})
    rig.feed(0.9)

    assert len(rig.events) == 1
    assert rig.resets == 0


# --- process behaviour ---

PATCHED_MAIN = textwrap.dedent(
    """
    import sys
    import threading
    import numpy as np
    from nova_helpers import wakeword
    from nova_helpers.audio import AudioDeviceError

    {setup}
    sys.exit(wakeword.main(["--model", "hey_jarvis", "--threshold", "0.5", "--input", "nova_mic"]))
    """
)

FAKE_MODEL = "wakeword._load_model = lambda name: ((lambda block: 0.0), (lambda: None))"


def start_wakeword(setup: str, env: dict[str, str] | None = None) -> subprocess.Popen[bytes]:
    return subprocess.Popen(
        [sys.executable, "-c", PATCHED_MAIN.format(setup=textwrap.dedent(setup))],
        cwd=HELPERS_DIR,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env={**os.environ, "PYTHONPATH": str(HELPERS_DIR), **(env or {})},
    )


def test_device_error_exits_nonzero() -> None:
    process = start_wakeword(
        f"""
        class Microphone:
            def __init__(self, node): pass
            def __enter__(self): raise AudioDeviceError("kein Gerät")
            def __exit__(self, *exc): pass
        wakeword.Microphone = Microphone
        {FAKE_MODEL}
        """
    )

    out, _err = process.communicate(timeout=TIMEOUT_S)

    assert process.returncode == 1
    assert out == '{"event":"error","message":"kein Gerät"}\n'.encode()


def test_device_error_while_listening_exits_nonzero() -> None:
    process = start_wakeword(
        f"""
        class Microphone:
            def __init__(self, node): pass
            def __enter__(self): return self
            def __exit__(self, *exc): pass
            def __iter__(self): raise AudioDeviceError("Gerät weg")
        wakeword.Microphone = Microphone
        {FAKE_MODEL}
        """
    )

    out, _err = process.communicate(timeout=TIMEOUT_S)

    assert process.returncode == 1
    assert out == b'{"event":"ready"}\n{"event":"error","message":"Ger\xc3\xa4t weg"}\n'


def test_ready_then_exits_zero_promptly_on_stdin_eof() -> None:
    process = start_wakeword(
        f"""
        class Microphone:
            def __init__(self, node): self.closed = threading.Event()
            def __enter__(self): return self
            def __exit__(self, *exc): self.closed.set()
            def __iter__(self):
                while not self.closed.wait(0.01):
                    yield np.zeros(1280, dtype=np.int16)
        wakeword.Microphone = Microphone
        {FAKE_MODEL}
        """
    )
    assert process.stdin is not None and process.stdout is not None
    assert process.stdout.readline() == b'{"event":"ready"}\n'

    started = time.monotonic()
    process.stdin.close()

    assert process.wait(timeout=TIMEOUT_S) == 0
    assert time.monotonic() - started < 3
    process.stdout.close()
    process.stderr and process.stderr.close()


def test_missing_model_file_exits_one_with_setup_hint(tmp_path: Path) -> None:
    process = start_wakeword("", env={"XDG_DATA_HOME": str(tmp_path)})

    out, _err = process.communicate(timeout=30)

    assert process.returncode == 1
    assert b'"event":"error"' in out
    assert b"python -m nova_helpers.setup" in out
    assert b"ready" not in out
