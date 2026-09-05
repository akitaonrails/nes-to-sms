# CV1 performance and presentation reassessment

Date: 2026-09-05. Assessed runtime: `2123ef8`. This is a diagnosis and
proposed work order, not an implemented scheduler fix. Keep the recent
sprite-palette and CHR-RAM materializer fixes; their static correctness and
cost improvements do not establish smooth real-emulator motion.

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
overclock. Run workspace tests, format and Clippy before code handoff. This
assessment changes no runtime, profile, Rust code, canonical ROM or emulator
settings, and does not claim a new SMB regression-test pass.
