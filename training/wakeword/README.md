# Wake Word „Hey Nova“ trainieren

Eigenständiges Trainingsprojekt für `models/hey_nova.onnx`. Nova selbst hängt zur Laufzeit nicht davon ab.

## Voraussetzungen

- `uv`, `git`, `wget`, `curl`, `unzip`, `ffmpeg`, Python 3.10 (von uv).
- NVIDIA-Grafikkarte. `torch` bringt sein eigenes CUDA mit (cu124), die System-CUDA-Version spielt keine Rolle.
- Mindestens 40 GB frei auf `/home`, die Grafikkarte nicht von Ollama oder Spielen belegt.
- Die Merkmalsmodelle von openWakeWord unter `~/.local/share/nova/openwakeword/` (`uv run --project helpers python -m nova_helpers.setup`).

Alles Weitere landet unter `~/.cache/nova-training`, auch die virtuelle Umgebung und der uv-Cache: Quellen unter `repos/`, Daten unter `data/`, Ergebnisse unter `out/`.

## Aufruf

```sh
./run.sh setup   # installieren, klonen, herunterladen; Vorhandenes wird übersprungen
./run.sh smoke   # kleiner Lauf in out-smoke/, 3 deutsche und 3 englische Clips nach samples/
./run.sh train   # voller Lauf im Vordergrund
./run.sh start   # voller Lauf losgelöst vom Terminal
```

`start` schreibt nach `out/train.log` und legt am Ende `out/DONE` oder `out/FAILED` mit dem Exit-Code an. Ergebnis: `out/hey_nova.onnx`.

Alle Befehle lassen sich nach einem Abbruch neu starten: Downloads laufen mit `wget -c` weiter, fehlende Clips werden ergänzt. Abgebrochene Merkmalsberechnung beginnt von vorn, das fertige Training überspringt sie.

## Abweichungen vom offiziellen Notebook

Die Klone bleiben unverändert. `run.sh` legt dafür `scripts/nova_train.py` im Cache an, einen Wrapper um `train.py` von openWakeWord.

| Abweichung | Grund |
|---|---|
| Kein TensorFlow, `onnx_tf`, `tensorflow_probability`; `onnx` ohne Pin | Nova braucht nur `.onnx`. Der Schalter `--convert_to_tflite` in `train.py` steht immer auf wahr, der Wrapper bricht die Umwandlung nach dem ONNX-Export ab. |
| Kein `datasets`, kein `deep-phonemizer` | Daten kommen direkt per `wget`. „nowa“ steht nicht im Wörterbuch; die Negativbeispiele dazu entstehen aus „hey nova“, das spart das Modell für unbekannte Wörter. |
| `torch` 2.5.1 cu124 | Lädt die älteren `.pt`-Stimmen und exportiert ONNX ohne Änderungen am Generator; bringt eigenes CUDA mit. |
| `piper-sample-generator` auf Commit `9c1019c` | Hat die von `train.py` erwartete Struktur, 16 kHz, die deutsche JSON und `min_phoneme_count`. Die Stimmen `.pt` stammen aus Release v2.0.0. |
| Zwei Stimmen in einem Lauf | Der Wrapper teilt jede Anfrage hälftig auf `de_DE-mls-medium` und `en_US-libritts_r-medium` auf, Dateien heißen `de_*.wav` und `en_*.wav`. |
| Deutsche Stimme: Satz wiederholen, ein Stück ausschneiden, Rauschen 0,333, Stapelgröße 4 | Die Stimme lallt bei kurzen Phrasen; so empfiehlt es die README des Generators. |
| Schreibweise je Stimme | Deutsches espeak liest „hey“ als „Hai“, darum bekommt die deutsche Stimme „hay“. Englisches espeak verschluckt in „nowa“ das v, darum spricht die englische Stimme nur „nova“. |
| `custom_negative_phrases` machen rund 10 % der Negativbeispiele aus | `train.py` allein spräche jede Phrase nur einmal pro 50.000 Texte. |
| Homophone der Zielphrase fallen aus den Negativbeispielen | Sonst landet etwa „hay nova“ dort. |
| Merkmale auf der CPU | Das Projekt hat kein `onnxruntime-gpu`. |
| VRAM-Grenze | `torch` darf nur so viel belegen, dass 1,5 GB für den Desktop frei bleiben. |
| Fehlauslösungs-Validierung in Stapeln zu 65.536 | `train.py` schiebt sonst 2,8 GB auf einmal auf die Grafikkarte. |
| AudioSet `bal_train/09.parquet` statt `bal_train09.tar` | Die Datei wurde im selben Datensatz umgestellt. |
| FMA small komplett, direkt von `os.unil.cloud.switch.ch` | Das ist die Quelle des Notebook-Datensatzes, dessen Ladeskript aktuelle `datasets` nicht mehr ausführen. Das Notebook nimmt nur 1 Stunde und empfiehlt mehr. |

## Ergebnis

Wird nach dem vollen Lauf ergänzt.
