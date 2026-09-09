; Source-time PPU authority, exclusively for the guarded hardware capability.
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.include "runtime/source_hardware_layout.inc"
.bank 0 slot 0
.section "source_ppu" free

; Deterministic cold bootstrap, not a universal NES power-on divider phase.
; CPU cycle0 = PPU line0/dot0; seven real reset transfers are included.
rt_source_hardware_init:
  call rt_source_init
  ld hl, 0
  ld (SC_LINE), hl
  ld hl, SP_BASE
  ld bc, SP_END-SP_BASE
  xor a
  call mem_fill
  ld hl, SH_BASE
  ld bc, SH_END-SH_BASE
  call mem_fill
  ld hl, $c000
  ld bc, $800
  call mem_fill
  ld hl, $c900
  ld bc, $100
  call mem_fill
  ld hl, CN_PALETTE
  ld bc, $20
  call mem_fill
  ld a, 8
  ld ($fffc), a
  ld hl, $8000
  ld bc, $800
  xor a
  call mem_fill
  ld ($fffc), a
  ld ($cb00), a
  ld ($cb01), a
  ld ($cb02), a
  ld hl, $cb08
  ld bc, $0a
  call mem_fill
  ld a, $24
  ld ($cb03), a
  ld a, 1
  ld (SP_STARTUP), a
  call rt_source_apu_init
  call rt_source_apu_cold_reset
  call rt_source_input_init
  call rt_source_input_frame
  ld hl, 0
  call rt_cnrom_packet_capture ; frame0 begins before the seven reset reads
  ld hl, 0
  call _sp_reset_read
  call _sp_reset_read
  ld b, 3
_sp_reset_stack:
  ld a, ($cb02)
  ld l, a
  ld h, 1
  call _sp_reset_read
  ld a, ($cb02)
  dec a
  ld ($cb02), a
  djnz _sp_reset_stack
  ld hl, $fffc
  call _sp_reset_read
  inc hl
  call _sp_reset_read
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush ; expose the declared complete seven-cycle epoch
.endif
  ret
_sp_reset_read:
  call rt_source_tick
  jp rt_source_bus_read_event

; Three PPU dots precede the original CPU transfer. Source clock already
; charged this CPU cycle. Hardware-domain state is never advanced by host IRQ.
rt_source_ppu_cycle:
  ld b, 3
_sp_cycle_dot:
  push bc
  call _sp_advance_dot
  pop bc
  djnz _sp_cycle_dot
  ret

; Isolated domain proof leaf, not yet selected by a profile or source clock.
; HL = first forbidden CPU-cycle span (0=ineligible). BC/DE/AF/IFF preserved.
; Only same-row forced-blank intervals: no PPU fetch or line/status callback
; may be summarized. Source CPU cycles and the other domains are caller-owned.
rt_source_ppu_blank_deadline:
  push af
  push bc
  push de
  ld a, ($cb09)
  and $18
  jr nz, _sp_blank_none
  ld a, (SP_ADDR_PENDING)
  or a
  jr nz, _sp_blank_none
  ld hl, (SC_LINE)
  ld de, 262
  or a
  sbc hl, de
  jr nc, _sp_blank_none
  add hl, de
  ld a, h
  or a
  jr nz, _sp_blank_position
  ld a, l
  cp 240
  jr nc, _sp_blank_position
  ld a, ($cb0f)
  and $3f
  cp $3f
  jr z, _sp_blank_none ; preserve the precise forced-blank palette guard
_sp_blank_position:
  ld hl, (SC_DOT)
  ld de, 341
  or a
  sbc hl, de
  jr nc, _sp_blank_none
  add hl, de
  ld a, h
  or l
  jr nz, _sp_blank_distance
  ld a, (SC_LINE+1)
  or a
  ld a, (SC_LINE)
  jr nz, _sp_blank_pre
  cp 241
  jr z, _sp_blank_one
  jr _sp_blank_distance
_sp_blank_pre:
  cp 5
  jr z, _sp_blank_one
_sp_blank_distance:
  ld hl, 341
  ld de, (SC_DOT)
  or a
  sbc hl, de
  ld de, 3
  ld b, 0
_sp_blank_divide:
  inc b
  or a
  sbc hl, de
  jr c, _sp_blank_divided
  jr nz, _sp_blank_divide
_sp_blank_divided:
  ld l, b
  ld h, 0
  jr _sp_blank_return
_sp_blank_one:
  ld hl, 1
  jr _sp_blank_return
_sp_blank_none:
  ld hl, 0
_sp_blank_return:
  pop de
  pop bc
  pop af
  ret

; BC completed CPU cycles; 0 is an exact no-op even if currently ineligible.
; Positive spans must end strictly before the deadline. The full precise
; effect in this domain is dot+=3*BC and the two per-dot transient clears.
; All registers/IFF, source cycles, v, pipeline and other domains preserved.
rt_source_ppu_blank_advance:
  push af
  push bc
  push de
  push hl
  ld a, b
  or c
  jr z, _sp_blank_advance_done
  call rt_source_ppu_blank_deadline
  ld d, b
  ld e, c
  or a
  sbc hl, de
  jr c, _sp_blank_error
  jr z, _sp_blank_error
  ld h, b
  ld l, c
  add hl, hl
  add hl, de
  ld de, (SC_DOT)
  add hl, de
  ld (SC_DOT), hl
  xor a
  ld (SP_ACTIVE), a
  ld (SP_ODD_SKIP), a
_sp_blank_advance_done:
  pop hl
  pop de
  pop bc
  pop af
  ret
_sp_blank_error:
  ld a, 1
  ld (SP_INTERVAL_ERROR), a
  jp rt_cnrom_unsupported

