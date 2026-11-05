//! Drives the real supervisor against fake helper processes and checks the `--log` files.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nova_app::commands::{Command, Reply};
use nova_app::helper::{HelperSpec, Incoming};
use nova_app::protocol::{HelperEvent, HelperKind};
use nova_app::supervisor::{Executor, LoopSettings, helper_specs, run};
use nova_core::config::Config;

/// Generous upper bound for anything that must happen; tests normally take far less.
const WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(10);
const FOLLOW_UP: Duration = Duration::from_millis(200);
/// Long enough for a wrongly armed deadline to fire.
const SEVERAL_FOLLOW_UPS: Duration = Duration::from_millis(800);
/// For a worker that must not time out.
const NO_WORK_TIMEOUT: Duration = WAIT;
const WORK_TIMEOUT: Duration = Duration::from_millis(100);

const HELLO_SCRIPT: &str = r#"{"auf_befehl":{"listen":[
    {"event":"speech_start"},{"event":"final","text":"Hallo"}]}}"#;
const SPEAK_SCRIPT: &str = r#"{"auf_befehl":{"speak":[
    {"event":"started","id":"2"},{"event":"done","id":"2"}]}}"#;

/// `stt` script that hears `text` right after `listen`.
fn listen_script(text: &str) -> String {
    format!(
        r#"{{"auf_befehl":{{"listen":[
            {{"event":"speech_start"}},{{"event":"final","text":"{text}"}}]}}}}"#
    )
}

fn saying_done() -> Executor {
    Arc::new(|_| Reply::Say("Erledigt.".into()))
}

fn saying_done_after(delay: Duration) -> Executor {
    Arc::new(move |_| {
        thread::sleep(delay);
        Reply::Say("Erledigt.".into())
    })
}

/// Speaks the app name it was asked to open, which shows the text `parse` saw.
fn echoing_the_app() -> Executor {
    Arc::new(|command| match command {
        Command::OpenApp(name) => Reply::Say(format!("„{name}“")),
        other => panic!("unexpected command {other:?}"),
    })
}

/// Temp directory with the logs and scripts of one test.
struct Rig {
    dir: PathBuf,
}

impl Rig {
    fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "nova-supervisor-test-{}-{test}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create test dir");
        Self { dir }
    }

    /// Writes a script file and returns its path as a fake helper argument.
    fn file(&self, name: &str, content: &str) -> String {
        let path = self.dir.join(name);
        fs::write(&path, content).expect("write test file");
        path.to_str().expect("UTF-8 temp path").to_owned()
    }

    fn path(&self, name: &str) -> String {
        self.dir
            .join(name)
            .to_str()
            .expect("UTF-8 temp path")
            .to_owned()
    }

    fn spec(&self, kind: HelperKind, extra: &[&str]) -> HelperSpec {
        // Every fake helper runs without a UTF-8 locale, like under the launcher.
        let mut args: Vec<String> = ["LC_ALL=C", "PYTHONCOERCECLOCALE=0", "PYTHONUTF8=0"]
            .map(str::to_owned)
            .to_vec();
        args.push("python3".to_owned());
        args.push(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_helper.py").to_owned());
        args.push("--log".to_owned());
        args.push(
            log_path(&self.dir, kind)
                .to_str()
                .expect("UTF-8")
                .to_owned(),
        );
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        HelperSpec {
            kind,
            program: "env".into(),
            args,
        }
    }

    fn start(self, wakeword: &[&str], stt: &[&str], tts: &[&str]) -> Running {
        self.start_with(Duration::from_secs(30), wakeword, stt, tts)
    }

    fn start_with(
        self,
        ready_timeout: Duration,
        wakeword: &[&str],
        stt: &[&str],
        tts: &[&str],
    ) -> Running {
        let settings = LoopSettings {
            ready_timeout,
            follow_up: FOLLOW_UP,
            work_timeout: NO_WORK_TIMEOUT,
        };
        self.launch(settings, saying_done(), wakeword, stt, tts)
    }

    fn start_executing(self, work_timeout: Duration, executor: Executor, stt: &[&str]) -> Running {
        let settings = LoopSettings {
            ready_timeout: WAIT,
            follow_up: FOLLOW_UP,
            work_timeout,
        };
        self.launch(settings, executor, &[], stt, &[])
    }

    fn launch(
        self,
        settings: LoopSettings,
        executor: Executor,
        wakeword: &[&str],
        stt: &[&str],
        tts: &[&str],
    ) -> Running {
        let specs = vec![
            self.spec(HelperKind::Wakeword, wakeword),
            self.spec(HelperKind::Stt, stt),
            self.spec(HelperKind::Tts, tts),
        ];
        let (tx, rx) = mpsc::channel();
        let handle = {
            let tx = tx.clone();
            thread::spawn(move || run(&specs, &settings, executor, tx, rx))
        };
        Running {
            dir: self.dir,
            tx,
            handle,
        }
    }
}

