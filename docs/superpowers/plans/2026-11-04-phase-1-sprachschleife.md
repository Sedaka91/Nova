# Phase 1: Sprachschleife Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `nova` erkennt „Hey Nova“, hört einen Satz, spricht ihn über Piper nach und lässt sich dabei unterbrechen.

**Architecture:** Ein Rust-Prozess startet drei Python-Helfer und koordiniert sie über JSON Lines. Eine reine Zustandsmaschine entscheidet, ein dünner Treiber führt aus. Das Rust-Programm fasst kein Audio an; `wakeword` und `stt` lesen `nova_mic`, `tts` schreibt auf `nova_sink`.

**Tech Stack:** Rust 2024, serde, serde_json, ctrlc, Threads mit `std::sync::mpsc`; Python 3.12 mit uv, openWakeWord (ONNX), faster-whisper, piper-tts, sounddevice; PipeWire `libpipewire-module-echo-cancel`.

**Spec:** `docs/superpowers/specs/2026-11-04-phase-1-sprachschleife-design.md`

## Global Constraints

- Bezeichner und Code-Kommentare englisch; Konfiguration und alles, was Nova sagt oder anzeigt, deutsch.
- Vor jedem Commit: `cargo fmt --all && cargo clippy --all-targets && cargo test`, zusätzlich `cd helpers && uv run pytest`, sobald Python-Code berührt ist.
- `unsafe_code = "forbid"` bleibt. Kein `sudo`, kein Root.
- Jede neue Option wird in `nova-core` typisiert und validiert (`deny_unknown_fields`).
- Fristen und Werte: `nachfrage_s = 8.0`, `schwelle = 0.5`, Bereitschaft 60 s, VAD-Vorlauf 0,5 s, Stille 0,8 s, Satz-Obergrenze 30 s, Wake-Sperrzeit 1 s, Blockgröße 80 ms, 16 kHz mono.
- Gesprochener Text: `Du sagtest: {text}`.
- Logs nur nach stderr. stdout gehört dem Protokoll.
- Keine Zugangsdaten. Keine Änderung an `~/.config/pipewire` durch Code oder Agent: Das Aktivieren macht der Nutzer.
- Commits klein und thematisch auf Branch `phase-1-sprachschleife`, Nachrichten deutsch, mit Zeile `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. Whisper erfindet bei Stille oder Rauschen Sätze („Untertitel im Auftrag …“): Solche Segmente dürfen nicht ausgesprochen werden, `final` bleibt leer (Task 8).
2. Sprechen beginnt genau beim Fristablauf: Ein spätes `speech_start` oder `final` in `Waiting` ändert nichts und spricht nichts (Task 3).
3. Umlaute unter dem Launcher ohne UTF-8-Locale: `Du sagtest: Größe „Äpfel“` kommt unverändert an, auch bei `LC_ALL=C` (Task 5).
4. USB-Mikrofon wird im Betrieb abgezogen: Der Helfer meldet `error` und endet mit Exit-Code ungleich null, Nova beendet sich klar statt zu hängen (Task 5, Task 7).
5. Bibliotheken drucken auf stdout (tqdm, Piper, ONNX): Das Protokoll bleibt lesbar, fremde Zeilen landen auf stderr (Task 5).

---

## Dateien

```
crates/nova-core/src/config.rs       WakeWord, Audio, Dialog + Validierung
crates/nova-app/src/lib.rs           Modulliste
crates/nova-app/src/protocol.rs      Befehle, Ereignisse
crates/nova-app/src/voice_loop.rs    reine Zustandsmaschine
crates/nova-app/src/helper.rs        ein Kindprozess
crates/nova-app/src/supervisor.rs    Treiber: Helfer, Bereitschaft, Neustart, Frist
crates/nova-app/src/main.rs          Verdrahtung, Ctrl+C
crates/nova-app/tests/supervisor.rs  Integrationstests
crates/nova-app/tests/fixtures/fake_helper.py
helpers/nova_helpers/{protocol,audio,setup,tts,wakeword,stt}.py
helpers/tests/
deploy/pipewire/nova-echo-cancel.conf
training/wakeword/{pyproject.toml,hey_nova.yml,run.sh,README.md}
models/hey_nova.onnx
```

---

### Task 1: Konfiguration erweitern

**Files:**
- Modify: `crates/nova-core/src/config.rs`, `config/nova.toml`

**Interfaces:**
- Produces: `Config.wakeword: WakeWord { modell: String, schwelle: f32 }`, `Config.audio: Audio { eingabe: String, ausgabe: String }`, `Config.dialog: Dialog { nachfrage_s: f32 }`; `WakeWord::model_arg(&self, root: &Path) -> String` (enthält `modell` ein `/`: `root.join(modell)`, sonst `modell` unverändert).

- [ ] **Step 1: Failing Tests schreiben** (`Config` aus einem TOML-String bauen; `Config::load` dafür in `parse(&str)` und Lesen trennen, wenn nötig)

```rust
#[test] fn rejects_threshold_above_one()   // schwelle = 1.5 -> Err(Invalid)
#[test] fn rejects_zero_follow_up()        // nachfrage_s = 0.0 -> Err(Invalid)
#[test] fn rejects_empty_audio_device()    // eingabe = "" -> Err(Invalid)
#[test] fn model_with_slash_is_a_path()    // "models/x.onnx", root "/r" -> "/r/models/x.onnx"
#[test] fn bare_model_name_stays_a_name()  // "hey_jarvis" -> "hey_jarvis"
```

Außerdem `shipped_config_is_valid` auf `aktivierungswort == "hey nova"` ändern.

- [ ] **Step 2:** `cargo test -p nova-core` ausführen. Erwartet: FAIL, Felder fehlen.

- [ ] **Step 3:** Typen und Prüfungen ergänzen; `config/nova.toml` wie in der Spec ändern, aber vorerst `modell = "hey_jarvis"` (Wechsel in Task 11).

- [ ] **Step 4:** `cargo test -p nova-core` ausführen. Erwartet: PASS.

- [ ] **Step 5:** Committen: `Konfiguration für Wake Word, Audio und Dialog ergänzen`.

---

### Task 2: Protokolltypen

**Files:**
- Create: `crates/nova-app/src/lib.rs`, `crates/nova-app/src/protocol.rs`
- Modify: `crates/nova-app/Cargo.toml`, `Cargo.toml` (`serde`, `serde_json` als Workspace-Abhängigkeit)

**Interfaces:**
- Produces:

```rust
pub enum HelperKind { Wakeword, Stt, Tts }

