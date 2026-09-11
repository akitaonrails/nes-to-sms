.ifndef CNROM_SOURCE_HARDWARE_EXPERIMENT
.ifndef MMC3_FULL_RUNTIME
; sat.s — Sprite Attribute Table (SAT) staging/upload, with software sprite
; variants for NES palette selection and horizontal/vertical flipping.
;
; SMS SAT layout in VRAM at $3F00:
;   $3F00-$3F3F  64 Y positions (1 byte each)
;   $3F40-$3F7F  unused gap
;   $3F80-$3FFF  64 (X, tile_number) pairs (2 bytes each) = 128 bytes
;
; The staging area in SMS RAM mirrors the NES OAM layout (64 x 4 = 256 bytes
; at $C900-$C9FF as written by SMB via $2004 / $4014 DMA):
;   Each entry is 4 bytes: [Y, tile, attr, X]
;   Byte 2 (attr): bit 5 = behind-background priority, bit 6 = H-flip,
;   bit 7 = V-flip, bits 1..0 = OAM palette.
;
; A sprite is hidden by setting its Y position to $D0.
;
; NES-to-SMS Y coordinate adjustment: SMS_Y = NES_Y + 1. NES OAM entries with
; raw Y >= $CF are off-screen for the SMS visible range and are omitted from the
; compacted SAT; one $D0 terminator hides the remaining SMS entries.
;
; ── Sprite pattern variants ───────────────────────────────────────────────
; The SMS Mode 4 SAT has only X and tile-number bytes; there are no per-sprite
; flip bits or palette select, so NES sprite attr bits can't be honoured in
; hardware. Instead, rt_sat_resolve makes a first pass over OAM: for each sprite
; whose attr requests palette bits or H/V flip (and whose mapped tile isn't the
; blank, and whose raw Y is visible), it generates a variant of the source tile's
; pattern (from ROM data_chr) into one of 16 VRAM scratch slots (424-439) and
; points the sprite at that scratch slot. The resolved SMS tile number for every
; sprite is stashed in a 64-byte table at $D400, which the SAT upload then
; streams out. Variants are keyed by (mapped tile, attr & $C3), ignoring priority
; bit $20, so the engine stays game-agnostic.
;
; Scratch budget is 16 distinct sprite variants per frame; beyond that, extra
; variants fall back to the base mapped tile (graceful, rarely hit).
; Variant scratch is only safe while VDP register 6 selects sprite base $2000.
; When PPUCTRL bit 3 selects the runtime's $0000 SMS sprite base, resolved
; sprites fall back to their mapped base tiles instead of writing variants to the
; $3500 scratch window (which would not line up with SAT tile numbers for base
; $0000 and could corrupt unrelated VRAM). A future base-aware allocator can
; reserve a safe $0000-base scratch window and re-enable variants there.
;
; RAM scratch:
;   $D400-$D43F  resolved SMS tile number per sprite (64 bytes)
;   $D440-$D44F  variant cache mapped-tile keys (16 bytes)
;   $D450-$D45F  variant cache attr keys (attr & $C3, 16 bytes)
;   $D460        scratch_next (next free variant scratch slot, 0..15)
;   $D461        current variant attr key ($03 palette, $40 H, $80 V)
;   $D462-$D463  current variant source base address
;   $D464        current variant source tile key
;   $D465-$D467  current variant row temps (plane0, plane1, mask)
;   $D468        current OAM entry visible flag for resolve pass
;   $D469        round-robin variant-cache victim cursor
;   $D46A-$D46B  rt_oam_dma DE save (keeps helper off native stack)
;   $D46C-$D471  stackless rotate-memory helper scratch (flags.s)
;   $D472-$D478  SMB HUD / IRQ status / reserved scratch (boot.s)
;   $D479-$D47C  rt_oam_dma AF/HL save (keeps helper off native stack)
;   $D47D-$D47E  far slot-1 bank stack next-free pointer (dispatch.s)
;   $D480-$D4BF  compacted OAM attr byte per visible SAT entry (64 bytes)
;   $D4C0-$D4FF  far slot-1 bank stack entries (dispatch.s)
;   $D500-$D5FF  Translated-call continuation segment 1 (dispatch.s)
;
; Sprite VRAM slot layout (relative to VDP sprite base reg6 = $2000):
;   0..166   packed sprite patterns ($00-$A6)
;   167      blank/transparent tile
;   168..183 16 variant scratch slots  (absolute VRAM $3500..$36E0)
;
; LOCKED MAPPING CONTRACT: The private variant path is proven as
; boot presentation -> rt_sat_upload -> rt_sat_resolve ->
; variant_get_scratch -> do_sprite_variant. Its temporary slot-2 map is
; locked-only: it may execute beneath an outer PPU guard at depth 1 or 2, or
; during presentation at depth 0 with IFF disabled after the boot boundary
; assertion. rt_restore_prg_window is the locked restore primitive.
; do_sprite_variant must not acquire its own guard or wrapper: that would deepen
; this stack-sensitive private path.
;
; CHR-RAM 8x16 sprites are resolved per OAM entry into SMS slots 0..127.

