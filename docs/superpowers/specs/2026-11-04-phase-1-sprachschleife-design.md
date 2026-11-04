# Phase 1: Sprachschleife mit Echo-Test

Nova erkennt „Hey Nova“, hört einen Satz und spricht ihn mit der Piper-Stimme Thorsten nach. Die App lässt sich dabei unterbrechen und löst nicht durch die eigene Stimme aus.

Stand: 2026-11-04. Maßgeblich bleiben der Projektplan und `docs/architektur.md`; diese Spec legt die Details für Phase 1 fest.

## Ziel und Abgrenzung

Bedienung in Phase 1 nur über das Terminal mit Log-Ausgabe, gestartet mit `cargo run -p nova-app`.

Enthalten:

- Rust-Koordinator mit Zustandsmaschine und Prozessführung
- Hilfsprozesse `wakeword`, `stt` und `tts`
- eigenes Wake-Word-Modell „Hey Nova“, lokal trainiert
- PipeWire-Echounterdrückung für den Betrieb über Lautsprecher

Nicht enthalten:

- Oberfläche, Router, Ollama, Claude Code
- akustische Bestätigung nach dem Wake Word (kein Ton, kein „Ja?“)
- Protokollierung gesprochener Anfragen unter `~/.local/state/nova`; Sätze erscheinen nur im Terminal-Log
- Autostart oder systemd-Dienst

## Gesamtbild

```
             nova_mic (Echo-Cancel-Quelle)
              │                    │
              ▼                    ▼
         wakeword (py)         stt (py)
              │  JSON Lines        │
              └───────┐   ┌────────┘
                      ▼   ▼
                 nova (Rust): Zustandsmaschine
                        │ JSON Lines
                        ▼
                     tts (py) ──▶ nova_sink ──▶ Lautsprecher
```

`wakeword` und `stt` öffnen jeweils selbst die Quelle `nova_mic`; PipeWire verteilt den Strom. Das Rust-Programm fasst kein Audio an.

Verworfen wurden PCM-Transport über Rust (Binärdaten passen nicht zu JSON Lines, zusätzliche Audiobibliothek) und ein gemeinsamer Python-Prozess (ein Absturz träfe alle Funktionen, widerspricht der Architektur).

## Rust

### Module in `nova-app`

| Modul | Zweck | Abhängigkeit |
|---|---|---|
| `protocol` | Serde-Typen für Befehle und Ereignisse der drei Helfer | keine |
| `voice_loop` | Zustandsmaschine als reine Funktion | `protocol` |
| `helper` | Kindprozess starten, Zeilen lesen und schreiben, stoppen | `protocol` |
| `main` | Konfiguration laden, Helfer starten, Ereignisschleife | alle |

Prozessführung läuft mit Threads und einem gemeinsamen `std::sync::mpsc`-Kanal, ohne tokio. Pro Helfer liest ein Thread stdout und ein zweiter leitet stderr ins `tracing`-Log weiter. Die Hauptschleife wartet mit `recv_timeout`, damit die Nachfragezeit ablaufen kann. Tokio kommt erst, wenn eine spätere Phase es braucht.

### Zustandsmaschine

Signatur: `step(dialog, input) -> (dialog, Vec<Action>)`. `dialog` enthält den Zustand und die offenen Äußerungen (siehe unten), `input` ist ein Helfer-Ereignis oder ein Zeitablauf. Die Funktion hat keine Seiteneffekte.

| Zustand | Eingabe | Aktionen | Folgezustand |
|---|---|---|---|
| `Lauschen` | `wakeword: wake` | `wakeword pause`, `stt listen`, Frist starten | `Zuhören` |
| `Zuhören` | `stt: speech_start` | Frist stoppen | `Zuhören` |
| `Zuhören` | Frist abgelaufen | `stt cancel`, `wakeword resume` | `Lauschen` |
| `Zuhören` | `stt: final` mit Text | `tts speak` | `Sprechen` |
| `Zuhören` | `stt: final` ohne Text | `stt cancel`, `wakeword resume` | `Lauschen` |
| `Sprechen` | `stt: speech_start` | `tts stop` (keine Frist, es wird ja schon gesprochen) | `Zuhören` |
| `Sprechen` | `tts: done` | Frist starten | `Nachfrage` |
| `Nachfrage` | `stt: final` mit Text | `tts speak` | `Sprechen` |
| `Nachfrage` | Frist abgelaufen | `stt cancel`, `wakeword resume` | `Lauschen` |

