.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
.include "runtime/chr_packet_target.inc"
.endif
.define M3P_OLD_LIVE $d7c0
.define M3P_NEEDED   $d7e0
.define M3P_DIRTY    $d980
.define M3P_CRAM     $d9b0
.define M3P_HUD_CRAM $d9d0
.define M3P_X        $d9f0
.define M3P_Y        $d9f1
.define M3P_HUD_X    $d9f2
.define M3P_SPLIT    $d9f3
.define M3P_REG1     $d9f4
.define M3P_REG0     $d9f5
.define M3P_STATE    $d9f6 ;0=old display valid,1=commit DI,2=blank fallback
.define M3P_PASS     $d9f7 ;2=old-live-protected fast build,1=reserve,0=build
.define M3P_CURSOR   $d9f8
.define M3P_REMAIN   $d9fa ;reserved/unused; counts live in registers/native stack
.define M3P_FLAGS    $d9fc ;bit0=copy pending PF palette, bit1=copy pending HUD
.define M3P_PHASE    $d9fd
.define M3P_BANK     $d9fe
.define M3P_BYTE     $d9ff
.define M3P_QUEUE    $da00

.section "present_mmc3" free

rt_mmc3_blank_display:
  ld a, 2
  ld (M3P_STATE), a
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  xor a
  ld (CNP_PAL_PHASE), a
  ld a, CNP_REG1_BLANK
.else
  ld a, $b0
.endif
  ld b, 1
  call vdp_set_register
  ld a, $06
  ld b, 0
  jp vdp_set_register

rt_mmc3_presentation_init:
  ld hl, $cc00
  ld bc, $0700
  xor a
  call _m3p_fill
  ld hl, $d600
  ld bc, $0600
  call _m3p_fill
  ld hl, $d800
  ld bc, $0180
  ld a, $ff
  call _m3p_fill
  ld hl, $ac00
  ld bc, $0400
  call _m3p_fill
  ld hl, 0
  ld (M3R_COUNT), hl
  ret
_m3p_fill:
rt_mmc3_fill_closed:
  ld e, a
  ld a, b
  or c
  jr z, _m3p_fill_done
_m3p_fill_chunk:
  ld a, b
  or a
  jr nz, _m3p_fill_full
  ld a, c
  cp 33
  jr c, _m3p_fill_tail
_m3p_fill_full:
  ; Counters/value live on the native stack: initialization deliberately
  ; overwrites publisher metadata, so none of that RAM is usable scratch.
  push de
  push bc
  ld (hl), e
  ld d, h
  ld e, l
  inc de
  ld bc, 31
  ldir                    ; propagate the seed, exactly32 destination bytes
  ex de, hl
  pop bc
  pop de
  ld a, c
  sub 32
  ld c, a
  jr nc, _m3p_fill_full_ready
  dec b
_m3p_fill_full_ready:
  ei
  nop
  di
  jr _m3p_fill_chunk
_m3p_fill_tail:
  push de
  ld (hl), e
  dec bc
  ld a, b
  or c
  jr z, _m3p_fill_one
  ld d, h
  ld e, l
  inc de
  ldir
  ex de, hl
  jr _m3p_fill_tail_ready
_m3p_fill_one:
  inc hl
_m3p_fill_tail_ready:
  pop de
  ei
  nop
  di
  ld a, b
  or c
_m3p_fill_done:
  ld a, e
  ret

rt_mmc3_prepare_display:
  ld a, (M3G_READY)
  or a
  jr nz, _m3p_prepare_old
  call rt_mmc3_presentation_init
  call rt_mmc3_blank_display
_m3p_prepare_old:
  ; A completed full build records every rendered slot in NEEDED. All
  ; remaining NT rows are zero-filled, so the exact ALL32-row live set is
  ; NEEDED union{slot0}. Stable packets retain NEEDED with their unchanged NT.
  ; Copy before clearing any current-build state; OLD_LIVE stays immutable.
  ld a, (M3G_READY)
  or a
  jr z, _m3p_prepare_needed
  ld hl, M3P_NEEDED
  ld de, M3P_OLD_LIVE
  ld bc, 32
  ldir
  ld a, (M3P_OLD_LIVE)
  or 1
  ld (M3P_OLD_LIVE), a
_m3p_prepare_needed:
  ld a, (M3G_COMPARE_BG)
  or a
  jr nz, _m3p_prepare_dirty
  ld hl, M3P_NEEDED
  ld bc, 32
  xor a
  call mem_fill
_m3p_prepare_dirty:
  ld hl, M3P_DIRTY
  ld bc, 48
  xor a
  call mem_fill
  ld a, (M3G_PRESENT_Y)
  ld (M3P_Y), a
  ei
  nop
  di
  ret

