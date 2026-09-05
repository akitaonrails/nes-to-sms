# CV1: next performance experiments

Date: 2026-09-05. Checkpoint: `d667ed3`. This is a measurement/research
follow-up, not a new optimization or a full-speed claim. It refines step 5 of
[the performance reassessment](cv1-performance-reassessment.md).

## New evidence: clock scaling, not just another screenshot

Six actual GPGX runs used the unchanged dagger-fixed ROM, frozen 420-tick
walking/heart contracts, NTSC-U and disabled frameskip. Inputs remain anchored
to game ticks. Each column below measures game updates, not rendered FPS.

| Numeric overclock | Walking updates/s | Heart updates/s | Physical intervals, walk / heart |
| --- | ---: | ---: | ---: |
| 250 | 5.0235 | 5.5545 | 5,010 / 4,531 |
| 375 | 14.7007 | 16.6452 | 1,712 / 1,512 |
| 500 | 19.2560 | 21.5291 | 1,307 / 1,169 |

All six pass the unchanged route requirements. Across clocks, state, area,
hearts and player Y match all landmarks; position differences are at most
one pixel. Heart collection remains tick 119. The additional host-only
RAM/SRAM observer changes neither guest instructions nor inputs: both 500%
`frames.csv` files are byte-identical to the preceding unobserved runs,
including every callback's pixel hash.

Walking tick-to-tick physical gaps after tick 60 are 1–5 frames at 500%,
2–7 at 375%, and 6–19 at 250%. The reported whole-route maxima of
17/23/35 count consecutive **nonadvancing callbacks**, including startup;
they are not these post-60 inter-tick gaps.

