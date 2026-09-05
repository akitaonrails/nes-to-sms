# Castlevania 1 Mapper-2 Completion Record

This document records the completed mapper-2 recovery and the reproducible
acceptance floor. It supersedes the old CV1 blocker descriptions in
`HANDOFF-cv1.md` and historical deep-work notes where they conflict.

## Scope and canonical input

The target is Castlevania (USA) (Rev 1), NES 2.0 mapper 2/submapper 2, with
eight 16 KiB PRG banks, CHR-RAM, vertical mirroring, and AND bus conflicts.
Use the gitignored `.roms/cv1.nes`; `profiles/cv1.toml` binds its PRG/CHR
payload SHA-256. Do not derive profile facts from a divergent SMS trace.

The present acceptance bar follows the project owner's SMB precedent: boot,
Start, recognizable first-stage rendering, responsive gameplay input, and a
trap-free soak. It is not a pixel-perfect, real-time, or full-game claim.

## Latest performance checkpoint (2026-09-05)

The coherent background/HUD build reaches **19.26 walking / 21.53 heart-route
game updates/s at 500%**, completing pickup and another 301 ticks. It improves
slightly over the previously installed handoff build (19.08/20.97), but remains
slower than the fastest intermediate (21.90/24.77), which retained background
and HUD faults. Maximum physical-frame gap is 17. Do not describe this as
full speed or omit the coherence cost. See
[the matched measurements](cv1-performance-reassessment.md#coherent-background-and-hud-delivery).

Background, palette, HUD and SAT now share a bounded publication contract.
Continuous fixed-HUD checks have zero outliers on both routes. Bounded audio
service prevents the IRQ's sequencer work from running uninterrupted across
the HUD deadline. Generic IRQ spill preservation and controller serial-index
ownership are tested with actual assembled code. Fresh SMB output remains
identical to the artifact that passed the full RAM/VDP/clear regression suite.

The consumed-return escape separately repairs the old deep-route stack
imbalance. First-door/indoor acceptance is distinct from whole-stage support;
the historical full-traversal claim below is not current acceptance.
Fixed-instruction input scripts may reach different states after scheduling
changes; use game-tick anchored core routes for before/after acceptance.

The longer indoor route exposed a second return-stack issue: validating
guest return bytes after `PLA; PLA` reads memory an interrupt may legally
reuse. The original 6502 path, with its real lag NMI injected before and
after each pop, confirms that reuse is valid. The profile's `consume_at`
boundary now validates live bytes and retires the owning software return
before the first `PLA`; the original guest instructions remain unchanged.
The final tail jump does not discard the frame again. Stale sequences,
missing hooks and known entries that bypass the guard fail generation.

Final real-core acceptance passes the unchanged first-door and indoor route:
1,306 indoor ticks and at least 674 pixels of confirmed movement without a
trap. The paired diagnostic observes the repaired ownership transfer and
another 124 ticks/80 pixels afterward. Both 420-tick walking/heart callback
logs remain byte-identical to the coherent rendering checkpoint, including
pixel hashes and cadence. Reproduce these checks using
[the acceptance guide](../profiles/cv1/acceptance/README.md).

The subsequent dagger fix adds the verified shared initializer `$DB8D` to
the CV1 profile: the weapon dispatcher branches there from `$DBA4`, but an
external conditional branch alone does not create a discovery root. The old
build trapped `$E1` immediately after spending a heart. The corrected build
passes single and repeated/airborne throws, then completes 1,306 indoor ticks.
This profile-only change retains the renderer and stack repairs. Its updated
420-tick measurements above supersede the preceding byte-identical checkpoint;
SMB output remains byte-identical.

## Completed course correction

The earlier work stalled because mapper banks were treated as interchangeable
32 KiB views and many inline `$CA6D` dispatch tables were decoded as code.
The completed implementation now provides:

- a generic eight-bank UxROM policy with bus-conflict-effective writes;
- independent fixed-window and physical `(bank, PC)` analysis;
- bank-qualified calls, live-bank tail dispatch, and mapper-aware raw PRG reads;
- profile-driven inline JumpEngine tables, including stack-aware returns;
- profile-driven `[[return_escape]]` edges for 6502 callees that discard a
  JSR return address as stack data before returning through an older caller;
- executable targets that deliberately overlap a pointer-table suffix;
- fixed-point roots for branch continuations stranded by embedded routines;
- a compact 512 KiB SMS layout with 17 translated-code banks and all eight raw
  PRG banks preserved;
- fail-closed unsupported mapper stores and unresolved dispatches.
- generic STX/STY OAM-DMA lowering and OAMADDR-aware DMA wrapping;
- shared NES `$2005`/`$2006` latch semantics;
- CHR-RAM nametable rematerialization across render-off screen builds;
- scene-scoped CHR-RAM background variants: complete screen rebuilds reclaim
  stale slots before applying the new screen's attribute palettes;
- CHR-RAM 8x16 sprite-pair generation with per-sprite table selection,
  palette selection, and horizontal/vertical flips;
- accumulator-safe mapped background writes: the mapped-tile helper formerly
  stored a 16-bit VDP address at `$CB17`, whose high byte overwrote
  `rt_ppu_write`'s saved accumulator at `$CB18`. Castlevania's RLE decoder
  therefore wrote one correct zero followed by mapped slot numbers (`$37`,
  `$38`, `$3A`...) into CIRAM. The helper now parks the address bytes
  separately and preserves 6502 `A` across `STA $2007` continuations.

The executable synthetic UxROM fixture selects and dispatches all eight banks.
Its bus-conflict and unsupported-store tests are part of
`crates/cli/tests/synthetic_pipeline.rs`.

## Historical canonical result

This section records the pre-coherent-renderer checkpoint. Its fixed-step
trace endpoints and ROM hash are historical, not the current acceptance
commands; use [the current routes](../profiles/cv1/acceptance/README.md).

Generation reports 416 discovered fixed-view functions, 898 lifted routines,
zero lift failures, one lower failure, and 80 strict unresolved traps. The one
lower failure is bank 5 `$BD8A`: an unreachable JumpEngine sentinel pointing
into an all-`$FF` filler region. The canonical NES reference does not execute it
across the 3,600-frame Start/move/jump/whip route, so it correctly remains a
fail-closed trap rather than being translated as bogus code.

The old 115,000,000-step smoke stopped during Stage 1 setup at system
state/substate `$05/$01`; it proved only boot and no-trap behavior. The current
180,000,000-step route reaches gameplay `$05/$06`, holds Right after frame 1850,
and records CV1's held-input byte `$00F7=01` without a trap. Mednafen cold-boots
to the title and accepts Pause/Start. Genesis Plus GX at 500% Z80 overclock now
shows a clean sword/logo title and coherent entrance scene instead of the
former repeated-pattern tile field.
The trace framebuffer reaches a coherent, recognizable Stage 1 playfield with
8x16 sprites. The NES oracle reports 234 live `(tile, subpalette)` pairs on the
dense title and 54 in Stage 1, both within the 256-slot SMS background table.
Reclaiming the allocator on complete screen rebuilds therefore preserves the
NES attribute palettes without recycling visible patterns. The Stage 1 trace
now matches the reference's blue sky, green canopy, dark lower backdrop, and
gray fence/ground layout. The canonical ROM SHA-256 after the accumulator,
candle-route, measured performance, and banked-dispatch audio fixes was
`676803e7f7698f4e209619e81b01d9af30e5b66a2315c97c9fd173a0924b0220`
(superseded by the rendering fixes below).
Stock-timing emulation remains slow.

## Performance hardening and candle-route repair

The July 21 profile found linear mapper dispatch, not rendering, as the largest
avoidable CPU cost: `_bd_loop` accounted for 24.11% of 180 million sampled
instructions while scanning as many as 897 records. Generated projects now
sort dispatch records by NES address, preserve fixed-bank precedence at
duplicate addresses, and emit a 128-entry high-byte page directory. The same
route attributes about 0.80% to `_bd_loop`; its Stage 1 completion upload moves
from synthetic IRQ frame 1904 to 1370.

A second profile-driven pass removed mapper state churn from fixed-bank reads:
slot 2 keeps the selected UxROM bank while the helper maps fixed PRG into slot
1 for the duration of the read, then restores the translated-code bank. The
transaction remains interrupt-atomic and leaves SRAM and the UxROM shadow
untouched. Sprite horizontal flips now use a pinned 256-byte bit-reversal table
at `$3D00` instead of per-plane permutations.

The reproducible benchmark stops after the same 2,200 translated game frames
on the heart route. Against commit `842c086`, sampled instructions fall from
143,713,111 to 143,282,703 (-0.30%) and Z80 cycles from 1,123,618,391 to
1,114,904,425 (-0.78%). More importantly for missed-vblank work, frame-handler
median falls from 100,181 to 92,181 cycles (-8.0%) and average from 119,800 to
114,537 (-4.4%). Both runs collect the heart, remain in gameplay, and trap
nowhere. Host wall time is intentionally excluded because it varied between
runs. A direct-address lowering experiment was rejected after it raised the
same route's total cycles by 0.16% despite executing fewer instructions.

A repeated Right/whip route isolated the reported broken-candle hang in two
steps. The first failure was a valid fixed-bank branch from `$DE32` to `$DE04`;
the CV1 profile roots that statically proven shared tail, removing strict stub
`$0040`. The remaining `$0000` RTI failure came from `$E7D0 -> $EC60`:
`$EC60` deliberately executes `PLA; PLA` to discard the return bytes of
`$EA77: JSR $E4D9`, then returns through the caller below it. Translated calls
store that frame in `TR_RET`, not on `$0100+S`, so the PLAs consumed a live NMI
frame and raised S by two.

The generic `[[return_escape]]` annotation now identifies that tail edge. Its
runtime bridge discards one translated continuation, recreates the original
6502 JSR bytes (`$EA79`, high then low) on the emulated stack, and fail-closes
with marker `$E5` if no valid frame exists. The separate `$E959` JumpEngine
route into `$EC60` remains unchanged because it already arranges real emulated
stack bytes. The canonical route crosses the former failure at step
105,468,688, reaches it again at step 127,976,128, and completes a
180,000,000-step soak with no trap. S is balanced at `$F8` after the event, and
the near-event framebuffer remains in Stage 1 rather than blinking white and
stalling. The focused route also collects the large heart: the heart counter at
`$0071` finishes at `$0A` (the initial `$05` plus five) while system state
`$0018=$05`, substate `$0019=$06` confirms gameplay continues.

## Audio: banked-dispatch accumulator fix

CV1's sound driver (bank 0: trigger entry `$8187`, per-frame driver `$838A`
called from both NMI paths, channel streams at `$879F`+) was already rooted
and translated; the silence was a runtime bug, not a discovery gap. Every
call into a switchable-window stub flows through `rt_translated_call_gate`,
which restores the 6502 accumulator from the pushed call frame, and then
`rt_banked_dispatch`, which clobbers A during the (bank, addr) lookup while
`rt_far_gate` re-reads the caller A from `$CB15` — a slot the dispatch path
never wrote. Banked callees therefore entered with a stale accumulator.
`$8187`'s first instruction (`STA $E5`) stores the sound index, so
`PlayMusic` always triggered sound `$00` and no channel ever activated.
`rt_banked_dispatch` now parks entry A into `$CB15` on entry
(`runtime/dispatch.s`). The validation harness does not model the banked
dispatch (indirect-dispatch routines are skipped), so
`crates/validation/src/runtime_stubs.rs` needs no parallel change.

