# Phase H — Predictable Z80 optimizer

Goal: close the frame-budget gap so the generated ROM plays at 100%
speed on a stock 3.58 MHz SMS (no 500% emulator overclock).

Measured starting point (2026-07-04, 200M-instruction PC profile over
the 1-1-clear route, `SMS_PC_PROFILE=1 trace-sms`):

- Frame budget: 59,736 Z80 cycles (228 × 262 lines). 99.4% of frames
  over budget, **average 9.75×, worst 65×**.
- Where the instructions go (top-40 symbols ≈ 78% of all execution):
  - **≥31% shadow-flag helpers** — `rt_set_nz_a` 7.4%, plus the
    `rt_cmp_a`/`rt_adc_a`/`rt_lsr_a`/`rt_sbc_a`/`rt_asl_a`/`rt_cpx_a`
    families and their `_no_[nzcv]` tails. This is the 6502 flag
    shadow ($CB03) being recomputed after nearly every ALU/load op.
  - **~14% software sprite pipeline** — `_dof_*` (OAM→SAT flip/expand
    loops), `rt_map_sprite_tile`, `do_sprite_variant`, `_vgs_alloc`.
    Pure runtime cost, per frame, independent of translated code.
  - **~13% memory dispatchers** — `rt_write_indexed`,
    `rt_read_indexed`, `rt_read_prg_high_indexed`, `rt_read_zp_ptr_y`
    and tails: every indexed access pays bounds classification at
    runtime.
  - **~4% call overhead** — `rt_far_call`, `rt_push6502`,
    `rt_pop6502`.
  - Translated straight-line code itself is a minority of execution.

The overhead is dominated by *our* machinery, not by SMB's logic —
exactly the right situation for an optimizer.

## LLVM verdict: not the tool

Researched 2026-07-04:

- LLVM has **no in-tree Z80 backend**. The principal out-of-tree
  backend ([jacobly0/llvm-z80](https://github.com/jacobly0/llvm-z80),
  moved into jacobly0/llvm-project) targets (e)Z80 for the TI-84 CE
  toolchain, tracks an old LLVM (~14), and is dormant (author moved to
  Zig). Community forks (llvm-z80 org, earl1k, grapereader) are
  experiments, not maintained production backends.
- Architectural mismatch: our program is not C-like IR — it is a 1:1
  6502→Z80 mapping with a byte-exact behavioral contract (shadow
  flags, NES RAM mirroring, hardware-window forwarding, exact write
  ordering that the differential oracle checks). Round-tripping
  through LLVM IR would discard that contract and re-derive semantics
  we must not change; LLVM's global optimizations are also explicitly
  NOT predictable, which the user asked for.
- Precedent for the chosen approach: SDCC's Z80 backend ships a
  rule-based **peephole optimizer** with liveness qualifiers
  ([SDCC z80 wiki](https://sourceforge.net/p/sdcc/wiki/z80/)), z88dk
  adds ~300 aggressive rules + `z88dk-copt` regex passes, and
  `mdlz80optimizer` does static Z80 source optimization. Rule-driven
  passes with dataflow-lite liveness are the industry approach for
  Z80 — and we have something none of them have: **three full-route
  byte-for-byte differential oracles to prove every rule**.

Decision: a clean, deterministic pass pipeline over our own lowered
Z80 stream (we own the emitter end to end), one pass = one written
contract, each individually toggleable, each landing only behind
green oracles.

## Pass list (ordered by measured yield)

**H.1 Native-flag branch fusion (attacks the ~31%).**
6502 `op` + conditional branch pairs lower today as
`<op> ; call rt_xxx_flags ; ld a,($CB03) ; and mask ; jp cond` — but
the Z80 ALU already computed N/Z/C natively for the same operation.
When the ONLY consumer of an instruction's flags is an immediately
following branch on N/Z/C (per-flag backward liveness over the
routine CFG, the `flags_live_after`/`scan_run` machinery extended to
per-flag granularity), emit the native-flag form:
`ld a,(x) ; or a ; jp z,...` for LDA+BEQ, `cp n ; jp c/nz/z` for
CMP+branches, native `srl/add` flag use for LSR/ASL/ADC pairs.
The shadow $CB03 update is skipped entirely on the fused path; V and
BIT-style consumers keep the helper. Contract: fusion fires only when
the branch is the sole flag consumer and the flag maps 1:1 to a
native Z80 flag (Z↔Z, N↔S, C↔C for adc/cmp-class with 6502 carry
polarity respected: 6502 CMP/SBC carry = !Z80 carry; ADC carry = Z80
carry).

**H.2 Indexed-access specialization (attacks the ~13%).**
`LDA/STA base,X/Y` with constant base where `base + $FF` provably
stays inside NES RAM ($0000-$06FF bases) lowers to direct
`ld hl,SHADOW+base ; add index ; ld a,(hl)` — no dispatcher, no
range checks. Bases in $0700-$07FF fall back to the dispatcher (page
crossing could leave RAM). Same for the PRG-ROM window when
`base ≥ $8000` and `base + $FF ≤ $BFFF` in the mapped low bank
(direct slot-2 read), and for `(zp),Y` pointer reads whose pointer
page is provably PRG-high (already partially specialized at runtime;
this moves the dispatch to compile time where the profile proves the
address).

**H.3 Sprite-pipeline runtime tuning (attacks the ~14%).**
Hand-optimize the `_dof_*` OAM→SAT loops in `runtime/*.s`: skip
rows whose OAM bytes are unchanged this frame (dirty-compare against
the staging copy already in RAM), table-drive the flip variants,
hoist bank selects out of the per-sprite loop. Pure runtime work; no
translated-code semantics involved.

**H.4 Call/stack overhead (attacks the ~4%).**
Same-bank `rt_far_call` → direct `call` (the fixed-point section
assigner already proves same-bank; extend the downgrade to the
remaining far calls). Inline `rt_push6502`/`rt_pop6502` (4-byte
bodies) at call sites.

**H.5 Peephole window (residual).**
Deterministic rule set over the emitted stream: redundant
`ld a,(x)` after `ld (x),a`; double shadow loads; dead `ld` into
overwritten registers; jump-to-next-instruction. SDCC-style rules
with explicit liveness qualifiers, each rule numbered and logged
(`--opt-log` prints rule firings per routine for auditability).

## Verification protocol (every pass, no exceptions)

1. `cargo test --workspace` green (375+).
2. Three-route differential parity, all NO DIVERGENCE:
   1-1-clear (4,900f), death-gameover (3,200f), bonus-pipe (3,400f).
3. Cycle re-measure: `SMS_PC_PROFILE=1` run prints the frame-budget
   line; the pass's measured win is recorded in this file.
4. Each pass toggleable (`NES2SMS_OPT=` env / profile flag) so any
   regression bisects to one pass in one run.

## Cycle accounting

trace-sms `frame_budget` line (implemented with the profiler) reports
budget utilization per frame using z80_emu's cycle counter:
`budget=59736 over_budget_frames=N avg=X worst=Y`. The Phase H exit
criterion: **avg ≤ 1.0× on all three routes** (every frame under
budget is the stretch goal; occasional over-budget frames are
tolerable — the IRQ-skip pacing already handles them gracefully).

## Measured progress

| Milestone | avg × budget | notes |
|-----------|--------------|-------|
| Baseline (pre-H) | 9.75× | worst 65× |
| H.1a boundary relaxation | 9.62× | hardware writes/PHA no longer flag boundaries; hardware reads kill NZ liveness; 77 static SET_NZ_A sites elided (PPU-streaming loops) |
| H.2 indexed specialization | 9.07× | direct add+access for provable RAM/PRG-low windows; rt_write_indexed sites 555→29, rt_read_indexed →67 |
| H.4a push/pop inline | (bundled) | PHA/PLA/PHP/PLP inline the 6502 stack ops; helper kept for RTI |
| H.1c table NZ + inline | 8.34× | 256-byte N/Z table pinned at $3E00; shadow-NZ update inlined at all emission sites (7 instructions, no call, no branch); rt_set_nz_a left the profile |
| H.3 variant persistence | (see next) | sprite variant pool persists across frames (CHR static on NROM); flush-on-full generation reset; _dof_*/do_sprite_variant left the profile, idle share 6.3%→12.2% |
