# CNROM source-bus checkpoint

Verified 2026-09-08. This is a restricted, synthetic-tested foundation, **not
playable Adventure Island or complete mapper support**. The opt-in
`CNROM_BUS_EXPERIMENT` capability deliberately rejects unfinished semantics.

## Implemented contract

- NES 2.0 mapper 3 submapper 1 (no conflict) and 2 (AND conflict), fixed
  16/32 KiB PRG, power-of-two 8–128 KiB CHR-ROM, horizontal/vertical mirroring
  and optional mirrored 2 KiB cartridge RAM. Ambiguous metadata fails closed.
- Raw CPU-bus addressing and physical CHR identity. Conflict reads use the
  byte at the actual written PRG address. Buffered PPUDATA reads, palette
  aliases, CIRAM aliases and OAM DMA data/address wrapping are implemented.
- Genuine guest JSR/RTS stack behavior, including low stack pointers, wrapping,
  synthetic returns and TXS unwinding. Unknown computed destinations trap;
  native translated-call ownership is not substituted for the guest stack.
- NMOS RMW original/final writes, with compound unofficial instructions using
  their modified operand rather than rereading immutable ROM after the write.
  The 6502 reference now emits both writes itself; duplicate test shims were
  removed. This establishes access order, not cycle accuracy.
- Bus helpers preserve caller registers, shadow flags, native stack, interrupt
  enable state and exact cartridge mappings across temporary SRAM/ROM access.

## Verification

The independent source-bus fixtures do not derive expected results from the
production CNROM model. Tests include 37 assembled projects, 20 negative
admission cases and 96 actual-helper ABI cases. Additional independent probes
cover TSX flags, DMA wrapping, grayscale reads and indirect-JMP page wrapping.
Run the assembled integration cases with the existing Docker toolchain:

```sh
cargo test -p nes_to_sms --test cnrom_pipeline -- --include-ignored
```

Five actual Genesis Plus GX runs passed: 128 KiB CHR at 100% and 500%, optional
RAM, guest-stack behavior, and absent-RAM rejection. Positive runs require an
executed completion marker and exact RAM; they are not visual or audio tests.
Workspace results: 609 passed, 180 ignored. Formatting and Clippy finish
successfully with existing warnings. Fresh SMB1/CV1/SMB3 builds are byte-identical
to the preserved same-day baseline builds. All three canonical SMB acceptance
routes match the corrected 6502 reference for 4,500 frames each. Canonical ROMs
remain unchanged. Local evidence: ignored `out/cnrom-core-smoke.qgSJoU/` and
`out/adventure-baseline.TvOphd/`.

## Deliberately unfinished

Timing-dependent PPUSTATUS reads, rendering/NMI enables and undriven CPU reads
trap; BRK/RTI and incompatible profiles are rejected before output mutation.
Source-cycle accounting, guest interrupt delivery, controller/open-bus fidelity,
graphics, audio completeness and peripheral variants remain separate gates.
The existing APU shim is not evidence of complete sound support.

Next is source-time scheduling followed by immutable source-frame rendering and
the [Adventure Island full-content checks](adventure-island-reference.md).
Host SMS VBlank must not replace original NES elapsed time. Any physical display
or sound adaptation will be documented separately from source correctness.
