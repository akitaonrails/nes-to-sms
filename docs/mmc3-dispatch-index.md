# MMC3 ROM dispatch index

Accepted implementation, 2026-09-08; independent correctness and structure
reviews pass.
This follows [committed-cell reuse](mmc3-cell-reuse.md) and applies the user's
speed-over-ROM-size preference without adding RAM or changing cartridge capacity.

## Change

The full MMC3 runtime previously performed a binary search before checking which
translated routine matched the requested NES address and current bank mapping.
Translation now precomputes that search's exact result for every address
`$8000–$FFFF`: 32,768 linker-relocated words occupying 64 KiB of ROM.

The existing ordered bank-constraint scan still checks the result. Wildcard
precedence, duplicate addresses, missing targets, traps, mapper changes and
return handling are unchanged. Empty pages and pages with more than 255 records
retain the old zero-count fallback to the page's first record. The table does
not cache a mutable mapper state or guess a game's future bank mapping.

Four full 16 KiB sections are allocated after actual ROM assets, currently
banks 122–125. Placement is explicit and checked before project files are
written; insufficient capacity fails closed. The 2 MiB output size is unchanged.
No hard-coded SMB3 addresses, post-link byte patches or new native RAM allocation
are involved. NROM/UxROM and non-full MMC3 retain their existing output paths.

The fixed-bank helper temporarily maps the appropriate index bank into slot 1,
reads the pointer and restores the original dispatch table's bank. Resident
X/Y, shadow flags, slot 2/SRAM and interrupt state are preserved. It uses no
temporary stack words beyond the existing caller's return address.

## Measured results

Same core, 500% overclock, unchanged interactive input policy:

| Workload | Cell-reuse baseline | ROM index | Throughput gain |
| --- | ---: | ---: | ---: |
| Full active level route | 5.361 updates/s | 5.735 updates/s | 6.98% |
| Early moving window | 3,896,938 T/update | 3,622,030 T/update | 7.60% |
| Later moving window | 4,038,839 T/update | 3,773,127 T/update | 7.00% |

The cumulative full-route gain from the blink-free publisher baseline is
**16.48%** (4.924 to 5.735 updates/s). This is not full speed or a 2× gain.
The index uses 64 KiB of otherwise unused ROM and no additional RAM/VRAM.

## Verification

All 32,768 pointers match both independently derived source semantics and the
old assembled helper. Actual Pause injection covers 168 instruction boundaries.
The emitted helper executes 21 instructions in exactly **159 nominal Z80 T**,
including RET but excluding the unchanged 17-T CALL. The old helper averaged
about 1,150 T on the measured moving windows; this local saving is not itself
a whole-game speedup.

Existing checks pass: 345,088 address/bank/IFF cases, 25,600 synthetic search
edge cases, 55 host IRQ injections, 17 assembled pipeline fixtures and 22,278
VDP transactions. Fresh same-day SMB1/CV1 builds match baseline builds exactly.
Workspace tests pass (585 passed, 169 ignored); formatting and Clippy checks
finish successfully with existing warnings visible.

The faster candidate exposed a limit in the original test
driver's idle-boundary sampling during the automatic goal sequence. Original
failed runs are retained. A separately reviewed diagnostic extension counts only
individually observed epochs at a real idle boundary, with neutral input and
strict game/life/layout continuity. It permits one monotonic exit flag edge,
not arbitrary scene transitions or synthetic controller decisions. Negative
tests and independent review admit this diagnostic, not a weaker game oracle.
The baseline replay remains identical across all three streams and 1,167 saved
snapshot files; candidate prefixes match both retained failed runs up to their
explicit accounting changes.

The final route completes level entry, mushroom pickup, goal, exit and cleared
map return. All seven guest-RAM/cartridge-RAM checkpoints match at the same
logical updates. The 500% port observer matches the installed core's complete
streams and saved snapshots: 3,061,751 events, zero strict violations, zero
gameplay blank frames and no renderer fallback. One known unattributed startup
enable remains documented. A separate 8,000-frame 100% smoke run has zero strict
violations and 28 safe deadline fallback blanks; it does not prove stock-speed
playability.

The 25 image-window differences from the immediate baseline remain recorded:
that baseline sampled the jump one update later. Every affected current image
set and sampled game/input record matches both earlier accepted baselines. The
saved-image bridge connects the discrepancy to the previously verified Mario
position difference, not a new background change. These are checkpoint and
logical-window comparisons, not equality of every physical frame.

Frozen candidate SHA-256:
`ef873763a4acbd65bcba6624da95978b091289b8b7cb9db7e12dd726380386ef`.
Local ignored evidence is under `out/smb3-dispatch-index.w2PISO/`,
`out/smb3-dense-tests.VClAI5/` and `out/smb3-dispatch-index-measure.RoYiY0/`.
