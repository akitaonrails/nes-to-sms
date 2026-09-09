; Opt-in NTSC source clock. Descriptor ABI v1. The original clock capability
; is rendering-disabled/synthetic; SOURCE_HARDWARE adds separately gated domains.
; CA80..CAFF aliases the INACTIVE legacy CHR reverse map. No legacy renderer
; or guest-NMI host bridge may run under this capability.
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
.define SC_CYCLES $ca80 ; completed source cycles, u32 modulo 2^32
.define SC_DOT $ca84
.define SC_LINE $ca86
.define SC_FRAME $ca88 ; independent u32 frame identity
.define SC_PC $ca8c
.define SC_OPCODE $ca8e
.define SC_OPERAND $ca8f
.define SC_SIZE $ca91
.define SC_SEQUENCE $ca92
.define SC_MODE $ca93
.define SC_BASE $ca94
.define SC_PENALTY $ca95
.define SC_PHASE $ca96
.define SC_TOTAL $ca97
.define SC_POLL1 $ca98
.define SC_POLL2 $ca99
.define SC_EFFECTIVE $ca9a
.define SC_WRONG $ca9c
.define SC_POINTER $ca9e
.define SC_INDEX $caa0
.define SC_CROSS $caa1
.define SC_RESULT $caa2
.define SC_VBLANK $caa3
.define SC_NMI_LINE $caa4 ; 1=asserted (physical pin low)
.define SC_NMI_EDGE $caa5
.define SC_ACCEPTED $caa6
.define SC_ENTRY $caa7
.define SC_DMA_PAGE $caa8
.define SC_DMA_PENDING $caa9
.define SC_DMA_ALIGN $caaa ; 0: odd numbered transfer=get; 1: opposite
.define SC_DMA_ACTIVE $caab
.define SC_EVENT_ADDR $caac
.define SC_BUS $caae
.define SC_EVENT_KIND $caaf ; 0=read,1=write
.define SC_TAKEN $cab0
.define SC_RESUME $cab2
.define SC_SAVED_A $cab4
.define SC_DMA_OFFSET $cab6
.define SC_TARGET $cab7
.define SC_FETCH $cab9
.define SC_Y $cabc
.define SC_X $cabd
.define SC_SPAN_START $cabe
.define SC_SPAN_REPEATS $cac2
.define SC_SPAN_HEAD $cac4
.define SC_SPAN_ZP $cac6
.define SC_SPAN_VALUE $cac7
.define SC_SPAN_DUMMY $cac8
.define SC_SPAN_CYCLES $cac9
.define SC_SPAN_RETURN $cacb
.define SC_END $cacd
.if SC_END > $cb00
  .fail "Source clock exceeds its exclusive reservation"
.endif

.bank 0 slot 0
.section "source_clock" free
rt_source_init:
  ld hl, SC_CYCLES
  ld bc, $80
  xor a
  call mem_fill
  ld hl, 261
  ld (SC_LINE), hl
  ret

; Inline descriptor is ten bytes; preserve every incoming register normally.
rt_source_begin:
  ex (sp), hl
  push af
  push bc
  push de
  ld (SC_Y), de
  ld a, (SC_PHASE)
  ld b, a
  ld a, (SC_TOTAL)
  cp b
  jp nz, rt_source_phase_error
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_boundary
.else
  call rt_cnrom_packet_service ; previous instruction complete, no source ticks
  call rt_source_apu_publish
.endif
.endif
  ld de, SC_PC
  ld bc, 10
  ldir
  ld (SC_RESUME), hl
  ld a, (SC_ACCEPTED)
  or a
  jr nz, _sc_enter_nmi
  call _sc_prepare
  call _sc_prefix
  pop de
  pop bc
  pop af
  ld hl, (SC_RESUME)
  ex (sp), hl
  ret
_sc_enter_nmi:
  pop de
  pop bc
  pop af
  pop hl                  ; discard only source_begin's own return ownership
  jp rt_source_nmi

rt_source_phase_error:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  ld a, $e9
  ld ($cb1d), a
  jp rt_unresolved_jsr_flash

; Inline head16/zp8 descriptor. A summary is permitted only from the exact
; completed branch state of this loop, with N/Z already matching its LDA.
; Registers, shadow P, IFF and mappings are untouched. No source bus event is
; performed here: the two span taps describe all six reads per iteration.
rt_source_poll_loop:
  ex (sp), hl
  push af
  push bc
  push de
  ld (SC_SPAN_VALUE), a
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  ld (SC_SPAN_HEAD), de
  ld a, (hl)
  inc hl
  ld (SC_SPAN_ZP), a
  ld (SC_SPAN_RETURN), hl
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.ifndef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  jp _sc_span_return       ; new domains do NOT inherit the old quiet proof
.endif
.endif
  ld l, a
  ld h, $c0
  ld a, (SC_SPAN_VALUE)
  or a
  jp z, _sc_span_return
  cp (hl)
  jp nz, _sc_span_return
  and $80
  ld b, a
  ld a, ($cb03)
  and $82
  cp b
  jp nz, _sc_span_return
  ld a, (SC_NMI_EDGE)
  ld b, a
  ld a, (SC_ACCEPTED)
  or b
  ld b, a
  ld a, (SC_ENTRY)
  or b
  ld b, a
  ld a, (SC_DMA_PENDING)
  or b
  ld b, a
  ld a, (SC_DMA_ACTIVE)
  or b
  jp nz, _sc_span_return
