; chrmap.s — runtime tile remapping + background sub-palette baking.
;
; THE PALETTE PROBLEM: the NES picks one of 4 background sub-palettes per
; 16x16 region via the attribute table. SMS Mode 4 has a single 16-colour
; background palette (CRAM 0-15) and only a 1-bit palette-select per tile, so
; converted NES tiles (pixel values 0-3) can only ever index CRAM 0-3 — one
; sub-palette. Clouds (sub-palette 2, white) therefore rendered with the bush
; palette (sub-palette 0, green).
;
; THE FIX (SMS-native): bake the sub-palette S into the tile's high bitplanes
; so a pixel becomes S*4 + nes_pixel, indexing the full CRAM 0-15. CRAM then
; holds all four NES bg sub-palettes at once (0-3, 4-7, 8-11, 12-15). Because
; the same NES tile is used with different sub-palettes (cloud vs bush share
; tiles), variants are generated on demand into a pool of SMS bg slots (0-255)
; and cached by (base slot, S). 1-1's distinct combos fit well under 256.
;
; Per-cell sub-palette S comes from the attribute table, stored in the existing
; nametable shadow ($CC00, 2 low bits; the high byte is otherwise 0 now). The
; tile write reads S there and resolves the variant.
;
; RAM:
;   $CA00        bg variant pool next-free slot (0-255)
;   $CA01-$CA06  do_variant scratch (p2, p3, slot, src ptr lo/hi, attr S)
;   $CA07        ring-wrapped flag (0 until slots 64-255 have all been used)
;   $CA40-$CAFF  reverse map for recycled slots 64-255: slot -> old base tile
;   $CC00-$D2FF  nametable shadow — active per-cell sub-palette S (0-3)
;   $D300-$D3DF  retired compact folded S mirror; reserved for future migration
;   $D600-$D9FF  variant cache FC[base*4 + S] -> pool slot ($FF = unassigned)

.define BGV_CACHE      $d600   ; FC[base*4+S] -> slot, 1024 bytes, $FF=empty
.define BGV_BSHADOW    $da00   ; base slot per cell (cell-indexed), 896 bytes
.define BGV_POOL_NEXT  $ca00
.define BGV_P2         $ca01
.define BGV_P3         $ca02
.define BGV_SLOT       $ca03
.define BGV_SRC        $ca04   ; + $ca05
.define BGV_ATTR_S     $ca06
.define BGV_RING_WRAPPED $ca07
.define BGV_REV_BASE   $ca40   ; 192 bytes: base tile for slots 64..255

.section "chrmap" free

; ─── rt_bg_gen_variant ──────────────────────────────────────────────────────
; Generate one background tile variant into a VRAM pool slot: copy the base
; tile's low bitplanes from ROM (data_chr) and fill the high bitplanes with the
; sub-palette S (so every pixel gains S*4).
;   Entry: A = pool slot (0-255), B = S (0-3), C = base slot (0-255).
;   Clobbers AF, BC, DE, HL. Leaves data_prg_low mapped in slot 2.
rt_bg_gen_variant:
  ld   (BGV_SLOT), a
  ; high-plane fill bytes from S: plane2 = (S&1)?$FF:0, plane3 = (S&2)?$FF:0
  ld   a, b
  and  $01
  jr   z, _gv_p2_zero
  ld   a, $ff
_gv_p2_zero:
  ld   (BGV_P2), a
  ld   a, b
  and  $02
  jr   z, _gv_p3_zero
  ld   a, $ff
_gv_p3_zero:
  ld   (BGV_P3), a
  ; source = data_chr ($8000) + base*32
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   de, $8000
  add  hl, de
  ld   (BGV_SRC), hl
  ; dest VRAM = slot*32 (bg region $0000-$1FE0)
  ld   a, (BGV_SLOT)
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   a, l
  out  ($bf), a
  ld   a, h
  or   $40
  out  ($bf), a
  ; map data_chr bank for the source reads
  ld   a, :data_chr
  ld   ($ffff), a
  ld   hl, (BGV_SRC)
  ld   b, 8
