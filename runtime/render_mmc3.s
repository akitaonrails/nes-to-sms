; Coherent, deliberately conservative full-frame MMC3 bring-up renderer.
; Pattern conversion targets cartridge SRAM; present_mmc3 owns guarded
; uploads/publication while this module builds an immutable next packet.
; No visible cache entry is overwritten during preparation. Pattern exhaustion
; traps instead of silently substituting another physical CHR tile.
;
; Bank0 SRAM: raw8000..87FF, frozen8800..97FF, OAM9800..98FF,
; records9900..997F, full32-row SMS NT A000..A7FF, BGkeysA800..AAFF,
; preparedSAT AB00..ABBF, hashheadsAC00..ADFF, chainsAE00..AFFF,
; mixed-raster secondary keys/palette/cut/row offsets B000..B5FF.
; The extra NT row is required for fine-Y scroll in 224-line mode's
; 256-pixel-high tilemap. BG uses256 slots at $0000..$1fff; fixed sprites
; use128 at $2000..$2fff. NT starts at $3700; the intervening VRAM is unused.
.ifdef MMC3_FULL_RUNTIME
.define M3R_COUNT   $c840
.define M3R_ROW     $c842
.define M3R_COL     $c843
.define M3R_Y      $c844 ; word: vertical loopy address at this tile row
.define M3R_V      $c846 ; word: current cell's loopy address
.define M3R_FINE_Y $c848
.define M3R_RECORD $c849 ; 0=playfield,40=HUD
.define M3R_PAL    $c84a
.define M3R_KEY    $c84b ; word: physical CHR tile (not logical tile number)
.define M3R_SLOT   $c84d
.define M3R_ATTR   $c84f
.define M3R_NT     $c858 ; word: prepared SMS NT cursor
.define M3R_HASH   $c85a
.define M3R_ROW_NT_HIGH $c85b
.define M3R_ROW_ATTR_HIGH $c85c
.define M3R_OAM    $c85d
.define M3R_PIXEL  $c85e
.define M3R_SPRITE_TILE $c85f
.define M3R_HUD_CRAM $c8a0
.define M3R_CRAM     $c8c0
.define M3R_P0       $c8e0
.define M3R_P1       $c8e1
.define M3R_BITS     $c8e2
.define M3R_CUT      $c8e4 ; first row of HUD inside a mixed 8-row pattern
.define M3R_KEY2     $c8e5 ; secondary physical CHR identity
.define M3R_PAL2     $c8e7
.define M3R_ROW_ATTR $c8e8
.define M3R_SECOND   $c8e9
.define M3R_OFFSET1  $c8ea ; source row = (output row + offset) & 7
.define M3R_OFFSET2  $c8eb
.define M3R_ROW_ATTR_LOW $c8ee
.define M3R_ROW_QUADRANT $c8ef
.define M3R_SPLIT    $c8f0
.define M3R_REG1     $c8f1
.define M3R_REG0     $c8f2
.define M3R_SPRITE_HALF $c8f3

.section "render_mmc3" free

rt_mmc3_frame_render:
  xor a
  jr _m3r_begin
; Caller proved both frozen BG sources, selected physical maps and raster
; geometry equal to the committed frame. Changed sprite patterns are staged;
; the publisher retains visible-slot ownership until its guarded transaction.
rt_mmc3_frame_render_bg_stable:
  ld a, 1
_m3r_begin:
  ld (M3G_COMPARE_BG), a    ; capture scratch is renderer's skip-BG flag
  di
  ld a, 8
  ld ($fffc), a
  call rt_mmc3_validate_layers
  ld a, (M3G_RECORD+9)
  and $18
  jr nz, _m3r_enabled
  ld (M3G_READY), a         ; rendering-off frames publish blank immediately
  jp rt_mmc3_blank_display
_m3r_enabled:
  call rt_mmc3_prepare_display
  ld a, (M3G_COMPARE_BG)
  or a
  jp nz, _m3r_bg_done
  ld a, 2
  ld (M3P_PASS), a           ; normal path protects ALL old-live slots
