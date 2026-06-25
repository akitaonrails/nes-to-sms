#!/usr/bin/env bash
# Container-side launcher: pin the Genesis Plus GX Z80 overclock and start
# RetroArch on the translated ROM at correct (60 fps) speed.
#
# Overclock: RetroArch validates genesis_plus_gx_overclock against the core's
# value list and reads it from the PER-CORE .opt (not retroarch-core-options.cfg);
# 500 is the highest accepted value (higher silently resets to 100). Override
# with the OVERCLOCK env var (e.g. OVERCLOCK=300).
#
# Frame pacing: a headless container usually has no working vsync (software GL)
# and no audio device, so neither can throttle the core -> it runs as fast as
# the host allows ("way too fast"). vrr_runloop_enable makes RetroArch pace to
# the core's exact framerate with its own monotonic timer, independent of
# vsync/audio, so the game runs at true 60 fps regardless of the overclock.
set -euo pipefail

ROM="${1:?usage: gpgx_entry.sh <rom.sms>}"
OVERCLOCK="${OVERCLOCK:-500}"

export HOME=/tmp/rahome
OPTDIR="$HOME/.config/retroarch/config/Genesis Plus GX"
mkdir -p "$OPTDIR"
printf 'genesis_plus_gx_overclock = "%s"\n' "$OVERCLOCK" > "$OPTDIR/Genesis Plus GX.opt"

OVR="$HOME/override.cfg"
cat > "$OVR" <<'CFG'
vrr_runloop_enable = "true"
video_vsync = "false"
audio_sync = "false"
fastforward_ratio = "1.000000"
CFG

echo "Launching RetroArch (Genesis Plus GX, Z80 overclock ${OVERCLOCK}%, paced to 60 fps) on $ROM"
exec retroarch -L /opt/cores/genesis_plus_gx_libretro.so --appendconfig "$OVR" "$ROM"
