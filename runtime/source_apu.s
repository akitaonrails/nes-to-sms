; Source-cycle NTSC APU control/sequencer, separate from SMS PSG publication.
; Channel/envelope/length authority remains CB30..CB55; PSG cache CB56..CB61.
; No host tick clocks this state. DMC and narrowly unqualified register/event
; collision windows fail closed instead of silently approximating CPU results.
; References: nesdev.org/wiki/APU_Sweep, /APU_Envelope, /APU_Status;
; NTSC frame event positions cross-checked against Mesen2 ApuFrameCounter.h.
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.include "runtime/source_hardware_layout.inc"
.define SAP_IRQ_FLAG $d501
.define SAP_INHIBIT $d502
.define SAP_MODE $d503
.define SAP_COUNTER $d504
.define SAP_NEXT $d506
.define SAP_STEP $d508
.define SAP_WRITE_DELAY $d509
.define SAP_WRITE_VALUE $d50a
.define SAP_BLOCK $d50b
.define SAP_READ_RECENT $d50c
.define SAP_DIRTY $d50d
.define SAP_LAST_HALF $d50e
.define SAP_HALF_VALID $d512
.define SAP_MUTED $d513
.define SAP_PERIOD $d514
.define SAP_TARGET $d516
.define SAP_SWEEP $d518
.define SAP_CHANNEL $d519
.define SAP_UNSUPPORTED $d51a
.define SAP_EVENTS $d51b ; last completed cycle:1=quarter,2=half,4=IRQ set
.define SAP_NEXT_EVENTS $d51c
.define SAP_END $d51d
.if SAP_END > $d5e0
  .fail "Source APU overlaps source input"
.endif

.bank 0 slot 0
.section "source_apu" free

; Declared synthetic APU epoch: sequencer position0 at source cycle0, 4-step,
; IRQ enabled. Authentic game reset must establish its separate phase contract.
rt_source_apu_init:
  ld hl, SH_IRQ_LINE
  ld bc, $e0
  xor a
  call mem_fill
  call apu_psg_init
  call _sap_next_event
  ld a, 1
  ld (SAP_DIRTY), a
  ret

; Boot-only cold reset phase selected by the central reset contract: after
; init, an implicit4017=00 reset is due in one cycle, before seven reset reads.
; Ordinary synthetic init keeps its exact declared epoch above unchanged.
rt_source_apu_cold_reset:
  xor a
  ld (SAP_WRITE_VALUE), a
  inc a
  ld (SAP_WRITE_DELAY), a
  ret

; One completed original CPU cycle, after source PPU advancement and before
; its bus transfer. SC_CYCLES is owned/incremented by the central clock only.
rt_source_apu_cycle:
  push af
  push bc
  push de
  push hl
  xor a
  ld (SAP_EVENTS), a
  ld hl, SAP_BLOCK
  call _sap_dec_nonzero
  ld hl, SAP_READ_RECENT
  call _sap_dec_nonzero
  ld hl, (SAP_COUNTER)
  inc hl
  ld (SAP_COUNTER), hl
  ld de, (SAP_NEXT)
  or a
  sbc hl, de
  jr nz, _sap_cycle_reset
  ld a, (SAP_NEXT_EVENTS)
  ld (SAP_EVENTS), a
  bit 2, a
  call nz, _sap_irq_event
  ld a, (SAP_NEXT_EVENTS)
  and 3
  call nz, _sap_clock_units
  ld a, (SAP_STEP)
  inc a
  cp 6
  jr c, _sap_cycle_step
  xor a
  ld h, a
  ld l, a
  ld (SAP_COUNTER), hl
_sap_cycle_step:
  ld (SAP_STEP), a
  call _sap_next_event
_sap_cycle_reset:
  ld a, (SAP_WRITE_DELAY)
  or a
  jr z, _sap_cycle_done
  dec a
  ld (SAP_WRITE_DELAY), a
  jr nz, _sap_cycle_done
  ld a, (SAP_WRITE_VALUE)
  and $80
  ld (SAP_MODE), a
  xor a
  ld (SAP_STEP), a
  ld h, a
  ld l, a
  ld (SAP_COUNTER), hl
  call _sap_next_event
  ld a, (SAP_MODE)
  or a
  jr z, _sap_cycle_done
  ld a, 3
  call _sap_clock_units
_sap_cycle_done:
  pop hl
  pop de
  pop bc
  pop af
  ret