With the fix, the canonical route matches the NES reference's audio
trajectory: `PlayMusic($55)` (all-stop, boot), `PlayMusic($27)` when
Stage 1 starts, then `PlayMusic($55)`/`PlayMusic($2A)`. APU shadow writes
land in `$CB30-$CB47` and `apu_frame_tick` drives the PSG: the acceptance
route logs 1,989 port-$7F writes (previously 8, init-only), with all three
tone channels sweeping ~30 distinct periods each and noise-rate changes.
`trace-sms` gained `SMS_LOG_PSG=<path>` to stream every PSG write for
offline pitch-trajectory analysis (audio plan F.5). Generation stats are
unchanged (416 functions, 898 lifted, 80 strict unresolved traps): the
canonical routes reach no new stubs, so none were resolved.

## Rendering: scroll presentation, sprite-0 phasing, and beacons

Three user-visible defects shared one presentation path. The scroll
presentation could read a torn or misclassified `$2005` pair:

- The per-frame flag clear (`$CB20`) also dropped bit2, so a frame whose
  game scroll sequence had not completed fell back to the live latch —
  which mid-sequence holds the status-bar value (0). CV1's timed split
  routine (`$F868` status pair, sprite-0 wait loops, `$F8EE` playfield
  pair) leaves that window open for ~14K steps per game frame and for
  ~564K steps across its screen-boundary transitions: presentation then
  showed scroll 0 for up to 9 consecutive frames and
  `rt_nt_project_scroll` read the 32-to-0 jump as a teleport, firing a
  full 896-cell re-materialization (~520K-step handler bursts) on both
  the drop and the snap back.
