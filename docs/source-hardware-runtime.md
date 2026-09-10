# Source hardware and immutable CHR-ROM presentation

Work in progress, 2026-09-09. This extends the accepted
[source-clock](source-clock-runtime.md) and
[banked execution](source-clock-scalability.md) foundations. It is **not yet
an accepted Adventure Island conversion or full CNROM support**.

## One source timeline

`CNROM_SOURCE_HARDWARE_EXPERIMENT` enables the new hardware path and derives
the existing source-clock and CNROM-bus capabilities. Old bus-only, clock-only,
SMB, CV1 and SMB3 paths retain their separate behavior.

The central clock counts original CPU cycles, advances three ordered PPU dots
per cycle, processes source APU timing, then performs the original CPU bus
transfer. SMS interrupts do not advance that timeline. Controllers have separate
physical pending samples, source-live buttons and two falling-edge serial
latches. Reading internal APU status does not refresh the external CPU bus
latch. Guest interrupts use the emulated 6502 stack, not the native Z80 stack.

The new deterministic cold bootstrap starts at CPU cycle zero, PPU line 0/dot 0,
with selected zeroed RAM and peripheral state. Seven reset bus transfers leave
S=$FD, P=$24 and source cycle 7/dot 21. PPU startup write inhibition and an
explicit cold APU reset phase are modeled separately from the old synthetic
epoch. These are declared power-on choices, not every possible hardware divider
alignment. The pinned FCEUX reference has different startup behavior; matching
gameplay requires demonstrated input/state alignment, not a guessed frame offset.

## Source execution and displayed frames

The PPU owns scrolling registers, physical fetch identity, sprite evaluation,
status and frame boundaries. The adapter follows this sequence:

1. Capture immutable CIRAM, OAM, palette and physical CHR mappings at the source
   visible origin. The origin is saved before pre-render tile prefetch advances
   the scroll address.
2. Execute and validate the entire visible interval. A starting snapshot alone
   cannot prove that later pixels use the same context.
3. At a safe completed instruction boundary, prepare and publish the validated
   packet. Source time is held while SMS presentation catches up.
4. Acknowledge only after committed display shadows are complete.

One packet is pending at a time. Host interrupts service the committed SMS
display and pending physical input only. Pause handling is port-free. Native
interrupt yields occur between closed VDP commands; saved software scratch is
not a claim to restore arbitrary half-written VDP commands.

## Storage ownership

| Region | New hardware-path owner |
| --- | --- |
| `$CA80–$CAFF` | Source clock and exact-interval metadata |
| `$C860–$C87F` | Source palette; old capabilities retain their prior location |
| `$D300–$D3FF` | Source PPU pipeline and evaluation |
| `$D400–$D43F` | Pending/committed packet identities and validation state |
| `$D500–$D5FF` | Source APU timing and controller state |
| `$CB30–$CB61` | Shared channel/envelope/PSG state, without a second clock |
| Physical ROM bank 1 | New-path renderer/publisher; bank-zero adapter restores caller mappings |

These allocations displace inactive legacy owners only under the new
capability. Source CIRAM/OAM, optional cartridge RAM, active flag scratch and
the native stack remain separate. Renderer fragments are explicit `.inc`
files so legacy inclusion order and placement remain stable.

## Evidence so far

Independent assembled fixtures cover PPU scrolling/prefetch and physical CHR
reads, sprite-zero and overflow timing, immutable packet retirement, banked
caller/interrupt preservation, serial controllers, and APU sequencing/channel
units. These fixtures do not establish translated instruction integration.

An actual Genesis Plus GX run of the banked literal picture matches all
256×224 displayed pixels across 176 complete stable frames at each requested
100/500 setting, with source time unchanged. An intentional native audio fixture
waits 120 host VBlanks after display preparation before publishing its tone;
the recorded stereo PCM measures approximately 440.399 Hz against the selected
440.397 Hz PSG period. This proves audible output, not Adventure music fidelity.

A subsequent original-6502 fixture writes the hardware registers itself and
publishes its complete source frame 2. Actual-core checks match all 57,344
pixels in 36/5 complete frames at 100/500 before its intentional terminal
diagnostic. Final source state is identical: cycle 87,531, frame 2, line 246,
dot 24. The diagnostic then deliberately alters display colors; those later
frames are not rendering evidence. This is still synthetic code, not a game.

Repeated native fine-scroll transactions pass complete-frame checks at 500.
At 100, each large update triggers the inherited publisher's explicit blank
fallback. The measured 1,823-byte atomic queue needs at least 33,984 nominal
Z80 cycles before final register publication, against a 7,524-cycle guarded
window. Retain the safe fallback and existing pattern capacity for now: 500
is a tested setting for this fixture, not a guarantee for arbitrary packets.
Stable still-image success does not establish nominal-speed motion without
blinking. A second name table would require a different VRAM allocator.

