; CV1-only shared sprite patterns and prepared SAT. Preparation/blanking require
; DI, the frozen frame record, and asserted depth-0 current-PRG mapping.
; Commit entries need only DI, a closed VDP latch and a complete prepared SAT:
; fixed code/internal RAM/ports permit any valid guard depth or SRAM/bank map.
; All entries clobber AF/BC/DE/HL and preserve IFF and mapper/SRAM ownership.
; Preparation never yields partial data. No IRQ/status-port read.
;
; The existing 64 pair slots at $2000-$2FFF are shared by (tile, attr&C3).
; Current pins live until SAT commit; pass A pins ALL next hits before pass B
; allocates. Neither visible generation can be evicted during preparation.
; 8x8 uses the same slots/source-aware converter (first 32 bytes only), not
; the old base patterns disabled by CA39. Register 6 therefore stays $FF;
; NES source-table selection changes SRAM reads, never the physical SMS base.
;
; CV1's vbuf remains dormant: C800 MUST stay zero. C820-C83D is frame_cv1.
.ifdef CV1_RUNTIME_HOOKS
.ifndef NES_CHR_RAM
  .fail "CV1 shared sprite cache requires CHR RAM"
.endif
.define RUNTIME_HAS_SPRITE_REGISTER_COMMIT 1
.define CV1_SAT_CHR_DIRTY $c801
.define CV1_SAT_FLAGS $c802       ; bit0 ready, bit1 blanked, bit2 init, bit3 misses
.define CV1_SAT_REG1 $c803
.define CV1_SAT_REG6 $c804
.define CV1_SAT_COUNT $c805
.define CV1_SAT_OAM_INDEX $c806
.define CV1_SAT_TILE $c807
.define CV1_SAT_ATTR $c808
.define CV1_SAT_SLOT $c809
.define CV1_SAT_VICTIM $c80a
.define CV1_SAT_BUILD_COUNT $c80b ; 16-bit, DIAG_CV1_SAT only
.define CV1_SAT_COMMIT_COUNT $c80d ; 16-bit, DIAG_CV1_SAT only
.define CV1_SAT_COMMIT_VISIBLE $c80f ; byte, DIAG_CV1_SAT only
.define CV1_SAT_COMMIT_FIRST $c83e
.define CV1_SAT_COMMIT_LAST $c83f
.define CV1_SAT_Y $c840
.define CV1_SAT_XT $c880
.define CV1_SAT_CURRENT $d440
.define CV1_SAT_NEXT $d448

.section "sat_cv1" free

; Public: freeze desired registers from the installed frame, then prepare.
rt_cv1_sat_prepare:
  ld hl, CV1_SAT_FLAGS
  res 0, (hl)
  call _cv1_sat_controls
  ; Mode identity: 1=8x16, 0=8x8/table0, 2=8x8/table1.
  ld a, ($cb08)
  bit 5, a
  ld a, 1
  jr nz, _cv1_sat_mode_known
  ld a, ($cb08)
  and 8
  rrca
  rrca
_cv1_sat_mode_known:
  ld b, a
  ld hl, CV1_SAT_FLAGS
  bit 2, (hl)
  jr z, _cv1_sat_mode_change
  ld a, (SAT_PAIR_MODE)
  cp b
  jr z, _cv1_sat_mode_ready
_cv1_sat_mode_change:
  push bc
  call rt_cv1_sat_blank
  call _cv1_sat_reset_cache
  pop bc
  ld a, b
  ld (SAT_PAIR_MODE), a
  ld hl, CV1_SAT_FLAGS
  set 2, (hl)
_cv1_sat_mode_ready:
  ; The BG owner may already have blanked for a full rebuild. That fence
  ; permits releasing old pins too; otherwise a full old union would trap
  ; on its first miss merely because the one allowed blank already happened.
  ld hl, CV1_SAT_FLAGS
  bit 1, (hl)
  call nz, _cv1_sat_reset_cache
  ; A sticky invalidation cannot wrap false-valid after 256 partial writes.
  ; Leave current pins intact: stale pixels remain displayed until commit.
  ld a, (CV1_SAT_CHR_DIRTY)
  or a
  jr z, _cv1_sat_pass_a
  call _cv1_sat_invalidate
  xor a
  ld (CV1_SAT_CHR_DIRTY), a

