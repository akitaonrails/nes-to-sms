# Master Plan

This is the canonical plan for the NES-to-SMS porting compiler. It supersedes
the architectural sections of `full-translation-plan.md` and `current-status-
and-gaps.md`. Those documents remain useful for SMB subsystem phasing and for
status reporting respectively, but architecture, sequencing, and principles
live here.

## North Star

**v1 acceptance:** Super Mario Bros. World 1-1 is playable on a Master System
emulator. Mario walks, runs, jumps, dies, can be hit by Goombas and Koopas,
and the level scrolls from start to flagpole. Title, menu, init, area parser,
player, collision, enemies, and scrolling are all generated end-to-end by the
pipeline. Audio is deferred. No Rust-side rendering of game state.

**v2 acceptance:** All of SMB up to the credits screen. All area types
(ground, underground, water, castle), warp zones, Bowser, game-over and
victory flow. Still no audio.

**v3 acceptance:** PSG audio approximation; the second NROM game runs through
the same pipeline with a different profile.

The pipeline is the deliverable. SMB is the proof.

## Principles

These are non-negotiable. They exist because the previous trajectory
violated them and stalled.

1. **Pipeline-driven, not slice-driven.** A unit of work is "the pipeline can
   now cover X end-to-end," not "I lifted one more SMB micro-routine." If a
   newly lifted routine is not reachable from the reset vector through the
   same discovery walk used for every other routine, it does not count.
2. **No Rust-side hand-ports.** Do not reimplement SMB rendering or game logic
   in Rust to make `poc.sms` look more complete. Either translate the SMB
   routine through the pipeline, or label the screen a fixture and exclude it
   from "subsystem complete" claims.
3. **Generic engine, game-specific profile.** The Rust crates (decoder, IR,
   analyzer, Z80 backend, runtime emitters) stay SMB-agnostic. Every SMB fact
   — labels, RAM map, replacement annotations, jump tables, data regions —
   lives in `profiles/smb.toml` or in Z80 runtime files. No `if game == "smb"`
   in `.rs` source. No hard-coded SMB addresses outside the profile.
4. **Fail closed.** Unsupported opcodes, unknown indirect targets, untagged
   memory accesses must error, not silently emit placeholder Z80. The current
   lifter already does this; preserve it everywhere.
5. **Differential validation is mandatory.** Every translated routine is
   verified by the in-Rust 6502 oracle running the original 6502 against the
   same RAM state the Z80 sees, on the same input vectors, with full state
   compared. No more 5-instruction toy interpreters.
6. **Verbose and boring first.** Conservative Z80 with shadow flags and RAM-
   backed X/Y is the default. Native-Z80-flag and register-allocated lowering
   come after the slow version is provably correct.
7. **No host tool installs.** All retro toolchains (assembler, emulators,
   trace bridges) live in `docker/` and `compose.yaml`. Already the policy;
   keep it.
8. **No commercial ROM bytes in the repo.** Generated artifacts are gitignored.
   Profiles reference local ROM paths only.

## Architecture

```
+-----------+   +-----------+   +----------+   +----------+   +-----------+
| iNES ROM  |-->|  Analyzer |-->|   IR     |-->|  Z80 BE  |-->| WLA-DX +  |
| + profile |   |  (Rust)   |   |  (Rust)  |   |  (Rust)  |   | runtime   |
+-----------+   +-----------+   +----------+   +----------+   +-----------+
                                                                    |
                                                                    v
                                                              +----------+
                                                              |  .sms    |
                                                              +----------+
```

### Rust crates (workspace layout)

| Crate / module       | Responsibility                                          |
|----------------------|----------------------------------------------------------|
| `nes_rom`            | iNES / NES 2.0 parsing, PRG/CHR split, vector read       |
| `cpu6502`            | 2A03 decoder, all official opcodes, addressing modes     |
| `analysis`           | Function discovery, CFG, code/data classifier, memory tags |
| `profile`            | TOML profile loader, schema, validation                  |
| `ir`                 | Semantic IR with explicit 6502 flags and tagged memory   |
| `oracle_6502`        | Small 2A03 interpreter used for differential testing     |
| `z80_emit`           | Z80 instruction encoder + assembly text emitter          |
| `z80_emu`            | Small Z80 emulator (Rust) for differential testing       |
| `lower`              | IR → Z80 lowering (the back end)                         |
| `sms_project`        | WLA-DX project layout writer (sections, banks, makefile) |
| `assets`             | CHR → SMS 4bpp tile, palette planner, name-table convert |
| `cli`                | One binary; takes `rom.nes profile.toml out_dir/`        |