_gv_row:
  ld   a, (hl)               ; plane 0 (low NES bitplane)
  out  ($be), a
  inc  hl
  ld   a, (hl)               ; plane 1
  out  ($be), a
  inc  hl                    ; skip source planes 2,3 (zero in data_chr)
  ld   a, (BGV_P2)
  out  ($be), a              ; plane 2 = S bit 0
  ld   a, (BGV_P3)
  out  ($be), a              ; plane 3 = S bit 1
  inc  hl
  inc  hl
  djnz _gv_row
  ld   a, :data_prg_low
  ld   ($ffff), a
  ret

; ─── rt_bg_get_variant ──────────────────────────────────────────────────────
; Resolve (base slot, S) to a bg pool slot, generating + caching on first use.
;   Entry: C = base slot, B = S (0-3).  Exit: A = pool slot.
;   Clobbers AF, DE, HL (B, C consumed).
rt_bg_get_variant:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl                ; base*4
  ld   a, b
  add  a, l
  ld   l, a
  jr   nc, _gbv_nc
  inc  h
_gbv_nc:
  ld   de, BGV_CACHE
  add  hl, de                ; HL = &FC[base*4+S]
  ld   a, (hl)
  inc  a                     ; $FF -> 0 (Z) means unassigned
  jr   z, _gbv_alloc
  dec  a
  ret                        ; A = cached slot
