#!/usr/bin/env bash
# Trains the "Hey Nova" wake word. See README.md.
#
#   ./run.sh setup   install, clone, download; existing data is skipped
#   ./run.sh smoke   setup, then a small run in out-smoke/
#   ./run.sh train   setup, then the full run in the foreground
#   ./run.sh start   ./run.sh train fully detached, log in out/train.log,
#                    marker out/DONE or out/FAILED holds the exit code
#   ./run.sh test [model.onnx]   scores the held-out takes and background audio
set -euo pipefail

PROJECT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CACHE=${NOVA_TRAINING_CACHE:-$HOME/.cache/nova-training}
OWW_MODELS=$HOME/.local/share/nova/openwakeword

OPENWAKEWORD_COMMIT=368c03716d1e92591906a84949bc477f3a834455
# Has the module layout train.py imports and writes 16 kHz (v3.x writes 22.05 kHz).
PIPER_GENERATOR_COMMIT=9c1019c932eda098aa70d63cdeac803dea983f53
VOICE_URL=https://github.com/rhasspy/piper-sample-generator/releases/download/v2.0.0
HF=https://huggingface.co/datasets
FMA_URL=https://os.unil.cloud.switch.ch/fma/fma_small.zip

export UV_CACHE_DIR=$CACHE/uv-cache
export UV_PROJECT_ENVIRONMENT=$CACHE/venv
export UV_PYTHON_INSTALL_DIR=$CACHE/python
export PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True
PYTHON=$CACHE/venv/bin/python

fetch() { # url target
    wget -c --tries=20 --waitretry=15 --read-timeout=60 --progress=dot:giga -O "$2" "$1"
}

clone() { # url dir ref
    [[ -d $2/.git ]] || git clone "$1" "$2"
    git -C "$2" checkout --quiet --detach "$3"
}

# Converts every *.$3 below $1 to 16 kHz mono WAV in $2. Markers sit beside the
# directories because train.py treats every directory entry as audio.
# A few FMA clips are known to be corrupt, those are skipped.
to_16k_wav() {
    [[ -e $2.complete ]] && return
    mkdir -p "$2"
    find "$1" -type f -name "*.$3" -print0 | xargs -0 -P "$(($(nproc) / 2))" -I{} bash -c '
        out=$1/$(basename "$2" ".$3").wav
        [[ -e $out ]] && exit 0
        ffmpeg -nostdin -loglevel error -y -i "$2" -ac 1 -ar 16000 -c:a pcm_s16le "$out.tmp.wav" \
            && mv "$out.tmp.wav" "$out" || { rm -f "$out.tmp.wav"; echo "skipped $2" >&2; }
        exit 0
    ' _ "$2" {} "$3"
    touch "$2.complete"
}

setup_project() {
    mkdir -p "$CACHE"/{repos,data,out,scripts}
    (cd "$PROJECT_DIR" && uv sync --frozen)
    write_scripts

    clone https://github.com/dscripka/openWakeWord "$CACHE/repos/openwakeword" "$OPENWAKEWORD_COMMIT"
    local resources=$CACHE/repos/openwakeword/openwakeword/resources/models
    mkdir -p "$resources"
    for model in melspectrogram embedding_model; do
        cp -u "$OWW_MODELS/$model.onnx" "$resources/"
    done

    local generator=$CACHE/repos/piper-sample-generator
    clone https://github.com/rhasspy/piper-sample-generator "$generator" "$PIPER_GENERATOR_COMMIT"
    fetch "$VOICE_URL/en_US-libritts_r-medium.pt" "$generator/models/en_US-libritts_r-medium.pt"
}