The core delays overclock activation for 100 callbacks. Initial picture
preparation finishes before that delay; it is not evidence of 500%-rate
preparation. The delayed tone does execute after activation. Continuous
source-generated motion and audio remain separate acceptance requirements.

Source-written audio now has a separate actual-core check: a translated 6502
program starts a constant-volume pulse, doubles its pitch and silences it.
Both 100/500 captures measure approximately 440.4/880.8 Hz in stable intervals,
with silence before start and after settling. All source register writes and
PSG publication cycles match independent expectations. This establishes the
source-to-audio path, not envelopes, every channel or Adventure's music/SFX.

## Open correctness and adaptation work

Horizontal composition now covers source fine scroll without the SMS edge gap.
Independent packets cover shifts, distinct neighbor palettes, both nametable
seams, fine Y, layer combinations and independent left clipping. Sprite tests
retain hardware selection occupancy even when prepared pixels are transparent.
Behind-background sprites now preserve first-opaque-sprite ownership before
testing background priority. Independent packet tests cover clipping, seams,
flips, banks and cache/residency transitions. A native actual-core 500 run
switches normal → behind → normal across 228 complete frames with exact pixels,
no intervening blank/mixed frames and no source-time advancement. This does not
establish arbitrary packet budgets or source-driven game sprite behavior.
The constant-context path still rejects unrepresented raster changes and
certain rendering-time register accesses.
DMC/arbitration and narrow APU races require explicit closure, not removal of
diagnostics. Peripheral fixtures do not establish all source-driven combinations.

