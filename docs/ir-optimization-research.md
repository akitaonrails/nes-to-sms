# LLVM-style optimization for the NES-to-SMS IR

Research date: 2026-09-07. Source inspection: `5b1ba1e` plus the in-progress
MMC3 banking changes. Research only: no optimizer, dependency, or new diagnostic
interface was added. This complements [the measured CV1 research](cv1-double-performance-research.md)
and does not change the active [SMB3 milestones](smb3-plan.md).

## Recommendation

Borrow LLVM's separation of semantic analysis, profitability, and target
lowering. Do not replace the pipeline with LLVM to obtain an optimization flag.
The promising next step is a small, verified value/effect analysis over a real
control-flow graph, followed by one hot-region optimization. Selective inlining
is useful **late in semantic optimization, before final lowering and bank
placement**. Inlining final assembly would lose precisely the stack, mapper,
flag, and source-identity information needed to make it safe.

This is a hypothesis about engineering leverage, not evidence of a 2× gain.
The current compiler already implements many of the obvious local improvements.

## What exists, and what would actually be new

| Inspected implementation | Already present | Missing generalization |
| --- | --- | --- |
| `crates/ir/src/lib.rs`: `Op`, `AddrExpr`, `MemRegion` | Explicit guest operations, address modes, hardware effects, return ownership | SSA values, block arguments/joins, general value ranges and memory effects |
| `crates/lower/src/lib.rs`: `flags_live_after`, `routine_incoming_flag_reads`, `nz_shadow_live_after` | Per-flag local liveness; conservative incoming-flag summaries; native branch fusion | CFG-wide fixed-point summaries and flag representations across joins; current summaries stop crediting writes after control-flow boundaries |
| `lower`: `IdxReg`, indexed helpers | X/Y resident in Z80 D/E; direct-region access; page-aligned addressing | General pointer lifetime allocation and range-proven nonaligned accesses |
| `lower`: `match_add16`, `match_copy_loop`, fill/decrement-loop and shift-run plans | Carry-chain, block-copy, and selected loop idioms | General loop induction/range analysis; costed alternatives beyond recognized shapes |
| `crates/z80_emit/src/lib.rs`: `native_call`, `far_call`, tail helpers | Same-section near calls/jumps; bank-restoring far transfers | Selective semantic call-site cloning and post-inline simplification |
| `crates/cli/src/pipeline.rs`: hot grouping, edge-weighted placement, frozen sizing/layout | Profile-informed colocation and stable final placement | Inlining decisions informed by resulting bank pressure and hot-edge costs |

The IR is an ordered machine-state program, not LLVM-style SSA. Its operations
implicitly use/define A/X/Y/P/S. `MemRegion` selects hardware semantics; it does
not prove that two accesses cannot alias. The emitter immediately maintains
bytes, assembly, relocations, and label metadata together. Consequently,
post-processing assembly text is not a safe general optimizer architecture.

Older conclusions in [optimizer-plan.md](optimizer-plan.md) and
[optimization-findings.md](optimization-findings.md) are historical, not current
performance ceilings. The later aligned-indexing experiment is especially
instructive: a much cheaper address recipe produced only **0.35–1.55%** measured
route-throughput gains. Removing a conspicuous instruction sequence is not the
same as removing a large fraction of gameplay work. [Measured results](cv1-aligned-indexing.md).

## Ranked opportunities

These ranks combine existing workload evidence, applicability, and proof cost;
they are not measured rankings of unimplemented passes.

### 1. CFG value/flag analysis, then a bounded hot-region combine

Build optimization blocks from the existing discovery/lifter's legal entries,
labels, branches, calls and escapes; do not create a competing ROM analyzer.
Compute use/def and per-flag liveness to a fixed point, including loops and
callee summaries. Key summaries and call edges by physical bank, CPU window,
and entry PC where applicable, never by a bare 16-bit address shared by banks.
Initially carry A/X/Y and individual N/Z/C/V values through
pure straight-line regions; merge incompatible facts to unknown. This need not
begin as a wholesale new SSA crate.

