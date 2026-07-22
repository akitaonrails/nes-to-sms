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
;   $D472-$D478  IRQ saved BC/status/HL/AF scratch (boot.s)
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

.define SAT_BLANK_REL    167
.define SAT_SCRATCH_BASE  168
.define SAT_SCRATCH_COUNT 16
.define SAT_RESOLVED      $d400
.define SAT_VAR_TILE_KEYS $d440
.define SAT_VAR_ATTR_KEYS $d450
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
  jr   _oam_dma_done
_oam_dma_aligned:
  ld   de, $c900             ; OAM staging
  ld   bc, $0100             ; 256 bytes
  ldir
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
;   Clobbers AF, BC, DE, HL. Restores the current PRG window in slot 2.
;   Locked-only private leaf; see the proven presentation chain above. Do not
;   add a guard/wrapper here, because it would deepen the stack-sensitive path.
do_sprite_variant:
  push af                    ; save scratch index
  ld   a, b
  and  $c3
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
.endif

  ; dest VRAM = $3500 + index*32
  pop  af                    ; scratch index
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl                ; index * 32
  ld   de, $3500
  add  hl, de
  ld   a, l
  out  ($bf), a
  ld   a, h
  or   $40                   ; VRAM write flag
  out  ($bf), a

  ; Map the static CHR asset for CHR-ROM builds. CHR-RAM variants read the
  ; staged planar bytes above and leave the current PRG window visible.
.ifndef NES_CHR_RAM
  ld   a, :data_chr
  ld   ($ffff), a
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
  ; Inline bit-reversal for H-flip. Stackless and preserves B (row counter)
  ; and C (output row index): E is shifted right, A accumulates reversed bits.
  ld   a, (SAT_VARIANT_P0)
  ld   e, a
  xor  a
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  ld   (SAT_VARIANT_P0), a
  ld   a, (SAT_VARIANT_P1)
  ld   e, a
  xor  a
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
  srl  e
  rla
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
  and  $c3
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
  and  SAT_SCRATCH_COUNT - 1
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
  call _sat_reverse_a
  ld   (SAT_VARIANT_P0), a
  ld   a, (SAT_VARIANT_P1)
  call _sat_reverse_a
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

_sat_reverse_a:
  ; Reverse all eight bits with three constant-time permutation stages. The
  ; former shift loop cost eight iterations for every plane row of every
  ; horizontally flipped 8x16 sprite, making ordinary animation a dominant
  ; frame expense. B is dead inside _sat_build_pair_8x16 after the attribute
  ; key has been parked in RAM.
  ld   b, a
  rrca
  rrca
  xor  b
  and  $aa
  xor  b                    ; swap adjacent 2-bit groups
  ld   b, a
  rrca
  rrca
  rrca
  rrca
  xor  b
  and  $66
  xor  b                    ; reverse around the nibble boundary
  rrca                      ; align the reversed bit order
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
  ld   hl, $c900             ; OAM staging
  ld   de, SAT_RESOLVED
  ld   b, 64
_res_loop:
  push bc
  ld   a, (hl)               ; raw NES Y
  cp   $cf
  jr   nc, _res_hidden
  inc  hl                    ; -> tile
  ld   a, (hl)               ; A = NES tile
  inc  hl                    ; -> attr
  ld   b, (hl)               ; B = attr
  inc  hl                    ; -> X
  inc  hl                    ; -> next entry Y
  call rt_map_sprite_tile    ; A = mapped rel tile (preserves BC, DE, HL)
  ld   c, a                  ; C = rel tile (default resolved value)
.ifndef NES_CHR_RAM
  cp   SAT_BLANK_REL         ; blank tile? leave transparent, no variant
  jr   z, _res_store
.endif
  ld   a, b
  and  $c3                   ; palette bits + H/V flip; ignore priority bit 5
  jr   z, _res_store         ; base palette, no flip -> use mapped tile
  ld   b, a                  ; B = variant attr key
  ld   a, ($cb08)            ; PPUCTRL bit3 => SMS base $0000: no safe scratch
  bit  3, a
  jr   nz, _res_store
_res_visible_variant:
  push hl                    ; preserve OAM pointer
  push de
  call variant_get_scratch   ; C = src rel, B = attr key -> A = scratch rel
  pop  de
  pop  hl
  ld   c, a                  ; C = resolved
_res_store:
  ld   a, c
  ld   (de), a               ; resolved[i]
  inc  de
  pop  bc
  dec  b
  jp   nz, _res_loop
  ret
_res_hidden:
  ld   a, SAT_BLANK_REL
  ld   (de), a               ; resolved value unused for hidden slots
  inc  de
  inc  hl
  inc  hl
  inc  hl
  inc  hl
  pop  bc
  dec  b
  jp   nz, _res_loop
  ret

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

  call rt_sat_resolve

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
  inc  hl
  inc  hl
  inc  hl
  inc  hl
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
  ld   hl, $c900             ; OAM (for X)
  ld   de, SAT_RESOLVED      ; resolved tile numbers
  ld   b, 64
_sat_xt_loop:
  ld   a, (hl)               ; NES Y controls visibility/compaction
  cp   $cf
  jr   nc, _sat_xt_skip
  inc  hl                    ; skip Y
  inc  hl                    ; skip tile
  inc  hl                    ; skip attr
  ld   a, (hl)               ; A = X
  inc  hl                    ; advance to next entry
  out  ($be), a              ; write X
  ld   a, (de)               ; resolved SMS tile
  inc  de
  out  ($be), a              ; write tile
  jr   _sat_xt_next
_sat_xt_skip:
  inc  hl                    ; skip Y
  inc  hl                    ; skip tile
  inc  hl                    ; skip attr
  inc  hl                    ; advance to next entry
  inc  de                    ; skip resolved tile for this hidden sprite
_sat_xt_next:
  djnz _sat_xt_loop
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
