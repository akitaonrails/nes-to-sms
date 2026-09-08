; Synthetic-only CNROM source bus. No rendering or guest IRQ/NMI scheduler.
; Fixed PRG, independent CHR8K latch, and mirrored 2K optional cartridge RAM.
; Public bus ABI: HL=raw NES effective address, A=written/read byte.
; Preserve BC/DE/HL/IFF/shadow P and exact slot2 ROM/SRAM controls.
.ifdef CNROM_BUS_EXPERIMENT
.define CN_CHR $c810
.define CN_PPU_LATCH $c811
.define CN_RESULT $c812
.define CN_T_HI $c814
.define CN_T_LO $c815
.define CN_FINE_X $c816
.define CN_PALETTE $c820
.define CN_UNSUPPORTED $e8

.bank 0 slot 0
.section "cpu_bus_cnrom" free
rt_cnrom_init:
  ; Explicit deterministic experiment power-on choice. Soft reset is separate
  ; and never rewrites this latch. Model tests cover other power-on states.
  xor a
  ld hl, CN_CHR
  ld bc, $30
  call mem_fill
.if CNROM_PRG_RAM == 1
  ld a, $0c
  ld ($fffc), a
  ld hl, $8000
  ld bc, $800
  xor a
  call mem_fill
  xor a
  ld ($fffc), a
.endif
  ret

rt_cpu_read_bus:
  push bc
  push de
  push hl
  ld a, i
  push af
  di
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  call _cn_read_locked
  ld (CN_RESULT), a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop af
  ld a, (CN_RESULT)
  pop hl
  pop de
  pop bc
  jp po, _cn_read_di
  ei
_cn_read_di:
  ret

rt_cpu_write_bus:
  push bc
  push de
  push hl
  push af
  ld c, a
  ld a, i
  push af
  di
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  ld a, c
  call _cn_write_locked
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop af
  jp po, _cn_write_di
  pop af
  pop hl
  pop de
  pop bc
  ei
  ret
_cn_write_di:
  pop af
  pop hl
  pop de
  pop bc
  ret

_cn_read_locked:
  ld a, h
  cp $20
  jr c, _cn_read_ram
  cp $40
  jp c, _cn_ppu_read
  cp $60
  jr c, _cn_read_io
  cp $80
  jr c, _cn_read_cart
_cn_read_prg:
  xor a
  ld ($fffc), a
  ld a, h
  cp $c0
  ld a, :data_prg_low
  jr c, _cn_prg_map
  ld a, h
  sub $40
  ld h, a
  ld a, :data_prg_high
_cn_prg_map:
  ld ($ffff), a
  ld a, (hl)
  ret
_cn_read_ram:
  and 7
  or $c0
  ld h, a
  ld a, (hl)
  ret
_cn_read_io:
  ld a, h
  cp $40
  jp nz, rt_cnrom_unsupported
  ld a, l
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
  jp rt_cnrom_unsupported ; no timed APU/controller/open-bus contract yet
.endif
  cp $15
  jp z, rt_apu_read
  ; Partially driven controller bits require CPU fetch/open-bus provenance.
  ; Do not claim the legacy constant-upper-bit approximation here.
  jp rt_cnrom_unsupported
_cn_read_cart:
.if CNROM_PRG_RAM == 1
  call _cn_cart_address
  ld a, (hl)
  ret
.else
  ; The translated bus lacks opcode/dummy-fetch open-bus provenance.
  jp rt_cnrom_unsupported
.endif

_cn_write_locked:
  ld b, a
  ld a, h
  cp $20
  jr c, _cn_write_ram
  cp $40
  jp c, _cn_ppu_write
  cp $60
  jr c, _cn_write_io
  cp $80
  jr c, _cn_write_cart
  ld a, b
rt_cnrom_write_register:
  ; Locked mapper event entry: A=B=written value, HL=pre-write CPU address.
.if CNROM_BUS_CONFLICTS == 1
  push bc
  call _cn_read_prg
  pop bc
  and b
.else
  ld a, b
.endif
  and CNROM_CHR_BANK_MASK
  ld (CN_CHR), a
  ret
