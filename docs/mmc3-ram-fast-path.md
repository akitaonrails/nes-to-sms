# MMC3 internal-RAM fast path

Measured 2026-09-07 against functional checkpoint `94c31bc`. This is a
runtime optimization, not a general IR optimizer or inliner. The translated
assembly and SMB3 profile are unchanged.

## Change and contract

The full-runtime read/write bus helpers now check the effective NES address
before entering general routing. For `$0000–$1FFF`, they perform the single
native RAM access at `$C000 + (address & $07FF)`. Other addresses retain the
existing guarded PPU, controller, SRAM, mapper and PRG paths.

The change adds **32 fixed-bank bytes**, with no extra RAM, metadata, or
per-call code growth. Successful accesses preserve raw HL, BC, DE, write A,
shadow P, entry interrupt state and SMS mapping controls. Native Z80 F is not
part of the existing helper preservation contract. Fast accesses do not
mask interrupts; host interrupt context restoration protects their temporary
register/stack state. Read-modify-write operations still perform both the
old-value and final-value writes.

Nominal instruction costs exclude the caller's CALL and interrupt service:

| Access | Before, interrupts enabled | After | Non-RAM miss penalty |
| --- | ---: | ---: | ---: |
| Internal-RAM read | 204 T | 74 T | +23 T |
| Internal-RAM write | 220 T | 95 T | +65 T |

With interrupts disabled, the old costs were 200/216 T. These are assembled
opcode costs, not the Rust interpreter's approximate cycle counter.

## Actual-core results

Both builds use the same compiler/profile, numeric `500` overclock, disabled
frameskip and NTSC-U in Genesis Plus GX. Baseline ROM is `ec5299b6…`; candidate
SHA-256 is `42cf60874649422c2fdb0ca7dbd11ce10235a343e9554d59bd10dd165f528201`.

| Matched workload | Before | After | Gain |
| --- | ---: | ---: | ---: |
| First 400 map updates | 10.1392 updates/s | 11.1277 updates/s | 9.75% |
| Active 1-1 ready→goal, 1,979 updates | 3.8229 updates/s | 4.0103 updates/s | 4.90% |

All 401 map completed-wait RAM snapshots and matched nonuniform displayed
images agree. Both full routes collect the mushroom, clear 1-1 and return to
the completed map with four lives; all seven major checkpoint guest/cart RAM
snapshots agree exactly. Input is controller-only, with no state loads or
guest-memory writes.

The instruction-bounded 200-map-update profile falls from 366,452,212 to
335,914,951 nominal T-states (8.33%). Bus routing saves 32,800,794 T; capture,
renderer and dispatch costs are unchanged. Fresh stock/instrumented/installed
core parity covers 7,000 physical frames before interpreting these costs.

**This does not fix blanking.** The matched map window has 154 flat frames in
both builds. Gameplay has 12,459/33,678 versus 12,463/32,143 flat-color callbacks,
with a longest streak of 11 in both. Faster game work shortens visible intervals;
the blank fraction therefore increases slightly. Do not label this smoother
presentation or comfortable playability. Moving-level attribution is needed
before selecting another optimization.

The follow-up moving-level profile now covers 200 instruction-bounded updates
after the mushroom (logical boundaries 1054→1254). It measures 5,207,533.835
nominal T-states/update: renderer **44.572%**, page-local dispatch **17.526%**,
translated residual **10.229%**, bus **7.499%**, other calls/returns **3.954%**,
capture **3.056%**. The renderer performs 159 full BG builds and 41 BG-stable
sprite updates. Its row/cell traversal, conversion, cache lookup and frozen
nametable reads all matter; another blanket bus optimization is not the
largest opportunity in this workload.

Three-core prefix parity covers 12,604 frames, controller decisions, dense
video, snapshots and sampled guest/cart boundaries. The ledger includes all
200 intervals even though one completed-wait boundary was not sampled by
the frame observer. The bounded run stops intentionally at the driver's tick
limit; that is not another full-clear result. In this moving window, flat
backdrop callbacks occupy 45.53%, with zero all-black callbacks.

## Verification

Passing evidence includes 180 bus-boundary vectors, 945 actual IM1 interrupt
injections, original-6502 pipeline parity for tagged zero-page/stack operations,
explicit RMW bus-write recording, 15 assembled fixtures, 12 full-runtime helper
tests and the IRQ bridge. Workspace tests pass (577, with 143 ignored); format
and lint complete with existing lint warnings visible. Fresh SMB1 and Castlevania
builds remain byte-identical to their accepted ROMs.

Ignored provenance is retained under `out/smb3-phase4-ram.Jog178/` and
`out/smb3-phase4-profile.yQ7czS/`. Existing long legacy evidence is reused only
because generated ROMs, production interpreters and the emulator core are
unchanged. No commercial ROM or extracted asset is committed.
