"""JSON Lines protocol towards Nova: commands on stdin, events on stdout, logs on stderr."""

import json
import logging
import os
import sys
import threading
from collections.abc import Iterator
from typing import Any, TextIO

log = logging.getLogger(__name__)


def claim_stdout() -> TextIO:
    """Reserves the real stdout for the protocol and sends everything else to stderr.

    Libraries print on stdout, some of them directly on fd 1, which would corrupt the
    protocol. All streams are forced to UTF-8 because the Application Launcher may start
    Nova without a UTF-8 locale. Must run before the first read from stdin.

    Returns:
        The text stream for an `Emitter`.
    """
    sys.stdout.flush()
    protocol_fd = os.dup(sys.__stdout__.fileno())
    os.dup2(sys.__stderr__.fileno(), sys.__stdout__.fileno())
    sys.stdin.reconfigure(encoding="utf-8", errors="replace")
    sys.stdout.reconfigure(encoding="utf-8", errors="backslashreplace", line_buffering=True)
    sys.stderr.reconfigure(encoding="utf-8", errors="backslashreplace")
    return open(protocol_fd, "w", encoding="utf-8", newline="\n")


def read_commands(stream: TextIO) -> Iterator[dict[str, Any]]:
    """Yields one command per line until EOF and skips anything that is not a JSON object."""
    for line in stream:
        line = line.strip()
        if not line:
            continue
        try:
            command = json.loads(line)
        except ValueError:
            log.warning("unparsable command: %r", line)
            continue
        if isinstance(command, dict):
            yield command
        else:
            log.warning("command is not an object: %r", line)


class Emitter:
    """Writes events as JSON lines; safe to call from audio and worker threads."""

    def __init__(self, stream: TextIO) -> None:
        self._stream = stream
        self._lock = threading.Lock()

    def __call__(self, event: str, **fields: Any) -> None:
        line = json.dumps({"event": event, **fields}, ensure_ascii=False, separators=(",", ":"))
        with self._lock:
            self._stream.write(f"{line}\n")
            self._stream.flush()


def setup_logging(name: str) -> None:
    """Logs to stderr, which Nova forwards to its own log, prefixed with the helper name."""
    logging.basicConfig(level=logging.INFO, stream=sys.stderr, format=f"{name} %(levelname)s %(message)s")
