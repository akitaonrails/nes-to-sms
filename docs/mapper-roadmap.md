# Compatibility-first mapper roadmap

Inventory and source review: 2026-09-08. This is the user-ordered queue, not a
compatibility list. None of the new targets below has been converted or tested
by this investigation. Approved order: Adventure Island → Zelda → Battletoads
→ Contra → Ganbare Goemon 2 → Gradius II → Parodius Da! → Batman: Return of
the Joker → Punch-Out!! → Fire Emblem → Castlevania III (USA). The added games
are grouped by VRC family; the earlier targets retain their relative order.
VRC6 is explicitly deferred. Additional speed optimization is deferred too.

## Active policy and preserved baseline

- [x] Preserve the accepted SMB3 checkpoint (`68b439a`) and its measured limits.
  More SMB3 speed work is **not a prerequisite** for Adventure Island.
- [x] Start Adventure Island as the only active new game. Preserve SMB3's
  inventory, level-clear, death/return, controller and presentation checks,
  and the SMB1/Castlevania I regression floor throughout the queue.
  See [SMB3 plan](smb3-plan.md) and [completion plan](completion-plan.md).

Compatibility takes priority over optimization: complete one game's conversion
and acceptance gates before starting the next. There is no full-speed gate in
this batch. Existing overclock-assisted validation is acceptable when recorded;
hangs, incorrect interrupts, display corruption and broken controls are not
excused as slowness. Keep speed measurements as regression evidence, not a new
optimization campaign. Preserve prior tested ROMs before every handoff.

The goal is reusable, complete mapper behavior, not another game-specific
adapter. Game completion and mapper completion are separate checkboxes: no
family is called fully supported while its declared hardware matrix has holes.
Other games may still need profiles/code discovery; a correct mapper alone
does not guarantee automatic translation of every game.

**Completion means the whole game, not one level or one representative route.**
For every queued title, cover all levels, alternate branches, bosses, enemies,
sprites/animations, backgrounds, menus, endings, music and sound effects, plus
applicable multiplayer, saves and other game modes. Maintain a content checklist
with evidence for each entry. A single ending does not prove unvisited branches
or missing audio. Full documented mapper support is required before advancing;
features unused by the current title still need implementation and tests.

The current [mapper policy](../crates/nes_rom/src/lib.rs) implements NROM (0)
and bounded NES 2.0 UxROM (2, submappers 1/2). The
[pipeline](../crates/cli/src/pipeline.rs) separately admits experimental MMC3 (4)
through explicit profile capabilities. That is not universal MMC3 compatibility.
CNROM has a tested [board-model foundation](../crates/nes_rom/src/cnrom.rs)
and opt-in [synthetic source-bus](cnrom-bus-runtime.md) and
[source-clock](source-clock-runtime.md) foundations, but
**rendered-game admission and full mapper support remain unfinished**. The
[Adventure Island reference](adventure-island-reference.md) pins the target,
records native execution evidence and tracks full-content acceptance. The existing
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

