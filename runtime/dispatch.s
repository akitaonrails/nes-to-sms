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
; ─── native-discipline far transfer (S1.2) ───────────────────────────────────
; NATIVE_CALLS builds (profile `stack_discipline = "native"`) replace the
; software continuation stack with the native Z80 stack: JSR lowers to
; CALL, RTS to RET. Cross-bank transfers preserve the invariant "every
; native return address is executed with the slot-1 bank it was emitted
; for" by pushing a [saved bank][restore thunk] frame before switching;
; the callee's RET unwinds through the thunk, which restores the bank.
;
; rt_far_tail — entry: BC = target label address, H = target SMS bank,
;               A = 6502 accumulator (rides through), DE = resident X/Y.
;   far JSR sites: ld bc,T / ld h,:T / call rt_far_tail
;   far JMP sites: ld bc,T / ld h,:T / jp  rt_far_tail
; A tail transfer whose top-of-stack is already the restore thunk skips
; the push: nothing executes between the two restores, so the
; intermediate bank is dead. This keeps cross-bank tail-jump cycles from
; leaking native stack. (Call sites always push: their own return address
; sits on top, and slot-1 return addresses can never equal the slot-0
; thunk address.)
;
; Scratch: NATIVE_FAR_A/NATIVE_FAR_BANK are main-thread-owned; the IRQ
; bridge saves/restores them per NMI depth (boot.s) so translated-NMI
; far transfers cannot corrupt an interrupted shim.
.define NATIVE_FAR_A    $ca2a
.define NATIVE_FAR_BANK $ca2b

.ifdef NATIVE_CALLS
; rt_far_ncall: same contract as rt_far_tail, minus the top-of-stack merge
; check — a CALL site's own return address was just pushed, so it can
; never be the restore thunk and the frame push is unconditional.
rt_far_ncall:
  ld   (NATIVE_FAR_A), a
  ld   a, h
  ld   (NATIVE_FAR_BANK), a
  jr   _far_push_frame

rt_far_tail:
  ld   (NATIVE_FAR_A), a
  ld   a, h
  ld   (NATIVE_FAR_BANK), a
  ld   hl, $0000
  add  hl, sp
  ld   a, (hl)
  cp   <_far_ret_thunk
  jr   nz, _far_push_frame
  inc  hl
  ld   a, (hl)
  cp   >_far_ret_thunk
  jr   z, _far_no_frame
_far_push_frame:
  ld   a, ($cb14)
  ld   l, a                  ; frame word: L = saved bank (H is don't-care)
  push hl
  ld   hl, _far_ret_thunk
  push hl
_far_no_frame:
  ld   a, (NATIVE_FAR_BANK)
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, (NATIVE_FAR_A)
  push bc
  ret                        ; transfer to BC

_far_ret_thunk:
  ld   (NATIVE_FAR_A), a
  pop  hl                    ; L = saved bank
  ld   a, l
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, (NATIVE_FAR_A)
  ret
.endif

; ─── translated software transfer gates ───────────────────────────────────────
.ifdef NATIVE_CALLS
; Runtime-internal dispatchers (computed RTS/JMP, RTI recovery) still name
; the tail gate; adapt its register contract onto the native far shim so
; every dispatch transfer maintains the bank-restore invariant.
rt_translated_tail_gate:
  ; Entry: BC=target, A=target bank, H=entry 6502 A.
  ld   l, a
  ld   a, h
  ld   h, l
  jp   rt_far_tail
rt_translated_call_gate:
  ; Software continuation frames do not exist in native builds.
  ld   a, $E6
  ld   ($cb1d), a
  jp   rt_unresolved_jsr
.else
rt_translated_call_gate:
  ; Entry: BC=target, A=target bank, HL=software frame base, DE=resident X/Y.
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, (hl)              ; entry A from frame[0]
  ld   h, b
  ld   l, c
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $01
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)