_cv1_sat_pass_a:
  ld hl, CV1_SAT_FLAGS
  res 3, (hl)                  ; reset for each generation AND overflow retry
  ld hl, CV1_SAT_NEXT
  ld bc, 8
  xor a
  call _cv1_sat_fill
  ld hl, CV1_SAT_Y
  ld bc, 64
  ld a, $e0
  call _cv1_sat_fill
  ld hl, CV1_SAT_XT
  ld bc, 128
  xor a
  call _cv1_sat_fill
  xor a
  ld (CV1_SAT_COUNT), a
  ld (CV1_SAT_OAM_INDEX), a
  ld a, ($cb09)
  bit 4, a
  jp z, _cv1_sat_prepared       ; hidden SAT even when only BG is enabled
_cv1_sat_a_loop:
  call _cv1_sat_oam
  ld a, (hl)
  cp $cf
  jr nc, _cv1_sat_a_next
  inc a
  ld b, a
  ld a, (CV1_SAT_COUNT)
  add a, $40
  ld e, a
  ld d, $c8
  ld a, b
  ld (de), a
  call _cv1_sat_key             ; HL now points at the source X
  ld b, (hl)
  call _cv1_sat_xt
  ld (hl), b
  call _cv1_sat_lookup
  jr nc, _cv1_sat_a_hit
  ld hl, CV1_SAT_FLAGS
  set 3, (hl)
  ld a, $ff
  jr _cv1_sat_a_store
_cv1_sat_a_hit:
  ld a, c
  ld (CV1_SAT_SLOT), a
  call _cv1_sat_pin
  ld a, (CV1_SAT_SLOT)
  add a, a
_cv1_sat_a_store:
  push af
  call _cv1_sat_xt
  inc l
  pop af
  ld (hl), a
  ld hl, CV1_SAT_COUNT
  inc (hl)
_cv1_sat_a_next:
  ld hl, CV1_SAT_OAM_INDEX
  inc (hl)
  ld a, (hl)
  cp 64
  jp c, _cv1_sat_a_loop

  ; Every hit already has complete Y/XT data and a next-generation pin.
  ; An all-hit frame needs neither allocation nor a second OAM traversal.
  ld hl, CV1_SAT_FLAGS
  bit 3, (hl)
  jp z, _cv1_sat_prepared
  xor a
  ld (CV1_SAT_OAM_INDEX), a
  ld (CV1_SAT_COUNT), a
_cv1_sat_b_loop:
  call _cv1_sat_oam
  ld a, (hl)
  cp $cf
  jp nc, _cv1_sat_b_next
  call _cv1_sat_key
  call _cv1_sat_xt
  inc l
  ld a, (hl)
  cp $ff
  jr nz, _cv1_sat_b_resolved
  ; Earlier misses may already have built this key: deduplicate again.
  call _cv1_sat_lookup
  jr nc, _cv1_sat_b_hit
  call _cv1_sat_allocate
  jr nc, _cv1_sat_b_build
  ; At most 64 OAM keys exist. An empty retry MUST fit; fail closed otherwise.
  ld hl, CV1_SAT_FLAGS
  bit 1, (hl)
  jp nz, _cv1_sat_bad
  call rt_cv1_sat_blank
  call _cv1_sat_reset_cache
  jp _cv1_sat_pass_a
_cv1_sat_b_build:
  ld (CV1_SAT_SLOT), a
  call _cv1_sat_pin
.ifdef DIAG_CV1_SAT
  ld hl, (CV1_SAT_BUILD_COUNT)
  inc hl
  ld (CV1_SAT_BUILD_COUNT), hl
.endif
  ld a, (CV1_SAT_ATTR)
  ld b, a
  ld a, (CV1_SAT_SLOT)
  add a, a
  ld c, a
  ld a, (SAT_PAIR_MODE)
  cp 1
  ld a, (CV1_SAT_TILE)
  jr nz, _cv1_sat_build_8
  call rt_sat_build_pair_8x16
  jr _cv1_sat_build_done
_cv1_sat_build_8:
  ld c, a                      ; existing 8-row planar/flip/palette converter
  xor a                        ; legacy scratch index unused in this backend
  call do_sprite_variant
_cv1_sat_build_done:
  ; Publish validity only AFTER every destination pattern byte exists.
  ld a, (CV1_SAT_SLOT)
  ld l, a
  ld h, $d4
  ld a, (CV1_SAT_TILE)
  ld (hl), a
  set 7, l
  ld a, (CV1_SAT_ATTR)
  ld (hl), a
  ld a, (CV1_SAT_SLOT)
  jr _cv1_sat_b_store
