#!/usr/bin/env bash
# Run the translated SMS ROM in RetroArch + Genesis Plus GX with the Z80
# overclocked 5x, all inside the nes-to-sms-retroarch Docker image (no host
# pollution). GUI is forwarded over X11 to the local display.
#
# Usage:  docker/run_gpgx.sh [out/smb/sms.sms]
#         OVERCLOCK=300 docker/run_gpgx.sh      # dial the Z80 overclock (% )
# Controls (RetroArch default): arrows = D-pad, Z/X = buttons, Enter = Start.
# Speed is paced to 60 fps by the core's timer, so the overclock only adds CPU
# headroom (keeps the heavy NMI from dropping frames); it does not change game
# speed. If it ever runs fast/slow, that's the frame pacing, not the overclock.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
ROM="${1:-out/smb/sms.sms}"

# Allow the container's X client to reach the host display (reverted easily
# with `xhost -local:`). Harmless local-only grant.
xhost +local: >/dev/null 2>&1 || true

exec docker run --rm -it \
  -e DISPLAY="${DISPLAY:-:0}" \
  -e OVERCLOCK="${OVERCLOCK:-500}" \
  -v /tmp/.X11-unix:/tmp/.X11-unix \
  -v "$REPO":/work \
  --device /dev/input \
  -v /run/udev/data:/run/udev/data:ro \
  --workdir /work \
  nes-to-sms-retroarch:latest \
  bash /work/docker/gpgx_entry.sh "$ROM"