rt_translated_tail_gate:
  ; Entry: BC=target, A=target bank, H=entry A, DE=resident X/Y.
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, h
  ld   h, b
  ld   l, c
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $02
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)
.endif

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
  ; The target is parked on the NATIVE STACK, not in fixed RAM: the old
  ; $CB2E scratch word was shared across invocations, so a nested IRQ
  ; whose handler ran its own far transfer inside this window clobbered
  ; the in-flight target (caught on Mednafen as a far_gate jump to $0000).
.ifdef DIAG_WILDJUMP
  ld   ($ca3f), a
  ld   a, b
  or   c
  jr   nz, _fg_entry_ok
  ld   a, $04                ; marker 4: far_gate ENTERED with target $0000
  jp   rt_diag_zero_cont
_fg_entry_ok:
  ld   a, ($ca3f)
.endif
  ld   h, b
  ld   l, c                  ; HL = target
  ld   c, a                  ; C = target bank
  push hl                    ; park target (re-entrancy safe)

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
  pop  hl                    ; target back
  ld   bc, _far_after
  push bc
  ld   a, ($cb15)           ; caller A (JSR/JMP preserve the accumulator)
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $03
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
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
.ifdef DIAG_WILDJUMP
  ld   ($ca3f), a
  ld   a, b
  or   c
  jr   nz, _fgc_entry_ok
  ld   a, $05                ; marker 5: far_gate_cont ENTERED with target $0000
  jp   rt_diag_zero_cont
_fgc_entry_ok:
  ld   a, ($ca3f)
.endif
  push bc                     ; park target on the native stack (see rt_far_gate)
  ld   b, h                   ; B = continuation high
  ld   c, a                   ; C = target bank
  ld   a, l                   ; A = continuation low

  ; RESERVE the full 3-byte frame FIRST, then fill it. The old order wrote
  ; the continuation bytes before advancing the pointer: a nested IRQ
  ; far-call landing between those writes reused the same slot and left a
  ; mixed/stale continuation behind (popped later as a wild jump).
  ; 16-bit INC/LD keep native flags intact.
  ld   hl, (FAR_BANK_STACK_PTR)
  inc  hl
  inc  hl
  inc  hl
  ld   (FAR_BANK_STACK_PTR), hl
  dec  hl                     ; HL = slot+2
  dec  hl
  ld   (hl), b                ; slot+1 = continuation high
  dec  hl
  ld   (hl), a                ; slot+0 = continuation low
  inc  hl
  inc  hl
  ld   a, ($cb14)
  ld   (hl), a                ; slot+2 = previous slot-1 bank

  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a
  pop  hl                     ; parked target
  ld   bc, _far_after_cont
  push bc
  ld   a, ($cb15)             ; caller A (JSR preserves the accumulator)
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $04
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
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
.ifdef DIAG_WILDJUMP
  ld   a, b
  or   c
  jr   nz, _fac_cont_ok
  ld   a, $02                ; marker 2: far LIFO popped zero continuation
  jp   rt_diag_zero_cont
_fac_cont_ok:
.endif
  pop  af                   ; restore target return A/F
  push bc
  ret                       ; resume call site continuation

.ifdef DIAG_WILDJUMP
; Zero-continuation freeze. A = marker naming the pop site; records the
; native SP + a 16-byte stack window and halts with a red backdrop so a
; Mednafen savestate captures the guilty pop in place (RAM keeps the TR
; frames, far LIFO, and shadow state exactly as the pop saw them).
;   $CA21  marker   $CA24  SP   $CA28  stack window SP-8..SP+7
rt_diag_zero_cont:
  di
  ld   ($ca21), a
  ld   hl, $0000
  add  hl, sp
  ld   ($ca24), hl
  ld   bc, $fff8
  add  hl, bc
  ld   de, $ca28
  ld   bc, $0010
  ldir
  ld   a, $03                ; RED backdrop
  call rt_boot_beacon
_dzc_halt:
  di
  halt
  jr   _dzc_halt
