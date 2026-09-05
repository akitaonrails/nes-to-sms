; Coherent CV1 background ownership. Raw CIRAM/CHR remain the producer truth;
; only this module may publish its folded NT, palette and background slots.
; The CV1 profile opts into this staged driver. Default/NROM code emits no
; bytes. SRAM access is fixed-bank, DI, with a closed VDP latch.
.ifdef CV1_COHERENT_BG
.ifndef CV1_RUNTIME_HOOKS
.fail "CV1_COHERENT_BG requires CV1_RUNTIME_HOOKS"
.endif
.ifndef NES_MIRRORING_VERTICAL
.fail "CV1 coherent background requires its vertical CIRAM profile"
.endif

.define CV1_BG_DIRTY       $a800
.define CV1_BG_FROZEN      $a900
.define CV1_BG_SHADOW0     $aa00
.define CV1_BG_CELL_INDEX  $ad80
.define CV1_BG_COUNTS0     $b100
.define CV1_BG_PINS        $b300
.define CV1_BG_CHR_DIRTY   $b320
.define CV1_BG_CHR_FROZEN  $b360
.define CV1_BG_PALETTE     $b3a0
.define CV1_BG_CRAM        $b3c0
.define CV1_BG_SELECTOR    $b3e0 ; committed metadata: 0 or1
.define CV1_BG_PHASE       $b3e1 ; preparation state; zero means complete
.define CV1_BG_RECORDS     $b3e2 ; bounded NT count0..32
.define CV1_BG_COPIED      $b3e3 ; next metadata exists
.define CV1_BG_FULL        $b3e4 ; explicit display-off rebuild
.define CV1_BG_TABLE       $b3e5 ; prepared source table, not CA13
.define CV1_BG_WINDOW      $b3e6 ; prepared coarse window, not CB2A
.define CV1_BG_PAL_DIRTY   $b3e7 ; producer palette intent
.define CV1_BG_PAL_PENDING $b3e8 ; frozen palette intent
.define CV1_BG_CURSOR      $b3e9 ; word: phase cursor
.define CV1_BG_CELL        $b3eb ; word: current folded cell0..895
.define CV1_BG_BASE        $b3ed
.define CV1_BG_SUBPAL      $b3ee
.define CV1_BG_SLOT        $b3ef
.define CV1_BG_ALLOC       $b3f0 ; next candidate0..254; FF is never a slot
.define CV1_BG_PROBES      $b3f1 ; candidates tried for this miss
.define CV1_BG_INIT        $b3f2
.define CV1_BG_SCAN        $b3f3 ; word: dirty source scan position
.define CV1_BG_ATTR        $b3f5 ; word: expanding attribute byte
.define CV1_BG_ATTR_CELL   $b3f7 ; 0..15 expansion cursor
.define CV1_BG_CHR_SCAN    $b3f8 ; any changed tile in the selected table
.define CV1_BG_SCROLL_COL $b3f9
.define CV1_BG_SCROLL_LEFT $b3fa
.define CV1_BG_SCROLL_ROW $b3fb
.define CV1_BG_COPY_CURSOR $b3fc ; word, metadata bytes copied
.define CV1_BG_RESET_CURSOR $b3fe ; word, reset bytes completed
.define CV1_BG_PACKET      $b400 ; 32*(VRAM low, write-high, slot)
.define CV1_BG_HUD         $b460 ; six frozen HUD bytes; separate owner
.define CV1_BG_LIVE_FLAGS  $b466 ; tile/attribute1, attribute2, CHRtable0/1=4/8
.define CV1_BG_FROZEN_FLAGS $b467
.define CV1_BG_REV_BASE    $b500
.define CV1_BG_REV_KEY     $b600 ; S0..3; FF=slot has no cache owner
.define CV1_BG_COUNTS1     $b700
.define CV1_BG_SHADOW1     $b900
.define CV1_BG_ATTRIBUTE_LINES 14
.define CV1_BG_DIRTY_LINES 48

.section "bg_cv1" free

; B=whole-step stock scanlines, selected without changing preparation state.
; Real assembled entry-to-RET maxima plus512T for coordinator/selection glue;
; HUD admission separately reserves two lines for IRQ latency. In particular
; CHR fanout, dirty scan and allocator exceed the old shared8192T estimate.
; Phase0 also owns the194T SAT-begin transition before the next service point.
rt_cv1_bg_chunk_lines:
  call rt_raw_ciram_sram_enable
  ld a, (CV1_BG_PHASE)
  cp 16
  jr nc, _cv1_bg_bad_phase
  cp 5
  jr z, _cv1_bg_attr_lines
  cp 6
  jr z, _cv1_bg_scan_lines
_cv1_bg_table_lines:
  ld e, a
  ld d, 0
  ld hl, _cv1_bg_step_lines
  add hl, de
  ld b, (hl)
  jp rt_raw_ciram_sram_disable
_cv1_bg_attr_lines:
  ld hl, (CV1_BG_CURSOR)
  call _cv1_bg_clean_bitmap
  ld b, 6
  jp z, rt_raw_ciram_sram_disable
  ld b, CV1_BG_ATTRIBUTE_LINES
  jp rt_raw_ciram_sram_disable
_cv1_bg_scan_lines:
  ld hl, (CV1_BG_SCAN)
  call _cv1_bg_clean_bitmap
  ld b, 8
  jp z, rt_raw_ciram_sram_disable
  ld b, CV1_BG_DIRTY_LINES
  jp rt_raw_ciram_sram_disable
_cv1_bg_bad_phase:
  ld a, $f8                  ; malformed preparation context, not pool overflow
  ld ($cb1d), a
  di
  jr _cv1_bg_bad_phase_halt
_cv1_bg_bad_phase_halt:
  halt
  jr _cv1_bg_bad_phase_halt
_cv1_bg_step_lines:
  .db 4, 18, 9, 40, 15, CV1_BG_ATTRIBUTE_LINES, CV1_BG_DIRTY_LINES
  .db 7, 44, 20, 12, 8, 4, 19, 9, 6

; Z only for an aligned, wholly clean frozen bitmap byte. Constant-time
; read-only classification; leaves HL at that bitmap byte on the fast path.
; A nonzero/partial byte retains the existing per-cell/fanout semantics.
_cv1_bg_clean_bitmap:
  ld a, h
  and 8
  ret nz
  ld a, l
  and 7
  ret nz
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  ld h, >CV1_BG_FROZEN
  ld a, (hl)
  or a
  ret

