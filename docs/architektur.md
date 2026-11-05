# Architektur

```
Mikrofon ─▶ wakeword (py) ─▶ stt (py) ─▶ nova (Rust): Router ─┬─ Stufe 1: feste Befehle
                                                               ├─ Stufe 2: Ollama
                                                               └─ Stufe 3: Claude Code (dauerhafte Sitzung)
                                     Lautsprecher ◀─ tts (py, Piper) ◀── Sätze/Floskeln
```

Der Rust-Prozess `nova` ist die Desktop-App und zugleich der einzige Koordinator; es gibt keinen getrennten Dienst. Er wird von Hand über den Application Launcher gestartet, startet die Hilfsprozesse als Kindprozesse, liest ihre Ereignisse und schickt Befehle. Der Router läuft in diesem Prozess und spricht Ollama per HTTP an. Echounterdrückung kommt von PipeWire (Echo-Cancel-Modul), Barge-in stoppt `tts` sofort, sobald `stt` Sprache meldet.

## Hilfsprozess-Protokoll

JSON Lines: eine JSON-Zeile pro Nachricht, stdin = Befehle an den Helfer, stdout = Ereignisse, stderr = Log.

| Helfer | Befehle (stdin) | Ereignisse (stdout) |
|---|---|---|
| `wakeword` | `{"cmd":"pause"}`, `{"cmd":"resume"}` | `{"event":"wake","score":0.93}` |
| `stt` | `{"cmd":"listen"}`, `{"cmd":"cancel"}` | `{"event":"speech_start"}`, `{"event":"partial","text":"…"}`, `{"event":"final","text":"…"}` |
| `tts` | `{"cmd":"speak","id":"1","text":"…"}`, `{"cmd":"stop"}` | `{"event":"started","id":"1"}`, `{"event":"done","id":"1"}` |

Jeder Helfer meldet beim Start `{"event":"ready"}`. Fehler: `{"event":"error","message":"…"}`.

`stt` hört nach `listen` weiter und liefert mehrere `final`, bis `cancel` kommt. `speech_start` kommt sofort, `final` erst nach Whisper; deshalb zählt das Rust-Programm offene Äußerungen und führt ihre Texte zusammen.

## Arbeits-Thread (Phase 2)

Feste Befehle laufen nicht in der Hauptschleife, sondern auf einem eigenen Arbeits-Thread (`supervisor::Executor`). So bleiben Barge-in und Ctrl+C während eines Befehls bedienbar. Der Dialog wechselt dafür in den Zustand `Working`: Er wartet auf die `Reply` des Threads, höchstens 20 s (`WORK_TIMEOUT`). Spricht der Nutzer währenddessen, wird das Ergebnis verworfen und Nova hört zu; läuft die Frist ab, sagt Nova das und der Thread wird verworfen. Jeder Befehl liefert eine `Reply` (`Say` oder `Silent`), Fehler sind gesprochene Sätze. Der Aufruf `nova spotify-login` läuft vor dem Start der Hilfsprozesse und braucht weder gültige `nova.toml` noch Audiogeräte.

## Phasen

0. ✅ Grundgerüst (Workspace, Konfiguration, Floskeln, Launcher-Eintrag)
1. Sprachschleife mit Echo-Test (umgesetzt, gemeinsame Abnahme mit Phase 2 offen)
2. Stufe 1: Musik, Wetter, Apps, Suche (umgesetzt, gemeinsame Abnahme mit Phase 1 offen)
3. Lokales Modell (Ollama): unbekannte Sätze gehen an das Modell, Aufwärmen und VRAM-Protokoll beim Start (umgesetzt, Abnahme am Gerät offen). Die Konfidenz-Schwelle `router.konfidenz_schwelle` wirkt erst mit Stufe 3 in Phase 4.
4. Claude Code: dauerhafte Sitzung mit Streaming-Eingabe
5. Charakter und Floskeln (Regler, Sperrliste, Ernst-Modus)
6. Obsidian Vault (Index, gezieltes Nachladen, Zurückschreiben)
7. Oberfläche (Kommandobrücke + Overlay)
8. Listen, Kalender und Aufgaben (mit Datums-Parsing durch das lokale Modell), ntfy, KDE Connect
9. Lernfunktion (Auswertung von Hand im Dashboard)
10. Feinschliff
