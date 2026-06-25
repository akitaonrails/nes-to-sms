; sat.s — Sprite Attribute Table (SAT) staging and upload, with software
; horizontal/vertical sprite flipping.
;
; SMS SAT layout in VRAM at $3F00:
;   $3F00-$3F3F  64 Y positions (1 byte each)
;   $3F40-$3F7F  unused gap
;   $3F80-$3FFF  64 (X, tile_number) pairs (2 bytes each) = 128 bytes
;
; The staging area in SMS RAM mirrors the NES OAM layout (64 x 4 = 256 bytes
; at $C900-$C9FF as written by SMB via $2004 / $4014 DMA):
;   Each entry is 4 bytes: [Y, tile, attr, X]
;   Byte 2 (attr): bit 6 = H-flip, bit 7 = V-flip, bits 1..0 = OAM palette.
;
; A sprite is hidden by setting its Y position to $D0.
;
; NES-to-SMS Y coordinate adjustment: SMS_Y = NES_Y + 1, hide if >= $D0.
;
; ── Horizontal / vertical flip ────────────────────────────────────────────
; The SMS Mode 4 SAT has only X and tile-number bytes; there are no per-sprite
; flip bits, so the NES H/V flip attributes can't be honoured in hardware.
; Instead, rt_sat_resolve makes a first pass over OAM: for each sprite whose
; attr requests a flip (and whose mapped tile isn't the blank), it bit-mirrors
; the source tile's pattern (from ROM data_chr) into one of 16 VRAM scratch
; slots (424-439) and points the sprite at that scratch slot. The resolved SMS
; tile number for every sprite is stashed in a 64-byte table at $D400, which
; the SAT upload then streams out. Flip is driven purely off the NES attr byte,
; so the engine stays game-agnostic.
;
; Scratch budget is 16 distinct flipped sprites per frame; beyond that, extra
; flipped sprites fall back to the unmirrored tile (graceful, rarely hit).
;
; RAM scratch (free region above the $CC00-$D3FF nametable shadow):
;   $D400-$D43F  resolved SMS tile number per sprite (64 bytes)
;   $D460        scratch_next (next free flip scratch slot, 0..15)
;   $D461        current flip bits ($40 H, $80 V), do_flip temp
;   $D462-$D463  current flip source base address, do_flip temp
;   $D500-$D5FF  H-flip byte bit-reverse LUT (page-aligned), built at boot
;
; Sprite VRAM slot layout (relative to VDP sprite base reg6 = $2000):
;   0..166   packed sprite patterns ($00-$A6)
;   167      blank/transparent tile
;   168..183 16 flip scratch slots  (absolute VRAM $3500..$36E0)
;
; TODO(Phase 3): Handle 8x16 sprites (NES PPU ctrl bit 5 = 1).
; TODO(Phase 3): Map NES OAM palette bits to SMS palette select.

.define SAT_BLANK_REL    167
.define SAT_SCRATCH_BASE  168
.define SAT_SCRATCH_COUNT 16
.define SAT_RESOLVED      $d400
.define SAT_SCRATCH_NEXT  $d460
.define SAT_FLIP_BITS     $d461
.define SAT_FLIP_SRC      $d462
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

; ─── do_flip ────────────────────────────────────────────────────────────────
; Mirror one source sprite tile into a VRAM scratch slot.
;   Entry: A = scratch index (0..15), B = flip bits ($40 H, $80 V),
;          C = source sprite tile (relative, 0..166).
;   Writes 32 bytes to VRAM at $3500 + index*32.
;   Clobbers AF, BC, DE, HL. Leaves data_prg_low mapped in slot 2.
do_flip:
  push af                    ; save scratch index
  ld   a, b
  and  $c0
  ld   (SAT_FLIP_BITS), a    ; flip bits

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
  ld   (SAT_FLIP_SRC), hl

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
  ld   a, (SAT_FLIP_BITS)
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
  ld   hl, (SAT_FLIP_SRC)
  add  hl, de                ; HL = source row pointer
  ld   a, (SAT_FLIP_BITS)
  and  $40                   ; H-flip?
  jr   z, _dof_plain
  ld   d, $d5                ; LUT page high byte (SAT_HFLIP_LUT = $D500)
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  ld   e, a
  ld   a, (de)
  out  ($be), a
  jr   _dof_row_done
_dof_plain:
  ld   a, (hl)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  out  ($be), a
  inc  hl
  ld   a, (hl)
  out  ($be), a
_dof_row_done:
  inc  c
  djnz _dof_row

  ld   a, :data_prg_low      ; restore translated-data bank
  ld   ($ffff), a
  ret

; ─── flip_get_scratch ─────────────────────────────────────────────────────────
; Allocate a scratch slot, flip the source tile into it, and return the SMS
; tile number to use. Entry: C = source rel tile, B = flip bits.
; Exit: A = SMS sprite tile (relative). Falls back to the unflipped tile if the
; per-frame scratch pool is exhausted.
flip_get_scratch:
  ld   a, (SAT_SCRATCH_NEXT)
  cp   SAT_SCRATCH_COUNT
  jr   c, _fgs_alloc
  ld   a, c                  ; pool full -> unmirrored tile
  ret
_fgs_alloc:
  call do_flip               ; A = index, B = flip bits, C = source rel
  ld   a, (SAT_SCRATCH_NEXT)
  ld   c, a                  ; index just used
  inc  a
  ld   (SAT_SCRATCH_NEXT), a
  ld   a, c
  add  a, SAT_SCRATCH_BASE
  ret

; ─── rt_sat_resolve ───────────────────────────────────────────────────────────
; First SAT pass: resolve each sprite's SMS tile number into $D400, performing
; software flips into VRAM scratch as needed. Clobbers AF, BC, DE, HL.
rt_sat_resolve:
  xor  a
  ld   (SAT_SCRATCH_NEXT), a ; reset scratch pool for this frame
  ld   hl, $c900             ; OAM staging
  ld   de, SAT_RESOLVED
  ld   b, 64
_res_loop:
  push bc                    ; save sprite counter
  inc  hl                    ; -> tile
  ld   a, (hl)               ; A = NES tile
  inc  hl                    ; -> attr
  ld   b, (hl)               ; B = attr
  inc  hl                    ; -> X
  inc  hl                    ; -> next entry Y
  call rt_map_sprite_tile    ; A = mapped rel tile (preserves BC, DE, HL)
  ld   c, a                  ; C = rel tile (default resolved value)
  ld   a, b
  and  $c0                   ; H or V flip requested?
  jr   z, _res_store
  ld   a, c
  cp   SAT_BLANK_REL         ; blank tile? leave transparent, don't flip
  jr   z, _res_store
  push de
  push hl
  call flip_get_scratch      ; C = src rel, B = flip bits -> A = scratch rel
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
; resolving software flips first. Called from irq_handler during VBlank.
rt_sat_upload:
  push af
  push hl
  push bc
  push de

  call rt_sat_resolve

  ; ── Phase 1: 64 Y positions to VRAM $3F00 ────────────────────────────────
  ld   a, $00
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

  ld   hl, $c900             ; Y is first byte of each 4-byte entry
  ld   b, 64
_sat_y_loop:
  ld   a, (hl)               ; NES Y
  inc  a                     ; SMS Y = NES Y + 1
  cp   $d0
  jr   c, _sat_y_visible
  ld   a, $d0                ; hide off-screen
_sat_y_visible:
  out  ($be), a
  inc  hl
  inc  hl
  inc  hl
  inc  hl
  djnz _sat_y_loop

  ; ── Phase 2: 64 (X, resolved tile) pairs to VRAM $3F80 ───────────────────
  ld   a, $80
  out  ($bf), a
  ld   a, $3f
  or   $40
  out  ($bf), a

  ld   hl, $c900             ; OAM (for X)
  ld   de, SAT_RESOLVED      ; resolved tile numbers
  ld   b, 64
_sat_xt_loop:
  inc  hl                    ; skip Y
  inc  hl                    ; skip tile
  inc  hl                    ; skip attr
  ld   a, (hl)               ; A = X
  inc  hl                    ; advance to next entry
  out  ($be), a              ; write X
  ld   a, (de)               ; resolved SMS tile
  inc  de
  out  ($be), a              ; write tile
  djnz _sat_xt_loop

  pop  de
  pop  bc
  pop  hl
  pop  af
  ret

.ends
