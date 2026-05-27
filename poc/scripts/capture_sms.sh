#!/usr/bin/env bash
# Run a generated .sms ROM in mednafen under xvfb and capture screenshots
# at fixed frame intervals using ImageMagick's import command.
#
# Usage:  capture_sms.sh <rom.sms> <out_dir> [seconds]
#
# Produces:
#   <out_dir>/frame_0000.png
#   <out_dir>/frame_0001.png
#   ...
# one frame per second (rounded).
set -euo pipefail

ROM="${1:?usage: capture_sms.sh <rom.sms> <out_dir> [seconds]}"
OUT="${2:?usage: capture_sms.sh <rom.sms> <out_dir> [seconds]}"
SECS="${3:-3}"

mkdir -p "$OUT"
export DISPLAY=:99
export PATH="/usr/games:$PATH"

# Start xvfb in background. Use a display that fits mednafen's
# 1024x960 default window (SMS native 256x192 × 3 + chrome).
Xvfb :99 -screen 0 1024x1024x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
XVFB_PID=$!
sleep 0.5

# Start mednafen.
export HOME=/tmp/mednafen-home
mkdir -p "$HOME/.mednafen"
mednafen -sound 0 -force_module sms "$ROM" >/tmp/mednafen.log 2>&1 &
MED_PID=$!
sleep 2 # let mednafen create its window

# Find the mednafen window. It uses class "mednafen" and name "sms" /
# game-specific title. xdotool's class search is the most reliable.
WINID=$(xdotool search --class mednafen 2>/dev/null | head -1 || echo "")
echo "mednafen window id: ${WINID:-<not found>}"

# Move/resize the window so we can capture its full content. Mednafen
# positions itself partly off-screen by default under xvfb (y=-96).
if [ -n "$WINID" ]; then
    xdotool windowmove "$WINID" 0 0 2>/dev/null || true
    sleep 0.5
fi

# Capture one screenshot per second.
for i in $(seq 0 "$((SECS-1))"); do
    sleep 1
    FRAME=$(printf "%04d" "$i")
    if [ -n "$WINID" ]; then
        # Capture the mednafen window content explicitly.
        import -window "$WINID" "$OUT/frame_$FRAME.png" 2>/dev/null || \
            import -window root "$OUT/frame_$FRAME.png" 2>/dev/null || true
    else
        import -window root "$OUT/frame_$FRAME.png" 2>/dev/null || true
    fi
done

# Cleanup.
kill $MED_PID 2>/dev/null || true
kill $XVFB_PID 2>/dev/null || true
wait 2>/dev/null || true

echo "Captured $(ls "$OUT"/frame_*.png 2>/dev/null | wc -l) frames in $OUT"