#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum WakewordCommand { Pause, Resume }
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum SttCommand { Listen, Cancel }
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum TtsCommand { Speak { id: String, text: String }, Stop }

#[serde(tag = "event", rename_all = "snake_case")]
pub enum HelperEvent {
    Ready, Error { message: String }, Wake { score: f32 },
    SpeechStart, Partial { text: String }, Final { text: String },
    Started { id: String }, Done { id: String },
}
```

- [ ] **Step 1: Failing Tests schreiben**

```rust
#[test] fn speak_serializes_like_the_protocol() {
    // {"cmd":"speak","id":"1","text":"Hallo"}
}
#[test] fn parses_every_event() {
    // je eine Zeile pro Variante aus docs/architektur.md
}
#[test] fn unknown_event_is_an_error() {}
```

- [ ] **Step 2:** `cargo test -p nova-app protocol` ausführen. Erwartet: FAIL.

- [ ] **Step 3:** Typen implementieren. `main.rs` nutzt weiterhin nur `nova_core`.

- [ ] **Step 4:** `cargo test -p nova-app protocol` ausführen. Erwartet: PASS.

- [ ] **Step 5:** Committen: `Protokolltypen der Hilfsprozesse anlegen`.

---

### Task 3: Zustandsmaschine

**Files:**
- Create: `crates/nova-app/src/voice_loop.rs`

**Interfaces:**
- Consumes: `HelperEvent`, `WakewordCommand`, `SttCommand` aus Task 2.
- Produces:

```rust
pub enum State { Waiting, Listening, Speaking, FollowUp }
pub enum Input { Event(HelperEvent), DeadlineExpired }
pub enum Action {
    Wakeword(WakewordCommand), Stt(SttCommand),
    Speak(String), StopSpeaking, ArmDeadline, DisarmDeadline,
}
pub fn step(state: State, input: Input) -> (State, Vec<Action>);
```

`Waiting` = Lauschen, `Listening` = Zuhören, `Speaking` = Sprechen, `FollowUp` = Nachfrage. Die Tabelle steht in der Spec. `Action::Speak` enthält den fertigen Satz, die `id` vergibt der Treiber. „Mit Text“ heißt: nach `trim()` nicht leer.

- [ ] **Step 1: Failing Tests schreiben.** Je ein Test pro Tabellenzeile, dazu:

```rust
#[test] fn wake_arms_deadline_and_starts_stt() {
    // Waiting + Wake -> Listening, [Wakeword(Pause), Stt(Listen), ArmDeadline]
}
#[test] fn final_is_echoed_with_template() {
    // Listening + Final("Hallo") -> Speaking, enthält Speak("Du sagtest: Hallo")
}
#[test] fn whitespace_final_counts_as_empty() {}
#[test] fn barge_in_stops_speech_without_deadline() {
    // Speaking + SpeechStart -> Listening, [StopSpeaking], kein ArmDeadline
}
#[test] fn final_while_speaking_is_dropped() {
    // Speaking + Final("Echo") -> Speaking, keine Aktionen
}
#[test] fn speech_start_in_follow_up_disarms_deadline() {}
#[test] fn empty_final_in_follow_up_rearms_deadline() {}
#[test] fn late_speech_events_in_waiting_change_nothing() {
    // Waiting + SpeechStart / Final("x") / DeadlineExpired -> Waiting, keine Aktionen
}
#[test] fn late_tts_done_while_listening_is_ignored() {}
```

- [ ] **Step 2:** `cargo test -p nova-app voice_loop` ausführen. Erwartet: FAIL.

- [ ] **Step 3:** `step` als `match (state, input)` implementieren; der Rest-Arm gibt den Zustand unverändert zurück und loggt auf `debug`.

- [ ] **Step 4:** `cargo test -p nova-app voice_loop` ausführen. Erwartet: PASS.

- [ ] **Step 5:** Committen: `Zustandsmaschine der Sprachschleife anlegen`.

---

### Task 4: Kindprozess

**Files:**
- Create: `crates/nova-app/src/helper.rs`, `crates/nova-app/tests/fixtures/fake_helper.py`

**Interfaces:**
- Consumes: `HelperKind`, `HelperEvent`.
- Produces:

```rust
pub struct HelperSpec { pub kind: HelperKind, pub program: PathBuf, pub args: Vec<String> }
pub enum Incoming { Event(HelperKind, HelperEvent), Exited(HelperKind) }
pub struct Helper { /* child, stdin */ }
impl Helper {
    pub fn spawn(spec: &HelperSpec, tx: Sender<Incoming>) -> io::Result<Self>;
    pub fn send(&mut self, command: &impl Serialize) -> io::Result<()>;
    pub fn stop(&mut self); // stdin schließen, bis 2 s warten, dann kill
}
```

Ein Thread liest stdout zeilenweise und schickt nach EOF `Exited`. Nicht parsebare Zeilen ergeben `tracing::warn!`. Ein zweiter Thread leitet stderr auf `tracing::info!` mit dem Helfernamen um. `Drop` ruft `stop`.

`fake_helper.py` ist ein Skript in reinem Python, das über Argumente gesteuert wird und `ready` meldet. `--script` nennt eine JSON-Datei mit `{"auf_befehl": {"listen": [Ereignisse], ...}}`. Dazu kommen `--crash-after N` (nach N Befehlen `sys.exit(1)`), `--crash-count-file PFAD` (Absturz nur beim ersten Lauf), `--noise` (druckt `kein json`), `--no-ready` und `--log PFAD` (empfangene Befehle anhängen).

- [ ] **Step 1: Failing Tests schreiben** (im `#[cfg(test)]`-Block, `python3` mit Fixture starten)

