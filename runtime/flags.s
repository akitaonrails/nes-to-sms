; flags.s — Shadow 6502 status byte helpers.
;
; The 6502 P register lives in SMS RAM at $CB03.
; Bit layout (6502 convention):  N V - B D I Z C
;                                 7 6 5 4 3 2 1 0
;
; All routines here preserve the registers they are not documented to change.
; The Rust lower crate emits `call rt_xxx` for every flag-producing 6502 op.
;
; Z80 carry convention vs 6502:
;   6502 ADC: C flag = carry out (same direction as Z80).
;   6502 SBC: borrows when C=0 (opposite of Z80 SBC which borrows when C=1).
;   This asymmetry is handled explicitly in rt_sbc_a.
;
; Overflow (V) detection:
;   6502 V is set when the signed result overflows.
;   Z80 PV flag after ADC/SBC represents overflow when no BCD is in play.
;   We use `jp po` / `jp pe` to test the Z80 PV flag.

.section "flags" free

; ─── rt_set_nz_a ──────────────────────────────────────────────────────────────
; Updates shadow P bits N (bit 7) and Z (bit 1) from the current value of A.
; A is preserved.  B/C preserved (push bc).
;
; Lean rewrite: branchless N, single branch for Z, one push/pop pair —
; vs the old triple push af/pop af juggle. This is the hottest runtime
; helper, so the per-call saving matters.
rt_set_nz_a:
  ; Table-driven (H.1c): rt_nz_table[v] holds the N/Z bits for v.
  push hl
  ld   l, a
  ld   h, $3e               ; rt_nz_table page (pinned, see table section)
  ld   a, ($cb03)
  and  %01111101            ; clear shadow N (bit 7) and Z (bit 1)
  or   (hl)
  ld   ($cb03), a           ; write updated shadow P
  ld   a, l                 ; restore caller's A
  pop  hl
  ret

; ─── rt_adc_a ─────────────────────────────────────────────────────────────────
; 6502 ADC: A = A + B + shadow_C.
; Entry: A = accumulator, B = operand M.
; Exit:  A = result.  Shadow P updated: N, V, Z, C.
; Preserves: B (caller may still need it).
rt_adc_a:
  ; H.1d branchless: Z80 F layout (S Z . H . PV N C) maps to 6502 P as
  ; S->N (same bit 7), C->C (same bit 0), Z bit6 -> bit1 (3x rlca),
  ; PV bit2 -> V bit6 (4x rlca).
  push bc
  push hl
  ld   c, a
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ld   a, c
  adc  a, b                 ; result; native S/Z/PV/C
  ld   c, a                 ; C = result
  push af
  pop  hl                   ; L = F
  ld   a, l
  and  %10000001            ; N + C already in place
  ld   h, a
  ld   a, l
  and  %01000000            ; Z
  rlca
  rlca
  rlca                      ; bit 6 -> bit 1
  or   h
  ld   h, a
  ld   a, l
  and  %00000100            ; PV (overflow)
  rlca
  rlca
  rlca
  rlca                      ; bit 2 -> bit 6
  or   h
  ld   h, a
  ld   a, ($cb03)
  and  %00111100            ; clear N, V, Z, C
  or   h
  ld   ($cb03), a
  ld   a, c
  pop  hl
  pop  bc
  ret

; ─── rt_sbc_a ─────────────────────────────────────────────────────────────────
; 6502 SBC: A = A - B - (1 - shadow_C).
; 6502 SBC borrows when C=0 (i.e. borrow = NOT shadow_C).
; Z80 SBC borrows when carry = 1, so we invert shadow_C before loading.
; Entry: A = accumulator, B = operand M.
; Exit:  A = result.  Shadow P updated: N, V, Z, C.
; Preserves: B.
rt_sbc_a:
  ; H.1d branchless. 6502 SBC subtracts (1 - C): Z80 carry-in = !shadow C
  ; (ccf after loading), and 6502 C_out = !borrow (xor bit 0).
  push bc
  push hl
  ld   c, a
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ccf                       ; Z80 sbc subtracts carry; 6502 subtracts (1-C)
  ld   a, c
  sbc  a, b
  ld   c, a
  push af
  pop  hl                   ; L = F
  ld   a, l
  and  %10000001
  xor  %00000001            ; 6502 C = !borrow
  ld   h, a
  ld   a, l
  and  %01000000
  rlca
  rlca
  rlca
  or   h
  ld   h, a
  ld   a, l
  and  %00000100
  rlca
  rlca
  rlca
  rlca
  or   h
  ld   h, a
  ld   a, ($cb03)
  and  %00111100
  or   h
  ld   ($cb03), a
  ld   a, c
  pop  hl
  pop  bc
  ret

