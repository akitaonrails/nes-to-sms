# MMC3 graphics bring-up contract

This documents the opt-in `MMC3_FULL_RUNTIME` backend. It is separate from
the bounded `MMC3_BANKING_EXPERIMENT`, NROM/UxROM rendering, and CV1 hooks.
It is **not yet a playable-game or normal-speed claim**.

The source/IRQ contract below still applies. Renderer blanking and performance
sections record earlier bring-up stages; the current
[pending/committed presentation contract](mmc3-presentation.md) supersedes those
publication details and adds staging/residency ownership. See
[current first-level results](smb3-plan.md) for the measured gameplay scope.

## Source state and assets

The emitter stores physical CHR-ROM in 16 KiB ROM banks after PRG data,
the boot CHR placeholder, and small assets. `MMC3_CHR_DATA_BASE` and
`MMC3_CHR_BANK_COUNT` (1 KiB units) come from actual allocation. Supported
geometry is power-of-two 8–256 KiB, further limited by the 2 MiB image's
remaining capacity. The boot placeholder is exactly 16 KiB of SMS tiles;
it is not evidence of translated gameplay graphics.

Mapper R0/R1 select even-aligned 2 KiB pairs; R2–R5 select 1 KiB pages.
Inversion swaps the two 4 KiB groups. Graphics keys identify physical CHR
tiles, not merely an 8-bit NES tile number. `$2007` pattern writes are
ignored while the PPU address still increments. Reads use physical CHR,
buffered CIRAM, and palette aliases, never SMS VRAM as guest source data.
Dynamic mirroring affects live CIRAM addressing, not previously frozen frames.

`rt_ppu_read`/`rt_ppu_write` retain the existing B=register/A=value ABI and
CB08–CB24 register locations. Full-mode transactions preserve DE, shadow P,
entry IFF, and exact slot-2 ROM/SRAM mapping. Loopy t, fine X, and the shared
`$2005/$2006` write toggle are tracked separately. Native SMB3's scroll
0/239 yields t=`$73A0`: vertical wrap then selects most visible pixels from
the second vertical nametable, despite PPUCTRL's low nametable bits being zero.

## Memory ownership

All SRAM addresses below refer to **bank 0**, control `$08`. Guest cartridge
RAM is CPU-owned bank 1, control `$0C`, at `$8000–$9FFF`; it never aliases this
renderer storage.

| Address | Owner / contents |
| --- | --- |
| C810–C82F | CPU mapper registers, IRQ state |
| C830–C8FF | Full-mode graphics controls, palette and tile scratch |
| C900–C9FF | Live NES OAM staging |
| 8000–87FF | Live raw CIRAM |
| 8800–8FFF / 9000–97FF | Frozen playfield / HUD CIRAM |
| 9800–98FF | Frozen OAM |
| 9900–997F | Two CHR/control/scroll/palette records |
| A000–A7FF | Prepared 32-row SMS nametable |
| A800–AAFF | Physical BG tile and subpalette keys |
| AB00–ABBF | Prepared SMS SAT |
| AC00–AFFF | Exact-key hash heads and chains |
| B000–B5FF | Mixed-raster secondary keys, palette, split row and source-row offsets |