_gbv_alloc:
  ; A full 1-1 traversal produces far more (base,S) combos (~800) than the 256
  ; background slots, but only ~150 are on screen at once. So slots 64-255 are a
  ; RING: a slot is only recycled after ~38 columns of scrolling, after its tile
  ; left the screen (SMB's 1-1 never scrolls back left). Slots 0-63 are pinned
  ; for the common tiles allocated on the first screen (sky, ground, brick,
  ; pipe, bush, status bar, ...) so the recurring graphics stay correct all
  ; level; only rarer mid-level-specific tiles ride the ring.
  ld   a, (BGV_POOL_NEXT)
  ld   (BGV_SLOT), a
  push hl                    ; save FC[idx] for the new mapping
  push bc                    ; keep B=S, C=base for variant generation
  call _bgv_invalidate_recycled_slot
  pop  bc
  pop  hl
  ld   a, (BGV_SLOT)
  ld   (hl), a               ; FC[idx] = slot
  ; Remember which base tile now owns this ring slot. Slots 0-63 are pinned and
  ; never recycled after wrap, so only slots 64-255 need reverse-map entries.
  cp   64
  jr   c, _gbv_remember_done
  sub  64
  ld   e, a
  ld   d, $00
  ld   hl, BGV_REV_BASE
  add  hl, de
  ld   (hl), c
_gbv_remember_done:
  ld   a, (BGV_SLOT)
  inc  a
  jr   nz, _gbv_set          ; 255 -> 0 means the ring wrapped
  ld   a, $01
  ld   (BGV_RING_WRAPPED), a
  ld   a, 64                 ; wrap back to the start of the ring (pin 0-63)
_gbv_set:
  ld   (BGV_POOL_NEXT), a
  ld   a, (BGV_SLOT)
  call rt_bg_gen_variant     ; A=slot, B=S, C=base
  ld   a, (BGV_SLOT)
  ret

; Clear the stale FC[old_base*4+old_S] entry before reusing a ring slot.
; Without this, a later request for the old (base,S) pair can hit the cache and
; return a slot whose VRAM pattern has since been regenerated for another tile.
; Entry: BGV_SLOT = slot being allocated. Clobbers AF, C, DE, HL.
_bgv_invalidate_recycled_slot:
  ld   a, (BGV_RING_WRAPPED)
  or   a
  ret  z                     ; first pass: reverse map not complete yet
  ld   a, (BGV_SLOT)
  cp   64
  ret  c                     ; pinned slots are never recycled
  sub  64
  ld   e, a
  ld   d, $00
  ld   hl, BGV_REV_BASE
  add  hl, de
  ld   c, (hl)               ; old base tile for this slot
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl                ; old base*4
  ld   de, BGV_CACHE
  add  hl, de                ; HL = &FC[old_base*4]
  ld   a, (BGV_SLOT)
  ld   e, a                  ; E = reused slot number
  ld   d, 4                  ; scan old base's four sub-palette entries
_gbv_inv_loop:
  ld   a, (hl)
  cp   e
  jr   nz, _gbv_inv_next
  ld   a, $ff
  ld   (hl), a
  ret
_gbv_inv_next:
  inc  hl
  dec  d
  jr   nz, _gbv_inv_loop
  ret

; ─── rt_bg_map_base_slot ─────────────────────────────────────────────────────
; Map a NES background tile byte through the active BG CHR map.
;   Entry: A = NES bg tile byte.
;   Exit:  A = mapped SMS base slot.
;   Preserves BC, DE, HL. Clobbers AF. Leaves data_prg_low mapped in slot 2.
; Helper-only scaffold for a later CIRAM materializer; current rendering paths
; still use their inlined lookups and BGV_BSHADOW.
rt_bg_map_base_slot:
  push hl
  push de
  push bc
  ld   c, a
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bg_map_base_slot_table1
  ld   de, data_chr_bg_map0
  jr   _bg_map_base_slot_ready
_bg_map_base_slot_table1:
  ld   de, data_chr_bg_map1
_bg_map_base_slot_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   c, (hl)                ; byte 0 of two-byte map record = base slot
  ld   a, :data_prg_low
  ld   ($ffff), a
  ld   a, c
  pop  bc
  pop  de
  pop  hl
  ret

; ─── _bgv_sub_palette ───────────────────────────────────────────────────────
; Read the per-cell sub-palette S (0-3) from the nametable shadow.
;   Entry: HL = SMS nametable low-byte address ($3700-$3EFE).
;   Exit:  A = S (0-3). Clobbers HL.
_bgv_sub_palette:
  inc  hl                    ; -> high-byte address
  ld   a, h
  cp   $3e
  jr   nc, _bgv_sub_palette_s0 ; hidden rows are outside the active shadow
  add  a, $95                ; $37xx -> $CCxx shadow
  ld   h, a
  ld   a, (hl)
  and  $03
  ret
_bgv_sub_palette_s0:
  xor  a
  ret

; ─── _bgv_base_addr ─────────────────────────────────────────────────────────
; Map an SMS nametable low-byte address to its per-cell base-slot shadow byte.
;   Entry: HL = NT low-byte address ($3700-$3EFE).
;   Exit:  HL = &BGV_BSHADOW[cell]. Clobbers A, DE.
_bgv_base_addr:
  ld   de, $c900             ; + $C900 == - $3700 (mod 16-bit)
  add  hl, de
  srl  h
  rr   l                     ; >>1 = cell index
  ld   de, BGV_BSHADOW
  add  hl, de
  ret

; ─── rt_write_mapped_bg_tile ────────────────────────────────────────────────
; Write a background nametable entry. Entry: A = NES tile byte; the caller has
; set the VDP write address to the cell. Resolves the (base, S) variant and
; writes its slot as the tile, palette 0 (sub-palette baked into the pixels).
; Preserves BC, DE, HL.
rt_write_mapped_bg_tile:
  push hl
  push de
  push bc
  ld   c, a
  ld   ($cb17), de           ; SMS nametable low-byte address

  ; base slot from the BG map
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bgw_table1
  ld   de, data_chr_bg_map0
  jr   _bgw_map_ready
_bgw_table1:
  ld   de, data_chr_bg_map1
_bgw_map_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   a, (hl)               ; base slot (bg tiles are 0-255)
  ld   c, a                  ; C = base slot
  ld   a, :data_prg_low
  ld   ($ffff), a

  ; In nametable range: record the base slot for this cell (so a later
  ; attribute write can re-resolve the variant), then read the sub-palette S.
  ld   hl, ($cb17)
  ld   a, h
  cp   $37
  jr   c, _bgw_s0
  cp   $40
  jr   nc, _bgw_s0
  push bc                    ; save base slot (C)
  call _bgv_base_addr        ; HL(low addr) -> base-shadow addr
  pop  bc
  ld   (hl), c               ; base-shadow[cell] = base slot
  ld   hl, ($cb17)
  call _bgv_sub_palette      ; A = S
  jr   _bgw_have_s
_bgw_s0:
  xor  a
_bgw_have_s:
  ld   b, a                  ; B = S
  call rt_bg_get_variant     ; -> A = pool slot
  ld   c, a                  ; C = variant slot

  ; write the nametable entry (re-set the address: gen may have moved it)
  ld   hl, ($cb17)
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0 (palette 0, tile bit 8 = 0)

  pop  bc
  pop  de
  pop  hl
  ret

; ─── rt_write_mapped_bg_tile_s ──────────────────────────────────────────────
; Helper-only explicit-subpalette variant of rt_write_mapped_bg_tile.
; Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable low-byte address.
; Resolves the (base slot, S) variant without consulting folded per-cell
; palette state. Still records the base slot in BGV_BSHADOW for folded SMS
; cells in the nametable range $3700-$3EFF so later explicit-S redraw helpers
; can re-resolve the cell.
; Preserves BC, DE, HL. Clobbers AF. Leaves data_prg_low mapped in slot 2.
; Scaffold only: current hot paths still call rt_write_mapped_bg_tile.
rt_write_mapped_bg_tile_s:
  push hl
  push de
  push bc
  ld   ($cb13), a            ; temporary save NES tile byte
  ld   a, b
  and  $03
  ld   ($cb16), a            ; explicit S
  ld   a, ($cb13)
  ld   c, a
  ld   ($cb17), de           ; SMS nametable low-byte address

  ; base slot from the BG map
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bgw_s_table1
  ld   de, data_chr_bg_map0
  jr   _bgw_s_map_ready
_bgw_s_table1:
  ld   de, data_chr_bg_map1
_bgw_s_map_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   a, (hl)               ; base slot (bg tiles are 0-255)
  ld   c, a                  ; C = base slot
  ld   a, :data_prg_low
  ld   ($ffff), a

  ; In nametable range: record the base slot for this folded SMS cell.
  ld   hl, ($cb17)
  ld   a, h
  cp   $37
  jr   c, _bgw_s_no_base_shadow
  cp   $3f
  jr   nc, _bgw_s_no_base_shadow
  push bc                    ; save base slot (C)
  call _bgv_base_addr        ; HL(low addr) -> base-shadow addr
  pop  bc
  ld   (hl), c               ; base-shadow[cell] = base slot
_bgw_s_no_base_shadow:
  ld   a, ($cb16)
  ld   b, a                  ; B = explicit S
  call rt_bg_get_variant     ; -> A = pool slot
  ld   c, a                  ; C = variant slot

  ; write the nametable entry (re-set the address: gen may have moved it)
  ld   hl, ($cb17)
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0 (palette 0, tile bit 8 = 0)

  pop  bc
  pop  de
  pop  hl
  ret

; ─── rt_write_mapped_bg_tile_s_noshadow ─────────────────────────────────────
; Write a background nametable entry from an explicit subpalette without
; touching the per-cell base-slot shadow.
;   Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable low-byte address.
;   Preserves BC, DE, HL. Clobbers AF. Leaves data_prg_low mapped in slot 2.
; Helper-only scaffold for a later CIRAM materializer; current paths still use
; BGV_BSHADOW so attribute redraw remains route-equivalent.
rt_write_mapped_bg_tile_s_noshadow:
  push hl
  push de
  push bc
  call rt_bg_map_base_slot    ; -> A = base slot, preserves B=S and DE=addr
  ld   c, a                   ; C = base slot, B = explicit S
  call rt_bg_get_variant      ; -> A = pool slot
  ld   h, a                   ; keep variant while restoring caller registers
  pop  bc
  pop  de

  ; Write the nametable entry. Variant generation may have moved the VDP addr.
  ld   a, e
  out  ($bf), a
  ld   a, d
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, h
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0 (palette 0, tile bit 8 = 0)

  pop  hl
  ret

; ─── rt_redraw_bg_cell_tile_s_noshadow ──────────────────────────────────────
; Helper-only scaffold for a future DA00-free attribute/materializer redraw.
; Redraw one SMS nametable cell from an explicit NES tile byte and subpalette
; without reading/writing BGV_BSHADOW, folded $CC00 state, or compact $D300
; state. Intentionally uncalled by current runtime paths.
;   Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable high-byte address.
;          The corresponding tile low-byte address is DE-1.
;   Preserves BC, DE, HL. Clobbers AF. Leaves data_prg_low mapped in slot 2.
rt_redraw_bg_cell_tile_s_noshadow:
  push de
  dec  de                    ; high-byte addr -> low-byte tile addr
  call rt_write_mapped_bg_tile_s_noshadow
  pop  de
  ret

; Map a NES sprite tile to an SMS sprite tile byte.
; Entry: A = NES OAM tile byte. Uses PPUCTRL bit 3 ($CB08) to choose NES sprite
; pattern table 0/1. Table 0 maps to tile bytes for SMS sprite base $2000;
; table 1 maps to tile bytes for SMS sprite base $0000.
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
;   high = $37 + (attr_offset >> 3)
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

; Update one NES attribute-table quadrant: set the sub-palette S (0-3) for the
; 2x2 cells it covers, and re-resolve each cell's baked-in tile variant. Doing
; the re-resolve here (rather than relying on a following tile write) makes the
; result correct regardless of whether SMB writes tiles or attributes first:
; the last writer (tile or attribute) sees both the base slot (from the cell's
; base-shadow) and S, so the final variant is right.
; Entry: A = NES palette selector 0..3, DE = SMS nametable high-byte address
; for the top-left tile. Preserves BC and DE.
rt_write_bg_attr_quadrant:
  push bc
  push de
  and  $03
  ld   (BGV_ATTR_S), a       ; S for this quadrant
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 62                 ; next row, same column
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  pop  de
  pop  bc
  ret

; Expensive helper-only explicit-subpalette redraw for one NES attribute-table
; quadrant. This does not update folded per-cell palette state; it uses the
; existing BGV_BSHADOW base-slot bytes to redraw the four covered cells by
; resolving each with explicit S. It is scaffold for later materializer work and
; is not called by current hot paths.
; Entry: A = S (0..3), DE = SMS nametable high-byte address for top-left tile.
; Preserves BC and DE. Clobbers AF, HL.
rt_redraw_bg_attr_quadrant_s:
  push bc
  push de
  and  $03
  ld   (BGV_ATTR_S), a       ; explicit S for this quadrant
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 62                 ; next row, same column
  ld   e, a
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one_s
  pop  de
  pop  bc
  ret

; Set S for one visible cell and rewrite its tile to the matching variant.
; Entry: DE = nametable high-byte address; S in BGV_ATTR_S. Preserves DE.
_chrmap_attr_write_one:
  push de
  ld   a, d
  cp   $3e
  jr   nc, _caw_done         ; hidden rows are outside the active $CC00-$D2FF shadow
  ; shadow byte (high-byte addr -> $CCxx). Skip the whole re-resolve if the
  ; sub-palette is unchanged (the usual case while scrolling), so attribute
  ; traffic stays cheap; the tile write keeps the variant correct otherwise.
  ld   h, d
  ld   l, e
  ld   a, h
  add  a, $95
  ld   h, a
  ld   a, (BGV_ATTR_S)
  ld   c, a                  ; C = new S
  ld   a, (hl)
  and  $03
  cp   c
  jr   z, _caw_done          ; unchanged -> done
  ld   (hl), c               ; store new S
  ; base slot for this cell, from the low-byte address (high addr - 1)
  ld   h, d
  ld   l, e
  dec  hl                    ; HL = NT low-byte address
  push hl                    ; save for the VDP write
  call _bgv_base_addr        ; HL -> base-shadow addr
  ld   c, (hl)               ; C = base slot
  ld   a, (BGV_ATTR_S)
  ld   b, a                  ; B = S
  call rt_bg_get_variant     ; -> A = variant slot
  ld   c, a
  pop  hl                    ; HL = NT low-byte address
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0
_caw_done:
  pop  de
  ret

; Redraw one cell using explicit S and the base-slot shadow only.
; Entry: DE = nametable high-byte address; S in BGV_ATTR_S. Preserves DE.
_chrmap_attr_write_one_s:
  push de
  ld   h, d
  ld   l, e
  dec  hl                    ; HL = NT low-byte address
  push hl                    ; save for the VDP write
  call _bgv_base_addr        ; HL -> base-shadow addr
  ld   c, (hl)               ; C = base slot
  ld   a, (BGV_ATTR_S)
  ld   b, a                  ; B = explicit S
  call rt_bg_get_variant     ; -> A = variant slot
  ld   c, a
  pop  hl                    ; HL = NT low-byte address
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0
  pop  de
  ret

.ends