- Worse, CV1's NMI ack read of `$2002` (`$C058`) consumed the synthetic
  sprite-0 arm every frame, so the STATUS pair itself captured as
  post-split and even a persistent post pair would have presented 0.
- The `NO_SCROLL_SPLIT` profile path presented the pre/status pair
  whole-frame whenever it ran; with no split band on screen the pre pair
  has no meaning.

Fixes (all generic, shared with SMB): `$CB20` bit2 is now sticky, making
`$CB23/$CB24` the persistent last-post playfield pair presented whenever
no fresh pair exists; the sprite-0 machine walks a three-phase
stale -> clear -> hit sequence per frame (the ack read observes the stale
hit, as on NES, instead of consuming the arm), implemented identically in
`runtime/ppu.s` and `crates/lower/src/lib.rs`'s inline status read; the
`NO_SCROLL_SPLIT` branch presents the playfield pair like the direct
path; and the production boot beacons (which wrote CRAM index 17 and
pointed the reg-7 backdrop at it on every deferred reg-1 apply — the
white full-screen blinks on display-off frames, since the SMS fills a
disabled display with the backdrop) are removed, leaving only halt-path
and DIAG_WILDJUMP diagnostic uses.

Measured on the 180M-step Right-hold route (3,000 frame dumps): before,
21 frames presented scroll 0 mid-walk (two plateaus at f2918-f2928 and
f2989-f2998) with three teleport presentations and five ~500K-step
glitch-driven handler bursts; after, zero torn presentations, zero
teleport jumps, and the four remaining ~500K frames are the game's own
`$2007` transition uploads (projection delta 0). CRAM writes dropped
2,826 -> 192 (the eliminated beacon traffic). Fixed-work benchmark:
1,117,808,472 cycles over 2,200 frames (handler p50 92,045, avg 112,979)
versus 1,114,904,425 (p50 92,181, avg 114,537) before — neutral, as
expected: the bursts were few, the win is correctness. The canonical ROM
SHA-256 after the scroll-presentation, sprite-0 phase, and beacon fixes
is `2fb3780aa38f601dbff48fb798b335c51609b9800d8a2af2efa11bc3db2ecdcd`.

