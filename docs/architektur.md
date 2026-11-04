# Architektur

```
Mikrofon ─▶ wakeword (py) ─▶ stt (py) ─▶ nova (Rust): Router ─┬─ Stufe 1: feste Befehle
                                                               ├─ Stufe 2: Ollama
                                                               └─ Stufe 3: Claude Code (dauerhafte Sitzung)
                                     Lautsprecher ◀─ tts (py, Piper) ◀── Sätze/Floskeln
```

Der Rust-Prozess `nova` ist die Desktop-App und zugleich der einzige Koordinator; es gibt keinen getrennten Dienst. Er wird von Hand über den Application Launcher gestartet, startet die Hilfsprozesse als Kindprozesse, liest ihre Ereignisse und schickt Befehle. Der Router läuft in diesem Prozess und spricht Ollama per HTTP an. Echounterdrückung kommt von PipeWire (Echo-Cancel-Modul), Barge-in stoppt `tts` sofort, sobald `stt` Sprache meldet.

## Hilfsprozess-Protokoll (Vorschlag für Phase 1)

JSON Lines: eine JSON-Zeile pro Nachricht, stdin = Befehle an den Helfer, stdout = Ereignisse, stderr = Log.

| Helfer | Befehle (stdin) | Ereignisse (stdout) |
|---|---|---|
| `wakeword` | `{"cmd":"pause"}`, `{"cmd":"resume"}` | `{"event":"wake","score":0.93}` |
| `stt` | `{"cmd":"listen"}`, `{"cmd":"cancel"}` | `{"event":"speech_start"}`, `{"event":"partial","text":"…"}`, `{"event":"final","text":"…"}` |
| `tts` | `{"cmd":"speak","id":"1","text":"…"}`, `{"cmd":"stop"}` | `{"event":"started","id":"1"}`, `{"event":"done","id":"1"}` |

Jeder Helfer meldet beim Start `{"event":"ready"}`. Fehler: `{"event":"error","message":"…"}`.

## Phasen

0. ✅ Grundgerüst (Workspace, Konfiguration, Floskeln, Launcher-Eintrag)
1. Sprachschleife mit Echo-Test
2. Stufe 1: Musik, Wetter, Apps, Suche
3. Lokales Modell (Ollama), Router, VRAM messen
4. Claude Code: dauerhafte Sitzung mit Streaming-Eingabe
5. Charakter und Floskeln (Regler, Sperrliste, Ernst-Modus)
6. Obsidian Vault (Index, gezieltes Nachladen, Zurückschreiben)
7. Oberfläche (Kommandobrücke + Overlay)
8. Listen, Kalender und Aufgaben (mit Datums-Parsing durch das lokale Modell), ntfy, KDE Connect
9. Lernfunktion (Auswertung von Hand im Dashboard)
10. Feinschliff
