; New source-hardware mode only; included inside the banked renderer section.
; Sprite-outer composition preserves original SAT occupancy. Source first
; opaque ownership precedes the winning sprite's behind-background test.
; C880 target raw tile survives all earlier-row reads into C890. Scratch
; lifetimes and helper clobbers are documented in the packet closure plan.

; HL=residency byte, B=bit. DE preserved.
_cnp_sprite_bitmap:
  ld a, (M3R_OAM)
  add a, a
  ld c, a
  ld a, (M3R_SPRITE_HALF)
  or c
  ld c, a
  and 7
  ld b, a
  ld a, 1
  jr z, _cnp_bitmap_bit
_cnp_bitmap_shift:
  add a, a
  djnz _cnp_bitmap_shift
_cnp_bitmap_bit:
  ld b, a
  ld a, c
  rrca
  rrca
  rrca
  and $0f
  add a, $10
  ld l, a
  ld h, $d4
  ret

; AF scratch; all conversion arguments preserved.
_cnp_sprite_touch:
  push bc
  push de
  push hl
  call _cnp_sprite_bitmap
  ld a, l
  add a, $10
  ld l, a
  ld a, (hl)
  or b
  ld (hl), a
  pop hl
  pop de
  pop bc
  ret

; HL=physical source tile, DE=VRAM destination, B=original attributes.
_cnp_priority_convert:
  ld (M3R_V), de
  ld a, b
  ld (M3R_ATTR), a
  ld de, $c880
  call _m3r_fetch_tile
  ld de, (M3R_V)
  call rt_chr_packet_sprite_stage_address
  ld (M3R_KEY2), de
  call _cnp_sprite_bitmap
  ld a, (hl)
  and b
  ld a, 0
  jr nz, _cnp_priority_resident
  inc a
_cnp_priority_resident:
  ld (M3R_ROW), a
  ld a, (M3R_OAM)
  add a, a
  add a, a
  add a, 3
  ld l, a
  ld h, $98
  ld a, (hl)
  ld (M3R_PAL2), a
  xor a
  ld (M3R_PIXEL), a
_cnp_priority_row:
  ; Host service only at a closed mapping/VDP boundary; source remains paused.
  ei
  nop
  di
  ld a, (M3R_OAM)
  add a, a
  add a, a
  ld l, a
  ld h, $98
  ld c, (hl)
  ld a, (M3R_SPRITE_HALF)
  add a, a
  add a, a
  add a, a
  add a, c
  inc a
  ld c, a
  ld a, (M3R_PIXEL)
  add a, c
  ld (M3R_CUT), a
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  ; Visible OAM Y<=238 plus1+15 is at most254: no wrapped top row.
  cp CNP_DISPLAY_LINES
.else
  cp 224
.endif
  jp nc, _cnp_priority_zero
  ld a, (M3R_PIXEL)
  ld c, a
  ld a, (M3R_ATTR)
  bit 7, a
  ld a, c
  jr z, _cnp_target_row
  xor 7
_cnp_target_row:
  add a, $80
  ld l, a
  ld h, $c8
  ld a, (hl)
  ld (M3R_P0), a
  ld a, l
  add a, 8
  ld l, a
  ld a, (hl)
  ld (M3R_P1), a
  ld a, (M3R_ATTR)
  bit 6, a
  jr z, _cnp_target_unflipped
  ld a, (M3R_P0)
  call _m3r_reverse
  ld (M3R_P0), a
  ld a, (M3R_P1)
  call _m3r_reverse
  ld (M3R_P1), a
_cnp_target_unflipped:
  xor a
  ld (M3R_BITS), a
  ld (M3R_SECOND), a
  ld (M3R_ROW_ATTR), a
_cnp_count_earlier:
  ld a, (M3R_OAM)
  ld b, a
  ld a, (M3R_SECOND)
  cp b
  jr z, _cnp_count_done
  call _cnp_earlier_y
  jr nc, _cnp_count_next
  ld a, (M3R_ROW_ATTR)
  inc a
  ld (M3R_ROW_ATTR), a
  cp 8
  jp z, _cnp_priority_zero
_cnp_count_next:
  ld a, (M3R_SECOND)
  inc a
  ld (M3R_SECOND), a
  jr _cnp_count_earlier
_cnp_count_done:
  xor a
  ld (M3R_SECOND), a
_cnp_decode_earlier:
  ld a, (M3R_OAM)
  ld b, a
  ld a, (M3R_SECOND)
  cp b
  jr z, _cnp_ownership_done
  call _cnp_earlier_y
  jr nc, _cnp_decode_next
  ld (M3R_OFFSET2), a
  inc hl
  ld a, (hl)
  ld (M3R_ROW_QUADRANT), a
  inc hl
  ld a, (hl)
  ld (M3R_ROW_ATTR_LOW), a
  inc hl
  ld a, (hl)
  ld (M3R_OFFSET1), a
  call _cnp_earlier_opacity
  ld b, a
  ld a, (M3R_BITS)
  or b
  ld (M3R_BITS), a
