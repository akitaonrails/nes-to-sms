# Adventure Island / CNROM reference and acceptance inventory

Verified 2026-09-08. Adventure Island is the active compatibility target;
this document records original-NES evidence, **not a completed SMS conversion**.
The [mapper roadmap](mapper-roadmap.md) defines the full-game completion gate.
All unchecked entries below remain required unless an explicitly documented
hardware assessment chooses an alternative under the user's authorization.

## Implementation checkpoint: board model, source bus and clock

The generic `nes_rom::cnrom` model covers fixed PRG, 8–128 KiB CHR selection,
conflict variants and optional mirrored 2 KiB RAM. The opt-in
[source-bus runtime](cnrom-bus-runtime.md) adds assembled bus and genuine guest
stack coverage, while rendered-game admission still fails closed.
A related header fix preserves all twelve NES 2.0
mapper bits so extended IDs cannot alias supported boards.

Independent semantic and structure reviews passed. Workspace tests: 598 passed,
174 ignored; formatting and Clippy succeeded with existing warnings. Fresh
SMB1/CV1/SMB3 projects assembled before/after this foundation are byte-identical.
The canonical SMB clear, death/game-over and bonus-pipe routes each match the
6502 reference for 4,500 frames. Canonical ROMs were not replaced. Evidence is
locally preserved under ignored `out/adventure-baseline.TvOphd/`.

The later bus checkpoint passes 609 workspace tests and its independent
assembled/core checks; its linked report records the updated regression evidence.
The [source-clock checkpoint](source-clock-runtime.md) subsequently passes
independent review with 618 workspace tests, 19 assembled projects and selected
actual-core checks; its report distinguishes fresh and reused evidence.
The [banked resume and quiet-wait checkpoint](source-clock-scalability.md) also
passes independent review, with 622 workspace tests and unchanged legacy ROMs.
Next: remaining source hardware, coherent graphics and full-game/audio validation.
This checkpoint does not close any rendered-game
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
reuse. The initial investigation downloaded no movies. The follow-up below
retrieved public **input-only** movies; no ROM was downloaded.

Existing `frame-diff` is useful for instruction/state comparisons, but its
renderer documents SMB-specific HUD placement and its APU reference shares
runtime approximations. It is **not independently sufficient** for this
game's graphics/audio. `tools/core_route.py` also discards audio callbacks.
Use native NES captures plus real SMS-core moving-frame captures, and add
actual audio evidence before closing that gate. The PSG maps square/triangle/
noise approximately and currently silences DMC; preserve all reachable events
and document hardware adaptation explicitly, without claiming identical NES
waveforms or accepting dropped tracks as success.

## Complete original-NES baseline (2026-09-08)

An original-NES playthrough now reaches **all 32 stage pairs, the final boss,
rescue/ending and return to title** using controller input only. This proves
the reference route works, **not that the SMS conversion is complete**.

The first candidate, ktwo's QuickNES/BizHawk movie 5870, has exactly this ROM's
payload: replacing only header bytes 7–15 in memory with the legacy values
produces its declared file SHA-1. Direct QuickNES-to-FCEUX replay nevertheless
failed to start correctly. The finite 6,000-frame failed attempt is preserved
in `out/adventure-quicknes-failed.Wi2HIW/`; its attract-mode activity is not
counted as playthrough evidence.

