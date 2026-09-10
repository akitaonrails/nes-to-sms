; Cold-only exact-byte atlas. Source time and old runtime paths are untouched.
.ifdef CNROM_SOURCE_HARDWARE_ATLAS_EXPERIMENT
.include "runtime/chr_atlas_layout.inc"
.bank 0 slot 0
.section "chr_atlas_gates" free
rt_chr_atlas_cold_init:
  ld a, 0
  jr _at_gate
rt_chr_atlas_begin:
  ld a, 1
  jr _at_gate
rt_chr_atlas_intern_bg:
  ld a, 2
  jr _at_gate
rt_chr_atlas_intern_pair:
  ld a, 3
  jr _at_gate
rt_chr_atlas_resolve:
  ld a, 4
  jr _at_gate
rt_chr_atlas_retire:
  ld a, 5
; IX/IY stay unused: the verified tracing core intentionally rejects them.
; Results are HL/DE only; A and flags do not survive the gate.
_at_gate:
  ld c, a
  ld a, i
  di
  push af                  ; caller IFF2 in P/V
  ld a, ($fffc)
  ld b, a
  ld a, ($fffe)
  ld d, a
  ld a, ($ffff)
  ld e, a
  push bc                  ; caller $fffc / action
  push de                  ; caller $fffe / $ffff
  ld a, ($cb14)
  push af                  ; caller slot-1 shadow
  ld a, CHR_ATLAS_CODE_BANK
  ld ($fffe), a
  ld ($cb14), a
  ld a, c
  call rt_chr_atlas_dispatch
  pop bc
  ld a, b
  ld ($cb14), a
  pop bc
  ld a, b
  ld ($fffe), a
  ld a, c
  ld ($ffff), a
  pop bc
  ld a, b
  ld ($fffc), a
  pop bc                   ; caller IFF2 flags in C
  bit 2, c
  ret z
  ei
  ret
.ends

.bank CHR_ATLAS_CODE_BANK slot 1
.section "chr_atlas" free
rt_chr_atlas_dispatch:
  push af
  ld a, 12
  ld ($fffc), a
  pop af
  or a
  jp z, _at_cold
  dec a
  jp z, _at_begin
  dec a
  jp z, _at_bg
  dec a
  jp z, _at_pair
  dec a
  jp z, _at_resolve
  dec a
  jp z, _at_retire
  ld a, 3
  jp _at_fault

; Exact exclusive clear: never optional guest SRAM8000..87FF.
_at_cold:
  ld hl, AT_PAYLOAD
  ld de, AT_PAYLOAD+1
  ld bc, AT_END-AT_PAYLOAD-1
  ld (hl), 0
  ldir
  ld hl, AT_HANDLES
  ld de, AT_HANDLES+1
  ld bc, AT_ACTIVE_NT-AT_HANDLES-1
  ld (hl), $ff
  ldir
  ld a, AT_ALLOCATED | AT_PERMANENT_ZERO
  ld (AT_FLAGS), a
  ld hl, 1
  ld (AT_ALLOCATED_COUNT), hl
  ld hl, 0
  ld (AT_HEADS), hl
  ; Both tables select provedzero slot0. Generic boot may have overwritten it.
  xor a
  out ($bf), a
  ld a, $40
  out ($bf), a
  ld b, 32
  xor a
_at_zero_pattern:
  out ($be), a
  djnz _at_zero_pattern
  ld a, $00
  out ($bf), a
  ld a, $47
  out ($bf), a
  call _at_zero_nt
  xor a
  out ($bf), a
  ld a, $77
  out ($bf), a
  call _at_zero_nt
  ld a, $f3
  ld b, 2
  call vdp_set_register
  ld a, 8
  ld ($fffc), a
  ld hl, $a000
  ld de, $a001
  ld bc, $07ff
  ld (hl), 0
  ldir
  ld a, 12
  ld ($fffc), a
  ld hl, 0
  ld de, 0
  ret
_at_zero_nt:
  ld bc, $0800
_at_zero_nt_byte:
  xor a
  out ($be), a
  dec bc
  ld a, b
  or c
  jr nz, _at_zero_nt_byte
  ret

