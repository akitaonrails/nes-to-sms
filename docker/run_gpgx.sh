#!/usr/bin/env bash
# Run the translated SMS ROM in RetroArch + Genesis Plus GX with the Z80
# overclocked 5x, all inside the nes-to-sms-retroarch Docker image (no host
# pollution). GUI is forwarded over X11 to the local display.
#
# Usage:  docker/run_gpgx.sh [out/smb/sms.sms]
#         OVERCLOCK=300 docker/run_gpgx.sh      # dial the Z80 overclock (% )
# Controls (RetroArch default): arrows = D-pad, Z/X = buttons,
# Enter = SMS Pause/NES Start.
# Speed is paced to 60 fps by the core's timer, so the overclock only adds CPU
# headroom (keeps the heavy NMI from dropping frames); it does not change game
# speed. If it ever runs fast/slow, that's the frame pacing, not the overclock.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
ROM="${1:-out/smb/sms.sms}"

# The receiver remains visible as hidraw even when the wireless controller is
# powered off, while RetroArch needs the controller's event/joystick device.
# Make that easy-to-miss state explicit before starting the container.
if grep -Fq 'Name="8BitDo Ultimate 2 Wireless Controller"' /proc/bus/input/devices 2>/dev/null; then
  echo "Gamepad detected: 8BitDo Ultimate 2 Wireless Controller"
else
  echo "WARNING: 8BitDo gamepad event device not detected." >&2
  echo "Power/connect the controller, then confirm it appears in /proc/bus/input/devices." >&2
  echo "Keyboard fallback: arrows = D-pad, Z/X = buttons, Enter = SMS Pause/Start." >&2
  if [ "${GPGX_REQUIRE_GAMEPAD:-0}" = "1" ]; then
    exit 2
  fi
fi

# Allow the container's X client to reach the host display (reverted easily
# with `xhost -local:`). Harmless local-only grant.
xhost +local: >/dev/null 2>&1 || true

# -i/-t only when stdin is a real terminal (allows `! docker/run_gpgx.sh`
# from non-TTY shells; RetroArch does not need stdin).
TTY_FLAGS=""
[ -t 0 ] && TTY_FLAGS="-it"
# Host audio: forward the PulseAudio/PipeWire socket so the PSG is audible.
PULSE_DIR="/run/user/$(id -u)/pulse"
AUDIO_ARGS=()
if [ -S "$PULSE_DIR/native" ]; then
  AUDIO_ARGS=(-v "$PULSE_DIR":/run/pulse -e PULSE_SERVER=unix:/run/pulse/native)
fi

exec docker run --rm $TTY_FLAGS \
  -e DISPLAY="${DISPLAY:-:0}" \
  -e OVERCLOCK="${OVERCLOCK:-500}" \
  -v /tmp/.X11-unix:/tmp/.X11-unix \
  -v "$REPO":/work \
  --device /dev/input \
  -v /run/udev/data:/run/udev/data:ro \
  "${AUDIO_ARGS[@]}" \
  --workdir /work \
  nes-to-sms-retroarch:latest \
  bash /work/docker/gpgx_entry.sh "$ROM"
