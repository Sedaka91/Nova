//! Driver of the voice loop: runs the helpers and executes the state machine's actions.

use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use nova_core::config::Config;
use serde::Serialize;

use crate::commands::{Command, Reply};
use crate::helper::{Helper, HelperSpec, Incoming};
use crate::protocol::{HelperEvent, HelperKind, SttCommand, TtsCommand, WakewordCommand};
use crate::voice_loop::{Action, Dialog, Input, step};

/// Timing of the voice loop.
#[derive(Debug, Clone)]
pub struct LoopSettings {
    /// How long a helper may take to report `ready`.
    pub ready_timeout: Duration,
    /// How long Nova waits for speech after the wake word and after an answer.
    pub follow_up: Duration,
    /// How long a command may run before Nova gives up on it.
    pub work_timeout: Duration,
}

/// Executes a command on a worker thread.
pub type Executor = Arc<dyn Fn(Command) -> Reply + Send + Sync>;

/// Runs all helpers and the voice loop until `Incoming::Shutdown` arrives.
///
/// # Errors
/// Fails if a helper cannot start, does not report `ready` in time, exits during the
/// initial start, or exits for the second time. The error names the helper and its last
/// `error` message.
///
/// # Panics
/// Panics if `specs` lacks a helper the voice loop talks to.
pub fn run(
    specs: &[HelperSpec],
    settings: &LoopSettings,
    executor: Executor,
    tx: Sender<Incoming>,
    rx: Receiver<Incoming>,
) -> anyhow::Result<()> {
    let slots = specs
        .iter()
        .map(|spec| {
            Ok(Slot {
                spec,
                helper: spawn(spec, tx.clone())?,
                restarted: false,
                last_error: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let mut driver = Driver {
        slots,
        settings,
        executor,
        tx,
        rx,
        dialog: Dialog::new(),
        deadline: None,
        spoken: None,
        running: None,
        next_id: 1,
    };
    let kinds: Vec<HelperKind> = specs.iter().map(|spec| spec.kind).collect();
    match driver.await_ready(&kinds, OnExit::Fatal)? {
        Flow::Continue => driver.run_loop(),
        Flow::Shutdown => Ok(()),
    }
}

/// Builds the start commands of all helpers.
pub fn helper_specs(config: &Config, root: &Path) -> Vec<HelperSpec> {
    let project = root.join("helpers");
    let spec = |kind: HelperKind, options: &[&str]| {
        let mut args = vec![
            "run".to_owned(),
            "--project".to_owned(),
            project.to_string_lossy().into_owned(),
            "python".to_owned(),
            "-m".to_owned(),
            format!("nova_helpers.{}", name(kind)),
        ];
        args.extend(options.iter().map(|option| (*option).to_owned()));
        HelperSpec {
            kind,
            program: "uv".into(),
            args,
        }
    };
    let input = config.audio.eingabe.as_str();
    vec![
        spec(
            HelperKind::Wakeword,
            &[
                "--model",
                &config.wakeword.model_arg(root),
                "--threshold",
                &config.wakeword.schwelle.to_string(),
                "--input",
                input,
            ],
        ),
        spec(
            HelperKind::Stt,
            &[
                "--model",
                &config.spracherkennung.whisper_modell,
                "--device",
                &config.spracherkennung.geraet,
                "--input",
                input,
            ],
        ),
        spec(
            HelperKind::Tts,
            &[
                "--voice",
                &config.stimme.piper_modell,
                "--output",
                &config.audio.ausgabe,
            ],
        ),
    ]
}

fn name(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Wakeword => "wakeword",
        HelperKind::Stt => "stt",
        HelperKind::Tts => "tts",
    }
}

fn spawn(spec: &HelperSpec, tx: Sender<Incoming>) -> anyhow::Result<Helper> {
    Helper::spawn(spec, tx).with_context(|| format!("starting helper {}", name(spec.kind)))
}

/// Whether the driver goes on after waiting for helpers.
enum Flow {
    Continue,
    Shutdown,
}

/// What an `Exited` means while waiting for `ready`.
#[derive(Clone, Copy)]
enum OnExit {
    Fatal,
    Restart,
}

/// One supervised helper and what the driver remembers about it.
struct Slot<'a> {
    spec: &'a HelperSpec,
    helper: Helper,
    restarted: bool,
    last_error: Option<String>,
}

struct Driver<'a> {
    slots: Vec<Slot<'a>>,
    settings: &'a LoopSettings,
    executor: Executor,
    /// Handed to restarted helpers and worker threads; also keeps `rx` connected.
    tx: Sender<Incoming>,
    rx: Receiver<Incoming>,
    dialog: Dialog,
    deadline: Option<Instant>,
    /// Id of the utterance in progress; a `done` with any other id is stale.
    spoken: Option<String>,
    /// Id of the run in progress; a reply with any other id is stale.
    running: Option<u64>,
    /// Next id for an utterance or a run.
    next_id: u64,
}

impl<'a> Driver<'a> {
    fn slot(&self, kind: HelperKind) -> &Slot<'a> {
        self.slots
            .iter()
            .find(|slot| slot.spec.kind == kind)
            .unwrap_or_else(|| panic!("no spec for helper {}", name(kind)))
    }

    fn slot_mut(&mut self, kind: HelperKind) -> &mut Slot<'a> {
        self.slots
            .iter_mut()
            .find(|slot| slot.spec.kind == kind)
            .unwrap_or_else(|| panic!("no spec for helper {}", name(kind)))
    }

    fn failure(&self, kind: HelperKind, what: &str) -> anyhow::Error {
        let helper = name(kind);
        match &self.slot(kind).last_error {
            Some(message) => anyhow!("helper {helper} {what}; last error: {message}"),
            None => anyhow!("helper {helper} {what}"),
        }
    }

    fn note_error(&mut self, kind: HelperKind, message: &str) {
        tracing::error!(?kind, message, "helper error");
        self.slot_mut(kind).last_error = Some(message.to_owned());
    }

    /// A failed send is only logged: the helper's `Exited` decides what happens.
    fn send(&mut self, kind: HelperKind, command: &impl Serialize) {
        if let Err(err) = self.slot_mut(kind).helper.send(command) {
            tracing::warn!(?kind, %err, "sending command failed");
        }
    }

    /// Waits for `ready` of the given helpers; all other events are dropped.
    fn await_ready(&mut self, kinds: &[HelperKind], on_exit: OnExit) -> anyhow::Result<Flow> {
        let mut pending = kinds.to_vec();
        let mut end = Instant::now() + self.settings.ready_timeout;
        while let Some(&first) = pending.first() {
            let timeout = end.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(timeout) {
                Ok(Incoming::Event(kind, HelperEvent::Ready)) => {
                    tracing::info!(?kind, "helper ready");
                    pending.retain(|pending_kind| *pending_kind != kind);
                }
                Ok(Incoming::Event(kind, HelperEvent::Error { message })) => {
                    self.note_error(kind, &message);
                }
                Ok(Incoming::Event(kind, event)) => {
                    tracing::debug!(?kind, ?event, "event dropped while waiting for ready");
                }
                Ok(Incoming::Reply(id, _)) => {
                    tracing::debug!(id, "reply dropped while waiting for ready");
                }
                Ok(Incoming::Exited(kind)) => match on_exit {
                    OnExit::Fatal => {
                        return Err(self.failure(kind, "exited while waiting for ready"));
                    }
                    OnExit::Restart => {
                        self.respawn(kind)?;
                        pending.push(kind);
                        end = Instant::now() + self.settings.ready_timeout;
                    }
                },
                Ok(Incoming::Shutdown) => return Ok(Flow::Shutdown),
                Err(_) => return Err(self.failure(first, "did not report ready in time")),
            }
        }
        Ok(Flow::Continue)
    }

    fn run_loop(&mut self) -> anyhow::Result<()> {
        loop {
            let received = match self.deadline {
                Some(at) => {
                    let timeout = at.saturating_duration_since(Instant::now());
                    self.rx.recv_timeout(timeout)
                }
                None => self.rx.recv().map_err(RecvTimeoutError::from),
            };
            match received {
                Ok(Incoming::Event(kind, event)) => self.on_event(kind, event),
                Ok(Incoming::Reply(id, reply)) => self.on_reply(id, reply),
                Ok(Incoming::Exited(kind)) => {
                    if let Flow::Shutdown = self.restart(kind)? {
                        return Ok(());
                    }
                }
                Ok(Incoming::Shutdown) => return Ok(()),
                Err(RecvTimeoutError::Timeout) => self.on_timeout(),
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("the driver holds a sender of its own channel")
                }
            }
        }
    }

    fn on_event(&mut self, kind: HelperKind, event: HelperEvent) {
        match &event {
            HelperEvent::Error { message } => {
                self.note_error(kind, message);
                return;
            }
            HelperEvent::Partial { text } => tracing::info!(text, "partial"),
            HelperEvent::Final { text } => tracing::info!(text, "final"),
            HelperEvent::Done { id } if self.spoken.as_ref() != Some(id) => {
                tracing::debug!(id, "stale done dropped");
                return;
            }
            HelperEvent::Done { .. } => self.spoken = None,
            _ => {}
        }
        self.advance(Input::Event(event));
    }

    fn on_reply(&mut self, id: u64, reply: Reply) {
        if self.running != Some(id) {
            tracing::debug!(id, "stale reply dropped");
            return;
        }
        self.running = None;
        self.advance(Input::Reply(reply));
    }

    fn on_timeout(&mut self) {
        if self.deadline.is_some_and(|at| Instant::now() >= at) {
            self.deadline = None;
            self.advance(Input::DeadlineExpired);
        }
    }

    fn advance(&mut self, input: Input) {
        let (next, actions) = step(self.dialog.clone(), input);
        self.enter(next);
        for action in actions {
            self.apply(action);
        }
    }

    fn enter(&mut self, next: Dialog) {
        if next.state != self.dialog.state {
            tracing::info!(from = ?self.dialog.state, to = ?next.state, "state changed");
        }
        self.dialog = next;
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Wakeword(command) => self.send(HelperKind::Wakeword, &command),
            Action::Stt(command) => self.send(HelperKind::Stt, &command),
            Action::Speak(text) => {
                tracing::info!(text, "speak");
                let id = self.next_id.to_string();
                self.next_id += 1;
                self.spoken = Some(id.clone());
                self.send(HelperKind::Tts, &TtsCommand::Speak { id, text });
            }
            Action::StopSpeaking => {
                self.spoken = None;
                self.send(HelperKind::Tts, &TtsCommand::Stop);
            }
            Action::ArmDeadline => {
                self.deadline = Some(Instant::now() + self.settings.follow_up);
            }
            Action::DisarmDeadline => self.deadline = None,
            Action::Run(command) => self.start_run(command),
            Action::AbandonRun => self.running = None,
            Action::ArmWorkDeadline => {
                self.deadline = Some(Instant::now() + self.settings.work_timeout);
            }
        }
    }

    /// Runs `command` on its own thread so that Nova stays interruptible meanwhile.
    fn start_run(&mut self, command: Command) {
        let id = self.next_id;
        self.next_id += 1;
        self.running = Some(id);
        let executor = Arc::clone(&self.executor);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let reply = executor(command);
            // A closed receiver means the loop has already ended.
            let _ = tx.send(Incoming::Reply(id, reply));
        });
    }

    /// Replaces an exited helper, which is allowed once per helper.
    fn respawn(&mut self, kind: HelperKind) -> anyhow::Result<()> {
        if self.slot(kind).restarted {
            return Err(self.failure(kind, "exited for the second time"));
        }
        tracing::warn!(?kind, "helper exited, restarting");
        let tx = self.tx.clone();
        let slot = self.slot_mut(kind);
        slot.restarted = true;
        slot.helper.stop();
        slot.helper = spawn(slot.spec, tx)?;
        Ok(())
    }

    /// Recovers from an exited helper and brings all helpers back to the waiting state.
    fn restart(&mut self, kind: HelperKind) -> anyhow::Result<Flow> {
        self.respawn(kind)?;
        if let Flow::Shutdown = self.await_ready(&[kind], OnExit::Restart)? {
            return Ok(Flow::Shutdown);
        }
        self.send(HelperKind::Tts, &TtsCommand::Stop);
        self.send(HelperKind::Stt, &SttCommand::Cancel);
        self.send(HelperKind::Wakeword, &WakewordCommand::Resume);
        self.deadline = None;
        self.spoken = None;
        self.running = None;
        self.enter(Dialog::new());
        Ok(Flow::Continue)
    }
}
