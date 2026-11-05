//! Starting allow-listed programs through `gtk-launch`.

use nova_core::apps::Apps;

use crate::commands::Reply;
use crate::tools;

/// Desktop ID for a spoken name; Whisper's hyphens count as spaces. Whole names only.
pub fn lookup<'a>(apps: &'a Apps, name: &str) -> Option<&'a str> {
    apps.desktop_id(&name.replace('-', " "))
}

/// Start the program if it is listed in `apps.toml`.
pub fn open(apps: &Apps, name: &str) -> Reply {
    let Some(id) = lookup(apps, name) else {
        return Reply::Say(format!("{name} ist nicht freigegeben."));
    };
    match tools::launch("gtk-launch", &[id]) {
        Ok(()) => Reply::Say(format!("{name} wird geöffnet.")),
        Err(err) => {
            tracing::warn!(?err, id, "gtk-launch failed");
            Reply::Say("Das konnte ich nicht öffnen.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Reply;
    use nova_core::apps::Apps;
    use std::collections::BTreeMap;

    fn apps() -> Apps {
        let map = BTreeMap::from([
            ("firefox".to_string(), "firefox".to_string()),
            ("vs code".to_string(), "com.microsoft.VSCode".to_string()),
        ]);
        Apps::from_map(map).unwrap()
    }

    #[test]
    fn lookup_matches_whole_names_only() {
        let apps = apps();
        assert_eq!(lookup(&apps, "firefox"), Some("firefox"));
        assert_eq!(lookup(&apps, "vs-code"), Some("com.microsoft.VSCode"));
        assert_eq!(lookup(&apps, "fire"), None);
        assert_eq!(lookup(&apps, "notepad"), None);
    }

    #[test]
    fn unlisted_program_is_rejected() {
        assert_eq!(
            open(&apps(), "notepad"),
            Reply::Say("notepad ist nicht freigegeben.".into())
        );
    }
}