.ifdef SPRITE_DYNAMIC_POOL
; On-demand pool: the profile chooses the slot window; the slot after the
; pool is the reserved transparent tile. Key tables reuse $D400-$D45F —
; the CHR-RAM resolve table and the legacy 16-entry keys, both unused in
; this configuration. Cache keys carry bit2 as the "dynamic source" flag:
; the id byte is then the NES tile and generation reads the ROM ext blob.
.define SAT_BLANK_REL    SPRITE_DYN_FIRST+SPRITE_DYN_SIZE
.define SAT_SCRATCH_BASE  SPRITE_DYN_FIRST
.define SAT_SCRATCH_COUNT SPRITE_DYN_SIZE
.define SAT_RESOLVED      $d400
.define SAT_VAR_TILE_KEYS $d400
.define SAT_VAR_ATTR_KEYS $d430
.else
.define SAT_BLANK_REL    167
.define SAT_SCRATCH_BASE  168
.define SAT_SCRATCH_COUNT 16
.define SAT_RESOLVED      $d400
.define SAT_VAR_TILE_KEYS $d440
.define SAT_VAR_ATTR_KEYS $d450
.endif
.define SAT_ATTRS         $d480
.define SAT_SCRATCH_NEXT  $d460
.define SAT_VARIANT_ATTR  $d461
.define SAT_VARIANT_SRC   $d462
.define SAT_VARIANT_TILE  $d464
.define SAT_VARIANT_P0    $d465
.define SAT_VARIANT_P1    $d466
.define SAT_VARIANT_MASK  $d467
.define SAT_VISIBLE_FLAG  $d468
.define SAT_RR_VICTIM     $d469
.define SAT_PAIR_MODE     $d468
.define SAT_PAIR_ROW      $d469
.define SAT_PAIR_TILE_KEYS $d400
.define SAT_PAIR_ATTR_KEYS $d480
.define SAT_OAM_DMA_DE_SAVE $d46a
.define SAT_OAM_DMA_AF_SAVE $d479
.define SAT_OAM_DMA_HL_SAVE $d47b

.section "sat" free

; ─── rt_oam_dma ───────────────────────────────────────────────────────────────
; Entry: A = high byte of source page in NES address space ($4014 write).
; Copies 256 bytes from SMS RAM $C000+(A<<8) to the OAM staging buffer $C900.
; Preserves AF/HL/DE; clobbers BC. `BC` is scratch at hardware-op boundaries,
; and saving AF/HL on native stack can cross the guard during nested NMI work.
rt_oam_dma:
  ld   (SAT_OAM_DMA_HL_SAVE), hl
  push af
  pop  hl
  ld   (SAT_OAM_DMA_AF_SAVE), hl
  ld   (SAT_OAM_DMA_DE_SAVE), de
  ld   h, a
  ld   l, $00
  ld   a, h
  add  a, $c0                ; remap to SMS RAM base $C000
  ld   h, a
  ld   a, ($cb0a)            ; NES DMA begins at OAMADDR and wraps at 256
  or   a
  jr   z, _oam_dma_aligned
  ld   e, a
  ld   d, $c9                ; first destination = $C900 + OAMADDR
  neg
  ld   c, a                  ; first span = 256 - OAMADDR
  ld   b, $00
  ldir
  ld   de, $c900
  ld   a, ($cb0a)
  ld   c, a                  ; wrapped span = OAMADDR
  ld   b, $00
  ldir
  jp   _oam_dma_done         ; jp: the unrolled aligned block is beyond jr range
_oam_dma_aligned:
  ; The aligned copy (OAMADDR == 0, the common case) is a fixed 256-byte
  ; RAM->RAM move into the OAM staging buffer. It never touches the VDP, so an
  ; unrolled LDI (16 cyc/byte) replaces LDIR (21 cyc/byte) with identical
  ; bytes: 4 x 64 LDI. `ldi` leaves A untouched, so A carries the outer count;
  ; BC is scratch here (it is destroyed either way and rt_oam_dma documents it).
  ld   de, $c900             ; OAM staging
  ld   a, 4                  ; 4 blocks of 64 bytes = 256
