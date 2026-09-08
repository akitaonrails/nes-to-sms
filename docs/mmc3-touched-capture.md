# MMC3 touched-block capture

Measured follow-up to `fc6d18d`, 2026-09-08. This optimizes frozen-source
capture, not guest scheduling, renderer dependencies or VDP deadlines.

## Change and ownership

Every real CIRAM store marks its normalized physical 16-byte block in two
independent 128-bit masks: playfield `$DD40–$DD4F`, HUD `$DD50–$DD5F`.
These 32 native bytes are separate from guest RAM, the exact PF difference
bitmap, the committed-source anchor and the unchanged `$DE40` stack floor.
No cartridge, SRAM or VRAM capacity changes.

The PPU marks both masks after storing, under the existing interrupt guard.
Same-value and change-then-restore writes remain conservatively marked.
Each capture consumes only its own intent after its snapshot is exact.
Untouched PF blocks still assign zero to every corresponding exact dirty bit;
touched blocks retain eight-byte comparisons. HUD comparisons are independent.
Initialization seeds both masks after clearing CIRAM. Single-record PF→HUD
copy invalidates HUD intent because frozen PF need not equal live CIRAM.

Touch intent describes equality to a snapshot, **not** committed display
ownership. READY, source anchoring, canceled packets, old-live protection and
publication rules remain unchanged. Host service cannot reenter the guest or
mutate CIRAM during capture.

## Matched moving measurements

Same Genesis Plus GX core, numeric `500`, two 200-update windows. A frozen
69-edge input replay starts from reset; all inputs occur at real observed idle
boundaries. Missing required input edges, checkpoints or arm state fail closed.

| Window | Before updates/sec | After updates/sec | Gain |
| --- | ---: | ---: | ---: |
| Early | 5.554 | 5.821 | 4.81% |
| Later | 5.177 | 5.341 | 3.16% |

Nominal Z80 T/update falls 3,373,932.265→3,221,086.015 early and
3,621,300.555→3,510,335.605 later. All 200 intervals conserve their ledger;
unknown/external costs are zero. All 199 early/198 later common completed
guest 2 KiB/cart 8 KiB states match exactly. The baseline misses two/three idle
samples; none is reconstructed. Candidate samples every endpoint.

Baseline replay reproduces its original 11,727-frame prefix and 582 saved
snapshots exactly. Candidate profilers reproduce their own installed-core
prefixes and 486/570 snapshots. These are matched moving-window gains, not an
adaptive full-traversal comparison, a cumulative multiplier or full speed.

The executed capture skips 90.55%/86.37% of PF bytes. Capture plus new write
marking saves 167,347/144,709 T per update; other timing and waiting costs grow
by 14,500/33,744 T. Renderer row walking, hashing and cell reuse execute exactly
the same exclusive instruction costs as before. Their cost is not hidden inside
the capture savings, and remains a separate optimization opportunity.

## Regression evidence and limits

The separate adaptive route collects the mushroom, clears 1-1 and returns to
the map with four lives. Its 20,375 gameplay callbacks contain no flat/black
frames. The full-500 port observer records 23,577 frames/2,832,600 events, zero
deadline violations or renderer fallback blanks; three complete streams and
all 1,179 snapshots, including terminal state, match the installed core.
Stock-100 records 8,000 frames/667,081 events, zero violations and 53 safe fallback
blanks. Stock-clock playability is not established.

Faster neutral loading exposed a sampling limit: three independently observed
epochs elapsed before the next sampled idle. One pinned 447→450 admission
requires continuous host/epoch observations, released input and unchanged
loading state; all other guards remain. The original failure is retained.
Adaptive movement starts at 511 instead of baseline 514, hence the separate replay.
Inventory open/close followed by level entry also passes, reaching level-ready
at tick 711. Its analogous loading gap uses a separate, candidate-only 647→650
admission with verified inventory history and the exact 22 physical samples.
The failed probe remains recorded; all preceding streams, 276 snapshots and
actual state columns reproduce exactly. Neither exception relaxes other gaps.

Assembled tests cover 4,124 physical-store vectors, independent PF/HUD capture
against forced scanning and the original ROM, cancellation, source pixels,
880 PPU Pause/host injection sites and 332 capture Pause sites. The marking
helper costs 151T + 17T CALL; whole PPU write 884T, DI-through-delayed-EI 860T.
Capture's maximum measured closed interval is 781T; existing entry 831T remains.
These opcode bounds supplement, not replace, actual display-deadline evidence.

Workspace: 587 passed, 174 ignored. The three new ignored tests were explicitly
run, along with 18 CPU pipeline fixtures and 22,278 publication transactions.
Formatting and Clippy pass with existing warnings visible. Fresh same-day
SMB1/CV1 baseline/candidate ROMs are byte-identical; preserved legacy outputs
are untouched. Previous-day differences are only SDSC day/checksum bytes.

Frozen ROM SHA-256:
`d6ce21b2896115444966740cdd4b190d5c820ed19542d6cdfe9b68326e427f89`.
Local ignored evidence: `out/smb3-touched-capture.En0WaL/CONTRACT.md`,
`out/smb3-touch-tests.sxQwh3/{PROVENANCE,STRUCTURE-REVIEW}.md`, and
`out/smb3-next-speed.eznN9e/`. Commercial ROMs are not redistributed.
