# Completion Plan

Step-by-step plan to take the current pipeline from "produces a structurally
valid `.sms`" to "Super Mario Bros. runs on the Master System with all or
most of the original NES ROM translated and the game playable end to end."

This document is the **execution** companion to
[`master-plan.md`](master-plan.md). The master plan defines architecture,
principles, and v1 targets. This document is the linear checklist that
takes us from now to done. Update it in place as we learn things —
re-order, add, remove. Do not throw it away and replace it with a
different list; the document's value is in being the durable record of
what we decided to do next and why.

## Definition of done

A `sms.sms` produced by this pipeline satisfies all of:

1. Boots and plays Super Mario Bros. from RESET through the title screen,
   through World 1-1, and into at least World 1-2 (so we exercise
   transitions and underground/water area types).
2. Player movement matches NES SMB behavior closely enough that a human
   can complete World 1-1 without practiced muscle memory failing.
3. Enemies (Goombas, Koopas) spawn, move, and interact with the player.
4. Collisions and blocks behave correctly: stand on ground, bonk question
   blocks, break bricks (when big), bump into pipes.
5. Scrolling tracks the player across all of 1-1.
6. Status bar updates: world/level, time, score, coins, lives.
7. Game-over and "world clear" transitions work.
8. PSG audio approximates the SMB pulse channels recognizably; triangle
   and noise are approximated; DMC is silent.
9. Every executed PRG byte is either translated 6502 (lifted into IR and
   lowered to Z80) or routed through a documented hardware shim. No
   `rt_unresolved_jsr` traps during normal play.
10. The differential test harness reports 100% of lifted routines passing
    `oracle_6502` ↔ `z80_emu` equivalence on randomized input vectors.
11. Mednafen and at least one other SMS emulator (Emulicious) load the
    ROM and produce visually equivalent output.

"All or most of the original NES ROM" is operationalized as: ≥95% of
PRG bytes either classified as data (in profile) or reached by analyzer
discovery (and successfully lifted+lowered). The remaining ≤5% may stay
as unreached cold paths, but each must be documented in
`reports/unreached.txt`.

Audio fidelity, BG palette accuracy, and exact-frame timing are not part
of the bar. SMB's gameplay being correct is the bar.

## Plan structure

Phases A–J. Each phase has numbered steps. Each step is one focused
unit of work with an acceptance criterion. Work them in order unless
discoveries demand re-sequencing.

When a step lands, change `- [ ]` to `- [x]` and add a one-line "outcome"
note. Don't delete completed entries — the history is the record.

---

## Phase A — Correctness foundation

Unblocks every later phase. Without a diff harness, every lowering bug
is invisible until it shows up visually, which is the worst place to
debug from.

- [x] **A.1 Diff harness library.** Added `validation` crate with
      `validate_routine(prg, routine, n_vectors) -> ValidationResult`.
      Self-contained: emits the runtime helpers it needs as Z80 bytes
      in the same `Program`, so a routine validates without depending
      on the `runtime/*.s` build. Skips routines that touch hardware
      or have external references.

      **Outcome:** 11 validation tests green (7 single-op + 4 SMB
      micro-slices including the prior PoC's $9CA6 pointer-increment
      and $B1B4 branch-store). The harness immediately found two real
      lowering bugs: (a) `CLC/SEC/CLI/SEI/CLV/CLD/SED` clobbered A
      because the naive shadow-P update loaded P into A; fixed by
      bracketing with push/pop AF. (b) `BranchIf` clobbered A for the
      same reason; fixed by switching to `bit n,(hl)` against shadow P
      which leaves A untouched. Required adding `bit_n_hl_ptr`,
      `set_n_hl_ptr`, `res_n_hl_ptr` to z80_emit and the matching
      `bit/set/res n,(hl)` opcodes to z80_emu's CB dispatcher.

- [x] **A.2 `--validate` cli flag.** Wired into `pipeline::run`. Default
      N=32 vectors per routine; `--validate-vectors N` overrides. Report
      goes to `reports/validation.txt`. Summary line appears in cli
      stdout. Run does NOT fail on red — the build is still considered
      successful (the project tree is emitted); validation surfaces the
      bug list for Phase D rather than blocking iteration.

      **Outcome:** initial SMB run reported 2 green / 20 red / 37 skipped
      out of 59 routines. The skipped set is mostly hardware-touching or
      ones with external JSR; the red set is the actionable list for
      Phase A.3 triage. Subsequent fixes (STX/STY/TXS/PHP/PLP/INX/INY/
      DEX/DEY no longer clobber A; indexed addressing remaps to SMS RAM;
      `add hl,rr` now supported in z80_emu; BranchIf uses `bit n,(hl)`)
      moved the SMB count to 6 green / 16 red / 37 skipped.

- [x] **A.3 Initial validation sweep.** Triage doc at
      `docs/validation-triage.md`. Six concrete bug classes (B1–B6)
      identified and fixed mid-A.2; remaining 16 reds catalogued.

      **Outcome:** validation surfaces real lowering bugs. The triage
      doc is a living register; new bug classes get appended as Phase D
      proceeds.

- [x] **A.4 z80_emu instruction trace.** Added
      `Cpu::run_until_ret_with_trace<F: FnMut(&Cpu, u16, u8)>`. Trace
      fires before each executed instruction with (cpu state, PC,
      opcode). Used for diagnosing the harder reds. Step-budget limit
      raised to 200K (oracle) / 2M (z80) to handle SMB's long loops.

- [ ] **A.5 Branch-coverage validation.** Many routines have flag-dependent
      branches. The 64-random-state vector approach misses some branches.
      Extend `validate_routine` to bias the random P byte so every
      branch in the routine is exercised in at least one vector.

      **Acceptance:** for a routine with `BCS`/`BCC`, both branches are
      exercised in the validation report.

      **Deferred:** the 32-vector random sweep already exercises both
      directions of nearly all branches in SMB routines we tested; no
      red failures remain to debug. Revisit if Phase D regressions
      indicate flag-path coverage gaps.

---

## Phase B — Coverage

The analyzer currently reaches ~8% of SMB's PRG. We need ≥95% before
"complete" is meaningful. This is mostly profile work; the engine is
ready.

- [x] **B.1 Doppelganger label import.** Fetched the canonical
      doppelganger SMB disassembly from 6502disassembly.com. Parsed
      addresses + label names directly from the HTML listing.
      Cross-referenced JSR call sites: labels that are JSR targets
      became `[[function]]` (141 entries), the rest became `[[label]]`
      (1314 entries). Profile size grew from 19 entries to 1455.

      **Outcome:** discovery jumped from 59 to **322 functions**, code
      bytes from 2,752 to **13,931** (8% → 43% of PRG classified as
      code). Validation went from 15 green / 0 red to **55 green /
      14 red / 253 skipped**. New reds (14) added to Phase D queue.

      Pipeline-side ancillary fixes required by the larger profile:
      - Cli now uses `z80_emit::unresolved_labels()` to find any patch
        target not defined and emits a trap stub for it. Replaces the
        previous external_calls-driven stub pass which had gaps.
      - Sections changed from `force` to `superfree` so the linker
        can place each in any 16 KiB bank.
      - Cli auto-rotates `generated_code_N` sections every ~14 KiB so
        none exceeds a bank's capacity.
      - ROM size bumped to 256 KiB (translated code alone is ~45 KiB
        and grows as coverage broadens).
      - Detected and worked around the lifter sometimes emitting both
        the entry name and the `L_XXXX` auto-label at the same byte
        (when a routine has a backward branch to its own entry); cli
        no longer pre-emits the alias in that case.

- [ ] **B.2 CDL ingestor.** Add `crates/cdl/` that parses a Mesen or
      FCEUX `.cdl` file (one byte per ROM byte: code/data/indirect-code).
      Expose a function the cli uses to merge CDL classifications into
      the analyzer's `ClassMap` before discovery walks.

      **Acceptance:** with a hand-recorded SMB CDL from Mesen (5-min
      play session of 1-1), coverage report shows ≥98% of PRG
      classified.

- [x] **B.3 Tighten analyzer on data.** Extracted 398 `[[data_region]]`
      entries from the doppelganger `.bulk` / `.dd1` / `.dd2` / `.byte` /
      `.word` lines and added them to `profiles/smb.toml`. The analyzer
      already respects data_regions (stops walking into them); the
      missing piece was the regions themselves.

      **Outcome:** lower failures dropped from 5 to **0**. All 319
      discovered routines lift and lower cleanly. Coverage breakdown:
      13,456 code bytes / 3,488 data bytes / 15,824 unknown bytes.

