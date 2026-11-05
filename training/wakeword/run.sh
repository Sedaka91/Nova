#!/usr/bin/env bash
# Trains the "Hey Nova" wake word. See README.md.
#
#   ./run.sh setup   install, clone, download; existing data is skipped
#   ./run.sh smoke   setup, then a small run in out-smoke/
#   ./run.sh train   setup, then the full run in the foreground
#   ./run.sh start   ./run.sh train fully detached, log in out/train.log,
#                    marker out/DONE or out/FAILED holds the exit code
set -euo pipefail

PROJECT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
CACHE=${NOVA_TRAINING_CACHE:-$HOME/.cache/nova-training}
OWW_MODELS=$HOME/.local/share/nova/openwakeword

OPENWAKEWORD_COMMIT=368c03716d1e92591906a84949bc477f3a834455
# Last generator commit with everything needed here: the module layout train.py
# imports, 16 kHz output, the German voice's JSON and min_phoneme_count (v2.0.0
# lacks the latter two, v3.x writes 22.05 kHz and dropped min_phoneme_count).
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
    fetch "$VOICE_URL/de_DE-mls-medium.pt" "$generator/models/de_DE-mls-medium.pt"
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

run_training() { # config output_dir
    cd "$CACHE"
    local features_complete=$2/.features_complete overwrite=()
    train_with "$1" --generate_clips
    # train.py skips augmentation when a feature file exists, even a half-written one.
    [[ -e $features_complete ]] || overwrite=(--overwrite)
    train_with "$1" --augment_clips "${overwrite[@]}"
    touch "$features_complete"
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

    mkdir -p "$PROJECT_DIR/samples"
    for voice in de en; do
        find "$CACHE/out-smoke/hey_nova/positive_train" -name "${voice}_*.wav" \
            | sort | sed -n 1,3p | xargs -I{} cp {} "$PROJECT_DIR/samples/"
    done
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
    cat >"$CACHE/scripts/nova_train.py" <<'PY'
"""Runs openWakeWord's unmodified train.py with the adjustments listed in README.md."""
import argparse
import gc
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
VOICES = {
    "de": str(GENERATOR / "models/de_DE-mls-medium.pt"),
    "en": str(GENERATOR / "models/en_US-libritts_r-medium.pt"),
}
# The MLS-trained German voice babbles on short phrases. The generator's README
# prescribes repeating the phrase, keeping one repetition and lowering the noise.
# These inputs are long, hence the small batch. The generator's automatic batch
# reduction fails together with min_phoneme_count.
VOICE_SETTINGS = {
    "de": {
        "min_phoneme_count": 300,
        "noise_scales": [0.333],
        "noise_scale_ws": [0.333],
        "batch_size": 4,
        "auto_reduce_batch_size": False,
    },
    "en": {},
}
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


def spelled_for(voice, text):
    """Spells a text so that espeak gets the intended sound from each voice."""
    if voice == "de":
        # German espeak reads "hey" as "Hai". The comma belongs to the repetition fix.
        return re.sub(r"\bhey\b", "hay", text) + ","
    # English espeak drops the v in "nowa", which is only meant for German speakers.
    return re.sub(r"\bnowa\b", "nova", text)


def generate_both_voices(text, max_samples, output_dir, generate=generator.generate_samples, **kwargs):
    """Splits a request evenly between the voices, counting clips of earlier runs."""
    kwargs.pop("file_names", None)

    def clips_of(voice):
        return len(list(Path(output_dir).glob(f"{voice}_*.wav")))

    per_voice = (max_samples + sum(map(clips_of, VOICES))) // len(VOICES)
    for voice, model in VOICES.items():
        settings = {**kwargs, **VOICE_SETTINGS[voice]}
        while (missing := per_voice - clips_of(voice)) > 0:
            # Without unique names the voices would overwrite each other's numbered clips.
            names = [f"{voice}_{uuid.uuid4().hex}.wav" for _ in range(missing)]
            texts = [spelled_for(voice, t) for t in random.sample(text, len(text))]
            try:
                generate(texts, output_dir, model=model, max_samples=missing, file_names=names, **settings)
            except torch.cuda.OutOfMemoryError:
                # Finished clips are on disk, the next round continues from there.
                if per_voice - clips_of(voice) == missing:
                    raise
            gc.collect()
            torch.cuda.empty_cache()


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
    free, total = torch.cuda.mem_get_info()
    if free - VRAM_RESERVE < VRAM_REQUIRED:
        sys.exit(f"Zu wenig freier Grafikspeicher ({free >> 20} MiB). Läuft ein Spiel oder Ollama?")
    torch.cuda.set_per_process_memory_fraction((free - VRAM_RESERVE) / total)


parser = argparse.ArgumentParser(add_help=False)
parser.add_argument("--training_config", required=True)
config = yaml.safe_load(Path(parser.parse_known_args()[0].training_config).read_text())
CUSTOM_NEGATIVES = list(config["custom_negative_phrases"])

generator.generate_samples = generate_both_voices
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
    start)
        mkdir -p "$CACHE/out"
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
        echo "Aufruf: $0 setup|smoke|train|start" >&2
        exit 2
        ;;
esac
