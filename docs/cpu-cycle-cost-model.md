# 6502 → Z80 cycle-cost model

Reference for future optimization, not an implementation proposal or a promise
of 2× performance. Numbers below are nominal instruction timings with no wait
states, DMA stalls or interrupt service. Use NMOS 6502/2A03 behavior, **not
65C02 tables**. The NES disables decimal arithmetic, although its D flag remains
observable. [NES CPU reference](https://www.nesdev.org/wiki/CPU).

## Normalize clocks before comparing instructions

An NTSC NES CPU runs at approximately **1.789773 MHz**; an NTSC SMS Z80 runs
at approximately **3.579545 MHz**. Thus one NES cycle affords approximately
**two Z80 T-states**, not one. A candidate taking `T` Z80 clocks to replace
`C` NES cycles has nominal time ratio `T / (2C)`. For example, 13 T replacing
four NES cycles is **1.625× the original time**, before translation overhead.
[NES clock derivation](https://www.nesdev.org/wiki/CPU),
[Sega service-manual clock specification](https://www.smspower.org/Development/SegaMasterSystemIIServiceManual).

Core option `500` is emulation overclocking, not SMS hardware. Nominal `T/5`
is only an approximation: the pinned core applies integer cycle scaling, and
interrupts, video deadlines and game progress still matter. Do not convert
nominal savings directly into measured updates/second. PAL needs a separate
clock and frame model.

## Representative native costs

The Z80 column is a **candidate primitive**, not an equivalent translation.
`rX` means an ordinary Z80 byte register already holding NES X; `HL` already
contains a correctly mapped effective address. Those preconditions are not free.

| NES operation | NES cycles | Z80 primitive | T-states |
| --- | ---: | --- | ---: |
| `LDA #n` | 2 | `LD A,n` | 7 |
| `LDA zp` / `STA zp` | 3 | `LD A,(HL)` / `LD (HL),A` | 7 |
| `LDA abs` / `STA abs` | 4 | `LD A,(nn)` / `LD (nn),A` | 13 |
| `LDA abs,X/Y` | 4; 5 on page carry | resolved `(HL)`; alternatively `(IX+d)` | 7; 19 |
| `STA abs,X/Y` | 5, including page carry | same address-prepared forms | 7; 19 |
| `INX` | 2 | `INC rX` | 4 |
| `TXA` | 2 | `LD A,rX` | 4 |
| `ADC #n` / `SBC #n` | 2 | `ADC A,n` / `SBC A,n` | 7 |
| conditional branch | 2 not taken; 3 taken; 4 taken across page | `JP cc,nn`; `JR cc,d` | 10; 7 not taken / 12 taken |
| `JSR` / `RTS` | 6 / 6 | `CALL nn` / `RET` | 17 / 10 |

Sources: MOS Technology's [NMOS programming manual, 6500-50A](https://www.bitsavers.org/components/mosTechnology/6500-50A_MCS6500pgmManJan76.pdf),
[NES instruction tables](https://www.nesdev.org/wiki/Instruction_reference), and
Zilog [UM0080, instruction descriptions/timing tables](https://www.zilog.com/docs/z80/um0080.pdf).

The Z80 has no dedicated zero-page addressing. Loading `HL` costs another
10 T; computing an indexed address, enforcing 8-bit zero-page wrapping and
mapping NES RAM can cost more. `IX+d` uses a signed constant displacement,
not an automatically equivalent NES X/Y index. Branch page crossing compares
the target with the PC after the branch operand; JR also has displacement and
condition restrictions.

For an eligible conditional branch taken with probability `p`, JR averages
`7+5p` T versus JP's 10 T: JR is faster below 60% taken, equal at 60%, and
slower above it. JR saves one byte but requires a reachable displacement and
a supported condition. Use measured edge frequencies, not a blanket substitution.

## Semantics that prevent one-op substitution

Loads and transfers set NES N/Z; Z80 `LD` does not. `INC r` preserves carry but
changes other native flags, including P/V; NES INX preserves V. ADC's native
sign/zero/overflow/carry results are useful only with proven input carry and
flag-layout conversion. SBC needs inverted carry-in/out conventions: NES carry
means no borrow, Z80 carry means borrow. Z80 N means subtraction, not NES sign.
Guest interrupt-visible P must remain correct, even if a following branch
could use native flags directly.

Likewise, native CALL/RET do not supply the observable NES stack page, RTS's
stored-PC-plus-one convention, rewritten returns or bank ownership. Count the
actual supported stack discipline, not merely 17+10 T.

Every NES bus cycle matters where devices observe it: indexed dummy reads,
RMW old-value then final-value writes, mapper conflicts, PPU/controller side
effects and DMA cannot become ordinary RAM accesses. CPU-cycle mapper IRQs
must follow source progress, not translated elapsed time.
[MOS hardware manual, Appendix A transcription](https://xotmatrix.com/6502/6502-single-cycle-execution.html).

## Worked copy-loop comparison

For `1 <= n <= 255`, ordinary non-overlapping RAM, no source-read page crossings,
and a same-page taken branch:

```asm
    LDX #0          ; 2
loop:
    LDA src,X       ; 4
    STA dst,X       ; 5
    INX             ; 2
    CPX #n          ; 2
    BNE loop        ; 3 taken, 2 final
```

Our derivation: `2 + 13n + 3(n−1) + 2 = 16n+1` NES cycles,
or `32n+2` nominal SMS T-states of time; code size is 13 bytes.

For a native forward copy, `LD HL,src; LD DE,dst; LD BC,n` costs 30 T;
LDIR costs `21(n−1)+16 = 21n−5`, totaling **21n+25 T**, 11 code bytes.
At `n=128`, that is 2713 versus 4098 normalized T-states. At `n=1`, however,
46 exceeds 34. Never enter LDIR with BC=0 expecting a zero-length operation.
[Zilog LDIR description, printed pages 132–133](https://www.zilog.com/docs/z80/um0080.pdf).

This is a memory-result comparison only. The NES loop leaves X=n, A=last byte,
Z=C=1, N=0 and V unchanged; LDIR instead advances HL/DE, exhausts BC and has
different A/flag effects. Fixups, register ownership and interrupt visibility
must be proven and priced. Bank boundaries, aliases and MMIO invalidate these
simple premises.

## Apply the model to this compiler

SMB3 full-bus lowering currently disables several existing fusions and loop
plans; their presence elsewhere is not evidence they run here. The current
[compiler census](smb3-performance-strategy.md#important-compiler-difference-from-smb1)
finds 528,926 executions of the common **55-T shadow-N/Z sequence**, totaling
**3.38%** of its measured workload—not all of it removable.

Evaluate each change as a vector: **elapsed T, emitted bytes, stack depth,
bank-register writes and maximum interrupt-disabled time**. Price setup,
guards, misses, flag reconciliation and restoration. Prove source/runtime
parity, then measure complete workloads and presentation deadlines. Reducing
opcode count alone is neither a correctness proof nor a speed result.