_at_begin:
  ; Renderer key-cache entries map keys to ordinals. They stay valid until
  ; some eviction reuses an ordinal for different bytes; then the whole
  ; cache resets before any stale ordinal can resolve. Per-packet resolve
  ; marks always reset so every used ordinal re-arms its PENDING protection.
  ld a, (AT_EVICTED)
  or a
  ld a, 8
  ld ($fffc), a
  jr z, _at_begin_resolved
  ld hl, $ac00
  ld de, $ac01
  ld bc, $03ff
  ld (hl), $ff
  ldir
  ld hl, 0
  ld ($c840), hl
_at_begin_resolved:
  ld hl, AT_SLOT_RESOLVED
  ld de, AT_SLOT_RESOLVED+1
  ld bc, 255
  ld (hl), 0
  ldir
  ld a, 12
  ld ($fffc), a
  xor a
  ld (AT_EVICTED), a
  ld (AT_RELEASED), a
  ld hl, AT_FLAGS
  ld bc, AT_CAPACITY
_at_clear_pending:
  ld a, (hl)
  and $fb
  ld (hl), a
  inc hl
  dec bc
  ld a, b
  or c
  jr nz, _at_clear_pending
  ld a, 2
  ld (AT_PHASE), a
  xor a
  ld (AT_FAULT), a
  ret

; Map dense ordinal to legal9-bit physical number; no RAM lookup ambiguity.
_at_physical:
  ld de, 376
  or a
  sbc hl, de
  add hl, de
  jr c, _at_physical_low
  ld de, 130
  add hl, de
  ex de, hl
  ret
_at_physical_low:
  ld de, 56
  or a
  sbc hl, de
  add hl, de
  jr c, _at_physical_same
  ld de, 64
  add hl, de
_at_physical_same:
  ex de, hl
  ret
_at_check_ordinal:
  ld a, h
  cp 1
  jr c, _at_ordinal_ok
  jr nz, _at_bad_ordinal
  ld a, l
  cp 122
  jr nc, _at_bad_ordinal
_at_ordinal_ok:
  ret
_at_bad_ordinal:
  ld a, 3
  jp _at_fault
_at_flag_address:
  ld de, AT_FLAGS
  add hl, de
  ret
_at_payload_address:
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld de, AT_PAYLOAD
  add hl, de
  ret
_at_link_address:
  add hl, hl
  ld de, AT_LINKS
  add hl, de
  ret
_at_head_address:
  ld l, a
  ld h, 0
  add hl, hl
  ld de, AT_HEADS
  add hl, de
  ret
_at_resolve:
  call _at_check_ordinal
  ld (AT_RESULT), hl
  call _at_flag_address
  ld a, (hl)
  bit 0, a
  jp z, _at_bad_ordinal
  or AT_PENDING
  ld (hl), a
  ld hl, (AT_RESULT)
  call _at_physical
  ld hl, (AT_RESULT)
  or a
  ret

; Candidate32 copying occurs only after raw renderer buffers are dead.
_at_bg:
  ld a, h
  cp >AT_BG_CANDIDATE
  jp nz, _at_bad_ordinal
  ld a, l
  cp <AT_BG_CANDIDATE
  jp nz, _at_bad_ordinal
  ld a, 8
  ld ($fffc), a
  ld de, $c880
  ld bc, 32
  ldir
  ld a, 12
  ld ($fffc), a
  ld hl, $c880
  call _at_hash32
  ld (AT_HASH), a
  call _at_head_address
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  ld de, AT_CAPACITY
  ld (AT_REMAIN), de
_at_bg_find:
  ld a, h
  and l
  cp $ff
  jp z, _at_allocate_bg
  call _at_check_ordinal
  ld (AT_CANDIDATE), hl
  call _at_flag_address
  bit 0, (hl)
  jp z, _at_bad_chain
  ; Pair nodes share the hash chains but never satisfy a single lookup.
  ld a, (hl)
  and AT_PAIR_FIRST | AT_PAIR_SECOND
  jp nz, _at_bg_next
  ld hl, (AT_CANDIDATE)
  call _at_payload_address
  ld de, $c880
  ld b, 32
_at_bg_compare:
  ld a, (de)
  cp (hl)
  jr nz, _at_bg_next
  inc de
  inc hl
  djnz _at_bg_compare
  ld hl, (AT_CANDIDATE)
  jp _at_resolve
