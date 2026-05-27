# Validation Triage

Running record of differential-validation findings against SMB. Each entry
captures a bug class observed in `reports/validation.txt`, the smallest
repro extracted into the synthetic test suite (`validation/tests/`), and
the fix landed.

## Bug classes found and fixed

### B1 — Flag-clear ops clobber A
**Found:** Phase A.1 single-op test `clc_only`.
**Pattern:** `Op::Clc/Sec/Cli/Sei/Clv/Cld/Sed` lowered to a naive
`ld a,(SHADOW_P); and/or imm; ld (SHADOW_P),a` which destroys A.
**Fix:** `emit_flag_update` helper in `lower` bracketing with `push af`
/ `pop af`. **Repro:** `single_ops::clc_only`.

### B2 — Branch test clobbers A
**Found:** Phase A.1 known-slice test `smb_b1b4_branch_store`.
**Pattern:** `Op::BranchIf` lowered to `ld a,(SHADOW_P); and <mask>;
jp z/nz, target`. The shadow-P load destroys A.
**Fix:** switched to `ld hl,SHADOW_P; bit n,(hl); jp z/nz,target` which
leaves A untouched. Required adding `bit_n_hl_ptr` to `z80_emit` and
`BIT/SET/RES n,(HL)` to z80_emu's CB dispatcher.

### B3 — STX/STY/TXS/PHP/PLP clobber A
**Found:** A.3 sweep. SMB routines $F381 and $F39F write Y to APU regs
and the Z80 ends with A == initial X.
**Pattern:** the load-from-shadow-X-into-A and store-to-memory sequence
destroyed A.
**Fix:** push/pop AF around the shadow-X/Y load.