- [x] **B.4 Jump-table profile support — partial.** Extracted every
      `.dd2` pointer from the doppelganger disassembly that targets a
      known code label. Promoted 104 such labels from `[[label]]` to
      `[[function]]` so the analyzer walks them as roots. (This is the
      same data a `[[jump_table]]` entry would have supplied, in
      simpler form.)

      **Outcome:** function count jumped from 319 → **490**. Code
      coverage grew from 13,456 → **18,269 bytes** (~56% of PRG).
      Translated.asm: 60 KB.

      **Deferred (full B.4):** runtime-side JumpEngine replacement.
      SMB's JumpEngine relies on the 6502 *stack* containing the
      JSR return address so it can pop the return, use it as a
      pointer into the inline `.dd2` table, then dispatch. Our
      translated JSR pushes onto the Z80 native stack instead, so the
      translated JumpEngine can't read the table. Needs either: (a)
      mark JumpEngine as `[[replacement]]` and write a Z80 dispatcher
      that knows the table layout, or (b) translate every JSR to push
      onto the emulated 6502 stack in addition to the Z80 stack.
      Documented as a known Phase E follow-up.

- [ ] **B.5 Inline-trampoline patterns.** Some NES games (Faxanadu is the
      NESRecomp reference; SMB has a few) use `JSR` with inline data
      bytes following the call: the callee pops the return address,
      reads bytes, advances, and returns past the inline data. Add a
      `[[inline_trampoline]]` profile entry: `addr`, `inline_bytes`,
      and let lower emit the correct CALL + skip-N-bytes pattern.

      **Acceptance:** synthetic test reproducing the pattern produces a
      correct ROM. SMB-specific inline trampolines (if any) added to
      `profiles/smb.toml`.

- [x] **B.6 Coverage report.** Extend `reports/discovery.txt` to break
      down PRG by: code (executed in CDL, if available), code (statically
      discovered), data (profile-declared), data (CDL-classified),
      unreached. Highlight unreached code as the next-to-attack list.

      **Acceptance:** the report has a clear coverage % line and a
      top-N unreached-routine list.

      **Outcome (2026-07-04):** the pipeline now writes
      `reports/coverage.txt` — a coverage % line plus every contiguous
      unknown PRG range with a hex peek. Using it, the 305 residual
      unknown ranges (6,462 bytes: the $A2xx-$AE4B area/level data
      streams, text/tile tables, music tables at $FD56+) were classified
      as profile `[[data_region]]` entries — none reachable as code from
      any root, full route at NES parity without traps. **PRG
      classification is now 100%** (22,788 code / 9,980 data / 0
      unknown), with 0 unresolved labels; the translation output is
      byte-identical to the validated build. Two traps learned: profile
      `end` is inclusive (an exclusive end swallows the next routine's
      entry byte and kills its walk), and unknown bytes INSIDE function
      extents are inline/jumped-over data the lifter must pass through —
      declaring them as (barrier) data regions truncates lifting.

---

## Phase C — IR/lifter completeness

The lifter must handle every instruction shape SMB can present.

- [ ] **C.1 Stable unofficial opcodes in IR.** Replace `Op::Unsupported`
      for SLO/RLA/SRE/RRA/DCP/ISC/SAX/LAX with proper IR ops that
      describe their semantics. LAX already lifts; the rest don't.
      Each is a memory read-modify-write plus an ALU op on A.

      **Acceptance:** lifting a hand-built routine with each opcode
      produces correct IR per `format_ir_op`. `ir` tests cover all
      seven.

- [ ] **C.2 Indirect addressing with page-wrap.** Verify and test that
      `LDA ($zp),Y` and `LDA ($zp,X)` lift with the correct AddrExpr,
      and that the page-wrap on `JMP ($XXFF)` is preserved as a flag
      the lower pass uses.

      **Acceptance:** `ir` test: `JMP ($30FF)` produces an op variant
      that flags the bug case for lowering.

- [ ] **C.3 BRK.** Currently lifts as `Op::Brk`. Lower it to a runtime
      call (`rt_brk`) that pushes PC+2 and P and jumps to the IRQ
      vector. NES BRK is rare in production code but must not crash.

      **Acceptance:** lifting+lowering `00` (BRK) emits a clean call to
      `rt_brk`.

- [ ] **C.4 RTI.** RTI pops P, then PC. Already lifts as `Op::Rti`;
      verify lower emits `pop6502 → SHADOW_P; ret` correctly. Add an
      end-to-end test.

      **Acceptance:** ir+lower test passes for an RTI-ended IRQ stub.

- [ ] **C.5 Inline dispatch trampolines.** From B.5; the IR side is the
      decode and the propagation through CFG analysis.

      **Acceptance:** trampoline pattern recognized at lift time when
      annotated; ops include the inline-data skip.

- [ ] **C.6 Diff sweep on synthetic ROMs.** Build a corpus of small
      synthetic ROMs (one per opcode group). Run each through the full
      pipeline and validate via Phase A.

      **Acceptance:** all synthetic ROMs validate green. `cargo test
      --workspace` includes them as integration tests.

---

## Phase D — Lower completeness

Every IR op produces correct Z80.

