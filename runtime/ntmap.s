; ntmap.s — NES nametable/CIRAM address helpers.
;
; This is a scaffold for the runtime nametable-shadow materializer. It does not
; write a full CIRAM tile shadow yet because chrmap.s still uses $CC00-$D2FF
; as the authoritative folded SMS per-cell subpalette state. The old compact
; $D300-$D3DF duplicate has been retired, and trace diagnostics currently
; reserve $D300-$D3FF only as dirty metadata space: $D300-$D3EF can hold a
; 1920-bit tile-dirty bitmap, and $D3F0-$D3FF can hold a 128-bit attr-dirty
; bitmap. Callers may use the full helper only once the folded-shadow storage
; collision is removed.

.define NT_ATTR_SHADOW $cb80
.define RAW_CIRAM_BYTES $0800

.section "ntmap" free

; Exactly one mirroring mode must be provided by the generated top-level asm.
.ifndef NES_MIRRORING_VERTICAL
.ifndef NES_MIRRORING_HORIZONTAL
.fail "missing NES nametable mirroring define"
.endif
.endif

.ifdef NES_MIRRORING_VERTICAL
.ifdef NES_MIRRORING_HORIZONTAL
.fail "conflicting NES nametable mirroring defines"
.endif
.endif

; Convert a NES PPU nametable address to a mirrored 2 KiB CIRAM shadow pointer.
;
; Entry: DE = NES PPU address $2000-$2FFF.
; Exit:  HL = $CC00 + mirrored CIRAM offset.
; Preserves: DE.
; Clobbers: AF, HL.
;
; Mirroring:
;   vertical:   pages 0,1,0,1 -> raw & $07FF
;   horizontal: pages 0,0,1,1 -> (raw & $03FF) | ((raw & $0800) >> 1)
rt_nt_ppuaddr_to_ciram:
  ld   a, d

.ifdef NES_MIRRORING_VERTICAL
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, $cc
  ld   h, a
  ld   l, e
  ret
.endif

.ifdef NES_MIRRORING_HORIZONTAL
  and  $03                   ; raw low 1 KiB offset high bits
  ld   h, a
  ld   a, d
  and  $08                   ; raw bit 11 selects CIRAM page 1
  srl  a                     ; move bit 3 -> bit 2
  or   h
  add  a, $cc
  ld   h, a
  ld   l, e
  ret
.endif

.ifdef RAW_CIRAM_BACKEND_SRAM

; Convert a NES PPU nametable address to the raw-CIRAM SRAM backend pointer.
;
; Backend: standard Sega mapper SRAM bank 0 in slot 2, reserving
; RAW_CIRAM_SRAM_BASE..RAW_CIRAM_SRAM_BASE+$07FF ($8000-$87FF by default).
;
; Entry: DE = NES PPU address $2000-$2FFF.
; Exit:  HL = RAW_CIRAM_SRAM_BASE + mirrored CIRAM offset.
; Preserves: DE.
; Clobbers: AF, HL.
rt_nt_ppuaddr_to_raw_ciram_sram:
  call rt_nt_ppuaddr_to_ciram   ; HL = $CC00 + mirrored CIRAM offset
  ld   a, h
  sub  $4c                      ; $CC00 -> $8000, preserving 0..$07FF offset
  ld   h, a
  ret

; Enable standard Sega mapper SRAM bank 0 in slot 2 ($8000-$BFFF).
; Preserves: BC, DE, HL. Clobbers: AF.
rt_raw_ciram_sram_enable:
  ld   a, RAW_CIRAM_SRAM_CTRL
  ld   ($fffc), a
  ret

; Restore slot 2 to ROM visibility. The $FFFF bank latch is preserved by the
; mapper, so disabling SRAM reveals whichever slot-2 ROM bank was active.
; Preserves: BC, DE, HL. Clobbers: AF.
rt_raw_ciram_sram_disable:
  xor  a
  ld   ($fffc), a
  ret

; Clear the 2 KiB raw-CIRAM SRAM area. Intended for boot-time initialization;
; must only run from code outside slot 2 because $8000-$BFFF is RAM while
; enabled.
; Preserves: DE. Clobbers: AF, BC, HL.
rt_raw_ciram_sram_clear:
  call rt_raw_ciram_sram_enable
  ld   hl, RAW_CIRAM_SRAM_BASE
  ld   bc, RAW_CIRAM_BYTES
  xor  a
  call mem_fill
  jp   rt_raw_ciram_sram_disable

