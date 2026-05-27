# nes-to-sms

A Rust pipeline that translates NES (mapper 0/NROM) ROMs into Sega Master
System projects. The output is a buildable WLA-DX project: extracted
assets, an annotated 6502 disassembly, lifted IR, lowered Z80 with
SMS-native runtime calls, and a `Makefile` that produces a `.sms` ROM.

The canonical plan lives in [`docs/master-plan.md`](docs/master-plan.md).
This README is the operational on-ramp.

## Status (v1 in progress)

Workspace contains 11 Rust crates, 10 Z80 runtime files, and 280 tests
passing. The pipeline runs end-to-end on `Super Mario Bros. (World).nes`
and emits a complete WLA-DX project tree. Audio is deferred. Playable
World 1-1 (the v1 target) is still many subsystems away — what's
implemented is the **pipeline**, not the gameplay.

Tracker:

| Phase | Status |
|-------|--------|
| 0.1 Crate split + workspace | done |
| 0.2 Profile schema + loader | done |
| 0.3 In-Rust 6502 oracle | done |
| 0.4 In-Rust Z80 emulator | done |
| 0.5 WLA-DX in docker | dockerfile done; not exercised yet |
| 0.6 Shadow flag model in IR/lower | done |
| 0.7 Synthetic test ROM corpus | initial 3 cases as integration tests |
| 1.1 Full 2A03 decoder | done (151 official + stable unofficial) |
| 1.2 Function discovery | done (vector + JSR walk + profile roots) |
| 1.3 Memory access classifier | done |
| 1.4 CDL ingestion | deferred |
| 1.5 Disassembly export | done (annotated translated.asm) |
| 2.1 IR → Z80 lowering | done with shadow flags |
| 2.2 JSR/RTS → CALL/RET | done |
| 2.3 SMS runtime library | scaffolded, partial behavior |
| 2.4 Banked WLA-DX project emission | done |
| 2.5 Differential test harness | partial (oracle + emu exist, no auto-diff yet) |
| 2.6 Flag-liveness optimization | deferred |
| 3.x Hardware semantic layer | scaffolded only |
| 4.x SMB drive-through | not started |

## Workspace layout

```
Cargo.toml                workspace root
crates/
  nes_rom/                iNES / NES 2.0 header parsing
  cpu6502/                2A03 instruction decoder (256 opcodes, all modes)
  profile/                TOML profile schema + loader
  ir/                     semantic IR with explicit flags + memory tags
  oracle_6502/            2A03 interpreter for differential testing
  z80_emit/               Z80 instruction encoder + asm text emitter
  z80_emu/                Z80 interpreter for differential testing
  assets/                 CHR → SMS 4bpp, NES palette → SMS CRAM, PPM preview
  analysis/               function discovery, CFG, code/data classification
  lower/                  IR → Z80 lowering with shadow flags + runtime calls
  sms_project/            WLA-DX project emitter (Makefile, link.cfg, sms.asm)
  cli/                    `nes-to-sms` binary (pipeline orchestrator)
runtime/                  hand-written Z80 SMS runtime (10 .s files)
profiles/                 game profiles (currently: smb.toml)
docker/                   Dockerfile.toolchain (Rust + WLA-DX + Mednafen)
docs/                     plans, status, research notes
```

## Usage

### Native run

```sh
cargo run --release -p nes_to_sms -- \
    "/path/to/rom.nes" \
    profiles/smb.toml \
    out/smb \
    --runtime runtime
```

This produces `out/smb/` with:

```
out/smb/
  Makefile                WLA-DX build driver
  link.cfg                link script enumerating .o files
  sms.asm                 top-level project: memory map, ROM map, includes
  runtime/                copied from --runtime arg
  generated/translated.asm   the lowered Z80 from your ROM
  data/chr.4bpp           NES CHR converted to SMS 4bpp tiles
  data/palette.cram       32-byte SMS CRAM
  data/nametable.bin      placeholder name-table (zeros)
  reports/discovery.txt   what analysis found and didn't
  reports/lifted.txt      per-routine IR summary
  reports/lower_failures.txt   routines that couldn't be lowered (if any)
```

### Build to `.sms`

