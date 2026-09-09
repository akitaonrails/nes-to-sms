.ifndef CNROM_SOURCE_HARDWARE_EXPERIMENT
.ifndef MMC3_FULL_RUNTIME
; chrmap.s — runtime tile remapping + background sub-palette baking.
;
; THE PALETTE PROBLEM: the NES picks one of 4 background sub-palettes per
; 16x16 region via the attribute table. SMS Mode 4 has a single 16-colour
; background palette (CRAM 0-15) and only a 1-bit palette-select per tile, so
; converted NES tiles (pixel values 0-3) can only ever index CRAM 0-3 — one
; sub-palette. Clouds (sub-palette 2, white) therefore rendered with the bush
; palette (sub-palette 0, green).
;
; THE FIX (SMS-native): bake the sub-palette S into the tile's high bitplanes
; so a pixel becomes S*4 + nes_pixel, indexing the full CRAM 0-15. CRAM then
; holds all four NES bg sub-palettes at once (0-3, 4-7, 8-11, 12-15). Because
; the same NES tile is used with different sub-palettes (cloud vs bush share
; tiles), variants are generated on demand into a pool of SMS bg slots (0-255)
; and cached by (base slot, S). 1-1's distinct combos fit well under 256.
;
; Per-cell sub-palette S comes from the attribute table, stored in the existing
; nametable shadow ($CC00, 2 low bits; the high byte is otherwise 0 now). The
; tile write reads S there and resolves the variant.
;
; MAPPING/LOCK CONTRACT: Every helper below that temporarily maps slot 2
; ($FFFF) or enables the CIRAM/CHR-RAM SRAM window is locked-only. It may run
; only beneath an outer PPU guard at depth 1 or 2, or on the presentation path
; at depth 0 with IFF disabled after the boot boundary assertion. The mapping
; helpers restore through rt_restore_prg_window, the locked restore primitive;
; no caller may substitute an unlocked bank restore.
;
; RAM:
;   $CA00        bg variant pool next-free slot (0-255)
;   $CA01-$CA06  do_variant scratch (p2, p3, slot, src ptr lo/hi, attr S)
;   $CA07        ring-wrapped flag (0 until slots 64-255 have all been used)
;   $CA08-$CA12  runtime guard/MRU bytes (boot.s/dispatch.s)
;   $CA40-$CAFF  reverse map for recycled slots 64-255: slot -> old base tile
;   $CC00-$D2FF  nametable shadow — active per-cell sub-palette S (0-3)
;   $D300-$D3FB  translated-call continuation stack frames
;   $D3FC-$D3FE  rt_ppu_write_cont continuation pointer/mode
;   $D500-$D5FF  translated-call continuation stack segment 1
;   $D600-$D9FF  variant cache FC[base*4 + S] -> pool slot ($FF = unassigned)

