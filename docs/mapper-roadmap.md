# Post-SMB3 mapper roadmap

Inventory and source review: 2026-09-08. This is a proposed queue, not a
compatibility list. None of the new targets below has been converted or tested
by this investigation. Priorities favor reusable hardware support and recognizable
games over collecting mapper numbers.

## Prerequisite and existing support

- [ ] Close the agreed SMB3 performance and blink-free playability gate before
  starting another mapper. Preserve its inventory, level-clear, death/return,
  controller and presentation checks, and the SMB1/Castlevania I regression floor.
  See [SMB3 plan](smb3-plan.md) and [completion plan](completion-plan.md).

The current [mapper policy](../crates/nes_rom/src/lib.rs) implements NROM (0)
and bounded NES 2.0 UxROM (2, submappers 1/2). The
[pipeline](../crates/cli/src/pipeline.rs) separately admits experimental MMC3 (4)
through explicit profile capabilities. That is not universal MMC3 compatibility.
CNROM is **not implemented**. The existing
[CV3 profile](../profiles/cv3.toml) is a mapper-5 rejection probe, not MMC5 support.

## What is actually available

Read-only inspection of the NES directory in the configured EmuDeck collection
found **96 files across 12 mapper IDs**: 73 NES 2.0 and 23 iNES headers. Only the
16-byte headers and file sizes were inspected; no payloads were copied or hashed.
All files have the trainer flag clear and enough bytes for their declared payload.
The reproducible header scanner, inventory and verification notes are preserved
locally under ignored `out/mapper-inventory.3SB48s/`; they are not ROM payloads
or a committed catalog of private collection paths.

| Mapper | Files | Mapper | Files |
| --- | ---: | --- | ---: |
| 0 | 8 | 1 | 23 |
| 2 | 10 | 3 | 3 |
| 4 | 37 | 5 | 2 |
| 7 | 2 | 10 | 4 |
| 23 | 3 | 25 | 2 |
| 69 | 1 | 210 | 1 |

These are file counts, not unique games or market coverage. Eleven basenames
occur twice. Headers report 90 NTSC, two PAL and four multi-region files; old
iNES timing flags and filename regions need independent confirmation. One old
iNES header, `no_match/Adventure Island 4.nes`, has nonzero reserved bytes.

Do not select by filename alone: root-level `Contra (Japan).nes` identifies
mapper 4, while `originals/Contra (Japan).nes` identifies mapper 23. The two
`Mega Man (USA).nes` copies identify mappers 4 and 2; the Metroid copies also
have different ROM geometry and battery flags. Matching names or headers do
not establish matching payloads, authenticity, patch provenance or ownership.

Before implementation, pin a lawfully obtained dump, payload hash, board variant
and RAM geometry. All sizes below are **PRG-ROM / CHR-ROM in KiB**; zero CHR-ROM
does not mean zero graphics memory. `m.s` means mapper/submapper.

## Main queue, in order

