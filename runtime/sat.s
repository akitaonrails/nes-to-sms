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
; RAM scratch (free region above the $CC00-$D3FF nametable shadow):
;   $D400-$D43F  resolved SMS tile number per sprite (64 bytes)
;   $D440-$D44F  variant cache mapped-tile keys (16 bytes)
;   $D450-$D45F  variant cache attr keys (attr & $C3, 16 bytes)
;   $D460        scratch_next (next free variant scratch slot, 0..15)
;   $D461        current variant attr key ($03 palette, $40 H, $80 V)
;   $D462-$D463  current variant source base address
;   $D464        current variant source tile key
;   $D465-$D467  current variant row temps (plane0, plane1, mask)
;   $D468        current OAM entry visible flag for resolve pass
;   $D480-$D4BF  compacted OAM attr byte per visible SAT entry (64 bytes)
;   $D500-$D5FF  H-flip byte bit-reverse LUT (page-aligned), built at boot
;
; Sprite VRAM slot layout (relative to VDP sprite base reg6 = $2000):
;   0..166   packed sprite patterns ($00-$A6)
;   167      blank/transparent tile
;   168..183 16 variant scratch slots  (absolute VRAM $3500..$36E0)
;
; TODO(Phase 3): Handle 8x16 sprites (NES PPU ctrl bit 5 = 1).

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
.define SAT_HFLIP_LUT     $d500   ; page-aligned: LUT[b] = SAT_HFLIP_LUT + b

.section "sat" free

; ─── rt_build_hflip_lut ────────────────────────────────────────────────────
; Build the 256-entry horizontal-flip byte LUT at $D500: LUT[i] = bit-reverse(i)
; (mirroring the 8 pixels of one 4bpp plane byte). Called once from boot.
rt_build_hflip_lut:
  ld   hl, SAT_HFLIP_LUT
  ld   c, $00                ; i
_bhl_loop:
  ld   a, c
  ld   b, 8
  ld   d, $00                ; reversed accumulator
_bhl_bit:
  srl  a                     ; bit0 of i -> carry, 0 -> bit7
  rl   d                     ; shift carry into d (MSB-first => reverse)
  djnz _bhl_bit
  ld   (hl), d
  inc  hl
  inc  c
  jr   nz, _bhl_loop         ; until i wraps 0
  ret

; ─── rt_oam_dma ───────────────────────────────────────────────────────────────
; Entry: A = high byte of source page in NES address space ($4014 write).
; Copies 256 bytes from SMS RAM $C000+(A<<8) to the OAM staging buffer $C900.
rt_oam_dma:
  push af
  push hl
  push de
  push bc
  ld   h, a
  ld   l, $00
  ld   a, h
  add  a, $c0                ; remap to SMS RAM base $C000
  ld   h, a
  ld   de, $c900             ; OAM staging
  ld   bc, $0100             ; 256 bytes
  ldir
  pop  bc
  pop  de
  pop  hl
  pop  af
  ret

; ─── do_sprite_variant ──────────────────────────────────────────────────────
; Generate one source sprite tile variant into a VRAM scratch slot.
;   Entry: A = scratch index (0..15), B = attr key ($03 pal, $40 H, $80 V),
;          C = source sprite tile (relative, 0..166).
;   Writes 32 bytes to VRAM at $3500 + index*32.
;   Clobbers AF, BC, DE, HL. Leaves data_prg_low mapped in slot 2.
do_sprite_variant:
  push af                    ; save scratch index
  ld   a, b
  and  $c3
  ld   (SAT_VARIANT_ATTR), a ; palette + flip key

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

  ; map data_chr bank into slot 2 for the source reads
  ld   a, :data_chr
  ld   ($ffff), a

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
  add  a, a
  add  a, a                  ; src_row * 4 (bytes per 4bpp row)
  ld   e, a
  ld   d, $00
  ld   hl, (SAT_VARIANT_SRC)
  add  hl, de                ; HL = source row pointer
  ld   a, (SAT_VARIANT_ATTR)
  and  $40                   ; H-flip?
  jr   z, _dof_plain
  ld   d, $d5                ; LUT page high byte (SAT_HFLIP_LUT = $D500)
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  ld   (SAT_VARIANT_P0), a
  inc  hl
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  ld   (SAT_VARIANT_P1), a
  jr   _dof_row_done
_dof_plain:
  ld   a, (hl)
  ld   (SAT_VARIANT_P0), a
  inc  hl
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
  djnz _dof_row

  ld   a, :data_prg_low      ; restore translated-data bank
  ld   ($ffff), a
  ret

; ─── variant_get_scratch ─────────────────────────────────────────────────────
; Reuse or allocate a scratch slot for (source rel tile, attr & $C3), generate
; it on miss, and return the SMS tile number to use. Entry: C = source rel tile,
; B = attr key. Exit: A = SMS sprite tile (relative). Falls back to the base tile
; if the per-frame scratch pool is exhausted.
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
  ld   a, (SAT_VARIANT_TILE) ; pool full -> base mapped tile
  ret
_vgs_alloc:
  ld   a, (SAT_VARIANT_ATTR)
  ld   b, a
  ld   a, (SAT_VARIANT_TILE)
  ld   c, a
  ld   a, (SAT_SCRATCH_NEXT)
  call do_sprite_variant     ; A = index, B = attr key, C = source rel
  ld   a, (SAT_SCRATCH_NEXT)
  ld   c, a                  ; index just used / returned on miss
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
  inc  a
  ld   (SAT_SCRATCH_NEXT), a
  ld   a, c
  add  a, SAT_SCRATCH_BASE
  ret
