# Handoff: Castlevania 1 (CV1) — real-emulator blocker RESOLVED

> **Superseded (2026-07-21):** the mapper-2 recovery and current acceptance
> result are recorded in `docs/cv1-recovery-plan.md`. In particular, the GPGX
> attract-transition freeze described below no longer reproduces: the canonical
> build accepts Start, renders a clean title/entrance background, and reaches
> movable first-stage gameplay at 500% Z80 overclock. The remainder of this
> file is retained as forensic history.

Last updated: 2026-07-11 (commit 8ffb7f8). SMB fully green on
`.roms/smb.nes` (three parity routes byte-for-byte, acceptance 4/4,
395 tests, 0 unresolved labels).

## One-line status

The Mednafen reboot loop is **fixed**. CV1 boots, runs, and advances
its task state machine on Mednafen exactly as in-harness (task
counter climbs, zero reboots over 280s, breadcrumbs clean). What
remains is a rendering-fidelity phase (the intro renders the same
garbled tiles in-harness AND on Mednafen — no emulator divergence)
plus speed (CV1 is heavy at 1×; GPGX 500% overclock is the play
path) and, once screens are recognizable, a CV1 acceptance route.

## IMPORTANT: ROM inputs

A ROM manager rewrote the NAS collection on 2026-07-11 at 14:45
(new headers AND different PRG bytes) and silently broke every build
that read the NAS paths afterwards. The untouched dumps live in
`/mnt/terachad/Emulators/EmuDeck/roms/nes/originals/`. Local copies
are in the repo's gitignored `.roms/` (`smb.nes`, `cv1.nes`) —
**always build from `.roms/`**.

## What was wrong (the reboot loop) — commit 8ffb7f8

Diagnosed with a DIAG_WILDJUMP instrumentation build (reboot counter
at $CA20 read ~80 reboots/min; a reset-arrival probe froze at
re-entry #2 showing HL=BC=$0000; per-site breadcrumbs then named
rt_far_gate jumping to a zeroed target). Four re-entrancy bug
classes, all fixed:

1. **Single-slot IRQ context saves.** irq_handler saved BC/HL/AF in
   fixed words ($D472/$D475/$D477); any nested/skip handler entry
   clobbered the outer context's registers. Now saved on the native
   stack. This was the direct reboot mechanism (and an SMB free-run
   trap at step ~121M, caught with SMS_TRAP_RING).
2. **Shared far-gate park.** rt_far_gate/rt_far_gate_cont parked the
   jump target in $CB2E across an interruptible window. Now parked on
   the native stack; the cont-LIFO reserves-then-fills.
3. **6502-stack op ordering.** Push wrote before publishing S; pop
   published before reading — an interrupt in the window pushes the
   bridge frame over the in-flight slot. All sites now
   reserve-then-write / read-then-publish (lower inline emitters,
   stack6502.s helpers, rt_brk).
4. **BRK/RTI semantics.** The NMI bridge now pushes a real 3-byte
   frame (sentinel PC $FFFF + P); RTI pops P+PC and native-returns on
   the sentinel or dispatches to a game-written PC (CV1's junk-BRK
   recovery rewrites the stacked PC — this is load-bearing); rt_brk
   builds a real frame and vectors to translated_irq without a native
   return; the non-sentinel RTI dispatch resets the abandoned native
   machinery (SP, TR frames, far LIFO, NMI depth) — the analog of the
   game's own TXS stack repair.

## Diagnostics that exist now (DIAG_WILDJUMP builds)

`sed -i '1i .define DIAG_WILDJUMP 1' out/cv1/sms.asm` before make:
- $CA20 reboot counter (reset-arrival probe at $0000; freezes with
  full context at re-entry #2: HL/A/SP/BC/DE + 16 stack bytes at
  $CA22-$CA3B).
- RAM canary: nt-shadow filled with $F7 (RST $30) → breadcrumb trap.
- Zero-continuation guards at every computed-transfer pop (markers in
  $CA21) + last-transfer breadcrumb ($CA3C site id, $CA3D target).
- trace-sms `SMS_TRAP_RING=1`: transfer-ring dump at the first trap.
- Savestate parsing: gzip → `MDFNSVST`; sections `[32-byte name][u32
  len]`; chunks `[u8 namelen][name][u32 len][data]`; task = MAIN/RAM
  offset 0x18.

## Current CV1 behavior (verified)

