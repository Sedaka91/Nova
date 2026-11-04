# Nova

Jarvis-artiger Sprachassistent auf CachyOS/KDE. Projektplan (maßgeblich): https://claude.ai/artifact/VRvZVknuzQCJrbkjB8GUGz
Architektur und Hilfsprozess-Protokoll: `docs/architektur.md`. Design: `docs/design.md`.

## Arbeitsregeln (Karpathy)

- **Erst denken:** Annahmen nennen, bei Unklarheit fragen statt raten. Gibt es einen einfacheren Weg, sag es.
- **Einfachheit:** Nur bauen, was gefragt ist. Keine spekulativen Abstraktionen, Optionen oder Fehlerbehandlung für Unmögliches.
- **Chirurgische Änderungen:** Nur anfassen, was die Aufgabe braucht. Keinen fremden Code „verbessern“, nichts ungefragt umformatieren.
- **Zielgetrieben:** Vor dem Umsetzen ein prüfbares Ziel festlegen (Test, Befehl), danach nachweisen, dass es erreicht ist.

## Stack

- Anwendung und Oberfläche in **Rust** (Workspace unter `crates/`). Python nur für Hilfsprozesse in `helpers/` (Wake Word, Spracherkennung, Sprachausgabe), verwaltet mit `uv`.
- Hilfsprozesse sprechen JSON Lines über stdin/stdout mit dem Rust-Programm.
- Konfiguration in `config/*.toml`; jede neue Option wird in `nova-core` typisiert und validiert.
- Oberflächen-Framework (Tauri oder nativ) ist **noch nicht entschieden** – nichts davon vorwegnehmen.
- Nova ist **eine einzige Desktop-App** (ein Prozess, eigenes Fenster, kein Browser-Tab): Kommandobrücke + Overlay.
- Start nur von Hand über den Application Launcher: kein Autostart, kein systemd-Dienst. Nova hört nur, solange die App läuft.
- Router im Rust-Programm, spricht Ollama per HTTP an. Wake Word: openWakeWord.
- Stufe 1 läuft als Rust-Module ohne Modell. Einzige Ausnahme: Kalender und Aufgaben, dort parst das lokale Modell Datum und Uhrzeit (kommt erst in Phase 8).
- Claude-Skills (Vault, Desktop, System) gibt es nur für Stufe 3.
- Entwickler-Skills: BeautifulRust, python-development, Superpowers (nur für große Bausteine), Frontend Design und UI/UX Pro Max (Phase 7). webapp-testing erst nach der Framework-Wahl.

## Befehle

```sh
cargo fmt --all && cargo clippy --all-targets && cargo test
cargo run -p nova-app             # liest ./config oder $NOVA_CONFIG_DIR
cd helpers && uv sync
```

## Sicherheit (gilt auch für den Code von Nova selbst)

- Nie `sudo` oder Root. Dateien löschen nur nach Klick in der App; der Dialog nennt den konkreten Pfad. Änderungen und Löschungen im Kalender dürfen per Stimme bestätigt werden.
- Ordnerfreigaben stehen als Notiz im Vault; der Agent darf sie nicht ändern.
- Regeln technisch erzwingen (Claude-Code-Berechtigungen, Prüfung im Rust-Programm), nicht nur per Prompt.
- Claude Code nur als offizieller Prozess mit Abo-Login; das Anmelde-Token wird nie ausgelesen. `ANTHROPIC_API_KEY` wird beim Start der Sitzung entfernt, damit nichts über die API abgerechnet wird.
- Zugangsdaten (Spotify, Google, Microsoft, ntfy) nur in KWallet über Secret Service, nie im Repo oder in `config/*.toml`. Outlook bekommt nur Kalenderrechte.
- Nur in `config/apps.toml` gelistete Programme dürfen starten. Neue Stufe-1-Regeln nur nach Bestätigung, nie automatisch. Die Auswertung der Lernfunktion läuft nur, wenn der Nutzer sie im Dashboard anstößt.
- Protokolle gesprochener Anfragen liegen unter `~/.local/state/nova`, nicht im Vault oder Repo, und werden nicht automatisch gelöscht.
- Der Ernst-Modus ist nicht abregelbar.

## Konventionen

- Bezeichner und Code-Kommentare englisch, Konfiguration und alles, was Nova sagt oder anzeigt, deutsch.
- Commits klein und thematisch; vor jedem Commit fmt, clippy, test.
