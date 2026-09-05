# Profiled return escapes

`[[return_escape]]` describes an absolute NES `JMP` that deliberately discards
one translated caller continuation. `caller` and `target` must match the ROM;
`return_addr` is the original JSR-pushed address, before the RTS increment.
Switchable-window annotations also require their physical `bank`.

Ordinary mode reconstructs the two return bytes at the guest stack pointer,
then follows the original jump. Use it when the destination itself consumes
those bytes. It is incompatible with native translated calls.

## A return already consumed before the jump

When the original code uses `PLA; PLA` before its escape, set
`stack_bytes_already_consumed = true` and `consume_at` to the first PLA.
The compiler validates this bounded contract:

- Both PLAs and the final absolute JMP occupy one lifted routine and the
  same PRG window. The two PLAs are consecutive original instructions.
- The suffix between the pair and JMP contains only loads or NOPs. Other
  suffixes need an explicit extension and verification, not a guessed match.
- No known vector, routine, branch, call, dispatch or replacement can enter
  after the first PLA through the final JMP, including an operand-byte entry.
- The consuming owner must actually lift and lower; missing, replaced,
  oversized or unsupported owners fail generation rather than become stubs.

Immediately before the first PLA, the runtime disables interrupts, validates
the software-frame pointer/ownership and the still-live low/high bytes at
guest `S+1`/`S+2`, then publishes exactly one software pop. The guest stack,
registers, flags, mapper state and prior interrupt state remain unchanged.
Original PLA/PLA/load/JMP instructions still run. The final JMP performs no
second pop and never reads freed stack slots: a valid interrupt may already
have reused them.

Optional `DIAG_CONSUMED_ESCAPE` increments its reserved success counter only
after that validated ownership transfer. It measures transfer before the
following deterministic PLA pair, not completion of the final jump. Disabled
diagnostics emit no instructions. The runtime and validation stubs implement
the same live-byte contract; canonical-ROM and actual assembled IRQ-boundary
tests cover the consuming path.
