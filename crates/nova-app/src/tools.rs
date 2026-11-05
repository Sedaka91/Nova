//! Shared helpers for child processes and HTTP.

use std::io::Read;
use std::process::{Command, Stdio};
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
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        // Lossy read: a non-UTF-8 message is still worth logging.
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        stderr = String::from_utf8_lossy(&bytes).trim().to_string();
    }
    let status = child
        .wait()
        .map_err(|err| ToolError::Failed(err.to_string()))?;
    if status.success() {
        Ok(())
    } else {
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
}
