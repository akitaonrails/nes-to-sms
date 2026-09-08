; Full MMC3 bring-up: coarse logical guest frames and immutable raster records.
; This is NOT a qualified-A12 clock. The explicitly selected SMB3 adapter
; admits one C0/C1 split after a completed guest NMI, delivers the actual IRQ
; vector at a later host VINT, and then publishes its two source records.
; SMS line interrupts replay display scroll only. Host input/audio/VBlank
; service continues while a guest handler or blanked renderer is busy.
.ifdef MMC3_FULL_RUNTIME
.define M3G_PACKET_CHANGED $c8f4
.define M3G_BG_CHANGED $c8ec
.define M3G_COMPARE_BG $c8ed
.section "frame_mmc3" free

rt_mmc3_sms_interrupt:
  push af
  push bc
  push de
  push hl
  ld a, ($cb15)
  ld b, a
  ld a, ($cb18)
  ld c, a
  push bc
  ld a, ($cb27)
  push af
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
  jp z, _m3f_line
  ld a, ($cb28)
  or a
  jp z, _m3f_exit
  ; The interrupted native A is 19 bytes above SP (ten pushed words).
  ld hl, 19
  add hl, sp
  ld a, (M3G_BUSY)
  or a
  jr nz, _m3f_service
  ld a, (hl)
  ld ($c83e), a
  ld ($c83c), de
_m3f_service:
  call rt_mmc3_display_rearm
  call rt_controller_latch
  call apu_frame_tick
  ld a, ($cb04)
  inc a
  ld ($cb04), a
  ld a, 1
  ld ($cb05), a
  ld a, (M3G_BUSY)
  or a
  jp nz, _m3f_exit
.ifdef MMC3_COOPERATIVE_WAIT
  ; Source main must reach a profile-verified idle loop before advancing
  ; the logical frame. Physical VINT always services input/audio above.
  ld a, ($c825)
  or a
  jp z, _m3f_exit
  xor a
  ld ($c825), a             ; one-shot ownership, consumed before guest entry
  ld a, (M3G_CANCEL)
  or a
  jr z, _m3f_event
  xor a
  ld (M3G_CANCEL), a
  ld a, ($c823)
  or a
  jr nz, _m3f_event         ; a real pending event requires actual E000 ack
  ld (M3G_PHASE), a         ; rendering-off canceled the unasserted event
  ld a, ($cb09)
  and $18
  jr nz, _m3f_event
  ; A completed main rendering-off epoch must blank even if NMI is off.
  ld a, 1
  ld (M3G_BUSY), a
  call rt_mmc3_capture_playfield
  call rt_mmc3_capture_single
  call rt_mmc3_frame_present
  xor a
  ld (M3G_BUSY), a
_m3f_event:
.endif
  ld a, (M3G_PHASE)
  or a
  jr nz, _m3f_irq_event
  ld a, ($c823)
  or a
  jp z, _m3f_nmi
_m3f_irq_event:
  ; An armed logical event remains pending while the guest masks IRQs.
  ld a, ($c822)
  or a
  jr z, _m3f_without_irq
  ld a, ($c823)
  or a
  jr nz, _m3f_pending
  ld a, (M3G_EPOCH)
  ld (M3G_IRQ_EPOCH), a
  ld a, 1
  ld ($c823), a
_m3f_pending:
  xor a
  ld ($c821), a
  ld a, ($cb03)
  bit 2, a
  jr nz, _m3f_without_irq    ; masking IRQ never suppresses NMI
  ld a, (M3G_STALE_IRQ)
  or a
  jp nz, rt_mmc3_raster_unsupported
  ld a, 1
  ld (M3G_BUSY), a
  ld (M3G_IRQ_V), a
  call _m3f_push_guest_frame
  ld a, :translated_irq
  ld ($cb14), a
  ld ($fffe), a
  ld de, ($c83c)
  ld a, ($c83e)
  ei
  call translated_irq
  di
  ld ($c83e), a
  ld ($c83c), de
  call rt_mmc3_capture_hud
  xor a
  ld (M3G_IRQ_V), a
  ei
  nop
  di
  jr _m3f_publish
