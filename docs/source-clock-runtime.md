# Source-time execution foundation

Implemented, tested and independently reviewed 2026-09-08; source-boundary flag
elision found in review has been corrected and passes the second review.
This is an opt-in, rendering-disabled timing foundation, **not playable
Adventure Island or complete CNROM support**. It follows the
[source-bus checkpoint](cnrom-bus-runtime.md).

## Why a separate clock

Original NES hardware events must depend on original instruction time, not
the duration of translated Z80 code or SMS VBlank. The full original Adventure
Island reference includes zero and double wait-helper entries between adjacent
frames; one helper entry cannot safely stand for one NES frame.

`CNROM_SOURCE_CLOCK_EXPERIMENT` is selected alone in `runtime_defines`; it
implies the existing raw CNROM bus internally. The previous bus-only capability
and existing games retain their paths. Neither flag admits a rendered game.

## Execution contract

The IR retains the decoded source instruction alongside its address and text.
Timing classification distinguishes bus sequence, base cycles and conditional
penalties. Missing or inconsistent metadata fails closed. Clock-mode lowering
preserves full guest flags and instruction boundaries, including compound RMW
operations and memory-reading NOPs; comments are not timing metadata.

Each instruction supplies an explicit ten-byte descriptor. The runtime performs
its ordered opcode, operand, pointer, dummy, data and stack accesses. Completed
source cycles count modulo 2³²; PPU dot/line progression and an independent
32-bit frame counter survive rollover. Bus timestamps include the completed
transfer cycle. Pure Z80 bookkeeping advances no source time.

The declared synthetic epoch is cycle zero at pre-render scanline 261, dot zero,
after reset handling, with rendering disabled. This is **not** an authentic
power-on/warmup model. Tested behavior includes conditional branch/index timing,
buffered source-bus accesses, PPUSTATUS polling/clearing, NMI enable/acceptance,
genuine NMI/BRK/RTI stack frames and 513/514-cycle DMA alignment. Guest returns
use exact decoded PCs, never native-stack sentinels. Host IRQ only acknowledges
the SMS interrupt; it cannot invent guest elapsed time or interrupts.

Clock state occupies `$CA80–$CABD`, reserved through `$CAFF`. That aliases the
inactive legacy reverse-CHR map, so legacy rendering entry points must remain
unreachable in this capability. Future presentation storage must not overwrite it.

## Evidence

Seven ordinary tests, 19 freshly assembled projects and six assembled-helper
tests pass. Literal expectations come from independent hardware references,
not a second copy of the production timing classifier. Tests cover RMW event
timestamps, stack/flag behavior, dynamic penalties, rollover, mapping/IFF
preservation and declared collision guards.

```sh
cargo test -p nes_to_sms --test cnrom_clock
cargo test -p nes_to_sms --test cnrom_clock -- --include-ignored
```

The second command requires the existing Docker WLA-DX image. Local evidence
and helper-test commands are in ignored
`out/tests/cnrom-clock-evidence-3513752.md`; the correction's red/green evidence
is in `out/tests/cnrom-clock-flags-evidence-3575871.md`.

Actual Genesis Plus GX probes—RMW, NMI, DMA and the corrected comparison/NMI
case at 100% and 500%—reach matching source outcomes. They require an executed completion marker, exact
source cycles/RAM and the intentional final unsupported-read trap. They do not
claim working APU, graphics or NES timing-race parity. Root integration evidence
is in ignored `out/cnrom-source-clock.sSqJ0N/`.

Review exposed five immediate/accumulator flag shortcuts that could leave stale
P at an NMI boundary. All five permanent tests fail with the preserved compiler
and pass after the source-mode-only correction. The original 14 fixture ROMs
remain byte-identical, preserving their helper/core and cost evidence.

Workspace: 618 passed, 193 ignored; formatting, Clippy and release build succeed
with existing warnings. The previous 37-project bus suite and 96 ABI cases pass
again. Fresh SMB1/CV1/SMB3 ROMs remain byte-identical to preserved baseline
builds; canonical launch artifacts are unchanged. The unchanged reference CPU,
harness and subject bytes retain the earlier three 4,500-frame SMB route passes;
those routes were not redundantly rerun for this checkpoint.

## Limits and next work

Rendering, external IRQ/APU/DMC, controllers, undriven reads and unrestricted
timing collisions remain unavailable. Conservative status-read windows near
VBlank set/clear and new NMI edges during vector entry/DMA trap explicitly.
This is not a blanket cycle-accurate CPU/PPU claim.

The current fixed directory fits 2,687 decoded boundaries; larger inputs fail
before output. Banked directory pages can remove this software layout limit
without omitting code or changing existing games.

The conservative clock is expensive: 64 measured DEX/BNE iterations consume
320 source cycles and approximately 558,489 Z80 cycles, with a 51-byte loop.
Boot and host IRQ are excluded. This **1,745.278 Z80/source-cycle** measurement
uses the existing interpreter's approximate cost accounting, not exact hardware
FPS. Practical execution requires timing-preserving idle-loop and quiet-interval
shortcuts, verified against this baseline. More cartridge ROM cannot enlarge
SMS VRAM or justify dropping source events or game content.
