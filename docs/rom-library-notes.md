# Local ROM Library Notes

The ROM library is available at:

`/mnt/terachad/Emulators/EmuDeck/roms`

Observed system directories include:

- `nes`
- `mastersystem`
- `gamegear`
- `genesis`
- `snes`
- `gb`
- `gbc`
- `gba`
- `msx1`
- `msx2`
- `fds`
- `sg-1000` content mixed under `mastersystem`
- several arcade and later-console directories

## NES Samples Present

The NES directory includes good candidates for mapper research and escalating
complexity:

- `Super Mario Bros. (World).nes`
- `Balloon Fight (USA).nes`
- `Excitebike (1984-11-30)(Nintendo)(JP-US).nes`
- `Ice Climber (USA, Europe, Asia) (En).nes`
- `Gradius (USA).nes`
- `Contra (USA).nes`
- `Mega Man (USA).nes`
- `Mega Man 2 (USA).nes`
- `Metroid (USA).nes`
- `Legend of Zelda, The (USA) (Rev 1).nes`
- `Super Mario Bros. 2 (USA) (Rev 1).nes`
- `Super Mario Bros. 3 (USA) (Rev 1).nes`
- `Kirby's Adventure (USA) (Rev 1).nes`
- `Castlevania III - Dracula's Curse (USA).nes`

Suggested use:

- Start with SMB.
- Add a small mapper classifier and run it over every NES file.
- Build a table of mapper, PRG size, CHR size, trainer, battery, mirroring, and
  NES 2.0 status.
- Pick the next target from low-risk mapper 0 games.

## Master System Samples Present

The Master System directory includes useful native references:

- `Sonic The Hedgehog (USA, Europe, Brazil) (En).sms`
- `Alex Kidd in Miracle World (World) (En) (Sega Ages).sms`
- `Castle of Illusion Starring Mickey Mouse (USA, Europe, Brazil) (En) (Rev 1).sms`
- `Ninja Gaiden (Europe, Brazil) (En).sms`
- `Phantasy Star (World) (En) (Sega Ages).sms`
- `R-Type (World).sms`
- `OutRun (World).sms`
- `Zillion (Europe, Brazil) (En) (Rev 2).sms`

Suggested use:

- Inspect SMS headers and ROM sizes.
- Compare native SMS tile, palette, and bank layouts.
- Use native games as references for runtime expectations, not as source code.

One observed SMS sample:

```text
262144 bytes  Sonic The Hedgehog (USA, Europe, Brazil) (En).sms
```

That is a 256 KB ROM, representative of banked SMS software.

## Confirmed SMB Header

Path:

`/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes`

Header bytes:

```text
4e 45 53 1a 02 01 01 08 00 00 00 00 02 00 00 01
```

File size:

```text
40976 bytes
```

Interpretation:

- Valid `.nes`.
- 32 KB PRG ROM.
- 8 KB CHR ROM.
- Mapper 0/NROM.
- No trainer.
- No battery RAM.
- File size matches header-declared PRG and CHR sizes.