_cn_write_ram:
  and 7
  or $c0
  ld h, a
  ld (hl), b
  ret
_cn_write_cart:
.if CNROM_PRG_RAM == 1
  call _cn_cart_address
  ld (hl), b
.endif
  ; No RAM chip: an undriven write has no board effect.
  ret
_cn_write_io:
  ld a, h
  cp $40
  jp nz, rt_cnrom_unsupported
  ld a, l
  cp $18
  jp nc, rt_cnrom_unsupported
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
  cp $14
  jp z, rt_source_dma_request
  jp rt_cnrom_unsupported
.endif
  cp $16
  jr z, _cn_strobe
  cp $14
  jr z, _cn_dma
  ld a, b
  jp rt_apu_write
_cn_strobe:
  ld a, b
  jp rt_controller_strobe
_cn_dma:
  ld h, b
  ld l, 0
  ld b, 0
_cn_dma_byte:
  call rt_cpu_read_bus
  ld c, a
  ld a, ($cb0a)
  ld e, a
  ld d, $c9
  ld a, c
  ld (de), a
  inc e
  ld a, e
  ld ($cb0a), a
  inc hl
  djnz _cn_dma_byte
  ret

_cn_cart_address:
  ld a, h
  and 7
  or $80
  ld h, a
  ld a, $0c
  ld ($fffc), a
  ret

; Source read at HL=physical PPU address0..3FFF. All external mappings belong
; to the enclosing CPU bus transaction, not presentation or live SMS VRAM.
_cn_ppu_source_read:
  ld a, h
  cp $20
  jr nc, _cn_source_nt
  ld a, (CN_CHR)
  srl a
  add a, CNROM_CHR_DATA_BASE
  ld ($ffff), a
  ld a, (CN_CHR)
  and 1
  rrca
  rrca
  rrca
  or h
  or $80
  ld h, a
  xor a
  ld ($fffc), a
  ld a, (hl)
  ret
_cn_source_nt:
  call _cn_ciram_address
  ld a, (hl)
  ret
_cn_ciram_address:
  ld a, h
.ifdef NES_MIRRORING_VERTICAL
  and 7
.else
  and 3
  ld c, a
  ld a, h
  and 8
  srl a
  or c
.endif
  or $80
  ld h, a
  ld a, 8
  ld ($fffc), a
  ret

_cn_ppu_read:
  ld a, l
  and 7
  cp 2
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
  jp z, rt_source_status_read
.else
  jp z, rt_cnrom_unsupported ; no fabricated vblank/sprite0 timing
.endif
  cp 4
  jr z, _cn_oam_read
  cp 7
  jr z, _cn_data_read
  ld a, (CN_PPU_LATCH)
  ret
_cn_oam_read:
  ld a, ($cb0a)
  ld l, a
  ld h, $c9
  ld a, (hl)
  ld (CN_PPU_LATCH), a
  ret
_cn_data_read:
  call _cn_ppu_address
  ld a, h
  cp $3f
  jr nc, _cn_palette_read
  ld a, ($cb11)
  push af
  call _cn_ppu_source_read
  ld ($cb11), a
  call _cn_ppu_increment
  pop af
  ld (CN_PPU_LATCH), a
  ret
_cn_palette_read:
  push hl
  call _cn_palette_address
  ld a, (hl)
  ld c, a
  ld a, ($cb09)
  and 1
  ld a, c
  jr z, _cn_palette_color
  and $30
_cn_palette_color:
  ld c, a
  ld a, (CN_PPU_LATCH)
  and $c0
  or c
  ld (CN_PPU_LATCH), a
  pop hl
  ld a, h
  sub $10
  ld h, a
  call _cn_ppu_source_read
  ld ($cb11), a
  call _cn_ppu_increment
  ld a, (CN_PPU_LATCH)
  ret

_cn_ppu_write:
  ld a, b
  ld (CN_PPU_LATCH), a
  ld a, l
  and 7
  or a
  jr z, _cn_ctrl_write
  cp 1
  jr z, _cn_mask_write
  cp 2
  ret z
  cp 3
  jr z, _cn_oam_address
  cp 4
  jr z, _cn_oam_write
  cp 5
  jp z, _cn_scroll_write
  cp 6
  jp z, _cn_address_write
  jp _cn_data_write