_m3r_pass:
  ld hl, $a000
  ld bc, $0800
  xor a
  call rt_mmc3_fill_closed
  ld a, (M3G_RECORD+12)
  ld l, a
  ld a, (M3G_RECORD+13)
  ld h, a
  and $70
  rrca
  rrca
  rrca
  rrca
  ld (M3R_FINE_Y), a
  ld (M3P_Y), a
  ld a, h
  and $0f
  ld h, a
  ld (M3R_Y), hl
  ld a, (M3G_RECORD+14)
  neg
  ld (M3P_X), a
  ld a, (M3G_RECORD+$40+14)
  neg
  ld (M3P_HUD_X), a
  ld hl, $a000
  ld (M3R_NT), hl
  xor a
  ld (M3R_ROW), a
_m3r_row:
  ; Common coarse-X ring for BOTH raster records. Source-column and mixed
  ; pattern construction stay screen-relative; only their NT destination is
  ; permuted. Both committed scroll values cancel this same permutation.
  ld a, (M3R_ROW)
  ld l, a
  ld h, 0
  .rept 6
  add hl, hl
  .endr
  ld de, $a000
  add hl, de
  ld a, (M3G_RECORD+12)
  and 31
  add a, a
  add a, l
  ld l, a
  ld (M3R_NT), hl
  xor a
  ld (M3R_RECORD), a
  ld a, (M3G_SPLIT)
  or a
  jr z, _m3r_row_record
  ld c, a
  ld a, (M3R_ROW)
  add a, a
  add a, a
  add a, a
  ld b, a
  ld a, (M3R_FINE_Y)
  ld e, a
  ld a, b
  sub e
  jr c, _m3r_row_record
  cp c
  jr c, _m3r_row_record
  ld a, $40
  ld (M3R_RECORD), a
_m3r_row_record:
  xor a
  ld (M3R_OFFSET1), a
  ld (M3R_OFFSET2), a
  ld hl, (M3R_Y)
  ld a, (M3R_RECORD)
  or a
  jr z, _m3r_row_vertical_ready
  ld a, (M3G_RECORD+$40+50)
  or a
  jr z, _m3r_row_vertical_ready
  ld a, (M3R_ROW)
  add a, a
  add a, a
  add a, a
  ld b, a
  ld a, (M3R_FINE_Y)
  ld c, a
  ld a, b
  sub c
  ld b, a
  ld a, (M3G_SPLIT)
  ld c, a
  ld a, b
  sub c
  call _m3r_hud_vertical
  ld a, c
  ld (M3R_OFFSET1), a
_m3r_row_vertical_ready:
  push hl
  ld a, (M3R_RECORD)
  add a, 12
  ld l, a
  ld h, $99
  ld c, (hl)
  inc hl
  ld b, (hl)
  pop hl
  ld a, l
  and $e0
  ld l, a
  ld a, c
  and $1f
  or l
  ld l, a
  ld a, h
  and $0b
  ld h, a
  ld a, b
  and 4
  or h
  ld h, a
  ld (M3R_V), hl
  call _m3r_row_addresses
  ei
  nop
  di
  xor a
  ld (M3R_COL), a
_m3r_cell:
  xor a
  ld (M3R_CUT), a
  ld (M3R_SECOND), a
_m3r_cell_source:
  ld a, (M3R_SECOND)
  or a
  jr nz, _m3r_secondary_tile
_m3r_primary_tile:
  ld hl, (M3R_V)
  ld a, (M3R_ROW_NT_HIGH)
  ld h, a
  ld c, (hl)
  jr _m3r_tile_ready
_m3r_secondary_tile:
  ; A secondary source may have different record/mirroring/vertical origin.
  ; Keep the ordinary calculation, without modifying the primary row cache.
  ei
  nop
  di
  ld hl, (M3R_V)
  call _m3r_nt_read
  ld c, a
_m3r_tile_ready:
  ld a, (M3R_RECORD)
  add a, 8
  ld l, a
  ld h, $99
  ld a, (hl)
  and $10
  rrca
  rrca
  ld b, a                  ; 1 KiB map group0 or4 from BG table selection
  ld a, c
  rrca
  rrca
  rrca
  rrca
  rrca
  rrca
  and 3
  or b
  call _m3r_physical_key     ; C=logicaltile, A=map index
  ld (M3R_KEY), hl
  ld a, (M3R_SECOND)
  or a
  jr nz, _m3r_secondary_attribute
_m3r_primary_attribute:
  ld a, (M3R_V)
  ld b, a
  and 2
  ld c, a
  ld a, (M3R_ROW_QUADRANT)
  or c
  ld c, a
  ld a, b
  rrca
  rrca
  and 7
  ld b, a
  ld a, (M3R_ROW_ATTR_LOW)
  or b
  ld l, a
  ld a, (M3R_ROW_ATTR_HIGH)
  ld h, a
  ld a, (hl)
  jr _m3r_attribute_ready