```rust
#[test] fn receives_ready_event() {}
#[test] fn skips_garbage_lines() {}          // --noise: danach kommt Ready trotzdem
#[test] fn reports_exit() {}                 // --crash-after 1: Exited nach einem Befehl
#[test] fn stop_ends_process_on_closed_stdin() {}
```

- [ ] **Step 2:** `cargo test -p nova-app helper` ausführen. Erwartet: FAIL.

- [ ] **Step 3:** `Helper` implementieren.

- [ ] **Step 4:** `cargo test -p nova-app helper` ausführen. Erwartet: PASS.

- [ ] **Step 5:** Committen: `Kindprozesse für Hilfsprozesse führen`.

---

### Task 5: Treiber, Python-Protokoll und Verdrahtung

Rust- und Python-Seite des Protokolls werden hier gemeinsam gegen dieselben Beispiele abgesichert.

**Files:**
- Create: `crates/nova-app/src/supervisor.rs`, `crates/nova-app/tests/supervisor.rs`, `helpers/nova_helpers/protocol.py`, `helpers/tests/test_protocol.py`
- Modify: `crates/nova-app/src/main.rs`, `helpers/pyproject.toml`, `Cargo.toml` (`ctrlc`)

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:

```rust
pub struct LoopSettings { pub ready_timeout: Duration, pub follow_up: Duration }
pub enum Signal { Helper(Incoming), Shutdown }
/// Runs until Shutdown; Err on a fatal helper failure.
pub fn run(specs: &[HelperSpec], settings: &LoopSettings, tx: Sender<Signal>, rx: Receiver<Signal>)
    -> anyhow::Result<()>;
pub fn helper_specs(config: &Config, root: &Path) -> Vec<HelperSpec>;
```