; Boot DI only. SRAM A800..BFFF belongs exclusively to this module/HUD packet.
; Internal OAM C900..C9FF and all translated continuation memory are untouched.
rt_cv1_bg_init:
  ; Match the exact32 bytes boot already installed, including entries that a
  ; later partial NES palette update does not touch. The retired folded
  ; attribute shadow CC00 is boot-only scratch; no live PPU state is borrowed.
  ld a, ($ffff)
  push af
  ld a, :data_palette
  ld ($ffff), a
  ld hl, data_palette
  ld de, $cc00
  ld bc, 32
  ldir
  pop af
  ld ($ffff), a
  call rt_raw_ciram_sram_enable
  ld hl, CV1_BG_DIRTY
  ld bc, $1800
  xor a
  call mem_fill
  ld hl, CV1_BG_SHADOW0
  ld bc, $0700               ; shadow0 plus per-cell packet index
  ld a, $ff
  call mem_fill
  ld hl, CV1_BG_SHADOW1
  ld bc, $0380
  ld a, $ff
  call mem_fill
  ld hl, CV1_BG_REV_KEY
  ld bc, $0100
  ld a, $ff
  call mem_fill
  ld hl, $d600
  ld bc, $0400
  ld a, $ff
  call mem_fill
  ld hl, $cc00
  ld de, CV1_BG_PALETTE
  ld bc, 32
  ldir
  jp rt_raw_ciram_sram_disable

; Raw producers: guarded DI entry, DE=NES address, A=byte. No video ports.
; Exact byte equality avoids turning repeated HUD/timer stores into full scans.
; DE survives; AF/BC/HL are caller-dead. The outer guard owns mapping restore.
rt_cv1_bg_raw_write:
  ld c, a
  ld a, d
  and 7
  or $80
  ld h, a
  ld l, e
  call rt_raw_ciram_sram_enable
  ld a, (hl)
  cp c
  jr z, _cv1_bg_raw_done
  ld (hl), c
  push de
  ld a, h
  and 7
  ld h, a
  ld de, CV1_BG_DIRTY
  call _cv1_bg_bit_set
  pop de
  ld a, d
  and 3
  cp 3
  ld a, 1
  jr nz, _cv1_bg_raw_flag
  ld a, e
  cp $c0
  ld a, 1
  jr c, _cv1_bg_raw_flag
  ld a, 3
_cv1_bg_raw_flag:
  ld hl, CV1_BG_LIVE_FLAGS
  or (hl)
  ld (hl), a
_cv1_bg_raw_done:
  jp rt_raw_ciram_sram_disable

rt_cv1_bg_chr_write:
  ld c, a
  ld hl, CHR_RAM_SRAM_BASE
  add hl, de
  call rt_raw_ciram_sram_enable
  ld (hl), c
  ; Every partial byte invalidates the source tile and sticky sprite cache.
  ld a, 1
  ld ($c801), a
  push de
  ld h, d
  ld l, e
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  ld de, CV1_BG_CHR_DIRTY
  call _cv1_bg_bit_set
  pop de
  ld a, d
  and $10
  ld a, 4
  jr z, _cv1_bg_chr_flag
  ld a, 8
_cv1_bg_chr_flag:
  ld hl, CV1_BG_LIVE_FLAGS
  or (hl)
  ld (hl), a
  jp rt_raw_ciram_sram_disable

; B=NES palette index0..31, C=already converted SMS color. Preserve canonical
; universal fanout and transparent mirrors; no CRAM changes until commit.
rt_cv1_bg_palette_write:
  call rt_raw_ciram_sram_enable
  ld a, b
  and $0f
  jr z, _cv1_bg_palette_universal
  and 3
  jr z, _cv1_bg_palette_done
  ld a, b
  add a, <CV1_BG_PALETTE
  ld l, a
  ld h, >CV1_BG_PALETTE
  call _cv1_bg_palette_byte
  jr _cv1_bg_palette_done
_cv1_bg_palette_universal:
  ld hl, CV1_BG_PALETTE
  ld b, 4
_cv1_bg_palette_fanout:
  call _cv1_bg_palette_byte
  inc l
  inc l
  inc l
  inc l
  djnz _cv1_bg_palette_fanout
_cv1_bg_palette_done:
  jp rt_raw_ciram_sram_disable
_cv1_bg_palette_byte:
  ld a, (hl)
  cp c
  ret z
  ld (hl), c
  ld a, 1
  ld (CV1_BG_PAL_DIRTY), a
  ret

; Bitmap primitives, SRAM mapped. HL=index, DE=bitmap. AF/BC/DE/HL clobbered.
_cv1_bg_bit_addr:
  ld a, l
  and 7
  ld b, a
  srl h
  rr l
  srl h
  rr l
  srl h
  rr l
  add hl, de
  ld a, 1
  inc b
_cv1_bg_bit_shift:
  dec b
  ret z
  add a, a
  jr _cv1_bg_bit_shift
_cv1_bg_bit_set:
  call _cv1_bg_bit_addr
  or (hl)
  ld (hl), a
  ret

; HL=folded cell, return pointer into CURRENT or NEXT shadow respectively.
; Next shadow is selected only after its bounded copy has completed.
_cv1_bg_current_shadow:
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_SHADOW0
  jr z, _cv1_bg_shadow_add
  ld de, CV1_BG_SHADOW1
_cv1_bg_shadow_add:
  add hl, de
  ret
_cv1_bg_next_shadow:
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_SHADOW1
  jr z, _cv1_bg_shadow_add
  ld de, CV1_BG_SHADOW0
  jr _cv1_bg_shadow_add

; A=slot, pointer to exact16-bit count. Slot255 remains zero/reserved.
_cv1_bg_current_count:
  ld l, a
  ld h, 0
  add hl, hl
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_COUNTS0
  jr z, _cv1_bg_count_add
  ld de, CV1_BG_COUNTS1
_cv1_bg_count_add:
  add hl, de
  ret
_cv1_bg_next_count:
  ld l, a
  ld h, 0
  add hl, hl
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_COUNTS1
  jr z, _cv1_bg_count_add
  ld de, CV1_BG_COUNTS0
  jr _cv1_bg_count_add