The working input source is Plamondonl7000's
[FCEUX 2.2.2 submission 5357](https://tasvideos.org/5357S), 141,407 frames.
It was cancelled for not attempting a speed record, not for lacking an ending.
The archive contains one FM2 controller movie, no ROM or saved state. Its
payload MD5 matches this input exactly (`JIuhrL/tKL5OaDmoWKXGCg==` in FM2's
base64 notation), and requests NTSC, old PPU and one controller.

Replay uses the already pinned FCEUX 2.5.0 container, with **one neutral frame
after Lua's power-on request** before applying movie frame zero. This is an
explicit API-start alignment, not a modified game or hand-edited input route.
Without that prelude, FCEUX misses the initial Start; the failed offset-zero
attempt is preserved separately. The working run advances 144,000 frames,
releasing all input after the movie ends. No cheats, memory writes, imported
states, ROM edits or gameplay patches were used.

Preserved first-run evidence: `out/adventure-full-frozen-first.Wi2HIW/`.
`frame_141000.png` shows the final encounter, `frame_142000.png` shows the
congratulations/rescue screen, and `frame_143999.png` shows the returned title
with score 322,900. The RAM census contains all area/round combinations.

| Artifact | SHA-256 |
| --- | --- |
| Public 5357 ZIP | `ae1aa5a2cb6512cb2e607eac6768c933a94a3ee5977b936699c8376dcd0ad185` |
| Original FM2 input | `341cd5ba388367d75958a483375d4c0b03e96f11187ed5041f583087dc816594` |
| Converted input-only text | `86ccff239bf67a18b7d26c696a285b3c3fbe1f060f1de0d7849ab367489fc6f8` |
| First-run sampled RAM CSV | `7dedb24fcfe08b242bc7161db829324e7338e1e19a7c4aead5e83bf8b5be6cdf` |
| First-run mapper/APU events | `800ae9ed0c3089c396ca73193805de7476681f93d4c8c4d0ed04417394822ead` |
| Native ending GD framebuffer | `26c3b199aef70bcac9c4b901e4c326da5dc3770068a14bc70f0e08e56509649a` |

The full route observes **107 CHR writes across all four banks**. Unlike the
short bring-up route, boss transitions write with PPUMASK `$1E` too. Apart
from the initial reset write, the sampled writes occur 1,113 or 1,354 original
CPU cycles after NMI entry, inside vblank: rendering-enabled does not mean a
visible-scanline write. Only PPUMASK `$00/$1E` occurs. `$4011=0` occurs 33
times, with no observed DMC enable. Sound was disabled in this first run;
no acoustic parity is claimed.

Completion still needs supplemental routes: every hidden/bonus/item outcome,
fireball use, deaths/continues and any reachable audio commands absent from
this run. The slower movie's ending is not proof of exhaustive content.
Current reproducible runner and an additional observation pass live under
`out/adventure-full-reference.Wi2HIW/`; all movie data and game captures stay
ignored. The runner accepts `AI_REF_OFFSET=1 AI_REF_LIMIT=144000` and the
collection-directory argument; the converted input must be from movie 5357.

### Exact stage and execution landmarks

A second identical-input pass records the stage-index writes and every frame's
logical handshake count. Its sampled RAM CSV and ending framebuffer are
byte-identical to the preserved first run. It uses the same offset, CPU/PPU
settings and ROM, with sound output enabled; the packaged CLI recording
option produced **no WAV**, so there is still no acoustic capture to accept.

Observed RAM fields are zero-based area `$37` and round `$38`. Round advance
is observed at callback PC `$815D`; wrapping resets `$38` and increments
`$37` around `$8167/$8169`. These frame intervals include transition screens,
not uninterrupted controllable gameplay. The first interval begins at game
initialization (frame 12); the final one includes the ending and returned title.

| Area | Round 1 frames | Round 2 frames | Round 3 frames | Round 4 frames |
| --- | --- | --- | --- | --- |
| 1 | 12–4334 | 4335–8587 | 8588–12801 | 12802–17587 |
| 2 | 17588–22139 | 22140–26465 | 26466–30490 | 30491–35185 |
| 3 | 35186–39703 | 39704–43799 | 43800–48112 | 48113–52933 |
| 4 | 52934–57128 | 57129–61260 | 61261–65390 | 65391–70087 |
| 5 | 70088–74348 | 74349–78443 | 78444–82934 | 82935–87878 |
| 6 | 87879–92256 | 92257–96365 | 96366–100506 | 100507–105241 |
| 7 | 105242–109547 | 109548–113818 | 113819–118016 | 118017–122906 |
| 8 | 122907–127349 | 127350–132251 | 132252–136789 | 136790–143999 |

`$B755` enters the main-frame handshake: it stores `$FF` in `$0B`, waits at
`$B759/$B75B` for NMI to clear that byte, then performs post-frame work before
returning. `$66` increments modulo 16 later in this helper, so it is not a
wide monotonic elapsed-frame counter. Use `ticks.csv`'s observed handshake
counts, original input frames, camera and stage state when building translated
routes; do not equate SMS physical frames with original game updates.

The sprite-return observation point `$C372` encounters 13 destinations:
`$88C3`, `$8924`, `$8931`, `$8C2E`, `$8EDA`, `$8EE6`, `$8EF1`, `$A72E`,
`$C3B8`, `$C3CA`, `$C3E1`, `$C413`, `$C42F`. These include normal returns
as well as synthetic dispatch; determine the controlling path rather than
turning this finite census into a permissive global RTS target whitelist.

Sound independently uses synthetic return dispatch: `$C6C1/$C6C5` load
destinations from `$C52D,X/$C52C,X`, push them, and RTS at `$C6C9`.
`$C6CA` can discard a return address with two PLA instructions. New effect
requests use `$A0` with `$A1` continuation state; music selection uses `$AB`
and is gated by `$AC`. All facts here were cross-checked with the repository's
6502 decoder, not a separate toy interpreter.

### Audio coverage is measurable, but not complete

Accepted effect selectors observed at `$C69A`:
`00 01 02 03 04 05 06 07 08 09 0A 0B 0D 0F 10 12`.
Accepted music selectors observed at `$C8CE`:
`01 02 03 04 05 06 07 08 0B 0C 0D 0E`.
These are selector IDs, not an assertion that every selector is an audible
track: for example, initialization/silencing paths must be identified.

The engine admits effect IDs below `$14` and music IDs below `$0F`.
Therefore effect candidates `$0C/$0E/$11/$13` and music `$09/$0A` require
reachability/content investigation and supplemental routes. They cannot be
checked off from the complete-playthrough movie. `summary.txt` records all
observed sound-return destinations and APU register values; `events.csv`
records accepted selections with original frame, PC and timing.

| Second-pass artifact | SHA-256 |
| --- | --- |
| Per-frame stage/input/handshake CSV | `4c897d952de36561550c78fd1430e36993c05871650dae87b107586b39b6db0f` |
| Mapper/stage/audio event CSV | `d3fae5f128016a2b4bc67d6a453d63e128bb086ac269f129d08447d3cc1629bf` |
| Dispatch/audio census | `f7e0f0048575a317c68bd32ee46bb44aede1ec12afe35d959143d9e1699a7968` |

The current evidence directory is frozen after this pass. Captures listed in
`captures.txt` belong to it; an older `frame_005999` capture in the directory
is only from the earlier bounded trial and is not part of its manifest.
