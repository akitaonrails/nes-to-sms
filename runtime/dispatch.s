; dispatch.s — Indirect jump and indexed memory access helpers.
;
; rt_indirect_jmp  — implements 6502 JMP ($xxxx).
; rt_unresolved_jsr — trap for JSR targets not resolved at translate time.
; rt_brk           — trap for 6502 BRK instruction.
; rt_read_indexed  — (HL+B) -> A.
; rt_write_indexed — A -> (HL+B).
; rt_read_zp_ptr_y — dereference zero-page pointer + Y.
; rt_write_zp_ptr_y — write through zero-page pointer + Y.
;
; NES-to-SMS address remapping rule (used in pointer dereferences):
;   NES addr < $0800 (NES RAM) → SMS addr = NES addr + $C000
;   NES addr $0000-$00FF (zero page) is covered by the above.
;   NES addr >= $0800 and < $2000 (mirrors) → remap to $C000 + (addr & $07FF)
;   NES addr $2000-$3FFF (PPU) → not valid to dereference as data; trap.
;   NES addr >= $8000 (PRG ROM) → address is already in SMS ROM space for NROM;
;     for NROM-256 the second bank ($C000-$FFFF) is visible at SMS $4000+.
;     TODO: For v1 we do not remap ROM addresses — translated code should not
;     be dereferencing PRG ROM pointers at runtime (it reads them as literals).
;
; NMOS 6502 page-crossing bug in JMP ($xxxx):
;   If the indirect address is at $xxFF, the high byte of the target is read
;   from $xx00 rather than $(xx+1)00. We do NOT reproduce this bug for v1
;   because SMB does not rely on it. Add a TODO comment in rt_indirect_jmp.

.section "dispatch" free

.define FAR_BANK_STACK_BASE $d4c0
.define FAR_BANK_STACK_PTR  $d47d
.define TR_RET_BASE         $d300
.define TR_RET_BRIDGE       $d500
.define TR_RET_FULL         $d600
.define TR_RET_PTR          $cb76
.define TR_RET_DIAG_PTR     $cb73
.define TR_RET_SCRATCH_A    $cb73
.define TR_RET_SCRATCH_BANK $cb74
.define DISPATCH_MRU_BASE   $ca08

; ─── rt_far_call ──────────────────────────────────────────────────────────────
; Cross-bank call helper. Translated `JSR L_XXXX` becomes:
;     call rt_far_call
;     .dw <target_addr>      ; logical slot-1 address ($4000-$7FFF)
;     .db :<target>          ; bank number
;
; Saves the current slot-1 bank to the Z80 stack, switches slot 1 to the
; target bank, calls the target, restores the slot-1 bank, then returns.
;
; Bank shadow: we mirror the slot-1 bank value in RAM at $CB14 so that nested
; far-calls can see it. rt_far_gate stores previous banks in a tiny RAM LIFO at
; FAR_BANK_STACK_BASE (next-free pointer FAR_BANK_STACK_PTR) instead of on the
; native Z80 stack; hot frame/NMI paths otherwise collide with runtime RAM.
; rt_far_gate_cont additionally stores its explicit continuation in that same
; LIFO so cross-bank CALL sites do not keep a caller-continuation word on the
; native stack while the target runs.
; $CB15 is transient scratch for preserving the 6502 accumulator across the
; mapper write before control reaches the translated target.
;
; Trade-off: each cross-bank call costs ~50 Z80 cycles plus 3 bytes of
; inline data versus the original 3-byte `call`. For SMB this is a few
; hundred extra calls per frame, well within Z80 budget at 4 MHz.
;
; Translated 6502 calls use a separate software continuation stack:
;   $D300-$D3FB segment 0 frames, bridge pointer $D500, $D500-$D5FF segment 1
;   frames, full pointer $D600. $CB76/$CB77 is the next-free pointer.
;
; Cross-bank translated software calls/tails enter slot-0 gates below for the
; mapper write; generated slot-1 code must not write $FFFE inline.
; ─── translated software transfer gates ───────────────────────────────────────
rt_translated_call_gate:
  ; Entry: BC=target, A=target bank, HL=software frame base, DE=resident X/Y.
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, (hl)              ; entry A from frame[0]
  ld   h, b
  ld   l, c
  jp   (hl)

rt_translated_tail_gate:
  ; Entry: BC=target, A=target bank, H=entry A, DE=resident X/Y.
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, h
  ld   h, b
  ld   l, c
  jp   (hl)

