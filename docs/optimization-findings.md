# Optimizer investigation — findings & plan

Goal: make the translated SMB ROM run at (or near) 60 fps on the SMS.
Measured baseline: **~370K Z80 cycles per game frame vs a ~59,736-cycle
budget → ~6.2× too slow (~10 fps)**. (Cycle metric: `FD_MEASURE_NMI=1
cargo run -p nes_to_sms --bin frame-diff -- <smb.nes> out/smb/sms.sms
--frames 260 --script start_right`, which counts approx T-states from
IRQ-inject until the NMI stack unwinds.)

## Why it's slow (it's NOT the hardware)

The SMS Z80 (3.58 MHz) is ~2× the NES 6502 (1.79 MHz). The slowness is
**translation overhead**: each 6502 instruction expands to a bundle of
Z80 instructions because the Z80 doesn't natively match the 6502 model:

- 6502 status flags (N/V/Z/C) are kept in a **shadow-P byte in RAM**
  ($CB03); most ops call a helper (`rt_set_nz_a`, `rt_adc_a`, …) to update
  it, and every branch reads it via `ld hl,SHADOW_P; bit n,(hl)`.
- X/Y have no Z80 home → they lived in RAM (now `inc/dec (hl)`).
- the 6502 stack, bank switching, etc. add more.

SMB on the NES already uses nearly the whole NES frame budget, so the
multiplied work blows past the SMS budget.

## What we tried, and the measured result

All validated against the differential oracle (init RAM byte-identical to
the real 6502; first divergence unchanged at frame 22; Mario still walks
and jumps).

| Change | Kind | Effect |
|---|---|---|
| Native-flag fusion: CMP/LDA/AND/ORA/EOR + branch → `cp`/`or a`/`and` + native `jp` when flags dead after | peephole | `rt_cmp_a` −53%, `rt_set_nz_a` −20% |
| INX/INY/DEX/DEY → `ld hl,SHADOW; inc/dec (hl)` (+ loop-branch fusion) | peephole | **~7%** (biggest single) |
| Lean `rt_set_nz_a`; CLC/SEC/etc → `set/res n,(hl)` | helper leaning | a few % |
| **16-bit add idiom lifter** → native `add`/`adc` carry threading | semantic-equivalence | **correct but fires once** |

**Cumulative: ~370K → ~341K cycles/frame (~8%, ~1.08×).**

## The keystone finding

Both promising ideas — "shrink the bloat with optimizer passes" and "use
the Z80 differently but equivalently" (the 16-bit lift) — **are correct
and, when they fire, are big wins.** But they fire rarely, all for the
**same reason**:

> Every 6502 flag lives in the shadow-P byte, read by bit-tests in
> branches *everywhere*. So flag-liveness analysis sees flags as "live"
> across nearly every JSR/RTS/branch. Since a lift/fusion is only sound
> when the flags it skips are provably **dead** afterward, the
> conservative liveness blocks almost all of them.

The 16-bit lift requires N/Z/C/V all dead after; a 16-bit add sets all
four and C/V are seldom overwritten before the next boundary → blocked.

**The bloat is rooted in one decision: representing 6502 flags as a
memory byte read pervasively.** That single choice forces the helper
calls, the `bit n,(hl)` branches, AND blocks the idiom lifting.

## The plan — option 2: native flags as the default representation

Make Z80 **native** flags the working representation; materialize the
shadow-P byte only where genuinely needed (PHP/PLP, and routines that
return a flag as a result). Then the fusions + the 16-bit lifter we
already built fire *everywhere* instead of once. Realistic ceiling ~2–3×
(~25–30 fps) — true 60 fps is blocked by the SMS having almost no
performance headroom over the NES (a faithful translation can't be as
tight as hand-written native Z80).

Executed as validated steps (oracle after each):

1. **Interprocedural flag-liveness** (foundation, lower risk). Compute,
   per routine, which incoming flags it may read before writing
   (conservative). Use it to stop treating every JSR/JMP-to-a-non-reading
   routine as "flags live", so liveness can see past calls to where the
   flags are actually overwritten. Immediately unblocks the built
   fusions + 16-bit lift. **← starting here.**
2. **Native-flag branches**: branches read native Z80 flags derived at
   the point of use; flag producers leave native flags; shadow-P written
   only when a later reader truly needs it (per the liveness above).
3. **Shadow-P materialization points**: PHP/PLP and flag-returning
   routine boundaries emit an explicit pack/unpack; everywhere else drops
   the shadow byte.

