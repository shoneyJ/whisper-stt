#!/usr/bin/env bash
# Append the i3 float-rule for whisper-stt's GTK4 indicator window (idempotent).
#
# We match by window title (the indicator code calls `.title("whisper-stt")`)
# rather than by WM_CLASS — title is set explicitly and stable across GTK4
# versions, while WM_CLASS varies with the gtk4-rs / glib mapping of
# `application_id` to instance/class fields.
set -euo pipefail

I3_CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}/i3/config"
MARKER_OPEN="# >>> whisper-stt indicator rule >>>"
MARKER_CLOSE="# <<< whisper-stt indicator rule <<<"

if [ ! -f "$I3_CONFIG" ]; then
    echo "i3 config not found at $I3_CONFIG" >&2
    exit 1
fi

if grep -qF "$MARKER_OPEN" "$I3_CONFIG"; then
    echo "whisper-stt indicator rule already present in $I3_CONFIG"
    exit 0
fi

cat >> "$I3_CONFIG" <<EOF

$MARKER_OPEN
for_window [title="^whisper-stt$"] floating enable, sticky enable, border none
$MARKER_CLOSE
EOF

echo "appended whisper-stt indicator rule to $I3_CONFIG"
echo "reload i3 with: i3-msg reload"