- In-harness: task 0→1→2→5→2, ~48M steps; same under
  SMS_REAL_PACING.
- Mednafen (clean build, no DIAG): task=1 at 90s, no reboots;
  DIAG build sampled at 60/150/280s: reboots stays 1, task 1→2.
- Rendering: the intro/copyright screen shows scattered garbled
  tiles — IDENTICALLY in-harness and on Mednafen (checkpoint
  frame-500 PPM == Mednafen recording frames). SAT matches between
  the two; nametable diffs are a constant tile-index offset
  (on-demand variant pool allocation order — benign).

## OPEN: the GPGX attract-transition freeze (commit 6f35a05 state)

CV1 runs on Mednafen (Enter starts the game — verified end-to-end via
scripted keypress + savestate task read). On GPGX it deterministically
dies at the title/attract transition: a phantom "$C3F2 push + RTS"
appears (dispatch target $C3F3 = the operand byte of LDA #$01), which
neither the harness (300M-step attract cycles, zero traps) nor
Mednafen ever produces. Event-tied, NOT overclock: reproduces at
OVERCLOCK=100 (just later in wall time, right after the title shows).

The recovery chain keeps the machine alive through several links
(realign -> $F0xx audio interior -> emulated-stack RTS -> bank-5 $FF
padding) but the game state has already drifted — the cascade is a
symptom. The root divergence is upstream and GPGX-timing-specific.

Debugging assets that work TODAY:
- RetroArch UDP telemetry: `echo "READ_CORE_RAM <hexoff> <len>" | nc
  -u -w1 127.0.0.1 55355` against a headless GPGX (`network_cmd_enable
  = "true"`; use `video_driver = "sdl2"` under Xvfb or captures are
  blank). WRITE_CORE_RAM pokes work too. Offsets are work-RAM relative
  ($C000 = 0).
- DIAG_WILDJUMP breadcrumbs: $CB1D trap marker, $CB1B/1C miss target,
  $CA3C/3D last computed transfer, $CA34 handler phase, $CA20 reboot
  counter, $CA36 pause-NMI PC capture (GPGX never fires it — works on
  Mednafen).
- Known-good: keyboard reaches the core (hold z -> $CB06 = 01).

Next tools for the root cause: extract GPGX Z80 state (RetroArch
savestate via configured hotkey + state-dir, then parse the GPGX
serialization), or an instruction-level GPGX-vs-harness lockstep at
the transition frame; or bisect which translated site pushes $C3F2
(instrument the few rt_rts_dispatch call sites with a DIAG log of
pushed pairs).

## Next phase (in order)

1. **Title letter polish** (the one open rendering bug). State:
   the title renders recognizably (commits 61a4878 + 10859d7 fixed
   the table-1 source overflow, added flush re-projection with the
   $CA13 presented-table damper, and keyed generation+invalidation
   to the latch) but five font tiles (133/134/135/152/153 — the
   T/A/K-class letters of PUSH START KEY and the bottom text) end
   ALL-ZERO in VRAM even though SMS_DUMP_SRAM shows real glyphs at
   their table-1 mirror offsets. SMS_WATCH_VRAM=0x10E0:0x20 (slot
   135) shows real glyph bytes written, then a later regeneration
   writing zeros (pc ≈ $0F8C-$0F9E emit loop, bank1=8 context).
   Working glyphs have nz=8 (plane-0 font rows). Next: stamp the
   VRAM watch log with a step/write counter to order events against
   flushes; find which trigger regenerates those five FC keys from
   the wrong table (suspects: a regen while $CA13 still presented
   table 0 late in the logo era with no later projection covering
   those keys; the band materializer rows; BSHADOW path).
2. **Gameplay visual pass**: press Start (PAUSE button or
   trace-sms --pause-at-frame ~560-650) and survey level rendering.
3. **Speed**: profile heavy frames (SMS_PC_PROFILE=1); Phase-R style
   residency for CV1's hot paths.
4. **CV1 acceptance route** + add to the regression gate.
5. Strip-or-keep boot beacons; the 13 lower failures are the benign
   data-walk class (JAM/AHX/ANE/oversize → trap stubs).

## After CV1: the mapper ladder (docs/mapper-plan.md)

Gradius (CNROM) → Blaster Master (MMC1) → Bonk's (MMC3) → Marble
Madness (AxROM) → CV3 (MMC5). Hard rule: full SMB gate + Alter Ego on
every commit. ROMs from `.roms/` only; never commit them.
