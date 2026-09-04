# Speed recovery plan (Phase S)

Goal: apply the lessons of docs/handport-comparison.md to the SMB conversion
and measure **how far automated conversion can go before manual tweaking**.
Each tier is progressively less automatic:

- Tier 1 = generic codegen/runtime changes, zero game knowledge → fully automatic.
- Tier 2 = per-game `[[replacement]]` profile entries + native runtime modules → manual but bounded and reusable as a pattern.
- Tier 3 = profile-annotated data relayout → annotation-driven, research-grade.

The deliverable is a measurement table: cycles/frame after every step, same
route, same metric, with the oracle green at every step.

## Measurement protocol (unchanged from optimizer phase)

- Metric: `FD_MEASURE_NMI=1 cargo run --release -p nes_to_sms --bin frame-diff --
  <smb.nes> out/smb/sms.sms --frames 260 --script start_right` — approx
  T-states from IRQ inject to NMI unwind; report steady-gameplay frames.
- Baseline (2026-07 optimizer phase end): **~330–410K cycles/frame**, ~5.5–6×
  over the 59,736 T budget. Re-measure at S0 from current HEAD.
- Correctness gates per step, in order of cost:
  1. `cargo test --workspace` green (includes differential single-op/slice tests).
  2. frame-diff parity route: init RAM byte-identical, first divergence frame
     unchanged from baseline, walk+jump intact (`start_right` route completes).
  3. On phase exit: all three acceptance routes (1-1 clear, death/game-over,
     bonus pipe) byte-parity as in docs/master-plan.md progress snapshot.
- Every optimization stays generic or profile-declared. No SMB constants in Rust.

## Phase S0 — Baseline re-measurement

1. Regenerate + assemble SMB from current HEAD (`out/smb-s0/`).
2. Record steady cycles/frame, plus (if cheap to add) a helper-call histogram
   (`rt_read_indexed`, `rt_write_indexed` equivalents, `rt_ppu_write`,
   call-gate entries per frame) to rank targets and later verify each step
   removed what it claimed.

## Phase S1 — Tier 1: generic codegen and runtime (fully automatic)

Ordered by expected ROI; measure after each.

### S1.1 Inline indexed addressing, single EA computation
- `LDA/LDX/LDY abs,X` style reads: emit inline `ld hl,base / ld c,<idxreg> /
  ld b,0 / add hl,bc / ld a,(hl)` — no helper call (saves call+ret 27 T and
  register shuffling; X/Y are already register-resident in D/E).
- Indexed stores: stop recomputing the EA and stop the `push af/pop af`
  save — compute EA into HL first using B/C only, then `ld (hl),a`.
- Read-modify-write (`DEC/INC/ASL/ROR abs,X` and zp,X): compute EA once, then
  `dec (hl)` / `rlc-class ops on (hl)` inline; drop `rt_dec_mem`-style helpers
  where the native flag result is compatible with the existing
  liveness machinery.
- Applies equally to `(zp),Y`: `ld l,(zp) / ld h,(zp+1)`-from-mirror + `add hl,de`-
  style sequence inline.
- Expected: largest single Tier-1 win; indexed access is the measured hot class.

### S1.2 Native CALL/RET under profile `stack_discipline = "native"`
- New profile assertion (default stays "software" — fail-safe): the game's
  JSR/RTS pairing is strictly LIFO and no routine consumes return addresses
  outside declared `[[jump_engine]]` / `[[return_escape]]` sites (SMB
  qualifies; the oracle + acceptance routes verify empirically).
- Same-bank call: plain `call L_target` (banks are assigned at emission time,
  so same-bank is statically known). RTS: plain `ret`.
- Cross-bank call: slot-0 far-call shim — save current $FFFE bank on native
  stack, switch, `call` target; callee's `ret` lands back in the shim (slot 0
  is always mapped), restore bank, `ret` to site. ~100 T vs today's ~450 T.
  A must be preserved (it is live across JSR); pass target via BC + bank in
  a spare byte or immediates, per current far-gate conventions.
