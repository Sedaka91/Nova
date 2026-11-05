//! `config/nova.toml`: personality sliders, routing, voice, integrations.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid value: {0}")]
    Invalid(String),
}

pub(crate) fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_owned(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConfigError::Parse {
        path: path.to_owned(),
        source,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub nova: General,
    pub persoenlichkeit: Personality,
    pub router: Router,
    pub floskeln: PhraseTiming,
    pub stimme: Voice,
    pub spracherkennung: Stt,
    pub modelle: Models,
    pub vault: Vault,
    pub lernen: Learning,
    pub ntfy: Ntfy,
    pub wakeword: WakeWord,
    pub audio: Audio,
    pub dialog: Dialog,
    pub wetter: Weather,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub aktivierungswort: String,
}

/// Sliders. `sarkasmus` is the phrase level (0..=2), the others are 0..=10.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Personality {
    pub sarkasmus: u8,
    pub gespraechigkeit: u8,
    pub foermlichkeit: u8,
    pub humor_haeufigkeit: u8,
    /// Share of "Boss" vs. "Chef" (0.0..=1.0).
    pub anrede_boss_anteil: f32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Router {
    /// Below this confidence the request goes one stage higher.
    pub konfidenz_schwelle: f32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhraseTiming {
    /// Seconds without an Opus answer before the second phrase plays.
    pub zweite_floskel_nach_s: f32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Voice {
    pub piper_modell: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stt {
    pub whisper_modell: String,
    /// "cpu", "cuda" or "auto".
    pub geraet: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    pub ollama_url: String,
    /// Empty until phase 3 picks a model.
    pub chat_modell: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vault {
    /// Empty until the vault path is set.
    pub pfad: String,
    /// Note inside the vault holding the approved folders (agent must not edit it).
    pub freigaben_notiz: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Learning {
    pub min_wiederholungen: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ntfy {
    pub server: String,
    pub thema: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeWord {
    /// Path relative to the project root (contains a `/`) or the name of a bundled model.
    pub modell: String,
    /// Detection threshold (0.0..=1.0).
    pub schwelle: f32,
}

impl WakeWord {
    /// The model argument for the helper: a path is resolved against `root`, a name stays as is.
    pub fn model_arg(&self, root: &Path) -> String {
        if self.modell.contains('/') {
            root.join(&self.modell).to_string_lossy().into_owned()
        } else {
            self.modell.clone()
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    /// PipeWire source the helpers record from.
    pub eingabe: String,
    /// PipeWire sink the helpers play to.
    pub ausgabe: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dialog {
    /// Seconds to wait for speech after the wake word and after an answer.
    pub nachfrage_s: f32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Weather {
    /// Home place, resolved on every request. Empty until set by hand.
    pub heimatort: String,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let config: Self = read_toml(path)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let p = &self.persoenlichkeit;
        if p.sarkasmus > crate::phrases::MAX_LEVEL {
            return Err(ConfigError::Invalid(format!(
                "persoenlichkeit.sarkasmus must be 0..={}",
                crate::phrases::MAX_LEVEL
            )));
        }
        for (name, value) in [
            ("gespraechigkeit", p.gespraechigkeit),
            ("foermlichkeit", p.foermlichkeit),
            ("humor_haeufigkeit", p.humor_haeufigkeit),
        ] {
            if value > 10 {
                return Err(ConfigError::Invalid(format!(
                    "persoenlichkeit.{name} must be 0..=10"
                )));
            }
        }
        if !(0.0..=1.0).contains(&p.anrede_boss_anteil) {
            return Err(ConfigError::Invalid(
                "persoenlichkeit.anrede_boss_anteil must be 0.0..=1.0".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.router.konfidenz_schwelle) {
            return Err(ConfigError::Invalid(
                "router.konfidenz_schwelle must be 0.0..=1.0".into(),
            ));
        }
        let url = &self.modelle.ollama_url;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(ConfigError::Invalid(
                "modelle.ollama_url must start with http:// or https://".into(),
            ));
        }
        if self.lernen.min_wiederholungen == 0 {
            return Err(ConfigError::Invalid(
                "lernen.min_wiederholungen must be at least 1".into(),
            ));
        }
        if !(0.0..=1.0).contains(&self.wakeword.schwelle) {
            return Err(ConfigError::Invalid(
                "wakeword.schwelle must be 0.0..=1.0".into(),
            ));
        }
        for (name, value) in [
            ("modell", &self.wakeword.modell),
            ("eingabe", &self.audio.eingabe),
            ("ausgabe", &self.audio.ausgabe),
        ] {
            if value.is_empty() {
                return Err(ConfigError::Invalid(format!("{name} must not be empty")));
            }
        }
        if self.dialog.nachfrage_s.is_nan() || self.dialog.nachfrage_s <= 0.0 {
            return Err(ConfigError::Invalid(
                "dialog.nachfrage_s must be greater than 0".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_config_is_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/nova.toml");
        let config = Config::load(&path).expect("config/nova.toml must load");
        assert_eq!(config.nova.aktivierungswort, "hey nova");
        assert_eq!(config.wetter.heimatort, "");
    }

    fn shipped_text() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/nova.toml");
        std::fs::read_to_string(path).unwrap()
    }

    fn assert_invalid(from: &str, to: &str) {
        let text = shipped_text();
        assert!(text.contains(from), "shipped config lacks `{from}`");
        let config: Config = toml::from_str(&text.replace(from, to)).unwrap();
        let result = config.validate();
        assert!(matches!(result, Err(ConfigError::Invalid(_))), "{result:?}");
    }

    #[test]
    fn rejects_threshold_above_one() {
        assert_invalid("schwelle = 0.5 ", "schwelle = 1.5 ");
    }

    #[test]
    fn rejects_zero_follow_up() {
        assert_invalid("nachfrage_s = 8.0", "nachfrage_s = 0.0");
    }

    #[test]
    fn rejects_ollama_url_without_scheme() {
        assert_invalid(
            "ollama_url = \"http://127.0.0.1:11434\"",
            "ollama_url = \"127.0.0.1:11434\"",
        );
    }

    #[test]
    fn rejects_empty_audio_device() {
        assert_invalid("eingabe = \"nova_mic\"", "eingabe = \"\"");
    }

    #[test]
    fn model_with_slash_is_a_path() {
        let wake = WakeWord {
            modell: "models/x.onnx".into(),
            schwelle: 0.5,
        };
        assert_eq!(wake.model_arg(Path::new("/r")), "/r/models/x.onnx");
    }

    #[test]
    fn bare_model_name_stays_a_name() {
        let wake = WakeWord {
            modell: "hey_jarvis".into(),
            schwelle: 0.5,
        };
        assert_eq!(wake.model_arg(Path::new("/r")), "hey_jarvis");
    }
}