## Deep Stage-1 traversal

Current-baseline correction (2026-09-05, `2123ef8`): replaying the documented
570M-step route traps with marker `$E2`, target `$0000`, at step 441,957,549
in NES bank 6. This predates the frame-handoff work. The historical success
below is not a current regression pass; retain the strict trap check while
investigating. Shorter stage/heart acceptance still passes.

The long traversal route (`profiles/cv1/acceptance/stage1-traverse-v2.sms.buttons`,
Start at frame 600, hold Right with periodic jump/whip pulses plus Up-hold
windows at the page-13 stairs) runs the full 570M-step budget (9,495 frame
dumps) with **zero runtime traps**. Depth evidence: 42 latchX wraps, Simon
reaches player page `$006D=09`/`ppos=0A` deep in the castle interior, the
courtyard and door transition render correctly, and the final frames show the
Stage-1 boss room (HUD "ENEMY" bar present, boss fight underway). Frame scan
over all 9,495 dumps: (a) 5 presented-scroll backward jumps, all in the known
benign `$2007` upload-stream family; (b) 0 near-white frames; (c) 111
double-image HUD candidates, all lag-24 r≈0.76-0.78 Simon-sprite overlap
false positives; (d) 334 handler frames >400K steps, all ~500K-step legitimate
`$2007` transition uploads. HUD digit strings show residual tile corruption
("SCORE-222323") late in the route — cosmetic, not one of the gating
signatures.

Two translation defects were found and fixed on this route:

