# Translation Feasibility

## Executive Assessment

A generic automatic NES-to-SMS ROM converter is not feasible in the strong sense
of "input any NES ROM, output a correct SMS ROM." The machines are too different
at the hardware boundary, and many NES games rely on mapper behavior, PPU timing,
APU behavior, and data/code layouts that cannot be recovered perfectly by static
analysis.

A practical tool is feasible if it is designed as a porting assistant:

- Parse and classify ROMs.
- Extract graphics, palettes, data, and code regions.
- Use emulator traces and code/data logs.
- Translate 6502 logic into an intermediate representation.
- Emit Z80 where translation is safe.
- Replace NES hardware register access with SMS runtime calls.
- Generate buildable SMS projects that can be completed with game-specific
  profiles.

## What Can Be Automated

### Header and Mapper Classification

This is straightforward for standard iNES/NES 2.0 images.

Outputs:

- PRG/CHR sizes.
- Mapper and submapper.
- Mirroring mode.
- Battery/trainer flags.
- RAM sizes where declared.
- Target risk score.

### NROM PRG Layout

NROM is the best first mapper:

- No PRG bank switching.
- No CHR bank switching.
- Fixed vectors.
- Simple ROM layout.

For NROM-256, the 32 KB NES PRG at `$8000-$FFFF` can be represented in SMS ROM
without needing a complicated mapper strategy.

### CHR Tile Conversion

NES 2bpp 8x8 tiles can be expanded into SMS 4bpp 8x8 tiles:

- Copy NES bitplanes into SMS bitplanes 0 and 1.
- Clear SMS bitplanes 2 and 3 for a conservative conversion.
- Later passes can recolor and use the extra SMS color depth.

### Static Disassembly Assistance

6502 instruction decoding and recursive descent can identify much of the code,
especially when seeded from reset/NMI/IRQ vectors and known jump tables.

This is useful but insufficient by itself. The project should consume trace/CDL
data as soon as possible.

### Some CPU Logic Translation

Many arithmetic, load/store, branch, and subroutine patterns can be translated
from 6502 into Z80 with a state model.

Useful strategy:

- Translate 6502 to an internal IR.
- Model flags explicitly.
- Emit Z80 from IR.
- Keep labels and comments tied back to source PRG addresses.

Do not try to translate directly from 6502 text to Z80 text without an IR.

See [6502 to Z80 Automation Research](6502-to-z80-automation.md) for the deeper
static-recompilation plan and the current list of blockers that need separate
research.

## What Is Hard or Requires Game Profiles

### Zero Page and Stack

6502 zero page has special addressing semantics and faster/smaller instructions.
The Z80 has no exact zero-page equivalent.

Possible strategies:

- Reserve a RAM block for emulated zero page.
- Map frequently used zero-page variables to direct Z80 registers during
  optimized per-routine translation.
- Start conservative, optimize later.

### Processor Flags

6502 and Z80 flags differ. Correct translation must preserve:

- Carry.
- Zero.
- Negative/sign.
- Overflow.
- Interrupt-disable state where relevant.

Many routines depend on exact carry and overflow behavior. Treat flags as part
of the IR, not as incidental assembler side effects.

### Indirect Jumps and Data Tables

NES games often intermix code, data, jump tables, compressed level data, and
graphics references. Static disassembly can misclassify bytes.

Required mitigation:

- Trace/CDL ingestion.
- User-authored per-game metadata.
- Existing disassembly imports where available.

### PPU Register Writes

NES code writes to `$2000-$2007`, `$4014`, and related addresses for rendering,
scrolling, palette, nametable updates, and OAM DMA.

SMS uses VDP ports, different table layouts, different scrolling registers, and
different VRAM update timing.

These writes need semantic replacement, not opcode translation.

### Mapper Registers

NES mapper writes are cartridge-specific. SMS mapper writes use a different
model.

Translation choices:

- NROM: no mapper rewrite required beyond ROM layout.
- UxROM: map PRG banks to SMS 16 KB banks if layout permits.
- MMC1/MMC3: require mapper-specific runtime and likely game-specific patches.

### IRQ and Raster Effects

MMC3 scanline IRQ and PPU-timed effects are not directly portable. SMS has line
interrupts, but timing and state differ.

Expect manual rewriting for games that depend on raster splits or precise
mid-frame updates.

### Audio

The NES APU and SMS PSG have different channel models. Music/sfx conversion is
possible only approximately unless rewritten.

Recommended staged approach:

1. Log APU writes.
2. Build a rough APU-to-PSG event converter.
3. Replace music engines with PSGlib-style data where possible.
4. Treat DMC as unsupported initially.

## Suggested Architecture

### Phase 1: Analyzer

Input: `.nes`.

Output:

- Parsed header.
- PRG/CHR files.
- Mapper report.
- Vector report.
- Initial risk report.

### Phase 2: Asset Extractor

Output:

- NES CHR tile sheets.
- SMS-converted tile binaries.
- Palette approximation tables.
- Nametable dumps where statically discoverable.

### Phase 3: Trace-Assisted Disassembly

Input:

- PRG ROM.
- Mapper profile.
- FCEUX/Mesen trace or CDL.
- Optional game profile.

Output:

- Code ranges.
- Data ranges.
- Labels.
- Control-flow graph.
- Annotated 6502 assembly.

### Phase 4: IR Translation

Output:

- 6502 semantic IR.
- Flag-aware operations.
- Memory access annotations.
- Hardware register access annotations.

### Phase 5: SMS Backend

Output:

- WLA-DX or SDCC/devkitSMS project.
- Z80 routines for translated logic.
- SMS runtime shims for VDP/input/audio.
- Converted assets.
- Build script.

### Phase 6: Game Profiles

Game profiles should describe:

- Known RAM variables.
- Known code/data regions.
- Hardware write meanings.
- Asset table formats.
- Level data formats.
- Mapper-specific behavior.
- Manual patches.

For SMB, import knowledge from doppelganger's disassembly instead of starting
from raw binary inference.

## Feasibility by Game Class

| Game class | Feasibility | Notes |
| --- | --- | --- |
| NROM, simple graphics/audio | High | Best first target |
| NROM, known disassembly | High | SMB fits here |
| UxROM with CHR RAM | Medium | PRG banking manageable, CHR updates require care |
| MMC1 | Medium-low | Serial mapper, CHR banking, mirroring changes |
| MMC3 | Low | IRQs and banked CHR common |
| Games with DMC-heavy audio | Low | SMS lacks direct equivalent |
| Raster/timing-heavy games | Low | Needs manual rendering rewrite |
| Late large games | Very low | Bank switching, compression, advanced effects |

## First Target Recommendation

Use `Super Mario Bros. (World).nes` first because:

- It is mapper 0/NROM.
- It has 32 KB PRG and 8 KB CHR.
- It has a known, mature public disassembly.
- A recent SMS proof-of-concept port already demonstrates feasibility.

The first milestone should be smaller than a full playable game:

1. Parse the ROM and emit a report.
2. Extract and convert CHR.
3. Generate a minimal SMS ROM that displays converted tiles.
4. Generate an annotated disassembly index linked to PRG addresses.
5. Hand-map a small SMB routine through the IR into Z80.