; ─── rt_cmp_a ─────────────────────────────────────────────────────────────────
; 6502 CMP: shadow flags from (A - B).  A is NOT written back.
; 6502 C set when A >= B (unsigned), i.e. no borrow occurred.
; Entry: A = accumulator, B = operand M.
; Exit:  A preserved.  Shadow N, Z, C updated.
rt_cmp_a:
  ; H.1d branchless F-mapping; A and B preserved.
  push bc
  push hl
  ld   c, a
  sub  b
  push af
  pop  hl                   ; L = F
  ld   a, l
  and  %10000001
  xor  %00000001            ; 6502 C = !borrow
  ld   h, a
  ld   a, l
  and  %01000000
  rlca
  rlca
  rlca                      ; Z: bit 6 -> bit 1
  or   h
  ld   h, a
  ld   a, ($cb03)
  and  %01111100            ; clear N, Z, C
  or   h
  ld   ($cb03), a
  ld   a, c
  pop  hl
  pop  bc
  ret

; ─── rt_cpx_a ─────────────────────────────────────────────────────────────────
; 6502 CPX: compare shadow X with B.  Shadow N, Z, C updated.
; Entry: B = operand M.  Exit: A clobbered with result.
rt_cpx_a:
  ; H.1d branchless F-mapping. A and B preserved — the 6502 CPX/CPY
  ; touch only flags (the old header comment claimed A was clobbered;
  ; the old CODE preserved it, and callers rely on that).
  push af
  push hl
  ld   a, ($cb00)
  sub  b
  push af
  pop  hl                   ; L = F
  ld   a, l
  and  %10000001
  xor  %00000001            ; 6502 C = !borrow
  ld   h, a
  ld   a, l
  and  %01000000
  rlca
  rlca
  rlca                      ; Z: bit 6 -> bit 1
  or   h
  ld   h, a
  ld   a, ($cb03)
  and  %01111100
  or   h
  ld   ($cb03), a
  pop  hl
  pop  af
  ret

; ─── rt_cpy_a ─────────────────────────────────────────────────────────────────
; 6502 CPY: compare shadow Y with B.  Shadow N, Z, C updated.
; Entry: B = operand M.  Exit: A clobbered with result.
rt_cpy_a:
  ; H.1d branchless F-mapping. A and B preserved — the 6502 CPX/CPY
  ; touch only flags (the old header comment claimed A was clobbered;
  ; the old CODE preserved it, and callers rely on that).
  push af
  push hl
  ld   a, ($cb01)
  sub  b
  push af
  pop  hl                   ; L = F
  ld   a, l
  and  %10000001
  xor  %00000001            ; 6502 C = !borrow
  ld   h, a
  ld   a, l
  and  %01000000
  rlca
  rlca
  rlca                      ; Z: bit 6 -> bit 1
  or   h
  ld   h, a
  ld   a, ($cb03)
  and  %01111100
  or   h
  ld   ($cb03), a
  pop  hl
  pop  af
  ret

; ─── rt_asl_a ─────────────────────────────────────────────────────────────────
; 6502 ASL accumulator: shadow C = old bit 7; A <<= 1; update N, Z.
rt_asl_a:
  ; H.1d branchless: add a,a; C = old bit 7; N/Z via the $3E00 table.
  push hl
  add  a, a
  ld   l, a
  sbc  a, a                 ; $FF if carry else $00
  and  %00000001            ; 6502 C bit
  ld   h, $3e
  or   (hl)                 ; + N/Z of the result
  ld   h, a
  ld   a, ($cb03)
  and  %01111100
  or   h
  ld   ($cb03), a
  ld   a, l
  pop  hl
  ret

; ─── rt_lsr_a ─────────────────────────────────────────────────────────────────
; 6502 LSR accumulator: shadow C = old bit 0; A >>= 1 (logical); update N (always 0), Z.
rt_lsr_a:
  ; H.1d branchless: srl; C = old bit 0; N always 0; Z via the table.
  push hl
  srl  a
  ld   l, a
  sbc  a, a
  and  %00000001
  ld   h, $3e
  or   (hl)
  ld   h, a
  ld   a, ($cb03)
  and  %01111100
  or   h
  ld   ($cb03), a
  ld   a, l
  pop  hl
  ret