; Write one raw NES CIRAM byte into the SRAM backend.
; Entry: DE = NES PPU nametable/attribute address, A = byte.
; Preserves: DE. Clobbers: AF, HL.
rt_raw_ciram_sram_write:
  push af
  call rt_nt_ppuaddr_to_raw_ciram_sram
  call rt_raw_ciram_sram_enable
  pop  af
  ld   (hl), a
  jp   rt_raw_ciram_sram_disable

; Read one raw NES CIRAM byte from the SRAM backend.
; Entry: DE = NES PPU nametable/attribute address.
; Exit:  A = byte.
; Preserves: BC, DE, HL. Clobbers: AF.
rt_raw_ciram_sram_read:
  push hl
  call rt_nt_ppuaddr_to_raw_ciram_sram
  call rt_raw_ciram_sram_enable
  ld   a, (hl)
  push af
  call rt_raw_ciram_sram_disable
  pop  af
  pop  hl
  ret

.endif

; Map a NES PPU attribute-table address to the compact mirrored attribute
; shadow. The shadow stores only the 64 attribute bytes for each of the two
; physical CIRAM pages, leaving the existing $CC00 folded SMS subpalette shadow
; untouched for current rendering.
;
; Entry: DE = NES PPU attribute address ($23C0/$27C0/$2BC0/$2FC0 mirrors).
; Exit:  HL = $CB80 + mirrored_attr_index (0..127).
; Preserves: DE.
; Clobbers: AF, HL.
rt_nt_attr_shadow_addr:
  call rt_nt_ppuaddr_to_ciram   ; HL = $CC00 + mirrored CIRAM offset

  ld   a, l
  and  $3f                      ; attribute byte within CIRAM page
  ld   l, a

  ld   a, h
  sub  $cc
  and  $04                      ; mirrored CIRAM page bit
  add  a, a
  add  a, a
  add  a, a
  add  a, a                     ; $04 -> $40
  or   l
  add  a, $80                  ; base low byte of NT_ATTR_SHADOW
  ld   l, a
  ld   h, $cb
  ret

; Write one NES attribute byte into the compact mirrored attribute shadow.
; Entry: DE = NES PPU attribute address, A = attr byte.
; Preserves: DE.
; Clobbers: AF, HL.
rt_nt_write_attr_shadow:
  push af
  call rt_nt_attr_shadow_addr
  pop  af
  ld   (hl), a
  ret

; Derive the NES background subpalette S for a tile from the compact mirrored
; attribute shadow. Helper-only scaffold for the later materializer; current
; rendering paths still use the folded $CC00 SMS subpalette state.
;
; Entry: DE = NES PPU tile address $2000-$2FBF.
; Exit:  A = S (0..3).
; Preserves: DE.
; Clobbers: AF, BC, HL.
rt_nt_attr_s_from_attr_shadow:
  call rt_nt_ppuaddr_to_ciram   ; HL = $CC00 + mirrored tile offset

  ld   a, h
  sub  $cc
  ld   b, a                    ; B = mirrored offset high byte (0..7)
  ld   c, l                    ; C = mirrored offset low byte

  ; HL = $CB80 + page*64 + attr_index.
  ld   a, b
  and  $04
  add  a, a
  add  a, a
  add  a, a
  add  a, a                    ; $04 -> $40
  ld   l, a
  ld   a, c
  and  $80
  rrca
  rrca
  rrca
  rrca                         ; coarse_y attr bit from offset bit 7
  add  a, l
  ld   l, a
  ld   a, b
  and  $03
  add  a, a
  add  a, a
  add  a, a
  add  a, a                    ; offset bits 8..9 -> attr bits 4..5
  add  a, l
  ld   l, a
  ld   a, c
  and  $1c
  srl  a
  srl  a                       ; coarse_x >> 2
  add  a, l
  add  a, $80                  ; base low byte of NT_ATTR_SHADOW
  ld   l, a
  ld   h, $cb

  ; B = shift = ((coarse_y & 2) << 1) | (coarse_x & 2), i.e. 0/2/4/6.
  ld   b, $00
  ld   a, c
  and  $40
  jr   z, _nt_attr_shadow_shift_y_done
  ld   b, $04
_nt_attr_shadow_shift_y_done:
  ld   a, c
  and  $02
  jr   z, _nt_attr_shadow_shift_ready
  inc  b
  inc  b
_nt_attr_shadow_shift_ready:
  ld   a, b
  or   a
  ld   a, (hl)
  jr   z, _nt_attr_shadow_shift_done
_nt_attr_shadow_shift_apply:
  srl  a
  djnz _nt_attr_shadow_shift_apply
_nt_attr_shadow_shift_done:
  and  $03
  ret

.ends