_m3r_secondary_attribute:
  ld hl, (M3R_V)
  ld a, l
  and $42
  ld c, a
  ld a, h
  and $0c
  or $23
  ld b, a
  ld a, h
  and 3
  rlca
  rlca
  rlca
  rlca
  ld d, a
  ld a, l
  rrca
  rrca
  rrca
  rrca
  and 8
  or d
  ld d, a
  ld a, l
  rrca
  rrca
  and 7
  or d
  or $c0
  ld l, a
  ld h, b
  push bc
  call _m3r_nt_read
  pop bc
_m3r_attribute_ready:
  bit 6, c
  jr z, _m3r_attr_bottom_done
  rrca
  rrca
  rrca
  rrca
_m3r_attr_bottom_done:
  bit 1, c
  jr z, _m3r_attr_done
  rrca
  rrca
_m3r_attr_done:
  and 3
  ld c, a
  ld a, (M3R_SECOND)
  or a
  ld a, c
  jp nz, _m3r_second_source_done
  ld (M3R_PAL), a
  ; A reloaded HUD can start at a different fine Y from the SMS tile grid.
  ; Each shifted cell joins the tail/head of two physical source patterns.
  ld a, (M3R_RECORD)
  or a
  jr z, _m3r_split_cell
  ld a, (M3R_OFFSET1)
  or a
  jp z, _m3r_bg_ready
  ld b, a
  ld (M3R_OFFSET2), a
  ld a, 8
  sub b
  ld (M3R_CUT), a
  call _m3r_push_source
  ld hl, (M3R_V)
  ld a, h
  or $70
  ld h, a
  call rt_mmc3_vertical_increment
  jp _m3r_second_v_ready
_m3r_split_cell:
  ; A split can fall inside a source pattern when fineY is nonzero.
  ; Compose precisely those rows; changing the whole cell would tear one
  ; scanline at C0/fineY7 and seven scanlines at C1/fineY0.
  ld a, (M3G_SPLIT)
  or a
  jp z, _m3r_bg_ready
  ld b, a
  ld a, (M3R_ROW)
  add a, a
  add a, a
  add a, a
  ld c, a
  ld a, (M3R_FINE_Y)
  ld e, a
  ld a, c
  sub e
  jp c, _m3r_bg_ready
  ld c, a
  ld a, b
  sub c
  jp c, _m3r_bg_ready
  or a
  jp z, _m3r_bg_ready
  cp 8
  jp nc, _m3r_bg_ready
  ld (M3R_CUT), a
  call _m3r_push_source
  ld a, $40
  ld (M3R_RECORD), a
  ld hl, (M3R_V)
  ld a, (M3G_RECORD+$40+50)
  or a
  jr z, _m3r_second_origin_ready
  xor a
  call _m3r_hud_vertical
  ld a, (M3R_CUT)
  ld b, a
  ld a, c
  cp b
  jp c, _m3r_second_offset_ok
  jp z, _m3r_second_offset_ok
  ; Three source patterns inside one SMS cell exceed this bounded adapter.
  jp rt_mmc3_raster_unsupported
_m3r_second_offset_ok:
  sub b
  and 7
  ld (M3R_OFFSET2), a
_m3r_second_origin_ready:
  ld a, (M3G_RECORD+$40+13)
  and 4
  ld b, a
  ld a, (M3G_RECORD+$40+12)
  and $1f
  ld c, a
  ld a, (M3R_COL)
  add a, c
  cp 32
  jr c, _m3r_second_x
  sub 32
  ld c, a
  ld a, b
  xor 4
  ld b, a
  ld a, c
_m3r_second_x:
  ld c, a
  ld a, l
  and $e0
  or c
  ld l, a
  ld a, h
  and $0b
  or b
  ld h, a
_m3r_second_v_ready:
  ld (M3R_V), hl
  ld a, 1
  ld (M3R_SECOND), a
  jp _m3r_cell_source
_m3r_push_source:
  ; Preserve the helper return below four source words on the native stack.
  pop bc
  ld hl, (M3R_KEY)
  push hl
  ld a, (M3R_PAL)
  push af
  ld hl, (M3R_V)
  push hl
  ld a, (M3R_RECORD)
  push af
  push bc
  ret
