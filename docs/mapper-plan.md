# Mapper support plan — from NROM to the major mappers

Goal: run the bulk of the NES library through the pipeline. Target
ladder, each rung a shippable milestone with its own stress ROM:

One mapper at a time; each milestone gets ONE test game from the
user's collection (/mnt/terachad/Emulators/EmuDeck/roms/nes), in
ascending difficulty:

| Milestone | Mapper | Test game (user's collection) | Their library |
|-----------|--------|-------------------------------|---------------|
| M1 | 2 (UxROM) | Castlevania (USA) (Rev 1) | 9 games |
| M2 | 3 (CNROM) | Gradius (USA) | 3 games |
| M3 | 1 (MMC1) | Blaster Master (USA) | 17 games |
| M4 | 4 (MMC3) | Bonk's Adventure / Adventure Island II | 22 games |
| M5 | 7 (AxROM) | Marble Madness (then Battletoads, the torture test) | 2 games |
| M6 | 5 (MMC5) | Castlevania III | 1 game, hardest |
| Later | 23/25 (VRC2/4), 69 (FME-7) | Kid Dracula, Gradius II, Batman RotJ | 3 games |

**Hard regression rule (user directive): every mapper-phase commit
must pass the FULL SMB gate (three routes byte-for-byte, 375 tests,
route expectations) and keep Alter Ego building. No exceptions, no
quick-gates on commit.**

NROM (SMB, Alter Ego) remains the regression floor: every milestone
must keep the three SMB routes byte-for-byte and Alter Ego building.

## The architectural insight

The SMS Sega mapper is itself a banked system, and the pipeline
already fights and wins the cross-bank battle for its own output
(far-gate transfers, per-section placement, fixed-point sizing).
NES mapper support is the same shape one level up:

- a NES PRG bank ↔ a set of SMS sections/banks,
- a NES bank switch ↔ an SMS `$FFFF`/`$FFFE` write through a shim,
- a cross-NES-bank call ↔ the existing `rt_far_gate` machinery.

## Cross-cutting changes (M0 — infrastructure)

**Loader** (`nes_rom`): already parses arbitrary PRG/CHR sizes and
mapper numbers. The *pipeline* assumptions move behind a
`RomLayout` abstraction: `fixed_bank()` (the CPU window that never
switches — UxROM/MMC1/MMC3 all fix the top), `switchable_windows()`,
`bank_count`, vectors read from the fixed bank.

**Label space**: NROM labels stay `L_XXXX`. Banked-region code gets
`L_bNN_XXXX` — a routine's identity is `(bank, cpu_addr)`. The IR,
profile, and reports carry the pair everywhere; the flat `u16`
address is only valid inside the fixed bank.

**Discovery** (bank-aware): walk the fixed bank from the vectors as
today. Into switchable windows, two mechanisms:
1. **Static bank-constant propagation**: the dominant idiom is
   `LDA #k / STA mapper_reg / ... / JSR $8xxx` — track the last
   constant written to the mapper register along the walk; calls
   into the window bind to that bank. Confidence-scoped: propagation
   resets at joins/calls unless both paths agree.
2. **Profile annotations**: `[[bank_entry]] bank = k, addr = 0x8xxx`
   for anything the propagation can't prove (indirect dispatch into
   banks). The trap-with-diagnostics path reports (bank, addr) pairs
   to add — the same fail-closed loop that worked for SMB.

**Reference oracle** (`frame-diff`'s NES bus + `trace`'s
expectations): implement each mapper's register semantics in the
reference bus at the same milestone. Without this there is no
parity gate, so it lands FIRST in every milestone.

**Runtime dispatch**: NES reads/writes in `$8000-$FFFF`:
- Writes = mapper register writes → per-mapper `rt_mapper_write`
  (today's NROM stub becomes a dispatch on the profile's mapper).
- Data reads from a switchable window → the indexed/const read
  dispatchers gain a banked path: current NES bank register (shadow
  byte) selects the SMS bank mapped into slot 2. The H.2-style
  compile-time specialization stays for fixed-bank constants.
- Code calls into a switchable window with a statically-known bank →
  direct far-gate to `L_bNN_XXXX`. Unknown bank → `rt_banked_call`:
  runtime lookup of (current bank, addr) in a generated table, then
  far-gate; misses trap with diagnostics.

