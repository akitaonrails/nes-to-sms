; CV1's completed graphics prologue -> IRQ handoff. Opt-in only; the normal
; NROM/SMB instruction stream is unchanged by these opt-in hooks. The phase3
; fallback freezes controls only and still has direct PPU/CHR/CRAM writers.
; CV1_COHERENT_BG instead prepares all BG/CRAM/HUD/SAT before READY: later raw
; producer writes affect only next-generation source/dirty intent, never the
; frozen visible payload. Its final consumer requires no control swap/barrier.
;
; CV1 does not use vbuf_push (that API is unfinished). Reserve $C820-$C83D
; from its dormant buffer; leave $C800 (vbuf_flush length) zero. $CA40-$CAFF
; is NOT free: chrmap owns it as the BG variant reverse map.
;
; Three records: READY packet, live producer, temporary interrupted-live copy.
; Fields 0..8: CTRL,MASK,scrollX,Y,split flags,preX,Y,postX,Y.
; Fields 9..12: pending reg1,table refresh,band dirty,off-render write count.
; Publication transfers pending intent to the packet; consumption restores
; newer live intent and merges anything the old packet did not consume.
; CA13/CB2A and the variant cache are committed presentation state: never swap.
; New full producers wait until READY/PENDING retires. The phase3 fallback
; serializes a resumed PPUDATA writer; the coherent path has no such wait.
; C810=pending. C811/12 is phase3 PPUDATA address scratch, or coherent stable
; guest-DE parking while pending=0 and the full prologue prepares its output.
.ifdef CV1_RUNTIME_HOOKS
.ifndef DEFER_SPRITE_REGISTERS
.fail "CV1 presentation requires translation.defer_sprite_registers"
.endif
.define CV1_FRAME_READY $c820
.define CV1_FRAME_PENDING $c810
.define CV1_FRAME_DATA_ADDR $c811
.define CV1_FRAME_PACKET $c821
.define CV1_FRAME_LIVE_SAVE $c82e
.define CV1_FRAME_HOOK_IFF $c83b
.define CV1_FRAME_PUBLISH_COUNT $c83c
.define CV1_FRAME_CONSUME_COUNT $c83d

.section "frame_cv1" free

.ifdef CV1_COHERENT_BG
; Original F868: STATUS, scroll0/0, CTRL FF&FE. Its status read can consume
; the synthetic clear edge before F8C7 waits for it. Physical HUD splitting
; belongs to SMS HINT; rearm the clear observation AFTER the original pair,
; leaving both original wait loops (including rendering-off timeouts) intact.
; Native CALL/RET ABI; DE, shadow flags except final N/Z, mapper and IFF stay
; unchanged. Guarded PPU calls provide their existing short service boundaries;
; do not turn this entire routine into one long DI interval.
rt_cv1_split_prepare:
  ld b, 2
  call rt_ppu_read
  xor a
  ld b, 5
  call rt_ppu_write
  xor a
  ld b, 5
  call rt_ppu_write
  ld a, ($c0ff)
  and $fe
  ld b, 0
  call rt_ppu_write
  ld a, ($cb09)
  and $18
  jr z, _cv1_split_flags
  ld a, 1
  ld ($cb12), a
_cv1_split_flags:
  ld a, ($c0ff)
  and $fe
  ld l, a
  ld h, $3e
  ld a, ($cb03)
  and $7d
  or (hl)
  ld ($cb03), a
  ld a, l
  ret
.endif

; Existing profile replacement ABI: native CALL/RET, D/E = NES X/Y.
; Original C11F: read STATUS; scroll FD,FC; CTRL FF. The status value is dead;
; final A and shadow N/Z come from FF. All other shadow flags and D/E survive.
rt_cv1_scroll_publish:
  ld a, i
  di
  jp po, _cv1_hook_di
  ld a, 1
  jr _cv1_hook_iff
_cv1_hook_di:
  xor a
_cv1_hook_iff:
  ld (CV1_FRAME_HOOK_IFF), a