_cn_ctrl_write:
.ifndef CNROM_SOURCE_CLOCK_EXPERIMENT
  bit 7, b
  jp nz, rt_cnrom_unsupported ; guest NMI scheduling is a later contract
.endif
  ld a, b
  ld ($cb08), a
  and 3
  rlca
  rlca
  ld c, a
  ld a, (CN_T_HI)
  and $73
  or c
  ld (CN_T_HI), a
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
  jp rt_source_nmi_line
.endif
  ret
_cn_mask_write:
  ld a, b
  and $18
  jp nz, rt_cnrom_unsupported ; synthetic-only, no misleading static graphics
  ld a, b
  ld ($cb09), a
  ret
_cn_oam_address:
  ld a, b
  ld ($cb0a), a
  ret
_cn_oam_write:
  ld a, ($cb0a)
  ld l, a
  ld h, $c9
  ld (hl), b
  inc a
  ld ($cb0a), a
  ret
_cn_scroll_write:
  ld a, ($cb0e)
  xor 1
  ld ($cb0e), a
  jr z, _cn_scroll_second
  ld a, b
  and 7
  ld (CN_FINE_X), a
  ld a, b
  srl a
  srl a
  srl a
  ld c, a
  ld a, (CN_T_LO)
  and $e0
  or c
  ld (CN_T_LO), a
  ret
_cn_scroll_second:
  ld a, b
  and $38
  rlca
  rlca
  ld c, a
  ld a, (CN_T_LO)
  and $1f
  or c
  ld (CN_T_LO), a
  ld a, b
  and 7
  rlca
  rlca
  rlca
  rlca
  ld c, a
  ld a, b
  and $c0
  rlca
  rlca
  or c
  ld c, a
  ld a, (CN_T_HI)
  and $0c
  or c
  ld (CN_T_HI), a
  ret
_cn_address_write:
  ld a, ($cb0e)
  xor 1
  ld ($cb0e), a
  jr z, _cn_address_second
  ld a, b
  and $3f
  ld (CN_T_HI), a
  ret
_cn_address_second:
  ld a, b
  ld (CN_T_LO), a
  ld ($cb10), a
  ld a, (CN_T_HI)
  ld ($cb0f), a
  ret
_cn_data_write:
  call _cn_ppu_address
  ld a, h
  cp $20
  jr c, _cn_data_increment ; CHR-ROM ignores writes
  cp $3f
  jr nc, _cn_palette_write
  call _cn_ciram_address
  ld (hl), b
  jr _cn_data_increment
_cn_palette_write:
  call _cn_palette_address
  ld a, b
  and $3f
  ld (hl), a
_cn_data_increment:
  jp _cn_ppu_increment
_cn_palette_address:
  ld a, l
  and $1f
  ld c, a
  and 3
  ld a, c
  jr nz, _cn_palette_index
  and $0f
_cn_palette_index:
  add a, <CN_PALETTE
  ld l, a
  ld h, >CN_PALETTE
  ret
_cn_ppu_address:
  ld a, ($cb0f)
  and $3f
  ld h, a
  ld a, ($cb10)
  ld l, a
  ret
_cn_ppu_increment:
  ld a, ($cb0f)
  ld h, a
  ld a, ($cb10)
  ld l, a
  ld a, ($cb08)
  and 4
  ld a, 1
  jr z, _cn_increment_ready
  ld a, 32
_cn_increment_ready:
  add a, l
  ld ($cb10), a
  ld a, h
  adc a, 0
  and $7f
  ld ($cb0f), a
  ret

; NMOS JMP(indirect) wraps the second pointer read within its CPU page.
rt_cpu_indirect_jump:
  push af
  call rt_cpu_read_bus
  ld c, a
  inc l
  call rt_cpu_read_bus
  ld b, a
  pop af
  jp rt_banked_tail_dispatch

rt_cnrom_unsupported:
  di
  ld a, CN_UNSUPPORTED
  ld ($cb1d), a
  jp rt_unresolved_jsr_flash
.ends
.endif