_m3r_second_source_done:
  ld (M3R_PAL2), a
  ld hl, (M3R_KEY)
  ld (M3R_KEY2), hl
  pop af
  ld (M3R_RECORD), a
  pop hl
  ld (M3R_V), hl
  pop af
  ld (M3R_PAL), a
  pop hl
  ld (M3R_KEY), hl
_m3r_bg_ready:
  ; A full source-cell/loopy calculation can precede this lookup. Admit
  ; committed display service before adding any hash traversal latency.
  ei
  nop
  di
  call _m3r_bg_slot
  jp c, _m3r_restart_reserve
  ld hl, (M3R_NT)
  ld (hl), a
  inc hl
  ld (hl), 0
  inc hl
  ld a, l
  and 63
  jr nz, _m3r_nt_ring_ready
  ld de, -64               ; wrap inside this row, never into the next row
  add hl, de
_m3r_nt_ring_ready:
  ld (M3R_NT), hl
  ; Host service is admitted only after both VRAM ports and mapper access
  ; are complete. A nested host VINT never reenters guest/render production.
  ei
  nop
  di
  ld hl, (M3R_V)
  ld a, l
  and $1f
  cp 31
  jr nz, _m3r_next_x
  ld a, l
  and $e0
  ld l, a
  ld a, h
  xor 4
  ld h, a
  ld (M3R_V), hl
  call _m3r_row_addresses
  ei
  nop
  di
  jr _m3r_x_advanced
_m3r_next_x:
  inc l
_m3r_x_done:
  ld (M3R_V), hl
_m3r_x_advanced:
  ld a, (M3R_COL)
  inc a
  ld (M3R_COL), a
  cp 32
  jp c, _m3r_cell
  ld hl, (M3R_Y)
  ld a, h
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
  jr z, _m3r_y_toggle
  cp 31
  jr z, _m3r_y_zero
  ld de, 32
  add hl, de
  jr _m3r_y_done
_m3r_y_toggle:
  ld a, h
  xor 8
  ld h, a
_m3r_y_zero:
  ld a, h
  and $0c
  ld h, a
  ld a, l
  and $1f
  ld l, a
_m3r_y_done:
  ld (M3R_Y), hl
  ld a, (M3R_FINE_Y)
  or a
  ld b, 28
  jr z, _m3r_row_limit
  inc b
_m3r_row_limit:
  ld a, (M3R_ROW)
  inc a
  ld (M3R_ROW), a
  cp b
  jp c, _m3r_row
  ld a, (M3P_PASS)
  cp 1
  jr nz, _m3r_bg_done
  xor a
  ld (M3P_PASS), a
  jp _m3r_pass
_m3r_restart_reserve:
  ; Fast-pass allocations modified ONLY old-nonlive slots. Preserve their
  ; exact hash/count, staged payload and DIRTY bits: the reserve pass may
  ; reuse these keys without reconverting them. Clear only current-needed.
  ; No helper/native call frame is open here; restart the ordinary geometry.
  ld hl, M3P_NEEDED
  ld bc, 32
  xor a
  call rt_mmc3_fill_closed
  ld a, 1
  ld (M3P_PASS), a
  jp _m3r_pass
_m3r_bg_done:
  ; Both raster segments use the PLAYFIELD coarse-X destination rotation.
  ; Their own fine-X remains independent, as do all source loopy addresses.
  ld a, (M3G_RECORD+12)
  and 31
  add a, a
  add a, a
  add a, a
  ld b, a
  ld a, (M3G_RECORD+14)
  add a, b
  neg
  ld (M3P_X), a
  ld a, (M3G_RECORD+$40+14)
  add a, b
  neg
  ld (M3P_HUD_X), a
  call _m3r_sprites
  xor a
  ld (M3R_RECORD), a
  ld de, M3P_CRAM
  call _m3r_palette
  ld a, $40
  ld (M3R_RECORD), a
  ld de, M3P_HUD_CRAM
  call _m3r_palette
  ld a, (M3G_SPLIT)
  ld (M3P_SPLIT), a
  ld a, (M3G_RECORD+8)
  and $20
  ld a, $f0
  jr z, _m3r_reg1
  or 2
