# CV1 performance and presentation reassessment

Date: 2026-09-05. Assessed baseline: `2123ef8`. The original diagnosis and
work order follow; implementation evidence is recorded below. Keep the recent
sprite-palette and CHR-RAM materializer fixes; their static correctness and
cost improvements do not establish smooth real-emulator motion.

## Implementation checkpoint: completed-prologue handoff

The opt-in CV1 bridge now publishes its scroll/control context after the full
NMI's DMA/stripe prologue (`$C11F`), before busy `$1B` is set. Lag NMIs retain
their input/audio semantics without publishing another graphics generation.
The IRQ consumes each packet once and preserves newer pending writes. Skipped
nested entries no longer reset the producer's sprite-zero/split phase.

A real-core boot test caught a startup race missed by instruction-paced tests:
reset enables NMI before initializing saved PRG bank `$24`. The first translated
NMI now waits for that initialization, without delaying subsequent bank-zero
frames. An assembled-code boundary regression fails the first candidate and
passes the corrected one.

Matched 420-game-tick routes, same GPGX `a7985a9`, NTSC-U, overclock `500`:

| Route | Baseline updates/s | Handoff updates/s | Longest physical-frame gap |
| --- | ---: | ---: | ---: |
| Walking | 15.7101 | 19.0808 | 28 → 6 |
| Walking + whip/heart | 20.8859 | 20.9730 | 6 → 6 |

Both routes pass state/area/heart checks; matched player/camera landmarks differ
by at most one pixel. The heart is collected at game tick 119, with another
301 ticks completed afterward. These are game-tick-entry rates, not rendered
FPS. Candidate SHA-256:
`9d84da1adf923bdeb713b0d4cc7535897634bd9fbc8fee3361317362fe406e1c`.
The separately regenerated SMB ROM remains byte-identical to its baseline.

This checkpoint does **not** stage all VRAM/CHR/CRAM writes or restore the HUD
split. Sprite and background presentation remain follow-up work; a faster tick
counter and clean endpoints are not proof of tear-free motion. Continuous core
captures and summaries are retained locally under
`out/cv1-presentation.ZNyZAL/phase2-r3-{walk,heart}/`.

Continuous review rejected an earlier candidate: an interrupt inside translated
`LDX $20` corrupted the stripe buffer's zero terminator, sending timer digits
vertically through the sky. The lowerer's global A spills `$CB27` and `$CB18`
were not interrupt-preserved. The CV1 bridge now saves both in one reentrant
native-stack word and restores them on every exit. Eight assembled-code tests
cover the producer contract, startup, instruction-boundary interruptions and
nested/line/skip exits. The new interruption tests fail the preserved bad build.

The corrected build has no stripe in the complete checked sky window (0/917
walking callbacks; baseline 0/954). Matched physical-frame neighborhoods show
no clearly new artifact. The walking capture has zero entirely black gameplay
callbacks after tick 60 versus 12 in baseline; the existing scrolling HUD and
Simon/flame overlay persist. This targeted check is not general pixel parity.

The scratch protection is CV1-opt-in to preserve SMB's existing output. A
generic IRQ/lowering fix remains necessary before claiming other profiles are
protected against these shared-lowerer spill races; preserve stack and SMB
RAM/VDP evidence when extending it.

The old fixed-instruction heart script collects the heart without a trap but
now ends in state `$0A/$00`, the courtyard door-transition task, rather than
its old `$05/$06` endpoint. Its fixed-time, indefinitely held Right input is
not a matched-work benchmark after scheduling changes. This is not an old
assertion pass or proof of completing the door transition; use the game-tick
anchored core route for matched heart acceptance. The long traversal still
hits `$E2`/target `$0000` (also present in baseline), so deep-level playability
remains unresolved.

### Next implementation boundary

1. Replace CV1's per-OAM-index sprite-pair cache with shared keys and explicit
   current/next visible-slot pins. Actual walking samples contained only 25
   distinct `(tile, attr & $C3)` keys; 4,438 of 4,801 sampled index-key changes
   reused a key visible in the preceding sample. These are sharing opportunities,
   not measured bake calls or a promised speedup. The earlier idle-only claim
   that sharing cannot help is not supported by this walking evidence.
2. Prepare the 192-byte SAT in unused CV1 buffer space `$C840-$C8FF`; commit
   it with matching sprite base/size in an early, bounded VBlank interval.
   Invalidate on partial CHR writes, never evict displayed or next-frame slots,
   and explicitly blank/rebuild if the old/new slot union exceeds capacity.
   Keep this separate from the existing `$C820-$C83D` handoff record.
