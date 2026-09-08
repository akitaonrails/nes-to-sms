# Super Mario Bros. 3 / MMC3 effort

Started 2026-09-07 at `80e30f6`. SMB3 is the user-selected next compatibility
target; SMB1 and Castlevania remain the regression floor. This supplements
the architecture in [master-plan.md](master-plan.md).

## Status: experimental first-level route verified; performance remains limited

`profiles/smb3.toml` identifies the local USA Rev 1 target and opts into the
experimental full runtime and profile-scoped cooperative single-split adapter.
The user has requested continuation until a playable SMB3 SMS ROM exists.
Cartridge RAM, physical CHR rendering and mapped-call translation now support
reset → animated title → controller-driven World 1 map → level 1-1 in Genesis
Plus GX at explicit 500% overclock. The input-only route now runs/jumps, hits
the mushroom question block, collects the mushroom, clears 1-1 and returns to
the normal map with the panel changed from `$03` to `$00`, lives still four,
and no trap. A separate no-input death route returns to the map with three lives.
This establishes functional first-level support, **not full-speed or full-game
compatibility**. The latest active traversal averages only 4.594 game updates/second
at `500`; 30.54% of callbacks in the gameplay window are uniform-color frames
during rebuilding, with a longest run of 11 callbacks. Presentation remains
visibly slow and intermittent; completing an automated route does not establish
comfortable human playability. See the measured contract below.
The adapter is not cycle-accurate MMC3/A12 emulation; see
[the runtime contract and limits](mmc3-graphics-runtime.md).

