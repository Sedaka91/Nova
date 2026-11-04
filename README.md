# Nova

Sprachassistent im Stil von Jarvis für CachyOS/KDE: Aktivierungswort „Nova“, Dialogmodus, dreistufiges Routing (feste Befehle → lokales Modell → Opus über Claude Code im Pro-Abo).

- Projektplan: https://claude.ai/artifact/VRvZVknuzQCJrbkjB8GUGz
- Design: [docs/design.md](docs/design.md), entschieden: Leitspur (https://claude.ai/artifact/W9fN34ZZdGoxcdS8uKaPu4)

## Aufbau

```
crates/nova-core     Typen, Konfiguration, Floskel-Bibliothek
crates/nova-app      Desktop-App (Binary `nova`)
helpers/             Python-Hilfsprozesse (Wake Word, STT, TTS) – uv-Projekt
config/              nova.toml, phrases.toml, apps.toml
deploy/              Launcher-Eintrag
docs/                Architektur, Design
```

## Start

```sh
cargo test                     # prüft auch die ausgelieferten Konfigurationsdateien
cargo run -p nova-app
```

## Stand

Phase 0 (Grundgerüst) erledigt. Nächster Schritt laut Plan – **Phase 1:** Wake Word, faster-whisper und Piper als Hilfsprozesse anbinden und zu einer Sprachschleife mit Echo-Test verbinden.

### Einmalig am PC

- Rust (`rustup`), `uv`, `playerctl`, Ollama installieren.
- Skills in Claude Code aktivieren: Superpowers, python-development, BeautifulRust, Frontend Design, UI/UX Pro Max (Installationsbefehle im jeweiligen Repo prüfen). webapp-testing erst nach der Framework-Wahl.
- In `config/nova.toml` den Vault-Pfad eintragen.
