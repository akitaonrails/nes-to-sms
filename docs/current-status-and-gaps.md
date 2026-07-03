# Current Status and Gaps

> **Status:** inventory and status report only. Architecture, sequencing, and
> the v1/v2/v3 targets are in [`master-plan.md`](master-plan.md). The "Next
> Meaningful Step" section at the bottom of this file describes slice-driven
> work that is now explicitly out of policy — see master plan principles 1
> and 2. Use this file to record state, not to plan changes.

## Executive Status

We do not yet have a finished or human-playable SMS port of Super Mario Bros.
We do have a Rust workspace pipeline that parses the NES ROM, discovers and
lifts hundreds of SMB routines, lowers them to Z80, emits a WLA-DX SMS project,
assembles a bootable ROM, and can run a deterministic `trace-sms` acceptance
route through the World 1-1 transition without hitting an unresolved-runtime
trap.

The active implementation now lives under the root `Cargo.toml` workspace. The
old `poc/` tree is legacy experiment history unless a task explicitly asks for
it. The project has moved beyond planning and one-off visual experiments, but
it still remains far from a complete game translation.

### 2026-06-25 Snapshot

- Latest SMB project generation reports **631/631 lifted routines**, **0 lift
  failures**, and **0 lower failures**.
- Remaining strict unresolved labels are **22 deferred sound-engine labels** in
  `$F3xx-$F6xx`; no non-sound gameplay labels remain in the current unresolved
  report.
- `profiles/smb/acceptance/1-1-clear.buttons` reaches the expected post-1-1 /
  1-2 transition state under `trace-sms` with `--expect-no-trap` and RAM checks
  for `$0760`, `$075C`, and `$000E`.
- `cargo test --workspace` passes for the current Rust workspace.
- Audio is still intentionally stubbed/deferred. Human playability, visual
  equivalence, second-emulator validation, and full game completion remain
  open.

### 2026-06-29 Snapshot

- The current green build has been visually audited against the generated
  `out/smb/checkpoints/1-1-clear/*.ppm` route checkpoints.
- No blocker was seen for scripted World 1-1 clear readability: the status bar
  split is stable, 1-1 terrain and the flagpole/castle are recognizable, and the
  post-route RAM checks still indicate transition into 1-2.
- Remaining visible v1 polish is concentrated in generic sprite/background
  behavior: Mario can be partly swallowed/tinted by bushes, and small stale
  sprite fragments appear in mid/late-route checkpoints.
- This does not prove broad game parity or manual end-to-end playability. 1-2
  visuals are still wrong after transition, audio is deferred, and raw-CIRAM /
  materializer parity remains post-v1 unless broader level coverage becomes the
  active target.
- A trace-renderer priority fix improved the refreshed checkpoint PPMs: Mario is
  now readable around bush areas and 1-1 route landmarks are clearer. Runtime
  SAT-tail clearing remains rejected for v1 because the added VDP work missed
  the accepted 301M-step checkpoint budget; post-terminator SAT bytes are ignored
  by SMS rendering. Remaining visible polish is the 01600 floating fragment and
  title/menu clutter.

## What We Have

### ROM and Asset Extraction

- Parses `Super Mario Bros. (World).nes` as iNES/NROM mapper 0.
- Confirms 32 KB PRG, 8 KB CHR, and vectors.
- Extracts:
  - `poc/out/smb/prg.bin`;
  - `poc/out/smb/chr.bin`;
  - `poc/out/smb/vectors.txt`;
  - `poc/out/smb/map.json`.
- Converts NES CHR 2bpp tiles into SMS Mode 4 4bpp tile data:
  - `poc/out/smb/tiles.sms4bpp`;
  - `poc/out/smb/tiles.ppm`.

### Bootable SMS Harness

- Generates a 32 KB SMS ROM at `poc/out/smb/poc.sms`.
- Writes a valid `TMR SEGA` header and checksum.
- Initializes SMS VDP Mode 4, CRAM, VRAM, name table, and sprite table.
- Boots under the Dockerized Mednafen smoke test.
- Does not require installing emulator/toolchain binaries on the host.

### Real SMB Title Data

- Decodes SMB title-screen VRAM update data from CHR/PPU `$1EC0`.
- Reconstructs the NES title nametable and converts it to SMS name-table format.
- Current artifacts:
  - `title_decode.txt`;
  - `title_nametable.bin`;
  - `title_sms_nametable.bin`.

### Title Menu Behavior Slice

- Implements a real title-menu cursor behavior slice.
- Maps SMS button 1 to NES Select.
- Maps SMS button 2 to NES Start for the current PoC.
- Cursor writes are based on SMB `DrawMushroomIcon` / `MushroomIconData`.
- This is a manual semantic port, not yet generated from full 6502 translation.

### World 1-1 Data Work

- Correctly resolves World 1-1 to:
  - `L_GroundArea6`;
  - `E_GroundArea6`.
- Extracts and reports real World 1-1 area/enemy streams.
- Decodes area header `$50 $21`.
- Produces a `DecodeAreaData`-style object classification trace.
- Builds a first SMS nametable for a World 1-1 screen from real terrain,
  scenery, metatile, and foreground object data.
- Current artifacts:
  - `world_1_1_data.txt`;
  - `world_1_1_area_trace.txt`;
  - `world_1_1_base_sms_nametable.bin`;
  - `world_1_1_initial_screen_sms_nametable.bin`;
  - `world_1_1_object_overlay.txt`.