1. **`$2525` NMI-epilogue trap (step 419,116,949).** The `$F3DA` jump engine
   (`JSR $CA6D` with a 32-entry inline table) pushes a `$F3/$7C` return pair
   the dispatched handler's RTS pops to resume at `$F37D`, but its profile
   annotation lacked `return_target`, so every case arm lowered to a bare
   tail dispatch. Handlers ending in a plain RTS unwound the *caller's*
   translated-return frame instead: the loop was truncated and the emulated
   pair leaked, leaving the 6502 stack two bytes short so the next NMI
   epilogue RTI popped a misaligned frame (`PC=$2525`, marker `$E2`).
   Chains that happened to pop a bit-6 frame masked the leak (balanced stack,
   wrong control flow), which is why the defect hid until a rare selector
   fired. Fix: `return_target = "L_F37D"` + `stack_return_bytes = 2` on the
   engine (no `tail_indices` — the ROM has zero `rt_rts_dispatch` sites and
   no reachable return-escape, so no chain self-consumes the pair), plus a
   new generic `translated_banked_call_with_continuation` emitter in
   `z80_emit` used by `emit_jump_engine_target` when a stack-aware
   continuation targets an unqualified switchable-window address (mapper 2).
   The fast repro for this class: an idle title screen traps the same way at
   step ~100.5M (selector `$14` → banked `$8867`).
2. **`$97EB` unresolved-dispatch trap (step 481,237,131, marker `$E2`).**
   Bank-6 `$97EB` (a per-frame poll routine the reference executes from frame
   3400 on) was never discovered, so the intra-bank `BCS $97EB` at `$9841`
   lowered to a fail-closed stub. Fixed with a reference-validated
   `[[bank_entry]] bank = 6, addr = 0x97eb` (FD_TRACE_PC oracle), which lifts
   the routine; unresolved labels drop 80 → 79. Nine other referenced-but-
   unlifted banked stubs remain latent (bank 5: `$8057/$80EE/$81D4/$8A72`,
   bank 6: `$81F7/$824D/$971D/$9ECB`); the reference did not execute them on
   this route, so by policy they stay fail-closed rather than being added
   from subject-side trap evidence.

Known remaining issue, off the acceptance route: an idle title screen (no
Start) still traps at step 100,655,295 with `unresolved_id=$0043`
(marker `$E1`, nes_bank=1) — the attract-demo path diverges from the
reference around frame ~1266 (reference spawns demo objects, subject does
not). The acceptance routes press Start at frame 600 and never hit it.

The canonical ROM SHA-256 after the traversal fixes is
`d7492a91cba647914aab5a27b4ccb7fb0b2b72f2dac8efa4cb253d99897a5a84`.

## Sprite palette: 8x16 pair vs. 8x8 copy-through VRAM collision

The NES ground-truth frame oracle (`FD_NES_DUMP`, docs/visual-parity.md) run
against CV1 exposed a sprite-palette corruption invisible to RAM parity: the
Stage-1 medusa/enemy sprites near the HUD rendered as a garish yellow/white
blob instead of the NES's small pink shapes. Diagnosis (all from the in-repo
oracle, no external emulator):

- The SMS OAM staging (`$C900`) carried the correct NES sub-palette attrs
  (pal 3, `attr & $03`), so the game logic and OAM DMA were right.
- The baked VRAM pattern for the affected pair slots had planes 2/3 that did
  not follow the `mask = plane0|plane1` rule the pair resolver bakes — proof
  a second writer was clobbering the resolver's output.
- `SMS_WATCH_VRAM=<slot>` showed two writers to the same VRAM slot: the 8x16
  pair resolver (`sat.s _sat_pair_emit`) wrote the full, correctly-baked tile,
  then the 8x8 base-sprite copy-through in the `$2007` pattern path
  (`ppu.s _ppw_no_inval` step 3) overwrote planes 0/1, leaving planes 2/3
  from the resolver's older source — a mismatched, wrong-palette tile.

Root cause: for CHR-RAM, both the per-OAM-entry 8x16 pair resolver and the 8x8
base-sprite copy-through target the SMS `$2000+` sprite-pattern region. The
copy-through is guarded by the *current* PPUCTRL bit 5, but CV1 uploads sprite
CHR during transient 8x8-mode windows, so it ran and clobbered resolver slots
the pair cache then never rebuilt.