.endif

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
.ifdef DIAG_WILDJUMP
  ld   a, b
  or   c
  jr   nz, _tr_rts_cont_ok
  ld   a, $01                ; marker 1: rt_translated_rts popped zero cont
  jp   rt_diag_zero_cont
_tr_rts_cont_ok:
.endif
  inc  hl
  ld   a, (hl)               ; return bank + translated-frame flags
  bit  6, a
  jr   z, _tr_rts_bank_ready ; ordinary frames already contain a clean bank
  ; A stack-aware JumpEngine frame represents an RTS address that the NES
  ; caller explicitly placed on $0100+S. Its handler's RTS consumes those two
  ; bytes before resuming the translated continuation.
  ld   a, ($cb02)
  inc  a
  inc  a
  ld   ($cb02), a
  ld   a, (hl)
  and  $1f                   ; SMS code bank (flags occupy the high bits)
_tr_rts_bank_ready:
  ld   ($cb14), a
  ld   ($fffe), a
  dec  hl
  dec  hl
  dec  hl                    ; HL = frame base, still owned
  ld   a, (hl)               ; restore returned A
  ld   (TR_RET_PTR), hl       ; publish pop after all frame reads
  ld   h, b
  ld   l, c
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $05
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)
_tr_rts_underflow:
  ; No live TR frame for this RTS: the native call context was abandoned
  ; (junk-BRK cascade + defensive recovery, realigned dispatch resume).
  ; NES semantics: the RTS pops whatever the GAME's 6502 stack holds —
  ; and ours ($C100+S) carries the game-arranged bytes (BRK frames,
  ; explicit pushes, the defensive vector's repairs). Fall back to the
  ; emulated-stack dispatch; a garbage pop chains into the dispatcher's
  ; realignment retries and only then the loud trap. Keep the $E3 mark
  ; for telemetry (cleared on successful dispatch by the next hit).
  ld   a, $e3
  ld   ($cb1d), a
  ld   a, c                  ; returned 6502 A (parked at entry)
  jp   rt_rts_dispatch

; ─── rt_translated_return_escape ─────────────────────────────────────────────
; Bridge a profiled 6502 tail escape that discards one JSR return address as
; stack data before returning through the caller below it. Translated JSRs keep
; continuations in TR_RET rather than at $C100+S, so the escape edge calls here
; with BC equal to the original 6502 JSR-pushed return PC. Pop one TR_RET frame,
; push BCH then BCL using 6502 stack order, and return with A and DE preserved.
; Invalid/empty translated stacks fail closed with marker $E5.
rt_translated_return_escape:
  push af
  ld   a, i                  ; P/V = prior IFF2
  di
  jp   po, _tr_escape_was_disabled
  ld   a, $01
  ld   ($d471), a
  jr   _tr_escape_find_frame
_tr_escape_was_disabled:
  xor  a
  ld   ($d471), a
_tr_escape_find_frame:
  ld   hl, (TR_RET_PTR)
  ld   (TR_RET_DIAG_PTR), hl
  ld   a, h
  cp   $d3
  jr   z, _tr_escape_check_seg0
  cp   $d5
  jr   z, _tr_escape_check_seg1
  cp   $d6
  jp   nz, _tr_escape_underflow
  ld   a, l
  or   a
  jp   nz, _tr_escape_underflow
  ld   hl, $d5fc             ; ptr == D600 pops final segment-1 frame
  jr   _tr_escape_pop_frame
_tr_escape_check_seg0:
  ld   a, l
  cp   $01
  jp   c, _tr_escape_underflow
  cp   $f9
  jp   nc, _tr_escape_underflow
  and  $03
  jp   nz, _tr_escape_underflow
  dec  hl
  dec  hl
  dec  hl
  dec  hl
  jr   _tr_escape_pop_frame
_tr_escape_check_seg1:
  ld   a, l
  or   a
  jr   z, _tr_escape_bridge
  and  $03
  jp   nz, _tr_escape_underflow
  dec  hl
  dec  hl
  dec  hl
  dec  hl
  jr   _tr_escape_pop_frame
_tr_escape_bridge:
  ld   hl, $d3f8
_tr_escape_pop_frame:
  ld   (TR_RET_PTR), hl       ; publish the discarded frame

  ld   a, ($cb02)            ; old 6502 S
  ld   l, a
  ld   h, $c1
  ld   (hl), b               ; JSR pushes return high first
  dec  l                     ; page wraps naturally within $C100-$C1FF
  ld   (hl), c               ; then return low
  sub  $02
  ld   ($cb02), a

  ld   a, ($d471)
  or   a
  jr   z, _tr_escape_return_disabled
  pop  af
  ei
  ret
_tr_escape_return_disabled:
  pop  af
  ret
_tr_escape_underflow:
  ld   a, $e5
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
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $06
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)
_ij_jump_di:
  ld   a, (TR_RET_SCRATCH_A)
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $07
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)