; Start from the immutable C821 control record. The driver owns full-prologue
; busy0/CA11=1 and cannot publish READY until prepare_step returns Z. No source
; mutation can occur at its safe interrupt service points. Each step enters
; with SRAMoff/currentPRG/guard0, DI and closed latch and restores that mapping.
rt_cv1_bg_prepare_begin:
  call rt_raw_ciram_sram_enable
  xor a
  ld (CV1_BG_RECORDS), a
  ld (CV1_BG_COPIED), a
  ld (CV1_BG_FULL), a
  ld (CV1_BG_CHR_SCAN), a
  ld (CV1_BG_CURSOR), a
  ld (CV1_BG_CURSOR + 1), a
  ld a, (CV1_FRAME_PACKET)
  and $10
  ld (CV1_BG_TABLE), a
  ld a, (CV1_FRAME_PACKET + 4)
  bit 2, a
  ld a, (CV1_FRAME_PACKET + 2)
  jr z, _cv1_bg_begin_scroll
  ld a, (CV1_FRAME_PACKET + 7)
_cv1_bg_begin_scroll:
  rrca
  rrca
  rrca
  and $1f
  ld b, a
  ld a, (CV1_FRAME_PACKET)
  and 1
  rrca
  rrca
  rrca
  or b
  ld (CV1_BG_WINDOW), a
  ; Most sprite-only generations change no background source/window/table.
  ; Do not copy/scan empty metadata or visit128 clean attribute steps. Dirty
  ; bits for an inactive CHR table may remain sticky until a normal freeze;
  ; changing table itself always takes the full generation path below.
  ld hl, $cb2a
  cp (hl)
  jr nz, _cv1_bg_begin_work
  ld a, (CV1_BG_INIT)
  or a
  jr z, _cv1_bg_begin_work
  ld a, (CV1_BG_TABLE)
  ld hl, $ca13
  cp (hl)
  jr nz, _cv1_bg_begin_work
  ld b, 5                  ; CIRAM or selected table0
  or a
  jr z, _cv1_bg_begin_mask
  ld b, 9                  ; CIRAM or selected table1
_cv1_bg_begin_mask:
  ld a, (CV1_BG_LIVE_FLAGS)
  and b
  jr nz, _cv1_bg_begin_work
  ld a, (CV1_BG_HUD)
  ld hl, $c813
  xor (hl)
  and 2
  jr nz, _cv1_bg_begin_work
  call _cv1_bg_freeze_palette
  xor a
  ld (CV1_BG_PHASE), a
  jp rt_raw_ciram_sram_disable
_cv1_bg_begin_work:
  ld a, 1
  ld (CV1_BG_PHASE), a
  jp rt_raw_ciram_sram_disable

rt_cv1_bg_prepare_step:
  call rt_raw_ciram_sram_enable
  ld a, (CV1_BG_PHASE)
  or a
  jp z, _cv1_bg_step_out
  cp 1
  jp z, _cv1_bg_freeze
  cp 2
  jp z, _cv1_bg_invalidate
  cp 3
  jp z, _cv1_bg_changed_cells
  cp 4
  jp z, _cv1_bg_scroll
  cp 5
  jp z, _cv1_bg_attributes
  cp 6
  jp z, _cv1_bg_dirty_scan
  cp 7
  jp z, _cv1_bg_resolve
  cp 8
  jp z, _cv1_bg_allocate
  cp 9
  jp z, _cv1_bg_build
  cp 10
  jp z, _cv1_bg_copy
  cp 11
  jp z, _cv1_bg_queue
  cp 12
  jp z, _cv1_bg_step_out
  cp 13
  jp z, _cv1_bg_reset
  cp 14
  jp z, _cv1_bg_full_scan
  jp _cv1_bg_finish
_cv1_bg_step_out:
  ld a, (CV1_BG_PHASE)
  ld b, a
  call rt_raw_ciram_sram_disable
  ld a, b
  or a
  ret z
  cp 12
  jr nz, _cv1_bg_step_more
  ld a, 2                  ; request an explicit coordinator-owned fence
  or a
  ret
_cv1_bg_step_more:
  ld a, 1
  or a
  ret
_cv1_bg_phase:
  ld (CV1_BG_PHASE), a
  jp _cv1_bg_step_out

; One64-byte copy+clear per step. Source is immutable until publication, but
; keeping separate live/frozen generations also preserves subsequent intent.
_cv1_bg_freeze:
  ld hl, (CV1_BG_CURSOR)
  ld a, h
  or a
  jr nz, _cv1_bg_freeze_chr
  ld de, CV1_BG_DIRTY
  add hl, de
  ld d, h
  ld e, l
  inc d
  jr _cv1_bg_freeze_chunk
_cv1_bg_freeze_chr:
  ld a, l
  or a
  jr nz, _cv1_bg_freeze_last
  ld hl, CV1_BG_CHR_DIRTY
  ld de, CV1_BG_CHR_FROZEN
_cv1_bg_freeze_chunk:
  ld b, 64
_cv1_bg_freeze_byte:
  ld a, (hl)
  ld (de), a
  ld (hl), 0
  inc hl
  inc de
  djnz _cv1_bg_freeze_byte
  ld hl, (CV1_BG_CURSOR)
  ld de, 64
  add hl, de
  ld (CV1_BG_CURSOR), hl
  jp _cv1_bg_step_out
_cv1_bg_freeze_last:
  ld a, (CV1_BG_LIVE_FLAGS)
  ld (CV1_BG_FROZEN_FLAGS), a
  xor a
  ld (CV1_BG_LIVE_FLAGS), a
  call _cv1_bg_freeze_palette
  xor a
  ld hl, CV1_BG_PINS
  ld bc, 32
  call mem_fill
  ld hl, 0
  ld (CV1_BG_CURSOR), hl
  ld a, (CV1_BG_INIT)
  or a
  jp z, _cv1_bg_request_full
  ld a, (CV1_BG_TABLE)
  ld hl, $ca13
  cp (hl)
  jp nz, _cv1_bg_request_full
  ; A change in fixed-band ownership is a scene boundary, not an incremental
  ; scroll: old six rows and the new page must be rebuilt consistently.
  ld a, (CV1_BG_HUD)
  and 2
  ld b, a
  ld a, ($c813)              ; committed HUD SPLIT bit1
  and 2
  cp b
  jp nz, _cv1_bg_request_full
  ld a, (CV1_BG_TABLE)
  or a
  ld b, 4
  jr z, _cv1_bg_frozen_chr_mask
  ld b, 8
_cv1_bg_frozen_chr_mask:
  ld a, (CV1_BG_FROZEN_FLAGS)
  and b
  jp z, _cv1_bg_scroll_begin
  ld a, 2
  jp _cv1_bg_phase
