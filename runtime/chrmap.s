; chrmap.s — runtime tile remapping for packed CHR assets.
;
; The SMS nametable/SAT reserve $3800-$3FFF, leaving 448 usable pattern
; slots. Generated data_chr_maps maps NES pattern-table tile identities to
; those packed SMS slots.

.section "chrmap" free

; Write a mapped background nametable entry to the current VDP data address.
; Entry: A = NES tile byte. Uses PPUCTRL bit 4 ($CB08) to choose NES BG
; pattern table 0/1. Writes two SMS nametable bytes: low tile byte, high byte.
; Clobbers: AF. Preserves BC, DE, HL.
rt_write_mapped_bg_tile:
  push hl
  push de
  push bc
  ld   c, a
  ld   ($cb17), de           ; SMS nametable low-byte address for shadow update

  ld   a, :data_chr_maps
  ld   ($ffff), a

  ld   a, ($cb08)
  bit  4, a
  jr   nz, _chrmap_bg_table1
  ld   de, data_chr_bg_map0
  jr   _chrmap_bg_base_ready
_chrmap_bg_table1:
  ld   de, data_chr_bg_map1
_chrmap_bg_base_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl                ; two bytes per BG map entry
  add  hl, de
  ld   a, (hl)
  out  ($be), a              ; SMS tile low byte
  inc  hl
  ld   a, (hl)
  ld   c, a
  out  ($be), a              ; SMS nametable high byte

  ; Mirror the high byte in RAM. Attribute-table writes need to update only the
  ; palette bit while preserving the profile-mapped CHR tile high bit.
  ;
  ; Only mirror actual SMS nametable addresses. During generated PPUDATA writes,
  ; DE can transiently hold non-nametable VRAM addresses; blindly adding $9400
  ; maps those into NES RAM ($31xx->$C5xx, $32xx->$C6xx) and corrupts SMB's
  ; collision block/metatile buffers.
  ld   hl, ($cb17)
  ld   a, h
  cp   $37
  jr   c, _chrmap_bg_skip_shadow
  cp   $40
  jr   nc, _chrmap_bg_skip_shadow
  inc  hl                    ; high-byte address within SMS nametable
  ld   a, h
  add  a, $95                ; $3700->$CC00, $3E00->$D300 (224-line base)
  ld   h, a
  ld   (hl), c
_chrmap_bg_skip_shadow:

  ld   a, :data_prg_low
  ld   ($ffff), a
  pop  bc
  pop  de
  pop  hl
  ret

; Map a NES sprite tile to an SMS sprite tile byte.
; Entry: A = NES OAM tile byte. Uses PPUCTRL bit 3 ($CB08) to choose NES sprite
; pattern table 0/1. The generated map is relative to VDP sprite base $2000.
; Exit: A = SMS sprite tile byte. Preserves BC, DE, HL.
rt_map_sprite_tile:
  push hl
  push de
  push bc
  ld   c, a

  ld   a, :data_chr_maps
  ld   ($ffff), a

  ld   a, ($cb08)
  bit  3, a
  jr   nz, _chrmap_sprite_table1
  ld   de, data_chr_sprite_map0
  jr   _chrmap_sprite_base_ready
_chrmap_sprite_table1:
  ld   de, data_chr_sprite_map1
_chrmap_sprite_base_ready:
  ld   l, c
  ld   h, $00
  add  hl, de
  ld   a, (hl)
  ld   ($cb13), a

  ld   a, :data_prg_low
  ld   ($ffff), a
  pop  bc
  pop  de
  pop  hl
  ld   a, ($cb13)
  ret

; Build DE = SMS nametable high-byte address for the top-left tile covered by
; attribute offset $CB19. Formula:
;   high = $38 + (attr_offset >> 3)
;   low  = 1 + ((attr_offset & 7) * 8)
rt_attr_base_tl:
  ld   a, ($cb19)
  and  $07
  add  a, a
  add  a, a
  add  a, a
  inc  a
  ld   e, a
  ld   a, ($cb19)
  srl  a
  srl  a
  srl  a
  add  a, $37
  ld   d, a
  ret

; Update one NES attribute-table quadrant in the SMS nametable.
; Entry: A = NES palette selector 0..3, DE = SMS nametable high-byte address
; for the top-left tile covered by the quadrant. Writes the 2x2 high-byte block,
; preserving the mapped tile high bit from the $CC00 shadow and updating only
; the SMS palette-select bit ($08).
; Clobbers AF, HL. Preserves BC and DE for callers.
rt_write_bg_attr_quadrant:
  push bc
  push de
  and  $03
  jr   z, _chrmap_attr_palette0
  ld   c, $08
  jr   _chrmap_attr_got_palette
_chrmap_attr_palette0:
  ld   c, $00
_chrmap_attr_got_palette:
  ld   b, c                  ; B = palette bit, C is free as write temp
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 62                 ; from +2 to +64 = next row, same column
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  pop  de
  pop  bc
  ret

_chrmap_attr_write_one:
  push de
  ld   h, d
  ld   l, e
  ld   a, h
  add  a, $95
  ld   h, a                  ; HL = shadow address for this high byte (224-line base)
  ld   a, (hl)
  and  $f7                   ; clear palette bit, preserve mapped tile bit
  or   b
  ld   (hl), a
  ld   c, a                  ; C = high byte to write after VDP address setup
  pop  de

  ld   a, e
  out  ($bf), a
  ld   a, d
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a
  ret

.ends
