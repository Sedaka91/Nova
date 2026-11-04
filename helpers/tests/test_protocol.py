"""Tests for the JSON Lines protocol; the example lines match the Rust tests."""

import io
import json
import os
import subprocess
import sys
import textwrap
import threading
from pathlib import Path

from nova_helpers.protocol import Emitter, read_commands

HELPERS_DIR = Path(__file__).resolve().parent.parent
TEXT = "Größe „Äpfel“"
# Switches off everything that would mask a missing explicit UTF-8 setup.
C_LOCALE = {"LC_ALL": "C", "PYTHONCOERCECLOCALE": "0", "PYTHONUTF8": "0"}


def run_python(code: str, stdin: bytes = b"", env: dict[str, str] | None = None) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        [sys.executable, "-c", textwrap.dedent(code)],
        cwd=HELPERS_DIR,
        input=stdin,
        capture_output=True,
        timeout=30,
        check=True,
        env={**os.environ, "PYTHONPATH": str(HELPERS_DIR), **(env or {})},
    )


def test_emit_keeps_umlauts_as_utf8():
    raw = io.BytesIO()
    stream = io.TextIOWrapper(raw, encoding="utf-8")

    Emitter(stream)("final", text=TEXT)

    assert raw.getvalue().decode("utf-8") == f'{{"event":"final","text":"{TEXT}"}}\n'


def test_emit_flushes_every_line():
    raw = io.BytesIO()
    stream = io.TextIOWrapper(raw, encoding="utf-8")

    Emitter(stream)("ready")

    assert raw.getvalue() == b'{"event":"ready"}\n'


def test_emit_from_many_threads_keeps_lines_whole():
    stream = io.StringIO()
    emit = Emitter(stream)

    def emit_many() -> None:
        for _ in range(200):
            emit("partial", text="x" * 500)

    threads = [threading.Thread(target=emit_many) for _ in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()

    lines = stream.getvalue().splitlines()
    assert len(lines) == 1600
    assert all(json.loads(line)["event"] == "partial" for line in lines)


def test_read_commands_skips_garbage_and_stops_at_eof():
    stream = io.StringIO('kein json\n\n{"cmd":"listen"}\n[1,2]\n{"cmd":"cancel"}\n')

    assert list(read_commands(stream)) == [{"cmd": "listen"}, {"cmd": "cancel"}]


def test_claimed_stdout_ignores_library_prints():
    result = run_python(
        """
        import logging
        import os
        from nova_helpers.protocol import Emitter, claim_stdout, setup_logging

        emit = Emitter(claim_stdout())
        setup_logging("test")
        print("lärm")
        os.write(1, b"c-bibliothek\\n")
        logging.getLogger("test").info("protokoll")
        emit("ready")
        """
    )

    assert result.stdout == b'{"event":"ready"}\n'
    stderr = result.stderr.decode("utf-8")
    assert "lärm" in stderr
    assert "c-bibliothek" in stderr
    assert "protokoll" in stderr


def test_utf8_with_c_locale():
    command = json.dumps({"cmd": "speak", "id": "1", "text": TEXT}, ensure_ascii=False)
    result = run_python(
        """
        import sys
        from nova_helpers.protocol import Emitter, claim_stdout, read_commands

        emit = Emitter(claim_stdout())
        print("l\\u00e4rm")  # argv is not valid UTF-8 for Python in this locale
        for command in read_commands(sys.stdin):
            emit("final", text=command["text"])
        """,
        stdin=f"{command}\n".encode(),
        env=C_LOCALE,
    )

    assert result.stdout.decode("utf-8") == f'{{"event":"final","text":"{TEXT}"}}\n'
    assert "lärm" in result.stderr.decode("utf-8")


def test_invalid_utf8_on_stdin_does_not_stop_the_loop():
    result = run_python(
        """
        import sys
        from nova_helpers.protocol import Emitter, claim_stdout, read_commands

        emit = Emitter(claim_stdout())
        for command in read_commands(sys.stdin):
            emit("final", text=command["cmd"])
        """,
        stdin=b'\xff\xfe kaputt\n{"cmd":"listen"}\n',
        env=C_LOCALE,
    )

    assert result.stdout == b'{"event":"final","text":"listen"}\n'