```python
# helpers/nova_helpers/protocol.py
def claim_stdout() -> TextIO        # dupliziert fd 1 für das Protokoll, lenkt fd 1 auf stderr um
def read_commands(stream: TextIO) -> Iterator[dict]   # beendet bei EOF, überspringt kaputte Zeilen
class Emitter:
    def __init__(self, stream: TextIO): ...
    def __call__(self, event: str, **fields) -> None  # thread-sicher, ensure_ascii=False, flush
def setup_logging(name: str) -> None
```

`stdin` und der Protokoll-Stream werden fest auf UTF-8 gestellt (`reconfigure(encoding="utf-8")` beziehungsweise `encoding="utf-8"`).

Treiberablauf: alle Helfer starten, auf `ready` warten (`ready_timeout`), dann Schleife mit `recv_timeout` bis zur Frist.
- Ein `Exited` während des Wartens auf `ready` ist fatal.
- `Exited` im Betrieb: Beim ersten Mal pro Helfer neu starten, `ready` abwarten, `tts stop`, `stt cancel`, `wakeword resume` senden, Frist löschen, `State::Waiting`. Beim zweiten Mal `Err` mit dem Helfernamen und der letzten `error`-Meldung.
- Fehlgeschlagenes `send` wird nur geloggt; das folgende `Exited` regelt es.
- `Speak` vergibt fortlaufende `id`s.

`helper_specs` erzeugt `uv run --project {root}/helpers python -m nova_helpers.{name}` plus:

| Helfer | Argumente |
|---|---|
| `wakeword` | `--model {model_arg} --threshold {schwelle} --input {eingabe}` |
| `stt` | `--model {whisper_modell} --device {geraet} --input {eingabe}` |
| `tts` | `--voice {piper_modell} --output {ausgabe}` |

`main.rs`: Wurzel ist `Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")`. Ctrl+C schickt über `ctrlc` ein `Signal::Shutdown`. Ein `Err` aus `run` führt zu Exit-Code ungleich null.

- [ ] **Step 1: Failing Rust-Tests schreiben** in `tests/supervisor.rs` (Fake-Helfer, `follow_up` 200 ms, `ready_timeout` 2 s; Prüfung über die `--log`-Dateien)

```rust
#[test] fn full_echo_round_trip() {
    // wake -> listen -> final "Hallo" -> tts erhält speak "Du sagtest: Hallo" -> done
    // -> nach Fristablauf stt cancel, wakeword resume -> Shutdown -> Ok
}
#[test] fn umlauts_survive_the_round_trip() {
    // "Größe „Äpfel“" mit LC_ALL=C im Fake-Helfer
}
#[test] fn barge_in_sends_stop() {}
#[test] fn crashed_helper_restarts_once_and_loop_recovers() {
    // --crash-count-file: danach wieder wake -> listen möglich
}
#[test] fn second_crash_is_fatal_and_names_helper() {}
#[test] fn missing_ready_is_fatal() {}          // --no-ready
#[test] fn helper_exit_during_startup_is_fatal() {} // zuvor gemeldete error-Meldung steht im Err
#[test] fn specs_use_project_root_and_config() {}
```

