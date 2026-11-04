//! Nova main process. Phase 1 adds the voice loop (wake word -> STT -> echo -> TTS).

use std::path::PathBuf;

use anyhow::Context;
use nova_core::{config::Config, phrases::Phrases};
use tracing_subscriber::EnvFilter;

fn config_dir() -> PathBuf {
    std::env::var_os("NOVA_CONFIG_DIR").map_or_else(|| PathBuf::from("config"), PathBuf::from)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let dir = config_dir();
    let config = Config::load(&dir.join("nova.toml")).context("loading nova.toml")?;
    let phrases = Phrases::load(&dir.join("phrases.toml")).context("loading phrases.toml")?;

    tracing::info!(
        wake_word = %config.nova.aktivierungswort,
        sarcasm = config.persoenlichkeit.sarkasmus,
        phrase_sets = phrases.floskel.len(),
        "configuration loaded"
    );
    Ok(())
}
