# Phase 2: Stufe 1 (feste Befehle)

Nova führt feste Befehle ohne Modell aus: Musik, Wetter, Programme öffnen, Google- und YouTube-Suche. Das Echo aus Phase 1 entfällt.

Stand: 2026-11-05. Maßgeblich bleiben der Projektplan und `docs/architektur.md`; diese Spec legt die Details für Phase 2 fest.

## Ziel und Abgrenzung

Bedienung weiter nur über das Terminal mit Log-Ausgabe, gestartet mit `cargo run -p nova-app`.

Enthalten:

- Erkennung fester Befehle als reine Funktion
- Musik-Transport über `playerctl`
- Spotify-Inhalte über die Web API, als letzter Baustein
- Wetter über Open-Meteo, für den Heimatort und genannte Orte
- Programme öffnen aus `config/apps.toml`
- Google- und YouTube-Suche im Browser

Nicht enthalten:

- Router, Ollama, Claude Code. Nicht erkannte Sätze beantwortet Nova vorerst mit „Das kann ich noch nicht.“
- Kalender und Aufgaben (Phase 8)
- Regler, Sperrliste und Floskeln (Phase 5); die Sätze dieser Phase stehen als feste Vorlagen im Code
- Protokollierung gesprochener Anfragen (Phase 9)
- Oberfläche; der Heimatort wird bis Phase 7 von Hand in der lokalen `nova.toml` gesetzt
- Erster YouTube-Treffer direkt abspielen; das bleibt offene Frage im Projektplan, die Suche öffnet die Ergebnisliste
- Spotify selbst starten, wenn es nicht läuft

## Stand von Phase 1

Phase 2 baut auf dem Branch `phase-1-sprachschleife` auf und läuft auf `phase-2-stufe-1`. Am 2026-11-05 lief die Schleife am Gerät mit `hey_jarvis` und aktivem Echo-Modul: Echo, Folgesatz und Rückkehr nach 8 s funktionieren. Barge-in kam im Test nicht vor. Offen bleiben das Modell `hey_nova.onnx` und die volle Abnahme aus der Spec von Phase 1. Beides ist keine Voraussetzung für Phase 2; die Abnahme von Phase 2 läuft bis dahin mit `hey_jarvis`.

## Gesamtbild

```
stt final ─▶ parse(text) ─┬─ Befehl    ─▶ Working ─▶ Arbeits-Thread ─▶ Antwort ─┬─ Say    ─▶ Speaking
                          │                                                     └─ Silent ─▶ FollowUp
                          └─ unbekannt ─▶ Speaking („Das kann ich noch nicht.“)
```

Pro Befehl läuft ein kurzer Arbeits-Thread, das Ergebnis kommt über den vorhandenen `mpsc`-Kanal zurück. So bleibt Nova während Netzwerkaufrufen unterbrechbar.

Verworfen wurden die Ausführung direkt in der Hauptschleife (ein hängender Dienst blockiert Barge-in und Ctrl+C) und tokio (zu groß für diese Aufrufe).

## Rust

### Module in `nova-app`

| Modul | Zweck | Abhängigkeit |
|---|---|---|
| `commands` | `Command`, `Reply`, `parse` (rein, von `voice_loop` genutzt) und `execute` (vom Arbeits-Thread aufgerufen) | die Module unten |
| `music` | Transport und Lautstärke über `playerctl --player=spotify` | Kindprozess |
| `spotify` | Suche, Abspielen, Login, Token-Erneuerung | `ureq`, `secrets` |
| `weather` | Geocoding, Vorhersage, Satz aus Vorlage | `ureq` |
| `launcher` | Treffer in `apps.toml` prüfen, `gtk-launch` starten | Kindprozess |
| `search` | URL bauen, `xdg-open` starten | Kindprozess |
| `secrets` | KWallet über `secret-tool` lesen und schreiben | Kindprozess |

Neue Abhängigkeiten: `ureq` (synchroner HTTP-Client), `percent-encoding`, sowie für PKCE `sha2` und `base64`. Zufallsbytes kommen aus `/dev/urandom`.

Alle Kindprozesse laufen ohne Shell, Argumente werden einzeln übergeben.

### Zustandsmaschine

