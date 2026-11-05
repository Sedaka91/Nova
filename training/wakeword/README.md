# Wake Word „Hey Nova“ trainieren

Eigenständiges Trainingsprojekt für `models/hey_nova.onnx`. Nova selbst hängt zur Laufzeit nicht davon ab.

## Voraussetzungen

- `uv`, `git`, `wget`, `curl`, `unzip`, `ffmpeg`, `flock`, Python 3.10 (von uv).
- NVIDIA-Grafikkarte. `torch` bringt sein eigenes CUDA mit (cu124), die System-CUDA-Version spielt keine Rolle.
- Mindestens 40 GB frei auf `/home`, die Grafikkarte nicht von Ollama oder Spielen belegt.
- Eigene Takes unter `~/.cache/nova-training/recordings/` (`./record.sh positive`, `./record.sh negative`).
- Die Merkmalsmodelle von openWakeWord unter `~/.local/share/nova/openwakeword/` (`uv run --project helpers python -m nova_helpers.setup`).

Alles Weitere landet unter `~/.cache/nova-training`, auch die virtuelle Umgebung und der uv-Cache: Quellen unter `repos/`, Daten unter `data/`, Ergebnisse unter `out/`.

## Aufruf

```sh
./run.sh setup   # installieren, klonen, herunterladen; Vorhandenes wird übersprungen
./run.sh smoke   # kleiner Lauf in out-smoke/
./run.sh train   # voller Lauf im Vordergrund
./run.sh start   # voller Lauf losgelöst vom Terminal
./run.sh test [modell.onnx]   # persönliche Testmenge, ohne Angabe out/hey_nova.onnx
```

`start` schreibt nach `out/train.log` und legt am Ende `out/DONE` oder `out/FAILED` mit dem Exit-Code an. Ergebnis: `out/hey_nova.onnx`. Fehlt der Marker und läuft kein Prozess mehr, ist der Lauf gestorben, etwa durch einen Absturz oder `kill -9`.

Ein zweites `start` bricht mit einer Meldung ab, solange ein Lauf die Sperre `out/.lock` hält, und lässt Protokoll und Marker unangetastet. Ein `train` im Vordergrund nimmt die Sperre nicht.

Alle Befehle lassen sich nach einem Abbruch neu starten: Downloads laufen mit `wget -c` weiter, fehlende Clips werden ergänzt. Abgebrochene Merkmalsberechnung beginnt von vorn, das fertige Training überspringt sie.

`.features_complete` enthält einen Hash aus der Konfiguration und den Namen der Trainings-Takes. Passt er nicht mehr, berechnet der nächste Lauf alle Merkmale neu, auch nach einer reinen Kommentaränderung in `hey_nova.yml`.

Ist die Grafikkarte belegt, läuft der Rauchtest mit `CUDA_VISIBLE_DEVICES= ./run.sh smoke` auf der CPU. Die VRAM-Sperre greift dann nicht, für den vollen Lauf ist das zu langsam.

## Eigene Takes

Die Aufnahmen sind persönliche Sprachdaten. Sie bleiben unter `~/.cache/nova-training`, nie im Repo, und verlassen den Rechner nicht. Ausschlussliste, Testmenge, Anteile und Startwert stehen in `hey_nova.yml` unter `own_takes`.

`run.sh` ruft vor der Clip-Erzeugung `scripts/prepare_takes.py` auf:

1. **Sprache finden:** Energietor in 20-ms-Fenstern, 30 dB unter dem lautesten Fenster. Stücke unter 100 ms (Tastenklick, Atmen) fallen weg, Pausen bis 400 ms bleiben zusammen, das längste Stück gilt.
2. **Zuschneiden:** 50 ms vor und 100 ms nach der Sprache bleiben stehen, wie in den synthetischen Clips. Danach gilt dieselbe Lage wie dort: `augment_clips` setzt das Ende des Clips 0 bis 200 ms vor das Ende des Fensters. Der Pegel bleibt unverändert, er liegt schon im Bereich der synthetischen Clips.
3. **Zurückhalten:** Aus jedem von 10 (positiv) und 6 (negativ) gleich langen Abschnitten der Sitzung kommt mit festem Startwert ein Take in die Testmenge, die je zwei ersten und letzten Takes nie. Der Rest geht ins Training. `takes/*/test/` enthält die unbearbeiteten Aufnahmen, das Training sieht sie nie.
4. **Vervielfachen:** Harte Links `own_take_NNN_K.wav` in `positive_train/` und `negative_train/`, bis sie 25 % beziehungsweise 10 % von `n_samples` ausmachen: 12.500 positive aus 37 Takes, 5.000 negative aus 22 Takes. `train.py` zählt sie mit und erzeugt nur die übrigen Clips. Die Takes stecken nur in den Trainingsordnern; die Recall-Werte im Trainingsprotokoll beruhen also nur auf der englischen Stimme, aussagekräftig für deine Stimme ist `run.sh test`.