download_data() {
    local data=$CACHE/data
    fetch "$HF/davidscripka/openwakeword_features/resolve/main/validation_set_features.npy" \
        "$data/validation_set_features.npy"
    fetch "$HF/davidscripka/openwakeword_features/resolve/main/openwakeword_features_ACAV100M_2000_hrs_16bit.npy" \
        "$data/openwakeword_features_ACAV100M_2000_hrs_16bit.npy"

    if [[ ! -e $data/mit_rirs.complete ]]; then
        mkdir -p "$data/mit_rirs"
        curl -fsS "https://huggingface.co/api/datasets/davidscripka/MIT_environmental_impulse_responses/tree/main/16khz" \
            | "$PYTHON" -I -c 'import json, sys; print("\n".join(e["path"] for e in json.load(sys.stdin)))' \
            | sed "s|^|$HF/davidscripka/MIT_environmental_impulse_responses/resolve/main/|" \
                >"$data/mit_rirs.urls"
        # A file, not stdin: wget garbled a line when reading the list from a pipe.
        wget -c --no-verbose --tries=20 --waitretry=15 -P "$data/mit_rirs" -i "$data/mit_rirs.urls"
        touch "$data/mit_rirs.complete"
    fi

    # The notebook's bal_train09.tar is now this Parquet part of the same dataset.
    fetch "$HF/agkphysics/AudioSet/resolve/main/data/bal_train/09.parquet" "$data/audioset_bal_train09.parquet"
    if [[ ! -e $data/audioset.complete ]]; then
        "$PYTHON" -I "$CACHE/scripts/extract_audioset.py" "$data/audioset_bal_train09.parquet" "$data/audioset"
        touch "$data/audioset.complete"
    fi
    to_16k_wav "$data/audioset" "$data/audioset_16k" flac

    fetch "$FMA_URL" "$data/fma_small.zip"
    if [[ ! -e $data/fma_16k.complete ]]; then
        unzip -q -o "$data/fma_small.zip" -d "$data/fma_small"
        to_16k_wav "$data/fma_small" "$data/fma_16k" mp3
        rm -rf "$data/fma_small"
    fi
}

train_with() { # config, relative to the cache
    "$PYTHON" -I "$CACHE/scripts/nova_train.py" --training_config "$1" "${@:2}"
}

# Identifies what the features are built from. Run after prepare_takes.py.
features_fingerprint() { # config
    {
        cat "$1"
        find "$CACHE/takes" -path '*/train/*.wav' -printf '%P\n' | LC_ALL=C sort
    } | sha256sum | cut -d' ' -f1
}

features_current() { # marker fingerprint
    [[ -f $1 && $(<"$1") == "$2" ]]
}

run_training() { # config output_dir
    cd "$CACHE"
    local features_complete=$2/.features_complete fingerprint overwrite=()
    # First, so that train.py counts the takes and generates only the rest.
    "$PYTHON" -I "$CACHE/scripts/prepare_takes.py" "$1"
    fingerprint=$(features_fingerprint "$1")
    train_with "$1" --generate_clips
    # train.py skips augmentation when a feature file exists, even a half-written or
    # outdated one. An empty marker from an older run counts as outdated.
    features_current "$features_complete" "$fingerprint" || overwrite=(--overwrite)
    train_with "$1" --augment_clips "${overwrite[@]}"
    echo "$fingerprint" >"$features_complete"
    train_with "$1" --train_model
}

smoke() {
    local config=$CACHE/scripts/hey_nova.smoke.yml
    sed -e 's/^n_samples: .*/n_samples: 500/' \
        -e 's/^n_samples_val: .*/n_samples_val: 100/' \
        -e 's/^steps: .*/steps: 500/' \
        -e 's|^output_dir: .*|output_dir: "./out-smoke"|' \
        "$PROJECT_DIR/hey_nova.yml" >"$config"
    run_training "$config" "$CACHE/out-smoke/hey_nova"
}