; ─── rt_far_gate ──────────────────────────────────────────────────────────────
; Compact far dispatch (H2): the call site loads DE = target and A = bank
; as immediates (no data-block decode) and transfers here. This shim MUST
; live in slot 0: switching $FFFE from code running in slot 1 swaps the
; executing bank under the PC (found the hard way).
;   far CALL sites:  ld ($cb15),a / ld de,T / ld a,:T / call rt_far_gate
;   far JMP  sites:  ld ($cb15),a / ld de,T / ld a,:T / jp  rt_far_gate
; For calls, the site's return address is already on the stack; for jumps
; the original caller's is. Either way the target's RET unwinds through
; _far_after, which restores the previous bank.
rt_far_gate:
  ; Phase R: target arrives in BC (DE holds resident 6502 X/Y and must
  ; flow through untouched). A = target bank; ($cb15) = caller A.
  ld   ($cb2e), bc          ; park target (dedicated gate scratch word)
  ld   c, a

  ; Save previous slot-1 bank in the RAM far-bank stack. Reserve first, then
  ; write, so a nested IRQ far-call cannot reuse the same slot.
  ld   hl, (FAR_BANK_STACK_PTR)
  inc  hl
  ld   (FAR_BANK_STACK_PTR), hl
  dec  hl
  ld   a, ($cb14)
  ld   (hl), a

  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a
  ld   bc, _far_after
  push bc
  ld   bc, ($cb2e)
  ld   h, b
  ld   l, c                  ; target; jump directly instead of push/ret
  ld   a, ($cb15)           ; caller A (JSR/JMP preserve the accumulator)
  jp   (hl)                  ; target RET unwinds through _far_after

; ─── rt_far_gate_cont ─────────────────────────────────────────────────────────
; Call-only compact far dispatch with an explicit RAM continuation.
;   far CALL sites: ld ($cb15),a / ld bc,T / ld a,:T / ld hl,CONT /
;                   jp rt_far_gate_cont / CONT:
; Entry: BC = target, HL = continuation, A = target bank, ($CB15) = caller A.
; Native stack use: only `_far_after_cont` is pushed for the target's RET.
; Continuation and previous bank live in the FAR_BANK_STACK_PTR LIFO as:
;   [continuation_lo, continuation_hi, previous_bank]
rt_far_gate_cont:
  ld   ($cb2e), bc            ; park target (dedicated gate scratch word)
  ld   b, h                   ; B = continuation high
  ld   c, a                   ; C = target bank
  ld   a, l                   ; A = continuation low

  ; Write continuation bytes, reserve the full 3-byte frame, then fill the
  ; previous-bank byte. Once the pointer advances, a nested IRQ far-call cannot
  ; reuse this frame even if it lands before the previous-bank write below.
  ; 16-bit INC/LD keep native flags intact.
  ld   hl, (FAR_BANK_STACK_PTR)
  ld   (hl), a                ; continuation low
  inc  hl
  ld   (hl), b                ; continuation high
  inc  hl
  inc  hl
  ld   (FAR_BANK_STACK_PTR), hl
  dec  hl
  ld   a, ($cb14)
  ld   (hl), a                ; previous slot-1 bank

  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a
  ld   bc, _far_after_cont
  push bc
  ld   bc, ($cb2e)
  ld   h, b
  ld   l, c                   ; target; jump directly instead of push/ret
  ld   a, ($cb15)             ; caller A (JSR preserves the accumulator)
  jp   (hl)                   ; target RET unwinds through _far_after_cont

rt_far_call:
  pop  hl                   ; HL = data block PC (just after the `call`)
  ld   ($cb15), a            ; preserve caller A for target entry
  ld   e, (hl)              ; E = target_lo
  inc  hl
  ld   d, (hl)              ; D = target_hi
  inc  hl
  ld   a, (hl)              ; A = target_bank
  inc  hl                   ; HL = pc to return to caller (past data)
  push hl                   ; final return PC

  ; Save target bank in C, then push current bank.
  ld   c, a                 ; C = target bank
  ld   a, ($cb14)           ; A = current bank
  push af                   ; stack: [final_ret, current_bank_in_AF]

  ; Switch slot 1 to target bank.
  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a

  ; "call DE" via push/jp trick. We need to return to _far_after.
  ld   bc, _far_after
  push bc                   ; stack: [final_ret, current_bank, _far_after]
  push de                   ; stack: [..., target]
  ld   a, ($cb15)            ; JSR preserves A; mapper writes used A as scratch
  ret                       ; jump to target

_far_after:
  ; Target's RET landed here. The caller's return PC is the only native-stack
  ; item left for this far call; the previous bank lives in the RAM LIFO.
  ; Preserve returned A in C. LD/16-bit INC/DEC keep returned flags intact.
  ld   c, a
  ld   hl, (FAR_BANK_STACK_PTR)
  dec  hl
  ld   a, (hl)              ; previous slot-1 bank
  ld   (FAR_BANK_STACK_PTR), hl
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, c                 ; restore target return A; flags are unchanged
  ret                       ; return to caller (past data)

