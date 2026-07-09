# Handoff: Castlevania 1 (CV1) — real-emulator hang

Last updated: 2026-07-09. Branch: master. All work committed; SMB
byte-for-byte green throughout (three acceptance routes, 375 tests).

## One-line status

CV1 (mapper 2 / UxROM, CHR-RAM) is **feature-complete in the
instrumented harness but HANGS on real emulators** (GPGX, Mednafen).
The hang is diagnosed down to a single-opcode CPU-emulation
divergence but the exact opcode is not yet pinned. SMB is done and
plays. CV1 does not.

## Exactly what is pending (the ONE blocker)

CV1 boots, runs its init, and then **livelocks in its first game task
("task 0", the copyright/intro)** — the 6502 task counter at NES RAM
`$18` never leaves 0 on a real emulator. On a real NES it reaches 1
within ~30 frames without any input, so advancing is correct and the
hang is a bug.

Proven facts (do not re-litigate):
- It is a **CPU-emulation divergence**, not data/pacing/timing.
  Evidence: `SMS_LOAD_STATE` transplants a real Mednafen crash state
  (RAM+VRAM+CRAM+cart SRAM+bank latches+Z80 regs) into `z80_emu` and
  runs forward — from Mednafen's *identical* stuck state, `z80_emu`
  ADVANCES the task. So `z80_emu` executes some instruction
  differently from Mednafen's accurate Z80.
- The real-6502 reference (`frame-diff`) advances task 0 without
  input → advancing is correct → the GENERATED Z80 code is wrong on
  real hardware, and `z80_emu` masks it. This is the
  "trace-green ≠ Mednafen-correct" class.
- `z80_emu` matches the 6502 reference (frame-diff is green except 4
  benign anchor-phase bytes), so frame-diff **structurally cannot**
  see this bug: it uses `z80_emu` as its own subject.
- Localized to the task-0 loop, which cycles through runtime helpers
  `rt_ror_a` ($0x1010), `rt_dec_mem` ($0x111E), `rt_read_zp_ptr_y`
  ($0x0E62), `_dispatch_remap_de` ($0x0DA4) and translated branches
  `JP P`/`JP Z` in bank 6 around $60F7-$6220 (SMS addresses; see
  `out/cv1/sms.sym`).

Ruled out empirically (do not repeat these):
- Pacing: `SMS_REAL_PACING=1` (15K insn/frame, boolean pending) still
  advances.
- RAM init: `$FF`-fill (hostile mode) still advances.
- Undocumented Z80 flags 3/5: `z80_emu` now models them; CV1 still
  advances and SMB stays green, so codegen doesn't read them.
- Hot-loop flag helpers audited: `rt_dec_mem` correct; `rt_ror_a` has
  a LATENT bug (when the rotate's new carry is set, the carry-handling
  block clobbers A before `bit 7,a`, so reconstructed 6502 N is wrongly
  0) — but it is deterministic across emulators, so NOT this
  divergence. Fix it anyway for correctness (see TODOs).

## THE decisive next step

Pin the divergent opcode via **instruction-level differential
execution against a correct Z80**. Two options:

1. **Mednafen debugger trace (fastest).** Mednafen has a built-in
   debugger with execution logging. Drive it (headless via xdotool,
   or a config) to dump a Z80 instruction trace of CV1's first
   ~3M instructions. Then run `z80_emu` on the same ROM from the same
   start and diff the two PC/flag streams. First divergence = the bug.
   The savestate parser (gzip + named chunks MAIN/Z80/VDP/CART, see
   `load_mednafen_state` in `crates/cli/src/bin/trace_sms.rs`) already
   works and can align states.

2. **Vendor an independent reference Z80 core** (e.g. a known-correct
   Rust Z80 crate, or port one) and lockstep it against `z80_emu`
   instruction-by-instruction on the task-0 path; assert equal
   registers+flags after each step. First mismatch = the bug. This is
   the more robust long-term tool (also catches future divergences).

