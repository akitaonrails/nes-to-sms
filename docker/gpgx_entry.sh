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
input_joypad_driver = "udev"
input_autodetect_enable = "true"
audio_driver = "pulse"
audio_enable = "true"
CFG

# Xvfb/CI captures need the SDL2 video backend; the default GL path can create
# a window while leaving its drawable black. Normal interactive launches keep
# RetroArch's default unless the caller opts in.
if [ -n "${GPGX_VIDEO_DRIVER:-}" ]; then
  printf 'video_driver = "%s"\n' "$GPGX_VIDEO_DRIVER" >> "$OVR"
fi

# Gamepad autoconfig: the container ships no joypad profiles, so RetroArch
# detects pads but reports "not configured". Provide one for the 8BitDo
# Ultimate 2 Wireless (XInput layout: south/east/west/north = 0/1/2/3,
# select/start = 6/7, D-pad = hat 0). GPGX SMS maps RetroPad B -> button 1
# (jump) and A -> button 2 (run); Start on the title = button 2.
ACDIR="$HOME/.config/retroarch/autoconfig/udev"
mkdir -p "$ACDIR"
cat > "$ACDIR/8BitDo Ultimate 2 Wireless Controller.cfg" <<'PAD'
input_driver = "udev"
input_device = "8BitDo Ultimate 2 Wireless Controller"
input_vendor_id = "11720"
input_product_id = "12555"
input_b_btn = "0"
input_a_btn = "1"
input_y_btn = "2"
input_x_btn = "3"
input_select_btn = "6"
input_start_btn = "7"
input_up_btn = "h0up"
input_down_btn = "h0down"
input_left_btn = "h0left"
input_right_btn = "h0right"
input_l_btn = "4"
input_r_btn = "5"
input_l2_axis = "+2"
input_r2_axis = "+5"
input_l_x_plus_axis = "+0"
input_l_x_minus_axis = "-0"
input_l_y_plus_axis = "+1"
input_l_y_minus_axis = "-1"
PAD

echo "Launching RetroArch (Genesis Plus GX, Z80 overclock ${OVERCLOCK}%, paced to 60 fps) on $ROM"
exec retroarch -L /opt/cores/genesis_plus_gx_libretro.so --appendconfig "$OVR" "$ROM"