.ifdef CV1_COHERENT_BG
  ld a, ($c01b)
  or a
  jr nz, _cv1_hook_ppu_begin  ; lag keeps the original guarded PPU ABI; no EI prep
  call _cv1_prepare_context_check
  ld a, (CV1_FRAME_READY)
  ld hl, CV1_FRAME_PENDING
  or (hl)
  jp nz, _cv1_present_bad
  ld (CV1_FRAME_DATA_ADDR), de
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  call z, _cv1_prepare_blank
  ld b, 17                  ; original guarded PPU ops3363T +512T glue
  call _cv1_prepare_admit
  ld de, (CV1_FRAME_DATA_ADDR)
.endif
_cv1_hook_ppu_begin:
  ld b, 2
  call rt_ppu_read
  ld a, ($c0fd)
  ld b, 5
  call rt_ppu_write
  ld a, ($c0fc)
  ld b, 5
  call rt_ppu_write
  ld a, ($c0ff)
  ld b, 0
  call rt_ppu_write
  ; D/E must survive the record copier too.
  ld a, ($c01b)
  or a
  jr nz, _cv1_hook_flags
.ifdef CV1_COHERENT_BG
  ; Second mapping-closed chunk: original PPU operations have completed.
  ; Native A is dead; guest shadowP and saved DE remain authoritative.
  ld b, 16
  call _cv1_prepare_admit
  ld de, (CV1_FRAME_DATA_ADDR)
.endif
_cv1_hook_capture_begin:
  push de
  ld de, CV1_FRAME_PACKET
  call _cv1_frame_capture
  ; Transfer all old dirty intent. A later producer write now belongs to
  ; the next packet even if this one has not been consumed yet.
  xor a
  ld ($cb2d), a
  ld ($cb7f), a
  ld ($cb78), a
  ld ($ca18), a
.ifdef CV1_COHERENT_BG
  call rt_cv1_frame_prepare_all
_cv1_hook_prepared:
.endif
  ld a, (CV1_FRAME_PUBLISH_COUNT)
  inc a
  ld (CV1_FRAME_PUBLISH_COUNT), a
  ld a, 1
  ld (CV1_FRAME_READY), a      ; publish LAST while DI
  pop de
_cv1_hook_flags:
  ld a, ($c0ff)
  ld l, a
  ld h, $3e
  ld a, ($cb03)
  and $7d
  or (hl)
  ld ($cb03), a
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  ld a, ($c0ff)
  ret z
  ei
  ret

.ifdef CV1_COHERENT_BG
; All safe EI points require these facts, proven once before the original
; C11F hardware operations. The helpers restore this exact state per step.
_cv1_prepare_context_check:
  ld a, i
  jp pe, _cv1_present_bad
  ld a, ($d47f)
  or a
  jp nz, _cv1_present_bad
  ld a, ($ca11)
  cp 1
  jp nz, _cv1_present_bad
  ld a, ($fffc)
  or a
  jp nz, _cv1_present_bad
  ld a, ($ffff)
  ld b, a
  ld a, ($cb62)
  and NES_PRG_BANK_MASK
  add a, NES_PRG_BANK_BASE
  cp b
  jp nz, _cv1_present_bad
  ret

; IRQ-origin consumer, called immediately after epoch bookkeeping and BEFORE
; old HUD/input overhead. Prepared payloads no longer require swapping live
; controls or blocking a PPUDATA producer. Every newer dirty bit remains live.
; A=1 committed, A=0 no commit. Exact FFFC/FFFF/CB62/guard/IFF are preserved.
rt_cv1_frame_try_present:
  ld a, ($ca12)
  or a
  jr nz, _cv1_present_none
  ld a, (CV1_FRAME_READY)
  ld hl, CV1_FRAME_PENDING
  or (hl)
  jr z, _cv1_present_none
  ld a, i
  jp pe, _cv1_present_bad
  ld a, ($d47f)
  cp 3
  jp nc, rt_ppu_guard_overflow
  ld a, ($c800)
  or a
  jr nz, _cv1_present_vbuf
  ld a, ($c802)
  bit 0, a
  jr z, _cv1_present_bad
  ; The entire last-visible-OUT bound must fit the latest E3:34 full NTSC
  ; stock lines=7752T. Tests execute every opcode, not emulator approximate
  ; cycle counters, and include this sample/admission/caller path.
  in a, ($7e)
  sub $e0
  cp 4
  jr nc, _cv1_present_late
  ld a, ($fffc)
  push af
  ld a, 8
  ld ($fffc), a
  call rt_cv1_bg_commit_admitted
  ld hl, CV1_BG_HUD
  call rt_cv1_hud_commit
  pop af
  ld ($fffc), a
  ; SAT/reg1 is LAST: display enable never precedes new BG/CRAM/HUD controls.
  call rt_cv1_sat_commit_admitted
  xor a
  ld (CV1_FRAME_READY), a
  ld (CV1_FRAME_PENDING), a
  ld a, (CV1_FRAME_CONSUME_COUNT)
  inc a
  ld (CV1_FRAME_CONSUME_COUNT), a
  ld a, 1
  ret
