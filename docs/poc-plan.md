# Proof-of-Concept Plan

> **Status:** historical. This document captured the first PoC's success
> criteria; most are checked off. Forward planning is in
> [`master-plan.md`](master-plan.md).

## Goal

Build an intentionally rough proof of concept that takes
`Super Mario Bros. (World).nes`, destructures it, converts its graphics resources
into Master System format, experiments with 6502-to-Z80 translation, and boots a
non-crashing SMS ROM in an emulator.

The first PoC does not need clean code, complete gameplay, audio, or broad mapper
support. It should produce concrete artifacts and document what breaks.

## Success Criteria

Minimum success:

- [x] Parse the SMB iNES/NES 2.0 header and split PRG/CHR.
- [x] Convert NES CHR tiles into SMS Mode 4 tile format.
- [x] Produce a structurally valid SMS ROM image.
- [x] Confirm the SMS ROM loads and runs under a headless SMS emulator smoke
  test.
- [x] Display-path scaffold: generated ROM loads converted SMB title-screen data
  decoded from the original `DrawTitleScreen`/`UpdateScreen` path.
- [x] Document PRG, CHR, RAM, tile, palette, and VDP mapping decisions.

Stretch success:

- [x] Generate a rough Z80 translation sketch for the SMB reset routine.
- [ ] Translate or manually bridge a small pure SMB 6502 routine into runnable
  Z80.
- Run a minimal game loop on SMS using translated logic.
- Show Mario or a level fragment with SMS-native VDP updates.
- Produce an annotated report of which original PRG addresses were translated,
  stubbed, or ignored.

## Host Tooling Policy

Use project-local Docker/Docker Compose for external tooling. Do not install
WLA-DX, cc65, emulator builds, or helper binaries directly on the host.

Recommended future layout:

```text
docker/
  Dockerfile.toolchain
compose.yaml
poc/
  input/
  out/
  notes/
```

The container should eventually include:

- Rust toolchain for the PoC code.
- WLA-DX or another Z80/SMS assembler.
- cc65/da65 for comparison disassembly.
- Mednafen for command-line SMS smoke runs.

Current scaffold:

- `compose.yaml` runs the PoC against the local ROM mount.
- `docker/Dockerfile.toolchain` provides Rust, cc65, and Mednafen without
  installing them on the host. WLA-DX is intentionally left as a future
  container addition.

## Phase 1: ROM Destructuring

Create a rough Rust CLI under `poc/` or a temporary script if faster.

Inputs:

- SMB `.nes` path.

Outputs:

- `out/smb/header.txt`
- `out/smb/prg.bin`
- `out/smb/chr.bin`
- `out/smb/vectors.txt`
- `out/smb/map.json`

Required analysis:

- iNES/NES 2.0 detection.
- PRG size, CHR size, mapper, mirroring, trainer, battery flags.
- Reset/NMI/IRQ vectors read from PRG CPU addresses `$FFFA-$FFFF`.
- NROM CPU address mapping from `$8000-$FFFF` to PRG offsets.

## Phase 2: Graphics Conversion

Convert NES CHR into SMS Mode 4 tiles.

Initial mapping:

- NES tile: 8x8, 2bpp, 16 bytes.
- SMS tile: 8x8, 4bpp, 32 bytes.
- NES bitplanes become SMS bitplanes 0 and 1.
- SMS bitplanes 2 and 3 start as zero.

Outputs:

- `out/smb/tiles.sms4bpp`
- `out/smb/tiles.png` for inspection if convenient.
- `out/smb/chr-report.txt`
- `out/smb/title_decode.txt`
- `out/smb/title_nametable.bin`
- `out/smb/title_sms_nametable.bin`

Open questions:

- Which SMB CHR tiles are background versus sprite tiles?
- Which palette approximations best preserve SMB's visual intent?
- How much CHR can stay resident in 16 KB SMS VRAM alongside nametable and
  sprite data?

## Phase 3: SMS Boot Harness

Build a minimal SMS ROM that initializes the Z80, VDP, CRAM, VRAM, and a blank
main loop.

Current visual target:

- Load SMB background CHR tiles into VRAM.
- Fill the SMS name table with the title-screen nametable decoded from the
  original SMB VRAM update buffer at CHR/PPU `$1EC0`.
- Set a simple SMS palette approximating NES colors.

Outputs:

- `out/smb/poc.sms`
- `out/smb/boot-log.txt`
- Optional screenshot in `out/smb/`.

This validates the SMS side before any 6502 translation work is trusted.

## Phase 4: Address and Memory Mapping

Use a conservative memory model first.

Suggested mappings:

| NES concept | NES address | SMS representation |
| --- | --- | --- |
| Zero page | `$0000-$00FF` | SMS RAM `$C000-$C0FF` |
| CPU stack | `$0100-$01FF` | SMS RAM `$C100-$C1FF` or native Z80 stack |
| NES RAM mirror base | `$0000-$07FF` | SMS RAM `$C000-$C7FF` |
| PPU registers | `$2000-$2007` | Calls to SMS VDP shim routines |
| OAM DMA | `$4014` | Calls to SMS sprite upload routines |
| APU registers | `$4000-$4017` | Stubbed for this PoC |
| PRG ROM | `$8000-$FFFF` | SMS ROM banks/labels |
| CHR ROM | PPU `$0000-$1FFF` | Converted SMS VRAM tile data |

For the PoC, prioritize correctness and observability over speed. Native Z80
register allocation can come later.

## Phase 5: 6502-to-Z80 Translation Experiment

Do not attempt whole-ROM translation first. Pick one small, known SMB routine
from the public disassembly or from a traced PRG address.

Recommended strategy:

- Decode 6502 instructions into a tiny internal representation.
- Preserve flags explicitly.
- Emit verbose Z80 assembly with comments showing source PRG addresses.
- Replace NES hardware writes with named shim calls.
- Mark unsupported instructions or addressing modes clearly.

First candidate routine types:

- Pure arithmetic/state update routine.
- Controller state processing.
- Simple RAM copy/fill routine.

Avoid first:

- NMI renderer.
- Scroll update logic.
- Sound engine.
- Any routine dominated by PPU/APU writes.

## Phase 6: Integrate a Tiny Loop

Once the SMS boot harness and one translated routine work independently:

- Initialize RAM with SMB-like state.
- Call the translated routine each frame.
- Reflect a small piece of state visually, such as sprite X/Y or tile selection.
- Keep rendering SMS-native.

This proves the translation path can feed a working SMS program without requiring
the entire game to run.

## Documentation Requirements

Each iteration should update docs with durable findings:

- `docs/poc-plan.md`: plan changes and milestone status.
- `docs/translation-feasibility.md`: confirmed blockers or validated mappings.
- `docs/rom-library-notes.md`: new local ROM observations.
- `poc/notes/`: scratch notes before promotion to `docs/`.

Record exact commands, generated files, emulator behavior, and whether crashes
are assembler, ROM-header, VDP-init, or translated-code issues.

## Key Risks

- SMB rendering depends on NES nametable and scroll behavior that SMS does not
  reproduce directly.
- Static disassembly will confuse data and code without trace/CDL help.
- Correct flag translation is easy to underestimate.
- SMS VDP access timing and VRAM layout will force renderer rewrites.
- Audio is intentionally skipped but will later require a separate strategy.