- [ ] **Step 2: Failing Python-Tests schreiben** in `helpers/tests/test_protocol.py`

```python
def test_emit_keeps_umlauts_as_utf8(): ...           # kein \u-Escape, Stream-Bytes UTF-8
def test_read_commands_skips_garbage_and_stops_at_eof(): ...
def test_claimed_stdout_ignores_library_prints(): ...
    # Subprozess: claim_stdout(), print("lärm"), Emitter ruft "ready":
    # stdout hat genau eine JSON-Zeile, "lärm" steht auf stderr
def test_utf8_with_c_locale(): ...                   # Subprozess mit LC_ALL=C
```

`pyproject.toml` bekommt dafür `requires-python = ">=3.12"`, `.python-version` mit `3.12` und die Dev-Gruppe mit `pytest`.

- [ ] **Step 3:** Beide Testläufe ausführen. Erwartet: FAIL.

- [ ] **Step 4:** Implementieren.

- [ ] **Step 5:** `cargo test && cd helpers && uv run pytest` ausführen. Erwartet: PASS.

- [ ] **Step 6:** Committen: `Treiber der Sprachschleife und Python-Protokoll anlegen`.

---

### Task 6: Python-Abhängigkeiten, Audio und Setup

Hier fallen die offenen technischen Punkte aus der Spec. Ergebnisse kommen in die Commit-Nachricht.

**Files:**
- Create: `helpers/nova_helpers/audio.py`, `helpers/nova_helpers/setup.py`, `helpers/tests/test_audio.py`
- Modify: `helpers/pyproject.toml`, `helpers/uv.lock`

**Interfaces:**
- Produces:

```python
SAMPLE_RATE = 16_000
BLOCK_SAMPLES = 1_280      # 80 ms
def data_dir() -> Path     # $XDG_DATA_HOME/nova, sonst ~/.local/share/nova
class Microphone:          # iterierbar, liefert np.ndarray[int16] der Länge BLOCK_SAMPLES
    def __init__(self, node: str): ...
    def __enter__/__exit__
class AudioDeviceError(Exception): ...   # Text nennt docs: „Echo-Modul aktivieren, siehe README“
class Speaker:
    def __init__(self, node: str, rate: int): ...
    def write(self, pcm: np.ndarray) -> None
    def abort(self) -> None    # verwirft Gepuffertes sofort
```

- [ ] **Step 1: Abhängigkeiten hinzufügen.** `uv add numpy sounddevice faster-whisper piper-tts openwakeword`, mit `tflite-runtime` per `[tool.uv] override-dependencies = ["tflite-runtime; python_version < '0'"]` ausgeschlossen. Erwartet: `uv sync` läuft durch. Schlägt das fehl, in `.python-version` und `requires-python` auf 3.11 wechseln.

- [ ] **Step 2: Laufzeit prüfen.** Importe von `openwakeword`, `faster_whisper`, `piper` ausführen und `WhisperModel("small", device="cuda", compute_type="int8_float16")` laden. Ergebnis festhalten: GPU nutzbar oder CPU. Prüfen, wie `faster_whisper` die Silero-VAD bereitstellt (`faster_whisper.vad`).

- [ ] **Step 3: Failing Tests schreiben** in `helpers/tests/test_audio.py` (`sounddevice` per Monkeypatch ersetzt)

```python
def test_microphone_sets_pipewire_node(monkeypatch): ...  # PIPEWIRE_NODE == "nova_mic", Gerät "pipewire"
def test_missing_device_raises_with_hint(monkeypatch): ...
def test_blocks_have_fixed_length(monkeypatch): ...
def test_stream_failure_ends_iteration_with_error(monkeypatch): ... # abgezogenes Mikrofon
def test_setup_targets_data_dir(monkeypatch, tmp_path): ...
```

