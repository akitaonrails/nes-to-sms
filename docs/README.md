# NES to Master System Research

This project starts as a research-driven toolchain, not as a blind binary
translator. The practical goal is to destructure NES ROMs into code, data,
graphics, audio, mapper behavior, and platform I/O, then generate a Master
System project that can be completed with game-specific profiles and patches.

The core findings are:

- A fully automatic NES-to-SMS converter for arbitrary commercial games is not
  realistic.
- A useful Rust tool is realistic if it focuses on analysis, extraction,
  trace-assisted disassembly, asset conversion, and generation of an SMS build
  scaffold.
- NROM/mapper 0 games are the right first target.
- `Super Mario Bros. (World).nes` is a strong first case because it is NROM and
  has an existing high-quality public disassembly.
- Existing attempts show that SMB-on-SMS is feasible, but the successful path is
  a porting workflow, not raw emulation or a direct 1:1 translation.

## Documents

**Canonical plan:**

- [Master Plan](master-plan.md) — architecture, principles, phases, validation,
  risks. Other plan docs are subordinate.
- [Completion Plan](completion-plan.md) — linear step-by-step checklist from
  current state to a fully runnable SMB-on-SMS ROM. Updated in place as work
  progresses.

**Status and subsystem phasing:**

- [Current Status and Gaps](current-status-and-gaps.md) — inventory only
- [Full Translation Plan](full-translation-plan.md) — SMB subsystem phasing

**Platform and research background:**

- [NES Platform Notes](nes-platform.md)
- [Master System Platform Notes](sms-platform.md)
- [Translation Feasibility](translation-feasibility.md)
- [6502 to Z80 Automation Research](6502-to-z80-automation.md)
- [NESRecomp Reuse Analysis](nesrecomp-reuse-analysis.md)
- [Tooling Survey](tooling.md)
- [Prior Attempts](prior-attempts.md)
- [Local ROM Library Notes](rom-library-notes.md)

**PoC history (not forward-looking):**

- [Z80 Output Subproject Plan](z80-output-plan.md)
- [Proof-of-Concept Plan](poc-plan.md)
- [PoC Findings](poc-findings.md)
- [PoC Worklog](poc-worklog.md)

## Recommended Initial Scope

1. Parse iNES/NES 2.0 headers.
2. Extract PRG ROM, CHR ROM/RAM metadata, mirroring, mapper, vectors, and banks.
3. Add NROM/mapper 0 support first.
4. Convert NES 2bpp CHR tiles into SMS Mode 4 4bpp tile data.
5. Generate an SMS project using WLA-DX or SDCC/devkitSMS.
6. Use `Super Mario Bros. (World).nes` as the first integration target.
7. Use trace/CDL data and existing disassemblies where available.
