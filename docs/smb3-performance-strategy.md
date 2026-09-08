# SMB3 performance: measurements and next strategy

Research and extra testing: 2026-09-08, baseline `d647588`. This is a research
and prioritization result, **not an implemented speedup**. The blink-free SMB3
ROM and accepted SMB1/CV1 ROMs are unchanged.

## Conclusion

The current backend does too much repeated work per NES update. It is not
principally spending its time converting pixels or sending bytes to the VDP.
Two complementary changes offer the strongest route forward:

1. Reuse resolved background cells, not just their uploaded patterns.
2. Recover safe compiler optimizations currently disabled by full MMC3 mode,
   starting with proven internal-RAM accesses and closed native-pointer regions.

The first is the SMS-oriented architectural change; the second reduces the
translation tax. Small jump substitutions, faster OUTI, or blanket CHR
preconversion cannot plausibly solve the whole problem.

Use the [6502/Z80 cycle-cost reference](cpu-cycle-cost-model.md) to price each
candidate, including clock normalization, address/flag setup and semantic
constraints. Whole-workload measurements remain the acceptance criterion.

## What was tested

The fixed ROM is SHA-256
`9a3a3fac277b3ef5936c65b9137777e99770cb593ed28514a5847c40102d6ce5`.
The admitted actual-core profiler and unchanged input-only route were reused.
New runs measured a later 200-update window at `500`, the early window at
`250`, an installed-core `250` parity run, and a read-only SRAM census at `500`.
No state loads, guest writes, core changes, input-driver edits or production
optimizations were introduced. The clock test changes only the requested clock;
both windows retain the original stage predicates and hard limits.

| Moving window | Clock | Nominal Z80 T/update | Observed updates/sec |
| --- | ---: | ---: | ---: |
| Earlier, existing baseline | 500 | 4,301,012 | 4.356 |
| Later, new test | 500 | 4,579,321 | 4.093 |
| Earlier, new test | 250 | 4,246,850 | 2.155 |

These windows are heavier than the **4.924 updates/sec full traversal**; do
not mix their denominators. Every profiler window conserves all 200 complete
intervals, with zero unknown/external or executed-data costs. The early `500`
window reaches 6.276 million T at its nearest-rank 95th percentile: average
speed alone also hides substantial update-time variation.

The later profiler matches the installed baseline's complete 14,188-row prefix
and 582 snapshot files. The `250` profiler matches its separate installed-core
run; all 200 common captured guest/cart boundaries also match `500`. One early
`500` endpoint observation is missing; none is reconstructed. Roughly halving
the clock roughly halves throughput while preserving this sampled game state:
evidence for CPU-bound execution, not an emulator refresh-setting explanation.
Nominal costs still change because wait loops and host service recur differently.

## Where execution time goes

Exclusive categories from actual executed PCs, including their source-boundary
checks; callees are not double-counted:

| Work | Early 500 | Later 500 |
| --- | ---: | ---: |
| Background/sprite resolution and preparation | 31.5% | 35.1% |
| Pending/committed publication, diffs and waits | 18.1% | 14.4% |
| Dispatch, calls and returns | 12.8% | 13.0% |
| Translated inline code, including lowering overhead | 12.4% | 12.8% |
| Memory-bus routing and mapper helpers | 11.9% | 12.2% |
| Frozen-source capture/comparison | 4.2% | 3.7% |
| Other, including guest waits, audio and host service | 9.2% | 8.8% |

“Translated inline code” is **not pure game logic**: it includes address setup,
flag packing and continuation scaffolding. Likewise, publication is not all
upload time. Early pixel-plane conversion costs only **1.29%**, while the OUTI
transport/setup category costs **0.23%**. Those shares exclude their callers and
other VDP helpers, so neither is a total rendering/bus-bandwidth estimate.

### Repeated work is directly observable

- Early: 106,720 cell lookups, 685 BG allocations, 294,733 hash-loop visits.
  Later: 126,208 lookups, 230 allocations, 375,870 visits. About 2.8–3.0 probes
  per lookup are paid even though more than 99% do not allocate a new BG tile.
