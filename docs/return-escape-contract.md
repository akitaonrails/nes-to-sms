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

## Ordinary calls consumed before a branched suffix

Use `[[return_consume]]` when a PLA pair discards an ordinary JSR return
and the remaining code can branch or return with RTS. This is separate from
the bounded JMP contract above; do not invent a terminal JMP annotation.

```toml
[[return_consume]]
at = 0x8100
bank = 3
calls = [{ caller = 0xc100, target = 0x9000 }]
```

`at` identifies the first of two adjacent PLAs. Each declared call must be
an original JSR with the stated operand. A call's optional `bank` identifies
its physical caller bank, independently of the pair's bank. Fixed-window
calls omit it. Native-call profiles cannot use this contract.

The call materializes its actual `caller + 2` high/low bytes on the guest
stack and marks its existing software continuation as their owner. An
ordinary RTS cleans up those bytes. At the consuming pair, generated code
checks the live return against the finite declared set, then reuses the
atomic ownership-transfer helper before either PLA runs. Original PLAs,
suffix branches and RTS remain intact; the consumed continuation is not
returned through or popped twice.

Generation rejects stale, overlapping, missing or replaced annotations and
known entries at the second PLA. Entry after both PLAs is permitted: it
skips consumption and retains normal-return cleanup. An undeclared caller
reaching the pair traps on invalid ownership or return bytes. Isolated
routine validation cannot establish this caller-context contract; use
integrated original-code and assembled-runtime tests, including interrupt
boundaries and guest-stack wrapping.