_cv1_present_late:
  ld a, 1
  ld (CV1_FRAME_PENDING), a
_cv1_present_none:
  xor a
  ret
_cv1_present_vbuf:
  ld a, $fa
  jr _cv1_present_trap
_cv1_present_bad:
  ld a, $f8
_cv1_present_trap:
  ld ($cb1d), a
  di
_cv1_present_halt:
  halt
  jr _cv1_present_halt

; Full C11F only: busy0 + CA11=1 blocks nested translated producers, while
; committed HUD/input/audio can run between bounded, mapping-closed steps.
rt_cv1_frame_prepare_all:
  ld a, 1
  ld ($ca12), a
  call rt_raw_ciram_sram_enable
  ld hl, CV1_FRAME_PACKET
  ld de, CV1_BG_HUD
  call rt_cv1_hud_prepare
  call rt_raw_ciram_sram_disable
  ; Preserve an intentionally-DI caller by fencing display off first. Normal
  ; full NMIs entered with EI and use the bounded service points below.
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  call z, _cv1_prepare_blank
  call rt_cv1_bg_prepare_begin
_cv1_prepare_bg:
  call _cv1_prepare_service
  call rt_cv1_bg_chunk_lines
  call _cv1_prepare_admit_parked
  call rt_cv1_bg_prepare_step
  or a
  jr z, _cv1_prepare_sprites
  cp 2
  jr nz, _cv1_prepare_bg
  call _cv1_prepare_blank
  call rt_cv1_bg_prepare_blanked
  jr _cv1_prepare_bg
_cv1_prepare_sprites:
  call rt_cv1_sat_prepare_begin
_cv1_prepare_sat:
  call _cv1_prepare_service
  call rt_cv1_sat_step_lines
  call _cv1_prepare_admit_parked
  call rt_cv1_sat_prepare_step
  or a
  jr z, _cv1_prepare_done
  cp 2
  jr nz, _cv1_prepare_sat
  call _cv1_prepare_blank
  jr _cv1_prepare_sat
_cv1_prepare_done:
  xor a
  ld ($ca12), a
  ret

; B=max whole-step stock lines. No volatile converter state crosses here.
; Restore guest DE before EI because IRQ entry mirrors it to CB00/01. Mapping
; is currentPRG/SRAMoff/guard0, latch closed, and all cursors are persistent.
_cv1_prepare_admit:
  push bc
  call _cv1_prepare_service
  jr _cv1_prepare_admit_check
; BG/SAT loops service BEFORE their read-only classification. This keeps the
; classifier outside the preceding chunk's tail-to-next-IRQ latency budget;
; the current chunk's clock begins at the predicate's following IN sample.
_cv1_prepare_admit_parked:
  push bc
_cv1_prepare_admit_check:
  ld a, ($c802)
  bit 1, a
  jr nz, _cv1_prepare_admitted
  call rt_cv1_hud_try_chunk
  jr nc, _cv1_prepare_admitted
  pop bc
  push bc
  call _cv1_prepare_wait_irq
  pop bc
  jr _cv1_prepare_admit
_cv1_prepare_admitted:
  pop bc
  ret
_cv1_prepare_service:
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  ret z
  ld de, (CV1_FRAME_DATA_ADDR)
  ei
  nop
  di
  ret