_oam_dma_blk:
  .rept 64
  ldi
  .endr
  dec  a
  jp   nz, _oam_dma_blk      ; jp (not jr): 64 LDIs exceed jr's 8-bit range
_oam_dma_done:
  ld   de, (SAT_OAM_DMA_DE_SAVE)
  ld   hl, (SAT_OAM_DMA_AF_SAVE)
  push hl
  ld   hl, (SAT_OAM_DMA_HL_SAVE)
  pop  af
  ret

; ─── do_sprite_variant ──────────────────────────────────────────────────────
; Generate one source sprite tile variant into a VRAM scratch slot.
;   Entry: A = scratch index (0..15), B = attr key ($03 pal, $40 H, $80 V),
;          C = source sprite tile (relative, 0..166).
;   Writes 32 bytes to VRAM at $3500 + index*32.
;   CV1's private shared-cache caller instead supplies CV1_SAT_SLOT and
;   writes $2000 + slot*64; A's legacy scratch index is ignored there.
;   Clobbers AF, BC, DE, HL. Restores the current PRG window in slot 2.
;   Locked-only private leaf; see the proven presentation chain above. Do not
;   add a guard/wrapper here, because it would deepen the stack-sensitive path.
do_sprite_variant:
  push af                    ; save scratch index
  ld   a, b
.ifdef SPRITE_DYNAMIC_POOL
  and  $c7                   ; bit2 = dynamic-source flag rides along
.else
  and  $c3
.endif
  ld   (SAT_VARIANT_ATTR), a ; palette + flip key
  ld   a, c
  ld   (SAT_VARIANT_TILE), a

.ifdef NES_CHR_RAM
  ; Dynamic CHR source: copy the selected NES sprite-table tile from the SRAM
  ; mirror into planar staging. The build-time data_chr asset is blank for
  ; CHR-RAM cartridges and cannot supply sprite palette/flip variants.
  ld   a, ($cb08)
  and  $08
  rrca
  rrca
  rrca                       ; table bit -> 0/1 in H before the *16 below
  ld   h, a
  ld   a, (SAT_VARIANT_TILE)
  ld   l, a
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl                ; table*4096 + tile*16
  ld   de, CHR_RAM_SRAM_BASE
  add  hl, de
  call rt_raw_ciram_sram_enable
  ld   de, $cb63
  ld   bc, 16
  ldir
  call rt_raw_ciram_sram_disable
  ld   hl, $cb63
  ld   (SAT_VARIANT_SRC), hl
.else
  ; source base in ROM = data_chr ($8000) + (256 + C)*32 = $A000 + C*32
.ifdef SPRITE_DYNAMIC_POOL
  ld   a, (SAT_VARIANT_ATTR)
  bit  2, a
  jr   z, _dof_static_src
  ; Dynamic source: park the ext TABLE entry address; the slot lookup and
  ; final source address resolve after the single sanctioned bank map
  ; below (an extra $FFFF write here would trip the nested-map guard).
  ld   a, c
  ld   e, a
  ld   d, $00
  ld   hl, data_chr_sprite_ext0
  add  hl, de
  ld   (SAT_VARIANT_SRC), hl
  jr   _dof_src_done
_dof_static_src:
.endif
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl                ; C * 32
  ld   de, $a000
  add  hl, de
  ld   (SAT_VARIANT_SRC), hl
.ifdef SPRITE_DYNAMIC_POOL
_dof_src_done:
.endif
.endif

  ; CV1's shared dynamic cache owns $2000 + slot*64 in both sprite sizes.
  ; Its 8x8 path uses this exact converter, not stale base copy-through.
  ; Other profiles retain the original 16-slot variant destination.
  pop  af                    ; scratch index
.ifdef CV1_RUNTIME_HOOKS
  ld   a, (CV1_SAT_SLOT)
.endif
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl                ; index * 32
.ifdef CV1_RUNTIME_HOOKS
  add  hl, hl
  ld   de, $2000
.else
.ifdef SPRITE_DYNAMIC_POOL
  ld   de, $2000+SPRITE_DYN_FIRST*32
.else
  ld   de, $3500
.endif
.endif
  add  hl, de
  ld   a, l
  out  ($bf), a
  ld   a, h
  or   $40                   ; VRAM write flag
  out  ($bf), a

  ; Map the static CHR asset for CHR-ROM builds. CHR-RAM variants read the
  ; staged planar bytes above and leave the current PRG window visible.
.ifndef NES_CHR_RAM
.ifdef SPRITE_DYNAMIC_POOL
  ld   a, (SAT_VARIANT_ATTR)
  bit  2, a
  ld   a, :data_chr
  jr   z, _dof_bank_ready
  ld   a, :data_chr_maps
_dof_bank_ready:
.else
  ld   a, :data_chr