_far_after_cont:
  ; Target's RET landed here. The continuation and previous bank live in the
  ; RAM LIFO; do not RET because the call site used `jp rt_far_gate_cont`.
  ; Preserve returned AF on the native stack only while this trampoline runs;
  ; the target call itself still carried no caller-continuation native frame.
  push af
  ld   hl, (FAR_BANK_STACK_PTR)
  dec  hl
  ld   a, (hl)              ; previous slot-1 bank
  ld   ($cb14), a
  ld   ($fffe), a
  dec  hl
  ld   b, (hl)              ; continuation high
  dec  hl
  ld   c, (hl)              ; continuation low; HL now points to frame base
  ld   (FAR_BANK_STACK_PTR), hl
  pop  af                   ; restore target return A/F
  push bc
  ret                       ; resume call site continuation

; ─── rt_translated_rts ────────────────────────────────────────────────────────
; Software RTS for translated 6502 code. Entry A = returned 6502 A, DE =
; resident X/Y. Native flags may be clobbered; 6502 flags live in SHADOW_P.
; Stackless: no native RET and no native-stack continuation frames.
rt_translated_rts:
  ld   c, a                  ; preserve returned A without touching DE
  ld   hl, (TR_RET_PTR)
  ld   (TR_RET_DIAG_PTR), hl  ; diagnostics: offending pointer on underflow
  ld   a, h
  cp   $d3
  jr   z, _tr_rts_check_seg0
  cp   $d5
  jr   z, _tr_rts_check_seg1
  cp   $d6
  jr   nz, _tr_rts_underflow
  ld   a, l
  or   a
  jr   nz, _tr_rts_underflow
  ld   hl, $d5fc             ; ptr == D600 pops final segment-1 frame
  jr   _tr_rts_pop_frame
_tr_rts_check_seg0:
  ld   a, l
  cp   $01
  jr   c, _tr_rts_underflow  ; ptr <= $D300
  cp   $f9
  jr   nc, _tr_rts_underflow ; D3FC and above are not frame pointers
  and  $03
  jr   nz, _tr_rts_underflow ; frames are 4-byte aligned
  dec  hl
  dec  hl
  dec  hl
  dec  hl                    ; HL = frame base = next-free - 4
  jr   _tr_rts_pop_frame
_tr_rts_check_seg1:
  ld   a, l
  or   a
  jr   z, _tr_rts_bridge     ; ptr == D500 pops D3F8
  and  $03
  jr   nz, _tr_rts_underflow
  dec  hl
  dec  hl
  dec  hl
  dec  hl
  jr   _tr_rts_pop_frame
_tr_rts_bridge:
  ld   hl, $d3f8
_tr_rts_pop_frame:
  ld   (hl), c               ; frame[0] = returned A scratch
  inc  hl
  ld   c, (hl)               ; continuation low
  inc  hl
  ld   b, (hl)               ; continuation high
  inc  hl
  ld   a, (hl)               ; return bank
  ld   ($cb14), a
  ld   ($fffe), a
  dec  hl
  dec  hl
  dec  hl                    ; HL = frame base, still owned
  ld   a, (hl)               ; restore returned A
  ld   (TR_RET_PTR), hl       ; publish pop after all frame reads
  ld   h, b
  ld   l, c
  jp   (hl)
_tr_rts_underflow:
  ld   a, $e3
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash

; ─── rt_far_jmp ───────────────────────────────────────────────────────────────
; Bank-aware cross-bank JMP. Translated `JMP L_XXXX` becomes:
;     call rt_far_jmp
;     .dw <target_addr>
;     .db :<target>
;
; Switches slot 1 to the target bank and tail-jumps. Because Z80 return
; addresses are bankless, this also pushes a restore trampoline: when the
; target eventually RETs, the previous slot-1 bank is restored before
; returning to the original caller.
rt_far_jmp:
  pop  hl                   ; HL = data block PC
  ld   ($cb15), a            ; preserve caller A for target entry
  ld   e, (hl)              ; E = target_lo
  inc  hl
  ld   d, (hl)              ; D = target_hi
  inc  hl
  ld   c, (hl)              ; C = target_bank
  ld   a, ($cb14)           ; A = current bank
  push af                   ; save previous bank
  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a

  ld   bc, _far_jmp_after
  push bc                   ; target RET lands here
  push de
  ld   a, ($cb15)            ; JMP preserves A; mapper writes used A as scratch
  ret