`augment_clips` behandelt jeden Ordnereintrag für sich: Position, EQ und Verzerrung je Clip, Tonhöhe, Bandsperre, Rauschen, Hintergrund, Pegel und Nachhall je Stapel. So unterscheiden sich die Kopien, ein Augmentierungslauf reicht (`augmentation_rounds: 1`).

`run.sh test` braucht `helpers/.venv`, in einem frischen Worktree also vorher `cd helpers && uv sync`. Es lädt das Modell wie Nova selbst (`inference_framework="onnx"`) und spielt jede Aufnahme in 80-ms-Blöcken ein, mit 2 s Stille davor und 1 s danach. Dazu kommen 60 s Hintergrund aus `audioset_16k` und `fma_16k`, Sperrzeit nach einer Auslösung 1 s. Ausgabe bei Schwelle 0,5: Spitzenwert je Take, Recall, Fehlauslösungen.

## Abweichungen vom offiziellen Notebook

Die Klone bleiben unverändert. `run.sh` legt dafür `scripts/nova_train.py` im Cache an, einen Wrapper um `train.py` von openWakeWord.

| Abweichung | Grund |
|---|---|
| Kein TensorFlow, `onnx_tf`, `tensorflow_probability`; `onnx` ohne Pin | Nova braucht nur `.onnx`. Der Schalter `--convert_to_tflite` in `train.py` steht immer auf wahr, der Wrapper bricht die Umwandlung nach dem ONNX-Export ab. |
| Kein `datasets`, kein `deep-phonemizer` | Daten kommen direkt per `wget`. „nowa“ steht nicht im Wörterbuch; die Negativbeispiele dazu entstehen aus „hey nova“, das spart das Modell für unbekannte Wörter. |
| `torch` 2.5.1 cu124 | Lädt die älteren `.pt`-Stimmen und exportiert ONNX ohne Änderungen am Generator; bringt eigenes CUDA mit. |
| `piper-sample-generator` auf Commit `9c1019c` | Hat die von `train.py` erwartete Struktur und 16 kHz. Die Stimme `.pt` stammt aus Release v2.0.0. |
| Nur `en_US-libritts_r-medium`, dazu eigene Takes statt einer deutschen Stimme | Die Clips von `de_DE-mls-medium` klangen beim Anhören nicht brauchbar. |
| Englische Stimme spricht immer „nova“ | Englisches espeak verschluckt in „nowa“ das v. |
| Eindeutige Dateinamen und gemischte Texte bei jeder Erzeugung | Ein Neustart ergänzt Clips, statt nummerierte Dateien zu überschreiben. |
| `custom_negative_phrases` machen rund 10 % der Negativbeispiele aus | `train.py` allein spräche jede Phrase nur einmal pro 50.000 Texte. |
| Homophone der Zielphrase fallen aus den Negativbeispielen | Sonst landet etwa „hay nova“ dort. |
| Merkmale auf der CPU | Das Projekt hat kein `onnxruntime-gpu`. |
| VRAM-Grenze | `torch` darf nur so viel belegen, dass 1,5 GB für den Desktop frei bleiben. |
| Fehlauslösungs-Validierung in Stapeln zu 65.536 | `train.py` schiebt sonst 2,8 GB auf einmal auf die Grafikkarte. |
| AudioSet `bal_train/09.parquet` statt `bal_train09.tar` | Die Datei wurde im selben Datensatz umgestellt. |
| FMA small komplett, direkt von `os.unil.cloud.switch.ch` | Das ist die Quelle des Notebook-Datensatzes, dessen Ladeskript aktuelle `datasets` nicht mehr ausführen. Das Notebook nimmt nur 1 Stunde und empfiehlt mehr. |

## Ergebnis (5. November 2026)

Trainingsdauer rund 30 Minuten auf der RTX 3060 Ti mit 50.000 Positivbeispielen und 50.000 Schritten.

| Messung | Wert |
|---|---|
| Recall englische Stimme (Trainingslog) | 0,24 |
| Fehlauslösungen pro Stunde (Trainingslog) | 0,0 |
| Eigene Test-Takes, Schwelle 0,5 | 8 von 10 |
| Eigene Negativ-Takes, Schwelle 0,5 | 0 von 6 ausgelöst |
| Hintergrund, 60 s | 0 ausgelöst, Spitze 0,002 |

Die Werte der eigenen Takes sind aus `run.sh test`. Zehn Takes sind eine kleine Stichprobe; die Abnahme am Gerät ist maßgeblich. Die 60 s Hintergrund stammen aus denselben Ordnern, aus denen das Training seinen Hintergrund mischt. Sie sind also kein unabhängiges Maß für Fehlauslösungen.