fn log_path(dir: &Path, kind: HelperKind) -> PathBuf {
    dir.join(format!("{kind:?}.log"))
}

/// A supervisor running on its own thread.
struct Running {
    dir: PathBuf,
    tx: Sender<Incoming>,
    handle: JoinHandle<anyhow::Result<()>>,
}

impl Running {
    fn lines(&self, kind: HelperKind) -> Vec<String> {
        fs::read_to_string(log_path(&self.dir, kind))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn count(&self, kind: HelperKind, line: &str) -> usize {
        self.lines(kind).iter().filter(|l| *l == line).count()
    }

    fn spoken_lines(&self) -> usize {
        self.lines(HelperKind::Tts)
            .iter()
            .filter(|line| line.contains(r#""cmd":"speak""#))
            .count()
    }

    fn wait_for(&self, kind: HelperKind, line: &str) {
        let end = Instant::now() + WAIT;
        while self.count(kind, line) == 0 {
            assert!(
                Instant::now() < end,
                "{kind:?} never received {line}; got {:?}",
                self.lines(kind)
            );
            thread::sleep(POLL);
        }
    }

    fn inject(&self, kind: HelperKind, event: HelperEvent) {
        self.tx
            .send(Incoming::Event(kind, event))
            .expect("supervisor is running");
    }

    /// Repeats the wake word until `stt` listens, because the supervisor drops events
    /// while it waits for `ready`; `step` ignores the surplus ones.
    fn wake(&self) {
        let listen = r#"{"cmd":"listen"}"#;
        let before = self.count(HelperKind::Stt, listen);
        let end = Instant::now() + WAIT;
        while self.count(HelperKind::Stt, listen) == before {
            assert!(Instant::now() < end, "stt never received listen");
            // A send fails once a fatal error ended `run`; `outcome` reports that.
            let wake = HelperEvent::Wake { score: 0.9 };
            self.tx
                .send(Incoming::Event(HelperKind::Wakeword, wake))
                .ok();
            thread::sleep(POLL);
        }
    }

    fn exited(&self, kind: HelperKind) {
        self.tx
            .send(Incoming::Exited(kind))
            .expect("supervisor is running");
    }

    fn open_gate(&self, gate: &str) {
        fs::write(gate, "").expect("open gate");
    }

    fn say(&self, text: &str) {
        self.inject(HelperKind::Stt, HelperEvent::Final { text: text.into() });
    }

    /// Waits for `run` to return on its own.
    fn outcome(self) -> anyhow::Result<()> {
        let end = Instant::now() + WAIT;
        while !self.handle.is_finished() {
            assert!(Instant::now() < end, "supervisor did not stop");
            thread::sleep(POLL);
        }
        fs::remove_dir_all(&self.dir).ok();
        self.handle.join().expect("supervisor thread panicked")
    }

    fn shut_down(self) -> anyhow::Result<()> {
        self.tx
            .send(Incoming::Shutdown)
            .expect("supervisor is running");
        self.outcome()
    }
}

fn error_text(result: anyhow::Result<()>) -> String {
    format!("{:#}", result.expect_err("run must fail"))
}

#[test]
fn full_round_trip_of_an_unknown_sentence() {
    let rig = Rig::new("round-trip");
    let stt_script = rig.file("stt.json", HELLO_SCRIPT);
    let tts_script = rig.file("tts.json", SPEAK_SCRIPT);
    let nova = rig.start(&[], &["--script", &stt_script], &["--script", &tts_script]);

    nova.wake();

    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"pause"}"#);
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn umlauts_survive_the_round_trip() {
    let rig = Rig::new("umlauts");
    let stt_script = rig.file("stt.json", &listen_script("Öffne Größe „Äpfel“"));
    let nova = rig.start_executing(
        NO_WORK_TIMEOUT,
        echoing_the_app(),
        &["--script", &stt_script],
    );

    nova.wake();

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"„größe äpfel“"}"#,
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn silence_after_wake_gives_up() {
    let nova = Rig::new("silence").start(&[], &[], &[]);

    nova.wake();

    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn speech_start_disarms_the_deadline() {
    let nova = Rig::new("disarm").start(&[], &[], &[]);

    nova.wake();
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    thread::sleep(SEVERAL_FOLLOW_UPS);

    assert_eq!(nova.count(HelperKind::Stt, r#"{"cmd":"cancel"}"#), 0);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn barge_in_sends_stop() {
    let nova = Rig::new("barge").start(&[], &[], &[]);

    nova.wake();
    nova.say("Eins");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);

    nova.wait_for(HelperKind::Tts, r#"{"cmd":"stop"}"#);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn late_done_of_a_stopped_utterance_is_ignored() {
    let nova = Rig::new("late-done").start(&[], &[], &[]);

    nova.wake();
    nova.say("Eins");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    nova.wait_for(HelperKind::Tts, r#"{"cmd":"stop"}"#);
    nova.say("Zwei");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"4","text":"Erledigt."}"#,
    );

    nova.inject(HelperKind::Tts, HelperEvent::Done { id: "2".into() });
    thread::sleep(SEVERAL_FOLLOW_UPS);
    assert_eq!(
        nova.count(HelperKind::Stt, r#"{"cmd":"cancel"}"#),
        0,
        "no deadline may run while the second utterance plays"
    );

    nova.inject(HelperKind::Tts, HelperEvent::Done { id: "4".into() });
    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn overlapping_utterances_run_one_command_with_both_texts() {
    let nova = Rig::new("overlap").start_executing(NO_WORK_TIMEOUT, echoing_the_app(), &[]);

    nova.wake();
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    nova.say("Öffne");
    nova.say("Firefox");

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"„firefox“"}"#,
    );
    assert_eq!(nova.spoken_lines(), 1);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn command_reply_is_spoken() {
    let rig = Rig::new("reply");
    let stt_script = rig.file("stt.json", &listen_script("Öffne Firefox."));
    let nova = rig.start_executing(NO_WORK_TIMEOUT, saying_done(), &["--script", &stt_script]);

    nova.wake();

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn silent_reply_speaks_nothing() {
    let rig = Rig::new("silent");
    let stt_script = rig.file("stt.json", &listen_script("Pause."));
    let nova = rig.start_executing(
        NO_WORK_TIMEOUT,
        Arc::new(|_| Reply::Silent),
        &["--script", &stt_script],
    );

    nova.wake();

    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    assert_eq!(nova.spoken_lines(), 0);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn work_deadline_speaks_the_timeout() {
    let rig = Rig::new("work-deadline");
    let stt_script = rig.file("stt.json", &listen_script("Öffne Firefox."));
    let nova = rig.start_executing(
        WORK_TIMEOUT,
        saying_done_after(3 * WORK_TIMEOUT),
        &["--script", &stt_script],
    );

    nova.wake();

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Das hat zu lange gedauert."}"#,
    );
    thread::sleep(SEVERAL_FOLLOW_UPS);
    assert_eq!(nova.spoken_lines(), 1, "the late reply must stay unspoken");
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn barge_in_drops_the_reply() {
    let nova = Rig::new("barge-working").start_executing(
        NO_WORK_TIMEOUT,
        saying_done_after(Duration::from_millis(300)),
        &[],
    );

    nova.wake();
    nova.say("Öffne Firefox.");
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    thread::sleep(SEVERAL_FOLLOW_UPS);
    nova.say("Hallo");

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"3","text":"Erledigt."}"#,
    );
    assert_eq!(
        nova.spoken_lines(),
        1,
        "the abandoned reply must stay unspoken"
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn stale_reply_does_not_answer_the_next_run() {
    let executor: Executor = Arc::new(|command| match command {
        Command::Pause => {
            thread::sleep(Duration::from_millis(100));
            Reply::Say("Alt.".into())
        }
        Command::Next => {
            thread::sleep(Duration::from_millis(400));
            Reply::Say("Neu.".into())
        }
        other => panic!("unexpected command {other:?}"),
    });
    let nova = Rig::new("stale-reply").start_executing(NO_WORK_TIMEOUT, executor, &[]);

    nova.wake();
    nova.say("Pause.");
    nova.inject(HelperKind::Stt, HelperEvent::SpeechStart);
    nova.say("Weiter.");

    nova.wait_for(HelperKind::Tts, r#"{"cmd":"speak","id":"3","text":"Neu."}"#);
    assert_eq!(nova.spoken_lines(), 1);
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn panicking_executor_is_caught_by_the_deadline() {
    let rig = Rig::new("panic");
    let stt_script = rig.file("stt.json", &listen_script("Öffne Firefox."));
    let nova = rig.start_executing(
        WORK_TIMEOUT,
        Arc::new(|_| panic!("worker failed")),
        &["--script", &stt_script],
    );

    nova.wake();

    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Das hat zu lange gedauert."}"#,
    );
    nova.inject(HelperKind::Tts, HelperEvent::Done { id: "2".into() });
    nova.say("Hallo");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"4","text":"Das hat zu lange gedauert."}"#,
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn crashed_helper_restarts_once_and_loop_recovers() {
    let rig = Rig::new("restart");
    let crash_file = rig.path("crashed");
    let nova = rig.start(
        &[],
        &["--crash-after", "1", "--crash-count-file", &crash_file],
        &[],
    );

    nova.wake();

    nova.wait_for(HelperKind::Tts, r#"{"cmd":"stop"}"#);
    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    nova.wake();
    nova.say("Wieder da");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn helpers_crashing_together_restart_and_loop_recovers() {
    let rig = Rig::new("together");
    let wakeword_file = rig.path("wakeword-crashed");
    let stt_file = rig.path("stt-crashed");
    let nova = rig.start(
        &["--crash-after", "1", "--crash-count-file", &wakeword_file],
        &["--crash-after", "1", "--crash-count-file", &stt_file],
        &[],
    );

    nova.wake();

    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    nova.wait_for(HelperKind::Stt, r#"{"cmd":"cancel"}"#);
    nova.wake();
    nova.say("Beide da");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );
    nova.shut_down().expect("clean shutdown");
}

#[test]
fn exit_during_restart_wait_restarts_that_helper_too() {
    let rig = Rig::new("restart-wait");
    let stt_crash = rig.path("stt-crashed");
    let tts_crash = rig.path("tts-crashed");
    let exit_gate = rig.path("exit-gate");
    let ready_gate = rig.path("ready-gate");
    let stt = [
        "--crash-after",
        "1",
        "--crash-count-file",
        &stt_crash,
        "--ready-gate",
        &ready_gate,
    ];
    let tts = [
        "--crash-count-file",
        &tts_crash,
        "--exit-gate",
        &exit_gate,
        "--ready-gate",
        &ready_gate,
    ];
    let nova = rig.start(&[], &stt, &tts);

    nova.wake();
    nova.wait_for(HelperKind::Stt, "gate");
    nova.open_gate(&exit_gate);
    nova.wait_for(HelperKind::Tts, "gate");
    nova.open_gate(&ready_gate);

    nova.wait_for(HelperKind::Wakeword, r#"{"cmd":"resume"}"#);
    nova.wake();
    nova.say("Alle da");
    nova.wait_for(
        HelperKind::Tts,
        r#"{"cmd":"speak","id":"2","text":"Erledigt."}"#,
    );

    // The restart of the other helper counted: its next exit is the second one.
    nova.exited(HelperKind::Tts);
    let text = error_text(nova.outcome());
    assert!(text.contains("tts"), "{text}");
    assert!(text.contains("second"), "{text}");
}

#[test]
fn second_exit_during_restart_wait_is_fatal_and_names_helper() {
    let rig = Rig::new("second-in-wait");
    let crash_file = rig.path("crashed");
    let exit_gate = rig.path("exit-gate");
    let ready_gate = rig.path("ready-gate");
    let stt = [
        "--crash-after",
        "1",
        "--crash-count-file",
        &crash_file,
        "--ready-gate",
        &ready_gate,
    ];
    // Without a crash count file the replacement exits right away as well.
    let nova = rig.start(&[], &stt, &["--exit-gate", &exit_gate]);

    nova.wake();
    nova.wait_for(HelperKind::Stt, "gate");
    nova.open_gate(&exit_gate);

    let text = error_text(nova.outcome());
    assert!(text.contains("tts"), "{text}");
    assert!(text.contains("second"), "{text}");
}

#[test]
fn shutdown_during_restart_wait_is_clean() {
    let rig = Rig::new("shutdown-in-wait");
    let crash_file = rig.path("crashed");
    let gate = rig.path("gate");
    let nova = rig.start(
        &[],
        &[
            "--crash-after",
            "1",
            "--crash-count-file",
            &crash_file,
            "--ready-gate",
            &gate,
        ],
        &[],
    );

    nova.wake();
    nova.wait_for(HelperKind::Stt, "gate");

    nova.shut_down().expect("clean shutdown");
}

#[test]
fn second_crash_is_fatal_and_names_helper() {
    let rig = Rig::new("second-crash");
    let stt_script = rig.file(
        "stt.json",
        r#"{"auf_befehl":{
            "listen":[{"event":"error","message":"erste Meldung"}],
            "cancel":[{"event":"error","message":"letzte Meldung"}]}}"#,
    );
    let nova = rig.start(&[], &["--script", &stt_script, "--crash-after", "1"], &[]);

    nova.wake();

    let text = error_text(nova.outcome());
    assert!(text.contains("stt"), "{text}");
    assert!(text.contains("letzte Meldung"), "{text}");
}

#[test]
fn lost_microphone_ends_the_loop() {
    let rig = Rig::new("microphone");
    let crash_file = rig.path("crashed");
    let nova = rig.start(
        &[],
        &[
            "--crash-after",
            "1",
            "--crash-count-file",
            &crash_file,
            "--fail-with",
            "Mikrofon nicht gefunden",
        ],
        &[],
    );

    nova.wake();

    let text = error_text(nova.outcome());
    assert!(text.contains("stt"), "{text}");
    assert!(text.contains("Mikrofon nicht gefunden"), "{text}");
}

#[test]
fn missing_ready_is_fatal() {
    let nova = Rig::new("no-ready").start_with(Duration::from_secs(2), &["--no-ready"], &[], &[]);

    let text = error_text(nova.outcome());
    assert!(text.contains("wakeword"), "{text}");
}

#[test]
fn helper_exit_during_startup_is_fatal() {
    let nova = Rig::new("startup").start(&[], &[], &["--fail-with", "Stimme fehlt"]);

    let text = error_text(nova.outcome());
    assert!(text.contains("tts"), "{text}");
    assert!(text.contains("Stimme fehlt"), "{text}");
}

#[test]
fn shutdown_while_waiting_for_ready_is_clean() {
    let nova = Rig::new("early-shutdown").start(&["--no-ready"], &[], &[]);

    nova.shut_down().expect("clean shutdown");
}

#[test]
fn specs_use_project_root_and_config() {
    let config_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/nova.toml");
    let mut config = Config::load(Path::new(config_path)).expect("repo config");
    config.wakeword.modell = "models/hey.onnx".into();
    config.wakeword.schwelle = 0.25;
    config.audio.eingabe = "ein".into();
    config.audio.ausgabe = "aus".into();
    config.spracherkennung.whisper_modell = "tiny".into();
    config.spracherkennung.geraet = "cpu".into();
    config.stimme.piper_modell = "stimme".into();

    let specs = helper_specs(&config, Path::new("/r"));

    let args = |module: &str, extra: &[&str]| -> Vec<String> {
        let mut args = ["run", "--project", "/r/helpers", "python", "-m"]
            .map(str::to_owned)
            .to_vec();
        args.push(format!("nova_helpers.{module}"));
        args.extend(extra.iter().map(|arg| (*arg).to_owned()));
        args
    };
    let got: Vec<_> = specs
        .iter()
        .map(|s| (s.kind, s.program.clone(), s.args.clone()))
        .collect();
    assert_eq!(
        got,
        vec![
            (
                HelperKind::Wakeword,
                PathBuf::from("uv"),
                args(
                    "wakeword",
                    &[
                        "--model",
                        "/r/models/hey.onnx",
                        "--threshold",
                        "0.25",
                        "--input",
                        "ein"
                    ]
                )
            ),
            (
                HelperKind::Stt,
                PathBuf::from("uv"),
                args(
                    "stt",
                    &["--model", "tiny", "--device", "cpu", "--input", "ein"]
                )
            ),
            (
                HelperKind::Tts,
                PathBuf::from("uv"),
                args("tts", &["--voice", "stimme", "--output", "aus"])
            ),
        ]
    );
}