- Row/cell work, frozen NT reads and hash resolution together cost **27.3%**
  in the early window. The existing horizontal NT ring reduces output changes,
  but a full build still resolves every source cell.
- A new read-only census verified prepared NT bytes against the actual committed
  native NT shadow in every sample. Across 198 consecutive observed boundaries,
  **178,090 of 183,744 physical cell identities remain equal: 96.9%**. Identity
  includes the actual primary/mixed tile keys, subpalettes, cuts and row offsets.
  Missing epochs are not bridged. This measures output reuse potential, **not**
  proof that dependency tracking can detect it cheaply or safely.
- The reused capture scratch flag is not a reliable historical full/stable marker
  at these boundaries. Build classification instead comes from actual entry-PC
  counts: 115 full/85 stable early, 136 full/64 stable later.

## Important compiler difference from SMB1

In `crates/lower/src/lib.rs`, the `if full_bus` block forces `nz_live` true and
clears compare/NZ/direct-branch fusion, copy/fill/decrement-loop, rotate-chain,
16-bit-add and shift-run plans. `emit_full_bus_memory` then intercepts memory
operations before ordinary lowering. Thus the presence of an optimization in
the repository does **not** mean SMB3 benefits from it.

The existing runtime internal-RAM fast path is real, but it is still a helper
call with generic address setup. In the early window, 499,130 of 615,695 bus
helper calls take that fast internal-RAM path, approximately **81.1%**. This
does not prove all their addresses statically; it identifies where range/effect
analysis could replace dispatch with direct native RAM operations.

An exact emitted-byte census finds 528,926 executions of the common 55-T NZ
materialization sequence: **3.38%** of total time. That is all such writes,
not the subset proven dead. Eliminating flag bookkeeping alone is therefore
not a 2× strategy. Restore individual safe cases; do not delete the full-mode
safety block wholesale. Mapper changes, stack observation, aliasing and guest
interrupt-visible flags remain correctness boundaries.

