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

/// The voice loop's state and what it counts towards the next spoken sentence.
///
/// `stt` sends `speech_start` at once but `final` only after Whisper, so a new utterance can
/// start before the previous `final` arrives. `open` counts utterances without a `final` yet,
/// `heard` collects their texts until the last one is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub state: State,
    open: u32,
    heard: Vec<String>,
}

impl Dialog {
    /// Starts in `Waiting`.
    pub fn new() -> Self {
        Self {
            state: State::Waiting,
            open: 0,
            heard: Vec::new(),
        }
    }
}

impl Default for Dialog {
    fn default() -> Self {
        Self::new()
    }
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
pub fn step(dialog: Dialog, input: Input) -> (Dialog, Vec<Action>) {
    use Action::*;
    use HelperEvent::*;
    use State::*;

    let give_up = || vec![Stt(SttCommand::Cancel), Wakeword(WakewordCommand::Resume)];
    let in_state = |state: State| Dialog {
        state,
        ..Dialog::new()
    };

    let Dialog {
        state,
        mut open,
        mut heard,
    } = dialog;
    match (state, input) {
        (Waiting, Input::Event(Wake { .. })) => (
            in_state(Listening),
            vec![
                Wakeword(WakewordCommand::Pause),
                Stt(SttCommand::Listen),
                ArmDeadline,
            ],
        ),
        (Listening | FollowUp, Input::Event(SpeechStart)) => {
            let opened = Dialog {
                state,
                open: open + 1,
                heard,
            };
            (opened, vec![DisarmDeadline])
        }
        (Speaking, Input::Event(SpeechStart)) => {
            let barged_in = Dialog {
                open: 1,
                ..in_state(Listening)
            };
            (barged_in, vec![StopSpeaking])
        }
        (Listening | FollowUp, Input::Event(Final { text })) => {
            open = open.saturating_sub(1);
            if !text.trim().is_empty() {
                heard.push(text.trim().to_owned());
            }
            if open > 0 {
                return (Dialog { state, open, heard }, vec![]);
            }
            let text = heard.join(" ");
            match (state, text.is_empty()) {
                (_, false) => (
                    in_state(Speaking),
                    vec![DisarmDeadline, Speak(format!("Du sagtest: {text}"))],
                ),
                (FollowUp, true) => (in_state(FollowUp), vec![ArmDeadline]),
                (_, true) => {
                    let mut actions = vec![DisarmDeadline];
                    actions.extend(give_up());
                    (in_state(Waiting), actions)
                }
            }
        }
        (Listening | FollowUp, Input::DeadlineExpired) => (in_state(Waiting), give_up()),
        (Speaking, Input::Event(Done { .. })) => (in_state(FollowUp), vec![ArmDeadline]),
        (state, input) => {
            tracing::debug!(?state, ?input, "input ignored");
            (Dialog { state, open, heard }, vec![])
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
    /// A dialog in `state` with no utterance open.
    fn at(state: State) -> Dialog {
        Dialog {
            state,
            ..Dialog::new()
        }
    }
    fn run(state: State, input: Input) -> (State, Vec<Action>) {
        let (dialog, actions) = step(at(state), input);
        (dialog.state, actions)
    }
    /// Feeds `inputs` in order and returns the final dialog and the actions of each step.
    fn feed(dialog: Dialog, inputs: Vec<Input>) -> (Dialog, Vec<Vec<Action>>) {
        let mut dialog = dialog;
        let mut all = Vec::new();
        for input in inputs {
            let (next, actions) = step(dialog, input);
            dialog = next;
            all.push(actions);
        }
        (dialog, all)
    }
    fn start() -> Input {
        ev(HelperEvent::SpeechStart)
    }
    fn spoken(actions: &[Vec<Action>]) -> Vec<String> {
        actions
            .iter()
            .flatten()
            .filter_map(|action| match action {
                Speak(text) => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
    fn give_up() -> Vec<Action> {
        vec![Stt(SttCommand::Cancel), Wakeword(WakewordCommand::Resume)]
    }

    #[test]
    fn wake_arms_deadline_and_starts_stt() {
        assert_eq!(
            run(Waiting, ev(HelperEvent::Wake { score: 0.9 })),
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
            run(Listening, ev(HelperEvent::SpeechStart)),
            (Listening, vec![DisarmDeadline])
        );
    }

    #[test]
    fn deadline_in_listening_gives_up() {
        assert_eq!(run(Listening, Input::DeadlineExpired), (Waiting, give_up()));
    }

    #[test]
    fn final_is_echoed_with_template() {
        let (s, actions) = run(Listening, fin("Hallo"));
        assert_eq!(s, Speaking);
        assert!(actions.contains(&Speak("Du sagtest: Hallo".into())));
    }

    #[test]
    fn empty_final_in_listening_gives_up() {
        let (s, actions) = run(Listening, fin(""));
        assert_eq!(s, Waiting);
        assert!(actions.ends_with(&give_up()));
        assert!(actions.contains(&DisarmDeadline));
    }

    #[test]
    fn whitespace_final_counts_as_empty() {
        let (s, actions) = run(Listening, fin("  \n "));
        assert_eq!(s, Waiting);
        assert!(!actions.iter().any(|a| matches!(a, Speak(_))));
        assert_eq!(run(FollowUp, fin(" ")), (FollowUp, vec![ArmDeadline]));
    }

    #[test]
    fn barge_in_stops_speech_without_deadline() {
        assert_eq!(
            run(Speaking, ev(HelperEvent::SpeechStart)),
            (Listening, vec![StopSpeaking])
        );
    }

    #[test]
    fn tts_done_starts_follow_up() {
        assert_eq!(
            run(Speaking, ev(HelperEvent::Done { id: "1".into() })),
            (FollowUp, vec![ArmDeadline])
        );
    }

    #[test]
    fn final_while_speaking_is_dropped() {
        assert_eq!(run(Speaking, fin("Echo")), (Speaking, vec![]));
    }

    #[test]
    fn speech_start_in_follow_up_disarms_deadline() {
        assert_eq!(
            run(FollowUp, ev(HelperEvent::SpeechStart)),
            (FollowUp, vec![DisarmDeadline])
        );
    }

    #[test]
    fn final_in_follow_up_is_echoed() {
        let (s, actions) = run(FollowUp, fin("Noch eins"));
        assert_eq!(s, Speaking);
        assert!(actions.contains(&Speak("Du sagtest: Noch eins".into())));
        assert!(actions.contains(&DisarmDeadline));
    }

    #[test]
    fn empty_final_in_follow_up_rearms_deadline() {
        assert_eq!(run(FollowUp, fin("")), (FollowUp, vec![ArmDeadline]));
    }

    #[test]
    fn deadline_in_follow_up_gives_up() {
        assert_eq!(run(FollowUp, Input::DeadlineExpired), (Waiting, give_up()));
    }

    #[test]
    fn late_speech_events_in_waiting_change_nothing() {
        for input in [
            ev(HelperEvent::SpeechStart),
            fin("x"),
            Input::DeadlineExpired,
        ] {
            assert_eq!(run(Waiting, input), (Waiting, vec![]));
        }
    }

    #[test]
    fn late_tts_done_while_listening_is_ignored() {
        assert_eq!(
            run(Listening, ev(HelperEvent::Done { id: "1".into() })),
            (Listening, vec![])
        );
    }

    #[test]
    fn overlapping_utterances_are_spoken_once_after_the_last_final() {
        let (dialog, actions) = feed(at(Listening), vec![start(), start(), fin("A"), fin("B")]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(actions[2], vec![]);
        assert_eq!(spoken(&actions), ["Du sagtest: A B"]);
        assert!(actions[3].contains(&Speak("Du sagtest: A B".into())));
    }

    #[test]
    fn empty_final_of_a_false_trigger_does_not_cancel_the_open_utterance() {
        let (dialog, actions) = feed(at(Listening), vec![start(), start(), fin(""), fin("Hallo")]);
        assert_eq!(dialog.state, Speaking);
        assert!(!actions.iter().flatten().any(|a| give_up().contains(a)));
        assert_eq!(spoken(&actions), ["Du sagtest: Hallo"]);
    }

    #[test]
    fn overlapping_utterances_in_follow_up_are_joined_without_arming() {
        let (dialog, actions) = feed(at(FollowUp), vec![start(), start(), fin("A"), fin("B")]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(spoken(&actions), ["Du sagtest: A B"]);
        assert!(!actions.iter().flatten().any(|a| *a == ArmDeadline));
    }

    #[test]
    fn empty_final_in_follow_up_does_not_arm_while_an_utterance_is_open() {
        let (dialog, actions) = feed(at(FollowUp), vec![start(), start(), fin(""), fin("Hallo")]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(spoken(&actions), ["Du sagtest: Hallo"]);
        assert!(!actions.iter().flatten().any(|a| *a == ArmDeadline));
    }

    #[test]
    fn only_the_last_empty_final_gives_up_in_listening() {
        let (dialog, actions) = feed(at(Listening), vec![start(), start(), fin(""), fin("")]);
        assert_eq!(dialog, Dialog::new());
        assert_eq!(actions[2], vec![]);
        assert!(actions[3].ends_with(&give_up()));
    }

    #[test]
    fn only_the_last_empty_final_rearms_in_follow_up() {
        let (dialog, actions) = feed(at(FollowUp), vec![start(), start(), fin(""), fin("")]);
        assert_eq!(dialog, at(FollowUp));
        assert_eq!(actions[2], vec![]);
        assert_eq!(actions[3], vec![ArmDeadline]);
    }

    #[test]
    fn barge_in_followed_by_overlapping_utterances_is_joined() {
        let (dialog, actions) = feed(at(Speaking), vec![start(), start(), fin("A"), fin("B")]);
        assert_eq!(actions[0], vec![StopSpeaking]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(spoken(&actions), ["Du sagtest: A B"]);
    }

    #[test]
    fn barge_in_utterance_alone_is_spoken_at_its_final() {
        let (dialog, actions) = feed(at(Speaking), vec![start(), fin("Stopp")]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(spoken(&actions), ["Du sagtest: Stopp"]);
    }

    #[test]
    fn heard_text_does_not_leak_into_the_next_sentence() {
        let (dialog, _) = feed(at(Listening), vec![start(), start(), fin("A"), fin("B")]);
        let done = ev(HelperEvent::Done { id: "1".into() });
        let (_, actions) = feed(dialog, vec![done, start(), fin("C")]);
        assert_eq!(spoken(&actions), ["Du sagtest: C"]);
    }

    #[test]
    fn final_while_speaking_is_not_kept_for_the_next_sentence() {
        let done = ev(HelperEvent::Done { id: "1".into() });
        let (_, actions) = feed(at(Speaking), vec![fin("Echo"), done, start(), fin("Hallo")]);
        assert_eq!(spoken(&actions), ["Du sagtest: Hallo"]);
    }

    #[test]
    fn waiting_clears_the_counters() {
        let (dialog, _) = feed(
            at(Listening),
            vec![start(), start(), fin("A"), Input::DeadlineExpired],
        );
        assert_eq!(dialog, Dialog::new());

        let wake = ev(HelperEvent::Wake { score: 0.9 });
        let (dialog, actions) = feed(dialog, vec![start(), wake, start(), fin("Hallo")]);
        assert_eq!(dialog.state, Speaking);
        assert_eq!(spoken(&actions), ["Du sagtest: Hallo"]);
    }
}
