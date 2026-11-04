//! `config/phrases.toml`: the phrase library (Floskel-Bibliothek).
//!
//! Placeholders: `{anrede}` is filled with "Boss" or "Chef" by the script,
//! `{pfad}` with the concrete path. Serious-mode phrases never depend on the sarcasm level.

use std::path::Path;

use serde::Deserialize;

use crate::config::{ConfigError, read_toml};

/// Highest sarcasm level (0 = neutral, 1 = medium, 2 = high).
pub const MAX_LEVEL: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Frage,
    Aufgabe,
    Recherche,
    Code,
    LangeWartezeit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SeriousCase {
    Fehler,
    Warnung,
    Loeschbestaetigung,
    Limit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhraseSet {
    pub typ: RequestKind,
    pub stufe: u8,
    pub varianten: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeriousSet {
    pub fall: SeriousCase,
    pub varianten: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phrases {
    #[serde(default)]
    pub floskel: Vec<PhraseSet>,
    #[serde(default)]
    pub ernst: Vec<SeriousSet>,
}

impl Phrases {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let phrases: Self = read_toml(path)?;
        phrases.validate()?;
        Ok(phrases)
    }

    /// Variants for a request kind at a sarcasm level, falling back to the next lower level.
    pub fn variants(&self, kind: RequestKind, level: u8) -> &[String] {
        (0..=level)
            .rev()
            .find_map(|l| self.floskel.iter().find(|s| s.typ == kind && s.stufe == l))
            .map_or(&[], |s| s.varianten.as_slice())
    }

    /// Serious-mode variants for a case.
    pub fn serious(&self, case: SeriousCase) -> &[String] {
        self.ernst
            .iter()
            .find(|s| s.fall == case)
            .map_or(&[], |s| s.varianten.as_slice())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        for set in &self.floskel {
            if set.stufe > MAX_LEVEL {
                return Err(ConfigError::Invalid(format!(
                    "floskel {:?}: stufe must be 0..={MAX_LEVEL}",
                    set.typ
                )));
            }
            if set.varianten.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "floskel {:?} stufe {}: no variants",
                    set.typ, set.stufe
                )));
            }
        }
        // Delete confirmations must always name the concrete path.
        for v in self.serious(SeriousCase::Loeschbestaetigung) {
            if !v.contains("{pfad}") {
                return Err(ConfigError::Invalid(format!(
                    "loeschbestaetigung without {{pfad}}: {v:?}"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped() -> Phrases {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/phrases.toml");
        Phrases::load(&path).expect("config/phrases.toml must load")
    }

    #[test]
    fn every_kind_has_neutral_variants() {
        let p = shipped();
        for kind in [
            RequestKind::Frage,
            RequestKind::Aufgabe,
            RequestKind::Recherche,
            RequestKind::Code,
            RequestKind::LangeWartezeit,
        ] {
            assert!(!p.variants(kind, 0).is_empty(), "{kind:?} lacks level 0");
        }
    }

    #[test]
    fn missing_level_falls_back_to_lower() {
        let p = shipped();
        assert_eq!(
            p.variants(RequestKind::Frage, 2),
            p.variants(RequestKind::Frage, 1)
        );
    }

    #[test]
    fn every_serious_case_is_covered() {
        let p = shipped();
        for case in [
            SeriousCase::Fehler,
            SeriousCase::Warnung,
            SeriousCase::Loeschbestaetigung,
            SeriousCase::Limit,
        ] {
            assert!(!p.serious(case).is_empty(), "{case:?} missing");
        }
    }
}