- [ ] **Step 4:** `audio.py` und `setup.py` implementieren. `setup.py` lädt die Stimme nach `data_dir()/piper/` und die openWakeWord-Basismodelle samt `hey_jarvis` nach `data_dir()/openwakeword/` (`openwakeword.utils.download_models`). Das ergänzt die Spec, weil der Helfer sonst selbst laden müsste.

- [ ] **Step 5:** `cd helpers && uv run pytest` ausführen. Erwartet: PASS.

- [ ] **Step 6: Am echten Gerät prüfen.** Ohne Echo-Modul ein vorhandener Knoten: `PIPEWIRE_NODE=<JLAB-Knoten>` öffnet die Quelle und liefert Pegel über null. Klappt das nicht, stellt `audio.py` intern auf `pw-record`/`pw-play` um, wie in der Spec vorgesehen; die Schnittstelle bleibt.

- [ ] **Step 7:** `uv run --project helpers python -m nova_helpers.setup` ausführen. Erwartet: Stimme (63 MB) und Modelle liegen unter `~/.local/share/nova/`.

- [ ] **Step 8:** Committen: `Audio-Zugriff und Modell-Setup für die Helfer anlegen`.

---

### Task 7: `tts` und `wakeword`

**Files:**
- Create: `helpers/nova_helpers/tts.py`, `helpers/nova_helpers/wakeword.py`, `helpers/tests/test_tts.py`, `helpers/tests/test_wakeword.py`

**Interfaces:**
- Consumes: `protocol`, `audio` (Tasks 5, 6).
- Produces:

```python
class TtsWorker:
    def __init__(self, emit, synthesize: Callable[[str], Iterable[np.ndarray]], speaker): ...
    def handle(self, command: dict) -> None
    def close(self) -> None
class WakeDetector:
    def __init__(self, emit, predict: Callable[[np.ndarray], float], reset: Callable[[], None],
                 threshold: float, clock=time.monotonic): ...
    def handle(self, command: dict) -> None
    def process_block(self, block: np.ndarray) -> None
```

Jedes Modul hat `main()` mit `argparse` wie in Task 5. Die Eingabeschleife liest stdin im Hauptthread, Audio läuft in einem Thread. EOF beendet den Prozess mit 0, `KeyboardInterrupt` ebenso. Ein `AudioDeviceError` meldet `error` und beendet mit 1.

- [ ] **Step 1: Failing Tests schreiben**

```python
# test_tts.py
def test_speak_emits_started_then_done_in_order(): ...
def test_queue_keeps_order_of_two_utterances(): ...
def test_stop_aborts_clears_queue_and_sends_no_done(): ...
    # speaker.abort aufgerufen, zweite Äußerung nie synthetisiert
def test_stop_when_idle_is_harmless(): ...
# test_wakeword.py
def test_score_above_threshold_emits_wake(): ...      # score-Feld vorhanden
def test_second_wake_within_one_second_is_blocked(): ...
def test_pause_drops_scores(): ...
def test_resume_resets_model_buffer(): ...
def test_device_error_exits_nonzero(): ...            # Subprozess, Audio-Fake wirft
```

- [ ] **Step 2:** `cd helpers && uv run pytest tests/test_tts.py tests/test_wakeword.py` ausführen. Erwartet: FAIL.

- [ ] **Step 3:** Implementieren. Piper: `PiperVoice.load` mit Modell aus `data_dir()/piper/`, blockweises `synthesize`. openWakeWord: `Model(wakeword_models=[…], inference_framework="onnx", melspec_model_path=…, embedding_model_path=…)`; ein Modellname wird auf `data_dir()/openwakeword/{name}*.onnx` aufgelöst, ein Pfad direkt genutzt. Fehlt eine Datei: `error` mit Hinweis auf `nova_helpers.setup`, Exit 1.

- [ ] **Step 4:** Tests erneut ausführen. Erwartet: PASS.

- [ ] **Step 5: Manuell prüfen.** `echo '{"cmd":"speak","id":"1","text":"Grüß dich."}' | uv run --project helpers python -m nova_helpers.tts --voice de_DE-thorsten-medium --output <JLAB-Senke>`: Stimme hörbar, `started` und `done` auf stdout.

- [ ] **Step 6:** Committen: `Hilfsprozesse für Sprachausgabe und Wake Word anlegen`.

