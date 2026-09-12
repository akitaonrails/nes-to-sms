; wram.s — NES cartridge work-RAM ($6000-$7FFF) backed by SMS cartridge SRAM.
;
; MMC1/MMC3-class boards carry battery-backed PRG-RAM at $6000-$7FFF (e.g.
; Zelda's save file plus its live working set). The SMS's 8 KiB on-board RAM is
; already fully committed (NES RAM at $C000, plus the runtime's shadows/pools),
; so the 8 KiB NES WRAM lives in cartridge SRAM instead.
;
; CHR-RAM presentation uses SRAM bank 0 ($FFFC = $08, RAW_CIRAM_SRAM_CTRL);
; WRAM uses the SECOND 16 KiB SRAM bank ($FFFC = $0C: RAM-enable + bank-select),
; so the two never overlap. Each access brackets the SRAM window: save the
; current slot-2 mapper control (the $FFFC register is the top of work RAM and
; reads back its last written value), map SRAM bank 1 into slot 2, do the
; access at $8000 + (addr - $6000), then restore the saved control so the next
; slot-2 ROM/data read sees its expected bank.
;
; NES $6000-$7FFF -> SMS slot-2 SRAM $8000-$9FFF: high byte $60-$7F maps to
; $80-$9F (subtract $60, add $80). All 8 KiB of the range is covered.

.define WRAM_SRAM_CTRL $0c    ; $FFFC: RAM enable (bit3) + SRAM bank 1 (bit2)

.section "wram" free

; rt_wram_write — store B into NES WRAM[HL].
;   Entry: HL = NES address ($6000-$7FFF), B = value.
;   Preserves AF, BC, DE, HL (and all flags — 6502 STA touches no flags).
rt_wram_write:
  push hl
  push af
  ld   a, ($fffc)            ; current slot-2 mapper control
  push af                    ; stash it on the stack (keeps BC intact)
  ld   a, WRAM_SRAM_CTRL
  ld   ($fffc), a            ; SRAM bank 1 -> slot 2
  ld   a, h
  sub  $60                   ; $60-$7F -> $00-$1F
  add  a, $80                ; -> $80-$9F : slot-2 SRAM $8000-$9FFF
  ld   h, a
  ld   (hl), b               ; WRAM[addr] = value
  pop  af                    ; A = saved control
  ld   ($fffc), a            ; restore slot-2 mapper control
  pop  af
  pop  hl
  ret

; rt_wram_read — load NES WRAM[HL] into A.
;   Entry: HL = NES address ($6000-$7FFF).
;   Exit:  A = byte. Preserves BC, DE, HL and the caller's flags (6502 LDA sets
;          only N/Z, which the caller re-derives from A afterwards).
rt_wram_read:
  push hl
  push bc
  push af                    ; preserve caller flags; A is overwritten below
  ld   a, ($fffc)
  ld   c, a
  ld   a, WRAM_SRAM_CTRL
  ld   ($fffc), a
  ld   a, h
  sub  $60
  add  a, $80
  ld   h, a
  ld   b, (hl)               ; B = WRAM[addr]
  ld   a, c
  ld   ($fffc), a            ; restore control
  pop  af                    ; restore caller flags (A discarded)
  ld   a, b                  ; A = value (LD does not disturb flags)
  pop  bc
  pop  hl
  ret

.ends