_cv1_sat_b_hit:
  ld a, c
  ld (CV1_SAT_SLOT), a
  call _cv1_sat_pin
  ld a, (CV1_SAT_SLOT)
_cv1_sat_b_store:
  add a, a
  push af
  call _cv1_sat_xt
  inc l
  pop af
  ld (hl), a
_cv1_sat_b_resolved:
  ld hl, CV1_SAT_COUNT
  inc (hl)
_cv1_sat_b_next:
  ld hl, CV1_SAT_OAM_INDEX
  inc (hl)
  ld a, (hl)
  cp 64
  jp c, _cv1_sat_b_loop
_cv1_sat_prepared:
  ld hl, CV1_SAT_FLAGS
  set 0, (hl)
  ret

; Private index/address helpers. _xt preserves B (the pending X coordinate).
_cv1_sat_oam:
  ld a, (CV1_SAT_OAM_INDEX)
  add a, a
  add a, a
  ld l, a
  ld h, $c9
  ret
_cv1_sat_key:
  inc l
  ld a, (hl)
  ld (CV1_SAT_TILE), a
  inc l
  ld a, (hl)
  and $c3
  ld (CV1_SAT_ATTR), a
  inc l
  ret
_cv1_sat_xt:
  ld a, (CV1_SAT_COUNT)
  add a, a
  add a, $80
  ld l, a
  ld h, $c8
  ret

; Return carry=miss, or C=slot with carry clear. Invalid attr=$FF cannot
; match a real attr&C3, even when the NES tile key itself is $FF.
_cv1_sat_lookup:
  ; Keep both query bytes resident. The common nonmatching-tile iteration
  ; is 40T rather than reloading absolute scratch and advancing an unused
  ; attr pointer (57T). Only candidate tile matches read the attr table.
  ld a, (CV1_SAT_TILE)
  ld d, a
  ld a, (CV1_SAT_ATTR)
  ld e, a
  ld a, d
  ld hl, SAT_PAIR_TILE_KEYS
  ld bc, $4000
_cv1_sat_lookup_loop:
  cp (hl)
  jr nz, _cv1_sat_lookup_next
  set 7, l
  ld a, e
  cp (hl)
  res 7, l
  ld a, d                      ; LD/RES preserve the comparison's Z/carry
  ret z
_cv1_sat_lookup_next:
  inc l
  inc c
  djnz _cv1_sat_lookup_loop
  scf
  ret

; A=slot -> HL=next-pin byte, A=bit mask. Clobbers BC/DE.
_cv1_sat_bit:
  ld e, a
  and 7
  ld l, a
  ld h, 0
  ld bc, _cv1_sat_bits
  add hl, bc
  ld c, (hl)
  ld a, e
  rrca
  rrca
  rrca
  and 7
  add a, $48
  ld l, a
  ld h, $d4
  ld a, c
  ret
_cv1_sat_pin:
  call _cv1_sat_bit
  or (hl)
  ld (hl), a
  ret

; Carry=full, otherwise A=slot outside current|next. Rotate the search start.
_cv1_sat_allocate:
  ld a, (CV1_SAT_VICTIM)
  ld c, a
  ld b, 64
_cv1_sat_alloc_loop:
  push bc
  ld a, c
  call _cv1_sat_bit
  ld e, a
  ld d, (hl)
  ld a, l
  sub 8
  ld l, a
  ld a, (hl)
  or d
  and e
  pop bc
  jr z, _cv1_sat_alloc_found
  inc c
  ld a, c
  and 63
  ld c, a
  djnz _cv1_sat_alloc_loop
  scf
  ret
_cv1_sat_alloc_found:
  ld a, c
  inc a
  and 63
  ld (CV1_SAT_VICTIM), a
  ld a, c
  or a
  ret

_cv1_sat_reset_cache:
  ld hl, CV1_SAT_CURRENT
  ld bc, 16
  xor a
  call _cv1_sat_fill
  xor a
  ld (CV1_SAT_VICTIM), a
_cv1_sat_invalidate:
  ld hl, SAT_PAIR_ATTR_KEYS
  ld bc, 64
  ld a, $ff
  jp _cv1_sat_fill

; Private repeated-byte fill: A=value, HL=start, BC=count >= 2. All callers
; have fixed 8/16/64/128-byte regions and discard AF/BC/DE/HL afterwards.
; Seeding then overlapping LDIR retains deterministic hidden SAT bytes.
_cv1_sat_fill:
  ld (hl), a
  ld d, h
  ld e, l
  inc de
  dec bc
  ldir
  ret

