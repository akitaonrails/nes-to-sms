; Source-clock CNROM adapter. The source PPU captures the initial immutable
; picture, then validates the ENTIRE visible interval before marking it ready.
; Rendering runs only at a safe CPU boundary, with source time back-pressured.
; Entry requires a closed SMS VDP command: the source bus never writes SMS
; ports. CB15/CB18 are preserved scratch, not hardware VDP latch authority.
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.include "runtime/chr_packet_layout.inc"
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
.include "runtime/chr_packet_target.inc"
.define CNP_PAL_PHASE $d40c ;0invalid,1first blank,2wrapped blank,3tail blank
.define CNP_PAL_LAST $d40d
.endif
.define CNP_STATE $d400 ;0=empty,1=visible interval,2=validated,3=presenting
.define CNP_REASON $d401
.define CNP_FRAME $d402 ; source frame identity captured at visible origin
.define CNP_COMMITTED $d406
.define CNP_INITIALIZED $d40a
.define CNP_PRIORITY $d40b
.define CNP_RESIDENT $d410
.define CNP_TOUCHED $d420
.define CNP_END $d430
.if CNP_END > $d440
  .fail "CNROM packet exceeds its exclusive envelope"
.endif
.define rt_mmc3_validate_layers rt_cnrom_packet_validate_layers
.define rt_mmc3_source_committed rt_cnrom_packet_committed
.define rt_mmc3_raster_unsupported rt_cnrom_packet_unsupported
.define rt_mmc3_graphics_trap rt_cnrom_packet_graphics_trap
.define packet_render rt_mmc3_frame_render
.define packet_display_rearm rt_mmc3_display_rearm

.bank 0 slot 0
.section "frame_cnrom" free

; Boot only, before the source begins. Clear presentation-owned ranges, never
; live CIRAM, OAM, palette or optional cartridge RAM in SRAM bank1.
rt_cnrom_packet_init:
  ld a, ($fffe)
  push af
  ld a, ($cb14)
  push af
  ld a, CHR_PACKET_CODE_BANK
  ld ($cb14), a
  ld ($fffe), a
  ld hl, CNP_STATE
  ld bc, $40
  xor a
  call mem_fill
  ld hl, $c830
  ld bc, $30
  call mem_fill
  ld hl, $c880
  ld bc, $80
  call mem_fill
  ld hl, $dc00
  ld bc, $160
  call mem_fill
  ld a, 8
  ld ($fffc), a
  ld hl, $8800
  ld bc, $1180
  xor a
  call mem_fill
  call rt_mmc3_presentation_init
  call rt_mmc3_blank_display
  xor a
  ld ($fffc), a
  inc a
  ld (CNP_INITIALIZED), a
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  ret

; HL is the source PPU's resolved visible-origin v, NOT its writable t.
; Preserve caller registers/IFF/mapping. No source cycles advance during copy.
rt_cnrom_packet_capture:
  push af
  push bc
  push de
  push hl
  ld a, i
  di
  push af
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  ld a, ($fffe)
  push af
  ld a, ($cb14)
  push af
  ld a, CHR_PACKET_CODE_BANK
  ld ($cb14), a
  ld ($fffe), a
  ld a, (CNP_STATE)
  or a
  jp nz, rt_cnrom_packet_unsupported
  ld a, (CNP_INITIALIZED)
  or a
  jp z, rt_cnrom_packet_unsupported
  ld a, 8
  ld ($fffc), a
  ld (M3G_RECORD+12), hl
  ld hl, SC_FRAME
  ld de, CNP_FRAME
  ld bc, 4
  ldir
  ld hl, $8000
  ld de, $8800
  ld bc, $0800
  call rt_chr_packet_copy_closed
  ld hl, $8800
  ld de, $9000
  ld bc, $0800
  call rt_chr_packet_copy_closed
  ld hl, $c900
  ld de, $9800
  ld bc, $0100
  call rt_chr_packet_copy_closed
  ld a, (CN_CHR)
  add a, a
  add a, a
  add a, a
  ld hl, M3G_RECORD
  ld b, 8