Once the opcode is known, the fix is usually a few lines in
`crates/z80_emu/src/lib.rs` (if `z80_emu` is wrong) OR in the flag-
reconstruction emitters / runtime helpers (if the generated code is
wrong). After the fix: re-verify CV1 on Mednafen AND re-run the full
SMB gate (the fix likely touches `z80_emu`, which the oracle uses).

## Tools already built for this (all committed)

- `SMS_LOAD_STATE=<raw mednafen state>` — transplant a savestate into
  `z80_emu`. Gunzip the `.mcs` first: `gzip -dc s.mcs > s.raw`.
- `SMS_REAL_PACING=1` — hostile pacing (15K insn/frame, `$FF` RAM).
- `SMS_TRAP_RAM_EXEC=1` — halt+dump when the Z80 executes from RAM.
- `SMS_DUMP_ON_ENABLE=<dir>` — framebuffer PPM at each display-enable
  edge (CV1 hides transitions behind display-off).
- `SMS_WATCH_ADDR=0xC018` — watch the task counter ($18).
- `FD_WATCH_TASK=1` (frame-diff) — log the reference's $18/$19/$0D.
- Boot beacons in `runtime/boot.s` (NES_CHR_RAM-guarded): border/
  backdrop colors mark boot milestones (green = entering translated
  reset, white = display-enable applied). Consider stripping for a
  "clean-looking" build (`grep rt_boot_beacon runtime/boot.s`).

## How to reproduce / observe

- Build CV1: `cargo run --release -p nes_to_sms --bin nes-to-sms -- "<CV1.nes>" profiles/cv1.toml out/cv1 --runtime runtime`
  then `docker compose run --rm --user root --workdir /work poc bash -lc 'cd out/cv1 && make'`.
  (CV1 ROM: `/mnt/terachad/Emulators/EmuDeck/roms/nes/Castlevania (USA) (Rev 1).nes`.)
- See it advance in-harness: `SMS_WATCH_ADDR=0xC018 target/release/trace-sms out/cv1/sms.sms --steps 40000000` → task goes 0→1→2→5.
- See it hang for real: `docker/run_gpgx.sh out/cv1/sms.sms` (or a
  Mednafen recording) → boots, beacons, then blank/frozen.

## Secondary TODOs (not the blocker)

- Fix `rt_ror_a` N-flag-when-carry-set (correctness; `runtime/flags.s`).
- The 4 benign anchor-phase divergent bytes ($000F/$0010/$004D/$00A9)
  — cosmetic, chase only after the hang.
- Once CV1 renders: record a CV1 acceptance route (its parity gate,
  like SMB's three) and add it to `profiles/cv1/acceptance/`.
- Strip or keep boot beacons per preference.

## After CV1: the mapper ladder (docs/mapper-plan.md)

CV1 done → Gradius (mapper 3 / CNROM) → Blaster Master (1 / MMC1) →
Bonk's Adventure (4 / MMC3) → Marble Madness (7 / AxROM) →
Castlevania III (5 / MMC5). The banked-dispatch, CHR-RAM-SRAM-mirror,
re-entrant-NMI, and BRK-as-interrupt machinery built for CV1 is
generic and carries forward. **Hard rule: every mapper-phase commit
must pass the full SMB gate (3 routes byte-for-byte + 375 tests) and
keep Alter Ego building.** No ROMs committed to the repo.

## Key files

- `crates/z80_emu/src/lib.rs` — the Z80 emulator (likely fix site).
- `crates/cli/src/bin/trace_sms.rs` — trace harness + `SMS_LOAD_STATE`.
- `crates/cli/src/bin/frame_diff.rs` — 6502 differential oracle.
- `runtime/flags.s`, `runtime/dispatch.s`, `runtime/ppu.s`,
  `runtime/ntmap.s`, `runtime/chrmap.s`, `runtime/boot.s` — runtime.
- `crates/lower/src/lib.rs` — 6502→Z80 codegen + flag emitters.
- `profiles/cv1.toml` — CV1 profile (ground-truth bank entries).
- `docs/mapper-plan.md` — full mapper phase log + this diagnosis.
