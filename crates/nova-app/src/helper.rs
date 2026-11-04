//! One supervised helper child process: JSON Lines over stdin/stdout.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::protocol::{HelperEvent, HelperKind};

/// How long `stop` waits for a helper to exit after closing its stdin.
const STOP_GRACE: Duration = Duration::from_secs(2);
const STOP_POLL: Duration = Duration::from_millis(20);

/// How to start one helper process.
#[derive(Debug, Clone)]
pub struct HelperSpec {
    pub kind: HelperKind,
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Messages delivered to the supervisor.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Event(HelperKind, HelperEvent),
    Exited(HelperKind),
    /// Shutdown request (e.g. Ctrl+C); never produced by `Helper` itself.
    Shutdown,
}

/// Reads one line, replacing invalid UTF-8; `None` on EOF or a hard I/O error.
fn read_line_lossy(reader: &mut impl BufRead) -> Option<String> {
    let mut buf = Vec::new();
    match reader.read_until(b'\n', &mut buf) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(String::from_utf8_lossy(&buf).trim_end().to_owned()),
    }
}

/// A running helper child process.
#[derive(Debug)]
pub struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Helper {
    /// Starts the helper and spawns reader threads for stdout and stderr.
    ///
    /// # Errors
    /// Returns an error if the process cannot be started.
    pub fn spawn(spec: &HelperSpec, tx: Sender<Incoming>) -> io::Result<Self> {
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let kind = spec.kind;

        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Some(line) = read_line_lossy(&mut reader) {
                match serde_json::from_str::<HelperEvent>(&line) {
                    Ok(event) => {
                        if tx.send(Incoming::Event(kind, event)).is_err() {
                            return;
                        }
                    }
                    Err(err) => tracing::warn!(?kind, %err, line, "unparsable helper line"),
                }
            }
            // A closed receiver just means nobody is listening anymore.
            let _ = tx.send(Incoming::Exited(kind));
        });
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Some(line) = read_line_lossy(&mut reader) {
                tracing::info!(?kind, "{line}");
            }
        });

        Ok(Self { child, stdin })
    }

    /// Sends one command as a JSON line.
    ///
    /// # Errors
    /// Returns an error if the helper is stopped or its stdin is broken.
    pub fn send(&mut self, command: &impl Serialize) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "helper stdin is closed"))?;
        let mut line = serde_json::to_string(command).map_err(io::Error::other)?;
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// Closes stdin, waits up to 2 s for exit, then kills. Idempotent.
    pub fn stop(&mut self) {
        self.stdin = None;
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(STOP_POLL);
        }
        // Errors mean the process is already gone; wait() reaps it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SttCommand;
    use std::sync::mpsc::{self, Receiver};

    const WAIT: Duration = Duration::from_secs(5);

    fn fixture() -> String {
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_helper.py").to_owned()
    }

    fn start(extra: &[&str]) -> (Helper, Receiver<Incoming>) {
        let (tx, rx) = mpsc::channel();
        let mut args = vec![fixture()];
        args.extend(extra.iter().map(|s| (*s).to_owned()));
        let spec = HelperSpec {
            kind: HelperKind::Stt,
            program: "python3".into(),
            args,
        };
        (Helper::spawn(&spec, tx).expect("spawn fake helper"), rx)
    }

    #[test]
    fn receives_ready_event() {
        let (_helper, rx) = start(&[]);
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Event(HelperKind::Stt, HelperEvent::Ready)
        );
    }

    #[test]
    fn skips_garbage_lines() {
        let (_helper, rx) = start(&["--noise"]);
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Event(HelperKind::Stt, HelperEvent::Ready)
        );
    }

    #[test]
    fn reports_exit() {
        let (mut helper, rx) = start(&["--crash-after", "1"]);
        rx.recv_timeout(WAIT).unwrap(); // Ready
        helper.send(&SttCommand::Listen).unwrap();
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Exited(HelperKind::Stt)
        );
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn stop_ends_process_on_closed_stdin() {
        let (mut helper, rx) = start(&[]);
        rx.recv_timeout(WAIT).unwrap(); // Ready
        helper.stop();
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Exited(HelperKind::Stt)
        );
        assert!(helper.child.try_wait().unwrap().is_some());
        helper.stop(); // idempotent
        assert!(helper.send(&SttCommand::Listen).is_err());
    }

    #[test]
    fn script_answers_commands() {
        let dir = std::env::temp_dir().join(format!("nova-helper-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("script.json");
        std::fs::write(
            &script,
            r#"{"auf_befehl":{"listen":[{"event":"speech_start"},{"event":"final","text":"hallo"}]}}"#,
        )
        .unwrap();
        let (mut helper, rx) = start(&["--script", script.to_str().unwrap()]);
        rx.recv_timeout(WAIT).unwrap(); // Ready
        helper.send(&SttCommand::Listen).unwrap();
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Event(HelperKind::Stt, HelperEvent::SpeechStart)
        );
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Event(
                HelperKind::Stt,
                HelperEvent::Final {
                    text: "hallo".into()
                }
            )
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn survives_invalid_utf8_lines() {
        let (_helper, rx) = start(&["--binary-noise"]);
        // No false Exited: the first message is Ready, and the process lives on.
        assert_eq!(
            rx.recv_timeout(WAIT).unwrap(),
            Incoming::Event(HelperKind::Stt, HelperEvent::Ready)
        );
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }
}