_sap_dec_nonzero:
  ld a, (hl)
  or a
  ret z
  dec (hl)
  ret

_sap_irq_event:
  ld a, 1
  ld (SAP_IRQ_FLAG), a
  ld a, (SAP_INHIBIT)
  or a
  jr nz, _sap_irq_inhibited
  inc a
  ld (SH_IRQ_LINE), a
  ret
_sap_irq_inhibited:
  ; The internal flag and external IRQ line are separate. Inhibited terminal
  ; flag pulses are not admitted to CPU reads until their race is qualified.
  ld a, (SAP_STEP)
  cp 5
  ret nz
  xor a
  ld (SAP_IRQ_FLAG), a
  ret

_sap_next_event:
  ld a, (SAP_STEP)
  ld e, a
  add a, a
  add a, e
  ld e, a
  ld d, 0
  ld hl, _sap_four_events
  ld a, (SAP_MODE)
  or a
  jr z, _sap_next_table
  ld hl, _sap_five_events
_sap_next_table:
  add hl, de
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  ld a, (hl)
  ld (SAP_NEXT_EVENTS), a
  ld (SAP_NEXT), de
  ret
_sap_four_events:
  .dw 7457
  .db 1
  .dw 14913
  .db 3
  .dw 22371
  .db 1
  .dw 29828
  .db 4
  .dw 29829
  .db 7
  .dw 29830
  .db 4
_sap_five_events:
  .dw 7457
  .db 1
  .dw 14913
  .db 3
  .dw 22371
  .db 1
  .dw 29829
  .db 0
  .dw 37281
  .db 3
  .dw 37282
  .db 0

; A=unit flags1quarter/2half; inhibit duplicate frame clocks in the immediately
; adjacent cycle when delayed4017 reset coincides with a sequencer event.
_sap_clock_units:
  ld b, a
  ld a, (SAP_BLOCK)
  or a
  ret nz
  ld a, (SAP_EVENTS)
  or b
  ld (SAP_EVENTS), a
  push bc
  call rt_source_apu_quarter
  pop bc
  bit 1, b
  call nz, rt_source_apu_half
  ld a, 2
  ld (SAP_BLOCK), a
  ld a, 1
  ld (SAP_DIRTY), a
  ret

rt_source_apu_quarter:
  ld a, (APU_SHADOW)
  ld d, a
  ld hl, ENV_P1
  ld c, 0
  call rt_apu_envelope_tick
  ld a, (APU_SHADOW+4)
  ld d, a
  ld hl, ENV_P2
  ld c, 1
  call rt_apu_envelope_tick
  ld a, (APU_SHADOW+$0c)
  ld d, a
  ld hl, ENV_NOISE
  ld c, 2
  call rt_apu_envelope_tick
  ld a, (APU_FLAGS)
  bit 5, a
  jr z, _sap_linear_dec
  ld a, (APU_SHADOW+8)
  and $7f
  ld (TRI_LINEAR), a
  jr _sap_linear_control
_sap_linear_dec:
  ld hl, TRI_LINEAR
  call _sap_dec_nonzero
_sap_linear_control:
  ld a, (APU_SHADOW+8)
  bit 7, a
  ret nz
  ld hl, APU_FLAGS
  res 5, (hl)
  ret

rt_source_apu_half:
  ld hl, SC_CYCLES
  ld de, SAP_LAST_HALF
  ld bc, 4
  ldir
  ld a, 1
  ld (SAP_HALF_VALID), a
  ld a, (APU_SHADOW)
  bit 5, a
  ld hl, LEN_P1
  call z, _sap_dec_nonzero
  ld a, (APU_SHADOW+4)
  bit 5, a
  ld hl, LEN_P2
  call z, _sap_dec_nonzero
  ld a, (APU_SHADOW+8)
  bit 7, a
  ld hl, LEN_TRI
  call z, _sap_dec_nonzero
  ld a, (APU_SHADOW+$0c)
  bit 5, a
  ld hl, LEN_NOISE
  call z, _sap_dec_nonzero
  ld c, 0
  call _sap_sweep_clock
  ld c, 1
  jp _sap_sweep_clock