.endif
  ld   ($ffff), a
.ifdef SPRITE_DYNAMIC_POOL
  ; Dynamic tiles: the parked ext-table entry now resolves to the real
  ; converted-tile address inside the mapped maps bank.
  ld   a, (SAT_VARIANT_ATTR)
  bit  2, a
  jr   z, _dof_src_ready
  ld   hl, (SAT_VARIANT_SRC)
  ld   a, (hl)
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl                ; ext slot * 32
  ld   de, data_chr_ext
  add  hl, de
  ld   (SAT_VARIANT_SRC), hl
_dof_src_ready:
.endif
.endif

  ld   b, 8                  ; rows remaining
  ld   c, $00                ; output row index r
_dof_row:
  ld   a, (SAT_VARIANT_ATTR)
  bit  7, a                  ; V-flip?
  ld   a, c
  jr   z, _dof_no_v
  neg
  add  a, 7                  ; src row = 7 - r
_dof_no_v:
.ifdef NES_CHR_RAM
  ld   e, a
  ld   d, $00
  ld   hl, (SAT_VARIANT_SRC)
  add  hl, de                ; planar plane-0 row
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  ld   de, 8
  add  hl, de                ; matching plane-1 row
  ld   a, (hl)
  ld   (SAT_VARIANT_P1), a
.else
  add  a, a
  add  a, a                  ; src_row * 4 (bytes per 4bpp row)
  ld   e, a
  ld   d, $00
  ld   hl, (SAT_VARIANT_SRC)
  add  hl, de                ; HL = source row pointer
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  inc  hl
  ld   a, (hl)
  ld   (SAT_VARIANT_P1), a
.endif
  ld   a, (SAT_VARIANT_ATTR)
  and  $40                   ; H-flip?
  jr   z, _dof_row_done
  ; Table-driven bit reversal. The pinned slot-0 page keeps this stackless and
  ; preserves B/C/DE; HL is dead once the source planes have been staged.
  ld   a, (SAT_VARIANT_P0)
  ld   l, a
  ld   h, >rt_bit_reverse_table
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  ld   a, (SAT_VARIANT_P1)
  ld   l, a
  ld   h, >rt_bit_reverse_table
  ld   a, (hl)
  ld   (SAT_VARIANT_P1), a
_dof_row_done:
  ld   a, (SAT_VARIANT_P0)
  out  ($be), a              ; plane 0
  ld   d, a
  ld   a, (SAT_VARIANT_P1)
  out  ($be), a              ; plane 1
  or   d
  ld   (SAT_VARIANT_MASK), a ; high planes only where source is opaque
  ld   a, (SAT_VARIANT_ATTR)
  bit  0, a                  ; NES sprite palette bit 0 -> SMS plane 2
  jr   z, _dof_p2_zero
  ld   a, (SAT_VARIANT_MASK)
  jr   _dof_p2_out
_dof_p2_zero:
  xor  a
_dof_p2_out:
  out  ($be), a
  ld   a, (SAT_VARIANT_ATTR)
  bit  1, a                  ; NES sprite palette bit 1 -> SMS plane 3
  jr   z, _dof_p3_zero
  ld   a, (SAT_VARIANT_MASK)
  jr   _dof_p3_out
_dof_p3_zero:
  xor  a
_dof_p3_out:
  out  ($be), a
  inc  c
  dec  b
  jp   nz, _dof_row

  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  ret

; ─── variant_get_scratch ─────────────────────────────────────────────────────
; Reuse or allocate a scratch slot for (source rel tile, attr & $C3), generate
; it on miss, and return the SMS tile number to use. Entry: C = source rel tile,
; B = attr key. Exit: A = SMS sprite tile (relative). Falls back to the base tile
; if the per-frame scratch pool is exhausted.
; Clobbers AF, BC, DE, HL. Locked-only transitively through do_sprite_variant;
; valid only in the documented outer-guard or IFF-disabled presentation contexts.
variant_get_scratch:
  ld   a, c
  ld   (SAT_VARIANT_TILE), a
  ld   a, b
.ifdef SPRITE_DYNAMIC_POOL
  and  $c7
.else
  and  $c3
.endif
  ld   (SAT_VARIANT_ATTR), a

  ld   a, (SAT_SCRATCH_NEXT)
  or   a
  jr   z, _vgs_miss
  ld   b, a                  ; entries to scan
  ld   hl, SAT_VAR_TILE_KEYS
  ld   de, SAT_VAR_ATTR_KEYS
  ld   c, $00                ; cache index
