; MMC3 physical 8 KiB PRG banking. The bounded experiment traps unsupported
; CHR/IRQ effects; MMC3_FULL_RUNTIME routes them to the explicit graphics
; adapter and adds an effective CPU bus with separate cartridge SRAM bank1.
; Raw PRG pairs occupy 16 KiB SMS banks at DATA_BASE. Helpers restore mappings.
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
.define M3_IRQ_ENABLED $c822
.define M3_IRQ_PENDING $c823
.define M3_MIRRORING $c824
.define M3_UNSUPPORTED $e8
.define M3_BAD_ADDRESS $e9

.bank 0 slot 0
.section "mapper_mmc3" free
rt_mmc3_init:
  xor a
  ld hl, M3_SELECT
.ifdef MMC3_FULL_RUNTIME
  ld b, $20
.else
  ld b, $12
.endif
_m3_init_clear:
  ld (hl), a
  inc hl
  djnz _m3_init_clear
.ifdef MMC3_FULL_RUNTIME
.ifndef NES_MIRRORING_VERTICAL
  ld a, 1
  ld (M3_MIRRORING), a
.endif
.endif
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
.ifdef MMC3_FULL_RUNTIME
  bit 0, l
  jr nz, _m3_write_irq_enable
  xor a
  ld (M3_IRQ_ENABLED), a
  ld (M3_IRQ_PENDING), a
  ld hl, $e000
  jr _m3_irq_callback
_m3_write_irq_enable:
  ld a, 1
  ld (M3_IRQ_ENABLED), a
  ld hl, $e001
  jr _m3_irq_callback
.else
  bit 0, l
  jp nz, _m3_unsupported       ; $E001 enables IRQs: no fake scanline clock
  jr _m3_write_done           ; $E000 disables/acknowledges an inactive IRQ
.endif
_m3_write_bank:
  bit 0, l
  jr nz, _m3_write_data
  ld a, b
.ifndef MMC3_FULL_RUNTIME
  bit 7, a
  jp nz, _m3_unsupported       ; CHR inversion needs a coherent CHR renderer
.endif
  ld (M3_SELECT), a
  call _m3_update_prg
.ifdef MMC3_FULL_RUNTIME
  call rt_mmc3_chr_changed
.endif
  jr _m3_write_done
_m3_write_data:
  ld a, (M3_SELECT)
  and 7
.ifndef MMC3_FULL_RUNTIME
  cp 6
  jp c, _m3_unsupported        ; CHR banks are not silently ignored
.endif
  add a, M3_REGS & $ff
  ld l, a
  ld h, M3_REGS >> 8
  ld (hl), b
  call _m3_update_prg
.ifdef MMC3_FULL_RUNTIME
  call rt_mmc3_chr_changed
.endif
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
.ifdef MMC3_FULL_RUNTIME
  ld (M3_MIRRORING), a
  call rt_mmc3_mirroring_changed
.else
.ifdef NES_MIRRORING_VERTICAL
  jp nz, _m3_unsupported
.else
  jp z, _m3_unsupported
.endif
.endif
  jr _m3_write_done           ; unchanged mirroring needs no rendering work
_m3_write_irq_latch:
  bit 0, l
  jr nz, _m3_write_irq_reload
  ld a, b
  ld (M3_IRQ_LATCH), a
.ifdef MMC3_FULL_RUNTIME
  ld hl, $c000
  jr _m3_irq_callback
.else
  jr _m3_write_done
.endif
_m3_write_irq_reload:
  ld a, 1
  ld (M3_IRQ_RELOAD), a
.ifdef MMC3_FULL_RUNTIME
  ld hl, $c001
_m3_irq_callback:
  ld a, b
  call rt_mmc3_irq_changed
.endif
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
.ifdef MMC3_FULL_RUNTIME
; A raw PLA may pop local PHA data, but not steal a live JSR return pair.
; Profiled consumption transfers the software owner before reaching PLA.
; BC identifies the source PLA for the existing unresolved-PC diagnostics.
rt_mmc3_guard_pla:
  push af
  push bc
  push hl
  ld hl, ($cb76)
  ld a, h
  cp $d3
  jr nz, _m3_pla_later_segment
  ld a, l
  or a
  jr z, _m3_pla_done          ; no software caller (reset or interrupt sentinel)
_m3_pla_later_segment:
  ld a, h
  cp $d5
  jr nz, _m3_pla_regular
  ld a, l
  or a
  jr nz, _m3_pla_regular
  ld hl, $d3fc
_m3_pla_regular:
  dec hl                    ; bank
  dec hl                    ; continuation high / owner flag
  bit 7, (hl)
  jr z, _m3_pla_done
  dec hl
  dec hl                    ; saved guest S
  ld a, ($cb02)
  cp (hl)
  jr nz, _m3_pla_done
  ld ($cb1b), bc
  jp rt_mmc3_unsupported
_m3_pla_done:
  pop hl
  pop bc
  pop af
  ret