.ifndef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld a, ($cb09)
  and $18
  jp nz, _sc_span_return
.endif
  ld a, (SC_PHASE)
  cp 3
  jp nz, _sc_span_return
  ld a, (SC_TOTAL)
  cp 3
  jp nz, _sc_span_return
  ld a, (SC_OPCODE)
  cp $d0
  jp nz, _sc_span_return
  ld a, (SC_OPERAND)
  cp $fc
  jp nz, _sc_span_return
  ld a, (SC_TAKEN)
  cp 1
  jp nz, _sc_span_return
  inc de
  inc de
  ld hl, (SC_PC)
  or a
  sbc hl, de
  jp nz, _sc_span_return
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call _sc_hardware_quiet_capacity
.else
  call _sc_quiet_capacity
.endif
  ld a, h
  or l
  jp z, _sc_span_return
  ld (SC_SPAN_REPEATS), hl
  ld d, h
  ld e, l
  add hl, hl
  add hl, de
  add hl, hl              ; six completed source cycles per iteration
  ld (SC_SPAN_CYCLES), hl
  ld hl, (SC_CYCLES)
  ld (SC_SPAN_START), hl
  ld hl, (SC_CYCLES+2)
  ld (SC_SPAN_START+2), hl
  ld a, (SC_BUS)
  ld (SC_SPAN_DUMMY), a
rt_source_quiet_span_begin:
  ; No hardware event is crossed. Preserve every old branch descriptor byte.
  ld de, (SC_SPAN_CYCLES)
  ld hl, (SC_CYCLES)
  add hl, de
  ld (SC_CYCLES), hl
  jr nc, _sc_span_add_dots
  ld hl, SC_CYCLES+2
  inc (hl)
  jr nz, _sc_span_add_dots
  inc hl
  inc (hl)
_sc_span_add_dots:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  ; CPU span owns its six reads per iteration; nested typed domain span owns
  ; the physical PPU fetches. Pending time is normalized at the outer end.
  ld (SHW_PENDING), de
  ld hl, (SC_SPAN_START)
  ld (SHW_START), hl
  ld hl, (SC_SPAN_START+2)
  ld (SHW_START+2), hl
  ld hl, (SC_DOT)
  ld (SHW_START_DOT), hl
  ld hl, (SC_LINE)
  ld (SHW_START_LINE), hl
  call rt_source_domain_flush
.else
  ld h, d
  ld l, e
  add hl, hl
  add hl, de
  ld de, (SC_DOT)
  add hl, de
  ld de, 341
_sc_span_lines:
  or a
  sbc hl, de
  jr c, _sc_span_dots_done
  push hl
  ld hl, (SC_LINE)
  inc hl
  ld a, h
  cp 1
  jr nz, _sc_span_line_done
  ld a, l
  cp 6
  jr nz, _sc_span_line_done
  ld hl, SC_FRAME
  call _sc_inc32
  ld hl, 0
_sc_span_line_done:
  ld (SC_LINE), hl
  pop hl
  jr _sc_span_lines
_sc_span_dots_done:
  add hl, de
  ld (SC_DOT), hl
.endif
rt_source_quiet_span_end:
_sc_span_return:
  pop de
  pop bc
  pop af
  ld hl, (SC_SPAN_RETURN)
  ex (sp), hl
  ret

.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
; Called only after the completed branch/PC/NZ/value/interrupt proof. A late
; already-asserted unmasked IRQ must reach the next real LDA's poll.
_sc_hardware_quiet_capacity:
  call rt_source_domain_boundary
  call rt_source_domain_flush
  ld a, (SH_IRQ_LINE)
  or a
  jr z, _sc_hw_quiet_domains
  ld a, ($cb03)
  and 4
  jr z, _sc_hw_quiet_zero
_sc_hw_quiet_domains:
  call rt_source_ppu_interval_deadline
  ld (SHW_KIND), a
  ld a, h
  or l
  jr z, _sc_hw_quiet_zero
  push hl
  call rt_source_apu_deadline
  pop de
  ld a, h
  or l
  jr z, _sc_hw_quiet_zero
  or a
  sbc hl, de
  jr nc, _sc_hw_quiet_ppu
  add hl, de
  jr _sc_hw_quiet_count
_sc_hw_quiet_ppu:
  ex de, hl