_m3f_without_irq:
  ld a, 2
  ld (M3G_BUSY), a
  call rt_mmc3_capture_single
  ei
  nop
  di
_m3f_publish:
  call rt_mmc3_frame_present
  xor a
  ld (M3G_PHASE), a
  ld (M3G_BUSY), a
_m3f_nmi:
  ld a, ($cb08)
  bit 7, a
  jp z, _m3f_restore_guest
  ld a, ($c823)
  or a
  jr z, _m3f_epoch
  ld a, 1
  ld (M3G_STALE_IRQ), a
_m3f_epoch:
  ld a, (M3G_EPOCH)
  inc a
  ld (M3G_EPOCH), a
  ld a, 1
  ld (M3G_BUSY), a
  ld ($ca11), a
  call _m3f_push_guest_frame
  ld a, :translated_nmi
  ld ($cb14), a
  ld ($fffe), a
  ld de, ($c83c)
  ld a, ($c83e)
  ei
  call translated_nmi
  di
  ld ($c83e), a
  ld ($c83c), de
  xor a
  ld ($ca11), a
  call rt_mmc3_capture_playfield
  ei
  nop
  di
  ld a, ($cb09)
  and $18
  jr z, _m3f_single
  ld a, ($c822)
  or a
  jr z, _m3f_single
.ifdef SMB3_MMC3_SINGLE_SPLIT
  ; The observed route uses BG table0 / 8x16 sprites, one C0/C1 split.
  ; Different configurations require evidence/a different adapter.
  ld a, ($cb08)
  and $30
  cp $20
  jp nz, rt_mmc3_raster_unsupported
  ld a, ($c820)
  cp $c0
  jr z, _m3f_arm
  cp $c1
  jp nz, rt_mmc3_raster_unsupported
_m3f_arm:
  ld (M3G_SPLIT), a
  ld a, 1
  ld (M3G_PHASE), a
  xor a
  ld (M3G_BUSY), a
  jr _m3f_restore_guest
.else
  jp rt_mmc3_raster_unsupported
.endif
_m3f_single:
  call rt_mmc3_capture_single
  ei
  nop
  di
  call rt_mmc3_frame_present
  xor a
  ld (M3G_BUSY), a
  ld (M3G_PHASE), a
_m3f_restore_guest:
  ; Hardware interrupts preserve native intermediate flags, but guest A/X/Y
  ; are only preserved if the actual translated handler preserves them.
  ld hl, 19
  add hl, sp
  ld a, ($c83e)
  ld (hl), a
  ld hl, 14                 ; saved DE word
  add hl, sp
  ld de, ($c83c)
  ld (hl), e
  inc hl
  ld (hl), d
  jr _m3f_exit
_m3f_line:
  call rt_mmc3_display_line
_m3f_exit:
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  pop af
  ld ($ffff), a
  pop af
  ld ($fffc), a
  pop af
  ld ($cb27), a
  pop bc
  ld a, b
  ld ($cb15), a
  ld a, c
  ld ($cb18), a
  pop hl
  pop de
  pop bc
  ld a, ($cb28)
  or a
  jr z, _m3f_exit_not_ready
  pop af
  ei
  ret
_m3f_exit_not_ready:
  pop af
  ret

_m3f_push_guest_frame:
  ld a, ($cb02)
  ld l, a
  ld h, $c1
  ld (hl), $ff
  dec l
  ld (hl), $ff
  dec l
  ld a, ($cb03)
  and $ef
  or $20
  ld (hl), a
  or 4
  ld ($cb03), a
  ld a, ($cb02)
  sub 3
  ld ($cb02), a
  ret