**SMS bank budget**: 256KB NES PRG × ~3-4× translation expansion
exceeds the 512KB SMS mapper ceiling for the biggest games. Plan:
translation is per-NES-bank sections, so cold banks can stay
UNTRANSLATED until proven reachable (discovery already only lifts
reached code; expansion factor applies to reached bytes, not the
whole ROM). Measure per-game; the Sega mapper addresses up to 4MB
if needed (mapper supports 256 banks; header size field caps at
1MB for standard emulators — verify per target).

## M1 — UxROM (Castlevania 1)

Mapper 2: one register (any `$8000-$FFFF` write) selects the 16KB
bank at `$8000-$BFFF`; `$C000-$FFFF` is fixed to the last bank.
**CHR is RAM**, not ROM — the second new subsystem:

**CHR-RAM runtime conversion**: the game uploads tiles through
`$2007` into pattern space at runtime. The build-time CHR converter
doesn't apply. Runtime path: `$2007` writes with a pattern-table
address accumulate into a 16-byte NES-tile staging buffer
($CBxx scratch); on the 16th byte (or address discontinuity) the
tile converts 2bpp→4bpp and uploads (~200 cycles per tile — level
loads write hundreds of tiles, all during transitions, fine).
The BG palette-variant machinery (built for static CHR) runs in
dynamic mode: variants regenerate on CHR upload (pool flush on
pattern writes to a mapped tile).

Sequence inside M1:
1. Reference bus: mapper-2 semantics + CHR-RAM (frame-diff).
2. RomLayout + vectors-from-fixed-bank (unblocks the loader error).
3. Fixed-bank-only discovery + lift + run: CV1 reset/init lives in
   the fixed bank; get the title screen up with banked calls
   trapped-and-reported.
4. Bank-constant propagation + `[[bank_entry]]` profile entries from
   the trap reports; iterate until the title route runs.
5. CHR-RAM conversion path; verify title visuals.
6. Level-1 route recorded against the reference; parity gate.

### M1 status (2026-07-08)

Architecture LANDED, SMB regression byte-for-byte through all of it:
per-bank 32 KiB translation units (interior aliases, cross-view and
global label dedup), the runtime (bank, addr) dispatch table with
rt_banked_dispatch (fail-closed: misses trap with the live bank in
$CB1A), lazy-trap window stubs, the UxROM slot-2 remap shim, and
ground-truth bank-entry harvesting from the reference oracle
(FD_LOG_BANK_ENTRIES). Root causes fixed along the way: indexed ROM
stores remapped to zero page (bank switching dead + zp corruption);
late .define compiled the mapper shim out entirely; rt_mapper_write
clobbered A (broke the double-STA bus-conflict idiom).