The first consumer should remove redundant flag packing or retain a pointer in
the measured CV1 sprite producer, not merely produce analysis reports. Preserve
the conservative lowering at unproven joins. LLVM's InstCombine lesson is to
canonicalize expressions with explicit legality tests; it is not to minimize IR
instruction count regardless of the target. [InstCombine guide](https://llvm.org/docs/InstCombineContributorGuide.html).

Treat N, Z, C and V independently. Z80 loads do not set N/Z; subtraction carry
has the opposite polarity; P/V means overflow for some operations and parity
for others. A representation may be native, inverted-native, derived from a
retained value, or materialized in shadow P. At a merge or helper boundary,
reconcile representations explicitly rather than assuming Z80 F always equals
guest P. PHP/PLP, BIT, RTI and IRQ bridges are observable consumers.
Keep 2A03 semantics: decimal arithmetic is disabled, but the guest D bit can
still be observed. An unsupported operation cannot become optimizable dead
code before the pipeline's required capability check has run.

### 2. Local value numbering and pointer retention, then loop extension

Eliminate an ordinary-RAM reload after a proven same-value store, reuse an
effective address, or replace repeated address construction with HL increments.
Only then extend across blocks/loops. General constant propagation can fold
guest expressions and prove index bounds; SCCP also reasons about executable
edges, whereas ordinary folding cannot safely discard a branch's other path.
[LLVM pass descriptions](https://llvm.org/docs/Passes.html#sccp-sparse-conditional-constant-propagation).

Use canonical NES storage identities: `$0000` and `$0800` alias; a pointer's
low/high bytes may be changed independently; `$FFFF + 1` wraps. A PRG read's
identity includes its physical page and a valid mapper-state fact, not just
the CPU address. Unknown stores/calls invalidate possibly affected facts.
NMI-shared memory cannot become a private register temporary merely because
the current function contains no aliasing write. LLVM's Mod/Ref and MemorySSA
provide the useful model—memory versions plus clobber queries—not permission
to treat our whole RAM array as nonaliasing locals.
[Alias analysis](https://llvm.org/docs/AliasAnalysis.html),
[MemorySSA](https://llvm.org/docs/MemorySSA.html).

Loop strength reduction and wider copy/fill recognition follow these proofs.
Preserve zero-iteration behavior, traversal direction, overlap, exact final
A/X/Y/flags, and interrupt-visible progress. Existing LDIR idioms are not a new
proposal. LICM must not hoist an MMIO read, bank-dependent read, or potentially
observable access from a loop that originally did not execute.
[LLVM loop definitions](https://llvm.org/docs/LoopTerminology.html).

### 3. Late, selective semantic inlining

Inlining can remove software continuation overhead and expose constant
arguments, dead flags, and common addresses across a call. Our ordinary calls
pass machine state rather than typed arguments; those incoming values need
explicit summaries before specialization is useful. LLVM's inline advisor
consults target costs and profile information and can defer a decision because
of outer-call opportunities. The applicable lesson is contextual benefit, not
copying its numerical threshold. [Inline advisor source](https://llvm.org/doxygen/InlineAdvisor_8cpp_source.html).

First admission policy:

- Small, hot, statically resolved, nonrecursive callees with known entry/exit
  state and no externally entered interior labels. Retain the out-of-line
  original when other callers or indirect tables still need it.
- Exclude `MaterializedJsr`, `ReturnConsume`, `ReturnEscape`,
  `ReturnEscapeConsume`, `RtsDispatch`, RTI, stack-address observation, and
  unknown transitive callees. A callee without PHA is not sufficient proof:
  descendants, aliases, and IRQ paths may observe S or return bytes.
- Exclude mapper writes/remapping continuations initially. Resolve MMC3 targets
  by physical 8 KiB page **and CPU window**; a label such as `$8000` alone is
  insufficient. Keep dynamic dispatch when bank identity is unproven.
- Clone labels hygienically, redirect only the clone's ordinary RTS exits,
  and preserve source PC plus original bank and inline call-chain provenance.
  Never erase return-ownership checks simply because the call disappeared.
- Recompute liveness/effects after cloning. Keep guest-visible registers,
  memory and live stack bytes consistent with the established translation
  contract. Preserve every guest I flag change and real Z80 interrupt contract.

Run initial cheap analysis and simplification; estimate candidate savings;
clone a bounded selection; simplify the clones; lower and place again. Reject
candidates that overflow a bank or make important previously-near edges far.
Freeze the selected clone set before the existing final sizing/layout pass;
use a bounded retry/rollback rather than an unstable optimize/place loop.

This differs from already-inlined runtime flag bodies. Those save helper
CALL/RET but do not generally expose a guest caller/callee region. Pure late
assembly inlining would save transfer overhead but miss most semantic benefit.

### 4. Costed Z80 instruction selection and constrained register allocation

Represent candidate recipes with real register/flag clobbers and score the
whole region, including entry/exit moves. Prefer a small verified catalogue
of alternative sequences over a generic superoptimizer project. Bansal and
Aiken jointly optimize instruction sequences and register mappings, including
transition costs; their cross-ISA results do not predict an SMS speedup.
[OSDI 2008 paper](https://www.usenix.org/legacy/event/osdi08/tech/full_papers/bansal/bansal.pdf).

Keep A/D/E residency as the baseline ABI. B/C/HL are scarce and heavily used
by helpers; IX/IY instructions are not automatically cheap, and alternate
registers already participate in interrupts. Start with one proven pointer
lifetime or private temporary, not global zero-page promotion. LLVM separates
instruction selection, allocation and target-specific legalization for this
reason. [Code-generator architecture](https://llvm.org/docs/CodeGenerator.html).

### 5. Guarded specialization and destination-oriented kernels

Specialize a hot call for proven constant state or a bounded table bank, with
the general path retained when needed. Observation-only profiles rank targets;
they do not establish legal input ranges. Batch PRG reads inside a bounded
fixed-code kernel only when mapping, interrupts, pointer wrapping and all
observers are accounted for. Representation changes that fuse NES OAM
production with SMS SAT production need a stronger consumer/alias proof than
ordinary expression optimization; defer them until an equivalent NES-OAM
region is verified. This remains profile/runtime work, never a Rust game port.

## A cost model appropriate to this target

Track at least nominal T-states, emitted bytes, peak stack/register pressure,
mapper transactions, and maximum interrupt-disabled duration. Reject unsafe
bank/stack/deadline candidates before comparing speed. LLVM similarly exposes
separate latency, throughput and code-size cost kinds; its generic defaults
are not a Z80 timing model. [TargetTransformInfo](https://llvm.org/doxygen/classllvm_1_1TargetTransformInfo.html).

For a candidate region, estimate:

`benefit = executions × (old body + old transfers − new body − new boundary work)`

Then account for changed bank placement and repeated interrupt/presentation
work separately. Frequency-weighted expected cost and worst-case latency are
different acceptance criteria. For example, Z80 conditional JP costs 10T;
JR costs 7T untaken and 12T taken. Thus JR's expected 7+5p is faster only for
taken probability below 0.6, before layout effects. Native CALL plus RET is
27T, but our software/far-call sequences cost more. Neither count guest opcodes
nor optimize x86-style superscalar throughput for this CPU.
[Zilog timing tables](https://www.zilog.com/docs/z80/um0080.pdf).

Existing walking data assigns 7.97% to generated JSR scaffolding and 7.47%
to software calls/returns. Eliminating both completely would imply about
**1.18×**, not 2×, under a fixed-work Amdahl calculation. Real inlining cannot
erase all of it, may expose further body savings, and may change recurring
service work. Similarly, the measured sprite-producer span is about 12–14%
of total work and overlaps instruction categories. These are historical
baseline bounds and prioritization clues, not a fresh benchmark or additive
savings forecast. [Measured attribution](cv1-double-performance-research.md).

## LLVM itself: feasible research, wrong immediate integration

LLVM can express wrapping `i8` arithmetic and explicit flag computations;
adopting it does not inherently discard guest semantics. But a correct bridge
would need state/effect modeling, verified lowering back to our ABI, banked
linkage and stack ownership, plus a suitable Z80 backend. Upstream's current
normal/experimental target lists do not contain Z80.
[Upstream target list](https://github.com/llvm/llvm-project/blob/main/llvm/CMakeLists.txt).

Out-of-tree options exist. The current `llvm-z80/llvm-z80` README documents
Z80 support and ELF/SDCC output paths but explicitly describes the project as
experimental, not production-stable. This updates older blanket statements
that all such work is dormant; it does not establish compatibility with our
WLA-DX runtime or Sega mapper. No backend was built or benchmarked here.
[Project's own status](https://github.com/llvm-z80/llvm-z80).

An LLVM-IR-only experiment is also possible for a closed arithmetic region,
without using an LLVM Z80 backend. However, importing its optimized CFG back
into our lowering is still new infrastructure. First implement the analogous
small local pass; consider LLVM/MLIR only if the accumulated analysis burden
justifies that bridge.

If exporting LLVM IR, do not attach `nsw`, `nuw`, `inbounds`, `noalias`, or
function memory attributes without proof. Guest wraparound and valid numeric
address zero must not become LLVM poison/undefined behavior. Volatile
preserves the count/order of volatile accesses but can move relative to
nonvolatile accesses; it is not by itself a complete NES MMIO/interrupt model.
Model mapper writes, PPU latches, controller shifts, DMA and interrupt-sensitive
publication as effects with the ordering they actually require.
[LLVM language semantics](https://llvm.org/docs/LangRef.html),
[undefined-behavior guide](https://llvm.org/docs/UndefinedBehavior.html).

## Evidence required before implementation is accepted

1. Freeze source/profile/ROM/core/route hashes and measure useful game updates,
   not repeated video callbacks. Use the existing actual-core source-span
   profiler, including inline flags, descendants, idle work, and IRQ service.
   Retain current benchmark contracts and numeric overclock settings.
   Whole-run `trace-sms` call counts can locate candidate edges, but its step
   allowance and idle/wait loops do not establish steady-state cycles/update.
2. Verify each admitted transformation through the normal lift/lower pipeline
   against `oracle_6502`: registers, live flags, S/return ownership, RAM and
   ordered observable effects. Enumerate finite small domains; add adversarial
   aliases, page/16-bit wrapping, CFG joins, early exits, and unsupported-form
   rejection. Random testing is evidence, not a universal proof.
3. Execute assembled output, not just emitted Rust stubs. Add IRQ injection
   at legal boundaries around any retained register, temporary or bank map;
   check IFF, shadow state, mapper restoration, and the `$DE40` native-stack
   floor. Synthetic MMC3 banking tests establish banking only, not a qualified
   IRQ or complete PPU oracle.
4. Run unchanged-clock A/B gameplay routes and moving-frame presentation
   checks. Shared changes retain SMB's three differential routes/checkpoint
   images and CV1 heart/projectile/indoor regressions. Report throughput and
   update-gap distributions, code size, and rejected candidates. A faster
   helper with unchanged gameplay throughput is not a delivered speedup.
5. Land one independently useful pass at a time. Recompute or invalidate
   affected analyses after rewriting; stale call/flag/alias facts are compiler
   bugs. Reuse existing reports/tests initially rather than building another
   profiling system. [LLVM analysis invalidation model](https://llvm.org/docs/NewPassManager.html).

Alive2 is useful precedent for validating optimization results, not an
off-the-shelf oracle for this IR or Z80 runtime. Its own README warns that
interprocedural transformations are unsupported; it cannot certify our
inliner or interrupts without additional modeling.
[Alive2 project](https://github.com/AliveToolkit/alive2).

First experiment after the SMB3 correctness work: census hot ordinary call
edges and the already-measured sprite producer with existing evidence, choose
one closed region, establish its live-state/effect contract, and compare
pointer/flag retention against a small semantic inline. Proceed only if the
complete region and actual gameplay measurements justify it. No 2× claim is
currently established for these proposed compiler passes.

## Follow-up: measured SMB3 costs after the first rendering improvement

The experimental MMC3 map now provides a second workload, distinct from CV1.
Reusing the existing actual-core profiler, a 200-complete-update window at
numeric `500` totals **338,119,350 nominal Z80 T-states**, or **1,690,596.75 per
map update**. Stock and instrumented cores agree across 7,000 physical-frame
RAM/state samples and their dense video hashes. Accounting closes without
unknown, external, or unclassified execution. This measures one map route,
not level traversal or stock-SMS performance.

| Exclusive execution category | Share | Nominal T-states/update |
| --- | ---: | ---: |
| Full memory routing and cartridge SRAM | 22.522% | 380,758 |
| Translated code, including inline lowering, excluding explicit waits | 16.840% | 284,692 |
| Shared calls, returns and banked dispatch | 16.919% | 286,037 |
| Renderer | 13.065% | 220,881 |
| Exact frozen-state capture/comparison | 12.391% | 209,480 |

The remaining roughly 18.26% includes PPU/DMA, original cooperative waits,
VBlank polling, VDP transfers, mapper register writes, audio and other runtime service. The translated category is
not pure guest logic: it includes the lowering recipes emitted inline.

This changes the immediate experiment ranking for MMC3. Investigate an
effective-address-checked internal-RAM fast path and the measured banked
dispatch search before adding a general inliner. Dispatch already has a
high-byte directory: the **page-local** linear search costs 13.317%, averaging
15.04 entries per successful lookup; other shared call/return work costs 3.602%.
Of the full-bus entries, 77.10% resolve to internal RAM, but the ledger does
not establish which call sites were compile-time constants. An `H < $20`
runtime guard could bypass general routing only after the effective address
is known, with a proposed budget of 64 fixed-bank bytes and no extra RAM.
Preserve exact address mirrors, mapper ownership, flags and interrupt
contracts; a profile-hot address is not proof of a constant address.
Capture alone has only roughly 12.4% of this workload to remove. These are
measured opportunities, not implemented optimizations or predicted gains.

Separately, retaining unchanged backgrounds already produced a measured
**2.22× map**, **1.75× idle-level**, and **1.18× active 1-1 traversal** throughput
gains. Both final active routes complete with identical guest/cart RAM at seven
matched checkpoints; the idle comparison ends at a then-unresolved target.
These results come from runtime rendering changes, not LLVM or inlining.
See [the scope and comparison details](mmc3-graphics-runtime.md).

The subsequent [32-byte checked-RAM experiment](mmc3-ram-fast-path.md) is now
implemented and measured: +9.75% map throughput but only +4.90% active traversal,
with unchanged game-state checkpoints and essentially unchanged rebuild
blanking. It is a runtime specialization, not an IR inliner. This reinforces
the requirement to profile the actual moving workload before choosing a pass.
Its later moving-level profile attributes 44.6% to rendering and 17.5% to
page-local dispatch, while bus routing is 7.5%; map cost shares are not a
substitute for that workload. Background invalidation is the next investigation.