_m3r_reg1:
  ld (M3P_REG1), a
  ld a, (M3G_RECORD+9)
  bit 1, a
  ld a, $26
  jr z, _m3r_reg0
  ld a, $06
_m3r_reg0:
  ld (M3P_REG0), a
  jp rt_mmc3_commit_display

; Four primary-row address bytes occupy previously unused renderer-owned
; C85B/C85C/C8EE/C8EF. Set at every row and horizontal nametable wrap,
; including each reserve restart; never valid across rows/frames by inference.
; Source is frozen RECORD/V, not live PPU or mapper state. Secondary reads
; bypass this cache and restore RECORD/V without changing these four bytes.
; AF/BC/DE/HL scratch; no open stack frame or retained general registers.
; Host-only IRQ/Pause preserves these bytes and exact SRAM mapping while BUSY2.
_m3r_row_addresses:
  ld hl, (M3R_V)
  call _m3r_nt_read
  ld a, h
  ld (M3R_ROW_NT_HIGH), a
  ld hl, (M3R_V)
  ld a, l
  and $40
  ld (M3R_ROW_QUADRANT), a
  ld a, h
  and $0c
  or $23
  ld b, a
  ld a, h
  and 3
  rlca
  rlca
  rlca
  rlca
  ld d, a
  ld a, l
  rrca
  rrca
  rrca
  rrca
  and 8
  or d
  or $c0
  ld l, a
  ld h, b
  call _m3r_nt_read
  ld a, h
  ld (M3R_ROW_ATTR_HIGH), a
  ld a, l
  ld (M3R_ROW_ATTR_LOW), a
  ret

; Read a frozen CIRAM source selected by M3R_RECORD. HL=NES nametable addr.
_m3r_nt_read:
  ld a, (M3R_RECORD)
  push hl
  add a, 15
  ld l, a
  ld h, $99
  ld a, (hl)
  pop hl
  or a
  ld a, h
  jr nz, _m3r_nt_horizontal
  and 7
  jr _m3r_nt_page
_m3r_nt_horizontal:
  and 3
  ld b, a
  ld a, h
  and 8
  rrca
  or b
_m3r_nt_page:
  ld b, a
  ld a, (M3R_RECORD)
  or a
  ld a, $88
  jr z, _m3r_nt_base
  ld a, $90
_m3r_nt_base:
  add a, b
  ld h, a
  ld a, (hl)
  ret

; C=logical tile number, A=resolved-map index. Return HL=physical tile ID.
_m3r_physical_key:
  ld b, a
  ld a, (M3R_RECORD)
  add a, b
  ld l, a
  ld h, $99
  ld a, (hl)
  ld b, a
  rrca
  rrca
  and $3f
  ld h, a
  ld a, b
  and 3
  rrca
  rrca
  ld l, a
  ld a, c
  and $3f
  or l
  ld l, a
  ret

; A=visible pixels since the explicit IRQ reload. The captured v supplies
; only vertical origin; final t supplies horizontal reload on each row.
; Return HL coarse vertical v, C fine row. Maximum route delta is31.
_m3r_hud_vertical:
  ld b, a
  ld a, (M3G_RECORD+$40+48)
  ld l, a
  ld a, (M3G_RECORD+$40+49)
  ld h, a
  ld a, b
  or a
  jr z, _m3r_hud_vertical_done
_m3r_hud_vertical_step:
  push bc
  call rt_mmc3_vertical_increment
  pop bc
  ei
  nop
  di
  djnz _m3r_hud_vertical_step
_m3r_hud_vertical_done:
  ld a, h
  and $70
  rrca
  rrca
  rrca
  rrca
  ld c, a
  ld a, h
  and $0f
  ld h, a
  ret

; Exact physical-source/palette/row-offset hash chains. Every slot0..255 is usable;
; FFFF is the sentinel, distinct from valid slot00FF.
_m3r_bg_slot:
  call _m3r_find_slot
  jr c, _m3r_missing
  jp rt_mmc3_reserve_slot
_m3r_missing:
  ld a, (M3P_PASS)
  cp 1
  ld a, 0
  ret z
  jp _m3r_allocate
_m3r_find_slot:
  ld hl, (M3R_KEY)
  ld a, (M3R_PAL)
  xor l
  xor h
  ld (M3R_HASH), a
  ld l, a
  ld h, 0
  add hl, hl
  ld de, $ac00
  add hl, de