_cnp_decode_next:
  ld a, (M3R_SECOND)
  inc a
  ld (M3R_SECOND), a
  jr _cnp_decode_earlier
_cnp_ownership_done:
  ld a, (M3R_BITS)
  cpl
  ld b, a
  ld a, (M3R_P0)
  ld c, a
  ld a, (M3R_P1)
  or c
  and b
  ld (M3R_COL), a
  ; Global left clipping can be applied after ownership: every competing
  ; sprite is clipped at precisely these same screen coordinates.
  ld a, (M3G_RECORD+9)
  bit 2, a
  jr nz, _cnp_right_clip
  ld a, (M3R_PAL2)
  cp 8
  jr nc, _cnp_right_clip
  or a
  ld b, a
  ld a, 1
  jr z, _cnp_left_mask
_cnp_left_shift:
  add a, a
  djnz _cnp_left_shift
_cnp_left_mask:
  dec a
  ld b, a
  ld a, (M3R_COL)
  and b
  ld (M3R_COL), a
_cnp_right_clip:
  ld a, (M3R_PAL2)
  sub 249
  jr c, _cnp_test_behind
  inc a
  ld b, a
  ld a, $ff
_cnp_right_shift:
  add a, a
  djnz _cnp_right_shift
  ld b, a
  ld a, (M3R_COL)
  and b
  ld (M3R_COL), a
_cnp_test_behind:
  ld a, (M3R_ATTR)
  bit 5, a
  jr z, _cnp_priority_output
  xor a
  ld (M3R_ROW_ATTR), a
  ld (M3R_ROW_ATTR_LOW), a
  ld a, $80
  ld (M3R_ROW_QUADRANT), a
_cnp_background_pixel:
  ld a, (M3R_PAL2)
  ld b, a
  ld a, (M3R_ROW_ATTR)
  add a, b
  jr c, _cnp_background_next
  ld b, a
  ld a, (M3R_CUT)
  ld c, a
  call _cnp_background_opaque
  jr z, _cnp_background_next
  ld a, (M3R_ROW_ATTR_LOW)
  ld b, a
  ld a, (M3R_ROW_QUADRANT)
  or b
  ld (M3R_ROW_ATTR_LOW), a
_cnp_background_next:
  ld a, (M3R_ROW_QUADRANT)
  srl a
  ld (M3R_ROW_QUADRANT), a
  ld a, (M3R_ROW_ATTR)
  inc a
  ld (M3R_ROW_ATTR), a
  cp 8
  jr c, _cnp_background_pixel
  ld a, (M3R_ROW_ATTR_LOW)
  cpl
  ld b, a
  ld a, (M3R_COL)
  and b
  ld (M3R_COL), a
  jr _cnp_priority_output
_cnp_priority_zero:
  xor a
  ld (M3R_COL), a
_cnp_priority_output:
  ld hl, (M3R_KEY2)
  ld a, (M3R_PIXEL)
  add a, a
  add a, a
  ld e, a
  ld d, 0
  add hl, de
  ld a, (M3R_COL)
  ld b, a
  ld a, (M3R_P0)
  and b
  call _cnp_store_byte
  ld a, (M3R_P1)
  and b
  call _cnp_store_byte
  ld a, (M3R_ATTR)
  bit 0, a
  ld a, 0
  jr z, _cnp_output_plane2
  ld a, b
_cnp_output_plane2:
  call _cnp_store_byte
  ld a, (M3R_ATTR)
  bit 1, a
  ld a, 0
  jr z, _cnp_output_plane3
  ld a, b
_cnp_output_plane3:
  call _cnp_store_byte
  ld a, (M3R_PIXEL)
  inc a
  ld (M3R_PIXEL), a
  cp 8
  jp c, _cnp_priority_row
  ld a, (M3R_ROW)
  or a
  jr z, _cnp_priority_clean
  ld de, (M3R_V)
  call rt_mmc3_pattern_stage
_cnp_priority_clean:
  ; Masked content is not described by the normal physical/attribute key.
  ld a, (M3R_OAM)
  add a, a
  ld b, a
  ld a, (M3R_SPRITE_HALF)
  or b
  ld l, a
  ld h, 0
  ld e, l
  ld d, h
  add hl, hl
  add hl, de
  ld de, $d800
  add hl, de
  ld (hl), $ff
  inc hl
  ld (hl), $ff
  ; Restore ordinary converter's geometry scratch before a subsequent sprite.
  xor a
  ld (M3R_CUT), a
  ld (M3R_OFFSET1), a
  ld (M3R_OFFSET2), a
  ret