_sc_hw_quiet_count:
  dec hl                    ; end strictly before either domain event
  ld de, 6
  ld b, 0
_sc_hw_quiet_divide:
  or a
  sbc hl, de
  jr c, _sc_hw_quiet_result
  inc b
  jr _sc_hw_quiet_divide
_sc_hw_quiet_result:
  ld l, b
  ld h, 0
  ret
_sc_hw_quiet_zero:
  ld hl, 0
  ret
.endif

; HL = complete quiet iterations (0..1820). Relative PPU dot distances avoid
; absolute u32 deadline wrap. End STRICTLY before guard-window start; starts
; inside line240/260 dot338 .. line241/261 dot4 return zero.
_sc_quiet_capacity:
  ld hl, (SC_LINE)
  ld de, 241
  or a
  sbc hl, de
  jr c, _sc_quiet_before_vblank
  jr z, _sc_quiet_after_set
  ld hl, (SC_LINE)
  ld de, 261
  or a
  sbc hl, de
  jr c, _sc_quiet_before_clear
  jr nz, _sc_quiet_zero
  call _sc_quiet_after_guard
  jr c, _sc_quiet_zero
  ld hl, 241             ; line261 -> next frame line240
  jr _sc_quiet_gap
_sc_quiet_after_set:
  call _sc_quiet_after_guard
  jr c, _sc_quiet_zero
_sc_quiet_before_clear:
  ld hl, 260
  jr _sc_quiet_target
_sc_quiet_before_vblank:
  ld hl, 240
_sc_quiet_target:
  ld de, (SC_LINE)
  or a
  sbc hl, de
_sc_quiet_gap:
  ld a, h
  or a
  jr nz, _sc_quiet_max
  ld a, l
  cp 97
  jr nc, _sc_quiet_max
  ld b, a
  ld hl, 338
  ld de, 341
  or a
  jr z, _sc_quiet_distance
_sc_quiet_gap_lines:
  add hl, de
  djnz _sc_quiet_gap_lines
_sc_quiet_distance:
  ld de, (SC_DOT)
  or a
  sbc hl, de
  jr c, _sc_quiet_zero
  jr z, _sc_quiet_zero
  dec hl                 ; no ending on the first guarded dot
  ld bc, 0
  ld de, 18              ; six CPU cycles = eighteen PPU dots
_sc_quiet_divide:
  or a
  sbc hl, de
  jr c, _sc_quiet_divided
  inc bc
  ld a, b
  cp 7
  jr nz, _sc_quiet_divide
  ld a, c
  cp $1c                 ; 1820
  jr nz, _sc_quiet_divide
_sc_quiet_divided:
  ld h, b
  ld l, c
  ret
_sc_quiet_max:
  ld hl, 1820
  ret
_sc_quiet_zero:
  ld hl, 0
  ret
_sc_quiet_after_guard:
  ld hl, (SC_DOT)
  ld de, 5
  or a
  sbc hl, de
  ret

; Non-observing RAM peeks determine conditional duration before early polls.
; The prefix STILL performs every original pointer/dummy bus access later.
_sc_prepare:
  xor a
  ld (SC_PHASE), a
  ld (SC_POLL2), a
  ld (SC_CROSS), a
  ld (SC_TAKEN), a
  ld (SC_ENTRY), a
  ld a, (SC_BASE)
  ld (SC_TOTAL), a
  ld hl, (SC_PC)
  ld (SC_FETCH), hl
  ld hl, (SC_OPERAND)
  ld (SC_EFFECTIVE), hl
  ld (SC_POINTER), hl
  ld (SC_WRONG), hl
  ld a, (SC_SEQUENCE)
  cp 4
  jp z, _sc_prepare_branch
  cp 3
  jp nc, _sc_prepare_poll
  ld a, (SC_MODE)
  cp 10
  jr z, _sc_prepare_indx
  cp 11
  jr z, _sc_prepare_indy
  cp 4
  jr z, _sc_prepare_zpx
  cp 5
  jr z, _sc_prepare_zpy
  cp 7
  jr z, _sc_prepare_absx
  cp 8
  jp nz, _sc_prepare_poll
  ld a, (SC_Y)
  jr _sc_prepare_index
_sc_prepare_absx:
  ld a, (SC_X)
  jr _sc_prepare_index
_sc_prepare_zpx:
  ld a, (SC_X)
  jr _sc_prepare_zp
_sc_prepare_zpy:
  ld a, (SC_Y)
_sc_prepare_zp:
  add a, l
  ld l, a
  ld h, 0
  ld (SC_EFFECTIVE), hl
  jp _sc_prepare_poll
_sc_prepare_indx:
  ld a, (SC_X)
  add a, l
  ld l, a
