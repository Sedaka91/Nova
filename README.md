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

Phase 0 (Grundgerüst) ist erledigt. **Phase 1** (Sprachschleife) und **Phase 2** (Stufe 1: Musik, Wetter, Apps, Suche) sind umgesetzt und zusammengeführt, die gemeinsame Abnahme am Gerät steht aus. Das Aktivierungswort ist „Hey Nova“ (`models/hey_nova.onnx`, Training unter `training/wakeword/`). Nächster Schritt laut Plan: Phase 3 (lokales Modell, Router).

### Stufe 1 einrichten

```sh
sudo pacman -S playerctl
```

Den `pacman`-Befehl führst du selbst aus; Nova und Claude Code nutzen kein `sudo`.

- **Heimatort:** `config/nova.toml` ist eingecheckt, dort gehört kein persönlicher Ort hinein. Stattdessen eine Kopie von `config/` außerhalb des Repos anlegen (`~/.config/nova`, so wie `deploy/nova.desktop` es tut), dort unter `[wetter]` `heimatort = "…"` setzen und Nova mit `NOVA_CONFIG_DIR=~/.config/nova` starten. So bleibt der Heimatort außerhalb des Repos. Ohne Heimatort sagt Nova, dass er fehlt.
- **Spotify:** Im Spotify-Dashboard eine App registrieren, Redirect-URI `http://127.0.0.1:8898/callback`. Danach einmalig anmelden; Nova fragt im Terminal nach der Client-ID und öffnet den Browser. Client-ID und Token landen in KWallet.

```sh
cargo run -p nova-app -- spotify-login
```

Wetterdaten von Open-Meteo.com (CC BY 4.0)

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
