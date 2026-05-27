# PoC Worklog

> **Status:** historical log of the slice-by-slice PoC trajectory (steps 1–12).
> That trajectory is no longer the active strategy; see
> [`master-plan.md`](master-plan.md). Future entries should record
> pipeline-level milestones (e.g. "Phase 0.3 — 6502 oracle lands"), not
> further hand-picked SMB micro-slices.

This log records concrete reverse-engineering and porting steps. It should be
updated whenever the PoC changes behavior.

## Step 1: ROM Split and Header Classification

- Parsed the local `Super Mario Bros. (World).nes` image.
- Confirmed mapper 0/NROM, 32 KB PRG, 8 KB CHR, no trainer, no battery.
- Extracted `prg.bin`, `chr.bin`, vectors, and a memory-map report.

## Step 2: SMS Boot Harness

- Generated a 32 KB SMS ROM directly from Rust.
- Added SMS header at `$7FF0`, export/32 KB marker `$4C`, and checksum.
- Initialized SMS VDP Mode 4, CRAM, VRAM, name table, and sprite table.
- Added Mednafen smoke and visual Compose services.

## Step 3: Real SMB Title-Screen Reconstruction

- Used the SMB disassembly to identify `DrawTitleScreen`.
- Found that it reads a VRAM update buffer from CHR/PPU `$1EC0`.
- Implemented the SMB `UpdateScreen` buffer command decoder in Rust.
- Reconstructed the NES title-screen nametable and attribute table.
- Converted the reconstructed nametable into SMS Mode 4 name table entries.
- Replaced the original random/sequential tile display with this real SMB title
  data.

Artifacts:

- `poc/out/smb/title_decode.txt`
- `poc/out/smb/title_nametable.bin`
- `poc/out/smb/title_sms_nametable.bin`
- `poc/out/smb/title_menu_port.asm`

## Step 4: Real Title Menu Cursor Slice

- Used SMB's `GameMenuRoutine` and `DrawMushroomIcon` as the next target.
- Mapped SMS button 1 to the NES Select action because SMS controllers do not
  have a Select button.
- Implemented a Z80 title-loop slice that reads SMS controller port `$DC`.
- Implemented debounced button detection in SMS RAM.
- Ported the real `DrawMushroomIcon` behavior:
  - one-player cursor writes tile `$CE` at NES nametable `$2249`;
  - two-player cursor moves tile `$CE` down one row;
  - blank tile `$24` clears the other cursor slots.
- Converted those NES nametable writes to SMS name table writes at runtime.

Result:

- Button 1 toggles the title-screen mushroom cursor between one-player and
  two-player positions.

## Step 5: First Start Transition Slice

- Used SMB's `GameText::WorldLivesDisplay` data as the first visible Start path.
- Mapped SMS button 2 to the NES Start action because SMS controllers do not
  have a Start button.
- Initially implemented runtime SMS VDP writes for these real SMB text commands:
  - `$21CD`, length 7: lives/cross placeholder;
  - `$214B`, length 9: `WORLD  - ` placeholder.
- Replaced that placeholder transition with a real static World 1-1 base screen
  generated from SMB terrain/scenery data.

Result:

- Button 2 swaps from the title screen to a World 1-1 base screen.
- This is still not full `InitializeGame`, `LoadAreaPointer`, or object/enemy
  parsing, but it is generated from the real World 1-1 header and terrain tables.

## Step 6: World 1-1 Data Extraction

- Verified World 1-1 uses `AreaPointer=$25`.
- Resolved that pointer to:
  - `L_GroundArea6` for area objects;
  - `E_GroundArea6` for enemies.
- Corrected an earlier note that had accidentally used `L_GroundArea1`, which
  is level 3-3 in the disassembly.
- Decoded the real header bytes `$50 $21`:
  - ground area;
  - `BackgroundColorCtrl=0`;
  - `PlayerEntranceCtrl=2`;
  - `GameTimerSetting=1`;
  - `TerrainControl=1`;
  - `BackgroundScenery=2`.
- Added `poc/out/smb/world_1_1_data.txt` to record the source byte streams and
  rough first-pass object/enemy field splits.
- Added `poc/out/smb/world_1_1_base_sms_nametable.bin`, generated from:
  - `BackSceneryData`;
  - `BackSceneryMetatiles`;
  - `TerrainRenderBits`;
  - `TerrainMetatiles`;
  - SMB metatile graphics tables.

