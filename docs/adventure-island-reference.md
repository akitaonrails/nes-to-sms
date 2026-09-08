# Adventure Island / CNROM reference and acceptance inventory

Verified 2026-09-08. Adventure Island is the active compatibility target;
this document records original-NES evidence, **not a completed SMS conversion**.
The [mapper roadmap](mapper-roadmap.md) defines the full-game completion gate.
All unchecked entries below remain required unless an explicitly documented
hardware assessment chooses an alternative under the user's authorization.

## Implementation checkpoint: board model only

The generic `nes_rom::cnrom` model covers fixed PRG, 8–128 KiB CHR selection,
conflict variants and optional mirrored 2 KiB RAM. Mapper 3 still fails closed
in the conversion pipeline. A related header fix preserves all twelve NES 2.0
mapper bits so extended IDs cannot alias supported boards.

Independent semantic and structure reviews passed. Workspace tests: 598 passed,
174 ignored; formatting and Clippy succeeded with existing warnings. Fresh
SMB1/CV1/SMB3 projects assembled before/after this foundation are byte-identical.
The canonical SMB clear, death/game-over and bonus-pipe routes each match the
6502 reference for 4,500 frames. Canonical ROMs were not replaced. Evidence is
locally preserved under ignored `out/adventure-baseline.TvOphd/`.

Next: mapper-neutral CPU bus and assembled CHR/RAM tests, then coherent graphics
and full-game/audio validation. This checkpoint does not close any rendered-game
or full-mapper acceptance checkbox below.

## Pinned input

Selected local basename: `Adventure Island (USA).nes`. It is a 65,552-byte
NES 2.0 image, mapper **3.2**, NTSC, 32 KiB PRG-ROM plus 32 KiB CHR-ROM,
vertical mirroring, no trainer, four-screen flag, battery, PRG-RAM or CHR-RAM.
Vectors are reset `$8000`, NMI `$B706`, IRQ `$B754`.

| Portion | SHA-256 |
| --- | --- |
| Complete file | `f4083971fb7341ba6560fdd8ef588bd6e2ad450a8694a563cec099a5d306d42e` |
| PRG + CHR, excluding header | `53bfc94fce46a25188f84f102810406f686a7fb13fb5e4ae8f13760106acb969` |
| PRG-ROM | `d3dad6dfe9fd47563cb02a7984a916b67bdde4b05c68201f3648475e3deda78b` |
| CHR-ROM | `fb30f3f809cacbcc6c3a0b434eacd3b22c229ff3fa7beb9648abb7e0937c00b6` |