---

### Task 8: `stt`

**Files:**
- Create: `helpers/nova_helpers/stt.py`, `helpers/tests/test_stt.py`

**Interfaces:**
- Consumes: `protocol`, `audio`.
- Produces:

```python
class Segmenter:   # reine Logik: entscheidet nur aus Blöcken und VAD-Wert
    def __init__(self, emit, is_speech: Callable[[np.ndarray], bool],
                 transcribe: Callable[[np.ndarray], str]): ...
    def handle(self, command: dict) -> None
    def process_block(self, block: np.ndarray) -> None
def load_model(name: str, device: str) -> WhisperModel  # "auto": CUDA, sonst CPU int8
def text_of(segments) -> str     # verwirft Segmente mit no_speech_prob > 0.6
```

`Segmenter` zählt in 80-ms-Blöcken: Vorlauf 6 Blöcke, Stille 10 Blöcke, Obergrenze 375 Blöcke, `partial` alle 12 Blöcke.

- [ ] **Step 1: Failing Tests schreiben** (Fakes für VAD und Transkription)

```python
def test_inactive_until_listen(): ...               # keine Ereignisse vor listen
def test_speech_start_once_per_utterance(): ...
def test_preroll_is_part_of_transcribed_audio(): ...  # 6 Blöcke vor Sprachbeginn enthalten
def test_final_after_800ms_silence(): ...
def test_silence_shorter_than_limit_keeps_utterance(): ...
def test_partial_about_every_second(): ...
def test_forced_final_after_30s(): ...
def test_keeps_listening_after_final(): ...         # zweite Äußerung ohne neues listen
def test_cancel_drops_buffer_and_stops(): ...       # kein final
def test_listen_twice_is_harmless(): ...
def test_no_speech_segments_are_dropped(): ...      # text_of: no_speech_prob 0.9 -> ""
def test_auto_falls_back_to_cpu(monkeypatch): ...   # CUDA-Laden wirft -> CPU int8, Log nennt Wahl
def test_cuda_failure_is_hard(monkeypatch): ...     # geraet="cuda" wirft weiter
```

- [ ] **Step 2:** `cd helpers && uv run pytest tests/test_stt.py` ausführen. Erwartet: FAIL.

- [ ] **Step 3:** Implementieren. Transkription läuft in einem Worker-Thread mit dem neuesten Auftrag, damit der Audio-Thread nie blockiert: Veraltete `partial`-Aufträge werden übersprungen, `final` nie. `language="de"`, `beam_size=1` für `partial`, `beam_size=5` für `final`.

- [ ] **Step 4:** Tests erneut ausführen. Erwartet: PASS.

- [ ] **Step 5: Manuell prüfen.** `stt` mit `--input <JLAB-Quelle>` starten, `{"cmd":"listen"}` senden, einen Satz sagen. Erwartet: `speech_start`, `partial`, `final` mit korrektem Text. Zeit von Satzende bis `final` messen; über 1,5 s auf CPU dem Nutzer melden, bevor es weitergeht.

- [ ] **Step 6:** Committen: `Hilfsprozess für Spracherkennung anlegen`.

---

### Task 9: PipeWire und erster Gesamtlauf

**Files:**
- Create: `deploy/pipewire/nova-echo-cancel.conf`
- Modify: `README.md`

- [ ] **Step 1:** Konfiguration schreiben: `libpipewire-module-echo-cancel`, `library.name = aec/libspa-aec-webrtc`, `source.props` mit `node.name = "nova_mic"`, `sink.props` mit `node.name = "nova_sink"`, `capture.props` mit `target.object` auf `alsa_input.usb-C-Media_Electronics_Inc._JLAB_TALK_PRO_MICROPHONE-00.analog-stereo`, `playback.props` mit `target.object` auf `alsa_output.pci-0000_00_1f.3.analog-stereo`. Keine Priorität, die die Standardgeräte ändert.

- [ ] **Step 2:** Einrichtung in `README.md` beschreiben: Symlink nach `~/.config/pipewire/pipewire.conf.d/`, `systemctl --user restart pipewire`.

- [ ] **Step 3:** Nutzer aktiviert das Modul. Erwartet: `wpctl status` zeigt `nova_mic` und `nova_sink`, `wpctl status` nennt weiterhin dieselben Standardgeräte.

