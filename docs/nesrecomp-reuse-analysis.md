# NESRecomp Reuse Analysis

## Snapshot

NESRecomp was cloned into `third_party/nesrecomp` at commit `3e921c5`
(`2026-05-18`). It is an active static NES recompiler that translates 6502 PRG
code into C and links that output against a native NES runner.

Its README describes the core model clearly:

```text
NES ROM + game.toml -> generated C -> native executable + NES runner
```

That is close to what this project needs architecturally, but not close enough
to use as a direct SMS ROM generator.

## What NESRecomp Already Solves

### ROM Extraction Discipline

`third_party/nesrecomp/EXTRACTION.md` is directly useful. It calls out a common
failure mode: loading PRG banks at the wrong base address creates analysis that
looks valid but returns wrong vectors and cross references.

For SMB specifically, NESRecomp documents the correct NROM-256 model:

- PRG is one contiguous 32 KB region mapped at `$8000-$ffff`;
- bank 0 covers `$8000-$bfff`;
- bank 1 covers `$c000-$ffff`;
- vectors should resolve to NMI `$8082`, RESET `$8000`, IRQ `$fff0`.

This should become an analyzer validation check. If our Rust tool parses an SMB
ROM and does not report those vectors, the ROM path, header handling, or address
mapping is wrong.

### Game Profile Model

NESRecomp uses `game.toml` to describe information static analysis cannot infer
reliably. Relevant concepts:

- mapper bank-switch routines;
- inline dispatch routines;
- inline pointer calls;
- manually supplied extra functions and labels;
- known tables and split tables;
- trampoline patterns;
- RAM read hooks;
- replacement functions;
- data regions;
- function merge/replace rules;
- stack-bail and conditional-bail rules.

This is the right shape for our project. A serious NES-to-SMS pipeline should
use a game profile, not pretend that arbitrary ROM bytes can always be
classified automatically.

### Function Discovery

The key file is `third_party/nesrecomp/recompiler/src/function_finder.c`.

NESRecomp starts from vectors and then uses:

- recursive control-flow walking;
- direct `JSR`/branch discovery;
- pointer-table scanning;
- known-table profile entries;
- split-table profile entries;
- mapper-aware bank reasoning;
- manual extra functions and labels;
- secondary-entry classification.

This is much stronger than a basic recursive disassembler and should directly
inform our Rust analyzer. The important lesson is that function discovery needs
evidence and confidence levels. NESRecomp's own `SUMMARY.md` notes that overly
aggressive discovery produced many false roots in Zelda, so our pipeline should
track why each code entry was accepted.

### 6502 Decode and Semantics

Useful files:

- `recompiler/src/cpu6502_decoder.h`
- `recompiler/src/code_generator.c`

NESRecomp has a complete opcode table with addressing modes, instruction sizes,
cycles, official opcodes, and some undocumented instructions such as `LAX`,
`SAX`, and read-style NOPs.

Its C emitter is useful as an executable specification of instruction behavior:

- loads update `N`/`Z`;
- arithmetic computes carry, overflow, sign, and zero explicitly;
- stores go through runtime memory helpers;
- branches become labels/gotos;
- calls can become direct function calls or dynamic dispatch;
- `JMP (addr)` accounts for the NMOS indirect-jump page-wrap bug;
- instruction boundaries call `maybe_trigger_vblank(cycles)`.

For us, this should become an IR lift, not direct Z80 text conversion.

### Pattern Library

`third_party/nesrecomp/PATTERNS.md` is focused on Faxanadu, but the lesson is
general: commercial NES games use calling patterns that are not represented by
ordinary `JSR`/`RTS` structure. Examples include inline-parameter trampolines,
RTS-as-dispatch, return-address manipulation, and split pointer tables.

Our project needs a pattern layer in the profile format. Even if SMB is simpler
than MMC1 games, adding this layer early prevents the analyzer from hard-coding
"normal subroutine" assumptions that will fail on the next game.

### Runtime Boundary

Useful files:

- `runner/include/nes_runtime.h`
- `runner/src/runtime.c`
- `runner/src/mapper.c`
- `runner/src/ppu_renderer.c`

NESRecomp separates translated CPU logic from platform behavior through helper
functions such as `nes_read`, `nes_write`, mapper functions, controller state,
PPU state, and frame/NMI scheduling. That boundary is exactly where this
project must diverge: the SMS backend cannot keep NES PPU behavior as-is if the
goal is a real `.sms` ROM.

### Test and Trace Strategy

NESRecomp includes TypeScript tests under `third_party/nesrecomp/tests/` that
build synthetic ROMs and inspect generated dispatch output. This is a useful
pattern for our own tests:

- generate tiny ROMs in tests;
- assert discovered functions and labels;
- assert generated backend output shape;
- avoid relying on commercial ROM files in automated tests.

Its `tools/` directory is also relevant:

- `build_symbols_from_lbl.py` converts ca65/ld65 labels into NESRecomp symbols;
- `parse_trace.py` parses recompiler traces;
- `mesen_mcp.lua` and `mesen_mcp_server.py` bridge emulator state for analysis;
- `check_ppu.py` inspects PPU state over TCP.

The equivalent for this project should be trace-assisted validation: Mesen/FCEUX
or another NES oracle for original behavior, then Mednafen/Emulicious or another
SMS oracle for generated behavior.

NESRecomp's internal audit documents are valuable but must be rechecked against
source before copying conclusions. For example, `COVERAGE.md` discusses earlier
gaps around `JMP (addr)` page wrapping and `BRK`, while the current source now
contains `nes_read16_jmpbug` and an explicit BRK policy hook.

## What Does Not Carry Over Directly

### C Output Is Not a Z80 Backend

NESRecomp emits C with native function calls, gotos, switches, helper calls,
global state, SDL-backed rendering, debug hooks, and host-memory arrays. A
Master System ROM needs Z80 assembly or linkable Z80 object code. The SMS cannot
run a C-style NES runner unless we build a very slow NES emulator in Z80, which
is not a useful target for Super Mario Bros.

### NES Hardware Simulation Is the Wrong Final Runtime

NESRecomp's runner simulates NES memory, PPU, APU, mapper, controllers, and
frame timing. For SMS output, hardware calls must become SMS-native behavior:

- NES nametable/palette writes must become VDP tilemap/palette writes.
- NES OAM updates must become SMS sprite attribute table updates.
- Controller reads must map to SMS controller ports.
- APU writes must eventually become PSG writes or music-engine replacement.
- Mapper writes must map to SMS bank registers only when semantically valid.

For the current project, this means `nes_write($2006)` cannot simply become a
Z80 store. It must become either a recognized rendering operation or a call into
an SMS renderer shim.

### Timing and Interrupts Need Reinterpretation

NESRecomp can call `maybe_trigger_vblank(cycles)` at instruction boundaries
because the native runner owns the whole simulation. SMS code executes on real
Z80 timing with real VDP interrupts. A direct translation would need either:

- a heavy cycle scheduler, likely too slow; or
- semantic replacement of frame tasks, NMI routines, scroll updates, and raster
  behavior.

For SMB/NROM this is manageable because the target game does not require complex
MMC IRQ behavior, but it still needs a frame-task rewrite.

## Recommended Architecture

### 1. Vendor NESRecomp as Reference, Not as the Core Binary

Keep `third_party/nesrecomp` as research input. Do not install its dependencies
on the host. If we build or run it, do so through project-local Docker or
Compose.

The cloned repository has an uninitialized `runner/nestopia-core` submodule, and
the current commit points that submodule at a private fork. That is another
reason not to make the SMS effort depend on building NESRecomp's full native
runner immediately. The recompiler source, docs, tests, and profile ideas are
usable without that submodule.

Near-term use:

- inspect algorithms;
- compare opcode semantics;
- run proposal generation if we can containerize it;
- reuse its game-profile concepts;
- compare generated C against our IR for correctness.

Do not make our project depend on editing NESRecomp's generated C.

### 2. Import the Front-End Shape Into Rust

Build a Rust analyzer with these modules:

| Module | Responsibility |
| --- | --- |
| `nes::rom` | iNES/NES 2.0 parsing, PRG/CHR banks, mapper metadata |
| `cpu6502::decode` | opcode table, addressing modes, instruction sizes |
| `analysis::discovery` | vector walk, CFG, function candidates, evidence |
| `profile` | TOML game profile compatible with NESRecomp concepts |
| `ir` | semantic CPU operations and typed memory effects |
| `sms::backend` | Z80 emission, SMS runtime calls, asset output |
| `validation` | compare 6502 routine behavior against generated Z80 |

NESRecomp's profile schema is a good starting point, but we should not clone it
blindly. We need SMS-specific profile entries such as "replace this NES PPU
routine with this SMS VDP routine" and "this RAM region is a metatile buffer".

### 3. Create a Semantic IR Before Z80

The core lift should turn one 6502 instruction into explicit operations:

```text
tmp = A + read8(addr) + C
C = tmp > 0xff
V = overflow_add(A, value, tmp)
A = tmp & 0xff
Z = A == 0
N = bit7(A)
```

This IR must preserve:

- registers `A`, `X`, `Y`, `S`;
- 6502 flags, including overflow;
- zero-page and stack behavior;
- absolute memory reads/writes;
- hardware register accesses;
- branch/call/return targets;
- cycle boundaries only where needed for frame behavior.