_sc_prepare_indy:
  ld h, 0
  ld (SC_POINTER), hl
  ld h, $c0
  ld c, (hl)
  inc l
  ld h, (hl)
  ld l, c
  ld (SC_EFFECTIVE), hl
  ld (SC_WRONG), hl
  ld a, (SC_MODE)
  cp 10
  jr z, _sc_prepare_poll
  ld a, (SC_Y)
_sc_prepare_index:
  add a, l
  ld l, a
  ld (SC_WRONG), hl
  jr nc, _sc_prepare_index_done
  inc h
  ld a, 1
  ld (SC_CROSS), a
  ld a, (SC_PENALTY)
  cp 1
  jr nz, _sc_prepare_index_done
  ld a, (SC_TOTAL)
  inc a
  ld (SC_TOTAL), a
_sc_prepare_index_done:
  ld (SC_EFFECTIVE), hl
_sc_prepare_poll:
  ld a, (SC_TOTAL)
  dec a
  ld (SC_POLL1), a
  ld a, (SC_SEQUENCE)
  cp 10
  ret nz
  xor a
  ld (SC_POLL1), a         ; BRK does not accept a second interrupt here
  ret
_sc_prepare_branch:
  ld a, 1
  ld (SC_POLL1), a
  ld a, (SC_OPCODE)
  rlca
  rlca
  and 3
  ld e, a
  ld d, 0
  ld hl, _sc_branch_masks
  add hl, de
  ld a, ($cb03)
  and (hl)
  ld c, 0
  jr z, _sc_branch_bit
  ld c, $20
_sc_branch_bit:
  ld a, (SC_OPCODE)
  xor c
  and $20
  ret nz
  ld a, 1
  ld (SC_TAKEN), a
  ld hl, (SC_PC)
  inc hl
  inc hl
  ld b, h
  ld a, (SC_OPERAND)
  ld e, a
  ld d, 0
  bit 7, a
  jr z, _sc_branch_positive
  dec d
_sc_branch_positive:
  add hl, de
  ld (SC_EFFECTIVE), hl
  ld a, 3
  ld (SC_TOTAL), a
  ld a, h
  cp b
  ret z
  ld h, b
  ld (SC_WRONG), hl
  ld a, 4
  ld (SC_TOTAL), a
  ld a, 3
  ld (SC_POLL2), a
  ret
_sc_branch_masks:
  .db $80,$40,$01,$02

_sc_prefix:
  call _sc_fetch_byte       ; opcode, cycle1
  ld a, (SC_SEQUENCE)
  cp 4
  jp z, _sc_prefix_branch
  cp 3
  jr z, _sc_prefix_implied
  cp 5
  jr z, _sc_prefix_implied
  cp 6
  jr z, _sc_prefix_pull
  cp 7
  jr z, _sc_prefix_jsr
  cp 8
  jr z, _sc_prefix_pull
  cp 9
  jr z, _sc_prefix_pull
  cp 10
  jr z, _sc_prefix_brk
  call _sc_fetch_byte       ; immediate or low operand
  ld a, (SC_SIZE)
  cp 3
  call z, _sc_fetch_byte
  ld a, (SC_SEQUENCE)
  cp 11
  ret nc                   ; JMP's remaining indirect reads use own helper
  ld a, (SC_MODE)
  cp 2
  ret z
  cp 10
  jr z, _sc_prefix_indx
  cp 11
  jr z, _sc_prefix_indy
  cp 4
  jr z, _sc_prefix_zp
  cp 5
  jr z, _sc_prefix_zp
  cp 7
  jr z, _sc_prefix_indexed
  cp 8
  jr z, _sc_prefix_indexed
  jr _sc_prefix_nop
_sc_prefix_implied:
  ld hl, (SC_FETCH)
  jp _sc_read
_sc_prefix_pull:
  call _sc_prefix_implied
  jp _sc_stack_dummy
_sc_prefix_jsr:
  call _sc_fetch_byte
  jp _sc_stack_dummy
_sc_prefix_brk:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  ld a, (SC_NMI_EDGE)
  or a
  jp nz, rt_cnrom_unsupported
  ld a, 1
  ld (SC_ENTRY), a
  jp _sc_fetch_byte         ; BRK padding byte, not next instruction
_sc_prefix_zp:
  ld hl, (SC_OPERAND)
  ld h, 0
  call _sc_read
  jr _sc_prefix_nop
_sc_prefix_indx:
  ld hl, (SC_OPERAND)
  ld h, 0
  call _sc_read
_sc_prefix_indy:
  ld hl, (SC_POINTER)
  call _sc_read
  inc l
  call _sc_read
  ld a, (SC_MODE)
  cp 10
  jr z, _sc_prefix_nop
_sc_prefix_indexed:
  ld a, (SC_SEQUENCE)
  or a
  jr nz, _sc_prefix_dummy
  ld a, (SC_CROSS)
  or a
  jr z, _sc_prefix_nop
_sc_prefix_dummy:
  ld hl, (SC_WRONG)
  call _sc_read