_m3r_lookup:
  ld c, (hl)
  inc hl
  ld a, (hl)
  cp $ff
  jr nz, _m3r_candidate
  scf
  ret
_m3r_candidate:
  ld l, c
  ld h, $a8
  ld a, (M3R_KEY)
  cp (hl)
  jr nz, _m3r_chain
  inc h
  ld a, (M3R_KEY+1)
  cp (hl)
  jr nz, _m3r_chain
  inc h
  ld a, (M3R_PAL)
  cp (hl)
  jr nz, _m3r_chain
  ld h, $b4
  ld a, (M3R_OFFSET1)
  cp (hl)
  jr nz, _m3r_chain
  ld h, $b3
  ld a, (M3R_CUT)
  cp (hl)
  jr nz, _m3r_chain
  or a
  ld a, c
  jr z, _m3r_found
  ld h, $b0
  ld a, (M3R_KEY2)
  cp (hl)
  jr nz, _m3r_chain
  inc h
  ld a, (M3R_KEY2+1)
  cp (hl)
  jr nz, _m3r_chain
  inc h
  ld a, (M3R_PAL2)
  cp (hl)
  jr nz, _m3r_chain
  ld h, $b5
  ld a, (M3R_OFFSET2)
  cp (hl)
  ld a, c
  jr nz, _m3r_chain
_m3r_found:
  or a                      ; clear carry: slotFF is a valid successful hit
  ret
_m3r_chain:
  ld l, c
  ld h, 0
  add hl, hl
  ld de, $ae00
  add hl, de
  ; Frozen hash traversal is a closed host-service boundary, even for a
  ; deliberately colliding 256-key chain. Producer/guest remain excluded.
  ei
  nop
  di
  jr _m3r_lookup
_m3r_allocate:
  call rt_mmc3_choose_slot
  ret c
  ld (M3R_SLOT), a
  call rt_mmc3_unlink_slot
  ld a, (M3R_SLOT)
  call rt_mmc3_reserve_slot
  ld l, a
  ld h, $a8
  ld a, (M3R_KEY)
  ld (hl), a
  inc h
  ld a, (M3R_KEY+1)
  ld (hl), a
  inc h
  ld a, (M3R_PAL)
  ld (hl), a
  ld h, $b0
  ld a, (M3R_KEY2)
  ld (hl), a
  inc h
  ld a, (M3R_KEY2+1)
  ld (hl), a
  inc h
  ld a, (M3R_PAL2)
  ld (hl), a
  inc h
  ld a, (M3R_CUT)
  ld (hl), a
  inc h
  ld a, (M3R_OFFSET1)
  ld (hl), a
  inc h
  ld a, (M3R_OFFSET2)
  ld (hl), a
  ld a, (M3R_HASH)
  ld l, a
  ld h, 0
  add hl, hl
  ld de, $ac00
  add hl, de
  ld c, (hl)
  inc hl
  ld b, (hl)
  ld (hl), 0
  dec hl
  ld a, (M3R_SLOT)
  ld (hl), a
  ld l, a
  ld h, 0
  add hl, hl
  ld de, $ae00
  add hl, de
  ld (hl), c
  inc hl
  ld (hl), b
  ld a, (M3R_SLOT)
  ld l, a
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ex de, hl
  ld hl, (M3R_KEY)
  ld a, (M3R_PAL)
  ld b, a
  call _m3r_convert_tile
  ld a, (M3R_SLOT)
  or a
  ret

; HL=physicaltile, DE=SMS VRAM destination, B=NES pal/XYflip bits.
; Bank0 SRAM restored before return; A/BC/DE/HL scratch. Host service occurs
; only at closed SRAM row boundaries; guest producer remains excluded.
_m3r_convert_tile:
  ld a, b
  ld (M3R_ATTR), a
  push de
  ld de, M3G_TILE
  call _m3r_fetch_tile
  ld a, (M3R_CUT)
  or a
  jr z, _m3r_tile_source_ready
  ld hl, (M3R_KEY2)
  ld de, M3G_TILE+16
  call _m3r_fetch_tile
_m3r_tile_source_ready:
  pop de
  call rt_mmc3_pattern_stage
  xor a
  ld (M3R_PIXEL), a
  jr _m3r_pattern_row
_m3r_fetch_tile:
  ld a, h
  rrca
  rrca
  and $0f
  add a, MMC3_CHR_DATA_BASE
  ld ($ffff), a
  xor a
  ld ($fffc), a
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld a, h
  and $3f
  or $80
  ld h, a
  ld bc, 16
  ldir
  ld a, 8
  ld ($fffc), a
  ret