; ─── rt_rol_a ─────────────────────────────────────────────────────────────────
; 6502 ROL accumulator: rotate left through shadow carry.
; New bit 0 = old shadow C; new shadow C = old bit 7.
rt_rol_a:
  ; Load shadow C into Z80 carry without later restoring stale Z80 flags.
  ld   b, a                  ; B = original A
  ld   a, ($cb03)
  rrca                      ; bit 0 (shadow C) -> Z80 carry
  ld   a, b                  ; restore original A, preserving carry
  ; RL A: rotate left through carry.  Old bit 7 -> carry.  Old carry -> bit 0.
  rl   a
  push af                   ; save result + Z80 carry (= new shadow C)
  ; Build new shadow P.
  ld   a, ($cb03)
  and  %01111100            ; clear N, Z, C
  ld   b, a
  pop  af
  push af
  jr   nc, _rol_a_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_rol_a_no_c:
  ; N from bit 7 of result.
  bit  7, a
  jr   z, _rol_a_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_rol_a_no_n:
  ; Z from result == 0.
  pop  af
  push af
  or   a
  jr   nz, _rol_a_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_rol_a_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af                   ; result in A
  ret

; ─── rt_ror_a ─────────────────────────────────────────────────────────────────
; 6502 ROR accumulator: rotate right through shadow carry.
; New bit 7 = old shadow C; new shadow C = old bit 0.
rt_ror_a:
  ld   b, a                  ; B = original A
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ld   a, b                  ; restore original A, preserving carry
  rr   a                    ; rotate right through carry; bit 0 -> carry; carry -> bit 7
  push af
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  push af
  jr   nc, _ror_a_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_ror_a_no_c:
  bit  7, a
  jr   z, _ror_a_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_ror_a_no_n:
  pop  af
  push af
  or   a
  jr   nz, _ror_a_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_ror_a_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af
  ret

; ─── rt_asl_mem ───────────────────────────────────────────────────────────────
; 6502 ASL memory: shifts byte at (HL) left in place; updates shadow N, Z, C.
; Entry: HL = SMS RAM address of operand.  Clobbers: A.
rt_asl_mem:
  push af                   ; preserve caller's A (6502 ASL mem leaves A alone)
  push hl
  push bc
  ld   a, (hl)
  ; Old bit 7 -> shadow C.
  ld   b, a
  rlca                      ; bit 7 -> carry
  push af                   ; carry = old bit 7
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  jr   nc, _asl_mem_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_asl_mem_no_c:
  ; Actual shift.
  ld   a, (hl)
  add  a, a                 ; logical left shift
  ld   (hl), a              ; write back
  ; N and Z from result.
  bit  7, a
  jr   z, _asl_mem_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_asl_mem_no_n:
  ld   a, (hl)
  or   a
  jr   nz, _asl_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_asl_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  bc
  pop  hl
  pop  af
  ret

; ─── rt_lsr_mem ───────────────────────────────────────────────────────────────
; 6502 LSR memory: shifts byte at (HL) right; updates shadow N (always 0), Z, C.
rt_lsr_mem:
  push af                   ; preserve caller's A (6502 LSR mem leaves A alone)
  push hl
  push bc
  ld   a, (hl)
  ; Old bit 0 -> shadow C.
  rrca                      ; bit 0 -> carry
  push af
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  jr   nc, _lsr_mem_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_lsr_mem_no_c:
  ld   a, (hl)
  srl  a
  ld   (hl), a
  ; N always 0.  Z from result.
  or   a
  jr   nz, _lsr_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_lsr_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  bc
  pop  hl
  pop  af
  ret

; ─── rt_rol_mem ───────────────────────────────────────────────────────────────
; 6502 ROL memory: rotate byte at (HL) left through shadow carry.
rt_rol_mem:
  push af                   ; preserve caller's A (restored at end)
  push hl
  push bc
  ; Load shadow C into Z80 carry. Do NOT restore flags before the `rl`,
  ; or the carry we just extracted is lost (the old bug). `ld a,(hl)`
  ; does not affect flags, so the carry survives into the `rl`.
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ld   a, (hl)
  rl   a                    ; rotate through carry; old bit 7 -> Z80 carry; shadow C -> bit 0
  ld   (hl), a
  push af                   ; save result + new carry
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  push af
  jr   nc, _rol_mem_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_rol_mem_no_c:
  bit  7, a
  jr   z, _rol_mem_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_rol_mem_no_n:
  pop  af
  push af
  or   a
  jr   nz, _rol_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_rol_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af                   ; discard the juggled result+flags
  pop  bc
  pop  hl
  pop  af                   ; restore caller's A
  ret