The checked local file has a NES 2.0 header, mapper 4/submapper 0, 256 KiB
PRG ROM, 128 KiB CHR ROM, 8 KiB volatile PRG RAM, no trainer, no battery,
and no four-screen flag. The current
[NES 2.0 submapper specification](https://www.nesdev.org/wiki/NES_2.0_submappers#004:_MMC3)
assigns mapper 4/submapper 0 Sharp MMC3 behavior. The host board model selects
that behavior explicitly; this is not identification of the physical chip.

- Whole-file SHA-256: `1ddc4b429490e298c05fa70389034107c0cb50f671c7c1baaa3f228224afc2e9`.
- PRG+CHR SHA-256, used by the profile:
  `959fdd32c71735d6fb2bd16a646d39f4ee65623273dd035e6a968e991bd13ef8`.
- Independently read final PRG bytes: `86 f4 40 ff 95 f7`, giving NMI `$F486`,
  RESET `$FF40`, IRQ `$F795`. The original mapper-rejection probe did not
  validate vectors; these values were independently read from the payload.

Reproduce the compatibility probe with a legally obtained matching ROM:

```sh
cargo run --release -p nes_to_sms --bin nes-to-sms -- \
  /path/to/smb3.nes profiles/smb3.toml out/smb3 --runtime runtime
```

Generation now succeeds for this matching payload; assemble the emitted project
with the Docker toolchain. Unresolved targets still trap. Do not use permissive
stubs or change the ROM header to masquerade as UxROM.

## Why this is a new architectural step

| Boundary | Starting implementation | SMB3 requirement |
| --- | --- | --- |
| PRG identity | Experimental independent 8 KiB banking; legacy UxROM retained | Full-game reachability and safe remapping continuations |
| Game memory | NES 2 KiB mirror plus runtime/render storage | Additional 8 KiB cartridge work RAM |
| Graphics | NROM CHR or CV1 CHR RAM | 128 KiB physical CHR, mapped in 1/2 KiB windows |
| Raster timing | Existing SMS split machinery | Qualified MMC3 IRQ events plus a separate SMS presentation strategy |

The [US PRG1 disassembly](https://github.com/captainsouthbird/smb3) is useful
for labels and control flow, not a replacement for translating the ROM.
Its [memory definitions](https://raw.githubusercontent.com/captainsouthbird/smb3/master/smb3.asm)
place 6,480 bytes of level tile data in cartridge RAM, followed by other game
state. With PRG inversion, bank 30 occupies `$8000`, bank 31 `$E000`, and
`$A000`/`$C000` switch independently. Its
[interrupt code](https://raw.githubusercontent.com/captainsouthbird/smb3/master/PRG/prg031.asm)
temporarily maps audio into both switchable windows and restores them; raster
handlers change CHR and scrolling for HUD and other partitions. Verify any
imported address against the pinned payload; some hardware comments in the
disassembly are inaccurate.

[MMC3 IRQ behavior](https://www.nesdev.org/wiki/MMC3) depends on qualified
PPU A12 edges, not simply a frame's scanline number. Reload, acknowledge,
enable, and chip-revision behavior need explicit tests. Hardware-derived
[MMC3 test sources](https://github.com/christopherpow/nes-test-roms/tree/master/mmc3_test_2)
provide independent cases; they validate NES mapper emulation, not SMS output.

The [SMS VDP](https://www.smspower.org/uploads/Development/msvdp-20021112.txt)
has 16 KiB VRAM and cannot change vertical scroll during active display.
Thus a literal port of SMB3's raster handler is insufficient. CHR expansion
and palette variants need bounded residency and coherent frame presentation.

[Sega cartridge RAM](https://www.smspower.org/Development/Mappers) overlays
slot-2 ROM, so RAM helpers must execute outside that window and restore mapper
state safely. Cartridge RAM must not overlap the runtime's existing render
storage. The older plan's universal 512 KiB ROM ceiling is incorrect: capacity
depends on mapper implementation. Measure output expansion and declare a
specific tested cartridge/emulator contract before claiming feasibility.

## Ordered implementation and acceptance gates

1. **Compatibility baseline (completed at `5b1ba1e`).** Pin identity and vectors;
   test mapper-4 rejection without creating or modifying output. Publish this
   plan and preserve SMB1/CV1 artifacts.
2. **Executable 8 KiB banking foundation (completed).** Define
   physical-bank/window identity and mapper state separately from the UxROM
   policy. Integrate it with a real consumer and synthetic executable fixture;
   do not land an unused board model. Cover both PRG modes, independent R6/R7,
   address aliases, masked bank selection, fixed vectors, data reads, and
   cross-window calls/returns. Then extend profile/discovery/dispatch while
   trapping unknown dynamic targets. Reuse `AnalysisWindow`; remove concrete
   16 KiB assumptions in orchestration rather than globally changing its size.
3. **Memory and CHR contract.** Prove non-overlapping cartridge work RAM,
   enable/protect behavior and interrupt-safe bank restoration. Track physical
   CHR identity, 2 KiB even alignment, inversion, bank-change invalidation,
   dynamic mirroring, cache residency and complete-frame commits. Measure
   emitted ROM size before committing to a cartridge capacity.
4. **Reference timing and SMS IRQ adaptation.** Obtain actual-core NES
   traces/captures; verify A12 qualification, deferred reload, zero-latch
   variants, acknowledge and interrupt masking. Separately implement SMS
   HUD/playfield composition. Neither a synthetic scanline clock nor one
   correct screenshot establishes raster fidelity.
5. **First visible conversion.** Reset → animated title → controller-driven
   world map, with no permissive stubs or injected game state. Gate moving
   frames, input response, bank restoration and stack bounds.
6. **First functional route (verified; performance limitations remain).** Enter 1-1, move/jump, hit a question block,
   collect its power-up, finish the level and return to the map. Compare game/cart-RAM state
   and consecutive HUD/playfield frames with NES evidence. Then expand to
   vertical scrolling, inventory, transformations, roulette and other splits.

The native input-only reference now proves that route with lives unchanged,
including the level panel changing to Mario-completed. It does not prove brick
destruction: 1-1's ordinary bricks form ground-supported stacks, requiring a
separate shell/tail setup rather than a simple head bump. That mechanic belongs
to expanded coverage, not an unsupported claim about this first route.

The translated route uses only controller input and read-only observation;
no state loads, guest-memory writes or permissive stubs. ROM SHA-256 is
`ec5299b6dace9689d54f37e8c3a8bb985781cdca64ac41f1d299602527fe8c3b`.
It reaches the question block at physical frame 6257, collects the mushroom
at 9106, starts the goal sequence at 33982 and reaches the completed map at
37468. The run ends after 60 further logical updates, at physical frame 37851.
The observer accounts for elapsed epochs when a brief wait boundary is missed,
but never fabricates intermediate RAM snapshots or input decisions. These
controller-based checkpoints are not a cycle-exact NES/SMS state oracle.

Measure logical game ticks/second at stock clock and explicit overclock,
Z80 cycles/tick, mapper overhead, CHR bytes uploaded/frame, peak tile residency,
and dropped/repeated presentations. SMB3 full speed is an experiment, not a
promise. Game-specific facts stay in profiles/runtime, not Rust hand-ports.

### Code-size-for-speed candidate: late selective inlining

The user proposed retaining calls/jumps in the intermediate representation
while emitting inlined final code. Keep canonical IR function boundaries for
discovery and validation; apply selected call-site expansion before final Z80
bank placement/linking, not by copying already-linked binary bytes. Recompute
flag liveness and branch labels after expansion. The existing backend already
inlines flag, addressing and selected hardware helpers, but does not have a
general translated-function inliner.

Start with measured hot, small leaf routines with known bank identity and no
observable return-stack manipulation or hardware side effects. Exclude unknown
dynamic calls, recursive cycles and remapping continuations until their
contracts are proven. Count eliminated translated-call bookkeeping and far-bank
transfers, plus newly removable flag/register work; subtract extra bank-crossing
cost introduced by code growth. Compare actual cycles/tick and emitted size
under a fixed budget. More banked ROM is a tradeoff, not extra directly mapped
RAM, and Z80 versus 6502 cycle counts must be normalized by clock frequency.
This is a candidate for the playability/performance phase, not an implemented
optimizer or a reason to delay the current banking/graphics foundation.

The [LLVM-style optimization research](ir-optimization-research.md) recommends
CFG-wide value/flag/effect analysis, then one measured region optimization and
selective inlining. It inventories existing passes, legal transformation
boundaries, target costs and required A/B evidence. Adopting individual passes
does not imply replacing the backend with LLVM or promising a speedup.

### Experimental banking contract

`MMC3_BANKING_EXPERIMENT` opts a synthetic profile into the new path. A separate
Sharp MMC3 board model supplies 8 KiB PRG / 1 KiB CHR identities and qualified
A12/M2 IRQ semantics. This bounded experiment executes PRG banking only: cartridge
RAM, banked CHR rendering, IRQ delivery and unsafe remapping continuations
remain rejected or trapped. SMB3 now uses the separate `MMC3_FULL_RUNTIME`
contract, not this bounded experiment.
Computed indirect jumps and indirect pointer accesses remain outside this
bounded contract; helper tests alone do not establish their pipeline support.

Raw PRG pages are packed in pairs into SMS banks starting at 96. The experiment
limits translated code to banks 4–31 in a 2 MiB image because its software-return
frame packs bank identity and flags into one byte. Full mode separates those
fields and permits additional code banks; its assembled tests include banks
31/32/64/95. The current SMB3 image is also 2 MiB. This cartridge size is tested in Genesis
Plus GX, not claimed compatible with every historical cartridge mapper.
Assembled fixtures cover both PRG modes, all physical pages, mapped-window
calls/returns, aliases, indexed boundaries and mapping/register preservation.
The generic `--validate` harness explicitly skips MMC3 until its bus supports
the mapper; these skips are not passing differential results.

An absolute-zero-page lowering omission was fixed with differential coverage.
A separate pre-existing stack-page RMW omission remains deferred: changing it
altered SMB1/CV1 artifacts, so it needs its own behavioral review. MMC3 rejects
that unsupported form explicitly instead of silently emitting incorrect code.

## Evidence and preservation

Initial evidence is under ignored `out/smb3-baseline.bo9UiK/`. Original-NES
capture is now working under `out/smb3-reference.jncimG/`: FCEUX 2.5.0 renders
the title, map and 1-1 on an input-only 1,601-frame route. This is original
NES evidence, not SMS output or a level-clear route. `PROVENANCE.md` records
ROM/emulator hashes, inputs and reproduction. The CLI Lua loader's fortified
`realpath` call caused the original abort; loading Lua through the existing UI
bypasses that branch without changing the emulator or adding a dependency.
The ignored runner is `bash out/smb3-reference.jncimG/run.sh`.

Additional native evidence under `out/smb3-irq-reference.i8pP7A/` records
1,563 NMI and 1,560 IRQ entries across that same route. Each IRQ-bearing frame
has one IRQ, with observed latch values `$C0/$C1`. At completed NMI and IRQ
handlers, the CHR mappings differ: playfield and HUD require separate records.
This is route-specific evidence, not a replacement for qualified-A12 semantics.
Save-only PPU records under `out/smb3-raster-records.TsUusn/` confirm the
vertical-scroll origin. A preliminary residency census is not a final bound:
subsequent intro captures proved that IRQ writes to PPUADDR reload the live
vertical position, invalidating unconditional continuity across the split.
Rendering-time PPUDATA reads also affect that position. The renderer must
distinguish those effects from later PPUSCROLL writes to the temporary address.

Useful original-game assertions are world `$0727`, map operation `$0729`,
tileset `$070A`, and cartridge-RAM layout/object pointers `$7EB9–$7EBC`.
Native frame 1101 enters tileset `$01`, layout `$BB82`, objects `$C527`.
During gameplay only, X is `$0075:$0090`, Y is `$0087:$00A2`, suit is `$00ED`,
death is `$00F1`, and Mario's lives are `$0736`. The zero-page fields are
overlaid in other game modes. Definitions from the
[source RAM declarations](https://raw.githubusercontent.com/captainsouthbird/smb3/master/smb3.asm)
were checked against native transitions and matching ROM stores. Completion
must include the actual exit sequence and cleared map panel; a zero status
byte alone is not evidence of a successful level clear.

Current `frame-diff` is **not** an MMC3 ground-truth oracle: it has no PPU
fetch/A12 stream, uses an instruction-based frame allowance, and does not
deliver mapper IRQs. Extending cartridge registers alone cannot fix that.

Before every mapper-phase commit: workspace tests, formatting/lint, all three
SMB differential routes and route expectations, isolated SMB/CV1 generation
and byte comparison, and Alter Ego assembly. Preserve moving-HUD evidence;
render changes require new consecutive real-core captures, not just static
checkpoints. Do not overwrite `out/smb` or `out/cv1` during experiments.

Preserved ROM SHA-256 values:

- SMB1: `b3e14180f904182882a40da5c1d7be4ff778e4f0518e4ddcf8de250ee23211b0`.
- CV1: `d4e3e0fe60b5b0df48531b2e48ec5134c3a551e284ecd03d3477ae36d06b4497`.

Phase 0 verification:

- Workspace: **530 passed, 114 ignored**. Formatting and Clippy completed;
  existing lint warnings remain visible in the retained log.
- All **13** assembled NROM/UxROM IRQ tests passed, including the moving-HUD
  split contracts.
- Isolated SMB1 and CV1 rebuilds match the hashes above byte-for-byte;
  canonical projects were not regenerated or modified.
- The 301-million-step SMB clear trace passes no-trap, terminal RAM and
  zero-stale-BG gates. All seven PPM checkpoints match the accepted HUD-fix
  baseline byte-for-byte.
- SMB clear (4,500 frames), pipe (3,000 frames) and death/game-over
  (3,300 frames) all report **NO DIVERGENCE** against the NES oracle.
- Alter Ego's existing generated project assembles in an isolated copy.
  Its original input was not located: this establishes assembly, not fresh
  pipeline-generation or gameplay parity for Alter Ego.
- Independent plan and implementation reviews accepted this as a compatibility
  baseline; the required regression gates subsequently passed. No production
  module boundaries or runtime code changed.

Banking foundation verification (reviewed final candidate):

- Workspace **564 passed, 118 ignored**; formatting and Clippy pass with
  existing warnings visible. All 13 assembled legacy IRQ tests pass.
- Nine MMC3 pipeline tests including Docker assembly/execution pass; assembled
  runtime helpers pass 240 seeded ABI combinations. Seeded helper tests are
  not gameplay evidence.
- The 2 MiB fixture completes in Genesis Plus GX at stock clock after 33
  physical frames, passing 25 RAM checks without guest-state writes.
- Isolated SMB1/CV1 rebuilds remain byte-identical. All three SMB differential
  routes pass; the 301-million-step trace passes its terminal, no-trap and
  BGV gates, with seven checkpoint images identical to the accepted baseline.
- Alter Ego's existing generated project still assembles; the original ROM
  remains unavailable for fresh generation. Logs are under ignored
  `out/smb3-banking.9Xi1aK/`.

The first independent implementation review reproduced bank-ambiguous flag
analysis, a computed-transfer remapping bypass and an incorrectly admitted
indirect jump. Added regression fixtures and conservative capability checks
resolve those findings; the second review accepted the bounded foundation and
independently reran its assembled tests. A separate structure review found no
module or dependency-direction blocker. All required legacy gates subsequently
passed on the final candidate. This completes banking, not SMB3 playability.