_far_jmp_after:
  ; Target's RET landed here. Stack: [original_return, previous_bank_in_AF].
  ; Preserve the target's returned AF while restoring mapper state.
  pop  bc                   ; B = previous slot-1 bank
  push af                   ; save target return A/F
  ld   a, b
  ld   ($cb14), a
  ld   ($fffe), a
  pop  af                   ; restore target return A/F
  ret

; ─── rt_indirect_jmp ──────────────────────────────────────────────────────────
; 6502 JMP ($ptr): reads the 16-bit jump target from (ptr) and (ptr+1).
; Entry: HL = address of the pointer (already remapped to SMS address space).
; Exit:  jumps to the target address.
; Clobbers: all (it does not return to caller).
;
; Tail transfer only: no native target push/RET and no far-gate return frame.
;
; TODO: The NMOS page-crossing bug ($xxFF high byte read from $xx00) is not
; reproduced here. If a future target relies on it, add a check: if L == $FF,
; set H unchanged and L = 0 for the second read.
rt_indirect_jmp:
  ; Phase R note: DE is the resident X/Y pair — this helper is a JMP
  ; (control transfer), so X/Y must SURVIVE into the target. Use BC for
  ; the pointer instead.
  di                        ; scratch holds incoming A until the tail jump
  ld   (TR_RET_SCRATCH_A), a
  ld   ($cb75), hl          ; diagnostics: the POINTER's address
  ld   c, (hl)              ; low byte of target
  inc  hl
  ld   b, (hl)              ; high byte of target
  ; ROM targets ($8000+) dispatch through the generated (bank, addr)
  ; table — never execute raw NES bytes (mapper plan M1).
  ld   a, b
  cp   $80
  jr   c, _ij_ram_target
  ld   a, (TR_RET_SCRATCH_A)
  jp   rt_banked_tail_dispatch
  ; RAM targets: remap NES RAM -> SMS RAM and jump.
_ij_ram_target:
  ld   h, b
  ld   l, c
  ld   a, h
  cp   $08
  jr   c, _ij_remap_ram
  cp   $20
  jr   c, _ij_remap_mirror
  jr   _ij_jump
_ij_remap_ram:
  add  a, $c0
  ld   h, a
  jr   _ij_jump
_ij_remap_mirror:
  and  $07
  add  a, $c0
  ld   h, a
_ij_jump:
  ld   a, (TR_RET_SCRATCH_A)
  ld   (TR_RET_SCRATCH_A), a ; keep diagnostics stable; A restored for target
  ld   a, ($cb7e)
  or   a
  jr   nz, _ij_jump_di
  ld   a, (TR_RET_SCRATCH_A)
  ei
  jp   (hl)
_ij_jump_di:
  ld   a, (TR_RET_SCRATCH_A)
  jp   (hl)

; ─── rt_banked_tail_dispatch ──────────────────────────────────────────────────
; BC = NES ROM target address, A = incoming translated A, DE resident X/Y.
; Slot-0 tail dispatch for computed JMP/RTS paths: scans the generated table,
; switches slot 1 from slot 0, restores A, and jumps directly to the target.
; No rt_far_gate and no native return/restore frame.
rt_banked_tail_dispatch:
  di
  ld   (TR_RET_SCRATCH_A), a   ; preserve incoming A through scan/mapper write
  ld   a, c
  ld   ($cb1b), a              ; requested target diagnostics / scan key
  ld   a, b
  ld   ($cb1c), a
  ld   a, :rt_dispatch_table
  ld   ($fffe), a
  ld   hl, rt_dispatch_table
  xor  a
  ld   ($cb7d), a
_btd_loop:
  ld   a, ($cb7d)
  inc  a
  ld   ($cb7d), a
  ld   a, (hl)                 ; entry addr lo
  or   a
  jr   nz, _btd_addr_nonzero
  inc  hl
  ld   a, (hl)                 ; entry addr hi
  or   a
  jr   z, _btd_miss
  dec  hl
_btd_addr_nonzero:
  ld   a, (hl)
  ld   c, a
  inc  hl
  ld   a, (hl)
  ld   b, a
  inc  hl
  ld   a, ($cb1c)
  cp   b
  jr   nz, _btd_skip
  ld   a, ($cb1b)
  cp   c
  jr   nz, _btd_skip
  ld   a, (hl)                 ; NES bank constraint
  cp   $ff
  jr   z, _btd_hit
  ld   c, a
  ld   a, ($cb62)
  cp   c
  jr   z, _btd_hit
_btd_skip:
  inc  hl
  inc  hl
  inc  hl
  inc  hl
  jr   _btd_loop