- PHA/PLA/PHP/PLP keep using the emulated 6502 stack page (unchanged).
- Interrupt handler already saves/restores banking state; verify, don't assume.
- Bank assignment: pack routines to maximize same-bank call locality (simple
  greedy by call-graph edge weight is enough; address-order is the fallback).
- Risk: highest of Tier 1. Gate on the full acceptance-route parity, not just
  the smoke route.

### S1.3 Cheap NROM hardware-write paths
- Measure first (S0 histogram): per-frame counts of `rt_ppu_write` by register
  and `rt_apu_write`.
- Replace the IFF2-snapshot (`ld a,i / di / jp po,...`) preamble with plain
  `di … ei` where the enclosing context provably has interrupts enabled, or
  restructure so translated code never touches VDP ports directly outside the
  IRQ (single-writer), making guards unnecessary on the RAM-shadow-only paths.
- Specialize the hot registers: $2007 data writes (buffer append) and
  $2005/$2006 latches should be short straight-line inline sequences; keep the
  general helper for the cold registers.

### S1.4 Build-time sprite flip variants (assets crate)
- Emit all needed flip combinations of sprite CHR tiles as an additional ROM
  data blob at conversion time (H, V, HV for all sprite tiles; 3× data_chr
  sprite portion — ROM is free at our sizes).
- `rt_sat_resolve` variant path becomes: pick precomputed tile address,
  `OTIR`/unrolled-`OUTI` 32 bytes from ROM to the VRAM scratch slot. Delete
  the runtime bit-mirroring loops. Cache/keying logic unchanged.
- Palette-bit variants (sub-palette selection) can keep the existing CRAM
  strategy; only flips move to build time in this step.

### S1.5 Unrolled OUTI upload paths (runtime, generic)
- SAT upload and vbuf flush: page-aligned unrolled OUTI blocks entered at a
  computed offset (`JP (IX)`/`JP (HL)`), replacing per-byte loops. 16 T/byte
  transfer floor; no loop overhead for variable lengths.
- Applies to the CHR-RAM streaming path (chrmap.s) as well if the histogram
  shows it hot.

### S1 exit criteria
- All acceptance routes parity-green; workspace tests green.
- Measured cycles/frame recorded per step in this file's results table.

## Phase S2 — Tier 2: profile-driven native replacements (SMB profile)

The `[[replacement]]` mechanism already exists (crates/profile). The hand
port (docs/handport-comparison.md §4) is the reference for what native
presentation looks like. Candidates, in order:

1. **VRAM stripe flush** (SMB `UpdateScreen`/`WriteBufferToScreen` chain,
   called from the translated NMI): replace with a native runtime routine
   that reads `VRAM_Buffer1` from the RAM mirror ($C301...) and decodes the
   NES stripe format (addr-hi, addr-lo, len|flags, data...) directly into
   VDP writes through the existing nametable-mapping layer — one guard for
   the whole flush instead of per-byte `rt_ppu_write` calls.
2. **Scroll application** ($2005 pair from the translated NMI): native
   replacement writing the SMS scroll registers via the existing split-scroll
   state, skipping the PPU-latch emulation on the hot path.
3. Anything else the S0/S2 histogram shows as presentation-shaped translated
   code (candidates: `MoveSpritesOffscreen`, `InitScroll`).

Rules: replacements live in `runtime/*.s` + `profiles/smb.toml` only; each
must be observably equivalent (frame-diff parity on all routes). Record in
the results table how many replacements were needed — that number is the
honest answer to "how far does automation go."

## Phase S3 — Tier 3: data-relayout experiment (annotation-driven)

**S3.1 DONE (2026-09-04):** the closure-analysis pass now runs on every
build and writes `reports/relayout.txt`. SMB findings: 138 indexed RAM
bases, 89 with const-clean spans. The aliased spans follow exactly the
pattern the hand port solved by construction — bare-const accesses are
the *player's* slot (e.g. `$0086` Player_X ×28) while `,X`/`,Y` index the
enemy slots — so const aliases are relocatable, not blockers. The actual
blockers are three RAM-target zero-page scratch pointers (`$00`, `$04`,
`$06` — the block-buffer pointer family, 21 dynamic-deref sites): the
transposition is sound iff their value ranges provably stay inside
non-relayouted regions (the block buffers). That is a bounded,
SMB-auditable proof, not open-ended research — the next Tier-3 step is a
pointer-target audit of those 21 sites (`$E7`/`$E9`/`$F5` point at
ROM area/enemy data and are irrelevant to RAM relayout).