`step` bleibt rein. `parse` wird in `step` aufgerufen und ersetzt das Echo. Neu sind der Zustand `Working`, die Eingabe `Reply` und die Aktionen `Run(Command)`, `AbandonRun` und `ArmWorkDeadline`.

```rust
enum Reply {
    Say(String),
    Silent,
}
```

| Zustand | Eingabe | Aktionen | Folgezustand |
|---|---|---|---|
| `Zuhören`, `Nachfrage` | letztes `final`, Befehl erkannt | Frist stoppen, `Run`, Arbeitsfrist starten | `Working` |
| `Zuhören`, `Nachfrage` | letztes `final`, unbekannt | Frist stoppen, Satz „Das kann ich noch nicht.“ | `Sprechen` |
| `Working` | `Reply::Say` | Frist stoppen, `tts speak` | `Sprechen` |
| `Working` | `Reply::Silent` | Nachfragefrist starten | `Nachfrage` |
| `Working` | `stt: speech_start` | `AbandonRun`, Frist stoppen | `Zuhören` mit einer offenen Äußerung |
| `Working` | Arbeitsfrist abgelaufen | `AbandonRun`, Satz „Das hat zu lange gedauert.“ | `Sprechen` |
| `Working` | `stt: final` | keine | `Working` |

Weitere Regeln:

- Das letzte `final` meint weiter die zusammengeführten Texte aus Phase 1.
- Die Arbeitsfrist beträgt 20 s und ist fest (`LoopSettings::work_timeout`). Sie fängt einen hängenden oder abgestürzten Arbeits-Thread ab, etwa wenn KWallet gesperrt ist und `secret-tool` wartet.
- Der Treiber vergibt jedem `Run` eine Id. Eine Antwort mit fremder Id ist veraltet und wird verworfen, wie heute ein `done`. `AbandonRun` löscht die erwartete Id; der Thread läuft aus, sein Ergebnis wird ignoriert.
- Beim Neustart eines Helfers löscht der Treiber die erwartete Id.
- Ein Befehl, der abgebrochen wurde, kann seine Wirkung trotzdem noch entfalten (etwa ein bereits gestartetes Programm). Es wird nichts rückgängig gemacht.

### Erkennung

`parse(text: &str) -> Option<Command>` ist rein und arbeitet mit festen Mustern, ohne Regex-Bibliothek.

Normalisierung: Kleinschreibung, Satzzeichen `.,!?;:„“"'` entfernen, Leerraum zusammenfassen, vorn „hey nova“ oder „nova“ und „bitte“ sowie hinten „bitte“ entfernen.

Die Regeln gelten in dieser Reihenfolge, der erste Treffer gewinnt:

| Regel | Muster | Befehl |
|---|---|---|
| 1 | genau „stopp“, „stop“ | `Dismiss` (still, tut nichts) |
| 2 | genau „play“, „spiel weiter“, „musik an“, „musik weiter“, „fortsetzen“ | `Play` |
| 3 | genau „pause“, „musik aus“, „musik stopp“, „musik pausieren“ | `Pause` |
| 4 | genau „weiter“, „nächstes lied“, „nächster titel“, „nächster song“, „überspringen“ | `Next` |
| 5 | genau „zurück“, „vorheriges lied“, „vorheriger titel“, „letztes lied“ | `Previous` |
| 6 | genau „lauter“ / „leiser“ | `VolumeStep(+10)` / `VolumeStep(-10)` |
| 7 | „lautstärke auf N“, optional mit „prozent“, N in 0..=100 | `VolumeSet(N)` |
| 8 | beginnt mit „wetter“, „wetterbericht“, „wie wird das wetter“, „wie ist das wetter“ | `Weather { place, day }` |
| 9 | „such bei youtube [nach] X“, „such auf youtube [nach] X“ | `Search(YouTube, X)` |
| 10 | „google [nach] X“, „such nach X“, „suche nach X“ | `Search(Google, X)` |
| 11 | „öffne X“, „starte X“, „mach X auf“, „X öffnen“, „X starten“ | `OpenApp(X)` |
| 12 | „spiel X“, „spiele X“, optional hinten „auf spotify“ | `PlayContent { kind, query }` |

Einzelheiten:

- Regel 1 gibt es, weil „Stopp“ beim Barge-in Nova unterbrechen soll und nicht „Das kann ich noch nicht.“ auslösen darf.
- Regel 8: `day` ist `Tomorrow`, wenn „morgen“ vorkommt, sonst `Today`. `place` ist der Text nach „ in “ oder „ für “ ohne das Wort „heute“ oder „morgen“; fehlt er, gilt der Heimatort. Die Regel greift nur am Satzanfang, damit Fragen wie „warum ist das Wetter so schlecht“ für den Router in Phase 3 frei bleiben.
- Regel 11: „den“, „die“, „das“ vor X werden entfernt.
- Regel 12: Beginnt X mit „die playlist“, „playlist“ oder „meine playlist“, ist `kind` `Playlist`; bei „das album“ oder „album“ `Album`; bei „das lied“, „den song“ oder „den titel“ `Track`; sonst `Any`.
- Ein Muster ohne Inhalt („such nach“, „spiel“) ergibt `None`.

### Ausführung

Jeder Befehl liefert `Reply`, nie einen Fehler; Fehler sind gesprochene Sätze. Die technische Ursache steht im Log.

| Befehl | Ablauf | Antwort bei Erfolg |
|---|---|---|
| `Dismiss` | nichts | `Silent` |
| `Play`, `Pause`, `Next`, `Previous` | `playerctl --player=spotify play` usw. | `Silent` |
| `VolumeStep` | `playerctl --player=spotify volume 0.1+` bzw. `0.1-` | `Silent` |
| `VolumeSet` | `playerctl --player=spotify volume {N/100}`, zum Beispiel `0.05` oder `1.00` | `Silent` |
| `PlayContent` | siehe Spotify | „Ich spiele {name}.“ |
| `Weather` | siehe Wetter | Wettersatz |
| `OpenApp` | exakter Treffer in `apps.toml`, dann `gtk-launch {id}` | „{name} wird geöffnet.“ |
| `Search` | `xdg-open` mit `https://www.google.com/search?q={begriff}` bzw. `https://www.youtube.com/results?search_query={begriff}`, URL-kodiert | „Ich suche nach {begriff}.“ |

| Fehlerfall | Satz |
|---|---|
| `playerctl` fehlt | „Playerctl ist nicht installiert.“ |
| kein Spotify-Player | „Spotify läuft gerade nicht.“ |
| Spotify nicht angemeldet | „Spotify ist noch nicht angemeldet.“ |
| Spotify ohne aktives Gerät | „Spotify läuft gerade nicht.“ |
| Spotify ohne Treffer | „Dazu habe ich auf Spotify nichts gefunden.“ |
| Spotify sonst | „Spotify ist gerade nicht erreichbar.“ |
| Ort nicht gefunden | „Den Ort {ort} kenne ich nicht.“ |
| Heimatort leer | „Dein Heimatort fehlt in den Einstellungen.“ |
| Wetterdienst nicht erreichbar | „Der Wetterdienst ist nicht erreichbar.“ |
| Programm nicht gelistet | „{name} ist nicht freigegeben.“ |
| `gtk-launch`, `xdg-open` fehlgeschlagen | „Das konnte ich nicht öffnen.“ |

### Wetter

Geocoding: `GET https://geocoding-api.open-meteo.com/v1/search?name={ort}&count=1&language=de`. Fehlt `results` in der Antwort (HTTP 200), ist der Ort unbekannt. Gesprochen wird der zurückgelieferte Name.

Vorhersage: `GET https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,weather_code&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max&timezone=auto&forecast_days=2`.

Vorlagen, Temperaturen gerundet, Grad mit „minus“ statt Vorzeichen:

- Heute, Codes unter 51 und unbekannte Codes: „In {ort} ist es gerade {zustand} bei {t} Grad. Heute {min} bis {max} Grad, Regenwahrscheinlichkeit {p} Prozent.“
- Heute, Codes ab 51: „In {ort} gibt es gerade {zustand} bei {t} Grad. Heute {min} bis {max} Grad, Regenwahrscheinlichkeit {p} Prozent.“
- Morgen: „Morgen in {ort}: {zustand}, {min} bis {max} Grad, Regenwahrscheinlichkeit {p} Prozent.“

WMO-Codes:

