# Handoff: Castlevania 1 (CV1) — real-emulator boot wild-jump

Last updated: 2026-07-11. Branch: master. SMB fully green throughout
(three parity routes byte-for-byte, 1-1-clear trace acceptance 4/4,
395 tests, Alter Ego generates + assembles).

## One-line status

The old "CPU-emulation divergence" class is **FIXED and verified
gone** (z80-diff lockstep clean; transplant now agrees with
Mednafen). CV1 still fails on real emulators, but the blocker is now
sharply narrowed: **a wild jump into work RAM during early init on
Mednafen** (not reproducible in-harness), likely causing an infinite
reboot cycle.

## What changed since the last handoff (all committed)

- z80_emu accuracy: MEMPTR/WZ, CP/BIT undocumented bits 3/5, EI
  semantics. Plus the external session's indexed ADC/SBC codegen fix
  (A preserved on native stack; rt_read_indexed clobbers BC).
- `z80-diff` (cargo feature `z80-diff`): locksteps z80_emu against
  rustzx-z80. CLI: `z80-diff --rom out/cv1/sms.sms --state <raw
  mednafen state> --sym out/cv1/sms.sym --steps N`.
- Runtime perf: SMB 4.7x -> 0.43x budget. A stackless-rotate carry
  bug (RNG killer) was found and fixed via frame-diff parity.
- trace-sms script clock: counts delivered game frames (mirrors the
  boot.s NMI gates), not raw injections — overrun-swallowed frames no
  longer desync routes.
- 512K layout rebalanced: translated 4-16, PRG data 17-24 (base is
  `sms_project::NES_PRG_BANK_BASE = 17`), assets 25-31. The perf
  rework grew CV1's translated code past the old 12-bank budget.
- boot.s: three `jr` to `_irq_skip_translated_nmi` became `jp`
  (out of 8-bit range under NES_CHR_RAM).

## Evidence chain for the new diagnosis (do not re-derive)

From a Mednafen savestate at 90s (`out/cv1_state_new.mc0`; gunzip →
`/tmp/cv1_new.raw`):

1. Task counter NES `$18` = 0 (work-RAM offset 0x18 of MAIN/RAM).
2. `z80-diff` lockstep from that state: **2,000,000 steps, zero
   divergence** — z80_emu matches rustzx exactly on this path.
3. `SMS_LOAD_STATE` transplant into trace-sms: task **stays 0** — the
   old "z80_emu advances where Mednafen hangs" proof no longer
   reproduces. Both CPUs agree now; the CPU-divergence class is dead.
4. The savestate Z80 PC = **$CE0A — inside work RAM**, IFF1=1, IM1,
   not halted. RAM there is zeros (Z80 NOP slide). $CE0A falls in the
   $CC00-$D2FF nametable sub-palette shadow, which a healthy
   in-harness run fills with a `00 03` pattern; on Mednafen it is
   empty because the game never wrote its nametables.
5. Runtime state in the same snapshot: $CB28 ready=1, $CB1A
   started=1, $CA11 depth=1 (normal: CV1's first NMI is resident),
   $CB08=$B0 (NMI enable set), VDP reg1=$B2 (frame INT enabled,
   display OFF), **$CB2D=$B2 deferred reg-1 write pending and never
   applied** — so no top-level frame IRQ ran presentation after that
   write. Zero page nearly empty; palette CRAM only has the beacon
   grayscale; SRAM head zeros.
6. Reading: execution wild-jumped into zeroed RAM, NOP-slides toward
   $E000+ (RAM mirror), wraps to $0000 (boot), **reboots** — an
   endless boot cycle. The all-white screen is the display-enable
   beacon backdrop, repainted every cycle.
7. In-harness from reset (even `SMS_REAL_PACING=1`): task advances
   0→1→2→5. The wild jump does NOT reproduce in trace-sms.

## THE decisive next step

Find the wild-jump origin on Mednafen. It happens in the first
seconds of boot/init. Options, in order of expected payoff:

1. **Diagnostic ROM build**: increment an unused RAM byte ($CB40) at
   boot entry (reboot counter), and log the last dispatch
   (bank,addr) + last jump-indirect target into fixed RAM bytes.
   Run Mednafen 10s, savestate, read the counters. Confirms the
   reboot cycle and localizes which dispatch/jump goes wild.
2. **Early savestate bisection**: savestates at 2s/4s/8s; find the
   first snapshot with wild PC or wiped zp; diff runtime state
   against a healthy in-harness dump at the same phase
   (`SMS_DUMP_RAM=0xC000:0x2000`).
3. **Mednafen debugger trace** of the first ~2M instructions, diffed
   against a trace-sms PC log from reset.

Suspect classes (the harness models these differently from Mednafen):
VDP status/pending semantics at the exact moment translated code
reads $BF mid-init; V-counter values during init loops; the first
frame-INT arriving at a different instruction than the harness's
step-60000 injection, interrupting an init window that isn't
re-entrant (a DI-bracket gap); port $DC/$3F input levels (trace-sms
does not model $3F — Mednafen forces levels until $3F is written).

## How to reproduce / observe

- Build CV1: `cargo run --release -p nes_to_sms --bin nes-to-sms --
  "/mnt/terachad/Emulators/EmuDeck/roms/nes/Castlevania (USA) (Rev 1).nes"
  profiles/cv1.toml out/cv1 --runtime runtime`, then
  `docker compose run --rm --user root --workdir /work poc bash -lc
  'cd out/cv1 && make'`.
- Headless Mednafen + recording (in the poc container): Xvfb +
  `mednafen -sound 0 -force_module sms -qtrecord out.mov rom.sms`;
  savestate via `xdotool key --window $WINID F5`, lands in
  `$HOME/.mednafen/mcs/*.mc0` (gzip).
- In-harness healthy reference: `SMS_WATCH_ADDR=0xC018
  target/release/trace-sms out/cv1/sms.sms --steps 60000000` (task
  0→1→2→5), `SMS_PC_PROFILE=1` for hot loops,
  `SMS_DUMP_RAM=0xC000:0x2000` for state dumps.
- Savestate parsing: gzip → `MDFNSVST` header; sections
  `[32-byte name][u32 len]` (MAIN/Z80/CART/PSG/VDP/PIO); chunks
  `[u8 namelen][name][u32 len][data]`. Task = MAIN/RAM offset 0x18.

## Secondary TODOs

- rt_ror_a N-flag latent bug: verify whether the external stackless
  rewrite superseded it (`runtime/flags.s`).
- 4 benign anchor-phase divergent bytes ($000F/$0010/$004D/$00A9).
- CV1 acceptance route once it renders; strip-or-keep boot beacons.
- 13 lower failures in the CV1 build are the known benign data-walk
  class (JAM/AHX/ANE/oversize → trap stubs).

## After CV1: the mapper ladder (docs/mapper-plan.md)

CV1 → Gradius (CNROM) → Blaster Master (MMC1) → Bonk's (MMC3) →
Marble Madness (AxROM) → CV3 (MMC5). Hard rule: every commit passes
the full SMB gate + Alter Ego builds. No ROMs in the repo.