Fix (generic, CHR-RAM only): a sticky "8x16 sprites in use" latch at `$CA39`,
set the first time PPUCTRL bit 5 is written high. Once latched, the 8x8
copy-through disables itself permanently — the pair resolver owns the sprite
region and reads its source from the SRAM CHR mirror (step 1), so 8x16 loses
nothing. Because `STA $2000` is lowered inline (bypassing `_ppu_w_ctrl`), the
latch lives in BOTH `runtime/ppu.s` and the lowerer's inline emitter
(`emit_ppu_ctrl_write_inline`, `crates/lower/src/lib.rs`); unit tests
`chr_ram_ppu_ctrl_latches_8x16_sprite_mode` /
`chr_rom_ppu_ctrl_has_no_8x16_latch` guard the emitter. SMB (CHR-ROM) is
unaffected — the latch is CHR-RAM-gated and SMB stays RAM+VDP byte-exact.
Limitation: a game that renders 8x8 sprites *after* having used 8x16 in an
earlier scene would keep the copy-through disabled; no such NES title is known
and CV1 is pure 8x16. Verify with the visual oracle if one appears.

## Performance profile and the motion-refresh glitch (2026-09-04)

Real-emulator symptom (screen recording, not a still screenshot which forces a
full refresh): during motion the background tile-materialization does not finish
each frame — parts of the screen show stale tiles ("CORE" for "SCORE", green-wall
blue speckles that shift per frame, a "FROZEN AGE" banner bleeding in). Root
cause is the **frame handler overrunning its budget**: when the NMI/presentation
runs long, the screen is presented half-materialized, differently each frame.
This is CV1-specific — SMB is CHR-ROM (tiles are permanent in VRAM), CV1 is
CHR-RAM (tiles are re-materialized from the SRAM mirror).

Per-frame cost measured on the heart route: median **1.5x** budget, average
**1.9x**, p99 **6.1x** (365k cycles), max **69x** (screen transitions). SMB peaks
at ~2x; CV1's heavy frames exceed even the 500% overclock cap, so no overclock
setting fully fixes the glitch — the fix must reduce per-frame cost.

`SMS_PC_PROFILE=1` hot-spot breakdown (heart route), by cost class:
- **~16% sprite pair baking** (`_sat_pair_emit`/`_sat_pair_*`,
  `_sat_build_pair_8x16`). Measured directly: CV1 does NOT shuffle OAM and the
  per-OAM-entry pair cache works correctly (frames 2000→2001: 13 visible
  sprites, 0 (tile,attr) changes → 0 rebuilds). The cost is **inherent
  animation** — Simon's whip/walk, torch flames, and enemies genuinely change
  tiles most frames, and each rebuild is 16 rows of per-pixel palette baking.
  Only lever: cheaper per-row baking (hoist the constant v-flip/h-flip/palette
  decisions out of the 16-row loop — est. ~4% total, verifiable by sprite-VRAM
  byte comparison). A (tile,attr) pool would NOT help (builds ≈ distinct).
- **~13% translated call/return continuation** (`_tr_cont_*`, `_tr_rts_*`).
  Inherent to CV1's emulated-stack discipline + UxROM banked dispatch; SMB
  avoids most of this with `stack_discipline="native"`, but CV1's cross-bank
  calls need the far-call machinery regardless. `_tr_cont_66` etc. are also
  partly just hot game code that follows a call site (not pure overhead).
- **~11% mapper fixed-high reads** (`rt_read_prg_high{,_indexed}`,
  `_rph_slot1_ei` 5.2%). Each `$C000-$FFFF` read maps `data_prg_high` into
  slot 1, reads, restores, bracketed by DI/EI. **De-guarding is ruled out for
  mapper 2**: the IRQ handler does not preserve slot 1 for UxROM (boot.s
  comment), and this is the CV1 re-entrancy class that caused the historical
  jp-$0000 reboot loop. A safe win here needs an IRQ-handler redesign that
  saves/restores slot 1 — high risk, defer.
- **~6% PPU status / sprite-0 handshake** (`_ppu_status_*`). Driven by CV1's
  $2002 poll loop (game-controlled count); could shave the per-read synthesis.
- Remainder: inherent translated game logic (`L_C030`, bank-0 `L_b0_*`).

