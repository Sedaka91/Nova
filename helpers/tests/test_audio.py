"""Tests for audio access and the model setup; `sounddevice` is replaced by a fake stream."""

import os
import threading
from pathlib import Path
from typing import Any

import numpy as np
import pytest

from nova_helpers import audio, setup
from nova_helpers.audio import BLOCK_SAMPLES, SAMPLE_RATE, AudioDeviceError, Microphone, Speaker, data_dir

HINT = "Echo-Modul aktivieren, siehe README"
THREAD_TIMEOUT_S = 5.0


class FakeStream:
    """Stands in for `sd.InputStream`/`sd.OutputStream` and records how it was opened."""

    def __init__(self, **options: Any) -> None:
        self.options = options
        self.node = os.environ.get("PIPEWIRE_NODE")
        self.props = os.environ.get("PIPEWIRE_PROPS")
        self.started = False
        self.closed = False
        FakeStream.last = self

    def start(self) -> None:
        self.started = True

    def close(self) -> None:
        self.closed = True

    def capture(self, samples: np.ndarray) -> None:
        self.options["callback"](samples.reshape(-1, 1), len(samples), None, None)

    def pull(self, frames: int) -> np.ndarray:
        output = np.full((frames, 1), -1, dtype=np.int16)
        self.options["callback"](output, frames, None, None)
        return output[:, 0]

    def fail(self) -> None:
        self.options["finished_callback"]()


@pytest.fixture(autouse=True)
def fake_streams(monkeypatch: pytest.MonkeyPatch) -> None:
    # Registers both variables so that monkeypatch restores them after the test.
    monkeypatch.setenv("PIPEWIRE_NODE", "unset")
    monkeypatch.setenv("PIPEWIRE_PROPS", "unset")
    monkeypatch.setattr(audio.sd, "InputStream", FakeStream)
    monkeypatch.setattr(audio.sd, "OutputStream", FakeStream)


def block(value: int = 0) -> np.ndarray:
    return np.full(BLOCK_SAMPLES, value, dtype=np.int16)


def test_block_is_80_ms_at_16_khz() -> None:
    assert SAMPLE_RATE == 16_000
    assert BLOCK_SAMPLES == SAMPLE_RATE * 80 // 1000


def test_microphone_sets_pipewire_node() -> None:
    with Microphone("nova_mic"):
        stream = FakeStream.last
        assert stream.node == "nova_mic"
        assert stream.options["device"] == "pipewire"
        assert stream.options["samplerate"] == SAMPLE_RATE
        assert stream.options["blocksize"] == BLOCK_SAMPLES
        assert stream.options["channels"] == 1
        assert stream.options["dtype"] == "int16"
        assert stream.started


def test_real_device_node_is_passed_through_unchanged() -> None:
    node = "alsa_input.usb-C-Media_Electronics_Inc._JLAB_TALK_PRO_MICROPHONE-00.analog-stereo"
    with Microphone(node):
        assert FakeStream.last.node == node


@pytest.mark.parametrize(
    "open_device", [lambda: Microphone("nova_mic").__enter__(), lambda: Speaker("nova_sink", 22_050)]
)
def test_streams_never_fall_back_to_the_default_device(open_device: Any) -> None:
    open_device()
    assert "node.dont-fallback = true" in FakeStream.last.props
    assert "node.dont-reconnect = true" in FakeStream.last.props


@pytest.mark.parametrize(
    ("open_device", "node"),
    [(lambda: Microphone("nova_mic").__enter__(), "nova_mic"), (lambda: Speaker("nova_sink", 22_050), "nova_sink")],
)
def test_missing_device_raises_with_hint(monkeypatch: pytest.MonkeyPatch, open_device: Any, node: str) -> None:
    def refuse(**_options: Any) -> None:
        raise audio.sd.PortAudioError("Error opening stream: 'No such file or directory' [ALSA error -2]")

    monkeypatch.setattr(audio.sd, "InputStream", refuse)
    monkeypatch.setattr(audio.sd, "OutputStream", refuse)

    with pytest.raises(AudioDeviceError) as error:
        open_device()

    assert HINT in str(error.value)
    assert node in str(error.value)


def test_blocks_have_fixed_length() -> None:
    with Microphone("nova_mic") as microphone:
        FakeStream.last.capture(block(7))
        FakeStream.last.capture(block(8))
        blocks = iter(microphone)
        first, second = next(blocks), next(blocks)

    for received in (first, second):
        assert received.shape == (BLOCK_SAMPLES,)
        assert received.dtype == np.int16
    assert first[0] == 7
    assert second[0] == 8


def test_blocks_survive_the_reuse_of_the_stream_buffer() -> None:
    samples = block(7)
    with Microphone("nova_mic") as microphone:
        FakeStream.last.capture(samples)
        samples.fill(0)
        assert next(iter(microphone))[0] == 7


def test_leaving_the_context_ends_iteration_and_closes_the_stream() -> None:
    with Microphone("nova_mic") as microphone:
        stream = FakeStream.last
        blocks = iter(microphone)
    assert list(blocks) == []
    assert stream.closed