_sc_prefix_nop:
  ld a, (SC_SEQUENCE)
  or a
  ret nz
  ld a, (SC_OPCODE)
  cp $b4
  ret z
  cp $bc
  ret z
  ; All stable memory NOPs have low five opcode bits $04,$0C,$14,$1C.
  ld a, (SC_OPCODE)
  and $1f
  cp 4
  jr z, _sc_nop_read
  cp $0c
  jr z, _sc_nop_read
  cp $14
  jr z, _sc_nop_read
  cp $1c
  ret nz
_sc_nop_read:
  ; Official BIT ($24/$2C), STY/LDY/CPY/CPX share some encodings: only the
  ; six absolute-indexed NOPs and explicit zp/absolute variants are NOPs.
  ld a, (SC_OPCODE)
  cp $04
  jr z, _sc_nop_final
  cp $44
  jr z, _sc_nop_final
  cp $64
  jr z, _sc_nop_final
  cp $0c
  jr z, _sc_nop_final
  and $1f
  cp $14
  jr z, _sc_nop_final
  cp $1c
  ret nz
_sc_nop_final:
  ld hl, (SC_EFFECTIVE)
  jp _sc_read
_sc_prefix_branch:
  call _sc_fetch_byte
  ld a, (SC_TAKEN)
  or a
  ret z
  ld hl, (SC_FETCH)
  call _sc_read
  ld a, (SC_TOTAL)
  cp 4
  ret nz
  ld hl, (SC_WRONG)
  jp _sc_read
_sc_fetch_byte:
  ld hl, (SC_FETCH)
  call _sc_read
  inc hl
  ld (SC_FETCH), hl
  ret
_sc_stack_dummy:
  ld a, ($cb02)
  ld l, a
  ld h, 1
  jp _sc_read

; Actual semantic data accesses validate the precomputed address. Prefix and
; stack accesses use internal entry points with their explicit raw addresses.
rt_source_read_bus:
  call _sc_check_address
  jp _sc_read
rt_source_write_bus:
  call _sc_check_address
  jp _sc_write
_sc_check_address:
  push af
  push de
  ld de, (SC_EFFECTIVE)
  ld a, h
  cp d
  jp nz, rt_source_phase_error
  ld a, l
  cp e
  jp nz, rt_source_phase_error
  pop de
  pop af
  ret
_sc_read:
  push bc
  push de
  push hl
  ld a, (SC_DMA_PENDING)
  or a
  call nz, _sc_dma
  call _sc_cycle
  call rt_source_bus_read_event
  ld (SC_RESULT), a
  call _sc_poll
  ld a, (SC_RESULT)
  pop hl
  pop de
  pop bc
  ret
_sc_write:
  push af
  push bc
  push de
  push hl
  push af
  call _sc_cycle
  pop af
  call rt_source_bus_write_event
  call _sc_poll
  pop hl
  pop de
  pop bc
  pop af
  ret
; Shared transfer observation points, also used by DMA. Cycle already advanced.
rt_source_bus_read_event:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  push af
  ld a, h
  cp $20
  jr c, _shw_read_ready
  cp $80
  jr nc, _shw_read_ready
  call rt_source_domain_flush
_shw_read_ready:
  pop af
rt_source_bus_read_transfer: ; post-flush for hardware; pending permitted RAM/ROM
.endif
  xor a
  ld (SC_EVENT_KIND), a
  ld (SC_EVENT_ADDR), hl
  call rt_cpu_read_bus
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  push af
  ld a, h
  cp $40
  jr nz, _sc_external_read
  ld a, l
  cp $15
  jr nz, _sc_external_read
  pop af                  ; internal APU status does not drive external bus
  ret
_sc_external_read:
  pop af
.endif
  ld (SC_BUS), a
  ret
rt_source_bus_write_event:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  push af
  ld a, h
  cp $20
  jr c, _shw_write_ready
  call rt_source_domain_flush
_shw_write_ready:
  pop af
rt_source_bus_write_transfer: ; post-flush for hardware; pending permitted RAM
.endif
  push af
  ld a, 1
  ld (SC_EVENT_KIND), a
  ld (SC_EVENT_ADDR), hl
  pop af
  ld (SC_BUS), a
  jp rt_cpu_write_bus
_sc_cycle:
  push af
  ld a, (SC_PHASE)
  inc a
  ld (SC_PHASE), a
  push bc
  ld b, a
  ld a, (SC_TOTAL)
  cp b
  jp c, rt_source_phase_error
  pop bc
  pop af
  jp _sc_tick
_sc_poll:
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld a, (SC_PHASE)
  ld b, a
  ld a, (SC_POLL1)
  cp b
  jr z, _sc_hardware_poll
  ld a, (SC_POLL2)
  cp b
  ret nz
