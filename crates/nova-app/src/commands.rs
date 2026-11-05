//! Fixed commands: recognition of German speech and their execution.

use nova_core::apps::Apps;

use crate::{launcher, music, search, spotify, weather};

/// Day a weather report refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Day {
    Today,
    Tomorrow,
}

/// Search engine for spoken searches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    Google,
    YouTube,
}

/// What kind of Spotify content was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentKind {
    Any,
    Playlist,
    Album,
    Track,
}

/// A fixed command that needs no model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Dismiss,
    Play,
    Pause,
    Next,
    Previous,
    VolumeStep(i8),
    VolumeSet(u8),
    Weather { place: Option<String>, day: Day },
    Search(Engine, String),
    OpenApp(String),
    PlayContent { kind: ContentKind, query: String },
}

/// What Nova says after a command ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Say(String),
    Silent,
}

/// Everything a command needs to run.
pub struct Context {
    /// Home place from `nova.toml`, used when no place was spoken.
    pub home: String,
    pub apps: Apps,
}

impl Context {
    /// Runs a command; failures come back as spoken sentences.
    pub fn execute(&self, command: Command) -> Reply {
        match command {
            Command::Dismiss => Reply::Silent,
            Command::Play
            | Command::Pause
            | Command::Next
            | Command::Previous
            | Command::VolumeStep(_)
            | Command::VolumeSet(_) => music::control(&command),
            Command::Weather { place, day } => weather::report(place.as_deref(), day, &self.home),
            Command::Search(engine, term) => search::open(engine, &term),
            Command::OpenApp(name) => launcher::open(&self.apps, &name),
            Command::PlayContent { kind, query } => spotify::play(kind, &query),
        }
    }
}

/// Lowercase, strip punctuation, collapse whitespace, drop wake and politeness words.
fn normalize(text: &str) -> String {
    let lowered = text.to_lowercase();
    let cleaned: String = lowered
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '.' | ',' | '!' | '?' | ';' | ':' | '„' | '“' | '"' | '\''
            )
        })
        .collect();
    let mut s = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    for prefix in ["hey nova", "nova", "bitte"] {
        s = strip_word_prefix(&s, prefix).unwrap_or(&s).to_string();
    }
    s = strip_word_suffix(&s, "bitte").unwrap_or(&s).to_string();
    s
}

/// Strips `word` from the front when it is a whole word.
fn strip_word_prefix<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_prefix(' ')
    }
}