_cnp_capture_pages:
  ld (hl), a
  inc hl
  inc a
  djnz _cnp_capture_pages
  ld a, ($cb08)
  ld (hl), a
  inc hl
  ld a, ($cb09)
  ld (hl), a
  inc hl
  ld a, ($cb0c)
  ld (hl), a
  inc hl
  ld a, ($cb0d)
  ld (hl), a
  ld a, (CN_FINE_X)
  ld (M3G_RECORD+14), a
.ifdef NES_MIRRORING_VERTICAL
  xor a
.else
  ld a, 1
.endif
  ld (M3G_RECORD+15), a
  ld hl, CN_PALETTE
  ld de, M3G_RECORD+16
  ld bc, 32
  ldir
  ld hl, M3G_RECORD+48
  ld bc, 16
  xor a
  call mem_fill
  ld hl, M3G_RECORD
  ld de, M3G_RECORD+$40
  ld bc, 64
  ldir
  xor a
  ld (CNP_REASON), a
  ld (M3G_SPLIT), a
  ld (M3C_VALID), a
  ld (M3C_ANCHORED), a
  ld (M3G_COMPARE_BG), a
  inc a
  ld (CNP_STATE), a
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop af
  jp po, _cnp_capture_di
  pop hl
  pop de
  pop bc
  pop af
  ei
  ret
_cnp_capture_di:
  pop hl
  pop de
  pop bc
  pop af
  ret

; The source PPU calls AFTER an ordered visible mutation, A=nonzero reason.
; Retain the first cause, and never publish a context invalidated later.
rt_cnrom_packet_visible_mutation:
  push af
  push bc
  ld b, a
  ld a, (CNP_STATE)
  cp 1
  jr nz, _cnp_mutation_done
  ld a, (CNP_REASON)
  or a
  jr nz, _cnp_mutation_done
  ld a, b
  or a
  jr nz, _cnp_mutation_record
  inc a
_cnp_mutation_record:
  ld (CNP_REASON), a
_cnp_mutation_done:
  pop bc
  pop af
  ret

; Only called once all visible source fetches and mutations were processed.
; An invalid interval fails before any VDP change, preserving the old picture.
rt_cnrom_packet_validate_complete:
  push af
  ld a, (CNP_STATE)
  cp 1
  jp nz, rt_cnrom_packet_unsupported
  ld a, (CNP_REASON)
  or a
  jp nz, rt_cnrom_packet_unsupported
  ld a, 2
  ld (CNP_STATE), a
  pop af
  ret

; Safe instruction-boundary service. The shared renderer deliberately yields
; to host IRQ only between closed SRAM/VDP transactions. No guest runs here.
rt_cnrom_packet_service:
  push af
  ld a, (CNP_STATE)
  cp 2
  jr z, _cnp_service_ready
  pop af
  ret
_cnp_service_ready:
  pop af
rt_cnrom_packet_present:
  push af
  push bc
  push de
  push hl
  ld a, i
  di
  push af
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  ld a, ($fffe)
  push af
  ld a, ($cb14)
  push af
  ld a, ($cb15)
  push af
  ld a, ($cb18)
  push af
  ld a, ($cb27)
  push af
  ld a, CHR_PACKET_CODE_BANK
  ld ($cb14), a
  ld ($fffe), a
  ld a, (CNP_STATE)
  cp 2
  jp nz, rt_cnrom_packet_unsupported
  ld a, (CNP_REASON)
  or a
  jp nz, rt_cnrom_packet_unsupported
  ld a, 3
  ld (CNP_STATE), a
  ld (M3G_BUSY), a
  xor a
  ld (M3C_VALID), a
  ld (M3C_ANCHORED), a
  ld (M3G_COMPARE_BG), a
  ld (M3G_SPLIT), a
  ld hl, CNP_TOUCHED
  ld bc, 16
  call mem_fill
  call packet_render
  ; Blank packets return through blank_display rather than the normal
  ; renderer callback. They still retire only after that VDP operation ends.
  ld a, (CNP_STATE)
  cp 3
  call z, rt_cnrom_packet_committed
  pop af
  ld ($cb27), a
  pop af
  ld ($cb18), a
  pop af
  ld ($cb15), a
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop af
  jp po, _cnp_service_di
  pop hl
  pop de
  pop bc
  pop af
  ei
  ret