_cv1_bg_freeze_palette:
  ld hl, CV1_BG_PALETTE
  ld de, CV1_BG_CRAM
  ld bc, 32
  ldir
  ld a, (CV1_BG_PAL_DIRTY)
  ld (CV1_BG_PAL_PENDING), a
  xor a
  ld (CV1_BG_PAL_DIRTY), a
  ret

; Eight source tiles per step. Invalidating FC never touches an old pattern;
; current counts retain old pixels until replacement NT words commit.
_cv1_bg_invalidate:
  ld a, (CV1_BG_TABLE)
  or a
  ld de, CV1_BG_CHR_FROZEN
  jr z, _cv1_bg_invalidate_table
  ld de, CV1_BG_CHR_FROZEN + 32
_cv1_bg_invalidate_table:
  ld hl, (CV1_BG_CURSOR)
  add hl, de
  ld c, (hl)
  ld a, c
  or a
  jr z, _cv1_bg_invalidate_next
  ld a, 1
  ld (CV1_BG_CHR_SCAN), a
  ld hl, CV1_BG_FROZEN_FLAGS
  set 0, (hl)
  ld hl, (CV1_BG_CURSOR)
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl                 ; bitmap byte owns eight*four FC entries
  ld de, $d600
  add hl, de
  ld b, 8
_cv1_bg_invalidate_bit:
  srl c
  jr nc, _cv1_bg_invalidate_skip
  ld (hl), $ff
  inc hl
  ld (hl), $ff
  inc hl
  ld (hl), $ff
  inc hl
  ld (hl), $ff
  inc hl
  jr _cv1_bg_invalidate_bit_next
_cv1_bg_invalidate_skip:
  ld de, 4
  add hl, de
_cv1_bg_invalidate_bit_next:
  djnz _cv1_bg_invalidate_bit
_cv1_bg_invalidate_next:
  ld hl, (CV1_BG_CURSOR)
  inc l
  ld (CV1_BG_CURSOR), hl
  ld a, l
  cp 32
  jp c, _cv1_bg_step_out
  ld hl, 0
  ld (CV1_BG_CURSOR), hl
  ld a, (CV1_BG_CHR_SCAN)
  or a
  ld a, 3
  jp nz, _cv1_bg_phase
  jp _cv1_bg_scroll_begin

; Eight committed cells per step on a CHR mutation, never every normal frame.
; Use the immutable reverse base for that displayed slot to select dirty source.
_cv1_bg_changed_cells:
  ld a, 8
  ld ($cb79), a
_cv1_bg_changed_cell:
  ld hl, (CV1_BG_CURSOR)
  call _cv1_bg_current_shadow
  ld a, (hl)
  cp $ff
  jr z, _cv1_bg_changed_next
  ld l, a
  ld h, >CV1_BG_REV_BASE
  ld l, (hl)
  ld h, 0
  ld a, (CV1_BG_TABLE)
  or a
  ld de, CV1_BG_CHR_FROZEN
  jr z, _cv1_bg_changed_table
  ld de, CV1_BG_CHR_FROZEN + 32
_cv1_bg_changed_table:
  call _cv1_bg_bit_addr
  and (hl)
  jr z, _cv1_bg_changed_next
  ld hl, (CV1_BG_CURSOR)
  call _cv1_bg_fold_to_raw
  ld de, CV1_BG_FROZEN
  call _cv1_bg_bit_set
_cv1_bg_changed_next:
  ld hl, (CV1_BG_CURSOR)
  inc hl
  ld (CV1_BG_CURSOR), hl
  ld de, $0380
  or a
  sbc hl, de
  jp z, _cv1_bg_scroll_begin
  ld hl, $cb79
  dec (hl)
  jr nz, _cv1_bg_changed_cell
  jp _cv1_bg_step_out

_cv1_bg_scroll_begin:
  ld a, (CV1_BG_WINDOW)
  ld hl, $cb2a
  sub (hl)
  and $3f
  cp 5
  jr c, _cv1_bg_scroll_right
  cp 60
  jp c, _cv1_bg_request_full
  neg
  and $3f
  ld (CV1_BG_SCROLL_LEFT), a
  ld a, (CV1_BG_WINDOW)
  jr _cv1_bg_scroll_column
_cv1_bg_scroll_right:
  ld (CV1_BG_SCROLL_LEFT), a
  ld a, ($cb2a)
  add a, 32
  and $3f
_cv1_bg_scroll_column:
  ld (CV1_BG_SCROLL_COL), a
  call _cv1_bg_scroll_first_row
  ld (CV1_BG_SCROLL_ROW), a
  ld a, 4
  jp _cv1_bg_phase
_cv1_bg_scroll:
  ld a, (CV1_BG_SCROLL_LEFT)
  or a
  jr z, _cv1_bg_attributes_begin
  ld hl, CV1_BG_FROZEN_FLAGS
  set 0, (hl)
  ; Four tile dirty marks per step, bounded even at a four-column advance.
  ld a, 4
  ld ($cb79), a
_cv1_bg_scroll_cell:
  ld a, (CV1_BG_SCROLL_ROW)
  ld l, a
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl
  ld a, (CV1_BG_SCROLL_COL)
  and $1f
  or l
  ld l, a
  ld a, (CV1_BG_SCROLL_COL)
  and $20
  rrca
  rrca
  rrca
  or h
  ld h, a
  ld de, CV1_BG_FROZEN
  call _cv1_bg_bit_set
  ld hl, CV1_BG_SCROLL_ROW
  inc (hl)
  ld a, (hl)
  cp 28
  jr c, _cv1_bg_scroll_more
  call _cv1_bg_scroll_first_row
  ld (CV1_BG_SCROLL_ROW), a
  ld hl, CV1_BG_SCROLL_COL
  inc (hl)
  ld a, (hl)
  and $3f
  ld (hl), a
  ld hl, CV1_BG_SCROLL_LEFT
  dec (hl)
  jr z, _cv1_bg_attributes_begin
_cv1_bg_scroll_more:
  ld hl, $cb79
  dec (hl)
  jr nz, _cv1_bg_scroll_cell
  jp _cv1_bg_step_out
_cv1_bg_scroll_first_row:
  ld a, (CV1_BG_HUD)
  bit 1, a
  ld a, 0
  ret z
  ld a, 6
  ret