Direct 6502-to-Z80 text conversion should stay out of the critical path.

### 4. Emit Conservative Z80 First

The first backend should prioritize correctness over size and speed:

- keep 6502 `A`, `X`, `Y`, `S`, and flags in SMS RAM or known shadow registers;
- map NES RAM `$0000-$07ff` into SMS work RAM;
- use helper routines for complex flag math;
- dispatch indirect calls through generated tables;
- emit labels tied to original PRG addresses;
- call SMS runtime shims for hardware effects.

This will be verbose, but it gives us a baseline for automated validation. Later
passes can allocate hot 6502 values into Z80 registers and peephole repeated
patterns.

### 5. Replace Hardware at the Routine Level

For SMB, the winning path is not to emulate the PPU. It is to translate game
logic and replace rendering/audio/input boundaries.

Examples:

- `AreaParserCore` should produce SMS metatile/tilemap updates.
- Player and enemy OAM construction should produce SMS sprite entries.
- Title/menu input should use SMS controller state.
- Scroll state should drive SMS VDP horizontal scroll and column streaming.
- Audio can remain stubbed until gameplay works.

NESRecomp's runtime boundary validates this approach, but the SMS runtime must
be a new target runtime.

## Super Mario Bros. Plan Using NESRecomp Ideas

### Step 1: Pull in the SMB Game Profile

NESRecomp's README points to `SuperMarioBrosNESRecomp` as the fully playable SMB
project. That repository should be cloned separately under `third_party/` for
its `game.toml`, labels, replacement hooks, and validation knowledge.

Expected use:

- seed function discovery;
- identify data regions;
- compare against our existing SMB disassembly-derived notes;
- avoid rediscovering known inline dispatch behavior.

### Step 2: Generate a Function Map

Use NESRecomp's discovery model as the benchmark, then produce our own report:

- vector entries;
- discovered functions;
- data ranges;
- unresolved indirect targets;
- hardware-touching routines;
- pure CPU routines;
- routines recommended for semantic replacement.

For SMB, the first high-value categories are:

- title/menu control;
- initialization and level pointer selection;
- area parser;
- object system;
- player movement;
- collision;
- enemy slots;
- OAM/sprite construction;
- sound engine, deferred.

### Step 3: Validate Pure CPU Routines

Pick routines with limited hardware coupling first:

- area pointer selection;
- level header decoding;
- controller normalization;
- simple object state transitions;
- score/timer arithmetic.

Run them through three paths:

1. original 6502 semantics;
2. lifted IR interpreter;
3. generated Z80 under a Z80 emulator or SMS emulator harness.

Only after these match should we expand into rendering and frame flow.

### Step 4: Build SMS Runtime Shims

Minimum required runtime for a playable SMB path:

- RAM mirror helpers;
- 6502 stack helpers or direct stack emulation;
- flag math helpers;
- controller read normalization;
- VDP tile upload;
- VDP tilemap writes;
- sprite table writer;
- frame interrupt scheduler;
- bank helpers, initially trivial for NROM;
- audio no-op stubs.

### Step 5: Translate by Subsystem

The project should proceed in this order:

1. title/menu;
2. initialization and area pointers;
3. area parser and background column renderer;
4. player movement;
5. collision and block interactions;
6. enemy object system;
7. scrolling and full World 1-1;
8. remaining level types;
9. audio.

Each subsystem should leave an artifact in `poc/out/smb/` and a worklog entry in
`docs/poc-worklog.md`.

## Feasibility Judgment

Repurposing NESRecomp to emit SMS-compatible Z80 is feasible only if
"repurpose" means reusing its research, profile model, discovery algorithms, and
instruction semantics.

It is not realistic as a simple backend swap from "emit C" to "emit Z80" because
the hardest parts are not syntax. The hard parts are:

- proving code/data boundaries;
- resolving indirect control flow;
- preserving 6502 flags on Z80;
- replacing NES PPU/APU/input semantics;
- fitting runtime and translated code into SMS RAM/ROM limits;
- validating behavior without depending on the original commercial ROM at
  runtime.

For this repository, the best path is:

1. keep NESRecomp as a local reference under `third_party/`;
2. clone the SMB NESRecomp project next for game-specific metadata;
3. implement a Rust analyzer/IR inspired by NESRecomp;
4. emit conservative Z80 plus SMS runtime calls;
5. progressively replace NES hardware routines with SMS-native routines;
6. use emulator-driven validation at every subsystem boundary.

This keeps automation realistic: not a magic ROM converter, but a repeatable
porting compiler that can become increasingly automated for games with strong
profiles.
