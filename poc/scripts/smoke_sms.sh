#!/usr/bin/env bash
set -euo pipefail

rom="${1:-out/smb/poc.sms}"

if [[ ! -f "$rom" ]]; then
  cargo run -- "/roms/nes/Super Mario Bros. (World).nes" out/smb
fi

mkdir -p out/smb/emulator

log="out/smb/emulator/mednafen-sms-smoke.log"
home_dir="out/smb/emulator/mednafen-home-$(id -u)"

mkdir -p "$home_dir"

set +e
timeout 5 env \
  HOME="$PWD/$home_dir" \
  SDL_VIDEODRIVER=dummy \
  SDL_AUDIODRIVER=dummy \
  /usr/games/mednafen \
    -force_module sms \
    -video.driver softfb \
    -sound 0 \
    "$rom" \
    >"$log" 2>&1
status=$?
set -e

if [[ "$status" != "0" && "$status" != "124" ]]; then
  echo "Mednafen SMS smoke failed with exit code $status. Log: $log" >&2
  exit "$status"
fi

if ! grep -q "Using module: sms" "$log"; then
  echo "Mednafen did not load the ROM with the SMS module. Log: $log" >&2
  exit 1
fi

if ! grep -q "ROM:       256KiB" "$log"; then
  echo "Mednafen did not recognize the generated ROM as 256KiB. Log: $log" >&2
  exit 1
fi

if ! grep -q "Mapper:    Sega" "$log"; then
  echo "Mednafen did not recognize the generated ROM as Sega-mapper. Log: $log" >&2
  exit 1
fi

if ! grep -q "Territory: Export" "$log"; then
  echo "Mednafen did not recognize the generated ROM as export territory. Log: $log" >&2
  exit 1
fi

echo "Mednafen SMS smoke completed. Log: $log"