; Same-row inactive domain includes mask-on vblank rows240..260, which have
; neither rendering fetches nor the forced-blank palette override. Mask-off
; retains the qualified blank-domain rules. All registers except HL preserved.
rt_source_ppu_inactive_deadline:
  push af
  ld a, ($cb09)
  and $18
  jr nz, _sp_inactive_vblank
  pop af
  jp rt_source_ppu_blank_deadline
_sp_inactive_vblank:
  push bc
  push de
  ld a, (SP_ADDR_PENDING)
  or a
  jp nz, _sp_blank_none
  ld hl, (SC_LINE)
  ld de, 240
  or a
  sbc hl, de
  jp c, _sp_blank_none
  ld de, 21
  or a
  sbc hl, de
  jp nc, _sp_blank_none
  jp _sp_blank_position

rt_source_ppu_inactive_advance:
  push af
  push bc
  push de
  push hl
  ld a, b
  or c
  jp z, _sp_blank_advance_done
  call rt_source_ppu_inactive_deadline
  ld d, b
  ld e, c
  or a
  sbc hl, de
  jp c, _sp_blank_error
  jp z, _sp_blank_error
  ld h, b
  ld l, c
  add hl, hl
  add hl, de
  ld de, (SC_DOT)
  add hl, de
  ld (SC_DOT), hl
  xor a
  ld (SP_ACTIVE), a
  ld (SP_ODD_SKIP), a
  jp _sp_blank_advance_done

; Pure lookahead of the already-qualified evaluator, never a source tick.
; A0/HL=first future overflow dot(FFFF none), A1/HL0=ineligible geometry.
; BC/DE/IFF preserved. Live PPU104bytes and SC_DOT are restored under DI.
rt_source_ppu_predict_overflow:
  push bc
  push de
  ld a, (SP_INTERNAL_MODE)
  or a
  jp nz, _sp_predict_invalid
  ld hl, (SC_LINE)
  ld de, 240
  or a
  sbc hl, de
  jp nc, _sp_predict_invalid
  ld a, (SC_DOT+1)
  or a
  jp nz, _sp_predict_invalid
  ld a, (SP_STATUS)
  and $20
  jr nz, _sp_predict_already_set
  ld a, (SP_PRED_VALID)
  or a
  jr z, _sp_predict_scan
  ld hl, (SP_PRED_LINE)
  ld de, (SC_LINE)
  or a
  sbc hl, de
  jr nz, _sp_predict_scan
  ld hl, (SP_OVERFLOW_DOT)
  jr _sp_predict_success
_sp_predict_already_set:
  ld hl, $ffff
  jr _sp_predict_success
_sp_predict_scan:
  ld a, i
  di
  push af
  ld hl, (SC_DOT)
  ld (SP_PRED_SAVED_DOT), hl
  ld hl, SP_BASE
  ld de, SP_BACKUP
  ld bc, SP_LIVE_END-SP_BASE
  ldir
  ld a, 1
  ld (SP_INTERNAL_MODE), a
_sp_predict_loop:
  ld hl, (SC_DOT)
  inc hl
  ld a, h
  or a
  jr z, _sp_predict_evaluate
  ld a, l
  or a
  jr nz, _sp_predict_none
_sp_predict_evaluate:
  ld (SC_DOT), hl
  call _sp_sprite_evaluate
  ld a, (SP_STATUS)
  and $20
  jr z, _sp_predict_loop
  ld hl, (SC_DOT)
  jr _sp_predict_restore
_sp_predict_none:
  ld hl, $ffff
_sp_predict_restore:
  ld (SP_PRED_RESULT), hl
  ld hl, SP_BACKUP
  ld de, SP_BASE
  ld bc, SP_LIVE_END-SP_BASE
  ldir
  ld hl, (SP_PRED_SAVED_DOT)
  ld (SC_DOT), hl
  xor a
  ld (SP_INTERNAL_MODE), a
  ld hl, (SC_LINE)
  ld (SP_PRED_LINE), hl
  ld hl, (SP_PRED_RESULT)
  ld (SP_OVERFLOW_DOT), hl
  ld a, 1
  ld (SP_PRED_VALID), a
  pop af
  jp po, _sp_predict_success
  ei
_sp_predict_success:
  xor a
  pop de
  pop bc
  ret
_sp_predict_invalid:
  ld hl, 0
  ld a, 1
  pop de
  pop bc
  ret

; Typed domain query. A0/HL0=ineligible, A1=inactive, A2=visible BG,
; A3=sprite-fetch interval. HL is the first forbidden positive CPU span.
; BC/DE/IFF preserved; private cache/work fields may change, live PPU may not.
rt_source_ppu_interval_deadline:
  push bc
  push de
  ld a, (SP_INTERNAL_MODE)
  or a
  jp nz, _sp_interval_none
  call rt_source_ppu_inactive_deadline
  ld a, h
  or l
  jr z, _sp_interval_rendered
  ld a, 1
  jp _sp_interval_query_return
_sp_interval_rendered:
  ld a, ($cb09)
  and $18
  jp z, _sp_interval_none
  ld a, (SP_ADDR_PENDING)
  ld b, a
  ld a, ($cb0a)
  or b
  ld b, a
  ld a, (SP_HIT_PENDING)
  or b
  jp nz, _sp_interval_none
  ld hl, (SC_LINE)
  ld de, 240
  or a
  sbc hl, de
  jp nc, _sp_interval_none
  ld hl, (SC_DOT)
  ld a, h
  or a
  jp nz, _sp_interval_sprite_window
  ld a, l
  or a
  jp z, _sp_interval_none ; row origin/first pixels stay precise
  ld hl, 256
  ld (SP_INTERVAL_SCRATCH), hl
  ld a, (SP_STATUS)
  and $40
  jr nz, _sp_interval_overflow
  ld a, ($cb09)
  and $18
  cp $18
  jr nz, _sp_interval_overflow
  ld a, (SP_ZERO_ACTIVE)
  or a
  jr z, _sp_interval_overflow
  ld a, (SP_ZERO_X)
  cp $ff
  jr z, _sp_interval_overflow
  ld c, a
  inc a
  ld e, a                  ; first potentially colliding dot
  ld a, c
  add a, 8
  jr nc, _sp_interval_sprite_end
  ld a, $ff
