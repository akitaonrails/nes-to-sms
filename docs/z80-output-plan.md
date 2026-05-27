# Z80 Output Subproject Plan

> **Status:** historical PoC notes. The canonical back-end plan is in
> [`master-plan.md`](master-plan.md) Phase 2. This file documents the early
> Rust raw-byte emitter and the three IR-backed micro-slices that motivated
> the architecture shift; the raw-byte emitter is scheduled for retirement
> once WLA-DX is in the pipeline.

## Goal

Branch the PoC away from opaque byte emission and toward a real Z80 output
pipeline. The subproject should eventually accept lifted 6502 IR and emit:

- readable Z80 assembly;
- final SMS ROM bytes;
- source mappings back to NES PRG addresses;
- runtime calls for SMS VDP/input/audio shims.

The current implementation starts with the SMS boot/title/world-screen harness,
because it is already known to boot in an emulator.

## Current Implementation

Implemented under `poc/src/z80_backend.rs`.

The backend now owns:

- emitted Z80 bytes;
- assembly listing lines;
- symbolic labels;
- unresolved 16-bit patches;
- generated local labels;
- aligned data blocks.

The PoC now writes:

- `poc/out/smb/poc.sms`: packed 32 KB SMS ROM;
- `poc/out/smb/generated.asm`: assembly listing generated from the same backend;
- `poc/out/smb/z80_translation_experiment.asm`: older 6502 reset sketch, kept as
  a separate research artifact.

## Architecture Direction

### Layer 1: Z80 Program Builder

This is the current backend. It emits instructions, labels, patched references,
and data. It is intentionally assembler-like but does not require WLA-DX or
another external assembler yet.

Near-term additions:

- source-map comments;
- named sections for runtime, translated routines, and data;
- optional external-assembler syntax mode;
- ROM bank placement metadata.

### Layer 2: SMS Runtime Helpers

Move SMS-specific routines into reusable emitters:

- VDP register writes;
- VDP address setup;
- VRAM/CRAM copy loops;
- name-table entry writes;
- controller reads;
- sprite table writes;
- no-op audio stubs.

These helpers should become the target for translated NES hardware accesses.

### Layer 3: 6502 IR to Z80

The backend should not translate 6502 directly from text. The next compiler
layer should lift decoded 6502 instructions into IR, then lower IR to Z80.

Initial lowering strategy:

- keep 6502 `A`, `X`, `Y`, `S`, and flags in explicit state;
- map NES RAM `$0000-$07ff` to SMS RAM `$c000-$c7ff`;
- emit helper calls for PPU/APU/controller ranges;
- only use native Z80 flags where the IR proves it is safe.

### Layer 4: Subsystem Replacements

For SMB, the backend must support semantic replacements:

- title/menu rendering to SMS name-table writes;
- area parser output to SMS metatile columns;
- OAM construction to SMS sprite table entries;
- controller state to SMS port reads;
- audio stubs first, PSG replacement later.

## Milestones

1. Completed: replace ad hoc SMS byte pushes with `z80_backend`.
2. Completed: emit `generated.asm` and `poc.sms` from the same source.
3. Completed: move SMS helper emitters out of `main.rs` into an `sms_runtime`
   submodule.
4. Completed: define the first small 6502 IR type and lower a pure routine into this
   backend.
5. Next: validate IR output against a tiny synthetic 6502 routine and a Z80
   state oracle.
6. Later: add WLA-DX or another assembler inside Docker for external assembly
   validation.

## Validation

Current validation:

```sh
cd poc
cargo check
cargo run -- "/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes" out/smb
cd ..
DOCKER_UID=$(id -u) DOCKER_GID=$(id -g) docker compose run --rm sms-smoke
```

The Dockerized Mednafen smoke run passes with the backend-generated ROM.

The first IR-backed artifact is:

- `poc/out/smb/ir_lowering_demo.asm`
- `poc/out/smb/ir_lowering_demo.bin`
- `poc/out/smb/ir_lowering_report.txt`
- `poc/out/smb/ir_lowering_validation.txt`

It lowers actual SMB PRG bytes at `$9CA6` into Z80. The recognized behavior is
an add-2 operation on the little-endian zero-page pointer `$E7/$E8`, mapped to
SMS RAM `$C0E7/$C0E8`. The validation file executes the generated Z80 bytes in
a tiny local state oracle for cases that include carry propagation.

The second IR-backed artifact is:

- `poc/out/smb/ir_branch_demo.asm`
- `poc/out/smb/ir_branch_demo.bin`
- `poc/out/smb/ir_branch_report.txt`
- `poc/out/smb/ir_branch_validation.txt`

It lowers SMB PRG bytes at `$B1B4`, a branch/store slice that consumes an
incoming zero flag and conditionally writes `$06` to zero-page `$0E`.

Both current IR artifacts now pass through a shared supported-range lifter for
the initial opcode subset (`LDA`, `STA`, `CLC`, `ADC`, `BNE`, `RTS`) rather than
hand-built IR vectors. Unsupported opcodes intentionally fail closed.

The third IR-backed artifact is:

- `poc/out/smb/ir_compare_demo.asm`
- `poc/out/smb/ir_compare_demo.bin`
- `poc/out/smb/ir_compare_report.txt`
- `poc/out/smb/ir_compare_validation.txt`

It lowers SMB PRG bytes at `$AEF9`, covering `CMP #imm` plus `BCS`. This proves
the lifter can handle the common 6502 compare/carry branch pattern and map it
to Z80's inverted `cp` carry convention.

## Current Boundary

This is not yet the full 6502-to-Z80 compiler. The project now has the first
output backend and the first IR-backed lowering path. The next meaningful step
is to broaden that generic lifter toward indexed addressing and longer routines,
while keeping oracle cases for each lowered routine.
