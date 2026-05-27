# Tooling Survey

## NES Reverse Engineering Tools

### da65 / cc65

`da65` is a mature 6502 disassembler from the cc65 toolchain. It supports info
files that identify labels, ranges, code, and data, and it can emit assembly
suitable for ca65.

Use case for this project:

- Baseline 6502 disassembly.
- Compare our Rust analyzer output against a known tool.
- Potentially generate ca65-compatible intermediate output for inspection.

Reference: https://cc65.github.io/doc/da65.html

### FCEUX Code/Data Logger

FCEUX has a Code/Data Logger that records which ROM bytes execute as code and
which are read as data during gameplay.

Use case:

- Distinguish code from data.
- Build per-game analysis profiles.
- Avoid pretending static recursive descent can solve every indirect jump and
  data table.

Reference: https://fceux.com/web/help/CodeDataLogger.html

### Mesen

Mesen is useful for NES debugging, trace logging, memory inspection, PPU viewing,
and code/data analysis workflows.

Use case:

- Generate traces.
- Confirm runtime mapper behavior.
- Observe RAM, PPU, APU, and OAM state while building game profiles.

Reference: https://www.mesen.ca/

### Existing Disassemblies

For Super Mario Bros., the doppelganger disassembly should be treated as a
primary source for the first target. It is much better than asking the first
version of this tool to infer all labels and data structures from raw bytes.

Reference: https://6502disassembly.com/nes-smb/

## SMS Build and Debug Tools

### WLA-DX

WLA-DX is a multi-platform assembler with Z80 and SMS support. It supports SMS
ROM header directives and bank-aware assembly workflows.

Use case:

- Initial generated Z80 assembly target.
- Banked SMS ROM output.
- Easier integration with hand-authored Z80 routines.

Reference: https://wla-dx.readthedocs.io/

### devkitSMS / SMSlib

devkitSMS provides a C/SDCC-oriented development environment and SMSlib runtime
helpers.

Use case:

- Alternative output backend for generated scaffolds.
- Runtime helpers for VDP, palettes, sprites, input, and PSG.
- Faster prototyping than pure assembly in some phases.

Reference: https://github.com/sverx/devkitSMS

### PSGlib

PSGlib is a music/sound-effect playback library for the SMS PSG.

Use case:

- Target format for converted or rewritten music.
- Runtime audio playback once APU-to-PSG conversion is defined.

Reference: https://github.com/sverx/PSGlib

### Emulicious

Emulicious is a strong debugger for Sega 8-bit targets. It includes debugging,
profiling, tracing, and visual inspection tools that are useful for SMS work.

Use case:

- Debug generated SMS ROMs.
- Inspect VDP state, tiles, sprites, palettes, and PSG behavior.
- Validate bank-switching behavior.

Reference: https://emulicious.net/

## Rust Crate Direction

Existing Rust crates can help with isolated pieces, but this project should not
depend on a crate to solve the whole conversion problem.

Likely useful crate categories:

- iNES/NES 2.0 parsing.
- 6502 instruction decoding.
- Z80 instruction encoding or assembly text generation.
- Bitplane/tile manipulation.
- CLI, diagnostics, and binary file handling.

Recommended approach:

- Own the project-specific IR.
- Use external tools for validation and comparison.
- Emit human-readable artifacts at every phase.
- Prefer deterministic, inspectable transformations over opaque decompilation.