; ─── rt_banked_tail_dispatch ──────────────────────────────────────────────────
; BC = NES ROM target address, A = incoming translated A, DE resident X/Y.
; Slot-0 tail dispatch for computed JMP/RTS paths: uses the generated page
; directory, then scans only the target address's high-byte group,
; switches slot 1 from slot 0, restores A, and jumps directly to the target.
; No rt_far_gate and no native return/restore frame.
rt_banked_tail_dispatch:
  di
  ld   (TR_RET_SCRATCH_A), a   ; preserve incoming A through scan/mapper write
.ifdef DIAG_WILDJUMP
  ld   a, b
  or   c
  jr   nz, _btd_target_ok
  ld   a, $03                ; marker 3: dispatch requested for NES $0000
  jp   rt_diag_zero_cont
_btd_target_ok:
  ld   a, (TR_RET_SCRATCH_A)
.endif
  ld   a, c
  ld   ($cb1b), a              ; requested target diagnostics / scan key
  ld   a, b
  ld   ($cb1c), a
  ld   a, :rt_dispatch_table
  ld   ($fffe), a
  ld   a, ($cb1c)
  cp   $80
  jp   c, _btd_miss
  sub  $80
  add  a, a
  ld   l, a
  ld   h, $00
  ld   bc, rt_dispatch_page_table
  add  hl, bc
  ld   a, (hl)
  inc  hl
  ld   h, (hl)
  ld   l, a
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
  jr   nz, _btd_miss
  ld   a, ($cb1b)
  cp   c
  jr   z, _btd_check_bank
  jr   c, _btd_miss         ; sorted page: next entry is above the target
  jr   _btd_skip
_btd_check_bank:
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
  xor  a
  ld   ($cb1d), a            ; recovered: clear any transient trap mark
  ld   a, (hl)
  ld   ($cb7c), a
  inc  hl
  ld   a, (hl)                 ; SMS bank
  ld   (TR_RET_SCRATCH_BANK), a
  inc  hl
  ld   c, (hl)                 ; label lo
  inc  hl
  ld   b, (hl)                 ; label hi
.ifdef NATIVE_CALLS
  ; Native discipline: a computed dispatch is a tail transfer — route it
  ; through the far shim so a bank change leaves the restore-thunk frame
  ; (or merges with the pending one). BC already holds the label.
  ld   a, (TR_RET_SCRATCH_BANK)
  ld   h, a                    ; H = target bank
  ld   a, ($cb7e)
  or   a
  jr   nz, _btd_native_di
  ld   a, (TR_RET_SCRATCH_A)
  ei
  jp   rt_far_tail
_btd_native_di:
  ld   a, (TR_RET_SCRATCH_A)
  jp   rt_far_tail
.endif
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
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $08
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)
_btd_jump_di:
  ld   a, (TR_RET_SCRATCH_A)
  .ifdef DIAG_WILDJUMP
  ld   ($ca3f), a            ; breadcrumb: last computed transfer
  ld   ($ca3d), hl
  ld   a, $09
  ld   ($ca3c), a
  ld   a, ($ca3f)
  .endif
  jp   (hl)
