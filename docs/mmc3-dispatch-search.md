# MMC3 full-runtime dispatch search

Full MMC3 dispatch uses a page-local binary lower bound before the existing
bank-matching scan. Other runtime modes retain their original emitted code
and tables. This accelerates shared dynamic calls and returns without
specializing a target to a potentially stale mapper bank.

## Table and selection contract

The six-byte records remain address-sorted:
`NES address (2), NES bank (1), SMS bank (1), SMS address (2)`.
The existing 128-pointer high-byte directory and final zero-address terminator
are unchanged. Full mode appends 128 record-count bytes.

For counts 1–255, `_btd_lower_bound` finds the first record whose low address
byte is at least the requested low byte. The original scan then checks the
address and live physical 8-KiB mapper window. It must start at the first
equal record: wildcard bank `$FF` retains its historical precedence over
concrete banks. Replacements and admitted continuation labels are unchanged.

A zero count selects the original scan, covering empty pages and pages with
more than 255 records. Counts never truncate. Missing addresses/banks and
targets below `$8000` retain the strict miss path; no permissive target is
introduced. All records and directories must fit one mapped 16-KiB ROM slot,
checked before project emission.

## Runtime bounds

The helper is 69 fixed-slot-zero bytes plus a three-byte call, with no new
RAM. It temporarily uses six native-stack bytes, including its return address.
DE is preserved below the page-base word; `EX (SP),HL` permits indexed lookup
without introducing IX/IY interpreter support. Bounds use the carry bit when
halving their sum, including counts near 255.

The entire search remains DI. Guest A, resident X/Y, shadow P/S and software
return ownership retain the existing dispatch contract. Only the original
hit/miss paths change mapping or apply the existing interrupt-return policy.
`$CB7D` counts remaining linear record visits, not binary comparisons.

## Verification

The current table's 2,637 records, terminator and original directory are
byte-identical to the prior output. The added counts make the table 16,208
bytes, leaving 176 bytes in its mapped slot.

Assembled property tests cover every current record, neighboring misses and
physical bank states, plus synthetic empty, non-power-of-two, 255/256/257-record
pages, duplicate precedence and pending host interrupts. Pipeline fixtures
retain original-6502 parity for remapped, computed, consumed and rewritten
returns, nested interrupts and repeated-call stack bounds. Capacity overflow
must fail before emitting a project. Canonical SMB/CV1 byte parity remains a
separate gate.

The design's nominal comparison cost is not the in-Rust interpreter's
approximate cycle counter. Actual throughput and visibility require matched
core runs; smaller lookup cost alone does not guarantee fewer blank frames.
The [measured follow-up](ir-optimization-research.md) records the actual
full-route gain and its unchanged absolute blanking count.