; ─── Window-routed nametable writes (E.5c runtime materializer, tiles) ───────
; The visible SMS table has 32 columns; the NES streams columns for the
; NEXT screen into the second nametable, and folding those writes directly
; (mod 32) overwrote on-screen columns mid-screen (field report: terrain
; drawing at the player's position). Routing rule:
;   - every tile write lands in the raw CIRAM SRAM store,
;   - the folded VRAM write only happens when the write's column lies
;     inside the projected window,
;   - at presentation, columns entering the window are projected from the
;     raw store (rt_nt_project_scroll).
; Window state:
;   $CB2A  projected window start column (0-63, NES coarse-scroll space)
;   $CB2B  projector scratch: column
;   $CB2C  projector scratch: row
; Attributes keep the existing folded path (follow-up; SMB's playfield
; palettes are coarse enough that entering columns look right).

; rt_nt_route_tile_write — raw-store a tile byte and classify visibility.
; Entry: DE = NES PPU tile address ($2000-$2FFF, offset < $3C0), A = byte.
; Exit:  carry SET   -> in-window: caller performs the folded VRAM write.
;        carry CLEAR -> out-of-window: raw-store only, caller skips VRAM.
; Preserves: DE. Clobbers: AF, HL, BC.
rt_nt_route_tile_write:
  call rt_raw_ciram_sram_write
  ; row = ((D & 3) << 3) | (E >> 5); rows 0-3 (status region) always render.
  ld   a, e
  rlca
  rlca
  rlca
  and  $07
  ld   b, a
  ld   a, d
  and  $03
  rlca
  rlca
  rlca
  or   b
  cp   4
  jr   nc, _nrt_col_check
.ifdef NES_CHR_RAM
  ; Rows 0-3 are the status-bar band. Write-time page gating is
  ; unsound (PPUCTRL's select at write time need not match display
  ; time — CV1 flips modes during column uploads). Band writes are
  ; RAW-ONLY; the presentation re-materializes rows 0-3 from the
  ; SELECTED page's raw CIRAM whenever the band is dirty ($CB78).
  ld   a, $01
  ld   ($cb78), a
  or   a                    ; carry clear: raw only
  ret
.else
  ; SMB-proven band rule: NT-A rows 0-3 render (fixed HUD); NT-B
  ; column tops raw-store only.
  ld   a, d
  and  $04
  jr   z, _nrt_in
  or   a
  ret
.endif
_nrt_col_check:
  ; column = (D bit2) * 32 | (E & $1F)   (vertical mirroring: $24xx = page 1)
  ld   a, d
  and  $04
  rlca
  rlca
  rlca                      ; bit 2 -> bit 5 (= 32)
  ld   b, a
  ld   a, e
  and  $1f
  or   b
  ld   hl, $cb2a
  sub  (hl)
  and  $3f
  cp   32
  jr   c, _nrt_in
  or   a                    ; carry clear: outside the window
  ret
_nrt_in:
  scf
  ret

; rt_nt_project_scroll — project columns entering the visible window.
; Entry: C = playfield scroll X about to be presented (the same value the
;        scroll apply will write, pre-negation). Reads PPUCTRL bit 0 for
;        the nametable select. Called from the frame IRQ during VBlank.
; Clobbers: AF, BC, DE, HL.
rt_nt_project_scroll:
  ld   a, ($cb08)
  and  $01
  rrca
  rrca
  rrca                      ; bit 0 -> bit 5 (= 32)
  ld   b, a
  ld   a, c
  rrca
  rrca
  rrca
  and  $1f
  or   b                    ; new window start column (0-63)
  ld   hl, $cb2a
  ld   b, (hl)              ; B = previous start
  ld   (hl), a
  sub  b
  and  $3f
  ret  z
  cp   5
  jr   nc, _nps_teleport    ; page flip/teleport: window content is raw-only
  ld   d, a                 ; D = entering-column count (1-4)
  ld   a, b
  add  a, 32
  and  $3f                  ; first entering column
_nps_loop:
  push af
  push de
  call _nt_project_col
  pop  de
  pop  af
  inc  a
  and  $3f
  dec  d
  jr   nz, _nps_loop
  ret
_nps_teleport:
  ; The window moved by 5+ columns at once (PPUCTRL page flip or a
  ; teleport). The screen the game drew there went through RAW stores
  ; only (out-of-window at write time) — materialize the ENTIRE window
  ; from raw CIRAM, including the rows 0-3 band. ~896 mapped writes:
  ; fine with rendering off (page flips happen behind disabled video);
  ; a one-frame overrun otherwise.
  ld   a, ($cb2a)           ; new window start
  ld   d, 32
_npt_loop:
  push af
  push de
  call _nt_project_col_all
  pop  de
  pop  af
  inc  a
  and  $3f
  dec  d
  jr   nz, _npt_loop
  ret

; rt_nt_materialize_band — project rows 0-3, columns 0-31 of the
; PPUCTRL-SELECTED page from raw CIRAM into the folded band (called
; from the presentation when $CB78 is set; the band shows scroll 0).
; Clobbers: AF, BC, DE, HL.
rt_nt_materialize_band:
  xor  a
  ld   ($cb78), a
  ld   a, ($cb08)
  and  $01
  rrca
  rrca
  rrca                      ; select bit 0 -> bit 5 (= column 32)
  ld   d, 32
_nmb_loop:
  push af
  push de
  call _nt_project_col_band
  pop  de
  pop  af
  inc  a
  dec  d
  jr   nz, _nmb_loop
  ret

; _nt_project_col — copy rows 4-27 of one column from raw CIRAM into the
; folded VRAM window through the CHR mapper (keeps shadows coherent).
; _nt_project_col_all — same, rows 0-27 (page-flip materialization).
; _nt_project_col_band — rows 0-3 only (band re-materialization).
; Entry: A = column (0-63). Clobbers: AF, BC, DE, HL. End row in $CB79.
_nt_project_col_band:
  ld   ($cb2b), a
  xor  a
  ld   ($cb2c), a
  ld   a, 4
  ld   ($cb79), a
  jr   _npc_row
_nt_project_col_all:
  ld   ($cb2b), a
  xor  a
  ld   ($cb2c), a
  ld   a, 28
  ld   ($cb79), a
  jr   _npc_row
_nt_project_col:
  ld   ($cb2b), a
  ld   a, 4
  ld   ($cb2c), a
  ld   a, 28
  ld   ($cb79), a
_npc_row:
  ; DE = NES tile address for (row, col)
  ld   a, ($cb2b)
  and  $1f
  ld   e, a
  ld   a, ($cb2c)
  and  $07
  rrca
  rrca
  rrca                      ; (row & 7) << 5
  or   e
  ld   e, a
  ld   a, ($cb2c)
  and  $18
  rrca
  rrca
  rrca                      ; row >> 3
  ld   d, a
  ld   a, ($cb2b)
  and  $20
  rrca
  rrca
  rrca                      ; column bit 5 -> address bit 10 ($04 in D)
  or   d
  or   $20
  ld   d, a
  call rt_raw_ciram_sram_read   ; A = raw tile (preserves DE)
  push af
  ; fold to the SMS table: $3700 + ((DE - $2000) & $3FF) * 2
  ld   a, d
  and  $03
  ld   d, a
  sla  e
  rl   d
  ld   a, d
  add  a, $37
  ld   d, a
  ld   a, e
  out  ($bf), a
  ld   a, d
  and  $3f
  or   $40
  out  ($bf), a
  pop  af
  call rt_write_mapped_bg_tile
  ld   a, ($cb2c)
  inc  a
  ld   ($cb2c), a
  ld   hl, $cb79
  cp   (hl)
  jr   c, _npc_row

  ; Re-apply the column's attributes from the raw attr shadow: the tile
  ; writes above resolved their palette variants against the folded attr
  ; state of the OLD column occupying these fold slots. Feeding the 8
  ; governing attribute bytes through rt_apply_attr_byte re-resolves the
  ; 4x4 groups with the correct sub-palettes (neighbor columns are
  ; re-resolved too, harmlessly — their state is already correct).
  xor  a
  ld   ($cb2c), a            ; attr group row 0..7
_npc_attr:
  ; DE = NES attr address $23C0 | page<<10 | gy*8 | groupx
  ld   a, ($cb2b)
  and  $1f
  rrca
  rrca                       ; (col&31)>>2 = groupx (col<32 so 2 rrca ok on 5-bit)
  and  $07
  ld   e, a
  ld   a, ($cb2c)
  rlca
  rlca
  rlca                       ; gy*8 (gy<8: 3-bit <<3)
  or   e
  or   $c0
  ld   e, a
  ld   a, ($cb2b)
  and  $20
  rrca
  rrca
  rrca                       ; page -> $04
  or   $23
  ld   d, a
  ; A = raw attr byte from the compact shadow ($CB80 + page*64 + off)
  push de
  call rt_nt_attr_shadow_addr
  ld   a, (hl)
  pop  de
  call rt_apply_attr_byte
  ld   a, ($cb2c)
  inc  a
  ld   ($cb2c), a
  cp   8
  jr   c, _npc_attr
  ret