- [ ] **Step 4: Gesamtlauf mit `hey_jarvis`.** `cargo run -p nova-app`. Der Nutzer sagt „Hey Jarvis“ und einen Satz. Erwartet: Echo über Lautsprecher, Log zeigt alle Zustandswechsel, Ctrl+C hinterlässt keine Prozesse (`pgrep -f nova_helpers` leer).

- [ ] **Step 5:** Committen: `PipeWire-Echounterdrückung für Nova bereitstellen`.

---

### Task 10: „Hey Nova“ trainieren

Läuft unabhängig von den Tasks 5–9 und blockiert keinen davon. Der Nutzer hat die Downloads laut Spec (ca. 23–27 GB) freigegeben. Vor dem Start prüfen: mindestens 40 GB frei auf `/home`, GPU nicht von Ollama oder Spielen belegt.

**Files:**
- Create: `training/wakeword/pyproject.toml`, `training/wakeword/.python-version`, `training/wakeword/hey_nova.yml`, `training/wakeword/run.sh`, `training/wakeword/README.md`, `models/hey_nova.onnx`

**Interfaces:**
- Produces: `models/hey_nova.onnx`, geladen über `[wakeword] modell = "models/hey_nova.onnx"`.

- [ ] **Step 1:** Projekt anlegen: Python 3.10, Pins aus dem offiziellen Trainings-Notebook von openWakeWord, `torch` mit eigenem CUDA. `run.sh` legt alles unter `~/.cache/nova-training` ab, klont `openwakeword` und `piper-sample-generator` dorthin und ist wiederholbar: Vorhandene Daten werden übersprungen. Unterstützt der Generator nur eine Stimme je Lauf, erzeugt `run.sh` die Beispiele in zwei Läufen (`de_DE-mls-medium.pt`, `en_US-libritts_r-medium.pt`) und führt die Ordner zusammen.

- [ ] **Step 2:** `hey_nova.yml` schreiben: `model_name: hey_nova`, `target_phrase: ["hey nova", "hey nowa"]`, `custom_negative_phrases: ["hey nora", "hey oma", "november", "nova"]`, `n_samples: 50000`, `n_samples_val: 5000`, `steps: 50000`, `target_false_positives_per_hour: 0.2`.

- [ ] **Step 3: Rauchtest.** Lauf mit `n_samples: 500`, `steps: 500`. Erwartet: Beispiele beider Stimmen werden erzeugt, Augmentierung läuft, eine `.onnx` entsteht. Stichprobe von 5 deutschen Clips anhören lassen: Klingt es nach „Hey Nowa“?

- [ ] **Step 4: Volles Training im Hintergrund.** Erwartet: `~/.cache/nova-training/out/hey_nova.onnx`, Log zeigt Recall und Fehlauslösungen pro Stunde.

- [ ] **Step 5:** Modell nach `models/` kopieren, Metriken und Laufdauer in `training/wakeword/README.md` eintragen.

- [ ] **Step 6:** Committen: `Wake-Word-Modell „Hey Nova“ trainieren`.

---

### Task 11: Abnahme und Doku

**Files:**
- Modify: `config/nova.toml`, `docs/architektur.md`, `README.md`

- [ ] **Step 1:** `modell = "models/hey_nova.onnx"` setzen. `cargo test` ausführen. Erwartet: PASS.

- [ ] **Step 2: Abnahme mit dem Nutzer** nach der Tabelle in der Spec durchführen, Ergebnisse in einer Tabelle festhalten. Wird ein Ziel verfehlt: Schwelle in 0,1er-Schritten anpassen; hilft das nicht, Modell mit mehr Negativbeispielen neu trainieren. Spec-Ziele nicht stillschweigend senken.

- [ ] **Step 3:** `docs/architektur.md` aktualisieren: Phase 1 ✅, `stt` hört bis `cancel`. `README.md`: Stand und Einrichtung inklusive `nova_helpers.setup`.

- [ ] **Step 4:** Nach Rückfrage die offene Frage zum Aktivierungswort im Projektplan als entschieden markieren.

- [ ] **Step 5:** Committen: `Phase 1 abschließen`.
