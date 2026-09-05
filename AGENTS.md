# Agent Notes

This file is the on-ramp for AI coding agents working on `nes-to-sms`.
Read it first. The project is a Rust workspace that translates NES
(mapper 0 / NROM) ROMs into buildable Sega Master System projects.

## Project overview

`nes-to-sms` is a pipeline, not a hand-port of any one game:

```
.nes ROM + profile.toml
    → parse / classify / discover
    → lift 6502 to semantic IR
    → lower IR to Z80
    → emit WLA-DX project (Makefile, link script, runtime, assets)
    → assemble to .sms
```

Super Mario Bros. is the active regression target, but the engine stays
game-agnostic. Game-specific facts live in TOML profiles and the Z80
runtime, not in Rust source.

Current state (measured from the workspace):

- Rust workspace with **13 crates** under `crates/`.
- About **35 kLOC of Rust** and **9.5 kLOC of hand-written Z80 runtime**.
- **`cargo test --workspace` passes** with roughly **478 tests**.
- The pipeline runs end-to-end on `Super Mario Bros. (World).nes` and
  emits a complete WLA-DX project tree.
- Generated ROMs boot under Mednafen; real-SMS speed remains an open
  issue. Phase S (docs/speed-recovery-plan.md, 2026-09-04) brought SMB
  from ~4.4× to ~1.98× over the frame budget — full speed fits GPGX's
  standard ≤200% overclock menu; frame-diff has RAM-parity AND
  VDP-parity oracles (FD_VDP_DUMP/FD_VDP_CHECK golden workflow — hashes
  are timing-sensitive, see docs/visual-parity.md) plus an
  FD_FAR_EDGES profile feeding the profile-driven bank placer; docs/handport-comparison.md explains the ceiling.
- Visual fidelity is gated by the NES ground-truth frame oracle
  (FD_NES_DUMP + scripts/nesref/compare_frames.py, docs/visual-parity.md).
  The BG variant ring uses NT refcounts (chrmap.s BGV_REFCNT) so visible
  slots are never recycled; `SMS_EXPECT_BGV_CONSISTENT=0` on the
  1-1-clear route is the regression guard. Known cosmetic limits
  (sprite behind-priority, unmapped score-popup tiles) are listed in
  docs/visual-parity.md.