/// Strips `word` from the end when it is a whole word.
fn strip_word_suffix<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let rest = s.strip_suffix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else {
        rest.strip_suffix(' ')
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn number_word(word: &str) -> Option<u8> {
    Some(match word {
        "null" => 0,
        "eins" => 1,
        "zwei" => 2,
        "drei" => 3,
        "vier" => 4,
        "fünf" => 5,
        "sechs" => 6,
        "sieben" => 7,
        "acht" => 8,
        "neun" => 9,
        "zehn" => 10,
        "zwanzig" => 20,
        "dreißig" | "dreissig" => 30,
        "vierzig" => 40,
        "fünfzig" => 50,
        "sechzig" => 60,
        "siebzig" => 70,
        "achtzig" => 80,
        "neunzig" => 90,
        "hundert" => 100,
        _ => return None,
    })
}

fn parse_volume(s: &str) -> Option<Command> {
    let rest = s.strip_prefix("lautstärke auf ")?;
    let rest = strip_word_suffix(rest, "prozent")
        .or_else(|| rest.strip_suffix('%').map(str::trim_end))
        .unwrap_or(rest);
    let n = number_word(rest).or_else(|| rest.parse::<u8>().ok())?;
    (n <= 100).then_some(Command::VolumeSet(n))
}

fn parse_weather(s: &str) -> Option<Command> {
    const STARTS: [&str; 4] = [
        "wie wird das wetter",
        "wie ist das wetter",
        "wetterbericht",
        "wetter",
    ];
    if !STARTS.iter().any(|p| s.starts_with(p)) {
        return None;
    }
    let words: Vec<&str> = s.split(' ').collect();
    let day = if words.contains(&"morgen") {
        Day::Tomorrow
    } else {
        Day::Today
    };
    let place = words
        .iter()
        .position(|w| matches!(*w, "in" | "für"))
        .and_then(|i| {
            let rest: Vec<&str> = words[i + 1..]
                .iter()
                .copied()
                .filter(|w| !matches!(*w, "heute" | "morgen"))
                .collect();
            non_empty(&rest.join(" "))
        });
    Some(Command::Weather { place, day })
}

fn parse_search(s: &str) -> Option<Command> {
    for p in [
        "such bei youtube",
        "such auf youtube",
        "suche bei youtube",
        "suche auf youtube",
    ] {
        if let Some(rest) = strip_word_prefix(s, p) {
            let rest = strip_word_prefix(rest, "nach").unwrap_or(rest);
            return non_empty(rest).map(|q| Command::Search(Engine::YouTube, q));
        }
    }
    for p in ["google", "such", "suche"] {
        if let Some(rest) = strip_word_prefix(s, p) {
            // "google X" needs no "nach"; "such X" is no search without it.
            let after_nach = strip_word_prefix(rest, "nach");
            let query = match (p, after_nach) {
                (_, Some(q)) => q,
                ("google", None) => rest,
                _ => continue,
            };
            return non_empty(query).map(|q| Command::Search(Engine::Google, q));
        }
    }
    None
}

fn strip_article(s: &str) -> &str {
    ["den", "die", "das"]
        .iter()
        .find_map(|a| strip_word_prefix(s, a))
        .unwrap_or(s)
}

fn parse_open(s: &str) -> Option<Command> {
    let app = if let Some(rest) = strip_word_prefix(s, "öffne").or(strip_word_prefix(s, "starte"))
    {
        rest
    } else if let Some(rest) = strip_word_prefix(s, "mach") {
        strip_word_suffix(rest, "auf")?
    } else {
        strip_word_suffix(s, "öffnen").or(strip_word_suffix(s, "starten"))?
    };
    non_empty(strip_article(app)).map(Command::OpenApp)
}

fn parse_play_content(s: &str) -> Option<Command> {
    let rest = strip_word_prefix(s, "spiel").or(strip_word_prefix(s, "spiele"))?;
    let rest = strip_word_suffix(rest, "auf spotify").unwrap_or(rest);
    let kinds: [(&[&str], ContentKind); 3] = [
        (
            &["die playlist", "meine playlist", "playlist"],
            ContentKind::Playlist,
        ),
        (&["das album", "album"], ContentKind::Album),
        (&["das lied", "den song", "den titel"], ContentKind::Track),
    ];
    for (prefixes, kind) in kinds {
        for p in prefixes {
            if let Some(q) = strip_word_prefix(rest, p) {
                return non_empty(q).map(|query| Command::PlayContent { kind, query });
            }
        }
    }
    non_empty(rest).map(|query| Command::PlayContent {
        kind: ContentKind::Any,
        query,
    })
}

/// Turns recognized speech into a fixed command, if it matches one.
pub fn parse(text: &str) -> Option<Command> {
    let s = normalize(text);
    let s = s.as_str();
    match s {
        "stopp" | "stop" => return Some(Command::Dismiss),
        "play" | "spiel weiter" | "spiele weiter" | "musik an" | "musik weiter" | "fortsetzen" => {
            return Some(Command::Play);
        }
        "pause" | "musik aus" | "musik stopp" | "musik pausieren" => return Some(Command::Pause),
        "weiter" | "nächstes lied" | "nächster titel" | "nächster song" | "überspringen" => {
            return Some(Command::Next);
        }
        "zurück" | "vorheriges lied" | "vorheriger titel" | "letztes lied" => {
            return Some(Command::Previous);
        }
        "lauter" => return Some(Command::VolumeStep(10)),
        "leiser" => return Some(Command::VolumeStep(-10)),
        _ => {}
    }
    parse_volume(s)
        .or_else(|| parse_weather(s))
        .or_else(|| parse_search(s))
        .or_else(|| parse_open(s))
        .or_else(|| parse_play_content(s))
}

#[cfg(test)]
mod tests {
    use super::Command::*;
    use super::*;

    fn weather(place: Option<&str>, day: Day) -> Command {
        Weather {
            place: place.map(String::from),
            day,
        }
    }

    fn content(kind: ContentKind, query: &str) -> Command {
        PlayContent {
            kind,
            query: query.into(),
        }
    }

    #[test]
    fn parse_recognizes_commands() {
        let table = vec![
            ("Stopp.", Dismiss),
            ("Pause!", Pause),
            ("Musik aus.", Pause),
            ("Spiel weiter.", Play),
            ("Spiele weiter.", Play),
            ("Nova, bitte stopp.", Dismiss),
            ("Weiter.", Next),
            ("Nächstes Lied.", Next),
            ("Zurück.", Previous),
            ("Lauter.", VolumeStep(10)),
            ("Leiser, bitte.", VolumeStep(-10)),
            ("Lautstärke auf 30.", VolumeSet(30)),
            ("Lautstärke auf 30 Prozent.", VolumeSet(30)),
            ("Lautstärke auf 30%.", VolumeSet(30)),
            ("Lautstärke auf 30 %.", VolumeSet(30)),
            ("Lautstärke auf null.", VolumeSet(0)),
            ("Lautstärke auf fünf.", VolumeSet(5)),
            ("Lautstärke auf dreißig.", VolumeSet(30)),
            ("Lautstärke auf hundert.", VolumeSet(100)),
            ("Hey Nova, wie wird das Wetter?", weather(None, Day::Today)),
            (
                "Wie wird das Wetter morgen in Bad Homburg?",
                weather(Some("bad homburg"), Day::Tomorrow),
            ),
            ("Wetter für Köln.", weather(Some("köln"), Day::Today)),
            (
                "Such bei YouTube nach Katzen.",
                Search(Engine::YouTube, "katzen".into()),
            ),
            (
                "Suche bei YouTube nach Katzen.",
                Search(Engine::YouTube, "katzen".into()),
            ),
            (
                "Suche auf YouTube Katzen.",
                Search(Engine::YouTube, "katzen".into()),
            ),
            (
                "Such auf YouTube nach Katzen.",
                Search(Engine::YouTube, "katzen".into()),
            ),
            (
                "Google Rust Lifetimes.",
                Search(Engine::Google, "rust lifetimes".into()),
            ),
            (
                "Such nach Größe von Äpfeln.",
                Search(Engine::Google, "größe von äpfeln".into()),
            ),
            ("Öffne den Firefox.", OpenApp("firefox".into())),
            ("Mach Dolphin auf.", OpenApp("dolphin".into())),
            ("Steam starten.", OpenApp("steam".into())),
            ("Spiel Daft Punk.", content(ContentKind::Any, "daft punk")),
            (
                "Spiele die Playlist Sport auf Spotify.",
                content(ContentKind::Playlist, "sport"),
            ),
            (
                "Spiel das Album Discovery.",
                content(ContentKind::Album, "discovery"),
            ),
            (
                "Spiel den Song One More Time.",
                content(ContentKind::Track, "one more time"),
            ),
        ];
        for (text, expected) in table {
            assert_eq!(parse(text), Some(expected), "input: {text}");
        }
    }

    #[test]
    fn parse_returns_none() {
        for text in [
            "Warum ist das Wetter so schlecht?",
            "Such nach.",
            "Such bei YouTube.",
            "Mach auf.",
            "Spiel.",
            "Lautstärke auf 130.",
            "Wie spät ist es?",
            "",
        ] {
            assert_eq!(parse(text), None, "input: {text}");
        }
    }

    fn context(home: &str) -> Context {
        Context {
            home: home.into(),
            apps: Apps::from_map(std::collections::BTreeMap::new()).unwrap(),
        }
    }

    #[test]
    fn execute_dismiss_is_silent() {
        assert_eq!(context("").execute(Dismiss), Reply::Silent);
    }

    #[test]
    fn execute_unlisted_app_is_refused() {
        assert_eq!(
            context("").execute(OpenApp("notepad".into())),
            Reply::Say("notepad ist nicht freigegeben.".into())
        );
    }

    #[test]
    fn execute_weather_without_home_asks_for_it() {
        assert_eq!(
            context("").execute(weather(None, Day::Today)),
            Reply::Say("Dein Heimatort fehlt in den Einstellungen.".into())
        );
    }
}
