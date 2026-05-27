# 6502 to Z80 Automation Research

## Executive Summary

True automation is possible only if we define "translation" as static
recompilation plus a target runtime, not as opcode-for-opcode assembly
conversion.

For this project, the viable automation path is:

1. Lift 6502 code into a semantic IR.
2. Preserve CPU state explicitly: registers, flags, stack, PC, cycles where
   needed, and memory side effects.
3. Classify memory accesses as RAM, ROM, PPU/APU/I/O, mapper, or unknown.
4. Emit Z80 for pure CPU logic.
5. Replace NES hardware accesses with SMS-native runtime calls.
6. Use game profiles, traces, and known disassemblies to fill static-analysis
   gaps.

Direct text conversion from 6502 assembly to Z80 assembly is too weak. The two
CPUs have different register models, flags, addressing modes, stack behavior,
interrupt behavior, and performance characteristics.

## Relevant Prior Art

### Jamulator

Andrew Kelley's Jamulator statically recompiled NES games with LLVM. It proved
that static recompilation can produce native output, but its own conclusion was
that full static recompilation is not broadly practical for emulation because a
game cannot be fully disassembled without executing it, and because CPU, PPU,
APU, and interrupts run as interacting systems.

Useful lesson for us: use static recompilation for controlled ports, not as a
universal emulator replacement.

### NESRecomp

NESRecomp is more directly relevant and current. It describes itself as a static
6502 recompiler framework for NES games. It translates 6502 machine code to C,
uses game configuration, discovers functions, supports mapper/runtime code, and
ships a Super Mario Bros. recompilation that is reported as fully playable.

Useful lessons for us:

- Use a game configuration file, not only blind ROM scanning.
- Discover functions from vectors, graph walking, pointer-table scanning, and
  explicit extra functions.
- Maintain a runner/runtime for PPU, APU, mapper, input, timing, and debugging.
- Treat undocumented opcodes as a first-class decode problem.
- Validate against an emulator oracle.

NESRecomp targets C/native PC, not SMS Z80, but its architecture is a better
model for our automation than a raw 6502-to-Z80 assembler transliterator.

### N64Recomp Pattern

N64Recomp documents a useful static-recompiler shape: split binaries into
functions using metadata, translate instructions literally into target code,
turn calls into function calls when possible, and provide a runtime for platform
behavior. The CPU is different, but the engineering pattern applies.

### Radare2 ESIL and Ghidra

Radare2 ESIL is an example of lifting instructions into a CPU-independent
semantic form. Ghidra supports both 6502 and Z80 processor modules. These tools
are useful references for instruction semantics, control-flow discovery, and
validation, though neither gives us a finished NES-to-SMS translator.

## Proposed Automation Architecture

### 1. Decoder

Decode every 6502 byte into:

- opcode mnemonic;
- addressing mode;
- instruction length;
- cycle baseline;
- official/unofficial classification;
- read/write/branch/call/return metadata.

For NES, use 2A03 semantics: decimal mode is absent, but undocumented NMOS 6502
opcodes may still appear.

### 2. Code Discovery

Use multiple sources:

- RESET/NMI/IRQ vectors;
- recursive descent through branches, `JSR`, `JMP`, `RTS`, `RTI`;
- pointer-table detection;
- trace/CDL logs from emulator runs;
- labels imported from public disassemblies;
- per-game profile overrides.

Never assume all bytes in PRG are code.

### 3. Semantic IR

Lift each instruction into IR operations such as:

- `A = read8(addr)`;
- `write8(addr, A)`;
- `tmp = A + value + C`;
- `set_flag(C, tmp > 0xff)`;
- `branch_if(Z == 0, label)`;
- `call(label)`;
- `return()`.

The IR should expose both data flow and hardware side effects. A CPU store to
`$2006` is not merely `mem[0x2006] = A`; it is a PPU address-latch operation.

### 4. Flag Model

Do not rely on Z80 flags accidentally matching 6502 flags.

The IR should model 6502 flags explicitly:

- `C`: carry/borrow;
- `Z`: zero;
- `I`: interrupt disable;
- `D`: ignored on NES 2A03 but still present in status;
- `B`: stack/status artifact;
- `V`: signed overflow;
- `N`: sign bit.

The Z80 backend can choose either:

- explicit RAM/register-backed emulated flags for correctness; or
- native Z80 flags only when a local proof shows they are equivalent until the
  next flag consumer.

Start with explicit flags. Optimize later.

### 5. Memory Model

Classify every access:

- `$0000-$07FF`: NES RAM mirrors;
- `$0100-$01FF`: 6502 stack page;
- zero page variables;
- `$2000-$2007`: PPU registers;
- `$4000-$4017`: APU, input, DMA;
- `$4020-$FFFF`: cartridge space, mapper registers, PRG ROM/RAM.

The Z80 backend should not emit raw stores for hardware ranges. It should emit
runtime calls such as:

```asm
call nes_ppu_write_2006
call nes_apu_write_4000
call mapper_write
```

Later, those runtime calls can become SMS-native semantic equivalents rather
than NES emulation.

### 6. Z80 Backend

Conservative register mapping:

| 6502 state | Z80 strategy |
| --- | --- |
| `A` | Z80 `A` when local, or RAM-backed when live across calls |
| `X` | `B`, `IXL`, or RAM-backed |
| `Y` | `C`, `IYL`, or RAM-backed |
| `SP` | emulated 6502 stack pointer byte plus SMS stack for calls |
| `PC` | labels/gotos for static code, explicit value for indirect dispatch |
| flags | explicit shadow status byte first |
| zero page | fixed SMS RAM block, e.g. `$C000-$C0FF` |
| NES RAM | fixed SMS RAM block, e.g. `$C000-$C7FF` if space allows |

The backend should emit verbose, boring Z80 first. Size and speed optimization
come after behavior matches.

### 7. Runtime Shims

Required shims:

- NES RAM mirrors;
- stack push/pop helpers if not inlined;
- indirect jump/call dispatcher;
- PPU write semantic layer;
- SMS VDP renderer;
- controller mapping;
- mapper/bank state;
- APU-to-PSG placeholder;
- NMI/frame scheduler.

For SMS output, the runtime should gradually stop pretending to be a NES and
instead implement the equivalent game-level effect on SMS hardware.

## Translation Levels

### Level 0: Interpreter Fallback

Keep a tiny 6502 interpreter in Z80 or Rust-generated support code for unknown
or hard routines. This is slow on SMS and probably not final-shippable, but it
can unblock discovery.

### Level 1: Literal Recompile

Translate every discovered 6502 instruction into Z80 sequences that update
emulated state exactly. Hardware accesses call NES-like shims.

This is the best first automation target.

### Level 2: Semantic Port

Recognize common NES rendering/audio/input routines and replace them with SMS
equivalents. Example: SMB area parser produces metatile columns; SMS backend
writes Mode 4 name-table columns.

This is the level needed for a real playable SMS port.

### Level 3: Optimized Native Z80

Prove local register/flag lifetimes and emit cleaner Z80. This is optional
until the ROM exceeds size/performance limits.

## Not Realistically Automatable Yet

These are not permanent impossibilities, but each needs separate research before
we claim automation.

### Code/Data Separation

Problem: PRG intermixes instructions, tables, compressed data, text, and
graphics pointers. Any byte can decode as a plausible 6502 instruction.

Research later:

- Trace/CDL ingestion.
- Disassembly import format.
- Confidence scoring for code regions.
- Validation against emulator coverage.

### Indirect Control Flow

Problem: `JMP (addr)`, jump tables, state machines, inline pointer tables, and
computed dispatch hide real targets from simple recursive descent.

Research later:

- Pointer-table scanner.
- Zero-page pointer value propagation.
- Profile syntax for inline dispatch.
- Runtime dispatch fallback.

### Mid-Instruction Entry and Overlapping Code

Problem: some 6502 programs intentionally branch into the middle of an
instruction or use bytes as both code and data.

Research later:

- Multi-entry basic blocks.
- Byte-level CFG instead of instruction-only CFG.
- Whether SMB relies on this in gameplay paths.

### Self-Modifying or RAM-Executed Code

Problem: code can be copied to RAM/SRAM and executed, or modified before
execution.

Research later:

- RAM execution detection from traces.
- Static mapping from ROM source bytes to RAM execution ranges.
- Interpreter fallback for unknown RAM code.

### Mapper Semantics

Problem: bank switching changes which code/data a CPU address refers to.

Research later:

- Mapper 0 exact support.
- MMC1/MMC3 bank-state propagation.
- SMS mapper layout constraints.
- Per-routine bank context annotations.

### Exact Flag Equivalence

Problem: 6502 and Z80 flags do not line up exactly. Overflow, borrow, decimal,
half-carry, parity/overflow, and undocumented flag behavior differ.

Research later:

- Flag-liveness analysis.
- Native-Z80 flag substitution rules.
- Per-instruction equivalence tests.
- Shadow-flag optimization.

### Cycle and Interrupt Timing

Problem: NES logic may depend on NMI timing, sprite DMA stalls, controller
polling timing, raster effects, or mapper IRQ timing.

Research later:

- Frame scheduler model.
- Per-routine cycle budgets.
- SMS line interrupt mapping.
- Fast path vs cycle-accurate fallback.

### PPU-to-VDP Semantics

Problem: NES PPU writes do not map 1:1 to SMS VDP writes. Name tables,
attributes, palettes, sprites, scrolling, and update timing differ.

Research later:

- Semantic PPU command capture.
- Nametable-to-SMS tilemap conversion.
- Attribute/palette mapping.
- Sprite OAM-to-SMS SAT mapping.
- Scrolling and column streaming.

### APU-to-PSG Audio

Problem: NES APU channels and SMS PSG channels differ. DMC has no direct SMS
equivalent.

Research later:

- APU write logger.
- Pulse-to-tone conversion.
- Triangle approximation.
- Noise channel mapping.
- Music-engine replacement vs dynamic conversion.

### Undefined and Undocumented Opcodes

Problem: NES games may use unofficial opcodes. Some are stable, some are
dangerous, and some halt the CPU.

Research later:

- Full 2A03 opcode semantics table.
- Emit semantics for used unofficial opcodes.
- Reject or trap unstable opcodes.

### ROM Size and Performance

Problem: literal Z80 output can be much larger and slower than original 6502.
SMS ROM banking can help size but not every access pattern ports cleanly.

Research later:

- Code size estimates by routine.
- Peephole optimizer.
- Register allocation.
- Bank layout planner.
- Hot-path profiling in emulator.

### Legal and Asset Boundaries

Problem: generated output must not embed copyrighted ROM bytes in committed
source artifacts.

Research later:

- Patch-based generation.
- User-owned ROM extraction at build time.
- Generated artifact ignore policy.

## Recommended Next Experiment

Build a minimal 6502-to-IR-to-Z80 compiler for isolated SMB routines:

1. Select a pure RAM/control routine with no PPU/APU writes.
2. Decode from PRG by CPU address.
3. Lift to explicit-state IR.
4. Emit conservative Z80 with shadow flags and RAM-backed `X/Y`.
5. Run both the original 6502 routine in a tiny interpreter and emitted Z80 in a
   Z80 emulator over the same test vectors.
6. Only then try a routine that touches PPU state.

Good candidates:

- controller normalization;
- simple pointer arithmetic;
- menu cursor state;
- area pointer resolution.

Avoid first:

- NMI;
- PPU update buffers;
- sprite DMA;
- music engine;
- collision or enemy code with many indirect dispatches.

## Sources

- Andrew Kelley, "Statically Recompiling NES Games into Native Executables with
  LLVM and Go": <https://andrewkelley.me/post/jamulator.html>
- NESRecomp repository: <https://github.com/mstan/nesrecomp>
- SuperMarioBrosNESRecomp repository:
  <https://github.com/mstan/SuperMarioBrosNESRecomp>
- N64Recomp repository: <https://github.com/N64Recomp/N64Recomp>
- Retrocomputing Stack Exchange, "Is it possible to make a ROM converter?":
  <https://retrocomputing.stackexchange.com/questions/8121/is-it-possible-to-make-a-rom-converter>
- NESdev forum, "Static recompiling experiments":
  <https://forums.nesdev.org/viewtopic.php?t=24864>
- NESdev Wiki, 2A03: <https://www.nesdev.org/wiki/2A03>
- NESdev Wiki, CPU unofficial opcodes:
  <https://www.nesdev.org/wiki/CPU_unofficial_opcodes>
- Radare2 Book, ESIL: <https://book.rada.re/emulation/esil.html>
- Zilog Z80 CPU User Manual: <https://www.zilog.com/docs/z80/z80cpu_um.pdf>
