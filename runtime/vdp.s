; vdp.s — SMS VDP helper routines.
;
; All VDP access goes through the two I/O ports:
;   $BE  data port  (reads/writes VRAM or CRAM data)
;   $BF  control port (reads status; writes address/register commands)
;
; VRAM write address command: write low byte, then (high byte | $40).
; CRAM write address command: write low byte, then ($C0 | high_bits).
; Register write command:     write value byte, then ($80 | reg_num).
;
; The VDP has 11 writable registers (0..10).

.section "vdp" free

; ─── vdp_init ─────────────────────────────────────────────────────────────────
; Writes the default Mode 4 register set.  Display is left off (reg 1 bit 6 = 0)
; so the caller can finish loading assets before enabling it.
vdp_init:
  ; Table of (reg, value) pairs, terminated by a register-number sentinel.
  ; We check the *register* byte (B) for $FF rather than the value byte —
  ; several registers legitimately take $FF as their value, so testing the
  ; value would terminate the loop early.
  ld  hl, _vdp_init_table
_vdp_init_loop:
  ld  a, (hl)               ; register number
  cp  $ff                   ; sentinel?
  ret z
  ld  b, a                  ; B = reg number
  inc hl
  ld  a, (hl)               ; value
  inc hl
  call vdp_set_register
  jr  _vdp_init_loop

_vdp_init_table:
  ; reg 0: $66 = %01100110
  ;   bit 6 = 1  lock horizontal scroll for rows 0-1 (coarse NES HUD/sprite-0 split)
  ;   bit 5 = 1  mode bit M4 (selects Mode 4)
  ;   bit 2 = 1  hide left-column (prevents scroll artifacts)
  ;   bit 1 = 1  mode bit M2
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ; Coherent source packets have no legacy HUD lock/default left clip. Set
  ; their stable R0 before later enabling224-line M1 in packet initialization.
  ; Besides avoiding a spurious initial split, this keeps a same-frame R0
  ; change from masking the height notification in GPGX162c343's frontend.
  .db 0, $06
.else
  .db 0, $66
.endif
  ; reg 1: $A0 = %10100000
  ;   bit 7 = 1  VBlank/frame interrupt enable
  ;   bit 6 = 0  display disabled (enabled after asset load in boot_main)
  ;   bit 5 = 1  mode bit M1
  ;   bit 1 = 0  8×8 sprites (bit 1 = 1 would select 8×16)
  .db 1, $a0
  ; reg 2: $FF — name table base.
  ;   In 224/240-line Mode 4, bits 3..2 select the base plus a $700 offset;
  ;   $FF therefore selects VRAM $3700. ($3800 is the 192-line interpretation.)
  .db 2, $ff
  ; reg 3: $FF — color table base (unused in Mode 4, set to all-ones by convention).
  .db 3, $ff
  ; reg 4: $FF — pattern generator table base (unused in Mode 4, all-ones).
  .db 4, $ff
  ; reg 5: $FF — sprite attribute table (SAT) base.
  ;   Bits 6..1 of $FF place the SAT at VRAM $3F00 in Mode 4.
  .db 5, $ff
  ; reg 6: $FF — sprite pattern generator base.
  ;   bit 2 = 1 selects $2000 as sprite tile base. The SMB CHR pack keeps NES
  ;   sprite table-0 tiles in physical slots $100-$1BF, which become sprite
  ;   tile bytes $00-$BF relative to the $2000 base.
  .db 6, $ff
  ; reg 7: $00 — border color = palette entry 0.
  .db 7, $00
  ; reg 8: $00 — horizontal scroll offset = 0.
  .db 8, $00
  ; reg 9: $00 — vertical scroll offset = 0.
  .db 9, $00
  ; reg 10: $FF — line interrupt counter (disabled; $FF means never fire).
  .db 10, $ff
  ; Sentinel.
  .db $ff, $ff

