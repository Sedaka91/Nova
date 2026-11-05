//! Browser search through `xdg-open`.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::commands::{Engine, Reply};
use crate::tools;

/// Search URL for `term` on `engine`, percent-encoded.
pub fn url(engine: Engine, term: &str) -> String {
    let encoded = utf8_percent_encode(term, NON_ALPHANUMERIC);
    match engine {
        Engine::Google => format!("https://www.google.com/search?q={encoded}"),
        Engine::YouTube => format!("https://www.youtube.com/results?search_query={encoded}"),
    }
}

/// Open the search in the default browser.
pub fn open(engine: Engine, term: &str) -> Reply {
    match tools::launch("xdg-open", &[&url(engine, term)]) {
        Ok(()) => Reply::Say(format!("Ich suche nach {term}.")),
        Err(err) => {
            tracing::warn!(?err, "xdg-open failed");
            Reply::Say("Das konnte ich nicht öffnen.".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Engine::{Google, YouTube};

    #[test]
    fn builds_encoded_urls() {
        assert_eq!(
            url(Google, "größe & c#"),
            "https://www.google.com/search?q=gr%C3%B6%C3%9Fe%20%26%20c%23"
        );
        assert_eq!(
            url(YouTube, "katzen"),
            "https://www.youtube.com/results?search_query=katzen"
        );
    }
}
