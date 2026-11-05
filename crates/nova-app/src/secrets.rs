//! Secrets in KWallet through `secret-tool`.
//!
//! Values travel over pipes only, never as arguments, so they do not show up in
//! the process list. Errors never quote a value.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

const PROGRAM: &str = "secret-tool";
const SERVICE: &str = "nova";

/// Arguments for `secret-tool store`; the value itself travels over stdin.
pub fn store_args(account: &str) -> Vec<String> {
    vec![
        "store".into(),
        format!("--label=Nova {account}"),
        "service".into(),
        SERVICE.into(),
        "account".into(),
        account.into(),
    ]
}

fn lookup_args(account: &str) -> Vec<String> {
    vec![
        "lookup".into(),
        "service".into(),
        SERVICE.into(),
        "account".into(),
        account.into(),
    ]
}

/// Read the secret stored for `account`; `None` when there is no entry.
///
/// # Errors
///
/// Fails when `secret-tool` is missing or reports a problem, such as a locked wallet.
pub fn read(account: &str) -> anyhow::Result<Option<String>> {
    let output = Command::new(PROGRAM)
        .args(lookup_args(account))
        .stdin(Stdio::null())
        .output()
        .context("starting secret-tool")?;
    lookup_result(output.status.code(), &output.stdout, &output.stderr)
}

/// `secret-tool lookup` exits with 1 and stays silent when there is no entry.
fn lookup_result(
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> anyhow::Result<Option<String>> {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    match code {
        Some(0) => {}
        Some(1) if stdout.is_empty() && stderr.is_empty() => return Ok(None),
        _ => bail!("secret-tool lookup failed with {code:?}: {stderr}"),
    }
    let value = std::str::from_utf8(stdout)
        .context("stored secret is not UTF-8")?
        .trim_end_matches('\n');
    Ok((!value.is_empty()).then(|| value.to_string()))
}

/// Store `value` for `account`, replacing an existing entry.
///
/// # Errors
///
/// Fails when `secret-tool` is missing or reports a problem, such as a locked wallet.
pub fn store(account: &str, value: &str) -> anyhow::Result<()> {
    let mut child = Command::new(PROGRAM)
        .args(store_args(account))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting secret-tool")?;
    let mut stdin = child.stdin.take().context("secret-tool has no stdin")?;
    let written = stdin.write_all(value.as_bytes());
    // Closing stdin is the end-of-value signal for secret-tool.
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("waiting for secret-tool")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "secret-tool store failed with {:?}: {}",
            output.status.code(),
            stderr.trim()
        );
    }
    written.context("writing to secret-tool")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_args_name_the_entry_and_no_value() {
        assert_eq!(
            store_args("x"),
            ["store", "--label=Nova x", "service", "nova", "account", "x"]
        );
    }

    #[test]
    fn lookup_args_name_the_entry() {
        assert_eq!(
            lookup_args("x"),
            ["lookup", "service", "nova", "account", "x"]
        );
    }

    #[test]
    fn lookup_returns_the_value() {
        assert_eq!(
            lookup_result(Some(0), b"geheim", b"").unwrap(),
            Some("geheim".into())
        );
    }

    #[test]
    fn lookup_drops_a_trailing_newline() {
        assert_eq!(
            lookup_result(Some(0), b"geheim\n", b"").unwrap(),
            Some("geheim".into())
        );
    }

    #[test]
    fn empty_result_means_no_entry() {
        assert_eq!(lookup_result(Some(0), b"", b"").unwrap(), None);
    }

    #[test]
    fn exit_one_without_output_means_no_entry() {
        assert_eq!(lookup_result(Some(1), b"", b"").unwrap(), None);
    }

    #[test]
    fn exit_one_with_stderr_is_an_error() {
        let err = lookup_result(Some(1), b"", b"Cannot get secret service").unwrap_err();
        assert!(err.to_string().contains("Cannot get secret service"));
    }

    #[test]
    fn other_failures_are_errors_without_stdout() {
        let err = lookup_result(None, b"geheim", b"").unwrap_err();
        assert!(!format!("{err:#}").contains("geheim"));
    }
}
