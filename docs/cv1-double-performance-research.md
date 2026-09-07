# CV1: research toward twice the gameplay throughput

Date: 2026-09-06. Correctness checkpoint: `b9fc51f`.
Follow-up: [aligned indexed-access experiment](cv1-aligned-indexing.md)
(2026-09-07) implements the first bounded lowering candidate, with measured
0.35–1.55% route gains and its own regression evidence.

This is a measured research assessment and implementation plan, **not an
implemented 2× optimization**. It supersedes the hotspot ranking in
[next performance experiments](cv1-next-performance-experiments.md), while
[master-plan.md](master-plan.md) still defines architecture and policy.

## Conclusion and target

The next substantial improvement needs to optimize **regions of computation
and their data representations**, not just individual opcode substitutions.
The best newly quantified region is the original NES sprite producer. Broader
opportunities are live-flag/register retention, address-range specialization,
and stack-safe call islands. No measured combination yet establishes that 2×
is attainable; the experiments below must earn that claim.

Keep the existing core, numeric overclock `500`, NTSC-U, disabled frameskip,
input routes, game logic and visual acceptance requirements unchanged.

| Frozen 420-update route | Current updates/s | 2× target updates/s |
| --- | ---: | ---: |
| Walking | 26.188920 | 52.377840 |
| Heart collection | 29.264596 | 58.529191 |

These are **game updates**, not the approximately 60 video callbacks/s the
emulator can produce while showing repeated frames. Reaching the targets
would not establish full speed at the stock SMS clock, nor performance in
every room. Add indoor throws, scrolling, stairs and transitions before
generalizing outdoor results.

## Fresh measurements, not historical bottlenecks

The accepted weapon fixes were committed first. The canonical CV1 ROM was
not changed during this research. New actual-GPGX instruction profiles cover
360 complete game-update intervals, ticks 60→420. The total includes active
execution, interrupted work, idle work and interrupt entry events.

| Workload | Nominal Z80 T/update | Non-IRQ idle T/update | Remaining T/update |
| --- | ---: | ---: | ---: |
| Walk, 500 | 715,396.00 | 113,155.61 | 602,240.39 |
| Heart, 500 | 627,597.08 | 109,552.62 | 518,044.46 |
| Walk, 250 | 744,979.59 | 50,567.09 | 694,412.50 |

The remaining column includes lag-handler and presentation-service work; it
is not a pure game-logic cost. At 250 the walk route passes at 12.391705
updates/s. Its sprite-producer direct cost is identical to 500, while other
work increases. This demonstrates repeat-work amplification, not a universal
linear relationship between the menu value and useful execution.