_at_bg_next:
  ld hl, (AT_REMAIN)
  dec hl
  ld (AT_REMAIN), hl
  ld a, h
  or l
  jp z, _at_bad_chain
  ld hl, (AT_CANDIDATE)
  call _at_link_address
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  jr _at_bg_find
_at_hash32:
  ld b, 32
_at_hash_len:
  xor a
_at_hash_byte:
  xor (hl)
  inc hl
  djnz _at_hash_byte
  ret
_at_allocate_bg:
  ld hl, 1
  ld (AT_SCAN), hl
_at_allocate_scan:
  ld hl, (AT_SCAN)
  call _at_flag_address
  ld a, (hl)
  and AT_PROTECTED
  jr z, _at_allocate_found
  ld hl, (AT_SCAN)
  inc hl
  ld (AT_SCAN), hl
  ld de, AT_CAPACITY
  or a
  sbc hl, de
  jr c, _at_allocate_scan
  call _at_capacity_release
  jp _at_allocate_bg
; When the displayed and pending pattern sets genuinely cannot share the
; pool, release the displayed generation ONCE per packet and let the
; publisher blank that frame before any canonical upload. Payload bytes
; reach VRAM only at commit, so the displayed picture is intact until then.
; A second exhaustion in the same packet still fails closed.
_at_capacity_release:
  ld a, (AT_RELEASED)
  or a
  jr z, _at_release_old
  ld a, 1
  jp _at_fault
_at_release_old:
  ld a, 1
  ld (AT_RELEASED), a
  ld hl, AT_FLAGS
  ld bc, AT_CAPACITY
_at_release_entry:
  ld a, (hl)
  and $fd                  ; clear AT_OLD
  ld (hl), a
  inc hl
  dec bc
  ld a, b
  or c
  jr nz, _at_release_entry
  ret
; Expired pairs are as reclaimable as expired singles — otherwise a churning
; sprite graveyard permanently shrinks the background pool until allocation
; fails closed with free storage in hand. Breaking a pair unlinks its
; 64-byte chain node (always owned by the first half) and frees the partner.
_at_allocate_found:
  ld hl, (AT_SCAN)
  ld (AT_CANDIDATE), hl
  call _at_flag_address
  ld a, (hl)
  and AT_PAIR_FIRST
  jr nz, _at_bg_break_first
  ld a, (hl)
  and AT_PAIR_SECOND
  jr nz, _at_bg_break_second
  bit 0, (hl)
  call nz, _at_unlink_candidate
_at_bg_reclaimed:
  ld hl, (AT_CANDIDATE)
  call _at_payload_address
  ex de, hl
  ld hl, $c880
  ld bc, 32
  ldir
  ld hl, (AT_CANDIDATE)
  call _at_flag_address
  ld (hl), AT_ALLOCATED | AT_PENDING | AT_DIRTY
  ld a, (AT_HASH)
  call _at_head_address
  push hl
  ld c, (hl)
  inc hl
  ld b, (hl)
  ld hl, (AT_CANDIDATE)
  call _at_link_address
  ld (hl), c
  inc hl
  ld (hl), b
  pop hl
  ld de, (AT_CANDIDATE)
  ld (hl), e
  inc hl
  ld (hl), d
  ld hl, (AT_ALLOCATED_COUNT)
  inc hl
  ld (AT_ALLOCATED_COUNT), hl
  ld hl, (AT_CANDIDATE)
  jp _at_resolve
_at_bg_break_first:
  ; The victim owns the pair's chain entry; its second half is freed.
  call _at_unlink_candidate
  ld hl, (AT_SCAN)
  inc hl
  jr _at_bg_free_partner
_at_bg_break_second:
  ; The first half one slot below owns the chain entry; unlink through it,
  ; free it, then reuse the scanned slot itself.
  ld hl, (AT_SCAN)
  dec hl
  ld (AT_CANDIDATE), hl
  call _at_unlink_candidate
  ld hl, (AT_SCAN)
  dec hl
_at_bg_free_partner:
  call _at_flag_address
  ld (hl), 0
  ld hl, (AT_ALLOCATED_COUNT)
  dec hl
  ld (AT_ALLOCATED_COUNT), hl
  ld hl, (AT_SCAN)
  ld (AT_CANDIDATE), hl
  jp _at_bg_reclaimed

