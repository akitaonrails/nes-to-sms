; ntmap.s — NES nametable/CIRAM address helpers.
;
; This is a scaffold for the runtime nametable-shadow materializer. It does not
; write a full CIRAM tile shadow yet because chrmap.s still uses $CC00-$D2FF
; as the authoritative folded SMS per-cell subpalette state. The old compact
; $D300-$D3DF duplicate has been retired. $D300-$D3FB and $D500-$D5FF now
; belong to translated-call continuation frames, and $D3FC-$D3FE to PPU write
; continuations, so callers may use the full helper only once replacement
; folded-shadow storage is assigned.

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

; Enable standard Sega mapper SRAM bank 0 in slot 2 ($8000-$BFFF). Valid only
; during boot reset DI, an outer PPU guard (depth 1/2), or asserted presentation.
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

; Clear the 2 KiB raw-CIRAM SRAM area. Boot reset DI only;
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

; Write one raw NES CIRAM byte into the SRAM backend (outer PPU guard only).
; Entry: DE = NES PPU nametable/attribute address, A = byte.
; Preserves: DE. Clobbers: AF, BC, HL.
rt_raw_ciram_sram_write:
  ld   c, a

.ifdef NES_MIRRORING_VERTICAL
  ld   a, d
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

.ifdef NES_MIRRORING_HORIZONTAL
  ld   a, d
  and  $03                   ; raw low 1 KiB offset high bits
  ld   h, a
  ld   a, d
  and  $08                   ; raw bit 11 selects CIRAM page 1
  srl  a                     ; move bit 3 -> bit 2
  or   h
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

  ld   a, RAW_CIRAM_SRAM_CTRL
  ld   ($fffc), a
  ld   (hl), c
  xor  a
  ld   ($fffc), a
  ret

; Locked materializer read: presentation DI/depth0 after boot.s's boundary
; assertion only. Stackless; not a public guarded API.
; Entry: DE = NES PPU nametable/attribute address.
; Exit:  A = byte.
; Preserves: DE. Clobbers: AF, BC, HL.
rt_raw_ciram_sram_read_locked:
  ; Inline mirroring-aware CIRAM -> slot-2 SRAM pointer math.
.ifdef NES_MIRRORING_VERTICAL
  ld   a, d
  and  $07
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif
.ifdef NES_MIRRORING_HORIZONTAL
  ld   a, d
  and  $03
  ld   h, a
  ld   a, d
  and  $08
  srl  a
  or   h
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif
  ld   a, RAW_CIRAM_SRAM_CTRL
  ld   ($fffc), a
  ld   a, (hl)
  ld   c, a
  xor  a
  ld   ($fffc), a
  ld   a, c
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
  ; Inline rt_nt_ppuaddr_to_ciram. Attribute updates can run from the nested
  ; frame path where every extra call frame matters.
  ld   a, d

.ifdef NES_MIRRORING_VERTICAL
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, $cc
  ld   h, a
  ld   l, e
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
.endif

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
; Clobbers: AF, BC, HL.
rt_nt_write_attr_shadow:
  ; Inline rt_nt_attr_shadow_addr. This helper is reached from the nested
  ; translated-NMI upload path; saving A with push/pop plus the helper call can
  ; cross the native stack guard. BC is caller-dead on both runtime call sites.
  ld   c, a
  ld   a, d

.ifdef NES_MIRRORING_VERTICAL
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, $cc
  ld   h, a
  ld   l, e
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
.endif

  ld   a, l
  and  $3f                   ; attribute byte within CIRAM page
  ld   l, a

  ld   a, h
  sub  $cc
  and  $04                   ; mirrored CIRAM page bit
  add  a, a
  add  a, a
  add  a, a
  add  a, a                  ; $04 -> $40
  or   l
  add  a, $80                ; base low byte of NT_ATTR_SHADOW
  ld   l, a
  ld   h, $cb
  ld   (hl), c
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
  ; Inline rt_raw_ciram_sram_write in the hottest $2007 tile path. The helper
  ; call frame alone can cross the native stack guard during nested frame work.
  ld   c, a

.ifdef NES_MIRRORING_VERTICAL
  ld   a, d
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

.ifdef NES_MIRRORING_HORIZONTAL
  ld   a, d
  and  $03                   ; raw low 1 KiB offset high bits
  ld   h, a
  ld   a, d
  and  $08                   ; raw bit 11 selects CIRAM page 1
  srl  a                     ; move bit 3 -> bit 2
  or   h
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

  ld   a, RAW_CIRAM_SRAM_CTRL
  ld   ($fffc), a
  ld   (hl), c
  xor  a
  ld   ($fffc), a
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
  ; Rows 0-3 are the fixed status band. Keep tile routing consistent with the
  ; attribute path and VDP top-row scroll lock: NT-A writes render, while NT-B
  ; writes remain raw-only for the scrolling playfield. The live PPUCTRL page
  ; during VBlank describes the next scroll latch, not necessarily the page
  ; currently visible in this fixed band.
  ld   a, d
  and  $04
  jr   z, _nrt_in
  or   a                    ; carry clear: NT-B -> raw only
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
rt_nt_materialize_window:
  ; Also called by the deferred FC flush (boot.s): after a BG-table
  ; switch invalidates the variant cache, a STATIC screen never
  ; rewrites its cells, so nothing would regenerate them — re-project
  ; the whole window so every visible cell picks up variants from the
  ; now-current pattern table.
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
  ; The playfield window may start in NT-B, but the locked status band is
  ; always sourced from NT-A. Restore it last so a full-window projection
  ; cannot leave page-B rows 0-3 folded over the HUD.
  jp   rt_nt_materialize_band

