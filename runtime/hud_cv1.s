; CV1 committed horizontal HUD. Opaque sprite0/BG overlap is row38; the
; canonical six-row nametable band leaves rows39..47 blank before playfield48.
; All helpers require DI and a closed VDP latch, clobber AF/BC/DE/HL, and leave
; mapper/guard state and guest shadows untouched. No alternate/index registers.
; R9 is a whole-frame latch on SMS: unequal pre/post Y explicitly disables the
; horizontal split and records Y_UNSUPPORTED, never pretends to split Y.
.ifdef CV1_RUNTIME_HOOKS
.define CV1_HUD_FLAGS $c813       ; valid1, split2, armed4, served8, unsupportedY16
.define CV1_HUD_PRE_X $c814       ; encoded SMS R8, not NES X
.define CV1_HUD_Y $c815
.define CV1_HUD_POST_X $c816
.define CV1_HUD_LINE $c817
.define CV1_HUD_R0 $c818          ; IE1 clear, all other policy bits preserved
.define CV1_HUD_EPOCH $c819
.define CV1_HUD_ARMED_EPOCH $c81a
.define CV1_HUD_SERVED_EPOCH $c81b
.define CV1_HUD_ENTRY_VC $c81c     ; DIAG_CV1_HUD only
.define CV1_HUD_LAST_VC $c81d
.define CV1_HUD_LATE_COUNT $c81e

.section "hud_cv1" free

; HL=immutable nine-byte frame controls (frame_cv1 packet layout), DE=pending
; six-byte record, possibly SRAM. No VDP I/O or mapping changes. The pending
; record contains flags, preR8, wholeR9, postR8, R10, baseR0, in that order.
rt_cv1_hud_prepare:
  push de                     ; pending base
  push hl
  inc hl
  inc hl
  inc hl
  inc hl
  ld a, (hl)
  ld b, a
  bit 2, a
  jr z, _cv1_hud_prepare_live
  inc hl
  inc hl
  inc hl
  jr _cv1_hud_prepare_post
_cv1_hud_prepare_live:
  dec hl
  dec hl
_cv1_hud_prepare_post:
  ld a, (hl)
  neg
  ld c, a
  inc hl
  ld a, (hl)
  inc de
  inc de
  ld (de), a                  ; whole-frame Y
  inc de
  ld a, c
  ld (de), a                  ; post/live R8
  pop hl                      ; immutable input base
  ld a, b
  ld b, 1
  and 6
  cp 6
  jr nz, _cv1_hud_prepare_store
  inc hl
  ld a, (hl)
  ld c, a                     ; MASK
  inc hl
  inc hl
  inc hl
  inc hl
  inc hl
  ld a, (hl)
  ld b, a                     ; preY
  dec de
  ld a, (de)                  ; chosen postY
  cp b
  ld b, 1
  jr z, _cv1_hud_prepare_equal_y
  set 4, b                    ; expose unsupported mid-frame vertical split
  jr _cv1_hud_prepare_store
_cv1_hud_prepare_equal_y:
  ld a, c
  bit 3, a                    ; no HUD split when NES background is disabled
  jr z, _cv1_hud_prepare_store
  set 1, b
  dec hl
  ld a, (hl)
  neg
  ld c, a                     ; preR8 only when a valid split exists
_cv1_hud_prepare_store:
  pop hl                      ; pending base
  ld a, b
  ld (hl), a
  inc hl
  bit 1, b
  ld a, c
  jr nz, _cv1_hud_prepare_pre_x
  inc hl
  inc hl
  ld a, (hl)                  ; no split: direct post/live X throughout
  dec hl
  dec hl
_cv1_hud_prepare_pre_x:
  ld (hl), a
  inc hl
  inc hl
  inc hl
  ld a, 38
  ld (hl), a
  inc hl
  ld a, VDP_R0_BASE
  and $ef
  ld (hl), a
  ret

; HL=pending six-byte record. Called inside an admitted complete graphics
; commit after BG/CRAM, before SAT's final reg1 enable. No epoch increment.
rt_cv1_hud_commit:
  ld de, CV1_HUD_FLAGS
  ld bc, 6
  ldir
  ; LDIR leaves C=0: commit never acknowledges status. Coherent commits are
  ; IRQ-origin only; the frame entry already acknowledged VINT and stale HINT.
  jp _cv1_hud_arm