The instrumented walk/heart 500 frame/state CSVs exactly match the preceding
normal-core runs of this same ROM. A fresh normal walk 250 run also exactly
matches its instrumented counterpart. Profiler accounting reports no errors;
all exclusive categories sum to its total. Nominal costs are the core's
unscaled master-cycle charges divided by 15. Its reciprocal, integer-rounded
overclock accounting is different; use the recorded scaled charges for that
purpose. See [core profiling](core-profiling.md) and the pinned
[GPGX timing implementation](https://github.com/libretro/Genesis-Plus-GX/blob/a7985a9c4278ac352f8ca7bb4d3cc6b36e9e3e7d/core/z80/z80.c#L202).

### Where the walking cost goes at 500

Selected **exclusive** categories, rounded; omitted categories remain in the
full local report, not silently discarded:

| Category | T/update | Share of all work |
| --- | ---: | ---: |
| Other generated operations, including inline flag work | 155,570 | 21.75% |
| Generated indexed operations, including inline flag work | 50,442 | 7.05% |
| Generated JSR scaffolding | 57,027 | 7.97% |
| Software call/return runtime | 53,411 | 7.47% |
| Fixed-PRG read helpers, including idle callers | 66,719 | 9.33% |
| Idle loop, direct instructions | 55,011 | 7.69% |
| CV1 SAT preparation/lookup/scheduling residual | 41,609 | 5.82% |
| CV1 HUD section | 32,710 | 4.57% |
| PPU section | 31,603 | 4.42% |
| CV1 frame-driver section | 30,084 | 4.21% |
| Generated conditional branches | 22,512 | 3.15% |
| BG preparation | 22,113 | 3.09% |
| Indirect/banked dispatch | 19,054 | 2.66% |
| APU shim, excluding mapper routines | 13,438 | 1.88% |
| Mapper writes | 11,775 | 1.65% |
| Out-of-line flag helpers | 10,060 | 1.41% |
| SAT upload | 3,492 | 0.49% |
| Sprite-pair pixel conversion | 197 | 0.028% |

Do not mistake the small **out-of-line** flag bucket for total flag cost.
`emit_adc_flags_inline`, `emit_set_nz_inline` and related sequences are charged
to their originating generated operations. Likewise the SAT preparation
bucket is not all pixel conversion. Pair conversion on the heart route is
964T/update, still only 0.154%; these routes do not support prioritizing
pre-flipped CHR as a large speed win.

Of the 66,719T fixed-PRG helper cost, 58,144T belongs to the main idle loop:
only about 8,574T is outside that context. The original `$C030` loop changes
game-visible random state. Replacing it with HALT is neither a free semantic
change nor proof of increased game throughput.

### Newly quantified region: the NES sprite producer

Bank 0 `$8052..$8159` builds/shuffles NES OAM, visits object/metasprite data,
and hides unused entries. Its **direct translated span** costs:

| Route | T/update | Share of all work |
| --- | ---: | ---: |
| Walk 500 | 87,790 | 12.27% |
| Heart 500 | 87,379 | 13.92% |
| Walk 250 | 87,790 | 11.78% |

Runtime callees are excluded. The preceding bank-0 PPU-buffer consumer,
`$800A..$8051`, costs another 10,619T walking, also excluding runtime callees.
These region totals **overlap** the opcode categories above.

The OAM permutation loops around `$80B6` and `$8123` repeatedly add, compare,
and branch to select the next sprite slot. The translated `$80B7` ADC alone
costs 8,799T/update walking; its subsequent CMP costs 6,250T. This gives us a
concrete region for retained native flags, pointer reuse, or a proven small
lookup table. It is not a request to rewrite object physics or collision logic.

Attribution used labels inserted at original-op comments in a **copied**
generated project. WLA rebuilt it with `make -B`; its entire ROM is
byte-identical to the canonical artifact. Original-op spans and WLA section
bounds determine ownership, not nearest-symbol guesses. Explicit runtime
subranges were checked against source. Small unowned/vector/entry residuals
remain visible in the report.

## What prior research actually contributes

### The SMB SMS hand port: native production, not a magic translator

The upstream HEAD remains `40a160cccc49971712db3d8f2db76aeba6210839`.
Its frame-complete handshake skips VDP and controller work on lag frames;
sprites are uploaded from separate Y and X/tile buffers through unrolled OUTI;
column updates and tile streaming are conditional. Page-organized tables
also permit low-byte pointer arithmetic. These are inspectable implementation
choices, not a new hardware-utilization benchmark.
[Source: main.asm](https://github.com/lackoftrack27/Super-Mario-Bros.-SMS/blob/40a160cccc49971712db3d8f2db76aeba6210839/main.asm#L409).

Our prepared SAT, frame-publication protocol and unrolled upload already
apply part of this lesson. The remaining opportunity is upstream: avoid
expensively constructing a NES-oriented representation only to traverse it
again for SMS. First optimize an equivalent NES-OAM producer; fuse it with
SAT generation only after proving all consumers and observable writes.
The old [hand-port comparison](handport-comparison.md) remains useful history,
but its claims about hard ceilings or the impossibility of proving index
bounds are not established limits of this compiler.

### Cross-ISA superoptimization: select sequences and register maps together

Bansal and Aiken's **Binary Translation Using Peephole Superoptimizers**
(OSDI 2008) learns equivalent instruction sequences and chooses register
maps with transition costs. It combines inexpensive execution screening
with formal equivalence checking. The directly applicable idea is a small,
target-costed catalogue of verified multi-op translations—not assuming that
individually cheap instructions compose into the cheapest block. Its
PowerPC/x86 results do not predict CV1 performance.
[Paper](https://www.usenix.org/legacy/event/osdi08/tech/full_papers/bansal/bansal.pdf).

Adaptation: harvest our hottest emitted sequences, retain 6502 semantics as
the specification, and compare candidate Z80 blocks with the existing
oracles. Exhaustive testing can prove small finite domains; randomized tests
alone cannot prove arbitrary memory/interrupt equivalence. Start with a few
bounded patterns rather than building a general superoptimizer first.

### Mature 8-bit compilers: cost and lifetime analysis

Millfork's Z80 variable-to-register pass scores non-overlapping lifetimes in
cycles and bytes and considers multiple register assignments. Its peephole
passes use forward/backward facts about registers and flags. This is a useful
model for promoting proven RAM temporaries and retaining pointers; do not
import rules for its other CPU variants into a plain SMS Z80 backend.
[Register pass](https://github.com/KarolS/millfork/blob/master/src/main/scala/millfork/assembly/z80/opt/ByteVariableToRegisterOptimization.scala),
[peephole rules](https://github.com/KarolS/millfork/blob/master/src/main/scala/millfork/assembly/z80/opt/AlwaysGoodZ80Optimizations.scala).

SDCC explicitly distinguishes speed from size optimization and uses
target-specific register-assignment costs. Its unsafe-read option warns that
extra speculative reads can harm memory-mapped I/O. That boundary matters
here: hardware reads and mapper changes cannot be treated as ordinary RAM.
This is algorithmic guidance, not a proposal to replace our pipeline with C.
[SDCC manual, optimization options and register allocation](https://sdcc.sourceforge.net/doc/sdccman.pdf).

LLVM-MOS illustrates a related architectural lesson: use zero-page pseudo
registers and static stack placement appropriate to a 6502, with interrupt
constraints. Our corresponding SMS opportunity is native register/pointer
retention, but arbitrary original RAM is not automatically a private local.
[LLVM-MOS EuroLLVM talk](https://llvm.org/devmtg/2022-05/slides/2022EuroLLVM-LLVM-MOS-6502Backend.pdf).

### Earlier NES recompilers: useful warnings, no demonstrated shortcut

Andrew Kelley's Jamulator documents how per-instruction synchronization and
observable machine state obstruct optimization, and how NES games manipulate
returns and enter overlapping instructions. Its conclusion concerns its
emulation design; it does not prove profile-guided SMS conversion impossible.
For us, the lesson is to establish safe region boundaries and retain the
recently fixed return-ownership contracts, not add an interpreter or make
every JSR a native CALL.
[First-person project report](https://andrewkelley.me/post/jamulator.html).

I also inspected `celsowm/nes2sms` at
`606fcfa4e056d4c8e692c8441676a80789cd83cd`. Its flow-aware translator dispatches
individual instructions and maps calls/returns directly; the inspected source
does not establish an answer to CV1's materialized/consumed returns or a
measured full-speed CV1 conversion. Treat it as another implementation to
compare, not a validated optimization oracle.
[Inspected translator](https://github.com/celsowm/nes2sms/blob/606fcfa4e056d4c8e692c8441676a80789cd83cd/src/nes2sms/core/assembly/flow_aware_translator.py).

## Architectural opportunities and constraints

### CPU: optimize T-states and live semantics, not opcode counts

An NTSC NES CPU runs at about 1.79 MHz; the SMS Z80 runs at about 3.58 MHz.
That nominal 2:1 clock ratio does not imply twice the useful work: their
instructions require different numbers of clock periods, and preserving one
6502 operation may take several Z80 instructions. Compare the full emitted
sequence's execution time, including flags and address calculation, against
the workload's frame deadline.
[NES CPU reference](https://www.nesdev.org/wiki/CPU),
[SMS clock reference](https://www.smspower.org/Development/ClockRate).

Z80 register moves cost 4T, loads through HL 7T, absolute accumulator loads
13T, and indexed IX/IY displacement loads 19T. CALL/RET cost 17T/10T before
our bookkeeping. Unrolled LDI/OUTI cost 16T per byte; repeated LDIR/OTIR
iterations cost 21T except the last. Shorter code is not always faster:
conditional JP is 10T, while JR is 12T taken and 7T untaken.
[Zilog CPU manual](https://www.zilog.com/docs/z80/um0080.pdf).

In our current `emit_indexed_read_direct`, a resident D/E index still needs
a conservative address-building sequence. These calculated costs exclude
subsequent shadow-flag work and any boundary saves:

| Address recipe | T-states | Required fact |
| --- | ---: | --- |
| Current full 16-bit address addition and load | 44 | General direct region |
| Same recipe without high-byte carry repair | 29 | Base-low + index cannot overflow |
| Load H with page, L with resident index, then load | 18 | Page-aligned base |
| Load through an already retained HL | 7 | Pointer valid and live across region |

The first safe improvement is range/alignment specialization. The larger
one is **amortizing address creation across a loop**, with register-allocation
and flag costs included. D/E residency and indexed inlining already exist;
so do local flag liveness, branch fusion and selected multi-byte idioms.
Extend those mechanisms rather than claiming them as new work.

6502 loads update N/Z while Z80 loads do not; subtract/compare carry polarity
also differs. Track which flags are live, their native representation and
where they must be materialized. Retaining native flags over a branch or
carry chain requires correct joins, helper clobbers, PHP/PLP and interrupt
entry handling. Do not globally replace shadow P with Z80 F.

### RAM and cartridge bus: layout is valuable, but space is already owned

The NES has 2 KiB internal CPU RAM with mirrors, and UxROM presents a
switchable `$8000..$BFFF` PRG window plus fixed `$C000..$FFFF` PRG. These
source addresses and bank identities remain observable to translated code.
[NES memory reference](https://www.nesdev.org/wiki/Nestech.txt),
[UxROM reference](https://www.nesdev.org/wiki/UxROM).

SMS has 8 KiB of work RAM, mirrored above it, and Sega mapping gives three
16-KiB cartridge windows; slot 2 may expose cartridge RAM. Mapper writes
also affect the RAM mirror. More addressable ROM is useful for lookup tables
and duplicated read-only data, not an excuse to overwrite live RAM.
[SMS memory-map reference](https://www.smspower.org/Development/MemoryMap).

Current CV1 uses fixed runtime code, banked translated code, and a slot-2
window shared by original PRG reads and cartridge SRAM. Raw CHR/CIRAM and
metadata already occupy that SRAM. Native SP starts at `$DFFC` and must
remain above the `$DE40` floor; see the current master-plan RAM contract.
Repacking original object arrays needs alias closure, including indirect
access and interrupts. Safer first steps are proven pointer retention and
read-only table copies colocated with hot translated code.

Batch mapping around bounded kernels where it actually saves transactions.
Preserve selected UxROM bank, bus-conflict behavior, mapper mirrors, interrupt
restore and PRG-as-data reads. CPU ROM execution is not intrinsically slower
than RAM execution on this target; copying code to scarce RAM is not a
general cache optimization.

### PPU versus VDP: produce the destination format without losing observability

SMS Mode 4 shares 16 KiB VRAM between patterns and tables. Tiles use 4bpp;
background entries support flip/palette/priority attributes, but sprite entries
do not offer per-sprite flip bits. The SAT separates Y from X/tile data and
supports 8×16 sprites. Conversion and variant residency must respect those
differences, not assume NES tile/OAM data can be uploaded unchanged.
[Charles MacDonald's hardware research](https://raw.githubusercontent.com/franckverrot/EmulationResources/master/consoles/sms-gg/Sega%20Master%20System%20VDP%20documentation.txt).

Use hardware scrolling, prepared columns, precomputed tile maps and batched
port output where the original writes' consumers are known. They are not
permission to discard CHR-RAM mutations, stale-cache checks or committed-frame
ownership. SMS's top horizontal-scroll lock covers only 16 pixels, not the
entire current CV1 HUD; HINT remains necessary for our larger split. Extended
224/240-line modes also have hardware-variant constraints.
[VDP register reference](https://www.smspower.org/Development/VDPRegisters).

NES OAM DMA transfers 256 bytes in 513/514 CPU cycles, excluding competing
DMC activity; it is hardware-assisted, not free CPU time.
[NES DMA research](https://www.nesdev.org/wiki/DMA).
There is no equivalent DMA upload in this SMS runtime. Optimize
the CPU producer and required output bytes; do not propose Genesis DMA,
Z80N instructions, a second processor, or arbitrarily faster VDP writes.
Overclock changes CPU scheduling, not the physical display's publication
windows. Preserve the coherent BG/SAT/HUD publication that fixed moving-frame
glitches. Reduced CPU work is useful only if it advances the next coherent
game frame without starving audio/input.

## Ordered implementation experiments

Each candidate gets a separate baseline, proof obligation, cost measurement
and rollback point. The implementing agent owns verification. Do not start
with an unrestricted allocator, whole-game native-stack conversion, or a
complete new graphics runtime.

1. **Finish the measurement contract for hot regions.** Preserve source-op
   spans in an opt-in compiler report; add callee-context attribution and
   inline-flag/address/setup breakdown. Extend the validated profiler window
   to existing indoor weapon/traversal routes. Add an event-level upper-exit
   reset observer: the current sampled `255→1` gate is ambiguous, not a pass.
   Host-only changes must leave normal-core traces unchanged. Stop adding
   instrumentation once it can rank and validate the selected candidate.

2. **One bounded cost-aware lowering prototype.** Start with aligned or
   proven non-crossing direct RAM access. Use generic analysis facts, not
   hard-coded CV1 addresses. Cover all admitted indices, page/wrap boundaries,
   mirrors, alias invalidation and live flags with original/translated tests.
   Include setup/spills and branch frequency in the cost model. Retain the
   conservative fallback when proof is missing. This validates the optimizer
   machinery; it is not expected to produce 2× alone.

3. **Optimize the measured sprite-producer region.** First try multi-op
   carry/compare and loop-pointer retention through the normal pipeline.
   A profile-declared native Z80 presentation replacement is an alternative
   if its original-region oracle closes. Preserve original OAM, temporary
   RAM, ordering, registers, live flags and every legal entry/exit. Test all
   index-permutation inputs, offscreen/sentinel cases, sprite counts and
   flips. A ROM lookup for the permutation needs a proven full-domain mapping
   and the correct surviving flags, not just an observed index sequence.
   Only then consider producer→SAT fusion, after checking every NES-OAM reader
   and preserving or proving dead each intermediate write. No Rust game port.

4. **Expand to register/flag-retaining regions and hot call islands.** Rank
   exact call edges. Use interprocedural effects and backward liveness to
   avoid repeated shadow-state materialization, and retain HL pointers or
   private RAM temporaries where aliases permit. Native CALL/RET or selective
   inlining needs a closed, stack-unobserved region, a correct software-stack
   boundary and bank restoration. Materialized returns, PLA/PLA unwinding,
   arranged returns and unknown dispatch remain on the proven path. Include
   code-bank growth and IRQ register-save costs when deciding whether it wins.

5. **Reduce remaining presentation and lag work, using new profiles.** The
   SAT preparation residual, HUD, PPU and frame-driver buckets deserve
   per-phase and call-context measurements. Seek redundant lookups, scans,
   rebuilds or state conversions, not blind removal of service on lag frames.
   Test dirty metadata, CHR epochs, pinned variants, partial writes, screen
   transitions and audio cadence. The old warm-slot hint was rejected; do
   not revive it without a new measured mechanism. Bounded map-once kernels
   and small colocated ROM tables follow when their active cost warrants it.

6. **Take small proven wins only where worthwhile.** `_cv1_bg_copy` has a
   calculated 6,930T gross saving per full 1,408-byte copy from unrolled LDI;
   measure copies per update and deadline effects before implementing it.
   SAT upload is already unrolled. Preconverted CHR variants remain deferred
   unless indoor/cold-cache profiles show materially different conversion
   costs. Never trade missing sprites or stale background columns for speed.

After every accepted candidate, reprofile: cost shares and interrupt
amplification change. Prefer a small first proof followed by the largest
verified region; do not spend repeated sessions chasing sub-percent sites.

### What a 2× budget would require

At fixed work mix, halving total execution cost is necessary for twice the
throughput. Making a 15% component free yields only about 1.18×. Our JSR
scaffolding plus software call/return runtime is roughly 15.4% of all walking
cost; even eliminating it is insufficient. The sprite region's 12.3% is an
upper bound on savings from its direct instructions, not an expected gain.
It overlaps those call/flag/index buckets.

The existing indexed-op bucket is only 7.05%; the 44→29 address recipe saves
only part of that bucket. Actual sprite-pixel conversion is smaller still.
Therefore a credible 2× program needs broader region improvements plus
reduced presentation/repeated-handler work, with **disjoint measured savings**.
Use the 602k non-idle walking T/update as a diagnostic baseline, not a promise
that exactly 301k will meet the throughput target. Admission thresholds and
recurring interrupts require end-to-end confirmation. If the first regions
do not reveal enough removable work, revise the target estimate explicitly.

## Verification and non-regression contract

The verification-planning workflow separates semantic correctness, final
instruction cost, and user-visible throughput; none substitutes for another.

- **Semantics:** existing `oracle_6502` versus emitted Z80, exhaustive small
  domains plus randomized full states, actual assembled runtime tests, and
  both helper implementations kept aligned. Cover legal internal entry points
  and every transformed memory/flag effect. No ad-hoc interpreter as proof.
- **Interrupts/returns:** rerun allocation success/failure, cross acceleration,
  projectile catch/expiry, dagger, upper-exit and return-escape tests. Inject
  IRQs at transformed boundaries; preserve guest S, live return bytes, native
  stack headroom and current bounded interrupt-masking behavior.
- **Actual gameplay:** unchanged normal-core 420-update walk/heart A/B at
  500 and 250; 2,400-update indoor dagger/cross/axe scenarios including capacity
  failure and rapid axe bursts; stairs/door/next-room progression. Controlled
  weapon fixtures do not certify natural pickup paths. Do not relax an
  inconclusive route oracle into a pass.
- **Rendering and audio:** compare coherent game-state-aligned frames and
  existing image/dirty-tile oracles; inspect intervening moving frames for
  stale left columns, white flashes, missing sprites and HUD tearing. Keep
  normal physical-frame behavior and sound/input cadence under observation;
  retiming is not expected to produce identical before/after callback CSVs.
  Exact CSV equality is required for observer-only changes, not faster ROMs.
- **SMB:** fresh canonical-ROM generation, original RAM/VDP/frame oracles,
  301-million-step 1-1→1-2 no-trap route, zero stale BG variants, lives and
  speed checks. Generic changes cannot regress the working mapper-0 game.
- **Build gates:** workspace tests, formatting, Clippy with existing warnings
  visible, Docker WLA assembly in a fresh output directory or with `make -B`.
  The generated Makefile currently misses dependencies on included assembly;
  plain `make` in a reused tree can silently benchmark an old ROM. Fix that
  dependency tracking separately before relying on incremental-build results.

Report actual update rate, nominal/scaled cost, inter-update gap distribution,
publication latency/blanking, bank switches, native-SP minimum and ROM growth.
Use identical core options and declared input contracts. Three repeated
normal-core runs per accepted performance candidate should agree; investigate
any nondeterminism. Never count faster host execution or higher overclock as
a translated-code optimization.

## Preserved checkpoint and reproducibility

Commit `b9fc51f` contains the verified weapon-return fixes. Its recorded gates
include 523 passing workspace tests (111 ignored), 5,046 allocation/cleanup
IRQ boundaries, successful allocation, source oracles, real-core weapon
routes and SMB's 1-1→1-2 route. These are reused checkpoint results, not all
rerun for this documentation-only research. See
[weapon-freeze evidence](cv1-weapon-freeze-2026-09-06.md) for limitations.
The research handoff additionally passes 64 host-side `test_core*.py` tests,
format checking and Git whitespace checks. Canonical ROM and `AGENTS.md`
hashes were rechecked unchanged.

Canonical CV1 SHA-256:
`50485bc3903625b4286cb3bb79c403cf234769b983f05155bc6311b276b604df`.
Accepted fresh SMB SHA-256:
`08c4aee776abc79a9e4977eff9bca5620ae68a89aa5891dd1015171cccc8044d`.
Normal core SHA-256:
`a6da7c738dfa87708d173b2034b71b84368d6adf5a53126ebb5791933ce929bd`.
Instrumented core SHA-256:
`4d82c8a8fd8e69ee1542b6cadec9f62afa6107748bf18399be4a629357d75437`.

Local ignored research evidence is in `out/cv1-double-research.J7pFZY/`:
three `profile-*/` runs, `normal-walk-250/`, `attribution.txt`, the label/span
scripts, and the byte-identical labeled project. Every route summary pins
ROM, core, route profile and runner hashes. Commercial inputs, generated
assembly/assets and screenshots are intentionally not committed.

Example reproduction with the already validated isolated profiler core:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_profile.py \
  out/gpgx-profile.xwbUpU/instrumented/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-routes.toml \
  walk out/cv1/acceptance/research-walk-500 --capture-every 10
```

Use a fresh destination, substitute `heart`, or add `--overclock 250`.
Run `tools/core_route.py` with the normal core and identical options for the
observer-parity comparison. Rebuilding the instrumented core is documented
in [core profiling](core-profiling.md); no host retro-tool installation is
needed. Leave the canonical playable artifacts unchanged until a candidate
passes its complete gates.