def test_slow_reader_loses_the_oldest_blocks(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(audio, "_MAX_QUEUED_BLOCKS", 2)
    with Microphone("nova_mic") as microphone:
        for value in (1, 2, 3):
            FakeStream.last.capture(block(value))
        blocks = iter(microphone)
        assert [next(blocks)[0], next(blocks)[0]] == [2, 3]


def test_stream_failure_ends_iteration_with_error() -> None:
    with Microphone("nova_mic") as microphone:
        FakeStream.last.capture(block(7))
        FakeStream.last.fail()
        blocks = iter(microphone)
        assert next(blocks)[0] == 7
        with pytest.raises(AudioDeviceError) as error:
            next(blocks)
    assert "nova_mic" in str(error.value)


def test_stalled_stream_ends_iteration_with_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(audio, "_STALL_TIMEOUT_S", 0.01)
    with Microphone("nova_mic") as microphone, pytest.raises(AudioDeviceError):
        next(iter(microphone))


def write_in_thread(speaker: Speaker, pcm: np.ndarray) -> tuple[threading.Thread, list[Exception]]:
    errors: list[Exception] = []

    def run() -> None:
        try:
            speaker.write(pcm)
        except AudioDeviceError as error:
            errors.append(error)

    thread = threading.Thread(target=run)
    thread.start()
    return thread, errors


def test_speaker_opens_node_with_the_voice_rate() -> None:
    Speaker("nova_sink", 22_050)
    stream = FakeStream.last
    assert stream.node == "nova_sink"
    assert stream.options["device"] == "pipewire"
    assert stream.options["samplerate"] == 22_050
    assert stream.options["channels"] == 1
    assert stream.options["dtype"] == "int16"
    assert stream.started


def test_speaker_plays_samples_in_order_and_pads_with_silence() -> None:
    speaker = Speaker("nova_sink", 1_000)
    speaker.write(np.array([1, 2, 3], dtype=np.int16))
    assert FakeStream.last.pull(2).tolist() == [1, 2]
    assert FakeStream.last.pull(4).tolist() == [3, 0, 0, 0]


def test_speaker_write_waits_for_the_buffer_to_drain() -> None:
    speaker = Speaker("nova_sink", 1_000)  # keeps at most 100 samples queued
    thread, errors = write_in_thread(speaker, np.ones(300, dtype=np.int16))

    thread.join(0.05)
    assert thread.is_alive()

    FakeStream.last.pull(200)
    thread.join(THREAD_TIMEOUT_S)
    assert not thread.is_alive()
    assert errors == []


def test_speaker_abort_discards_the_buffer_and_releases_the_writer() -> None:
    speaker = Speaker("nova_sink", 1_000)
    thread, errors = write_in_thread(speaker, np.ones(300, dtype=np.int16))
    thread.join(0.05)

    speaker.abort()

    thread.join(THREAD_TIMEOUT_S)
    assert not thread.is_alive()
    assert errors == []
    assert FakeStream.last.pull(3).tolist() == [0, 0, 0]


def test_speaker_write_fails_after_stream_failure() -> None:
    speaker = Speaker("nova_sink", 1_000)
    thread, errors = write_in_thread(speaker, np.ones(300, dtype=np.int16))
    thread.join(0.05)

    FakeStream.last.fail()

    thread.join(THREAD_TIMEOUT_S)
    assert not thread.is_alive()
    assert len(errors) == 1
    with pytest.raises(AudioDeviceError):
        speaker.write(np.ones(1, dtype=np.int16))


def test_speaker_write_fails_when_the_stream_stalls(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(audio, "_STALL_TIMEOUT_S", 0.01)
    speaker = Speaker("nova_sink", 1_000)
    with pytest.raises(AudioDeviceError):
        speaker.write(np.ones(300, dtype=np.int16))


def test_data_dir_follows_xdg(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    assert data_dir() == tmp_path / "nova"


@pytest.mark.parametrize("value", [None, ""])
def test_data_dir_defaults_to_local_share(monkeypatch: pytest.MonkeyPatch, tmp_path: Path, value: str | None) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    if value is None:
        monkeypatch.delenv("XDG_DATA_HOME", raising=False)
    else:
        monkeypatch.setenv("XDG_DATA_HOME", value)
    assert data_dir() == tmp_path / ".local" / "share" / "nova"


def test_setup_targets_data_dir(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    voices: list[tuple[str, Path]] = []
    models: list[tuple[list[str], str]] = []
    monkeypatch.setattr(setup, "download_voice", lambda voice, directory: voices.append((voice, directory)))
    monkeypatch.setattr(setup, "download_models", lambda names, target: models.append((names, target)))

    setup.main()

    voice_dir = tmp_path / "nova" / "piper"
    model_dir = tmp_path / "nova" / "openwakeword"
    assert voices == [("de_DE-thorsten-medium", voice_dir)]
    assert models == [(["hey_jarvis"], str(model_dir))]
    assert voice_dir.is_dir()
    assert model_dir.is_dir()
