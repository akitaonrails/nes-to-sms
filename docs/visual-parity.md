# Visual parity: NES ground-truth frame comparison

RAM-parity (frame-diff) proves the *game logic* matches the NES. It says
nothing about presentation: palette translation, folded-BG variant selection,
scroll-split rendering, sprite mapping. This workflow renders ground-truth NES
frames from the reference core and compares them cell-by-cell against the SMS
build's checkpoint framebuffers. It found four presentation bugs on SMB that
three RAM-parity routes and the VDP hash oracle all missed — and disproved
three "bugs" that were actually authentic NES behavior.

## The oracle

`frame-diff`'s NES reference bus already captures everything the PPU needs:
nametable writes land in the `chr_ram` shadow ($2000-$2FFF), OAM is the $0200
RAM page, palette writes are captured from $2007 (with the $3F10/$14/$18/$1C
hardware mirrors folded down — SMB parks the sky color at $3F10), and the last
$2005 first-write is the frame's playfield X scroll. `render_nes_ppm` renders
BG + sprites from that state with SMB's presentation model (rows above y=32
unscrolled, the rest at the playfield scroll), honoring PPUMASK blanking.

```sh
FD_NES_DUMP="out/nesref:60,220,720,1600,2705,3940,4450" \
  target/release/frame-diff .roms/smb.nes out/smb/sms.sms \
  --frames 4500 --buttons-script profiles/smb/acceptance/1-1-clear.buttons
```

writes `ref_NNNNN.ppm` ground-truth frames at the listed frames. Because both
sides run the same buttons script on the same frame counter, the frames align
exactly with `trace-sms --checkpoint-script` dumps — no emulator boot-offset
guessing.

Compare cell-by-cell (quantizes the NES colors into the SMS 2-bit space; cells
that differ by more than quantization get flagged):

```sh
python3 scripts/nesref/compare_frames.py out/nesref/ref_03940.ppm \
  out/smb/checkpoints/1-1-clear/03940_flagpole.ppm
```

Mean distance ~25-28 = one quantization step (master-palette rounding, not a
bug); >40 = content-level difference worth root-causing.

## What it found on SMB (2026-09-04)

Fixed:

1. **Stale variant-ring cells** (black square in the flagpole cloud): the BG
   variant ring recycled slots still referenced by visible cells once the
   camera locked while allocations continued. Fixed generically with per-slot
   NT refcounts (`runtime/chrmap.s` BGV_REFCNT): counts are rebuilt from the
   live nametable when the ring first wraps and maintained incrementally by
   `_bgv_nt_write` (the single NT-entry writer, which reads the old slot back
   from VRAM). The allocator skips referenced slots; if the whole ring is
   referenced it degrades to the old steal. Regression guard:
   `SMS_EXPECT_BGV_CONSISTENT=0` on the 1-1-clear route.
2. **Tracer rendered the status bar scrolled**: the runtime's split-scroll
   (pre-scroll + line IRQ) was fine on GPGX; trace-sms cleared its split latch
   at frame-IRQ injection before the checkpoint dump. Fixed by latching the
   ended frame's split for the renderer.
3. **Tracer suppressed the split at random**: the status-port fake (bit 7
   toggling every 100 reads) made the runtime's end-of-handler pacing read see
   phantom overruns and set the sticky 60-frame split-suppression counter.
   Fixed by modeling frame-INT-pending for real (`frame_int_pending`).
4. **Tracer ignored display blanking**: the runtime maps NES PPUMASK to VDP
   reg 1 bit 6 correctly; the tracer rendered blanked frames anyway. The NES
   blanks the 1-1→1-2 transition (mask=$06 at frame 4450); the tracer now
   renders blanked frames as backdrop, matching hardware.

Confirmed authentic (not bugs):

- **Pink staircase / flagpole base**: NES color $36 really is pinkish tan;
  our $2B is the correct SMS quantization. (CRT-era memory says "orange".)
- **Status-bar coin flipping dark/orange**: the ?-block/coin palette flash
  animation ($27→$17 dim phase) — our $0B→$01 tracks it.
- **Green fragment right of the castle**: an entering hill, present on the
  NES reference too.

Known limitations (documented, not fixed):

- **Per-sprite behind-background priority**: SMS has only a per-tile BG
  priority bit. Mario walking "into" the castle door renders in front on the
  SMS build; the NES hides him behind the opaque door pixels. A generic fix
  would need heuristics for setting NT priority bits on cells that NES
  behind-sprites overlap.
- **Sprite tiles $A7-$FF unmapped** (SMB CHR packing budget): floating score
  popups ("400" at the flag) resolve to the transparent fallback tile and are
  invisible. Benign degrade; a fix needs slot-budget headroom or usage-driven
  sprite packing.
- **Master-palette rounding**: the converter's NES→SMS color LUT rounds a few
  boundary colors differently than the Nestopia reference palette (e.g. $17
  → $01 vs ideal $05). Cosmetic; a LUT audit against a chosen master palette
  would tighten it.

## VDP hash goldens are timing-sensitive

`FD_VDP_CHECK` per-frame hashes snap at fixed instruction boundaries while the
translated NMI handler routinely spans them, so **any change to handler cycle
cost invalidates the goldens even when the write trajectory is identical**
(verified: byte-identical VRAM at quiescent frames, hashes diverging from
frame 13 after a +322-cycle change). Only compare hashes between builds with
identical timing; otherwise re-validate via RAM parity + this visual workflow,
then regenerate goldens from the newly trusted build.

## FCEUX container (secondary)

`docker/Dockerfile.nesref` + the `nesref` compose service exist for running a
real emulator against the route (`scripts/nesref/gen_capture_lua.py` generates
a state-keyed Lua capture script). Ubuntu jammy's fceux 2.5.0 currently
crashes under xvfb with the dummy SDL audio driver ("buffer overflow
detected"), so the in-repo FD_NES_DUMP oracle is the primary path — it is
also frame-exact, which an external emulator is not without sync work.
