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
- **M3 ◑ (in progress)** Start → gameplay.
  - The subject correctly transitions title→GameMode on Start
    (`$0770` 0→1, `$0772` resets, `$074A`=$10=Start).
  - **Blocker:** GameMode init then stalls — the player never spawns
    ($0086/$000E freeze) and the gameplay path hits the unresolved-jsr
    trap. The 47 unresolved labels are dominated by the **sound engine**
    (`L_F0D7..L_F64D` = NES `$F000+`) plus specific enemy init/move
    routines. Level-start music is the likely trap; sprite-0-hit
    emulation and the area parser are also on this path.
  - **Reference caveat:** the reference NES controller model's
    ReadJoypads timing doesn't coincide with the Start window, so the
    *reference* stays at the title. Needs fixing for proper gameplay
    differential validation (the subject's controller path works).
- **M4 ☐** Playable 1-1 in mednafen.

## Next concrete steps

1. Resolve the GameMode trap. Audio is deferred (master plan), so the
   cleanest unblock is to make the sound-engine entry a faithful no-op
   (or lift the sound routines so APU writes are stubbed) rather than
   trap. Confirm the exact trapping routine id via the `$CB1B` marker.
2. Fix the reference controller (ReadJoypads timing / button delivery)
   so frame-diff can validate the gameplay path the same way it
   validated the title.
3. With both, drive `--script start_right` and converge player physics
   (x/y position, scroll) against the reference, then validate visually
   in mednafen.

## Invariant to preserve

Fail closed on unresolved labels by default (`jp rt_unresolved_jsr`).
`--debug-unresolved-stubs` is the opt-in visual hack and must not be the
default. Audio-routine no-ops should be a *targeted* allowance, not a
blanket suppression that hides real gameplay bugs.