The `out/smb/` tree is a WLA-DX project. To assemble (requires WLA-DX
1.0+ on PATH or via the project Docker image):

```sh
cd out/smb && make
```

The dockerized build avoids host installs:

```sh
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm poc \
    bash -c 'cargo run --release -p nes_to_sms -- /roms/nes/...nes profiles/smb.toml out/smb --runtime runtime && cd out/smb && make'
```

## How the pipeline works

```
.nes + profile.toml
        │
        ▼
   nes_rom::parse           ── header, PRG, CHR, vectors
        │
        ▼
   profile::load            ── extra roots, labels, data regions,
        │                      replacement annotations
        ▼
   analysis::analyze        ── function discovery (vector + JSR walk +
        │                      profile roots), CFG, code/data classifier
        ▼
   ir::lift_range × N       ── per-function: 6502 → IR with explicit flags
        │                      and tagged memory accesses
        ▼
   lower::lower_routine     ── IR → Z80 (shadow flags in RAM, hardware
        │                      accesses route to runtime calls)
        ▼
   z80_emit::Program         ── byte buffer + WLA-DX assembly listing
        │
        ▼
   assets::nes_chr_to_sms_4bpp + palette mapping
        │
        ▼
   sms_project::emit_project ── writes Makefile + link.cfg + sms.asm
                                + runtime/ + generated/ + data/ + reports/
```

The Z80 output is conservative and explicit:

- 6502 `A` ↔ Z80 `A`.
- 6502 `X`/`Y` live at SMS RAM `$CB00`/`$CB01`.
- 6502 status byte lives at `$CB03` (shadow P).
- NES `$0000-$07FF` mirrors to SMS RAM `$C000-$C7FF`.
- Every flag-producing 6502 op calls a runtime helper (`rt_set_nz_a`,
  `rt_adc_a`, etc.) that updates shadow P.
- Hardware accesses (PPU, OAM-DMA, APU, controller, mapper) become calls
  to `rt_ppu_write`, `rt_oam_dma`, etc.

See `docs/master-plan.md` "Architecture" section for full details.

## Differential testing

Both an in-Rust 6502 oracle (`oracle_6502`) and an in-Rust Z80 emulator
(`z80_emu`) are implemented. The automated diff harness that runs every
discovered routine through both is **not yet wired up** — it's the next
piece of work in Phase 2.5.

For now, oracle and emu are exercised individually:

```sh
cargo test -p oracle_6502    # 46 tests
cargo test -p z80_emu        # 79 tests
cargo test -p nes_to_sms     # includes the SMB pointer-increment slice
                             # validated against the 6502 oracle
```

## Known limitations

- **Lowering coverage.** Stable unofficial opcodes (SLO, RLA, SRE, RRA,
  DCP, ISC, SAX) emit `Op::Unsupported` and fail the routine. The lifter
  detects them; the lower crate hasn't yet learned them.
- **Indirect dispatch.** `JMP (indirect)` routes through `rt_indirect_jmp`
  but the runtime doesn't yet reproduce the NMOS `$XXFF` page-wrap bug.
- **Controller strobe.** The `$4016` strobe write isn't yet emitted by
  lower; the runtime stub returns deterministic state.
- **OAM staging layout.** The runtime uses NES OAM 4-byte format at
  `$C900-$C9FF` and converts to SMS SAT at upload time.
- **Audio.** APU writes are logged to a ring buffer and discarded.
- **Code coverage on SMB.** Current discovery finds ~2.8 KB of code from
  22 profile-supplied function entries. Broadening coverage requires
  either trace-data ingestion or more profile entries.

## Repo policies

See [`AGENTS.md`](AGENTS.md). Highlights:

- No commercial ROM files committed.
- All non-Rust retro tooling lives in `docker/`.
- The engine stays SMB-agnostic; SMB knowledge lives in `profiles/smb.toml`
  and the Z80 runtime, never in Rust source.
- Progress is measured by pipeline coverage, not by lifted micro-slices.

## License

MIT OR Apache-2.0 for project code. Profile data and runtime asm are MIT.
ROM files and asset extracts are subject to their original copyright and
are not redistributed.