_cv1_bg_attributes_begin:
  ld a, (CV1_BG_FROZEN_FLAGS)
  bit 1, a
  jp z, _cv1_bg_dirty_begin
  ld hl, $03c0
  ld (CV1_BG_CURSOR), hl
  xor a
  ld (CV1_BG_ATTR_CELL), a
  ld a, 5
  jp _cv1_bg_phase
_cv1_bg_attributes:
  ld hl, (CV1_BG_CURSOR)
  call _cv1_bg_clean_bitmap
  jr nz, _cv1_bg_attribute_dirty
  ; Skip eight unchanged attribute bytes together, never synthesize their
  ; four-row fanout. The existing next transition owns the page boundary.
  ld hl, (CV1_BG_CURSOR)
  ld a, l
  or 7
  ld l, a
  ld (CV1_BG_CURSOR), hl
  jp _cv1_bg_attributes_next
_cv1_bg_attribute_dirty:
  ld hl, (CV1_BG_CURSOR)
  ld de, CV1_BG_FROZEN
  call _cv1_bg_bit_addr
  and (hl)
  jr z, _cv1_bg_attributes_next
  ; One four-cell row of the attribute fanout per step; dirty bitmap coalesces
  ; overlaps before any count delta or slot allocation can occur.
  ld hl, (CV1_BG_CURSOR)
  ld a, l
  and $38
  ld l, a
  ld a, h
  and 4
  ld h, 0
  ld d, a
  add hl, hl
  add hl, hl
  add hl, hl
  add hl, hl                 ; attribute row*128
  ld a, h
  or d
  ld h, a
  ld a, (CV1_BG_CURSOR)
  and 7
  add a, a
  add a, a
  or l
  ld l, a
  ld a, (CV1_BG_ATTR_CELL)
  ld e, a
  ld d, 0
  add hl, de                ; row offset0/32/64/96
  ld a, h
  and 3
  cp 3
  jr nz, _cv1_bg_attribute_row
  ld a, l
  cp $80                    ; rows28..31 have no224-line SMS fold
  jr nc, _cv1_bg_attributes_next
_cv1_bg_attribute_row:
  ld (CV1_BG_ATTR), hl
  ld a, 4
  ld ($cb79), a
_cv1_bg_attribute_cell:
  ld hl, (CV1_BG_ATTR)
  ld de, CV1_BG_FROZEN
  call _cv1_bg_bit_set
  ld hl, (CV1_BG_ATTR)
  inc hl
  ld (CV1_BG_ATTR), hl
  ld hl, $cb79
  dec (hl)
  jr nz, _cv1_bg_attribute_cell
  ld a, (CV1_BG_ATTR_CELL)
  add a, 32
  ld (CV1_BG_ATTR_CELL), a
  cp 128
  jp c, _cv1_bg_step_out
_cv1_bg_attributes_next:
  xor a
  ld (CV1_BG_ATTR_CELL), a
  ld hl, (CV1_BG_CURSOR)
  inc hl
  ld a, l
  or a
  jr nz, _cv1_bg_attributes_store
  ld a, h
  cp 8
  jr z, _cv1_bg_dirty_begin
  ld hl, $07c0
_cv1_bg_attributes_store:
  ld (CV1_BG_CURSOR), hl
  jp _cv1_bg_step_out
_cv1_bg_dirty_begin:
  ld a, (CV1_BG_FROZEN_FLAGS)
  bit 0, a
  jp z, _cv1_bg_finish_begin
  ld hl, 0
  ld (CV1_BG_SCAN), hl
  ld a, 6
  jp _cv1_bg_phase

; Skip clean bitmap bytes eight cells at a time; at most16 probes per call.
_cv1_bg_dirty_scan:
  ld hl, (CV1_BG_SCAN)
  call _cv1_bg_clean_bitmap
  jr nz, _cv1_bg_dirty_slow
  ; Up to sixteen clean bitmap bytes (128 raw cells) with one address
  ; calculation. Stop BEFORE a nonzero byte: its next step retains the
  ; complete old cell/window/remap logic and its independently larger bound.
  ld b, 16
_cv1_bg_clean_span:
  inc l
  jp z, _cv1_bg_finish_begin
  djnz _cv1_bg_clean_more
  jr _cv1_bg_clean_store
_cv1_bg_clean_more:
  ld a, (hl)
  or a
  jr z, _cv1_bg_clean_span
_cv1_bg_clean_store:
  ld h, 0
  add hl, hl
  add hl, hl
  add hl, hl
  ld (CV1_BG_SCAN), hl
  jp _cv1_bg_step_out
_cv1_bg_dirty_slow:
  ld a, 16
  ld ($cb79), a
_cv1_bg_dirty_probe:
  ld hl, (CV1_BG_SCAN)
  ld a, h
  cp 8
  jp z, _cv1_bg_finish_begin
  ld de, CV1_BG_FROZEN
  call _cv1_bg_bit_addr
  ld c, a
  ld a, (hl)
  or a
  jr nz, _cv1_bg_dirty_byte
  ld hl, (CV1_BG_SCAN)
  ld a, l
  or 7
  ld l, a
  jr _cv1_bg_dirty_next
_cv1_bg_dirty_byte:
  and c
  jr z, _cv1_bg_dirty_advance
  ld hl, (CV1_BG_SCAN)
  inc hl
  ld (CV1_BG_SCAN), hl
  dec hl
  ; A raw tile is visible only if its world column owns this folded cell.
  ld a, h
  and 3
  cp 3
  jr nz, _cv1_bg_dirty_tile
  ld a, l
  cp $80
  jr nc, _cv1_bg_dirty_continue
_cv1_bg_dirty_tile:
  ld a, h
  and 3
  ld b, a
  ld a, l
  and $e0
  or b
  cp $c0                    ; exact row<6 iff h&3=0 and l<C0
  jr nc, _cv1_bg_dirty_window
  ld a, b
  or a
  jr nz, _cv1_bg_dirty_window
  ld a, (CV1_BG_HUD)
  bit 1, a
  jr z, _cv1_bg_dirty_window
  bit 2, h
  jr nz, _cv1_bg_dirty_continue
  jr _cv1_bg_dirty_visible
_cv1_bg_dirty_window:
  ld a, h
  and 4
  rlca
  rlca
  rlca
  ld b, a
  ld a, l
  and $1f
  or b
  ld b, a
  ld a, (CV1_BG_WINDOW)
  ld c, a
  ld a, b
  sub c
  and $3f
  cp 32
  jr nc, _cv1_bg_dirty_continue