_sc_hardware_poll:
  ld a, (SC_NMI_EDGE)
  or a
  jr nz, _sc_accept
  ld a, (SC_ACCEPTED)
  or a
  ret nz                  ; a branch's earlier poll remains accepted
  ld a, ($cb03)
  and 4                   ; CLI/SEI/PLP still have old P at their poll
  ret nz                  ; RTI restores P before its final stack reads
  ld a, (SH_IRQ_LINE)
  or a
  ret z
  ld a, 2
  ld (SC_ACCEPTED), a
  ret
.else
  ld a, (SC_NMI_EDGE)
  or a
  ret z
  ld a, (SC_PHASE)
  ld b, a
  ld a, (SC_POLL1)
  cp b
  jr z, _sc_accept
  ld a, (SC_POLL2)
  cp b
  ret nz
.endif
_sc_accept:
  ld a, 1
  ld (SC_ACCEPTED), a
  xor a
  ld (SC_NMI_EDGE), a
  ret

; One original CPU cycle, no instruction-phase changes. Frame/dot counters
; advance by local increments, so completed-cycle u32 wrap changes no deadline.
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
rt_source_tick:
.endif
_sc_tick:
  push af
  push bc
  push de
  push hl
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_tick
  pop hl
  pop de
  pop bc
  pop af
  ret
.else
  ld hl, SC_CYCLES
  call _sc_inc32
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  call rt_source_ppu_cycle
  call rt_source_apu_cycle
  pop hl
  pop de
  pop bc
  pop af
  ret
.else
  ld b, 3
_sc_dot:
  ld hl, (SC_DOT)
  inc hl
  ld de, 341
  or a
  sbc hl, de
  jr nc, _sc_new_line
  add hl, de
  ld (SC_DOT), hl
  ld a, h
  or a
  jr nz, _sc_dot_done
  ld a, l
  cp 1
  jr nz, _sc_dot_done
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sc_clear_line
  ld a, l
  cp 241
  jr nz, _sc_dot_done
  ld a, 1
  ld (SC_VBLANK), a
  call rt_source_nmi_line
  jr _sc_dot_done
_sc_clear_line:
  ld a, l
  cp 5                    ; 261 = $0105
  jr nz, _sc_dot_done
  xor a
  ld (SC_VBLANK), a
  call rt_source_nmi_line
  jr _sc_dot_done
_sc_new_line:
  ld (SC_DOT), hl          ; zero (one-dot increments never skip a line)
  ld hl, (SC_LINE)
  inc hl
  ld de, 262
  or a
  sbc hl, de
  jr nc, _sc_new_frame
  add hl, de
  ld (SC_LINE), hl
  jr _sc_dot_done
_sc_new_frame:
  ld (SC_LINE), hl
  ld hl, SC_FRAME
  call _sc_inc32
_sc_dot_done:
  djnz _sc_dot
  pop hl
  pop de
  pop bc
  pop af
  ret
.endif
.endif
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
rt_source_inc32:
.endif
_sc_inc32:
  inc (hl)
  ret nz
  inc hl
  inc (hl)
  ret nz
  inc hl
  inc (hl)
  ret nz
  inc hl
  inc (hl)
  ret

rt_source_nmi_line:
  push bc
  ld a, (SC_VBLANK)
  ld c, a
  ld a, ($cb08)
  rlca
  and c
  ld c, a
  ld a, (SC_NMI_LINE)
  xor c
  and c
  jr z, _sc_line_stable
  ld a, (SC_ENTRY)
  or a
  jp nz, rt_cnrom_unsupported
  ld a, (SC_DMA_ACTIVE)
  or a
  jp nz, rt_cnrom_unsupported
  ld a, 1
  ld (SC_NMI_EDGE), a
_sc_line_stable:
  ld a, c
  ld (SC_NMI_LINE), a
  pop bc
  ret

rt_source_status_read:
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  call rt_source_ppu_status_window
.else
  ; Explicitly unsupported same-dot set/clear neighborhoods, not fake races.
  ld hl, (SC_LINE)
  ld a, h
  or a
  jr nz, _sc_status_pre
  ld a, l
  cp 240
  jr z, _sc_status_before
  cp 241
  jr z, _sc_status_after
  jr _sc_status_value
_sc_status_pre:
  ld a, l
  cp 4
  jr z, _sc_status_before
  cp 5
  jr nz, _sc_status_value
_sc_status_after:
  ld hl, (SC_DOT)
  ld a, h
  or a
  jr nz, _sc_status_value
  ld a, l
  cp 5
  jp c, rt_cnrom_unsupported
  jr _sc_status_value
_sc_status_before:
  ld hl, (SC_DOT)
  ld de, 338
  or a
  sbc hl, de
  jp nc, rt_cnrom_unsupported
.endif
_sc_status_value:
  ld a, (CN_PPU_LATCH)
  and $1f
  ld c, a
  ld a, (SC_VBLANK)
  rrca
  or c
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld c, a
  ld a, (SP_STATUS)
  and $60
  or c