_btd_miss:
  ; Misaligned-return realignment (NES semantics): an RTI/RTS-dispatch
  ; target with no table entry is a return into the MIDDLE of a lifted
  ; instruction (junk-BRK recovery returns to BRK+2; a real 6502
  ; re-synchronizes with the instruction stream within a few bytes).
  ; Approximate: dispatch to the NEXT lifted boundary — one table pass
  ; for the smallest fixed-bank entry >= target. Falls back to the
  ; data BRK-walk (below) when no entry follows; switchable-bank
  ; targets keep the loud trap (fail closed).
  ld   a, ($cb1c)
  cp   $c0
  jp   c, _btd_trap_flash
  ld   a, $ff
  ld   ($ca15), a            ; best addr = $FFFF (none)
  ld   ($ca16), a
  ld   a, :rt_dispatch_table
  ld   ($fffe), a
  ld   hl, rt_dispatch_table
_btd_next_loop:
  ld   a, (hl)
  ld   c, a                  ; entry addr lo
  inc  hl
  ld   a, (hl)
  ld   b, a                  ; entry addr hi
  inc  hl
  or   c
  jr   z, _btd_next_done     ; terminator (addr $0000)
  ld   a, (hl)               ; NES bank constraint
  cp   $ff
  jr   nz, _btd_next_skip    ; only fixed-bank entries realign
  ; entry >= target?
  ld   a, ($cb1c)
  cp   b
  jr   z, _btd_next_hicmp_eq
  jr   nc, _btd_next_skip    ; target hi > entry hi -> entry below target
  jr   _btd_next_ge
_btd_next_hicmp_eq:
  ld   a, ($cb1b)
  cp   c
  jr   z, _btd_next_ge
  jr   nc, _btd_next_skip
_btd_next_ge:
  ; entry < best?
  ld   a, ($ca16)
  cp   b
  jr   c, _btd_next_skip
  jr   nz, _btd_next_better
  ld   a, ($ca15)
  cp   c
  jr   c, _btd_next_skip
  jr   z, _btd_next_skip
_btd_next_better:
  ld   a, c
  ld   ($ca15), a
  ld   a, b
  ld   ($ca16), a
  ld   ($ca17), hl           ; best entry ptr (at the bank byte)
_btd_next_skip:
  inc  hl
  inc  hl
  inc  hl
  inc  hl
  jr   _btd_next_loop
_btd_next_done:
  ld   a, ($ca16)
  cp   $ff
  jr   nz, _btd_next_take
  ld   a, ($ca15)
  cp   $ff
  jr   z, _btd_trap          ; nothing after target: try the data walk
_btd_next_take:
  ld   hl, ($ca17)
  jp   _btd_hit
_btd_trap:
  ; Realignment exhausted: the target sits in a DATA region (no lifted
  ; instruction boundary within reach — music/tables). NES semantics
  ; for a walk through data: the bytes execute until a $00 acts as BRK
  ; and re-enters the IRQ vector. Emulate that: scan fixed PRG for the
  ; next $00 and take the software interrupt from there. Each cycle
  ; makes forward progress (BRK+2 resumes past the previous zero), so
  ; the walk crosses the data desert until realignment lands back in
  ; real code or the game's defensive vector rewrites control flow.
  ld   a, ($cb1c)
  cp   $c0
  jp   c, _btd_trap_flash    ; switchable-bank target: keep fail-closed
  ld   a, ($cb14)
  ld   ($fffe), a            ; put the caller's bank back in slot 1
  ; DI/depth0 locked transaction: assert canonical slot 2 before scanning.
  ld   a, i
  jp   pe, _btd_slot2_bad
  ld   a, ($d47f)
  or   a
  jp   nz, _btd_slot2_bad
  ld   a, ($fffc)
  or   a
  jp   nz, _btd_slot2_bad
  ld   a, ($ffff)
  ld   b, a
