//! Pure state machine of the voice loop. A supervisor executes its actions.

use crate::protocol::{HelperEvent, SttCommand, WakewordCommand};

/// Phase of the voice loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Only the wake word listens.
    Waiting,
    /// Wake word heard, waiting for the user's sentence.
    Listening,
    /// Nova is speaking.
    Speaking,
    /// Nova finished speaking, the user may continue without wake word.
    FollowUp,
}

/// Input for one transition.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Event(HelperEvent),
    DeadlineExpired,
}

/// Side effect requested by a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Wakeword(WakewordCommand),
    Stt(SttCommand),
    /// Finished sentence; the driver assigns the id.
    Speak(String),
    StopSpeaking,
    ArmDeadline,
    DisarmDeadline,
}

/// Computes the next state and the actions to execute.
pub fn step(state: State, input: Input) -> (State, Vec<Action>) {
    use Action::*;
    use HelperEvent::*;
    use State::*;

    let give_up = || vec![Stt(SttCommand::Cancel), Wakeword(WakewordCommand::Resume)];
    let speak = |text: &str| {
        vec![
            DisarmDeadline,
            Speak(format!("Du sagtest: {}", text.trim())),
        ]
    };

    match (state, input) {
        (Waiting, Input::Event(Wake { .. })) => (
            Listening,
            vec![
                Wakeword(WakewordCommand::Pause),
                Stt(SttCommand::Listen),
                ArmDeadline,
            ],
        ),
        (Listening, Input::Event(SpeechStart)) => (Listening, vec![DisarmDeadline]),
        (Listening, Input::DeadlineExpired) => (Waiting, give_up()),
        (Listening, Input::Event(Final { text })) if !text.trim().is_empty() => {
            (Speaking, speak(&text))
        }
        (Listening, Input::Event(Final { .. })) => {
            let mut actions = vec![DisarmDeadline];
            actions.extend(give_up());
            (Waiting, actions)
        }
        (Speaking, Input::Event(SpeechStart)) => (Listening, vec![StopSpeaking]),
        (Speaking, Input::Event(Done { .. })) => (FollowUp, vec![ArmDeadline]),
        (FollowUp, Input::Event(SpeechStart)) => (FollowUp, vec![DisarmDeadline]),
        (FollowUp, Input::Event(Final { text })) if !text.trim().is_empty() => {
            (Speaking, speak(&text))
        }
        (FollowUp, Input::Event(Final { .. })) => (FollowUp, vec![ArmDeadline]),
        (FollowUp, Input::DeadlineExpired) => (Waiting, give_up()),
        (state, input) => {
            tracing::debug!(?state, ?input, "input ignored");
            (state, vec![])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;
    use State::*;

    fn ev(e: HelperEvent) -> Input {
        Input::Event(e)
    }
    fn fin(t: &str) -> Input {
        ev(HelperEvent::Final { text: t.into() })
    }
    fn give_up() -> Vec<Action> {
        vec![Stt(SttCommand::Cancel), Wakeword(WakewordCommand::Resume)]
    }

    #[test]
    fn wake_arms_deadline_and_starts_stt() {
        assert_eq!(
            step(Waiting, ev(HelperEvent::Wake { score: 0.9 })),
            (
                Listening,
                vec![
                    Wakeword(WakewordCommand::Pause),
                    Stt(SttCommand::Listen),
                    ArmDeadline
                ]
            )
        );
    }

    #[test]
    fn speech_start_in_listening_disarms_deadline() {
        assert_eq!(
            step(Listening, ev(HelperEvent::SpeechStart)),
            (Listening, vec![DisarmDeadline])
        );
    }

    #[test]
    fn deadline_in_listening_gives_up() {
        assert_eq!(
            step(Listening, Input::DeadlineExpired),
            (Waiting, give_up())
        );
    }

    #[test]
    fn final_is_echoed_with_template() {
        let (s, actions) = step(Listening, fin("Hallo"));
        assert_eq!(s, Speaking);
        assert!(actions.contains(&Speak("Du sagtest: Hallo".into())));
    }

    #[test]
    fn empty_final_in_listening_gives_up() {
        let (s, actions) = step(Listening, fin(""));
        assert_eq!(s, Waiting);
        assert!(actions.ends_with(&give_up()));
        assert!(actions.contains(&DisarmDeadline));
    }

    #[test]
    fn whitespace_final_counts_as_empty() {
        let (s, actions) = step(Listening, fin("  \n "));
        assert_eq!(s, Waiting);
        assert!(!actions.iter().any(|a| matches!(a, Speak(_))));
        assert_eq!(step(FollowUp, fin(" ")), (FollowUp, vec![ArmDeadline]));
    }

    #[test]
    fn barge_in_stops_speech_without_deadline() {
        assert_eq!(
            step(Speaking, ev(HelperEvent::SpeechStart)),
            (Listening, vec![StopSpeaking])
        );
    }

    #[test]
    fn tts_done_starts_follow_up() {
        assert_eq!(
            step(Speaking, ev(HelperEvent::Done { id: "1".into() })),
            (FollowUp, vec![ArmDeadline])
        );
    }

    #[test]
    fn final_while_speaking_is_dropped() {
        assert_eq!(step(Speaking, fin("Echo")), (Speaking, vec![]));
    }

    #[test]
    fn speech_start_in_follow_up_disarms_deadline() {
        assert_eq!(
            step(FollowUp, ev(HelperEvent::SpeechStart)),
            (FollowUp, vec![DisarmDeadline])
        );
    }

    #[test]
    fn final_in_follow_up_is_echoed() {
        let (s, actions) = step(FollowUp, fin("Noch eins"));
        assert_eq!(s, Speaking);
        assert!(actions.contains(&Speak("Du sagtest: Noch eins".into())));
        assert!(actions.contains(&DisarmDeadline));
    }

    #[test]
    fn empty_final_in_follow_up_rearms_deadline() {
        assert_eq!(step(FollowUp, fin("")), (FollowUp, vec![ArmDeadline]));
    }

    #[test]
    fn deadline_in_follow_up_gives_up() {
        assert_eq!(step(FollowUp, Input::DeadlineExpired), (Waiting, give_up()));
    }

    #[test]
    fn late_speech_events_in_waiting_change_nothing() {
        for input in [
            ev(HelperEvent::SpeechStart),
            fin("x"),
            Input::DeadlineExpired,
        ] {
            assert_eq!(step(Waiting, input), (Waiting, vec![]));
        }
    }

    #[test]
    fn late_tts_done_while_listening_is_ignored() {
        assert_eq!(
            step(Listening, ev(HelperEvent::Done { id: "1".into() })),
            (Listening, vec![])
        );
    }
}
