# CV1 performance and presentation reassessment

Date: 2026-09-05. Assessed baseline: `2123ef8`. The original diagnosis and
work order follow; implementation evidence is recorded below. Keep the recent
sprite-palette and CHR-RAM materializer fixes; their static correctness and
cost improvements do not establish smooth real-emulator motion.

Next optimization work is scoped in
[the clock-scaling findings and ranked experiments](cv1-next-performance-experiments.md).
That follow-up keeps this delivered ROM unchanged and separates measurement
gaps from proposed SMS/Z80-specific optimizations.

## Dagger follow-up

The dagger freeze was a strict unresolved-code trap, not another rendering
stall. Original `$DBA4: BEQ $DB8D` reaches a shared projectile initializer;
adding that verified discovery root to the CV1 profile resolves the path.
No runtime, renderer or generic engine code changed.

At 500%, the same 420-tick routes now measure 19.256 walking and 21.529
heart-route updates/s (1,307/1,169 physical intervals; maximum gap still 17).
These are essentially the previous cadence, not a speed optimization.
Fixed-HUD checks report zero outliers in 1,126/988 callbacks, with no whole
blank frames or recurrence of the checked outdoor stripe. The freshly built
SMB ROM remains byte-identical to its accepted regression artifact.

Real-input dagger tests complete 1,306 indoor ticks after entering the room.
A repeated-throw variant spends hearts on three throws, including airborne
throws, and continues to world X 745 without trapping. Original-ROM oracle
tests cover the initializer's initial/active branches and discovery ownership.
Local evidence: `out/cv1-dagger.SuwNRt/`; current normal ROM SHA-256:
`9b236c7772cafa3c3a9a766ddfed5cb7f611c8bd03ce9074c5d56badc87e695d`.

## Coherent background and HUD delivery

The CV1 profile now enables `CV1_COHERENT_BG`. Raw PPU writes accumulate
intent; a bounded preparation pass builds nametable, palette, HUD and sprite
output before publishing it together. Exact current/next tile ownership stops
visible patterns from being recycled. Large scene changes explicitly blank
and rebuild; ordinary entering columns do not require blanking.

Matched 420-game-tick routes on the same GPGX core, NTSC-U, numeric overclock
`500`, with frameskip disabled:

| Build | Walking updates/s | Heart-route updates/s |
| --- | ---: | ---: |
| Previously installed handoff build | 19.0808 | 20.9730 |
| Fastest intermediate, background/HUD faults retained | 21.9039 | 24.7712 |
| Coherent background/HUD + bounded audio service | 19.2855 | 21.5107 |

The coherent build takes 1,305/1,170 physical-frame intervals. This is a
small improvement over the installed build, but a real speed cost against
the fastest intermediate; it is **not** full speed. State/area/hearts match,
walking positions differ by at most one pixel, and heart-route landmarks
match exactly. Pickup occurs at tick 119 with 301 subsequent ticks. The
longest physical-frame gap is 17, versus 7 in the fastest intermediate.

Every captured gameplay callback after tick 60 has the expected fixed
96×48 left-HUD region: zero outliers in 1,127 walking and 992 heart samples,
and no entirely black/white frames. This is a targeted continuous check,
not general NES pixel parity. Palette/priority limitations remain.

Sparse background scanning and batches of eight hidden sprites reduce
repeated scheduling work. Two larger alternatives were rejected on evidence:
previous compact sprite positions predict too few expensive cache hits, and
scene-transition game code can write raw graphics while unfinished preparation
still reads them. Overlapping those operations without a new source-lifetime
protocol would reintroduce mixed frames.

The complete maximum graphics transaction uses 7,725 exact Z80 T-states;
its latest allowed `$E3` admission leaves 7,752 at stock NTSC224 timing.
These are executed-opcode bounds, not the tracer's approximate cycle count.
The physical HUD split uses committed controls, including repeated frames.
Audio now services the HUD between bounded sequencer stages while retaining
audio state/PSG ordering, interrupt exclusion and the native-stack floor.
Previously its uninterrupted IRQ tail could carry the split into the playfield.

A separately instrumented real-core walking run passes at both 100% and
500%: 14,636 observed stock-clock HUD services finish on lines 38–44, and
1,284 overclocked services finish on line 38. Neither run has missing observed
physical HUD epochs or detected active publication/slot-ownership violations.
Two stock late-rearm counters occur during an explicitly blanked rebuild,
without an active split. The diagnostic uses a 30-record packet limit to
fund its instrumentation; normal output retains 32. These are bounded timing
checks, not normal-ROM speed measurements or stock-speed playability claims.

Shared IRQ protection now covers `$CB15/$CB18/$CB27` for NROM as well as
UxROM. Physical controller polling no longer rewinds a partially read serial
transaction; the guest's `$4016` strobe owns that reset. This is not a new
implementation of the complete NES controller protocol.

Pre-dagger checkpoint ROM SHA-256:
`61a6d2bf280a2f85b991f4e6915a2b9c0ff328b2507e0b7c24e427a95ac16064`.
The final lifetime repair below retains **byte-identical callback CSVs**,
including every pixel hash, state and frame interval, on both 420-tick routes.
Local evidence: `phase5-final-{walk,heart}` and the preceding
`phase4-final-{walk,heart}` under `out/cv1-presentation.ZNyZAL/`.
The freshly generated SMB artifact retains
SHA-256 `ac0d63bc0b39f2d2f03f2934974232da65dfb7a24a33db5e3f6de74062ccafc5`:
unchanged RAM/VDP goldens, the World 1-1 clear and seven identical checkpoint
images, and the real-core 300% smoke remain valid.

