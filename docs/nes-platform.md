# NES Platform Notes

## ROM Container

Most NES ROM files use the iNES or NES 2.0 container format. The file begins
with a 16-byte header, followed by an optional 512-byte trainer, PRG ROM in
16 KB units, CHR ROM in 8 KB units, and optional extra data.

Important header fields:

- Bytes 0-3: `4E 45 53 1A`, the `NES<EOF>` magic.
- Byte 4: PRG ROM size in 16 KB units.
- Byte 5: CHR ROM size in 8 KB units. Zero means CHR RAM.
- Byte 6: lower mapper bits, mirroring, battery RAM, trainer flag.
- Byte 7: upper mapper bits, VS/PlayChoice flags, NES 2.0 marker.
- NES 2.0 extends mapper, submapper, RAM, timing, and console information.

Reference: https://www.nesdev.org/wiki/INES

## CPU Memory Map

The NES CPU is Ricoh 2A03/2A07, based on the 6502 without decimal mode.

Typical CPU address map:

| Address range | Meaning |
| --- | --- |
| `$0000-$07FF` | 2 KB internal RAM |
| `$0800-$1FFF` | Mirrors of internal RAM |
| `$2000-$2007` | PPU registers |
| `$2008-$3FFF` | Mirrors of PPU registers |
| `$4000-$4017` | APU and I/O registers |
| `$4018-$401F` | Normally disabled APU/I/O test area |
| `$4020-$5FFF` | Cartridge expansion area |
| `$6000-$7FFF` | Usually PRG RAM, save RAM, or mapper-controlled |
| `$8000-$FFFF` | Usually PRG ROM and mapper registers |

Fixed vectors are supplied by the cartridge at:

- `$FFFA-$FFFB`: NMI vector.
- `$FFFC-$FFFD`: reset vector.
- `$FFFE-$FFFF`: IRQ/BRK vector.

Reference: https://www.nesdev.org/wiki/CPU_memory_map

## PPU Memory Model

The PPU has its own address space, separate from CPU RAM:

| PPU range | Meaning |
| --- | --- |
| `$0000-$1FFF` | Pattern tables, usually CHR ROM/RAM |
| `$2000-$2FFF` | Nametables |
| `$3000-$3EFF` | Mirrors of nametables |
| `$3F00-$3F1F` | Palette RAM |
| `$3F20-$3FFF` | Palette mirrors |

NES background and sprite graphics use 8x8 tiles encoded as 2 bits per pixel.
Each tile is 16 bytes: 8 bytes for bitplane 0 and 8 bytes for bitplane 1.

Reference:

- https://www.nesdev.org/wiki/PPU_memory_map
- https://www.nesdev.org/wiki/PPU_pattern_tables

## Sprites and Palettes

The NES has 64 hardware sprites in OAM. Sprites are either 8x8 or 8x16,
depending on PPU control state. Only 8 sprites can be rendered on one scanline.

Palette handling is a major translation issue:

- Backgrounds use four 4-color subpalettes.
- Sprites use four 4-color subpalettes.
- Palette index 0 is treated specially as transparent/universal background in
  several contexts.
- Background attributes operate over coarse 16x16-pixel regions.

The Master System has more raw palette freedom, but the semantics do not map
directly.

## APU

NES audio has five channels:

- Pulse 1.
- Pulse 2.
- Triangle.
- Noise.
- DMC sample playback.

This does not map cleanly to the Master System PSG. Pulse channels are the
easiest. Triangle requires approximation. DMC has no native equivalent on the
base SMS.

Reference: https://www.nesdev.org/wiki/APU

## Mappers

The mapper is cartridge hardware. It controls PRG banking, CHR banking, nametable
mirroring, RAM, IRQs, and sometimes special hardware behavior.

This is the main reason generic conversion is hard. CPU instruction translation
alone is not enough; mapper behavior is part of the running program.

Recommended mapper priority:

1. Mapper 0 / NROM: fixed PRG and CHR, no bank switching.
2. Mapper 2 / UxROM: simple PRG bank switching, usually CHR RAM.
3. Mapper 1 / MMC1: serial register, PRG/CHR banking, mirroring control.
4. Mapper 4 / MMC3: PRG/CHR banking plus scanline IRQ behavior.

NROM reference: https://www.nesdev.org/wiki/NROM

## SMB Local Header Observation

The local ROM at:

`/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes`

has this first 16-byte header:

```text
4e 45 53 1a 02 01 01 08 00 00 00 00 02 00 00 01
```

Observed file size:

```text
40976 bytes
```

Interpretation:

- Magic: valid `.nes`.
- PRG ROM: `0x02 * 16 KB = 32 KB`.
- CHR ROM: `0x01 * 8 KB = 8 KB`.
- Mapper: 0 / NROM.
- Trainer: no.
- Battery RAM: no.
- Mirroring bit set, matching the common SMB vertical-mirroring behavior.
- File size matches `16 + 32768 + 8192`.

This makes SMB a suitable first target.