_sp_interval_sprite_end:
  ld d, a
  ld a, ($cb09)
  and 6
  cp 6
  jr z, _sp_interval_sprite_clip_done
  ld a, e
  cp 9
  jr nc, _sp_interval_sprite_clip_done
  ld e, 9
_sp_interval_sprite_clip_done:
  ld a, d
  cp e
  jr c, _sp_interval_overflow ; whole window clipped
  ld a, (SC_DOT)
  cp e
  jr c, _sp_interval_before_sprite
  cp d
  jp c, _sp_interval_none
  jp z, _sp_interval_none
  jr _sp_interval_overflow
_sp_interval_before_sprite:
  ld l, e
  ld h, 0
  ld (SP_INTERVAL_SCRATCH), hl
_sp_interval_overflow:
  call rt_source_ppu_predict_overflow
  or a
  jp nz, _sp_interval_none
  ld a, h
  and l
  cp $ff
  jr z, _sp_interval_bg_limit
  ld de, (SP_INTERVAL_SCRATCH)
  or a
  sbc hl, de
  jr nc, _sp_interval_bg_limit
  add hl, de
  ld (SP_INTERVAL_SCRATCH), hl
_sp_interval_bg_limit:
  ld hl, (SP_INTERVAL_SCRATCH)
  ld de, (SC_DOT)
  or a
  sbc hl, de
  jp c, _sp_interval_none
  jp z, _sp_interval_none
  call _sp_interval_divide
  ld (SP_INTERVAL_LIMIT), hl
  ld a, 2
  jr _sp_interval_query_return
_sp_interval_sprite_window:
  cp 1
  jr nz, _sp_interval_none
  ld a, l
  cp 2
  jr c, _sp_interval_none
  cp 65
  jr nc, _sp_interval_none
  ld hl, 321
  ld de, (SC_DOT)
  or a
  sbc hl, de
  call _sp_interval_divide
  ld (SP_INTERVAL_LIMIT), hl
  ld a, 3
  jr _sp_interval_query_return
_sp_interval_none:
  ld hl, 0
  xor a
_sp_interval_query_return:
  pop de
  pop bc
  ret
_sp_interval_divide:
  ld de, 3
  ld b, 0
_sp_interval_divide_loop:
  inc b
  or a
  sbc hl, de
  jr c, _sp_interval_divided
  jr nz, _sp_interval_divide_loop
_sp_interval_divided:
  ld l, b
  ld h, 0
  ret

; Isolated materializer, not yet selected by the coordinator. The typed span
; owns omitted fetches; mode2 marks reconstruction callbacks, not source time.
rt_source_ppu_interval_advance:
  push af
  push bc
  push de
  push hl
  ld l, a
  ld a, b
  or c
  jp z, _sp_blank_advance_done
  ld a, l
  ld (SP_INTERVAL_KIND), a
  ld (SP_INTERVAL_COUNT), bc
  call rt_source_ppu_interval_deadline
  ld b, a
  ld a, (SP_INTERVAL_KIND)
  cp b
  jp nz, _sp_interval_error
  or a
  jp z, _sp_interval_error
  ld de, (SP_INTERVAL_COUNT)
  or a
  sbc hl, de
  jp c, _sp_interval_error
  jp z, _sp_interval_error
  ld hl, (SC_DOT)
  ld (SP_INTERVAL_START), hl
  ld (SP_INTERVAL_SAVED_DOT), hl
  add hl, de
  add hl, de
  add hl, de
  ld (SP_INTERVAL_END), hl
  call _sp_load_v
  ld (SP_INTERVAL_V), hl
  ld a, i
  di
  push af
  ld a, 2
  ld (SP_INTERNAL_MODE), a
  ld a, (SP_INTERVAL_KIND)
  cp 1
  jr z, _sp_interval_inactive_advance
  xor a
  ld (SP_ODD_SKIP), a
  inc a
  ld (SP_ACTIVE), a
  ld a, (SP_INTERVAL_KIND)
  cp 3
  jr z, _sp_interval_sprite_advance
  call _sp_interval_bg_advance
  jr _sp_interval_finish
_sp_interval_inactive_advance:
  ld bc, (SP_INTERVAL_COUNT)
  call rt_source_ppu_inactive_advance
  jr _sp_interval_finish
_sp_interval_sprite_advance:
  ld hl, (SC_DOT)
  inc hl
  ld (SC_DOT), hl
  call _sp_sprite_fetch
  ld hl, (SC_DOT)
  ld de, (SP_INTERVAL_END)
  or a
  sbc hl, de
  jr nz, _sp_interval_sprite_advance
_sp_interval_finish:
  xor a
  ld (SP_INTERNAL_MODE), a
  pop af
  jp po, _sp_blank_advance_done
  ei
  jp _sp_blank_advance_done
_sp_interval_error:
  ld a, 2
  ld (SP_INTERVAL_ERROR), a
  jp rt_cnrom_unsupported

; Preserve every evaluator phase. Only the BG prefix is summarized; a final
; aligned16..23-dot suffix removes every unknown initial shifter bit.
_sp_interval_bg_advance:
  ld hl, (SP_INTERVAL_END)
  ld de, 16
  or a
  sbc hl, de
  jp c, _sp_interval_bg_tail
  ld a, l
  and $f8
  ld l, a
  ld (SP_INTERVAL_SUFFIX), hl
  ld de, (SP_INTERVAL_START)
  or a
  sbc hl, de
  jp c, _sp_interval_bg_tail
  ld a, l
  cp 16
  jp c, _sp_interval_bg_tail
