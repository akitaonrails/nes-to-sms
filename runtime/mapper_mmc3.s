; Experimental executable 8 KiB PRG banking. This deliberately does not claim
; CHR banking, cartridge RAM or qualified MMC3 IRQ support: active unsupported
; register effects trap. Raw PRG pairs occupy 16 KiB SMS banks at DATA_BASE.
; Slot 2 stays canonical; all PRG reads are interrupt-atomic slot-1 reads.
.ifdef NES_MMC3
.define M3_SELECT $c810
.define M3_REGS $c811
.define M3_WINDOW_8000 $c819
.define M3_WINDOW_A000 $c81a
.define M3_WINDOW_C000 $c81b
.define M3_WINDOW_E000 $c81c
.define M3_RAM_PROTECT $c81e
.define M3_IRQ_LATCH $c820
.define M3_IRQ_RELOAD $c821
.define M3_UNSUPPORTED $e8
.define M3_BAD_ADDRESS $e9

.bank 0 slot 0
.section "mapper_mmc3" free
rt_mmc3_init:
  xor a
  ld hl, M3_SELECT
  ld b, $12
_m3_init_clear:
  ld (hl), a
  inc hl
  djnz _m3_init_clear
  jp _m3_update_prg

; A = NES address high byte; result = physical 8 KiB bank. Other registers
; survive. This helper is usable while slot 1 contains a dispatch table.
rt_mmc3_bank_for_a:
  cp $80
  jp c, _m3_bad_address
  cp $a0
  jr c, _m3_bank_8000
  cp $c0
  jr c, _m3_bank_a000
  cp $e0
  jr c, _m3_bank_c000
  ld a, (M3_WINDOW_E000)
  ret
_m3_bank_8000:
  ld a, (M3_WINDOW_8000)
  ret
_m3_bank_a000:
  ld a, (M3_WINDOW_A000)
  ret
_m3_bank_c000:
  ld a, (M3_WINDOW_C000)
  ret

; A = value, HL = full NES register address. Preserve AF/DE/shadow P and
; entry IFF2. No slot-2 transaction is opened or modified.
rt_mmc3_write:
  push af
  ld b, a
  ld a, i
  di
  push af
  ld a, h
  cp $80
  jp c, _m3_bad_address
  and $e0
  cp $80
  jr z, _m3_write_bank
  cp $a0
  jr z, _m3_write_ram
  cp $c0
  jr z, _m3_write_irq_latch
  bit 0, l
  jp nz, _m3_unsupported       ; $E001 enables IRQs: no fake scanline clock
  jr _m3_write_done           ; $E000 disables/acknowledges an inactive IRQ
_m3_write_bank:
  bit 0, l
  jr nz, _m3_write_data
  ld a, b
  bit 7, a
  jp nz, _m3_unsupported       ; CHR inversion needs a coherent CHR renderer
  ld (M3_SELECT), a
  call _m3_update_prg
  jr _m3_write_done
_m3_write_data:
  ld a, (M3_SELECT)
  and 7
  cp 6
  jp c, _m3_unsupported        ; CHR banks are not silently ignored
  add a, M3_REGS & $ff
  ld l, a
  ld h, M3_REGS >> 8
  ld (hl), b
  call _m3_update_prg
  jr _m3_write_done
_m3_write_ram:
  bit 0, l
  jr z, _m3_write_mirroring
  ld a, b
  ld (M3_RAM_PROTECT), a       ; experiment has no cartridge RAM chip
  jr _m3_write_done
_m3_write_mirroring:
  ld a, b
  and 1
.ifdef NES_MIRRORING_VERTICAL
  jp nz, _m3_unsupported
.else
  jp z, _m3_unsupported
.endif
  jr _m3_write_done           ; unchanged mirroring needs no rendering work
_m3_write_irq_latch:
  bit 0, l
  jr nz, _m3_write_irq_reload
  ld a, b
  ld (M3_IRQ_LATCH), a
  jr _m3_write_done
_m3_write_irq_reload:
  ld a, 1
  ld (M3_IRQ_RELOAD), a
_m3_write_done:
  pop af
  jp po, _m3_write_done_di
  pop af
  ei
  ret
_m3_write_done_di:
  pop af
  ret

_m3_update_prg:
  xor a
  ld ($ca0e), a               ; dispatch MRU invalid after mapping changes
  ld a, (M3_REGS + 7)
  and NES_MMC3_PRG_BANK_COUNT - 1
  ld (M3_WINDOW_A000), a
  ld a, NES_MMC3_PRG_BANK_COUNT - 1
  ld (M3_WINDOW_E000), a
  ld a, (M3_SELECT)
  bit 6, a
  jr nz, _m3_update_inverted
  ld a, (M3_REGS + 6)
  and NES_MMC3_PRG_BANK_COUNT - 1
  ld (M3_WINDOW_8000), a
  ld a, NES_MMC3_PRG_BANK_COUNT - 2
  ld (M3_WINDOW_C000), a
  ret
_m3_update_inverted:
  ld a, (M3_REGS + 6)
  and NES_MMC3_PRG_BANK_COUNT - 1
  ld (M3_WINDOW_C000), a
  ld a, NES_MMC3_PRG_BANK_COUNT - 2
  ld (M3_WINDOW_8000), a
  ret

; HL = effective NES PRG address. A = byte; C/DE and entry IFF survive.
; Raw data is mapped only while DI, and never publishes a data bank in CB14.
rt_mmc3_read_prg:
  ld a, h
  cp $80
  jr nc, _m3_read_rom
  ; Indexed $FFxx reads wrap through $FFFF into NES RAM, not SMS ROM.
  ; Other low addresses are not valid PRG helper inputs.
  cp $20
  jp nc, _m3_bad_address
  and $07
  or $c0
  ld h, a
  ld a, (hl)
  ret
_m3_read_rom:
  ld a, i
  di
  jp po, _m3_read_di
  call _m3_read_locked
  ei
  ret
_m3_read_di:
  jp _m3_read_locked
_m3_read_locked:
  ld a, h
  call rt_mmc3_bank_for_a
  ld b, a
  srl a
  add a, MMC3_PRG_DATA_BASE
  ld ($fffe), a
  ld a, b
  and 1
  rrca
  rrca
  rrca                       ; odd 8 KiB bank occupies slot-1 $6000-$7FFF
  or $40
  ld b, a
  ld a, h
  and $1f
  or b
  ld h, a
  ld b, (hl)
  ld a, ($cb14)
  ld ($fffe), a
  ld a, b
  ret

_m3_bad_address:
  ld a, M3_BAD_ADDRESS
  jr _m3_trap
_m3_unsupported:
rt_mmc3_unsupported:
  ld a, M3_UNSUPPORTED
_m3_trap:
  di
  ld ($cb1d), a
  jp rt_unresolved_jsr_flash
.ends
.endif