; Every acknowledged physical VBlank, even READY=0 or presentation busy.
; Flags, not epoch equality alone, own validity across eight-bit wrap.
rt_cv1_hud_vblank:
  call rt_cv1_hud_epoch_begin
rt_cv1_hud_rearm:
  ld c, 1                     ; intentional stale-HINT acknowledgment on rearm
  jp _cv1_hud_arm

; Epoch-only early entry: let the coherent consumer commit a frozen packet
; before paying to rearm old controls. Caller then rearms only on no commit.
rt_cv1_hud_epoch_begin:
  ld hl, CV1_HUD_EPOCH
  inc (hl)
  ld hl, CV1_HUD_FLAGS
  res 2, (hl)
  res 3, (hl)
  ret
_cv1_hud_arm:
  ld a, (CV1_HUD_FLAGS)
  bit 0, a
  ret z
  in a, ($7e)
  sub $e0
  cp 29                       ; E0..FC; latest FC is physical258, not stock252
  jr nc, _cv1_hud_late
  ; Two complete stock lines (456T) remain before final line261 reload/latch.
  ; The short HUD-only arm is bounded independently of the longer SAT burst.
  ; IE1 may have a stale pending latch from an earlier disabled underflow.
  ; Disable, acknowledge ONLY at this intentional VBlank boundary, then arm.
  ld a, (CV1_HUD_R0)
  out ($bf), a
  ld a, $80
  out ($bf), a
  ld a, c
  or a
  jr z, _cv1_hud_no_ack
  in a, ($bf)
_cv1_hud_no_ack:
  ld a, (CV1_HUD_FLAGS)
  bit 1, a
  jr z, _cv1_hud_direct
  ld a, (CV1_HUD_PRE_X)
  out ($bf), a
  ld a, $88
  out ($bf), a
  ld a, (CV1_HUD_Y)
  out ($bf), a
  ld a, $89
  out ($bf), a
  ld a, (CV1_HUD_LINE)
  out ($bf), a
  ld a, $8a
  out ($bf), a
  ld a, (CV1_HUD_EPOCH)
  ld (CV1_HUD_ARMED_EPOCH), a
  ld hl, CV1_HUD_FLAGS
  set 2, (hl)
  res 3, (hl)
  ld a, (CV1_HUD_R0)
  or $10
  out ($bf), a
  ld a, $80
  out ($bf), a
  ret

_cv1_hud_late:
.ifdef DIAG_CV1_HUD
  ld hl, CV1_HUD_LATE_COUNT
  inc (hl)
.endif
  ; A delayed frame cannot restart the counter in active display. Fail direct
  ; rather than leave the whole playfield at preX. Do not acknowledge VINT.
_cv1_hud_direct:
  ld hl, CV1_HUD_FLAGS
  res 2, (hl)
  ld a, (CV1_HUD_POST_X)
  out ($bf), a
  ld a, $88
  out ($bf), a
  ld a, (CV1_HUD_Y)
  out ($bf), a
  ld a, $89
  out ($bf), a
  jp _cv1_hud_park

; Actual line IRQ already acknowledged status at entry. The DI polling entry
; below has not: leaving HINT pending but disabled is safe until VBlank's
; deliberate acknowledgment, and avoids clearing a pending VINT here.
rt_cv1_hud_line:
  ld a, (CV1_HUD_FLAGS)
  bit 2, a
  ret z
.ifdef DIAG_CV1_HUD
  in a, ($7e)
  ld (CV1_HUD_ENTRY_VC), a
.endif
  ld a, (CV1_HUD_POST_X)
  out ($bf), a
  ld a, $88
  out ($bf), a
.ifdef DIAG_CV1_HUD
  in a, ($7e)
  ld (CV1_HUD_LAST_VC), a
.endif
  ld hl, CV1_HUD_FLAGS
  res 2, (hl)
  set 3, (hl)
  ld a, (CV1_HUD_EPOCH)
  ld (CV1_HUD_SERVED_EPOCH), a
_cv1_hud_park:
  ld a, (CV1_HUD_R0)
  out ($bf), a
  ld a, $80
  out ($bf), a
  ld a, $ff
  out ($bf), a
  ld a, $8a
  out ($bf), a
  ret

; One active-display sample; no EI, status reads, full NMI or mapping changes.
; The caller must preserve live registers (notably DE=PPUDATA address) when
; using this at a complete transaction boundary inside a guarded DI wait.
rt_cv1_hud_poll_line:
  in a, ($7e)
  cp 38
  ret c
  cp $e0
  ret nc
  jp rt_cv1_hud_line