Current limitation:

- The base screen includes background scenery and ground terrain.
- It does not yet apply `ProcessAreaData` objects, blocks, pipes, coins, enemies,
  player sprites, scrolling, or collision.
- The next object layer must follow `DecodeAreaData` rather than simple byte
  splitting. The parser handles page markers, row 12-15 special cases, object
  length buffers, and delayed multi-column objects.

## Step 7: DecodeAreaData Classification Trace

- Added `poc/out/smb/world_1_1_area_trace.txt`.
- Stopped treating `$FE` as a standalone page marker; in SMB area data it can be
  an ordinary row-14 object byte.
- Classified World 1-1 area objects through the same categories used by
  `DecodeAreaData`:
  - large objects;
  - small objects;
  - special rows 12, 13, 14, and 15;
  - row-13 page-control commands.
- Tracked d7 page-select events so the trace shows page, column, row, handler,
  and length/data nibble for each object.
- Added a first enemy stream trace. Enemy parsing still needs the real
  `ProcessEnemyData` state machine before it should drive gameplay.

## Step 8: First Real World 1-1 Object Overlay

- Added `poc/out/smb/world_1_1_initial_screen_sms_nametable.bin`.
- The Start transition now loads a page-1 World 1-1 screen with real foreground
  objects over the scenery/terrain base.
- The overlay is generated by a small active-object column renderer:
  - objects start at their decoded page/column;
  - row objects render for `len + 1` columns;
  - pipes render as two-column objects;
  - question blocks render as one-column objects.
- Implemented the first semantic render subset from the area trace:
  - `QuestionBlock(power-up)`;
  - `QuestionBlock(coin)`;
  - `RowOfBricks`;
  - `VerticalPipe(decoration)`.
- Added `poc/out/smb/world_1_1_object_overlay.txt` to list which page-1 objects
  are rendered and which are deferred.
- Verified the generated `poc.sms` still boots under the Dockerized Mednafen SMS
  smoke service.

Current limitation:

- This overlay is still Rust-side pre-rendering for the first static screen.
- It has a local active-object model, but it does not yet implement the exact
  three SMB object slots or integrate with scrolling SMS VDP updates.

## Current Boundary

The PoC now uses real SMB title rendering data, title-menu behavior slices, real
World 1-1 area/enemy streams, a `DecodeAreaData` classification trace, and a
first parser-driven foreground object overlay.

## Step 9: Z80 Output Backend

- Added `poc/src/z80_backend.rs` as the first Z80-output subproject.
- Replaced the SMS boot/title/world-screen ad hoc byte-push path with a
  label-aware backend.
- The backend now emits both:
  - final Z80 machine code used in `poc.sms`;
  - a readable assembly listing in `poc/out/smb/generated.asm`.
- Implemented symbolic labels, local generated labels, aligned data blocks, and
  16-bit label patching.
- Kept the SMS ROM packer responsible for the `TMR SEGA` header and checksum.
- Verified the regenerated `poc.sms` with the Dockerized Mednafen SMS smoke
  service.

Current limitation:

- This is the Z80 output layer, not the full 6502 IR/lifter yet.
- `z80_translation_experiment.asm` remains a sketch, separate from the new
  backend-generated `generated.asm`.

## Step 10: SMS Runtime Split and First IR Lowering

- Added `poc/src/sms_runtime.rs`.
- Moved VDP register writes, VDP address setup, VRAM/CRAM copy loops, title
  cursor writes, and SMB text/name-table writes out of `main.rs`.
- Added `poc/src/ir6502.rs` as the first semantic 6502 IR module.
- Grounded the first IR demo in actual SMB PRG bytes at `$9CA6`:
  - `A5 E7`: `LDA $E7`;
  - `18`: `CLC`;
  - `69 02`: `ADC #$02`;
  - `85 E7`: `STA $E7`;
  - `A5 E8`: `LDA $E8`;
  - `69 00`: `ADC #$00`;
  - `85 E8`: `STA $E8`;
  - `60`: `RTS`.
