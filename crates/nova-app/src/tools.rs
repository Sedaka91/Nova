//! Shared helpers for child processes and HTTP.

use std::io::Read;
use std::os::unix::process::CommandExt;
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

/// Start a long-lived GUI program in its own process group with no stdio attached,
/// so Ctrl+C or the exit of Nova does not reach it. Waits for the launcher itself only.
pub fn launch(program: &str, args: &[&str]) -> Result<(), ToolError> {
    let status = Command::new(program)
        .args(args)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::Missing,
            _ => ToolError::Failed(err.to_string()),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(ToolError::Failed(status.to_string()))
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
    fn launch_reports_missing_failed_and_ok() {
        assert!(matches!(
            launch("nova-gibt-es-nicht", &[]),
            Err(ToolError::Missing)
        ));
        assert!(matches!(launch("false", &[]), Err(ToolError::Failed(_))));
        assert!(launch("true", &[]).is_ok());
    }

    #[test]
    fn launched_child_has_its_own_process_group() {
        let path = std::env::temp_dir().join(format!("nova-launch-{}", std::process::id()));
        let script = format!("ps -o pgid= $$ > {}", path.display());
        assert!(launch("sh", &["-c", &script]).is_ok());
        let pgid: i32 = std::fs::read_to_string(&path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::fs::remove_file(&path).ok();
        assert_ne!(pgid, nix::unistd::getpgrp().as_raw());
    }

    #[test]
    fn failed_run_keeps_stderr() {
        match run("sh", &["-c", "echo boom >&2; exit 1"]) {
            Err(ToolError::Failed(text)) => assert!(text.contains("boom")),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