_sp_interval_eval_prefix:
  ld hl, (SC_DOT)
  inc hl
  ld (SC_DOT), hl
  call _sp_sprite_evaluate
  ld hl, (SC_DOT)
  ld de, (SP_INTERVAL_SUFFIX)
  or a
  sbc hl, de
  jr nz, _sp_interval_eval_prefix
  ld a, (SP_INTERVAL_SUFFIX)
  srl a
  srl a
  srl a
  ld b, a
  ld a, (SP_INTERVAL_START)
  srl a
  srl a
  srl a
  ld c, a
  ld a, b
  sub c
  ld b, a
_sp_interval_skip_tiles:
  call rt_source_ppu_increment_x
  djnz _sp_interval_skip_tiles
  call _sp_load_v
  push hl                   ; v after the skipped pattern-high completions
  ld a, l
  and $1f
  jr z, _sp_interval_seed_wrap
  dec hl
  jr _sp_interval_seed_tile
_sp_interval_seed_wrap:
  ld a, l
  or $1f
  ld l, a
  ld a, h
  xor 4
  ld h, a
_sp_interval_seed_tile:
  ld (SP_INTERVAL_SEED_V), hl
  call _sp_store_v
  call _sp_nt_address
  call rt_source_ppu_raw_read
  ld (SP_TILE), a
  call _sp_attr_address
  call rt_source_ppu_raw_read
  ld b, a
  call _sp_bg_attr_value
  call _sp_bg_pattern_address
  call rt_source_ppu_raw_read
  ld (SP_PATTERN_LOW), a
  ld de, 8
  add hl, de
  call rt_source_ppu_raw_read
  ld (SP_PATTERN_HIGH), a
  pop hl
  call _sp_store_v
  ld hl, 0
  ld (SP_BG_LOW), hl
  ld (SP_BG_HIGH), hl
  ld (SP_ATTR_LOW), hl
  ld (SP_ATTR_HIGH), hl
_sp_interval_bg_tail:
  ld hl, (SC_DOT)
  inc hl
  ld (SC_DOT), hl
  call _sp_render_dot
  ld hl, (SC_DOT)
  ld de, (SP_INTERVAL_END)
  or a
  sbc hl, de
  jr nz, _sp_interval_bg_tail
  ret

_sp_advance_dot:
  xor a
  ld (SP_ACTIVE), a
  ld (SP_ODD_SKIP), a
  ld a, (SP_ADDR_PENDING)
  or a
  jr z, _sp_no_address_load
  xor a
  ld (SP_ADDR_PENDING), a
  ld hl, (SP_ADDR_VALUE)
  call _sp_store_v
_sp_no_address_load:
  ld hl, (SC_DOT)
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ; Odd NTSC rendering frames omit pre-render dot340, not a CPU cycle.
  ld a, h
  cp 1
  jr nz, _sp_regular_dot
  ld a, l
  cp 83                   ; dot339
  jr nz, _sp_regular_dot
  ld de, (SC_LINE)
  ld a, d
  cp 1
  jr nz, _sp_regular_dot
  ld a, e
  cp 5
  jr nz, _sp_regular_dot
  ld a, (SP_ODD)
  or a
  jr z, _sp_regular_dot
  ld a, ($cb09)
  and $18
  jr z, _sp_regular_dot
  inc hl
  ld a, 1
  ld (SP_ODD_SKIP), a
_sp_regular_dot:
.endif
  inc hl
  ld de, 341
  or a
  sbc hl, de
  jr c, _sp_same_line
  ld (SC_DOT), hl
  ld hl, (SC_LINE)
  inc hl
  ld de, 262
  or a
  sbc hl, de
  jr c, _sp_next_line
  ld (SC_LINE), hl
  ld hl, SC_FRAME
  call rt_source_inc32
  ld a, (SP_ODD_SKIP)
  or a
  call nz, rt_source_ppu_fetch_complete ; odd frame's last dummy read at0:0
  ld a, (SP_ODD)
  xor 1
  ld (SP_ODD), a
  xor a
  ld (SP_ZERO_ACTIVE), a  ; sprite evaluation does not run on pre-render
  ld (SP_HIT_PENDING), a
  ld a, (SP_PREFETCH_DIRTY)
  or a
  jp nz, rt_cnrom_unsupported
  call rt_source_input_frame
  ld hl, (SP_ORIGIN)
  jp rt_cnrom_packet_capture
_sp_next_line:
  add hl, de
  ld (SC_LINE), hl
  ld a, h
  or a
  ret nz
  ld a, l
  cp 240
  jp z, rt_cnrom_packet_validate_complete
  ret nc
  ld a, (SP_ZERO_NEXT)
  ld (SP_ZERO_ACTIVE), a
  ld a, (SP_ZERO_NEXT_LOW)
  ld (SP_ZERO_LOW), a
  ld a, (SP_ZERO_NEXT_HIGH)
  ld (SP_ZERO_HIGH), a
  ld a, (SP_ZERO_NEXT_X)
  ld (SP_ZERO_X), a
  ld a, (SP_ZERO_NEXT_ATTR)
  ld (SP_ZERO_ATTR), a
  ret
_sp_same_line:
  add hl, de
  ld (SC_DOT), hl
  ld a, h
  or a
  jp nz, _sp_render_dot
  ld a, l
  cp 1
  jp nz, _sp_render_dot
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sp_prerender_clear
  ld a, l
  cp 241
  jp nz, _sp_render_dot
  xor a
  ld (SP_VBL_EDGE_NEW), a
  ld a, (SP_VBL_SUPPRESS)
  or a
  jp nz, _sp_render_dot
  ld a, (SC_NMI_EDGE)
  or a
  jr nz, _sp_vblank_set
  ld a, (SC_NMI_LINE)
  or a
  jr nz, _sp_vblank_set
  ld a, ($cb08)
  and $80
  ld (SP_VBL_EDGE_NEW), a  ; never claim ownership of an older pending edge