Weitere Regeln:

- Die Frist ist in `Zuhören` und `Nachfrage` dieselbe: `[dialog] nachfrage_s`.
- Ereignisse, für die ein Zustand keine Zeile hat, werden geloggt und ignoriert, etwa ein spätes `tts: done`.
- `stt` bleibt ab dem Wake Word durchgehend aktiv, auch während `Sprechen` (Barge-in) und `Nachfrage`. Nach jedem `final` hört es von selbst weiter, bis `cancel` kommt.
- `stt: speech_start` in `Nachfrage` stoppt die Frist, damit ein längerer Satz nicht abgeschnitten wird. Der Zustand bleibt `Nachfrage`, bis `final` kommt; ist `final` leer, startet die Frist neu.
- Während `Sprechen` eintreffende `final`-Ereignisse ohne vorheriges `speech_start` werden verworfen, sie stammen aus Restecho.
- Offene Äußerungen werden gezählt, weil `stt` `speech_start` sofort sendet, `final` aber erst nach Whisper, also rund 1 s später. Beginnt der Nutzer nach einer Pause weiterzusprechen oder greift die 30-s-Obergrenze, kommt das nächste `speech_start` vor dem vorigen `final`. Jedes `speech_start` außer in `Lauschen` erhöht den Zähler, jedes `final` senkt ihn und legt seinen Text ab, sofern er nicht leer ist. Solange noch eine Äußerung offen ist, passiert nichts: keine Aktion, kein Zustandswechsel, keine Frist. Erst das letzte `final` löst die Zeile der Tabelle aus, mit allen Texten, durch je ein Leerzeichen verbunden; „mit Text“ und „ohne Text“ meinen diesen verbundenen Text. So geht kein Satzteil verloren, und ein leeres `final` einer Fehlauslösung bricht keinen laufenden Satz ab.
- Ein Barge-in führt mit einer offenen Äußerung nach `Zuhören`. Jeder Wechsel nach `Lauschen` löscht Zähler und Texte.
- `partial` wird in Phase 1 nur geloggt.
- Der gesprochene Text ist `Du sagtest: {text}`. Die Satzvorlage liegt im Rust-Code und zieht in Phase 2 in die Module um.

### Fehler

- Meldet ein Helfer `error`, wird die Meldung geloggt; der Helfer entscheidet selbst, ob er weiterläuft.
- Endet ein Helfer unerwartet, wird er einmal neu gestartet; danach erhält `tts` ein `stop`, `stt` ein `cancel`, `wakeword` ein `resume`, und die Schleife kehrt nach `Lauschen` zurück. Der Zähler gilt pro Helfer für die ganze Laufzeit.
- Endet er ein zweites Mal, beendet sich Nova mit einer klaren Meldung und einem Exit-Code ungleich null.
- Meldet ein Helfer nicht innerhalb von 60 s `ready`, gilt das als Fehlstart und Nova beendet sich.
- Bei Ctrl+C und jedem Beenden werden alle Kindprozesse gestoppt. Helfer beenden sich außerdem selbst, sobald stdin geschlossen ist, damit keine Waisen zurückbleiben.
- Nicht parsebare Zeilen auf stdout werden als Warnung geloggt und übersprungen.

## Python

Die Helfer sind Module in `helpers/nova_helpers/`: `wakeword.py`, `stt.py`, `tts.py`, dazu `protocol.py` für das gemeinsame Lesen und Schreiben von JSON Lines. Start aus Rust mit `uv run --project helpers python -m nova_helpers.<name>`. Der Rust-Prozess übergibt die Konfiguration als Kommandozeilenargumente.

Audio läuft über `sounddevice`. Die Geräte `nova_mic` und `nova_sink` sind PipeWire-Knoten und für PortAudio nicht direkt per Name sichtbar: Die Helfer öffnen das ALSA-Gerät `pipewire` und wählen den Knoten über die Umgebungsvariable `PIPEWIRE_NODE`. Klappt das nicht zuverlässig, ersetzen `pw-record` und `pw-play` als Kindprozesse mit rohem PCM `sounddevice`; die Protokolle bleiben davon unberührt.