_cv1_bg_dirty_visible:
  jp _cv1_bg_load_cell
_cv1_bg_dirty_advance:
  ld hl, (CV1_BG_SCAN)
_cv1_bg_dirty_next:
  inc hl
  ld (CV1_BG_SCAN), hl
_cv1_bg_dirty_continue:
  ld hl, $cb79
  dec (hl)
  jp nz, _cv1_bg_dirty_probe
  jp _cv1_bg_step_out

; HL=raw CIRAM tile offset. Save folded cell, actual tile and raw attributes.
_cv1_bg_load_cell:
  ld d, h
  ld e, l
  ld a, h
  and 3
  ld h, a
  ld (CV1_BG_CELL), hl
  ld h, d
  set 7, h
  ld a, (hl)
  ld (CV1_BG_BASE), a
.ifdef PROFILE_TOP_TILE_REMAP_ROWS
  ; This is a profile display policy; raw CIRAM is never rewritten.
  ld a, d
  and 3
  rlca
  rlca
  rlca
  ld b, a
  ld a, e
  rlca
  rlca
  rlca
  and 7
  or b
  cp PROFILE_TOP_TILE_REMAP_ROWS
  jr nc, _cv1_bg_remap_done
  ld a, (CV1_BG_BASE)
.ifdef PROFILE_TOP_TILE_REMAP_FROM_0
  cp PROFILE_TOP_TILE_REMAP_FROM_0
  jr z, _cv1_bg_remap
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_1
  cp PROFILE_TOP_TILE_REMAP_FROM_1
  jr z, _cv1_bg_remap
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_2
  cp PROFILE_TOP_TILE_REMAP_FROM_2
  jr z, _cv1_bg_remap
.endif
.ifdef PROFILE_TOP_TILE_REMAP_FROM_3
  cp PROFILE_TOP_TILE_REMAP_FROM_3
  jr z, _cv1_bg_remap
.endif
  jr _cv1_bg_remap_done
_cv1_bg_remap:
  ld a, PROFILE_TOP_TILE_REMAP_TO
  ld (CV1_BG_BASE), a
_cv1_bg_remap_done:
.endif
  ; Attribute address = page|3C0|((row>>2)*8)|(column>>2).
  ld a, d
  and 3
  rlca
  rlca
  rlca
  ld b, a
  ld a, e
  rrca
  rrca
  rrca
  rrca
  rrca
  and 4
  or b
  add a, a
  ld b, a
  ld a, e
  rrca
  rrca
  and 7
  or b
  or $c0
  ld l, a
  ld a, d
  and 4
  or $83
  ld h, a
  ld c, (hl)
  ld a, e
  and $40                   ; row bit1 selects bottom quadrants
  rrca
  rrca
  rrca
  rrca
  ld b, a
  ld a, e
  and 2
  or b
  inc a
  ld b, a
_cv1_bg_sub_shift:
  dec b
  jr z, _cv1_bg_sub_ready
  srl c
  jr _cv1_bg_sub_shift
_cv1_bg_sub_ready:
  ld a, c
  and 3
  ld (CV1_BG_SUBPAL), a
  ld a, 7
  jp _cv1_bg_phase

; Return raw tile offset corresponding to folded HL in the prepared window.
_cv1_bg_fold_to_raw:
  ld a, h
  or a
  jr nz, _cv1_bg_fold_window
  ld a, l
  cp $c0
  jr nc, _cv1_bg_fold_window
  ld a, (CV1_BG_HUD)
  bit 1, a
  ret nz                    ; fixed six rows from NT-A
_cv1_bg_fold_window:
  ld a, (CV1_BG_WINDOW)
  ld b, a
  ld a, l
  sub b
  and $1f
  add a, b
  and $20
  rrca
  rrca
  rrca
  or h
  ld h, a
  ret

_cv1_bg_cache_addr:
  ld a, (CV1_BG_BASE)
  ld l, a
  ld h, 0
  add hl, hl
  add hl, hl
  ld a, (CV1_BG_SUBPAL)
  or l
  ld l, a
  ld de, $d600
  add hl, de
  ret
_cv1_bg_resolve:
  call _cv1_bg_cache_addr
  ld a, (hl)
  cp $ff
  jr z, _cv1_bg_miss
  ld (CV1_BG_SLOT), a
  jp _cv1_bg_resolved
_cv1_bg_miss:
  xor a
  ld (CV1_BG_PROBES), a
  ld a, 8
  jp _cv1_bg_phase

; At most16 candidates per step. Never use a slot owned by current OR pending
; output. Exhaustion requests an admitted blank/reset, never steals a slot.
_cv1_bg_allocate:
  ld a, 16
  ld ($cb79), a
_cv1_bg_candidate:
  ld a, (CV1_BG_ALLOC)
  ld (CV1_BG_SLOT), a
  inc a
  cp $ff
  jr c, _cv1_bg_candidate_store
  xor a
_cv1_bg_candidate_store:
  ld (CV1_BG_ALLOC), a
  ld a, (CV1_BG_SLOT)
  call _cv1_bg_current_count
  ld a, (hl)
  inc hl
  or (hl)
  jr nz, _cv1_bg_candidate_busy
  ld a, (CV1_BG_SLOT)
  ld l, a
  ld h, 0
  ld de, CV1_BG_PINS
  call _cv1_bg_bit_addr
  and (hl)
  jr nz, _cv1_bg_candidate_busy
  ; Reserve before the first pattern byte. A reverse entry may outlive its FC
  ; assignment after CHR invalidation: clear FC only when it still names us.
  ld a, (CV1_BG_SLOT)
  ld l, a
  ld h, >CV1_BG_REV_KEY
  ld a, (hl)
  cp $ff
  jr z, _cv1_bg_candidate_reserved
  ld b, a
  dec h
  ld l, (hl)
  ld h, 0
  add hl, hl
  add hl, hl
  ld a, l
  or b
  ld l, a
  ld de, $d600
  add hl, de
  ld a, (CV1_BG_SLOT)
  cp (hl)
  jr nz, _cv1_bg_candidate_reserved
  ld (hl), $ff
_cv1_bg_candidate_reserved:
  call _cv1_bg_pin
  ld a, 9
  jp _cv1_bg_phase
_cv1_bg_candidate_busy:
  ld hl, CV1_BG_PROBES
  inc (hl)
  ld a, (hl)
  cp $ff
  jp z, _cv1_bg_request_full
  ld hl, $cb79
  dec (hl)
  jp nz, _cv1_bg_candidate
  jp _cv1_bg_step_out