## First door and indoor return-stack repair

The unchanged long route exposed a later `$E5` trap indoors. After the game
consumed a return with `PLA; PLA`, an interrupt could legally reuse those
freed guest-stack bytes before the old helper validated them. Original NES
execution with injected lag NMIs confirms the lifetime error. The profile
now identifies the first PLA with `consume_at`; validation and the single
software-frame retirement happen while the bytes are live, before either
pop. The original guest instructions remain intact. Missing, stale, bypassed
or replaced hooks fail generation.

Both final actual-core 500% routes pass the original strict endpoints:

- Normal: ordered first-door animation and room setup, then 1,306 indoor
  game ticks; confirmed endpoint world X 723–724, at least 674 pixels beyond
  its confirmed starting window, with no trap.
- Diagnostic: 78 validated pre-PLA ownership transfers; after the first
  observed transfer, another 124 ticks and 80 confirmed pixels without a trap.
  This separately instrumented result is not a normal-ROM speed measurement.

Actual assembled tests cover 50 interruptible instruction boundaries and
reproduce the failure on the old ROM. The repaired helper adds 8 exact
T-states, no native-stack word, and retains the `$DE40` floor. Its local
interrupt-service bound, including HINT handling, is 844T versus a 2,052T
allowance; the diagnostic is 874T. Existing graphics commit, IRQ-prefix and
HUD bounds are unchanged. This does not establish universal indoor beam
timing, stair ascent, a stage clear or boss support.

Evidence: `phase5-{normal,diagnostic}-traversal-r2` under the same local
artifact directory. [Acceptance commands](../profiles/cv1/acceptance/README.md)
include the separate long routes and the opt-in
[functional tracer](functional-video-trace.md), whose synthetic clock is
explicitly not a performance oracle.

The following sections retain historical checkpoint evidence and the original
work order; their unresolved-status statements are not the current work queue.

## Historical shared-sprite scheduling measurements

The next implementation uses shared `(tile, attr & $C3)` sprite slots with
separate current/next pins, a prepared 192-byte SAT, and deferred sprite
register writes. Partial CHR writes invalidate keys without recycling displayed
slots. A late commit retains its packet and returns; subsequent `$2007` writes
must first finish that packet, preserving graphics order. New full NMIs cannot
overwrite a pending packet, while valid lag/input/audio work remains enabled.

Matched 420-tick routes at the same GPGX `a7985a9`, NTSC-U, overclock `500`:

| Route | Handoff updates/s | Shared-sprite updates/s | Change |
| --- | ---: | ---: | ---: |
| Walking | 19.0808 | 21.9039 | +14.8% |
| Walking + whip/heart | 20.9730 | 24.7712 | +18.1% |

These isolated sprite measurements use ROM SHA-256
`e4e2e7dabf6e236dd9953f6095221eb346f64ef9f6cceba68f6201af858fad5c`.
The integrated current-profile build
`3737e02afcfc53a9dab887acaeccf6718950a0e2619c20a45d3a7ddfd46797df`
passes both routes with identical per-physical-frame state and pixel hashes.
The freshly regenerated SMB ROM remains byte-identical to its baseline.
State/area/hearts match; position differences stay within one pixel. Pickup
occurs at tick 119 with 301 subsequent ticks. Maximum physical-frame gap is 7
versus 6 in the handoff checkpoint: the mean improves, not every worst case.
Earlier scheduling/lookup candidates were rejected because actual cadence
regressed despite lower helper instruction costs.

Assembled execution bounds the full admitted SAT/scroll/register wrapper at
3,921 real Z80 T-states. Its latest permitted `$EC` counter value leaves
4,332 T-states at stock NTSC224 timing. This is not the tracer's approximate
cycle count. Actual-core sprite diagnostics at 100/500 stayed in blanking;
the 100 run covers boot, not matched gameplay. Direct background/CRAM writes
and the scrolling HUD remain separate work, so this is not tear-free output.

Continuous callback review found no recurrence of the prior narrow sky stripe:
0/813 walking and 0/875 heart samples; no completely black/white gameplay
callbacks after tick 60 (1008/875 checked). These targeted checks do not imply
pixel-perfect NES parity. Evidence: local `phase3-p9-{walk,heart}` directories
under `out/cv1-presentation.ZNyZAL/`.

The deep-route stack error also has a confirmed independent cause: NES
`$EE9F/$EEA0` consumes a materialized return, then the translated RTS used to
consume it again. A profile-declared already-consumed escape validates the
ownership and original bytes before retiring the software continuation.
Canonical 6502 and actual assembled-code tests establish the repair; the
original build fails the same test. A separate real-core probe completes the
first door and 124 indoor game ticks. Neither that probe nor a trap-free
instruction-timed outdoor soak establishes full-stage completion.

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

### Original implementation sequence

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

## Original work order and acceptance gates

This was the implementation plan. The delivery and measured limitations at
the top of this document supersede it: the 60-tick/s target was not reached.

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