.ifdef NES_PRG_BANK_BASE
  ld   a, ($cb62)
  and  NES_PRG_BANK_MASK
  add  a, NES_PRG_BANK_BASE
  cp   b
.else
  ld   a, :data_prg_low
  cp   b
.endif
  jp   nz, _btd_slot2_bad
  ; Locked map to fixed PRG; no calls/pushes in this transaction.
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, ($cb1c)
  sub  $40                   ; NES $C000-$FFFF -> slot 2 $8000-$BFFF
  ld   h, a
  ld   a, ($cb1b)
  ld   l, a
  ld   a, 64
  ld   ($ca14), a            ; walk budget
_btd_walk:
  ld   a, (hl)
  or   a
  jr   z, _btd_walk_found
  inc  hl
  ld   a, h
  cp   $c0                   ; ran off the top of slot 2: give up
  jr   nc, _btd_walk_none
  ld   a, ($ca14)
  dec  a
  ld   ($ca14), a
  jr   nz, _btd_walk
_btd_walk_none:
  xor  a
  ld   ($fffc), a
.ifdef NES_PRG_BANK_BASE
  ld   a, ($cb62)
  and  NES_PRG_BANK_MASK
  add  a, NES_PRG_BANK_BASE
.else
  ld   a, :data_prg_low
.endif
  ld   ($ffff), a
  jr   _btd_trap_flash
_btd_walk_found:
  ; NES BRK at HL(slot2) -> NES addr = HL + $4000; return PC = addr + 2
  xor  a
  ld   ($fffc), a
.ifdef NES_PRG_BANK_BASE
  ld   a, ($cb62)
  and  NES_PRG_BANK_MASK
  add  a, NES_PRG_BANK_BASE
.else
  ld   a, :data_prg_low
.endif
  ld   ($ffff), a
  ld   a, h
  add  a, $40
  ld   h, a
  inc  hl
  inc  hl
  xor  a
  ld   ($cb1d), a            ; walking, not trapped
  ld   a, (TR_RET_SCRATCH_A)
  jp   rt_brk
_btd_slot2_bad:
  di
  ld   a, ($cb62)
  ld   ($cb1a), a
  ld   a, RT_BTD_SLOT2_BAD
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash
_btd_trap_flash:
  ld   a, ($cb14)
  ld   ($fffe), a
  ld   a, ($cb62)
  ld   ($cb1a), a
  ld   a, $e2
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash

; ─── rt_banked_dispatch ───────────────────────────────────────────────────────
; BC = NES ROM target address. Look it up through the generated high-byte page
; directory, then the address-sorted dispatch records for that page. Entries
; are .dw nes_addr / .db nes_bank / .db sms bank / .dw label,
; terminated by addr $0000. Fixed-bank entries carry nes_bank $FF and
; match any window state; window entries ($8000-$BFFF) also require the
; current UxROM bank shadow ($CB62) to match. Hit -> far-gate jump (bank
; restore on return included). Miss -> loud trap ($CB1D=$E2, target in
; $CB1B/1C) — fail closed, never run raw NES bytes.
rt_banked_dispatch:
  ; Entry A is the 6502 accumulator the callee expects: the transfer gate
  ; restored it from the pushed call frame before jumping to the stub.
  ; The lookup below clobbers A, and rt_far_gate re-reads the caller A from
  ; $CB15, so park it there first — otherwise a banked callee whose first
  ; instruction stores A (CV1's sound-trigger entry STA $E5 at $8187)
  ; silently receives a stale accumulator.
  ld   ($cb15), a
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
  ld   a, b
  cp   $80
  jp   c, _bd_miss
  sub  $80
  add  a, a
  ld   l, a
  ld   h, $00
  ld   de, rt_dispatch_page_table
  add  hl, de
  ld   e, (hl)
  inc  hl
  ld   d, (hl)
  ex   de, hl
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
  jr   nz, _bd_miss
  ld   a, e
  cp   c
  jr   z, _bd_check_bank
  jr   c, _bd_skip
  jr   _bd_miss             ; sorted page: next entry is above the target
_bd_check_bank:
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

; ─── rt_rti ───────────────────────────────────────────────────────────────────
; 6502 RTI: pop P, then PCL, PCH from the emulated stack and resume at the
; popped PC. Interrupt frames pushed by the runtime (the NMI bridge in
; boot.s, rt_brk) carry PC values a game either leaves alone or rewrites:
;   - sentinel $FFFF (NMI bridge): the frame belongs to the native bridge
;     call — return natively. Fast path, taken by every normal NMI.
;   - anything else (BRK return address, or a recovery that rewrote the
;     stacked PC before RTI): transfer through the banked dispatcher.
;     6502 semantics, not native call pairing.
; Entry: A = 6502 A (preserved), DE = resident X/Y (preserved). Lowered
; RTI tail-jumps here.
rt_rti:
  ld   (TR_RET_SCRATCH_A), a
  ld   a, ($cb02)            ; 6502 S
  ld   h, $c1
  inc  a
  ld   l, a
  ld   a, (hl)               ; P (pushed last, popped first)
  ld   ($cb03), a
  inc  l                     ; page-wrapping, like the 6502 stack
  ld   c, (hl)               ; PCL
  inc  l
  ld   b, (hl)               ; PCH
  ld   a, ($cb02)
  add  a, 3
  ld   ($cb02), a            ; S += 3
  ld   a, b
  and  c
  inc  a                     ; Z iff PC == $FFFF (both bytes $FF)
  jr   nz, _rti_dispatch
  ld   a, (TR_RET_SCRATCH_A)
  ret
_rti_dispatch:
  ; A non-sentinel RTI is a 6502-level control transfer that ABANDONS the
  ; interrupted native context: CV1's junk-dispatch recovery rewrites the
  ; stacked PC (after TXS stack repair) and RTIs to its main flow, never
  ; returning through anything between. The abandoned native stack words,
  ; translated-call frames, far-LIFO frames, and NMI depth bookkeeping
  ; would otherwise leak and later unwind through mismatched or virgin
  ; (zero) frames — observed on Mednafen as a jp $0000 reboot loop, ~80
  ; reboots/minute. Reset all of it, exactly as the game's own TXS reset
  ; the 6502 stack. (The bridge's sentinel path above never comes here.)
  ld   sp, $dffc              ; native stack base (see boot.s)
  ld   hl, TR_RET_BASE
  ld   (TR_RET_PTR), hl       ; translated-call frames: empty
  ld   hl, FAR_BANK_STACK_BASE
  ld   (FAR_BANK_STACK_PTR), hl
  xor  a
  ld   ($cb7e), a             ; not in handler context
  ld   ($ca12), a             ; presentation guard clear
  ld   a, ($ca11)
  or   a
  jr   z, _rti_depth_done     ; interrupted outside any translated NMI
  ld   a, $01                 ; resident-NMI depth (main-in-NMI games);
  ld   ($ca11), a             ; a nested handler that BRKed is abandoned
_rti_depth_done:
  ei                          ; frame IRQs must keep flowing to the game
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
; NES BRK is a software interrupt: push PCH, PCL (return = BRK+2), then
; P with the B flag, set I, and vector through the IRQ handler. Games
; tolerate junk-code excursions this way (CV1's task engine lands in
; data banks and recovers via BRK->IRQ; the recovery may rewrite the
; stacked PC or reset S entirely before its RTI). All of that only
; works if the frame is real and the eventual RTI honors the stacked
; PC — see rt_rti. Control never returns here: 6502 semantics transfer
; to the IRQ vector, and the RTI dispatches wherever the (possibly
; rewritten) frame says.
; Entry: HL = NES return PC (BRK site + 2), A = 6502 A, DE = resident
; X/Y (preserved). Lowered BRK tail-jumps here.
rt_brk:
  ld   (TR_RET_SCRATCH_A), a
  ld   b, h                  ; B = PCH, C = PCL
  ld   c, l
  ; RESERVE the 3-byte frame first (publish S-3), then fill: an IRQ/NMI
  ; bridge push landing mid-fill would otherwise clobber the frame.
  ld   a, ($cb02)            ; 6502 S
  ld   l, a
  ld   h, $c1
  sub  3
  ld   ($cb02), a            ; publish S-3
  ld   (hl), b               ; PCH at old S
  dec  l                     ; page-wrapping, like the 6502 stack
  ld   (hl), c               ; PCL at S-1
  dec  l
  ld   a, ($cb03)
  or   $30                   ; pushed P carries B + bit 5 (6502 BRK)
  ld   (hl), a               ; P at S-2
  ld   a, ($cb03)
  or   $04                   ; live P: I set on interrupt entry
  ld   ($cb03), a
  ld   a, :translated_irq
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, (TR_RET_SCRATCH_A)
  jp   translated_irq

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

; ─── rt_read_prg_high ─────────────────────────────────────────────────────────
; Entry HL = effective NES $C000-$FFFF. Exit A = byte; preserves C/DE.
; The fixed high image is mapped only for this outer transaction.
rt_read_prg_high:
  ld   a, h
  cp   $c0
  jp   c, _rph_bad
.ifndef NES_PRG_BANK_BASE
  ; NROM has no mutable NES bank shadow, and irq_handler saves/restores the
  ; interrupted slot-2 bank on its own re-entrant stack frame (boot.s). A
  ; map/read/restore sequence here is therefore IRQ-safe with interrupts
  ; enabled — an IRQ landing mid-sequence presents from the canonical low
  ; bank and puts data_prg_high back before returning. No DI, no IFF juggle.
  ; (The generated inline fixed-high reads already rely on this invariant.)
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, h
  sub  $40
  ld   h, a
  ld   b, (hl)
  ld   a, :data_prg_low
  ld   ($ffff), a
  ld   a, b
  ret
.else
  ; Mapper builds keep the selected NES bank live in slot 2. Do not disturb
  ; it merely to read the fixed bank: this helper executes from slot 0, so it
  ; can map the fixed PRG image into slot 1, read it at $4000-$7FFF, and
  ; restore the translated-code bank from its authoritative $CB14 shadow.
  ; Keeping the transaction interrupt-atomic prevents an IRQ from entering
  ; while slot 1 contains data rather than translated code. This path needs
  ; no mapper guard depth, SRAM snapshot, or selected-bank reconstruction
  ; because slot 2 and $FFFC remain untouched.
  ld   a, i
  di
  jp   po, _rph_slot1_di
_rph_slot1_ei:
  ld   a, :data_prg_high
  ld   ($fffe), a
  ld   a, h
  sub  $80
  ld   h, a
  ld   b, (hl)
  ld   a, ($cb14)
  ld   ($fffe), a
  ld   a, b
  ei
  ret
_rph_slot1_di:
  ld   a, :data_prg_high
  ld   ($fffe), a
  ld   a, h
  sub  $80
  ld   h, a
  ld   b, (hl)
  ld   a, ($cb14)
  ld   ($fffe), a
  ld   a, b
  ret
.endif
_rph_bad:
  di
  ld   a, RT_PRG_HIGH_BAD_ADDRESS
  ld   ($cb1d), a
_rph_halt:
  halt
  jr   _rph_halt

; ─── rt_read_prg_high_indexed ─────────────────────────────────────────────────
; Read a byte from the original NES fixed PRG window ($C000-$FFFF).
; Entry: HL = NES base address in $C000-$FFFF, B = unsigned offset.
; Exit:  A = byte at (HL + B). Preserves C/DE; clobbers B/HL/native flags.
;        Slot-2 mapper state is restored exactly.
rt_read_prg_high_indexed:
  ld   a, l
  add  a, b
  ld   l, a
  ld   a, h
  adc  a, 0
  ld   h, a
  jp   rt_read_prg_high

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
  jp   rt_read_prg_high

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
