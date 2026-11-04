"""Downloads the voice and the wake word models; the helpers themselves never download."""

import logging

from openwakeword.utils import download_models
from piper.download_voices import download_voice

from nova_helpers.audio import data_dir
from nova_helpers.protocol import setup_logging

VOICE = "de_DE-thorsten-medium"
WAKE_WORD = "hey_jarvis"

log = logging.getLogger(__name__)


def main() -> None:
    """Fetches what is missing into `data_dir()`, leaving existing files alone."""
    voice_dir = data_dir() / "piper"
    model_dir = data_dir() / "openwakeword"
    voice_dir.mkdir(parents=True, exist_ok=True)
    model_dir.mkdir(parents=True, exist_ok=True)

    log.info("downloading voice %s to %s", VOICE, voice_dir)
    download_voice(VOICE, voice_dir)
    log.info("downloading wake word models to %s", model_dir)
    download_models([WAKE_WORD], str(model_dir))
    log.info("setup finished")


if __name__ == "__main__":
    setup_logging("setup")
    main()
