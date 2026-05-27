; stack6502.s — Emulated 6502 stack operations.
;
; The 6502 stack lives at NES $0100-$01FF, mirrored at SMS $C100-$C1FF.
; The stack pointer byte (S) lives in shadow register at $CB02.
; The 6502 stack grows downward: push decrements S after write,
; pop increments S before read.
;
; NOTE: These routines touch the EMULATED 6502 stack only.
; The Z80 native stack (at $DFFE growing down) is completely separate.
; JSR/RTS use the Z80 native CALL/RET — not these routines.
; Only 6502 PHA, PHP, PLA, PLP use these.

.section "stack6502" free

; ─── rt_push6502 ──────────────────────────────────────────────────────────────
; 6502 PUSH: write A to $C100 + shadow_S, then decrement shadow_S.
; Entry: A = value to push.
; Exit:  A preserved.  Shadow S decremented.
rt_push6502:
  push hl
  push de
  ld   d, a                  ; preserve pushed value / caller A
  ; Compute destination address: $C100 + S.
  ld   a, ($cb02)           ; load shadow S
  ld   l, a                 ; low byte = S
  ld   h, $c1               ; high byte = $C1 → address $C100 + S
  ld   (hl), d              ; write value to emulated 6502 stack
  ; Decrement shadow S (wraps within page: $00 - 1 = $FF).
  ld   a, ($cb02)
  dec  a
  ld   ($cb02), a
  ld   a, d                 ; restore caller's A
  pop  de
  pop  hl
  ret

; ─── rt_pop6502 ───────────────────────────────────────────────────────────────
; 6502 POP: increment shadow_S, then read A from $C100 + shadow_S.
; Entry: (nothing).
; Exit:  A = popped value.  Shadow S incremented.
rt_pop6502:
  push hl
  push bc
  ; Increment shadow S first (6502 pull = pre-increment).
  ld   a, ($cb02)
  inc  a
  ld   ($cb02), a
  ; Read from $C100 + new_S.
  ld   l, a
  ld   h, $c1
  ld   a, (hl)              ; A = popped value
  pop  bc
  pop  hl
  ret

.ends