Das Protokoll bleibt wie in `docs/architektur.md`. Es ändert sich nur in einem Punkt: `stt` hört nach `listen` bis `cancel` weiter und liefert mehrere `final`.

Im Prozess läuft eine Lese-Schleife für stdin, die Audioarbeit in einem eigenen Thread. Logs gehen ausschließlich nach stderr, stdout gehört dem Protokoll.

Python-Version: `tflite-runtime` hat nur Wheels bis Python 3.11. Erste Wahl ist Python 3.12 mit ausgeschlossener `tflite-runtime` über `tool.uv`, weil openWakeWord auch mit ONNX läuft. Scheitert das, wird auf 3.11 gepinnt.

### `wakeword`

openWakeWord mit ONNX-Inferenz, 16 kHz mono, Blöcke von 80 ms. Löst aus, wenn der Wert `[wakeword] schwelle` erreicht. Nach einem `wake` gilt 1 s Sperrzeit. Bei `pause` bleibt der Prozess geladen, schließt aber den Audiostrom nicht, damit `resume` sofort greift; die Werte werden verworfen und der Modellpuffer bei `resume` geleert.

### `stt`

faster-whisper mit `[spracherkennung]`-Modell, `language="de"`. Silero-VAD erkennt Sprache auf 16 kHz.

- Ringpuffer mit 0,5 s Vorlauf, damit kein Wortanfang fehlt
- `speech_start`, sobald die VAD Sprache meldet
- `partial` etwa jede Sekunde durch erneutes Transkribieren des bisherigen Puffers
- `final` nach 0,8 s Stille oder spätestens nach 30 s Sprache

Geräteauswahl: `geraet = "cuda"` schlägt beim Laden hart fehl, `"auto"` versucht CUDA und fällt auf CPU mit `int8` zurück und nennt die Wahl im Log. Das System hat CUDA 13; ob die CTranslate2-Wheels damit laufen, wird beim Bau geprüft.

### `tts`

`piper-tts` mit dem Modell aus `[stimme]`, Ausgabe blockweise auf `nova_sink`. Eine Warteschlange nimmt `speak`-Befehle auf. `started` kommt vor dem ersten Audioblock, `done` nach dem letzten. `stop` leert die Warteschlange und bricht die Ausgabe sofort ab; für die abgebrochene Äußerung kommt kein `done`.

### Modelle

`uv run --project helpers python -m nova_helpers.setup` lädt die Stimme `de_DE-thorsten-medium` (63 MB, Hugging Face `rhasspy/piper-voices`) nach `~/.local/share/nova/piper/`. Die Helfer laden selbst nichts nach und melden stattdessen `error` mit dem Hinweis auf den Befehl. `faster-whisper-small` liegt bereits im Hugging-Face-Cache.

## Wake Word „Hey Nova“

`training/wakeword/` ist ein eigenes uv-Projekt mit Python 3.10, getrennt von `helpers/`, weil die Trainings-Toolchain alte Versionen verlangt (TensorFlow 2.8, `protobuf<4`). Laufzeitabhängigkeiten von Nova gibt es dorthin nicht.

| Teil | Festlegung |
|---|---|
| Rohdaten | `~/.cache/nova-training`, nie im Repo, ca. 23–27 GB |
| Generator-Stimmen | `de_DE-mls-medium.pt` und `en_US-libritts_r-medium.pt` |
| Zielphrasen | „hey nova“, zusätzlich „hey nowa“ für deutsche Aussprache |
| Negativbeispiele | automatisch aus der Phonem-Nähe plus „hey nora“, „hey oma“, „november“, „nova“ allein |
| Hintergrund | ACAV100M-Merkmale, AudioSet-Teil, FMA small, MIT-Raumimpulse |
| Mindestumfang | 50.000 Positivbeispiele |
| Ergebnis | `models/hey_nova.onnx` im Repo |

