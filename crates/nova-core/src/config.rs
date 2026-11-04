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
        if self.lernen.min_wiederholungen == 0 {
            return Err(ConfigError::Invalid(
                "lernen.min_wiederholungen must be at least 1".into(),
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
        assert_eq!(config.nova.aktivierungswort, "nova");
    }
}