_sp_vblank_set:
  ld a, 1
  ld (SC_VBLANK), a
  call rt_source_nmi_line
  jp _sp_render_dot
_sp_prerender_clear:
  ld a, l
  cp 5
  jp nz, _sp_render_dot
  xor a
  ld (SC_VBLANK), a
  ld (SP_STATUS), a
  ld (SP_STARTUP), a
  ld (SP_VBL_SUPPRESS), a
  ld (SP_VBL_EDGE_NEW), a
  call rt_source_nmi_line
  jp _sp_render_dot

; Selected NTSC transfer phase: only line241 dots0..2 are special. A dot0
; read prevents the upcoming flag/set edge; dot1/2 retain the returned set bit
; but withdraw this newly generated, not-yet-polled edge. Prior accepted or
; pending NMI ownership is never cleared. The common reader clears VBlank/w.
rt_source_ppu_status_window:
  ld hl, (SC_LINE)
  ld a, h
  or a
  ret nz
  ld a, l
  cp 241
  ret nz
  ld hl, (SC_DOT)
  ld a, h
  or a
  ret nz
  ld a, l
  or a
  jr nz, _sp_status_after_set
  ld a, 1
  ld (SP_VBL_SUPPRESS), a
  ret
_sp_status_after_set:
  cp 3
  ret nc
  ld a, (SP_VBL_EDGE_NEW)
  or a
  ret z
  xor a
  ld (SC_NMI_EDGE), a
  ld (SP_VBL_EDGE_NEW), a
  ret

; v is the sole shared 15-bit loopy register (big endian legacy ABI).
_sp_load_v:
  ld a, ($cb0f)
  ld h, a
  ld a, ($cb10)
  ld l, a
  ret
_sp_store_v:
  ld a, h
  and $7f
  ld ($cb0f), a
  ld a, l
  ld ($cb10), a
  ret
rt_source_ppu_increment_x:
  call _sp_load_v
  ld a, l
  and $1f
  cp $1f
  jr z, _sp_x_wrap
  inc hl
  jp _sp_store_v
_sp_x_wrap:
  ld a, l
  and $e0
  ld l, a
  ld a, h
  xor 4
  ld h, a
  jp _sp_store_v
rt_source_ppu_increment_y:
  call _sp_load_v
  ld a, h
  and $70
  cp $70
  jr z, _sp_y_coarse
  ld de, $1000
  add hl, de
  jp _sp_store_v
_sp_y_coarse:
  ld a, h
  and $0f
  ld h, a
  and 3
  ld b, a
  ld a, l
  and $e0
  ld c, a
  ld a, b
  cp 3
  jr nz, _sp_y_add
  ld a, c
  cp $a0                  ; coarse29 wraps and flips vertical nametable
  jr z, _sp_y_toggle
  cp $e0                  ; coarse31 wraps without flipping
  jr z, _sp_y_wrap
_sp_y_add:
  ld de, $20
  add hl, de
  jp _sp_store_v
_sp_y_toggle:
  ld a, h
  xor 8
  ld h, a
_sp_y_wrap:
  ld a, h
  and $fc
  ld h, a
  ld a, l
  and $1f
  ld l, a
  jp _sp_store_v
_sp_copy_x:
  call _sp_load_v
  ld a, h
  and $7b
  ld h, a
  ld a, (CN_T_HI)
  and 4
  or h
  ld h, a
  ld a, l
  and $e0
  ld l, a
  ld a, (CN_T_LO)
  and $1f
  or l
  ld l, a
  jp _sp_store_v
_sp_copy_y:
  call _sp_load_v
  ld a, h
  and 4
  ld h, a
  ld a, (CN_T_HI)
  and $7b
  or h
  ld h, a
  ld a, l
  and $1f
  ld l, a
  ld a, (CN_T_LO)
  and $e0
  or l
  ld l, a
  jp _sp_store_v

; Literal source bus taps: address setup on odd dot, data read on even dot.
; CHR physical identity is resolved at the read, never at SMS publication.
rt_source_ppu_fetch_address:
  ld (SP_FETCH_KIND), a
  ld (SP_FETCH_ADDR), hl
rt_source_ppu_fetch_address_ready:
  ret
rt_source_ppu_fetch_complete:
  ld hl, (SP_FETCH_ADDR)
  ld a, (CN_CHR)
  ld (SP_FETCH_BANK), a
  call rt_source_ppu_raw_read
  ld (SP_FETCH_VALUE), a
rt_source_ppu_fetch_read:
  ret
_sp_nt_address:
  call _sp_load_v
  ld a, h
  and $0f
  or $20
  ld h, a
  ret
_sp_attr_address:
  call _sp_load_v
  ld a, l
  rrca
  rrca
  and 7
  ld c, a
  ld a, l
  and $80
  rlca
  rlca
  rlca
  rlca                    ; v bit7 -> attribute bit3
  or c
  ld c, a
  ld a, h
  and 3
  rlca
  rlca
  rlca
  rlca                    ; v bits8..9 -> attribute bits4..5
  or c
  or $c0
  ld l, a
  ld a, h
  and $0c
  or $23
  ld h, a
  ret
_sp_bg_pattern_address:
  ld a, (SP_TILE)
  ld l, a
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld a, ($cb08)
  and $10
  or h
  ld h, a
  ld a, ($cb0f)
  rrca
  rrca
  rrca
  rrca
  and 7
  or l
  ld l, a
  ret

_sp_render_dot:
  xor a
  ld (SP_ACTIVE), a
  ld a, ($cb09)
  and $18
  jp z, _sp_forced_blank
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sp_render_pre
  ld a, l
  cp 240
  ret nc
  jr _sp_render_active
_sp_render_pre:
  ld a, l
  cp 5
  ret nz
