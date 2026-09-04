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
SMS_EXPECT_BGV_CONSISTENT=0 \
target/release/trace-sms out/smb/sms.sms --steps 301000000 \
  --buttons-script profiles/smb/acceptance/1-1-clear.buttons \
  --checkpoint-script profiles/smb/acceptance/1-1-clear.checkpoints \
  --checkpoint-dir out/smb/checkpoints/1-1-clear \
  --expect-no-trap \
  --expect-ram 0x075C=01 \
  --expect-ram 0x0760=02 \
  --expect-ram 0x075A=02
```

The expectations assert that the route did not hit the unresolved-runtime
trap and reached the expected post-1-1 state: LevelNumber `$075C=01`
(second level), AreaNumber `$0760=02`, and both lives intact
(`$075A=02` — the route clears 1-1 and idles at the 1-2 start without
dying). The script was re-recorded 2026-07-04 against the frame-diff NES
reference after the translation reached byte-for-byte parity; the old
script encoded pre-parity buggy behavior and dies on a real NES.

`SMS_EXPECT_BGV_CONSISTENT=0` is the stale-variant regression guard: at every
checkpoint it verifies each painted nametable cell still references the tile
slot its folded-BG bookkeeping resolves to. The pre-refcount allocator failed
this with 2 stale cells at the flagpole checkpoint (variant ring recycled
slots under live cells while the camera was scroll-locked), so the check makes
that bug class `git bisect`-able. It applies to folded-BG (CHR-ROM) profiles
only — identity-mode CHR-RAM profiles (CV1) use different bookkeeping.

The checkpoint directory should contain inspectable route artifacts such as
`00060_title-before-start.ppm` / `.txt` through
`04450_post-transition-1-2.ppm` / `.txt`. The final checkpoint is inside the
301-million-step budget under the delivered-game-frame clock; the former frame
4700 target was unreachable even for the accepted pre-mapper ROM despite its
post-transition RAM state already being green.

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
