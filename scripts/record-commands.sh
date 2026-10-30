#!/usr/bin/env bash
# Record your own voice for the voice-command accuracy test
# (doc/plan/07-voice-commands.md, V2 step 7). One 3 s clip per phrase in
# tests/fixtures/commands/phrases.tsv, saved as <id>.wav next to it.
# Existing clips are kept; delete one to record it again.
# Then: just eval-commands
set -euo pipefail
dir="$(cd "$(dirname "$0")/.." && pwd)/tests/fixtures/commands"
secs="${SECS:-3}"

grep -v '^#' "$dir/phrases.tsv" | while IFS=$'\t' read -r id _app _expected say; do
    out="$dir/$id.wav"
    [[ -f "$out" ]] && continue
    printf '\nSay:  %s\nPress Enter, then speak (%s s)... ' "$say" "$secs"
    read -r </dev/tty
    timeout "$secs" parecord --rate=16000 --channels=1 --format=s16le \
        --file-format=wav "$out" || true
    echo "saved $id.wav"
done
echo "done: $(ls "$dir"/*.wav 2>/dev/null | wc -l) clips in $dir"