_btd_hit:
  ld   a, (hl)
  ld   ($cb7c), a
  inc  hl
  ld   a, (hl)                 ; SMS bank
  ld   (TR_RET_SCRATCH_BANK), a
  inc  hl
  ld   c, (hl)                 ; label lo
  inc  hl
  ld   b, (hl)                 ; label hi
  ld   a, (TR_RET_SCRATCH_BANK)
  ld   ($cb14), a
  ld   ($fffe), a
  ld   h, b
  ld   l, c
  ld   a, ($cb7e)
  or   a
  jr   nz, _btd_jump_di
  ld   a, (TR_RET_SCRATCH_A)
  ei
  jp   (hl)
_btd_jump_di:
  ld   a, (TR_RET_SCRATCH_A)
  jp   (hl)
_btd_miss:
  ld   a, ($cb14)
  ld   ($fffe), a
  ld   a, ($cb62)
  ld   ($cb1a), a
  ld   a, $e2
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash

; ─── rt_banked_dispatch ───────────────────────────────────────────────────────
; BC = NES ROM target address. Look it up in the generated dispatch
; table: entries of .dw nes_addr / .db nes_bank / .db sms_bank / .dw label,
; terminated by addr $0000. Fixed-bank entries carry nes_bank $FF and
; match any window state; window entries ($8000-$BFFF) also require the
; current UxROM bank shadow ($CB62) to match. Hit -> far-gate jump (bank
; restore on return included). Miss -> loud trap ($CB1D=$E2, target in
; $CB1B/1C) — fail closed, never run raw NES bytes.
rt_banked_dispatch:
  ; MRU fast path: repeated dispatches to the same (bank, target) —
  ; loops far-calling one routine — skip the table scan entirely.
  ; $CA08..$CA0E: tgt lo, tgt hi, nes bank, sms bank, label lo, label hi, valid.
  ; Keep this out of $CB80-$CBFF: that range is the compact NES attribute shadow.
  ld   a, ($ca0e)
  or   a
  jr   z, _bd_slow
  ld   a, ($ca08)
  cp   c
  jr   nz, _bd_slow
  ld   a, ($ca09)
  cp   b
  jr   nz, _bd_slow
  ld   a, ($ca0a)
  ld   l, a
  ld   a, ($cb62)
  cp   l
  jr   nz, _bd_slow
  ld   a, ($ca0c)
  ld   c, a
  ld   a, ($ca0d)
  ld   b, a
  ld   a, ($ca0b)           ; A = sms bank, BC = label
  jp   rt_far_gate
_bd_slow:
  di                        ; the table scan remaps slot 1 WITHOUT the
                            ; $CB14 discipline — a nested handler would
                            ; restore the caller's bank mid-scan and the
                            ; table reads turn to garbage (phantom
                            ; terminator). Interrupts return below.
  push de                   ; preserve resident X/Y through the search
  ld   a, ($cb14)
  push af                   ; caller's slot-1 bank (restored before far-gate)
  ld   a, :rt_dispatch_table
  ld   ($fffe), a
  ld   hl, rt_dispatch_table
  xor  a
  ld   ($cb7d), a           ; scan counter (diagnostics)
_bd_loop:
  ld   a, ($cb7d)
  inc  a
  ld   ($cb7d), a
  ld   e, (hl)              ; entry addr lo
  inc  hl
  ld   d, (hl)              ; entry addr hi
  inc  hl
  ld   a, d
  or   e
  jr   z, _bd_miss
  ; address match?
  ld   a, d
  cp   b
  jr   nz, _bd_skip
  ld   a, e
  cp   c
  jr   nz, _bd_skip
  ; bank constraint: entry nes_bank $FF matches anything; else compare
  ; with the mapper shadow (only meaningful for window targets).
  ld   a, (hl)
  cp   $ff
  jr   z, _bd_hit
  ld   e, a
  ld   a, ($cb62)
  cp   e
  jr   z, _bd_hit
_bd_skip:
  inc  hl                   ; skip nes_bank
  inc  hl                   ; skip sms bank
  inc  hl                   ; skip label lo
  inc  hl                   ; skip label hi
  jr   _bd_loop