; C=channel0/1. Compute target continuously, including disabled/shift0 sweeps.
; Return A=1 muted/0 audible; PERIOD/TARGET/SWEEP/CHANNEL scratch is explicit.
_sap_sweep_target:
  ld a, c
  ld (SAP_CHANNEL), a
  add a, a
  add a, a
  add a, <APU_SHADOW+1
  ld l, a
  ld h, >APU_SHADOW
  ld a, (hl)
  ld (SAP_SWEEP), a
  inc hl
  ld e, (hl)
  inc hl
  ld a, (hl)
  and 7
  ld d, a
  ld (SAP_PERIOD), de
  ld h, d
  ld l, e
  ld a, (SAP_SWEEP)
  and 7
  ld b, a
  jr z, _sap_target_shifted
_sap_target_shift:
  srl h
  rr l
  djnz _sap_target_shift
_sap_target_shifted:
  ld a, (SAP_SWEEP)
  bit 3, a
  jr z, _sap_target_add
  ex de, hl
  or a
  sbc hl, de
  ld a, (SAP_CHANNEL)
  or a
  jr nz, _sap_target_nonnegative
  dec hl
_sap_target_nonnegative:
  bit 7, h
  jr z, _sap_target_ready
  ld hl, 0
  jr _sap_target_ready
_sap_target_add:
  add hl, de
_sap_target_ready:
  ld (SAP_TARGET), hl
  ld a, h
  cp 8
  jr nc, _sap_target_muted
  ld hl, (SAP_PERIOD)
  ld a, h
  or a
  jr nz, _sap_target_audible
  ld a, l
  cp 8
  jr c, _sap_target_muted
_sap_target_audible:
  xor a
  ret
_sap_target_muted:
  ld a, 1
  ret

_sap_sweep_clock:
  call _sap_sweep_target
  ld e, a
  ld a, (SAP_CHANNEL)
  add a, <SWEEP_P1
  ld l, a
  ld h, >SWEEP_P1
  ld a, (hl)
  or a
  jr nz, _sap_sweep_reload_check
  ld a, e
  or a
  jr nz, _sap_sweep_reload
  ld a, (SAP_SWEEP)
  bit 7, a
  jr z, _sap_sweep_reload
  and 7
  jr z, _sap_sweep_reload
  ; Apply BEFORE reload handling, even when the reload flag is already set.
  push hl
  ld a, (SAP_CHANNEL)
  add a, a
  add a, a
  add a, <APU_SHADOW+2
  ld l, a
  ld h, >APU_SHADOW
  ld de, (SAP_TARGET)
  ld (hl), e
  inc hl
  ld a, (hl)
  and $f8
  or d
  ld (hl), a
  pop hl
  jr _sap_sweep_reload
_sap_sweep_reload_check:
  ld a, (SAP_CHANNEL)
  ld b, a
  ld a, (APU_FLAGS)
  rrca
  rrca
  rrca
  inc b
_sap_sweep_reload_bit:
  rrca
  djnz _sap_sweep_reload_bit
  jr c, _sap_sweep_reload
  dec (hl)
  ret
_sap_sweep_reload:
  ld a, (SAP_SWEEP)
  rrca
  rrca
  rrca
  rrca
  and 7
  ld (hl), a
  ld hl, APU_FLAGS
  ld a, (SAP_CHANNEL)
  or a
  jr nz, _sap_sweep_clear2
  res 3, (hl)
  ret
_sap_sweep_clear2:
  res 4, (hl)
  ret

; A=value, HL=source4000..4017 write. All registers/IFF preserved. Harmless
; DMC register initialization remains possible, but nonzero direct PCM or
; enabling DMC fails until its CPU-read/DMA/IRQ arbitration is implemented.
rt_source_apu_write:
  push af
  push bc
  push de
  push hl
  ld b, a
  ld a, h
  cp $40
  jp nz, _sap_bad_access
  ld a, l
  cp $18
  jp nc, _sap_bad_access
  cp $16
  jp z, _sap_bad_access
  cp $14
  jp z, _sap_bad_access
  cp $17
  jr z, _sap_write_frame
  cp $15
  jr nz, _sap_write_pcm
  bit 4, b
  jp nz, _sap_dmc_unsupported
_sap_write_pcm:
  cp $11
  jr nz, _sap_write_channel
  ld a, b
  and $7f
  jp nz, _sap_dmc_unsupported
_sap_write_channel:
  call _sap_length_write_guard
  ld a, b
  call rt_apu_write
  ld a, 1
  ld (SAP_DIRTY), a
  jr _sap_write_done