; A=slot0..255, HL=32byte bitmap. Return HL=byte,B=mask; DE preserved.
_m3p_bit:
  ld c, a
  ld b, >rt_mmc3_bit_masks
  ld a, (bc)
  ld b, a
  ld a, c
  rrca
  rrca
  rrca
  and 31
  add a, l
  ld l, a
  ret nc
  inc h
  ret

rt_mmc3_reserve_slot:
  push af
  ld hl, M3P_NEEDED
  call _m3p_bit
  ld a, (hl)
  or b
  ld (hl), a
  pop af
  ret

rt_mmc3_choose_slot:
  ld e, 0                  ; prefer an old-unreferenced slot
_m3p_choose_pass:
  ld d, 0
_m3p_choose:
  ld a, d
  ld hl, M3P_NEEDED
  call _m3p_bit
  ld a, (hl)
  and b
  jr nz, _m3p_choose_next
  ld a, e
  or a
  jr nz, _m3p_choose_found
  ld a, d
  ld hl, M3P_OLD_LIVE
  call _m3p_bit
  ld a, (hl)
  and b
  jr z, _m3p_choose_found
_m3p_choose_next:
  inc d
  ; A complete bitmap probe has no open VDP transaction. Large residency
  ; searches must not postpone the committed raster's host line/VINT service.
  ei
  nop
  di
  jr nz, _m3p_choose
  ld a, (M3P_PASS)
  cp 2
  scf
  ret z                    ; fast pass may not replace any old-live pattern
  inc e
  ld a, e
  cp 2
  jr c, _m3p_choose_pass
  ld a, $eb                ; all256 distinct needed keys remain protected
  jp rt_mmc3_graphics_trap
_m3p_choose_found:
  ld a, d
  or a                     ; clear carry: selected slot (including FF) is valid
  ret

; Remove the chosen initialized slot from its OLD bucket before changing key.
; Count is dense initialized-slot highwater, not current visible residency.
rt_mmc3_unlink_slot:
  ld hl, (M3R_COUNT)
  ld a, h
  or a
  jr nz, _m3p_unlink_old
  ld a, (M3R_SLOT)
  cp l
  jr c, _m3p_unlink_old
  ld l, a
  ld h, 0
  inc hl
  ld (M3R_COUNT), hl
  ret
_m3p_unlink_old:
  ld a, (M3R_SLOT)
  ld l, a
  ld h, $a8
  ld a, (hl)
  inc h
  xor (hl)
  inc h
  xor (hl)
  ld l, a
  ld h, 0
  add hl, hl
  ld bc, $ac00
  add hl, bc
_m3p_unlink_find:
  ld c, (hl)
  inc hl
  ld a, (hl)
  dec hl
  cp $ff
  jr nz, _m3p_unlink_candidate
  ld a, $eb
  jp rt_mmc3_graphics_trap
_m3p_unlink_candidate:
  ld a, (M3R_SLOT)
  cp c
  jr z, _m3p_unlink_found
  ld l, c
  ld h, 0
  add hl, hl
  ld bc, $ae00
  add hl, bc
  ei
  nop
  di
  jr _m3p_unlink_find
_m3p_unlink_found:
  push hl
  ld l, c
  ld h, 0
  add hl, hl
  ld bc, $ae00
  add hl, bc
  ld c, (hl)
  inc hl
  ld b, (hl)
  pop hl
  ld (hl), c
  inc hl
  ld (hl), b
  ret

; DE=original VRAM tile address. Mark dirty and return mapped SRAM destination.
rt_mmc3_pattern_stage:
  push de
  ld a, e
  rlca
  rlca
  rlca
  and 7
  ld hl, M3P_DIRTY
  push af
  ld a, d
  add a, l
  ld l, a
  pop af
  ld b, a
  ld a, 1
  jr z, _m3p_stage_mask
_m3p_stage_shift:
  add a, a
  djnz _m3p_stage_shift
_m3p_stage_mask:
  or (hl)
  ld (hl), a
  pop de
  ld a, d
  cp $20
  jr nc, _m3p_stage_sprite
  add a, $a0
  ld d, a
  ld a, 12
  ld ($fffc), a
  ret
_m3p_stage_sprite:
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
rt_chr_packet_sprite_stage_address:
.endif
  ld a, 8
  ld ($fffc), a
  ld hl, $7980
  ld a, d
  cp $26
  jr c, _m3p_stage_sprite_add
  jr nz, _m3p_stage_sprite_upper
  ld a, e
  cp $80
  jr c, _m3p_stage_sprite_add
