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
- **M3 ◑ (in progress)** Start → gameplay. Verified working on the
  SUBJECT directly (trace-sms):
  - Start press → GameMode (`$0770` 0→1, `$0772` resets).
  - **1-1 loads and Mario spawns at the correct start position**
    (`$0086`=$28 page 0, `$00CE`/Y=$B0). No unresolved-jsr trap on this
    path (earlier "trap" reading was a misread; the milestone is *not*
    reached, i.e. no trap).
  - Controller input is delivered: the game sees Right
    (`$06FC`=$01 = Right in SMB's MSB-first SavedJoypad order).
  - **Headline blocker: Mario won't walk.** With Right held, X stays
    `$28` and X-speed stays 0 — input reaches the game but the
    horizontal-movement physics doesn't apply. Not a trap → a
    translation bug in the player-movement path. Needs differential
    isolation.
  - **Reference controller: fixed.** Two issues resolved: (a) the
    reference must latch "NMI enabled" like the runtime's `$CB1A` latch,
    or it stops running ReadJoypads when SMB briefly disables NMI;
    (b) inputs must be round-tripped through the SMS $DC mapping
    (`effective_nes_buttons`) so both sides see the same aliased NES
    buttons (Start→B+Start). The reference now reads the controller
    every frame and decodes Start correctly.
  - **Why the reference still won't enter GameMode on Start:** the
    title **demo** diverges at frame 22 (see below), so by the Start
    frame (40) the two title states differ. Pressing Start *before*
    frame 22, or fixing the frame-22 divergence, is required for a
    valid gameplay diff.
- **Frame-22 divergence (root of the demo + physics issues):** both
  VRAM update buffers (`$0300-$03FF` VRAM_Buffer1, `$0400-$04FF`
  VRAM_Buffer2) diverge — the subject has them *full* during
  DrawTitleScreen while the reference has already *flushed* them. The
  subject is one frame behind in the fill/flush cycle. This phasing
  cascades: the subject spawns a demo player ($0086=$28 at frame 26)
  that the reference doesn't. The demo auto-plays Mario, so this is the
  **same physics bug** surfacing in attract mode.
- **M4 ☐** Playable 1-1 in mednafen.

## Next concrete steps (for a focused follow-up session)

1. **Isolate the player-movement physics bug.** It's the headline
   blocker and likely the root of the frame-22 demo divergence too.
   Approach: add a `start_early` script that presses Start within the
   matched window (frames 15-19, before the frame-22 demo divergence)
   so both sides enter GameMode from identical state, then diff the
   player-movement vars ($0086 X, $0057/$0700 X-speed, $074A/direction
   bits) frame by frame. The first diverging var names the broken
   routine. Suspect: the joypad→Left_Right_Buttons split or the
   horizontal-accel routine.
2. **Resolve the frame-22 VRAM-buffer fill/flush phasing.** Confirm
   whether WriteBufferToScreen flushes large buffers the same frame the
   reference does; the subject lagging one frame suggests a buffer-size
   or per-frame-flush-cap difference in the runtime VRAM-buffer code.
3. Once both are fixed and frame-diff matches through gameplay,
   validate visually in mednafen (`--buttons-at-frame 30:start
   45:right`).

## Verified-working summary (the good news)

Boot → title (22 frames byte-identical to reference) → Start →
GameMode → 1-1 area load → Mario spawns at the correct start position,
and the game reads controller input. The pipeline genuinely translates
SMB through gameplay entry. The remaining gap is the horizontal-movement
physics (Mario won't walk) and the title-demo render-buffer phasing.

## Invariant to preserve

Fail closed on unresolved labels by default (`jp rt_unresolved_jsr`).
`--debug-unresolved-stubs` is the opt-in visual hack and must not be the
default. Audio-routine no-ops should be a *targeted* allowance, not a
blanket suppression that hides real gameplay bugs.
