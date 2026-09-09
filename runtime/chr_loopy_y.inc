; HL=15-bit loopy v -> next vertical pixel. DE preserved; AF/BC scratch.
rt_mmc3_vertical_increment:
  ld a, h
  and $70
  cp $70
  jr z, _m3p_vertical_coarse
  ld a, h
  add a, $10
  ld h, a
  ret
_m3p_vertical_coarse:
  ld a, h
  and $0f
  ld h, a
  and 3
  rlca
  rlca
  rlca
  ld b, a
  ld a, l
  rlca
  rlca
  rlca
  and 7
  or b
  cp 29
  jr z, _m3p_vertical_toggle
  cp 31
  jr z, _m3p_vertical_zero
  ld a, l
  add a, 32
  ld l, a
  ret nc
  inc h
  ret
_m3p_vertical_toggle:
  ld a, h
  xor 8
  ld h, a
_m3p_vertical_zero:
  ld a, h
  and $0c
  ld h, a
  ld a, l
  and $1f
  ld l, a
  ret