- [x] **D.1 Stable unofficial opcodes in lower.** DCP, ISC, SLO, RLA,
      SRE, RRA all lift in the IR as `(DecMem|IncMem|AslMem|RolMem|
      LsrMem|RorMem)` followed by `(CmpMem|SbcMem|OraMem|AndMem|
      EorMem|AdcMem)`. The harness's runtime stubs for asl_mem/lsr_mem/
      rol_mem/ror_mem (placeholders in Phase A) now have real
      implementations matching the runtime/*.s contract. SAX still
      Unsupported (no dedicated IR op yet — needs one to express
      `M := A & X` without modifying A).

      **Outcome:** lower failures dropped from 7 to 6 (only SAX
      remains). LDX/LDY/STX/STY support all addressing modes (ZpConst,
      Const, AbsIndexedX/Y, ZpIndexedX/Y) via the shared `emit_ldxy_mem`
      / `emit_stxy_mem` helpers. Validation moved from 55 green / 14
      red to **56 green / 13 red**.

- [ ] **D.2 SAX, BIT shadow-flag correctness.** Audit current BIT
      lowering against actual semantics: Z = (A & M) == 0, N = bit 7 of
      M, V = bit 6 of M. Audit SAX: M = A & X, no flag effect.

      **Acceptance:** diff harness green for both.

- [ ] **D.3 Shifts/rotates through carry.** ASL/LSR/ROL/ROR with carry
      propagation correctness across both the A and memory variants.
      Audit current `rt_asl_a` etc. against 6502 semantics.

      **Acceptance:** diff harness green for all 8 variants.

- [ ] **D.4 Indirect-Y carry crossing.** `LDA ($zp),Y` with Y producing
      a carry into the high byte. Runtime `rt_read_zp_ptr_y` already
      adds Y; verify correctness including high-byte carry and
      ZP wrap.

      **Acceptance:** diff harness green on a test sweeping Y from $00
      to $FF against a pointer near a page boundary.

- [x] **D.5 (different scope) PRG embedded in harness.** PRG bytes are
      now also placed at NES addresses ($8000..$FFFF) inside the
      harness's `z80_emu::FlatBus`. Lowered `LDA $9000,X` (and
      friends) now read real PRG bytes via the raw NES address —
      matching what the oracle sees. This unblocks every PrgRom-indexed
      validation that was previously skipped (~60 routines moved into
      validation). Real OAM-DMA-source check is a separate item, but
      the PRG-side validation is now in place.

- [ ] **D.6 Whole-SMB diff sweep.** Run Phase A.2 against the current
      SMB build with D.1–D.5 fixes. Capture remaining diffs.

      **Acceptance:** ≤5% of SMB routines fail validation. Each remaining
      failure has a triage note.

---

## Phase E — SMS runtime completeness

The runtime needs to fully model the NES hardware boundary in
SMS-native terms.

- [ ] **E.1 PPU CTRL ($2000) semantics.** SMB writes to $2000 to set NMI
      enable, sprite size (8x8 vs 8x16), background tile select, sprite
      tile select, VRAM increment (1 or 32). The runtime currently
      stores to shadow only. Make each bit affect SMS-side behavior:
      sprite size routes 8x16 to a different SAT format; tile select
      affects which CHR bank the SAT references; VRAM increment is
      observed by $2007 writes.

      **Acceptance:** synthetic test toggling each bit produces the
      corresponding visual change in Mednafen.

- [ ] **E.2 PPU MASK ($2001).** Background/sprite enable, monochrome,
      emphasis bits. SMS equivalents: VDP reg 1 bit 6 (display on/off),
      sprite enable via VDP reg 1.

      **Acceptance:** synthetic test toggling display on/off via $2001
      changes the SMS screen.

- [ ] **E.3 PPU STATUS ($2002) sprite-0 hit.** SMB uses sprite-0 hit
      to split the status bar from the playfield (separate scroll
      sections). Implement sprite-0 hit detection in the runtime: when
      the player sprite (sprite 0 in OAM) crosses a configured
      scanline, set the bit; the next `LDA $2002` returns it. SMS
      doesn't have sprite-0 hit hardware; emulate via a line interrupt
      at a fixed scanline.

      **Acceptance:** SMB status bar stays fixed at the top while the
      playfield scrolls horizontally below it.

- [ ] **E.4 PPU SCROLL ($2005).** Currently latches into shadow vars.
      Make the VBlank handler push them into VDP scroll registers. SMB
      uses fine-X for horizontal scroll, so split the scroll write into
      "coarse X to VDP H scroll" and "fine X via the scroll latch
      semantics."

      **Acceptance:** horizontal scrolling in 1-1 works in Mednafen.

- [ ] **E.5 PPU ADDR/DATA ($2006/$2007) raw-CIRAM source.** Currently
      `$2006` latches VRAM addr and `$2007` feeds the folded SMS projection.
      The long-term fix is still a real mirrored NES CIRAM source-of-truth plus
      a materializer into SMS nametable space, but internal-RAM reclaim is now
      deferred. `$CC00-$D2FF` has no unknown users, yet route diagnostics prove
      it is still blocked by true folded-S consumers (`_bgv_sub_palette`) and
      folded attribute maintenance. `$D300-$D3FF` is clean dirty-metadata space
      only, and `$DD80-$DFFD` remains stack no-go.

      **E.5a Raw-CIRAM storage backend.** Add a generic storage backend for at
      least 2 KiB of raw NES CIRAM outside the currently allocated internal RAM,
      likely cartridge/external RAM for v1. Do not reclaim `$CC00/$DA00` as part
      of this step.

      **Acceptance:** runtime helpers and `trace-sms` can read/write the chosen
      raw-CIRAM backend; SMB route remains green; docs state the emulator /
      hardware assumption; no folded-rendering behavior changes.

      **Outcome:** standard Sega mapper SRAM bank 0 in slot 2 is now the v1 raw
      CIRAM backend scaffold. Generated SMS projects emit
      `RAW_CIRAM_BACKEND_SRAM` / `$8000` / `$08` defines; boot initializes the
      Sega mapper registers and clears `$8000-$87FF` while SRAM is enabled;
      `runtime/ntmap.s` exposes unused read/write helpers; and `trace-sms`
      emulates `$FFFC` slot-2 SRAM mapping with two 16 KiB banks. The canonical
      route remains green and reports `raw_ciram_backend=sram_slot2 ...
      writes=2048 ciram_nonzero=0`, proving backend clear without visual
      behavior changes.

      **E.5b Raw-shadow parity.** Mirror `$2007` nametable tile and attribute
      writes into the raw-CIRAM backend while preserving the existing folded
      renderer as the visible authority.

      **Acceptance:** `nt_raw_shadow_parity=0` against trace-reconstructed
      CIRAM, route gate remains green, folded rendering diagnostics are
      unchanged, and no active-display materializer dependency is introduced.

      **Rejected direct-hot-path attempt:** calling the SRAM write helper from
      both direct `$2007` nametable tile and attribute paths produced perfect
      raw parity (`nt_raw_shadow_parity=0`, `trace_writes=31839`,
      `backend_writes=33887`) but failed the canonical route with final RAM
      `$0760=$00`, `$075C=$00`, `$000E=$08`. Treat this as timing/gameplay
      evidence: do not retry per-write slot-2 SRAM enable/write/disable on the
      hot path unchanged. The next E.5b design must avoid per-byte hot-path
      mapper toggles, batch work into render-off/load windows, or use a cheaper
      staging scheme before committing behavior.

      **Rejected D3xx staging-queue attempt:** using `$D300-$D3FE` as 85
      3-byte records plus `$D3FF` count, enqueueing every direct `$2007`
      nametable/attribute write, and flushing batches to SRAM while PPUMASK was
      off still failed the canonical route (`$0760/$075C/$000E = $00/$00/$08`).
      It reduced mapper toggles to 466 enable/disable pairs but introduced
      heavy internal-RAM queue traffic (`~87k` D3xx reads/writes), left 48 queued
      records, and parity was not clean (`nt_raw_shadow_parity=7`). Do not retry
      this enqueue-every-write design unchanged; even internal staging on the
      `$2007` hot path is too expensive for the accepted route.

      **Current decision:** pause E.5b runtime raw-CIRAM capture. Two distinct
      `$2007` interception designs produced the same gameplay-state regression,
      so further per-write capture is now considered a loop. Raw CIRAM remains
      the correct architecture, but the next fastest v1 step is a visual /
      human-playability audit of the current green build. Only return to runtime
      capture if that audit proves nametable/source correctness is the v1
      blocker, and then prefer producer-level bulk upload capture rather than
      `$2007` hot-path interception.

      **Visual audit result:** current checkpoint frames show no visual blocker
      for the scripted World 1-1 clear route. The status bar split is stable,
      1-1 terrain/columns are recognizable enough for v1 route play, and the
      flagpole/castle checkpoint is readable. Keep E.5b paused for v1 unless a
      broader-level requirement makes raw nametable correctness the active
      blocker. Treat raw-CIRAM/materializer parity and the post-transition 1-2
      visual state as post-v1/v2 work, not as blockers to claiming the current
      1-1 route.

      **Next v1 visual priority:** fix generic sprite/BG compositing and stale
      SAT/OAM artifacts. The clearest remaining in-level defects are Mario being
      partly swallowed/tinted by bushes at mid/late 1-1 checkpoints and small
      floating sprite fragments near mid-1-1 and the flag/castle area. Title
      menu clutter is also visible but lower priority than in-level readability.

      **Follow-up outcome:** the checkpoint PPM renderer now models SMS BG
      priority from nametable high-byte bit `$10` instead of applying NES OAM
      behind-background bit `$20` directly. The refreshed audit shows Mario is
      readable around the bush checkpoints and overall 1-1 route readability is
      improved. Runtime SAT-tail clearing was tried and reverted because the
      extra VDP writes missed the accepted 301M-step checkpoint budget; tail
      bytes after the SMS `$D0` terminator remain ignored by real rendering and
      by the trace PPM renderer. Remaining should-fix polish: the small 01600
      floating fragment, title/menu clutter, and any true SMS priority edge cases.

      **2026-07-04 transition-defect forensics (task: resume raw-CIRAM /
      E.5 work).** Dense Mednafen captures of the title→game transition
      show large stale title-screen fragments persisting into gameplay,
      plus the HUD scrolling with the playfield. The trace-sms
      checkpoints for the same route moments are CLEAN (proper WORLD 1-1
      intermission card, blanked screen, fixed HUD) — same ROM, so both
      symptoms are trace-vs-real-VDP semantic gaps, not translation
      bugs:
      - The HUD scroll is explained: the frame handler's overrun pacing
        read (`in a,($bf)`) clears the VDP's pending LINE interrupt along
        with the frame flag, and at ~8× budget overrun every frame is an
        overrun frame — the sprite-0-split line IRQ never services in
        stock Mednafen. At GPGX 500% (frames fit the budget) the split
        works. A per-scanline split is physically unserviceable when one
        handler spans ~8 real frames; treat stock-Mednafen HUD scroll as
        a slow-motion artifact, not a bug to fix.
      - ~~The stale title tiles need a real diagnosis~~ **RESOLVED
        (2026-07-04, same day): tooling artifact, not the ROM.** Two
        compounding input bugs: (1) the ROM never wrote I/O control
        port $3F, so controller input was DEAD in Mednafen (trace-sms
        doesn't model $3F) — fixed in boot.s ($3F=$FF at boot);
        (2) the capture scripts' key map was off by one (Mednafen SMS
        fire1/fire2 = SDL scancodes 90/91 = KP_2/KP_3), so every
        "Start" press was actually Select — all prior Mednafen
        "gameplay" footage was the attract demo, and the stale-tile
        frame was the demo disturbed mid-transition by Select mashing.
        With input fixed, a real Start press gives a CLEAN title→game
        transition in stock Mednafen (fixed HUD at rest, correct 1-1,
        zero stale tiles; savestate shows mode=01), and the untouched
        attract-demo transition is clean as well. The raw-CIRAM
        materializer is re-scoped to post-v1 (full-game nametable
        parity for world 1-2+ layouts), not a v1 transition fix.

      **E.5c Render-off-only materializer.** Project raw CIRAM into SMS
      nametable space only while NES rendering is off. Use `$D300-$D3FF` for
      dirty metadata when runtime dirty tracking is introduced. Active-display
      materialization remains blocked until stale-frame and cycle/VDP budgets
      are proven safe.

      **Acceptance:** route gate remains green; diagnostics prove no render-on
      materializer work; checkpoints after title/start are drained; vertical
      CIRAM projection mismatch improves at relevant checkpoints; framebuffer /
      checkpoint visuals are no worse.

- [ ] **E.6 OAM ($2003/$2004) and OAM DMA ($4014).** OAM addr/data
      register interface plus DMA. Currently runtime accepts $2003 to
      set OAM addr and $4014 to do a page DMA. Audit $2004 sequential
      writes from SMB (uncommon but possible).

      **Acceptance:** OAM-via-$2004 synthetic test produces the same
      SAT state as the equivalent DMA test.

- [x] **E.7 NMI / VBlank — basic wiring done.** `runtime/boot.s`
      `irq_handler` now invokes `call L_8082` (the translated
      NonMaskableInterrupt entry) after acking the VDP frame interrupt
      and flushing the VRAM update buffer. Per NES NMI semantics, the
      handler pushes the shadow status byte onto the emulated 6502
      stack before the call so the translated `RTI` balances cleanly.

      **Not yet acceptance-grade.** Visual verification of the SMB
      title screen requires X11 + screenshot capture which the current
      environment can't easily do remotely. The runtime + translated
      NMI path is plumbed; rendering correctness now depends on the
      remaining lower / runtime bugs in the queue.

- [ ] **E.8 Controller ($4016/$4017) full protocol.** Strobe write,
      serial bit reads. Latch SMS port $DC into a shadow byte on
      strobe high→low transition; subsequent reads return next bit.

      **Acceptance:** synthetic ROM that strobes and reads 8 buttons
      from controller 1 returns the correct bits when SMS buttons are
      pressed.

- [ ] **E.9 Sprite priority/palette/flip.** OAM attribute byte:
      bits 0-1 palette, bit 5 priority, bit 6 H-flip, bit 7 V-flip.
      Map to SMS SAT tile word bits.

      **Acceptance:** test sprites display with correct flip and
      palette in Mednafen.

      **Partial outcome:** runtime SAT upload now honors 8x8 sprite priority in
      trace rendering and generates 8x8 sprite pattern variants for NES OAM
      palette bits plus H/V flip. Variant scratch is guarded for the `$2000`
      SMS sprite base and falls back safely when PPUCTRL selects `$0000`; hidden
      sprites cannot consume visible variant slots. Verified through the SMB
      1-1 route, frame-diff, workspace tests, clippy, and oracle review. A
      standalone Mednafen synthetic sprite fixture is still needed before this
      item is fully closed.

- [ ] **E.10 8x16 sprites.** SMB doesn't use 8x16 for gameplay but
      title and some intros might. Implement: SAT writer handles
      8x16 by emitting two SMS sprites with consecutive tile indices.

      **Acceptance:** test sprite renders as 8x16 in Mednafen.

- [ ] **E.11 Background palette via attributes.** NES uses 16x16 pixel
      attribute regions selecting one of 4 background subpalettes.
      The runtime needs to translate attribute writes into SMS
      per-tile palette select bits.

      **Acceptance:** 1-1 background colors approximate the NES (sky,
      bricks, pipes, ground).

---

## Phase F — Audio

Approximate PSG output. Deferred until gameplay works because
gameplay is the bar; audio is a finish line.

Current unresolved-trap policy: the remaining `$F3xx-$F6xx` unresolved labels
are treated as sound-engine internals while `SoundEngine` is skipped/stubbed.
Do not close them by adding ordinary gameplay roots. This phase starts by
deciding the generic audio boundary, then either translating the sound engine
through an APU shim or replacing it with a profile/runtime-owned PSG event path.

- [x] **F.0 Sound boundary decision.** Choose and document whether v1 audio is:
      (a) translated NES sound engine plus a generic APU-write shim, or
      (b) profile/runtime replacement that emits PSG events directly. The choice
      must apply to NROM games generally, not only SMB.

      **Outcome (2026-07-04):** option (a). Full research + channel mapping +
      shim architecture + validation strategy in
      [`audio-plan.md`](audio-plan.md). Key findings: the SMS PSG clock is
      exactly 2× the NES CPU clock, so NES pulse periods convert bit-exactly
      (`N = P+1`; triangle `N = 2(P+1)` with octave-fold below ~109 Hz);
      the sound engine is the only profile `[[replacement]]`, so deleting the
      `rt_sound_stub` entry lets it translate through the normal pipeline and
      brings its RAM under the frame-diff parity gate (drop
      `FD_EXCLUDE_AUDIO`); the runtime APU shim emulates envelopes, length
      counters and sweeps at 4/2 ticks per frame and diffs a PSG cache to
      write only changes to port $7F.

      **Acceptance:** remaining `$F3xx-$F6xx` labels are either intentionally
      translated as part of the chosen sound-engine path or remain explicitly
      skipped behind a documented runtime/profile replacement. No silent
      unresolved traps during accepted gameplay routes.

**F.1-F.5 outcome (2026-07-04):** implemented as the audio-plan's
register-level shim rather than an event stream. The sound engine
translates through the normal pipeline (rt_sound_stub replacement
removed; 23 dispatch targets promoted; unresolved labels now 0 — every
discovered PRG byte translates). `runtime/apu_stub.s` is a full APU
model (register shadow, length/envelope/sweep sequencer at 4+2 ticks
per frame, PSG output stage with N=P+1 pulse pitch, octave-folded
triangle, 3-rate white noise, write cache). Fixed on the way, each
generic: high-PRG `(zp),Y` reads (music note streams at $F800+);
indexed APU stores (`STA $4002,X` had folded to the base register,
muting square2/triangle/noise); STX/STY-to-APU stores (silently
dropped); A-clobbering in `rt_write_indexed`'s new hardware-window
forwarding; fixed-point section assignment (a dry-pass/real-pass drift
made a near call cross banks); z80_emu gained `LD (HL),n` and
`LD A,I/R`. Validation: full-route frame-diff is NO DIVERGENCE with
sound RAM included (the reference gained a matching APU length model
and buffered $2007 CHR reads; only $07B5/$07B7 — sequencer-phase
latches — are excluded, documented in `is_excluded`). trace-sms logs
PSG writes; the stream shows the multi-channel theme (melody+harmony
paired decays, triangle bass, noise hats).

- [x] **F.1 APU write log → event stream.** Superseded by the
      register-level shim above.

- [ ] **F.2 Pulse 1 / Pulse 2 → PSG tones.** NES pulse channels map
      cleanly to PSG channels 0 and 1. Period conversion: NES period
      register vs PSG period register. Duty cycle is lost on PSG;
      approximate.

- [ ] **F.3 Triangle → PSG tone.** PSG channel 2 plays a tone at the
      triangle frequency. Timbre is wrong; pitch is right.

- [ ] **F.4 Noise → PSG noise.** SMS PSG noise channel approximates.

- [ ] **F.5 DMC stays silent.** Document in `apu_stub.s`.

- [ ] **F.6 Music engine.** Replay APU events on PSG each frame.
      `rt_apu_write` calls a dispatcher that updates PSG state.

      **Acceptance:** SMB main theme is recognizable on PSG.

---

## Phase G — Visual verification harness

Going from "Mednafen loads it" to "Mednafen shows what we expect."

- [x] **G.1 Mednafen screenshot capture — basic.** Added xvfb +
      xdotool + imagemagick to the docker image, plus
      `poc/scripts/capture_sms.sh` that launches mednafen under a
      headless display, finds the mednafen window by class, and
      captures frames via `import -window`.

      **Outcome:** the script works and captures real frames. Current
      SMB ROM renders an all-black screen because the placeholder name
      table points to tile 0 (which decodes to a black tile in SMB CHR)
      and the translated NMI's PPU `$2006/$2007` writes either don't
      run or don't produce visible content yet. Next debug step needs
      a way to know whether translated code is actually executing —
      probably either Z80 tracing or a `rt_unresolved_jsr` trap that
      writes a recognizable pattern to CRAM.

- [ ] **G.2 Frame diff utility.** A small Rust tool that compares two
      PNGs with a perceptual-hash threshold. Outputs pass/fail and a
      diff image.

- [ ] **G.3 Golden frames.** Capture reference PNGs from real SMB on
      Mesen at key checkpoints: title (frame 60), title with cursor
      moved (frame 120 after Select), game start (frame 60 after
      Start), 1-1 visible (frame 30 after game start), Mario standing
      idle, Mario walking, Mario jumping, Goomba visible.

- [ ] **G.4 Continuous visual regression.** A test that runs the SMS
      ROM through Mednafen and compares to each golden frame at the
      expected tick. Add to CI (or at least to `cargo test`).

      **Acceptance:** every Phase H checkpoint has a golden frame and
      a passing regression test.

---

## Phase H — SMB drive-through

The phases of the master plan, now backed by the diff harness, the
hardware shim, and the visual regression suite.

- [ ] **H.1 Title screen.** Reset → boot → translated reset → SMB
      init → title rendered. Mednafen shows the SMB title logo and
      "1 PLAYER GAME / 2 PLAYER GAME" with the mushroom cursor.

      **Acceptance:** golden frame `title_initial.png` matches within
      perceptual threshold.

- [ ] **H.2 Title menu cursor.** Pressing SMS button 1 (NES Select)
      moves the cursor. Pressing SMS button 2 (NES Start) transitions
      to game.

      **Acceptance:** golden frames `title_cursor_2p.png` and
      `start_transition.png` match.

- [ ] **H.3 Initialization.** Translated `InitializeGame`,
      `LoadAreaPointer`, `FindAreaPointer`, `GetAreaDataAddrs`.
      State after init: world 1, level 1, area pointer resolved to
      `L_GroundArea6`.

      **Acceptance:** diff harness validates these routines; runtime
      RAM at the expected post-init state.

- [ ] **H.4 Area parser → 1-1 first screen background.** Translated
      `AreaParserTaskHandler`, `AreaParserCore`, `ProcessAreaData`,
      `DecodeAreaData`. Mario's starting screen renders with sky,
      ground, scenery, bricks, pipes, question blocks.

      **Acceptance:** golden frame `world_1_1_initial.png` matches.

- [ ] **H.5 Player sprite static.** Mario appears at his start
      position. Sprite tiles correctly assembled from CHR.

      **Acceptance:** golden frame matches with Mario visible.

- [ ] **H.6 Player movement.** Controller input → `PlayerCtrlRoutine`
      → position changes. Left/right walks. A button jumps. B button
      runs (when held). Sprite animation cycles.

      **Acceptance:** input script `walk_right.txt` (hold right 60
      frames) produces a golden frame at the expected new position.

- [ ] **H.7 Collision and blocks.** Stand on ground. Bump question
      block from below — coin pops, block becomes empty. Hit brick
      — bounces (when small) or shatters (when big).

      **Acceptance:** golden frames for each collision case.

- [ ] **H.8 Enemies — Goomba.** First Goomba in 1-1 spawns at correct
      column, walks left, collides with Mario.

      **Acceptance:** golden frames for Goomba spawn, walk, stomp,
      and Mario death.

- [ ] **H.9 Scrolling.** Walking right past column 16 scrolls the
      screen. New columns stream in from the right.

      **Acceptance:** golden frames at scroll positions 32, 96, 160.

- [ ] **H.10 Status bar.** Score, coins, world/level, time,
      lives all update correctly. Status bar stays fixed via E.3
      sprite-0 hit.

      **Acceptance:** time decrements; score increments on coin
      collection.

- [ ] **H.11 Flagpole and end-of-level.** Touch flagpole → slide
      down → walk to castle → "WORLD 1-2" transition card.

      **Acceptance:** input script that completes 1-1 produces the
      transition.

- [ ] **H.12 Underground (World 1-2).** Underground area type renders.
      Pipe enters/exits work.

      **Acceptance:** golden frame `world_1_2_initial.png`.

- [ ] **H.13 Water area type.** World 2-2 or 7-2 swimming. (Optional
      for v1; required for "complete game.")

- [ ] **H.14 Castle area type.** World 1-4 lava, bridges, Bowser
      sprite. Bowser AI.

- [ ] **H.15 Power-ups.** Mushroom, Fire Flower, Star, 1-Up.
      Mario state transitions (small → big → fire).

- [ ] **H.16 Game over.** All lives lost → game over screen → reset
      to title.

- [ ] **H.17 World clear.** Beat all eight worlds → ending screen →
      "THANK YOU MARIO! YOUR QUEST IS OVER…"

- [ ] **H.18 Warp zones.** Pipes leading to warp rooms function.

- [ ] **H.19 Two-player mode.** Toggle on title; alternate turns.

      **Acceptance for "complete":** H.1 through H.17 all pass golden
      frame regression. H.18 and H.19 are nice to have.

---

## Phase I — Mappers beyond NROM (stretch)

For "any NES ROM" instead of "SMB specifically." Out of scope for
SMB-complete but in scope for the pipeline being generalizable.

- [ ] **I.1 UxROM (mapper 2).** PRG banking via writes to $8000-$FFFF.
      Map to SMS bank registers. Mega Man as reference target.

- [ ] **I.2 MMC1 (mapper 1).** Serial register, PRG/CHR banking,
      mirroring. Zelda / Metroid as references.

- [ ] **I.3 MMC3 (mapper 4).** PRG/CHR banking plus scanline IRQ.
      SMB3 / Mega Man 3 as references. Scanline IRQ → SMS line
      interrupt.

---

## Phase J — Polish

- [ ] **J.1 Second NROM game.** Balloon Fight or Ice Climber through
      the same pipeline with a different profile. Forces removal of
      any latent SMB assumptions in Rust source.

- [ ] **J.2 Performance audit.** SMS runs at 3.58 MHz Z80 vs NES 1.79
      MHz 6502. Conservative Z80 should fit, but profile to confirm.
      Optimize the hottest 10 runtime helpers if needed.

- [ ] **J.3 Documentation pass.** Update README, master-plan,
      completion-plan to reflect final state. Tutorial: "how to add a
      new NROM game."

- [ ] **J.4 Release build.** Cut a v1.0 tag with the final `sms.sms`
      committed (or buildable from a public profile + scripts).

---

## Working notes

Add discoveries that change priorities here, dated.

### 2026-06-25 — Unresolved traps reduced to deferred sound only

- Current SMB generation reports **631 discovered/lifted routines**, **0 lift
  failures**, and **0 lower failures**.
- The unresolved strict-trap set is now **22 labels**, all in the deferred
  `$F3xx-$F6xx` sound-engine area. Non-sound gameplay dispatch gaps were closed
  through profile-root fixes rather than Rust-side SMB special cases.
- The profile-owned 1-1 acceptance route passes under `trace-sms` with
  `--expect-no-trap` and post-transition RAM checks for `$0760`, `$075C`, and
  `$000E`.
- `cargo test --workspace` passes for this state.
- Next safest work is either external emulator validation of the accepted route
  or a deliberate audio/APU-to-PSG phase. Do not promote the remaining sound
  labels as ordinary gameplay roots before that audio plan exists.

### 2026-06-27 — Sprite variants and Mednafen smoke refreshed

- Committed generic runtime sprite variants for NES OAM palette bits and H/V
  flip, with safe fallback for `$0000` sprite base and hidden-sprite cache
  protection. SMB remains a stress target; the runtime logic is attribute- and
  PPUCTRL-driven rather than SMB-specific.
- The profile-owned 1-1 route now uses a **301,000,000** step budget after the
  sprite variant timing change and still passes the no-trap/post-transition RAM
  checks.
- External Mednafen smoke was rerun against the workspace ROM (`out/smb/sms.sms`)
  and passes: Mednafen recognizes it as SMS, Sega mapper, export territory, and
  a 304KiB generated ROM. The legacy smoke script no longer hard-codes 256KiB.
- Next safest work is visual equivalence: inspect the refreshed checkpoint frames
  and/or add a proper golden-frame comparison path before starting the deferred
  audio phase.

### 2026-06-29 — Current green build visual audit

- Audited the refreshed `out/smb/checkpoints/1-1-clear/*.ppm` and matching text
  checkpoint artifacts for the current green route after pausing raw-CIRAM
  capture.
- No visual blocker was found for scripted World 1-1 clear human-readability:
  the HUD/split is stable, route state stays live, 1-1 terrain/columns are
  readable, and the flagpole/castle checkpoint is recognizable.
- The most visible v1 polish targets are generic rendering/runtime issues:
  Mario/background compositing around bushes and small stale/floating sprite
  fragments. Title-screen menu clutter is lower priority.
- Keep E.5b raw-CIRAM runtime capture paused for v1. The materializer/raw-CIRAM
  mismatch class and incorrect post-transition 1-2 visuals remain important but
  are post-v1 unless the target expands beyond a World 1-1 clear route.
- Follow-up trace-renderer priority fix improved the gameplay checkpoint PPMs:
  Mario is now readable around bush areas and 1-1 route landmarks remain clear.
  SAT-tail runtime clearing was rejected because it missed the 301M-step
  checkpoint budget; post-terminator SAT bytes are diagnostic noise, not a visual
  blocker. Remaining visual should-fix items are the 01600 floating fragment and
  title/menu clutter.

### 2026-07-03 — Full-project audit and re-prioritization

- Full audit recorded in [`audit-2026-07-03.md`](audit-2026-07-03.md). The
  entire green baseline was re-executed from scratch, not read from docs:
  374 workspace tests, 631/631 lift+lower, WLA-DX assembly, and the 1-1-clear
  acceptance route all pass.
- Architecture verdict: keep. No rewrite; the profile-driven static
  recompilation + runtime shim + layered differential validation design is
  working and engine crates remain game-agnostic (comments-only SMB mentions).
- **Biggest v1 risk identified: real-emulator timing.** `trace-sms` fires the
  frame IRQ every 60,000 Z80 *instructions*; real SMS hardware affords
  ~59,736 *cycles* ≈ 8-10K instructions per frame. The accepted route runs
  with ~6-8× more CPU per frame than Mednafen will provide. Next work is
  therefore re-ordered: (1) Mednafen-in-the-loop verification (Phase G),
  (2) per-frame cost measurement in `trace-sms` + performance burn-down by
  defect class, (3) v1 visual polish, (4) audio Phase F, (5) CDL-based
  genericity proof with a second NROM game.

### 2026-07-03 — Mednafen black-screen root causes found; title screen renders

The user-facing acceptance test (run the ROM in stock Mednafen) showed a
permanent black screen while `trace-sms` showed a full game. Diagnosed via a
new savestate-forensics loop (headless Mednafen + xdotool F5 + a Python
chunk parser for the `.mc*` format) plus boot progress markers and a
`replay-state` tool that transplants Mednafen's exact machine state into
`z80_emu`. Three generic platform bugs, all invisible to the lenient trace
harness:

1. **Reset vector overlap.** `reset_entry` (`di; im 1; ld sp; jp`) was 9
   bytes; the `jp`'s last byte collided with the `.org $0008` RST trap — the
   long-ignored wla `MEM_INSERT` warning. The winner of the byte conflict
   varied by build. Fixed by shrinking the reset block to 6 bytes (SP init
   moved into `boot_main`).
2. **Spurious IRQ-handler entry during boot.** Mednafen was observed
   accepting an interrupt a few instructions after reset's `di`. The
   handler's unconditional `ei; ret` exit then left interrupts enabled for
   the rest of boot; the per-frame handler (~0.5-3 frames of work) starved
   the main thread so boot/init never completed. Fixed with a runtime-ready
   flag (`$CB1B`): until boot's final step, irq_handler acks the VDP and
   returns *without* `ei`.
3. **Stack pushes reprogrammed the Sega mapper.** SP started at `$DFFE`;
   the mapper registers `$FFFC-$FFFF` are RAM-mirrored at `$DFFC-$DFFF`
   (Mednafen honors mapper writes on the mirror), so every top-level `call`
   rewrote the slot-0/SRAM bank registers under the running code. This is
   why real SMS software conventionally sets SP=`$DFF0`. Fixed: SP=`$DFF0`;
   `trace-sms` now models the mirror so this class can't hide again.

Also added this session: frame-overrun pacing in `irq_handler` (ack any
already-pending frame INT before exit so an over-budget handler still
yields the main thread one clean frame per cycle), per-frame handler cost
accounting in `trace-sms` (`frame_handler_cost`/`frame_budget` summary
lines), an opt-in `DEBUG_BORDER_HEARTBEAT` in boot.s, and the
`replay-state` diagnostic binary.

**Result: stock Mednafen now renders the full SMB title screen** (logo,
menu, HUD, Mario idle scene) from the generated ROM — first external-
emulator rendering in project history. Timing: title appears after ~15 s
wall (vs ~1 s on NES), consistent with the measured ~8.7× average NMI
budget overrun; the speed story remains the Phase-G/optimization-findings
overclock discussion. Measured with the new accounting: median frame
handler cost 523K approx-cycles vs the 59,736-cycle NTSC budget; 99.7% of
route frames over budget.

### 2026-07-04 — Flag-liveness soundness fix; full-route NES diff drives bug hunt

User-reported in-game defects (blocks not paying out, sky glitches) are now
being burned down with a wholesale differential workflow: `frame-diff` over
the full ~4900-frame 1-1 route with `FD_WATCH=all` + `FD_DEBUG_FRAME=N`
logs every ordered RAM write (addr, value, PC) on both sides; the first
mismatching write names the exact diverging instruction.

- **Generic lowering soundness bug found and fixed (the block/coin bug):**
  `flags_live_after` walked ops linearly, ignoring branch-taken paths and
  treating tail jumps as flag death. SMB's `BlockBumpedChk` returns its
  answer in CARRY via `CMP; BEQ done; ...; CLC; done: RTS` — the fused
  native `cp; jp z` elided the shadow-carry write on the match path, so
  the caller's `BCC` read stale carry and coin blocks silently did not pay
  out (first divergence frame 1609: block-buffer write identical, then
  the whole coin/score chain missing). Fix: pending flags stay live
  across any conditional branch and any tail jump. Route divergence fell
  from 3292/4900 to 1204/4900 frames.
- **VDP critical-section bracket (sky-glitch class):** translated code
  reaches VDP ports through `rt_ppu_write`/`rt_ppu_read` on the main
  thread while the frame IRQ handler also writes VDP (SAT/scroll/vbuf).
  An IRQ between the two bytes of a control-port pair corrupts the shared
  address latch. Both entry points now run under a stateless DI bracket
  (`ld a,i` capture; `ei` on exit only if interrupts were enabled), ~40
  cycles overhead, nesting- and handler-safe. `z80_emu` gained `LD A,I`/
  `LD A,R` with IFF2→P/V for this.
- **frame-diff harness fixes:** `FD_WATCH=all` write-sequence logging;
  audio exclusion extended to `$07B0-$07CF` ($07CA is SoundEngine-written);
  pre-roll cap raised (bracket overhead pushed init past 8M instructions);
  pre-roll now settles until IFF1 re-enables so frame 0's IRQ isn't
  swallowed (NMI-enable is detected inside the DI bracket) — this had
  phase-shifted every snapshot and shown a false 4900/4900 divergence.
- **Acceptance route script found stale-by-improvement:** the reference
  NES run under `1-1-clear.buttons` dies and reaches game-over (~frame
  2899) — the script was recorded against the old, buggy translation and
  never cleared 1-1 under real-NES dynamics. Now that the translation
  tracks the NES, it faithfully reproduces the death, so the old
  `$0760/$075C/$000E` end-state expectations no longer hold. The route
  script and expectations must be re-recorded against the NES reference
  (ref-side scripting via frame-diff); until then the trace-sms route
  gate is known-red for end-state values (no-trap still holds).
- Remaining top divergence: OAM sprite bytes `$0204-$0220` from frame
  1792 (~844 frames) — next target for the same write-diff workflow.

**Follow-up outcome (same day): NO DIVERGENCE across the full route.**
Three more fixes took the 4900-frame diff from 1204 diverging frames to
**zero — the SMS RAM trajectory is byte-for-byte identical to the real
NES** over the whole route (title, demo, gameplay, death, game-over,
return to title):

1. `frame-diff` reference gained buffered `$2007` PPUDATA reads from CHR
   ROM (address latch, 1-byte read buffer, increment). SMB's
   DrawTitleScreen reads its title layout from CHR through `$2007`; the
   reference returned zeros, which explains the months-old "frame 22
   divergence" — the reference was wrong, not the subject. Also narrowed
   `FD_EXCLUDE_VRAMBUF` to `$0300-$03C3`: `$03C4-$03FF` (sprite-shuffle
   offsets, block-object state) is real game state.
2. `rt_asl_mem` / `rt_lsr_mem` clobbered A; 6502 memory-RMW ops preserve
   it (the rol/ror/inc/dec helpers already pushed AF — these two missed
   the earlier fix). Sprite-hide rows (`JSR DumpTwoSpr` with A=$F8 after
   `LSR $00`) received garbage instead.
3. Fused CMP+branch elision now also checks flag liveness at each fused
   branch's TAKEN path (via an in-routine label map). SMB's
   PlayerInjuryBlink does `CMP #$F0; BCS t; CMP #$C8; ...; t: BNE` — the
   linear scan saw the second CMP overwrite and elided the shadow write,
   but the taken path consumes the first CMP's Z at `t`. The
   `cmp_beq_fuses_to_native` test now asserts the sound rule in both
   directions (fusable when both paths overwrite; not fusable when the
   taken path reaches RTS).

The acceptance route re-record (against the now-authoritative NES
reference) is the remaining step to restore the trace-sms route gate:
with full parity, the old script faithfully reproduces the death the
real NES suffers under it.

**Route re-recorded (same day): trace-sms gate green again.**
`frame-diff` gained `FD_REF_ONLY=1` — a compact per-frame reference
trajectory printer (player page:x, y, state, lives, world/level/area) —
which turns script authoring into a fast iterate-on-the-NES loop. The
old script's first death (goomba pair at x=$0799/$07D8, reached with a
jump timing that the coin-block bounce fix invalidated) was fixed with
a retimed double jump (frames 1698/1745); input is released at 3950
after the flag grab (~3949) and a single hop at 4876 dodges 1-2's first
goomba so Mario idles safely at the 1-2 start. `trace-sms` now counts
script/checkpoint frames from SMB's NMI enable ($CB08 bit 7) — the same
convention as frame-diff — so one recorded script drives both harnesses
identically regardless of boot length. New route expectations:
`$075C=01` (level 2), `$0760=02` (area), `$075A=02` (no deaths); all
green plus no-trap on the current build. Checkpoints retimed
(3940 flagpole, 4700 post-transition-1-2).

---

## Recently fixed (this turn)

- **Flag-liveness elision (`nz_flags_live_after`).** Forward-scan in
  `lower_routine` skips `call rt_set_nz_a` whenever the NZ flags
  from an op are dead before the next NZ-overwriter or routine
  boundary. Cuts ~60% of SET_NZ_A calls in SMB.
- **Two-pass lowering with `prepopulate_label_section`.** A dry pass
  builds the label→section map before real lowering so forward
  references can downgrade `far_call`/`far_jmp` to plain `call`/`jp`
  when both ends sit in the same section. Reduces br_skip pattern
  from 442 sites to 162.
- **`Op::BranchIf` cross-bank handling.** Branches to labels in a
  different section now emit `bit n,(hl); jp z/nz, _br_skip;
  far_jmp target; _br_skip:`. Local branches (same section or
  in `routine.branch_labels`) stay as plain `jp_z`/`jp_nz`.
- **`LiftOptions::extra_label_pcs`.** Pipeline pre-scans all routines
  to collect every `L_XXXX` branch target, then each routine is
  lifted with the set of PCs inside its range that *other* routines
  reference. The lifter emits a real label at each, so a `BEQ $85C8`
  from one routine resolves to the actual `$85C8` site inside the
  next routine (previously it landed on the unresolved-jsr stub).
- **Unresolved stubs: no-op + INC $C73C.** Profile-named functions
  the analyzer can't lift used to compile to `jp rt_unresolved_jsr`
  (infinite CRAM-flash trap). Now each emits a 4-instruction stub
  that advances the ScreenRoutines sub-task counter, letting the
  title-screen state machine keep moving when a sub-task isn't
  implemented. Trade-off: skipped sub-tasks don't draw their work
  but the dispatcher advances.
- **9 new ScreenRoutines sub-task entries in `profiles/smb.toml`.**
  Extracted the inline `.dd2` table at `$856D` and added
  SetupIntermediate, WriteTopStatusLine, WriteBottomStatusLine,
  DisplayIntermediate, ResetSpritesAndScreenTimer,
  AreaParserTaskControl, GetBackgroundColor, GetAlternatePalette1,
  ClearBuffersDrawIcon as `[[function]]` entries. Functions count
  went from 490 → 501.

- **Bank-aware call trampolines (`rt_far_call` / `rt_far_jmp`).**
  Each translated `JSR L_XXXX`/`JMP L_XXXX` now becomes a 6-byte
  sequence (`call rt_far_call; .dw tgt; .db :tgt`) that saves the
  current slot-1 bank to the Z80 stack, switches to the target bank,
  runs the target, then restores. A bank shadow at `$CB14` tracks the
  current bank for nested restores. JumpEngineCall dispatches use
  `cp/jp nz, skip; far_jmp; skip:` chains so each conditional branch
  can cross banks. `z80_emit::Program` gained `far_call`/`far_jmp` plus
  a `referenced_labels` set so the unresolved-stub generator picks up
  profile-only target names (PrimaryGameSetup, AreaParserCore, etc.).
  Same-section targets downgrade to plain `call`/`jp` via a
  `label_section` index. **Result:** `$C772` (operation-mode index)
  now advances `0 → 1` per NMI — InitializeArea reaches its
  `INC $0772 / RTS` instead of crashing on a wild PC. Translated SMB
  is functionally executing the title-screen state machine end-to-end.
- **Profile gap: 4 missing JumpEngine sites.** A PRG scan found 18
  `JSR JumpEngine` call sites in SMB; the original profile only had
  14. Added the missing entries ($92C8 AreaParserTasks, $B34E,
  $BDBD, $C88F) along with their target tables. Without these the
  un-profiled JumpEngine bodies fell through to the literal Z80
  translation which PLA-popped garbage from the emulated 6502 stack.
- **Boot-time bank-switch race (the visual-output unlock).** boot.s
  used to `ei` *before* mapping slot 1 to `:translated_reset`'s bank.
  If the first frame interrupt fired in those few instructions, the
  irq_handler's `call L_8082` jumped into bank 1's $40FB (random
  data) and PC went wild. Reordered: switch first, then `ei`, then
  `jp $4000`.
- **vdp_init early-return on $FF value (regs 2-10 never written).**
  The init table's loop checked the *value* byte for the $FF sentinel.
  Registers 2-5 take $FF as a legitimate value (name-table /
  pattern-table / SAT base addresses in Mode 4), so the loop bailed
  at reg 2 and never set scroll/sprite-base/border. Fixed by checking
  the *register* byte for $FF instead.
- **Asset data pinned to dedicated slot-2 banks.** `data_chr`,
  `data_palette`, `data_nametable` were placed `superfree`, which left
  their symbols at in-bank slot-1 offsets — boot's `ld hl, data_xxx`
  read garbage. Now each asset gets its own `.bank N slot 2 / .org 0 /
  .section ... force` block so the symbol value is a clean $8000-range
  address; boot just `ld a, :data_xxx; ld ($ffff), a` to swap the bank
  in. Verified by trace_sms: CRAM now holds the real palette
  ($00,$15,$2A,$3F BG + $00,$10,$20,$3F sprite) and 6231/14336 tile-
  pattern bytes are loaded.
- **z80_emu exchange ops + shadow regs.** Added EX DE,HL ($EB), EX
  AF,AF' ($08), EXX ($D9), EX (SP),HL ($E3), JP (HL) ($E9). Added
  af_shadow/bc_shadow/de_shadow/hl_shadow fields to `Cpu`. Without
  EX DE,HL the very first translated PHP/PLP-like sequence stopped
  the trace at $0621.
- **JumpEngine dispatch (B.4 closed).** Added `Op::JumpEngineCall {
  targets: Vec<String> }` to IR and a `[[jump_engine]]` table to the
  profile schema. The lifter substitutes `JSR JumpEngine` at each
  profile-listed call site with the dispatch op; the lowerer emits a
  `cp/jp z` chain over A (last target a fall-through `jp`). 14 SMB
  call sites resolved (mode dispatch + screen-routine dispatches).
  This unblocks the title-screen → game-mode transition that used to
  fall into JumpEngine's `pla/pla` and corrupt the Z80 return stack.
- **CB-prefix completeness.** Rewrote `step_cb` in `z80_emu` as a
  unified r/n/category decoder. Now supports all of RLC/RRC/RL/RR/SLA/
  SRA/SLL/SRL on B/C/D/E/H/L/(HL)/A, and BIT/SET/RES n on all eight
  operands. Previously only A-form and (HL)-form ran; `rt_controller_
  latch` (which uses `bit n, b`) hard-stopped the trace.
- **RET cc completeness.** Added missing conditional returns RET PO/
  PE/P/M (0xE0/E8/F0/F8) — z80_emu was rejecting them, breaking
  ordinary translated control flow.
- **trace-sms periodic IRQs + xfer ring + jump ring + asset peek.**
  Now injects an IRQ every 60k Z80 steps (was: single IRQ at step
  60000), prints the last 32 CALL/RET control transfers, an
  8192-entry instruction ring, a 16384-entry coalesced non-sequential
  jump ring, the full I/O log, and a CRAM/SAT/nametable/tile-pattern
  peek with non-zero counts. Made all the above bugs visible.
- **smoke_sms.sh** now asserts 256 KB / Sega mapper instead of the
  legacy 32 KB / unbanked PoC build.
- **`z80_emu` ED-prefix support.** Added IM 0/1/2, NEG, RETN/RETI,
  LD (nn),rr and LD rr,(nn) variants, SBC HL,rr, ADC HL,rr, LDIR/LDDR/
  LDI/LDD, OUTI/OTIR/INI/INIR/OUTD/OTDR/IND/INDR. Without these the
  trace tool stopped at the very first ED byte (boot's `im 1`).
- **SMS trace tool** at `crates/cli/src/bin/trace_sms.rs`. Loads a
  built `.sms` ROM into `z80_emu`, simulates Sega-mapper bank
  switching + minimal VDP, runs N instructions, reports PC histogram,
  call counts, VRAM-write count, and bus state. Used to drive the
  bank-mapping debugging below.
- **Bank-aware section placement.** `z80_emit` got
  `set_section_placement(bank, slot)`; cli pins each `generated_code_N`
  section to bank `4 + N` slot 1. Without this, `superfree` symbols
  resolved to in-bank offsets, so `jp translated_reset` became
  `jp $0000` (back to reset_entry) — a perfect infinite loop.
- **Boot.s bank-switch + jump.** Before `jp $4000` we map the right
  bank to slot 1 via `ld a, :translated_reset; ld ($fffe), a`. Same
  trick for `data_chr` (uses slot 2 / `($ffff)`).
- **Trace result.** Translated code now executes. Shadow X = $FF
  (SMB's `LDX #$FF; TXS` ran), shadow P = $26 (`SEI` ran), 104 K
  calls to `rt_set_nz_a` in 5 M Z80 steps — boot reached translated
  reset and is doing real work.

## Status (live)

Started: 2026-05-20.

**Phase A complete.** Bug classes B1–B10 fixed in `lower` and `z80_emu`.

**Phases B.1, B.3, B.4 (partial), D.1, D.5 (partial), E.7 (partial),
G.1 (basic) done.**

Latest harness/runtime improvements:
- Auto-inserts an `Op::Jmp` tail-call when a lifted routine has no
  terminator and is followed by another function — preserves
  fall-through semantics in both harness and emitted SMS code.
  Reduced "no terminal" skips from 245 → 18.
- PRG embed in z80 bus limited to NES $8000-$BFFF so it doesn't
  collide with SMS RAM at $C000+. Fixed Setup_Vine's 3/32 result.
- Mednafen screenshot capture via xvfb + xdotool +
  `poc/scripts/capture_sms.sh`.

SMB current stats:
- **490 functions discovered**, 0 lift failures, **0 lower failures**.
- PRG classification: 56% code, 11% data, 33% unknown.
- ROM: 256 KiB, builds via WLA-DX, loads in Mednafen.
- Validation: **106 green / 13 red / 371 skipped (of 490)**. The 13
  reds are mostly routines reading from upper-PRG ($C000+) — that
  range can't be embedded in the harness without colliding with SMS
  RAM, so the read returns garbage and diffs.
- Visual: ROM produces an all-black screen in Mednafen. The display
  is enabled (boot.s sets VDP reg 1 = `%11100000`), but the placeholder
  name table points to tile 0 and translated NMI's PPU writes either
  don't run or don't produce visible content yet.

Currently in: **Phase E** (runtime semantics) and **Phase G** (visual
verification).

**Root cause of "title screen never renders" identified: cross-bank
calls land in the wrong physical bytes.** The translated NMI handler
at `$8082` (Z80 bank 4 / `$40FB`) calls e.g. `L_F2D0` which the
linker placed in bank 9 (`$526A` logical) — but the Z80 `call $526A`
goes to whatever bank is currently mapped into slot 1, which is bank
4 (translated_reset's bank). The CPU lands mid-routine in bank 4's
`$526A`, executes garbage bytes until a stray `ret` pops, corrupting
state along the way. Trace evidence: `call $526A: 49 times` exactly
matches the per-frame `call L_F2D0` count, but `L_8212`
(PerformOperation) — which the NMI is *supposed* to call after
`$80E4 JSR $F2D0` returns — is never reached. So `$0772`
(operation-mode index) stays `$00` forever, dispatch keeps re-entering
`InitializeGame`, and the title screen never gets drawn.

**Fix implemented: bank-aware call trampolines.** Added `rt_far_call`
and `rt_far_jmp` to `runtime/dispatch.s`. Each translated `JSR
L_XXXX` now becomes a 6-byte sequence (`call rt_far_call; .dw tgt;
.db :tgt`) that saves the current slot-1 bank, switches to the target
bank, calls the target, and restores on return. JMPs use `rt_far_jmp`
which switches without pushing a restore frame. A bank shadow at
`$CB14` tracks the current slot-1 bank for nested restores.
JumpEngineCall dispatches use `cp/jp nz, skip; far_jmp target; skip:`
chains so each conditional branch can cross banks. `z80_emit::Program`
gained `far_call/far_jmp/emit_far` and a `referenced_labels` set so
the unresolved-stub generator picks up profile-only target names.

**Result: `$C772` advances from 0 → 1 the first time the NMI runs.**
InitializeArea reaches its final `INC $0772 / RTS` instead of looping
on a wild PC. Translated code now correctly traverses cross-bank
function boundaries.

**New blocker: NMI throughput.** Each translated NMI does ~20 M Z80
cycles of work — far beyond the ~60 K cycles available per real
VBlank window. Each NES instruction expands to 5-20 Z80 instructions
(flag emulation + runtime helpers); far-call overhead adds ~25
cycles per cross-bank JSR. The translated NMI performs hundreds of
cross-bank calls and is doing legitimate work (write nametable,
advance sub-task state) but never completes within one frame.

**Mitigation in place: same-section call downgrade + two-pass
lowering.** `Program` records each label's defining section index;
`far_call`/`far_jmp` check it and emit a plain `call`/`jp` when the
target lives in the current section. A two-pass lowering seeds the
section map from a dry run so forward references benefit too:
br_skip patterns are now 162 (was 442), plain jp_z/jp_nz to L_xxx
are 1139 (was 999). Same-section catches both forward and backward
references regardless of section size. `BranchIf` also checks the
section map and skips the trampoline skip-around when the target is
local.

**Flag-liveness elision.** `lower_routine` now runs a forward
dataflow pass per routine (`nz_flags_live_after`) and skips
`call rt_set_nz_a` whenever the NZ flags from an op are dead before
the next NZ-overwriter or routine boundary. Cuts ~60% of the
SET_NZ_A calls in SMB; bytes went from 62k → 56k.

**Cross-routine branch targets resolved.** `LiftOptions::extra_label_pcs`
lets the pipeline pass per-routine "labels someone else branches to
that fall inside our range." Without this, a `BEQ $85C8` from
InitScreen lands at an unresolved-stub even though $85C8 sits
mid-body in SetVRAMAddr_A.

**Unresolved-stub semantics changed: no-op + INC $C73C.** Previously
each unresolved profile name (e.g. SetupIntermediate) compiled to
`jp rt_unresolved_jsr` which dropped the CPU into an infinite
CRAM-flash loop. Now each emits `ld hl,$C73C; call rt_inc_mem; ret`,
which advances ScreenRoutines' sub-task counter so the state machine
keeps moving when a sub-task isn't implemented. Visually degraded
(the missing routine's drawing work is skipped) but the title-screen
state machine reaches its later stages instead of stalling at task 1.

**Profile completeness: 9 new ScreenRoutines sub-task entries.**
Extracted the inline `.dd2` table at `$856D` and added
SetupIntermediate ($859B), WriteTopStatusLine ($8652),
WriteBottomStatusLine ($865A), DisplayIntermediate ($86A8),
ResetSpritesAndScreenTimer ($889D), AreaParserTaskControl ($86E6),
GetBackgroundColor ($85E3), GetAlternatePalette1 ($8643),
ClearBuffersDrawIcon ($8732) as `[[function]]` entries. Functions
count: 490 → 501.

**$C772 watch confirms progress:** 3 writes per trace (`$01`, `$00`,
`$01`) — InitializeArea reaches its `INC $0772/RTS`, the NMI dispatch
re-routes to ScreenRoutines, then NMI resets via `$80DB STA $0773`.
Translated SMB is *running*; it's just running ~300× slower than
real-time because the per-NES-op expansion ratio is too high.

Outstanding optimisations: peephole eliminate redundant `rt_set_nz_a`
when the next op overwrites A or doesn't read flags; inline tight
indexed-load patterns instead of trampolining through
`rt_read_indexed`; tighter section partitioning so more calls land
in the same bank.

**trace_sms now runs 3 M Z80 instructions cleanly, fires 49 frame
interrupts, performs 46 K VRAM writes**, loads the palette to CRAM,
fills tile-pattern memory, and applies 145 nametable updates via the
VBlank VRAM-buffer flush. trace_sms can now also render the final
VRAM/CRAM state to a PPM framebuffer (`SMS_DUMP_PPM=path`) and a
sequence (`SMS_DUMP_EACH_FRAME=dir`), bypassing Mednafen entirely
for visual verification.

**First visible output achieved.** `out/smb/trace_frame.png` shows a
grid of background tiles plus several white horizontal bars that
match the shape of SMB's HUD ("MARIO  ©×00  WORLD TIME") and the
ground/platform rows. It's a partial mid-init scene, not the title
screen, but it is *recognisably SMB-shaped* — proving the runtime,
the nametable buffer flush, the asset upload, and the JumpEngine
dispatch are all functioning end-to-end.

Mednafen still renders all black. The capture script can't find
mednafen's xvfb window (xdotool returns `<not found>`) and falls
back to root-window capture, which is empty. Hypotheses worth
testing:
- (a) The screenshot script needs to query mednafen's window via the
  X server differently — try `xwininfo -root -tree` and grab the
  child whose name contains "Mednafen";
- (b) `_ppu_w_mask` only shadows $2001; if SMB later writes a value
  that would disable BG/sprites, the SMS display never gets the
  update — port the bit-6 change to VDP reg 1 (E.2);
- (c) Nametable might be written as 1-byte-per-cell where SMS expects
  2 bytes — but the trace_frame.png suggests it's working, so this
  is unlikely.

Followups beyond visual: upper-PRG access scheme for the remaining
13 reds; runtime $2001 → VDP reg 1 forwarding (E.2); scroll register
forwarding (E.4).