; Snapshot record offsets:0..7 physicalCHR,8CTRL,9MASK,10scrollX,11scrollY,
; 12 t-low,13 t-high,14fineX,15mirroring;16..47 rawNESpalette.
; 48 IRQ v-low,49 IRQ v-high,50 explicit IRQ reload-present (0/1).
; All three IRQ fields are zero when no explicit IRQ-time reload occurred.
rt_mmc3_capture_playfield:
  xor a
  ld (M3G_PACKET_CHANGED), a
  ld (M3G_BG_CHANGED), a
  ld a, (M3G_READY)
  or a
  jr nz, _m3f_capture_existing
  inc a
  ld (M3G_PACKET_CHANGED), a
  ld (M3G_BG_CHANGED), a
_m3f_capture_existing:
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, 8
  ld ($fffc), a
  ld hl, $8000
  ld de, $8800
  ld bc, $0800
  call _m3f_compare_copy
  xor a
  ld (M3G_COMPARE_BG), a
  ld hl, $c900
  ld de, $9800
  ld bc, $0100
  call _m3f_compare_copy
  ld de, M3G_RECORD
  jp _m3f_capture_record
rt_mmc3_capture_hud:
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, 8
  ld ($fffc), a
  ld hl, $8000
  ld de, $9000
  ld bc, $0800
  call _m3f_compare_copy
  ld de, M3G_RECORD+$40
_m3f_capture_record:
  ; Compare the BG-selected physical four-page map independently. The
  ; remaining pages may change sprite patterns without invalidating BG.
  ; A table-selection change is separately invalidated by CTRL below.
  push de
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, ($cb08)
  and $10
  rrca
  rrca
  ld c, a
  add a, e
  ld e, a
  ld a, c
  add a, $50
  ld l, a
  ld h, $c8
  ld bc, 4
  call _m3f_compare_copy
  pop de
  xor a
  ld (M3G_COMPARE_BG), a
  ld hl, M3G_CHR_MAP
  ld bc, 8
  call _m3f_compare_copy
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, ($cb08)
  call _m3f_record_put
  ld a, ($cb09)
  call _m3f_record_put
  ; Raw $2005 bytes are packet metadata, not the renderer's BG address.
  ; $2006 can replace t without replacing them; fine X commits separately.
  xor a
  ld (M3G_COMPARE_BG), a
  ld a, ($cb0c)
  call _m3f_record_put
  ld a, ($cb0d)
  call _m3f_record_put
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, (M3G_TEMP_LO)
  call _m3f_record_put
  ld a, (M3G_TEMP_HI)
  call _m3f_record_put
  xor a
  ld (M3G_COMPARE_BG), a
  ld a, (M3G_FINE_X)
  call _m3f_record_put
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, ($c824)
  call _m3f_record_put
  xor a
  ld (M3G_COMPARE_BG), a
  ld hl, M3G_PALETTE
  ld bc, 32
  call _m3f_compare_copy
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ; Only an actual IRQ-time $2006 reload changes the vertical raster origin.
  ; Do not include stale live v in an NMI or no-reload record's identity.
  ld a, (M3G_IRQ_V)
  and 2
  jr z, _m3f_record_no_reload
  ld a, ($cb10)
  call _m3f_record_put
  ld a, ($cb0f)
  call _m3f_record_put
  ld a, 1
  jp _m3f_record_put
_m3f_record_no_reload:
  xor a
  call _m3f_record_put
  xor a
  call _m3f_record_put
  xor a
  jp _m3f_record_put
rt_mmc3_capture_single:
  ; Conservative fallback: changing any copied HUD identity invalidates
  ; BG too. The split-record path classifies palette/fine-X independently.
  ld a, 1
  ld (M3G_COMPARE_BG), a
  ld a, 8
  ld ($fffc), a
  ld hl, $8800
  ld de, $9000
  ld bc, $0800
  call _m3f_compare_copy
  ld hl, M3G_RECORD
  ld de, M3G_RECORD+$40
  ld bc, $40
  call _m3f_compare_copy
  xor a
  ld (M3G_SPLIT), a
  ret