_sp_render_active:
  ld a, 1
  ld (SP_ACTIVE), a
  call _sp_sprite_evaluate
  call _sp_shift_dot
  call _sp_pixel
  ld hl, (SC_DOT)
  ld a, h
  or a
  jr z, _sp_bg_visible
  ld a, l
  or a
  jr z, _sp_bg_dot256
  cp 1
  jr z, _sp_bg_dot257
  cp 24                   ; pre-render vertical reload dots280..304
  jp c, _sp_sprite_fetch
  cp 49
  jr nc, _sp_after_y_reload
  ld a, (SC_LINE+1)
  or a
  call nz, _sp_copy_y
_sp_after_y_reload:
  ld a, (SC_DOT)
  cp 65
  jp c, _sp_sprite_fetch
  cp 81
  jr c, _sp_bg_prefetch
  cp 82
  jr z, _sp_bg_dummy_complete
  cp 84
  jr z, _sp_bg_dummy_complete
  cp 81
  jr z, _sp_bg_last_reload
  cp 83
  jr z, _sp_bg_dummy_address
  ret
_sp_bg_dot257:
  call _sp_reload_bg
  call _sp_load_v
  ld c, l                 ; first garbage read retains pre-reload low pins
  push bc
  call _sp_copy_x
  call _sp_nt_address
  pop bc
  ld l, c
  ld a, 7
  jp rt_source_ppu_fetch_address
_sp_bg_dot256:
  call _sp_bg_cycle
  jp rt_source_ppu_increment_y
_sp_bg_visible:
  ld a, l
  or a
  ret z
  jp _sp_bg_cycle
_sp_bg_prefetch:
  cp 65
  jr nz, _sp_bg_cycle
  ld a, (SC_LINE+1)
  or a
  jr z, _sp_bg_cycle
  call _sp_load_v
  ld (SP_ORIGIN), hl       ; before the two pre-render tile fetches
  xor a
  ld (SP_PREFETCH_DIRTY), a
  ld a, (CN_CHR)
  ld (SP_PREFETCH_BANK), a
  jr _sp_bg_cycle
_sp_bg_last_reload:
  call _sp_reload_bg
_sp_bg_dummy_address:
  call _sp_nt_address
  ld a, 7
  jp rt_source_ppu_fetch_address
_sp_bg_dummy_complete:
  jp rt_source_ppu_fetch_complete

_sp_bg_cycle:
  ld a, (SC_DOT)
  and 7
  cp 1
  jr z, _sp_bg_nt
  cp 3
  jr z, _sp_bg_attr
  cp 5
  jr z, _sp_bg_low_address
  cp 7
  jr z, _sp_bg_high_address
  call rt_source_ppu_fetch_complete
  ld b, a
  ld a, (SP_FETCH_DEST)
  cp 1
  jr z, _sp_bg_tile_value
  cp 2
  jr z, _sp_bg_attr_value
  cp 3
  jr z, _sp_bg_low_value
  ld a, b
  ld (SP_PATTERN_HIGH), a
  jp rt_source_ppu_increment_x
_sp_bg_tile_value:
  ld a, b
  ld (SP_TILE), a
  ret
_sp_bg_attr_value:
  call _sp_load_v
  ld a, l
  and $40
  jr z, _sp_attr_top
  srl b
  srl b
  srl b
  srl b
_sp_attr_top:
  bit 1, l
  jr z, _sp_attr_left
  srl b
  srl b
_sp_attr_left:
  ld a, b
  and 3
  ld (SP_ATTRIBUTE), a
  ret
_sp_bg_low_value:
  ld a, b
  ld (SP_PATTERN_LOW), a
  ret
_sp_bg_nt:
  call _sp_reload_bg
  call _sp_nt_address
  ld a, 1
  jr _sp_bg_setup
_sp_bg_attr:
  call _sp_attr_address
  ld a, 2
  jr _sp_bg_setup
_sp_bg_low_address:
  call _sp_bg_pattern_address
  ld a, 3
  jr _sp_bg_setup
_sp_bg_high_address:
  call _sp_bg_pattern_address
  set 3, l
  ld a, 4
_sp_bg_setup:
  ld (SP_FETCH_DEST), a
  jp rt_source_ppu_fetch_address
_sp_shift_dot:
  ld hl, (SC_DOT)
  ld a, h
  or a
  jr nz, _sp_shift_late
  ld a, l
  cp 2
  ret c
  jr _sp_shift_bg
_sp_shift_late:
  ld a, l
  cp 2
  jr c, _sp_shift_bg       ; dots256/257
  cp 66
  ret c
  cp 82
  ret nc                  ; dots322..337
_sp_shift_bg:
  ld hl, (SP_BG_LOW)
  add hl, hl
  ld (SP_BG_LOW), hl
  ld hl, (SP_BG_HIGH)
  add hl, hl
  inc l                   ; logical high-plane serial input is one
  ld (SP_BG_HIGH), hl
  ld hl, (SP_ATTR_LOW)
  add hl, hl
  ld (SP_ATTR_LOW), hl
  ld hl, (SP_ATTR_HIGH)
  add hl, hl
  ld (SP_ATTR_HIGH), hl
  ret
_sp_reload_bg:
  ld a, (SP_PATTERN_LOW)
  ld (SP_BG_LOW), a
  ld a, (SP_PATTERN_HIGH)
  ld (SP_BG_HIGH), a
  ld a, (SP_ATTRIBUTE)
  rrca
  sbc a, a
  ld (SP_ATTR_LOW), a
  ld a, (SP_ATTRIBUTE)
  rrca
  rrca
  sbc a, a
  ld (SP_ATTR_HIGH), a
  ret

; Secondary OAM evaluation follows alternating primary read/secondary write
; dots, including the diagonal overflow scan. CPU OAM access during rendering
; remains separately guarded until its corruption/read-latch races are tested.
_sp_sprite_evaluate:
  ld a, (SC_LINE+1)
  or a
  ret nz                  ; pre-render fetches retain line239 secondary OAM
  ld hl, (SC_DOT)
  ld a, h
  or a
  jr z, _sp_eval_low
  ld a, l
  or a
  ret nz                  ; evaluation ends after dot256
  jr _sp_eval_even
