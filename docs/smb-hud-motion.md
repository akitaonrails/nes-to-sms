# SMB moving-HUD regression investigation

## Claim and verification plan (2026-09-07)

Keep SMB's status text fixed during scrolling, without moving the playfield
to the HUD scroll or changing game state. Static end-of-frame checkpoints
cannot establish this: acceptance needs consecutive actual-core video frames.
CV1's separate coherent presentation path must remain unchanged.

The initial read-only capture uses the installed, unmodified GPGX core at
numeric overclock `500`, NTSC-U, frameskip disabled. Temporary input/capture
scripts and native video frames are under `out/smb-hud-motion.s67Vzq/`.
This first route entered attract-mode scrolling, not player-controlled play.
The later `player-before-*` and `player-repeat-*` captures use SMS button 2
(libretro A) to start SMB correctly and verify gameplay mode in RAM.

The canonical SMB, pre-aligned-indexing baseline, and aligned-indexing candidate
each show 196 moving-camera frames whose static text row (pixels 16–23) differs
from its stationary reference. All have overrun counter zero. Thus the observed
fault predates the latest indexing optimization. Their manifests record ROM,
core, capture-script hashes and options.

Initial hypothesis: column projection delays `_apply_frame_scroll` past VBlank,
triggering its direct-playfield fallback. Moving the call before projection
reduced 196 faulty attract frames to 48, but changed some playfield frames
incorrectly. That intermediate candidate was rejected.

## Correction

Three separate timing faults explained the motion-only displacement:

1. Install the split **before SAT/column preparation**. Freeze the corresponding
   playfield X; do not let a subsequent producer move it before the line IRQ.
2. Retain SMB's last captured pre-scroll when its next NMI has not rewritten
   the pre pair yet. The per-NMI valid flag is transient; a missing flag does
   not mean the live playfield latch is a suitable HUD scroll.
3. Poll the split between completed graphics transactions and before the
   pacing acknowledgment. When pacing consumes a physical VBlank, rearm the
   old committed HUD without publishing a newer playfield scroll.

The old active-display guard remains. SMB no longer suppresses the split for
60 frames after an overrun: blocked splits and consumed VBlanks now have
explicit service paths. Other profiles retain their existing behavior.
The change adds no per-frame scanline wait, no guest RAM writes, and no bank
or interrupt-enable changes inside the polling helper. `$D472-$D473` uses two
retired IRQ-save bytes, documented in the master RAM contract.

## Actual-core motion results

Each run captures 3,000 consecutive physical frames. Start is held at frames
450–469; Right + Run begins at 650; Jump is additionally held when
`frame % 100 < 45`. No inventory, RAM or emulator timing is patched. At frame
675 the camera is stationary and gameplay is active; its static labels
(all pixels y16–23) and world number (x144–175, y24–31) form the references.
Count moving gameplay frames only: `$0770=1`, `$0772=3`, camera low byte nonzero.
Both independent regions produced these same mismatch counts:

| Numeric overclock | Before | Corrected |
|---|---:|---:|
| 500 | 182 / 1,154 moving frames | **0 / 1,152** |
| 300 | 1,503 / 2,008 moving frames | **0 / 2,005** |

These are pixel comparisons from actual video callbacks, not static trace
renders. Physical-frame input anchoring means small scheduling differences can
change later collisions/deaths; these counts are not a speed benchmark or
proof of identical full-screen pixels. Early traversal retains the same tick
advance over frames 675–800 at each clock (125 at 500, 104 at 300). This does
not claim full native-SMS speed or certify every level/overclock.

Corrected ROM SHA-256:
`b3e14180f904182882a40da5c1d7be4ff778e4f0518e4ddcf8de250ee23211b0`.
Core SHA-256:
`a6da7c738dfa87708d173b2034b71b84368d6adf5a53126ebb5791933ce929bd`.
Captured output: `player-repeat-500/`, `player-repeat-300/`; native frame
buffers, RAM CSV, provenance manifests and `metrics.json` remain ignored.

Evidence owner: the implementing agent. Completed verification scope:

- Repeat actual-core capture; inspect static HUD rows across moving frames,
  including player-controlled scrolling and the user's clock when known.
- Compare moving playfield pixels and game-state progression; investigate any
  difference rather than assuming a steadier HUD implies correct presentation.
- Run existing SMB differential/acceptance routes and variant-cache invariant.
- Rebuild CV1 and establish byte identity for this SMB-only runtime change.
- Run workspace tests, formatting and lint checks before handoff.

Promotion requires all these checks to pass and preservation of the old
canonical ROM and matching symbols for comparison or rollback.

## Regression evidence

- Three new assembled-code tests cover early installation with missing pre
  validity/previous overrun, frozen one-shot polling and register preservation,
  and rearming consumed VBlanks without restarting pre-scroll during active
  display. All **13** generic NROM/UxROM IRQ tests pass with local assembled
  projects. Run with `IRQ_NROM_PROJECT=<smb-project>` and
  `IRQ_UXROM_PROJECT=<cv1-project>` using `cargo test -p nes_to_sms --test
  generic_irq_spills -- --ignored --nocapture`.
- The 301-million-step 1-1-clear trace passes its no-trap, terminal RAM and
  `SMS_EXPECT_BGV_CONSISTENT=0` gates. All seven checkpoint PPMs are byte-identical
  to the accepted aligned-indexing baseline, including flagpole and World 1-2.
- CV1 rebuild is byte-identical to the playable ROM, SHA-256
  `d4e3e0fe60b5b0df48531b2e48ec5134c3a551e284ecd03d3477ae36d06b4497`.
- Workspace tests, formatting and Clippy pass; existing lint warnings remain.
- Final 4,500-frame clear, 3,000-frame pipe and 3,300-frame death/game-over
  differential checks all report **NO DIVERGENCE** against the NES oracle.

## Handoff

The corrected project is regenerated/assembled at `out/smb`; ROM and symbols
must match `final-smb/` under the evidence directory. The old canonical ROM
and matching symbols are preserved at `canonical-backup/` there (old ROM hash
`3e80e670e23e992855c0ceb1f847bb267a39b89a47d2f17f0bc933357e27d1cc`).
Reload the ROM in the emulator; an already loaded game retains its old bytes.
No emulator settings were changed. The user's exact launch path/clock was not
confirmed, so the measured acceptance is limited to the named ROM/core and
300/500 settings above.