; rt_nt_materialize_band — project rows 0-3, columns 0-31 of NT-A from raw
; CIRAM into the folded band (called
; from the presentation when $CB78 is set; the band shows scroll 0).
; Clobbers: AF, BC, DE, HL.
rt_nt_materialize_band:
  xor  a
  ld   ($cb78), a
  ; Before a game establishes a split-scroll pair, the top rows belong to the
  ; same presented page as the playfield (title/transition screens included).
  ; Once a post-split pair exists, keep the fixed status band on NT-A. Use the
  ; presented-window latch rather than live PPUCTRL, which games toggle during
  ; VBlank uploads.
  ld   a, ($cb20)
  bit  2, a
  jr   nz, _nmb_fixed_page
  ld   a, ($cb2a)
  and  $20
  jr   _nmb_page_ready
_nmb_fixed_page:
  xor  a
_nmb_page_ready:
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
  call rt_raw_ciram_sram_read_locked ; presentation-locked, stackless read
  ld   ($cb13), a               ; park raw tile without spending native stack
.ifdef NES_CHR_RAM
  ; CHR-RAM only: resolve this cell's sub-palette S from the raw attribute
  ; shadow now, while DE still holds the NES tile address; it is staged into
  ; the folded $CCxx shadow below (after the fold) so the single tile write
  ; picks the correct variant, replacing the removed second attribute pass
  ; (see loop end). CHR-ROM (SMB) keeps the two-pass path: its authoritative
  ; sub-palette state flows differently and the raw attr shadow read here is
  ; not equivalent for it (verified: SMB bonus-pipe VDP parity regressed).
  call rt_nt_attr_s_from_attr_shadow ; A = S (0..3); preserves DE, clobbers BC/HL
  ld   c, a                          ; park S across the fold + VDP address set
.endif
.ifdef PROFILE_TOP_TILE_REMAP_ROWS
  ; Profile-owned, display-only cleanup for transition-fill tiles in a fixed
  ; top band. Never modify raw CIRAM: later projections must retain the game's
  ; actual writes and can apply a different presentation policy.
  ld   a, ($cb2c)
  cp   PROFILE_TOP_TILE_REMAP_ROWS
  jr   nc, _npc_top_remap_done
  ld   a, ($cb13)
.ifdef PROFILE_TOP_TILE_REMAP_FROM_0
  cp   PROFILE_TOP_TILE_REMAP_FROM_0
  jr   z, _npc_top_remap_replace
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_1
  cp   PROFILE_TOP_TILE_REMAP_FROM_1
  jr   z, _npc_top_remap_replace
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_2
  cp   PROFILE_TOP_TILE_REMAP_FROM_2
  jr   z, _npc_top_remap_replace
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_3
  cp   PROFILE_TOP_TILE_REMAP_FROM_3
  jr   z, _npc_top_remap_replace
.endif
  jr   _npc_top_remap_done
_npc_top_remap_replace:
  ld   a, PROFILE_TOP_TILE_REMAP_TO
  ld   ($cb13), a
_npc_top_remap_done:
.endif
  ; fold to the SMS table: $3700 + ((DE - $2000) & $3FF) * 2
  ld   a, d
  and  $03
  ld   d, a
  sla  e
  rl   d
  ld   a, d
  add  a, $37
  ld   d, a
.ifdef NES_CHR_RAM
  ; Stage the resolved S (parked in C) into the folded $CCxx shadow for this
  ; cell ($37xx -> $CCxx: high byte + $95) so rt_write_mapped_bg_tile reads the
  ; correct sub-palette. Preserves D/E for the VDP address + tile write.
  ld   a, d
  add  a, $95
  ld   h, a
  ld   l, e
  ld   (hl), c
.endif
  ld   a, e
  out  ($bf), a
  ld   a, d
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, ($cb13)
  call rt_write_mapped_bg_tile
  ld   a, ($cb2c)
  inc  a
  ld   ($cb2c), a
  ld   hl, $cb79
  cp   (hl)
  jr   c, _npc_row
.ifdef NES_CHR_RAM
  ; CHR-RAM: no second attribute pass. _npc_row staged each cell's correct S
  ; into the folded $CCxx shadow (from the raw attribute shadow) before its
  ; single tile write, so every projected cell already resolved the right
  ; variant. The former _npc_attr / rt_apply_attr_byte pass re-resolved all
  ; 4x4 groups (neighbour columns included) and cost ~11% of frame time while
  ; scrolling; it also left window-gate-skipped scroll-edge cells with stale
  ; sub-palettes (the "left part not refreshed" fragments). In-place attribute
  ; changes to already-visible cells are still handled by the direct $2007
  ; attribute path (ppu.s rt_apply_attr_byte).
  ret
.else

  ; CHR-ROM (SMB): re-apply the column's attributes from the raw attr shadow.
  ; The tile writes above resolved their palette variants against the folded
  ; attr state of the OLD column occupying these fold slots. Feeding the 8
  ; governing attribute bytes through rt_apply_attr_byte re-resolves the 4x4
  ; groups with the correct sub-palettes (neighbor columns are re-resolved too,
  ; harmlessly — their state is already correct).
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
.endif
