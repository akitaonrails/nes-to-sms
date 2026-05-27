# Milestone Status (divergence-driven plan)

Adopted after a step-back re-evaluation: the architecture is sound
(strict-build NMI completes in ~64K Z80 instructions, ~10× real-time —
an optimization concern, not a wall). The previous lack of progress was
methodological: routines were fixed by guessing, with no whole-frame
oracle to say *where* the translation first diverges from real SMB.

## The methodology: `frame-diff`

`crates/cli/src/bin/frame_diff.rs` runs the original SMB PRG on
`oracle_6502` (reference) and the generated SMS ROM on `z80_emu`
(subject), frame by frame, under an identical simplified PPU/controller
model, and reports the first frame + addresses where NES game-state RAM
($0000-$07FF) diverges. Excludes the 6502 stack page and JumpEngine
dispatch scratch (implementation details, not game state).

Run: `cargo run --release -p nes_to_sms --bin frame-diff -- <smb.nes> out/smb/sms.sms --frames N --script {none|start}`
Useful: `FD_TRAJ=0xADDR` dumps one address's per-frame trajectory both sides.

## Milestones

- **M0 ✓** git baseline + re-evaluation.
- **M1 ✓** frame-diff harness reports first divergence.
- **M2 ✓** Title/init matches the reference for 22 frames. Operation-mode
  state machine ($0770/$0772/$073C) matches in lockstep. Init snapshot
  (RAM at NMI-enable) is byte-identical.
  - **Bug fixed:** `rt_ror_mem`/`rt_rol_mem` lost the shadow carry (a
    `pop af` wiped the carry that `rrca` had just extracted, before the
    `rr`/`rl`). Every ROR/ROL-memory rotated with carry 0. Visible as the
    LFSR RNG ($07A7) degenerating to a pure right-shift. Fixed; RNG now
    matches the reference exactly. This was a *general* correctness bug,
    not RNG-specific.
  - Residual frame-22 divergence is the VRAM render-buffer fill/flush
    *phasing* during DrawTitleScreen + attract-demo object positions —
    rendering detail, not game logic.
- **M3 ✓** Start → gameplay → **Mario walks and jumps in 1-1**.
  Verified on the SUBJECT via frame-diff (script `start_right` /
  `start_jump`):
  - Start press (frames 40-44) → GameMode (`$0770` 0→1).
  - "WORLD 1-1" intermediate screen displays (`ScreenTimer` `$07A0`
    counts down as an *interval* timer — see below), then
    `ScreenRoutineTask` `$073C` advances 07→08 (AreaParserTaskControl
    parses the level) →…→0C (steady gameplay).
  - **1-1 loads, Mario spawns at `$0086`=$28**, `GameEngineSubroutine`
    `$000E`→$08 (PlayerCtrlRoutine, live gameplay).
  - **Walk:** holding Right ramps `Player_X_Speed` `$0057` 00→$18 and
    `$0086` climbs $28→$55+ — Mario walks right.
  - **Jump:** holding A drives `Player_Y_Position` `$00CE` through a
    clean parabola (B0→6E apex→back down) with `$009F` Y-speed going
    negative (FC) then through 0 to positive — correct gravity.
  - **The earlier "Mario won't walk" was a test artifact, not a bug.**
    Input was being applied during the title/intermediate screen, which
    correctly ignores it (GameCoreRoutine isn't the active task yet).
    Once input is held *after* the intermediate screen expires
    (~frame 200), movement physics work.
- **Key insight — `ScreenTimer` is an interval timer.** `DecTimers`
  (`$8100`) decrements `SelectTimer,x` ($0780+) for x=0..$14 every frame,
  but the full range x=0..$23 (which includes `ScreenTimer` `$07A0`,
  index $20) only every 21 frames (gated by `IntervalTimerControl`
  `$077F`). So the "WORLD 1-1" screen correctly lingers ~150 frames
  before gameplay starts. Tests must wait this out before asserting on
  gameplay state. *(Minor: the subject's interval-decrement cadence
  looked like ~16-18 frames rather than exactly 21 — worth a later
  check, but not a playability blocker.)*
- **Controller mapping (frame-diff `sms_dc_to_nes`) is now mode-aware**,
  mirroring `rt_controller_latch`: in title mode (`$0770`==0) SMS
  Button1→NES Select and Button2→NES Start (Start *alone*, so SMB's
  `GameMenuRoutine` `cmp #$10` matches); in gameplay Button1→A,
  Button2→B. The previous fixed B+Start alias made the reference reject
  Start and never enter GameMode.
- **Reference-as-oracle past the title is still blocked** by the
  frame-22 demo/VRAM-buffer divergence (below): with a late Start the
  reference's DemoTimer has expired so it resets the title instead of
  starting. The subject's gameplay is validated directly; using the
  reference to diff *gameplay* needs the frame-22 divergence fixed first.
- **Frame-22 divergence (title-render phasing, demo only):** both VRAM
  update buffers (`$0300-$03FF`, `$0400-$04FF`) diverge — the subject has
  them *full* during DrawTitleScreen while the reference has already
  *flushed* them (subject is one flush-frame behind). This affects the
  attract **demo** and the reference's title timing; it does **not**
  affect Start→gameplay, which is validated working.
- **M4 ◑** Playable 1-1 — validated in the in-repo emulator (walk +
  jump). Final step: confirm visually in mednafen.

## Next concrete steps

1. **Visual confirmation in mednafen.** Load `out/smb/sms.sms`, press
   Start, wait out the "WORLD 1-1" screen (~2.5 s), then hold Right /
   Button1 — Mario should walk and jump.
2. **Resolve the frame-22 VRAM-buffer fill/flush phasing** (title/demo
   only). Confirm whether WriteBufferToScreen flushes large buffers the
   same frame the reference does; the subject lagging one flush-frame
   suggests a buffer-size or per-frame-flush-cap difference. Fixing this
   unblocks reference-as-oracle for gameplay and corrects the attract
   demo.
3. **Check the interval-timer cadence** (~16-18 vs 21 frames) so the
   intermediate-screen duration matches real SMB.

## Verified-working summary (the good news)

Boot → title (22 frames byte-identical to reference) → Start →
GameMode → "WORLD 1-1" intermediate screen → 1-1 area parse → Mario
spawns at the correct start position → **walks right (X-speed ramps to
$18) and jumps (parabolic Y arc with correct gravity) under controller
input.** The pipeline genuinely translates SMB into a playable 1-1.
Remaining: visual confirmation in mednafen and the title-demo
render-buffer phasing (cosmetic to gameplay).

## Invariant to preserve

Fail closed on unresolved labels by default (`jp rt_unresolved_jsr`).
`--debug-unresolved-stubs` is the opt-in visual hack and must not be the
default. Audio-routine no-ops should be a *targeted* allowance, not a
blanket suppression that hides real gameplay bugs.
