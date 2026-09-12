# Handoff — Zelda (MMC1) flagship conversion

Last updated: 2026-09-12. All work below is committed and pushed; the working
tree is clean at `94ebbbf`.

## Context and goal

The plan is **one popular game per mapper** (`docs/mapper-roadmap.md`), not breadth
across small games. Approved order: Adventure Island → **Zelda** → Battletoads →
Contra → … The flagship per mapper is NROM=SMB, **MMC1(1)=Zelda**, UxROM(2)=
Castlevania/DuckTales/Metal Gear, CNROM(3)=Adventure Island, MMC3(4)=SMB3/Kirby,
etc. (An earlier detour converted small NROM games — Lode Runner/Bomberman/Ice
Climber — because NROM was the only mapper the *conversion* pipeline already
supported. That was off-plan; the real work is extending the pipeline+runtime to
each flagship's mapper. See `docs/conversion-strategy-2026-09.md`.)

**Zelda** = mapper 1 (MMC1), 128 KiB PRG (8×16K banks), 8 KiB CHR-**RAM**,
battery. PRG mode 3: `$8000-$BFFF` switchable 16K, `$C000-$FFFF` fixed to the last
bank. Vectors (fixed bank): nmi=`$E484`, reset=`$FF50`, irq=`$FFF0`. ROM at
`.roms/zelda.nes`; profile `profiles/zelda.toml`; build output `out/zelda/`.

## Ground-truth tooling (built this session — use it)

`crates/nes_ref` wraps **tetanes** (a cycle-accurate NES emulator) as the real
reference. Key finding: the in-repo `oracle_6502` (used by `frame-diff`) is
simplified and only matches SMB because both were co-tuned — it cannot validate
new games. tetanes can.

- `./target/release/nes-ref-dump <rom> <frames> [addr:len | out.ppm]` — dumps the
  real NES 2 KiB RAM (hash or hex window) or renders the authoritative frame.
- **Correct-grade check** (the parity gate for any new game): compare real-NES RAM
  to the SMS subject's NES RAM, phase-scanning subject `--steps` to align free-run:
  ```
  ./target/release/nes-ref-dump .roms/zelda.nes 120 0x0000:2048 | grep '$' | sed 's/.*: //' > tet.txt
  SMS_DUMP_RAM=0xC000:0x800 ./target/release/trace-sms out/zelda/sms.sms --steps N 2>/dev/null | grep '^RAM' | sed 's/.*: //' > sub.txt
  # count non-stack ($0100-$01FF excluded) byte diffs; ~1% = correct-grade, SMB baseline is 11/1792
  ```
  `SMS_DUMP_RAM=0xC000:len` maps SMS `$C000` → NES `$0000`.

## What is done (Zelda now converts → builds → runs → banks correctly)

Commits `e92e712`, `65c0424`, `c75cff2`, `169f1db`, `94ebbbf`:

1. **MMC1 geometry** (`crates/nes_rom/src/lib.rs`): `MapperPolicy::Mmc1 { bank_count }`
   mirrors UxROM's switchable+fixed layout for all analysis/lowering; MMC1 mode-3
   geometry is *identical* to UxROM. `resolve_mapper_policy` admits mapper 1
   (2/4/8/16 banks). Only the runtime bank-switch differs.

2. **WRAM in cartridge SRAM** (`runtime/wram.s`): `rt_wram_read`/`rt_wram_write` back
   NES `$6000-$7FFF` in SRAM **bank 1** (`$FFFC=$0C`, slot-2 `$8000-$9FFF`). CHR-RAM
   uses SRAM **bank 0** (`$FFFC=$08`) so they never overlap. `ir` emits `StaMem` for
   PRG-RAM (not fail-closed); `lower` routes PRG-RAM `StaMem`/`LdaMem` (const + X/Y
   indexed) to the helpers when `mapper==1`; `sms_project` admits mapper 1 in the
   banked PRG-asset/config validation. **Limitation:** STX/STY and indirect WRAM
   modes are still fail-closed — extend `emit_wram_addr_hl` / the STX/STY arms in
   `crates/ir/src/lib.rs` if Zelda hits them.

3. **`rt_wram_read/write` registered** in `pipeline.rs` `RUNTIME_PROVIDED` (~line
   3273) so the lowerer doesn't emit duplicate unresolved stubs.

4. **Banked-dispatch discovery** (`profiles/zelda.toml` + `crates/profile`): Zelda
   calls switchable routines as `LDA #bank; JSR $FFAC; JSR target`, where `$FFAC` is
   its MMC1 PRG-register serial writer (`8D 00 E0` ×5 with `4A` between). Scanning
   the ROM for `A9 <bank> 20 AC FF` followed by `20 <lo> <hi>` JSRs harvests all 40
   (target, bank) pairs, expressed as `[[bank_call]]` (dispatch rewrite) +
   `[[bank_entry]]` (discovery root). `validate_bank_annotations` and the
   `return_consume` bank check now admit mapper 1. Discovery: 18→44 functions,
   722→1614 code bytes; the switchable-window dispatch trap is gone.

5. **MMC1 serial latch** (`runtime/apu_stub.s` `rt_mmc1_write`; `sms_project` emits
   `.define NES_MMC1 1` for mapper 1): the 5-bit shift register, with its persistent
   state in **cartridge SRAM bank 1 at `$BFFF`** (this sidesteps the "no free
   internal RAM" wall — see Gotchas). bit7 resets; the 5th write commits to the
   register chosen by address bits 13-14; only the PRG register (`$E000-$FFFF`)
   remaps the bank via `$CB62` + `rt_restore_prg_window`. `rt_mapper_write`
   dispatches to it under `NES_MMC1`.

**Current runtime state:** Zelda builds, runs without trapping, drives PPU writes,
briefly sets `ppu_mask=$01`, and its RAM tracks the real NES **~85% (266/1792)**.
`SMB stays byte-exact` (`frame-diff .roms/smb.nes out/smb/sms.sms --frames 120` →
NO DIVERGENCE) — every change is gated on mapper 1 / `NES_MMC1`.

## Remaining work (priority order)

1. **Indirect-dispatch-through-RAM** — the 266-byte divergence. The unresolved
   labels are mostly `$6Dxx-$72xx` (WRAM) targets: Zelda builds jump tables / copies
   dispatch code into RAM and `JMP`s through it, which static discovery can't
   follow. This is the same hard class Ice Climber hit. Options: (a) ground-truth
   harvest the indirect targets by observing tetanes at each `JMP ($zp)` site; (b) a
   runtime indirect-dispatch that maps a live NES RAM target to its translated entry.
   Inspect `out/zelda/reports/unresolved_labels.txt` and the `JMP ($..)` sites in the
   discovered code first.

2. **CHR-RAM rendering** — `NES_CHR_RAM` is already defined and the `raw_ciram_sram`
   + CV1 bg path exists (`runtime/chrmap.s`, `runtime/bg_cv1.s`, `runtime/ppu.s`).
   Rendering isn't stable yet (`ppu_mask` only flickers to `$01`). This likely
   depends on #1 (the init that enables display runs through the RAM dispatch).

3. **WRAM addressing completeness** — add STX/STY and indirect ($zp),Y modes to the
   WRAM path if Zelda's save/inventory code needs them (currently fail-closed).

4. **Verification** — once it renders, compare against
   `nes-ref-dump .roms/zelda.nes 120 out.ppm`. Drive input with
   `trace-sms --buttons-at-frame N:a,b` (both-buttons chord = Start if action input
   is enabled; Zelda may need a proper input profile).

## Gotchas / lessons

- **SMS internal RAM is FULL.** The runtime diagnostic says
  `no_internal_ram_without_reclaim`. Three allocation traps hit while hunting for a
  free byte: `$CA3B` is the high byte of a `ld ($ca3a),de`; `$D4A0` is inside
  `SAT_ATTRS` (`$D480-$D4BF`); `$DE40` is live stack space. **Solution used: put
  MMC1 shift-register state in cartridge SRAM, not internal RAM.** Do the same for
  any future banked-mapper state. The RAM map is documented in `runtime/boot.s`
  (top comment) — trust it over a symbol grep, and beware word-sized writes.
- **SMB is the regression gate.** After any lower/ir/runtime change, run
  `frame-diff .roms/smb.nes out/smb/sms.sms --frames 120` — it must print NO
  DIVERGENCE. All MMC1 changes are `.ifdef NES_MMC1` / `mapper==1` gated.
- **Regen copies `runtime/` into `out/`** on every convert — always edit
  `runtime/*.s` (not `out/zelda/runtime/*.s`) and re-run the converter.

## Build / run commands

```
# convert
cargo run --release -q -p nes_to_sms --bin nes-to-sms -- .roms/zelda.nes profiles/zelda.toml out/zelda --runtime runtime
# assemble/link (docker)
DOCKER_UID=1026 DOCKER_GID=1026 docker compose run --rm --workdir /work poc bash -lc "make -C out/zelda"
# run + diagnostics
./target/release/trace-sms out/zelda/sms.sms --steps 8000000 2>/dev/null | grep -iE "Runtime diagnostics|Cpu state"
# diagnostics envs: SMS_DUMP_PPM=f.ppm, SMS_DUMP_RAM=0xC000:len, SMS_WATCH_VRAM=addr:len,
#                   SMS_DUMP_VRAM=addr:len, SMS_DUMP_CRAM=1
```
