# Hand-port comparison: lackoftrack27's SMB SMS port vs. this pipeline

Date: 2026-09-04. Source studied: https://github.com/lackoftrack27/Super-Mario-Bros.-SMS
(v1.00, WLA-DX, ~43k lines of Z80 source; doppelganger's SMBDIS, MrWint's PAL
disassembly, and TakuikaNinja's smb1-bugfix credited "used as reference").

## Why this document exists

The hand port is a **routine-by-routine port of the same disassembly our
pipeline translates** — same routine names (`AreaParserTaskHandler`,
`ProcessAreaData`, `SpriteShuffler`, `OperModeExecutionTree`), same RAM
variable names, same timers/PRNG, and the **same NES-format level and enemy
object data parsed at runtime**. It plays essentially identically to the NES
original, and it fits the SMS frame budget with room to spare (~90% utilization,
VBlank scanline-budgeted in source comments).

That settles a question our optimizer work (docs/optimizer-plan.md,
docs/optimization-findings.md) could only bound from one side: **SMB itself
fits the SMS. Our ~6× frame overrun (~330–410K cycles vs. the 59,736-cycle
budget) is 100% translation overhead** — not the game, not the hardware, and
not (primarily) the flag model we spent the optimizer phase on.

## The five multipliers

The port is fast because of five representation changes, none of which is a
peephole optimization. Ranked by cycle impact:

### 1. The object array is transposed into RAM pages (dominant)

SMB's inner loops are `LDA Enemy_X_Position,X` — the 6502's cheapest idiom and
the Z80's most expensive. The port carves RAM into 32 pages of 256 bytes, one
per object slot, with identical field offsets in every page. The 6502's `X`
register **becomes register H** and the field name **becomes L**:

```asm
    LD L, <Enemy_X_Position   ; 7 T
    LD A, (HL)                ; 7 T   — total 14 T, zero arithmetic
```

`LD L, <field` appears 839 times. Iterating objects is `INC H`. Our equivalent
(`ld hl,base / ld a,d / ld b,a / call rt_read_indexed`) costs ~70 T per read,
and indexed stores recompute the address from scratch (~100+ T for a
read-modify-write the port does in 14 T). Same NES timer loop, side by side:

```asm
; hand port (~46 T/iteration)     ; ours today (~200 T/iteration)
DecTimersLoop:                    ld hl,$C780
    LD A, (DE)                    ld a,d / ld b,a
    OR A                          call rt_read_indexed
    JR Z, SkipExpTimer            or a / jp z,L_8116
    DEC A                         push af / ld hl,$C780
    LD (DE), A                    ld a,d / ld c,a / ld b,$00
SkipExpTimer:                     add hl,bc / pop af
    DEC E                         dec (hl)
    DJNZ DecTimersLoop            dec d / jp p,L_810E
```

This transposition requires knowing "X here is always an object index in
[0,6]" — an invariant that exists in the programmer's head, not in the
instruction stream. It is the part a static translator cannot discover.

### 2. Flags are audited, not emulated

Zero flag-emulation machinery anywhere. The author found the ~10 sites in 43k
lines where SMB deliberately exploits 6502 carry quirks (`ADC` without `CLC`,
inverted `SBC` borrow) and fixed each with a folded constant
(`ADD A,$19 ; +1 due to carry being set`) or a single `CCF`. Everything else
runs native Z80 flags. This *validates our own measurement*: when we removed
30% of shadow-flag helper calls, cycles moved ~0.5%. Flags were never the mass.

### 3. Calls are native

`JSR` is `CALL` (17 T). `JumpEngine` is a 1-byte `RST $28` doing the NES
original's pop-return-address trick on the native stack, using `EXX` to
preserve HL. Our JSR emits ~50 instructions (~400–500 T) of segmented,
bank-qualified continuation-stack bookkeeping — generality SMB never uses.
SMB performs hundreds of JSR/RTS per frame.

### 4. The NES PPU is deleted at build time, not emulated at runtime

Our runtime emulates PPU registers (guarded `rt_ppu_write` on every
`$2000-$2007` access), shadows CIRAM, folds attribute tables into
sub-palettes, and bit-mirrors CHR data at runtime to synthesize flipped
sprite tiles. The port's stored data is **already in destination format**:

- Metatile tables hold finished SMS nametable *words* with flip/palette/
  priority bits baked in by the assembler (`.dw $07C4` = H+V-flipped tile).
  The attribute table does not exist anywhere in the program.
- The RAM sprite table is byte-identical to the hardware SAT
  (`Sprite_Y_Position` 64 bytes, then X/tile pairs); upload is 192 unrolled
  `OUTI`s (~3.1K T). No per-frame conversion pass.
- The stripe buffer (`VRAM_Buffer1`) holds literal VDP control-port bytes
  plus an entry offset into a page-aligned unrolled OUTI block reached by
  `JP (IX)` — variable-length copy with zero loop overhead.
- Sprite flipping is pre-baked assets: enemies carry a second, mirrored frame
  table 12 bytes away; the player has an **entire mirrored ROM bank per
  facing direction** (and pre-recolored copies for fire/star palettes). ROM
  is spent to buy CPU.
- CHR animation (coin/? block cycling) is VRAM tile streaming from ROM, not
  bank switching; player animation streams 256 bytes of tiles per change,
  mutually exclusive with BG tile streaming (one `SBC HL,DE` picks which).

### 5. The frame is architected for graceful overrun

Game logic runs in the main loop; the interrupt performs a *fixed,
scanline-budgeted* set of VRAM writes (the author annotates costs in source:
`;[CPU TIME: 10 LINES]`). `FrameDoneFlag` turns an overrun into a clean lag
frame — one skipped VDP update, never corruption. The NES sprite-0-hit status
bar split becomes an SMS line interrupt at line 7. The NES original demands
NMI completion every frame; we inherited that demand, which is why we need
the emulator overclock.

Other techniques worth recording: 197 data sections declared `BITWINDOW 8`
(page-crossing-free tables) licensing an 11 T 8-bit `ADD A,L / LD L,A`
pointer add in 203 places; `ALIGN $100` lookup tables replacing arithmetic
(512-byte block-buffer address LUT); `IXH/IXL/IYH/IYL` as four extra 8-bit
scratch registers standing in for zero-page temporaries; SP-as-memset
(`.REPEAT $20 / PUSH HL` clears the sprite table at 5.5 T/byte); `OUTI`
chosen over `OTIR` in hot paths (16 vs 21 T/byte); 23 `.REPEAT`-unrolled
loops; no self-modifying code.

## What we misjudged

1. **We fought the wrong bottleneck first.** The optimizer phase attacked the
   flag representation; measured yield ~1% of cycles. The cost ranking the
   port demonstrates is: data representation ≫ calling convention ≫
   hardware-layer emulation ≫ flags.
2. **We paid CPU for what ROM can buy.** Runtime sprite flipping, runtime
   attribute folding, runtime variant caches — the port precomputes all of it
   into a 256KB+ cart. SMS carts go to 4MB; build-time asset expansion was
   treated as an afterthought.
3. **We kept generality SMB never needed.** Bank-qualified continuation
   stacks, return-escape bridging, IFF2-snapshot guards on every PPU write —
   built for the hard cases, paid on every call and hardware access of an
   NROM game.
4. **The "2× clock headroom" premise was false** (already established in
   docs/optimization-findings.md: ~3.5 MHz Z80 ≈ 1 MHz 6502 for general
   code). The port does not disprove this; it routes around it by not
   emitting "general code."

## What this means for the pipeline

Three tiers of fixes, ranked by automation level (the implementation plan
lives in docs/speed-recovery-plan.md):

- **Tier 1 (fully automatic, generic):** inline indexed addressing with
  single effective-address computation; native CALL/RET under a profile
  `stack_discipline` assertion (same-bank direct + slot-0 far-call shim);
  cheap NROM PPU-write paths; build-time pre-flipped sprite tile variants
  uploaded by `OTIR` instead of runtime bit-mirroring; unrolled OUTI
  SAT/vbuf upload paths.
- **Tier 2 (per-game profile + runtime, sanctioned by the existing
  `[[replacement]]` mechanism):** replace SMB's *presentation* routines
  (VRAM stripe flush, scroll application) with SMS-native runtime modules
  operating on the same translated RAM state — the hand port is the
  reference implementation for what these should look like.
- **Tier 3 (research-grade):** profile-annotated RAM relayout (the array
  transposition). Requires whole-program closure proof that every access to
  the region is through the annotated bases. May land as a feasibility
  report rather than a win.

**Realistic ceiling:** Tier 1+2 plausibly takes ~340K → 100–150K
cycles/frame (~2–2.5× over budget → full speed at ~200% emulator overclock,
or ~25–30 fps with lag-frame tolerance). Native 60 fps stays out of reach
without Tier 3, because the residual is the expression of the game logic
itself.

## Strategic reframe

The project's unique asset is the **differential oracle**, not the translator.
The hand port took one skilled author a very long time with no verification
tooling beyond playtesting. Our pipeline can serve as: *automatic translation
as a correct, slow scaffold + oracle-verified incremental replacement of hot
routines with native Z80*. That is the workflow the porting community's
"manual reimplementation is the only way" consensus actually needs tooling
for — machine-verified correctness with human-quality speed where it counts.
It also argues for staying on NROM-class targets: mapper generality (CV1)
multiplies exactly the overheads that hurt most.
