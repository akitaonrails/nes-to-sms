# MMC3 pending and committed presentation

## Measured result and scope

This is the experimental `MMC3_FULL_RUNTIME` publisher, not a general MMC3
or full-speed claim. It keeps the previous committed image during preparation.
See [SMB3 status](smb3-plan.md) and the existing
[PPU/IRQ contract](mmc3-graphics-runtime.md).

The input-only title/map/inventory/1-1-clear tests pass at explicit `500` in
Genesis Plus GX. All seven full-route guest/cart checkpoints match the earlier
baseline at the same logical ticks; 2,336 common nonflat logical windows overlap.
This does not establish identical physical frames or every intermediate input.

| Full-route measurement | Before presentation change | Current publisher |
| --- | ---: | ---: |
| Gameplay callbacks | 24,302 | 26,351 |
| Flat gameplay callbacks | 8,667 | 0 |
| Longest flat run | 11 | 0 |
| Active game updates/second at 500 | 5.366 | 4.924 |

Coherent presentation still costs **8.24% traversal throughput**. A separate
200-update moving profile measures 4,301,011.88 nominal Z80 T/update versus
3,992,685.325 before this change. All 200 captured guest/cart boundaries match;
one of the 201 endpoint observations is unavailable, while the profiler covers all 200
intervals. Unknown and executed-data instruction costs are zero. Optimization
does not turn these results into a full-speed claim.

The actual physical-write observer covers all 30,387 full-route frames and
2,604 publications: zero renderer fallback disables and zero unexpected active
CRAM/VRAM writes or stream violations. Its complete input/video streams and all
1,179 snapshots, including final state, match the installed core. The 17 flat
map callbacks are genuine source-rendering-off transitions, not upload failures.
The separate 8,000-frame stock-100 diagnostic also has zero unexpected writes;
it retains 23 safe burst fallbacks and one publication-deadline fallback.
This bounded diagnostic does not establish full-route stock-speed playability.
Independent production correctness and structure reviews passed. Seven assembled
checks were independently rerun, including source pixels, residency/re-enable,
capacity, queue boundaries and interrupt service. Workspace tests pass (582);
fresh SMB1/CV1 ROMs match their accepted builds byte-for-byte. The observer's
author performed the production review, not an independent review of their own
instrumentation; observer validity is supported separately by the pinned-core
synthetic, negative-control and complete-execution parity checks.

## Ownership

`frame_mmc3.s` owns completed immutable source packets and guest-event order.
`render_mmc3.s` resolves exact physical patterns and builds pending NT/SAT and
palettes. `present_mmc3.s` owns residency protection, staged uploads, bounded
publication and committed shadows. Host display service consumes only committed
scroll/palette state; it may run during preparation but cannot reenter the guest.
All new code is full-mode-only. NROM/UxROM emit their existing runtime paths.

Frozen CIRAM comparison/copy admits host service at most every 64 bytes. The
previous 128-byte equal-data loop could delay stock-clock palette rearm into
active display; the shorter boundary retains exact bytes and change flags.
The [touched-block capture path](mmc3-touched-capture.md) skips source blocks
known equal to each snapshot. Touched playfield blocks compare in eight-byte
groups; every exact byte-change bitmap byte is still assigned anew. HUD has
independent touch intent and cannot overwrite playfield evidence. Clean skips
also admit bounded host service; skipping128 source bytes is not a128-byte copy.

The physical cartridge remains 32 KiB SRAM. Bank-1 guest `$8000–$9FFF` is
untouched; its distinct upper half stages BG data. There is no added emulator
memory or altered guest hardware capacity.