_m3p_stage_sprite_upper:
  ld hl, $8f80
_m3p_stage_sprite_add:
  add hl, de
  ex de, hl
  ret

; HL=physicalCHRtile,B=conversion attributes,DE=fixed VRAM destination.
; Preserve all arguments; NZ iff payload identity changed. Hidden tiles retain
; their exact old key; mode changes resolve new physical half identities.
rt_mmc3_sprite_changed:
  push bc
  push de
  push hl
  push hl
  ld a, d
  and $0f
  ld h, a
  ld l, e
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  ld d, h
  ld e, l
  add hl, hl
  add hl, de
  ld de, $d800
  add hl, de
  pop de
  ld a, (hl)
  xor e
  ld c, a
  ld (hl), e
  inc hl
  ld a, (hl)
  xor d
  or c
  ld c, a
  ld (hl), d
  inc hl
  ld a, (hl)
  xor b
  or c
  ld (hl), b
  pop hl
  pop de
  pop bc
  ret

; Queue records: source16,destination16,length16,SRAMcontrol8,reserved8.
; Adjacent commands coalesce. A full queue is flushed without losing a byte;
; visible commands require the explicit display-off fallback in that case.
_m3p_queue_reset:
  ld hl, M3P_QUEUE
  ld (M3P_CURSOR), hl
  ret

_m3p_enqueue:
  ld (M3P_BANK), a
  push af
  push bc
  push de
  push hl
  ld hl, (M3P_CURSOR)
  ld a, h
  cp $dc
  jr nz, _m3p_enqueue_room
  ld a, (M3P_PHASE)
  or a
  jr z, _m3p_enqueue_flush
  ld a, (M3P_STATE)
  cp 2
  jr z, _m3p_enqueue_flush
  call _m3p_new_blank       ; overflow must not cut an old active frame in half
  call rt_mmc3_blank_display
_m3p_enqueue_flush:
  call _m3p_flush
  ld hl, (M3P_CURSOR)
_m3p_enqueue_room:
  ld a, h
  cp $da
  jr nz, _m3p_merge_try
  ld a, l
  or a
  jr z, _m3p_enqueue_new
_m3p_merge_try:
  push hl                  ; queue end, followed by src/dst/len/AF arguments
  ld de, -8
  add hl, de
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  inc hl
  inc hl
  ld c, (hl)
  inc hl
  ld b, (hl)
  inc hl
  ld a, (M3P_BANK)
  cp (hl)
  jr nz, _m3p_merge_miss
  ex de, hl
  add hl, bc
  ex de, hl                ; DE=previous source end
  ld hl, 2
  add hl, sp
  ld a, (hl)
  cp e
  jr nz, _m3p_merge_miss
  inc hl
  ld a, (hl)
  cp d
  jr nz, _m3p_merge_miss
  pop hl
  push hl
  ld de, -6
  add hl, de
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  add hl, bc
  ex de, hl                ; DE=previous VDP destination end
  ld hl, 4
  add hl, sp
  ld a, (hl)
  cp e
  jr nz, _m3p_merge_miss
  inc hl
  ld a, (hl)
  cp d
  jr nz, _m3p_merge_miss
  inc hl
  ld e, (hl)
  inc hl
  ld d, (hl)
  ld h, b
  ld l, c
  add hl, de
  ld b, h
  ld c, l
  pop hl
  dec hl
  dec hl
  dec hl
  ld (hl), b
  dec hl
  ld (hl), c
  pop hl
  pop de
  pop bc
  pop af
  ret
_m3p_merge_miss:
  pop hl
_m3p_enqueue_new:
  pop de
  ld (hl), e
  inc hl
  ld (hl), d
  inc hl
  pop de
  ld (hl), e
  inc hl
  ld (hl), d
  inc hl
  pop de
  ld (hl), e
  inc hl
  ld (hl), d
  inc hl
  pop af
  ld (hl), a
  inc hl
  ld (hl), 0
  inc hl
  ld (M3P_CURSOR), hl
  ret

; Input A=byte count1..128, HL=source. Unrolled OUTI has no active-display
; timing claim: caller admits every complete burst in VBlank or display-off.
; DE/BC scratch. The stack-entered suffix returns to this helper's caller.
_m3p_copy_burst:
  ld (M3P_BYTE), a
  ld b, a
  ld c, $be
  cp 128
  jp z, _m3p_out128
  push hl
  ld a, 128
  sub b
  add a, a
  ld l, a
  ld h, 0
  ld de, _m3p_out128
  add hl, de
  ex (sp), hl
  ret
_m3p_out128:
  .rept 128
  outi
  .endr
  ret