_cnp_service_di:
  pop hl
  pop de
  pop bc
  pop af
  ret

; This callback is AFTER all publication and committed NT/SAT shadow copies.
; READY's earlier store must never release source backpressure by itself.
rt_cnrom_packet_committed:
  ld a, (CNP_STATE)
  cp 3
  jp nz, rt_cnrom_packet_unsupported
  ld hl, CNP_TOUCHED
  ld de, CNP_RESIDENT
  ld b, 16
_cnp_residency_ack:
  ld a, (de)
  or (hl)
  ld (de), a
  inc hl
  inc de
  djnz _cnp_residency_ack
  ld hl, CNP_FRAME
  ld de, CNP_COMMITTED
  ld bc, 4
  ldir
  xor a
  ld (CNP_STATE), a
  ld (M3G_BUSY), a
  ld (M3C_VALID), a
  ld (M3C_ANCHORED), a
  ret

; A source rendering-off picture still has its frozen universal backdrop.
; SMS display-disable alone would retain the previous frame's CRAM color.
rt_cnrom_packet_blank:
  xor a
  ld (M3G_READY), a
  ld (M3C_VALID), a
  ld (M3C_ANCHORED), a
  call rt_mmc3_blank_display
  ld a, 16
  call vdp_set_cram_addr
  ld a, (M3G_RECORD+16)
  and $3f
  ld l, a
  ld h, 0
  ld de, rt_chr_packet_colors
  add hl, de
  ld a, (hl)
  out ($be), a
  xor a
  ld b, 7
  jp vdp_set_register

; Bring-up limits are admission errors, not silent changes to source pixels.
; Full mapper acceptance must replace these guards with tested adaptation.
rt_cnrom_packet_validate_layers:
  xor a
  ld (CNP_PRIORITY), a
  ld a, (M3G_SPLIT)
  or a                    ; horizontal composition owns CUT in this adapter
  jp nz, rt_cnrom_packet_unsupported
  ld a, (M3G_RECORD+14)
  cp 8
  jp nc, rt_cnrom_packet_unsupported
  ld a, (M3G_RECORD+9)
  and $e1                  ; grayscale/emphasis require a color resolver
  jp nz, rt_cnrom_packet_unsupported
  ld a, (M3G_RECORD+9)
  and $18
  ret z
_cnp_validate_sprites:
  push bc
  push hl
  ld hl, $9800
  ld b, 64
_cnp_validate_sprite:
  ld a, (hl)
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
  cp CNP_DISPLAY_LINES-1
.else
  cp $df
.endif
  jr nc, _cnp_validate_sprite_next
  inc hl
  inc hl
  bit 5, (hl)
  jr z, _cnp_validate_priority_done
  ld a, 1
  ld (CNP_PRIORITY), a
_cnp_validate_priority_done:
  dec hl
  dec hl
_cnp_validate_sprite_next:
  inc hl
  inc hl
  inc hl
  inc hl
  djnz _cnp_validate_sprite
  pop hl
  pop bc
  ret

; Host service never clocks source PPU/APU, serial input or translated code.
; Every register and mapping byte survives rearm, including mismatched slot1
; hardware/shadow values in adversarial ABI fixtures.
rt_cnrom_packet_sms_interrupt:
  push af
  push bc
  push de
  push hl
  ld a, ($fffc)
  push af
  ld a, ($ffff)
  push af
  ld a, ($fffe)
  push af
  ld a, ($cb14)
  push af
  in a, ($bf)
  bit 7, a
  jr z, _cnp_irq_done
  ld a, (CNP_INITIALIZED)
  or a
  jr z, _cnp_irq_done
  call rt_source_input_sample_physical
  ld a, CHR_PACKET_CODE_BANK
  ld ($cb14), a
  ld ($fffe), a
  call packet_display_rearm
_cnp_irq_done:
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop hl
  pop de
  pop bc
  pop af
  ei
  reti

rt_cnrom_packet_unsupported:
  ld a, $ea
rt_cnrom_packet_graphics_trap:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  ld ($cb1d), a
  di
_cnp_trap:
  halt
  jr _cnp_trap

.include "runtime/chr_loopy_y.inc"
.ends
.endif
