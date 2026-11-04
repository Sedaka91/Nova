//! JSON Lines protocol between Nova and its helper processes.

use serde::{Deserialize, Serialize};

/// The helper processes Nova supervises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperKind {
    Wakeword,
    Stt,
    Tts,
}

/// Commands sent to the wake word helper (stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum WakewordCommand {
    Pause,
    Resume,
}

/// Commands sent to the speech recognition helper (stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum SttCommand {
    Listen,
    Cancel,
}

/// Commands sent to the speech output helper (stdin).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum TtsCommand {
    Speak { id: String, text: String },
    Stop,
}

/// Events emitted by any helper (stdout).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HelperEvent {
    Ready,
    Error { message: String },
    Wake { score: f32 },
    SpeechStart,
    Partial { text: String },
    Final { text: String },
    Started { id: String },
    Done { id: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speak_serializes_like_the_protocol() {
        let cmd = TtsCommand::Speak {
            id: "1".into(),
            text: "Hallo".into(),
        };
        assert_eq!(
            serde_json::to_string(&cmd).unwrap(),
            r#"{"cmd":"speak","id":"1","text":"Hallo"}"#
        );
    }

    #[test]
    fn simple_commands_serialize_like_the_protocol() {
        let s = |v: &dyn erased::Ser| v.json();
        assert_eq!(s(&WakewordCommand::Pause), r#"{"cmd":"pause"}"#);
        assert_eq!(s(&WakewordCommand::Resume), r#"{"cmd":"resume"}"#);
        assert_eq!(s(&SttCommand::Listen), r#"{"cmd":"listen"}"#);
        assert_eq!(s(&SttCommand::Cancel), r#"{"cmd":"cancel"}"#);
        assert_eq!(s(&TtsCommand::Stop), r#"{"cmd":"stop"}"#);
    }

    #[test]
    fn parses_every_event() {
        let cases = [
            (r#"{"event":"ready"}"#, HelperEvent::Ready),
            (
                r#"{"event":"error","message":"kaputt"}"#,
                HelperEvent::Error {
                    message: "kaputt".into(),
                },
            ),
            (
                r#"{"event":"wake","score":0.5}"#,
                HelperEvent::Wake { score: 0.5 },
            ),
            (r#"{"event":"speech_start"}"#, HelperEvent::SpeechStart),
            (
                r#"{"event":"partial","text":"hal"}"#,
                HelperEvent::Partial { text: "hal".into() },
            ),
            (
                r#"{"event":"final","text":"hallo"}"#,
                HelperEvent::Final {
                    text: "hallo".into(),
                },
            ),
            (
                r#"{"event":"started","id":"1"}"#,
                HelperEvent::Started { id: "1".into() },
            ),
            (
                r#"{"event":"done","id":"1"}"#,
                HelperEvent::Done { id: "1".into() },
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(serde_json::from_str::<HelperEvent>(line).unwrap(), expected);
        }
    }

    #[test]
    fn unknown_event_is_an_error() {
        assert!(serde_json::from_str::<HelperEvent>(r#"{"event":"bogus"}"#).is_err());
    }

    mod erased {
        pub trait Ser {
            fn json(&self) -> String;
        }
        impl<T: serde::Serialize> Ser for T {
            fn json(&self) -> String {
                serde_json::to_string(self).unwrap()
            }
        }
    }
}