; Atomic-phase suffix: same byte semantics, but DE is the live remaining
; count and must survive address selection. No VDP control port is touched.
_m3p_stream_copy:
  ; M3P_BYTE belongs to the old writer; this path never consumes it.
  ld b, a
  ld c, $be
  cp 128
  jp z, _m3p_out128
  push hl
  ld a, 128
  sub b
  add a, a
  add a, <_m3p_out128
  ld l, a
  ld a, >_m3p_out128
  adc a, 0
  ld h, a
  ex (sp), hl
  ret

; Select a burst with stock-clock headroom: throughF0 at most128 bytes,
; F1..F6 at most64, F7..F9 at most16, FA one byte. FB..FF remains reserve.
; NTSC224 repeatsE5..EA, but either occurrence has MORE time than F0.
; Caller DI. Return A=admitted count, preserve source/destination/count.
_m3p_admit:
_m3p_admit_again:
  ld a, (M3P_STATE)
  cp 2
  jr z, _m3p_admit_large
_m3p_sample_beam:
  in a, ($7e)
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  cp $f0
.else
  cp $e0
.endif
  jr c, _m3p_admit_wait
  cp $fb
  jr nc, _m3p_admit_wait
  cp $f1
  jr c, _m3p_admit_large
  cp $f7
  jr c, _m3p_admit_medium
  cp $fa
  jr nc, _m3p_admit_one
_m3p_admit_small:
  ld a, b
  or a
  jr nz, _m3p_admit_small_max
  ld a, c
  cp 16
  ret c
_m3p_admit_small_max:
  ld a, 16
  ret
_m3p_admit_one:
  ld a, b
  or c
  ret z
  ld a, 1
  ret
_m3p_admit_medium:
  ld a, b
  or a
  jr nz, _m3p_admit_medium_max
  ld a, c
  cp 64
  ret c
_m3p_admit_medium_max:
  ld a, 64
  ret
_m3p_admit_large:
  ld a, b
  or a
  jr nz, _m3p_admit_large_max
  ld a, c
  cp 128
  ret c
_m3p_admit_large_max:
  ld a, 128
  ret
_m3p_admit_wait:
  ld a, (M3P_PHASE)
  or a
  jr z, _m3p_admit_early
_m3p_fallback_blank:
  push bc
  call rt_mmc3_blank_display
  pop bc
  jr _m3p_admit_large
_m3p_admit_early:
  ei
  nop
  di
  jr _m3p_admit_again

; Only the atomic stream earns the later128-byte boundary. The early/old
; writer retains its independently proven F0 limit and its original path.
; Sole caller enters STATE1/DI. The post-admission state check handles a
; real deadline blank and exits permanently to the old writer on fallback.
_m3p_stream_admit:
_m3p_stream_sample_beam:
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
rt_chr_packet_pal_admit:
  call rt_chr_packet_pal_sample
  jp c, _m3p_fallback_blank
  push af
  ld a, (CNP_PAL_PHASE)
  cp 3
  jr z, _m3p_pal_tail
  pop af
  jp _m3p_admit_large
_m3p_pal_tail:
  pop af
  cp $fb
  jp nc, _m3p_fallback_blank
.else
  in a, ($7e)
  cp $e0
  jr c, _m3p_fallback_blank
  cp $fb
  jr nc, _m3p_fallback_blank
.endif
  cp $f3
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  jp c, _m3p_admit_large
.else
  jr c, _m3p_admit_large
.endif
  cp $f7
  jr c, _m3p_admit_medium
  cp $fa
  jr nc, _m3p_admit_one
  jp _m3p_admit_small

; Transfer one queue; blank/early transfers admit host service at every burst.
_m3p_flush:
  ld hl, M3P_QUEUE
_m3p_command:
  ld de, (M3P_CURSOR)
  ld a, h
  cp d
  jr nz, _m3p_command_read
  ld a, l
  cp e
  jp z, _m3p_queue_reset
_m3p_command_read:
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  push de
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  ld c, (hl)
  inc hl
  ld b, (hl)
  inc hl
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  ; All producer spans are positive. A malformed zero suffix aliases128
  ; OUTI in the compact unrolled selector; reject it before any VDP write.
  ld a, b
  or c
  jp z, rt_cnrom_packet_unsupported
.endif
  ld a, (hl)
  ld ($fffc), a
  inc hl
  inc hl
  ex (sp), hl              ; HL=source, stack=next command
  ld a, (M3P_STATE)
  cp 1
  jr nz, _m3p_burst
  ; Only the DI atomic phase can retain the VDP address between suffixes.
  ; Pause never accesses the VDP; early/blank host service uses old writer.
  ld a, e