- SMB builds use `[translation] stack_discipline = "native"` (native
  CALL/RET via `rt_far_tail`) and thirteen `[[replacement]]` hooks in
  `runtime/hooks_smb.s`. The canonical SMB test ROM is `.roms/smb.nes`
  (NOT the EmuDeck "Super Mario Bros. (World).nes" — different dump;
  the profile's `MoveLakitu` at $CF28 is data there).

## Canonical documentation

- `docs/master-plan.md` — architecture, principles, RAM/ROM layout,
  phased goals, and validation strategy. This wins when docs conflict.
- `docs/completion-plan.md` — the current ordered work queue.
- `docs/current-status-and-gaps.md` — status inventory only; do not use
  its old slice-driven "next step" as policy.
- `README.md` — operational on-ramp, but some counts (crates, tests,
  progress percentages) can become stale; trust the workspace and the
  docs above.

## Technology stack

- **Language:** Rust 1.95+ with edition 2024.
- **Build system:** Cargo workspace rooted at `Cargo.toml`.
- **Assembler / linker:** WLA-DX (Z80) and `wlalink`.
- **Emulators for validation:** Mednafen, Genesis Plus GX via Docker,
    Emulicious as a cross-check.
- **Containerization:** `docker/Dockerfile.toolchain` + `compose.yaml`.
- **No host installs policy:** WLA-DX, Mednafen, and other retro tooling
  should only be installed inside the Docker image, not on the host.

## Workspace layout

```
Cargo.toml              workspace root (resolver = "3", edition 2024)
crates/
  nes_rom/              iNES / NES 2.0 header parsing, PRG/CHR/vectors
  cpu6502/              2A03 instruction decoder (all official + stable
                        unofficial opcodes; unstable opcodes decode to
                        mnemonics but fail closed in the pipeline)
  analysis/             function discovery, CFG, code/data classification
  profile/              TOML profile schema + loader
  ir/                   semantic IR with explicit flags + memory tags
  lower/                IR → Z80 lowering (shadow flags, runtime calls)
  z80_emit/             Z80 instruction encoder + WLA-DX asm text emitter
  z80_emu/              in-Rust Z80 interpreter for differential tests
  oracle_6502/          in-Rust 6502 interpreter for differential tests
  assets/               CHR → SMS 4bpp, palette mapping, PPM previews
  sms_project/          WLA-DX project writer (Makefile, link.cfg, sms.asm)
  validation/           differential harness: oracle vs. z80_emu
  cli/                  `nes-to-sms` binary + `trace-sms`, `frame-diff`,
                        `z80-diff`, `replay-state` utilities
runtime/                hand-written Z80 SMS runtime (.s files)
profiles/               game profiles: smb.toml, cv1.toml, alterego.toml, cv3.toml
profiles/smb/           SMB-specific acceptance routes / checkpoints
docs/                   plans, status, research notes
tools/                  small shell/python helpers for iteration
tests/synthetic/        synthetic test ROM inputs (managed by tests)
poc/                    legacy proof-of-concept history; treat as
                        disposable unless a task explicitly asks for it
```

## Crate responsibilities

| Crate | Responsibility |
|-------|----------------|
| `nes_rom` | Parse iNES headers, split PRG/CHR, read reset/NMI/IRQ vectors. |
| `cpu6502` | Decode every 6502 opcode and addressing mode. |
| `analysis` | Discover functions from vectors + JSR walk + profile roots; classify code vs. data; tag memory accesses. |
| `profile` | Load TOML profiles: vectors, functions, labels, data regions, jump tables, replacements, RAM tags. |
| `ir` | Lift 6502 into semantic IR with explicit flags and region-tagged memory ops. |
| `lower` | Lower IR to Z80 using SMS RAM shadows for X/Y/S/P and runtime calls for hardware. |
| `z80_emit` | Encode Z80 bytes and emit readable WLA-DX assembly. |
| `z80_emu` | Minimal Z80 interpreter covering exactly the opcode subset the back end emits. |
| `oracle_6502` | Instruction-accurate 6502 interpreter; decimal mode is a no-op (2A03). |
| `assets` | Convert NES CHR to SMS 4bpp tiles, build palettes, emit name tables. |
| `sms_project` | Write the complete buildable WLA-DX project tree. |
| `validation` | Run randomized differential tests: 6502 oracle vs. lowered Z80 under emu. |
| `cli` / `nes_to_sms` | Command-line pipeline orchestrator and helper binaries. |

## Build and test commands

Full workspace check:

```sh
cargo test --workspace
```

Focused checks:

```sh
cargo test -p <crate>
cargo test -p validation --test single_ops
cargo test -p validation --test known_slices
cargo test -p nes_to_sms --test synthetic_pipeline
```

Format and lint before handoff:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
```

Clippy currently emits warnings in several binaries/tests but finishes
successfully. Do not mask warnings unless a task explicitly requires it;
prefer fixing them or leaving them visible.

Generate an SMS project:

```sh
cargo run --release -p nes_to_sms --bin nes-to-sms -- \
    "/path/to/rom.nes" \
    profiles/smb.toml \
    out/smb \
    --runtime runtime
```

Add `--validate --validate-vectors N` to run the differential harness over
every lifted routine (slow on a large ROM; defaults to 32 vectors per
routine). By default unresolved translated labels trap at runtime via
`rt_unresolved_jsr`; use `--debug-unresolved-stubs` only for visual
experiments.

Assemble the generated output with WLA-DX:

```sh
make -C out/smb
```

This writes `out/smb/sms.sms`. WLA-DX and Mednafen should be used through
Docker, not installed on the host.

Docker commands:

```sh
# Build the toolchain image
docker compose build poc

# The compose service defaults to the legacy /work/poc working dir.
# For workspace commands, override the workdir:
docker compose run --rm --workdir /work poc bash -lc \
    'cargo run --release -p nes_to_sms -- /roms/nes/Super\ Mario\ Bros.\ \(World\).nes profiles/smb.toml out/smb --runtime runtime && cd out/smb && make'

# Mednafen smoke test (requires a built ROM)
docker compose run --rm poc bash scripts/smoke_sms.sh out/smb/sms.sms
```

Trace a built SMS ROM:

```sh
cargo run -p nes_to_sms --bin trace-sms -- out/smb/sms.sms --steps 200000
```

Options include `--buttons start`, `--pad1-raw DF`, `--expect-no-trap`.
Environment knobs include `SMS_WATCH_ADDR=0xC000`, `SMS_DUMP_PPM=<file>`,
and `SMS_DUMP_EACH_FRAME=<dir>`.

Other helper binaries:

```sh
cargo run -p nes_to_sms --bin frame-diff -- ...
cargo run -p nes_to_sms --bin z80-diff -- ...
cargo run -p nes_to_sms --bin replay-state -- ...
```

## Code style guidelines

- Match the surrounding file: naming, comment density, and structure.
- Keep the engine **game-agnostic**. SMB facts belong in `profiles/smb.toml`
  or `runtime/*.s`, never in Rust source. Avoid `if game == "smb"` and
  hard-coded SMB addresses in `.rs` files.
- **Fail closed:** unsupported opcodes, unknown indirect targets, and
  untagged/unsupported memory semantics must report errors or validation
  skips, not silently emit bogus Z80.
- Make **minimal** changes. A bug fix does not need a surrounding cleanup,
  and a small feature does not need premature abstraction.
- Runtime helper behavior is implemented twice: hand-written Z80 in
  `runtime/*.s` and Rust-emitted stubs in
  `crates/validation/src/runtime_stubs.rs`. Keep them semantically aligned;
  a divergence surfaced by the harness is a bug.
- Use workspace-level dependencies. Each crate's `Cargo.toml` should
  reference workspace crates with `{ workspace = true }`.
- Comments and docstrings that describe old behavior must be updated when
  the code changes.

## Testing instructions

The test pyramid:

1. **Unit tests** inside each crate (`cargo test -p <crate>`).
2. **Decoder tests** in `cpu6502` for every opcode/addressing mode.
3. **Oracle/emu tests** in `oracle_6502` and `z80_emu` for instruction
   semantics.
4. **Differential tests** in `validation/tests/single_ops.rs` and
   `validation/tests/known_slices.rs`. These lift tiny 6502 routines,
   lower them to Z80, and compare the 6502 oracle against the Z80 emulator
   on randomized input vectors.
5. **Integration tests** in `crates/cli/tests/synthetic_pipeline.rs` run
   synthetic NES ROMs through the full pipeline.
6. **End-to-end validation** on real ROMs: generate a project, assemble it,
   and run it under Mednafen or `trace-sms` with acceptance routes stored
   in `profiles/smb/acceptance/`.

Validation constraints:

- The differential harness skips routines that touch hardware (PPU/APU/
  OAM-DMA/controller/mapper) or use indirect dispatch, because those need
  the full runtime to validate meaningfully.
- `--validate` writes `reports/validation.txt` but intentionally does not
  fail project generation on red results.
- Every translated routine should eventually be validated against the
  in-Rust 6502 oracle; do not accept ad-hoc toy interpreters as proof.

## Security and repository policies

- **Do not commit commercial ROM files.** `.gitignore` excludes `*.nes`,
  `*.sms`, `out/`, `**/out/`, `target/`, and generated artifacts.
- Keep local ROM paths out of committed code and docs; examples may use
  placeholder paths.
- Do not commit secrets, credentials, or personal paths.
- Docker bind-mounts ROM directories read-only (`:ro`) where possible.
- Generated reports under `out/<project>/reports/` may contain excerpts of
  PRG bytes or asset data; they are gitignored and should not be committed.

## Architecture boundaries and constraints

- **Pipeline-driven, not slice-driven.** A unit of work is "the pipeline can
  now cover X end-to-end," not "I lifted one more SMB micro-routine."
  Routines must be reachable through the analyzer/profile pipeline.
- **No Rust-side hand-ports.** Do not reimplement SMB rendering or game
  logic in Rust. Either translate the routine through the pipeline, or
  label the screen a fixture.
- **Generic engine, game-specific profile.** The Rust crates stay
  SMB-agnostic. Labels, RAM maps, replacements, jump tables, and data
  regions live in `profiles/*.toml` and the Z80 runtime.
- **Verbose and boring first.** Conservative Z80 with shadow flags and
  RAM-backed X/Y is the default. Native-Z80-flag shortcuts and register
  allocation come only after correctness is proven.
- **Banked from day one.** The SMS output assumes a banked ROM layout even
  for NROM games, so future MMC1/MMC3 targets are not a rewrite.

## SMS RAM layout (contract with the runtime)

The lower crate and runtime agree on these addresses:

```
$C000-$C0FF   NES zero page mirror
$C100-$C1FF   Emulated 6502 stack page
$C200-$C7FF   NES RAM mirror ($0200-$07FF)
$C800-$C8FF   VRAM update buffer
$C900-$CAFF   Sprite attribute staging
$CB00-$CBFF   Runtime state (X at $CB00, Y at $CB01, shadow P at $CB03, etc.)
$CC00-$D2FF   Folded SMS per-cell subpalette shadow (temporary)
$D300-$D3FF   Dirty-metadata reserve
$D600-$D9FF   BG variant cache
$DA00-$DD7F   BG base-slot shadow
$DD80-$DFFD   Native Z80 stack headroom
```

The Z80 SP lives at `$DFFE` and grows down. Native Z80 stack and emulated
6502 stack are separate.

## Common gotchas

- `compose.yaml` sets `working_dir: /work/poc` by default. Use
  `--workdir /work` for workspace commands.
- `trace-sms` currently expects a ROM path even for `--help` and will
  panic if none is supplied.
- The README and older docs may cite stale test counts or crate counts;
  verify with `cargo test --workspace` and `ls crates/`.
- The `poc/` directory is legacy. Unless a task says otherwise, work in
  the root workspace.
- `--validate` is slow on SMB because it runs every discovered routine.
  Use it intentionally, not on every edit.
- Unresolved translated labels trap by default. Only enable
  `--debug-unresolved-stubs` for short visual experiments.
- **RetroArch config location (cost hours if missed):** the bundled
  `out/emulator-host/RetroArch-Linux-x86_64/RetroArch-Linux-x86_64.AppImage`
  prints "Setting $HOME to …AppImage.home" at startup, but it does **not**
  read its config from that portable home — it reads and writes
  `~/.config/retroarch/retroarch.cfg` (the user's real home) plus per-core
  options in `~/.config/retroarch/config/Genesis Plus GX/Genesis Plus GX.opt`.
  Editing anything under `…AppImage.home/.config/retroarch/` has no effect.
  For the 8BitDo Ultimate 2 (and any pad RetroArch reports "… not
  configured"): set `input_autodetect_enable = "false"` and
  `config_save_on_exit = "false"` in the real cfg and bind
  `input_player1_*_btn` explicitly (udev button order for that pad:
  b=0 a=1 y=2 x=3 l=4 r=5 select=6 start=7 l3=9 r3=10, D-pad = hat
  `h0up/h0down/h0left/h0right`); `evtest /dev/input/event24` confirms the
  pad emits events. Overclock lives in the per-core `.opt`
  (`~/.config/retroarch/config/Genesis Plus GX/Genesis Plus GX.opt`, since
  `game_specific_options=true`/`global_core_options=false`) as
  `genesis_plus_gx_overclock = "NNN"` — a **bare number, NO `%`** (steps
  100/125/…/500). A value with `%` (e.g. `"250%"`) is silently rejected and
  reset to default `"100"` on the next exit, so it never applies — which looks
  exactly like "overclock does nothing." Confirm a value took by a load→exit
  cycle: if the `.opt` still holds it, it's valid; if it reverted to `"100"`,
  the format was wrong. Core options save on exit independently of
  `config_save_on_exit`, so edit the `.opt` only while RetroArch is stopped.
  SMB needs ~2.07x worst-case, so `"300"` is smooth. Stop RetroArch
  (`pkill -f RetroArch-Linux` / `pkill -f genesis_plus_gx_libretro`) before
  editing; launch with Bash `run_in_background: true` (the AppImage wrapper
  re-execs and detaches, so plain `&`/`nohup`/`setsid` redirects lose the
  process and its log, and `pgrep -f RetroArch-Linux` matches your own shell
  command — grep `[g]enesis_plus_gx_libretro` instead).

## License

Project code is MIT OR Apache-2.0. Profile data and runtime assembly are
MIT. ROM files and extracted assets remain subject to their original
copyright and are not redistributed.