Payload/PRG/CHR CRC32 values are `F8A713BE`, `34A8BD90`, `F01F13A3`.
These match bootgod's physical USA cartridge records, which also confirm
vertical mirroring and the CNROM board. The payload SHA-1 is
`e8ea2deb2d65fc64f0dc038a2c95ca6ce8ca31b9`; all component SHA-1 values match
the corresponding catalog entry. This establishes payload identity against
those records, not ownership or the history of the local header.
[Physical cartridge record](https://nescartdb.com/profile/view/854/adventure-island),
[component SHA-1 catalog](https://nesdir.github.io/F8A713BE_USA.html).

The secondary catalog labels its layout "Horizontal"; do not override the
verified header from that wording. Nintendo's horizontal **arrangement** is
iNES vertical **mirroring**; the primary cartridge record explicitly says
vertical mirroring. Always test actual nametable aliases.
[PCB terminology](https://nescartdb.com/guides/view/pcb).

Read-only identification script and per-8-KiB CHR-bank hashes are preserved
under ignored `out/adventure-reference.kECfiP/identify.py`. Reproduce with
`python3 out/adventure-reference.kECfiP/identify.py /path/to/adventure-island.nes`.
No ROM bytes or extracted assets belong in commits.

## Mapper scope: standard support is not every CNROM-related board

The hardware matrix must cover fixed 16/32 KiB PRG, an 8 KiB CHR window,
fixed nametable wiring, all write-address aliases, and conflict handling.
Oversized mapper 3 reaches 128 KiB CHR; Hayauchi Super Igo adds mirrored
2 KiB PRG-RAM. Both are required mapper-3 extensions, not new CPU bank modes.
Adventure Island alone exercises neither. [CNROM hardware](https://www.nesdev.org/wiki/CNROM).

NES 2.0 submapper 1 excludes conflicts; submapper 2 selects bitwise AND with
the currently visible PRG byte. Submapper 0 leaves behavior ambiguous;
require an explicit verified policy rather than silently assuming no conflicts.
Test both writes of an original 6502 read-modify-write instruction.
[Submapper definitions](https://www.nesdev.org/wiki/NES_2.0_submappers).

- [ ] Standard 3.1/3.2: geometry, aliases, conflicts, CHR readback and live rendering.
- [ ] Oversized CHR: every connected bank bit and unused-bit masking.
- [ ] Mirrored 2 KiB PRG-RAM: data persists across CHR changes; no invented RAM.
- [ ] Power-on versus soft reset: do not assume the physical latch resets to zero.
  Mesen initializes it from its power-on policy, a useful independent cross-check.
  [Mesen implementation](https://github.com/SourMesen/Mesen2/blob/master/Core/NES/Mappers/Nintendo/CNROM.h).
- [ ] Security behavior is separately identified: mapper **185**, with selected
  CHR-enable values and disabled-pattern open bus, is not ordinary 3.2.
- [ ] Aerobics Studio's speech-board extension is explicitly assessed:
  `$6000–$7FFF`, message bits 0–2 and a bit-6 falling edge trigger speech.
  The hardware reference reports missing speech emulation/misc-ROM data.

Last two entries: [CNROM extensions and security](https://www.nesdev.org/wiki/CNROM).
Recommendation: finish Adventure Island and standard/oversized/RAM mapper-3
behavior without inventing speech data or treating mapper 185 as mapper 3.
Keep speech/security extensions visibly unsupported until independently
specified and tested; any queue advancement with that exception must name the
exception, not report universal CNROM-family completeness. The user permits
practical hardware decisions, not silent feature deletion.

## Original-NES execution census

Completed a **bounded 1,800-frame bring-up route**, not a level clear:
power-on; no input through frame 899; Start during 900–905; Right+B from
1000; A held when `frame % 100 < 30`. Original title and moving gameplay
captures are `frame_300.png` and `frame_1400.png` in the ignored evidence
directory. Inputs are applied before each frame advance; filenames use the
zero-based loop index. No guest RAM writes, cheats or imported states.

Emulator: Ubuntu FCEUX 2.5.0, revision
`6c3a31a4f2c09be297a32f510e74b383f858773b`, old PPU, NTSC, sound output
disabled; Docker image `nes-to-sms-nesref` ID
`sha256:c291d28b7597352c18a7f203f6833e086e4bdc31e5c054b85aabde5216e44670`.
Reproduce using `bash out/adventure-reference.kECfiP/run.sh /path/to/nes-directory`.
The runner mounts the collection read-only and owns its temporary containers.

Observed, with the limitations of this short route:

- 1,795 NMI entries. Six CHR writes selected banks 0/3; all occurred with
  PPUMASK zero. Five were 1,113 original CPU cycles after NMI entry; the
  reset-time write preceded the first NMI. Banks 1/2 remain untested here.
- Writes execute at `$B81A` using a conflict-safe table at `$B865+X`.
  FCEUX's write callback reports the following PC `$B81D`.
- Only PPUMASK `$00` and `$1E` were observed: no one-layer-only mode in this
  route. No direct `$2007` read was observed. Direct `$2002` reads occurred
  only in reset loops `$8009`/`$800E`; this is not proof of absence of
  indexed/mirrored reads or later sprite-zero effects.
- `$4015=$0F` and `$4011=0` each occurred twice; no DMC enable was observed.
  Music/effects used ordinary APU writes. **No audio playback parity is
  established**, because sound output was disabled.

**Stack semantics are essential.** The repository's `cpu6502` decoder confirms
reset sets S to `$3F`; `$0140+` is deliberately available for data. Executed
TSX sites `$C328`/`$C375` save S in `$64`. Sprite emission pushes ROM-derived
destinations and dispatches with RTS `$C372`; `$C470` restores S, then
PLA/TAX/RTS unwinds work when the sprite budget is exhausted. Minimum S seen
at the watched stack instructions was `$28`. These are not ordinary balanced
native CALL/RET routines. Discover and validate synthetic return destinations
through profiles/analysis, and preserve real 6502 stack behavior.

The ignored Lua script's first attempts were discarded. Current FCEUX docs
describe `memory.registerread`, but this packaged build lacks it; the census
instead watches executed absolute LDA/LDX/LDY/BIT PPU-read sites. FCEUX's CLI
`--loadlua` has the already diagnosed fortified-realpath buffer abort. The
existing SMB3 GUI-load workaround works; no emulator binary was patched.
See `out/smb3-reference.jncimG/PROVENANCE.md` for that diagnosis.

## Full-game content and audio acceptance

Create evidence rows for **every stage**, not only each reused environment:

| Area | Required rounds | Additional gate |
| --- | --- | --- |
| 1 | 1-1, 1-2, 1-3, 1-4 | Boss; hidden continue bee |
| 2 | 2-1, 2-2, 2-3, 2-4 | Boss; bonus and ordinary 2-3 routes |
| 3 | 3-1, 3-2, 3-3, 3-4 | Boss; moving platforms and slopes |
| 4 | 4-1, 4-2, 4-3, 4-4 | Boss; ice |
| 5 | 5-1, 5-2, 5-3, 5-4 | Boss; complete platform sections |
| 6 | 6-1, 6-2, 6-3, 6-4 | Boss; later enemy sequences |
| 7 | 7-1, 7-2, 7-3, 7-4 | Boss; bonus and ordinary 7-3 routes |
| 8 | 8-1, 8-2, 8-3, 8-4 | Final boss, rescue and ending |

The author's [mechanics and level inventory](https://kb.speeddemosarchive.com/Hudson%27s_adventure_Island/Game_Mechanics)
provides useful candidate RAM landmarks: camera `$003A:$0006`, player X
`$0584`, subpixel `$0574`, speed `$05A4`, boss state `$0672`. Verify against
this dump before asserting them in routes. Its timer addresses `$0876/$0877`
require investigation of NES RAM mirroring; do not blindly use them as linear
RAM offsets. Main-game stages and bonus routes must be covered separately.

- [ ] Every background, enemy, boss, animation, palette and four-bank CHR identity.
- [ ] Walking/running/jumping; axe and fireball trajectories; skateboard,
  item loss, invincibility, energy depletion, fruit/milk, flower, eggplant,
  hidden eggs, keys, bonus rooms, pots and all discovered item-cycle outcomes.
- [ ] Checkpoints, death/retry, game over, bee-enabled continue, pause/resume,
  score/extra-life behavior, title/demo and ending. No cartridge saves are
  declared for this one-player target.
- [ ] Enumerate the translated sound engine's complete command/track table;
  map every reachable music/effect ID to an observed event. Capture title,
  environments, bosses, bonuses, items, hits/death, stage clear and ending,
  including effect interruption/resumption and simultaneous channels. This
  event list is a test plan, not a claim that each label is a separate track.

Hudson's NES-TB-USA manual establishes the eight-area boss progression,
controls, core items and hidden bee/continue rule; use its
[manual transcription](https://www.world-of-nintendo.com/manuals/nes/adventure_island.shtml)
as a publisher-authored content reference, not proof of successful conversion.

## Evidence tools and remaining gaps

The author's [TAS submission 5870](https://tasvideos.org/5870S) provides an
input-movie candidate: BizHawk 1.12.1, 128,945 frames. It skips sections via
bonuses in 2-3/7-3 and avoids fireballs. Thus even successful ending playback
needs supplemental content routes. Its game-version file SHA-1 differs from
this NES 2.0 file; verify payload/header provenance and synchronization before
reuse. No input movie or ROM was downloaded during this investigation.

Existing `frame-diff` is useful for instruction/state comparisons, but its
renderer documents SMB-specific HUD placement and its APU reference shares
runtime approximations. It is **not independently sufficient** for this
game's graphics/audio. `tools/core_route.py` also discards audio callbacks.
Use native NES captures plus real SMS-core moving-frame captures, and add
actual audio evidence before closing that gate. The PSG maps square/triangle/
noise approximately and currently silences DMC; preserve all reachable events
and document hardware adaptation explicitly, without claiming identical NES
waveforms or accepting dropped tracks as success.