3. Stage bounded background/palette work while preserving active and pending
   variant ownership; expose scroll only after its dependencies are ready.
   Restore the HUD split only after real-core deadline evidence. Direct
   `$2007` writes and transient register-6 changes remain live today.

Keep each boundary measured against the same core routes, interrupted-code
tests, physical captures and SMB regression floor. Do not merge sprite-cache
and background scratch allocations independently or treat a queued VRAM write
as though its active tile refcount had already changed.

## Measured baseline: approximately 18 gameplay updates/s at 500%

Ran the installed Genesis Plus GX core directly through libretro in the
existing Docker image, without RetroArch, desktop composition, frameskip,
throttling, or changes to user configuration. Captures come from actual core
video callbacks, not the trace renderer.

- Core: `v1.7.4 a7985a9`; SHA-256
  `a6da7c738dfa87708d173b2034b71b84368d6adf5a53126ebb5791933ce929bd`.
- CV1 output SHA-256:
  `d734663bd284e11d018eafa199486df415a74d23bf771ad4ecf9d4044e02e971`.
  Generated `boot.s` and `ntmap.s` match the checkout.
- Options: overclock `500`, region `ntsc-u`, frameskip `disabled`; the core
  queried and received those values. Defaults were supplied for other options.
- Route: wait for `$18/$19=01/01` (title), wait 120 physical frames, hold
  Start for 30 frames; after `$18/$19=05/06`, idle 180 frames then hold Right
  for 1,020 more frames. Frames below are zero-indexed physical frames.

| Observation, physical frames 2050–2550 | Result |
| --- | ---: |
| Elapsed physical frame intervals | 500 |
| Runtime IRQ counter advances (`$CB04`) | 495 |
| CV1 gameplay tick advances (`$C01A`) | 152 |
| Gameplay ticks per second, normalized to 60 Hz | **18.24** |
| Horizontal scroll advance | 152 pixels |
| Trap marker | 0 throughout the route |

`$1A` increments at NES `$C1E4`, called from the non-lag game path at
`$C0A4`; it is not the runtime IRQ counter. In this stable gameplay window
its observed deltas are only 0 or 1. The wider 1,020-frame walking window
averages 18.65 ticks/s. These are game-update measurements, not host rendering
FPS, and are not directly comparable to instruction-budget trace routes.

A 100% boot reached title state at physical frame 2616 versus 407 at 500%:
overclock is effective. The 100% run missed the short Start pulse and entered
attract mode, so it is **not** a matched gameplay benchmark. An earlier probe
also mistakenly targeted demo state `02/00`; exclude it from gameplay claims.

Temporary evidence is under ignored `out/cv1-reassess.VJSDsu/`:
`probe.py`, `ticks-500/frames.csv`, `ticks-500/summary.json`, and
`play-500/stage-*.ppm`. These are diagnostic artifacts, not a supported tool
or distributable ROM. Recreate the state-anchored route above if cleaned up.

## Why yesterday's evidence was insufficient

1. **The trace “frame cost” is not a complete CV1 gameplay frame.**
   `trace_sms.rs` starts the timer at IRQ injection and stops at the first
   re-enabled interrupt boundary. `boot.s` executes `ei` before calling
   `translated_nmi`. Thus the usual sample covers the presentation/bridge
   portion and excludes the following translated game computation. The
   reported 177k → 143k reduction remains useful for that measured interval;
   calling it total game-frame cost is misleading. SMB's `frame-diff` has a
   different, stack-unwind-based measurement; do not conflate the two.
2. **Snapshots hide intermediate display states.** IRQ injection and image
   capture wait for `IFF1` and the EI delay. IRQ spacing is instruction-based
   (60,000, or 15,000 with `SMS_REAL_PACING`), not cycle/scanline-based. A final
   coherent VRAM image cannot prove a coherent sequence of displayed frames.
3. **Full-frame CPU budget is not the blanking budget.** The runtime waits
   for V-counter >= `$E0`, then performs conversion, SAT upload, column
   projection, scroll application and buffer flushing without an end deadline.
   For NTSC 224-line mode, nominal blanking is only about 8,664 Z80 cycles,
   or 43,320 at 500%, versus 298,680 for an entire overclocked frame. The
   143k trace average is not an isolated upload timing, but it certainly is
   not evidence that the commit fits blanking. Disabling CPU interrupts does
   not stop the VDP scanning the screen.