; Pure scheduling predicate: B=conservative STOCK line cost including the
; caller/admission tail/step CALL+RET. Carry=defer, clear=admit. Exactly one
; counter sample, no EI/status/mapper writes. Coordinator owns register parking
; and IRQ-enabled waiting. Two extra lines protect IRQ service/rearm overhead.
; Never carry a served epoch across VBlank. An already rearmed split may admit
; a short chunk even on final blank261: finish plus reserve before next line38.
; Caller must service IRQs immediately before sampling. Previous-frame ARMED
; cannot survive that service point: its pending HINT is serviced, or VINT
; starts/rearms a new epoch. This relies on the bounded service/IE1 contract.
rt_cv1_hud_try_chunk:
  in a, ($7e)
  ld c, a
  ld a, b
  or a
  jr z, _cv1_hud_chunk_defer
  ld a, ($c802)
  bit 1, a                    ; explicit display-off fence: no visible HUD
  jr nz, _cv1_hud_chunk_admit
  ld a, (CV1_HUD_FLAGS)
  bit 0, a
  jr z, _cv1_hud_chunk_admit
  bit 1, a
  jr z, _cv1_hud_chunk_after
  ld a, c
  cp $e0
  jr nc, _cv1_hud_chunk_blank
  cp 38
  jr nc, _cv1_hud_chunk_served
  ld a, (CV1_HUD_FLAGS)
  bit 2, a
  jr z, _cv1_hud_chunk_defer
  ld a, c
  add a, b
  jr c, _cv1_hud_chunk_defer
  add a, 2
  jr c, _cv1_hud_chunk_defer
  cp 38
  jr c, _cv1_hud_chunk_admit
  jr _cv1_hud_chunk_defer
_cv1_hud_chunk_served:
  ld a, (CV1_HUD_FLAGS)
  bit 3, a
  jr z, _cv1_hud_chunk_defer
_cv1_hud_chunk_after:
  ld a, c
  cp $e0
  jr nc, _cv1_hud_chunk_defer
  add a, b
  jr c, _cv1_hud_chunk_defer
  add a, 2
  jr c, _cv1_hud_chunk_defer
  cp $e0
  jr c, _cv1_hud_chunk_admit
  jr _cv1_hud_chunk_defer
_cv1_hud_chunk_blank:
  ld a, (CV1_HUD_FLAGS)
  and $0c
  cp 4                       ; ARMED, never SERVICED from a previous frame
  jr nz, _cv1_hud_chunk_defer
  ld a, (CV1_HUD_EPOCH)
  ld hl, CV1_HUD_ARMED_EPOCH
  cp (hl)
  jr nz, _cv1_hud_chunk_defer
  ld a, b
  add a, 2
  jr c, _cv1_hud_chunk_defer
  cp 38                      ; conservative even with zero blank time left
  jr c, _cv1_hud_chunk_admit
_cv1_hud_chunk_defer:
  scf
  ret
_cv1_hud_chunk_admit:
  xor a
  ret

; Replacement for CV1's tail pacing status read. An acknowledged VINT owns
; physical HUD rearming even when no new producer/presentation runs. Otherwise
; poll AFTER acknowledgment so a just-due HINT cannot be swallowed. Preserve
; the original A/F and BC/DE for the existing VINT-overrun bookkeeping.
rt_cv1_hud_tail_ack:
  in a, ($bf)
  push af
  bit 7, a
  jr z, _cv1_hud_tail_line
  push bc
  call rt_cv1_hud_vblank
  pop bc
  pop af
  ret
_cv1_hud_tail_line:
  call rt_cv1_hud_poll_line
  pop af
  ret

; DI-only audio stage boundary: AF/BC/DE/HL are dead, the VDP latch is closed,
; and the frame entry's D474 classification is no longer live. Retain any
; acknowledged VINT for final IRQ pacing, then rearm its physical HUD epoch.
; Otherwise poll HINT AFTER the status read so a just-due split is not lost.
; Tail jumps avoid an extra native word: the deepest path is this caller's
; CALL plus hud_vblank's epoch CALL, equal to the existing sweep CALL+PUSH.
rt_cv1_hud_audio_poll:
  in a, ($bf)
  bit 7, a
  jr z, _cv1_hud_audio_line
  ld a, $80
  ld ($d474), a
  jp rt_cv1_hud_vblank
_cv1_hud_audio_line:
  jp rt_cv1_hud_poll_line
.ends
.endif
