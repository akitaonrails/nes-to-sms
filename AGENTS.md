# Agent Notes

## Read first

- `docs/master-plan.md` is the canonical architecture/principles doc. `docs/completion-plan.md` is the current ordered work queue. `docs/current-status-and-gaps.md` is inventory only; its old slice-driven "next step" is explicitly out of policy.
- This is now a Rust workspace, not a planning-only repo. The root `Cargo.toml` owns 13 crates under `crates/`; `poc/` is legacy/disposable experiment history unless the user asks for it.

## Commands that matter

- Full local check: `cargo test --workspace` (currently passes; `trace-sms` emits one unused-parens warning).
- Focused checks: `cargo test -p <crate>` or `cargo test -p validation <test_name>`; useful validation tests live in `crates/validation/tests/{single_ops,known_slices}.rs`.
- Format/lint before code handoff: `cargo fmt --all` then `cargo clippy --workspace --all-targets --all-features`.
- Generate an SMS project: `cargo run --release -p nes_to_sms --bin nes-to-sms -- <rom.nes> profiles/smb.toml out/smb --runtime runtime`. Add `--validate --validate-vectors N` for the differential report. Unresolved translated labels trap by default; use `--debug-unresolved-stubs` only for visual experiments.
- Assemble generated output with WLA-DX: `make -C out/smb` (writes `out/smb/sms.sms`). WLA-DX/Mednafen belong in Docker, not host installs.
- Docker gotcha: `compose.yaml` defaults to the legacy `poc/` working dir. For workspace commands use `docker compose run --rm --workdir /work poc bash -lc '<command>'`.
- Trace a built SMS ROM: `cargo run -p nes_to_sms --bin trace-sms -- out/smb/sms.sms --steps 200000`; add `--buttons start` or `--pad1-raw DF` to simulate controller input. Optional env knobs include `SMS_WATCH_ADDR=0xC000`, `SMS_DUMP_PPM=<file>`, and `SMS_DUMP_EACH_FRAME=<dir>`.

## Architecture boundaries

- CLI entrypoint is `crates/cli/src/main.rs`; pipeline wiring is `crates/cli/src/pipeline.rs`.
- Crate roles: `nes_rom` parses ROMs, `cpu6502` decodes 2A03, `analysis` discovers/classifies code, `profile` loads TOML profiles, `ir` lifts 6502, `lower` emits Z80, `z80_emit` encodes/lists Z80, `z80_emu` and `oracle_6502` power validation, `assets` converts CHR/palette data, `sms_project` writes the WLA-DX tree, `validation` compares oracle vs lowered Z80.
- Game-specific SMB facts belong in `profiles/smb.toml` or `runtime/*.s`, not in Rust source. Avoid `if SMB` logic and hard-coded SMB addresses in `.rs` files.
- Runtime helper behavior exists twice: hand-written Z80 in `runtime/*.s` and Rust-emitted test stubs in `crates/validation/src/runtime_stubs.rs`; keep them semantically aligned.

## Project constraints

- The deliverable is a porting-assistant pipeline with SMB as proof, not a promise of arbitrary automatic NES conversion.
- Do not make progress by Rust-side hand-porting SMB gameplay/rendering or by lifting isolated slices that are not reachable through the analyzer/profile pipeline.
- Fail closed: unsupported opcodes, unknown indirect targets, and untagged/unsupported memory semantics should report errors or validation skips, not silently emit bogus Z80.
- `--validate` writes `reports/validation.txt` but intentionally does not fail the project generation on red results.
- Do not commit ROMs or generated ROM artifacts. `.gitignore` excludes `*.nes`, `*.sms`, `out/`, `**/out/`, and `target/`; use local ROM paths only in examples/docs.
- External retro tooling (assemblers, emulators, trace tools) should stay reproducible through `docker/`/`compose.yaml`, not ad hoc host setup.