| Code | Zustand |
|---|---|
| 0 | klar |
| 1 | überwiegend klar |
| 2 | teils bewölkt |
| 3 | bedeckt |
| 45, 48 | neblig |
| 51, 53, 55, 56, 57 | Nieselregen |
| 61, 63 | Regen |
| 65 | starker Regen |
| 66, 67 | gefrierender Regen |
| 71, 73, 77 | Schneefall |
| 75 | starker Schneefall |
| 80, 81, 82 | Regenschauer |
| 85, 86 | Schneeschauer |
| 95, 96, 97, 99 | Gewitter |
| andere | wechselhaft |

Der Dienst ist für private Nutzung kostenlos (unter 10.000 Aufrufe pro Tag, CC BY 4.0). Der Hinweis auf Open-Meteo steht im README.

### Spotify

Grundlage ist der Development Mode nach der Änderung vom Februar 2026: Die App gehört dem Premium-Konto des Nutzers, `GET /search` liefert höchstens 10 Treffer, `GET /artists/{id}/top-tracks` ist entfernt. Die Player-Endpunkte und `GET /me/playlists` bleiben verfügbar.

Einmaliger Login:

1. Der Nutzer registriert eine App im Spotify-Dashboard mit der Redirect-URI `http://127.0.0.1:8898/callback`.
2. `nova spotify-login` fragt im Terminal nach der Client-ID und öffnet den Browser.
3. Authorization Code mit PKCE; ein einmaliger `TcpListener` auf `127.0.0.1:8898` nimmt die Antwort an, prüft `state` und gibt nach 120 s auf.
4. Client-ID und Refresh-Token landen in KWallet.

Scopes: `user-modify-playback-state`, `user-read-playback-state`, `playlist-read-private`.

Abspielen:

1. Access-Token mit dem Refresh-Token holen. Ein neu geliefertes Refresh-Token ersetzt das alte in KWallet. Das Token wird nicht zwischengespeichert, jeder Befehl holt es neu.
2. `GET /me/player/devices`; ohne Gerät gilt „Spotify läuft gerade nicht.“ Sonst wird das aktive Gerät gewählt, ohne aktives das erste.
3. Treffer nach `kind`:
   - `Any`: `GET /search?type=artist,track&limit=1`. Stimmt der Künstlername ohne Groß- und Kleinschreibung mit der Anfrage überein, gilt der Künstler, sonst der Titel.
   - `Playlist`: erst `GET /me/playlists?limit=50` nach einem Namen durchsuchen, der die Anfrage enthält, dann `GET /search?type=playlist&limit=1`.
   - `Album`, `Track`: `GET /search?type=album|track&limit=1`.
4. `PUT /me/player/play?device_id={id}` mit `context_uri` für Künstler, Alben und Playlists, mit `uris` für Titel.

### Geheimnisse

`secrets` ruft `secret-tool` auf, Attribute `service=nova`, `account=spotify_client_id` bzw. `account=spotify_refresh_token`. Der Wert geht beim Schreiben über stdin, nie als Argument. Tokens erscheinen nie im Log.

### Fehler

- Alle HTTP-Aufrufe haben 5 s Timeout.
- Ein Fehler eines Befehls beendet weder Schleife noch Hilfsprozesse.
- `playerctl`, `gtk-launch`, `xdg-open` und `secret-tool` werden erst beim Gebrauch gesucht. Ein fehlendes Programm verhindert also nicht den Start von Nova.

## Konfiguration

Änderungen, alle typisiert und validiert in `nova-core`:

```toml
# nova.toml
[wetter]
heimatort = ""              # Name, wird bei jeder Anfrage aufgelöst; später in den Einstellungen gesetzt
```

Ein leerer Heimatort ist gültig. `apps.toml` wird in Phase 2 zum ersten Mal gelesen, `nova-core` bekommt dafür `Apps::load`. Geprüft wird: Schlüssel klein geschrieben und ohne führendes oder folgendes Leerzeichen, Werte nicht leer und ohne `/`. Fehlerhafte Einträge verhindern den Start.

Ausgelieferte Liste:

```toml
[apps]
firefox = "firefox"
chrome = "google-chrome"
dolphin = "org.kde.dolphin"
konsole = "org.kde.konsole"
kate = "org.kde.kate"
spotify = "spotify"
obsidian = "obsidian"
discord = "discord"
"vs code" = "com.microsoft.VSCode"
steam = "steam"
```

## Sicherheit