The current `poc/` collapses analyzer, IR, decoder, Z80 emit, and SMS runtime
into one crate. The first refactor is to split these along the table above.

### Z80-side runtime (`runtime/*.s`)

Hand-written, lives in `runtime/` and is assembled by WLA-DX alongside the
generated code. Provides:

| Module                | Provides                                              |
|-----------------------|-------------------------------------------------------|
| `boot.s`              | SMS header, VDP/CRAM/VRAM init, jump to translated reset |
| `vdp.s`               | VDP register write, VRAM/CRAM address, byte/word upload |
| `nametable.s`         | Name-table writers in SMS coordinates                |
| `sat.s`               | Sprite attribute table builder + uploader            |
| `vbuf.s`              | VRAM update buffer (the SMS analog of SMB's `VRAM_Buffer1`) |
| `vblank.s`            | VBlank handler that flushes the update buffer        |
| `input.s`             | SMS controller read + SMB Select/Start remap         |
| `flags.s`             | Shadow 6502 status byte helpers (set/clear/test C/Z/N/V) |
| `stack6502.s`         | Emulated 6502 stack at SMS `$C100-$C1FF`             |
| `apu_stub.s`          | APU write logger (no-op until audio phase)           |
| `mapper_nrom.s`       | Trivial NROM bank model                              |
| `dispatch.s`          | Indirect jump-table dispatcher                       |

These are hand-authored, not generated. They are reviewed once and reused
across games.

### SMS RAM layout

```
$C000-$C0FF   NES zero page mirror
$C100-$C1FF   Emulated 6502 stack page
$C200-$C7FF   NES RAM mirror ($0200-$07FF)
$C800-$C8FF   Legacy vbuf; CV1 instead reserves it for frame/SAT state below
$C900-$C9FF   NES-format OAM staging (64 entries: Y, tile, attr, X)
$CA00-$CAFF   BG/runtime state, including BG reverse map at $CA40-$CAFF
$CB00-$CBFF   Translated runtime state (frame counter, shadow flags, etc.)
$CC00-$D2FF   Folded SMS per-cell subpalette shadow (temporary)
$D300-$D3FB   Software translated-call continuation frames (segment 0)
$D3FC-$D3FF   PPU continuation/mode and nested slot-2 guard state
$D400-$D4BF   Sprite cache/resolution, IRQ and runtime scratch
$D4C0-$D4FF   Far slot-1 bank/continuation stack
$D500-$D5FF   Software translated-call continuation frames (segment 1)
$D600-$D9FF   BG variant cache
$DA00-$DD7F   BG base-slot shadow
$DD80-$DE3F   BG variant ring-slot nametable refcounts (192 bytes)
$DE40-$DFFB   Native Z80 stack headroom / no-go for persistent shadows
$DFFC-$DFFF   RAM aliases of Sega mapper registers ($FFFC-$FFFF)
```

The runtime initializes Z80 SP to `$DFFC`; pushes pre-decrement below the
mapper aliases. Native Z80 stack and emulated 6502 stack are separate.
`chrmap.s::BGV_REFCNT` already occupies `$DD80-$DE3F`; this is an existing
allocation, not a new reservation. Native SP must not cross below `$DE40`.

Under CV1's runtime opt-in, `$C800` remains zero (vbuf is dormant).
`$C801-$C80F` holds sprite state/optional diagnostics; `$C810-$C812` is pending
commit/address scratch; `$C820-$C83D` holds frame handoff records and accounting;
`$C83E-$C83F` is optional timing diagnostics. Prepared SAT is `$C840-$C87F`
(64 Y bytes) plus `$C880-$C8FF` (128 X/tile bytes), separate from NES OAM.
Consult `frame_cv1.s` and `sat_cv1.s` before assigning unused bytes; this is
profile-specific ownership, not a generic free/double-buffered vbuf allocation.

With `CV1_COHERENT_BG`, `$C811-$C812` instead parks guest X/Y while the full
graphics prologue prepares a packet; it cannot be reused while that packet is
pending. `$C813-$C81E` holds committed HUD controls and physical-frame epochs.
`$C81F` is reserved for an optional consumed-return diagnostic, not HUD state.
The coherent backend owns cartridge SRAM `$A800-$BC7F` as follows:

| SRAM | Owner |
| --- | --- |
| `$A800-$A8FF`, `$A900-$A9FF` | Live/frozen CIRAM dirty bitmaps |
| `$AA00-$AD7F`, `$B900-$BC7F` | Two folded nametable slot shadows |
| `$AD80-$B0FF` | Per-cell pending-record index |
| `$B100-$B2FF`, `$B700-$B8FF` | Two sets of exact 16-bit slot refcounts |
| `$B300-$B31F` | Pending slot pins |
| `$B320-$B35F`, `$B360-$B39F` | Live/frozen CHR dirty bitmaps |
| `$B3A0-$B3BF`, `$B3C0-$B3DF` | Live/frozen SMS palette |
| `$B3E0-$B3FF` | Preparation state and cursors |
| `$B400-$B45F` | Up to 32 three-byte nametable records |
| `$B460-$B465`, `$B466-$B467` | Frozen HUD controls, live/frozen work flags |
| `$B500-$B5FF`, `$B600-$B6FF` | Slot reverse base/key maps |

Raw PPU writes update source data and live intent only. Preparation may write
new pattern slots only when neither committed refcounts nor pending pins own
them. It preserves NES OAM at `$C900-$C9FF`; the prepared SAT is separate.
The IRQ publishes nametable, palette, HUD and SAT together in a bounded
blanking interval. Oversized scene changes explicitly blank and rebuild;
they never recycle displayed slots or silently truncate a packet. Safe
preparation yield points restore guest registers and close mapper/VDP state.
These allocations are CV1-specific; NROM retains its existing renderer.

Full raw NES CIRAM source-of-truth needs 2 KiB (`$CC00-$D3FF` if stored in
internal RAM), including 1920 tile bytes plus the 128 compact attribute bytes
already mirrored at `$CB80-$CBFF`. That storage does not fit in current
internal RAM without reclaiming existing folded rendering shadows. The eventual
internal-RAM candidate remains `$CC00-$D3FF`, but it is blocked until folded-S /
BG-shadow dependencies and software-continuation/guard ownership are replaced.
For v1, raw CIRAM storage is allowed to
use a generic external/cartridge-RAM backend instead of making internal-RAM
reclaim a prerequisite. This keeps the pipeline generic while breaking the
current deadlock: first prove raw-CIRAM parity in a separate storage backend,
then materialize from it, and only later optimize storage back into internal RAM
if needed.

The historical `$D300-$D3FF` dirty-bitmap proposal is not globally available:
software-stack profiles use its continuation frames, and PPU/guard state owns
the final four bytes. A trace from a native-stack profile cannot authorize
reusing it across profiles. `$DE40-$DFFB` remains reserved for native-stack
headroom even when a route leaves space unused. Any future internal CIRAM or
dirty-metadata allocation must explicitly retire or relocate the existing
owners; a folded-visible repaint is not itself a raw-CIRAM source of truth.

### SMS ROM bank plan

Bank layout assumed banked from day one even though SMB is NROM, so MMC1/MMC3
games later are not a rewrite.

```
Bank 0  ($0000-$3FFF): SMS header, runtime, VBlank handler, dispatch
Bank 1  ($4000-$7FFF): Translated code, slot 1
Bank 2+ ($8000-$BFFF): Translated code overflow + data, slot 2
```

Code-size blow-up of 3–6× is expected. SMB's 32 KB PRG may need 96–192 KB of
Z80. SMS supports up to ~4 MB cart; SMB will fit comfortably.

## Phases

Each phase has explicit exit criteria. A phase is **not** complete until the
exit criteria are demonstrated through the pipeline, not by hand.

### Phase 0 — Foundation fixes

The current PoC needs corrections before new features land. Estimated 1–2
weeks of work.

**0.1 Crate split.** Refactor `poc/` into the crate layout above. The
existing 1,756-line `main.rs` is broken up; SMB-specific code is moved into
a `profiles/smb.toml` + Z80 runtime fixtures.

**0.2 Profile schema and loader.** Adopt a TOML schema modeled on
NESRecomp's, narrowed to what SMB+SMS needs: vectors, extra function roots,
label imports, data regions, replacement-routine annotations, indirect-
dispatch hints, RAM region tags, mapper kind.

**0.3 In-Rust 6502 oracle.** Implement the 2A03 interpreter (decimal mode
absent). Cycle accuracy is **not** required; instruction-accurate flag and
memory side effects are required. This is the engine of differential
testing.

**0.4 In-Rust Z80 emulator.** Either pull a vetted crate (e.g. `z80`) or
write a minimal interpreter covering the opcodes the back end emits.
Coverage grows alongside the back end.

**0.5 External assembler in the loop.** Add WLA-DX to
`docker/Dockerfile.toolchain`. Emit `.s` files from the back end; assemble
to `.sms` via a Make-driven WLA-DX project. The Rust raw-byte emitter
remains only as a cross-check and is retired in Phase 2.

**0.6 Explicit shadow flag model.** The IR holds an explicit 6502 status
byte. Lowering writes to it after every flag-producing op. Native-Z80-flag
shortcuts are not allowed until a Phase 2 liveness pass enables them.

**0.7 Synthetic test ROM corpus.** A handful of tiny synthetic NES ROMs that
exercise: vector setup, JSR/RTS, branches, indirect jump, zero-page indexed,
absolute,X/Y, PPU register writes, OAM DMA, and the 6502 flag matrix. These
become the regression suite the engine runs against on every change.

**Exit criteria:**
- `cargo run -- synthetic/arith.nes profiles/synthetic.toml out/arith/`
  produces an SMS project that WLA-DX assembles and Mednafen boots.
- The in-Rust oracle and the in-Rust Z80 emu agree byte-for-byte on the
  synthetic corpus.
- Every SMB-specific constant has moved out of `.rs` source.

### Phase 1 — Front end completion

Estimated 3–4 weeks.

**1.1 Full 2A03 decoder.** All 151 official opcodes + the documented-stable
unofficial ones SMB uses. Addressing modes: imm, zp, zp,X, zp,Y, abs, abs,X,
abs,Y, (zp,X), (zp),Y, indirect, accumulator, implied, relative.

**1.2 Function discovery.** Vector walk → recursive descent through
direct branches/JSR/JMP → merged with profile roots → produces a CFG with
per-edge evidence (direct branch, JSR, profile root, indirect target with
profile hint). Per-byte code/data classification with confidence levels.

**1.3 Memory access classifier.** Every load/store tagged:
`{nes_ram, zero_page, stack, ppu_2000_2007, oam_dma_4014,
apu_io_4000_4017, mapper, prg_data}`. Hardware accesses become first-class
IR primitives (`ppu_write(reg, value)`), never raw stores.

**1.4 CDL ingestion (optional, defer if behind).** Accept Mesen/FCEUX
`.cdl` and intersect with discovery. Cheap to add later; not blocking.

**1.5 Disassembly export.** Emit annotated ca65-style assembly for the
discovered CFG with labels from the profile and PRG addresses in
comments. This is the inspectable artifact reviewers actually read.

**Exit criteria:**
- Discovery applied to SMB resolves the doppelganger label set with ≥95%
  coverage (track the gap as a report in `out/smb/discovery_report.txt`).
- Vectors resolve to NMI `$8082`, RESET `$8000`, IRQ `$fff0` — the
  NESRecomp-documented NROM-256 truth values.
- The annotated disassembly is hand-comparable to doppelganger's for
  picked routines.

### Phase 2 — Back end completion

Estimated 4–6 weeks.

**2.1 IR → Z80 lowering for every IR op.** Shadow flags by default. Map
6502 A→Z80 A; X→IXL or RAM-backed; Y→IYL or RAM-backed; zero page in
`$C000-$C0FF`; NES RAM mirror at `$C000-$C7FF`. Emulated 6502 stack at
`$C100-$C1FF`.

**2.2 JSR/RTS → translated continuation stack.** Generated JSRs push a
bank-qualified continuation into the runtime `TR_RET` software stack; RTS pops
and dispatches that frame without leaving a long-lived native Z80 return word.
The emulated 6502 stack normally carries explicit `PHA`/`PLA`/`PHP`/`PLP`
values and interrupt frames. A profile `[[return_escape]]` annotation bridges
the uncommon 6502 idiom where a tail-jumped callee deliberately consumes its
caller's JSR return bytes as stack data: it discards one `TR_RET` frame and
materializes the corresponding two bytes on `$C100-$C1FF`. JMP indirect routes
through the runtime `dispatch.s` table; profile data provides target facts.

**2.3 SMS runtime library finished.** All modules from the architecture
table written and tested standalone.

**2.4 Banked WLA-DX project emission.** Generated `.s` files placed into
banks; data sections separated from code. Makefile orchestrates assembly.

**2.5 Differential test harness.** For every discovered routine the
harness runs `(input vectors) → oracle vs. generated Z80 under emu`.
Failure stops the build. The synthetic corpus runs in CI on every commit.

**2.6 Flag-liveness pass (optional optimization).** Only when correctness
is proven, add a pass that detects when the next consumer of a flag is
close enough to use native Z80 flags directly, eliding shadow flag
writes. Off by default.

**Exit criteria:**
- All synthetic corpus ROMs translate cleanly, oracle-match, and boot.
- A representative SMB routine (e.g. `LoadAreaPointer`) translates,
  oracle-matches over a battery of input area pointers, and the
  generated Z80 assembles under WLA-DX.

### Phase 3 — Hardware semantic layer

Estimated 4–6 weeks.

**3.1 PPU writes → VRAM update buffer.** SMB queues PPU writes through
`VRAM_Buffer1/2` and flushes in NMI. The translation preserves that
structure: the queue lives at `$C800-$C8FF`; the SMS `vblank.s` flushes
it to the VDP via the control/data ports; NES name-table addresses
convert to SMS name-table addresses at flush time. Direct `$2006/$2007`
writes from translated code route through the same buffer.

**3.2 OAM construction → SAT.** SMB writes a 256-byte OAM staging
area then issues OAM DMA via `$4014`. Translate the staging writes
literally; replace the DMA with an SMS SAT upload (Y-positions then
H/V/tile). NES 8x8 entries map 1:1; 8x16 splits into two SMS sprites.

**3.3 Scrolling.** SMB writes `$2005` twice per frame. Map to SMS VDP
scroll registers + column streaming for incoming columns.

**3.4 Palette planner.** NES master palette → SMS 6-bit BBGGRR. One
static mapping per area type for v1; per-screen planning later.

**3.5 Controller normalization.** SMB Select/Start remap: SMS button 1
= NES Select, SMS button 2 = NES Start. Pause button = NES Start
alternative if the user prefers.

**3.6 APU stub.** Writes logged to a ring buffer; no PSG output until
audio phase. Translated music engine code runs but produces silence.

**Exit criteria:**
- A test program that does only PPU writes through the SMB pattern
  produces the expected SMS name-table state under Mednafen and at least
  one other SMS emulator (Emulicious).
- OAM staging + `$4014` DMA pattern in a synthetic ROM produces the
  expected SMS SAT state.

### Phase 4 — SMB drive-through

Estimated 8–12 weeks, the long phase. Each milestone is a real subsystem
running through the pipeline.

**4.1 Title.** `DrawTitleScreen`, `UpdateScreen`, title menu cursor,
mushroom icon — translated. The current Rust-side hand-port is retired.

**4.2 Init + area pointers.** `InitializeGame`, `LoadAreaPointer`,
`FindAreaPointer`, `GetAreaDataAddrs`. RESET → idle → translated game
init reaches "World 1-1 selected" state.

**4.3 Area parser.** `AreaParserTaskHandler`, `AreaParserCore`,
`ProcessAreaData`, `DecodeAreaData`. World 1-1 first screen renders from
translated code, no Rust pre-rendering. Three SMB object slots, page
markers, row 12-15 specials all handled.

**4.4 Player rendering.** `PlayerGfxHandler`, sprite tile selection,
OAM staging. Mario appears at the start position.

**4.5 Player movement.** Controller normalization, `PlayerCtrlRoutine`,
movement physics in slices. Left/right/jump changes position and
animation. Collision still stubbed.

**4.6 Collision and blocks.** Block buffer collision, question block /
brick bump, coin tile updates. Mario stands on the ground and collides
with blocks.

**4.7 Enemies.** Enemy parser, slot init, Goomba/Koopa movement, sprite
rendering, player-enemy collision. First Goomba in 1-1 appears, walks,
hurts Mario.

**4.8 Scrolling and full 1-1.** Horizontal scroll state, column
streaming from area parser, flagpole, end-of-level. World 1-1 is
playable from start to flagpole.

**v1 SHIPS HERE.**

### Phase 5 — Full SMB and second target (v2/v3)

**5.1** Underground / water / castle area types. Warp zones, vines,
elevators, Bowser, victory/game-over flow.
**5.2** PSG audio. APU log replay → PSG events. Triangle and DMC
approximated or replaced. Pulse channels map cleanly.
**5.3** Second NROM game (Balloon Fight or Ice Climber). Different
profile, same engine. Forces removal of any latent SMB assumptions.

## Validation strategy (layered)

Two oracles, used at different points.

### Layer 1 — In-Rust 6502 oracle vs. in-Rust Z80 emu

The default oracle. Fast, deterministic, runs on every commit. For each
discovered routine:

1. Generate N random input vectors (A, X, Y, flags, relevant RAM bytes).
2. Run the 6502 interpreter on the original PRG bytes with that initial
   state.
3. Run the generated Z80 in the in-Rust Z80 emu with the same initial
   state laid out in SMS RAM.
4. Compare final A, X, Y, flags, and touched RAM. Any divergence fails.

### Layer 2 — Mesen trace bridge

The end-to-end oracle. Slower, run on subsystem completion and on
suspected regressions.

1. Drive Mesen via its MCP/Lua scaffolding (NESRecomp already provides
   `mesen_mcp.lua` + `mesen_mcp_server.py`; containerize them).
2. Capture per-frame state from real SMB execution on a recorded input
   script.
3. Run the SMS build under Mednafen / Emulicious with the same input
   script applied through `input.s`.
4. Compare RAM mirrors and OAM/sprite-attribute state at a coarse frame
   cadence. Visual screenshots compared by perceptual hash.

Layer 2 catches what layer 1 cannot: PPU/NMI/scroll timing interactions
and inter-routine bugs.

## Red flags — how to know we're off track

If any of these are true, stop adding features and fix the trajectory:

- A new SMB constant appears in `.rs` source outside the profile loader.
- A lifted SMB routine validates against anything other than the in-Rust
  6502 oracle.
- A "subsystem complete" claim is backed by Rust-side rendering instead
  of translated Z80.
- The synthetic test corpus is not passing on `main`.
- The discovery walk doesn't reach a routine but it's being translated
  anyway.
- Native Z80 flags are being used before the liveness pass exists.
- The Rust raw-byte emitter is still in use after Phase 2 exits.

## Known risks

- **SMS VRAM budget.** 16 KB VRAM ≈ 16 KB of SMS 4bpp tiles + name table
  + SAT. SMB's 8 KB NES CHR expands to 16 KB SMS — fills the whole VRAM.
  Background and sprite tiles cannot both stay resident in 1-1's tileset
  without overlap. Plan a CHR streaming/swapping strategy in Phase 2.
- **Code-size blow-up.** Conservative Z80 with shadow flags is 3–6× the
  6502 byte count. SMB → 96–192 KB Z80. Banked from day one.
- **Frame timing.** SMS VBlank is shorter than NES. Large VRAM update
  queues need a split-across-frames fallback in `vbuf.s`.
- **DMC.** No SMS equivalent. v3 audio either skips it or substitutes
  PSG noise.
- **Self-modifying / RAM-executed code.** SMB doesn't do this; if a later
  target does, the interpreter-fallback in `dispatch.s` covers it slowly.

## Progress snapshot

**2026-09-04: Phase S (speed recovery) first session.** After studying
lackoftrack27's hand-made SMB SMS port (docs/handport-comparison.md),
implemented Tier-1 generic codegen upgrades (native CALL/RET stack
discipline, carry-threaded shift runs, inline memory shifts, fill-loop
lifting, indexed-EA cleanups) and Tier-2 profile replacements (3 hooks in
runtime/hooks_smb.s). SMB steady frame: 261.6K → 178.9K cycles (4.38× →
3.00× over budget), byte-parity on all three acceptance routes, 479
tests green. Plan + results: docs/speed-recovery-plan.md.

**2026-07-04: v1 COMPLETE.** Byte-for-byte NES parity on three full
routes (1-1 clear, death/game-over, bonus-pipe with the underground
coin room), all four audio channels (Phase 5.2 done early via the
APU->PSG register shim), 100% PRG classification, 0 unresolved labels,
input working in stock Mednafen, 375 tests. Remaining from Phase 5:
5.1 (world 1-2+ visuals / raw-CIRAM) and 5.3 (second NROM target).
New Phase H planned: predictable Z80 optimizer to close the ~8x frame
budget overrun (docs/optimizer-plan.md).

Earlier snapshot, as of 2026-05-20, the pipeline runs end-to-end on `Super Mario Bros.
(World).nes` and produces a 64 KiB `.sms` that WLA-DX assembles cleanly
and Mednafen loads as a Sega Master System ROM. Test suite: 280 passing
across 11 Rust crates (10,615 LOC). Z80 runtime: 2,395 lines across 10
`.s` files.

Concretely:

| Phase | Status |
|-------|--------|
| 0.1 Crate split + workspace             | done (12 crates) |
| 0.2 Profile schema + loader             | done; `profiles/smb.toml` covers vectors + 22 functions |
| 0.3 In-Rust 6502 oracle                 | done (46 tests, full official + stable unofficial) |
| 0.4 In-Rust Z80 emulator                | done (79 tests, covers every opcode the back end emits) |
| 0.5 WLA-DX in docker                    | done; rebuild via `docker compose build poc` |
| 0.6 Shadow flag model in IR/lower       | done |
| 0.7 Synthetic test ROM corpus           | partial (3 in-memory ROMs as integration tests) |
| 1.1 Full 2A03 decoder                   | done (151 official + stable unofficial; unstable fail closed) |
| 1.2 Function discovery                  | done (vector walk + JSR + profile roots; range-trim heuristic in cli) |
| 1.3 Memory access classifier            | done (zp / stack / ram / mirror / ppu / oam / apu / mapper / prg) |
| 1.4 CDL ingestion                       | deferred |
| 1.5 Annotated disassembly export        | done (`generated/translated.asm` + `reports/lifted.txt`) |
| 2.1 IR → Z80 lowering                   | done with shadow flags |
| 2.2 JSR/RTS → CALL/RET                  | done |
| 2.3 SMS runtime library                 | scaffolded; flag/ALU/stack helpers complete; PPU/SAT partial; controller is a stub |
| 2.4 Banked WLA-DX project emission      | done; single-include monolithic build |
| 2.5 Differential test harness           | partial (oracle + emu both exist; auto-diff loop over discovered routines not yet wired) |
| 2.6 Flag-liveness optimization          | deferred |
| 3.1 PPU writes → VRAM update buffer     | runtime stubs in place; not driven by translated code yet |
| 3.2 OAM construction → SAT              | runtime stubs in place; not driven yet |
| 3.3 Scrolling                           | not started |
| 3.4 Palette planner                     | placeholder grayscale only |
| 3.5 Controller normalization            | stub only |
| 3.6 APU stub                            | done (ring-buffer log) |
| 4.x SMB drive-through                   | not started |
| 5.x Full SMB / audio / second game      | not started |

The lowering crate fails closed on 6 SMB routines that contain stable
unofficial opcodes (SLO, RLA, DCP, SAX) or that the discovery walk
landed inside data (ANE, AHX, JAM $12). The pipeline records these in
`reports/lower_failures.txt` and continues; the failed routines emit no
body, and callers land on the `L_XXXX` alias label with no
instructions, then fall through. Adding stable-unofficial lowering and
tightening discovery on data is the natural next batch of work.

## Reconciliation with other docs

- `docs/full-translation-plan.md` — kept for SMB subsystem phasing notes,
  but its architectural assertions defer to this file.
- `docs/current-status-and-gaps.md` — status report only, not a plan.
  Update it when state changes; do not put architecture decisions there.
- `docs/6502-to-z80-automation.md` — research background; principles
  here override its sketches where they conflict.
- `docs/nesrecomp-reuse-analysis.md` — research background; profile
  schema shape is borrowed from here.
- `docs/translation-feasibility.md` — research background, unchanged.
- `docs/poc-plan.md` and `docs/poc-findings.md` — historical PoC notes,
  not a forward plan.

When in doubt, this file wins.