; A byte-identical completed packet can retain the committed display.
; Guest IRQ execution and acknowledgement are independent of this shortcut.
rt_mmc3_frame_present:
  ld a, 8
  ld ($fffc), a
  call rt_mmc3_validate_layers
  ld a, (M3G_READY)
  or a
  jr z, _m3f_rebuild
  ld a, (M3G_PACKET_CHANGED)
  or a
  jr nz, _m3f_rebuild
  ld a, (M3G_SPLIT)
  ld b, a
  ld a, (M3R_SPLIT)
  cp b
  ret z
_m3f_rebuild:
  ld a, 2
  ld (M3G_BUSY), a
  ld a, (M3G_READY)
  or a
  jp z, rt_mmc3_frame_render
  ld a, (M3G_BG_CHANGED)
  or a
  jp nz, rt_mmc3_frame_render
  ld a, (M3G_SPLIT)
  ld b, a
  ld a, (M3R_SPLIT)
  cp b
  jp z, rt_mmc3_frame_render_bg_stable
  jp rt_mmc3_frame_render

; This experimental renderer publishes both layers or a fully blank packet.
; Reject partial-layer records before either reuse or visible VDP mutation.
; SRAM bank0 must be mapped. AF scratch, all other registers preserved.
rt_mmc3_validate_layers:
  ld a, (M3G_RECORD+9)
  call _m3f_validate_mask
  ld a, (M3G_RECORD+$40+9)
_m3f_validate_mask:
  and $18
  ret z
  cp $18
  ret z
  jp rt_mmc3_raster_unsupported

; Exact compare-before-overwrite, at most64 bytes between host-service
; boundaries. BUSY excludes guest reentry, so sources remain immutable.
_m3f_compare_copy:
  ld a, b
  or a
  jr nz, _m3f_copy_chunk
  ld a, c
  cp 65
  jr c, _m3f_copy_last
_m3f_copy_chunk:
  push bc
  ld bc, 64
  call _m3f_copy_part
  pop bc
  ld a, c
  sub 64
  ld c, a
  jr nc, _m3f_copy_count
  dec b
_m3f_copy_count:
  ei
  nop
  di
  jp _m3f_compare_copy
_m3f_copy_last:
  ld a, b
  or c
  ret z
  call _m3f_copy_part
  ei
  nop
  di
  ret
_m3f_copy_part:
  ld a, (M3G_PACKET_CHANGED)
  or a
  jr z, _m3f_compare_byte
  ld a, (M3G_COMPARE_BG)
  or a
  jr z, _m3f_copy_changed
  ld a, (M3G_BG_CHANGED)
  or a
  jr nz, _m3f_copy_changed
_m3f_compare_byte:
  ld a, (de)
  cp (hl)
  jr nz, _m3f_copy_mark
  ldi
  jp pe, _m3f_compare_byte
  ret
_m3f_copy_mark:
  call _m3f_mark_changed
_m3f_copy_changed:
  ldir
  ret
_m3f_record_put:
  ld b, a
  ld a, (de)
  cp b
  jr z, _m3f_record_same
  call _m3f_mark_changed
_m3f_record_same:
  ld a, b
  ld (de), a
  inc de
  ret

_m3f_mark_changed:
  ld a, 1
  ld (M3G_PACKET_CHANGED), a
  ld a, (M3G_COMPARE_BG)
  or a
  ret z
  ld (M3G_BG_CHANGED), a
  ret

rt_mmc3_raster_unsupported:
  ld a, $ea
rt_mmc3_graphics_trap:
  ld ($cb1d), a
  di
_m3f_halt:
  halt
  jr _m3f_halt

.ends
.endif