_m3p_stream_low:
  out ($bf), a
  ld a, d
  or $40
_m3p_stream_high:
  out ($bf), a
  ld d, b
  ld e, c
  jp _m3p_stream_burst
_m3p_burst:
  call _m3p_admit
  push bc
  push de
  push af
  ld a, e
  out ($bf), a
  ld a, d
  or $40                   ; C000 encoded CRAM retains its command bits
  out ($bf), a
  pop af
  call _m3p_copy_burst
  pop de
  pop bc
  ld a, (M3P_BYTE)
  add a, e
  ld e, a
  jr nc, _m3p_burst_dest
  inc d
_m3p_burst_dest:
  ld a, (M3P_BYTE)
  neg
  add a, c
  ld c, a
  jr c, _m3p_burst_remaining
  dec b
_m3p_burst_remaining:
  ld a, (M3P_STATE)
  cp 1
  jr z, _m3p_burst_no_host
  ei
  nop
  di
_m3p_burst_no_host:
  ld a, b
  or c
  jr nz, _m3p_burst
  pop hl
  jp _m3p_command

; HL=current source, DE=unconsumed bytes, native stack=next descriptor.
_m3p_stream_burst:
  ld b, d
  ld c, e
  call _m3p_stream_admit
  ld b, a
  ld a, (M3P_STATE)
  cp 1
  jr nz, _m3p_stream_fallback
  ld a, e
  sub b
  ld e, a
  jr nc, _m3p_stream_count
  dec d
_m3p_stream_count:
  ld a, b
  call _m3p_stream_copy
  ld a, d
  or e
  jr nz, _m3p_stream_burst
  pop hl
  jp _m3p_command
_m3p_stream_fallback:
  ; Admission has ALREADY disabled display. Its register writes destroyed
  ; the stream address, so recover it before handing off to the old writer.
  ; Preserve remaining/source and the descriptor-stack word exactly.
  push hl
  push de
  ld hl, 4
  add hl, sp
  ld c, (hl)
  inc hl
  ld b, (hl)
  ld hl, -8
  add hl, bc
  ld e, (hl)
  inc hl
  ld d, (hl)               ; DE=descriptor's initial source
  inc hl
  ld c, (hl)
  inc hl
  ld b, (hl)               ; BC=descriptor's initial encoded VDP address
  pop hl
  ex (sp), hl              ; HL=current source, stack=remaining,next descriptor
  push hl
  or a
  sbc hl, de
  add hl, bc
  ex de, hl                ; DE=recovered current VDP address
  pop hl
  pop bc
  jp _m3p_burst

; A=0old-unreferenced,1old-live for current originalVRAM address in DE.
_m3p_pattern_live:
  ld a, (M3G_READY)
  or a
  ret z
  ld a, d
  cp $20
  jr nc, _m3p_sprite_live
  ld a, d
  rlca
  rlca
  rlca
  ld b, a
  ld a, e
  rlca
  rlca
  rlca
  and 7
  or b
  ld hl, M3P_OLD_LIVE
  call _m3p_bit
  ld a, (hl)
  and b
  ret z
  ld a, 1
  ret
_m3p_sprite_live:
  ; Fixed per-OAM pair. HiddenE0 is the only early-upload exemption.
  ld a, d
  and $0f
  rlca
  rlca
  ld b, a
  ld a, e
  rlca
  rlca
  and 3
  or b
  ld l, a
  ld h, $d7
  ld a, (hl)
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  cp CNP_SAT_HIDDEN
.else
  cp $e0
.endif
  ld a, 0
  ret z
  ld a, (M3R_REG1)
  bit 1, a
  jr nz, _m3p_sprite_used
  bit 5, e                 ; second half unused in8x8 mode
  ld a, 0
  ret nz
_m3p_sprite_used:
  ld a, 1
  ret

_m3p_collect_patterns:
  ld de, 0
_m3p_collect_pattern:
  ; D identifies one DIRTY byte, E/32 its bit. A zero byte proves all
  ; eight payloads unchanged. Collection never creates a new dirty bit:
  ; pattern_stage only reasserts the selected bit; guest work is excluded.
  ld a, e
  or a
  jr nz, _m3p_collect_selected
  ld a, d
  add a, <M3P_DIRTY
  ld l, a
  ld h, >M3P_DIRTY
  ld a, (hl)
  or a
  jr nz, _m3p_collect_selected
  inc d
  ei
  nop
  di
  jp _m3p_collect_limit
