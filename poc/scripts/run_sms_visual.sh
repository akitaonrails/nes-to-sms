#!/usr/bin/env bash
set -euo pipefail

rom="${1:-out/smb/poc.sms}"

if [[ ! -f "$rom" ]]; then
  cargo run -- "/roms/nes/Super Mario Bros. (World).nes" out/smb
fi

mkdir -p out/smb/emulator

home_dir="out/smb/emulator/mednafen-visual-home-$(id -u)-$$"
mkdir -p "$home_dir"

HOME="$PWD/$home_dir" \
  /usr/games/mednafen \
    -force_module sms \
    -video.driver softfb \
    -sound 0 \
    "$rom"