.endif
  push af
  xor a
  ld (SC_VBLANK), a
  ld ($cb0e), a
  call rt_source_nmi_line
  pop af
  ld (CN_PPU_LATCH), a
  ret

rt_source_push:
  push hl
  push af
  ld a, ($cb02)
  ld l, a
  ld h, 1
  dec a
  ld ($cb02), a
  pop af
  call _sc_write
  pop hl
  ret
rt_source_pop:
  push hl
  ld a, ($cb02)
  inc a
  ld ($cb02), a
  ld l, a
  ld h, 1
  call _sc_read
  pop hl
  ret
rt_source_jsr:
  push af
  push hl
  ld hl, (SC_PC)
  inc hl
  inc hl
  ld a, h
  call rt_source_push
  ld a, l
  call rt_source_push
  call _sc_fetch_byte      ; late high operand fetch, cycle6
  pop hl
  pop af
  ret
rt_source_rts:
  ld (SC_SAVED_A), a
  call _sc_pop_target
  call _sc_read            ; RTS's final dummy read uses stacked PC
  inc hl
  jp _sc_control_dispatch
rt_source_rti:
  ld (SC_SAVED_A), a
  call rt_source_pop
  and $cf
  or $20
  ld ($cb03), a
  call _sc_pop_target
  jp _sc_control_dispatch
_sc_pop_target:
  call rt_source_pop
  ld (SC_TARGET), a
  call rt_source_pop
  ld (SC_TARGET+1), a
  ld hl, (SC_TARGET)
  ret
rt_source_indirect_jump:
  ld (SC_SAVED_A), a
  call _sc_read
  ld (SC_TARGET), a
  inc l                   ; NMOS page-wrap, not INC HL
  call _sc_read
  ld h, a
  ld a, (SC_TARGET)
  ld l, a
_sc_control_dispatch:
  ld a, (SC_PHASE)
  ld b, a
  ld a, (SC_TOTAL)
  cp b
  jp nz, rt_source_phase_error
  ld a, (SC_SAVED_A)
  ld b, h
  ld c, l
  jp rt_banked_tail_dispatch

rt_source_nmi:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  ld (SC_SAVED_A), a
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld a, (SC_ACCEPTED)     ; 1=NMI, 2=source APU IRQ
  ld (SC_ENTRY), a
.endif
  xor a
  ld (SC_ACCEPTED), a
  ld (SC_PHASE), a
  ld (SC_POLL1), a
  ld (SC_POLL2), a
  ld a, 7
  ld (SC_TOTAL), a
.ifndef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld a, 1
  ld (SC_ENTRY), a
.endif
  ld hl, (SC_PC)
  call _sc_read
  call _sc_read
  ld a, h
  call rt_source_push
  ld a, l
  call rt_source_push
  ld a, ($cb03)
  and $ef
  or $20
  call rt_source_push
  ld hl, $fffa
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
  ld a, (SC_ENTRY)
  cp 2
  jr nz, _sc_interrupt_vector
  ld hl, $fffe
.endif
  jr _sc_interrupt_vector
rt_source_brk:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  ld (SC_SAVED_A), a
  ld hl, (SC_PC)
  inc hl
  inc hl
  ld a, h
  call rt_source_push
  ld a, l
  call rt_source_push
  ld a, ($cb03)
  or $30
  call rt_source_push
  ld hl, $fffe
_sc_interrupt_vector:
  ld a, ($cb03)
  or 4
  ld ($cb03), a
  call _sc_read
  ld (SC_TARGET), a
  inc hl
  call _sc_read
  ld h, a
  ld a, (SC_TARGET)
  ld l, a
  xor a
  ld (SC_ENTRY), a
  jp _sc_control_dispatch

rt_source_dma_request:
  ld a, b
  ld (SC_DMA_PAGE), a
  ld a, 1
  ld (SC_DMA_PENDING), a
  ret
_sc_dma:
.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
  call rt_source_domain_flush
.endif
  push hl
  xor a
  ld (SC_DMA_PENDING), a
  ld (SC_DMA_OFFSET), a
  ld a, 1
  ld (SC_DMA_ACTIVE), a
  call _sc_tick           ; halt read; no ordinary instruction poll
  call rt_source_bus_read_event
  ld a, (SC_CYCLES)
  ld b, a
  ld a, (SC_DMA_ALIGN)
  xor b
  and 1
  jr z, _sc_dma_pairs      ; next transfer odd=get under default phase
  call _sc_tick
  call rt_source_bus_read_event ; alignment repeats halted read address
