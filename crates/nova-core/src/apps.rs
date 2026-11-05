//! `config/apps.toml`: the programs Nova may start, by spoken name.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::config::{ConfigError, read_toml};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppsFile {
    apps: BTreeMap<String, String>,
}

/// Allowed programs: spoken name (lowercase) to `.desktop` ID.
#[derive(Debug)]
pub struct Apps {
    map: BTreeMap<String, String>,
}

impl Apps {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let file: AppsFile = read_toml(path)?;
        Self::from_map(file.apps)
    }

    pub fn from_map(map: BTreeMap<String, String>) -> Result<Self, ConfigError> {
        for (name, id) in &map {
            if name.is_empty() || name.trim() != name || name.to_lowercase() != *name {
                return Err(ConfigError::Invalid(format!(
                    "apps key `{name}` must be non-empty, lowercase and without surrounding spaces"
                )));
            }
            // A leading `-` would reach `gtk-launch` as an option.
            if id.is_empty() || id.contains('/') || id.starts_with('-') {
                return Err(ConfigError::Invalid(format!(
                    "apps value for `{name}` must be a desktop ID (non-empty, no `/`, no leading `-`)"
                )));
            }
        }
        Ok(Self { map })
    }

    /// The desktop ID for an exact spoken name.
    pub fn desktop_id(&self, name: &str) -> Option<&str> {
        self.map.get(name).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_apps_are_valid() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/apps.toml");
        let apps = Apps::load(&path).expect("config/apps.toml must load");
        assert_eq!(apps.desktop_id("vs code"), Some("com.microsoft.VSCode"));
        assert_eq!(apps.desktop_id("firefox"), Some("firefox"));
        assert_eq!(apps.desktop_id("nope"), None);
    }

    #[test]
    fn rejects_bad_entries() {
        for (key, value) in [
            ("Firefox", "firefox"),
            (" firefox", "firefox"),
            ("", "firefox"),
            ("firefox", ""),
            ("firefox", "a/b"),
            ("firefox", "-x"),
        ] {
            let map = BTreeMap::from([(key.to_owned(), value.to_owned())]);
            let result = Apps::from_map(map);
            assert!(
                matches!(result, Err(ConfigError::Invalid(_))),
                "{key:?} = {value:?}: {result:?}"
            );
        }
    }
}
