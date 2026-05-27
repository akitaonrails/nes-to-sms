# Master System Platform Notes

## CPU Memory Map

The Master System uses a Z80 CPU with a 64 KB address space. The normal memory
map is:

| Address range | Meaning |
| --- | --- |
| `$0000-$BFFF` | Cartridge ROM/RAM or other cartridge hardware |
| `$C000-$DFFF` | 8 KB system RAM |
| `$E000-$FFFF` | Mirror of system RAM |

With the common Sega mapper:

| Address range | Meaning |
| --- | --- |
| `$0000-$03FF` | Unpaged ROM |
| `$0400-$3FFF` | ROM mapper slot 0 |
| `$4000-$7FFF` | ROM mapper slot 1 |
| `$8000-$BFFF` | ROM/RAM mapper slot 2 |
| `$C000-$DFFF` | System RAM |
| `$E000-$FFFF` | System RAM mirror |
| `$FFFC` | Cartridge RAM mapper control |
| `$FFFD` | Slot 0 bank control |
| `$FFFE` | Slot 1 bank control |
| `$FFFF` | Slot 2 bank control |

Reference:

- https://www.smspower.org/Development/MemoryMap
- https://www.smspower.org/Development/Mappers

## Cartridge Banking

The common Sega mapper exposes three mostly 16 KB ROM slots. Larger ROMs switch
which 16 KB bank appears in each slot by writing to `$FFFD-$FFFF`.

Implications for NES translation:

- NROM-128 and NROM-256 fit comfortably without complex banking.
- NES mappers with 8 KB or 16 KB PRG banks can often be represented by SMS
  16 KB banks, but not always with the same fixed/swappable layout.
- NES CHR banking does not naturally map to SMS because SMS graphics are loaded
  into VDP VRAM by software, not exposed as a PPU pattern-table bus.
- MMC3-style IRQ timing needs runtime recreation using SMS line interrupts,
  polling, or per-game rewrites.

## VDP and Video Memory

SMS Mode 4 is the relevant graphics mode for most native SMS games.

The VDP has:

- 16 KB VRAM.
- CRAM palette memory.
- A name table for background tile maps.
- Sprite attribute data.
- VDP registers for scrolling, display mode, interrupts, table addresses, and
  other control state.

Unlike the NES, the Z80 does not directly address VRAM as memory. The program
uses VDP ports to set an address and write/read VRAM or CRAM data.

Reference: https://www.smspower.org/Development/VDPRegisters

## Tiles

SMS Mode 4 tiles are 8x8 and 4 bits per pixel. Each tile is 32 bytes, stored as
four bitplanes.

NES CHR tiles are 8x8 and 2 bits per pixel, 16 bytes per tile. A mechanical
conversion can expand two NES bitplanes into the lower two SMS bitplanes and
clear the upper two. Better conversions may remap palettes and use all 4 bpp.

Reference: https://www.smspower.org/Development/Tiles

## Palettes

SMS Mode 4 has two assignable 16-color palettes:

- Background palette: first 16 CRAM entries.
- Sprite palette: second 16 CRAM entries.

SMS colors use two bits each for red, green, and blue, packed as `%00BBGGRR`.

Reference: https://www.smspower.org/Development/Palette

Translation implications:

- NES palettes are smaller per subpalette but more structurally tied to
  attribute tables.
- SMS has more colors available per screen, but background tiles generally
  choose from the background palette, while sprites use the sprite palette.
- A naive conversion can preserve NES colors approximately.
- A good conversion needs per-screen or per-level palette planning.

## Sprites

The SMS supports 64 sprites total and 8 sprites per scanline. Sprite size and
layout differ from NES sprite handling, and all sprite graphics must be present
in VDP VRAM.

Translation implications:

- NES and SMS both have the 8-sprites-per-scanline limit, which helps.
- NES 8x16 sprites need mapping to SMS sprite patterns.
- Games that rely on NES sprite overflow behavior or exact OAM evaluation will
  need special handling.

Reference: https://www.smspower.org/Development/Sprites

## Audio

The base SMS sound chip is SN76489-compatible PSG:

- Three tone channels.
- One noise channel.
- Per-channel volume control.

Reference: https://www.smspower.org/Development/SN76489

NES-to-SMS audio mapping:

| NES APU | SMS PSG approximation |
| --- | --- |
| Pulse 1 | PSG tone channel |
| Pulse 2 | PSG tone channel |
| Triangle | PSG tone channel with compromised timbre |
| Noise | PSG noise channel |
| DMC | No native equivalent |

Some SMS and Mark III configurations include YM2413 FM support, but this is not
universal and should be treated as optional enhancement, not the base target.

