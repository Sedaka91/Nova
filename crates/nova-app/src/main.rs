//! Nova main process: the voice loop (wake word -> STT -> command -> TTS).

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::Context;
use nova_app::commands::Context as Commands;
use nova_app::helper::Incoming;
use nova_app::spotify;
use nova_app::supervisor::{self, LoopSettings};
use nova_core::{apps::Apps, config::Config, phrases::Phrases};
use tracing_subscriber::EnvFilter;

/// Loading the speech models may take this long on a cold start.
const READY_TIMEOUT: Duration = Duration::from_secs(60);
/// Catches a hanging or crashed worker thread, for example behind a locked KWallet.
const WORK_TIMEOUT: Duration = Duration::from_secs(20);

fn config_dir() -> PathBuf {
    std::env::var_os("NOVA_CONFIG_DIR").map_or_else(|| PathBuf::from("config"), PathBuf::from)
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_writer(std::io::stderr)
        .init();

    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("spotify-login") => return spotify::login(),
        Some(other) => anyhow::bail!("unknown argument `{other}` (only `spotify-login` exists)"),
    }

    let dir = config_dir();
    let config = Config::load(&dir.join("nova.toml")).context("loading nova.toml")?;
    let phrases = Phrases::load(&dir.join("phrases.toml")).context("loading phrases.toml")?;

    let apps = Apps::load(&dir.join("apps.toml")).context("loading apps.toml")?;
    let commands = Arc::new(Commands {
        home: config.wetter.heimatort.clone(),
        apps,
    });

    tracing::info!(
        wake_word = %config.nova.aktivierungswort,
        sarcasm = config.persoenlichkeit.sarkasmus,
        phrase_sets = phrases.floskel.len(),
        "configuration loaded"
    );

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let specs = supervisor::helper_specs(&config, &root);
    let settings = LoopSettings {
        ready_timeout: READY_TIMEOUT,
        follow_up: Duration::try_from_secs_f32(config.dialog.nachfrage_s)
            .context("dialog.nachfrage_s is out of range")?,
        work_timeout: WORK_TIMEOUT,
    };

    let (tx, rx) = mpsc::channel();
    let shutdown = tx.clone();
    ctrlc::set_handler(move || {
        // A closed receiver means the loop has already ended.
        let _ = shutdown.send(Incoming::Shutdown);
    })
    .context("installing the Ctrl+C handler")?;

    supervisor::run(
        &specs,
        &settings,
        Arc::new(move |command| commands.execute(command)),
        tx,
        rx,
    )
}