_cv1_sat_controls:
  ld a, ($cb09)
  and $18
  ld a, $b0
  jr z, _cv1_sat_no_display
  or $40
_cv1_sat_no_display:
  ld b, a
  ld a, ($cb08)
  bit 5, a
  ld a, b
  jr z, _cv1_sat_small
  or 2
_cv1_sat_small:
  ld (CV1_SAT_REG1), a
  ld a, $ff
  ld (CV1_SAT_REG6), a
  ret

; Public destructive-rebuild fence; leaves frozen desired enable unchanged.
rt_cv1_sat_blank:
  call _cv1_sat_controls
  ld hl, CV1_SAT_FLAGS
  bit 1, (hl)
  ret nz
  call _cv1_sat_early_blank
  ld a, (CV1_SAT_REG1)
  and $bf
  out ($bf), a
  ld a, $81
  out ($bf), a
  ld hl, CV1_SAT_FLAGS
  set 1, (hl)
  ret

_cv1_sat_early_blank:
  in a, ($7e)
  sub $e0
  cp 13                       ; admit E0..EC, never ED..FF or active display
  jr nc, _cv1_sat_early_blank
  ret

; Fixed 192-byte SAT payload: 192*16 = 3072 real Z80 T-states (OUTI).
; NTSC224 counter sequence is 00..EA,E5..FF. The LATEST EC is physical242,
; leaving 19 complete lines (4332 T at stock clock). Thus even its repeated
; E5..EA phase is safe. The labeled straight-line burst plus poll is <4096 T;
; its exact assembled instruction timing is checked by sprite_commit.rs.
; Public nonblocking entry: exactly ONE admission sample. Carry set means late,
; no RAM/port/pin mutation; clear means committed. Never poll again after a hit.
rt_cv1_sat_try_commit:
  ld hl, CV1_SAT_FLAGS
  bit 0, (hl)
  jp z, _cv1_sat_bad
_cv1_sat_try_sample:
  in a, ($7e)
  sub $e0
  cp 13
  jr c, _cv1_sat_commit_admitted
  scf
  ret

; Public explicitly blocking entry, used only by the pending PPUDATA barrier
; (and standalone callers). Same admitted burst and carry-clear success ABI.
rt_cv1_sat_commit:
  ld hl, CV1_SAT_FLAGS
  bit 0, (hl)
  jp z, _cv1_sat_bad
  call _cv1_sat_early_blank
_cv1_sat_commit_admitted:
.ifdef DIAG_CV1_SAT
  in a, ($7e)
  ld (CV1_SAT_COMMIT_FIRST), a
.endif
  xor a
  out ($bf), a
  ld a, $7f
  out ($bf), a
  ld hl, CV1_SAT_Y
  ld bc, $40be
  .rept 64
    outi
  .endr
  ld a, $80
  out ($bf), a
  ld a, $7f
  out ($bf), a
  ld hl, CV1_SAT_XT
  ld bc, $80be
  .rept 128
    outi
  .endr
  ld a, (CV1_SAT_REG6)
  out ($bf), a
  ld a, $86
  out ($bf), a
  ld a, (CV1_SAT_REG1)
  out ($bf), a
  ld a, $81
  out ($bf), a
_cv1_sat_commit_finished:
.ifdef DIAG_CV1_SAT
  in a, ($7e)
  ld (CV1_SAT_COMMIT_LAST), a
  ld a, (CV1_SAT_COUNT)
  ld (CV1_SAT_COMMIT_VISIBLE), a
  ld hl, (CV1_SAT_COMMIT_COUNT)
  inc hl
  ld (CV1_SAT_COMMIT_COUNT), hl
.endif
  ; Pins change only after the entire SAT and matching registers exist.
  ld hl, CV1_SAT_NEXT
  ld de, CV1_SAT_CURRENT
  ld bc, 8
  ldir
  xor a
  ld ($cb2d), a
  ld a, 4
  ld (CV1_SAT_FLAGS), a
  ret

_cv1_sat_bad:
  ld a, $e8
  ld ($cb1d), a
_cv1_sat_bad_halt:
  halt
  jr _cv1_sat_bad_halt
_cv1_sat_bits:
  .db 1,2,4,8,16,32,64,128
.ends
.endif