; ─── rt_ror_mem ───────────────────────────────────────────────────────────────
; 6502 ROR memory: rotate byte at (HL) right through shadow carry.
; Preserves caller's A.
rt_ror_mem:
  push af                   ; preserve caller's A (restored at end)
  push hl
  push bc
  ; Load shadow C into Z80 carry. `rrca` rotates A (= shadow P) so bit 0
  ; (shadow C) lands in the Z80 carry; the rotated A is scratch. We must
  ; NOT pop/restore flags before the `rr` below, or the carry is lost.
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ld   a, (hl)              ; `ld` does not affect flags, carry preserved
  rr   a                    ; old bit 0 -> carry; shadow C -> bit 7
  ld   (hl), a
  push af
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  push af
  jr   nc, _ror_mem_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_ror_mem_no_c:
  bit  7, a
  jr   z, _ror_mem_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_ror_mem_no_n:
  pop  af
  push af
  or   a
  jr   nz, _ror_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_ror_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af                   ; discard the juggled result+flags
  pop  bc
  pop  hl
  pop  af                   ; restore caller's A
  ret

; ─── rt_inc_mem ───────────────────────────────────────────────────────────────
; 6502 INC memory: M = M + 1; update shadow N, Z.  C unchanged.
rt_inc_mem:
  push af
  push bc
  ld   a, (hl)
  inc  a
  ld   (hl), a
  ld   a, ($cb03)
  and  %01111101            ; clear N and Z only
  ld   b, a
  ld   a, (hl)
  bit  7, a
  jr   z, _inc_mem_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_inc_mem_no_n:
  ld   a, (hl)
  or   a
  jr   nz, _inc_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_inc_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  bc
  pop  af
  ret

; ─── rt_dec_mem ───────────────────────────────────────────────────────────────
; 6502 DEC memory: M = M - 1; update shadow N, Z.  C unchanged.
rt_dec_mem:
  push af
  push bc
  ld   a, (hl)
  dec  a
  ld   (hl), a
  ld   a, ($cb03)
  and  %01111101            ; clear N and Z
  ld   b, a
  ld   a, (hl)
  bit  7, a
  jr   z, _dec_mem_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_dec_mem_no_n:
  ld   a, (hl)
  or   a
  jr   nz, _dec_mem_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_dec_mem_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  bc
  pop  af
  ret

; ─── rt_bit_mem ───────────────────────────────────────────────────────────────
; 6502 BIT: test A AND M.
; Entry: A = accumulator, B = M (the memory operand).
; Updates: shadow Z = ((A & B) == 0); shadow N = bit 7 of B; shadow V = bit 6 of B.
; A is NOT written back.  B is preserved.
; Uses D as scratch (saved/restored).
rt_bit_mem:
  push af
  push bc
  push de
  ld   d, a                 ; D = accumulator (preserved across routine)
  ld   e, b                 ; E = M operand
  ; Build new shadow P: clear N (7), V (6), Z (1).
  ld   a, ($cb03)
  and  %00111101
  ld   c, a                 ; C = P with N, V, Z cleared
  ; Z: (accumulator & M) == 0.
  ld   a, d                 ; accumulator
  and  e                    ; A & M
  jr   nz, _bit_no_z
  ld   a, c
  or   %00000010            ; set 6502 Z
  ld   c, a
_bit_no_z:
  ; N = bit 7 of M.
  bit  7, e
  jr   z, _bit_no_n
  ld   a, c
  or   %10000000
  ld   c, a
_bit_no_n:
  ; V = bit 6 of M.
  bit  6, e
  jr   z, _bit_no_v
  ld   a, c
  or   %01000000
  ld   c, a
_bit_no_v:
  ld   a, c
  ld   ($cb03), a
  pop  de                   ; restore caller's DE
  pop  bc                   ; restore caller's BC
  pop  af                   ; restore caller's AF (A = accumulator)
  ret

.ends