_sap_write_frame:
  ld a, b
  ld (SAP_WRITE_VALUE), a
  ld (APU_SHADOW+$17), a
  and $40
  ld (SAP_INHIBIT), a
  jr z, _sap_write_delay
  xor a
  ld (SAP_IRQ_FLAG), a
  ld (SH_IRQ_LINE), a
_sap_write_delay:
  ld a, (SC_CYCLES)
  and 1
  add a, 3
  ld (SAP_WRITE_DELAY), a
_sap_write_done:
  pop hl
  pop de
  pop bc
  pop af
  ret

; Length-load/halt writes at or immediately before half-clock have hardware
; reload-vs-decrement rules not supplied by the old register shim. Guard the
; exact two-cycle frontier until independently qualified, including4017 clocks.
_sap_length_write_guard:
  ld a, l
  cp $10
  ret nc
  and 3
  jr z, _sap_length_sensitive
  cp 3
  ret nz
_sap_length_sensitive:
  push bc
  push de
  push hl
  ld a, (SAP_EVENTS)
  bit 1, a
  jp nz, _sap_length_unsupported
  ld a, (SAP_WRITE_DELAY)
  cp 1
  jr nz, _sap_length_regular
  ld a, (SAP_WRITE_VALUE)
  bit 7, a
  jp nz, _sap_length_unsupported
_sap_length_regular:
  ld a, (SAP_NEXT_EVENTS)
  bit 1, a
  jr z, _sap_length_guard_done
  ld hl, (SAP_NEXT)
  ld de, (SAP_COUNTER)
  or a
  sbc hl, de
  ld a, h
  or a
  jr nz, _sap_length_guard_done
  ld a, l
  cp 2
  jp c, _sap_length_unsupported
_sap_length_guard_done:
  pop hl
  pop de
  pop bc
  ret

; A=prior external CPU bus, return4015status with bit5 retained. Central bus
; must not replace external SC_BUS with this CPU-internal register result.
; Rapid re-reads and same-cycle IRQ-set collisions are explicit race guards.
rt_source_apu_read:
  push bc
  push de
  push hl
  and $20
  ld b, a
  ld a, (SAP_EVENTS)
  bit 2, a
  jp nz, _sap_status_unsupported
  ld a, (SAP_READ_RECENT)
  or a
  jp nz, _sap_status_unsupported
  ld a, (SAP_IRQ_FLAG)
  or a
  jr z, _sap_status_lengths
  set 6, b
  xor a
  ld (SAP_IRQ_FLAG), a
  ld (SH_IRQ_LINE), a
  ld a, 3
  ld (SAP_READ_RECENT), a
_sap_status_lengths:
  ld a, (LEN_P1)
  or a
  jr z, _sap_status_p2
  set 0, b
_sap_status_p2:
  ld a, (LEN_P2)
  or a
  jr z, _sap_status_tri
  set 1, b
_sap_status_tri:
  ld a, (LEN_TRI)
  or a
  jr z, _sap_status_noise
  set 2, b
_sap_status_noise:
  ld a, (LEN_NOISE)
  or a
  jr z, _sap_status_done
  set 3, b
_sap_status_done:
  ld a, b
  pop hl
  pop de
  pop bc
  ret

; Relative cycles to next required APU processing boundary. This does NOT
; authorize skipping: central must prove interval processing before batching.
; Return HL, preserve AF/BC/DE/IFF. Phase/block/read guards are deadlines too.
rt_source_apu_deadline:
  push af
  push bc
  push de
  ld hl, (SAP_NEXT)
  ld de, (SAP_COUNTER)
  or a
  sbc hl, de
  ld a, (SAP_WRITE_DELAY)
  call _sap_min_deadline
  ld a, (SAP_BLOCK)
  call _sap_min_deadline
  ld a, (SAP_READ_RECENT)
  call _sap_min_deadline
  pop de
  pop bc
  pop af
  ret
_sap_min_deadline:
  or a
  ret z
  ld b, a
  ld a, h
  or a
  jr nz, _sap_min_take
  ld a, b
  cp l
  ret nc
_sap_min_take:
  ld l, b
  ld h, 0
  ret

