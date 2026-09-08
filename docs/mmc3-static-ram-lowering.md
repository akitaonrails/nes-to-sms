# Direct internal-RAM lowering for full MMC3

Accepted implementation, 2026-09-08; independent correctness and evidence
review passes.
This follows the accepted [ROM dispatch index](mmc3-dispatch-index.md).

## Change and limits

Full-mode translation now folds constant NES internal-RAM addresses into their
native SMS addresses at build time: `$C000 | (address & $07FF)`. Zero-page
constants and absolute constants below `$2000` use direct loads/stores, avoiding
effective-address preservation and runtime bus-helper calls.

One private access selector feeds the existing 22-opcode implementation.
Indexed, indirect, PPU/APU/controller, cartridge RAM and PRG accesses retain
their old paths. Flags, resident X/Y, emulated stack behavior and full-mode
optimization restrictions are unchanged. Read-modify-write operations still
write the old value and then the final value, preserving their address across
flag calculations. No runtime assembly, public option, new cache or RAM/VRAM
allocation is introduced. Generated code shrinks by 25,068 bytes; the output
ROM remains 2 MiB.

## Exact primitive costs

Assembled old/new sequences confirm these nominal Z80 savings:

| Operation on a constant internal-RAM address | Saved T-states |
| --- | ---: |
| Simple load | 105 |
| Simple store | 126 |
| Read-modify-write | 315 |

These include address setup and helper CALL/RET costs, not just the replacement
instruction. They are not whole-game speedup percentages.

## Evidence and input-timing qualification

The explicitly full-mode differential harness compares against the original
6502 interpreter, with no unresolved-helper RET stubs. It exercises mirrors,
all 22 operations, every status-byte value and RMW edge cases. The interpreter
is instruction-accurate, not bus-cycle-accurate: NMOS old/final write ordering
is therefore checked separately against that source contract. Assembled tests
add exact timing and real Pause/host-IRQ injection with stack/mapper canaries.

The input-only full route completes level entry, mushroom pickup, goal and
cleared-map return at `500`, with no gameplay blank frames or strict video-port
violations. However, the faster ROM misses a different idle observation during
level-entry settling. The unchanged test policy consequently begins movement
at update 514 rather than 511. Later checkpoint memory differs, so this route
does **not** establish matched-workload speed or cross-ROM checkpoint parity.
The old fixed-update early/later profiling attempts fail and remain recorded.

A separate controlled benchmark preserves the title/map prefix, waits with
neutral input until a fixed actual idle boundary, then holds one button set.
Its full starting guest/cartridge memory and all common completed states must
match between ROMs. The first rightward workload dies identically in both ROMs
after 81 observed epochs; it is useful bounded correctness evidence, not a
completed timing result. The final fallback holds Mario at the left boundary.
Both ROMs complete it with identical starting memory, all 212 commonly observed
completed states, and all 211 logical-window image sets. Installed/profile core
streams and every saved snapshot also match within each ROM.

| Fixed left-boundary workload at `500` | ROM-index baseline | Direct RAM |
| --- | ---: | ---: |
| Mean nominal T/update, 200 complete intervals | 2,502,151 | 2,484,861 |
| Physical frames for 200 updates | 1,600 | 1,590 |
| Observed updates/second | 7.490 | 7.537 |

This establishes a **0.63% stationary-gameplay throughput gain**, not moving
traversal speed. Do not combine it with the earlier 16.48% traversal result.
The instruction ledger explains the small gain: bus/inline categories fall by
about 170,000 T/update, but cooperative-wait categories grow by about 153,000.
In this workload, much of the saved CPU time becomes synchronization waiting.
More ROM or fewer instructions alone does not remove that delay.

Final checks pass: 12,928 differential vectors, 4,224 assembled old/new/oracle
states, 3,573 actual interrupt injections, 18 pipeline fixtures, and 587 workspace
tests (171 ignored). Fresh same-day SMB1/CV1 baseline/current ROM pairs are
byte-identical. The full `500` observer records 2,958,205 events with zero strict
violations/fallbacks/gameplay blank frames and exact installed-core saved parity.
The bounded `100` smoke has 33 safe deadline fallback blanks and no strict
violations; stock-speed playability and one startup-caller uncertainty remain
outside the established result.

Frozen candidate SHA-256:
`f625150854dc1c34f8e81cc172460f6af367cf56aa913528db29b1dc90db199b`.
Ignored evidence: `out/smb3-static-ram.CfW3w0/`,
`out/smb3-static-tests.pznfj8/`, `out/smb3-static-ram-measure.aMMMrG/`.
