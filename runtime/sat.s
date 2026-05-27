; sat.s — Sprite Attribute Table (SAT) staging and upload.
;
; SMS SAT layout in VRAM at $3F00:
;   $3F00-$3F3F  64 Y positions (1 byte each)
;   $3F40-$3F7F  unused gap
;   $3F80-$3FFF  64 (X, tile_number) pairs (2 bytes each) = 128 bytes
;
; The staging area in SMS RAM mirrors this layout:
;   $C900-$C93F  64 Y positions (1 byte each)
;   $C940-$C9BF  64 (X, tile_number) pairs (128 bytes)
;
; A sprite is hidden by setting its Y position to $D0 (208 decimal, below
; the visible SMS screen which is 192 lines tall — $C0 would be on-screen
; but $D0 is safely off).
;
; NES OAM layout (256 bytes at $C900-$C9FF as written by SMB via $2004):
;   Each entry is 4 bytes: [Y, tile, attr, X]
;   Byte 0: Y position (top of sprite minus 1 on NES)
;   Byte 1: tile index
;   Byte 2: attribute (palette, flip flags)
;   Byte 3: X position
;
; For v1 the staging area stores NES OAM format (64 × 4 = 256 bytes at $C900).
; rt_sat_upload converts on the fly: reads NES OAM entries, writes SMS SAT.
;
; NES-to-SMS Y coordinate adjustment:
;   NES sprite Y is (screen_y - 1); SMS sprite Y is screen_y directly.
;   So SMS_Y = NES_Y + 1.  Hide if result >= $D0.
;
; Horizontal flip and vertical flip:
;   NES attr byte: bit 6 = H-flip, bit 7 = V-flip, bits 1..0 = OAM palette.
;   SMS Mode 4 sprites have only X and tile-number bytes in the SAT. There are
;   no per-sprite flip/palette bits, so v1 ignores NES attr flags.
;
; TODO(Phase 3): Handle 8x16 sprites (NES PPU ctrl bit 5 = 1).
; TODO(Phase 3): Map NES OAM palette bits to SMS palette select.
; TODO(Phase 3): Honour H/V flip flags.

.section "sat" free

; ─── rt_oam_dma ───────────────────────────────────────────────────────────────
; Entry: A = high byte of source page in NES address space (i.e. NES $XX00).
; SMB writes A to $4014 to trigger a 256-byte DMA copy from $XX00 to OAM.
; We translate the copy: source is SMS RAM at $C000 + (A << 8), destination
; is the OAM staging buffer at $C900. The actual SMS SAT upload happens
; later via `rt_sat_upload` from the VBlank handler.
;
; NES RAM is only mapped at $0000-$07FF; SMB writes use pages $02 or $07.
; Higher pages are undefined here — accept and copy whatever is at the
; mirrored SMS RAM address.
rt_oam_dma:
  push af
  push hl
  push de
  push bc
  ld   h, a                 ; HL = (A << 8) in NES address space
  ld   l, $00
  ld   a, h
  add  a, $c0               ; remap to SMS RAM base $C000
  ld   h, a
  ld   de, $c900            ; OAM staging
  ld   bc, $0100            ; 256 bytes
  ldir
  pop  bc
  pop  de
  pop  hl
  pop  af
  ret

; ─── rt_sat_upload ────────────────────────────────────────────────────────────
; Copies sprite data from NES OAM staging at $C900 into SMS VRAM SAT at $3F00.
; Called from irq_handler during VBlank.
; Clobbers: AF, HL, BC, DE.
rt_sat_upload:
  push af
  push hl
  push bc
  push de

  ; ── Phase 1: Write 64 Y positions to VRAM $3F00 ──────────────────────────
  ; Set VDP write address to $3F00.
  ld   a, $00
  out  ($bf), a             ; low byte
  ld   a, $3f
  or   $40                  ; write-address flag
  out  ($bf), a             ; high byte | $40

  ld   hl, $c900            ; NES OAM base (Y is first byte of each 4-byte entry)
  ld   b, 64                ; 64 sprites
_sat_y_loop:
  ld   a, (hl)              ; NES Y (screen_y - 1 on NES, 0-based)
  inc  a                    ; SMS Y = NES Y + 1
  ; If Y >= $D0 hide the sprite.
  cp   $d0
  jr   c, _sat_y_visible
  ld   a, $d0
_sat_y_visible:
  out  ($be), a             ; write Y to VRAM
  inc  hl                   ; skip tile byte
  inc  hl                   ; skip attr byte
  inc  hl                   ; skip X byte
  inc  hl                   ; advance to next entry's Y byte
  djnz _sat_y_loop

  ; ── Phase 2: Write 64 (X, tile) pairs to VRAM $3F80 ──────────────────────
  ; Set VDP write address to $3F80.
  ld   a, $80
  out  ($bf), a             ; low byte = $80
  ld   a, $3f
  or   $40
  out  ($bf), a

  ld   hl, $c900            ; restart at OAM base
  ld   b, 64
_sat_xt_loop:
  ; X byte is the 4th byte (offset 3) of each OAM entry.
  inc  hl                   ; skip Y
  ld   d, (hl)              ; D = tile index
  inc  hl                   ; skip tile
  inc  hl                   ; skip attr
  ld   a, (hl)              ; A = X position
  inc  hl                   ; advance to next entry

  ; Write X byte to VRAM.
  out  ($be), a

  ; Write mapped tile number byte. SMS Mode 4 SAT does not have a high
  ; attribute byte; the CHR map is relative to the sprite base selected in VDP
  ; register 6.
  ld   a, d                 ; tile index (low byte)
  call rt_map_sprite_tile
  out  ($be), a

  djnz _sat_xt_loop

  pop  de
  pop  bc
  pop  hl
  pop  af
  ret

.ends