_cv1_prepare_wait_irq:
  ld a, (CV1_FRAME_HOOK_IFF)
  or a
  jp z, rt_cv1_hud_poll_line
  ld de, (CV1_FRAME_DATA_ADDR)
  ei
  ; A direct/no-split title has no HINT. HALT would wake only at VBlank,
  ; which the chunk predicate rejects, and repeat forever. Poll with an IRQ
  ; service opportunity so the beam can progress into an admissible phase.
  nop
  di
  ret
_cv1_prepare_blank:
  ld a, ($c802)
  bit 1, a
  ret nz
_cv1_prepare_blank_wait:
  call _cv1_prepare_service
  in a, ($7e)
  sub $e0
  cp 13
  jp c, rt_cv1_sat_blank
  call _cv1_prepare_wait_irq
  jr _cv1_prepare_blank_wait
.endif

; Private DI-only copier. DE=record destination; clobbers AF/BC/DE/HL.
_cv1_frame_capture:
  ld hl, _cv1_frame_fields
  ld b, 13
_cv1_capture_byte:
  ld a, (hl)
  inc hl
  ld c, (hl)
  inc hl
  push hl
  ld l, a
  ld h, c
  ld a, (hl)
  ld (de), a
  inc de
  pop hl
  djnz _cv1_capture_byte
  ret

; Private DI-only copier. DE=record source; clobbers AF/BC/DE/HL.
_cv1_frame_install:
  ld hl, _cv1_frame_fields
  ld b, 13
_cv1_install_byte:
  ld a, (hl)
  inc hl
  ld c, (hl)
  inc hl
  push hl
  ld l, a
  ld h, c
  ld a, (de)
  ld (hl), a
  inc de
  pop hl
  djnz _cv1_install_byte
  ret

; IRQ has checked READY and owns DI. Keep the current producer record separate
; while unchanged presentation helpers consume the frozen packet.
rt_cv1_frame_begin:
  xor a
  ld (CV1_FRAME_READY), a
  ld a, (CV1_FRAME_CONSUME_COUNT)
  inc a
  ld (CV1_FRAME_CONSUME_COUNT), a
  ; Fall through: retries share the copier but never consume READY again.
rt_cv1_frame_resume:
  ld de, CV1_FRAME_LIVE_SAVE
  call _cv1_frame_capture
  ld de, CV1_FRAME_PACKET
  jp _cv1_frame_install

; Preparation finished but its final bounded commit was late. The prepared
; SAT owns old reg1; transfer all remaining dirty intent to live ONCE, not on
; every future retry (an unconsumed CA18 count would otherwise be added again).
; Retain only frozen controls; resume installs zero pending-intent fields.
rt_cv1_frame_suspend:
  ld de, CV1_FRAME_PACKET
  call _cv1_frame_capture
  ld hl, CV1_FRAME_PACKET + 9
  ld bc, 4
  xor a
  call mem_fill
  xor a
  ld ($cb2d), a
  call rt_cv1_frame_end
  ; Publish only after live control is restored; caller still owns DI.
  ld a, 1
  ld (CV1_FRAME_PENDING), a
  ret

; No translated producer ran during presentation (DI). Preserve newer pending
; writes saved at begin, and merge unconsumed old intent before restoring live
; control. reg1 is a latest-value latch; a newer live value wins over old.
rt_cv1_frame_end:
  ld a, (CV1_FRAME_LIVE_SAVE + 9)
  or a
  jr nz, _cv1_restore_reg1_ready
  ld a, ($cb2d)
  ld (CV1_FRAME_LIVE_SAVE + 9), a
_cv1_restore_reg1_ready:
  ld a, ($cb7f)
  ld hl, CV1_FRAME_LIVE_SAVE + 10
  or (hl)
  ld (hl), a
  ld a, ($cb78)
  inc hl
  or (hl)
  ld (hl), a
  ld a, ($ca18)
  inc hl
  add a, (hl)
  jr nc, _cv1_restore_count_ready
  ld a, $ff
_cv1_restore_count_ready:
  ld (hl), a
  ld de, CV1_FRAME_LIVE_SAVE
  jp _cv1_frame_install

_cv1_frame_fields:
  .dw $cb08, $cb09, $cb0c, $cb0d, $cb20, $cb21, $cb22, $cb23, $cb24
  .dw $cb2d, $cb7f, $cb78, $ca18
.ends
.endif
