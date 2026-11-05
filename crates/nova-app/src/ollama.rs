//! Local chat model (stage 2) through Ollama's HTTP API.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools;

/// Below the 20 s work deadline of the voice loop, so a stalled Ollama gets its own sentence.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(15);
/// The model stays in VRAM this long after the last request.
const KEEP_ALIVE: &str = "30m";
/// Small context keeps the KV cache, the hidden VRAM cost, low.
const NUM_CTX: u32 = 2048;
/// Spoken answers stay short.
const NUM_PREDICT: u32 = 120;
const SYSTEM_PROMPT: &str = "Du bist Nova, ein Sprachassistent. Antworte immer auf Deutsch, \
    in höchstens zwei kurzen Sätzen, ohne Aufzählungen und ohne Markdown.";

/// Why the local model did not answer; each variant has its own spoken sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OllamaError {
    Unreachable,
    ModelMissing,
    Timeout,
    Other,
}

impl OllamaError {
    pub fn sentence(self) -> &'static str {
        match self {
            Self::Unreachable => "Das Sprachmodell ist gerade nicht erreichbar.",
            Self::ModelMissing => "Das Sprachmodell ist nicht installiert.",
            Self::Timeout => "Das Sprachmodell hat zu lange gebraucht.",
            Self::Other => "Das Sprachmodell hat einen Fehler gemeldet.",
        }
    }
}

/// Request body for `POST /api/chat`: one stateless turn with the system prompt.
pub fn chat_body(model: &str, text: &str) -> Value {
    json!({
        "model": model,
        "stream": false,
        "keep_alive": KEEP_ALIVE,
        "options": { "num_ctx": NUM_CTX, "num_predict": NUM_PREDICT },
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": text },
        ],
    })
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

/// The answer text, without Markdown characters since it goes to the speech output.
pub fn parse_chat(json: &str) -> Result<String, serde_json::Error> {
    let response: ChatResponse = serde_json::from_str(json)?;
    Ok(strip_markdown(&response.message.content))
}

fn strip_markdown(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '*' | '#' | '`'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Maps a transport failure to the sentence that describes it.
fn classify_transport(err: &ureq::Error) -> OllamaError {
    tracing::warn!(%err, "Ollama request failed");
    match err {
        ureq::Error::Timeout(_) => OllamaError::Timeout,
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound => OllamaError::Unreachable,
        ureq::Error::Io(io) if is_connection_error(io.kind()) => OllamaError::Unreachable,
        _ => OllamaError::Other,
    }
}

/// A refused or dropped connection, as opposed to a bad answer on an open one.
fn is_connection_error(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::{ConnectionAborted, ConnectionRefused, ConnectionReset, NotConnected};
    matches!(
        kind,
        ConnectionRefused | ConnectionReset | ConnectionAborted | NotConnected
    )
}

/// Asks the local model `text`; `base_url` is `models.ollama_url`.
pub fn chat(base_url: &str, model: &str, text: &str) -> Result<String, OllamaError> {
    chat_with_timeout(base_url, model, text, CHAT_TIMEOUT)
}

fn chat_with_timeout(
    base_url: &str,
    model: &str,
    text: &str,
    timeout: Duration,
) -> Result<String, OllamaError> {
    let url = format!("{}/api/chat", base_url.trim_end_matches('/'));
    let body_text =
        serde_json::to_string(&chat_body(model, text)).map_err(|_| OllamaError::Other)?;
    let mut response = tools::agent_with_timeout(timeout)
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body_text)
        .map_err(|err| classify_transport(&err))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|err| classify_transport(&err))?;
    match status {
        200 => parse_chat(&body).map_err(|_| {
            // A serde error may quote the body, so it stays out of the log.
            tracing::warn!("Ollama answer is not a chat response");
            OllamaError::Other
        }),
        404 if body.contains("not found") => Err(OllamaError::ModelMissing),
        _ => {
            tracing::warn!(status, "Ollama returned an error");
            Err(OllamaError::Other)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Serves one connection with `status` and `body`, then closes.
    fn serve_once(status: &'static str, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        });
        url
    }

    #[test]
    fn body_has_one_system_and_one_user_turn() {
        let body = chat_body("qwen2.5:3b", "Wie spät ist es?");
        assert_eq!(body["model"], "qwen2.5:3b");
        assert_eq!(body["stream"], false);
        assert_eq!(body["keep_alive"], "30m");
        assert_eq!(body["options"]["num_ctx"], 2048);
        assert_eq!(body["options"]["num_predict"], 120);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "Wie spät ist es?");
    }

    #[test]
    fn parse_takes_the_message_and_strips_markdown() {
        let json = r#"{"message":{"role":"assistant","content":" **Es ist** `Mittag`. "}}"#;
        assert_eq!(parse_chat(json).unwrap(), "Es ist Mittag.");
    }

    #[test]
    fn parse_rejects_other_json() {
        assert!(parse_chat(r#"{"error":"boom"}"#).is_err());
    }

    #[test]
    fn answer_from_a_running_server() {
        let url = serve_once(
            "200 OK",
            r#"{"message":{"role":"assistant","content":"Hallo."}}"#,
        );
        assert_eq!(chat(&url, "m", "Hallo").unwrap(), "Hallo.");
    }

    #[test]
    fn unknown_model_is_reported_as_missing() {
        let url = serve_once("404 Not Found", r#"{"error":"model \"m\" not found"}"#);
        assert_eq!(chat(&url, "m", "x"), Err(OllamaError::ModelMissing));
    }

    #[test]
    fn other_status_is_a_generic_error() {
        let url = serve_once("500 Internal Server Error", r#"{"error":"oops"}"#);
        assert_eq!(chat(&url, "m", "x"), Err(OllamaError::Other));
    }

    #[test]
    fn closed_port_is_unreachable() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}");
        assert_eq!(chat(&url, "m", "x"), Err(OllamaError::Unreachable));
    }

    #[test]
    fn stalled_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_secs(3));
        });
        let result = chat_with_timeout(&url, "m", "x", Duration::from_millis(300));
        assert_eq!(result, Err(OllamaError::Timeout));
    }

    #[test]
    fn every_error_has_a_german_sentence() {
        for error in [
            OllamaError::Unreachable,
            OllamaError::ModelMissing,
            OllamaError::Timeout,
            OllamaError::Other,
        ] {
            assert!(error.sentence().ends_with('.'));
        }
    }
}
