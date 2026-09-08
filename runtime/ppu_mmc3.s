; MMC3 full-mode PPU source state. No producer operation writes SMS VRAM.
; C810..C82F belongs to mapper_mmc3.s. C830..C8FF belongs to this backend.
; Guest CIRAM uses SRAM bank0 $8000..87FF; cartridge RAM uses bank1 elsewhere.
; Every public PPU transaction restores exact SRAM control/slot2 bank/IFF/DE.
.ifdef MMC3_FULL_RUNTIME
.define M3G_DIRTY       $c830
.define M3G_BUSY        $c831
.define M3G_PHASE       $c832
.define M3G_READY       $c833
.define M3G_TEMP_HI     $c834
.define M3G_TEMP_LO     $c835
.define M3G_FINE_X      $c836
.define M3G_SPLIT       $c837
.define M3G_PRESENT_X   $c838
.define M3G_PRESENT_Y   $c839
.define M3G_HUD_X       $c83a
.define M3G_HUD_Y       $c83b
.define M3G_PPU_BUS     $c83f
.define M3G_CHR_MAP     $c850 ; eight resolved physical 1 KiB pages
.define M3G_PALETTE     $c860 ; raw NES palette, including aliased entries
.define M3G_TILE        $c880 ; sixteen raw pattern bytes
.define M3G_RECORD     $9900 ; playfield record, HUD record at $9940
.define M3G_IRQ_V      $c8f7 ; bit0=guest IRQ active, bit1=explicit v reload
.define M3G_CANCEL     $c8f6 ; main disabled rendering before logical split
.define M3G_STALE_IRQ  $c8f8 ; pending IRQ outlived its frozen packet
.define M3G_EPOCH      $c8f9 ; diagnostic NMI epoch, ownership uses sticky flag
.define M3G_IRQ_EPOCH  $c8fa
; Independent physical-CIRAM write intent for the PF/HUD frozen snapshots.
; Native DD40..DD5F is not guest RAM, renderer scratch or the DE40 stack floor.
; Each bit covers16 bytes. Capture consumes only its own128-bit map.
.define M3T_PF        $dd40
.define M3T_HUD       $dd50

.section "ppu_mmc3" free

; Mapper callbacks run DI. Recompute only the eight-byte map/dirty intent;
; they never enter guest code or mutate a frozen render record.
rt_mmc3_chr_changed:
  push de
  ld hl, M3G_CHR_MAP
  ld a, ($c811)
  and (MMC3_CHR_BANK_MASK & $fe)
  ld (hl), a
  inc hl
  inc a
  ld (hl), a
  inc hl
  ld a, ($c812)
  and (MMC3_CHR_BANK_MASK & $fe)
  ld (hl), a
  inc hl
  inc a
  ld (hl), a
  inc hl
  ld de, $c813
  ld b, 4
_m3g_chr_single:
  ld a, (de)
  and MMC3_CHR_BANK_MASK
  ld (hl), a
  inc hl
  inc de
  djnz _m3g_chr_single
  ld a, ($c810)
  bit 7, a
  jr z, _m3g_chr_done
  ld hl, M3G_CHR_MAP
  ld de, M3G_CHR_MAP+4
  ld b, 4
_m3g_chr_swap:
  ld c, (hl)
  ld a, (de)
  ld (hl), a
  ld a, c
  ld (de), a
  inc hl
  inc de
  djnz _m3g_chr_swap
_m3g_chr_done:
  pop de
rt_mmc3_mirroring_changed:
  ld a, 1
  ld (M3G_DIRTY), a
  ret

rt_mmc3_irq_changed:
  ; Register storage/acknowledgement belongs to mapper_mmc3. The logical
  ; raster adapter inspects that state only at a completed guest NMI.
  ld a, ($c823)
  or a
  ret nz
  ld (M3G_STALE_IRQ), a     ; E000 acknowledgement retires old ownership
  ret

