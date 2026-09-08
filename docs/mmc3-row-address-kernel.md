# MMC3 frozen-row address optimization

Implementation experiment, 2026-09-08. This is the first bounded step from the
[performance strategy](smb3-performance-strategy.md), not cross-frame cell reuse
or a full-speed claim. The implementation and evidence passed independent review.

## Change and invariants

The full MMC3 renderer now computes native frozen nametable and attribute-row
addresses at row entry and horizontal nametable wrap. Primary cells reuse those
address components. Previously each tile and attribute read repeated frozen
record selection, mirroring and address construction.

Four previously unused, full-mode renderer-owned bytes hold the row components:
`C85B/C85C` contain native nametable/attribute high bytes; `C8EE/C8EF` contain
the attribute-row low base and vertical quadrant. They are recomputed for every
row and reserve-pass restart. No general-purpose register or native stack frame
is retained across hash lookup, allocation or pattern conversion.

Secondary sources in mixed cells retain the original address calculation. They
may have different frozen records, mirroring, CHR maps or vertical origins;
restoring the primary source does not overwrite its row components. Physical
CHR keys, palette extraction, slot ownership and the destination ring remain
unchanged. No dependency is inferred across frames.

Existing host-service boundaries remain. Additional closed boundaries follow
row-address setup and precede secondary-source calculation. The publisher,
blanking thresholds, cartridge capacity and guest instruction lowering do not
change. NROM/UxROM runtime paths do not include this renderer.

## Initial measured result

Same installed Genesis Plus GX core, numeric `500`, unchanged controller-only
input policy and completed guest updates; accepted baseline `d647588`.

| Workload | Before | Candidate | Throughput gain |
| --- | ---: | ---: | ---: |
| Full active 1-1 traversal, updates/sec | 4.924 | 5.104 | 3.66% |
| Early 200-update window, updates/sec | 4.356 | 4.533 | 4.05% |
| Later 200-update window, updates/sec | 4.093 | 4.292 | 4.87% |

Early nominal cost falls from 4,301,011.88 to 4,131,289.715 Z80 T/update;
later cost falls from 4,579,321.14 to 4,363,827.85. Both instruction ledgers
close all 200 intervals, conserve their totals and have zero unknown/external
costs. These heavier windows are not interchangeable with the full traversal.

All seven full-route guest-RAM/cartridge-RAM checkpoints match at the same
logical ticks. All 2,339 common nonflat logical windows contain matching images;
the candidate has zero flat/black gameplay callbacks out of 25,457. This does
not claim equality of every transient physical frame. All 200 early and 201
later commonly observed guest/cart boundaries match; the baseline's missing
early observation is not reconstructed. Both profiler runs match their installed
route prefixes and available snapshots exactly.

Both held-B and the baseline's held-A+B inventory routes open/close inventory
and enter the level. The full-500 physical observer records 29,415 frames and
3,298,125 VDP events with zero strict violations or renderer fallback blanks.
Its complete input/video streams and all 1,155 snapshots, including final state,
match the installed core. The
separate 8,000-frame stock-100 port diagnostic finds no strict deadline/stream
violations, retaining 24 safe fallback blanks across 171 publications. It is
not a full-route stock-speed playability test.

## Evidence and acceptance

Focused assembled checks pass: 81,920 independently derived address/key cases,
448 renderer frame chains, and 1,152 old/new source comparisons. The latter
include 768 secondary paths and 72 matching unsupported three-source cases.
Actual host-ISR injections check register, mapping, scratch and guest ownership.
Fail-closed instruction accounting confirms a maximum 602-T setup helper
(excluding its caller) and reduces the tested longest closed source span from
2,802 to 1,449 nominal T, excluding interrupt service. These bounds are distinct
from whole-frame throughput and physical VDP timing evidence.

Frozen candidate ROM SHA-256:
`6880b9b474c8bad5e2ad25ee4847ce5b3e7fdaeeb7fd8bb53d647c44df132015`.
Local ignored source/build evidence is under `out/smb3-row-kernel.Hnj88g/`;
actual runs, wrappers and comparisons are under `out/smb3-row-measure.blsKH5/`.
No commercial ROMs, extracted assets or personal collection paths belong in Git.

Relocated publication timing and transaction checks pass. Fresh same-day
baseline-runtime and candidate-runtime builds of SMB1 and CV1 are byte-identical.
Against the preserved previous-day ROMs, each differs only at SDSC day byte
`7FE6` and the corresponding SMS checksum byte `7FFA`; no ROM bytes were patched.
Their canonical outputs remain untouched.

Workspace tests pass: 582 passed, 162 ignored. Format, diff and Clippy checks
finish successfully; existing warnings remain visible. Independent production
and test review approved this bounded change. Verification planning kept source
correctness, timing, actual-core behavior and measured gains as separate gates.