; Advance an event-free interval only. Central owns source time, PPU/bus/IRQ
; arbitration and proves that no intervening register transfer is omitted.
; BC0 is an exact no-op. Positive BC must finish STRICTLY before every APU
; deadline, including delayed reset and the short block/read-race countdowns.
; Validate everything before mutation; preserve all caller registers and IFF.
rt_source_apu_quiet_advance:
  push af
  ld a, b
  or c
  jr z, _sap_quiet_zero
  push bc
  push de
  push hl
  ld hl, (SAP_NEXT)
  ld de, (SAP_COUNTER)
  or a
  sbc hl, de
  jp c, _sap_span_unsupported
  jp z, _sap_span_unsupported
  call rt_source_apu_deadline
  or a
  sbc hl, bc
  jp c, _sap_span_unsupported
  jp z, _sap_span_unsupported
  ld hl, (SAP_COUNTER)
  add hl, bc
  ld (SAP_COUNTER), hl
  ld hl, SAP_WRITE_DELAY
  call _sap_quiet_countdown
  ld hl, SAP_BLOCK
  call _sap_quiet_countdown
  ld hl, SAP_READ_RECENT
  call _sap_quiet_countdown
  xor a
  ld (SAP_EVENTS), a
  pop hl
  pop de
  pop bc
_sap_quiet_zero:
  pop af
  ret
_sap_quiet_countdown:
  ld a, (hl)
  or a
  ret z
  ; An active byte-sized deadline implies BC<256 and BC<countdown.
  sub c
  ld (hl), a
  ret

; Safe CPU-boundary service; publish changed PSG parameters, never advance APU
; counters. Physical host audio may hold the last note during backpressure.
rt_source_apu_publish:
  push af
  ld a, (SAP_DIRTY)
  or a
  jr nz, _sap_publish_dirty
  pop af
  ret
_sap_publish_dirty:
  push bc
  push de
  push hl
  xor a
  ld (SAP_DIRTY), a
  ld c, 0
  call _sap_sweep_target
  ld (SAP_MUTED), a
  ld c, 1
  call _sap_sweep_target
  add a, a
  ld b, a
  ld a, (SAP_MUTED)
  or b
  ld (SAP_MUTED), a
  call rt_apu_psg_publish
  pop hl
  pop de
  pop bc
  pop af
  ret

; SMS has only10-bit PSG periods. Octave-fold oversized pulse periods, as the
; existing triangle adapter does; never wrap2048 to1 and produce a false pitch.
rt_source_apu_fold_tone:
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
; HL=raw N1..4096 -> calibrated10-bit period. AF scratch; preserve every
; other register/IFF, actual slot1 latch and logical bank shadow separately.
rt_source_apu_pal_period:
  ld a, h
  or l
  jr z, _sap_pal_bad_period
  ld a, h
  cp $10
  jr c, _sap_pal_period_valid
  jr nz, _sap_pal_bad_period
  ld a, l
  or a
  jr nz, _sap_pal_bad_period
_sap_pal_period_valid:
  push de
  ld a, ($fffe)
  push af
  ld a, ($cb14)
  push af
  ld a, CHR_PACKET_CODE_BANK
  ld ($fffe), a
  ld ($cb14), a
  add hl, hl
  ld de, rt_source_apu_pal_periods
  add hl, de
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  pop af
  ld ($cb14), a
  pop af
  ld ($fffe), a
  pop de
  ret
_sap_pal_bad_period:
  ld a, 6
  jp _sap_unsupported
.else
  ld a, h
  cp 4
  ret c
  srl h
  rr l
  jr rt_source_apu_fold_tone
.endif

_sap_bad_access:
  ld a, 4
  jr _sap_unsupported
_sap_dmc_unsupported:
  ld a, 1
  jr _sap_unsupported
_sap_status_unsupported:
  ld a, 2
  jr _sap_unsupported
_sap_length_unsupported:
  ld a, 3
  jr _sap_unsupported
_sap_span_unsupported:
  ld a, 5
_sap_unsupported:
  ld (SAP_UNSUPPORTED), a
  ld a, $e9
  jp rt_cnrom_packet_graphics_trap

.ends
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
.bank CHR_PACKET_CODE_BANK slot 1
.section "source_apu_pal_periods" free
rt_source_apu_pal_periods:
  .incbin "data/pal_psg_periods.bin"
rt_source_apu_pal_periods_end:
.if rt_source_apu_pal_periods_end-rt_source_apu_pal_periods != 8194
  .fail "PAL PSG period table must contain4097 words"
.endif
.ends
.bank 0 slot 0
.endif
.endif
