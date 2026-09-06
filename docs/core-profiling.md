# Actual-core profiling

This is an isolated, host-only measurement tool for translated SMS ROMs. It
does not replace the installed emulator or change guest code, RAM or inputs.
Nominal Z80 T-states and the core's overclock-scaled master clocks are separate
quantities; multiplying physical frames by the overclock setting is not an
instruction-cost measurement.

## Pinned source and isolation

The approved reference is [Genesis Plus GX at
`a7985a9c4278ac352f8ca7bb4d3cc6b36e9e3e7d`](https://github.com/libretro/Genesis-Plus-GX/tree/a7985a9c4278ac352f8ca7bb4d3cc6b36e9e3e7d).
The read-only checkout is `.slim/clonedeps/repos/libretro__Genesis-Plus-GX`;
`.slim/clonedeps.json` records its provenance. Editable stock/instrumented
copies and compiled cores belong under ignored `out/`. Build only in Docker,
using the existing `nes-to-sms-retroarch` image; no host installs are required.
The existing `AGENTS.md` is deliberately unchanged.

Preserve upstream license notices in local copies. Genesis Plus GX has its
own license, not this project's MIT/Apache license. Do not commit or publish
the downloaded sources, compiled cores, commercial ROMs or extracted assets.

## Evidence required before trusting costs

1. Build the unmodified pinned source and compare its complete `frames.csv`
   with the installed core on identical ROM, options and frozen inputs.
2. Compare the instrumented build with that stock twin in the same way.
3. Test accounting against the actual upstream CPU implementation: conditional
   and repeating instructions, pre-write bank identity, interrupts, HALT,
   complete tick boundaries and frame-cycle rebasing.
4. Require conservation across instruction rows, complete tick intervals and
   totals. Keep unknown/external costs visible. Conservation alone does not
   establish attribution correctness.

The frozen `tools/core_route.py` still hashes every video callback even when
`--capture-every 1000` saves only occasional PPM files. Different core binaries
have different hashes: use explicit CSV equality for instrumentation parity;
do not weaken the normal runner's `--compare` provenance checks.

## Stock rebuild checkpoint

The September 5, 2026 local checkpoint uses CV1 ROM
`70283c950f1f5edc2a386bdb7a8ebc3a14ea9ad74513096374b0ca780554bf22`.
The default upstream Unix build with Docker GCC 12.2 produces core
`e76dd8fb152e7efb24638c579ad43de3eb8d7d0fd531c5e47cf35373834faa62`.
Walking and heart routes at numeric `500`, plus walking at `250`, pass and
match every installed-core CSV row exactly. Walking/heart throughput at
`500` remains 19.2560/21.5291 game updates per second. These are baseline
checks, **not an optimization result**.

Local evidence: `out/gpgx-profile.xwbUpU/stock-{walk,heart}-500/` and
`stock-walk-250/`.

## Build and run

Use fresh, ignored destinations. The patcher rejects a nonmatching CPU source
hash and refuses destinations outside an `out/` tree or inside `.slim/`.
It does not download dependencies. The approved reference must already exist.

```sh
mkdir -p out/gpgx-measurement
cp -a .slim/clonedeps/repos/libretro__Genesis-Plus-GX out/gpgx-measurement/stock
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -v "$PWD:/work" -w /work/out/gpgx-measurement/stock \
  --entrypoint make nes-to-sms-retroarch -f Makefile.libretro platform=unix -j8
cp -a out/gpgx-measurement/stock out/gpgx-measurement/instrumented
python3 tools/apply_gpgx_profile.py out/gpgx-measurement/instrumented
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -v "$PWD:/work" -w /work/out/gpgx-measurement/instrumented \
  --entrypoint make nes-to-sms-retroarch -f Makefile.libretro platform=unix -j8

docker run --rm --network none --user "$(id -u):$(id -g)" \
  -v "$PWD:/work" -w /work --entrypoint python3 nes-to-sms-retroarch \
  tools/core_profile.py out/gpgx-measurement/instrumented/genesis_plus_gx_libretro.so \
  out/cv1/sms.sms profiles/cv1/acceptance/core-routes.toml \
  walk out/gpgx-measurement/profile-walk --capture-every 1000
```

The default window starts immediately after the instruction advancing the
route to tick 60 and ends immediately after the advance to 420. The latter
instruction belongs to the preceding interval. Thus `profile.json` covers
360 complete intervals, not delayed host-frame samples. Stage predicates and
RAM offsets come from the route TOML, not game addresses embedded in C.
`profile-wrapper.json` records wrapper provenance and dump errors separately
from the unchanged runner's summary.

Interrupt identity is tracked from reset, including before arming; windows
may open or close inside an IRQ. CV1 uses ordinary `EI; RET`, so the ledger
matches the saved return PC/SP rather than treating every RET as IRQ exit.
Instruction identity uses the physical ROM mapping **before** execution.
`USE_CYCLES` charges preserve upstream rounding; independent instruction
cycle deltas detect residuals, and cumulative totals survive frame rebasing.

Run focused actual-core tests in Docker:

```sh
docker run --rm --network none --user "$(id -u):$(id -g)" \
  -v "$PWD:/work" -w /work --entrypoint python3 \
  -e GPGX_STOCK_CORE=/work/out/gpgx-measurement/stock/genesis_plus_gx_libretro.so \
  -e GPGX_PROFILE_CORE=/work/out/gpgx-measurement/instrumented/genesis_plus_gx_libretro.so \
  nes-to-sms-retroarch -m unittest discover -s tools -p test_gpgx_profile_core.py -v
python3 -m unittest discover -s tools -p 'test_core*.py'
```

Tiny original SMS fixtures run on the actual upstream CPU, not a toy
interpreter. Tests compare callbacks, full RAM and serialized guest state
against stock. The one serialized host IRQ-callback pointer is normalized
using its exact ELF symbol; guest registers/memory are not normalized.

## Instrumented checkpoint

Instrumented core `4d82c8a8fd8e69ee1542b6cadec9f62afa6107748bf18399be4a629357d75437`
matches all three stock-route CSVs exactly. Each report closes 360 intervals
with no accounting errors or unknown/external costs. Five actual-core tests
pass, including branches, LDIR repeats, mapper writes, HALT, nested INT/Pause
NMI, RET/RETI/RETN, counter wrap and arming/opening/closing inside an IRQ.

| Route / clock | Nominal T-states / interval | Scaled master clocks, complete window |
| --- | ---: | ---: |
| Walk / 500 | 976,408.69 | 1,008,168,601 |
| Heart / 500 | 856,574.49 | 884,351,100 |
| Walk / 250 | 1,857,459.56 | 3,924,724,465 |

Keep source category and IRQ depth as separate axes: CV1's actual game work
runs inside an interrupt too. `_tr_cont_*` labels and WLA `_sizeof_*` values
are not function extents. Use validated bank-qualified spans; retain gaps,
data execution and unclassified costs. Local interpretation and source proof
are in `out/gpgx-profile.xwbUpU/ATTRIBUTION.md`. Profiling identifies candidate
work; only unchanged-clock gameplay A/B can establish a speed improvement.