| Storage | Full-mode owner |
| --- | --- |
| Native CC00–D2FF, D600–D6FF | Committed 2048-byte NT, split around CPU frames |
| D700–D7BF | Committed 192-byte SAT |
| D7C0–D7DF / D7E0–D7FF | Immutable old-live / current-needed BG bitmaps |
| D800–D97F | 128 fixed sprite pattern identities |
| D980–D9AF | 384 staged-pattern dirty bits |
| D9B0–D9EF | Pending playfield and HUD palettes |
| D9F0–D9FF | Pending registers, publication state and queue scratch |
| DA00–DBFF | 64 eight-byte upload descriptors |
| DC00–DCFF | Exact 2048-bit previous/current frozen PF byte differences |
| DD00–DD32 | Previous 51-byte frozen PF record |
| DD33–DD3E | Prior validity/split and cell-reuse scratch/reserve |
| DD3F | Frozen source is anchored to the committed NT |
| DD40–DD4F / DD50–DD5F | Independent PF / HUD physical-CIRAM touch masks |
| SRAM bank 1 A000–BFFF | 256 staged BG patterns |
| SRAM bank 0 9980–9FFF, B600–BF7F | 128 staged sprite patterns |

Bank-0 live/frozen CIRAM, OAM, records, keys and prepared NT/SAT retain their
documented locations. Native D300–D5FF belongs to CPU continuations; neither
the publisher nor its shadow copy writes there. The legacy `DIAG_WILDJUMP`
canary clear is excluded in full mode because those bytes now have an owner.

Cell reuse is anchored to the completed native NT shadow, never the pending
SRAM NT. The serialized PF producer saves the old record and committed split,
assigns every bitmap byte afresh, and only then validates the evidence. Validity
requires both prior `READY` and the explicit source anchor. Capture clears the
anchor before overwrite; an unanchored capture forces packet/BG resolution even
if its new bytes equal a discarded pending packet. Thus cancellation never
confuses an unpublished source with the still-visible committed frame. `BUSY`
excludes another producer during publication and its native shadow copy, after
which the anchor is restored. Exact-packet return also restores the equivalent
anchor; source-off clears it. The renderer's completion callback adds two native
stack bytes during the publisher call, without enlarging the stack allocation.
HUD capture leaves PF evidence alone. Graphics initialization seeds both touch
masks after clearing live CIRAM; it does not certify the cell-reuse evidence.
Every reused slot enters `NEEDED`, including reserve-pass restarts; old-live
protection remains unchanged.

SMS VRAM remains 16 KiB: BG `$0000–$1FFF`, fixed sprites `$2000–$2FFF`, NT
`$3700–$3EFF`, and SAT `$3F00–$3FFF` with its hardware gap. No pattern slot
that the committed NT or active SAT references is uploaded early.

## Persistent identity and preparation

BG keys retain physical CHR identity, subpalette and mixed-raster row/offset
dependencies. An ordinary build protects **all old-live slots** and current-needed
slots, allocating only old-nonlive space. If that space runs out, it restarts the
existing reserve/build passes. Only NEEDED is cleared: hash identities, staged
bytes and dirty bits survive. The reserve pass protects future existing keys
before any old-live replacement is staged. Every replacement remains deferred
until publication. Exactly 256 required keys fit; the existing 257-key error
remains explicit instead of evicting a needed tile.

Full rendering writes 28 or 29 NT rows and zero-fills all remaining rows.
Consequently the exact committed **32-row** live set is previous NEEDED union
slot 0, even when no visible cell uses slot 0. Preparation copies that bitmap;
BG-stable frames retain NEEDED, and full builds clear it only after copying.
This is an exact lifetime invariant, not a visible-footprint approximation.

Sprite slots are fixed per OAM entry, with 128 patterns reserved for 64 pairs.
Identity changes stage new payloads; hidden and active transitions still update
SAT. Source record equality and the existing partial-layer guard precede reuse.
Raw scroll bytes are not substituted for resolved loopy t/fine-X semantics.

NT/SAT differences are exact byte runs. Runs may bridge at most 24 equal bytes,
trim equal tails, and never cross an independently mapped source/destination
segment. Remaining count stays in BC; host service is admitted at closed
boundaries at most every 16 compared bytes. Adjacent descriptors coalesce only
when source, destination and SRAM bank all agree.