- [ ] **1. CNROM — Adventure Island (USA).** Available: `3.2`, **32/32**,
  NES 2.0, NTSC. A small first extension: fixed PRG and switchable 8 KiB CHR
  make graphics-bank identity testable without another CPU banking scheme.
  Submapper 2 explicitly requires AND bus conflicts. Prove writes against the
  currently visible ROM byte, then title → first area → death/restart → area
  transition. Gradius (USA), also `3.2`, **32/32**, is a useful second game.
  [CNROM hardware](https://www.nesdev.org/wiki/CNROM),
  [submapper definitions](https://www.nesdev.org/wiki/NES_2.0_submappers).

- [ ] **2. MMC1 — The Legend of Zelda (USA, Rev 1).** Available as
  `Legend of Zelda, The (USA) (Rev 1).nes`: `1.0`, **128/0**, NES 2.0,
  NTSC; header declares 8 KiB CHR-RAM and 8 KiB battery-backed PRG-RAM.
  This is the highest-priority larger family: serial register writes, reset,
  PRG modes, mirroring and persistent saves. Consecutive-cycle write filtering
  must follow original 6502 bus semantics, including RMW and reset exceptions,
  not elapsed Z80 time. Route: obtain sword → cross screens → dungeon entry →
  save/reload. Add Ninja Gaiden (USA), `1.0`, **128/128**, as the CHR-ROM
  companion; Zelda alone cannot establish split CHR-ROM banking coverage.
  Local Tetris is `1.5`, an unbanked-PRG variant, so it is a poor primary probe.
  [MMC1 hardware and variants](https://www.nesdev.org/wiki/INES_Mapper_001).

- [ ] **3. AxROM — Marble Madness (USA).** Available: `7.1`, **128/0**,
  NES 2.0, NTSC, 8 KiB CHR-RAM. Introduces whole-window 32 KiB PRG switching
  and selectable one-screen nametables, without bus conflicts in this variant.
  Audit remapped execution, return continuations and vectors: no high PRG window
  stays fixed. Route: first course → next course → timeout/restart. Follow with
  Battletoads (USA), `7.2`, **256/0**, to cover the conflict variant and a
  different action workload. [AxROM hardware](https://www.nesdev.org/wiki/INES_Mapper_007).

- [ ] **4. VRC2/VRC4 family — Crisis Force (Japan), then verified VRC2 Contra.**
  Crisis Force is available as `23.2` (**VRC4e**), **128/128**, NES 2.0,
  NTSC; its header declares 2 KiB PRG-RAM, so RAM-size mirroring needs explicit
  tests. It provides a concrete register-wiring variant, fine CHR banks and
  IRQs. Route: start → scrolling combat → death/continue → stage transition.
  Gradius II (Japan) (En), `25.1` (**VRC4b**), **128/128**, is a later wiring
  cross-check, subject to payload provenance. For VRC2, use only the
  `originals/Contra (Japan).nes` candidate: iNES mapper 23, **128/128**,
  submapper unknown. Confirm the board before admission; VRC2b's single-bit
  memory and lack of IRQ are not VRC4 behavior. Do not silently reinterpret
  ambiguous mapper 23 as VRC4. [VRC2/VRC4](https://www.nesdev.org/wiki/VRC2_and_VRC4),
  [wiring/submapper distinctions](https://www.nesdev.org/wiki/NES_2.0_submappers).

- [ ] **5. FME-7 — Batman: Return of the Joker (USA).** Available: `69.0`,
  **128/256**, NES 2.0, NTSC, 8 KiB PRG-RAM. Although a smaller family, this
  available recognizable game adds a genuinely different interrupt model:
  CPU-cycle-counted 16-bit IRQs, plus bankable ROM/RAM at `$6000` and 1 KiB
  CHR windows. Prove counter wrap/acknowledgment and memory selection before
  first-stage scrolling, weapons, death and boss/transition checks. Do not
  reuse SMB3's game-scoped raster adapter as a general IRQ implementation.
  This game does **not** establish Sunsoft 5B expansion-audio support.
  [FME-7 hardware](https://www.nesdev.org/wiki/Sunsoft_FME-7).

- [ ] **6. MMC5 — Castlevania III: Dracula's Curse (USA).** Available in
  root and `originals/`: `5.0`, **256/128**, NES 2.0, NTSC, battery flag clear.
  Put this after simpler families: PRG/CHR modes, separate rendering contexts,
  ExRAM/nametables and IRQ behavior cross several existing boundaries.
  Route: first level → boss → branch selection; add a later rising-water
  reference because the US game uses ExRAM as a third nametable there.
  Maintain a per-feature support matrix: CV3 does not exercise every MMC5
  capability, and passing it must not imply extended-attribute, split-mode or
  expansion-audio completeness. [MMC5 hardware](https://www.nesdev.org/wiki/MMC5),
  [rising-water distinction](https://www.nesdev.org/wiki/Game_bugs).

## Conditional follow-ups, not blockers for the main queue

These are deliberate hardware-value exceptions to the broad-coverage priority.
Do not start them merely to fill missing mapper numbers.

- [ ] **7. MMC2 (9) — Mike Tyson's Punch-Out!! / Punch-Out!!.** No mapper-9
  file exists locally; dump/revision/geometry are unverified and acquisition is
  a prerequisite. The recognizable target would justify PPU-fetch-driven CHR
  latches, which cannot be modeled only by observing CPU register writes.
  Acceptance: first opponent's changing poses → knockdown → next opponent,
  with independent latch-trigger fetch tests.
  [MMC2 game association](https://www.nesdev.org/wiki/Cartridge_and_mappers%27_history),
  [MMC2/MMC4 differences](https://www.nesdev.org/wiki/MMC4).

- [ ] **8. MMC4 (10) — Fire Emblem: Dark Dragon and the Sword of Light.**
  Available only as the Quirino v1.0 English translation in `no_match/`:
  `10.0`, **256/128**, NES 2.0, NTSC, battery flag set. Patch/base provenance
  is a blocker; a verified original is preferable for the first reference.
  Reuse the latch-family work, but test MMC4's different low-pattern trigger
  range, 16 KiB PRG window and save memory separately. Route: opening map →
  movement → battle → save/reload. Defer rather than claiming broad coverage
  from a small translated-game subset. [MMC4 hardware](https://www.nesdev.org/wiki/MMC4).

- [ ] **9. VRC6 (24/26) — Akumajou Densetsu (Japan).** Not available locally;
  no geometry or submapper has been verified. This is a later audio-focused
  extension, not a substitute for US mapper-5 CV3. Its two pulse channels and
  sawtooth require an explicit SMS audio approximation and measured budget,
  alongside IRQ and register-wiring tests. Route: intro music → first-level
  action → death/transition, checking audio progression as well as game state.
  [VRC6 hardware](https://www.nesdev.org/wiki/VRC6),
  [audio and mapper-24/26 distinction](https://www.nesdev.org/wiki/VRC6_audio).

Color Dreams (11) and Namco 163 (19) have no local examples and are not scheduled.
The translated Splatterhouse file is **210.2, Namco 340**, not Namco 163; it
cannot validate N163 IRQ or expansion audio. Defer this and other small,
special-purpose families until a concrete game justifies the work.
[Namco-family distinction](https://www.nesdev.org/wiki/INES_Mapper_210).

## Definition of done for each family

1. Record verified dump/board facts in a profile; keep commercial bytes out of
   Git. Reject ambiguous variants and unsupported features explicitly.
2. Exercise the board model and real generated runtime together: bank-qualified
   calls/data, remapping, returns, RAM protection/mirroring, interrupts and
   relevant PPU fetches. Compare synthetic programs with the existing 6502
   oracle; use hardware-tested cases from the
   [NESdev test catalog](https://www.nesdev.org/wiki/Emulator_tests) where applicable.
3. Run the proposed route on original NES and translated SMS cores with normal
   input. Compare guest RAM/save state and source-aligned graphics; extend
   routes for features not reached. A title screen is not completion.
4. Measure before/after with the same inputs and completed guest updates;
   report core, region, overclock, stalls and blank intervals. No speed is
   predicted by this roadmap. SMS RAM/ROM capacity and expansion-audio costs
   remain feasibility questions, not guaranteed fits.
5. Preserve SMB1, CV1 and the accepted SMB3 routes. Document exact supported
   board/features and remaining gaps rather than labeling a whole mapper done
   because one game boots.