_vgs_scan:
  ld   a, (SAT_VARIANT_TILE)
  cp   (hl)
  jr   nz, _vgs_scan_next
  ld   a, (SAT_VARIANT_ATTR)
  ex   de, hl
  cp   (hl)
  ex   de, hl
  jr   z, _vgs_hit
_vgs_scan_next:
  inc  hl
  inc  de
  inc  c
  djnz _vgs_scan

_vgs_miss:
  ld   a, (SAT_SCRATCH_NEXT)
  cp   SAT_SCRATCH_COUNT
  jr   c, _vgs_alloc
  ; Pool full: evict ONE slot round-robin (H2). No bulk flush, no memo
  ; clear — memo hits verify their slot's keys, so entries pointing at a
  ; reused slot simply miss. This removed the flush storm (the pool
  ; overflows every few frames in busy scenes; full regeneration bursts
  ; and 80-byte invalidation loops were ~2% of all execution).
  ld   a, (SAT_RR_VICTIM)
  inc  a
.ifdef SPRITE_DYNAMIC_POOL
  cp   SAT_SCRATCH_COUNT
  jr   c, _vgs_victim_ok
  xor  a
_vgs_victim_ok:
.else
  and  SAT_SCRATCH_COUNT - 1
.endif
  ld   (SAT_RR_VICTIM), a
  jr   _vgs_alloc_at         ; A = victim slot

_vgs_alloc:
  ld   a, (SAT_SCRATCH_NEXT)
  push af
  inc  a
  ld   (SAT_SCRATCH_NEXT), a
  pop  af
_vgs_alloc_at:
  ; A = slot to (re)generate into.
  push af
  ld   a, (SAT_VARIANT_ATTR)
  ld   b, a
  ld   a, (SAT_VARIANT_TILE)
  ld   c, a
  pop  af
  push af
  call do_sprite_variant     ; A = index, B = attr key, C = source rel
  pop  af
  ld   c, a                  ; C = slot
  ld   e, a
  ld   d, $00
  ld   hl, SAT_VAR_TILE_KEYS
  add  hl, de
  ld   a, (SAT_VARIANT_TILE)
  ld   (hl), a
  ld   hl, SAT_VAR_ATTR_KEYS
  add  hl, de
  ld   a, (SAT_VARIANT_ATTR)
  ld   (hl), a
  ld   a, c
  add  a, SAT_SCRATCH_BASE
  ret
_vgs_hit:
  ld   a, c
  add  a, SAT_SCRATCH_BASE
  ret

.ifdef NES_CHR_RAM
; Build one NES 8x16 sprite pair in the SMS $2000 sprite-pattern region.
; Entry: A = NES OAM tile byte, B = attr ($03 palette, $40 H, $80 V),
;        C = even destination SMS tile (2 * OAM entry index).
.ifdef CV1_RUNTIME_HOOKS
rt_sat_build_pair_8x16:
.endif
_sat_build_pair_8x16:
  ld   (SAT_VARIANT_TILE), a
  ld   a, b
  and  $c3
  ld   (SAT_VARIANT_ATTR), a
  ld   a, (SAT_VARIANT_TILE)
  and  $01
  ld   h, a
  ld   a, (SAT_VARIANT_TILE)
  and  $fe
  ld   l, a
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   de, CHR_RAM_SRAM_BASE
  add  hl, de
  ld   (SAT_VARIANT_SRC), hl

  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   de, $2000
  add  hl, de
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a

  call rt_raw_ciram_sram_enable
  xor  a
  ld   (SAT_PAIR_ROW), a
_sat_pair_row_loop:
  ld   a, (SAT_PAIR_ROW)
  ld   c, a
  ld   a, (SAT_VARIANT_ATTR)
  bit  7, a
  ld   a, c
  jr   z, _sat_pair_row_ready
  ld   a, 15
  sub  c
_sat_pair_row_ready:
  ld   c, a
  and  $07
  ld   e, a
  ld   d, $00
  ld   a, c
  and  $08
  jr   z, _sat_pair_have_offset
  ld   a, e
  add  a, 16
  ld   e, a
_sat_pair_have_offset:
  ld   hl, (SAT_VARIANT_SRC)
  add  hl, de
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  ld   de, 8
  add  hl, de
  ld   a, (hl)
  ld   (SAT_VARIANT_P1), a

  ld   a, (SAT_VARIANT_ATTR)
  bit  6, a
  jr   z, _sat_pair_emit
  ld   a, (SAT_VARIANT_P0)
  ld   l, a
  ld   h, >rt_bit_reverse_table
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  ld   a, (SAT_VARIANT_P1)
  ld   l, a
  ld   h, >rt_bit_reverse_table
  ld   a, (hl)
  ld   (SAT_VARIANT_P1), a