_m3p_collect_selected:
  push de
  ld a, e
  rlca
  rlca
  rlca
  and 7
  ld hl, M3P_DIRTY
  call _m3p_bit
  ld a, d
  add a, l
  ld l, a
  ld a, (hl)
  and b
  jr z, _m3p_collect_next
  call _m3p_pattern_live
  ld b, a
  ld a, (M3P_PHASE)
  cp b
  jr nz, _m3p_collect_next
  push de
  call rt_mmc3_pattern_stage ; same dirty bit, now resolves source address
  ex de, hl
  pop de
  ld a, ($fffc)
  ld bc, 32
  call _m3p_enqueue
_m3p_collect_next:
  pop de
  ei
  nop
  di
  ld a, e
  add a, 32
  ld e, a
  jr nc, _m3p_collect_limit
  inc d
_m3p_collect_limit:
  ld a, d
  cp $30
  jr nz, _m3p_collect_pattern
  ld a, 8
  ld ($fffc), a
  ret

; Exact differing-byte runs, bridging up to24 equal bytes. A command costs
; more than24 OUTI bytes; source/VDP segment boundaries remain independent.
; M3R_Y is the
; source->VDP address delta. Native committed shadows remain unchanged here.
_m3p_diff:
  ld a, b
  or c
  ret z
_m3p_diff_seek:
  ld a, (de)
  cp (hl)
  jr nz, _m3p_diff_start
  call _m3p_diff_step
  jr nz, _m3p_diff_seek
  ret
_m3p_diff_start:
  ld (M3R_NT), hl
  xor a
  ld (M3R_COL), a
_m3p_diff_run:
  ld a, (de)
  cp (hl)
  jr z, _m3p_diff_same
  ld (M3R_KEY), hl         ; absolute last differing byte, not run length
  xor a
  ld (M3R_COL), a
  jr _m3p_diff_advance
_m3p_diff_same:
  ld a, (M3R_COL)
  inc a
  ld (M3R_COL), a
_m3p_diff_advance:
  call _m3p_diff_step
  jr z, _m3p_diff_emit
  ld a, (M3R_COL)
  cp 25
  jr c, _m3p_diff_run
_m3p_diff_emit:
  push bc                  ; BC remains the unexamined byte count
  push hl
  push de
  ld hl, (M3R_KEY)
  ld de, (M3R_NT)
  or a
  sbc hl, de
  inc hl
  ld b, h
  ld c, l
  ld hl, (M3R_NT)
  push hl
  ld de, (M3R_Y)
  add hl, de
  ex de, hl
  pop hl
  ld a, 8
  call _m3p_enqueue
  pop de
  pop hl
  pop bc
  ld a, b
  or c
  jr nz, _m3p_diff_seek
  ret
_m3p_diff_step:
  inc hl
  inc de
  dec bc
  ld a, l
  and 15                  ; closed host boundary at most16 compared bytes
  jr nz, _m3p_diff_no_host
  ei
  nop
  di
_m3p_diff_no_host:
  ld a, b
  or c
  ret

_m3p_new_blank:
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
rt_chr_packet_pal_acquire:
  ei
  nop
  di
  in a, ($7e)
  cp $0b
  jr c, rt_chr_packet_pal_acquire
  cp $d2
  jr nc, rt_chr_packet_pal_acquire
_m3p_pal_anchor:
  in a, ($7e)
  cp $0b
  jr c, rt_chr_packet_pal_acquire
  cp $f0
  jr c, _m3p_pal_anchor
  cp $f2
  jr nc, rt_chr_packet_pal_acquire
  ld (CNP_PAL_LAST), a
  ld a, 1
  ld (CNP_PAL_PHASE), a
  ret

; DI and a previously acquired epoch required. A=raw counter, carry=expired.
; Preserve BC/DE/HL/IX/IY and IFF. No data/control ports or source state.
rt_chr_packet_pal_sample:
  push bc
  in a, ($7e)
  ld b, a
  ld a, (CNP_PAL_LAST)
  ld c, a
  ld a, (CNP_PAL_PHASE)
  cp 1
  jr z, _m3p_pal_first
  cp 2
  jr z, _m3p_pal_middle
  cp 3
  jr nz, _m3p_pal_expired
  ld a, b
  cp c
  jr c, _m3p_pal_expired
  cp $d2
  jr c, _m3p_pal_expired
  jr _m3p_pal_valid
_m3p_pal_first:
  ld a, b
  cp c
  jr nc, _m3p_pal_valid
  cp $0b
  jr c, _m3p_pal_set_middle
  cp $d2
  jr c, _m3p_pal_expired
  cp $f0
  jr nc, _m3p_pal_expired
  jr _m3p_pal_set_tail
