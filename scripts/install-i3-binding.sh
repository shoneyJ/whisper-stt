#!/usr/bin/env bash
# Append F9 hold-to-talk bindings to ~/.config/i3/config (idempotent).
set -euo pipefail

I3_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/i3/config"
MARKER_OPEN="# >>> whisper-stt bindings >>>"
MARKER_CLOSE="# <<< whisper-stt bindings <<<"

if [ ! -f "$I3_CONFIG" ]; then
    echo "i3 config not found at $I3_CONFIG" >&2
    exit 1
fi

if ! command -v whisperctl >/dev/null 2>&1; then
    echo "warning: 'whisperctl' is not on PATH" >&2
    echo "         build + install with: cargo build --release --workspace && \\" >&2
    echo "         install -Dm755 target/release/whisperctl ~/.local/bin/whisperctl" >&2
    echo "         (and the same for whisperd)" >&2
    echo "         continuing anyway — i3 will look it up at exec time." >&2
fi

if grep -qF "$MARKER_OPEN" "$I3_CONFIG"; then
    echo "whisper-stt bindings already present in $I3_CONFIG"
    exit 0
fi

cat >> "$I3_CONFIG" <<EOF

$MARKER_OPEN
bindsym            F9 exec --no-startup-id whisperctl start
bindsym --release  F9 exec --no-startup-id whisperctl stop
$MARKER_CLOSE
EOF

echo "appended whisper-stt bindings to $I3_CONFIG"
echo "reload i3 with: i3-msg reload"