GPGX's scanline loop and SMS overclock path are visible in the matching
[system source](https://github.com/libretro/Genesis-Plus-GX/blob/a7985a9/core/system.c)
and [libretro source](https://github.com/libretro/Genesis-Plus-GX/blob/a7985a9/libretro/libretro.c).

## Graphics findings and limits

The core captures directly reproduce HUD scrolling/wrapping. This has an
explicit cause: `profiles/cv1.toml` sets `scroll_split=false` to avoid the
previous line-interrupt storm. It is a separate missing behavior, not an
attribute-cache issue. Do not simply re-enable it without fixing IRQ timing.

The runtime's `$CA12` guard prevents presentation re-entry; it does not
certify completion of the producer's OAM, scroll and background state. Heavy
presentation can run on lag interrupts while the game is still producing
its next frame. Together with unbounded VRAM writes, this is a strong
candidate for the motion-only artifacts and wasted work.

The exact reported left-side stale region has **not** been isolated to one
write path. Beam-timed writes, window-edge mapping and host presentation
remain distinguishable possibilities. The headless sequence does not prove
or exclude an additional RetroArch/compositor refresh issue. Next evidence
must compare continuous output and endpoint state, not another isolated
clean screenshot.

## What to take from the SMB hand-port

At hand-port commit `40a160c`, the main loop publishes `FrameDoneFlag` only
after processing game logic. The interrupt skips graphics consumption on
lag frames. It uploads prepared SAT and column buffers with unrolled `OUTI`
blocks, and chooses player-tile streaming or animated-background streaming
instead of doing both. The source annotates scanline costs. These are
directly useful architectural examples, not proof that the same budgets
automatically fit CV1's 224-line mode.
See [the actual implementation](https://github.com/lackoftrack27/Super-Mario-Bros.-SMS/blob/40a160cccc49971712db3d8f2db76aeba6210839/main.asm#L315).

The earlier comparison already identified the handshake but did not make it
the CV1 priority. Page-organized objects and native calls still explain
hand-port speed, but require alias/stack proofs. UxROM itself does not forbid
native far calls; CV1's return-address manipulation makes a blanket switch
unsafe. CHR-RAM similarly rules out assuming all patterns are immutable.

## Revised work order and acceptance gates

1. **Establish timing evidence before optimizing.** Count physical frames,
   game ticks, lag intervals and presentation commits separately. Timestamp
   VDP writes and presentation phases against scanlines. Use cycle-weighted
   profiles per game tick, not instruction-hit percentages or continuation
   label names. Preserve the existing trace as a functional oracle.
2. **Prove and implement a completed-frame handoff.** Identify CV1's actual
   producer boundaries, including resident/nested NMI and its `$1B` busy
   path. Keep game-specific annotations in profiles/runtime. Consume one
   coherent OAM/scroll/background generation once; retain the previous
   display on lag interrupts. Keep required input/audio/NMI service alive.
   A flag alone is insufficient while direct `$2007` writes mutate visible
   VRAM or producers can overwrite staged data.
3. **Separate preparation from bounded display commits.** Prepare tile
   variants and destination-format column/SAT data outside the commit window;
   upload changed data in bounded bursts. Publish scroll only when its
   entering columns and referenced patterns are ready. Large rebuilds remain
   display-off. Account for limited RAM/VRAM: use bounded staging, not an
   assumed spare full framebuffer. Preserve slot-bank and SRAM guard contracts.
4. **Restore the HUD split under real timing.** Fix the pending-line-IRQ
   behavior and verify split positioning on lag frames before enabling it.
5. **Optimize the remaining measured hotspots.** Assess block PRG/SRAM
   access, byteplane sprite conversion, and native-safe call regions. Measure
   sprite-cache misses across walking, whipping, enemies and CHR/palette
   changes; one adjacent idle-frame comparison cannot rule out a shared cache.
   Never remove mapper DI/bank guards without an interrupt-preservation proof.

Each implementation step needs CV1 title, candle/heart, scrolling and
traversal regressions, plus continuous core video and improved game-tick
throughput at fixed overclock. Anchor before/after inputs to game state/ticks
so a speed change does not silently change the route. Target 60 game ticks/s
at 500%; report partial improvements honestly. Full stock-SMS speed is not
promised by this plan.

Shared-runtime changes must retain SMB RAM/VDP goldens (right, bonus-pipe,
death), the 1-1-clear route and real-core playability at its established
overclock. Run workspace tests, format and Clippy before code handoff. The
original assessment changed no runtime, profile, Rust code, canonical ROM or
emulator settings. The later implementation checkpoint and its bounded
regression evidence are recorded at the top of this document.
