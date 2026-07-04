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
  push bc
  ld   b, a                 ; B = caller A (preserved; also the N/Z source)
  ld   a, ($cb03)
  and  %01111101            ; clear shadow N (bit 7) and Z (bit 1)
  ld   c, a                 ; C = P with N,Z cleared
  ld   a, b
  and  %10000000            ; isolate N = bit 7 of the value
  or   c                    ; merge N into P
  ld   c, a
  ld   a, b
  or   a                    ; Z80 Z = (value == 0)
  jr   nz, _set_nz_done
  set  1, c                 ; set shadow Z (bit 1)
_set_nz_done:
  ld   a, c
  ld   ($cb03), a           ; write updated shadow P
  ld   a, b                 ; restore caller's A
  pop  bc
  ret

; ─── rt_adc_a ─────────────────────────────────────────────────────────────────
; 6502 ADC: A = A + B + shadow_C.
; Entry: A = accumulator, B = operand M.
; Exit:  A = result.  Shadow P updated: N, V, Z, C.
; Preserves: B (caller may still need it).
rt_adc_a:
  push bc
  ; Step 1: extract shadow C into Z80 carry.
  ;   Shadow P is at $CB03, bit 0 = C.
  ;   RRCA rotates A right through carry; bit 0 goes to carry flag.
  ld   c, a                 ; save accumulator
  ld   a, ($cb03)
  rrca                      ; bit 0 (shadow C) -> Z80 carry flag
  ld   a, c                 ; restore accumulator
  ; Step 2: ADC A, B with native Z80 carry (which now holds shadow C).
  adc  a, b
  ; Step 3: build updated shadow P from Z80 flag results.
  push af                   ; save result A + Z80 flags
  ld   a, ($cb03)
  and  %00111100            ; clear N (7), V (6), Z (1), C (0); keep D (3), I (2), B (4), U (5)
  ld   c, a                 ; C = P with arithmetic flags cleared
  pop  af
  push af
  ; Set shadow C from Z80 carry.
  jr   nc, _adc_no_c
  ld   a, c
  or   %00000001
  ld   c, a
_adc_no_c:
  pop  af
  push af
  ; Set shadow V from Z80 PV (overflow).
  ;   PV is set on signed overflow for addition; jp po = parity odd = overflow clear.
  jp   po, _adc_no_v
  ld   a, c
  or   %01000000            ; set 6502 V (bit 6)
  ld   c, a
_adc_no_v:
  pop  af
  push af
  ; Set shadow N from bit 7 of result.
  bit  7, a
  jr   z, _adc_no_n
  ld   a, c
  or   %10000000            ; set 6502 N (bit 7)
  ld   c, a
_adc_no_n:
  pop  af
  push af
  ; Set shadow Z if result == 0.
  or   a
  jr   nz, _adc_no_z
  ld   a, c
  or   %00000010            ; set 6502 Z (bit 1)
  ld   c, a
_adc_no_z:
  ld   a, c
  ld   ($cb03), a           ; write updated shadow P
  pop  af                   ; final result in A
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
  push bc
  ; Step 1: compute Z80 carry = NOT(shadow_C).
  ld   c, a                 ; save accumulator
  ld   a, ($cb03)
  rrca                      ; shadow C -> Z80 carry
  ccf                       ; invert: 6502 borrow-in = !C
  ld   a, c                 ; restore accumulator
  ; Step 2: SBC A, B.
  sbc  a, b
  ; Step 3: build updated shadow P.
  push af
  ld   a, ($cb03)
  and  %00111100            ; clear N, V, Z, C
  ld   c, a
  pop  af
  push af
  ; Shadow C = Z80 carry (a borrow-out in Z80 SBC means C=0 in 6502, i.e. borrow occurred;
  ; Z80 carry after SBC = 1 means borrow, so 6502 C = NOT Z80_carry).
  jr   c, _sbc_carry_clear  ; Z80 carry set = borrow = 6502 C clear
  ld   a, c
  or   %00000001            ; 6502 C = 1 (no borrow)
  ld   c, a
_sbc_carry_clear:
  pop  af
  push af
  ; Shadow V from Z80 PV.
  jp   po, _sbc_no_v
  ld   a, c
  or   %01000000
  ld   c, a
_sbc_no_v:
  pop  af
  push af
  bit  7, a
  jr   z, _sbc_no_n
  ld   a, c
  or   %10000000
  ld   c, a
_sbc_no_n:
  pop  af
  push af
  or   a
  jr   nz, _sbc_no_z
  ld   a, c
  or   %00000010
  ld   c, a
_sbc_no_z:
  ld   a, c
  ld   ($cb03), a
  pop  af
  pop  bc
  ret

; ─── rt_cmp_a ─────────────────────────────────────────────────────────────────
; 6502 CMP: shadow flags from (A - B).  A is NOT written back.
; 6502 C set when A >= B (unsigned), i.e. no borrow occurred.
; Entry: A = accumulator, B = operand M.
; Exit:  A preserved.  Shadow N, Z, C updated.
rt_cmp_a:
  push af
  push bc
  ; Subtract B from A without storing result.
  ld   c, a                 ; save A
  sub  b                    ; Z80 sub; carry set on borrow (unsigned A < B)
  push af
  ; Build new shadow P.
  ld   a, ($cb03)
  and  %01111100            ; clear N (7), Z (1), C (0)
  ld   b, a
  pop  af
  push af
  ; C: Z80 carry after SUB = 1 means borrow = A < B = 6502 C clear.
  jr   c, _cmp_no_c
  ld   a, b
  or   %00000001            ; 6502 C = 1 (A >= B)
  ld   b, a
