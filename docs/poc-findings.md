# PoC Findings

> **Status:** historical. Findings from the first PoC. The "Next Steps" at
> the bottom of this file are superseded by
> [`master-plan.md`](master-plan.md) Phase 0+.

## Implemented Artifacts

The first proof-of-concept implementation lives in `poc/` as a small Rust CLI.
It accepts a `.nes` file and an output directory:

```sh
cd poc
cargo run -- "/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes" out/smb
```

Equivalent containerized generation path:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm poc
```

Containerized emulator smoke path:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm sms-smoke
```

Visible emulator path:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose up -d sms-visual
docker compose stop sms-visual
```

The Compose path keeps cc65 and future external retro tooling inside the project
toolchain image instead of installing binaries directly on the host.

## Generated Outputs

The current run writes:

| File | Purpose |
| --- | --- |
| `poc/out/smb/header.txt` | Parsed iNES/NES 2.0 metadata |
| `poc/out/smb/prg.bin` | Raw 32 KB PRG ROM |
| `poc/out/smb/chr.bin` | Raw 8 KB CHR ROM |
| `poc/out/smb/vectors.txt` | NMI/reset/IRQ vectors |
| `poc/out/smb/map.json` | Initial NES-to-SMS memory mapping notes |
| `poc/out/smb/tiles.sms4bpp` | NES CHR expanded to SMS Mode 4 tile format |
| `poc/out/smb/tiles.ppm` | Quick visual tile-sheet preview |
| `poc/out/smb/title_nametable.bin` | NES title nametable reconstructed from real SMB VRAM commands |
| `poc/out/smb/title_sms_nametable.bin` | Reconstructed title nametable converted to SMS format |
| `poc/out/smb/title_decode.txt` | Decoded `DrawTitleScreen`/`UpdateScreen` command stream |
| `poc/out/smb/poc.sms` | 32 KB SMS tile-demo ROM |
| `poc/out/smb/z80_translation_experiment.asm` | Reset-routine translation sketch |
| `poc/out/smb/summary.txt` | Human-readable run summary |

Generated files remain ignored by git.

## Confirmed SMB Properties

The local SMB ROM is still classified as mapper 0/NROM:

- PRG ROM: 32 KB.
- CHR ROM: 8 KB.
- Reset vector: `$8000`.
- Mirroring: vertical.
- Trainer: absent.
- Battery RAM: absent.

The header has NES 2.0 marker bits set, but the practical layout is the same
simple NROM layout needed for this PoC.

## CHR to SMS Tile Conversion

The PoC converts each 16-byte NES 2bpp tile into a 32-byte SMS Mode 4 tile:

- NES bitplane 0 becomes SMS bitplane 0.
- NES bitplane 1 becomes SMS bitplane 1.
- SMS bitplanes 2 and 3 are cleared.

For SMB, this produces:

- Input CHR: 8,192 bytes.
- Output SMS tile data: 16,384 bytes.

That exactly fills SMS VRAM if loaded in full, so a real renderer will need a
more careful VRAM layout. The demo ROM loads all converted tile data first, then
places its name table near `$3800`, which overwrites part of the loaded tile
area. This is acceptable for the current tile-pattern smoke test but not for a
real port.

## Real Title-Screen Mapping

The current ROM no longer fills the SMS name table with sequential tile numbers.
It follows the original SMB title-screen path:

- `DrawTitleScreen` reads a VRAM update buffer from CHR/PPU `$1EC0`.
- The PoC decodes that buffer using the same command format consumed by
  `UpdateScreen`.
- The decoded writes reconstruct the NES title nametable and attribute bytes.
- The resulting nametable is converted into SMS Mode 4 name table entries.
- The SMS ROM loads the NES background CHR page `$1000-$1FFF` as SMS tile
  indices `0-255`.

This is still not gameplay, but it is now based on real SMB rendering data and
real SMB rendering code behavior.

## SMS ROM Generation

The PoC writes a raw 32 KB `.sms` image directly from Rust. It does not require a
Z80 assembler yet.

Current SMS behavior:

- Z80 startup code begins at `$0000`.
- VDP is initialized for Mode 4.
- SMB background CHR data is copied to VRAM.
- A simple CRAM palette is loaded.
- The reconstructed SMB title-screen name table is copied to `$3800`.
- Sprites are hidden.
- Display is enabled.
- Execution enters an infinite loop.

The ROM has a `TMR SEGA` header at `$7FF0`, export/32 KB marker `$4C`, and a
computed checksum.

Emulator smoke result:

- MAME was tested first but Debian's MAME SMS driver requires an external Master
  System BIOS (`mpr-12808.ic2`), so it is not a good no-BIOS smoke runner.
- Mednafen loads `poc.sms` with the `sms` module, recognizes it as a 32 KiB ROM,
  detects mapper `None`, detects territory `Export`, and runs until the smoke
  timeout terminates it.
- A visible Mednafen service (`sms-visual`) opens the generated ROM through the
  host X11 display without installing Mednafen on the host.
- The smoke test is headless, so it validates load/run behavior but not visual
  correctness yet.

## Translation Findings

The generated translation sketch starts at SMB reset vector `$8000` and keeps
the reset routine aligned through the idle loop at `$8057`.

Confirmed initial mappings:

| NES operation | PoC Z80 strategy |
| --- | --- |
| `LDA #imm` | `ld a,imm` |
| `LDX #imm` | `ld b,imm` as emulated X |
| `LDY #imm` | `ld c,imm` as emulated Y |
| NES RAM `$0000-$07FF` | SMS RAM base `$C000` |
| `STA $2000/$2001` | PPU write shim call |
| `LDA $2002` | PPU read shim call |
| APU writes | Stubbed |
| `CMP` + `BCS` | Z80 `cp` + `jp nc` for that pattern |

Important blocker: rendering cannot be translated as plain stores. NES PPU
accesses must become SMS VDP routines with different timing and data layout.

The title-screen work demonstrates the intended approach: do not guess what to
draw. Decode what SMB's 6502 renderer would have sent to PPU memory, then map
that semantic result onto SMS VRAM.

The visible title menu now has two runtime behavior slices:

- SMS button 1 maps to NES Select and toggles the mushroom icon using SMB's real
  `DrawMushroomIcon` data.
- SMS button 2 maps to NES Start and swaps to a World 1-1 screen generated from
  the real `L_GroundArea6` header, background scenery table, terrain table,
  metatile graphics tables, and a first `DecodeAreaData`-classified object
  overlay.

Important correction:

- `L_GroundArea1` is level 3-3 in the disassembly. World 1-1 is
  `L_GroundArea6`, paired with `E_GroundArea6`.

Current World 1-1 object-overlay scope:

- Rendered: page-1 coin/power-up question blocks, the first brick row, and the
  first decoration pipe.
- Model: active object rendering by decoded page/column and handler duration.
- Deferred: exact three-slot SMB object buffers, scrolling column updates,
  hidden block state, enemies, Mario sprites, collision, and block interaction.

See [PoC Worklog](poc-worklog.md) for step-by-step implementation notes.

## Next Steps

1. Replace the static page-1 overlay with a live three-slot `ProcessAreaData`
   state model.
2. Render World 1-1 columns page-by-page from that model.
3. Add the first enemy parser milestone for Goomba placement.
4. Add Mario sprite placement using SMB's real player sprite tiles and OAM
   positions.
5. Move stable Rust-side renderer behavior into generated Z80 routines where
   feasible.
