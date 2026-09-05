# Functional video tracing

`trace-sms --functional-video ntsc224` supplies a deterministic, instruction-paced
VCounter and interrupt clock for runtime code that polls active-display and
VBlank admission windows. Without this option, the existing fixed-E0 VCounter
and SMB diagnostic scheduling remain unchanged.

This is **not a beam emulator or a speed oracle**. Use the real-core acceptance
routes for gameplay cadence, continuous video, and presentation deadlines.
The tracer does not model horizontal phase, exact Z80 timing, or VDP contention.

## Clock and interrupt rules

One synthetic epoch spans 60,000 scheduler steps, or 15,000 when
`SMS_REAL_PACING=1` is set. Step zero is line 224 of a 262-line diagnostic
frame. The counter follows the NTSC-224 sequence `00..EA, E5..FF`; repeated
reads during one step do not advance it. An executed instruction advances one
step, and an interruptible HALT consumes idle scheduler steps until it wakes.

VBlank and line events share this clock and each retain one pending bit while
interrupts are disabled. VDP register enables gate delivery, not event arrival.
Reading status port BF acknowledges both bits and closes the VDP control latch;
VBlank takes precedence when both are pending. Pending events do not form a
backlog. The coarse line event uses the existing `R10 + 1` convention once per
epoch, not the hardware reload/underflow model. Writing R10 schedules its next
future occurrence; values at least 223 park it without acknowledging a pending
event. HCounter reads remain unsupported (`FF`).

`EI; HALT` resumes on an eligible interrupt. A DI HALT, `--no-irq`, or a HALT
with no enabled future interrupt source stops. Runtime traps and native-stack
checks remain strict. PAL/other display modes and the separate route-search
loops are rejected explicitly.

## Running and interpreting a trace

```sh
cargo build --release -p nes_to_sms --bin trace-sms
SMS_ABORT_BAD_SP=1 target/release/trace-sms out/game/sms.sms \
  --functional-video ntsc224 --steps 20000000 --expect-no-trap
```

Use a profile-owned input script when the game needs controller input. Existing
scripts still count the tracer's delivered translated-NMI opportunities, not
synthetic epochs, physical video callbacks, or game update ticks. Changing modes
can therefore require a different input schedule. `--game-frames` likewise does
not measure gameplay ticks.

The final `Functional video:` line separates scheduler steps, synthetic epochs,
and delivered frame/line IRQ counts. A trap-free bounded run alone does not prove
that the game reached a level: inspect the requested game-state RAM expectations
and captured output as well. Tracer screenshots are diagnostic renders, not
real-core video evidence.

## Focused checks

```sh
cargo test -p nes_to_sms --bin trace-sms
cargo test -p nes_to_sms --test trace_functional_video
```

The optional assembled-runtime test requires an already generated and assembled
coherent-presentation project; it does not install an assembler or require a ROM
in the repository:

```sh
TRACE_FUNCTIONAL_PROJECT=out/game cargo test -p nes_to_sms --bin trace-sms \
  functional_clock_unblocks -- --ignored
```