LLVM's useful lesson is explicit memory/clobber and profitability reasoning,
not a wholesale compiler replacement. Retained addresses/values must be killed
at possible aliases, mapper changes and observable boundaries.
[MemorySSA](https://llvm.org/docs/MemorySSA.html),
[InstCombine guidance](https://llvm.org/docs/InstCombineContributorGuide.html).

## SMS-specific strategies, in priority order

### 1. Incremental resolved-cell updates on the existing ring

Track changes to frozen nametable/attribute bytes and relevant physical CHR
mapping, then recompute affected cells and newly entering columns. Retain the
current forced-full renderer as a fallback and differential reference. Separate
ordinary playfield rows from mixed/HUD rows initially; a changed split, mirroring,
reload or source geometry can conservatively request a full rebuild.

This is more than the current dirty upload system: avoid constructing identical
results in the first place. Retain old-live/current-needed ownership and frozen
packet ordering. Attribute writes affect multiple cells; CHR remapping can affect
an entire dependency group. Same output keys alone are not valid invalidation.

LackofTrack's hand-port explicitly buffers vertical column updates and streams
player patterns when their identity changes. It already knows what changed;
our generic backend currently rediscovers much of that information. Its source
is an architectural precedent, not reusable SMB3 game logic or a generic-speed
guarantee. [Column updates and tile streaming](https://github.com/lackoftrack27/Super-Mario-Bros.-SMS/blob/main/main.asm).

### 2. A bounded native row kernel and cheaper exact-key lookup

Resolve frozen record selection, mirroring, row pointers and attribute bases
once per bounded span. Advance HL/DE instead of reconstructing addresses per
read. Handle wraps and mixed rows explicitly, with the existing fallback.
Separately test a small full-key shortcut or better bucket distribution against
the observed collision work. Do not change slot lifetime merely to speed lookup.

These are smaller stepping stones toward incremental rendering. Z80 native
16-bit pointers and block instructions are useful when setup and ABI costs are
amortized; prefix-heavy indexed instructions are not automatically cheaper.
[Zilog instruction timings](https://www.zilog.com/docs/z80/um0080.pdf).

### 3. Proven direct RAM lowering, then native hot regions

First admit zero-page and demonstrably bounded internal-RAM operations, with
canonical NES mirroring and exact final registers/flags. Then restore one
memory-safe fusion or loop idiom. Keep PRG/SRAM/MMIO and unknown pointers on
the conservative bus path. RMW's old/final writes and interrupt-visible progress
must remain correct. Extend pointer/flag retention across a hot region only
after these local cases pass the existing oracle and actual runtime tests.

### 4. Reduce recurring dynamic dispatch, selectively

Early execution makes 48,179 bank-aware tail dispatches per 200 updates and
285,301 lower-bound iterations. A proven direct continuation or a small exact
bank-qualified target cache could bypass repeated searches. Consider selective
semantic inlining only where source return bytes, remapping and external entries
permit it; preserve the out-of-line target and conservative miss path.

The search itself is 7.97%, implying at most about **1.087×** if it vanished
without other effects. Ordinary native CALL/RET costs 27 T; our software/banked
transfers cost more, but blanket inlining is still not a 2× explanation.
[Zilog manual](https://www.zilog.com/docs/z80/um0080.pdf),
[existing IR/inlining research](ir-optimization-research.md).

### 5. ROM-for-compute tradeoffs, only after a measured census

Pre-expand frequently converted CHR identities or native lookup tables where
fetch/setup is cheaper than computation. Avoid blanket four-subpalette expansion:
128 KiB of 2bpp CHR becomes 1 MiB at 4bpp with four variants, before other output.
The current image is already 2 MiB, with only 96 KiB after its last linked
section. Other gaps exist, but fixed/banked placement must be proven before
treating them as usable capacity. Compression adds runtime cost too.

Sega's VDP has 16 KiB VRAM, 32-byte patterns, two 16-color palette banks and
background flip attributes. More cartridge ROM does not add VRAM or automatic
DMA. BG mirror canonicalization may save residency, but those flip attributes
must not be assumed available for sprites. Current palette/mixed-row semantics
also prevent treating all variants as interchangeable.
[Sega hardware manual](https://www.smspower.org/Development/SMSOfficialDocs).

## What not to prioritize

Do not weaken blanking deadlines, drop guest updates, disable sound, or increase
overclock as an “optimization.” Already-implemented OUTI/LUT/bulk-copy techniques
should not be presented as new gains. Polling categories include required beam
waiting: replacing a busy wait with HALT saves instruction activity, not the
waited-for physical time, and may miss required display-service boundaries.

Even erasing the early 27.3% cell/read/hash subtotal gives only **1.38×** under
fixed-work Amdahl assumptions. A real 2× needs roughly half the total elapsed
cost removed through several complementary changes; full NES-speed playback
is a substantially larger target. Neither the census nor these ceilings is a
measured optimization result.

## Ordered implementation experiments

- [x] Implement one bounded frozen-row/address kernel; retain mixed-row fallback.
  [Accepted result](mmc3-row-address-kernel.md): 3.66% full-traversal gain,
  matching checkpoints and no gameplay flashes on the measured route.
- [ ] Add exact source-dependency invalidation and resolved-cell reuse on the ring.
- [ ] Admit proven internal-RAM lowering, then one safe full-mode fusion/loop case.
- [ ] Profile remaining dynamic edges before selecting direct continuation/cache/inlining.
- [ ] Consider selective CHR pre-expansion only if conversion becomes significant.

For every experiment: freeze the old ROM; compare source pixels and forced-full
rendering; test mirroring/scroll wrap/CHR remap/attributes/split/re-enable/cache
capacity and IRQ boundaries; run inventory and full 1-1 input-only routes;
require zero gameplay flashes at `500` and no unsafe writes at `100`; compare
both moving windows and full traversal at unchanged clock; retain exact SMB1/CV1
ROM parity. Compiler changes additionally require 6502 differential tests and
mapper/stack/RMW coverage. Reject a change that shifts cost without improving
measured throughput or violates committed-frame ownership.

Local ignored evidence and repeatable commands are recorded in
`out/smb3-speed-study.AWarm1/PROVENANCE.md`; no commercial ROMs or extracted
assets belong in Git. The post-SMB3 compatibility queue is separate from this
performance work: [mapper roadmap](mapper-roadmap.md).