_sp_eval_low:
  ld a, l
  cp 1
  jr nz, _sp_eval_not_start
  xor a
  ld (SP_EVAL_N), a
  ld (SP_EVAL_M), a
  ld (SP_EVAL_COUNT), a
  ld (SP_EVAL_DONE), a
  ld (SP_EVAL_COPY), a
  ld (SP_ZERO_NEXT), a
  ld (SP_ZERO_NEXT_LOW), a
  ld (SP_ZERO_NEXT_HIGH), a
_sp_eval_not_start:
  ld a, l
  cp 65
  jr nc, _sp_eval_scan
  bit 0, l
  ret nz
  srl l
  dec l
  ld h, 0
  ld de, SP_SECONDARY
  add hl, de
  ld (hl), $ff
  ret
_sp_eval_scan:
  ld a, (SC_LINE+1)
  or a
  ret nz                  ; no sprite evaluation on pre-render
  ld a, (SC_DOT)
  and 1
  jr z, _sp_eval_even
  ld a, (SP_EVAL_N)
  add a, a
  add a, a
  ld b, a
  ld a, (SP_EVAL_M)
  or b
  ld l, a
  ld h, $c9
  ld a, (hl)
  ld (SP_OAM_LATCH), a
  ret
_sp_eval_even:
  ld a, (SC_LINE+1)
  or a
  ret nz
  ld a, (SP_EVAL_DONE)
  or a
  jp nz, _sp_eval_next_n
  ld a, (SP_EVAL_COUNT)
  cp 8
  jr nc, _sp_eval_overflow
  add a, a
  add a, a
  ld b, a
  ld a, (SP_EVAL_M)
  or b
  ld l, a
  ld h, 0
  ld de, SP_SECONDARY
  add hl, de
  ld a, (SP_OAM_LATCH)
  ld (hl), a
  ld a, (SP_EVAL_M)
  or a
  jr nz, _sp_eval_copy
  call _sp_eval_in_range
  jp nc, _sp_eval_next_n
  ld a, (SP_EVAL_COUNT)
  ld l, a
  ld h, 0
  ld de, SP_SECONDARY_INDEX
  add hl, de
  ld a, (SP_EVAL_N)
  ld (hl), a
  or a
  jr nz, _sp_eval_copy
  ld a, 1
  ld (SP_ZERO_NEXT), a
_sp_eval_copy:
  ld a, (SP_EVAL_M)
  inc a
  and 3
  ld (SP_EVAL_M), a
  ret nz
  ld a, (SP_EVAL_COUNT)
  inc a
  ld (SP_EVAL_COUNT), a
  jp _sp_eval_next_n
_sp_eval_overflow:
  ld a, (SP_EVAL_COPY)
  or a
  jr nz, _sp_eval_overflow_copy
  call _sp_eval_in_range
  jr nc, _sp_eval_diagonal
  ld a, (SP_STATUS)
  or $20
  ld (SP_STATUS), a
  ld a, 4
_sp_eval_overflow_copy:
  dec a
  ld (SP_EVAL_COPY), a
  jr nz, _sp_eval_linear
  ld a, 1
  ld (SP_EVAL_DONE), a
_sp_eval_linear:
  ld a, (SP_EVAL_M)
  inc a
  and 3
  ld (SP_EVAL_M), a
  jr z, _sp_eval_next_n
  ld a, (SP_EVAL_DONE)
  or a
  ret z
  xor a                   ; completed overflow probe resumes dummy Y reads
  ld (SP_EVAL_M), a
  ret
_sp_eval_diagonal:
  ld a, (SP_EVAL_M)
  inc a
  and 3
  ld (SP_EVAL_M), a
_sp_eval_next_n:
  ld a, (SP_EVAL_N)
  inc a
  and $3f
  ld (SP_EVAL_N), a
  ret nz
  ld a, 1
  ld (SP_EVAL_DONE), a
  xor a
  ld (SP_EVAL_M), a
  ret
_sp_eval_in_range:
  ld a, ($cb08)
  and $20
  ld b, 8
  jr z, _sp_eval_height
  ld b, 16
_sp_eval_height:
  ld a, (SP_OAM_LATCH)
  ld c, a
  ld a, (SC_LINE)
  sub c
  jr nc, _sp_eval_nonnegative
  or a                    ; hidden Y>=240 does not wrap onto the top
  ret
_sp_eval_nonnegative:
  cp b
  ret

_sp_sprite_fetch:
  ld a, (SC_DOT)
  dec a
  ld b, a
  srl a
  srl a
  srl a
  ld (SP_FETCH_SLOT), a
  ld a, b
  and 7
  cp 0
  jr z, _sp_sprite_dummy_address
  cp 2
  jr z, _sp_sprite_dummy_address
  cp 4
  jr z, _sp_sprite_low_address
  cp 6
  jr z, _sp_sprite_high_address
  call rt_source_ppu_fetch_complete
  ld b, a
  ld a, (SP_FETCH_SLOT)
  or a
  ret nz                  ; sprite0, if selected, is always first priority
  ld a, (SP_ZERO_NEXT)
  or a
  ret z
  ld a, (SP_FETCH_KIND)
  cp 5
  ld a, b
  jr z, _sp_sprite_low_value
  ld a, (SP_FETCH_KIND)
  cp 6
  ret nz
  ld a, b
  ld (SP_ZERO_NEXT_HIGH), a
  ret
_sp_sprite_low_value:
  ld (SP_ZERO_NEXT_LOW), a
  ret
_sp_sprite_dummy_address:
  call _sp_nt_address
  ld a, 7
  jp rt_source_ppu_fetch_address
_sp_sprite_low_address:
  call _sp_sprite_pattern_address
  ld a, 5
  jp rt_source_ppu_fetch_address