The 250→375 speed increase is approximately 3× for 1.5× the requested
clock. This establishes nonlinear throughput, **not its cause**. Recurring
interrupt work, admission windows and lag-dependent game paths need separate
accounting. GPGX implements SMS overclock through reciprocal per-instruction
cycle scaling, with integer rounding and a startup delay; do not infer a
precise CPU-work budget by multiplying elapsed frames by the menu value.
See the matching [frontend](https://github.com/libretro/Genesis-Plus-GX/blob/a7985a9/libretro/libretro.c#L1324)
and [Z80 timing implementation](https://github.com/libretro/Genesis-Plus-GX/blob/a7985a9/core/z80/z80.c#L202).

Stopped-core flags provide a useful locator, not CPU percentages. In walking
ticks 60–419, `(busy=1, ready=0, guard=0, BG phase=0)` occurs in 3,016/4,380
callbacks at 250%, versus 277/1,125 at 500%. This directs the next probe
toward the translated body and its interrupts; it does not prove that graphics
are cheap, since frame-boundary sampling can miss whole preparation passes.

## A profiling error to avoid

A separate 60-million-instruction functional trace assigns 9.71% of its
**approximate** cycles to `_tr_cont_66`. Inspecting the generated instructions
shows that label aliases original `$C030`: update `$6F` using `$1A` and a
fixed-ROM table, then loop. It is not continuation bookkeeping. Its repeated
table reads also contaminate a whole-run mapper-read total.

This trace includes boot and an old instruction-timed input script; it is
excluded from steady-gameplay rankings. Its default SMB-named RAM printouts
are not CV1 state evidence. Removing the loop or replacing it with HALT is
not semantics-preserving: it writes game-visible state. A shorter idle loop
also need not increase game throughput.

## Ranked experiments

### 1. Account for work across one complete game update

Before choosing an optimizer, measure exclusive costs for translated game
instructions, software-call machinery, mapper transactions, graphics
preparation, admission polling, graphics commit, and interrupt/audio service.
Gate collection to game state and ticks 60–420; separate boot, outdoor and
indoor/throw phases. Use bank-qualified instruction spans, not the nearest
continuation label. Count calls, mapper changes, cache misses, admitted/rejected
chunks, publication and consumption alongside costs.

Preferred next affordance: a temporary instrumented build of the matching
GPGX core, with host-side accounting around instruction execution and IRQ
entry/exit, leaving the guest ROM unchanged. Account for nested service,
HALT, taken/repeating instruction costs, and frame-counter rebasing. Compare
every normal-core callback against the instrumented-core run before trusting
its attribution. Keep nominal Z80 T-states separate from scaled core clocks.
The existing functional tracer is a weaker candidate finder, not a substitute.
No new core dependency or permanent profiler was introduced in this assessment.

### 2. Exploit Z80 addressing and block operations in proven regions

The current generic indexed RAM-read emitter already inlines address
calculation; proposing that again would duplicate completed work. Its ordinary
eight-instruction sequence costs **44 nominal T-states**, excluding flags and
surrounding code. If analysis proves no low-byte carry, dropping the three
high-byte repair instructions gives **29T**. For an aligned base, loading H
with the page and L directly from the resident index gives **18T**.

Start with hot original-code regions whose index bounds and RAM mirrors can
be proven. Differential tests must cover bound endpoints, page crossings,
zero-page wrapping, alias writes, live flags and interrupts. Unknown bounds
keep the conservative path. This is smaller than transposing all object RAM;
that larger change needs whole-program alias/access closure, not observed
indices alone. Generic Rust must not acquire CV1 addresses.

For mapped tables or contiguous uploads, count **transactions per byte**.
An SMS-native bounded kernel in fixed code can map once, process several
bytes with HL/DE/BC, and restore once. A table copied into an executing code
bank can avoid mapping altogether where pointer provenance is proven. Include
packing effects, ROM growth and IRQ latency; never simply remove DI or assume
slot 1 survives an interrupt. Existing fixed-high reads already preserve the
selected UxROM bank in slot 2.

### 3. Use native CALL/RET only inside proven stack-safe islands

SMB already uses the native discipline. CV1's mapper is not itself the
obstacle: its materialized returns and return-consuming paths are. First
count hot call edges after excluding idle work. Then seek a closed region
without stack observation, indirect escapes or unproven callees. A native
interior still needs a correct software-stack boundary and bank restore.
Test original/translated RAM, flags, guest S, live return bytes, nested IRQs,
and the native `$DE40` floor. The dagger and indoor return-escape routes are
mandatory gates, not optional follow-ups.

### 4. Spend ROM on graphics conversion only where provenance permits

The hand port stores destination-ready data and uses page-organized objects
and unrolled transfers. Its completed-frame handshake and prepared SAT are
already reflected in our current CV1 driver; those are no longer new wins.
See its [actual implementation](https://github.com/lackoftrack27/Super-Mario-Bros.-SMS/blob/40a160cccc49971712db3d8f2db76aeba6210839/main.asm#L315).

SMS background entries carry hardware flip and palette-selection bits;
sprites have no corresponding per-entry flip bits. Therefore “let the VDP
flip NES sprites” is not available. The 16-KiB shared VRAM and one sprite
palette also rule out assuming unlimited resident variants or a free second
complete scene. These constraints follow from
[Sega's software reference](https://www.smspower.org/Development/SMSOfficialDocs).

First census actual pair builds and CHR mutations across walking, hearts,
throws and the door. `runtime/sat.s` still recomputes row offsets and tests
palette/flip choices inside its 16-row pair loop. Specialized row kernels can
hoist those choices without assuming immutable CHR. Pre-expanded ROM variants
are a larger option only for proven source streams, with an exact fallback
after partial CHR writes. Require byte-identical converted pixels for every
palette/flip combination, transparency, both halves and partial writes.
The previous warm-slot-hint experiment was rejected; do not substitute its
idle hits for a moving-route cache census.

### 5. Small bounded transfer improvement, not a full-speed strategy

`_cv1_bg_copy` copies 1,408 metadata bytes in 22 fixed 64-byte chunks when a
changed cell requires it. Replacing each LDIR with 64 LDIs would change its
transfer body from 1,339T to 1,024T: **6,930T gross saved per complete copy**,
not per gameplay tick. One inline site grows by 126 ROM bytes; shared blocks
add call overhead. Preserve flags, addresses, ownership and chunk deadlines.
The arithmetic uses [Zilog's LDI/LDIR timings](https://www.zilog.com/docs/z80/um0080.pdf#page=144),
not the tracer's approximate cycles. SAT upload is already unrolled OUTI.
Measure copy frequency and scheduler thresholds before pursuing this small win.

## Evidence, reproduction and acceptance

Local, ignored evidence: `out/cv1-performance-research.RNaF5H/`, containing
six summaries, callback/phase CSVs, `analysis.txt`, the one-off `analyze.py`
and `functional-profile.log`. The reused observer is
`out/cv1-presentation.ZNyZAL/core_phase_probe_occupancy_frozen.py`
(SHA-256 `881ac9b1316ebd3f94ec75bf2e37121c77d494639bce0eb37367c64f01a606d2`).
ROM SHA-256 remains
`9b236c7772cafa3c3a9a766ddfed5cb7f611c8bd03ce9074c5d56badc87e695d`.
Core/profile/runner hashes are recorded in every summary. No assets are
committed. Reproduce the clock sweep with the supported runner:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_route.py out/emulator-host/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-routes.toml \
  walk out/cv1/acceptance/research-walk-375 --overclock 375 --capture-every 1
```

Use fresh output directories, both routes and 250/375/500. Cross-clock runs
are sensitivity experiments, not optimization A/B comparisons; the normal
`--compare` correctly rejects unequal core options. Compare candidate builds
at identical settings and preserve the frozen landmark tolerances.

Verification owner: the implementing agent. Budget: one gated attribution
capture per distinct workload, focused original/assembled tests for the chosen
mechanism, then normal-core A/B on both 420-tick routes and indoor throws.
Report mean throughput **and** gap distribution. Re-run continuous HUD/stripe
and ownership checks, with stock-clock deadline tests for changed helpers.
Shared-code changes additionally require SMB RAM/VDP goldens, 1-1 clear and
real-core smoke. Unchanged artifacts can reuse their existing evidence.

This assessment passes all 35 host observer tests. Runtime, profile, generated
ROMs and emulator configuration are unchanged; prior CV1/SMB correctness
evidence remains applicable. Full-stage playability and full speed remain
unproven. Implement only the next measured, bounded candidate, not all five
experiments as one rewrite.
