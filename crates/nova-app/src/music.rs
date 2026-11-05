//! Spotify transport and volume through `playerctl`.

use crate::commands::{Command, Reply};
use crate::tools::{self, ToolError};

/// `playerctl` arguments for a music command; `None` for any other command.
pub fn args(command: &Command) -> Option<Vec<String>> {
    let tail: Vec<String> = match command {
        Command::Play => vec!["play".into()],
        Command::Pause => vec!["pause".into()],
        Command::Next => vec!["next".into()],
        Command::Previous => vec!["previous".into()],
        Command::VolumeStep(step) => {
            let sign = if *step < 0 { '-' } else { '+' };
            let fraction = f32::from(step.unsigned_abs()) / 100.0;
            vec!["volume".into(), format!("{fraction}{sign}")]
        }
        Command::VolumeSet(percent) => {
            vec![
                "volume".into(),
                format!("{}.{:02}", percent / 100, percent % 100),
            ]
        }
        _ => return None,
    };
    Some(
        std::iter::once("--player=spotify".to_string())
            .chain(tail)
            .collect(),
    )
}

/// Run a music command; commands without `playerctl` arguments stay silent.
pub fn control(command: &Command) -> Reply {
    let Some(args) = args(command) else {
        return Reply::Silent;
    };
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = tools::run("playerctl", &refs);
    if let Err(err) = &result {
        tracing::warn!(?err, ?args, "playerctl failed");
    }
    reply_for(result)
}

/// Spoken outcome of a `playerctl` call.
pub fn reply_for(result: Result<(), ToolError>) -> Reply {
    match result {
        Ok(()) => Reply::Silent,
        Err(ToolError::Missing) => Reply::Say("Playerctl ist nicht installiert.".into()),
        Err(ToolError::Failed(_)) => Reply::Say("Spotify läuft gerade nicht.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Command::*;

    #[test]
    fn builds_playerctl_arguments() {
        assert_eq!(args(&Play).unwrap(), ["--player=spotify", "play"]);
        assert_eq!(args(&Pause).unwrap(), ["--player=spotify", "pause"]);
        assert_eq!(args(&Next).unwrap(), ["--player=spotify", "next"]);
        assert_eq!(args(&Previous).unwrap(), ["--player=spotify", "previous"]);
        assert_eq!(
            args(&VolumeStep(10)).unwrap(),
            ["--player=spotify", "volume", "0.1+"]
        );
        assert_eq!(
            args(&VolumeStep(-10)).unwrap(),
            ["--player=spotify", "volume", "0.1-"]
        );
        assert_eq!(
            args(&VolumeSet(5)).unwrap(),
            ["--player=spotify", "volume", "0.05"]
        );
        assert_eq!(
            args(&VolumeSet(100)).unwrap(),
            ["--player=spotify", "volume", "1.00"]
        );
        assert_eq!(args(&Dismiss), None);
    }

    #[test]
    fn dismiss_is_silent_without_running_anything() {
        assert_eq!(control(&Dismiss), Reply::Silent);
    }

    #[test]
    fn replies_to_tool_results() {
        assert_eq!(reply_for(Ok(())), Reply::Silent);
        assert_eq!(
            reply_for(Err(ToolError::Missing)),
            Reply::Say("Playerctl ist nicht installiert.".into())
        );
        assert_eq!(
            reply_for(Err(ToolError::Failed("No players found".into()))),
            Reply::Say("Spotify läuft gerade nicht.".into())
        );
    }
}
