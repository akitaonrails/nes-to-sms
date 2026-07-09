# Mapper support plan — from NROM to the major mappers

Goal: run the bulk of the NES library through the pipeline. Target
ladder, each rung a shippable milestone with its own stress ROM:

One mapper at a time; each milestone gets ONE test game from the
user's collection (/mnt/terachad/Emulators/EmuDeck/roms/nes), in
ascending difficulty:

| Milestone | Mapper | Test game (user's collection) | Their library |
|-----------|--------|-------------------------------|---------------|
| M1 | 2 (UxROM) | Castlevania (USA) (Rev 1) | 9 games |
| M2 | 3 (CNROM) | Gradius (USA) | 3 games |
| M3 | 1 (MMC1) | Blaster Master (USA) | 17 games |
| M4 | 4 (MMC3) | Bonk's Adventure / Adventure Island II | 22 games |
| M5 | 7 (AxROM) | Marble Madness (then Battletoads, the torture test) | 2 games |
| M6 | 5 (MMC5) | Castlevania III | 1 game, hardest |
| Later | 23/25 (VRC2/4), 69 (FME-7) | Kid Dracula, Gradius II, Batman RotJ | 3 games |

**Hard regression rule (user directive): every mapper-phase commit
must pass the FULL SMB gate (three routes byte-for-byte, 375 tests,
route expectations) and keep Alter Ego building. No exceptions, no
quick-gates on commit.**

NROM (SMB, Alter Ego) remains the regression floor: every milestone
must keep the three SMB routes byte-for-byte and Alter Ego building.

## The architectural insight

The SMS Sega mapper is itself a banked system, and the pipeline
already fights and wins the cross-bank battle for its own output
(far-gate transfers, per-section placement, fixed-point sizing).
NES mapper support is the same shape one level up:

- a NES PRG bank ↔ a set of SMS sections/banks,
- a NES bank switch ↔ an SMS `$FFFF`/`$FFFE` write through a shim,
- a cross-NES-bank call ↔ the existing `rt_far_gate` machinery.

## Cross-cutting changes (M0 — infrastructure)

**Loader** (`nes_rom`): already parses arbitrary PRG/CHR sizes and
mapper numbers. The *pipeline* assumptions move behind a
`RomLayout` abstraction: `fixed_bank()` (the CPU window that never
switches — UxROM/MMC1/MMC3 all fix the top), `switchable_windows()`,
`bank_count`, vectors read from the fixed bank.

**Label space**: NROM labels stay `L_XXXX`. Banked-region code gets
`L_bNN_XXXX` — a routine's identity is `(bank, cpu_addr)`. The IR,
profile, and reports carry the pair everywhere; the flat `u16`
address is only valid inside the fixed bank.

**Discovery** (bank-aware): walk the fixed bank from the vectors as
today. Into switchable windows, two mechanisms:
1. **Static bank-constant propagation**: the dominant idiom is
   `LDA #k / STA mapper_reg / ... / JSR $8xxx` — track the last
   constant written to the mapper register along the walk; calls
   into the window bind to that bank. Confidence-scoped: propagation
   resets at joins/calls unless both paths agree.
2. **Profile annotations**: `[[bank_entry]] bank = k, addr = 0x8xxx`
   for anything the propagation can't prove (indirect dispatch into
   banks). The trap-with-diagnostics path reports (bank, addr) pairs
   to add — the same fail-closed loop that worked for SMB.

**Reference oracle** (`frame-diff`'s NES bus + `trace`'s
expectations): implement each mapper's register semantics in the
reference bus at the same milestone. Without this there is no
parity gate, so it lands FIRST in every milestone.

**Runtime dispatch**: NES reads/writes in `$8000-$FFFF`:
- Writes = mapper register writes → per-mapper `rt_mapper_write`
  (today's NROM stub becomes a dispatch on the profile's mapper).
- Data reads from a switchable window → the indexed/const read
  dispatchers gain a banked path: current NES bank register (shadow
  byte) selects the SMS bank mapped into slot 2. The H.2-style
  compile-time specialization stays for fixed-bank constants.
- Code calls into a switchable window with a statically-known bank →
  direct far-gate to `L_bNN_XXXX`. Unknown bank → `rt_banked_call`:
  runtime lookup of (current bank, addr) in a generated table, then
  far-gate; misses trap with diagnostics.

**SMS bank budget**: 256KB NES PRG × ~3-4× translation expansion
exceeds the 512KB SMS mapper ceiling for the biggest games. Plan:
translation is per-NES-bank sections, so cold banks can stay
UNTRANSLATED until proven reachable (discovery already only lifts
reached code; expansion factor applies to reached bytes, not the
whole ROM). Measure per-game; the Sega mapper addresses up to 4MB
if needed (mapper supports 256 banks; header size field caps at
1MB for standard emulators — verify per target).