Two sub-experiments; either may terminate in a feasibility report:

1. **Closure analysis pass** for a profile-declared `[[ram_array]]` region
   (base addresses, stride, index register): verify every access to the
   region across the whole program is `base,X`-shaped with a declared base
   (or a declared direct exception). Emit `reports/relayout.txt` listing
   violations. This is pure analysis — no codegen risk.
2. If closure holds for a candidate family (best first candidate: the enemy
   slot arrays), implement transposed lowering: region moved to dedicated
   page-per-slot layout, `base,X` access lowered to `ld h,page_base+idx? /
   ld l,<field>` form. All translated accesses go through the rewrite; the
   RAM mirror contract for that region changes, so the oracle's RAM layout
   mapping must learn the transposition too.
- This phase is allowed to conclude "not worth it" with numbers.

## Results table (2026-09-04 session)

Metric: `FD_MEASURE_NMI=1 frame-diff … --frames 260 --script start_right`,
steady_avg (last third of frames). Parity = init RAM byte-identical + no
divergence across the route. All steps also hold parity on the
death-gameover (500f) and bonus-pipe (700f) routes and pass the 1-1-clear
trace-sms route with RAM expectations at session end.

| Step | Build | Cycles/frame | vs 59,736 budget | Parity | Notes |
|------|-------|-------------|------------------|--------|-------|
| S0 baseline (HEAD e315972) | out/smb-s0 | 261,614 | 4.38× | ✓ | July optimizer work already in tree |
| S1.1a indexed window → $C7FF | out/smb-s1 | 258,476 | 4.33× | ✓ | killed remaining rt_read/write_indexed |
| S1.2 native CALL/RET | out/smb-s2 | 222,864 | 3.73× | ✓ | `stack_discipline = "native"` + rt_far_tail |
| S1.3a shift-run lifting | out/smb-s3 | 217,545 | 3.64× | ✓ | carry-threaded ASL/LSR/ROL/ROR A runs |
| S1.3b mem-shift inline + EA fix | out/smb-s4 | 210,465 | 3.52× | ✓ | CB (hl) forms; indexed RMW EA w/o push af |
| S1.3c fill-loop lifting | out/smb-s5 | 204,262 | 3.42× | ✓ | MoveSpritesOffscreen + 1 more site |
| S2 hooks: TopScore + Shuffler | out/smb-s6 | 189,613 | 3.17× | ✓ | first `[[replacement]]` pair |
| S2 hook: ReadJoypads | out/smb-s7 | 180,436 | 3.02× | ✓ | serial bit loop → latch bit-reverse |
| S1.5 SAT resolve loop | out/smb-s8 | 178,928 | 3.00× | ✓ | page-walk hidden-sprite fast path |
| S1.6 PRG-high read de-guard | out/smb-s9/s11 | 177,080 | 2.96× | ✓ | irq_handler already saves/restores $FFFF, so the DI/IFF dance and the rt_restore_prg_window calls on NROM fixed-high reads were vestigial |

Negative result (reverted): static call-graph section clustering
(union-find on `external_calls` edges) measured 181.1K — *worse* than
address order, which already encodes SMB's locality. Bank packing needs
dynamic call-pair frequencies (PGO via FD_PROFILE), not static edges.

| S2 hook: BlockBufferCollision (+dispatch/JMP/jump-engine replacement edges) | out/smb-s12 | 169,387 | 2.84× | ✓ | `$E3F0`+`$9BE1` native; replacements now intercept every transfer kind |
| S2 hook: attribute-table renderer | out/smb-s13 | 158,266 | 2.65× | ✓ | `$88AE` whole-routine (reached via computed dispatch); one shift-count bug caught by the oracle |
| S2 hooks: X/Y offscreen bits | out/smb-s14 | 150,650 | 2.52× | ✓ | `$F1F6`/`$F239`/`$F26D`; code now fits 8 banks (was 12) |