- Nur exakte Treffer in `apps.toml` starten; es gibt keine Teilwort-Suche und keinen Rückgriff auf den Pfad.
- Die Suche übergibt `xdg-open` nur eine URL mit festem Anfang und kodiertem Begriff.
- Keine Shell, kein `sudo`.
- Client-ID und Refresh-Token liegen nur in KWallet, nie in `config/*.toml`.

## Tests

| Ebene | Prüfung |
|---|---|
| `commands::parse` | jede Regel mit Sätzen in Whisper-Schreibweise (Großschreibung, Satzzeichen), Reihenfolge, Muster ohne Inhalt, Fragen, die nicht greifen dürfen |
| `voice_loop` | jede neue Tabellenzeile, veraltete Antwort, Barge-in in `Working`, zusammengeführte Äußerungen vor `Run` |
| `weather` | Vorlagen, Rundung, „minus“, alle WMO-Gruppen, Antwort ohne `results`, echtes JSON als Beispiel |
| `spotify` | Auswahl aus Suchantworten, Geräteauswahl, PKCE-Prüfwert gegen das Beispiel aus RFC 7636 |
| `search` | URL-Kodierung mit Umlauten, Leerzeichen und `&` |
| `launcher` | gelistet, nicht gelistet, Teilwort wird abgelehnt |
| `supervisor` | Integrationstest mit Fake-Helfer: Befehl bis Antwort, stille Antwort, Arbeitsfrist, Barge-in in `Working` |
| `nova-core` | ausgelieferte Konfiguration lädt, ungültige `apps.toml` wird abgelehnt |

Netzwerk und `playerctl` werden in Tests nicht angefasst: Die Module trennen Antwort-Auswertung (rein, getestet) von Aufruf (dünn, in der Abnahme geprüft).

Vor jedem Commit laufen `cargo fmt --all`, `cargo clippy --all-targets` und `cargo test`.

## Abnahme am Gerät

Mit Echo-Modul, Ausgabe über Lautsprecher, gesprochen vom Nutzer. Voraussetzungen: `playerctl` installiert, Spotify-App registriert und `nova spotify-login` gelaufen, Heimatort in der lokalen `nova.toml` gesetzt.

| Prüfung | Ziel |
|---|---|
| Transport | Play, Pause, Weiter, Zurück: je 5 von 5 |
| Lautstärke | „lauter“, „leiser“, „Lautstärke auf 30“ wirken |
| Spotify-Inhalte | Künstler, Album, Titel, eigene Playlist: je 3 von 3 |
| Wetter | Heimatort heute und morgen, ein genannter Ort, ein unbekannter Ort |
| Programme | alle zehn Einträge öffnen, ein nicht gelistetes Programm wird abgelehnt |
| Suche | Google und YouTube öffnen mit korrektem Begriff, auch mit Umlauten |
| Unbekannt | eine allgemeine Frage ergibt „Das kann ich noch nicht.“ |
| Barge-in in `Working` | Sprechen während einer Wetteranfrage verwirft die Antwort, 3 von 3 |
| Programm überlebt Nova | per Stimme ein Programm öffnen, Nova mit Ctrl+C beenden: das Programm läuft weiter |
| Browser-Suche ohne Wartezeit | nach einer Suche kommt die Antwort sofort, kein „Das hat zu lange gedauert.“ |
| Offline | ohne Netz antwortet Nova mit dem Fehlersatz, die Schleife läuft weiter |
| Musik im Raum | Wake Word und „Pause“ bei Musik in üblicher Lautstärke: mindestens 8 von 10 |

Die letzte Prüfung ist das bekannte Risiko dieser Phase: Das Echo-Modul rechnet nur heraus, was über `nova_sink` läuft, Spotify spielt aber auf das Standardgerät. Fällt sie durch, wird nachgebessert, entweder durch Spotify auf `nova_sink` oder durch Leiserstellen der Musik beim Wake Word. Die Wahl fällt anhand des Ergebnisses, das Ziel wird nicht stillschweigend gesenkt.

Lässt sich die Lautstärke über `playerctl` bei der nativen Spotify-App nicht setzen, weicht `music` für `VolumeSet` und `VolumeStep` auf `PUT /me/player/volume` aus.

## Dokumentation

Nach Abschluss werden aktualisiert: `docs/architektur.md` (Phasenstatus, Arbeits-Thread, Zustand `Working`), `README.md` (Stand, Spotify-Login, `playerctl`, Hinweis auf Open-Meteo).
