# Banked source execution and verified quiet waits

Implemented, tested and independently reviewed 2026-09-08. This extends
the [source-clock foundation](source-clock-runtime.md), not rendered Adventure
Island admission or complete CNROM support.

## Complete dispatch without the single-bank ceiling

The source-clock path now stores complete high-byte groups of decoded PCs in
banked ROM. A fixed 128-entry directory supplies each group's bank and address.
Every group has a real terminator, including empty pages. Missing targets still
trap; dispatch never substitutes a nearby instruction or truncates entries.

Source-only builds use a 4 MiB image. Translated code is placed around reserved
assets, raw CHR and dispatch records; symbolic references retain the correct
physical bank. Unaddressable output fails before project creation. Existing
game layouts and the earlier bus-only capability are unchanged.

This assumes an expanded Sega-compatible mapper implementing all eight bank
bits, not every original cartridge revision. Historical mapper chips had smaller
limits. Actual-core testing is not physical-cartridge certification.
[Hardware reference](https://www.smspower.org/Development/Mappers).

The current Adventure inventory contains 6,887 decoded PCs across 477 routines.
Its estimated code fits in 13 banks and its resume records need three; unknown
content selectors and final game assembly remain separate validation work.

## Exact acceleration, explicitly enabled

```toml
[translation]
runtime_defines = ["CNROM_SOURCE_CLOCK_EXPERIMENT"]
source_clock_fast_forward = true

[[source_poll_loop]]
at = 0x8100
zp = 0x20
```

The addresses above illustrate a synthetic fixture. The compiler verifies the
original `LDA zp / BNE back` bytes, decoded identities and same-page branch.
Acceleration defaults off. Runtime checks require an equivalent completed loop
boundary, matching nonzero RAM/A/N/Z and no pending interrupt, DMA, entry or
rendering activity. Otherwise execution uses the precise path.

A summary advances only complete six-cycle iterations, strictly before the next
supported hardware boundary, with a maximum of 10,920 cycles. It preserves guest
state and the completed branch's bus state; the original NMI handler clears the
wait byte. It never substitutes one wait for one frame. Tests reconstruct every
opcode, operand, RAM and dummy read represented by a summary.

New PPU/APU/input domains must supply verified deadlines or disable this path.
The rendering-disabled proof cannot be carried over merely by enabling hardware.

## Measured evidence and limits

The finite idle-wait comparison matches all 27,524 source-bus transfers and guest
state. Approximate Z80 work falls from 48,950,669 to 193,237 cycles, about 253×.
This is **idle-wait acceleration, not a game-wide speedup or hardware FPS**.
The unannotated 320-source-cycle DEX/BNE control remains at 558,489 approximate
Z80 cycles with acceleration off or on; ordinary instruction cost is unchanged.

Tests include 29 assembled projects and 15 freshly executed helper checks,
full 256-entry page lookup, distant code
and strict misses, genuine stack/interrupt returns, rollover, deadline limits
and guarded fallbacks. Genesis Plus GX checks at 100%/500% cover ordinary
off/on waits, compiler-remapped code and a separately controlled bank-255
placement. The latter tests the core's upper bank bits, not natural compiler
placement of that small fixture.

Workspace: 622 passed, 205 ignored; formatting, Clippy and release build succeed
with existing warnings. Fresh SMB1/CV1/SMB3 and earlier bus-only test ROMs remain
byte-identical to their baselines. Root evidence is locally preserved in ignored
`out/cnrom-phase4-root.NFeNgv/`; prior unchanged SMB oracle routes are explicitly
reused, not reported as newly run. Rendering, audio, source input and full-game
acceptance remain required before advancing the mapper queue.