_cv1_bg_pin:
  ld a, (CV1_BG_SLOT)
  ld l, a
  ld h, 0
  ld de, CV1_BG_PINS
  jp _cv1_bg_bit_set

_cv1_bg_build:
  ld a, (CV1_BG_BASE)
  ld c, a
  ld a, (CV1_BG_SUBPAL)
  ld b, a
  ld a, (CV1_BG_SLOT)
  call rt_bg_gen_variant
  ; Converter restores ROM visibility; reacquire our metadata only afterward.
  call rt_raw_ciram_sram_enable
  call _cv1_bg_cache_addr
  ld a, (CV1_BG_SLOT)
  ld (hl), a                ; key becomes valid AFTER its complete pattern
  ld l, a
  ld h, >CV1_BG_REV_BASE
  ld a, (CV1_BG_BASE)
  ld (hl), a
  inc h
  ld a, (CV1_BG_SUBPAL)
  ld (hl), a
_cv1_bg_resolved:
  call _cv1_bg_pin
  ld hl, (CV1_BG_CELL)
  call _cv1_bg_current_shadow
  ld a, (CV1_BG_SLOT)
  cp (hl)
  jr nz, _cv1_bg_resolved_change
  ld a, (CV1_BG_FULL)
  or a
  jp z, _cv1_bg_next_cell
_cv1_bg_resolved_change:
  ld a, (CV1_BG_COPIED)
  or a
  ld a, 11
  jp nz, _cv1_bg_phase
  ld hl, 0
  ld (CV1_BG_COPY_CURSOR), hl
  ld a, 10
  jp _cv1_bg_phase

; Current metadata is immutable; copy896 shadow +512 counts only for an actual
; changed cell. Exactly64 bytes per step (22 chunks), no unconditional copies.
_cv1_bg_copy:
  ld hl, (CV1_BG_COPY_CURSOR)
  ld de, $0380
  or a
  sbc hl, de
  jr nc, _cv1_bg_copy_counts
  add hl, de
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_SHADOW0
  ld bc, CV1_BG_SHADOW1 - CV1_BG_SHADOW0
  jr z, _cv1_bg_copy_addresses
  ld de, CV1_BG_SHADOW1
  ld bc, CV1_BG_SHADOW0 - CV1_BG_SHADOW1
  jr _cv1_bg_copy_addresses
_cv1_bg_copy_counts:
  ld a, (CV1_BG_SELECTOR)
  or a
  ld de, CV1_BG_COUNTS0
  ld bc, CV1_BG_COUNTS1 - CV1_BG_COUNTS0
  jr z, _cv1_bg_copy_addresses
  ld de, CV1_BG_COUNTS1
  ld bc, CV1_BG_COUNTS0 - CV1_BG_COUNTS1
_cv1_bg_copy_addresses:
  add hl, de
  ld d, h
  ld e, l
  add hl, bc
  ex de, hl
  ld bc, 64
  ldir
  ld hl, (CV1_BG_COPY_CURSOR)
  ld de, 64
  add hl, de
  ld (CV1_BG_COPY_CURSOR), hl
  ld de, $0580
  or a
  sbc hl, de
  jp nz, _cv1_bg_step_out
  ld a, 1
  ld (CV1_BG_COPIED), a
  ld a, 11
  jp _cv1_bg_phase

_cv1_bg_queue:
  ld a, (CV1_BG_FULL)
  or a
  jr nz, _cv1_bg_queue_delta
  ld hl, (CV1_BG_CELL)
  ld de, CV1_BG_CELL_INDEX
  add hl, de
  ld a, (hl)
  cp $ff
  jr nz, _cv1_bg_queue_record
  ld a, (CV1_BG_RECORDS)
  cp 32
  jp nc, _cv1_bg_request_full
  ld (hl), a
  inc a
  ld (CV1_BG_RECORDS), a
  dec a
_cv1_bg_queue_record:
  ld l, a
  ld h, 0
  ld d, h
  ld e, l
  add hl, hl
  add hl, de
  ld de, CV1_BG_PACKET
  add hl, de
  push hl
  ld hl, (CV1_BG_CELL)
  add hl, hl
  ld a, h
  add a, $77               ; NT base3700 plus VDP write flag40
  ld d, a
  ld e, l
  pop hl
  ld (hl), e
  inc hl
  ld (hl), d
  inc hl
  ld a, (CV1_BG_SLOT)
  ld (hl), a
_cv1_bg_queue_delta:
  ld hl, (CV1_BG_CELL)
  call _cv1_bg_next_shadow
  ld a, (hl)
  ld c, a
  ld a, (CV1_BG_SLOT)
  ld (hl), a
  cp c
  jr z, _cv1_bg_queue_full_write
  ld a, c
  cp $ff
  jr z, _cv1_bg_queue_inc
  call _cv1_bg_next_count
  ld a, (hl)
  sub 1
  ld (hl), a
  inc hl
  jr nc, _cv1_bg_queue_inc
  dec (hl)
_cv1_bg_queue_inc:
  ld a, (CV1_BG_SLOT)
  call _cv1_bg_next_count
  inc (hl)
  jr nz, _cv1_bg_queue_full_write
  inc hl
  inc (hl)
_cv1_bg_queue_full_write:
  ld a, (CV1_BG_FULL)
  or a
  jr z, _cv1_bg_next_cell
  ; Only the explicit, already-blanked full rebuild takes this path.
  ld hl, (CV1_BG_CELL)
  add hl, hl
  ld a, l
  out ($bf), a
  ld a, h
  add a, $77
  out ($bf), a
  ld a, (CV1_BG_SLOT)
  out ($be), a
  xor a
  out ($be), a
_cv1_bg_next_cell:
  ld a, (CV1_BG_FULL)
  or a
  ld a, 6
  jp z, _cv1_bg_phase
  ld a, 14
  jp _cv1_bg_phase

_cv1_bg_request_full:
  ld a, (CV1_BG_FULL)
  or a
  jp nz, _cv1_bg_pool_failure ; even an empty255-slot working set cannot fit
  ld a, 12
  jp _cv1_bg_phase