_vgs_hit:
  ld   a, c
  add  a, SAT_SCRATCH_BASE
  ret

; ─── rt_sat_resolve ───────────────────────────────────────────────────────────
; First SAT pass: resolve each sprite's SMS tile number into $D400, performing
; software palette/flip variants into VRAM scratch as needed. Clobbers AF, BC,
; DE, HL.
rt_sat_resolve:
  xor  a
  ld   (SAT_SCRATCH_NEXT), a ; reset scratch pool for this frame
  ld   hl, $c900             ; OAM staging
  ld   de, SAT_RESOLVED
  ld   b, 64
_res_loop:
  push bc                    ; save sprite counter
  ld   a, (hl)               ; raw NES Y
  cp   $cf                   ; hidden/off-screen? preserve timing but don't allocate
  jr   nc, _res_mark_hidden
  ld   a, $01
  jr   _res_mark_store
_res_mark_hidden:
  xor  a
_res_mark_store:
  ld   (SAT_VISIBLE_FLAG), a
  inc  hl                    ; -> tile
  ld   a, (hl)               ; A = NES tile
  inc  hl                    ; -> attr
  ld   b, (hl)               ; B = attr
  inc  hl                    ; -> X
  inc  hl                    ; -> next entry Y
  call rt_map_sprite_tile    ; A = mapped rel tile (preserves BC, DE, HL)
  ld   c, a                  ; C = rel tile (default resolved value)
  ld   a, c
  cp   SAT_BLANK_REL         ; blank tile? leave transparent, no variant
  jr   z, _res_store
  ld   a, b
  and  $c3                   ; palette bits + H/V flip; ignore priority bit 5
  jr   z, _res_store         ; base palette, no flip -> use mapped tile
  ld   b, a                  ; B = variant attr key if variants are safe
  ld   a, ($cb08)            ; PPUCTRL shadow: runtime uses bit3 => SMS base $0000
  bit  3, a
  jr   nz, _res_store         ; $0000 base has no safe aligned variant scratch
  ld   a, (SAT_VISIBLE_FLAG)
  or   a
  jr   nz, _res_visible_variant
  ; Hidden/off-screen sprites preserve comparable VBlank timing when variant
  ; scratch is safe, but restore scratch_next and the base tile afterward so
  ; they cannot exhaust the visible sprite variant cache.
  push bc                    ; save attr key + base mapped tile
  push de
  push hl
  ld   a, (SAT_SCRATCH_NEXT)
  push af
  call variant_get_scratch
  pop  af
  ld   (SAT_SCRATCH_NEXT), a
  pop  hl
  pop  de
  pop  bc
  jr   _res_store
_res_visible_variant:
  push de
  push hl
  call variant_get_scratch   ; C = src rel, B = attr key -> A = scratch rel
  pop  hl
  pop  de
  ld   c, a
_res_store:
  ld   a, c
  ld   (de), a               ; resolved[i]
  inc  de
  pop  bc
  djnz _res_loop
  ret

; ─── rt_sat_upload ────────────────────────────────────────────────────────────
; Copies sprite data from NES OAM staging at $C900 into SMS VRAM SAT at $3F00,
; resolving software sprite variants first. Called from irq_handler during VBlank.
rt_sat_upload:
  push af
  push hl
  push bc
  push de

  call rt_sat_resolve

  ; ── Phase 1: compact visible Y positions to VRAM $3F00 ───────────────────
  ; SMS treats Y=$D0 as an end-of-list terminator, unlike NES OAM where hidden
  ; sprites can appear anywhere. Scan NES OAM in order, write only visible
  ; sprites, then emit one terminator. Also test raw NES Y before adding 1 so
  ; hidden values like $FF do not wrap to SMS Y=$00.
  ld   a, $00
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

  ld   hl, $c900             ; Y is first byte of each 4-byte entry
  ld   de, SAT_ATTRS         ; compacted attrs mirror visible SAT order
  ld   b, 64
_sat_y_loop:
  ld   a, (hl)               ; NES Y
  cp   $cf                   ; raw $CF..$FF would be SMS $D0..$00/off-screen
  jr   nc, _sat_y_skip
  inc  a                     ; visible SMS Y = NES Y + 1
_sat_y_visible:
  out  ($be), a
  push hl
  inc  hl                    ; -> tile
  inc  hl                    ; -> attr
  ld   a, (hl)
  ld   (de), a               ; attr for this compacted SAT entry
  inc  de
  pop  hl
_sat_y_skip:
  inc  hl
  inc  hl
  inc  hl
  inc  hl
  djnz _sat_y_loop

  ; End-of-list terminator. If all 64 sprites were visible this writes just past
  ; the Y table into the unused $3F40 gap, which is harmless; otherwise it hides
  ; all stale SAT entries after the compacted visible list.
  ld   a, $d0
  out  ($be), a

  ; ── Phase 2: compact visible (X, resolved tile) pairs to VRAM $3F80 ───────
  ld   a, $80
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

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

  pop  de
  pop  bc
  pop  hl
  pop  af
  ret

.ends
