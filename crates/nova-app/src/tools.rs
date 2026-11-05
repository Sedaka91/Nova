//! Shared helpers for child processes and HTTP.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Why a child process did not succeed.
#[derive(Debug)]
pub enum ToolError {
    /// The program is not installed.
    Missing,
    /// The program ran and exited non-zero; carries its stderr.
    Failed(String),
}

/// Run `program` with `args` (no shell), wait for it and check its exit status.
pub fn run(program: &str, args: &[&str]) -> Result<(), ToolError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::Missing,
            _ => ToolError::Failed(err.to_string()),
        })?;
    // Read stderr on a detached thread: a launched GUI program may keep the pipe
    // open long after the child exits, and that must never delay `run`.
    let (sender, receiver) = mpsc::channel();
    if let Some(mut pipe) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            let _ = sender.send(String::from_utf8_lossy(&bytes).trim().to_string());
        });
    }
    let status = child
        .wait()
        .map_err(|err| ToolError::Failed(err.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        let stderr = receiver
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_default();
        Err(ToolError::Failed(stderr))
    }
}

/// HTTP agent with a 5 s timeout; 4xx/5xx are returned as responses so bodies stay readable.
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_reports_missing_failed_and_ok() {
        assert!(matches!(
            run("nova-gibt-es-nicht", &[]),
            Err(ToolError::Missing)
        ));
        assert!(matches!(run("false", &[]), Err(ToolError::Failed(_))));
        assert!(run("true", &[]).is_ok());
    }

    #[test]
    fn run_returns_while_a_background_child_holds_stderr() {
        let start = std::time::Instant::now();
        assert!(run("sh", &["-c", "sleep 5 &"]).is_ok());
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn failed_run_keeps_stderr() {
        match run("sh", &["-c", "echo boom >&2; exit 1"]) {
            Err(ToolError::Failed(text)) => assert!(text.contains("boom")),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