CV1 now: boots, uploads CHR, switches banks, dispatches its master
task table. BLOCKED inside the FIRST real NMI: _irq_call_translated_nmi
fires once and never returns. Forensics (SMS_LOG_ZPY + control-transfer
ring + per-callee SMS_WATCH_PC): the NMI line runs L_C8CD once, then
the task dispatcher L_C1E4 is entered THREE times (re-entry without
completing the NMI), task 0 (L_b6_B7DC) entered once; L_CCEE (late in
the NMI line) never reached. The endless cycle spans object-physics
routines (L_ECEA/EE35/EF43, ASL-heavy fixed-point) + per-lap bank
switches + PPU reg writes — the shape of the logo animation task
pumping forever. (zp),Y source reads verified sane ($889E bank 0,
$FDD3 fixed). Init RAM snapshot matches the reference byte-for-byte;
frame-0 write streams identical (58/58). RESOLVED: CV1's first NMI
never RTIs BY DESIGN — the main flow lives inside it and later vblank
NMIs re-enter through the $7F guard (NES NMIs are edge-triggered).
The handler now runs the translated NMI with interrupts enabled
(ei/call/di bracket); games that gate re-entry via PPUCTRL bit 7
(SMB) skip at the $CB08 check — NES-equivalent either way. CV1 then
runs 790+ frames, draws its license screen into the nametable
(FIRST PIXELS — garbled patterns), and wedges on a (0,$A824)
dispatch the reference never executes (upstream divergence).
Update: table select was already PPUCTRL-bit4-aware; the garble was
the VARIANT POOL rebaking from the blank build-time CHR asset —
CHR-RAM builds (NES_CHR_RAM define) now read the base tile back from
VRAM (planes 0/1 = the uploaded 2bpp) into a staging buffer.
ANNOTATION DISCIPLINE (hard rule): subject-trap-derived bank entries
are FORBIDDEN — divergence artifacts annotate garbage roots that
decode into BRK/JAM data-walks. Only reference-harvest entries
(FD_LOG_BANK_ENTRIES; now also logs jmp-(ind) landings and RAM_EXEC)
plus hand-verified bytes may be added. (The 'zp $0032' reading was a
diagnostic bug: E1 trap ids are LIST INDICES, E2 ids are addresses —
tools/cv1_verified_loop.py now decodes both.) STATUS: ALL TRANSLATION
TRAPS CLEAR — CV1 boots, uploads CHR-RAM byte-perfectly (tile $F2
verified against the reference's new CHR-RAM ground-truth store),
draws its license screen into CIRAM, and runs with no traps.
RENDERING DIAGNOSIS (full chain, verified cell-by-cell):
1. The license screen's mapped NT cells read tile $001 =
   variant(base 0, S3): the ATTR rewrite resolved with a ZERO
   BGV_BSHADOW because the tile writes never took the mapped path.
   The variant pool itself is HEALTHY (cache populated, ~194 slots).
2. CV1 renders whole screens from NT-B ($2400) via PPUCTRL select;
   writes can happen while the select differs. The HUD-band rule is
   now page-aware (PPUCTRL bit0), but write-time gating is
   insufficient in principle.
3. The window model ($CB2A, 64-col space) materializes only columns
   ENTERING on scroll deltas, and only with rendering ON
   (mat_sched render=off does nothing). Full-page-flip games (CV1
   license/menus) never get the alternate page materialized.
4. DESIGN NEEDED (next session): on PPUCTRL NT-select change and on
   rendering-enable, enqueue full-window re-materialization from raw
   CIRAM — the projector/materializer machinery exists; it needs
   these two triggers. Raw CIRAM is already byte-correct (glyphs
   verified), CHR uploads byte-perfect, so materialization alone
   should produce the license/logo/title screens.
5. IMPLEMENTED (guarded behind NES_CHR_RAM; SMB path byte-identical):
   teleport/page-flip full-window materialization (delta >= 5 now
   projects all 32 columns, rows 0-27, instead of bailing) and
   present-time BAND materialization (rows 0-3 raw-only at write
   time + $CB78 dirty flag; rt_nt_materialize_band projects the
   SELECTED page's rows 0-3, cols 0-31). _nt_project_col gained an
   end-row parameter ($CB79) with _all/_band variants.
6. CURRENT STATE: checkpoints render ALL BLACK — possibly CORRECT: at
   frame ~706 CV1 sits stalled after the license fade-out (CRAM dark).
   The earlier 'garbled fragments' were stale pre-materialization
   VRAM. NEXT SESSION, in order:
   a. THE STALL: the subject freezes post-license while the reference
      advances (ref reaches ram[$18]=1 in 3 frames; subject never).
      Hunt the wait byte: FD write-fork near the last matching frame;
      ppu_mask never enables in the subject.
   b. Verify rendering on a LIT frame once the stall clears (dump
      CRAM at checkpoint time to confirm palette state first —
      SMS_DUMP_RAM works for RAM; add a CRAM dump if needed).
   c. Watch variant-pool thrash: band materialization re-resolves 128
      cells per dirty presentation; consider materialize-once-per-
      select-flip if the ring churns.
6b. RENDER-PIPELINE ROOT CAUSE (definitive, end-of-session): CV1 sets
   PPUCTRL bit4 (BG pattern table $1000) and uses table-1 tile
   indices >= 192 (HUD text at $D0+). The CHR-RAM identity mapping
   gives table 1 only 192 physical slots (SMS budget: ~440 tiles
   total, table 0 took 256) — so those tiles were DROPPED at upload
   ($1C00+ writes) and UNMAPPED (base 0) in the NT map: every text
   cell rendered variant(0,S) = the solid tile. Verified end-to-end:
   raw CIRAM rows hold the correct text; the assembled map0 is
   perfect identity; map1[>=192] = 0.
   DESIGN (next session): dynamic CHR-RAM tile allocation — a
   512-entry NES-tile -> SMS-slot table in RAM (the $D300 candidate
   region), assigned on first pattern upload; the $2007 pattern
   write path redirects rows to the assigned slot, and the NT
   mapping path consults the SAME table instead of the static map.
   Games never use both full tables simultaneously, so ~440 slots
   suffice. Also gives MMC3 CHR-banking a foundation (slot
   assignment per (bank, tile)).
   ALSO: display-enable edge captures (SMS_DUMP_ON_ENABLE=dir) show
   CV1 cycling its screens with correct palettes — one full-screen
   mosaic capture proves end-to-end rendering works modulo the tile
   allocation above.

7. SESSION UPDATE (overnight): BRK-AS-INTERRUPT implemented — NES BRK
   vectors through the IRQ handler and RTIs; CV1's engine TOLERATES
   junk task dispatches this way on real hardware (bank-0 bytes at
   task addresses are data ending in BRK). This killed the task-engine
   stall: the subject now tracks the reference's task progression
   ($18=1 in 3 frames, exactly like the ref). Also fixed: a
   re-entrancy hole where nested handlers restored slot 1 mid-table-
   scan ($CB14 not updated by the dispatch) — the scan is now DI-
   bracketed with a $CB7E in-handler flag for nesting-aware EI; and
   the band materializer starved the frame loop (CV1 dirties the band
   every frame) — now write-through for the selected page + full
   re-materialization only on PPUCTRL select CHANGE. Frames flow to
   1500+ (logo checkpoint reached).
   REMAINING: ppu_mask never enables in the TRACE harness (its
   synthetic $2002 model; the FD harness subject DOES enable by
   frame 6 — harness artifact, not translation); real-emulator
   (Mednafen) 90s run still black — CV1 at 1x is slow, needs a longer
   run or GPGX overclock to reach its screens. Next: longer real-emu
   run / fix trace $2002 vblank model / then title+demo visuals and
   the level-1 parity route. Also new this round: oversize
data-walk stubs (mis-rooted lifts up to 90 KiB get loud trap stubs),
wrap-safe + post-emit section rotation, snapshot keys for banked
units, indirect-landing ground-truth harvest (136 entries).

## M2 — MMC1

Serial 5-write register protocol (shim buffers the shift register),
PRG mode variants (16KB switch low/high, 32KB), CHR 4KB banking,
mirroring control (the materializer already parametrizes vertical/
horizontal; MMC1 switches it at runtime → the fold/projector
mirroring define becomes a runtime flag).

## M3 — MMC3 (SMB3, Kirby)

8KB PRG banking (two switchable + two fixed windows — the label
space and dispatch generalize from 16KB to window-granularity),
2KB/1KB CHR banking (CHR-ROM again, but banked: the build-time
converter emits per-bank tile sets; the runtime CHR window state
selects which SMS tile base the BG/sprite mappers use — this is the
big one for the CHR pipeline), and the **scanline IRQ**: map the
MMC3 counter onto the SMS VDP line interrupt (the split machinery
already drives it; MMC3 games configure a line and flip scroll/banks
there — same shape as the SMB sprite-0 split, now data-driven).

## M4 — MMC5 (Castlevania III)

Everything above plus ExRAM modes, fill mode, vertical split,
8×16-attribute mode, multiplier, PCM. Scoped only after M3 ships;
several MMC5 features (extended attributes per tile) map poorly to
Mode 4 and may need per-game compromises. Honest flag: this rung
may land as "CV3-specific subset of MMC5".

## Verification protocol (unchanged in spirit)

Per milestone: reference-bus mapper first; then trap-driven
iteration to boot; then a recorded route with full-frame RAM parity;
SMB three-route + Alter Ego regression on every commit.

### Real-pacing investigation state (end of marathon session)

Instrumented environments (trace, FD) run CV1 completely: task engine
in lockstep with the reference, screens cycling, recognizable
courtyard/intro renders. REAL emulators (Mednafen, GPGX) stall in
early boot. Evidence chain from the boot beacons (border-color
milestones, NES_CHR_RAM builds only) + savestate forensics:
- boot entry, runtime-init, first frame IRQ, first translated NMI,
  and the first PPUMASK-driven reg-1 application ALL fire;
- the handler's SAT upload writes VRAM (savestate shows the SAT);
- the game's OWN uploads (NT/pattern via $2007) never begin — VRAM
  outside the SAT is empty at 40s;
- SRAM at $8800 read/write self-test PASSES on Mednafen.
So the game wedges between its first NMIs and its first PPUDATA
burst — only under real pacing. The NMI nesting-depth gate (max 2
in-flight translated NMIs, $CBE9) was added on principle (unbounded
nesting under overrun = stack death) but did not resolve it.

NEXT SESSION (the decisive tool): a pacing-hostile trace mode —
real per-frame step budgets, level-held IRQ re-fire semantics, and
$FF RAM init — to reproduce the stall inside the debuggable harness,
then the usual forensics. Also: boot beacons + SRAM self-test stay
(NES_CHR_RAM-guarded); MRU dispatch cache + deferred FC flush landed
(boot ~2x faster); 512K compact banked layout (1 MiB was never the
issue but compactness is safer across emulators).

### SMOKING GUN (marathon session close): Z80 executes RAM on Mednafen

A Mednafen savestate taken 60s into a real-emulator run: the Z80 PC
is $F314 — which is RAM ($C000-$DFFF mirrored), and disassembles as
data, not code. The Z80 has jumped into RAM and is executing garbage.
The emulated 6502 task counter ($18) is still 0 — the game never
reached task 1. This is a REAL CRASH specific to real emulators; no
harness (trace, FD, even the new SMS_REAL_PACING hostile mode)
reproduces it — they idealize whatever triggers the wild jump.

Candidate causes (next-session hunt, in rough priority):
1. A far-gate / rt_far_gate corruption path that only fires under a
   specific bank/interrupt interleaving the harness never hits — a
   bad $CB14/$FFFE/$FFFF interaction pushing a garbage return.
2. rt_brk / translated_irq returning to a corrupted address (the
   BRK-as-interrupt path is new and manipulates the stack).
3. Stack overflow into low RAM under sustained real-pacing overrun
   (the depth gate caps NMI nesting but far-call/dispatch nesting is
   uncapped) — SP was $30DD in the savestate (healthy there, but a
   transient dip could corrupt).
4. Uninitialized RAM read the harness zero-fills but real HW leaves
   as $FF (the hostile mode $FF-fills but didn't trigger it).

TOOL TO BUILD FIRST: a Z80 execution-address guard in the trace
(trap the instant PC leaves ROM slots 0-2 / enters $C000+), then run
under every pacing/RNG/input permutation until it fires; OR transplant
the Mednafen savestate's full state (RAM+regs+bank latches) into
z80_emu and single-step forward from the crash edge. The savestate
parser (docs: gzip + named chunks MAIN/Z80/VDP/CART) already works.

What DID land this session and holds (SMB byte-for-byte): the
SMS_REAL_PACING hostile trace mode (15K insn/frame, boolean pending,
$FF RAM), which found + cleared one real lag-path trap (0,$820D); the
presentation re-entrancy guard ($CBEA); boot beacons + SRAM
self-test; MRU dispatch cache; NMI depth gate; 512K compact layout.

### BREAKTHROUGH: the real-emulator hang is a CPU-emulation divergence

Built SMS_LOAD_STATE=<raw mednafen state> in trace-sms: transplants a
Mednafen savestate (RAM, VRAM, CRAM, cart SRAM, bank latches, full
Z80 regs) into z80_emu and runs forward. (Gunzip the .mcs first;
parser = named chunks MAIN/Z80/VDP/CART.)

DECISIVE RESULT: transplanting Mednafen's STUCK state (task counter
$18 = 0, frozen there for 30s across 10 savestates) and running
forward, MY emulator ADVANCES the task to 1. So z80_emu, given
Mednafen's exact state, makes progress Mednafen never does — the hang
is a **CPU-emulation divergence**, not data/pacing/RNG. z80_emu
executes some instruction differently from Mednafen's accurate Z80,
and on that difference CV1 progresses in-harness but hangs on real
hardware. This is the 'trace-green != Mednafen-correct' class: a
flag/opcode bug (in the generated flag-reconstruction, or in
z80_emu) that z80_emu and the generator happen to AGREE on, so the
frame-diff oracle — which uses z80_emu as its subject — structurally
cannot see it. (The 4 anchor-phase byte diffs are unrelated noise.)

Corollary: EVERY translated game shares whatever this is; CV1 just
exercises the path SMB doesn't.

NEXT (the real fix): differential Z80 execution against an
INDEPENDENT reference Z80 (vendor/port a known-correct core), lock-
step against z80_emu on the CV1 task-0 code path; first divergent
instruction = the bug. Candidates to scrutinize first: DAA, the
rotate/shift P/V and undocumented 3/5 flag bits, block-op flags,
16-bit ADC/SBC (ED-prefixed) flags, and EI/interrupt-acceptance
timing — whichever the 6502->Z80 flag-reconstruction emitters lean
on. Then re-verify CV1 on Mednafen and re-run the SMB gate (the fix
may change z80_emu, so SMB parity must be re-confirmed).