Lower-risk reuse already in tree: the fusion machinery
(`emit_fused_branches`, `cmp_cond_to_z80`/`nz_cond_to_z80`) and the
`Add16Plan` lifter are written and tested — they just need liveness to
let them fire.

## Results so far (measured, cycles/frame, steady gameplay)

| After | cycles | note |
|---|---|---|
| fusion + lean rt_set_nz_a (cycle counting added here) | 370K | baseline for the cycle metric |
| INX/INY/DEX/DEY → inc/dec (hl) | 345K | ~7%, biggest single |
| CLC/SEC → set/res (hl) | 341K | ~1% |
| step 1: interprocedural flag-liveness | 337K | ~1% |
| step 2 core: native-flag N/Z branches (A-holds-NZ) | 336K | ~0.3% |

Plus the earlier instruction-measured fusion/leaning (~8% in instructions,
more in cycles). **Cumulative is roughly 10–15%, still ~5.5× over budget.**

### The hard lesson

The cost is *so diffuse* that each bounded, safe optimization moves the
needle ~0.3–7%, and the big semantic-equivalence lift (16-bit add) only
fires twice — gated by C/V flag-liveness, which is itself gated by the
shadow-P representation. Confirmed: **the bulk of the cost is the
producer side — `rt_set_nz_a`/`rt_adc_a`/etc. after nearly every op.**
The branch-side wins (fusion, branch-via-A) don't remove those, because a
producer can only skip its shadow update when the liveness proves *no*
reader needs shadow-P — and as long as some branches still read shadow-P
(non-fused, cross-block), the producers must keep writing it.

### What would actually move it (and the realistic ceiling)

The only thing that removes the producer cost wholesale: **make native
branches the default everywhere** so shadow-P is read essentially nowhere
(only PHP/PLP), letting the NZ-liveness mark almost all shadow updates
dead — then drop them. That means whole-block flag dataflow + materialize
shadow-P only at the rare true-read points. It's a large, multi-session
rewrite, and even fully done the realistic ceiling is ~2–3× (~25–30 fps),
not 60 — the SMS has too little headroom over the NES for a faithful
translation to ever hit native speed on a maxed-out game like SMB.

**Bottom line for decision-making:** the pipeline produces a correct,
playable-logic SMB 1-1 ROM; making it *full-speed* is not reachable by
this translation strategy. ~25–30 fps is the optimistic target after a
major flag-and-register rearchitecture; ~10–12 fps is where bounded
optimization lands.

## Update: we pushed the flag rearchitecture (option 1) and it's decided

Built interprocedural flag-liveness, native-flag N/Z branches
(`a_holds_nz` tracking → `or a; jp`), and native-flag-aware producer
liveness (`nz_shadow_live_after`). Net: **`rt_set_nz_a` calls dropped 30%
(159K → 110K)** — a real, validated codegen win (init byte-identical, walk
OK). **But steady-frame cycles moved ~0.5%.**

That is the decisive result. The flag machinery is the most-*called*
overhead but **not the dominant per-frame *cycle* cost.** Steady
gameplay — especially with scrolling, which runs the AreaParser's
incremental column loading every frame — is ~330–410K cycles dominated by
the sheer volume of work in heavy routines (area parsing, sprite/OAM
assembly, indexed memory), not by flags. Eliding even the #1 helper
wholesale barely registers.

**Conclusion:** the ~6× gap is irreducibly diffuse. No single optimization
(or even the whole flag rearchitecture) closes it; it's the cumulative
cost of faithfully executing every 6502 operation of a game that already
maxes out the NES, on a CPU only ~2× faster. Full speed would require not
translating but re-implementing — outside this project's generic-engine
design. The realistic state is a correct, deterministic NES→SMS translator
with a playable-but-slow (~6–10 fps) SMB 1-1. Further perf work has very
low ROI.

## Other Z80 advantages worth exploiting later (generic, game-agnostic)

- `ldir`/`lddr` for 6502 copy/fill loops (`lda $s,x; sta $d,x; dex; bne`).
- `add hl,de` / `sbc hl,de` for 16-bit math on consecutive byte pairs
  (vs the per-byte lift, when lo/hi are adjacent in RAM).
- 16-bit `inc/dec rr`, `ex de,hl`, the alternate register set (`exx`) for
  cheap scratch/state save in hot helpers.

## Invariants (must hold)

- Validate every change with the frame-diff oracle: init RAM
  byte-identical, first divergence no earlier than frame 22, walk+jump
  intact. The oracle validates *observable* equivalence — exactly the
  right check for "different procedure, same result" optimizations.
- Optimizations stay generic (idiom patterns, not "SMB does X at $addr").
