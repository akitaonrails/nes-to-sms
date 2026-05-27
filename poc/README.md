# NES-to-SMS Proof of Concept

This directory is for rough, iterative experiments against
`Super Mario Bros. (World).nes`. Code here can be disposable. The important
output is evidence: extracted resources, generated SMS assets, bootable ROM
experiments, translation notes, and findings copied back into `docs/`.

## Constraints

- Do not commit commercial ROM files.
- Do not install 6502/Z80/SMS/NES tools directly on the host.
- Use project-local Docker/Docker Compose tooling when external binaries are
  needed.
- Audio is out of scope for the first PoC.

## Working Layout

- `input/` is for local-only ROM references, symlinks, or copied test inputs.
- `out/` is for generated artifacts: PRG/CHR dumps, converted tiles, assembly,
  SMS ROMs, emulator logs, screenshots, and reports.
- `notes/` is for scratch notes that are not ready for `docs/`.

Generated artifacts in `input/` and `out/` are ignored by git.

## Run

Direct host Rust run:

```sh
cargo run -- "/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes" out/smb
```

Containerized run from the repository root:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm poc
```

Containerized emulator smoke run from the repository root:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm sms-smoke
```

Open a visible Mednafen window from the repository root:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose up -d sms-visual
```

Stop the visible emulator:

```sh
docker compose stop sms-visual
```

Current outputs include `out/smb/prg.bin`, `out/smb/chr.bin`,
`out/smb/tiles.sms4bpp`, `out/smb/tiles.ppm`, `out/smb/poc.sms`, and
`out/smb/generated.asm`. The older `out/smb/z80_translation_experiment.asm`
remains as a reset-routine sketch. `out/smb/ir_lowering_demo.asm`,
`out/smb/ir_lowering_demo.bin`, `out/smb/ir_lowering_report.txt`, and
`out/smb/ir_lowering_validation.txt` are the first IR-backed lowering artifacts
from actual SMB PRG bytes. `out/smb/ir_branch_demo.asm`,
`out/smb/ir_branch_demo.bin`, `out/smb/ir_branch_report.txt`, and
`out/smb/ir_branch_validation.txt` are the first branch-aware IR artifacts.
`out/smb/ir_compare_demo.asm`, `out/smb/ir_compare_demo.bin`,
`out/smb/ir_compare_report.txt`, and `out/smb/ir_compare_validation.txt` cover
the first compare/carry-branch slice.

## First Target

Expected local ROM:

```text
/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes
```

Observed header:

```text
4e 45 53 1a 02 01 01 08 00 00 00 00 02 00 00 01
```

This is mapper 0/NROM with 32 KB PRG and 8 KB CHR, making it the lowest-risk
commercial NES target in the current library.