_sat_pair_emit:
  ld   a, (SAT_VARIANT_P0)
  out  ($be), a
  ld   c, a
  ld   a, (SAT_VARIANT_P1)
  out  ($be), a
  or   c
  ld   (SAT_VARIANT_MASK), a
  ld   c, a
  ld   a, (SAT_VARIANT_ATTR)
  bit  0, a
  ld   a, $00
  jr   z, _sat_pair_p2_ready
  ld   a, c
_sat_pair_p2_ready:
  out  ($be), a
  ld   a, (SAT_VARIANT_ATTR)
  bit  1, a
  ld   a, $00
  jr   z, _sat_pair_p3_ready
  ld   a, (SAT_VARIANT_MASK)
_sat_pair_p3_ready:
  out  ($be), a
  ld   a, (SAT_PAIR_ROW)
  inc  a
  ld   (SAT_PAIR_ROW), a
  cp   16
  jr   c, _sat_pair_row_loop
  call rt_raw_ciram_sram_disable
  ret

; Each OAM entry owns one even SMS pair slot. Rebuild its pair only when the
; source tile or palette/flip key changes.
_sat_resolve_8x16:
  ld   a, (SAT_PAIR_MODE)
  cp   1
  jr   z, _res16_cache_ready
  ld   a, 1
  ld   (SAT_PAIR_MODE), a
  ld   hl, SAT_PAIR_TILE_KEYS
  ld   bc, $0040
  ld   a, $ff
  call mem_fill
  ld   hl, SAT_PAIR_ATTR_KEYS
  ld   bc, $0040
  call mem_fill
_res16_cache_ready:
  ld   hl, $c900
  ld   de, SAT_PAIR_TILE_KEYS
  ld   b, 64
_res16_loop:
  push bc
  ld   a, (hl)
  cp   $cf
  jr   nc, _res16_hidden
  inc  hl
  ld   a, (hl)
  ld   (SAT_VARIANT_TILE), a
  inc  hl
  ld   a, (hl)
  and  $c3
  ld   (SAT_VARIANT_ATTR), a
  inc  hl
  inc  hl
  ld   a, (SAT_VARIANT_TILE)
  ld   c, a
  ld   a, (de)
  cp   c
  jr   nz, _res16_regen
  push hl
  ld   a, e
  add  a, $80
  ld   l, a
  ld   h, $d4
  ld   a, (SAT_VARIANT_ATTR)
  cp   (hl)
  pop  hl
  jr   z, _res16_next
_res16_regen:
  ld   a, (SAT_VARIANT_TILE)
  ld   (de), a
  push hl
  ld   a, e
  add  a, $80
  ld   l, a
  ld   h, $d4
  ld   a, (SAT_VARIANT_ATTR)
  ld   (hl), a
  pop  hl
  push hl
  push de
  ld   a, e
  add  a, a
  ld   c, a
  ld   a, (SAT_VARIANT_ATTR)
  ld   b, a
  ld   a, (SAT_VARIANT_TILE)
  call _sat_build_pair_8x16
  pop  de
  pop  hl
  jr   _res16_next
_res16_hidden:
  inc  hl
  inc  hl
  inc  hl
  inc  hl
_res16_next:
  inc  de
  pop  bc
  dec  b
  jp   nz, _res16_loop
  ret
.endif

; ─── rt_sat_resolve ───────────────────────────────────────────────────────────
; First SAT pass: resolve each sprite's SMS tile number into $D400, performing
; software palette/flip variants into VRAM scratch as needed. Clobbers AF, BC,
; DE, HL. Locked-only transitively through rt_map_sprite_tile and
; variant_get_scratch; valid only in the documented outer-guard or IFF-disabled
; presentation contexts.
rt_sat_resolve:
.ifdef NES_CHR_RAM
  ld   a, ($cb08)
  bit  5, a
  jp   nz, _sat_resolve_8x16
  xor  a
  ld   (SAT_PAIR_MODE), a
.endif
  ; H.3: the variant pool persists across frames (see _vgs_miss).
  ; H2: hidden sprites (raw Y >= $CF — most of the 64 slots in typical
  ; frames) take a fast path: their resolved value is never uploaded
  ; (phase-1 compaction skips them by Y), so the old "preserve timing"
  ; full resolve for hidden slots was pure waste.
  ; Both buffers are page-aligned ($C900 staging, $D400 resolved), so the
  ; loop walks L/E only, and B stays the live counter: the hidden-sprite
  ; majority pays no per-slot push/pop and no 16-bit pointer arithmetic.
  ld   hl, $c900             ; OAM staging
  ld   de, SAT_RESOLVED
  ld   b, 64