# Written into the cache so that the scripts never sit next to downloaded material.
write_scripts() {
    cat >"$CACHE/scripts/extract_audioset.py" <<'PY'
"""Writes the embedded audio files of an AudioSet Parquet part into a directory."""
import sys
from pathlib import Path

import pyarrow.parquet as pq

source, target = sys.argv[1:]
Path(target).mkdir(parents=True, exist_ok=True)
for batch in pq.ParquetFile(source).iter_batches(columns=["audio"], batch_size=64):
    for audio in batch.column("audio").to_pylist():
        Path(target, Path(audio["path"]).name).write_bytes(audio["bytes"])
PY
    cat >"$CACHE/scripts/prepare_takes.py" <<'PY'
"""Trims the recorded takes, holds some out and adds the rest to train.py's clip folders.

Runs in the cache directory. The clips of train.py are short and end right after
the speech, augment_clips then places that end 0 to 200 ms before the end of the
clip. Trimmed takes get the same treatment. Each folder entry is augmented on its
own, so hard links to the same take turn into differing training examples.
"""
import random
import shutil
import sys
from pathlib import Path

import numpy as np
import soundfile as sf
import yaml

FRAME = 320  # 20 ms at 16 kHz
GATE_DB = 30  # below the loudest frame
MIN_SPEECH_FRAMES = 5  # shorter bursts are key clicks and breath
MAX_GAP_FRAMES = 20  # the pause between "hey" and "nova" can reach 360 ms
# Silence around the speech, as in the synthetic clips.
LEAD_SAMPLES = 800
TAIL_SAMPLES = 1600
SESSION_EDGE = 2  # takes at each end of a session that are never held out


def speech_frames(samples, path):
    """Returns the first and the end frame of the longest stretch of speech."""
    n_frames = len(samples) // FRAME
    frames = samples[: n_frames * FRAME].reshape(n_frames, FRAME).astype(np.float64)
    power = (frames**2).mean(axis=1)
    active = np.flatnonzero(power > power.max() * 10 ** (-GATE_DB / 10))
    runs = np.split(active, np.flatnonzero(np.diff(active) > 1) + 1)
    spans = []
    for run in runs:
        if len(run) < MIN_SPEECH_FRAMES:
            continue
        if spans and run[0] - spans[-1][1] <= MAX_GAP_FRAMES:
            spans[-1][1] = run[-1] + 1
        else:
            spans.append([run[0], run[-1] + 1])
    if not spans:
        sys.exit(f"{path}: keine Sprache gefunden, Take in hey_nova.yml ausschließen")
    return max(spans, key=lambda span: span[1] - span[0])


def trim(source, target):
    samples, rate = sf.read(source, dtype="int16")
    first, end = speech_frames(samples, source)
    start = max(0, first * FRAME - LEAD_SAMPLES)
    sf.write(target, samples[start : end * FRAME + TAIL_SAMPLES], rate, subtype="PCM_16")


def held_out(numbers, count, rng):
    """Picks one take from each of count equal stretches of the session."""
    return {int(rng.choice(list(part))) for part in np.array_split(numbers[SESSION_EDGE:-SESSION_EDGE], count)}


config = yaml.safe_load(Path(sys.argv[1]).read_text())
own = config["own_takes"]
rng = random.Random(own["seed"])
shutil.rmtree("takes", ignore_errors=True)

for kind in ("positive", "negative"):
    recordings = Path("recordings", kind)
    train_dir = Path("takes", kind, "train")
    test_dir = Path("takes", kind, "test")
    train_dir.mkdir(parents=True)
    test_dir.mkdir()
    excluded = set(own[kind]["excluded"])
    numbers = sorted(int(p.stem.removeprefix("take_")) for p in recordings.glob("take_*.wav"))
    numbers = [number for number in numbers if number not in excluded]
    test = held_out(numbers, own[kind]["held_out"], rng)

    trimmed = []
    for number in numbers:
        name = f"take_{number:03d}.wav"
        if number in test:
            # Tests hear the unprocessed recording, like the microphone stream.
            shutil.copy(recordings / name, test_dir / name)
        else:
            trim(recordings / name, train_dir / name)
            trimmed.append(train_dir / name)

    clips = Path(config["output_dir"], config["model_name"], f"{kind}_train")
    clips.mkdir(parents=True, exist_ok=True)
    for clip in clips.glob("own_*.wav"):
        clip.unlink()
    n_copies = round(own[kind]["share"] * config["n_samples"])
    for copy in range(n_copies):
        take = trimmed[copy % len(trimmed)]
        (clips / f"own_{take.stem}_{copy}.wav").hardlink_to(take)
    print(f"{kind}: {len(trimmed)} Takes als {n_copies} Clips, zurückgehalten: {sorted(test)}")
PY
    cat >"$CACHE/scripts/test_takes.py" <<'PY'
"""Scores the held-out takes and background audio at Nova's threshold."""
import random
import sys
import wave
from pathlib import Path

import numpy as np
from openwakeword.model import Model

THRESHOLD = 0.5
BLOCK = 1280  # 80 ms
LOCKOUT_BLOCKS = 12  # Nova ignores further activations for 1 s
SAMPLE_RATE = 16000
BACKGROUND_SECONDS = 60
SEED = 1
# The model needs its context window of 1.28 s and ignores its first blocks.
LEAD = np.zeros(2 * SAMPLE_RATE, dtype=np.int16)
TAIL = np.zeros(SAMPLE_RATE, dtype=np.int16)


def read_wav(path):
    with wave.open(str(path)) as file:
        return np.frombuffer(file.readframes(file.getnframes()), dtype=np.int16)


def scores(model, samples):
    model.reset()
    blocks = [samples[i : i + BLOCK] for i in range(0, len(samples) - BLOCK + 1, BLOCK)]
    return [float(max(model.predict(block).values())) for block in blocks]


def activations(block_scores):
    count = 0
    locked_until = 0
    for index, score in enumerate(block_scores):
        if score >= THRESHOLD and index >= locked_until:
            count += 1
            locked_until = index + LOCKOUT_BLOCKS
    return count


def score_takes(model, directory, label):
    """Prints each take's peak score and returns how many reached the threshold."""
    reached = 0
    for path in sorted(directory.glob("*.wav")):
        peak = max(scores(model, np.concatenate([LEAD, read_wav(path), TAIL])))
        hit = peak >= THRESHOLD
        reached += hit
        print(f"{label} {path.stem}  Spitze {peak:.3f}  {'ausgelöst' if hit else '-'}")
    return reached


def background(data):
    paths = sorted([*(data / "audioset_16k").glob("*.wav"), *(data / "fma_16k").glob("*.wav")])
    random.Random(SEED).shuffle(paths)
    parts = []
    for path in paths:
        parts.append(read_wav(path))
        if sum(map(len, parts)) >= BACKGROUND_SECONDS * SAMPLE_RATE:
            break
    return np.concatenate(parts)[: BACKGROUND_SECONDS * SAMPLE_RATE]


model_path, feature_models, cache = sys.argv[1:]
model = Model(
    wakeword_models=[model_path],
    inference_framework="onnx",
    melspec_model_path=f"{feature_models}/melspectrogram.onnx",
    embedding_model_path=f"{feature_models}/embedding_model.onnx",
)
positive_dir = Path(cache, "takes/positive/test")
negative_dir = Path(cache, "takes/negative/test")
hits = score_takes(model, positive_dir, "positiv")
false_takes = score_takes(model, negative_dir, "negativ")
background_scores = scores(model, background(Path(cache, "data")))
false_background = activations(background_scores)

n_positive = len(list(positive_dir.glob("*.wav")))
n_negative = len(list(negative_dir.glob("*.wav")))
print(f"Hintergrund {BACKGROUND_SECONDS} s  Spitze {max(background_scores):.3f}")
print(f"Schwelle {THRESHOLD}")
print(f"Recall {hits}/{n_positive} = {hits / n_positive:.2f}")
print(f"Fehlauslösungen: {false_takes}/{n_negative} Negativ-Takes, {false_background} im Hintergrund")
PY
    cat >"$CACHE/scripts/nova_train.py" <<'PY'
"""Runs openWakeWord's unmodified train.py with the adjustments listed in README.md."""
import argparse
import importlib.machinery
import importlib.util
import os
import random
import re
import runpy
import sys
import uuid
from pathlib import Path

import pronouncing
import torch
import yaml

CACHE = Path.cwd()
OPENWAKEWORD = CACHE / "repos/openwakeword"
GENERATOR = CACHE / "repos/piper-sample-generator"
VRAM_RESERVE = int(1.5 * 1024**3)
VRAM_REQUIRED = 3 * 1024**3
VALIDATION_BATCH = 65536
# Share of custom negatives in each adversarial list. train.py alone would speak
# each of them only once per pass through 50000 texts.
CUSTOM_NEGATIVE_SHARE = 0.05
ADVERSARIAL_SOURCE = "hey nova"
TARGET_SOUND = [["HH", "EY"], ["N", "OW", "V", "AH"]]

sys.path[:0] = [str(OPENWAKEWORD), str(GENERATOR)]

import generate_samples as generator  # noqa: E402
import openwakeword.data  # noqa: E402
import openwakeword.utils  # noqa: E402


class TfliteSkipped(Exception):
    """train.py reached the TFLite conversion, which Nova does not need."""


def generate_english(text, max_samples, output_dir, generate=generator.generate_samples, **kwargs):
    """Generates with the default voice, in unique files that a restart adds to.

    English espeak drops the v in "nowa", which is only meant for German speakers.
    """
    kwargs["file_names"] = [f"{uuid.uuid4().hex}.wav" for _ in range(max_samples)]
    texts = [re.sub(r"\bnowa\b", "nova", t) for t in random.sample(text, len(text))]
    generate(texts, output_dir, max_samples=max_samples, **kwargs)


def sounds_like_target(text):
    words = [pronouncing.phones_for_word(word) for word in text.split()]
    if not all(words):
        return False
    return [[p.rstrip("012") for p in phones[0].split()] for phones in words] == TARGET_SOUND


def adversarial_texts(input_text, N, generate=openwakeword.data.generate_adversarial_texts, **kwargs):
    """Adversarial texts for a target, more custom negatives, without homophones of it.

    "nowa" is the same sound as "nova" and not in the dictionary, which would
    make openWakeWord fetch a phonemizer model. "hey nova" stands in for it.
    """
    texts = [t for t in generate(ADVERSARIAL_SOURCE, N, **kwargs) if not sounds_like_target(t)]
    repeats = max(1, int(N * CUSTOM_NEGATIVE_SHARE) // len(CUSTOM_NEGATIVES))
    texts += CUSTOM_NEGATIVES * repeats
    random.shuffle(texts)
    return texts


def features_on_cpu(*args, compute=openwakeword.utils.compute_features_from_generator, **kwargs):
    """Uses the CPU, as the project has no onnxruntime-gpu."""
    kwargs.update(device="cpu", ncpu=max(1, (os.cpu_count() or 2) // 2))
    compute(*args, **kwargs)


def skip_tflite():
    """train.py's flag defaults to the truthy string "False", so conversion always runs."""

    def prepare(*_args, **_kwargs):
        raise TfliteSkipped

    # torch inspects the specs of everything in sys.modules, so stubs need one.
    for name in ("onnx_tf", "onnx_tf.backend", "tensorflow"):
        sys.modules[name] = importlib.util.module_from_spec(importlib.machinery.ModuleSpec(name, None))
    sys.modules["onnx_tf.backend"].prepare = prepare


def split_validation_batches(loader=torch.utils.data.DataLoader):
    """Keeps train.py's false positive set, one 2.8 GB batch, from filling the VRAM.

    Its loops add up results over batches. The other validation set is smaller
    than the limit and stays whole, as train.py only reads its last batch.
    """

    def split(dataset, batch_size=None, **kwargs):
        limited = batch_size and min(batch_size, VALIDATION_BATCH)
        return loader(dataset, batch_size=limited, **kwargs)

    torch.utils.data.DataLoader = split


def limit_vram():
    if not torch.cuda.is_available():
        return  # CUDA_VISIBLE_DEVICES="" runs everything on the CPU, see README.md
    free, total = torch.cuda.mem_get_info()
    if free - VRAM_RESERVE < VRAM_REQUIRED:
        sys.exit(f"Zu wenig freier Grafikspeicher ({free >> 20} MiB). Läuft ein Spiel oder Ollama?")
    torch.cuda.set_per_process_memory_fraction((free - VRAM_RESERVE) / total)


parser = argparse.ArgumentParser(add_help=False)
parser.add_argument("--training_config", required=True)
config = yaml.safe_load(Path(parser.parse_known_args()[0].training_config).read_text())
CUSTOM_NEGATIVES = list(config["custom_negative_phrases"])

generator.generate_samples = generate_english
openwakeword.data.generate_adversarial_texts = adversarial_texts
openwakeword.utils.compute_features_from_generator = features_on_cpu
skip_tflite()
split_validation_batches()
limit_vram()

sys.argv[0] = str(OPENWAKEWORD / "openwakeword/train.py")
try:
    runpy.run_path(sys.argv[0], run_name="__main__")
except TfliteSkipped:
    pass
PY
}

case ${1:-} in
    setup)
        setup_project
        download_data
        ;;
    smoke)
        setup_project
        download_data
        smoke
        ;;
    train)
        setup_project
        download_data
        run_training "$PROJECT_DIR/hey_nova.yml" "$CACHE/out/hey_nova"
        ;;
    test)
        helpers=$(cd "$PROJECT_DIR/../../helpers" && pwd)
        if [[ ! -x $helpers/.venv/bin/python ]]; then
            echo "$helpers/.venv fehlt. Zuerst ausführen: cd $helpers && uv sync" >&2
            exit 1
        fi
        mkdir -p "$CACHE/scripts"
        write_scripts
        "$helpers/.venv/bin/python" -I "$CACHE/scripts/test_takes.py" \
            "${2:-$CACHE/out/hey_nova.onnx}" "$OWW_MODELS" "$CACHE"
        ;;
    start)
        mkdir -p "$CACHE/out"
        # The detached run inherits descriptor 9 and so holds the lock until it ends.
        exec 9>"$CACHE/out/.lock"
        if ! flock -n 9; then
            echo "Es läuft bereits ein Training, Protokoll: $CACHE/out/train.log" >&2
            exit 1
        fi
        rm -f "$CACHE/out/DONE" "$CACHE/out/FAILED"
        setsid nohup nice -n 10 bash "${BASH_SOURCE[0]}" detached \
            >"$CACHE/out/train.log" 2>&1 </dev/null &
        echo "Läuft im Hintergrund, Protokoll: $CACHE/out/train.log"
        ;;
    detached)
        code=0
        bash "${BASH_SOURCE[0]}" train || code=$?
        if ((code == 0)); then
            echo "$code" >"$CACHE/out/DONE"
        else
            echo "$code" >"$CACHE/out/FAILED"
        fi
        ;;
    *)
        echo "Aufruf: $0 setup|smoke|train|start|test [modell.onnx]" >&2
        exit 2
        ;;
esac
