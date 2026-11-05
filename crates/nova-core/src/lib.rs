//! Shared types and configuration for Nova.

pub mod apps;
pub mod config;
pub mod phrases;

/// Routing stage chosen for a request. On low router confidence the next higher stage wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Fixed commands, no model involved.
    Command = 1,
    /// Local chat model via Ollama.
    Local = 2,
    /// Opus via the persistent Claude Code session.
    Opus = 3,
}

impl Stage {
    /// The next higher stage, used when the router is unsure.
    pub fn escalate(self) -> Self {
        match self {
            Stage::Command => Stage::Local,
            Stage::Local | Stage::Opus => Stage::Opus,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Stage;

    #[test]
    fn escalate_moves_up_and_saturates() {
        assert_eq!(Stage::Command.escalate(), Stage::Local);
        assert_eq!(Stage::Local.escalate(), Stage::Opus);
        assert_eq!(Stage::Opus.escalate(), Stage::Opus);
    }
}