_res_loop:
  ld   a, (hl)               ; raw NES Y
  cp   $cf
  jr   c, _res_visible
_res_hidden:
  ld   a, SAT_BLANK_REL      ; resolved value unused for hidden slots
_res_store_common:           ; A = resolved value; L at the entry's Y byte
  ld   (de), a
  inc  e
  ld   a, l
  add  a, $04
  ld   l, a
  djnz _res_loop
  ret
_res_visible:
.ifdef SMB_RUNTIME_HOOKS
  ; Hidden entries do no VDP work; poll before each variable-cost variant.
  call rt_smb_hud_poll
.endif
  push bc                    ; B is the loop counter; calls below need B/C
  inc  l                     ; -> tile
  ld   a, (hl)               ; A = NES tile
  inc  l                     ; -> attr
  ld   b, (hl)               ; B = attr
  dec  l
  dec  l                     ; L back at the entry's Y byte
  call rt_map_sprite_tile    ; A = mapped rel tile (preserves BC, DE, HL)
  ld   c, a                  ; C = rel tile (default resolved value)
.ifndef NES_CHR_RAM
  cp   SAT_BLANK_REL         ; blank tile? leave transparent, no variant
  jr   z, _res_vdone
.endif
  ld   a, b
  and  $c3                   ; palette bits + H/V flip; ignore priority bit 5
  jr   z, _res_vdone         ; base palette, no flip -> use mapped tile
  ld   b, a                  ; B = variant attr key
  ld   a, ($cb08)            ; PPUCTRL bit3 => SMS base $0000: no safe scratch
  bit  3, a
  jr   nz, _res_vdone
_res_visible_variant:
  push hl                    ; preserve OAM pointer
  push de
  call variant_get_scratch   ; C = src rel, B = attr key -> A = scratch rel
  pop  de
  pop  hl
  ld   c, a                  ; C = resolved
_res_vdone:
  ld   a, c
  pop  bc
  jr   _res_store_common

; ─── rt_sat_upload ────────────────────────────────────────────────────────────
; Copies sprite data from NES OAM staging at $C900 into SMS VRAM SAT at $3F00,
; resolving software sprite variants first. This is the presentation entry of
; the proven private variant chain; it runs with IFF disabled at depth 0 after
; the boot boundary assertion (or under an outer PPU guard at depth 1/2).
; Preserves AF, BC, DE, HL. Its mapping descendants restore via the locked
; rt_restore_prg_window primitive.
rt_sat_upload:
  push af
  push hl
  push bc
  push de

.ifdef NES_CHR_RAM
  call rt_sat_resolve        ; 8x16 pair resolver (CHR-ROM resolves fused
                             ; into the X/T sweep below)
.endif

  ; ── Phase 1: compact visible Y positions to VRAM $3F00 ───────────────────
  ; NOTE: no SAT double buffering — a second table at $3E00 collided with
  ; the fold of NES nametable rows 28-29 ($3E00-$3EFF), turning brick
  ; tiles into 64 garbage sprites on alternate frames (the title-band
  ; flicker). The vblank-aligned presentation makes the single-table
  ; upload tear-safe: ~2K cycles inside the blanking window.
  ld   a, $00
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

  ld   hl, $c900             ; Y is first byte of each 4-byte entry
  ld   c, 0                  ; compacted visible-sprite count
  ld   b, 64
_sat_y_loop:
  ld   a, (hl)               ; NES Y
  cp   $cf                   ; raw $CF..$FF would be SMS $D0..$00/off-screen
  jr   nc, _sat_y_skip
  inc  a                     ; visible SMS Y = NES Y + 1
_sat_y_visible:
  out  ($be), a
  inc  c
_sat_y_skip:
  ; staging is page-aligned ($C900-$C9FF): stride the low byte only
  ld   a, l
  add  a, $04
  ld   l, a
  djnz _sat_y_loop

  ; Hide every remaining SAT slot. The classic Y=$D0 end-of-list terminator
  ; only exists in 192-line mode — in our 224-line mode $D0 is a VISIBLE
  ; line (208) and entries past the compacted list kept rendering stale
  ; sprites (field report: squashed-goomba remains following the player).
  ; Write Y=$E0 (line 224+, offscreen in 224-line mode) to all 64-N slots.
  ld   a, 64
  sub  c                     ; A = remaining slots
  jr   z, _sat_y_done
  ld   b, a
  ld   a, $e0
_sat_y_hide:
  out  ($be), a
  djnz _sat_y_hide
_sat_y_done:

  ; ── Phase 2: compact visible (X, resolved tile) pairs to VRAM $3F80 ───────
  ld   a, $80
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

.ifdef NES_CHR_RAM
  ld   a, ($cb08)
  bit  5, a
  jr   nz, _sat_xt_8x16
