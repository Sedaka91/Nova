//! Local chat model (stage 2) through Ollama's HTTP API.

use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools;

/// Below the 20 s work deadline of the voice loop, so a stalled Ollama gets its own sentence.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(15);
/// A cold load of the model may take a while; it runs in the background.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(120);
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
    let body = post(base_url, "/api/chat", &chat_body(model, text), timeout)?;
    parse_chat(&body).map_err(|_| {
        // A serde error may quote the body, so it stays out of the log.
        tracing::warn!("Ollama answer is not a chat response");
        OllamaError::Other
    })
}

/// POST `body` to `path` and return the answer text when Ollama said 200.
fn post(
    base_url: &str,
    path: &str,
    body: &Value,
    timeout: Duration,
) -> Result<String, OllamaError> {
    let url = format!("{}{path}", base_url.trim_end_matches('/'));
    let body_text = serde_json::to_string(body).map_err(|_| OllamaError::Other)?;
    let mut response = tools::agent_with_timeout(timeout)
        .post(&url)
        .header("Content-Type", "application/json")
        .send(body_text)
        .map_err(|err| classify_transport(&err))?;
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| classify_transport(&err))?;
    match status {
        200 => Ok(text),
        404 if text.contains("not found") => Err(OllamaError::ModelMissing),
        _ => {
            tracing::warn!(status, "Ollama returned an error");
            Err(OllamaError::Other)
        }
    }
}

/// Loads `model` into memory without a prompt, so the first question is not slow.
pub fn warm_up(base_url: &str, model: &str) -> Result<(), OllamaError> {
    let body = json!({ "model": model, "keep_alive": KEEP_ALIVE });
    post(base_url, "/api/generate", &body, WARM_UP_TIMEOUT).map(drop)
}

/// A model Ollama currently keeps in memory, with the bytes that sit in VRAM.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Loaded {
    pub name: String,
    pub size: u64,
    pub size_vram: u64,
}

impl Loaded {
    /// Ollama moved part of the model to the CPU, which makes answers slow.
    pub fn partly_on_cpu(&self) -> bool {
        self.size_vram < self.size
    }
}

#[derive(Deserialize)]
struct PsResponse {
    models: Vec<Loaded>,
}

/// Parse `GET /api/ps`: the models currently loaded.
pub fn parse_ps(json: &str) -> Result<Vec<Loaded>, serde_json::Error> {
    serde_json::from_str::<PsResponse>(json).map(|response| response.models)
}

/// Models Ollama currently keeps in memory.
pub fn loaded(base_url: &str) -> Result<Vec<Loaded>, OllamaError> {
    let url = format!("{}/api/ps", base_url.trim_end_matches('/'));
    let mut response = tools::http_agent()
        .get(&url)
        .call()
        .map_err(|err| classify_transport(&err))?;
    if response.status().as_u16() != 200 {
        return Err(OllamaError::Other);
    }
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| classify_transport(&err))?;
    parse_ps(&text).map_err(|_| OllamaError::Other)
}

/// Startup task: load the model and log how much of it sits in VRAM. Never fails Nova.
pub fn warm_up_and_report(base_url: &str, model: &str) {
    if let Err(err) = warm_up(base_url, model) {
        tracing::warn!(
            ?err,
            model,
            "model warm-up failed; answers may be slow or fail"
        );
        return;
    }
    match loaded(base_url) {
        Ok(models) => match models.iter().find(|m| m.name == model) {
            Some(m) if m.partly_on_cpu() => tracing::warn!(
                model,
                size = m.size,
                size_vram = m.size_vram,
                "model is only partly in VRAM"
            ),
            Some(m) => tracing::info!(
                model,
                size = m.size,
                size_vram = m.size_vram,
                "model in VRAM"
            ),
            None => tracing::warn!(model, "model not listed as loaded"),
        },
        Err(err) => tracing::warn!(?err, "could not read the loaded models"),
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
    fn parse_ps_reads_loaded_models() {
        let json = r#"{"models":[{"name":"qwen2.5:3b","model":"qwen2.5:3b","size":2500,
            "size_vram":2500,"context_length":2048,"expires_at":"2026-11-05T12:00:00Z"}]}"#;
        let models = parse_ps(json).unwrap();
        assert_eq!(
            models,
            [Loaded {
                name: "qwen2.5:3b".into(),
                size: 2500,
                size_vram: 2500,
            }]
        );
        assert!(!models[0].partly_on_cpu());
    }

    #[test]
    fn partial_offload_is_detected() {
        let models = parse_ps(r#"{"models":[{"name":"m","size":4000,"size_vram":1500}]}"#).unwrap();
        assert!(models[0].partly_on_cpu());
    }

    #[test]
    fn nothing_loaded_is_an_empty_list() {
        assert_eq!(parse_ps(r#"{"models":[]}"#).unwrap(), []);
    }

    #[test]
    fn warm_up_accepts_a_200() {
        let url = serve_once("200 OK", r#"{"done_reason":"load"}"#);
        assert_eq!(warm_up(&url, "m"), Ok(()));
    }

    #[test]
    fn warm_up_reports_a_missing_model() {
        let url = serve_once("404 Not Found", r#"{"error":"model \"m\" not found"}"#);
        assert_eq!(warm_up(&url, "m"), Err(OllamaError::ModelMissing));
    }

    #[test]
    fn loaded_reads_the_ps_endpoint() {
        let url = serve_once(
            "200 OK",
            r#"{"models":[{"name":"m","size":10,"size_vram":10}]}"#,
        );
        assert_eq!(loaded(&url).unwrap()[0].name, "m");
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