- [ ] **3. AxROM — Battletoads (USA).** Available: `7.2`, **256/0**,
  NES 2.0, NTSC, 8 KiB CHR-RAM. Introduces whole-window 32 KiB PRG switching
  and selectable one-screen nametables, with bus conflicts in this variant.
  Audit remapped execution, return continuations and vectors: no high PRG window
  stays fixed. Route: first-stage combat → death/continue → stage transition;
  extend coverage to later scrolling and split effects before broad claims.
  Marble Madness (USA), `7.1`, **128/0**, is an optional no-conflict companion,
  not a prerequisite game conversion.
  [AxROM hardware](https://www.nesdev.org/wiki/INES_Mapper_007).

- [ ] **4. VRC2 — Contra (Japan), after board verification.** Use only the
  `originals/Contra (Japan).nes` candidate: iNES mapper 23, **128/128**,
  submapper unknown. Confirm the board before admission; VRC2b's single-bit
  memory and lack of IRQ are not VRC4 behavior. Do not silently reinterpret
  ambiguous mapper 23 as VRC4 or use the collection's mapper-4 namesake.
  Route: start → scrolling combat → death/continue → stage transition.
  Follow with Goemon 2 through the same mapper implementation. VRC4 is a
  separate family milestone below, not something Contra's success establishes.
  [VRC2/VRC4](https://www.nesdev.org/wiki/VRC2_and_VRC4),
  [wiring/submapper distinctions](https://www.nesdev.org/wiki/NES_2.0_submappers).

- [ ] **5. VRC2 — Ganbare Goemon 2 (Japan).** This is the action-game sequel,
  **not Ganbare Goemon Gaiden** or Goemon Gaiden 2. The inspected collection
  does not contain it; a verified matching dump is required. Its VRC2b board
  provides a second real-game check of the implementation used by Contra,
  including the single-bit readback behavior. Route: opening area → scrolling
  and room changes → death/restart → later areas and completion. No Goemon-only
  mapper branch or compatibility patch may substitute for the shared model.
  [Board and readback reference](https://www.nesdev.org/wiki/VRC2_and_VRC4).

- [ ] **6. VRC4 — Gradius II (Japan).** Available translated candidate:
  `25.1` (**VRC4b**), **128/128**, NES 2.0, NTSC. Verify its patch/base and
  declared RAM before admission. Implement VRC4 IRQs, banking/mirroring modes
  and this wiring variant generically; do not treat it as VRC2 with extra
  game hooks. Route: gameplay start → scrolling and weapons → death/continue
  → boss/stage transitions, then complete the game. This is Gradius II,
  not the CNROM Gradius mentioned under Adventure Island.
  [VRC4 reference](https://www.nesdev.org/wiki/VRC2_and_VRC4).

- [ ] **7. VRC4 — Parodius Da! (Japan).** Use the Japanese VRC4e/mapper-23
  cartridge version. The local **Parodius (Europe)** file declares MMC3/mapper 4
  and cannot validate VRC4. A verified Japanese dump is therefore required.
  Reuse Gradius II's VRC4 model with the VRC4e address wiring; prove IRQ and
  CHR/PRG behavior without game-specific exceptions. Route: character selection
  → scrolling/weapons → death/continue → bosses/stages and completion.
  [Board reference](https://www.nesdev.org/wiki/Talk:VRC4).

- [ ] **8. FME-7 — Batman: Return of the Joker (USA).** Available: `69.0`,
  **128/256**, NES 2.0, NTSC, 8 KiB PRG-RAM. Although a smaller family, this
  available recognizable game adds a genuinely different interrupt model:
  CPU-cycle-counted 16-bit IRQs, plus bankable ROM/RAM at `$6000` and 1 KiB
  CHR windows. Prove counter wrap/acknowledgment and memory selection before
  first-stage scrolling, weapons, death and boss/transition checks. Do not
  reuse SMB3's game-scoped raster adapter as a general IRQ implementation.
  This game does **not** establish Sunsoft 5B expansion-audio support.
  [FME-7 hardware](https://www.nesdev.org/wiki/Sunsoft_FME-7).

- [ ] **9. MMC2 (9) — Mike Tyson's Punch-Out!! / Punch-Out!!.** No mapper-9
  file exists locally; dump/revision/geometry are unverified and acquisition is
  a prerequisite. The recognizable target would justify PPU-fetch-driven CHR
  latches, which cannot be modeled only by observing CPU register writes.
  Bring-up route: first opponent's changing poses → knockdown → next opponent,
  with independent latch-trigger fetch tests.
  [MMC2 game association](https://www.nesdev.org/wiki/Cartridge_and_mappers%27_history),
  [MMC2/MMC4 differences](https://www.nesdev.org/wiki/MMC4).

- [ ] **10. MMC4 (10) — Fire Emblem: Dark Dragon and the Sword of Light.**
  Selected local candidate is the Quirino v1.0 English translation:
  `10.0`, **256/128**, NES 2.0, NTSC, battery flag set. Patch/base provenance
  is a blocker; a verified original is preferable for the first reference.
  Reuse the latch-family work, but test MMC4's different low-pattern trigger
  range, 16 KiB PRG window and save memory separately. Route: opening map →
  movement → battle → save/reload. Defer rather than claiming broad coverage
  from a small translated-game subset. [MMC4 hardware](https://www.nesdev.org/wiki/MMC4).

- [ ] **11. MMC5 — Castlevania III: Dracula's Curse (USA).** Available in
  root and `originals/`: `5.0`, **256/128**, NES 2.0, NTSC, battery flag clear.
  Put this after the selected latch families: PRG/CHR modes, separate rendering
  contexts, ExRAM/nametables and IRQ behavior cross several existing boundaries.
  Route: first level → boss → branch selection; add a later rising-water
  reference because the US game uses ExRAM as a third nametable there.
  Maintain a per-feature support matrix: CV3 does not exercise every MMC5
  capability, and passing it must not imply extended-attribute, split-mode or
  expansion-audio completeness. [MMC5 hardware](https://www.nesdev.org/wiki/MMC5),
  [rising-water distinction](https://www.nesdev.org/wiki/Game_bugs).

Goemon 2, Japanese Parodius Da! and Punch-Out!! need verified local inputs.
Gradius II and Fire Emblem need patch/base provenance checks. These are
prerequisites, not permission to silently reorder the approved queue. Resolve
them before their implementation turns; if still blocked, report the exact
missing input and ask for it rather than substituting another game.

## Additional VRC2/VRC4 choices

Except for the explicitly queued Goemon 2, Gradius II and Parodius Da!, these
are optional companions, not changes to the approved primary order.
Use the Japanese Famicom cartridge versions indicated by the hardware sources;
another region, rerelease or patch can use a different mapper or memory layout.

| Family/variant | Mapper | Other representative games |
| --- | ---: | --- |
| VRC2a | 22 | TwinBee 3; Ganbare Pennant Race |
| VRC2b | 23 | Konami Wai Wai World; Ganbare Goemon 2; Getsu Fūma Den; Dragon Scroll |
| VRC2c | 25 | Ganbare Goemon Gaiden: Kieta Ōgon Kiseru |
| VRC4a | 21 | Wai Wai World 2 |
| VRC4b | 25 | Gradius II; Bio Miracle Bokutte Upa |
| VRC4c | 21 | Ganbare Goemon Gaiden 2 |
| VRC4e | 23 | Crisis Force; Akumajou Special: Boku Dracula-kun (Kid Dracula); Parodius Da!; Tiny Toon Adventures |

Board associations: [NESdev family reference](https://www.nesdev.org/wiki/VRC2_and_VRC4)
and [board inventory](https://www.nesdev.org/wiki/Talk:VRC4).
VRC4 adds IRQs and banking/mirroring features that VRC2 does not have;
passing Contra does not establish VRC4 support. Mapper numbers 23 and 25
alone do not distinguish the families. Use verified board facts and
[NES 2.0 submappers](https://www.nesdev.org/wiki/NES_2.0_submappers).

Fresh read-only header recheck still finds 96 files. Besides the selected
Contra candidate, available alternatives are:

- **Crisis Force (Japan):** `23.2`, VRC4e, 128/128 KiB PRG/CHR.
- **Gradius II (Japan) (En):** `25.1`, VRC4b, 128/128; translation provenance
  needs verification before using it as the reference.
- **Kid Dracula (Castlevania Anniversary Collection):** `23.2`, VRC4e,
  128/128; header declares battery-backed RAM. Verify the rerelease's behavior
  and geometry rather than assuming it is the original Japanese board.
- **Ganbare Goemon Gaiden, English translation:** `25.3`, VRC2c, 256/256,
  battery flag set. Verify patch/base provenance and expanded geometry.

Contra plus Goemon 2 checks reuse across games on VRC2b, not other VRC2 wiring.
The queued Gradius II plus Parodius checks VRC4b/VRC4e and IRQs. Synthetic
hardware tests must cover remaining family variants; optional TwinBee 3 would
add a real VRC2a cross-check, and Crisis Force another real VRC4e cross-check.
Kid Dracula is another available VRC4e alternative, not another mapper family.
TwinBee 3 was not found locally. Do not insert unrequested companion conversions
ahead of the user's eleven selected games.

## Conditional follow-ups, not blockers for the main queue

These are deliberate hardware-value exceptions to the broad-coverage priority.
Do not start them merely to fill missing mapper numbers.

**VRC6 (24/26) — skipped for this effort; do not start without renewed request.**
  Akumajou Densetsu (Japan)
  uses mapper 24; **Madara** and **Esper Dream 2** use mapper 26, with the
  register address-line wiring swapped. Akumajou is therefore not the only
  VRC6 game. No mapper-24/26 file was found in the inspected local collection;
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

## One-game-at-a-time execution protocol

For each game, the implementing agent owns the following ordered work and its
evidence. Do not open another game's implementation lane before closing this one.

1. **Pin and scope.** Record dump/payload hash, board/submapper, ROM/RAM geometry,
   reset behavior and a hardware-feature matrix. Mark each row unimplemented,
   implemented, synthetic-tested and real-game-tested separately. An ambiguous
   header needs investigation, not a guessed chip assignment.
2. **Implement the shared mapper.** Add board semantics, bus effects and event
   timing through the generic pipeline and runtime. Game addresses/reachability
   belong in profiles. No per-game mapper shortcuts, permissive stubs or rendered
   fixtures count as conversion. Existing SMB3 raster assumptions must not become
   a supposedly generic IRQ implementation for these families.
3. **Prove hardware behavior.** Differentially test original 6502 programs and
   assembled Z80 against independent expected board behavior. Cover banking,
   aliases, boundary/reset/RMW cases, RAM protection/saves and interrupts or PPU
   latches as applicable. Close matrix rows that the selected game never uses
   with synthetic/hardware-reference tests rather than leaving them untested.
4. **Complete the game conversion.** Close the full content checklist above,
   including all levels and branches, sprites, music and sound effects, not just
   representative scene types. Boot, controls, transitions, death/retry and
   applicable saves must work. The short routes in the queue are bring-up
   milestones only. Require complete playthrough evidence and additional routes
   for content or modes a single playthrough misses. Compare audio events and
   audible output throughout, including sample playback and expansion channels
   where used. Prefer reproducible input routes from reset; any continuation used
   for later-state tests must have verified provenance, not conceal broken loading.
5. **Check reuse and regressions.** Re-run previously accepted games after shared
   changes. Test the second selected game of a family without game-specific mapper
   edits; any discovered defect gets a generic fix and a regression case for both.
   Optional companion probes may supply coverage without becoming full conversions
   inserted into the queue. Retain SMB1/CV1/SMB3 behavior and presentation gates.
6. **Review and hand off.** Run workspace tests, relevant assembled/differential
   suites, formatting and lint checks. Record exact ROM/core/input identities,
   observed behavior and any remaining limitations. Missing content, broken audio
   or rendering defects must not be silently waived as cosmetic; apply and record
   the hardware-adaptation policy below where necessary. Have correctness and changed
   ownership boundaries reviewed, create a focused validated commit, preserve
   the old artifact, then update the game's launch ROM. Only then advance.

Use the minimum evidence that establishes each claim, not repeated broad runs
without a changed boundary. The implementer records results; a separate reviewer
checks completion claims and material mapping/timing/ownership changes. An
unsupported hardware feature, known gameplay blocker or unfinished validation
is not a completed milestone. Stop for missing external inputs. When physical
SMS constraints arise, the user authorizes choosing the best workable adaptation
and proceeding: document the hardware evidence, chosen behavior and fidelity
tradeoff. This authority does not turn omitted game content or unimplemented
mapper semantics into complete support.

## Mapper completeness gates

These are scope checklists, not statements of existing support. At each family
start, expand them into documented board/variant-specific positive and negative
tests. Unimplemented documented variants stay visibly open; ambiguous variants
fail closed. Never mark a mapper family complete solely because one title boots.

| Family | Required coverage beyond the initial game route |
| --- | --- |
| CNROM | CHR bank selection and physical identity; documented conflict/no-conflict variants, resets, aliases and bank masking |
| MMC1 | Serial/reset and consecutive-write semantics; all PRG/CHR modes, mirroring, CHR-ROM/RAM, RAM enable/protection, saves and board-specific upper banking |
| AxROM | Whole-window remapping, vectors/returns across switches, one-screen mirroring, CHR-RAM and conflict/no-conflict variants |
| VRC2 | Documented a/b/c wiring, PRG/CHR banking, mirroring, single-bit interface and RAM differences; prove absence of VRC4-only effects |
| VRC4 | Documented wiring variants, PRG modes, fine CHR banks, mirroring/RAM and IRQ count/prescale/acknowledgment behavior |
| FME-7 | PRG/CHR and ROM/RAM selection, mirroring, counter/enable/acknowledgment and wrap; include 5B variant audio support and tests, not just Batman's non-audio board |
| MMC2/MMC4 | Each family's exact PPU-fetch latch triggers/order, CPU/CHR bank modes, mirroring and applicable RAM/saves; do not merge their differing triggers |
| MMC5 | PRG/CHR modes and rendering contexts, RAM/protection, ExRAM/nametables, extended attributes, split behavior, IRQs, multiplier and audio/register semantics |

Mapper-provided sound needs explicit SMS output behavior and tests; preserving
register writes while silently dropping a mapper feature does not close that
row. All game music and sound effects remain required, including NES APU sample
playback where used. Exact NES timbre is not implied on SMS. The user authorizes
best-recommendation hardware adaptations without waiting for approval; document
output mappings and fidelity limits. Omitted content or unimplemented features
remain explicit gaps, not silently completed rows.

## Evidence rules for each family

1. Record verified dump/board facts in a profile; keep commercial bytes out of
   Git. Reject ambiguous variants and unsupported features explicitly.
2. Exercise the board model and real generated runtime together: bank-qualified
   calls/data, remapping, returns, RAM protection/mirroring, interrupts and
   relevant PPU fetches. Compare synthetic programs with the existing 6502
   oracle; use hardware-tested cases from the
   [NESdev test catalog](https://www.nesdev.org/wiki/Emulator_tests) where applicable.
3. Run the milestone and completion routes on original NES and translated SMS cores with normal
   input. Compare guest RAM/save state and source-aligned graphics; extend
   routes for features not reached. A title screen is not completion.
4. Record before/after regression measurements with the same inputs and completed guest updates;
   report core, region, overclock, stalls and blank intervals. No speed is
   predicted by this roadmap. SMS RAM/ROM capacity and expansion-audio costs
   remain feasibility questions, not guaranteed fits.
5. Preserve SMB1, CV1 and the accepted SMB3 routes. Document exact supported
   board/features and remaining gaps rather than labeling a whole mapper done
   because one game boots.
