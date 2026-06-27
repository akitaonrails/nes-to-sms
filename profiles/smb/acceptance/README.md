# SMB Acceptance Inputs

This directory contains profile-owned input scripts for `trace-sms` acceptance
runs. They are regression data for the SMB stress target, not converter logic.

## Format

### Button scripts

Each non-comment line is:

```text
FRAME:buttons
```

`trace-sms` holds that button set from `FRAME` until the next event. Blank
lines and `#` comments are ignored. Button names match `--buttons-at-frame`,
for example `right`, `a`, `start`, or comma-separated combinations such as
`right,a`.

### Checkpoint scripts

Each non-comment line is:

```text
FRAME:name
```

`trace-sms --checkpoint-script` writes a named `.ppm` framebuffer and `.txt`
state summary when the route reaches that frame. Checkpoints are route-visible
acceptance artifacts; they make rendering/runtime divergence inspectable instead
of relying only on final RAM state.

## 1-1 clear smoke test

After generating and assembling `out/smb/sms.sms`, run:

```sh
target/release/trace-sms out/smb/sms.sms --steps 301000000 \
  --buttons-script profiles/smb/acceptance/1-1-clear.buttons \
  --checkpoint-script profiles/smb/acceptance/1-1-clear.checkpoints \
  --checkpoint-dir out/smb/checkpoints/1-1-clear \
  --expect-no-trap \
  --expect-ram 0x0760=01 \
  --expect-ram 0x075C=01 \
  --expect-ram 0x000E=07
```

The expectations assert that the route did not hit the unresolved-runtime trap
and reached the expected post-1-1 transition state (`area=1`, `level=1`, player
state `$0E=07`).

The checkpoint directory should contain inspectable route artifacts such as
`00060_title-before-start.ppm` / `.txt` through `04900_post-transition.ppm` /
`.txt`.

## Shared route semantic diff

Use the same `.buttons` route with `frame-diff` to compare the NES reference RAM
trajectory against the generated SMS ROM:

```sh
FD_EXCLUDE_VRAMBUF=1 FD_EXCLUDE_AUDIO=1 \
  target/release/frame-diff \
  "/path/to/Super Mario Bros. (World).nes" out/smb/sms.sms \
  --frames 300 \
  --buttons-script profiles/smb/acceptance/1-1-clear.buttons
```

`FD_EXCLUDE_VRAMBUF=1` separates render-buffer phasing from gameplay-state
drift. `FD_EXCLUDE_AUDIO=1` is intentional while `SoundEngine` is stubbed and
the `$F3xx-$F7xx` audio phase is deferred.

## External emulator smoke

After assembling `out/smb/sms.sms`, verify that Mednafen recognizes the ROM as
an SMS/Sega-mapper/export build:

```sh
docker compose run --rm --workdir /work/poc --user root sms-smoke \
  bash scripts/smoke_sms.sh ../out/smb/sms.sms
```

The smoke test intentionally accepts any generated ROM size that is at least
16KiB and a 16KiB multiple; current workspace builds are larger than the old
legacy PoC's fixed 256KiB image.
