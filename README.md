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
deploy/              Launcher-Eintrag, PipeWire-Echounterdrückung
docs/                Architektur, Design
```

## Start

```sh
cargo test                     # prüft auch die ausgelieferten Konfigurationsdateien
cargo run -p nova-app
```

## Stand

Phase 0 (Grundgerüst) erledigt. Nächster Schritt laut Plan – **Phase 1:** Wake Word, faster-whisper und Piper als Hilfsprozesse anbinden und zu einer Sprachschleife mit Echo-Test verbinden.

### Echounterdrückung (PipeWire)

Die Sprachschleife nutzt die virtuellen Geräte `nova_mic` und `nova_sink`. Die Standardgeräte des Systems bleiben unverändert. Voraussetzung: `uv run --project helpers python -m nova_helpers.setup` ist einmal gelaufen (lädt die Stimme).

```sh
# aktivieren (vom Repo-Wurzelverzeichnis aus)
mkdir -p ~/.config/pipewire/pipewire.conf.d
ln -s "$PWD/deploy/pipewire/nova-echo-cancel.conf" ~/.config/pipewire/pipewire.conf.d/nova-echo-cancel.conf
systemctl --user restart pipewire pipewire-pulse wireplumber

# prüfen: nova_mic und nova_sink erscheinen, die Standardgeräte (*) bleiben gleich
wpctl status

# rückgängig machen
rm ~/.config/pipewire/pipewire.conf.d/nova-echo-cancel.conf
systemctl --user restart pipewire pipewire-pulse wireplumber
```

Der Neustart unterbricht laufende Audiowiedergabe und Anrufe kurz.

### Einmalig am PC

- Rust (`rustup`), `uv`, `playerctl`, Ollama installieren.
- Skills in Claude Code aktivieren: Superpowers, python-development, BeautifulRust, Frontend Design, UI/UX Pro Max (Installationsbefehle im jeweiligen Repo prüfen). webapp-testing erst nach der Framework-Wahl.
- In `config/nova.toml` den Vault-Pfad eintragen.