_sp_sprite_high_address:
  call _sp_sprite_pattern_address
  set 3, l
  ld a, 6
  jp rt_source_ppu_fetch_address
_sp_sprite_pattern_address:
  ld a, (SP_FETCH_SLOT)
  add a, a
  add a, a
  ld l, a
  ld h, 0
  ld de, SP_SECONDARY
  add hl, de
  ld a, (SC_LINE)
  sub (hl)
  ld c, a
  inc hl
  ld b, (hl)              ; tile
  inc hl
  ld d, (hl)              ; attributes
  inc hl
  ld e, (hl)              ; X
  ld a, (SP_FETCH_SLOT)
  or a
  jr nz, _sp_sprite_not_zero_slot
  ld a, d
  ld (SP_ZERO_NEXT_ATTR), a
  ld a, e
  ld (SP_ZERO_NEXT_X), a
_sp_sprite_not_zero_slot:
  ld a, ($cb08)
  and $20
  jr nz, _sp_sprite_16
  ld a, c
  and 7
  bit 7, d
  jr z, _sp_sprite_8_row
  xor 7
_sp_sprite_8_row:
  ld c, a
  ld a, ($cb08)
  and 8
  add a, a
  ld d, a                 ; pattern-table bit12
  jr _sp_sprite_tile
_sp_sprite_16:
  ld a, c
  and 15
  bit 7, d
  jr z, _sp_sprite_16_row
  xor 15
_sp_sprite_16_row:
  ld c, a
  ld a, b
  and 1
  rlca
  rlca
  rlca
  rlca
  ld d, a
  res 0, b
  bit 3, c
  jr z, _sp_sprite_16_first
  inc b
_sp_sprite_16_first:
  res 3, c
_sp_sprite_tile:
  ld l, b
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld a, h
  or d
  ld h, a
  ld a, l
  or c
  ld l, a
  ret

_sp_pixel:
  ld a, (SP_HIT_PENDING)
  or a
  jr z, _sp_pixel_not_pending
  ld a, (SP_STATUS)
  or $40
  ld (SP_STATUS), a
  xor a
  ld (SP_HIT_PENDING), a
_sp_pixel_not_pending:
  ld a, (SC_LINE+1)
  or a
  ret nz
  ld hl, (SC_DOT)
  ld a, h
  or a
  ret nz                  ; x255 never raises sprite-zero hit
  ld a, l
  or a
  ret z
  ld a, ($cb09)
  and $18
  cp $18
  ret nz
  ld a, (SP_STATUS)
  and $40
  ret nz
  ld a, (SP_ZERO_ACTIVE)
  or a
  ret z
  ld a, l
  cp 9
  jr nc, _sp_pixel_unclipped
  ld a, ($cb09)
  and 6
  cp 6
  ret nz
_sp_pixel_unclipped:
  ld a, (SP_ZERO_X)
  ld c, a
  ld a, l
  dec a
  sub c
  cp 8
  ret nc
  ld b, a
  ld a, (SP_ZERO_ATTR)
  and $40
  ld a, b
  jr nz, _sp_pixel_sprite_bit
  xor 7
_sp_pixel_sprite_bit:
  ld b, a
  ld a, (SP_ZERO_LOW)
  ld c, a
  ld a, (SP_ZERO_HIGH)
  or c
  inc b
_sp_pixel_sprite_shift:
  rrca
  djnz _sp_pixel_sprite_shift
  ret nc
  ; Dot1 selects the first pixel before any shift; dots2..256 select after
  ; shifting. The source sprite-zero flag follows the selected pixel by a dot.
  ld a, (SP_BG_LOW+1)
  ld c, a
  ld a, (SP_BG_HIGH+1)
  or c
  ld c, a
  ld a, (CN_FINE_X)
  xor 7
  inc a
  ld b, a
  ld a, c
_sp_pixel_bg_shift:
  rrca
  djnz _sp_pixel_bg_shift
  ret nc
  ld a, 1
  ld (SP_HIT_PENDING), a
  ret

; Constant-context adapter guard, not a claim that mid-frame changes do not
; exist on CNROM. The real source transfer happens before this notification.
rt_source_ppu_mutation:
  push af
  push bc
  push de
  push hl
  ld b, a
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sp_mutation_pre
  ld a, l
  cp 240
  jr nc, _sp_mutation_done
  ld a, b
  call rt_cnrom_packet_visible_mutation
  jr _sp_mutation_done
_sp_mutation_pre:
  ld a, l
  cp 5
  jr nz, _sp_mutation_done
  ld hl, (SC_DOT)
  ld de, 321
  or a
  sbc hl, de
  jr c, _sp_mutation_done
  ld a, 1
  ld (SP_PREFETCH_DIRTY), a
_sp_mutation_done:
  pop hl
  pop de
  pop bc
  pop af
  ret

; CPU $2004/$2007 contention and nonzero OAMADDR rendering corruption remain
; explicitly unsupported. VBlank/post-render accesses retain normal bus rules.
rt_source_ppu_guard_render_access:
  push af
  push hl
  ld a, ($cb09)
  and $18
  jr z, _sp_access_safe
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sp_access_pre
  ld a, l
  cp 240
  jp c, rt_cnrom_unsupported
  jr _sp_access_safe
_sp_access_pre:
  ld a, l
  cp 5
  jp z, rt_cnrom_unsupported
_sp_access_safe:
  pop hl
  pop af
  ret

_sp_forced_blank:
  ; Palette-address backdrop override is observable even with both layers
  ; off. The first constant-context adapter does not flatten that into $3F00.
  ld a, ($cb0f)
  and $3f
  cp $3f
  ret nz
  ld hl, (SC_LINE)
  ld a, h
  or a
  ret nz
  ld a, l
  cp 240
  ret nc
  ld a, 9
  jp rt_source_ppu_mutation
.ends
.endif