_m3r_pattern_row:
  ; Converted bytes are SRAM-only. Every row is a closed host-service
  ; boundary even while the old committed raster remains visible.
  ei
  nop
  di
  ld a, (M3R_ATTR)
  ld (M3R_ROW_ATTR), a
  ld b, 0
  ld a, (M3R_CUT)
  or a
  jr z, _m3r_pattern_source
  ld c, a
  ld a, (M3R_PIXEL)
  cp c
  jr c, _m3r_pattern_source
  ld b, 16
  ld a, (M3R_PAL2)
  ld (M3R_ROW_ATTR), a
_m3r_pattern_source:
  ld a, (M3R_OFFSET1)
  ld c, a
  ld a, b
  or a
  jr z, _m3r_pattern_offset_ready
  ld a, (M3R_OFFSET2)
  ld c, a
_m3r_pattern_offset_ready:
  ld a, (M3R_PIXEL)
  add a, c
  and 7
  ld c, a
  ld a, (M3R_ROW_ATTR)
  bit 7, a
  ld a, c
  jr z, _m3r_pattern_y
  xor 7
_m3r_pattern_y:
  add a, b
  add a, <M3G_TILE
  ld l, a
  ld h, >M3G_TILE
  ld a, (hl)
  ld (M3R_P0), a
  ld a, l
  add a, 8
  ld l, a
  ld a, (hl)
  ld (M3R_P1), a
  ld a, (M3R_ROW_ATTR)
  bit 6, a
  jr z, _m3r_pattern_x
  ld a, (M3R_P0)
  call _m3r_reverse
  ld (M3R_P0), a
  ld a, (M3R_P1)
  call _m3r_reverse
  ld (M3R_P1), a
_m3r_pattern_x:
  ld a, (M3R_P0)
  ld c, a
  ld (de), a
  inc de
  ld a, (M3R_P1)
  ld (de), a
  inc de
  or c
  ld c, a
  ld a, (M3R_ROW_ATTR)
  and 1
  jr z, _m3r_plane2
  ld a, c
_m3r_plane2:
  ld (de), a
  inc de
  ld a, (M3R_ROW_ATTR)
  and 2
  jr z, _m3r_plane3
  ld a, c
_m3r_plane3:
  ld (de), a
  inc de
  ld a, (M3R_PIXEL)
  inc a
  ld (M3R_PIXEL), a
  cp 8
  jp c, _m3r_pattern_row
  ld a, 8
  ld ($fffc), a
  ret
_m3r_reverse:
  ld b, 8
  ld c, 0
_m3r_reverse_bit:
  rrca
  rl c
  djnz _m3r_reverse_bit
  ld a, c
  ret

; Fixed per-OAM pair slots0..63 use128 patterns at$2000..2FFF, within
; the184-pattern sprite allowance. Source identity still uses physical CHR.
_m3r_sprites:
  xor a
  ld (M3R_CUT), a
  ld (M3R_OFFSET1), a
  ld (M3R_OFFSET2), a
  ld (M3R_OAM), a
  ld (M3R_RECORD), a
_m3r_sprite:
  ld a, (M3R_OAM)
  add a, a
  add a, a
  ld l, a
  ld h, $98
  ld a, (hl)
  ld c, a
  ld a, (M3R_OAM)
  ld e, a
  ld d, $ab
  ld a, c
  cp $df
  jp nc, _m3r_sprite_hide
  ld (de), a               ; both NES/SMS add1 to their stored sprite Y
  inc hl
  ld a, (hl)
  ld (M3R_SPRITE_TILE), a
  inc hl
  ld a, (hl)
  ld (M3R_ATTR), a
  inc hl
  ld c, (hl)
  ld a, (M3R_OAM)
  add a, a
  add a, $40
  ld e, a
  ld a, c
  ld (de), a
  inc de
  ld a, (M3R_OAM)
  add a, a
  ld (de), a
  xor a
  ld (M3R_SPRITE_HALF), a
