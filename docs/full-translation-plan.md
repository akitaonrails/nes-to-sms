# Full SMB NES-to-SMS Translation Plan

> **Status:** subordinate to [`master-plan.md`](master-plan.md). This document
> covers SMB-specific subsystem phasing only. Architecture, principles, crate
> layout, validation strategy, and global sequencing live in the master plan;
> when this file conflicts, the master plan wins. The v1 target defined here
> ("complete game") has been narrowed in the master plan to **playable World
> 1-1** with audio deferred — see master plan Phase 4 for the canonical
> sequence.

This plan defines the path from the current PoC to a complete SMS version of
Super Mario Bros. It is intentionally subsystem-based. A direct 6502-to-Z80
opcode translation is only useful for pure game logic; NES hardware behavior
must be replaced with SMS-native shims.

## Ground Rules

- Do not install retro tooling on the host. Use project Docker/Compose.
- Keep generated ROMs and commercial ROM data out of version control.
- Every milestone must produce an artifact in `poc/out/smb/` and an entry in
  `docs/poc-worklog.md`.
- Prefer real SMB data and behavior over approximations. If a temporary
  approximation is used, document it as such.

## Phase 1: Stable Translation Substrate

Goal: stop emitting opaque byte blobs where possible.

- Add a small internal Z80 emitter with labels, comments, and source mappings.
- Generate `poc/out/smb/generated.asm` beside `poc.sms`.
- Keep raw-byte emission only for instructions not yet represented.
- Add source-map comments pointing to SMB labels and PRG addresses.

Acceptance:

- `poc.sms` still boots in Mednafen.
- Generated assembly/report identifies every emitted routine by purpose.

## Phase 2: Title Mode

Goal: complete the title/menu mode before gameplay.

- Port `DrawTitleScreen` and `UpdateScreen` semantics.
- Port `GameMenuRoutine`.
- Port `DrawMushroomIcon`.
- Map SMS input to NES title actions.
- Replace hand-selected title behavior with generated translation where safe.

Acceptance:

- Title screen is rendered from SMB data.
- Cursor toggles one/two-player.
- Start transitions into a game-init/intermediate state.

## Phase 3: Initialization and Pointers

Goal: reproduce SMB's area selection state.

- Port `InitializeGame`.
- Port `LoadAreaPointer`, `FindAreaPointer`, and `GetAreaDataAddrs`.
- Port RAM initialization ranges.
- Generate reports for selected world/level pointers.

Acceptance:

- World 1-1 resolves to `L_GroundArea6` and `E_GroundArea6` at runtime.
- Header fields match the disassembly-derived report.

## Phase 4: Area Parser and Background Renderer

Goal: render playable-level backgrounds from real level data.

- Port `AreaParserTaskHandler`.
- Port `AreaParserCore`.
- Port `ProcessAreaData` and `DecodeAreaData`.
- Port terrain, background scenery, foreground scenery, block-buffer writes.
- Port `RenderAreaGraphics` to SMS name table writes.
- Replace the current Rust-side World 1-1 base predecode with translated logic.

Acceptance:

- World 1-1 page 0 renders terrain, scenery, blocks, pipes, and question blocks.
- The first several screens match NES layout semantically.
- Scrolling updates columns using SMS VDP writes.

Current progress:

- Corrected World 1-1 data binding to `L_GroundArea6` / `E_GroundArea6`.
- Added a `DecodeAreaData` classification trace for all World 1-1 area objects.
- Added a page-1 active-object overlay for question blocks, bricks, and the
  first pipe. This proves the object classifications can drive SMS metatiles
  column-by-column, but it is not yet the exact live three-slot area parser.

## Phase 5: Player Rendering and Movement

Goal: draw and move Mario with real SMB logic.

- Port player state RAM.
- Port controller normalization.
- Port `PlayerCtrlRoutine` and movement physics in slices.
- Port `PlayerGfxHandler` and sprite tile selection.
- Map NES OAM data to SMS sprite table layout.

Acceptance:

- Mario appears at the correct start position.
- Left/right/jump movement changes position and animation.
- Collision can be stubbed only until Phase 6.

## Phase 6: Collision and Blocks

Goal: make the first screen interactive.

- Port block buffer collision routines.
- Port question block/brick bump handling.
- Port coin and power-up block metadata.
- Keep power-up behavior minimal until enemy/object phases are active.

Acceptance:

- Mario stands on the ground and collides with blocks.
- Question blocks and bricks can be hit and update the SMS nametable.

## Phase 7: Enemies and Object System

Goal: reproduce moving actors.

- Port enemy data parser.
- Port enemy slots and object initialization.
- Port Goomba/Koopa movement first.
- Port sprite rendering for enemies.
- Port collision between player and enemies.

Acceptance:

- First Goomba in 1-1 appears at the correct position.
- It moves, animates, and interacts with Mario.

## Phase 8: Scrolling and Full Level Progression

Goal: make World 1-1 playable end to end.

- Port horizontal scrolling state.
- Port column streaming from area parser.
- Port flagpole and end-of-level state.
- Port timer, score, lives, coins enough for 1-1.

Acceptance:

- World 1-1 can be played from start to flagpole.

## Phase 9: Remaining Worlds and Game Modes

Goal: broaden from 1-1 to full game.

- Validate all area types: ground, underground, water, castle.
- Port special objects: warp zones, vines, bridges, elevators, Bowser.
- Port two-player flow and continue/world-select behavior.
- Port game-over/victory modes.

Acceptance:

- All original areas load and render.
- Each area type has a verified smoke path.

## Phase 10: Audio

Goal: add approximate SMS PSG audio after gameplay works.

- Log NES APU writes.
- Map pulse channels to PSG tones.
- Approximate triangle/noise.
- Defer or replace DMC.

Acceptance:

- Music and effects are recognizable enough for gameplay testing.

## Final Acceptance

- `poc.sms` becomes the release candidate SMS ROM.
- Boots in Mednafen and at least one other SMS emulator.
- Title screen, 1-player flow, levels, enemies, collision, scrolling, score,
  timer, lives, and game-over/victory flow work.
- Known deviations from NES are documented.