| Sound stage 1: SoundEngine native shell | out/smb-s15 | 149,720 | 2.51× | ✓ | idle-guarded SFX handlers, delegated event paths |
| Sound stage 2: native music tick | out/smb-s16 | 149,194 | 2.50× | ✓ | steady tick native; note fetches delegate at channel labels |
| PPU: plain-call writes (native mode) + hot-first dispatch | out/smb-s18 | 145,880 | 2.44× | ✓ | stackless-cont scheme moot under native calls; $2007/$2006 tested first |

| PPU: rt_ppudata_apply factored + native stripe flush | out/smb-s19 | 136,475 | 2.28× | ✓ | $2007 apply core is now a subroutine; rt_smb_flush_vram_buffer drives it per byte with one normalized source pointer and a local address step — per-byte guard entry, register dispatch, PPUADDR shadow traffic, and the (zp),Y helper are gone from the flush |

**Net: −47.8% (4.38× → 2.28× over budget).**

Sound-engine measurement note: stubbing SoundEngine entirely measured
only ~5.8K cycles/frame — the earlier "sound ~17%" symbol-class estimate
had misattributed the F-range offscreen-bits routines. After the two
sound stages, the engine's remaining profile footprint is sub-1%
diffuse; what remains of the stub delta is the APU→PSG shim work and
active-SFX processing, which are real work, not overhead. The remaining
big-ticket item is the folded-BG/variant machinery inside the $2007
tile-write path (rt_write_mapped_bg_tile + _bgv_*: map lookup, base
shadow, sub-palette resolve, variant pool — ~250-400 T per tile write,
~5-6K/frame): that is a rendering-model redesign, not a pass. Full-speed gameplay now needs
a ~300% emulator overclock (GPGX exposes up to 500%), vs ~450%+ at S0 and
~600-700% at the July baseline (330-410K).

Automation boundary observed so far: S1.* are fully generic (fire on any
NROM game); S2 stands at **seven** hand-written replacement routines
(~500 lines of Z80 in runtime/hooks_smb.s) plus two profile assertions
(`stack_discipline`, `runtime_defines`). Each hook is an exact
transliteration whose RAM/flag/register exit contract is documented at
the definition and enforced by the byte-parity oracle — one real bug
across all seven was caught at frame 7 of the first route run.

## Remaining cost classes (S8 profile, steady frame ≈ 178.9K)

| Class | Share | Path forward |
|-------|-------|--------------|
| Translated sound engine + music-stream reads (`func_E3F0`, `L_F1xx`, `ProcLoopCommand`, `rt_read_zp_ptr_y`, `rt_restore_prg_window`) | ~17% | Tier-2 native PSG driver reading the same data (the hand port's SoundDriver is the reference); largest coherent win left |
| PPU-write emulation (`_rt_ppu_write_body`, `_ppudata_*`, `_bgw_*`, `_chrmap_attr_*`, guards) | ~13% | run-mode batching: cache route/window/SRAM mapping across sequential $2007 writes after a $2006 pair |
| Area parser translated bodies (`L_88D0`, `SetAttrib`, `StoreMT`) | ~8% | S1-class codegen depth or Tier-3 relayout |
| SAT/OAM (`_oam_dma_aligned` LDIR is floor; `_res_*`, `_sat_*`) | ~5% | S1.4 build-time pre-flip variants still open |
| Far-shim transfers | ~4% | call-graph-aware bank packing (3-pass layout) |
| Rest: diffuse translated game logic | ~50% | Tier-3 data relayout is the only structural lever |

## Non-goals

- 60 fps on real hardware. The residual after S2 is translated game-logic
  expression; docs/handport-comparison.md explains why that last ~2× needs
  Tier 3 or manual porting.
- Audio timing fidelity, CV1/mapper work (unchanged by this phase).