## M1 — UxROM (Castlevania 1)

Mapper 2: one register (any `$8000-$FFFF` write) selects the 16KB
bank at `$8000-$BFFF`; `$C000-$FFFF` is fixed to the last bank.
**CHR is RAM**, not ROM — the second new subsystem:

**CHR-RAM runtime conversion**: the game uploads tiles through
`$2007` into pattern space at runtime. The build-time CHR converter
doesn't apply. Runtime path: `$2007` writes with a pattern-table
address accumulate into a 16-byte NES-tile staging buffer
($CBxx scratch); on the 16th byte (or address discontinuity) the
tile converts 2bpp→4bpp and uploads (~200 cycles per tile — level
loads write hundreds of tiles, all during transitions, fine).
The BG palette-variant machinery (built for static CHR) runs in
dynamic mode: variants regenerate on CHR upload (pool flush on
pattern writes to a mapped tile).

Sequence inside M1:
1. Reference bus: mapper-2 semantics + CHR-RAM (frame-diff).
2. RomLayout + vectors-from-fixed-bank (unblocks the loader error).
3. Fixed-bank-only discovery + lift + run: CV1 reset/init lives in
   the fixed bank; get the title screen up with banked calls
   trapped-and-reported.
4. Bank-constant propagation + `[[bank_entry]]` profile entries from
   the trap reports; iterate until the title route runs.
5. CHR-RAM conversion path; verify title visuals.
6. Level-1 route recorded against the reference; parity gate.

### M1 status (2026-07-08)

Architecture LANDED, SMB regression byte-for-byte through all of it:
per-bank 32 KiB translation units (interior aliases, cross-view and
global label dedup), the runtime (bank, addr) dispatch table with
rt_banked_dispatch (fail-closed: misses trap with the live bank in
$CB1A), lazy-trap window stubs, the UxROM slot-2 remap shim, and
ground-truth bank-entry harvesting from the reference oracle
(FD_LOG_BANK_ENTRIES). Root causes fixed along the way: indexed ROM
stores remapped to zero page (bank switching dead + zp corruption);
late .define compiled the mapper shim out entirely; rt_mapper_write
clobbered A (broke the double-STA bus-conflict idiom).

CV1 now: boots, uploads CHR, switches banks, dispatches its master
task table. BLOCKED inside the FIRST real NMI: _irq_call_translated_nmi
fires once and never returns. Forensics (SMS_LOG_ZPY + control-transfer
ring + per-callee SMS_WATCH_PC): the NMI line runs L_C8CD once, then
the task dispatcher L_C1E4 is entered THREE times (re-entry without
completing the NMI), task 0 (L_b6_B7DC) entered once; L_CCEE (late in
the NMI line) never reached. The endless cycle spans object-physics
routines (L_ECEA/EE35/EF43, ASL-heavy fixed-point) + per-lap bank
switches + PPU reg writes — the shape of the logo animation task
pumping forever. (zp),Y source reads verified sane ($889E bank 0,
$FDD3 fixed). Init RAM snapshot matches the reference byte-for-byte;
frame-0 write streams identical (58/58). NEXT PROBE: instrument Z80 SP
at the three dispatcher entries — legit nested dispatch vs return-path
corruption (far-gate/JumpEngine ret interplay). Then CHR-RAM visuals
and the level-1 parity route.

## M2 — MMC1

Serial 5-write register protocol (shim buffers the shift register),
PRG mode variants (16KB switch low/high, 32KB), CHR 4KB banking,
mirroring control (the materializer already parametrizes vertical/
horizontal; MMC1 switches it at runtime → the fold/projector
mirroring define becomes a runtime flag).

## M3 — MMC3 (SMB3, Kirby)

8KB PRG banking (two switchable + two fixed windows — the label
space and dispatch generalize from 16KB to window-granularity),
2KB/1KB CHR banking (CHR-ROM again, but banked: the build-time
converter emits per-bank tile sets; the runtime CHR window state
selects which SMS tile base the BG/sprite mappers use — this is the
big one for the CHR pipeline), and the **scanline IRQ**: map the
MMC3 counter onto the SMS VDP line interrupt (the split machinery
already drives it; MMC3 games configure a line and flip scroll/banks
there — same shape as the SMB sprite-0 split, now data-driven).

## M4 — MMC5 (Castlevania III)

Everything above plus ExRAM modes, fill mode, vertical split,
8×16-attribute mode, multiplier, PCM. Scoped only after M3 ships;
several MMC5 features (extended attributes per tile) map poorly to
Mode 4 and may need per-game compromises. Honest flag: this rung
may land as "CV3-specific subset of MMC5".

## Verification protocol (unchanged in spirit)

Per milestone: reference-bus mapper first; then trap-driven
iteration to boot; then a recorded route with full-frame RAM parity;
SMB three-route + Alter Ego regression on every commit.