_m3p_pal_middle:
  ld a, b
  cp $d2
  jr nc, _m3p_pal_set_tail
  cp $0b
  jr nc, _m3p_pal_expired
  cp c
  jr c, _m3p_pal_expired
  jr _m3p_pal_valid
_m3p_pal_set_middle:
  ld a, 2
  jr _m3p_pal_set_phase
_m3p_pal_set_tail:
  ld a, 3
_m3p_pal_set_phase:
  ld (CNP_PAL_PHASE), a
_m3p_pal_valid:
  ld a, b
  ld (CNP_PAL_LAST), a
  pop bc
  or a
  ret
_m3p_pal_expired:
  xor a
  ld (CNP_PAL_PHASE), a
  ld a, b
  pop bc
  scf
  ret

; Carry clear means final register tail has its reserved blank interval.
rt_chr_packet_pal_final:
  call rt_chr_packet_pal_sample
  ret c
  push af
  ld a, (CNP_PAL_PHASE)
  cp 3
  jr z, _m3p_pal_final_tail
  pop af
  or a
  ret
_m3p_pal_final_tail:
  pop af
  cp $fb
  ccf
  ret
.else
  ei
  nop
  di
  in a, ($7e)
  cp $e0
  jr c, _m3p_new_blank
  cp $e2
  jr nc, _m3p_new_blank
  ret
.endif

rt_mmc3_commit_display:
  call _m3p_palette_changes ; exact compare OUTSIDE the atomic VBlank interval
  call _m3p_queue_reset
  xor a
  ld (M3P_PHASE), a
  call _m3p_collect_patterns
rt_mmc3_upload_early:
  call _m3p_flush
  ld a, 1
  ld (M3P_PHASE), a
  call _m3p_collect_patterns
  ld a, 8
  ld ($fffc), a
  ld a, (M3G_READY)
  or a
  jr z, _m3p_initial_tables
  ld a, (M3G_COMPARE_BG)
  or a
  jr nz, _m3p_sat_diff
  ld hl, $9700
  ld (M3R_Y), hl
  ld hl, $a000
  ld de, $cc00
  ld bc, $0700
  call _m3p_diff
  ld hl, $a700
  ld de, $d600
  ld bc, $0100
  call _m3p_diff
_m3p_sat_diff:
  ld hl, $9400
  ld (M3R_Y), hl
  ld hl, $ab00
  ld de, $d700
  ld bc, 64
  call _m3p_diff
  ld hl, $9440
  ld (M3R_Y), hl
  ld hl, $ab40
  ld de, $d740
  ld bc, 128
  call _m3p_diff
  jr _m3p_tables_ready
_m3p_initial_tables:
  ; Initial/re-enabled display has no valid committed shadow: initialization
  ; cleared native metadata, NOT the previous VRAM. Publish every NT/SAT byte
  ; while already blank, including zero cells and newly hidden sprite entries.
  ld hl, $a000
  ld de, $7700
  ld bc, $0800
  ld a, 8
  call _m3p_enqueue
  ld hl, $ab00
  ld de, $7f00
  ld bc, 64
  ld a, 8
  call _m3p_enqueue
  ld hl, $ab40
  ld de, $7f80
  ld bc, 128
  ld a, 8
  call _m3p_enqueue
_m3p_tables_ready:
  ld hl, M3P_CRAM
  ld de, $c000
  ld bc, 32
  ld a, 8
  call _m3p_enqueue
  call _m3p_new_blank
  ld a, (M3P_STATE)
  cp 2
  jr z, _m3p_commit_state
  ld a, 1
  ld (M3P_STATE), a
_m3p_commit_state:
  ; Once final publication starts, old display service is excluded. Copy
  ; its successor palettes now, not as unbounded work at the frame deadline.
  ld a, (M3P_FLAGS)
  bit 0, a
  jr z, _m3p_palette_pf_ready
  ld hl, M3P_CRAM
  ld de, M3R_CRAM
  ld bc, 32
  ldir
_m3p_palette_pf_ready:
  ld a, (M3P_FLAGS)
  bit 1, a
  jr z, _m3p_palette_hud_ready
  ld hl, M3P_HUD_CRAM
  ld de, M3R_HUD_CRAM
  ld bc, 32
  ldir
_m3p_palette_hud_ready:
  call _m3p_flush
  ld a, (M3P_STATE)
  cp 2
  jr z, _m3p_publish_wait
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  call rt_chr_packet_pal_final
  jr nc, rt_mmc3_publish_complete
.else
  in a, ($7e)
  cp $e0
  jr c, _m3p_publish_late
  cp $fb
  jr c, rt_mmc3_publish_complete