The prepared NT uses a common horizontal ring: logical column `i` is stored at
`(i+c)&31`, where `c` is the playfield's frozen coarse X. Both playfield and HUD
scroll registers include that same `8*c` offset plus their independent fine X.
This permutes destinations without changing source selection or mixed-pattern
construction. Ordinary horizontal movement can retain most playfield NT cells;
screen-fixed HUD rows still rotate. The committed scroll, not pending state,
owns HINT service. BG-stable frames retain that exact NT and skip its redundant
diff/shadow copy; SAT, palettes and all publication rules still run.

The normalized-pixel test decodes the actual 256-pixel-high SMS tilemap and
independently derives source pixels from frozen CIRAM, CHR, palettes and loopy
state. It passes on both the preceding unrotated fixture and the ring fixture.
It checks fine X 0–7, coarse X 0/1/31 and wrap, independent HUD coordinates,
split/fine-Y/reload cases, CHR inversion, subpalettes, screen-fixed left masking,
and unchanged sprite X. The runtime's top horizontal and right vertical scroll
locks remain off. This does not expand the existing 32-column source footprint
or claim to fix the baseline's horizontally wrapped edge.

Bitmap masks use a 256-byte aligned lookup in permanent ROM bank 0. The
constrained FREE section must fit without overwriting or moving into a switchable
bank; it adds no RAM or stack state. The helper preserves its original C output,
address/flag tail and DE contract. Collection skips eight pattern probes only
when their exact DIRTY byte is zero; nonzero groups retain all existing live-slot
and phase checks. Collection cannot clear or create dirty identities.

Fill and shadow copies use at most 32-byte bulk chunks with closed host-service
boundaries. Remaining counters stay on the native stack because initialization
overwrites publisher metadata itself. Positive-count end pointers, registers
and flags are preserved; zero count is an explicit no-op, not an accidental
65536-byte transfer. Bulk instruction timing is not a substitute for actual
stock-clock interrupt/rearm measurements.

## Publication and fallback

`M3P_STATE` at D9F6 distinguishes committed-visible preparation (0), atomic
DI publication (1), and explicitly blanked initial/fallback/off work (2).
Old display service remains available while waiting for a **new** VBlank.
Early uploads touch only old-nonlive patterns and use address-restoring bursts,
because intervening host service may change the VDP address.

STATE1 sets the VDP address once per command and streams bounded OUTI suffixes.
Pause has no VDP writes; maskable host interrupts are not admitted inside the
transaction. Source, remaining length and descriptor ownership are preserved.
The private stream-admission entry requires STATE1/DI: its sole caller enters
after the command dispatcher checks STATE1. Successful suffixes keep that
ownership; the retained post-admission state check detects a real deadline
blank and exits permanently to the old writer. It is not a general state0/2
admission helper. Stream copying does not write the old writer's count scratch
at D9FF; each old copy overwrites that byte before its two count consumers.
If a deadline rejects the next suffix, display is disabled **before** recovering
the current destination and resuming the conservative writer. No byte is skipped
or replayed. Queue overflow likewise waits for an admitted VBlank before blanking;
the pending 65th descriptor survives the flush. Valid over-budget packets are
completed coherently, not dropped, trapped or left pending indefinitely.

The stream guard permits 128 bytes through VCounter F2, 64 through F6, 16 through
F9, and one at FA. The old early writer retains its separate 128-through-F0 guard.
These are NTSC 224-line bounds, not the larger 192-mode VBlank budget and not an
assumed emulator overclock. The repeated EA→E5 portion and FF→00 wrap are tested.