; A=index. Carry means target screenY in this sprite; A=relative row,
; HL=frozen Y. Invalid/offscreen Y never wraps onto the top of the viewport.
_cnp_earlier_y:
  add a, a
  add a, a
  ld l, a
  ld h, $98
  ld b, (hl)
  ld a, (M3R_CUT)
  sub b
  jr c, _cnp_earlier_absent
  jr z, _cnp_earlier_absent
  dec a
  ld c, a
  ld a, (M3G_RECORD+8)
  bit 5, a
  ld a, c
  jr z, _cnp_earlier_short
  cp 16
  ret
_cnp_earlier_short:
  cp 8
  ret
_cnp_earlier_absent:
  or a
  ret

; Earlier raw opacity, aligned into target screen span. No target-plane writes.
_cnp_earlier_opacity:
  ld a, (M3G_RECORD+8)
  bit 5, a
  jr z, _cnp_earlier8
  ld a, (M3R_OFFSET2)
  ld c, a
  ld a, (M3R_ROW_ATTR_LOW)
  bit 7, a
  ld a, c
  jr z, _cnp_earlier16_row
  xor 15
_cnp_earlier16_row:
  ld (M3R_OFFSET2), a
  and 8
  rrca
  rrca
  rrca
  ld c, a
  ld a, (M3R_ROW_QUADRANT)
  and $fe
  or c
  ld c, a
  ld a, (M3R_ROW_QUADRANT)
  and 1
  rlca
  rlca
  ld b, a
  jr _cnp_earlier_key
_cnp_earlier8:
  ld a, (M3R_OFFSET2)
  ld c, a
  ld a, (M3R_ROW_ATTR_LOW)
  bit 7, a
  ld a, c
  jr z, _cnp_earlier8_row
  xor 7
_cnp_earlier8_row:
  ld (M3R_OFFSET2), a
  ld a, (M3G_RECORD+8)
  and 8
  rrca
  ld b, a
  ld a, (M3R_ROW_QUADRANT)
  ld c, a
_cnp_earlier_key:
  ld a, c
  rrca
  rrca
  rrca
  rrca
  rrca
  rrca
  and 3
  or b
  call _m3r_physical_key
  ld de, $c890
  call _m3r_fetch_tile
  ld a, (M3R_OFFSET2)
  and 7
  add a, $90
  ld l, a
  ld h, $c8
  ld c, (hl)
  ld a, l
  add a, 8
  ld l, a
  ld a, (hl)
  or c
  ld c, a
  ld a, (M3R_ROW_ATTR_LOW)
  bit 6, a
  ld a, c
  call nz, _m3r_reverse
  ld c, a
  ld a, (M3R_PAL2)
  ld b, a
  ld a, (M3R_OFFSET1)
  sub b
  jr c, _cnp_overlap_left
  cp 8
  jr nc, _cnp_no_overlap
  or a
  ld b, a
  ld a, c
  ret z
_cnp_overlap_right_shift:
  srl a
  djnz _cnp_overlap_right_shift
  ret
_cnp_overlap_left:
  neg
  cp 8
  jr nc, _cnp_no_overlap
  ld b, a
  ld a, c
_cnp_overlap_left_shift:
  add a, a
  djnz _cnp_overlap_left_shift
  ret
_cnp_no_overlap:
  xor a
  ret

; B=screen X,C=screen Y; Z iff prepared source BG is transparent.
; FineX is already baked; NT retains its source coarse-X ring. Only low
; planes determine opacity, irrespective of CRAM RGB equality. Restore SRAM0.
_cnp_background_opaque:
  ld a, b
  and 7
  ld (M3R_OFFSET1), a
  ld a, b
  rrca
  rrca
  rrca
  and 31
  ld b, a
  ld a, (M3G_RECORD+12)
  and 31
  add a, b
  and 31
  add a, a
  ld e, a
  ld a, (M3P_Y)
  add a, c
  ld c, a
  and 7
  add a, a
  add a, a
  ld (M3R_OFFSET2), a
  ld a, c
  and $f8
  ld l, a
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  ld d, $a0
  add hl, de
  ld l, (hl)
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld a, (M3R_OFFSET2)
  ld e, a
  ld d, $a0
  add hl, de
  ld a, 12
  ld ($fffc), a
  ld c, (hl)
  inc hl
  ld a, (hl)
  or c
  ld c, a
  ld a, 8
  ld ($fffc), a
  ld a, (M3R_OFFSET1)
  ld b, a
  ld a, $80
  or a
  inc b
_cnp_bg_mask:
  dec b
  jr z, _cnp_bg_test
  srl a
  jr _cnp_bg_mask
_cnp_bg_test:
  and c
  ret

; Exact candidate comparison; BC/DE preserved, HL advances one byte.
_cnp_store_byte:
  cp (hl)
  jr z, _cnp_store_same
  ld (hl), a
  ld a, 1
  ld (M3R_ROW), a
_cnp_store_same:
  inc hl
  ret