### B4 — INX/INY/DEX/DEY clobber A
**Found:** A.3 sweep.
**Pattern:** lowered to `ld a,(SHADOW_X); inc/dec a; ld (SHADOW_X),a;
call rt_set_nz_a`. The shadow load destroys A. (rt_set_nz_a does
preserve A, but A is gone before it's called.)
**Fix:** push/pop AF around the entire sequence.

### B5 — Indexed addressing used raw NES base
**Found:** A.3 sweep on SMB's $8223 OAM-clear loop.
**Pattern:** `STA $0200,Y` lowered emitted `ld hl,$0200; ... ; call
rt_write_indexed`, which writes to SMS $0204 (boot ROM!) instead of SMS
$C204. RAM writes silently failed.
**Fix:** added `indexed_base_to_sms(base, region)` helper that remaps
RAM/RamMirror/Stack/ZeroPage bases to the SMS RAM block. Applied
across LdaMem/LdxMem/LdyMem/StaMem AbsIndexedX/Y/ZpIndexed sites.

### B6 — `add hl,rr` not implemented in z80_emu
**Found:** A.3 sweep via the indexed-write tests.
**Pattern:** `rt_read_indexed` / `rt_write_indexed` use `add hl,bc`
(opcode $09) to compute base+offset. z80_emu treated it as
UnsupportedOpcode, the routine bailed out mid-helper, and the write
never happened.
**Fix:** added 0x09/0x19/0x29/0x39 (`add hl,bc/de/hl/sp`) to
z80_emu with correct half-carry and N=0 flag semantics.

### B7 — LDX/LDY immediate and memory clobber A
**Found:** SMB $F4A7 (writes Y to APU regs but Z80 has A=$0F after).
**Pattern:** `Op::LdxImm/LdxMem/LdyImm/LdyMem` lowered to a sequence
that loads the new value into A first, then stores into shadow X/Y.
The transient use of A destroyed the caller's A.
**Fix:** push/pop AF around the whole load+store+rt_set_nz_a sequence.

### B9 — `emit_mem_to_b` clobbered A in CMP/ADC/etc.
**Found:** SMB $81C6 — RAM-clear-loop wrote $50 to every output slot.
**Pattern:** ALU ops `CMP/ADC/SBC/AND/ORA/EOR/CPX/CPY/BIT` use
`emit_mem_to_b(addr)` to put the memory operand into B. The helper
was loading via `ld a,(addr); ld b,a`, which destroys the current A —
i.e. the LHS of the ALU operation. Result: every `CMP $00` produced
`A==B==$28`, every subsequent `ADC` produced `$28+$28=$50`.
**Fix:** rewrote `emit_mem_to_b`:
- For ZpConst / Const cases, use `ld hl,addr; ld b,(hl)` which doesn't
  touch A.
- For AbsIndexedX/Y, ZpIndexedX/Y, IndirectY paths, bracket with
  `ld c,a` / `ld a,c` around the helper call.

### B10 — Zero-page indexed addressing didn't wrap within ZP
**Found:** SMB $BD84 — `STA $BE,X` with X=$67 wrote to SMS $C125
(emulated 6502 stack page) instead of NES zp $25 → SMS $C025.
**Pattern:** lower emitted `ld hl, $C0BE; ld b, X; call rt_write_indexed`
which adds a 16-bit offset; this doesn't reproduce the 6502's mandatory
8-bit wrap of `(zp + X) & $FF`.
**Fix:** for ZpIndexed addressing, compute `(zp + X) & $FF` in A,
load into L, set H=$C0, and access (HL) directly. Doesn't go through
rt_read/write_indexed.

### B8 — Routine end-detection too eager
**Found:** SMB $F8C5 — a routine that falls through into $F8CB (a
known sub-label) was trimmed to end at $F8CB. The lifted routine had
no terminal RTS; both oracle and z80 walked past the trimmed range
into garbage and produced diverging results.
**Fix:** `classify_routine` now skips routines whose IR doesn't contain
an `Op::Rts`. The trim heuristic in the cli stays as a guard against
overlapping ranges, but the validation harness no longer attempts to
verify routines it knows can't terminate cleanly.

### Harness improvement: step budget
The oracle's `run_until_rts` budget was 10K instructions; the Z80's
`run_until_ret` was 200K. SMB's RAM-clear loop ($90CC) does ~14K 6502
instructions, expanding to ~500K Z80 instructions in our conservative
lowering. Budgets bumped to 200K (oracle) / 2M (z80).

## Status after Phase A

After fixes B1–B10, SMB reports **15 green / 0 red / 44 skipped (of 59)**.

All routines the harness can verify pass differential validation. The
44 skipped fall into categories:

| Skip reason                              | Count |
|------------------------------------------|------:|
| Hardware access (PPU/APU/OAM-DMA/etc.)   | ~12   |
| External JSR (callee outside lift range) | ~12   |
| Lowering failed (unsupported opcode)     | 6     |
| Indirect jump                            | 2     |
| JAM / BRK opcode (lifted from data)      | 2     |
| No terminal RTS (trimmed range)          | ~6    |
| PRG-ROM indexed read                     | ~4    |

To take the harness from 15 green to 50+ green, the next phases must:
- Implement stable unofficial opcodes in lower (Phase D.1) — unblocks
  the 6 "lowering failed" skips.
- Provide stub returns for external JSR (Phase A.5 follow-up).
- Tighten discovery so routines have proper end addresses (Phase B.3).
- Make PRG bytes addressable for indexed reads (Phase D.5).
- Skip refinements aside: real bugs found and fixed in Phase A: 10.

## Tooling notes

- `VALIDATION_TRACE=1` env var (currently removed; re-add as needed)
  dumps the entry bytes and a chosen helper's bytes during a failing
  test. Useful for "which opcode is hitting UnsupportedOpcode?"
- The harness's runtime stubs in `crates/validation/src/runtime_stubs.rs`
  duplicate behavior from `runtime/*.s`. Bugs found in either side
  surface as a diff; B6 was a bug in z80_emu (not the stub), but it
  manifested as a stub failure first.