On the final stream fixture, exact emitted-instruction accounting gives
sample-to-next-sample bounds of 2603/1677/928/665 T. Including one 66-T Pause
and the measured 160-T path to actual display disable gives combined bounds
of 2829/1903/1154/891 T. Corresponding conservative remaining windows
are 2964/2052/1368/1140 T. This proof includes command transitions; it must be rerun
after code relocation or edits. It assumes DI and at most one Pause per bounded
interval. `Cpu.cycles` in the ordinary Rust interpreter is approximate and is
**not** this timing source. The test-only scoreboard is fail-closed and checked
against actual pinned-core opcode fixtures at both 100 and 500.

Before waiting for VBlank, pending palettes are compared exactly with committed
native buffers. D9FC bits 0/1 record whether playfield/HUD native copies are
needed; READY=0 forces both. Unchanged 32-byte native copies are omitted inside
publication, but the actual 32-byte CRAM upload and rearm are **always retained**.
Host display service only reads committed buffers and cannot invalidate this
comparison; pending data remains immutable until commit.

Palettes, scroll, split and mode-preserving registers publish together. A pending
host VINT is serviced while publication ownership still excludes redundant rearm;
only then does STATE return to 0. Native NT/SAT shadows update after publication
with closed host-service boundaries. Actual enabled-display VRAM writes and
non-HUD active CRAM writes remain separate, mandatory emulator checks.

A completed guest rendering-off packet blanks normally; transient NMI mask writes
do not publish partial source state. Re-enable has no valid native shadow, so
READY=0 enqueues the **entire NT and SAT** while already blank. Comparing against
zeroed initialization metadata would leave old VRAM tile indices and hidden SAT
bytes intact; the source-off/re-enable regression test covers that failure.

## Reproducing focused checks

Generate and Docker-assemble the ordinary SMB3 project first. Keep prior accepted
outputs separate; do not overwrite a canonical regression project during testing.
Point the existing assembled test harness at the resulting project directory:

```sh
export TRACE_FUNCTIONAL_PROJECT=out/smb3-candidate
cargo test -p nes_to_sms --bin trace-sms mmc3_full_bg_stable_matches_forced_rebuild -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_needed_lifetime_matches_all_committed_rows -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_common_ring_matches_source_pixels -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_bulk_copy_fill_preserve_boundaries -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_capture_copy_services_host_every_64_bytes -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_bitmap_lut -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_dirty_group_skip_preserves_every_payload -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_palette_copy_flags_match_always_copy -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_fast_residency_restart_retains_staged_payloads -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_publication_ -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_stream_transactions_preserve_bytes_and_fallback_position -- --ignored --nocapture
cargo test -p nes_to_sms --bin trace-sms mmc3_full_renderer_ -- --ignored --nocapture
```

The matrix compares whole VRAM, palettes and presentation registers against
forced rebuilding, including frozen-source mutation and nested host service.
Lifetime tests cover full→stable→stable→full, padding-only slot 0, fine-Y 0/7,
capacity/restart, and source-off/re-enable. Diff tests cover zero/all-equal input,
endpoint changes, 256/257-byte counts, gap boundaries and queue overflow.
Transaction tests check every emitted byte, bank, stack, shadow P, descriptor
transition, fallback position and Pause boundary. The exact timing suite covers
71,148 paths plus 82 fallback cases; 22,278 whole-transaction cases also assert
the sole stream caller's STATE1/DI ownership, old-only recovery and fresh count
scratch writes before old-writer reads. These counts describe the frozen
candidate, not a substitute for the actual video checks. The partial-mask and logical
IRQ suites remain required, as do exact SMB1/CV1 assembled ROM parity and the
workspace checks.

Actual acceptance additionally requires the unchanged input-only map/inventory/
1-1-clear routes, full guest/cart checkpoints, logical-window nonflat image
comparison, dense flat-frame counts/episodes, a source-aligned moving profile,
and actual VDP deadline observations at 500 plus the bounded stock-100 diagnostic.
A functional clear or a lower flat-frame percentage alone is insufficient:
absolute interruptions, longest run and update throughput must be reported.
The measured zero-blink result applies to this route at 500, not every game
screen or stock-clock playability. Oversized packets retain a coherent fallback.
