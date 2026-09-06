# CV1 Acceptance Inputs

Use the canonical, gitignored `.roms/cv1.nes` and `profiles/cv1.toml`.
Keep commercial ROMs, generated projects and captures out of commits.

## Interactive testing

After generating and assembling `out/cv1/sms.sms`:

```sh
OVERCLOCK=500 docker/run_gpgx.sh out/cv1/sms.sms
```

Keyboard fallback: arrows, `Z`/`X`, and `Enter` for SMS Pause/NES Start.
The launcher warns if its configured gamepad is absent;
`GPGX_REQUIRE_GAMEPAD=1` makes that fatal. Wait for Simon to appear: a
background-only setup frame is not gameplay. Stock speed is not playable.

With the dagger equipped and hearts available, press Up + attack (SMS button
2) to throw it. Regression testing covers both grounded and airborne throws,
heart expenditure and continued indoor movement. The original-code/discovery
check is reproducible separately:

```sh
cargo test -p nes_to_sms --test cv1_dagger
CV1_DAGGER_NES=/path/to/input.nes cargo test -p nes_to_sms \
  --test cv1_dagger -- --ignored
```

## Matched walking and heart routes

The accepted sprite-zero handshake correction measures 26.22/29.26 walking/
heart updates per second at numeric `500`; this is still below normal NES
speed. Its original-code and assembled protocol checks can be rerun with:

```sh
CV1_SPLIT_NES=/path/to/input.nes cargo test -p nes_to_sms --test cv1_split -- --ignored
CV1_HANDOFF_PROJECT=out/cv1 cargo test -p nes_to_sms --test frame_handoff split_ -- --include-ignored
```

`core-routes.toml` is the frozen 420-game-tick comparison contract. The runner
boots through real input, captures actual libretro video callbacks, checks
state/movement/traps, and observes heart collection plus subsequent progress.
It does not use screenshot timing or synthetic IRQ counts as game updates.

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_route.py out/emulator-host/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-routes.toml \
  walk out/cv1/acceptance/walk --capture-every 1
```

Repeat with `heart` and a different output directory. Output directories must
not already exist. Add `--compare PATH/summary.json` to enforce the frozen
state/landmark comparison against a prior run. Preserve the core, profile,
runner and ROM hashes when comparing measurements; do not loosen tolerances
or replace the baseline to hide a failure.

Current measured cadence, continuous HUD checks and timing limits are in
[the performance reassessment](../../../docs/cv1-performance-reassessment.md).
These routes cover the courtyard and first heart, not a complete stage.

## First door and indoor traversal

The separate `core-traversal.toml` route requires the ordered door animation,
room setup, its one canonical game-counter reset, and sustained indoor
movement. It rejects traps, stalls, missing phases and discontinuities. Raw
coordinates remain in the CSV; neighboring callbacks must confirm the
16-bit movement endpoint so a torn RAM sample cannot finish the test.

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_traversal.py out/emulator-host/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-traversal.toml \
  indoor out/cv1/acceptance/indoor
```

Normal `indoor` requires 1,306 indoor ticks and at least 608 pixels of
confirmed movement. It does not claim that a specific helper ran. To test
the repaired consumed-return helper, generate a **separate** project, add
`.define DIAG_CONSUMED_ESCAPE 1` beside the runtime defines in its `sms.asm`,
then assemble it. Run that ROM with `indoor_escape` and a new output path.
This mode requires an actual success-counter increment, followed by at least
120 ticks and 80 pixels of confirmed movement. Never substitute this
instrumented ROM's timing for normal-ROM performance.

The counter marks validated software-return ownership transfer **before**
the original `PLA; PLA` consumes the live guest bytes. It is not a claim
that the subsequent tail jump has already finished.

Both modes record hashes, core options, raw callback observations, transition
captures and `summary.json`. A passed route covers the first indoor stair
approach, not a stage clear or boss.

## First indoor stair ascent

Original-code tests cover the two bank-6 conditional-branch tails reached
while entering the first staircase (`$9ECB` collision tail and `$971D`
approach timeout). Run the profile check normally, or include canonical-ROM
oracle/discovery checks:

```sh
cargo test -p nes_to_sms --test cv1_stairs
CV1_STAIRS_NES=/path/to/input.nes cargo test -p nes_to_sms \
  --test cv1_stairs -- --ignored
```

For interactive checking, approach the staircase from its lower-left end
and hold Up + Right. An input-only actual-core extension of the unchanged
indoor route verifies stair actor mode 4, ascent from Y192 to Y96, return
to normal actor mode 0, and movement along the upper platform. The extension
totals 2,500 ticks beyond the original indoor endpoint without a
trap, including a later death and respawn. This is exploratory coverage,
not a new frozen acceptance route or proof of completing the room/stage.
The historical `ENEMY` HUD label is present throughout ordinary gameplay;
its presence does not establish a boss encounter.

## Second stair, upper exit and next-room play

The separate `core-upper-exit.toml` contract extends the unchanged 1,306-tick
indoor route from boot. It backtracks to the first staircase, climbs both
stairs, crosses the upper exit, then requires next-room gameplay for at least
120 updates and 40 pixels of movement. Repeated callbacks without advancing
game ticks cannot confirm completion.

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --entrypoint python3 -v "$PWD:/work" -w /work nes-to-sms-retroarch \
  tools/core_upper_exit.py out/emulator-host/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-upper-exit.toml \
  out/cv1/acceptance/upper-exit
```

The observer rejects traps, death, stalls, unexpected states and counter
discontinuities. One source-verified counter reset is required during the
upper-exit transition; it earns no elapsed-tick credit. Merely changing area
or displaying `STAGE 02` is insufficient. This route does not prove a boss
fight or complete-stage fidelity.

Use a fresh output directory. Logs retain hashes, effective core options,
every video callback and transition screenshots, including failure evidence.
Runs never load emulator states or alter guest RAM. Screenshots are not
restorable states; GPGX serialization alone omits cartridge SRAM, so it is
not a valid substitute for the from-boot route.

## Canonical NES reference inputs

`stage1-smoke.buttons` and `heart-smoke.buttons` record reference-NES input
intent. For example:

```sh
FD_PAUSE_START=1 target/release/frame-diff \
  .roms/cv1.nes out/cv1/sms.sms --frames 3600 \
  --buttons-script profiles/cv1/acceptance/stage1-smoke.buttons --ref-only
```

The historical `*.sms.buttons` files use instruction/injected-IRQ timing.
Their old fixed-step endpoints are not current acceptance after scheduling
changes. The coherent renderer also needs a progressing video counter; the
tracer's legacy constant-`E0` mode is not a compatible CV1 gameplay clock.
Use actual-core routes for speed, motion and hardware-timing claims.

## Functional tracer smoke

The opt-in [functional video clock](../../../docs/functional-video-trace.md)
allows bounded runtime debugging without weakening graphics admission guards:

```sh
SMS_ABORT_BAD_SP=1 SMS_DUMP_RAM=C000:40 \
  target/release/trace-sms out/cv1/sms.sms \
  --functional-video ntsc224 --steps 180000000 \
  --pause-at-frame 600 --buttons-at-frame 1850:right --expect-no-trap
```

This schedule has reached outdoor gameplay with movement and a clean native
stack. Its synthetic epochs and delivered interrupts are not gameplay ticks
or physical frames; it does not replace the indoor or performance routes.

Run the host-only observer tests with
`python3 -m unittest discover -s tools -p 'test_core*.py'`.