_cmp_no_c:
  pop  af
  push af
  ; N: bit 7 of (A - B).
  bit  7, a
  jr   z, _cmp_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_cmp_no_n:
  pop  af
  push af
  ; Z: (A - B) == 0.
  or   a
  jr   nz, _cmp_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_cmp_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af                   ; discard temp flags
  pop  bc
  pop  af                   ; restore original A
  ret

; ─── rt_cpx_a ─────────────────────────────────────────────────────────────────
; 6502 CPX: compare shadow X with B.  Shadow N, Z, C updated.
; Entry: B = operand M.  Exit: A clobbered with result.
rt_cpx_a:
  push af
  ld   a, ($cb00)           ; load shadow X
  pop  af
  ; Fall through to shared compare logic (we now have X in A, B = operand).
  ; We cannot call rt_cmp_a directly as it saves/restores A from the caller's A.
  ; Use the same inline logic.
  push af
  push bc
  ld   c, a                 ; save X value
  ld   a, ($cb00)           ; reload X (A was caller's A above)
  sub  b
  push af
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  push af
  jr   c, _cpx_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_cpx_no_c:
  pop  af
  push af
  bit  7, a
  jr   z, _cpx_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_cpx_no_n:
  pop  af
  push af
  or   a
  jr   nz, _cpx_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_cpx_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af
  pop  bc
  pop  af
  ret

; ─── rt_cpy_a ─────────────────────────────────────────────────────────────────
; 6502 CPY: compare shadow Y with B.  Shadow N, Z, C updated.
; Entry: B = operand M.  Exit: A clobbered with result.
rt_cpy_a:
  push af
  push bc
  ld   a, ($cb01)           ; load shadow Y
  sub  b
  push af
  ld   a, ($cb03)
  and  %01111100
  ld   b, a
  pop  af
  push af
  jr   c, _cpy_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_cpy_no_c:
  pop  af
  push af
  bit  7, a
  jr   z, _cpy_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_cpy_no_n:
  pop  af
  push af
  or   a
  jr   nz, _cpy_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_cpy_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af
  pop  bc
  pop  af
  ret

; ─── rt_asl_a ─────────────────────────────────────────────────────────────────
; 6502 ASL accumulator: shadow C = old bit 7; A <<= 1; update N, Z.
rt_asl_a:
  ; Capture bit 7 into a temp.
  push af
  rlca                      ; bit 7 -> carry, A rotated (bit 7 wraps to bit 0)
  ; Z80 carry now = old bit 7 = new 6502 shadow C.
  push af                   ; save carry state
  ld   a, ($cb03)
  and  %01111110            ; clear N, C (keep Z for now — will replace)
  ; We also need to clear Z; clear all of N, Z, C.
  and  %01111100
  ld   b, a
  pop  af                   ; restore carry
  push af
  jr   nc, _asl_a_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_asl_a_no_c:
  ; Compute actual shift: restore original A from first push.
  pop  af                   ; flags with carry; A is shifted-rotated (has bit 7 at 0)
  pop  af                   ; original A before rlca
  add  a, a                 ; logical shift left (bit 7 goes to carry, bit 0 = 0)
  push af                   ; save result + carry (same as shadow C we already set)
  ; N from bit 7 of result.
  bit  7, a
  jr   z, _asl_a_no_n
  ld   a, b
  or   %10000000
  ld   b, a
_asl_a_no_n:
  ; Z from result == 0.
  pop  af
  push af
  or   a
  jr   nz, _asl_a_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_asl_a_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af                   ; result in A
  ret

; ─── rt_lsr_a ─────────────────────────────────────────────────────────────────
; 6502 LSR accumulator: shadow C = old bit 0; A >>= 1 (logical); update N (always 0), Z.
rt_lsr_a:
  push af
  ; Move bit 0 into carry.
  rrca                      ; bit 0 -> carry; A rotated right (bit 0 wraps to bit 7)
  push af                   ; save carry = old bit 0
  ld   a, ($cb03)
  and  %01111100            ; clear N, Z, C
  ld   b, a
  pop  af
  push af
  jr   nc, _lsr_a_no_c
  ld   a, b
  or   %00000001
  ld   b, a
_lsr_a_no_c:
  ; Compute actual shift.
  pop  af                   ; flags after rrca; A has bit 0 at bit 7
  pop  af                   ; original A
  srl  a                    ; logical shift right; bit 0 -> carry, bit 7 = 0
  push af
  ; N is always 0 after LSR (bit 7 of result is always 0).
  ; Z from result == 0.
  or   a
  jr   nz, _lsr_a_no_z
  ld   a, b
  or   %00000010
  ld   b, a
_lsr_a_no_z:
  ld   a, b
  ld   ($cb03), a
  pop  af
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