_sc_dma_pairs:
  ld a, (SC_DMA_PAGE)
  ld h, a
  ld a, (SC_DMA_OFFSET)
  ld l, a
  call _sc_tick
  call rt_source_bus_read_event
  push af
  call _sc_tick
  pop af
  ld hl, $2004
  call rt_source_bus_write_event
  ld a, (SC_DMA_OFFSET)
  inc a
  ld (SC_DMA_OFFSET), a
  jr nz, _sc_dma_pairs
  xor a
  ld (SC_DMA_ACTIVE), a
  pop hl
  ret

.ifdef CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT
; Only the domain calls are deferred. Every original CPU transfer and poll
; remains present. The materialized PPU/APU epoch is SC_CYCLES-SHW_PENDING.
; Called inside the preserving source-tick wrapper; scratch registers allowed.
rt_source_domain_format_v3:
rt_source_domain_tick:
  ld hl, (SHW_REMAIN)
  ld a, h
  or l
  jr nz, _shw_defer_cycle
  call rt_source_domain_flush
  ld a, (SC_ENTRY)
  ld b, a
  ld a, (SC_DMA_PENDING)
  or b
  ld b, a
  ld a, (SC_DMA_ACTIVE)
  or b
  ld b, a
  ld a, (SC_NMI_EDGE)
  or b
  ld b, a
  ld a, (SC_ACCEPTED)
  or b
  jr nz, _shw_precise_cycle
  ld a, (CNP_STATE)
  cp 2
  jr nc, _shw_precise_cycle
  call rt_source_ppu_interval_deadline
  ld (SHW_KIND), a
  ld a, h
  or l
  jr z, _shw_precise_cycle
  push hl
  call rt_source_apu_deadline
  pop de
  ld a, h
  or l
  jr z, _shw_precise_cycle
  or a
  sbc hl, de
  jr nc, _shw_ppu_first
  add hl, de
  jr _shw_capacity
_shw_ppu_first:
  ex de, hl
_shw_capacity:
  dec hl                    ; strict interval: never reach either deadline
  ld a, h
  or l
  jr z, _shw_precise_cycle
  ld (SHW_REMAIN), hl
  ld hl, (SC_CYCLES)
  ld (SHW_START), hl
  ld hl, (SC_CYCLES+2)
  ld (SHW_START+2), hl
  ld hl, (SC_DOT)
  ld (SHW_START_DOT), hl
  ld hl, (SC_LINE)
  ld (SHW_START_LINE), hl
_shw_defer_cycle:
  ld hl, SC_CYCLES
  call _sc_inc32
  ld hl, (SHW_PENDING)
  inc hl
  ld (SHW_PENDING), hl
  ld hl, (SHW_REMAIN)
  dec hl
  ld (SHW_REMAIN), hl
  ret
_shw_precise_cycle:
  ld hl, SC_CYCLES
  call _sc_inc32
  call rt_source_ppu_cycle
  jp rt_source_apu_cycle

; Normalize all source domains without charging a source cycle. Preserve all
; registers/IFF; hardware callers execute their event tap only AFTER this.
; An interval diagnostic may reenter the common trap: never recursively flush.
rt_source_domain_flush:
  push af
  ld a, (SHW_FLUSH_ACTIVE)
  or a
  jr nz, _shw_flush_return
  push bc
  push de
  push hl
  ld bc, (SHW_PENDING)
  ld a, b
  or c
  jr z, _shw_flush_empty
  ld a, 1
  ld (SHW_FLUSH_ACTIVE), a
  ld (SHW_SPAN_COUNT), bc
rt_source_domain_span_begin:
  ; START is before the first deferred cycle; SC_CYCLES is the true endpoint.
  ; Kind1 is inactive; kind2/3 reconstruct BG/sprite fetches. No status/line
  ; or APU event is crossed. MODE2 callbacks are internal, not source events.
  ld a, (SHW_KIND)
  call rt_source_ppu_interval_advance
  call rt_source_apu_quiet_advance
  xor a
  ld (SHW_FLUSH_ACTIVE), a
  ld hl, 0
  ld (SHW_PENDING), hl
  ld (SHW_REMAIN), hl
rt_source_domain_span_end:
  jr _shw_flush_registers
_shw_flush_empty:
  ld hl, 0
  ld (SHW_REMAIN), hl
_shw_flush_registers:
  pop hl
  pop de
  pop bc
_shw_flush_return:
  pop af
  ret

; Safe completed-instruction service. Ordinary quiet instructions do not force
; materialization; an accepted interrupt or actual hardware publication does.
rt_source_domain_boundary:
  push af
  ld a, (SC_ACCEPTED)
  or a
  call nz, rt_source_domain_flush
  ld a, (CNP_STATE)
  cp 2
  jr nz, _shw_boundary_audio
  call rt_source_domain_flush
  call rt_cnrom_packet_service
_shw_boundary_audio:
  ld a, (SAP_DIRTY)
  or a
  jr z, _shw_boundary_done
  call rt_source_domain_flush
  call rt_source_apu_publish
_shw_boundary_done:
  pop af
  ret
.endif
.ends
.endif