; Boot DI. Entire SRAM bank0 belongs to this backend; bank1 is untouched.
rt_mmc3_graphics_init:
  ld a, 8
  ld ($fffc), a
  ld hl, $8000
  ld bc, $4000
  xor a
  call mem_fill
  ; The bank0 bulk clear is also a source mutation. Seed AFTER it (and after
  ; boot's earlier native clears), never infer that frozen buffers are valid.
  call rt_mmc3_touch_all
  xor a
  ld ($fffc), a
  jp rt_mmc3_chr_changed

rt_mmc3_touch_all:
  ld hl, M3T_PF
  ld bc, 32
  ld a, $ff
  jp mem_fill
rt_mmc3_touch_hud_all:
  ld hl, M3T_HUD
  ld bc, 16
  ld a, $ff
  jp mem_fill

; DI, called AFTER the real source store. HL=physical8000..87FF; AF/BC/HL
; scratch, DE unchanged. The following increment reloads its own source v.
; No guest event or host service can intervene between store and both marks.
rt_mmc3_ciram_touched:
  ld a, l
  rrca
  rrca
  rrca
  rrca
  ld c, a
  ld b, >rt_mmc3_bit_masks
  ld a, (bc)
  ld c, a
  ld a, h
  and 7
  add a, a
  sla l
  adc a, 0
  add a, <M3T_PF
  ld l, a
  ld h, >M3T_PF
  ld a, (hl)
  or c
  ld (hl), a
  ld a, l
  add a, 16
  ld l, a
  ld a, (hl)
  or c
  ld (hl), a
  ret

; Fixed-bank guarded entry points. Input B=register index; writes preserve
; original AF. Read result A; BC/HL scratch. Shadow P is never modified.
rt_ppu_write:
  push af
  ld c, a
  ld a, i
  di
  push af
  push de
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  ld a, c
  ld (M3G_PPU_BUS), a
  call _m3p_write
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop de
  pop af
  jp po, _m3p_write_di
  pop af
  ei
  ret
_m3p_write_di:
  pop af
  ret

rt_ppu_write_cont:
  push hl
  call rt_ppu_write
  pop hl
  jp (hl)

rt_ppu_read:
  ld a, i
  di
  push af
  push de
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  call _m3p_read
  ld (M3G_PPU_BUS), a
  ld c, a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop de
  pop af
  jp po, _m3p_read_di
  ld a, c
  ei
  ret
_m3p_read_di:
  ld a, c
  ret

_m3p_write:
  ld a, 1
  ld (M3G_DIRTY), a
  ld a, b
  and 7
  jp z, _m3p_ctrl
  dec a
  jp z, _m3p_mask
  dec a
  ret z                    ; PPUSTATUS is read-only
  dec a
  jp z, _m3p_oamaddr
  dec a
  jp z, _m3p_oamwrite
  dec a
  jp z, _m3p_scroll
  dec a
  jp z, _m3p_addr
  jp _m3p_datawrite

_m3p_ctrl:
  ld a, c
  ld ($cb08), a
  and 3
  add a, a
  add a, a
  ld b, a
  ld a, (M3G_TEMP_HI)
  and $f3
  or b
  ld (M3G_TEMP_HI), a
  ret
_m3p_mask:
  ld a, c
  ld ($cb09), a
.ifdef MMC3_COOPERATIVE_WAIT
  and $18
  ret nz
  ld a, (M3G_BUSY)
  or a
  ret nz                   ; transient NMI/IRQ writes remain atomic source state
  inc a
  ld (M3G_CANCEL), a
.endif
  ret
_m3p_oamaddr:
  ld a, c
  ld ($cb0a), a
  ret
_m3p_oamwrite:
  ld a, ($cb0a)
  ld l, a
  ld h, $c9
  ld (hl), c
  inc a
  ld ($cb0a), a
  ret

_m3p_scroll:
  ld a, ($cb0b)
  or a
  jr nz, _m3p_scroll_y
  ld a, c
  ld ($cb0c), a
  and 7
  ld (M3G_FINE_X), a
  ld a, c
  rrca
  rrca
  rrca
  and $1f
  ld b, a
  ld a, (M3G_TEMP_LO)
  and $e0
  or b
  ld (M3G_TEMP_LO), a
  jp _m3p_toggle
_m3p_scroll_y:
  ld a, c
  ld ($cb0d), a
  and $38
  rlca
  rlca
  ld b, a
  ld a, (M3G_TEMP_LO)
  and $1f
  or b
  ld (M3G_TEMP_LO), a
  ld a, c
  and 7
  rlca
  rlca
  rlca
  rlca
  ld b, a
  ld a, c
  rlca
  rlca
  and 3
  or b
  ld b, a
  ld a, (M3G_TEMP_HI)
  and $0c
  or b
  ld (M3G_TEMP_HI), a
  jp _m3p_toggle

_m3p_addr:
  ld a, ($cb0b)
  or a
  jr nz, _m3p_addr_low
  ld a, c
  and $3f
  ld (M3G_TEMP_HI), a
  jp _m3p_toggle
_m3p_addr_low:
  ld a, c
  ld (M3G_TEMP_LO), a
  ld ($cb10), a
  ld a, (M3G_TEMP_HI)
  ld ($cb0f), a
  ld a, (M3G_IRQ_V)
  bit 0, a
  jr z, _m3p_toggle
  or 2
  ld (M3G_IRQ_V), a
_m3p_toggle:
  ld a, ($cb0b)
  xor 1
  ld ($cb0b), a
  ld ($cb0e), a
  ret

_m3p_datawrite:
  ld a, ($cb10)
  ld l, a
  ld a, ($cb0f)
  and $3f
  ld h, a
  cp $20
  jp c, _m3p_increment       ; CHR-ROM writes are ignored, but v increments
  cp $3f
  jr nc, _m3p_palette_write
  call rt_mmc3_ciram_address
  ld a, 8
  ld ($fffc), a
  ld (hl), c
  call rt_mmc3_ciram_touched
  jp _m3p_increment
_m3p_palette_write:
  call _m3p_palette_address
  ld a, c
  and $3f
  ld (hl), a
  jp _m3p_increment

_m3p_read:
  ld a, b
  and 7
  cp 2
  jr z, _m3p_status
  cp 4
  jr z, _m3p_oamread
  cp 7
  jr z, _m3p_dataread
  ld a, (M3G_PPU_BUS)
  ret
_m3p_status:
  ld a, (M3G_PPU_BUS)
  and $1f
  ld c, a
  ld a, ($cb05)
  or a
  jr z, _m3p_status_clear
  set 7, c
_m3p_status_clear:
  xor a
  ld ($cb05), a
  ld ($cb0b), a
  ld ($cb0e), a
  ld a, c
  ret
_m3p_oamread:
  ld a, ($cb0a)
  ld l, a
  ld h, $c9
  ld a, (hl)
  ret
_m3p_dataread:
  ld a, ($cb10)
  ld l, a
  ld a, ($cb0f)
  and $3f
  ld h, a
  cp $3f
  jr nc, _m3p_palette_read
  push hl
  call rt_mmc3_ppu_source_read
  ld b, a
  ld a, ($cb11)
  ld c, a
  ld a, b
  ld ($cb11), a
  pop hl
  call _m3p_read_increment
  ld a, c
  ret
_m3p_palette_read:
  push hl
  call _m3p_palette_address
  ld a, (hl)
  ld c, a
  ld a, ($cb09)
  and 1
  jr z, _m3p_palette_read_color
  ld a, c
  and $30
  ld c, a
_m3p_palette_read_color:
  pop hl
  push bc
  res 4, h                 ; palette read also refills buffer from $2Fxx
  call rt_mmc3_ppu_source_read
  ld ($cb11), a
  pop bc
  call _m3p_read_increment
  ld a, c
  ret

_m3p_palette_address:
  ld a, l
  and $1f
  bit 4, a
  jr z, _m3p_palette_index
  ld b, a
  and 3
  ld a, b
  jr nz, _m3p_palette_index
  and $0f                  ; $3F10/14/18/1C alias $3F00/04/08/0C
_m3p_palette_index:
  add a, <M3G_PALETTE
  ld l, a
  ld h, >M3G_PALETTE
  ret

; The selected single-split adapter invokes this actual guest IRQ during
; logical visible rendering. Match the pinned native reference's vertical
; $2007 read increment there; ordinary producer accesses retain +1/+32.
; This does not supply a generic dot-level PPU fetch/address clock.
_m3p_read_increment:
.ifdef SMB3_MMC3_SINGLE_SPLIT
  ld a, (M3G_IRQ_V)
  bit 0, a
  jr z, _m3p_increment
  ld a, ($cb09)
  and $18
  jr z, _m3p_increment
  ld a, ($cb10)
  ld l, a
  ld a, ($cb0f)
  ld h, a
  push bc
  call rt_mmc3_vertical_increment
  pop bc
  jr _m3p_increment_done
.endif
_m3p_increment:
  ld a, ($cb10)
  ld l, a
  ld a, ($cb0f)
  ld h, a
  ld a, ($cb08)
  bit 2, a
  jr nz, _m3p_inc32
  inc hl
  jr _m3p_increment_done
_m3p_inc32:
  ld a, l
  add a, 32
  ld l, a
  jr nc, _m3p_increment_done
  inc h
_m3p_increment_done:
  ld a, l
  ld ($cb10), a
  ld a, h
  and $7f                  ; internal v is 15-bit; bus masks to 14 bits
  ld ($cb0f), a
  ret

; HL=15-bit loopy v -> next vertical pixel. DE preserved; AF/BC scratch.
rt_mmc3_vertical_increment:
  ld a, h
  and $70
  cp $70
  jr z, _m3p_vertical_coarse
  ld a, h
  add a, $10
  ld h, a
  ret
_m3p_vertical_coarse:
  ld a, h
  and $0f
  ld h, a
  and 3
  rlca
  rlca
  rlca
  ld b, a
  ld a, l
  rlca
  rlca
  rlca
  and 7
  or b
  cp 29
  jr z, _m3p_vertical_toggle
  cp 31
  jr z, _m3p_vertical_zero
  ld a, l
  add a, 32
  ld l, a
  ret nc
  inc h
  ret
_m3p_vertical_toggle:
  ld a, h
  xor 8
  ld h, a
_m3p_vertical_zero:
  ld a, h
  and $0c
  ld h, a
  ld a, l
  and $1f
  ld l, a
  ret

; HL=PPU address $2000..3EFF -> HL=bank0SRAM CIRAM address. C preserved.
rt_mmc3_ciram_address:
  ld a, ($c824)
  or a
  ld a, h
  jr nz, _m3p_horizontal
  and 7
  or $80
  ld h, a
  ret
_m3p_horizontal:
  and 3
  ld b, a
  ld a, h
  and 8
  rrca
  or b
  or $80
  ld h, a
  ret

; Locked source read. HL=$0000..3EFF, A=result; BC/HL scratch, DE preserved.
; Caller owns exact mapper restoration; source never depends on SMS VRAM.
rt_mmc3_ppu_source_read:
  ld a, h
  cp $20
  jr nc, _m3p_source_ciram
  ld c, l
  and 3
  ld b, a
  ld a, h
  rrca
  rrca
  and 7
  add a, <M3G_CHR_MAP
  ld l, a
  ld h, >M3G_CHR_MAP
  ld a, (hl)
  push af
  and $0f
  rlca
  rlca
  or b
  or $80
  ld h, a
  ld l, c
  pop af
  rrca
  rrca
  rrca
  rrca
  and $0f
  add a, MMC3_CHR_DATA_BASE
  ld ($ffff), a
  xor a
  ld ($fffc), a
  ld a, (hl)
  ret
_m3p_source_ciram:
  call rt_mmc3_ciram_address
  ld a, 8
  ld ($fffc), a
  ld a, (hl)
  ret

; DMA uses the same CPU bus semantics, including mirroring and bank1 RAM.
; Entry A=NES source page; preserve AF/BC/DE/HL and entry IFF.
rt_oam_dma:
  push af
  push bc
  push de
  push hl
  ld h, a
  ld l, 0
  ld a, i
  di
  push af
  ld a, ($cb0a)
  ld e, a
  ld d, $c9
  ld a, h
  cp $20
  jr nc, _m3p_dma_bus
  and 7
  or $c0
  ld h, a
  ld a, e
  or a
  jr z, _m3p_dma_ram_aligned
  neg
  ld c, a
  ld b, 0
  ldir
  ld de, $c900
  ld a, ($cb0a)
  ld c, a
  ld b, 0
  ldir
  jr _m3p_dma_done
_m3p_dma_ram_aligned:
  ld bc, $0100
  ldir
  jr _m3p_dma_done
_m3p_dma_bus:
  ld b, 0
_m3p_dma_loop:
  push bc
  push hl
  call rt_mmc3_read_bus
  ld (de), a
  pop hl
  pop bc
  inc hl
  inc e
  djnz _m3p_dma_loop
_m3p_dma_done:
  ld a, 1
  ld (M3G_DIRTY), a
  pop af
  jp po, _m3p_dma_di
  pop hl
  pop de
  pop bc
  pop af
  ei
  ret
_m3p_dma_di:
  pop hl
  pop de
  pop bc
  pop af
  ret

.ends
.endif