.endif
  ; CHR-ROM: variant resolution is FUSED into this sweep (H.5): each
  ; visible sprite maps its tile and resolves flips/palette variants
  ; in place, killing the separate 64-slot resolve pass and the $D400
  ; intermediate. D shadows the next SAT XT VDP low address so the
  ; write position can be re-established after a variant *generation*
  ; touches the VDP scratch window (cache hits touch nothing; the
  ; unconditional re-address after the variant path costs ~30 cycles on
  ; the rare flip/palette sprites only).
  ld   hl, $c900             ; OAM staging (page-aligned: walk L)
  ld   e, 64                 ; slots
  ld   d, $80                ; next XT VDP low address
_sat_xt_loop:
  ld   a, (hl)               ; NES Y controls visibility/compaction
  cp   $cf
  jr   nc, _sat_xt_skip
  inc  l
  ld   c, (hl)               ; NES tile
  inc  l
  ld   b, (hl)               ; attr
  inc  l
  ld   a, (hl)               ; X
  inc  l
  out  ($be), a              ; write X
  inc  d
  ld   a, c                  ; C keeps the NES tile (rt_map_sprite_tile preserves BC)
  call rt_map_sprite_tile    ; A = mapped rel tile (preserves BC, DE, HL)
.ifdef SPRITE_DYNAMIC_POOL
  cp   SAT_BLANK_REL
  jr   z, _sat_xt_blank
  inc  a                     ; $FF = on-demand sentinel from the map
  jr   z, _sat_xt_dynamic
  ld   c, a                  ; undo the inc: static mapped tile
  dec  c
  jr   _sat_xt_have_static
_sat_xt_blank:
  ld   c, a
  jr   _sat_xt_out
_sat_xt_dynamic:
  ; Dynamic tile: base $2000 is guaranteed (only map0 carries sentinels),
  ; and identity attributes still generate through the same pool. C already
  ; holds the NES tile the ext table is keyed by.
  ld   a, b
  and  $c3
  or   $04
  ld   b, a
  push hl
  push de
  call variant_get_scratch   ; C = NES tile, B = attr|dyn -> A = pool rel
  pop  de
  pop  hl
  ld   c, a
  ld   a, d
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a
  jr   _sat_xt_out
_sat_xt_have_static:
  ld   a, b
  and  $c3                   ; palette bits + H/V flip; ignore priority
  jr   z, _sat_xt_out
  jr   _sat_xt_static_variant
.else
  ld   c, a
  cp   SAT_BLANK_REL
  jr   z, _sat_xt_out
  ld   a, b
  and  $c3                   ; palette bits + H/V flip; ignore priority
  jr   z, _sat_xt_out
.endif
_sat_xt_static_variant:
  ld   b, a
  ld   a, ($cb08)            ; PPUCTRL bit3 => $0000 base: no safe scratch
  bit  3, a
  jr   nz, _sat_xt_out
  push hl
  push de
  call variant_get_scratch   ; C=src, B=attr key -> A = scratch rel
  pop  de
  pop  hl
  ld   c, a
  ; re-establish the SAT XT write position (a generation moved it)
  ld   a, d
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a
_sat_xt_out:
  ld   a, c
  out  ($be), a              ; write tile
  inc  d
  dec  e
  jp   nz, _sat_xt_loop
  jr   _sat_upload_done
_sat_xt_skip:
  ld   a, l
  add  a, $04                ; advance to next entry
  ld   l, a
  dec  e
  jp   nz, _sat_xt_loop
  jr   _sat_upload_done

.ifdef NES_CHR_RAM
_sat_xt_8x16:
  ; The pair resolver assigns even SMS tiles by original OAM index. Hidden
  ; entries are skipped in SAT order but retain their index-owned cache slot.
  ld   hl, $c900
  ld   b, 64
  ld   c, 0
_sat_xt_16_loop:
  ld   a, (hl)
  cp   $cf
  jr   nc, _sat_xt_16_skip
  inc  hl
  inc  hl
  inc  hl
  ld   a, (hl)
  inc  hl
  out  ($be), a              ; X
  ld   a, c
  add  a, a
  out  ($be), a              ; even pair tile
  jr   _sat_xt_16_next
_sat_xt_16_skip:
  inc  hl
  inc  hl
  inc  hl
  inc  hl
_sat_xt_16_next:
  inc  c
  djnz _sat_xt_16_loop
.endif

_sat_upload_done:
  pop  de
  pop  bc
  pop  hl
  pop  af
  ret

.ends
.endif ; separate full MMC3 graphics backend
.endif ; source hardware owns source OAM and packet SAT
