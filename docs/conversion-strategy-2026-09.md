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

## Validation result (2026-09-11) — the recompiler was never the blocker

P0's first step (the `nes_ref`/tetanes reference) already overturned the pessimistic
read above. Comparing the 2 KiB internal RAM of the **real NES (tetanes)** against
the **SMS subject**, scanning frame phase to align:

| game | non-stack bytes differing (best phase) | match |
|------|----------------------------------------|-------|
| SMB (known byte-exact vs oracle) | 11 / 1792 | 99.4% |
| Balloon Fight (oracle said "150 diffuse bytes diverge @ frame 0") | 7 / 1792 | 99.6% |

Both games show the *same* ~1% residual against the real NES, and that residual is
entirely timing-phase state — animation/RNG counters ($12, $19), a position pair
both offset by exactly 0x0C ($D1/$D2), ±1 counters — because the two free-run
without synchronized input. That ~99% + timing-phase residual is the **signature of
a correct translation** (SMB, which is byte-exact against the oracle, sets the
baseline). Balloon Fight matches that signature.

**Conclusion:** Balloon Fight's SMS CPU translation is essentially correct. The
"150-byte frame-0 divergence" that made it look broken was a **simplified-oracle
artifact**, not a real bug — the oracle simply can't track a game it wasn't
co-tuned with. The recompiler already emits correct-grade Z80 for a new NROM game
from a *minimal* profile. The remaining Balloon Fight issue (green background) is
therefore in the **rendering path** (CHR/nametable/attribute mapping, P3), a far
more contained problem than a CPU-translation campaign.

This **lowers the per-game cost estimate substantially**: the work per new game is
now "confirm correct-grade against tetanes, then fix rendering," not "grind byte-
exact CPU parity from scratch." P1 (generic recompiler hardening) stays valuable
for the games that *do* surface real bugs, but it is no longer the assumed default
for every game.

## Honest expectation

Still a **game-by-game program**, but the validation result above resets the cost
downward: the recompiler already produces correct-grade CPU translations for a new
NROM game, so most games will *not* need an SMB-sized byte-exact CPU campaign — they
need correctness *confirmation* against tetanes plus rendering fidelity. Cost
concentrates in P3 (generic rendering) and in the minority of games that surface
real P1 bugs. Landed this session: P2's JAM→data-boundary auto-discovery, and P0's
real-reference (`nes_ref`/tetanes) with the validation that reframed the whole plan.

## Breadth validation (2026-09-11) — the approach generalizes

Converted three more NROM games with only a minimal profile + auto-added
indirect-dispatch roots (JAM auto-boundary gave 0 lower failures on all), and
measured correct-grade against tetanes:

| game | correct-grade (RAM vs real NES) | render |
|------|--------------------------------|--------|
| Lode Runner | 8/1792 — **99.6%** | title near-perfect; only a slightly darker bg blue |
| Bomberman | 18/1792 — **99.0%** | logo + menu correct; backdrop white instead of black (CRAM[0]) |
| Ice Climber | 544/1792 — 69.6% | incomplete: discovery reached only 60 functions (RAM-pointer dispatch blocks it) |

Two of three are correct-grade CPU translations produced essentially
mechanically — confirming the recompiler generalizes. The failures are now
cleanly separated by *kind*: Ice Climber is a **discovery** gap (indirect
dispatch through RAM pointers, not a translation bug), and the Bomberman/BF
rendering faults are **palette/backdrop** issues in the runtime, not CPU. This
is exactly the P2/P3 split the plan predicts.

## Next steps, in order
1. **Wire tetanes into frame-diff** behind `FD_REF=tetanes` with whole-frame vs
   instruction pre-roll alignment (phase-scan or a RAM anchor), so "correct-grade
   vs real NES" becomes a one-command gate instead of a manual phase scan.
2. **Balloon Fight rendering (P3):** the green background is a CHR/attribute/
   nametable mapping issue, not CPU divergence — chase it in the runtime render
   path, generalizing SMB-shaped assumptions as needed.
3. **Convert the next NROM games** (Excitebike, Ice Climber, Lode Runner) and
   confirm each is correct-grade against tetanes with a minimal profile.
