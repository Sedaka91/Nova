#!/usr/bin/env bash
# Records wake word takes as 16 kHz mono WAV, outside the repository.
# Usage: record.sh [positive|negative] [total takes]. Existing takes are kept, so a
# session can be stopped with q and continued later.
set -euo pipefail

kind=${1:-positive}
total=${2:-60}
seconds=2.5

case "$kind" in
    positive) prompt="„Hey Nova“" ;;
    negative) prompt="etwas Ähnliches, zum Beispiel „Hey Nora“, „Hey Oma“, „November“, „Nova“" ;;
    *)
        echo "Verwendung: $0 [positive|negative] [Anzahl]" >&2
        exit 2
        ;;
esac

dir=$HOME/.cache/nova-training/recordings/$kind
mkdir -p "$dir"
take=$(find "$dir" -name 'take_*.wav' | wc -l)

echo "Sprich nach Enter $prompt. Jedes Take dauert $seconds s, q beendet."
while ((take < total)); do
    read -r -p "[$((take + 1))/$total] Enter oder q: " answer
    [[ $answer == q ]] && break
    file=$(printf '%s/take_%03d.wav' "$dir" "$take")
    # timeout ends pw-record with SIGINT so that it finalizes the WAV header.
    timeout --signal=INT "$seconds" pw-record --rate 16000 --channels 1 --format s16 "$file" || true
    take=$((take + 1))
done
echo "$take Takes in $dir"
