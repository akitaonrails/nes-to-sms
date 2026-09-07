# Aligned indexed access: first cost-aware lowering experiment

Date: 2026-09-07. Baseline source: `2932630` (weapon fixes at `b9fc51f`).
This completes the bounded first lowering experiment in the
[2× performance plan](cv1-double-performance-research.md). It does not
complete the larger performance target.

## Claim and implementation

For a page-aligned base and an unsigned byte index, the effective address
cannot cross the page. After the existing direct-memory classification, emit
`LD H,page; LD L,D/E; LD A,(HL)` for reads, or `LD (HL),A` for writes.
The address/access body becomes 18T instead of 44T for reads or 52T for
writes. Shadow-flag updates outside that body are unchanged. Stores preserve
A without a spill. Both resident index registers are preserved.

This applies generically to direct RAM and safe low-PRG reads. It changes
neither classification nor banking: unaligned bases retain the full carry
repair, hardware accesses retain their existing paths, and no bounds learned
from gameplay are assumed. No profile, runtime helper, RAM layout, return
contract, interrupt mask or graphics publication protocol changes.

The existing actual-core profiler was sufficient for this narrow claim, so
no persistent diagnostic interface was added. Verification owner: the
implementing agent. Evidence budget: exhaustive index/live-flag domains plus
existing differential tests; unchanged-clock normal-core routes; the existing
weapon/IRQ tests; and SMB's established reference and presentation gates.

## Measured results

All four candidate routes pass their frozen landmark/observation requirements.
Each was run three times; the complete frame/state CSVs repeat exactly.
Core, input contracts and settings are unchanged, including disabled frameskip.

| Route / numeric overclock | Before updates/s | After updates/s | Improvement |
| --- | ---: | ---: | ---: |
| Walk / 500 | 26.188920 | 26.353458 | 0.63% |
| Heart / 500 | 29.264596 | 29.367039 | 0.35% |
| Walk / 250 | 12.391705 | 12.583776 | 1.55% |
| Heart / 250 | 14.139074 | 14.235041 | 0.68% |

The walking actual-core profile over ticks 60→420 falls from 715,396.00 to
710,048.56 nominal T/update (0.75%). The candidate profiler CSV exactly
matches the normal core. Counter conservation passes with no accounting
errors. These totals include idle and recurring IRQ work, not just the
changed instructions. Do not extrapolate their difference into a hardware
speed multiplier.

Walking and heart collection still have post-tick-60 inter-update gap
p50/p95/maximum of 2/3/4 physical frames at 500. Heart collection is still
observed at tick 119. Their final captured pixels are byte-identical to the
baseline. Position differences at walking landmarks are at most one pixel,
within the existing tolerance; no tolerances were changed.

Generated code sections shrink by 676 bytes for CV1 and 230 for SMB. Both
cartridge images retain their 512-KiB size. SMB's established 260-frame
`start_right` diagnostic changes from 118,740 to 118,656 steady approximate
cycles/frame; this is the in-Rust diagnostic, not an actual-core timing claim.

## Correctness and regression evidence

- Workspace: **528 passing tests, 111 ignored**. Formatting and Clippy pass;
  existing Clippy warnings remain visible.
- New tests cover **184,320 explicit states**: every index and all N/V/Z/C
  combinations across aligned loads/stores, both index registers, RAM aliases,
  low-PRG endpoints, index-register loads, ALU consumers, store/read carry
  chains, and the unaligned fallback's page crossings. Memory contents vary
  across states. This is not exhaustive over all possible RAM contents.
- Tests use the existing `oracle_6502` and normal lifter/lowerer/Z80 emulator.
  The new test-local bus supplies NES RAM mirroring; the general harness's
  FlatBus intentionally does not. No CPU semantics were reimplemented.
- Exact emitted-byte tests check the three-instruction recipe and retention
  of carry repair for unaligned bases.
- Eighteen assembled/stub return tests pass, including current allocation
  success/failure and projectile cleanup at legal IRQ boundaries, plus four
  original-ROM weapon tests. The separate historical late-check reproducer
  requiring an older pre-fix artifact was filtered out; it is not counted
  as a pass. Current upper-exit and weapon IRQ coverage was rerun.
- Actual GPGX completes 2,400 indoor updates for natural dagger, seeded cross,
  seeded axe, and rapid axe bursts followed by movement, without traps.
  Fixtures use the same inventory values and input schedules as the accepted
  weapon checkpoint. They establish continued play, not exact equivalence of
  every enemy encounter or natural acquisition of seeded weapons.
- The existing input-only 32-neutral-update upper-exit variant passes its
  unchanged gates and reaches area 2 with positive health. This does not
  certify the ambiguous reset observation in the original frozen sequence.
- SMB's 301-million-step 1-1→1-2 route passes no-trap, zero stale BG variants,
  level/area and lives checks. All seven checkpoint images are byte-identical
  to the accepted build. Its 4,500-frame NES-RAM comparison has no divergence;
  the flagpole's 13 NES-vs-SMS differing cells are identical to baseline,
  not new defects.
- SMB's bonus-pipe (3,000 frames) and death/game-over (3,300 frames) routes
  also have no NES-RAM divergence. Discovery, coverage and unresolved-label
  reports are unchanged for both games; CV1's pre-existing lower-failure
  report is unchanged too.

The first test invocation lacked optional original-ROM/pre-fix-artifact
environment variables; it was not accepted as a gate. Three exploratory
weapon invocations also used incorrect inventory IDs and are excluded from
the weapon evidence. The corrected runs are the `*-verified` directories.
Neither setup issue changed the canonical ROM or acceptance requirements.

## Reproduction and artifacts

Local ignored evidence: `out/cv1-aligned.ZAZrF7/`. Baseline CV1 SHA-256:
`50485bc3903625b4286cb3bb79c403cf234769b983f05155bc6311b276b604df`.
Candidate CV1 SHA-256:
`d4e3e0fe60b5b0df48531b2e48ec5134c3a551e284ecd03d3477ae36d06b4497`.
Candidate SMB SHA-256:
`b0bbcb4a1f93a5d052ea32519fcd987618ce591b65f31e1d2959e9452de06bfa`.

After these gates passed, the candidate CV1 project was copied to `out/cv1/`;
ROM and symbol equality were checked. The preceding playable project remains
under `baseline-cv1/` in the evidence directory. The canonical SMB output was
not overwritten; its tested candidate remains separate.

Generate fresh candidate projects normally and Docker-assemble with `make -B`.
Run `cargo test -p lower -p validation`, then the workspace checks. Use
`tools/core_route.py ... --compare <baseline-summary>` for actual-core routes;
each summary pins the core, ROM, runner and route hashes. See the research
report for profiler setup, the weapon-freeze report for fixtures, and the SMB
acceptance README for the full traversal command. Commercial artifacts stay
untracked. Preserve a known-good project before updating playable output.

## Next decision

Keep this small, generic improvement, but do not spend the next session
extending it to rare address cases. The measurement confirms that this alone
does not materially close the 2× gap. Next target: bank-0 `$8052..$8159`,
especially sprite-index permutation and pointer/flag retention, with the
original-region oracle and live-state contract established before replacing
anything. Broader call/register optimization remains subsequent work.
