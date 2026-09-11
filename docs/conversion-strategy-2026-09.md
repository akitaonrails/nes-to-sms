# Conversion strategy — how to actually complete the plan (2026-09-11)

This revises the implementation approach after the first new-game conversion
(Balloon Fight) exposed why the current path does not scale to many games.

## What the Balloon Fight conversion proved

Balloon Fight now builds, boots, runs, enables rendering, and shows a
recognizable title — via new NROM-128 pipeline support and a minimal profile.
But its SMS translation **diverges from the reference at frame 0** (≈150 bytes of
game state), and the rendering artifacts (wrong background tiles) are downstream
of that divergence, not a rendering bug.

Two structural findings:

1. **The differential oracle can't validate new games.** `frame-diff`'s reference
   is `oracle_6502` — a *simplified*, frame-granular 6502+PPU model (fixed
   instructions/frame, synthetic sprite-0, approximate timing). SMB is byte-exact
   against it only because **both SMB's runtime and the oracle were co-tuned**.
   For a new game the two disagree on how far per-frame execution gets — on
   Balloon Fight the oracle does not even write the addresses that "diverge",
   i.e. the two run different amounts of code per frame. So byte-exact debugging
   against this oracle chases oracle artifacts, not real translation bugs.

2. **Correctness is the per-game bottleneck, and it is currently un-toolable.**
   SMB reached byte-exactness through a long manual campaign; without a
   *trustworthy* reference, that campaign can't even be run for a new game.

The whole project's playable output is ≈2 games because each one is a from-scratch
campaign of this kind. Repeating that per game does not finish a large plan.

## The strategic change: make per-game cost fall, not repeat

The goal is not "grind each game byte-exact by hand" — it is to **drive the
marginal cost per game toward mechanical**, by investing every fix into shared,
game-agnostic infrastructure. Priority order:

### P0 — Trustworthy ground truth (unblocks everything else)
Adopt a **real, cycle-accurate NES core as the differential reference** (e.g. a
Rust NES core, or a headless libretro NES core driven like the existing
`capture_core.py` SMS harness), replacing/supplementing `oracle_6502` for
new-game validation. Only against real ground truth is a divergence a *real bug*.
Everything below depends on this.

### P1 — Harden the recompiler generically
With a real reference, fix the **game-agnostic** translation bugs new games
surface (flag edge cases, addressing modes, helper clobbers) so the core
recompiler emits correct Z80 out-of-the-box. Each fix helps every future game —
this is the compounding lever. Regression-test each against the growing game set.

### P2 — Stronger auto-discovery (less per-game annotation)
The manual profile work for Balloon Fight was: indirect-dispatch handler roots,
inline-data regions, jump tables. Automate these:
- **JAM/illegal opcode → data boundary**: when the code walker hits an illegal
  opcode, treat it as the end of code / start of inline data instead of emitting
  a failing stub. (Directly removes the "lower failures" class.)
- **Indirect-dispatch discovery**: find the pointer table feeding a `JMP ($zp)`
  and root its targets automatically.
These shrink a new game's profile to vectors + `[[chr_pack]]`.

### P3 — Generalize the runtime beyond SMB
The PPU→VDP and CHR-variant paths are tuned for SMB/CV1. New games need the
folded-BG/variant materializer and CHR-pack conventions to be data-driven, not
SMB-shaped, so a correct translation also *renders* correctly.

### P4 — Sequence and scope realistically
- Order the roadmap by **tractability**, not the current arbitrary order: simple
  NROM games first (they harden P1-P3 cheaply), then one mapper family at a time.
- New mappers must be added to the **conversion** pipeline (not just the oracle)
  before their games can convert.
- Per game, the milestone is "byte-exact vs the real reference, then renders,
  then plays"; performance stays deferred (overclock is acceptable, per the
  existing plan).

## Honest expectation

This is a **months-long, game-by-game program**, but the strategic change makes
it *converge*: with a real reference plus P1-P3 infrastructure, each new game
costs less than the last, instead of each being another SMB-sized campaign. The
first executable step is P2's JAM→data-boundary auto-discovery (bounded,
game-agnostic, removes a whole class of per-game annotation), landed while P0's
real-reference integration is scoped.