; JMP(indirect) reads the high byte from the same CPU page (NMOS wrap bug),
; using the live mapper for both reads, then dispatches the resulting NES PC.
rt_mmc3_indirect_jump:
  push af
  call rt_mmc3_read_bus
  ld c, a
  inc l
  call rt_mmc3_read_bus
  ld b, a
  pop af
  jp rt_banked_tail_dispatch

; Verified cooperative wait: HL=guest tick mirror, BC=enable mirror.
; Publish intent atomically, preserving all registers/P/IFF and mappings.
; The scheduler consumes C825 before entering the actual guest handlers.
rt_mmc3_wait_boundary:
  push af
  ld a, i
  push af
  di
  ld a, (bc)
  or a
  jr z, _m3_wait_done
  bit 7, (hl)
  jr nz, _m3_wait_done
  ld a, 1
  ld ($c825), a
_m3_wait_done:
  pop af
  jp po, _m3_wait_di
  pop af
  ei
  ret
_m3_wait_di:
  pop af
  ret

; Effective CPU-bus operations. HL is a raw NES address; A is the written or
; read byte. Preserve BC/DE/HL, entry IFF, shadow P and exact mapper controls.
rt_mmc3_read_bus:
  push bc
  push de
  push hl
  ld a, i
  push af
  di
  call _m3_read_bus_locked
  ld c, a
  pop af
  ld a, c
  pop hl
  pop de
  pop bc
  jp po, _m3_bus_return_di
  ei
  ret
rt_mmc3_write_bus:
  push bc
  push de
  push hl
  ld c, a
  ld a, i
  push af
  di
  ld a, c
  call _m3_write_bus_locked
  ld c, a
  pop af
  ld a, c
  pop hl
  pop de
  pop bc
  jp po, _m3_bus_return_di
  ei
_m3_bus_return_di:
  ret

_m3_read_bus_locked:
  ld a, h
  cp $20
  jr c, _m3_bus_read_ram
  cp $40
  jr c, _m3_bus_read_ppu
  cp $60
  jr c, _m3_bus_read_io
  cp $80
  jr c, _m3_bus_read_cart
  ; Renderer/DMA callers may temporarily map data independently of CB14.
  ; The full CPU bus restores the exact entry slot, not only its code shadow.
  ld a, ($fffe)
  push af
  call rt_mmc3_read_prg
  ld b, a
  pop af
  ld ($fffe), a
  ld a, b
  ret
_m3_bus_read_ram:
  and 7
  or $c0
  ld h, a
  ld a, (hl)
  ret
_m3_bus_read_ppu:
  ld a, l
  and 7
  ld b, a
  jp rt_ppu_read
_m3_bus_read_io:
  ld a, h
  cp $40
  jp nz, rt_mmc3_unsupported
  ld a, l
  cp $15
  jp z, rt_apu_read
  cp $16
  jr z, _m3_bus_read_pad
  cp $17
  jp nz, rt_mmc3_unsupported
_m3_bus_read_pad:
  jp rt_controller_read
_m3_bus_read_cart:
  ld a, (M3_RAM_PROTECT)
  bit 7, a
  jp z, rt_mmc3_unsupported ; disabled reads are open bus, not invented RAM
  ld a, h
  and $1f
  or $80
  ld h, a
  ld a, ($fffc)
  push af
  ld a, $0c
  ld ($fffc), a
  ld b, (hl)
  pop af
  ld ($fffc), a
  ld a, b
  ret

_m3_write_bus_locked:
  ld b, a
  ld a, h
  cp $20
  jr c, _m3_bus_write_ram
  cp $40
  jr c, _m3_bus_write_ppu
  cp $60
  jr c, _m3_bus_write_io
  cp $80
  jr c, _m3_bus_write_cart
  ld a, b
  jp rt_mmc3_write
_m3_bus_write_ram:
  and 7
  or $c0
  ld h, a
  ld (hl), b
  ld a, b
  ret
_m3_bus_write_ppu:
  ld a, l
  and 7
  ld c, b
  ld b, a
  ld a, c
  jp rt_ppu_write
_m3_bus_write_io:
  ld a, h
  cp $40
  jp nz, rt_mmc3_unsupported
  ld a, l
  cp $18
  jp nc, rt_mmc3_unsupported
  cp $16
  jr z, _m3_bus_strobe
  cp $14
  jr z, _m3_bus_dma
  ld a, b
  jp rt_apu_write
_m3_bus_strobe:
  ld a, b
  jp rt_controller_strobe
_m3_bus_dma:
  ld a, b
  jp rt_oam_dma
_m3_bus_write_cart:
  ld a, (M3_RAM_PROTECT)
  and $c0
  cp $80
  jr nz, _m3_bus_cart_write_ignored
  ld a, h
  and $1f
  or $80
  ld h, a
  ld a, ($fffc)
  push af
  ld a, $0c
  ld ($fffc), a
  ld (hl), b
  pop af
  ld ($fffc), a
_m3_bus_cart_write_ignored:
  ld a, b
  ret
.endif
.ends
.endif