### Z80 Output Backend

- Added `poc/src/z80_backend.rs`.
- Generates both:
  - final Z80 machine code used by `poc.sms`;
  - readable assembly at `poc/out/smb/generated.asm`.
- Supports labels, local generated labels, data blocks, alignment, and 16-bit
  patching.

### SMS Runtime Emitters

- Added `poc/src/sms_runtime.rs`.
- Contains reusable emitters for:
  - VDP register writes;
  - VDP address setup;
  - VRAM/CRAM copies;
  - SMS name-table writes;
  - title cursor writes;
  - first SMB text/name-table writes.

### First Generic 6502 IR/Lifter Work

- Added `poc/src/ir6502.rs`.
- Current supported generic lifter subset:
  - `LDA #imm`;
  - `LDA zp`;
  - `STA zp`;
  - `CLC`;
  - `ADC #imm`;
  - `BNE rel`;
  - `CMP #imm`;
  - `BCS rel`;
  - `BCC rel`;
  - `RTS`.
- Unsupported opcodes fail closed instead of silently emitting bogus Z80.

Current validated real SMB slices:

- `$9CA6`: add 2 to little-endian zero-page pointer `$E7/$E8`.
- `$B1B4`: branch/store based on incoming Z flag.
- `$AEF9`: `CMP #$03` plus `BCS`, validating 6502/Z80 carry inversion.

Artifacts:

- `ir_lowering_demo.asm/bin/report/validation`;
- `ir_branch_demo.asm/bin/report/validation`;
- `ir_compare_demo.asm/bin/report/validation`.

## What Is Partially Done

### Area Rendering

We have real data extraction, object classification, and a first static
foreground overlay. We do not yet have the real live SMB area parser running in
translated Z80 or as a faithful state machine.

### Z80 Translation

We have the beginning of a generic lifter and validated tiny slices. We do not
yet have whole routine translation, call graph translation, indirect dispatch,
or a live integration path where generated Z80 game logic drives the SMS screen.

### Hardware Mapping

We have SMS-native VDP writes for the current PoC screens. We do not yet have a
general replacement layer for NES PPU register behavior, OAM DMA, scrolling,
palette updates, sprite construction, APU writes, or mapper writes.

## What Is Still Missing

### Compiler and Analysis

- Full 6502 decoder coverage.
- Addressing modes beyond the tiny subset:
  - zero-page indexed;
  - absolute;
  - absolute indexed;
  - indirect indexed;
  - accumulator and implied ops;
  - stack ops;
  - jumps, calls, returns, RTI.
- Correct explicit 6502 flag model.
- Flag liveness tracking so native Z80 flags are used only when safe.
- Function discovery / CFG walking.
- Code/data separation.
- Branch target stitching across larger routines.
- JSR/RTS call graph handling.
- Indirect jumps and jump tables.
- Integration with NESRecomp-style game profiles.

### SMB Game Logic

- Real `InitializeGame`.
- Real `LoadAreaPointer`, `FindAreaPointer`, and `GetAreaDataAddrs`.
- Live `AreaParserTaskHandler`, `AreaParserCore`, `ProcessAreaData`, and
  `DecodeAreaData`.
- Three-slot SMB area object state and `AreaObjectLength` counters.
- Real block buffer writes and column streaming.
- Enemy parser and enemy slot initialization.
- Player state, controller normalization, movement, jump physics, and animation.
- Collision with terrain, blocks, enemies, and items.
- Scrolling and camera state.
- Timer, score, lives, coins, power-ups, death/game-over/victory flow.

### SMS Rendering Runtime

- Live scrolling column updates.
- Sprite table generation from NES OAM-like game state.
- Sprite tile selection for Mario and enemies.
- Attribute/palette strategy beyond the current simple mapping.
- VRAM budget management for all level types.
- Handling SMS sprite limits and flicker strategy.

### Audio

- Completely deferred.
- Missing APU write logging.
- Missing PSG mapping.
- Missing music/sfx conversion or replacement engine.

### Validation

- Need a proper 6502 oracle for generated routine comparisons.
- Need broader Z80 emulator/state tests beyond the tiny local interpreters.
- Need visual regression screenshots for title and world screens.
- Need a second SMS emulator check, not only Mednafen.
- Need trace/CDL input from an NES emulator.

## Honest Feasibility Assessment

The work so far proves:

- the SMS output side can boot;
- real SMB graphics and level data can be extracted and mapped;
- real SMB code bytes can be lifted into IR and lowered to Z80 for small slices;
- correctness can be validated at the slice level.

It does not yet prove:

- that the whole SMB game can be automatically translated;
- that generated Z80 will fit timing/size limits;
- that NES rendering semantics can be replaced fully enough for all levels;
- that player/enemy/collision systems can be ported without substantial manual
  semantic rewrites.

## Next Meaningful Step

See [`master-plan.md`](master-plan.md) Phase 0 for the concrete next-step
list. Summary: crate split, profile schema, in-Rust 6502 oracle, in-Rust Z80
emu, WLA-DX in the docker image, explicit shadow flag model, synthetic test
ROM corpus. Slice-by-slice IR work on hand-picked SMB addresses is no longer
considered progress; routines must be reachable from the discovery walk and
validated against the in-Rust 6502 oracle.
