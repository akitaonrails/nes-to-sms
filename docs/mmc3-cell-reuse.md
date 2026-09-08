# MMC3 exact committed-cell reuse

Measured implementation, 2026-09-08; corrected candidate passed independent
correctness and ownership review.
This follows the [frozen-row optimization](mmc3-row-address-kernel.md).

## Change and correctness boundary

The full MMC3 renderer can reuse a committed playfield tile slot when its exact
source dependencies are unchanged. Capture compares all 2048 frozen CIRAM bytes
and replaces a 256-byte change bitmap every time. It preserves the previous
51-byte PF record and committed split before replacing the source packet.
Native `$DC00-$DD3F` owns this evidence; no guest RAM, cartridge capacity or
stack allocation is added. See the [ownership contract](mmc3-presentation.md).

Reuse requires matching vertical scroll, mirroring, split and selected physical
BG CHR pages, with no explicit reload. Only complete, unshifted PF rows qualify;
first, mixed and HUD rows retain full resolution. Horizontal page identity is
checked against mirroring and ring position. Both the source tile byte and its
attribute byte must be unchanged. Palette changes still reach the publisher.

Hits read the committed native NT, not the pending SRAM image, and reserve their
slot on every pass. Existing old-live protection and capacity/restart behavior
remain. Source-off invalidates evidence; HUD capture cannot replace it. `BUSY`
prevents a new producer until the previous native shadow copy is complete.
An explicit source anchor distinguishes a committed packet from a pending one
that can be cancelled. Capture clears that anchor; if it was absent, both
packet and BG resolution are forced even when the next source bytes are equal.
Only completed publication/shadow copy or equivalent exact-packet completion
restores the anchor. The completion callback adds two native stack bytes during
publication. Publisher timing guards, guest lowering and NROM/UxROM paths remain
unchanged.

## Measured performance

Same installed Genesis Plus GX core, numeric `500`, unchanged input-only policy.
These are completed guest updates, not host video callbacks or native NES speed.

| Workload | Row-cache baseline | Cell reuse | Throughput gain |
| --- | ---: | ---: | ---: |
| Full active 1-1 traversal, updates/sec | 5.104 | 5.361 | 5.04% |
| Early 200-update window, updates/sec | 4.533 | 4.807 | 6.06% |
| Later 200-update window, updates/sec | 4.292 | 4.640 | 8.09% |

Early nominal cost falls from 4,131,289.715 to 3,896,938.48 Z80 T/update;
later from 4,363,827.85 to 4,038,838.895. Both ledgers conserve their totals.
Combined with row caching, full-traversal throughput improves **8.88%** over
`d647588`. This is useful but far short of a doubling or full-speed gameplay.

Seven logical guest/cart checkpoints match. All 200 early and 201 later common
captured boundary states match, with exact installed-core profiler prefixes.
The accepted inventory-button sequence reaches level-ready. The candidate has
zero flat/black images across 24,317 gameplay callbacks.

The full-500 observer records 28,246 frames and 3,204,419 VDP events, with zero
strict violations or renderer fallback blanks. Its complete streams and all
1,167 snapshots, including terminal state, match the installed core. A separate
8,000-frame stock-100 diagnostic records zero strict violations and 23 safe
fallback blanks. Both retain the known single unattributed startup-enable caller;
this is not evidence of stock-speed full-route playability.

## Motion comparison and input-sampling limitation

Twenty-five common logical image windows do not overlap the baseline hashes:
835–845 and 849–862. The faster run misses idle observation 832; the unchanged
controller policy consequently starts Mario's jump one guest update later.
A bounded snapshot-only replay covers all 25 windows. Each ROM reproduces its
original input/video prefixes and 27 shared snapshot files exactly. All 425
same-publication-stage snapshot pairs have identical committed NT and palettes;
the only SAT changes are Mario's two Y entries, and every changed pixel lies
inside those displaced sprite rectangles. Stage alignment uses the frozen-source
epoch and observed preparation/exit, not a guessed whole-frame time shift.

The additional 200 cross-stage pairs are retained separately: comparing old
against newly published frames can differ, including NT bytes in 24 pairs.
Those are not same-publication evidence. The original 25 hash failures remain
recorded; they are explained by differing input history, not relabeled identical
video. This diagnosis, plus independent source/forced-full tests, limits the
claim to the tested dependencies and route rather than all possible gameplay.

The corrected anchor candidate retains exactly the first candidate's image
sets and observed decision states in those 25 windows, and identical input
decisions/timing throughout the route. All 2,343 common gameplay windows overlap
between these candidates. Three other raw shadow-status samples differ only in
the nonphysical B bookkeeping bit; they remain recorded, not called complete
raw-state parity. PHP and interrupt pushes explicitly construct their B bit.

## Evidence

Frozen ROM SHA-256:
`b844323640af305d463028cbba055d845dd9a005c3f38ff0022a5a893156c2ad`.
Local ignored evidence: `out/smb3-cell-anchored.V8RdBD/CONTRACT.md`,
`out/smb3-cell-tests.t9wsaj/`, and `out/smb3-cell-fixed-measure.Tqq9IV/`.
Commercial ROMs and extracted assets are not redistributed.

Independent review rejected the first candidate's `READY`-only validity:
MASK-off/on can abandon a captured PF without publishing it. A real-scheduler
test reproduced stale output against independently derived source pixels.
The corrected source-anchor protocol passes that same test, including a next
capture with no additional changes, and verifies reuse resumes after publication.
The original failing artifact/log is retained, not overwritten.

Focused assembled tests cover 6,912 independently derived physical dependency
cases, 24 isolated frame guards, seven row boundaries, and all 2,048 bitmap
bits. A 12-frame capture/reuse/forced-full sequence exercises 3,447 reuse hits;
two sparse attribute-pressure cases each force a real restart with 1,704 hits
and exact forced-full VRAM/CRAM/register parity. Existing 448 source-pixel chains,
160 BG-stable cases, capacity rejection and 22,278 publication transactions pass.
IRQ/Pause injection covers capture, with an exact 831-T entry-prefix bound and
658-T eight-byte kernel bound, excluding handler execution.
An actual nested host IRQ with the new publisher return word live preserves
context and the still-uncommitted anchor; tested minimum SP is `$DFCE`, above
the unchanged `$DE40` floor.

Workspace tests pass (582 passed, 167 ignored); format, diff and Clippy checks
finish successfully with existing warnings visible. Fresh same-day baseline and
candidate SMB1/CV1 builds are byte-identical. Against preserved previous-day
ROMs, only the SDSC day and corresponding checksum byte differ; no ROM patching
or system-clock override was used. Canonical legacy outputs remain untouched.