_bd_hit:
  ; Diagnostics + MRU key: the matched (target, entry-bank).
  ld   a, c
  ld   ($cb7a), a
  ld   ($ca08), a
  ld   a, b
  ld   ($cb7b), a
  ld   ($ca09), a
  ld   a, ($cb62)
  ld   ($ca0a), a           ; keyed on the LIVE bank (what the fast path compares)
  ld   a, (hl)
  ld   ($cb7c), a           ; matched entry's NES bank ($FF = fixed)
  inc  hl                   ; -> sms bank byte
  ld   a, (hl)
  inc  hl
  ld   c, (hl)              ; label lo
  inc  hl
  ld   b, (hl)              ; label hi
  ld   e, a                 ; park sms bank
  pop  af                   ; caller's slot-1 bank
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, e                 ; A = target's sms bank, BC = label
  ld   ($ca0b), a
  ld   a, c
  ld   ($ca0c), a
  ld   a, b
  ld   ($ca0d), a
  ld   a, $01
  ld   ($ca0e), a           ; MRU valid
  ld   a, e
  pop  de                   ; restore resident X/Y
  push af
  ld   a, ($cb7e)
  or   a
  jr   nz, _bd_stay_di      ; inside the frame handler: keep DI
  pop  af
  ei
  jp   rt_far_gate
_bd_stay_di:
  pop  af
  jp   rt_far_gate
_bd_miss:
  pop  af
  ld   ($cb14), a
  ld   ($fffe), a
  pop  de
  ld   a, c
  ld   ($cb1b), a
  ld   a, b
  ld   ($cb1c), a
  ld   a, ($cb62)
  ld   ($cb1a), a           ; live NES bank at miss time (diagnostics)
  ld   hl, $0000
  add  hl, sp
  ld   a, (hl)
  ld   ($cb73), a           ; Z80 caller return address (diagnostics)
  inc  hl
  ld   a, (hl)
  ld   ($cb74), a
  ld   a, $e2               ; distinct marker: banked-dispatch miss
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash

; ─── _dispatch_remap_de ───────────────────────────────────────────────────────
; Remaps a NES address in DE to the SMS equivalent if it falls in NES RAM.
; NES $0000-$07FF → SMS $C000-$C7FF (add $C000).
; NES $0800-$1FFF → SMS $C000-$C7FF (mirror: add $C000 and mask to $07FF).
; Other addresses are returned unchanged (ROM, PPU — caller's problem).
; Clobbers: AF.
_dispatch_remap_de:
  ld   a, d
  cp   $08                  ; is high byte < $08? (i.e. NES addr < $0800)
  jr   c, _remap_ram        ; yes: simple $C000 offset
  cp   $20                  ; is high byte < $20? (i.e. addr $0800-$1FFF = mirrors)
  jr   c, _remap_mirror
  ; $2000+ — return unchanged.
  ret
_remap_ram:
  ; NES $0000-$07FF → add $C000.
  ld   a, d
  add  a, $c0
  ld   d, a
  ret
_remap_mirror:
  ; NES $0800-$1FFF — mask to $07FF, then add $C000.
  ld   a, e                 ; keep low byte as-is
  ld   e, a
  ld   a, d
  and  $07                  ; mask high bits to stay within 2KB
  add  a, $c0
  ld   d, a
  ret

; ─── rt_rts_dispatch ──────────────────────────────────────────────────────────
; 6502 `PHA hi / PHA lo / RTS` computed jump. Pop lo, then hi from the
; emulated 6502 stack ($C100 + S), add 1, and transfer through
; rt_banked_tail_dispatch. Lowered code tail-jumps here (no native helper
; return frame) because the 6502 semantics transfer control; they don't return.
rt_rts_dispatch:
  ld   (TR_RET_SCRATCH_A), a ; preserve incoming A while popping shadow stack
  ld   a, ($cb02)           ; 6502 S
  inc  a
  ld   l, a
  ld   h, $c1
  ld   c, (hl)              ; lo (S+1)
  inc  a
  ld   l, a
  ld   b, (hl)              ; hi (S+2)
  ld   ($cb02), a           ; S += 2
  ; BC = target + 1
  inc  bc
  ; RAM-target computed jumps would need translated RAM code — trap via
  ; the dispatcher's miss path ($E2) if the table has no entry.
  ld   a, (TR_RET_SCRATCH_A)
  jp   rt_banked_tail_dispatch

; ─── rt_unresolved_jsr ────────────────────────────────────────────────────────
; Trap: called when the Rust back end emitted a JSR to an address that could
; not be resolved to a translated label at compile time.
; This halts the Z80 with a visible pattern: continuously writes $FF to CRAM
; addr 0 to make the border flash, then halts.
;
; TODO: In a later phase, rt_unresolved_jsr should look up the target in a
; runtime dispatch table (for indirect JSR through profile-annotated jump tables).
rt_unresolved_jsr:
  di
  ld   a, $e1
  ld   ($cb1d), a            ; trace-sms runtime trap marker
  ; Flash screen: write $FF (bright white) to CRAM palette 0.
rt_unresolved_jsr_flash:
  di
_ujsr_flash:
  xor  a
  out  ($bf), a             ; CRAM addr 0 low
  ld   a, $c0
  out  ($bf), a             ; CRAM addr command
  ld   a, $ff
  out  ($be), a             ; white
  xor  a
  out  ($be), a             ; black
  jr   _ujsr_flash          ; loop forever

; ─── rt_brk ───────────────────────────────────────────────────────────────────
; Trap: BRK is used in NES programs to trigger the IRQ/BRK vector.
; For v1 we treat it as a fatal error (SMB never intentionally BRKs).
; Same trap as rt_unresolved_jsr.
; NES BRK is a software interrupt: push state, vector through the IRQ
; handler, RTI back. Games tolerate junk-code excursions this way
; (CV1's task engine lands in data banks and recovers via BRK->RTI).
; Model: far-call the translated IRQ vector and return to the caller
; (the byte after the BRK). The 6502 B-flag/P-push subtleties are not
; modeled — handlers that inspect the pushed P for the B bit would
; need them (none of the current targets do).
rt_brk:
  ld   a, ($cb14)
  push af
  ld   a, :translated_irq
  ld   ($cb14), a
  ld   ($fffe), a
  call translated_irq
  pop  af
  ld   ($cb14), a
  ld   ($fffe), a
  ret

; ─── rt_read_indexed ──────────────────────────────────────────────────────────
; Read a byte at (HL + B).
; Entry: HL = base address, B = unsigned offset.
; Exit:  A = byte at (HL + B). Preserves DE; clobbers BC/HL.
; Generated callers reload HL/B for each indexed read and only consume A after
; the call. Avoid saving BC/HL on the native stack inside nested NMI work.
rt_read_indexed:
  ld   c, b
  ld   b, 0
  add  hl, bc               ; HL = base + offset (unsigned 8-bit offset)
  ld   a, (hl)
  ret

; ─── rt_read_prg_high_indexed ─────────────────────────────────────────────────
; Read a byte from the original NES fixed PRG window ($C000-$FFFF).
; Entry: HL = NES base address in $C000-$FFFF, B = unsigned offset.
; Exit:  A = byte at (HL + B). HL and B preserved. Slot 2 restored to
;        data_prg_low because most translated PRG table reads expect it there.
rt_read_prg_high_indexed:
  push hl
  push bc
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, h
  sub  $40                   ; $C000->$8000 within slot 2
  ld   h, a
  ld   c, b
  ld   b, 0
  add  hl, bc
  ld   a, (hl)
  push af
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  pop  af
  pop  bc
  pop  hl
  ret

; ─── rt_write_indexed ─────────────────────────────────────────────────────────
; Write C to (HL + B).
; Entry: HL = base address, B = unsigned offset, C = value.
; Exit:  (HL + B) = C. Preserves DE; clobbers AF/BC/HL.
; Store helpers are op-boundary calls; callers only need the 6502 accumulator
; preserved in A after STA. Keep the helper stackless for nested NMI pressure.
rt_write_indexed:
  ; HL += B without destroying C (the value to store/return in A).
  ld   a, l
  add  a, b
  ld   l, a
  ld   a, h
  adc  a, 0
  ld   h, a
  ; Hardware windows must not be written as plain memory: indexed stores
  ; like SMB's `STA $4000,X` (X = channel offset) target APU registers,
  ; and `STA $2000,X` targets PPU registers. Forward them to the shims;
  ; plain-memory writes fall through.
  ld   a, h
  cp   $40
  jr   z, _wi_maybe_apu
  cp   $20
  jr   c, _wi_plain
  cp   $40
  jr   c, _wi_ppu           ; $2000-$3FFF: PPU register mirrors
_wi_plain:
  ld   (hl), c              ; write value
  ld   a, c                 ; STA leaves the 6502 accumulator intact: the
                            ; range checks above clobbered A, restore it
                            ; (returning the address byte in A corrupted
                            ; every store that followed an indexed store)
  ret
_wi_maybe_apu:
  ld   a, l
  cp   $18
  jr   nc, _wi_plain        ; $4018+: not an APU register
  cp   $16
  jr   z, _wi_strobe
  ld   a, c
  call rt_apu_write         ; A = value, HL = $40xx (preserves A)
  ret
_wi_strobe:
  ld   a, c
  call rt_controller_strobe
  ret
_wi_ppu:
  ld   a, l
  and  $07
  ld   b, a
  ld   a, c
  call rt_ppu_write         ; A = value, B = register index
  ld   a, ($cb18)           ; body may clobber C/A; restore STA accumulator
  ret

; ─── rt_read_zp_ptr_y ─────────────────────────────────────────────────────────
; 6502 (zp),Y addressing mode read.
; Reads a 16-bit pointer from zero-page at B and B+1, adds Y, dereferences.
; Entry: B = zero-page address (0..255).
; Exit:  A = byte at ((zp[B+1] << 8) | zp[B]) + Y.
; Clobbers: AF, BC, HL. Preserves DE (resident translated X/Y).
;
; Zero page is mirrored at SMS $C000-$C0FF.
; Pointer target is remapped to SMS space if it falls in NES RAM.
rt_read_zp_ptr_y:
  ld   c, e                 ; Phase R: capture resident Y without touching DE
  ; Read pointer from zero page.
  ld   l, b                 ; zero-page offset
  ld   h, $c0               ; SMS base for zero page = $C000
  ld   a, (hl)              ; low byte of pointer
  ; Wrap within zero page for high byte (6502 ZP wraps, not 6502 page-cross bug).
  inc  l                    ; L wraps within $00-$FF automatically (no carry to H)
  ld   h, (hl)              ; high byte of pointer
  ld   l, a                 ; HL = NES pointer value.
  ; Add Y to form effective address.
  ld   b, 0
  add  hl, bc               ; HL = pointer + Y (16-bit)
  ; NES $C000-$FFFF is fixed high PRG: read it through the data_prg_high
  ; copy in slot 2 (SMB's music note streams live at $F800-$FFFF and are
  ; dereferenced via (zp),Y — reading the SMS RAM mirror here fed garbage
  ; notes to the translated sound engine).
  ld   a, h
  cp   $c0
  jr   nc, _rzpy_prg_high
  ; Remap NES RAM/mirrors to SMS RAM in HL without clobbering resident DE.
  cp   $08
  jr   c, _rzpy_remap_ram
  cp   $20
  jr   c, _rzpy_remap_mirror
  jr   _rzpy_deref
_rzpy_remap_ram:
  ld   a, h
  add  a, $c0
  ld   h, a
  jr   _rzpy_deref
_rzpy_remap_mirror:
  ld   a, h
  and  $07
  add  a, $c0
  ld   h, a
_rzpy_deref:
  ; Dereference.
  ld   a, (hl)
  ret
_rzpy_prg_high:
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, h
  sub  $40                  ; $C000-$FFFF -> $8000-$BFFF in slot 2
  ld   h, a
  ld   a, (hl)
  ld   c, a                 ; park result while restoring slot-2 PRG window
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  ld   a, c
  ret

; ─── rt_write_zp_ptr_y ────────────────────────────────────────────────────────
; 6502 (zp),Y addressing mode write.
; Reads pointer from zero page at B and B+1, adds Y, writes A there.
; Entry: B = zero-page address, A = value to write.
; Clobbers: AF (carries through — A still holds the written value on return).
; Preserves HL, BC, DE.
rt_write_zp_ptr_y:
  push hl
  push bc
  ld   c, a                 ; save value to write without touching DE
  ; Read pointer from zero page.
  ld   l, b
  ld   h, $c0
  ld   a, (hl)
  inc  l
  ld   h, (hl)
  ld   l, a                 ; HL = NES pointer value
  ; Add Y.
  ld   a, e                 ; resident Y
  add  a, l
  ld   l, a
  jr   nc, _wzy_addr_ready
  inc  h
_wzy_addr_ready:
  ; Hardware windows: forward APU/PPU targets to the shims (see
  ; rt_write_indexed).
  ld   a, h
  cp   $40
  jr   z, _wzy_maybe_apu
  cp   $20
  jr   c, _wzy_plain
  cp   $40
  jr   c, _wzy_ppu
_wzy_plain:
  ; Remap NES RAM/mirrors to SMS RAM in HL without clobbering resident DE.
  ld   a, h
  cp   $08
  jr   c, _wzy_remap_ram
  cp   $20
  jr   c, _wzy_remap_mirror
  jr   _wzy_store_plain
_wzy_remap_ram:
  add  a, $c0
  ld   h, a
  jr   _wzy_store_plain
_wzy_remap_mirror:
  and  $07
  add  a, $c0
  ld   h, a
_wzy_store_plain:
  ld   a, c                 ; restore value
  ld   (hl), a              ; write
  pop  bc
  pop  hl
  ret

_wzy_maybe_apu:
  ld   a, l
  cp   $18
  jr   nc, _wzy_plain
  cp   $16
  jr   z, _wzy_strobe
  ld   a, c
  call rt_apu_write
  pop  bc
  pop  hl
  ret
_wzy_strobe:
  ld   a, c
  call rt_controller_strobe
  pop  bc
  pop  hl
  ret
_wzy_ppu:
  ld   a, l
  and  $07
  ld   b, a
  ld   a, c
  call rt_ppu_write
  pop  bc
  pop  hl
  ret


.ends