Conclusion: unlike SMB (which had a single ~50% win in native calls), CV1's cost
is spread with no single big *safe* win. Smoothness needs a **campaign** of
several 3-5% verified steps. Ranked by (impact x safety): sprite per-row baking
hoist (safe, ~4%); PPU-status read slimming (moderate, ~3%); then the risky
mapper-read de-guard (needs the slot-1 IRQ-save redesign, ~5%). Each step must
pass the CV1 acceptance gates + sprite/VDP byte comparison and carry a
regression check, per the SMB Phase-S discipline.

## Materializer single-pass attribute resolution (2026-09-05)

**Reassessment:** the real-core test in
[CV1 performance reassessment](cv1-performance-reassessment.md) measures only
about 18 gameplay ticks/s at 500% and identifies a timing blind spot in the
trace evidence. The fix below improves attribute correctness and the measured
IRQ-to-EI interval; it does not establish that motion glitches are solved.
Use that reassessment's work order for performance/presentation follow-up.

First materializer-campaign step, addressing one scroll-edge attribute defect.
`_nt_project_col` (runtime/ntmap.s) used to materialize each entering column in
**two passes**: `_npc_row` wrote the 24 tiles resolving each variant against the
*old* folded sub-palette state, then `_npc_attr` re-applied the 8 governing
attribute bytes through `rt_apply_attr_byte`, re-resolving all 4x4 groups
(including three neighbour columns already correct). That second pass was ~11%
of frame time while scrolling AND — because its `_attr_quad_in_window` gate
skipped quadrants at the window edge — it left scroll-edge cells with the
previous screen's sub-palette: the "left part not refreshed" fragments (e.g. a
`CONAMI` title sliver bleeding down the left edge during Stage 1 scroll).

The fix (CHR-RAM only): `_npc_row` now resolves each cell's correct S from the
raw attribute shadow (via the existing, proven `rt_nt_attr_s_from_attr_shadow`)
and stages it into the folded `$CCxx` shadow *before* the single tile write, so
every projected cell resolves the right variant the first time. The `_npc_attr`
pass is dropped. In-place attribute changes to already-visible cells are still
handled by the direct `$2007` path (`ppu.s rt_apply_attr_byte`).

Results (continuous-scroll route): **p99 frame cost 3.87M -> 338k cycles
(-91%, 65x -> 5.7x budget)** — the full-window rebuilds that caused the visible
hitches; **avg 177k -> 143k (-19%, 2.97x -> 2.39x)**; max 4.6M -> 2.4M. The
median rises slightly (per-cell S extraction on every projected cell) but that
is moot under overclock where the win is killing the 65x tail. Verified: the
entrance (full materialization, no scroll) renders **byte-identical** before/
after (so the S extraction matches the old full-rebuild output exactly); the
scroll-edge `CONAMI` bleed is gone; CV1 acceptance (smoke + heart) green; min
native SP $DF8E (safe). **CHR-ROM (SMB) keeps the two-pass path** — its
authoritative sub-palette flows differently and the single-pass regressed SMB's
`bonus-pipe` VDP parity, so the optimization is `.ifdef NES_CHR_RAM`-gated and
SMB stays VDP+RAM byte-exact on all three golden routes.

Regression guards: SMB's `FD_VDP_CHECK` goldens catch any CHR-ROM breakage (they
caught the ungated regression); CV1 acceptance + the scroll frame-cost
distribution catch CHR-RAM breakage.

## Reproduce CV1 acceptance

### Real-core gameplay timing

Use actual GPGX video frames and profile-owned game-tick input routes when
comparing speed. This does not use the instruction-paced trace clock:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_route.py out/emulator-host/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-routes.toml \
  heart out/cv1/core-heart-before --capture-every 1
```

Use `walk` in place of `heart` and a new output directory for each run.
Add `--compare out/cv1/core-heart-before/summary.json` on a later build's
matching route to check state, item collection and player/camera landmarks.
The profile requests 500% overclock; invalid/unqueried options, traps, stalled
game ticks, missing heart collection, or incomplete routes fail the command.
Outputs include provenance, physical-frame CSV, gameplay update rate and PPM
captures. Frame hashes are observations, not a built-in visual-parity oracle;
check movement/scroll and the captured sequence as well as throughput.

Baseline `2123ef8`, core `a7985a9`, 420 game ticks per route: walk takes
1,602 physical intervals (15.7101 updates/s, worst lag 28 frames); heart takes
1,205 (20.8859 updates/s, worst lag 6 frames), collecting the large heart.
Compare within the same route: whip animation changes how much scrolling work
is performed. Continuous and stride-10 heart capture produce identical CSVs.

Runner tests: `python3 -m unittest discover -s tools -p test_core_route.py`.

Assembled handoff/IRQ regression tests (requires the locally built CV1 project):

```sh
CV1_HANDOFF_PROJECT=out/cv1 cargo test -p nes_to_sms \
  --test frame_handoff -- --ignored