The SMS 224-line mode has a 256-pixel-high tilemap. Fine Y therefore needs an
additional source row; retaining only 28 rows incorrectly wraps the bottom.
The NT starts at `$3700`. The allocator uses 256 BG patterns at `$0000–$1FFF`
and 128 fixed sprite patterns at `$2000–$2FFF`, enough for 64 independent
8×16 pairs. The remaining VRAM before the NT is unused, not extra allocated
cache capacity. See the hardware register
and tilemap descriptions in [SMS Power's VDP documentation](https://www.smspower.org/Development/Tilemap).

## Logical interrupts versus physical display

`SMB3_MMC3_SINGLE_SPLIT` names a deliberately coarse bring-up adapter, **not
an MMC3 qualified-A12 model**. Native NES observations on the fixed reference
route show one IRQ per frame, latches `$C0/$C1`, BG table 0 and 8×16 sprites.
An outer completed translated NMI freezes the playfield record. For the real
profile, `MMC3_COOPERATIVE_WAIT` restricts subsequent guest events to a one-shot
request (C825) from a verified original wait loop. Physical VINT alone cannot
interrupt half-finished source main work with the next logical event.
Main rendering-off cancels an unasserted split, including off→on before the
next wait; transient handler writes do not. The actual translated IRQ runs,
acknowledges E000, and supplies the HUD record. Unsupported
adapter configurations trap `$EA`. Current PPUCTRL.NMI is respected; the
legacy sticky-enable policy is not used.

Guest P.I defers IRQ but never suppresses NMI. Pending IRQ ownership survives
until actual E000 acknowledgement, including acknowledgement inside NMI.
If an unacknowledged IRQ outlives its frozen packet, later unmasked delivery
traps `$EA` before entering the wrong frame. The masked frame's single-record
fallback is an explicit fidelity limit, not a claim of cross-epoch raster
compatibility. C8F6 stores cancellation, C8F7 IRQ/reload intent, C8F8 sticky
stale ownership, and C8F9/C8FA diagnostic epoch counters. Epoch wrap cannot
clear stale ownership. Synthetic fixtures without cooperative annotations
retain the coarse host-VINT bring-up mode; it is unsuitable for actual SMB3
transitions and is not the real profile's scheduler.

CPU-owned IRQ fields are latch C820, reload C821, enabled C822, pending C823;
C824 holds mirroring. Mapper writes and E000 acknowledgement own those
register effects. The adapter consumes reload and asserts pending only for
its declared logical event. CHR/mirroring/IRQ change callbacks run DI, preserve
DE/P, and never reenter guest code.

SMS HINT only replays **committed display** horizontal scroll/palette. It
does not execute the guest IRQ or impersonate a qualified NES A12 edge.
HUD vertical `$2005` writes do not restart the visible vertical address:
this does **not** apply to an IRQ that explicitly reloads v through `$2006`.
Native intro evidence shows such a reload plus a rendering-time `$2007` read;
the adapter records live v and reload intent separately from final t. Its
visible IRQ `$2007` reads advance fine/coarse Y, matching the pinned native
reference's [OLDPPU read implementation](https://github.com/TASEmulators/fceux/blob/6c3a31a4f2c09be297a32f510e74b383f858773b/src/ppu.cpp#L805).
This narrowly scoped behavior does not simulate PPU fetch advances or establish
general rendering-time bus semantics. Pure-scroll IRQs continue NMI vertical
state. Reloaded HUD rows compose physical source tails/heads independently
of the SMS grid's fine Y. Keys include both physical sources, subpalettes,
cut row and source-row offsets; an unsupported three-source boundary traps.

## Coherence and evidence

The first renderer blanks while rebuilding, then publishes complete patterns,
NT, SAT and palette. Host interrupt service is admitted between closed tile
transactions and bounded NT-upload chunks. A new guest graphics producer
cannot overwrite the two frozen records. Exactly 256 distinct BG keys fit;
the 257th traps `$EB` while the display remains blank, never evicting a visible
pattern. Rendering-off frames publish blank immediately instead of waiting
for an IRQ that need not occur.

Independent BG-only (`$08`) and sprites-only (`$10`) rendering is not yet
implemented. Either layer combination in either frozen playfield or HUD
record traps `$EA` before publication, exact-packet reuse, or visible VDP
mutation. The existing committed display/blanking and backdrop remain intact
on that trap. Native route census observed only `$00` (2,060 writes), `$18`
(1,760) and `$1E` (289); those accepted configurations are tested separately.
This explicit experimental boundary prevents silently rendering a layer that
the guest disabled; it is not a claim of independent-layer support.

Completed byte-identical packets retain the committed display without
blanking or rebuilding. Capture establishes exact source equality before replacing
the previous frozen record: both CIRAM snapshots, OAM, physical CHR maps,
palette, control/mask, scroll, t, fine X, mirroring, IRQ reload v/intent, and
the committed split.
C8F4 accumulates differences; READY is mandatory for reuse. Exact comparison
is chunked into at most 64 bytes between host-service opportunities, with
BUSY excluding guest producer reentry. Transient rendering-off writes inside
NMI are source-only; a completed rendering-off packet still blanks.

The first incremental path also retains BG patterns, exact-key metadata and
the prepared/visible nametable when **both frozen BG sources are unchanged**.
C8EC records BG differences; C8ED selects the comparison group (then becomes
the renderer's skip-BG scratch). Both CIRAM snapshots, each record's
BG-selected four physical CHR pages, control/mask, t, mirroring,
explicit IRQ reload and the committed split participate. Physical pages are
already resolved through MMC3 inversion; changes to the other CHR half do
not invalidate BG. Control changes conservatively invalidate it, including
pattern-table selection. An earlier sprite/palette difference cannot bypass
a later BG comparison.

OAM, non-BG CHR, palettes, raw scroll bytes and fine X can therefore reuse BG.
Raw `$2005` bytes (record10/11) remain exact-packet identities but are not
rendering inputs: BG addressing uses t12/13 and IRQ reload48..50, while
fine X14 is committed as SMS scroll. In particular, `$2006` can replace t
without changing the saved raw scroll bytes. Coarse-t changes still invalidate
BG; no visible-footprint or offscreen-CIRAM relaxation is applied.
The renderer
still blanks, rebuilds sprite patterns/SAT and both palettes, and publishes
at VBlank. Active/hidden transitions keep the existing per-OAM slot rules;
there is no visible sprite-pattern overwrite. Single-record fallback
conservatively treats any copied HUD-record change as a BG change. There is
no SAT-only unblanked update or new BG cache allocation in this initial path.
The later [touched-block capture](mmc3-touched-capture.md) adds 32 native bytes
of conservative source-write intent without weakening exact packet comparison.
READY, partial-layer rejection and render-off behavior
remain mandatory.

Existing assembled helper tests in `trace_sms.rs` use `TRACE_FUNCTIONAL_PROJECT`:

```sh
cargo test -p nes_to_sms --test mmc3_pipeline \
  mmc3_full_bus_assembled_matches_original_6502 -- --ignored --nocapture
TRACE_FUNCTIONAL_PROJECT=out/tests/<generated-fixture>/sms \
  cargo test -p nes_to_sms --bin trace-sms mmc3_full_ppu -- --ignored --nocapture
TRACE_FUNCTIONAL_PROJECT=out/tests/<generated-fixture>/sms \
  cargo test -p nes_to_sms --bin trace-sms mmc3_full_renderer -- --ignored --nocapture
TRACE_FUNCTIONAL_PROJECT=out/tests/<generated-fixture>/sms \
  cargo test -p nes_to_sms --bin trace-sms mmc3_full_packets -- --ignored --nocapture
TRACE_FUNCTIONAL_PROJECT=out/tests/<generated-fixture>/sms \
  cargo test -p nes_to_sms --bin trace-sms mmc3_full_bg_stable -- --ignored --nocapture
TRACE_FUNCTIONAL_PROJECT=out/tests/<generated-fixture>/sms \
  cargo test -p nes_to_sms --bin trace-sms mmc3_full_partial_layers -- --ignored --nocapture
```

Use the separate `mmc3_full_irq_bridge_fixture` output for
`mmc3_full_irq_bridge_preserves_context_and_pending_order`;
its handlers are actual translated 6502, also checked with `oracle_6502` and
the MMC3 board model. The other fixtures establish CHR/mirroring/PPU ABI,
all four fine-Y 0/7 × split 192/193 pixel boundaries, frozen-state isolation,
eight additional IRQ-reload fine-Y 0/1 combinations with distinct nametable
rows, palette identities, sprite flips, and capacity limits. The IRQ fixture
also covers one-shot scheduling, main/transient blanking, pending ownership,
actual NMI acknowledgement, and stale-event traps. They are **synthetic
renderer evidence**, not an original-NES frame oracle or a game run.

The tiny renderer fixture measured roughly 1.69–1.81 million interpreter
T-states per rebuild, with a longest measured DI span of 32,576 T-states.
Those runs did not deliver host interrupts and do not establish steady-state
game speed, display duty cycle, or hardware IRQ latency.

After BG classification, the exact-packet fixture measured first capture
73,546 plus rebuild 1,548,233 interpreter T-states, versus identical capture
222,067 plus reuse 230 T-states. Before this increment those figures were
70,268 / 1,548,049 / 220,866 / 230 respectively: classification adds cost,
and does not optimize the identical-CIRAM comparison itself.
Its reuse path leaves VRAM, CRAM and VDP registers unchanged; individual source
identity changes, HUD-only writes, split-only changes and rendering-off are
separate assertions. These numbers describe that synthetic packet only,
not an actual-game speedup.
The nine-byte raw-scroll classification correction adds 72 interpreter
T-states per two-record capture in this fixture (73,618 first; 222,139
unchanged), with rebuild and exact-reuse costs unchanged. That interpreter
estimate is not the nominal Z80 cost: the added opcodes cost 74 T-states for
two records, as counted by the actual-core profile.

The incremental fixture compares 160 dependency/configuration cases against
forced full rebuilds, including fine-Y/split boundaries, physical CHR table
selection/inversion, both records, IRQ reload, palette/fine-X, and sprite
active/hidden changes. Entire VRAM and both raster palettes/register sets
must agree, with BG/NT bytes unchanged on eligible packets. Each case also
mutates live producer state after freezing and repeats with one nested host
VINT, checking host service without guest reentry. Raw-scroll cases cover
either byte or both, independently in either/both records, including fine-X
changes and raw/t disagreement after actual PPUADDR writes. All 48 such
configuration cases then issue a real PPUSCROLL coarse-t change and require
BG invalidation, with the same forced-full and nested-interrupt comparisons.
Without the injected IRQ,
one active moving sprite takes **42,097 T** on the BG-stable path versus
**1,783,920–2,028,068 T** for the same forced-full packet. These synthetic
costs exclude capture and do not predict how often real gameplay qualifies;
matched actual-core map/level A/B measurements remain a separate gate.

The first actual-core map A/B now establishes a **2.22× route-specific gain**.
With the same profile, compiler, translated code, source-timed controller
inputs and Genesis Plus GX core at numeric `500`, the first 400 map updates
take 4,814 physical frames before this change versus 2,168 afterward. At the
core's reported 59.922743 Hz this is **4.979 → 11.056 game updates/second**.
All 401 matched completed-wait snapshots have identical full 2 KiB guest RAM,
including stack and controller state; 42 matched captured frame pairs have
identical RGB pixels. Renderer-BUSY=2 samples fall from 66.47% to 25.55%.
That is a renderer occupancy measurement, not measured blank-frame duty:
captures were sparse and selected around publications. These observations
establish map correctness and throughput only, not playable-level performance
or full-speed execution on stock SMS hardware.

A separate no-input level comparison, through the death animation, covers
560 updates with all 561 matched completed-wait guest-RAM snapshots identical.
The same optimization reduces 8,969 physical frames to 5,120: **3.741 → 6.554
updates/second (1.75×)** at `500`. Both builds subsequently hit the same strict
unresolved death-return target. This is a bounded correctness/performance
comparison before that stop, not successful death recovery or active gameplay
acceptance. Its remaining non-render work is substantial and needs profiling.

The final matched **active 1-1 route** completes on both builds. From initial
ready state through the goal sequence, the same 1,979 updates take 36,588
physical frames before versus 31,020 after: **3.241 → 3.823 updates/second
(1.18×)** at `500`. All seven matched map/level/block/mushroom/goal/exit/map-return
checkpoints have identical full guest RAM and cartridge RAM. Dense video
classification records uniform-color callbacks falling from 19,223/40,438
to 12,459/33,678 in the gameplay window (about 47.54% → 36.99%), with a maximum
11-callback flat streak in both builds. Thus the larger map gain does **not**
generalize to moving gameplay. Both routes keep four lives and mark 1-1 complete;
the remaining cadence and rebuild blanking are severe playability limitations.

## Remaining acceptance limits

The subsequent raw-scroll dependency correction removes only unused `$2005`
metadata from BG identity, while retaining exact packet-change detection and
resolved PPU address/fine-X handling. Against the accepted RAM-fast-path ROM,
the same full route improves from **4.010 to 4.594 updates/second (+14.57%)**
at `500`: 29,571 → 25,811 physical intervals for 1,979 updates. All seven
full guest/cart RAM checkpoints agree. Gameplay uniform-color callbacks fall
from 12,463/32,143 to 8,667/28,383 (**38.77% → 30.54%**), with no black frames
and the same longest flat streak of 11 callbacks. Nonflat image sets overlap
at all 2,338 common logical ticks; this does not establish equality of every
physical/intermediate frame or normal-speed playability.

In the narrower 200-update moving profile, full builds fall from 159 to 115;
nominal work falls from 1,041,506,767 to 913,194,013 T-states (12.32%). Capture
comparison becomes slightly more expensive because raw-only changes no longer
short-circuit BG comparisons, but avoiding builds dominates. Map throughput,
blanking duty and all 401 completed images remain unchanged. Of 7,000 physical
map frames, 46 differ only on isolated rebuild-entry frames with identical
neighbors and committed display state. This is consistent with changed
within-frame blanking timing; exact transient pixel magnitude/region was not
captured and remains unverified.

The apparent title-ground checker artifact was a **false positive from image
preview scaling**. Raw native title pixels contain the alternating blocks:
the frozen SMS CIRAM pages and six ground CHR patterns match the native title
snapshot byte-for-byte. In one retained title comparison, 6,944 black/white
ground pixels match exactly after a one-line vertical alignment; without that
alignment, 1,120 differ. The authentic pattern must not be replaced with stripes.
The remaining one-line boundary discrepancy is distinct: native C1 title IRQ
writes the low `$2006` byte after visible pixels of line193, whereas the adapter
currently applies its reloaded record at line193. This inference uses recorded
CPU-cycle offsets and the pinned core's [frame/scanline loop](https://github.com/TASEmulators/fceux/blob/6c3a31a4f2c09be297a32f510e74b383f858773b/src/ppu.cpp#L1738);
it is not a generic latch→pixel timing rule. C0 map/level handlers perform
different writes and must be checked separately before any alignment change.

The map Mario icon also looked fragmented in a scaled preview but matched the
native comparison at pixel level. Actual SMS frame5808 versus native frame1000
has identical OAM35/36 and physical CHR tiles `$082C–$082F`. All 256 pixels at
x64–79/y33–48 match under a one-to-one five-color translation. Red and skin-tone quantization
changed its appearance, not its shape or position in that state. This checks
one matched map icon, not all sprite animations or split-crossing sprites.

Actual reset → title → input-responsive map passed the phase-2 functional
route in Genesis Plus GX at numeric `500`, without guest-memory writes. ROM
SHA256 `1db48a519d3b76a72d9fc4383aadff859b67136a934bf522e575a89dd775d966`
reached the map at physical frame2014, then Right/Up inputs moved its source
coordinates from (32,64) to (64,32). The 6,000-frame run observed 330 map ticks,
259 distinct video hashes, and no trap. This establishes input-responsive map
progress alone, not level1, normal NES cadence, or general mapper timing.
The later phase-3 route above separately establishes first-level completion.

Historical pre-cooperative probes showed curtain/intro publications amid
mostly blank frames: one 1,600-frame probe had 194 renderer-busy exits and
93 distinct video hashes; an earlier 6,000-frame run remained near early intro.
These older results are superseded for title/map reachability, not evidence
of the current display duty cycle. Changed packets still blank during rebuild;
the renderer is not a final playable-performance implementation.
Persistent residency/dirty updates and visible motion/duty-cycle measurement
belong to the subsequent performance phase. Fine-X edge wrapping, sprite
behind-BG priority, sprites crossing a CHR raster split, palette-HINT timing,
PPU rendering-time address quirks, and finer guest IRQ scheduling still need
explicit evidence or additional support. Do not interpret this adapter as
general MMC3 timing compatibility.
