; Source-timed NES-001 with two official standard controllers, no expansion.
; https://www.nesdev.org/wiki/Controller_reading
; https://www.nesdev.org/wiki/NES_controller
; Bits7..5 retain the prior CPU bus; D4..1 are pulled to zero, D0 is serial.
; Host sampling changes pending physical input only. Source-live promotion
; occurs at the source frame boundary, never at an SMS frame or wait count.
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.define SNI_PENDING $d5e0 ; two mapped physical button bytes
.define SNI_LIVE $d5e2
.define SNI_LATCH $d5e4
.define SNI_CURSOR $d5e6
.define SNI_STROBE $d5e8 ; all three 2A03 output latch bits, bit0 is pad strobe
.define SNI_CONNECTED $d5e9 ; bit0/1: official pad connected; others reserved
.define SNI_PAUSE_PENDING $d5ea
.define SNI_PAUSE_HOLD $d5eb
.define SNI_REPLAY $d5ec ; fixed replay fixtures feed LIVE at source-frame tap
.define SNI_PAUSE_BUTTON $d5ed ; chosen once per event:8=Start,4=Select
.define SNI_END $d5ee
.if SNI_END > $d5f0
  .fail "Source input exceeds its exclusive reservation"
.endif

.bank 0 slot 0
.section "source_input" free

; Boot/reset initialization, no source-time advancement. Both standard pads
; are connected by default; absent-device fixtures clear the relevant bit.
rt_source_input_init:
  ld hl, SNI_PENDING
  ld bc, 16
  xor a
  call mem_fill
  ld a, 3
  ld (SNI_CONNECTED), a
  ret

; Host-only physical sampling: both SMS ports, fixed A/B+directions mapping.
; Pause alone becomes Start; Pause with both buttons becomes Select. Selection
; occurs once at source promotion, with A/B consumed only for the Select chord.
; AF/BC scratch; no source-live, serial, clock or APU state is touched.
rt_source_input_sample_physical:
  in a, ($dc)
  cpl
  ld b, a
  call _sni_map_sms
  ld (SNI_PENDING), a
  ld a, b
  rlca
  rlca
  and 3
  ld c, a
  in a, ($dd)
  cpl
  and $0f
  add a, a
  add a, a
  or c
  call _sni_map_sms
  ld (SNI_PENDING+1), a
  ret
_sni_map_sms:
  ; A=active-high SMS UDLR12 in bits0..5; return NES AB00UDLR.
  push bc
  ld b, a
  and $0f
  rlca
  rlca
  rlca
  rlca
  ld c, a
  ld a, b
  rrca
  rrca
  rrca
  rrca
  and 3
  or c
  pop bc
  ret

; Port-free Pause handler helper. Central SMS NMI wrapper calls then RETN.
; Repeated pending Pause events coalesce; no in-progress serial byte changes.
rt_source_input_pause:
  push af
  ld a, 1
  ld (SNI_PAUSE_PENDING), a
  pop af
  ret

; Called once at actual source frame line0/dot0, before visible capture.
; The caller owns source frame identity and deadline; this module has no
; counter/host-rate approximation. Replay fixtures set LIVE at this tap.
; All registers/IFF preserved. Source calls are serialized by the clock owner.
rt_source_input_frame:
  push af
  push bc
  push de
  push hl
  ld a, (SNI_REPLAY)
  or a
  jr nz, _sni_frame_latches
  ld hl, SNI_PENDING
  ld de, SNI_LIVE
  ld bc, 2
  ldir
  ld a, (SNI_PAUSE_PENDING)
  or a
  jr z, _sni_frame_hold
  xor a
  ld (SNI_PAUSE_PENDING), a
  ld a, (SNI_LIVE)
  and 3
  cp 3
  ld a, 8
  jr nz, _sni_frame_pause_button
  ld a, 4
_sni_frame_pause_button:
  ld (SNI_PAUSE_BUTTON), a
  ld a, 4
  ld (SNI_PAUSE_HOLD), a
_sni_frame_hold:
  ld a, (SNI_PAUSE_HOLD)
  or a
  jr z, _sni_frame_latches
  dec a
  ld (SNI_PAUSE_HOLD), a
  ld a, (SNI_PAUSE_BUTTON)
  ld b, a
  ld a, (SNI_LIVE)
  bit 2, b
  jr z, _sni_frame_pause_apply
  and $fc
_sni_frame_pause_apply:
  or b
  ld (SNI_LIVE), a
_sni_frame_latches:
  ld a, (SNI_STROBE)
  bit 0, a
  call nz, _sni_latch_both
  pop hl
  pop de
  pop bc
  pop af
  ret

; A=source write $4016. Store OUT0..2; latch/rewind on high strobe or the
; falling edge, never on a repeated zero. All caller registers/IFF preserved.
rt_source_input_strobe:
  push af
  push bc
  push de
  push hl
  and 7
  ld b, a
  ld a, (SNI_STROBE)
  ld c, a
  ld a, b
  ld (SNI_STROBE), a
  or c
  bit 0, a
  call nz, _sni_latch_both
  pop hl
  pop de
  pop bc
  pop af
  ret
_sni_latch_both:
  ld hl, SNI_LIVE
  ld de, SNI_LATCH
  ld bc, 2
  ldir
  xor a
  ld (SNI_CURSOR), a
  ld (SNI_CURSOR+1), a
  ret

; B=port0/1, A=prior source CPU bus. Return merged A; preserve BC/DE/HL/IFF.
; Each read is an actual ordered CPU transfer, including dummy/DMA reads.
; DMC double-clock behavior is guarded by the central DMC capability gate.
rt_source_input_read:
  push bc
  push de
  push hl
  and $e0
  ld e, a
  ld a, b
  cp 2
  jp nc, rt_cnrom_packet_unsupported
  ld d, b
  inc b
  ld a, (SNI_CONNECTED)
_sni_connected_shift:
  rrca
  djnz _sni_connected_shift
  jr nc, _sni_read_zero
  ld c, d
  ld b, 0
  jr _sni_read_connected
_sni_read_zero:
  ld a, e
  jr _sni_read_done
_sni_read_connected:
  ld a, (SNI_STROBE)
  bit 0, a
  jr nz, _sni_read_live
  ld hl, SNI_CURSOR
  add hl, bc
  ld a, (hl)
  cp 8
  jr nc, _sni_read_one
  inc (hl)
  ld d, a
  ld hl, SNI_LATCH
  add hl, bc
  ld a, (hl)
  ld b, d
  inc b
_sni_read_shift:
  rrca
  djnz _sni_read_shift
  and $80
  rlca
  or e
  jr _sni_read_done
_sni_read_live:
  ld hl, SNI_LIVE
  add hl, bc
  ld a, (hl)
  and 1
  or e
  jr _sni_read_done
_sni_read_one:
  ld a, e
  or 1
_sni_read_done:
  pop hl
  pop de
  pop bc
  ret

.ends
.endif