_m3r_sprite_half:
  ld a, (M3G_RECORD+8)
  bit 5, a
  jr z, _m3r_sprite8
  ld a, (M3R_SPRITE_TILE)
  and 1
  rlca
  rlca
  ld b, a
  ld a, (M3R_SPRITE_TILE)
  and $fe
  ld c, a
  ld a, (M3R_SPRITE_HALF)
  ld e, a
  ld a, (M3R_ATTR)
  bit 7, a
  ld a, e
  jr z, _m3r_pair_y
  xor 1
_m3r_pair_y:
  or c
  ld c, a
  jr _m3r_sprite_key
_m3r_sprite8:
  ld a, (M3G_RECORD+8)
  and 8
  rrca
  ld b, a
  ld a, (M3R_SPRITE_TILE)
  ld c, a
_m3r_sprite_key:
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
  push hl
  ld a, (M3R_OAM)
  ld l, a
  ld h, 0
  add hl, hl
  ld a, (M3R_SPRITE_HALF)
  or l
  ld l, a
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld de, $2000
  add hl, de
  ex de, hl
  pop hl
  ld a, (M3R_ATTR)
  ld b, a
  call rt_mmc3_sprite_changed
  call nz, _m3r_convert_tile
  ld a, (M3G_RECORD+8)
  bit 5, a
  jr z, _m3r_sprite_next
  ld a, (M3R_SPRITE_HALF)
  inc a
  ld (M3R_SPRITE_HALF), a
  cp 2
  jp c, _m3r_sprite_half
  jr _m3r_sprite_next
_m3r_sprite_hide:
  ld a, $e0
  ld (de), a
_m3r_sprite_next:
  ei
  nop
  di
  ld a, (M3R_OAM)
  inc a
  ld (M3R_OAM), a
  cp 64
  jp c, _m3r_sprite
  ret

_m3r_palette:
  ld c, 0
_m3r_palette_color:
  ld a, c
  and 3
  ld a, c
  jr nz, _m3r_palette_source
  xor a                    ; common NES backdrop for every transparent slot
_m3r_palette_source:
  ld b, a
  ld a, (M3R_RECORD)
  add a, 16
  add a, b
  ld l, a
  ld h, $99
  ld a, (hl)
  and $3f
  ld l, a
  ld h, 0
  push de
  ld de, _m3r_colors
  add hl, de
  pop de
  ld a, (hl)
  ld (de), a
  inc de
  inc c
  ; Pending palettes are SRAM/native preparation, not an open CRAM write.
  ; Do not concatenate two 32-color DI loops while old display is serviced.
  ei
  nop
  di
  ld a, c
  cp 32
  jr c, _m3r_palette_color
  ret

rt_mmc3_display_rearm:
  ld a, (M3P_STATE)
  or a
  ret nz
  ld a, (M3G_READY)
  or a
  ret z
  xor a
  call vdp_set_cram_addr
  ld hl, M3R_CRAM
  ld bc, 32
  call vdp_write_block
  ld a, (M3G_PRESENT_X)
  ld b, 8
  call vdp_set_register
  ld a, (M3G_PRESENT_Y)
  ld b, 9
  call vdp_set_register
  ld a, (M3R_SPLIT)
  or a
  jr z, _m3r_display_no_split
  dec a
  ld b, 10
  call vdp_set_register
  ld a, (M3R_REG0)
  or $10
  ld b, 0
  jp vdp_set_register
_m3r_display_no_split:
  ld a, (M3R_REG0)
  ld b, 0
  jp vdp_set_register

rt_mmc3_display_line:
  ld a, (M3P_STATE)
  or a
  ret nz
  ld a, (M3G_READY)
  or a
  ret z
  xor a
  call vdp_set_cram_addr
  ld hl, M3R_HUD_CRAM
  ld bc, 32
  call vdp_write_block
  ld a, (M3G_HUD_X)
  ld b, 8
  call vdp_set_register
  ld a, (M3R_REG0)
  ld b, 0
  jp vdp_set_register

_m3r_colors:
  .db $15,$10,$20,$20,$11,$01,$01,$00,$00,$00,$04,$00,$00,$00,$00,$00
  .db $2A,$34,$30,$31,$22,$12,$02,$01,$05,$04,$04,$04,$14,$00,$00,$00
  .db $3F,$39,$35,$36,$37,$27,$17,$0B,$0A,$0D,$0D,$1C,$38,$00,$00,$00
  .db $3F,$3E,$3A,$3B,$3B,$3B,$2B,$2F,$1F,$1E,$2E,$2E,$3E,$2A,$00,$00
.ends
.endif