.endif
_m3p_publish_late:
  call rt_mmc3_blank_display
_m3p_publish_wait:
  call _m3p_new_blank
rt_mmc3_publish_complete:
  ld a, (M3P_X)
  ld (M3G_PRESENT_X), a
  ld b, 8
  call vdp_set_register
  ld a, (M3P_Y)
  ld (M3G_PRESENT_Y), a
  ld b, 9
  call vdp_set_register
  ld a, (M3P_HUD_X)
  ld (M3G_HUD_X), a
  ld a, (M3P_SPLIT)
  ld (M3R_SPLIT), a
  or a
  jr z, _m3p_publish_no_split
  dec a
  ld b, 10
  call vdp_set_register
  ld a, (M3P_REG0)
  or $10
  jr _m3p_publish_reg0
_m3p_publish_no_split:
  ld a, (M3P_REG0)
  and $ef
_m3p_publish_reg0:
  push af
  ld a, (M3P_REG0)
  ld (M3R_REG0), a
  pop af
  ld b, 0
  call vdp_set_register
  ld a, (M3P_REG1)
  ld (M3R_REG1), a
  ld b, 1
  call vdp_set_register
  ld a, 1
  ld (M3G_READY), a
  ; The transaction may have held this VBlank's host IRQ pending. Drain it
  ; while rearm is still excluded: all new pixels/registers are committed,
  ; and replaying their CRAM after a long shadow-copy prefix can hit line0.
  ; Host input/audio still runs; BUSY excludes guest production throughout.
  ei
  nop
  di
  xor a
  ld (M3P_STATE), a
  ; Shadow copies occur AFTER coherent publication, with the new complete
  ; display serviced throughout. Guest remains excluded until this returns.
  ld a, 8
  ld ($fffc), a
  ld a, (M3G_COMPARE_BG)
  or a
  jr nz, _m3p_shadow_sat
  ld hl, $a000
  ld de, $cc00
  ld bc, $0700
  call _m3p_shadow_copy
  ld hl, $a700
  ld de, $d600
  ld bc, $0100
  call _m3p_shadow_copy
_m3p_shadow_sat:
  ld hl, $ab00
  ld de, $d700
  ld bc, 192
rt_chr_packet_copy_closed:
_m3p_shadow_copy:
  ld a, b
  or c
  ret z
_m3p_shadow_chunk:
  ld a, b
  or a
  jr nz, _m3p_shadow_full
  ld a, c
  cp 33
  jr c, _m3p_shadow_tail
_m3p_shadow_full:
  push bc
  ld bc, 32
  ldir
  pop bc
  ld a, c
  sub 32
  ld c, a
  jr nc, _m3p_shadow_full_ready
  dec b
_m3p_shadow_full_ready:
  ei
  nop
  di
  jr _m3p_shadow_chunk
_m3p_shadow_tail:
  ldir
  ei
  nop
  di
  ld a, b
  or c
  ret

; Completed pending palettes stay immutable until publication. Host raster
; service only reads committed buffers and cannot invalidate this comparison.
; READY0 has no committed identity, so always copies both32-byte palettes.
_m3p_palette_changes:
  ld a, 3
  ld (M3P_FLAGS), a
  ld a, (M3G_READY)
  or a
  ret z
  ld hl, M3P_CRAM
  ld de, M3R_CRAM
  call _m3p_palette_same
  ld a, 0
  jr z, _m3p_palette_pf_same
  inc a
_m3p_palette_pf_same:
  ld (M3P_FLAGS), a
  ld hl, M3P_HUD_CRAM
  ld de, M3R_HUD_CRAM
  call _m3p_palette_same
  ret z
  ld a, (M3P_FLAGS)
  or 2
  ld (M3P_FLAGS), a
  ret
_m3p_palette_same:
  ld b, 32
_m3p_palette_same_byte:
  ld a, (de)
  cp (hl)
  ret nz
  inc hl
  inc de
  ld a, l
  and 15
  jr nz, _m3p_palette_same_next
  ei
  nop
  di
_m3p_palette_same_next:
  djnz _m3p_palette_same_byte
  xor a
  ret

.ends

; Permanent bank0 and a whole aligned page make BC a direct original-input
; index, preserving C, DE, the native stack and the existing address/flag tail.
; FREE placement must fail if no page fits; never overwrite another table.
.section "mmc3_bit_masks" BANK 0 SLOT 0 ALIGN 256 FREE RETURNORG
rt_mmc3_bit_masks:
  .rept 32
  .db 1,2,4,8,16,32,64,128
  .endr
rt_mmc3_bit_masks_end:
.ends