; ─── vdp_set_register ─────────────────────────────────────────────────────────
; Entry: A = value to write, B = register index (0..10).
; Writes the two-byte register command to the control port $BF.
; Preserves: A, B. Clobbers: C. Stackless for IRQ/presentation hot paths.
vdp_set_register:
  ld   c, a
  out  ($bf), a             ; first byte = value
  ld   a, b
  or   $80                  ; second byte = $80 | register number
  out  ($bf), a
  ld   a, c
  ret

; ─── vdp_set_vram_addr ────────────────────────────────────────────────────────
; Sets VRAM write address.
; Entry: A = low byte of VRAM address, D = high byte (bits 5..0 used).
; The write-address flag is OR'd into the high byte ($40).
; Clobbers: none.
vdp_set_vram_addr:
  push af
  out  ($bf), a             ; send low byte first
  ld   a, d
  and  $3f                  ; mask to 6 bits
  or   $40                  ; set write-address flag
  out  ($bf), a
  pop  af
  ret

; ─── vdp_set_cram_addr ────────────────────────────────────────────────────────
; Sets CRAM write address.
; Entry: A = CRAM address (0..31).
; The CRAM-write flag is $C0 OR'd into the second byte.
; Clobbers: none.
vdp_set_cram_addr:
  push af
  out  ($bf), a             ; low byte = address (0..31, fits in one byte)
  ld   a, $c0               ; second byte = $C0 means "write CRAM"
  out  ($bf), a
  pop  af
  ret

; ─── vdp_write_block ──────────────────────────────────────────────────────────
; Writes BC bytes from memory at HL to the VDP data port $BE.
; The caller must have set the VRAM or CRAM write address beforehand.
; Entry: HL = source pointer, BC = byte count.
; Clobbers: HL, BC.  Preserves AF.
vdp_write_block:
  push af
  ld   a, b
  or   c
  jr   z, _vdp_write_done   ; nothing to write
_vdp_write_loop:
  ld   a, (hl)
  out  ($be), a
  inc  hl
  dec  bc
  ld   a, b
  or   c
  jr   nz, _vdp_write_loop
_vdp_write_done:
  pop  af
  ret

; ─── vdp_read_block ───────────────────────────────────────────────────────────
; Reads BC bytes from the VDP data port $BE into memory at HL.
; The caller must have set the VRAM read address beforehand
; (read address: write low byte, then high byte without $40/$C0 flags).
; Entry: HL = destination pointer, BC = byte count.
; Clobbers: HL, BC.  Preserves AF.
; NOTE: For v1 this is not called from translated code; included for completeness.
vdp_read_block:
  push af
  ld   a, b
  or   c
  jr   z, _vdp_read_done
_vdp_read_loop:
  in   a, ($be)
  ld   (hl), a
  inc  hl
  dec  bc
  ld   a, b
  or   c
  jr   nz, _vdp_read_loop
_vdp_read_done:
  pop  af
  ret

; ─── vdp_clear_vram ───────────────────────────────────────────────────────────
; Fills all 16 KB of VRAM with $00.
; SMS VRAM is 16 KB ($0000-$3FFF).
vdp_clear_vram:
  ; Set VRAM write address to $0000.
  push af
  push bc
  push hl
  xor  a
  ld   d, a
  call vdp_set_vram_addr
  ; Write 16384 ($4000) zero bytes.
  ld   hl, $4000            ; loop counter
_vdp_clr_vram_loop:
  xor  a
  out  ($be), a
  dec  hl
  ld   a, h
  or   l
  jr   nz, _vdp_clr_vram_loop
  pop  hl
  pop  bc
  pop  af
  ret

; ─── vdp_clear_cram ───────────────────────────────────────────────────────────
; Fills all 32 bytes of CRAM with $00 (black palette).
vdp_clear_cram:
  push af
  push bc
  ; Set CRAM write address to 0.
  xor  a
  call vdp_set_cram_addr
  ; Write 32 zero bytes.
  ld   b, 32
_vdp_clr_cram_loop:
  xor  a
  out  ($be), a
  djnz _vdp_clr_cram_loop
  pop  bc
  pop  af
  ret

.ends