```

These eight tests execute the assembled runtime in the in-repository Z80
emulator, including interruptions inside translated instructions. They are
explicitly ignored by the ordinary workspace run because ROMs are not shared.

### Instruction-paced functional checks

```sh
cargo run --release -p nes_to_sms --bin nes-to-sms -- \
  .roms/cv1.nes profiles/cv1.toml out/cv1 --runtime runtime
docker compose run --rm --user "$(id -u):$(id -g)" --workdir /work poc \
  bash -lc 'make -C out/cv1'
SMS_DUMP_PPM=out/cv1/graphics-acceptance/stage.ppm \
  target/release/trace-sms out/cv1/sms.sms --steps 180000000 \
  --pause-at-frame 600 --buttons-at-frame 1850:right \
  --expect-no-trap --expect-ram 0x0018=05 --expect-ram 0x0019=06 \
  --expect-ram 0x00F7=01

# Candle/heart regression: crosses the formerly fatal return escape, collects
# the large heart, remains in gameplay, and soaks after the event.
target/release/trace-sms out/cv1/sms.sms --steps 180000000 \
  --pause-at-frame 600 \
  --buttons-script profiles/cv1/acceptance/heart-smoke.sms.buttons \
  --expect-no-trap --expect-ram 0x0071=0A \
  --expect-ram 0x0018=05 --expect-ram 0x0019=06

# Fixed-work performance benchmark used for the before/after measurements.
target/release/trace-sms out/cv1/sms.sms --steps 300000000 \
  --game-frames 2200 --pause-at-frame 600 \
  --buttons-script profiles/cv1/acceptance/heart-smoke.sms.buttons \
  --expect-no-trap --expect-ram 0x0071=0A \
  --expect-ram 0x0018=05 --expect-ram 0x0019=06

# Deep Stage-1 traversal: full budget must complete with no trap.
SMS_DUMP_EACH_FRAME=out/cv1_traverse/f10 \
  target/release/trace-sms out/cv1/sms.sms --steps 570000000 \
  --pause-at-frame 600 \
  --buttons-script profiles/cv1/acceptance/stage1-traverse-v2.sms.buttons \
  --expect-no-trap
```

For canonical-reference checks, use
`profiles/cv1/acceptance/stage1-smoke.buttons` with `FD_PAUSE_START=1`.
For GPGX, `docker/run_gpgx.sh` reports whether the configured 8BitDo event
device is actually present; set `GPGX_REQUIRE_GAMEPAD=1` to fail instead of
falling back to the keyboard.

## Regression floor

SMB must remain green before mapper work is accepted. The current build has
657/657 lifted functions, no lift/lower failures, no unresolved labels, and
passes the documented 301-million-step 1-1 clear route with `$075C=01`,
`$0760=02`, `$075A=02`, and no trap. Run `cargo test --workspace`, format, and
Clippy before handoff.

## Deferred quality work

These are follow-on improvements, not mapper-2 blockers:

- improve palette quantization and localized scene/window seams;
- expand return-escape annotations only when a canonical route proves another
  stack-unwinding edge; stale target metadata must continue to fail closed;
- continue stock-timing work on fixed-PRG reads and CHR-RAM sprite builds;
- author a human route through the Stage 1 boss and Stage 2 entry;
- resolve additional strict stubs only when canonical execution reaches them;
- extend real-emulator soak coverage;
- audio quality: music and SFX are live through the APU shim (see the audio
  section above); remaining gaps are DMC (silent by design), noise-rate
  quantization to three PSG rates, triangle bass octave-folding, and
  240 Hz sequencer timing approximated per video frame.

Never raise routine limits, translate filler/data walks, add Rust-side CV1
gameplay, or harvest roots from SMS traps to make these items appear green.