.define BGV_CACHE      $d600   ; FC[base*4+S] -> slot, 1024 bytes, $FF=empty
.define BGV_BSHADOW    $da00   ; base slot per cell (cell-indexed), 896 bytes
.define BGV_POOL_NEXT  $ca00
.define BGV_P2         $ca01
.define BGV_P3         $ca02
.define BGV_SLOT       $ca03
.define BGV_SRC        $ca04   ; + $ca05
.define BGV_ATTR_S     $ca06
.define BGV_RING_WRAPPED $ca07
.define BGV_REV_BASE   $ca40   ; 192 bytes: base tile for slots 64..255
; Nametable reference counts for ring slots 64-255 (BGV_REFCNT - 64 + slot).
; Maintained by _bgv_nt_write (the single NT-entry writer): the old slot is
; read back from VRAM before the write, so counts track the nametable exactly.
; The allocator skips slots with a nonzero count — a slot that is still on
; screen is never recycled, which is what used to paint stale garbage when the
; ring wrapped while the camera was locked (e.g. SMB's flagpole screen).
; Lives in the measured native-stack headroom: SP low-water over a full
; 1-1-clear route is $DFC4, so $DD80-$DE3F is safely below the stack.
.define BGV_REFCNT     $dd80   ; 192 bytes: NT refcount for slots 64..255

.section "chrmap" free

; ─── rt_bg_gen_variant ──────────────────────────────────────────────────────
; Generate one background tile variant into a VRAM pool slot: copy the base
; tile's low bitplanes from ROM (data_chr) and fill the high bitplanes with the
; sub-palette S (so every pixel gains S*4).
;   Entry: A = pool slot (0-255), B = S (0-3), C = base slot (0-255).
;   Clobbers AF, BC, DE, HL. Restores the current PRG window in slot 2.
;   Locked-only: temporarily maps slot 2 (or the CHR-RAM SRAM window). Valid
;   contexts are outer PPU guard depth 1/2, or presentation depth 0 with IFF
;   disabled after the boot boundary assertion; rt_restore_prg_window restores.
rt_bg_gen_variant:
  ld   (BGV_SLOT), a
  ; high-plane fill bytes from S: plane2 = (S&1)?$FF:0, plane3 = (S&2)?$FF:0
  ld   a, b
  and  $01
  jr   z, _gv_p2_zero
  ld   a, $ff
_gv_p2_zero:
  ld   (BGV_P2), a
  ld   a, b
  and  $02
  jr   z, _gv_p3_zero
  ld   a, $ff
_gv_p3_zero:
  ld   (BGV_P3), a
  ; source = data_chr ($8000) + base*32
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   de, $8000
  add  hl, de
  ld   (BGV_SRC), hl
  ; dest VRAM = slot*32 (bg region $0000-$1FE0)
  ld   a, (BGV_SLOT)
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   a, l
  out  ($bf), a
  ld   a, h
  or   $40
  out  ($bf), a
.ifdef NES_CHR_RAM
  ; CHR-RAM: pattern sources live in the cartridge-SRAM mirror
  ; (CHR_RAM_SRAM_BASE), maintained by the $2007 pattern-write path.
  ; NES tile layout is PLANAR: 8 bytes plane 0, then 8 bytes plane 1.
  ; Source = mirror + (BG table bit << 12) + base*16. Copy the 16
  ; bytes to staging ($CB63 p0 rows, $CB6B p1 rows), then emit rows
  ; interleaved with the S-plane fills.
  push bc
  ; Source table = the PRESENTED table ($CA13, maintained by the FC
  ; flush), not live PPUCTRL: games toggle bit 4 dozens of times per
  ; frame during uploads, and a variant generated against a transient
  ; table value would persist (the flush damper sees no presented
  ; change and never re-projects). Before the first flush ($CA13=$FF)
  ; fall back to live PPUCTRL.
.ifdef CV1_COHERENT_BG
  ; The source generation is immutable during stepped preparation. Committed
  ; CA13 stays unchanged until the matching NT words are published.
  ld   a, (CV1_BG_TABLE)
.else
  ld   a, ($ca13)
  cp   $ff
  jr   nz, _gvr_have_table
  ld   a, ($cb08)
.endif
_gvr_have_table:
  and  $10                   ; PPUCTRL bit 4 = BG table
  rrca
  rrca
  rrca
  rrca                       ; $10 -> $01: H must be table*0x100 BEFORE the
  ld   h, a                  ; <<4 below. The old code loaded $10 directly:
  ld   l, c                  ; (0x1000|base)*16 overflows 16 bits and wraps
  add  hl, hl                ; to base*16 — every table-1 tile read its
  add  hl, hl                ; TABLE-0 bytes (zeros for CV1's logo/title
  add  hl, hl                ; font), rendering empty patterns.
  add  hl, hl                ; *16 -> table*4096 + base*16
  ld   de, CHR_RAM_SRAM_BASE
  add  hl, de
  call rt_raw_ciram_sram_enable
  ld   de, $cb63
  ld   bc, 16
  ldir                       ; staging <- 16 planar bytes
  call rt_raw_ciram_sram_disable
  pop  bc
  ; re-set the VDP DEST address (untouched above, but be explicit)
  ld   a, (BGV_SLOT)
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  add  hl, hl
  ld   a, l
  out  ($bf), a
  ld   a, h
  or   $40
  out  ($bf), a
  ld   hl, $cb63             ; plane-0 rows; plane-1 at +8
  ld   b, 8
_gvr_emit_row:
  ld   a, (hl)               ; plane 0 row
  ld   (BGV_SRC), a
  out  ($be), a
  push bc
  ld   bc, 8
  add  hl, bc                ; -> plane 1 row
  ld   a, (hl)
  out  ($be), a
  ld   c, a
  ld   a, (BGV_SRC)
  or   c                     ; NES pixel 0 keeps universal background colour
  ld   c, a
  ld   a, (BGV_P2)
  and  c
  out  ($be), a
  ld   a, (BGV_P3)
  and  c
  out  ($be), a
  ld   bc, -7                ; back to next plane-0 row
  add  hl, bc
  pop  bc
  djnz _gvr_emit_row
  call rt_restore_prg_window
  ret
.else
  ; map data_chr bank for the source reads
  ld   a, :data_chr
  ld   ($ffff), a
  ld   hl, (BGV_SRC)
  ld   b, 8
_gv_row:
  ld   a, (hl)               ; plane 0 (low NES bitplane)
  ld   (BGV_SRC), a
  out  ($be), a
  inc  hl
  ld   a, (hl)               ; plane 1
  out  ($be), a
  ld   c, a
  ld   a, (BGV_SRC)
  or   c                     ; only non-zero NES pixels select sub-palette S
  ld   c, a
  inc  hl                    ; skip source planes 2,3 (zero in data_chr)
  ld   a, (BGV_P2)
  and  c
  out  ($be), a              ; plane 2 = S bit 0
  ld   a, (BGV_P3)
  and  c
  out  ($be), a              ; plane 3 = S bit 1
  inc  hl
  inc  hl
  djnz _gv_row
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  ret
.endif

; ─── rt_bg_get_variant ──────────────────────────────────────────────────────
; ─── _bgv_nt_write ──────────────────────────────────────────────────────────
; The single writer for background nametable entries.
;   Entry: HL = NT low-byte VRAM address, C = variant slot.
;   Preserves BC, DE, HL. Clobbers AF.
; Reads the old slot back from VRAM first (code-0 prefetch read) and moves the
; ring refcount from it to the new slot, so BGV_REFCNT tracks the nametable
; exactly — no shadow-state inference, no drift.
_bgv_nt_write:
  ; Pre-wrap every ring slot is assign-once, so counts are not consulted;
  ; skip the read-back and keep the hot path as cheap as the old inline
  ; write. _bgv_ref_rebuild reconstructs the counts when the ring first
  ; wraps, and from then on this maintains them incrementally.
  ld   a, (BGV_RING_WRAPPED)
  or   a
  jr   nz, _bgv_nt_write_counted
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0 (palette 0, tile bit 8 = 0)
  ret
_bgv_nt_write_counted:
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  out  ($bf), a              ; code 0 = VRAM read: prefetches the old byte
  push af                    ; small delay for the VDP prefetch
  pop  af
  in   a, ($be)              ; old slot (tile low byte)
  call _bgv_ref_dec
  ; re-set the address for writing (the prefetch advanced it)
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, c
  out  ($be), a              ; tile low byte = variant slot
  xor  a
  out  ($be), a              ; high byte = 0 (palette 0, tile bit 8 = 0)
  ld   a, c
  ; fall through: count the new reference

; _bgv_ref_inc: A = slot. Saturating ++ for ring slots (64-255); $FF sticks.
; Preserves BC, DE, HL. Clobbers AF.
_bgv_ref_inc:
  cp   64
  ret  c
  push hl
  call _bgv_refcnt_addr
  ld   a, (hl)
  inc  a
  jr   z, _bgv_ref_inc_done  ; saturated at $FF: pinned forever (safe side)
  ld   (hl), a
_bgv_ref_inc_done:
  pop  hl
  ret

; _bgv_ref_dec: A = slot. Saturating -- (0 and $FF are sticky).
; Preserves BC, DE, HL. Clobbers AF.
_bgv_ref_dec:
  cp   64
  ret  c
  push hl
  call _bgv_refcnt_addr
  ld   a, (hl)
  or   a
  jr   z, _bgv_ref_dec_done
  cp   $ff
  jr   z, _bgv_ref_dec_done
  dec  (hl)
_bgv_ref_dec_done:
  pop  hl
  ret

; Rebuild the ring refcounts from the live nametable. Called once, at the
; moment the ring first wraps (BGV_RING_WRAPPED 0 -> 1): before that no slot
; is ever reused so no counts are needed, and afterwards _bgv_nt_write
; maintains them incrementally. One 896-cell VRAM scan (~55k cycles) — a
; one-frame hiccup at most once per scene, instead of a per-write tax.
; Clobbers AF, BC, DE, HL. Leaves the VDP address pointing into the NT
; (every caller re-sets the address before its next VRAM access).
; Mark the ring wrapped; on the 0 -> 1 transition rebuild the refcounts from
; the live nametable. Preserves BC, DE, HL. Clobbers AF.
_bgv_ring_mark_wrapped:
  ld   a, (BGV_RING_WRAPPED)
  or   a
  ret  nz
  ld   a, $01
  ld   (BGV_RING_WRAPPED), a
  push bc
  push de
  push hl
  call _bgv_ref_rebuild
  pop  hl
  pop  de
  pop  bc
  ret

_bgv_ref_rebuild:
  ld   hl, BGV_REFCNT
  ld   de, BGV_REFCNT + 1
  ld   bc, 191
  ld   (hl), $00
  ldir
  xor  a
  out  ($bf), a
  ld   a, $37                ; NT base $3700, code 0 (VRAM read)
  out  ($bf), a
  ld   bc, 896
_bgv_rr_loop:
  in   a, ($be)              ; tile low byte (auto-increment)
  cp   64
  jr   c, _bgv_rr_skip
  push bc
  call _bgv_refcnt_addr
  ld   a, (hl)
  inc  a
  jr   z, _bgv_rr_sat        ; saturate at $FF
  ld   (hl), a
_bgv_rr_sat:
  pop  bc
_bgv_rr_skip:
  in   a, ($be)              ; discard the high byte
  dec  bc
  ld   a, b
  or   c
  jr   nz, _bgv_rr_loop
  ret

; HL = &BGV_REFCNT[A - 64] (caller guarantees A >= 64). Clobbers AF, HL.
_bgv_refcnt_addr:
  ld   l, a
  ld   h, $dd                ; BGV_REFCNT - 64 = $DD40
  ld   a, l
  add  a, $40
  ld   l, a
  ret  nc
  inc  h
  ret

; Resolve (base slot, S) to a bg pool slot, generating + caching on first use.
;   Entry: C = base slot, B = S (0-3).  Exit: A = pool slot.
;   Clobbers AF, DE, HL (B, C consumed).
rt_bg_get_variant:
.ifdef PROFILE_CHR_RAM_BG_IDENTITY
  ; A dense CHR-RAM screen can require more than 256 (tile,sub-palette)
  ; combinations, causing the variant ring to recycle slots that are still
  ; visible. Identity mode pins each NES tile to the same SMS slot and uses
  ; the first background palette. $D600[base] caches the presented CHR table
  ; for which that slot was generated.
  ld   a, c
  ld   (BGV_SLOT), a
  ld   l, a
  ld   h, $00
  ld   de, BGV_CACHE
  add  hl, de
  ld   a, ($ca13)
  cp   $ff
  jr   nz, _gbv_identity_have_table
  ld   a, ($cb08)
_gbv_identity_have_table:
  and  $10
  or   $80
  ld   (BGV_ATTR_S), a
  cp   (hl)
  jr   z, _gbv_identity_done
  ld   a, (BGV_SLOT)
  ld   c, a
  ld   b, $00
  call rt_bg_gen_variant
  ld   a, (BGV_SLOT)
  ld   l, a
  ld   h, $00
  ld   de, BGV_CACHE
  add  hl, de
  ld   a, (BGV_ATTR_S)
  ld   (hl), a
_gbv_identity_done:
  ld   a, (BGV_SLOT)
  ret
.endif
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl                ; base*4
  ld   a, b
  add  a, l
  ld   l, a
  jr   nc, _gbv_nc
  inc  h
_gbv_nc:
  ld   de, BGV_CACHE
  add  hl, de                ; HL = &FC[base*4+S]
  ld   a, (hl)
  inc  a                     ; $FF -> 0 (Z) means unassigned
  jr   z, _gbv_alloc
  dec  a
  ret                        ; A = cached slot
_gbv_alloc:
  ; A full 1-1 traversal produces far more (base,S) combos (~800) than the 256
  ; background slots, but only ~150 are on screen at once. So slots 64-255 are a
  ; RING: a slot is only recycled after ~38 columns of scrolling, after its tile
  ; left the screen (SMB's 1-1 never scrolls back left). Slots 0-63 are pinned
  ; for the common tiles allocated on the first screen (sky, ground, brick,
  ; pipe, bush, status bar, ...) so the recurring graphics stay correct all
  ; level; only rarer mid-level-specific tiles ride the ring.
  ; Probe the ring for a slot no nametable cell references (BGV_REFCNT == 0).
  ; Recycling a still-visible slot painted stale garbage when the ring wrapped
  ; while the screen was static (SMB flagpole endgame). If every ring slot is
  ; referenced (a denser screen than the ring can hold), fall back to stealing
  ; the current candidate — the pre-refcount behaviour.
  ld   a, (BGV_POOL_NEXT)
  ld   e, a
  ld   d, 192
_gbv_probe:
  ld   a, e
  cp   64
  jr   c, _gbv_chosen        ; pre-ring slots are always fresh
  push hl
  call _bgv_refcnt_addr
  ld   a, (hl)
  pop  hl
  or   a
  jr   z, _gbv_chosen        ; unreferenced: take it
  ld   a, e
  inc  a
  jr   nz, _gbv_probe_next
  call _bgv_ring_mark_wrapped
  ld   a, 64                 ; wrap back to the ring start (0-63 pinned)
_gbv_probe_next:
  ld   e, a
  dec  d
  jr   nz, _gbv_probe
_gbv_chosen:
  ld   a, e
  ld   (BGV_SLOT), a
  push hl                    ; save FC[idx] for the new mapping
  push bc                    ; keep B=S, C=base for variant generation
  call _bgv_invalidate_recycled_slot
  pop  bc
  pop  hl
  ld   a, (BGV_SLOT)
  ld   (hl), a               ; FC[idx] = slot
  ; Remember which base tile now owns this ring slot. Slots 0-63 are pinned and
  ; never recycled after wrap, so only slots 64-255 need reverse-map entries.
  cp   64
  jr   c, _gbv_remember_done
  sub  64
  ld   e, a
  ld   d, $00
  ld   hl, BGV_REV_BASE
  add  hl, de
  ld   (hl), c
_gbv_remember_done:
  ld   a, (BGV_SLOT)
  inc  a
  jr   nz, _gbv_set          ; 255 -> 0 means the ring wrapped
  call _bgv_ring_mark_wrapped
  ld   a, 64                 ; wrap back to the start of the ring (pin 0-63)
_gbv_set:
  ld   (BGV_POOL_NEXT), a
  ld   a, (BGV_SLOT)
  call rt_bg_gen_variant     ; A=slot, B=S, C=base
  ld   a, (BGV_SLOT)
  ret

; Clear the stale FC[old_base*4+old_S] entry before reusing a ring slot.
; Without this, a later request for the old (base,S) pair can hit the cache and
; return a slot whose VRAM pattern has since been regenerated for another tile.
; Entry: BGV_SLOT = slot being allocated. Clobbers AF, C, DE, HL.
_bgv_invalidate_recycled_slot:
  ld   a, (BGV_RING_WRAPPED)
  or   a
  ret  z                     ; first pass: reverse map not complete yet
  ld   a, (BGV_SLOT)
  cp   64
  ret  c                     ; pinned slots are never recycled
  sub  64
  ld   e, a
  ld   d, $00
  ld   hl, BGV_REV_BASE
  add  hl, de
  ld   c, (hl)               ; old base tile for this slot
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, hl                ; old base*4
  ld   de, BGV_CACHE
  add  hl, de                ; HL = &FC[old_base*4]
  ld   a, (BGV_SLOT)
  ld   e, a                  ; E = reused slot number
  ld   d, 4                  ; scan old base's four sub-palette entries
_gbv_inv_loop:
  ld   a, (hl)
  cp   e
  jr   nz, _gbv_inv_next
  ld   a, $ff
  ld   (hl), a
  ret
_gbv_inv_next:
  inc  hl
  dec  d
  jr   nz, _gbv_inv_loop
  ret

; Discard every cached (tile, sub-palette) assignment before a complete
; visible-window rebuild.  Full rebuild callers immediately rewrite all 896
; visible cells, so no live nametable entry can retain an invalidated slot.
; Resetting the allocator here prevents variants from earlier scenes from
; consuming the finite 256-slot background pattern table; a dense but valid
; current screen can then use all slots without recycling visible patterns.
; Clobbers: AF, BC, DE, HL.
rt_bg_reset_variant_cache:
  ld   hl, BGV_CACHE
  ld   de, BGV_CACHE + 1
  ld   bc, $03ff
  ld   (hl), $ff
  ldir
  ; The full rebuild rewrites every cell through _bgv_nt_write, which reads
  ; the stale slots back and would decrement counts for content that is being
  ; discarded wholesale — start the refcounts from zero instead.
  ld   hl, BGV_REFCNT
  ld   de, BGV_REFCNT + 1
  ld   bc, 191
  ld   (hl), $00
  ldir
  xor  a
  ld   (BGV_POOL_NEXT), a
  ld   (BGV_RING_WRAPPED), a
  ret

; Regenerate every assigned variant in its existing VRAM slot after the NES
; background pattern-table select changes.  The cache key is (tile,S), while
; PPUCTRL selects one source table globally, so slot ownership does not need to
; change: only the 4bpp pixels do.  Preserving the slots is important because
; all visible nametable cells contain those slot numbers.  The former
; reset-and-rematerialize path renumbered the cache and rewrote 896 cells; on a
; real VDP that work ran far beyond VBlank and exposed a half-old/half-new
; screen.  Entry: $CA13 already contains the newly presented table bit.
; Clobbers: AF, BC, DE, HL.
rt_bg_refresh_variant_cache:
  ld   hl, BGV_CACHE
  ld   de, $0000             ; D = base tile, E = sub-palette S
_bgv_refresh_loop:
  ld   a, (hl)
  cp   $ff
  jr   z, _bgv_refresh_next
  push hl
  push de
  ld   c, d                  ; base tile
  ld   b, e                  ; sub-palette S
  call rt_bg_gen_variant     ; A = existing slot
  pop  de
  pop  hl
_bgv_refresh_next:
  inc  hl
  inc  e
  ld   a, e
  cp   4
  jr   c, _bgv_refresh_loop
  ld   e, 0
  inc  d
  jr   nz, _bgv_refresh_loop
  ret

; ─── rt_bg_map_base_slot ─────────────────────────────────────────────────────
; Map a NES background tile byte through the active BG CHR map.
;   Entry: A = NES bg tile byte.
;   Exit:  A = mapped SMS base slot.
;   Preserves BC, DE, HL. Clobbers AF. Restores the current PRG window in slot 2.
; Helper-only scaffold for a later CIRAM materializer; current rendering paths
; still use their inlined lookups and BGV_BSHADOW.
; Locked-only: temporarily maps slot 2; valid only under the outer PPU guard
; at depth 1/2, or in presentation at depth 0 with IFF disabled after the boot
; boundary assertion. rt_restore_prg_window is the locked restore primitive.
rt_bg_map_base_slot:
  push hl
  push de
  push bc
  ld   c, a
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bg_map_base_slot_table1
  ld   de, data_chr_bg_map0
  jr   _bg_map_base_slot_ready
_bg_map_base_slot_table1:
  ld   de, data_chr_bg_map1
_bg_map_base_slot_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   c, (hl)                ; byte 0 of two-byte map record = base slot
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  ld   a, c
  pop  bc
  pop  de
  pop  hl
  ret

; ─── rt_bgv_sub_palette ───────────────────────────────────────────────────────
; Read the per-cell sub-palette S (0-3) from the nametable shadow.
;   Entry: HL = SMS nametable low-byte address ($3700-$3EFE).
;   Exit:  A = S (0-3). Clobbers HL.
rt_bgv_sub_palette:
  inc  hl                    ; -> high-byte address
  ld   a, h
  cp   $3e
  jr   nc, rt_bgv_sub_palette_s0 ; hidden rows are outside the active shadow
  add  a, $95                ; $37xx -> $CCxx shadow
  ld   h, a
  ld   a, (hl)
  and  $03
  ret
rt_bgv_sub_palette_s0:
  xor  a
  ret

; ─── rt_bgv_base_addr ─────────────────────────────────────────────────────────
; Map an SMS nametable low-byte address to its per-cell base-slot shadow byte.
;   Entry: HL = NT low-byte address ($3700-$3EFE).
;   Exit:  HL = &BGV_BSHADOW[cell]. Clobbers A, DE.
rt_bgv_base_addr:
  ld   de, $c900             ; + $C900 == - $3700 (mod 16-bit)
  add  hl, de
  srl  h
  rr   l                     ; >>1 = cell index
  ld   de, BGV_BSHADOW
  add  hl, de
  ret

; ─── rt_write_mapped_bg_tile ────────────────────────────────────────────────
; Write a background nametable entry. Entry: A = NES tile byte; the caller has
; set the VDP write address to the cell. Resolves the (base, S) variant and
; writes its slot as the tile, palette 0 (sub-palette baked into the pixels).
; Clobbers AF, BC, DE, HL. Restores the current PRG window in slot 2.
; Locked-only: its temporary slot-2 map is valid only under an outer PPU guard
; at depth 1/2, or in presentation at depth 0 with IFF disabled after the boot
; boundary assertion. Its inline restore is equivalent to the locked
; rt_restore_prg_window primitive.
rt_write_mapped_bg_tile:
  ld   c, a
  ; $CB18 is rt_ppu_write's saved 6502 accumulator.  Do not use the old
  ; overlapping `$CB17` word scratch here: storing DE there replaced the
  ; accumulator with the nametable high byte and corrupted repeated STA $2007
  ; loops.  C already owns the tile byte, so $CB13 can safely park the address
  ; high byte for this helper.
  ld   a, e
  ld   ($cb17), a
  ld   a, d
  ld   ($cb13), a

.ifdef NES_CHR_RAM
  ; CHR-RAM: the base IS the NES tile index within the active BG table
  ; (identity — sources live in the SRAM mirror, keyed by the same
  ; index; the static ROM maps don't apply).
.else
  ; base slot from the BG map
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bgw_table1
  ld   de, data_chr_bg_map0
  jr   _bgw_map_ready
_bgw_table1:
  ld   de, data_chr_bg_map1
_bgw_map_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   a, (hl)               ; base slot (bg tiles are 0-255)
  ld   c, a                  ; C = base slot
  ; Inline rt_restore_prg_window in the hottest nametable write path to avoid
  ; one more call frame while a translated NMI is nested under the frame IRQ.
.ifdef NES_PRG_BANK_BASE
  ld   a, ($cb62)
  add  a, NES_PRG_BANK_BASE
  ld   ($ffff), a
.else
  ld   a, :data_prg_low
  ld   ($ffff), a
.endif
.endif

  ; In nametable range: record the base slot for this cell (so a later
  ; attribute write can re-resolve the variant), then read the sub-palette S.
  ld   a, ($cb17)
  ld   l, a
  ld   a, ($cb13)
  ld   h, a
  ld   a, h
  cp   $37
  jr   c, _bgw_s0
  cp   $40
  jr   nc, _bgw_s0
  call rt_bgv_base_addr        ; HL(low addr) -> base-shadow addr (preserves BC)
  ld   (hl), c               ; base-shadow[cell] = base slot
  ld   a, ($cb17)
  ld   l, a
  ld   a, ($cb13)
  ld   h, a
  call rt_bgv_sub_palette      ; A = S
  jr   _bgw_have_s
_bgw_s0:
  xor  a
_bgw_have_s:
  ld   b, a                  ; B = S
  call rt_bg_get_variant     ; -> A = pool slot
  ld   c, a                  ; C = variant slot

  ; write the nametable entry (re-set the address: gen may have moved it)
  ld   a, ($cb17)
  ld   l, a
  ld   a, ($cb13)
  ld   h, a
  jp   _bgv_nt_write         ; HL = NT low addr, C = variant slot

; ─── rt_write_mapped_bg_tile_s ──────────────────────────────────────────────
; Helper-only explicit-subpalette variant of rt_write_mapped_bg_tile.
; Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable low-byte address.
; Resolves the (base slot, S) variant without consulting folded per-cell
; palette state. Still records the base slot in BGV_BSHADOW for folded SMS
; cells in the nametable range $3700-$3EFF so later explicit-S redraw helpers
; can re-resolve the cell.
; Preserves BC, DE, HL. Clobbers AF. Restores the current PRG window in slot 2.
; Scaffold only: current hot paths still call rt_write_mapped_bg_tile.
; Locked-only: temporarily maps slot 2; valid only under the outer PPU guard
; at depth 1/2, or in presentation at depth 0 with IFF disabled after the boot
; boundary assertion. rt_restore_prg_window is the locked restore primitive.
rt_write_mapped_bg_tile_s:
  push hl
  push de
  push bc
  ld   c, a                  ; save NES tile byte before loading explicit S
  ld   a, b
  and  $03
  ld   ($cb16), a            ; explicit S
  ld   a, e
  ld   ($cb17), a
  ld   a, d
  ld   ($cb13), a            ; address high; never overlap saved A at $CB18

  ; base slot from the BG map
  ld   a, :data_chr_maps
  ld   ($ffff), a
  ld   a, ($cb08)
  bit  4, a
  jr   nz, _bgw_s_table1
  ld   de, data_chr_bg_map0
  jr   _bgw_s_map_ready
_bgw_s_table1:
  ld   de, data_chr_bg_map1
_bgw_s_map_ready:
  ld   l, c
  ld   h, $00
  add  hl, hl
  add  hl, de
  ld   a, (hl)               ; base slot (bg tiles are 0-255)
  ld   c, a                  ; C = base slot
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)

  ; In nametable range: record the base slot for this folded SMS cell.
  ld   a, ($cb17)
  ld   l, a
  ld   a, ($cb13)
  ld   h, a
  ld   a, h
  cp   $37
  jr   c, _bgw_s_no_base_shadow
  cp   $3f
  jr   nc, _bgw_s_no_base_shadow
  call rt_bgv_base_addr        ; HL(low addr) -> base-shadow addr (preserves BC)
  ld   (hl), c               ; base-shadow[cell] = base slot
_bgw_s_no_base_shadow:
  ld   a, ($cb16)
  ld   b, a                  ; B = explicit S
  call rt_bg_get_variant     ; -> A = pool slot
  ld   c, a                  ; C = variant slot

  ; write the nametable entry (re-set the address: gen may have moved it)
  ld   a, ($cb17)
  ld   l, a
  ld   a, ($cb13)
  ld   h, a
  call _bgv_nt_write         ; HL = NT low addr, C = variant slot

  pop  bc
  pop  de
  pop  hl
  ret

; ─── rt_write_mapped_bg_tile_s_noshadow ─────────────────────────────────────
; Write a background nametable entry from an explicit subpalette without
; touching the per-cell base-slot shadow.
;   Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable low-byte address.
;   Preserves BC, DE, HL. Clobbers AF. Restores the current PRG window in slot 2.
; Helper-only scaffold for a later CIRAM materializer; current paths still use
; BGV_BSHADOW so attribute redraw remains route-equivalent.
; Locked-only transitively through rt_bg_map_base_slot: valid only under an
; outer PPU guard at depth 1/2, or in presentation at depth 0 with IFF disabled
; after the boot boundary assertion. rt_restore_prg_window is the locked
; restore primitive.
rt_write_mapped_bg_tile_s_noshadow:
  push hl
  push de
  push bc
  call rt_bg_map_base_slot    ; -> A = base slot, preserves B=S and DE=addr
  ld   c, a                   ; C = base slot, B = explicit S
  call rt_bg_get_variant      ; -> A = pool slot (clobbers DE, HL)
  pop  bc                     ; caller B/C
  pop  de                     ; entry DE = NT low-byte address
  push bc

  ; Write the nametable entry. Variant generation may have moved the VDP addr.
  ld   c, a                  ; C = variant slot
  ld   h, d
  ld   l, e
  call _bgv_nt_write         ; HL = NT low addr, C = variant slot

  pop  bc
  pop  hl
  ret

; ─── rt_redraw_bg_cell_tile_s_noshadow ──────────────────────────────────────
; Helper-only scaffold for a future DA00-free attribute/materializer redraw.
; Redraw one SMS nametable cell from an explicit NES tile byte and subpalette
; without reading/writing BGV_BSHADOW, folded $CC00 state, or the $D3xx
; continuation/diagnostic slots. Intentionally uncalled by current runtime paths.
;   Entry: A = NES tile byte, B = S (0..3), DE = SMS nametable high-byte address.
;          The corresponding tile low-byte address is DE-1.
;   Preserves BC, DE, HL. Clobbers AF. Restores the current PRG window in slot 2.
;   Locked-only transitively through rt_write_mapped_bg_tile_s_noshadow; use
;   only beneath outer PPU guard depth 1/2, or in presentation with IFF disabled
;   at depth 0 after the boot boundary assertion.
rt_redraw_bg_cell_tile_s_noshadow:
  push de
  dec  de                    ; high-byte addr -> low-byte tile addr
  call rt_write_mapped_bg_tile_s_noshadow
  pop  de
  ret

; Map a NES sprite tile to an SMS sprite tile byte.
; Entry: A = NES OAM tile byte. Uses PPUCTRL bit 3 ($CB08) to choose NES sprite
; pattern table 0/1. Table 0 maps to tile bytes for SMS sprite base $2000;
; table 1 maps to tile bytes for SMS sprite base $0000.
; Exit: A = SMS sprite tile byte. Preserves BC, DE, HL; clobbers AF.
; Locked-only: temporarily maps slot 2; valid only under the outer PPU guard
; at depth 1/2, or in presentation at depth 0 with IFF disabled after the boot
; boundary assertion. rt_restore_prg_window is the locked restore primitive.
rt_map_sprite_tile:
.ifdef NES_CHR_RAM
  ; Pattern writes are copied to the SMS sprite region under the same NES tile
  ; number, so dynamic CHR uses an identity map. Static packing maps are built
  ; from the ROM's CHR asset, which is intentionally blank on CHR-RAM carts.
  ret
.else
  push hl
  push de
  push bc
  ld   c, a

  ld   a, :data_chr_maps
  ld   ($ffff), a

  ld   a, ($cb08)
  bit  3, a
  jr   nz, _chrmap_sprite_table1
  ld   de, data_chr_sprite_map0
  jr   _chrmap_sprite_base_ready
_chrmap_sprite_table1:
  ld   de, data_chr_sprite_map1
_chrmap_sprite_base_ready:
  ld   l, c
  ld   h, $00
  add  hl, de
  ld   a, (hl)
  ld   ($cb13), a

  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  pop  bc
  pop  de
  pop  hl
  ld   a, ($cb13)
  ret
.endif

; Build DE = SMS nametable high-byte address for the top-left tile covered by
; attribute offset $CB19. Formula:
;   high = $37 + (attr_offset >> 3)
;   low  = 1 + ((attr_offset & 7) * 8)
rt_attr_base_tl:
  ld   a, ($cb19)
  and  $07
  add  a, a
  add  a, a
  add  a, a
  inc  a
  ld   e, a
  ld   a, ($cb19)
  srl  a
  srl  a
  srl  a
  add  a, $37
  ld   d, a
  ret

; Update one NES attribute-table quadrant: set the sub-palette S (0-3) for the
; 2x2 cells it covers, and re-resolve each cell's baked-in tile variant. Doing
; the re-resolve here (rather than relying on a following tile write) makes the
; result correct regardless of whether SMB writes tiles or attributes first:
; the last writer (tile or attribute) sees both the base slot (from the cell's
; base-shadow) and S, so the final variant is right.
; Entry: A = NES palette selector 0..3, DE = SMS nametable high-byte address
; for the top-left tile. Preserves BC and DE.
rt_write_bg_attr_quadrant:
  push bc
  push de
  and  $03
  ld   (BGV_ATTR_S), a       ; S for this quadrant
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 62                 ; next row, same column
  ld   e, a
  call _chrmap_attr_write_one
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one
  pop  de
  pop  bc
  ret

; Expensive helper-only explicit-subpalette redraw for one NES attribute-table
; quadrant. This does not update folded per-cell palette state; it uses the
; existing BGV_BSHADOW base-slot bytes to redraw the four covered cells by
; resolving each with explicit S. It is scaffold for later materializer work and
; is not called by current hot paths.
; Entry: A = S (0..3), DE = SMS nametable high-byte address for top-left tile.
; Preserves BC and DE. Clobbers AF, HL.
rt_redraw_bg_attr_quadrant_s:
  push bc
  push de
  and  $03
  ld   (BGV_ATTR_S), a       ; explicit S for this quadrant
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 62                 ; next row, same column
  ld   e, a
  call _chrmap_attr_write_one_s
  ld   a, e
  add  a, 2
  ld   e, a
  call _chrmap_attr_write_one_s
  pop  de
  pop  bc
  ret

; Set S for one visible cell and rewrite its tile to the matching variant.
; Entry: DE = nametable high-byte address; S in BGV_ATTR_S. Preserves DE.
_chrmap_attr_write_one:
  push de
  ld   a, d
  cp   $3e
  jr   nc, _caw_done         ; hidden rows are outside the active $CC00-$D2FF shadow
  ; shadow byte (high-byte addr -> $CCxx). Skip the whole re-resolve if the
  ; sub-palette is unchanged (the usual case while scrolling), so attribute
  ; traffic stays cheap; the tile write keeps the variant correct otherwise.
  ld   h, d
  ld   l, e
  ld   a, h
  add  a, $95
  ld   h, a
  ld   a, (BGV_ATTR_S)
  ld   c, a                  ; C = new S
  ld   a, (hl)
  and  $03
  cp   c
  jr   z, _caw_done          ; unchanged -> done
  ld   (hl), c               ; store new S
  ; base slot for this cell, from the low-byte address (high addr - 1)
  ld   h, d
  ld   l, e
  dec  hl                    ; HL = NT low-byte address
  push hl                    ; save for the VDP write
  call rt_bgv_base_addr        ; HL -> base-shadow addr
  ld   c, (hl)               ; C = base slot
  ld   a, (BGV_ATTR_S)
  ld   b, a                  ; B = S
  call rt_bg_get_variant     ; -> A = variant slot
  ld   c, a
  pop  hl                    ; HL = NT low-byte address
  call _bgv_nt_write         ; HL = NT low addr, C = variant slot
_caw_done:
  pop  de
  ret

; Redraw one cell using explicit S and the base-slot shadow only.
; Entry: DE = nametable high-byte address; S in BGV_ATTR_S. Preserves DE.
_chrmap_attr_write_one_s:
  push de
  ld   h, d
  ld   l, e
  dec  hl                    ; HL = NT low-byte address
  push hl                    ; save for the VDP write
  call rt_bgv_base_addr        ; HL -> base-shadow addr
  ld   c, (hl)               ; C = base slot
  ld   a, (BGV_ATTR_S)
  ld   b, a                  ; B = explicit S
  call rt_bg_get_variant     ; -> A = variant slot
  ld   c, a
  pop  hl                    ; HL = NT low-byte address
  call _bgv_nt_write         ; HL = NT low addr, C = variant slot
  pop  de
  ret

.ends
.endif ; separate full MMC3 graphics backend
.endif ; source hardware uses immutable packet presentation