; Coordinator called rt_cv1_sat_blank at a safe admitted boundary. Resume the
; requested reset only after the physical display-off flag is established.
rt_cv1_bg_prepare_blanked:
  ld a, ($c802)
  bit 1, a
  jp z, _cv1_bg_pool_failure
  call rt_raw_ciram_sram_enable
  ld a, 1
  ld (CV1_BG_FULL), a
  ld (CV1_BG_COPIED), a
  xor a
  ld (CV1_BG_RECORDS), a
  ld (CV1_BG_SELECTOR), a
  ld (CV1_BG_ALLOC), a
  ld hl, 0
  ld (CV1_BG_RESET_CURSOR), hl
  ld a, 13
  jp _cv1_bg_phase
; Reset disjoint regions in64-byte chunks while display is explicitly off.
; A ROM descriptor avoids borrowing source/OAM or copying unbounded blocks.
_cv1_bg_reset:
  ld hl, (CV1_BG_RESET_CURSOR)
  ld a, h
  ld b, a
  ld a, l
  and $c0
  ld c, a
  ld a, l
  and $3f
  ld l, a
  ld h, 0
  ld de, _cv1_bg_reset_regions
  add hl, de
  ld e, (hl)
  inc hl
  ld d, (hl)
  inc hl
  ld a, d
  or e
  jr z, _cv1_bg_reset_done
  ld a, (hl)               ; region byte count in64-byte chunks
  ld ($cb79), a
  inc hl
  ld a, (hl)               ; fill byte
  ld l, c
  ld h, b
  add hl, de
  ld bc, 64
  call mem_fill
  ld hl, (CV1_BG_RESET_CURSOR)
  ld de, 64
  add hl, de
  ld a, h
  rlca
  rlca
  ld b, a
  ld a, l
  rlca
  rlca
  and 3
  or b
  ld b, a
  ld a, ($cb79)
  cp b
  jr nz, _cv1_bg_reset_store
  ld a, l
  and $3f
  add a, 4
  ld l, a
  ld h, 0
_cv1_bg_reset_store:
  ld (CV1_BG_RESET_CURSOR), hl
  jp _cv1_bg_step_out
_cv1_bg_reset_done:
  ld hl, 0
  ld (CV1_BG_SCAN), hl
  ld a, 14
  jp _cv1_bg_phase
_cv1_bg_reset_regions:
  .dw CV1_BG_SHADOW0
  .db 28, $ff              ; shadow0+cell indexes1792 bytes
  .dw CV1_BG_SHADOW1
  .db 14, $ff
  .dw CV1_BG_COUNTS0
  .db 8, 0
  .dw CV1_BG_COUNTS1
  .db 8, 0
  .dw CV1_BG_PINS
  .db 1, 0                 ; includes liveCHR32: stable producer, already frozen
  .dw CV1_BG_REV_KEY
  .db 4, $ff
  .dw $d600
  .db 16, $ff
  .dw 0

_cv1_bg_full_scan:
  ld hl, (CV1_BG_SCAN)
  ld de, $0380
  or a
  sbc hl, de
  jp z, _cv1_bg_finish_begin
  add hl, de
  inc hl
  ld (CV1_BG_SCAN), hl
  dec hl
  call _cv1_bg_fold_to_raw
  jp _cv1_bg_load_cell

; Retire packet-index ownership before READY. Metadata itself remains frozen
; until commit. At most one index per step; no896-byte clear per generation.
_cv1_bg_finish_begin:
  ld hl, 0
  ld (CV1_BG_CURSOR), hl
  ld a, 15
  jp _cv1_bg_phase
_cv1_bg_finish:
  ld hl, (CV1_BG_CURSOR)
  ld a, h
  or l
  jr nz, _cv1_bg_finish_index
  ; Initialize retirement cursor with the immutable record count plus1.
  ld a, (CV1_BG_RECORDS)
  inc a
  ld l, a
  ld h, 0
_cv1_bg_finish_index:
  dec l
  ld (CV1_BG_CURSOR), hl
  jr z, _cv1_bg_prepared
  dec l
  ld e, l
  ld d, 0
  add hl, hl
  add hl, de
  ld de, CV1_BG_PACKET
  add hl, de
  ld e, (hl)
  inc hl
  ld a, (hl)
  sub $77
  ld d, a
  srl d
  rr e
  ld hl, CV1_BG_CELL_INDEX
  add hl, de
  ld (hl), $ff
  jp _cv1_bg_step_out
_cv1_bg_prepared:
  xor a
  ld (CV1_BG_PHASE), a
  jp _cv1_bg_step_out

_cv1_bg_pool_failure:
  ld a, $eb                ; explicit CV1 BG working-set overflow, not stealing
  ld ($cb1d), a
  di
_cv1_bg_pool_halt:
  halt
  jr _cv1_bg_pool_halt

; Frozen packet commit ONLY. SRAM8 is already mapped by the combined wrapper;
; DI/closed latch, any valid outer guard, no mapping changes or source reads.
; Caller has admitted the WHOLE BG+CRAM+HUD+SAT transaction at E0..E3. No polling
; or conversion here. Selector is published after NT ports, while DI prevents
; any allocator from observing the intermediate point before final SAT enable.
rt_cv1_bg_commit_admitted:
  ld a, (CV1_BG_RECORDS)
  or a
  jr z, _cv1_bg_commit_palette
  ld d, a
  ld c, $bf
  ld hl, CV1_BG_PACKET
_cv1_bg_commit_record:
  outi
  outi
  dec c
  outi
  inc c
  xor a
  out ($be), a
  dec d
  jr nz, _cv1_bg_commit_record
_cv1_bg_commit_palette:
  ld a, (CV1_BG_PAL_PENDING)
  or a
  jr z, _cv1_bg_commit_metadata
  xor a
  out ($bf), a
  ld a, $c0
  out ($bf), a
  ld hl, CV1_BG_CRAM
  ld bc, $20be
  otir
_cv1_bg_commit_metadata:
  ld a, (CV1_BG_COPIED)
  or a
  jr z, _cv1_bg_commit_latches
  ld a, (CV1_BG_SELECTOR)
  xor 1
  ld (CV1_BG_SELECTOR), a
_cv1_bg_commit_latches:
  ld a, (CV1_BG_TABLE)
  ld ($ca13), a
  ld a, (CV1_BG_WINDOW)
  ld ($cb2a), a
  ld a, 1
  ld (CV1_BG_INIT), a
  xor a
  ld (CV1_BG_RECORDS), a
  ld (CV1_BG_COPIED), a
  ld (CV1_BG_PAL_PENDING), a
  ret

.ends
.endif