- Recognized behavior: add 2 to little-endian zero-page pointer `$E7/$E8`.
- Lowered that IR to real Z80 bytes using SMS RAM `$C0E7/$C0E8`.
- Generated:
  - `poc/out/smb/ir_lowering_demo.asm`;
  - `poc/out/smb/ir_lowering_demo.bin`;
  - `poc/out/smb/ir_lowering_report.txt`;
  - `poc/out/smb/ir_lowering_validation.txt`.
- Added a tiny local Z80 state oracle for the generated 18-byte routine.
- Validated pointer inputs `$0000`, `$00FE`, `$12FF`, and `$FFFF`, including
  carry propagation from `$E7` to `$E8`.

Current limitation:

- The first IR lowering intentionally uses native Z80 carry across the local
  `ADC` chain. That is valid for this slice, but the broader compiler still
  needs explicit flag liveness rules or shadow flags.
- The IR currently covers only the instructions needed for this slice.

## Step 11: Branch-Aware IR Slice

- Added a second IR lowering demo from actual SMB PRG bytes at `$B1B4`.
- Added a generic supported-range lifter for the first 6502 opcode subset:
  `LDA #imm`, `LDA zp`, `STA zp`, `CLC`, `ADC #imm`, `BNE rel`, and `RTS`.
- Reworked the `$9CA6` pointer increment and `$B1B4` branch/store artifacts to
  go through this shared lifter instead of hand-built IR vectors.
- Source bytes:
  - `D0 04`: `BNE $B1BA`;
  - `A9 06`: `LDA #$06`;
  - `85 0E`: `STA $0E`;
  - `60`: `RTS`.
- Recognized behavior: if incoming Z is clear, branch around the store;
  otherwise write `$06` to zero-page `$0E`.
- Lowered `BNE` to Z80 `jp nz,label` and mapped `$0E` to SMS RAM `$C00E`.
- Generated:
  - `poc/out/smb/ir_branch_demo.asm`;
  - `poc/out/smb/ir_branch_demo.bin`;
  - `poc/out/smb/ir_branch_report.txt`;
  - `poc/out/smb/ir_branch_validation.txt`.
- Added a local branch-state oracle for the generated 9-byte routine.
- Validated both incoming-flag paths:
  - incoming Z set falls through and stores `$06`;
  - incoming Z clear branches and preserves the old `$0E` value.

Current limitation:

- This branch slice consumes an incoming flag from its caller/context. Whole
  routine translation must track flag producers and consumers explicitly.
- The generic lifter is still deliberately narrow; unsupported opcodes fail
  closed instead of producing placeholder Z80.

## Step 12: Compare and Carry-Branch IR Slice

- Broadened the generic lifter to cover:
  - `CMP #imm`;
  - `BCS rel`;
  - `BCC rel`.
- Added a third IR lowering demo from actual SMB PRG bytes at `$AEF9`.
- Source bytes:
  - `C9 03`: `CMP #$03`;
  - `B0 01`: `BCS $AEFE`;
  - `60`: `RTS`.
- Recognized behavior: branch to external `$AEFE` when `A >= $03`.
- Lowered 6502 `CMP`/`BCS` using Z80 `cp`/`jp nc`, because Z80 carry after
  `cp` is set for `A < imm`, the inverse of the 6502 carry meaning after `CMP`.
- Generated:
  - `poc/out/smb/ir_compare_demo.asm`;
  - `poc/out/smb/ir_compare_demo.bin`;
  - `poc/out/smb/ir_compare_report.txt`;
  - `poc/out/smb/ir_compare_validation.txt`.
- Validated the generated Z80 with `A=$00`, `$02`, `$03`, and `$80`.

Current limitation:

- The `$AEFE` branch target is outside this small slice, so the generated label
  is a validation stub. Full translation must resolve and lower the branch
  target body instead of stubbing it.

## Next Steps

1. Broaden the generic lifter to cover indexed addressing and register-indexed
   zero-page/absolute loads.
2. Use the state oracle pattern for each newly lowered routine slice.
3. Implement a small `ProcessAreaData` state model with three object slots and
   `AreaObjectLength` counters.
4. Render World 1-1 columns page-by-page from that state model instead of
   selecting page 1 objects directly.
5. Add the first enemy parser milestone for Goomba placement.
6. Add Mario sprite placement using SMB's real player sprite tiles and OAM
   positions.
7. Start moving stable Rust-side logic into generated Z80 routines where
   feasible.