The current top-224-line viewport is provisional; source coordinates remain
256×240 until the full content/edge audit establishes an appropriate transform.
Existing native Adventure checkpoints already show moving objects in rows
224–239, so treating that strip as disposable decoration is not acceptable.
Top/center crops also remove sampled boss/enemy pixels. The chosen next target
is therefore a separate **PAL SMS-II 240-line** capability, preserving full
source geometry. Independent tests now cover all 240 rows, sprite edges,
blanking-counter phases and every calibrated pulse/triangle period. Actual-core
500 tests match 225/226 complete frames with short/tall sprites across repeated
priority changes, without blank/mixed frames. A separate translated-source
fixture matches all 61,440 pixels and the unchanged NTSC source endpoint.
This is bounded target evidence, not Adventure Island acceptance. Source timing remains
NTSC; the physical display is 50 Hz with explicit backpressure, not a claim of
60 FPS. Two recorded PAL pulse tones match approximately 109.203/216.486 Hz,
including the rounding-before-octave-fold boundary; this is not full music
fidelity. Publication timing assumes bounded intervals between counter samples,
no extra hardware wait states and at most one Pause edge within an atomic
interval. Emulator settings must be scoped to this target,
never applied globally to SMB, CV1 or SMB3.
Simply selecting 240 lines is not a portable NTSC remedy: the
[measured SMS VDP timings](https://www.smspower.org/uploads/Development/msvdp-20021112.txt)
report missing retrace in that mode on NTSC hardware. The same hardware
research explains the exposed backdrop strip under horizontal fine scroll;
the new adapter therefore needs composed edge pixels, not an added crop.
PSG timbre, duty and low-frequency adaptation are distinct from missing sounds.
No tile, sprite, bank, level or audio selector may be silently truncated.

## Adventure Island bring-up

The actual PAL/500 run reaches source frame 300 without traps. Five neutral
guest-state checkpoints match the original NES zero page, control registers
and CHR bank. The same 64 zero-versus-FF power-on differences remain elsewhere
in RAM; their later relevance is not waived. Comparisons wait for the matching
immutable packet to be displayed, rather than comparing its predecessor.
The complete title matches all 61,440 pixels after the declared palette
conversion; earlier checkpoints exclude exactly 505 known emulator-overlay
pixels. No crop, nearest-frame search or image-error tolerance is used.

This game run exposed a generic lowering defect: annotated computed RTS
instructions called the untimed dispatcher, unlike ordinary timed RTS. The
fix changes only the timed helper selection. An original-instruction regression
reproduces the missing three bus reads before the fix and verifies all six
afterward; the legacy untimed path remains unchanged.

Stable title fidelity does not establish coherent motion. A separate census
finds 90 one-callback uniform-blue fallback frames during title updates at 500;
the prior and subsequent complete images are intact. The first failure has
17 queued transfers totaling 3,596 bytes and reaches the guarded PAL deadline.
The largest observed queue contains 7,990 bytes: its payload alone exceeds
the guarded 500 blanking window. Upload scheduling and VRAM residency need
further work; reducing dispatcher overhead alone cannot close this failure.

An input-only actual-core route now enters level 1, moves right, jumps, lands,
and consumes button release without traps. It finishes at source frame 527;
Start promotion, serial consumption, gameplay call-site identity and player
state are observed directly. No memory patches or imported save states are
used. This is a first-level control checkpoint, not a level clear, exact moving
pixel/audio comparison, full-game completion or complete music/SFX support.
The measured gameplay interval advances only about **0.136 source frames per
second** at PAL/500, using the core's output clock, not accelerated headless
wall time. Normal-speed playability remains far from established.

There is also a physical-hardware qualification: active-display VRAM has its
own bandwidth limit, independent of CPU overclock. Hardware tests report
corruption with writes spaced more closely than about 26 nominal CPU cycles;
the pinned GPGX SMS writer does not model that contention. Its successful
captures therefore do not establish real-SMS safety of rapid active uploads.
[Hardware timing tests](https://www.smspower.org/forums/16298-VDPTimingConstraints)
and [the selected core's writer](https://raw.githubusercontent.com/ekeeke/Genesis-Plus-GX/162c343/core/vdp_ctrl.c)
define this distinction. Exact encoded-pattern sharing and residency were
measured before choosing a hardware-oriented approach; no emulator-only raster
adaptation is enabled.
The first exact-byte census supports a hardware-oriented alternative: two
sampled old/new picture pairs fit 247/342 patterns in a legal 378-slot pool
with two name tables and aligned sprite pairs. This preserves every referenced
pattern byte and table attribute. A separately opted-in canonical-pattern
allocator and double-buffered name tables are now being implemented. Identical
32-byte patterns share storage; aligned sprite pairs and a permanent transparent
tile retain explicit ownership. Preparation may write only unused patterns and
the inactive table; the final bounded switch preserves the old picture until
the new one is ready.

The atlas primitives are implemented and independently exercised: seven
assembled fixtures drive the cold/begin/intern/resolve/retire gates directly,
covering the permanent zero and blank double tables, dedupe and hash-collision
chains, the dense-to-physical mapping at each legality boundary, capacity
fail-closed at 378 slots, two-generation retirement with exact stale-chain
unlink on eviction, and caller bank restoration. Aligned 8x16 pair interning
is admitted: one 64-byte candidate lands on even sprite-addressable ordinals
192..376 (physical 256..506, even-aligned and contiguous across the 375/376
seam), singles and pairs share chains but are separated by kind flags, and
expired singles or pairs inside the window are reclaimed with exact unlinking.
The bank gate was rewritten without IX so the verified tracing core — which
intentionally rejects DD/FD opcodes — can execute it; the original gate had
never run. Gate results are HL/DE only; A and flags do not survive. The publisher integration is now implemented and measured. The renderer
interns every converted pattern (NT entries carry 9-bit canonical physicals;
SAT tiles are pair physical low bytes), the legacy key cache persists as a
key-to-ordinal map reset exactly on atlas evictions, pattern and name-table
uploads run deadline-free against the inactive table, and the atomic window
shrinks to SAT + CRAM + registers + one reg-2 flip. Slots became pure cache
entries, so OLD_LIVE stays empty under the atlas — dense allocation preserves
the unlink invariant across eviction resets (the first actual-core run
trapped exactly there). Expired pairs are harvested by single allocation
(breaking the pair and freeing the partner), and when a displayed/pending
pattern union genuinely exceeds the 378-slot pool — Mode 4's pattern space
is fully allocated, so the pool cannot grow — the allocator releases the
displayed generation once per packet and the publisher blanks that single
frame before any upload; a second exhaustion still fails closed.

Ten assembled fixtures cover the primitives plus a full three-packet
publisher run (pixel-exact on both alternating tables, no fallback after the
first publish, no re-allocation for identical packets) and the
eviction-reset, pair-harvest and capacity-release regressions. The actual
Genesis Plus GX PAL/500 neutral title route now completes to source frame
300 with no trap: all five in-range guest-state landmarks match with zero
zero-page differences, every one of the baseline's 240 distinct displayed
images reappears pixel-identical, and the 90 single-frame blank fallbacks
shrink to 16 short episodes (41 callbacks) confined to title-animation
phases whose old/new pattern unions exceed the pool. The input-only first-level route also completes on the atlas build with
byte-exact guest endpoints (player X 96, camera 367, terminal Y 152,
source frame 527). During the gameplay span the baseline's 12 spurious
single-frame blanks disappear entirely — the only remaining blank episodes
are the source's own display-off level loading, present in both runs at
matching lengths. The route baseline was captured on a different pinned
core build, so this comparison rests on guest endpoints and the uniform
frame census, not pixel-hash identity. Rendered frames from the first-level GPGX route confirm coherent gameplay
visually, not just at the RAM endpoints: the forest background, ground
band, HUD score/health, fruit pickups and Master Higgins all render
correctly, with the sprite in its walk pose on the ground and its jump
pose airborne as the scene scrolls — the atlas double-buffered publisher
holds through motion. This is title and first-level route coherence
evidence (guest-state + uniform-census + visual inspection), not
full-game acceptance, exact gameplay pixel-hash parity against the NES
oracle, normal-speed playability or a capacity guarantee for other games.
Remaining first-level acceptance gates: a pixel-parity comparison of the
gameplay route against FD_NES_DUMP ground truth (title is pixel-exact;
gameplay is not yet), and a level-clear route beyond the current
enter/move/jump/land control checkpoint. Byte-identity gates are checked with
the SDSC header's build-date byte and checksum masked; the assembler stamps
the UTC build date at $7FE6, which is the only bit of nondeterminism.

## Measured execution prerequisites

Hardware-mode acceleration cannot inherit the earlier quiet-loop proof. Every
active PPU/APU/input/interrupt domain must participate in deadline selection and
precise-versus-summarized state/event comparison before fast execution is enabled.
The precise Adventure bootstrap's exclusive PC profile attributes at least
51.55% of approximate opcode cycles to PPU routines, 19.36% to source-clock
work, 8.20% to bus routines and 5.84% to the APU. These are the top 40 ranges
(84.95% of total), not inclusive call costs or steady-game FPS. This makes
ordinary-instruction event batching necessary alongside idle-loop summaries.

The separate `CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT` opt-in retains every
original CPU transfer and interrupt poll while
deferring qualified PPU/APU work inside a single scanline. Source cycles
remain current; hardware state is materialized at `source cycles − pending`.
Hardware accesses, deadlines, interrupt entry and diagnostics synchronize that
state before observing it. This is not permission to skip rendering events or
reuse the old quiet-loop proof. Trace consumers must explicitly acknowledge
the versioned coordinate format rather than read stale PPU positions.
The initial blank-only `deferred-v1` implementation reduced the fixture's
cold-start time by approximately 14%/13% at 100/500. The rendered-domain
`deferred-v2` extension also materializes qualified background/sprite fetch
intervals. Four precise/deferred source fixtures match every transfer and
normalized hardware state; the rendered case additionally matches 81,940
physical PPU fetch events and all 104 live PPU bytes at each span endpoint.
Its actual-core completion improves from 6,322 → 4,405 callbacks at 100 and
1,285 → 922 at 500, approximately 30%/28% less cold-start time. All available
complete pre-diagnostic images match, with identical final source state.
The faster 500 case offers three complete frames before its intentional
diagnostic, versus five in the precise case; neither run is gameplay FPS or
continuous-motion evidence.

The explicitly acknowledged `deferred-v3` format additionally summarizes
profile-verified `LDA zp / BNE` waits. Its proof covers the previous completed
branch, all PPU/APU deadlines, pending work and already-asserted unmasked IRQs;
it retires only complete six-cycle iterations, at most 18 within one row.
Independent expansion matches 86,996 CPU transfers and 81,940 PPU fetch events
against fully precise execution. In a separate mixed setup/wait fixture,
ordinary → fast actual-core callbacks fall from 4,392 → 3,845 at 100 and
919 → 814 at 500, with identical guest/source endpoints. This approximately
12%/11% reduction is not a game-speed claim and must not be added directly to
percentages from a different workload.

A measured materialization-speed campaign (2026-09-10) rewrote the hottest
deferred-path loops with exact-equivalence proofs at every step: division
tables, an analytic then per-sprite-batched evaluation walker, per-slot
sprite-window replay, and a register-only per-sprite overflow predictor
that eliminated the backup copies entirely. Actual-core callbacks to
source frame 300 fell 101,167 → 62,807 (−38%) across five commits, each
gated on the 104-byte endpoint-equality fixtures, the literal overflow
dots, exact guest-state landmarks and untouched game baselines. The
post-campaign profile is flat: about 21% is per-instruction verified
clock/bus dispatch and 13% the precise 16..23-dot span suffix; no single
remaining range exceeds 4.1%. Interactive speed cannot come from further
micro-optimization of the interpreter — the remaining gap to nominal is
orders of magnitude, and closing it requires graduating verified spans of
translated code to native execution between hardware-visible events, the
coordinated clock/APU/bus/span design this document already names as the
measured execution prerequisite.

The immediate gate is genuine source-generated frames and interrupts, followed
by matched Adventure checkpoints and the full
[content and mapper acceptance queue](mapper-roadmap.md).