; Unlink exact ordinal from its old byte hash before overwriting backing.
; Pair-first nodes were chained under their 64-byte hash; second halves are
; never chained and must not reach this helper.
_at_unlink_candidate:
  ld a, 1
  ld (AT_EVICTED), a
  ld hl, (AT_CANDIDATE)
  call _at_flag_address
  ld a, (hl)
  and AT_PAIR_FIRST
  ld hl, (AT_CANDIDATE)
  call _at_payload_address
  ld b, 32
  jr z, _at_unlink_hash
  ld b, 64
_at_unlink_hash:
  call _at_hash_len
  call _at_head_address
  ld (AT_PREDECESSOR), hl ; address of head/link containing current ordinal
  ld hl, AT_CAPACITY
  ld (AT_COMPARE_OFFSET), hl
_at_unlink_walk:
  ld hl, (AT_PREDECESSOR)
  ld e, (hl)
  inc hl
  ld d, (hl)
  ld hl, (AT_CANDIDATE)
  or a
  sbc hl, de
  jr z, _at_unlink_found
  ex de, hl
  call _at_check_ordinal
  call _at_link_address
  ld (AT_PREDECESSOR), hl
  ld hl, (AT_COMPARE_OFFSET)
  dec hl
  ld (AT_COMPARE_OFFSET), hl
  ld a, h
  or l
  jr nz, _at_unlink_walk
_at_bad_chain:
  ld a, 2
  jp _at_fault
_at_unlink_found:
  ld hl, (AT_CANDIDATE)
  call _at_link_address
  ld c, (hl)
  ld (hl), $ff
  inc hl
  ld b, (hl)
  ld (hl), $ff
  ld hl, (AT_PREDECESSOR)
  ld (hl), c
  inc hl
  ld (hl), b
  ld hl, (AT_ALLOCATED_COUNT)
  dec hl
  ld (AT_ALLOCATED_COUNT), hl
  ret

; Aligned 8x16 sprite-pair interning: one 64-byte candidate at
; AT_PAIR_CANDIDATE holds the top then bottom pattern. The first slot is an
; even sprite-addressable ordinal (192..376) so its physical pattern lands
; even at $2000+ with the second physically contiguous; the SMS 8x16 tile
; bit0 mask then selects the pair exactly. The top half stages in atlas SRAM
; (AT_PAIR_STAGE) and the bottom in the shared $C880 bounce row, so both are
; readable while atlas SRAM is mapped.
_at_pair:
  ld a, h
  cp >AT_PAIR_CANDIDATE
  jp nz, _at_bad_ordinal
  ld a, l
  cp <AT_PAIR_CANDIDATE
  jp nz, _at_bad_ordinal
  ld a, 8
  ld ($fffc), a
  ld de, $c880
  ld bc, 32
  ldir
  push hl
  ld a, 12
  ld ($fffc), a
  ld hl, $c880
  ld de, AT_PAIR_STAGE
  ld bc, 32
  ldir
  pop hl
  ld a, 8
  ld ($fffc), a
  ld de, $c880
  ld bc, 32
  ldir
  ld a, 12
  ld ($fffc), a
  ld hl, AT_PAIR_STAGE
  call _at_hash32
  ld c, a
  ld hl, $c880
  call _at_hash32
  xor c
  ld (AT_HASH), a
  call _at_head_address
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  ld de, AT_CAPACITY
  ld (AT_REMAIN), de
_at_pair_find:
  ld a, h
  and l
  cp $ff
  jp z, _at_allocate_pair
  call _at_check_ordinal
  ld (AT_CANDIDATE), hl
  call _at_flag_address
  bit 0, (hl)
  jp z, _at_bad_chain
  ; Singles share the hash chains but never satisfy a pair lookup.
  ld a, (hl)
  and AT_PAIR_FIRST
  jr z, _at_pair_next
  ld hl, (AT_CANDIDATE)
  call _at_payload_address
  ld de, AT_PAIR_STAGE
  ld b, 32
_at_pair_compare_top:
  ld a, (de)
  cp (hl)
  jr nz, _at_pair_next
  inc de
  inc hl
  djnz _at_pair_compare_top
  ld de, $c880
  ld b, 32
