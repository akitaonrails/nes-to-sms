# Performance plan — using the SMS hardware, 2026-09

This plan picks up after the mapper-breadth and multi-game verification work.
It supersedes nothing in [`speed-recovery-plan.md`](speed-recovery-plan.md) or
the CV1 performance docs — those record the *translation-level* wins already
banked (native CALL/RET, replacement hooks, native sound, the edge-weighted
placer, the VDP-parity oracle). This plan is about the **next lever**: the
PPU→VDP and sprite translation layer, which the current profile says is where
the frame actually goes.

## Baseline and measurement discipline

Measured on the canonical build (`.roms/smb.nes` + `profiles/smb.toml`, the
committed `out/smb`) with `FD_MEASURE_NMI=1 FD_PROFILE=1 … --script start_right`
over 240 frames:

- **Steady NMI frame ≈ 105K cycles ≈ 1.76× the 59,736-cycle SMS budget.**
  (261K at the start of the speed campaign; 118K at speed-recovery session 8.)
- Full-speed today needs ~300% GPGX overclock.

Every change below must hold the existing gates: **RAM byte-parity** on all
acceptance routes (`frame-diff`, no divergence), **VDP byte-parity**
(`FD_VDP_DUMP` goldens + `FD_VDP_CHECK`), and the workspace test suite. Regenerate
VDP goldens from the trusted predecessor per change. Measure before and after
with `FD_PROFILE`; do not optimize on intuition.

## Where the frame goes (SMB steady frame, by category)

| Category | Share | What it is |
| --- | ---: | --- |
| **PPU/VDP writes** | **41.6%** | nametable tile + attribute writes, CHR/pattern mapping, `$2007` apply, background-variant machinery |
| **Sprites / OAM / SAT** | **18.7%** | OAM-DMA copy, sprite-attribute-table X/Y/tile upload sweeps |
| Translated game logic | 31.5% | diffuse lifted 6502 → Z80 |
| Banking / far-calls | 5.1% | cross-bank shims, return thunks |
| Block/collision | 3.1% | block-buffer collision helper |
| Sound | 0.0% | already native (delegation engine) |

**~60% of every frame is the PPU-emulation and sprite layer.** That is the
budget worth attacking, and it is where "use the SMS hardware better" is
literally true — the SMS VDP has its own name table, its own sprite attribute
table, and address auto-increment that the current path underuses.

## The core problem

The translation currently reproduces **NES PPU semantics** faithfully: the game
writes tiles/attributes through a modeled `$2006/$2007` port, and the runtime
converts each write into the equivalent SMS VDP name-table/CRAM/pattern update,
resolving palette variants and folded-BG cases per write. It is correct (VDP
byte-parity) but it pays NES-shaped costs on SMS-shaped hardware: per-tile
apply overhead, attribute-quadrant expansion, and a variant/pattern pool that
re-addresses the VDP repeatedly.

The SMS VDP wants **coalesced, auto-incrementing bursts**: set the address once,
stream a run with `OTIR`/unrolled `OUT`, and touch each VRAM word at most once
per frame. The opportunity is to move from "translate each NES write" to
"reconcile the NES frame's net effect onto the VDP once."

## Workstreams, in priority order

### P1 — Coalesce the PPU/VDP write path (target: the 41.6%)

The prior campaign already landed same-value repaint skips, wrap-gated skips,
and a fused SAT upload. The next structural step is **net-effect reconciliation**
per frame rather than per write:

1. **Shadow the NES name/attribute table in RAM** (already partly present via
   the VDP-parity model) and, at the NMI boundary, diff it against the last
   uploaded VDP name table. Upload only changed cells, and upload **contiguous
   runs** with a single VDP address set + streamed `OUT` loop, exploiting the
   VDP's auto-increment. Measured NES data says 67% of CIRAM tile writes and
   56% of attribute writes are same-value — a diff-and-run upload turns those
   into zero VDP traffic without per-write guard cost.
2. **Fold attribute expansion out of the hot path.** The NES 2-bit attribute
   quadrant maps to SMS per-tile palette bits; precompute the tile→palette
   mapping once per changed attribute byte, not once per covered tile.
3. **Batch sequential `$2007` runs** at the emulation boundary: a strided write
   run (the common `LDA;STA $2007` uploader) should lower to one address set
   plus a block move, not N modeled writes. Extend the existing strided-fill
   idea from RAM to the VDP port.

Gate each with `FD_VDP_CHECK`. Expected reachable: single-digit-K cycles/frame
back from the 44K the PPU category costs today.

### P2 — Native sprite / OAM→SAT path (target: the 18.7%)

OAM-DMA (`_oam_dma_aligned`, 3.4%) plus the SAT X/T/Y sweeps (~7%) dominate the
sprite layer. The SMS SAT is a different shape (Y table then X/tile table) but
is uploaded the same way every frame.

1. **DMA the OAM shadow to the SAT in one reformatting pass**, emitting the SMS
   SAT's Y-run and X/tile-run with two address-set + streamed-`OUT` loops,
   instead of the per-slot loops. Software sprite flip (SMS has no flip bit)
   stays, but the flip lookup should be table-driven, not branchy.
2. **Skip unchanged SAT slots** with the same shadow-diff discipline as P1 —
   sprite Y for off-screen slots is a constant, so most frames touch few slots.

### P3 — Trim the diffuse translated logic (the 31.5%, lower ROI)

This is the long tail — no single symbol above ~1.2%. The tractable pieces:

- **Tier-3 relayout** of the hottest translated routines by the edge-weighted
  placer (union far-edges across builds to a fixed point — the method that
  finally beat address-order).
- **More idiom lifts** where the profile shows a recognizable 6502 pattern
  lowered verbatim (carry-threaded runs, `(zp),Y` streaming, ROR chains).
- Continue the **replacement-hook** approach only where a routine is both hot
  and has an auditable register/state contract; this is near exhausted for SMB.

### P4 — Native-span graduation for the non-fast-path mappers

The mappers this session added (VRC2/VRC4, FME-7, MMC2/4, MMC5) are verified in
the oracle but have **no SMS conversion runtime yet**. When they graduate to SMS
builds they should be born on the coalesced VDP path (P1/P2) rather than
re-deriving the per-write model. Separately, the CNROM/Adventure source-hardware
runtime remains ~250× off real-time at 500% overclock; its documented blocker is
**verified translated code running natively between hardware events**
([source-hardware-runtime.md](source-hardware-runtime.md)) — a design campaign,
not micro-optimization, and out of scope until a source-hardware game is the
active target.

## Sequencing

1. Regenerate VDP goldens from the current `out/smb`; confirm they gate.
2. P1.1 (name-table diff-and-run upload) — biggest single expected win; land it
   behind the parity gates, measure, commit.
3. P1.2 / P1.3 (attribute fold, `$2007` run batching).
4. P2 (SAT reformat + slot skip).
5. Re-profile; if the PPU/sprite categories have dropped below the translated
   tail, pivot to P3.

Success is measured only in `FD_PROFILE` cycles/frame with parity held, not in
lines changed. The near-term target is **sub-1.5× budget (≈90K cycles)**; the
stretch goal that would remove the overclock requirement is 1.0× (59,736).
