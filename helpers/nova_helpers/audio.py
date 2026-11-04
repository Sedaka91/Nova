"""Audio access for the helpers: PipeWire nodes, opened through the ALSA device `pipewire`."""

import os
import queue
import threading
from collections.abc import Callable, Iterator
from pathlib import Path
from types import TracebackType
from typing import Any, Self

import numpy as np
import sounddevice as sd

SAMPLE_RATE = 16_000
BLOCK_SAMPLES = 1_280  # 80 ms

_DEVICE = "pipewire"
# Without these, PipeWire silently routes to the default device when the node is missing or
# goes away, and Nova would keep running without echo cancellation.
_STRICT_TARGET = "{ node.dont-fallback = true node.dont-reconnect = true }"
_HINT = "Echo-Modul aktivieren, siehe README"
_STALL_TIMEOUT_S = 2.0
_MAX_QUEUED_BLOCKS = 25  # 2 s
_SPEAKER_BLOCKS_PER_S = 25  # 40 ms
_SPEAKER_QUEUED_PER_S = 10  # 100 ms


class AudioDeviceError(Exception):
    """An audio node cannot be opened or stopped delivering; the text is meant for the user."""


def data_dir() -> Path:
    """Returns the directory for downloaded models: `$XDG_DATA_HOME/nova`, else `~/.local/share/nova`."""
    base = os.environ.get("XDG_DATA_HOME")
    return Path(base) / "nova" if base else Path.home() / ".local" / "share" / "nova"


def _unavailable(node: str) -> AudioDeviceError:
    return AudioDeviceError(f"Audiogerät {node} ist nicht verfügbar. {_HINT}")


def _open_stream(stream_type: Callable[..., Any], node: str, **options: Any) -> Any:
    """Opens and starts a mono 16-bit stream on the PipeWire node `node`.

    The node is passed through the environment, which the ALSA plugin reads on open.

    Raises:
        AudioDeviceError: If the node does not exist.
    """
    os.environ["PIPEWIRE_NODE"] = node
    os.environ["PIPEWIRE_PROPS"] = _STRICT_TARGET
    try:
        stream = stream_type(device=_DEVICE, channels=1, dtype="int16", **options)
        stream.start()
    except (sd.PortAudioError, ValueError) as error:
        raise _unavailable(node) from error
    return stream


class Microphone:
    """Yields blocks of `BLOCK_SAMPLES` int16 samples from a PipeWire node.

    Iteration ends when the context is left, which may happen on another thread. It raises
    `AudioDeviceError` when the stream fails or stalls. A reader that falls behind loses
    the oldest blocks.
    """

    def __init__(self, node: str) -> None:
        self._node = node
        self._blocks: queue.Queue[np.ndarray | None] = queue.Queue(maxsize=_MAX_QUEUED_BLOCKS)
        self._stream: Any = None
        self._closed = False

    def __enter__(self) -> Self:
        self._stream = _open_stream(
            sd.InputStream,
            self._node,
            samplerate=SAMPLE_RATE,
            blocksize=BLOCK_SAMPLES,
            callback=self._on_block,
            finished_callback=self._on_finished,
        )
        return self

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> None:
        self._closed = True
        self._stream.close()
        self._offer(None)

    def __iter__(self) -> Iterator[np.ndarray]:
        while True:
            try:
                block = self._blocks.get(timeout=_STALL_TIMEOUT_S)
            except queue.Empty:
                raise _unavailable(self._node) from None
            if block is not None:
                yield block
            elif self._closed:
                return
            else:
                raise _unavailable(self._node)

    def _on_block(self, indata: np.ndarray, _frames: int, _time: Any, _status: Any) -> None:
        self._offer(indata[:, 0].copy())

    def _on_finished(self) -> None:
        self._offer(None)

    def _offer(self, item: np.ndarray | None) -> None:
        while True:
            try:
                self._blocks.put_nowait(item)
                return
            except queue.Full:
                try:
                    self._blocks.get_nowait()
                except queue.Empty:
                    pass


class Speaker:
    """Plays mono int16 samples on a PipeWire node; `abort` may run on another thread.

    The stream stays open and plays silence while nothing is queued.
    """

    def __init__(self, node: str, rate: int) -> None:
        self._node = node
        self._queued = np.empty(0, dtype=np.int16)
        self._max_queued = rate // _SPEAKER_QUEUED_PER_S
        self._changed = threading.Condition()
        self._failed = False
        self._stream = _open_stream(
            sd.OutputStream,
            node,
            samplerate=rate,
            blocksize=rate // _SPEAKER_BLOCKS_PER_S,
            callback=self._on_output,
            finished_callback=self._on_finished,
        )

    def write(self, pcm: np.ndarray) -> None:
        """Queues `pcm` and returns once at most 100 ms are left to play, or after `abort`.

        Raises:
            AudioDeviceError: If the stream failed or stalled.
        """
        with self._changed:
            self._queued = np.concatenate((self._queued, pcm))
            while True:
                if self._failed:
                    raise _unavailable(self._node)
                if len(self._queued) <= self._max_queued:
                    return
                if not self._changed.wait(timeout=_STALL_TIMEOUT_S):
                    raise _unavailable(self._node)

    def abort(self) -> None:
        """Discards everything queued at once and releases a waiting `write`."""
        with self._changed:
            self._queued = self._queued[:0]
            self._changed.notify_all()

    def _on_output(self, outdata: np.ndarray, frames: int, _time: Any, _status: Any) -> None:
        with self._changed:
            samples = self._queued[:frames]
            self._queued = self._queued[frames:]
            self._changed.notify_all()
        outdata[: len(samples), 0] = samples
        outdata[len(samples) :, 0] = 0

    def _on_finished(self) -> None:
        with self._changed:
            self._failed = True
            self._changed.notify_all()