_at_pair_compare_bottom:
  ld a, (de)
  cp (hl)
  jr nz, _at_pair_next
  inc de
  inc hl
  djnz _at_pair_compare_bottom
  jp _at_pair_admit
_at_pair_next:
  ld hl, (AT_REMAIN)
  dec hl
  ld (AT_REMAIN), hl
  ld a, h
  or l
  jp z, _at_bad_chain
  ld hl, (AT_CANDIDATE)
  call _at_link_address
  ld e, (hl)
  inc hl
  ld d, (hl)
  ex de, hl
  jr _at_pair_find

; Only even sprite-addressable ordinals may open a pair, and both slots must
; be simultaneously unprotected. Expired singles or pairs in the window are
; reclaimed with exact unlinking; displayed and pending generations stay
; untouched, and exhaustion fails closed like the single allocator.
_at_allocate_pair:
  ld hl, 192
_at_pair_scan:
  ld (AT_SCAN), hl
  call _at_flag_address
  ld a, (hl)
  and AT_PROTECTED
  jr nz, _at_pair_scan_next
  inc hl
  ld a, (hl)
  and AT_PROTECTED
  jr z, _at_pair_found
_at_pair_scan_next:
  ld hl, (AT_SCAN)
  inc hl
  inc hl
  ld de, 378
  or a
  sbc hl, de
  add hl, de
  jr c, _at_pair_scan
  call _at_capacity_release
  jp _at_allocate_pair
_at_pair_found:
  ld hl, (AT_SCAN)
  ld (AT_CANDIDATE), hl
  call _at_flag_address
  bit 0, (hl)
  call nz, _at_pair_reclaim_slot
  ld hl, (AT_SCAN)
  inc hl
  ld (AT_CANDIDATE), hl
  call _at_flag_address
  bit 0, (hl)
  call nz, _at_pair_reclaim_slot
  ; Both payload halves are contiguous rows of the first ordinal.
  ld hl, (AT_SCAN)
  call _at_payload_address
  ex de, hl
  ld hl, AT_PAIR_STAGE
  ld bc, 32
  ldir
  ld hl, $c880
  ld bc, 32
  ldir
  ld hl, (AT_SCAN)
  call _at_flag_address
  ld (hl), AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_FIRST
  inc hl
  ld (hl), AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_SECOND
  ld a, (AT_HASH)
  call _at_head_address
  push hl
  ld c, (hl)
  inc hl
  ld b, (hl)
  ld hl, (AT_SCAN)
  call _at_link_address
  ld (hl), c
  inc hl
  ld (hl), b
  pop hl
  ld de, (AT_SCAN)
  ld (hl), e
  inc hl
  ld (hl), d
  ld hl, (AT_ALLOCATED_COUNT)
  inc hl
  inc hl
  ld (AT_ALLOCATED_COUNT), hl
  ld hl, (AT_SCAN)
  ld (AT_CANDIDATE), hl
_at_pair_admit:
  ; Protect the second half for this packet, then resolve the first.
  ld hl, (AT_CANDIDATE)
  inc hl
  call _at_flag_address
  ld a, (hl)
  or AT_PENDING
  ld (hl), a
  ld hl, (AT_CANDIDATE)
  jp _at_resolve
; (AT_CANDIDATE) names an expired allocated slot being reclaimed. Pair
; seconds carry no chain entry but still leave the allocated census.
_at_pair_reclaim_slot:
  ld a, (hl)
  and AT_PAIR_SECOND
  jp z, _at_unlink_candidate
  ld a, 1
  ld (AT_EVICTED), a
  ld hl, (AT_ALLOCATED_COUNT)
  dec hl
  ld (AT_ALLOCATED_COUNT), hl
  ret
; This pure metadata primitive is called by publication only after shadowcopy.
_at_retire:
  ld hl, AT_FLAGS
  ld bc, AT_CAPACITY
_at_retire_entry:
  ld a, (hl)
  and $f9
  bit 2, (hl)
  jr z, _at_retire_store
  or AT_OLD
_at_retire_store:
  ld (hl), a
  inc hl
  dec bc
  ld a, b
  or c
  jr nz, _at_retire_entry
  xor a
  ld (AT_PHASE), a
  ret
_at_fault:
  ld (AT_FAULT), a
  jp rt_cnrom_packet_unsupported
.ends
.bank 0 slot 0
.endif
