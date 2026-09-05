; CV1's completed graphics prologue -> IRQ handoff. Opt-in only; the normal
; NROM/SMB instruction stream is unchanged. This freezes control ownership,
; NOT all graphics: direct PPU/CHR/CRAM writes still reach live VRAM.
;
; CV1 does not use vbuf_push (that API is unfinished). Reserve $C820-$C83D
; from its dormant buffer; leave $C800 (vbuf_flush length) zero. $CA40-$CAFF
; is NOT free: chrmap owns it as the BG variant reverse map.
;
; Three records: READY packet, live producer, temporary interrupted-live copy.
; Fields 0..8: CTRL,MASK,scrollX,Y,split flags,preX,Y,postX,Y.
; Fields 9..12: pending reg1,table refresh,band dirty,off-render write count.
; Publication transfers pending intent to the packet; consumption restores
; newer live intent and merges anything the old packet did not consume.
; CA13/CB2A and the variant cache are committed presentation state: never swap.
; A late prepared SAT retains only packet controls. New full producers wait,
; while already-running logic may resume; its first PPUDATA write is a barrier.
; C810 = pending commit; C811-C812 = DI-only PPUDATA address scratch.
.ifdef CV1_RUNTIME_HOOKS
.ifndef DEFER_SPRITE_REGISTERS
.fail "CV1 presentation requires translation.defer_sprite_registers"
.endif
.define CV1_FRAME_READY $c820
.define CV1_FRAME_PENDING $c810
.define CV1_FRAME_DATA_ADDR $c811
.define CV1_FRAME_PACKET $c821
.define CV1_FRAME_LIVE_SAVE $c82e
.define CV1_FRAME_HOOK_IFF $c83b
.define CV1_FRAME_PUBLISH_COUNT $c83c
.define CV1_FRAME_CONSUME_COUNT $c83d

.section "frame_cv1" free

; Existing profile replacement ABI: native CALL/RET, D/E = NES X/Y.
; Original C11F: read STATUS; scroll FD,FC; CTRL FF. The status value is dead;
; final A and shadow N/Z come from FF. All other shadow flags and D/E survive.
rt_cv1_scroll_publish:
  ld a, i
  di
  jp po, _cv1_hook_di
  ld a, 1
  jr _cv1_hook_iff
_cv1_hook_di:
  xor a
_cv1_hook_iff:
  ld (CV1_FRAME_HOOK_IFF), a
  ld b, 2
  call rt_ppu_read
  ld a, ($c0fd)
  ld b, 5
  call rt_ppu_write
  ld a, ($c0fc)
  ld b, 5
  call rt_ppu_write
  ld a, ($c0ff)
  ld b, 0
  call rt_ppu_write
  ; D/E must survive the record copier too.
  ld a, ($c01b)
  or a
  jr nz, _cv1_hook_flags
  push de
  ld de, CV1_FRAME_PACKET
  call _cv1_frame_capture
  ; Transfer all old dirty intent. A later producer write now belongs to
  ; the next packet even if this one has not been consumed yet.
  xor a
  ld ($cb2d), a
  ld ($cb7f), a
  ld ($cb78), a
  ld ($ca18), a
  ld a, (CV1_FRAME_PUBLISH_COUNT)
  inc a
  ld (CV1_FRAME_PUBLISH_COUNT), a
  ld a, 1
  ld (CV1_FRAME_READY), a      ; publish LAST while DI
  pop de
_cv1_hook_flags:
  ld a, ($c0ff)
  ld l, a
  ld h, $3e
  ld a, ($cb03)
  and $7d
  or (hl)
  ld ($cb03), a
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  ld a, ($c0ff)
  ret z
  ei
  ret

; Private DI-only copier. DE=record destination; clobbers AF/BC/DE/HL.
_cv1_frame_capture:
  ld hl, _cv1_frame_fields
  ld b, 13
_cv1_capture_byte:
  ld a, (hl)
  inc hl
  ld c, (hl)
  inc hl
  push hl
  ld l, a
  ld h, c
  ld a, (hl)
  ld (de), a
  inc de
  pop hl
  djnz _cv1_capture_byte
  ret

; Private DI-only copier. DE=record source; clobbers AF/BC/DE/HL.
_cv1_frame_install:
  ld hl, _cv1_frame_fields
  ld b, 13
_cv1_install_byte:
  ld a, (hl)
  inc hl
  ld c, (hl)
  inc hl
  push hl
  ld l, a
  ld h, c
  ld a, (de)
  ld (hl), a
  inc de
  pop hl
  djnz _cv1_install_byte
  ret

; IRQ has checked READY and owns DI. Keep the current producer record separate
; while unchanged presentation helpers consume the frozen packet.
rt_cv1_frame_begin:
  xor a
  ld (CV1_FRAME_READY), a
  ld a, (CV1_FRAME_CONSUME_COUNT)
  inc a
  ld (CV1_FRAME_CONSUME_COUNT), a
  ; Fall through: retries share the copier but never consume READY again.
rt_cv1_frame_resume:
  ld de, CV1_FRAME_LIVE_SAVE
  call _cv1_frame_capture
  ld de, CV1_FRAME_PACKET
  jp _cv1_frame_install

; Preparation finished but its final bounded commit was late. The prepared
; SAT owns old reg1; transfer all remaining dirty intent to live ONCE, not on
; every future retry (an unconsumed CA18 count would otherwise be added again).
; Retain only frozen controls; resume installs zero pending-intent fields.
rt_cv1_frame_suspend:
  ld de, CV1_FRAME_PACKET
  call _cv1_frame_capture
  ld hl, CV1_FRAME_PACKET + 9
  ld bc, 4
  xor a
  call mem_fill
  xor a
  ld ($cb2d), a
  call rt_cv1_frame_end
  ; Publish only after live control is restored; caller still owns DI.
  ld a, 1
  ld (CV1_FRAME_PENDING), a
  ret

; No translated producer ran during presentation (DI). Preserve newer pending
; writes saved at begin, and merge unconsumed old intent before restoring live
; control. reg1 is a latest-value latch; a newer live value wins over old.
rt_cv1_frame_end:
  ld a, (CV1_FRAME_LIVE_SAVE + 9)
  or a
  jr nz, _cv1_restore_reg1_ready
  ld a, ($cb2d)
  ld (CV1_FRAME_LIVE_SAVE + 9), a
_cv1_restore_reg1_ready:
  ld a, ($cb7f)
  ld hl, CV1_FRAME_LIVE_SAVE + 10
  or (hl)
  ld (hl), a
  ld a, ($cb78)
  inc hl
  or (hl)
  ld (hl), a
  ld a, ($ca18)
  inc hl
  add a, (hl)
  jr nc, _cv1_restore_count_ready
  ld a, $ff
_cv1_restore_count_ready:
  ld (hl), a
  ld de, CV1_FRAME_LIVE_SAVE
  jp _cv1_frame_install

_cv1_frame_fields:
  .dw $cb08, $cb09, $cb0c, $cb0d, $cb20, $cb21, $cb22, $cb23, $cb24
  .dw $cb2d, $cb7f, $cb78, $ca18
.ends
.endif