Falls die Generator-Skripte nur eine Stimme je Lauf unterstützen, werden die Beispiele in zwei Läufen erzeugt und zusammengeführt. Fehlt die deutsche Stimme in einer Version von `piper-sample-generator`, gilt v2.0.0.

Bis das Modell fertig ist, läuft die Entwicklung mit dem fertigen `hey_jarvis`. Der Wechsel ist nur eine Konfigurationsänderung. Phase 1 gilt erst mit `models/hey_nova.onnx` als abgeschlossen.

## Konfiguration

Änderungen in `config/nova.toml`, alle typisiert und validiert in `nova-core`:

```toml
[nova]
aktivierungswort = "hey nova"

[wakeword]
modell = "models/hey_nova.onnx"   # Pfad oder Name eines fertigen Modells (hey_jarvis)
schwelle = 0.5                    # 0.0..=1.0

[audio]
eingabe = "nova_mic"
ausgabe = "nova_sink"

[dialog]
nachfrage_s = 8.0                 # größer als 0
```

`[stimme]` und `[spracherkennung]` bleiben unverändert. Enthält `modell` einen `/`, ist es ein Pfad relativ zur Projektwurzel, sonst der Name eines fertigen Modells. Die Projektwurzel ist das Repo und wird beim Kompilieren aus `CARGO_MANIFEST_DIR` festgelegt, nicht aus dem Konfigurationsordner: Der Launcher liest die Konfiguration aus `~/.config/nova`, `helpers/` und `models/` bleiben aber im Repo. Wird das Repo verschoben, ist `cargo install --path crates/nova-app` erneut nötig.

## PipeWire

`deploy/pipewire/nova-echo-cancel.conf` lädt `libpipewire-module-echo-cancel` mit dem WebRTC-Verfahren und legt `nova_mic` und `nova_sink` an. Die Standardgeräte des Systems bleiben unverändert; nur Nova nutzt die virtuellen Geräte. Die Quelle ist das JLAB-Mikrofon, die Senke die eingebaute Audioausgabe, jeweils namentlich eingetragen.

Aktiviert wird von Hand per Symlink nach `~/.config/pipewire/pipewire.conf.d/` und Neustart von PipeWire. Das Repo beschreibt das im README; Nova ändert selbst nichts an der Audio-Konfiguration. Fehlen die Geräte beim Start, melden die Helfer `error` mit Hinweis auf diesen Schritt.

## Tests

| Ebene | Prüfung |
|---|---|
| `voice_loop` | Jede Tabellenzeile, Barge-in, verworfene Rest-`final`, Fristablauf |
| `protocol` | Serialisieren und Parsen aller Nachrichten |
| `helper` und `main` | Integrationstest mit einem Fake-Helfer-Skript: ganzer Durchlauf ohne Audio, Neustart nach Absturz, Abbruch nach zweitem Absturz |
| `nova-core` | Ausgelieferte Konfiguration lädt, ungültige Schwelle und Frist werden abgelehnt |
| Python | Protokolltests der Helfer mit gefälschtem Audio und gefälschten Modellen, ohne Audiogerät |

Vor jedem Commit laufen `cargo fmt --all`, `cargo clippy --all-targets` und `cargo test`.

## Abnahme am Gerät

Mit eingeschaltetem Echo-Modul, Ausgabe über Lautsprecher, gesprochen vom Nutzer.

| Prüfung | Ziel |
|---|---|
| Echo-Test | mindestens 18 von 20 Durchläufen: Wake Word, Satz, korrektes Echo |
| Nachfrage | Folgesatz ohne Wake Word klappt, nach 8 s zurück in `Lauschen` |
| Barge-in | Ausgabe stoppt in unter 300 ms, 5 von 5 Versuchen |
| Selbstauslösung | keine in 20 Echo-Ausgaben |
| Fehlauslösung | höchstens 1 in 1 Stunde bei Hintergrundgeräuschen oder Gespräch |
| Beenden | Ctrl+C hinterlässt keine Python-Prozesse |

## Dokumentation

Nach Abschluss werden aktualisiert: `docs/architektur.md` (Phase-Status, Protokoll-Änderung bei `stt`), `README.md` (Einrichtung, Stand) und im Projektplan die offene Frage zum Aktivierungswort.
