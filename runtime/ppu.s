; ppu.s — NES PPU register emulation on SMS.
;
; NES PPU registers (CPU-visible addresses):
;   $2000 PPUCTRL   (W)   — control flags
;   $2001 PPUMASK   (W)   — rendering enable/mask
;   $2002 PPUSTATUS (R)   — VBlank / sprite-0 / overflow flags
;   $2003 OAMADDR   (W)   — OAM address
;   $2004 OAMDATA   (R/W) — OAM data
;   $2005 PPUSCROLL (W)   — scroll (write twice: X then Y)
;   $2006 PPUADDR   (W)   — VRAM address (write twice: hi then lo)
;   $2007 PPUDATA   (R/W) — VRAM data
;
; Shadow registers in SMS RAM ($CB08-$CB24):
;   $CB08  ppu_ctrl  (shadow of $2000)
;   $CB09  ppu_mask  (shadow of $2001)
;   $CB0A  oam_addr  (shadow of $2003)
;   $CB0B  scroll_toggle (0 = first write = X, 1 = second write = Y)
;   $CB0C  scroll_x latch
;   $CB0D  scroll_y latch
;   $CB0E  ppuaddr_toggle (0 = first write = high byte, 1 = second = low)
;   $CB0F  ppuaddr_hi latch
;   $CB10  ppuaddr_lo latch
;   $CB11  ppudata read buffer
;   $CB12  synthetic sprite-0 phase (0 = before hit, 1 = hit reached)
;   $CB20  split-scroll flags: bit0 = after sprite-0 hit this frame,
;                              bit1 = pre-split scroll valid,
;                              bit2 = post-split scroll valid
;   $CB21  pre-split scroll X
;   $CB22  pre-split scroll Y
;   $CB23  post-split scroll X
;   $CB24  post-split scroll Y
;
; VBlank flag at $CB05 (set by irq_handler, cleared when $2002 is read).
;
; VRAM write buffer at $C800 (see vbuf.s).
; SAT staging area at $C900 (see sat.s).
;
; rt_ppu_write: entry A = value, B = register index (0..7).
; rt_ppu_write_cont: same, but returns by jumping to the continuation address
; stored at $D3FC/$D3FD instead of consuming a native return frame.
; rt_ppu_read:  entry B = register index (0..7). Returns A.

.section "ppu" free

; ─── rt_ppu_write ─────────────────────────────────────────────────────────────
; VDP critical section: translated code calls this from the main thread
; while the frame IRQ handler also writes VDP ports. rt_vdp_lock keeps the
; handler from interleaving with the control-port pairs / data bursts the
; register paths below may emit (see runtime/vdp.s).
rt_ppu_write:
  ex   af, af'              ; save value argument + caller flags without stack
  xor  a
  ld   ($d3fe), a            ; ordinary call path returns with `ret`
  jp   _rt_ppu_write_entry

rt_ppu_write_cont:
  ex   af, af'              ; save value argument + caller flags without stack
  ld   a, $01
  ld   ($d3fe), a            ; continuation path returns with `jp (hl)`
_rt_ppu_write_entry:
  ld   a, i                 ; P/V := IFF2 (1 = interrupts enabled)
  di                        ; atomic vs the frame IRQ handler from here
  jp   po, _pw_was_disabled
  ld   a, $01
  ld   ($cb19), a            ; _ppu_w_done must restore EI
  ex   af, af'              ; restore value/flags for _rt_ppu_write_body
  jp   _rt_ppu_write_body    ; tail-call: avoid one native stack frame
_pw_was_disabled:
  xor  a
  ld   ($cb19), a            ; leave interrupts off (handler/nested context)
  ex   af, af'              ; restore value/flags for _rt_ppu_write_body
  jp   _rt_ppu_write_body    ; tail-call: avoid one native stack frame

_rt_ppu_write_body:
  ; Hot PPU writes run under the frame IRQ/NMI bridge, where the native stack is
  ; at its tightest. Keep this body stackless: $CB18 holds the write value,
  ; $CB19 records whether rt_ppu_write must restore EI, $CB1E/$CB1F preserve
  ; resident translated X/Y (DE), and alternate AF owns the caller's A+F until
  ; _ppu_w_done. Runtime code must not use alternate AF elsewhere while inside
  ; this body.
  ld   ($cb18), a
  ld   ($cb1e), de
  ex   af, af'
  ; Dispatch on register index in B.
  ld   a, b
  cp   0
  jp   z, _ppu_w_ctrl
  cp   1
  jp   z, _ppu_w_mask
  cp   2
  jp   z, _ppu_w_status    ; writes to $2002 are ignored
  cp   3
  jp   z, _ppu_w_oamaddr
  cp   4
  jp   z, _ppu_w_oamdata
  cp   5
  jp   z, _ppu_w_scroll
  cp   6
  jp   z, _ppu_w_ppuaddr
  cp   7
  jp   z, _ppu_w_ppudata
  ; Unknown register — ignore.
  jp   _ppu_w_done

_ppu_w_ctrl:
  ; $2000 PPUCTRL: store to shadow and sync the SMS-side state it controls.
  ; Bits of interest for future phases:
  ;   bit 0-1: nametable select
  ;   bit 2:   VRAM addr increment (0=+1, 1=+32)
  ;   bit 3:   sprite pattern table (0=$0000, 1=$1000)
  ;   bit 4:   background pattern table
  ;   bit 5:   sprite size (0=8x8, 1=8x16)
  ;   bit 7:   NMI enable (we use frame INT, not NMI, so ignore)
  ld   a, ($cb18)
.ifdef NES_CHR_RAM
  ; Nametable-select change: the band (rows 0-3) must re-materialize
  ; from the newly selected page (see ntmap.s band rule).
  ld   c, a
  ld   a, ($cb08)
  xor  c
  and  $01
  jr   z, _pwc_no_flip
  ld   a, $01
  ld   ($cb78), a
_pwc_no_flip:
  ; BG pattern-table switch (bit 4): variant cache keys are per-table
  ; tile indices. Games toggle PPUCTRL constantly during uploads —
  ; flush LAZILY (flag $CB7F; the presentation flushes once per frame
  ; at most). Rendering is off during upload bursts, so a briefly
  ; stale variant can't be seen.
  ld   a, ($cb08)
  xor  c
  and  $10
  jr   z, _pwc_no_tflip
  ld   a, $01
  ld   ($cb7f), a
_pwc_no_tflip:
  ld   a, c
.endif
  ld   ($cb08), a
  ; Mirror NES PPUCTRL bit 3 into SMS VDP register 6. The current CHR pack
  ; places NES sprite table 1 in SMS slots $000-$0FF and sprite table 0 in
  ; slots $100-$1FF, so table switches must also switch the SMS sprite base.
  bit  3, a
  jr   nz, _ppu_sprite_base_0000
  ld   a, $ff                ; bit 2 = 1: sprite pattern base $2000
  jr   _ppu_sprite_base_set
_ppu_sprite_base_0000:
  ld   a, $fb                ; bit 2 = 0: sprite pattern base $0000
_ppu_sprite_base_set:
  ; Inline vdp_set_register in this hot PPUCTRL path: the helper's call frame
  ; and AF save can cross the native stack guard during nested NMI work.
  out  ($bf), a
  ld   a, $86                ; register-write command for VDP reg 6
  out  ($bf), a
  jp   _ppu_sync_vdp_reg1

_ppu_w_mask:
  ; $2001 PPUMASK: store to shadow and mirror rendering enable to SMS VDP
  ; register 1. NES bit 3 enables background and bit 4 enables sprites; the SMS
  ; has a single display-enable bit, so display is on when either NES plane is
  ; enabled and off when both are disabled. This keeps blanking generic instead
  ; of relying on boot's initial always-on display state.
  ld   a, ($cb18)
  ld   ($cb09), a
  jp   _ppu_sync_vdp_reg1

_ppu_sync_vdp_reg1:
  ; Compose SMS VDP register 1 from NES PPU shadows:
  ;   bit 7 = frame interrupt enable (kept on; SMS IRQ drives the NES NMI shim)
  ;   bit 6 = display enable (from PPUMASK bg/sprite enable bits)
  ;   bit 5 = Mode 4 / M1
  ;   bit 4 = 224-line mode
  ;   bit 1 = sprite size (from PPUCTRL bit 5: 0=8x8, 1=8x16)
  ld   a, %10110000          ; frame INT on, display off, M1 + 224-line mode
  ld   c, a
  ld   a, ($cb09)
  and  $18                   ; PPUMASK bg or sprite enable
  jr   z, _ppu_reg1_display_done
  ld   a, c
  or   $40
  ld   c, a
_ppu_reg1_display_done:
  ld   a, ($cb08)
  bit  5, a                  ; PPUCTRL sprite size: 8x16 when set
  jr   z, _ppu_reg1_sprite_done
  ld   a, c
  or   $02
  ld   c, a
_ppu_reg1_sprite_done:
  ; Defer the actual reg-1 write to the vblank-aligned presentation
  ; ($CB2D latch; 0 = no pending write — real values always have bit 7
  ; set). SMB's NMI toggles PPUMASK off during its VRAM update and back
  ; on after; under frame overrun that pair lands mid-display and
  ; blanked a band of scanlines (the alternating black-band flicker).
  ; Applied once per frame in vblank, the off/on pair collapses.
  ld   a, c
  ld   ($cb2d), a
  jp   _ppu_w_done

_ppu_w_status:
  ; $2002 is read-only; writes are ignored on real hardware.
  jp   _ppu_w_done

_ppu_w_oamaddr:
  ; $2003 OAMADDR: set OAM byte address.
  ld   a, ($cb18)
  ld   ($cb0a), a
  jp   _ppu_w_done

_ppu_w_oamdata:
  ; $2004 OAMDATA: write one byte to OAM staging at $C900 + oam_addr.
  ;   Auto-increment oam_addr after each write.
  ld   a, ($cb0a)           ; current OAM address
  ld   l, a
  ld   h, $c9               ; $C900 + oam_addr
  ld   a, ($cb18)
  ld   (hl), a              ; write value
  ; Increment OAM addr (wraps at 256 within the page).
  ld   a, ($cb0a)
  inc  a
  ld   ($cb0a), a
  jp   _ppu_w_done

_ppu_w_scroll:
  ; $2005 PPUSCROLL: double-write.
  ;   First write  (toggle=0): X scroll → $CB0C; toggle becomes 1.
  ;   Second write (toggle=1): Y scroll → $CB0D; toggle becomes 0.
  ; A complete pair is also captured into the split-scroll scheduler. Pairs
  ; before PPUSTATUS returns sprite-0 hit are pre-split; pairs after are
  ; post-split. The last complete pair in each phase wins.
  ld   a, ($cb0b)           ; scroll toggle
  or   a
  jr   nz, _ppu_w_scroll_y
  ; First write = X scroll.
  ld   a, ($cb18)
  ld   ($cb0c), a
  ld   a, 1
  ld   ($cb0b), a
  jp   _ppu_w_done
_ppu_w_scroll_y:
  ; Second write = Y scroll.
  ld   a, ($cb18)
  ld   ($cb0d), a
  ld   a, ($cb20)
  bit  0, a
  jr   nz, _ppu_w_scroll_capture_post
  ld   a, ($cb0c)
  ld   ($cb21), a
  ld   a, ($cb0d)
  ld   ($cb22), a
  ld   hl, $cb20
  set  1, (hl)
  jr   _ppu_w_scroll_capture_done
_ppu_w_scroll_capture_post:
  ld   a, ($cb0c)
  ld   ($cb23), a
  ld   a, ($cb0d)
  ld   ($cb24), a
  ld   hl, $cb20
  set  2, (hl)
_ppu_w_scroll_capture_done:
  xor  a
  ld   ($cb0b), a
  jp   _ppu_w_done

_ppu_w_ppuaddr:
  ; $2006 PPUADDR: double-write.
  ;   First write  (toggle=0): high byte → $CB0F; toggle becomes 1.
  ;   Second write (toggle=1): low byte  → $CB10; toggle becomes 0.
  ld   a, ($cb0e)
  or   a
  jr   nz, _ppu_w_ppuaddr_lo
  ld   a, ($cb18)
  ld   ($cb0f), a           ; high byte
  ld   a, 1
  ld   ($cb0e), a
  jp   _ppu_w_done
_ppu_w_ppuaddr_lo:
  ld   a, ($cb18)
  ld   ($cb10), a           ; low byte
  xor  a
  ld   ($cb0e), a
  jp   _ppu_w_done

_ppu_w_ppudata:
  ; $2007 PPUDATA: write A at the current NES VRAM address.
  ;   Current VRAM address is formed from ($CB0F << 8) | $CB10.
  ;   Auto-increment: +1 if ppu_ctrl bit 2 = 0, +32 if bit 2 = 1.
  ; For nametable bytes, write directly to the SMS nametable. The older
  ; one-byte queued-buffer header wraps on SMB's 1 KiB title-screen clears
  ; (4 bytes of record metadata per NES byte), so direct writes are the safer
  ; first-pass behavior while translated code runs during VBlank.
  ld   a, ($cb0f)
  ld   d, a
  ld   a, ($cb10)
  ld   e, a
  ld   a, d
  cp   $3f
  jp   z, _ppudata_direct_palette
  ld   a, d
  cp   $20
  jp   c, _ppudata_pattern_write
  cp   $30
  jp   nc, _ppudata_discard_direct
  and  $03
  cp   $03
  jp   nz, _ppudata_direct_nametable_tile
  ld   a, e
  cp   $c0
  jp   nc, _ppudata_direct_attribute

_ppudata_direct_nametable_tile:
  ; Window routing (E.5c): raw-store every tile write; only in-window
  ; columns reach the folded VRAM table (out-of-window columns are
  ; projected later when they scroll in). Preserves DE.
  ld   a, ($cb18)
  ; Inline rt_nt_route_tile_write in this hottest nested-NMI path. The helper
  ; has no other runtime callers, and its call frame alone can cross the
  ; native stack guard while SMB clears/materializes nametables.
  ld   c, a

.ifdef NES_MIRRORING_VERTICAL
  ld   a, d
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

.ifdef NES_MIRRORING_HORIZONTAL
  ld   a, d
  and  $03                   ; raw low 1 KiB offset high bits
  ld   h, a
  ld   a, d
  and  $08                   ; raw bit 11 selects CIRAM page 1
  srl  a                     ; move bit 3 -> bit 2
  or   h
  add  a, (RAW_CIRAM_SRAM_BASE >> 8)
  ld   h, a
  ld   l, e
.endif

  ld   a, RAW_CIRAM_SRAM_CTRL
  ld   ($fffc), a
  ld   (hl), c
  xor  a
  ld   ($fffc), a
  ; row = ((D & 3) << 3) | (E >> 5); rows 0-3 (status region) always render.
  ld   a, e
  rlca
  rlca
  rlca
  and  $07
  ld   b, a
  ld   a, d
  and  $03
  rlca
  rlca
  rlca
  or   b
  cp   4
  jr   nc, _ppudata_tile_col_check
.ifdef NES_CHR_RAM
  ; Rows 0-3 are the status-bar band. Writes to the currently-selected page
  ; render immediately; writes to the other page are raw-only.
  ld   a, ($cb08)
  and  $01
  rlca
  rlca                      ; select bit 0 -> bit 2 ($2400 bit)
  ld   b, a
  ld   a, d
  and  $04
  cp   b
  jr   z, _ppudata_tile_in
  jp   _ppudata_discard_direct
.else
  ; SMB-proven band rule: NT-A rows 0-3 render (fixed HUD); NT-B column tops
  ; raw-store only.
  ld   a, d
  and  $04
  jr   z, _ppudata_tile_in
  jp   _ppudata_discard_direct
.endif
_ppudata_tile_col_check:
  ; column = (D bit2) * 32 | (E & $1F)   (vertical mirroring: $24xx = page 1)
  ld   a, d
  and  $04
  rlca
  rlca
  rlca                      ; bit 2 -> bit 5 (= 32)
  ld   b, a
  ld   a, e
  and  $1f
  or   b
  ld   hl, $cb2a
  sub  (hl)
  and  $3f
  cp   32
  jp   nc, _ppudata_discard_direct
_ppudata_tile_in:
  ; DE = NES nametable byte. Convert `(DE - $2000) & $03FF` to
  ; SMS `$3700 + offset * 2` (224-line-mode name table base), write tile
  ; low byte and clear attrs.
  ld   a, d
  and  $03
  ld   d, a
  sla  e
  rl   d
  ld   a, d
  add  a, $37
  ld   d, a
  ld   a, e
  out  ($bf), a
  ld   a, d
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, ($cb18)
  call rt_write_mapped_bg_tile
  jp   _ppudata_inc_addr

_ppudata_pattern_write:
.ifndef NES_CHR_RAM
  ; CHR-ROM carts never write pattern space meaningfully — discard.
  jp   _ppudata_discard_direct
.else
  ; CHR-RAM (mapper plan M1, SRAM-mirror design): a $2007 write into
  ; pattern space ($0000-$1FFF).
  ;  1. Raw 2bpp byte -> cartridge-SRAM CHR mirror (CHR_RAM_SRAM_BASE
  ;     + addr): the SOURCE for BG variant generation. No VRAM budget,
  ;     no VDP port cost.
  ;  2. Invalidate the tile's cached variants (FC[tile & $FF]) so the
  ;     next NT reference regenerates from the new bytes.
  ;  3. If the tile is in the SPRITE pattern table (PPUCTRL bit 3),
  ;     copy-through to the fixed VRAM sprite region ($2000 + fold).
  ld   a, ($cb18)            ; the data byte
  ld   ($cb13), a            ; park it (transient scratch)
  push bc
  push de
  push hl
  ; --- 1. SRAM mirror store ---
  ld   hl, CHR_RAM_SRAM_BASE
  add  hl, de
  call rt_raw_ciram_sram_enable
  ld   a, ($cb13)
  ld   (hl), a
  call rt_raw_ciram_sram_disable
  ; --- 2. FC invalidation: base = ((D:E) >> 4) & $FF ---
  ld   a, e
  rrca
  rrca
  rrca
  rrca
  and  $0f
  ld   c, a
  ld   a, d
  rlca
  rlca
  rlca
  rlca
  and  $f0
  or   c
  ld   l, a
  ld   h, $00
  add  hl, hl
  add  hl, hl                ; *4 (S variants per base)
  ld   bc, $d600            ; BGV_CACHE (chrmap.s)
  add  hl, bc
  ld   a, $ff
  ld   (hl), a
  inc  hl
  ld   (hl), a
  inc  hl
  ld   (hl), a
  inc  hl
  ld   (hl), a
  ; --- 3. sprite copy-through when this write's table is the sprite table ---
  ld   a, ($cb08)
  and  $08                   ; PPUCTRL bit 3
  rlca                       ; -> $10 (the table bit of the address)
  ld   c, a
  ld   a, d
  and  $10
  cp   c
  jr   nz, _ppw_done
  ; SMS sprite addr = $2000 + ((nes & $0FF0) << 1 | (nes & 7) << 2 | (nes >> 3) & 1)
  ld   a, e
  and  $07
  add  a, a
  add  a, a
  ld   c, a
  ld   a, e
  rrca
  rrca
  rrca
  and  $01
  or   c
  ld   c, a
  ld   a, e
  and  $f0
  ld   l, a
  ld   a, d
  and  $0f
  ld   h, a
  add  hl, hl
  ld   a, l
  or   c
  ld   l, a
  ld   a, h
  or   $20
  ld   h, a
  ld   a, l
  out  ($bf), a
  ld   a, h
  and  $3f
  or   $40
  out  ($bf), a
  ld   a, ($cb13)
  out  ($be), a
_ppw_done:
  pop  hl
  pop  de
  pop  bc
  jp   _ppudata_inc_addr
.endif

_ppudata_discard_direct:
  ; non-nametable PPU writes are ignored for now
  jp   _ppudata_inc_addr

_ppudata_direct_palette:
  ; NES palette RAM lives at $3F00-$3F1F mirrored through $3FFF. Convert the
  ; NES palette index being written to SMS CRAM's --BBGGRR byte format and
  ; write it directly to the corresponding CRAM entry. This lets games load
  ; their own palettes through ordinary $2006/$2007 traffic instead of being
  ; stuck with the boot placeholder palette.
  ld   a, ($cb18)            ; A = NES palette colour index

  ; P = NES palette index (low 5 bits). Read it from E (PPUADDR low) BEFORE the
  ; LUT lookup clobbers DE. Background tiles now bake the sub-palette into their
  ; pixels and always use SMS palette 0 (CRAM 0-15 = the four NES bg
  ; sub-palettes), so the colour map is direct:
  ;   P = 0 or $10  -> universal background: write CRAM 0,4,8,12 (every bg
  ;                    sub-palette's colour 0, which the NES reads from $3F00).
  ;   P in 4/8/$C/$14/$18/$1C -> NES colour-0 mirrors; skip (kept = sky).
  ;   else          -> CRAM[P] directly (bg colours 1-3 of each sub-palette;
  ;                    sprite colours in 16-31, whose colour 0 is transparent).
  ld   b, a                  ; B = NES colour index (save across LUT)
  ld   a, e
  and  $1f
  ld   ($cb16), a            ; P (stashed; B is needed for the colour index)

  ; SMS colour from the NES master-palette LUT -> C.
  ld   a, b
  and  $3f
  ld   l, a
  ld   h, $00
  ld   de, _nes_to_sms_palette
  add  hl, de
  ld   a, (hl)
  ld   c, a                  ; C = SMS --BBGGRR colour

  ld   a, ($cb16)
  ld   b, a                  ; B = P
  cp   $00
  jr   z, _pal_universal
  cp   $10
  jr   z, _pal_universal
  cp   $04
  jp   z, _ppudata_inc_addr
  cp   $08
  jp   z, _ppudata_inc_addr
  cp   $0c
  jp   z, _ppudata_inc_addr
  cp   $14
  jp   z, _ppudata_inc_addr
  cp   $18
  jp   z, _ppudata_inc_addr
  cp   $1c
  jp   z, _ppudata_inc_addr
  ; normal entry: CRAM[P] = C
  ld   a, b
  call vdp_set_cram_addr
  ld   a, c
  out  ($be), a
  jp   _ppudata_inc_addr

_pal_universal:
  xor  a
  call vdp_set_cram_addr
  ld   a, c
  out  ($be), a
  ld   a, $04
  call vdp_set_cram_addr
  ld   a, c
  out  ($be), a
  ld   a, $08
  call vdp_set_cram_addr
  ld   a, c
  out  ($be), a
  ld   a, $0c
  call vdp_set_cram_addr
  ld   a, c
  out  ($be), a
  jp   _ppudata_inc_addr

_ppudata_direct_attribute:
  ; Attribute byte at $23C0/$27C0/$2BC0/$2FC0. Store to the raw attr shadow,
  ; then expand through the window-gated core below.
  ld   a, ($cb18)
  ld   ($cb15), a            ; attr byte
  call rt_nt_write_attr_shadow ; preserves DE; current folded rendering unchanged
  call _apply_attr_core
  jp   _ppudata_inc_addr

; ─── rt_apply_attr_byte ──────────────────────────────────────────────────────
; Apply one NES attribute byte to the folded window (window-gated).
; Entry: A = attribute byte, DE = NES attribute address ($23C0-$2FFF).
; Used by the ppudata path above and by the scroll projector (ntmap.s),
; which re-applies an entering column's attributes from the raw shadow.
rt_apply_attr_byte:
  ld   ($cb15), a
_apply_attr_core:
  ; Expand the four 2-bit palette selectors to SMS palette bits across the
  ; covered 4x4 tile area, preserving each tile's CHR remap high bit through
  ; the nametable shadow. Each quadrant (2 columns wide) is skipped when its
  ; columns lie outside the projected window ($CB2A): rewriting those fold
  ; slots would corrupt the columns currently displayed there (E.5c).
  ld   a, d
  and  $04
  ld   ($cb16), a            ; NT page flag ($04 = second nametable)
  ld   a, e
  sub  $c0
  ld   ($cb19), a            ; attr offset 0..63

  ; Top-left quadrant: bits 0-1, base + 0.
  call rt_attr_base_tl
  call _attr_quad_in_window
  jr   nc, _attr_q_tr
  ld   a, ($cb15)
  and  $03
  call rt_write_bg_attr_quadrant

_attr_q_tr:
  ; Top-right quadrant: bits 2-3, base + 4 bytes (2 tiles).
  call rt_attr_base_tl
  ld   a, e
  add  a, 4
  ld   e, a
  call _attr_quad_in_window
  jr   nc, _attr_q_bl
  ld   a, ($cb15)
  srl  a
  srl  a
  and  $03
  call rt_write_bg_attr_quadrant

_attr_q_bl:
  ; Bottom-left quadrant: bits 4-5, base + 128 bytes (2 rows).
  call rt_attr_base_tl
  ld   a, e
  add  a, $80
  ld   e, a
  call _attr_quad_in_window
  jr   nc, _attr_q_br
  ld   a, ($cb15)
  srl  a
  srl  a
  srl  a
  srl  a
  and  $03
  call rt_write_bg_attr_quadrant

_attr_q_br:
  ; Bottom-right quadrant: bits 6-7, base + 132 bytes (2 rows + 2 tiles).
  call rt_attr_base_tl
  ld   a, e
  add  a, $84
  ld   e, a
  call _attr_quad_in_window
  jr   nc, _attr_q_done
  ld   a, ($cb15)
  srl  a
  srl  a
  srl  a
  srl  a
  srl  a
  srl  a
  and  $03
  call rt_write_bg_attr_quadrant
_attr_q_done:
  ret

; _attr_quad_in_window — carry SET when the quadrant's 2-column pair lies
; inside the projected window. Entry: DE = folded SMS nametable low-byte
; address of the quadrant's top-left cell. Preserves DE. Clobbers AF, HL.
_attr_quad_in_window:
  ; HUD band (fold rows 0-3 <=> fold high byte $37): NT-A attr quadrants
  ; always apply (the split keeps those rows on screen); NT-B ones never
  ; do (their fold slots display the HUD).
  ld   a, d
  cp   $38
  jr   nc, _aqw_windowed
  ld   a, ($cb16)
  or   a
  jr   z, _aqw_yes
  or   a                     ; carry clear: NT-B over the HUD band
  ret
_aqw_windowed:
  ld   a, e
  rrca                       ; fold col = (E >> 1) & $1F
  and  $1f
  ld   l, a
  ld   a, ($cb16)
  rlca
  rlca
  rlca                       ; $04 -> $20
  or   l                     ; world column (0-63)
  ld   hl, $cb2a
  sub  (hl)
  and  $3f
  cp   31                    ; both columns (c, c+1) must fit
  ret
_aqw_yes:
  scf
  ret

_ppudata_inc_addr:
  ; Auto-increment VRAM address.
  ld   a, ($cb08)           ; ppu_ctrl
  bit  2, a
  jr   z, _ppudata_inc1
  ; Increment by 32 (down the column).
  ld   a, ($cb10)
  add  a, 32
  ld   ($cb10), a
  jr   nc, _ppudata_inc_done
  ld   a, ($cb0f)
  inc  a
  ld   ($cb0f), a
  jr   _ppudata_inc_done
_ppudata_inc1:
  ; Increment by 1 (across the row).
  ld   a, ($cb10)
  inc  a
  ld   ($cb10), a
  jr   nz, _ppudata_inc_done
  ld   a, ($cb0f)
  inc  a
  ld   ($cb0f), a
_ppudata_inc_done:
  jp   _ppu_w_done

_ppu_w_done:
  ld   de, ($cb1e)
  ld   a, ($cb19)
  or   a
  jr   z, _ppu_w_maybe_return_disabled
  ld   a, ($d3fe)
  or   a
  jr   nz, _ppu_w_return_cont_ei
  ex   af, af'              ; restore caller A+flags
  ei
  ret
_ppu_w_return_cont_ei:
  ld   hl, ($d3fc)
  ex   af, af'              ; restore caller A+flags
  ei
  jp   (hl)
_ppu_w_maybe_return_disabled:
  ld   a, ($d3fe)
  or   a
  jr   nz, _ppu_w_return_cont_disabled
  ex   af, af'              ; restore caller A+flags
  ret
_ppu_w_return_cont_disabled:
  ld   hl, ($d3fc)
  ex   af, af'              ; restore caller A+flags
  jp   (hl)

; ─── rt_ppu_read ──────────────────────────────────────────────────────────────
; Entry: B = register index (0..7).
; Exit:  A = value.
rt_ppu_read:
  ; Reads overwrite A/NZ; no need to preserve caller AF while probing IFF2.
  ld   a, i                 ; P/V := IFF2
  di
  jp   po, _pr_was_disabled
  ld   a, $01
  ld   ($cb19), a            ; _ppu_r_done must restore EI
  jp   _rt_ppu_read_body     ; tail-call: avoid one native stack frame
_pr_was_disabled:
  xor  a
  ld   ($cb19), a            ; leave interrupts off (handler/nested context)
  jp   _rt_ppu_read_body     ; tail-call: avoid one native stack frame
_rt_ppu_read_body:
  ; Read helpers return A and may clobber BC/HL/native flags, but DE holds
  ; resident translated X/Y and must survive. Keep the hot read body stackless.
  ; $CB19 records whether the wrapper observed interrupts enabled and should
  ; re-enable them on return.
  ld   ($cb1e), de
  ld   a, b
  cp   2
  jp   z, _ppu_r_status
  cp   4
  jp   z, _ppu_r_oamdata
  cp   7
  jp   z, _ppu_r_ppudata
  ; All other registers: return 0.
  xor  a
  jp   _ppu_r_done

_ppu_r_status:
  ; $2002 PPUSTATUS:
  ;   bit 7 = VBlank flag
  ;   bit 6 = sprite-0 hit (coarse synthetic timing)
  ;   bit 5 = sprite overflow (always 0 for v1)
  ;   bits 4..0 = open bus (0)
  ; Reading $2002 clears the VBlank flag and resets the $2005/$2006 toggles.
  ;
  ; SMB uses sprite 0 as a split-screen timing barrier: wait for bit 6 to
  ; clear, then wait for it to set. We do not emulate scanlines yet, so each
  ; frame starts with $CB12=0 and the first status poll while rendering is
  ; enabled returns no sprite-0 hit and arms the synthetic hit for subsequent
  ; polls. This preserves the clear-then-hit handshake without SMB-specific
  ; Rust or hard-coded translated labels.
  ld   a, ($cb05)           ; VBlank pending flag
  ld   c, a
  ; Clear VBlank flag.
  xor  a
  ld   ($cb05), a
  ; Reset scroll and VRAM address toggles.
  ld   ($cb0b), a
  ld   ($cb0e), a

  ; Start result in C: $80 if VBlank was pending, else $00.
  ld   a, c
  or   a
  jr   z, _ppu_r_status_no_vbl
  ld   c, $80
  jr   _ppu_r_status_sprite0
_ppu_r_status_no_vbl:
  ld   c, $00

_ppu_r_status_sprite0:
  ; Only report sprite-0 hit while NES background/sprite rendering is enabled
  ; (PPUMASK bits 3 or 4). When disabled, keep the synthetic phase clear.
  ld   a, ($cb09)
  and  $18
  jr   z, _ppu_r_status_return
  ld   a, ($cb12)
  or   a
  jr   z, _ppu_r_status_arm_sprite0
  ld   hl, $cb20
  set  0, (hl)                ; subsequent $2005 pairs are post-sprite-0 hit
  ld   a, c
  or   $40
  jp   _ppu_r_done
_ppu_r_status_arm_sprite0:
  ld   a, $01
  ld   ($cb12), a

_ppu_r_status_return:
  ld   a, c
  jp   _ppu_r_done

_ppu_r_oamdata:
  ; $2004 OAMDATA read: return byte from SAT staging at oam_addr; auto-increment.
  ld   a, ($cb0a)
  ld   l, a
  ld   h, $c9
  ld   c, (hl)              ; read OAM byte
  ld   a, ($cb0a)
  inc  a
  ld   ($cb0a), a
  ld   a, c
  jp   _ppu_r_done

_ppu_r_ppudata:
  ; $2007 PPUDATA read. For pattern-table addresses ($0000-$1FFF), emulate
  ; the NES read buffer: return the previously buffered byte, then load the
  ; buffer from raw CHR at the current PPU address and increment the address.
  ; This is enough for SMB's DrawTitleScreen, which performs the required
  ; dummy read after setting PPUADDR to $1EC0.
  ld   a, ($cb11)
  ld   c, a                  ; C = value to return after refill/increment

  ld   a, ($cb0f)
  cp   $20
  jp   nc, _ppu_r_ppudata_non_chr

  ; Map raw NES CHR into slot 2, read $8000 + PPUADDR, then restore slot 2 to
  ; the lower PRG data window expected by translated table reads.
  ld   a, :data_chr_nes
  ld   ($ffff), a
  ld   a, ($cb0f)
  add  a, $80
  ld   h, a
  ld   a, ($cb10)
  ld   l, a
  ld   a, (hl)
  ld   ($cb11), a
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  call _ppu_r_inc_addr
  ld   a, c
  jp   _ppu_r_done

_ppu_r_ppudata_non_chr:
  ; Name-table/palette readback is not needed for SMB's current boot path.
  ; Preserve buffered-read timing by replacing the buffer with zero.
  xor  a
  ld   ($cb11), a
  call _ppu_r_inc_addr
  ld   a, c
  jp   _ppu_r_done

_ppu_r_inc_addr:
  ; Auto-increment current PPU address for PPUDATA reads, mirroring writes:
  ; +1 by default, +32 when PPUCTRL bit 2 is set. Clobbers A only.
  ld   a, ($cb08)
  bit  2, a
  jr   z, _ppu_r_inc1
  ld   a, ($cb10)
  add  a, 32
  ld   ($cb10), a
  ret  nc
  ld   a, ($cb0f)
  inc  a
  ld   ($cb0f), a
  ret
_ppu_r_inc1:
  ld   a, ($cb10)
  inc  a
  ld   ($cb10), a
  ret  nz
  ld   a, ($cb0f)
  inc  a
  ld   ($cb0f), a
  ret

_ppu_r_done:
  ld   de, ($cb1e)
  ld   c, a
  ld   a, ($cb19)
  or   a
  ld   a, c
  jr   z, _ppu_r_done_no_ei
  ei
_ppu_r_done_no_ei:
  ret

_nes_to_sms_palette:
  ; Same coarse NES-master-palette -> SMS --BBGGRR mapping used by the Rust
  ; asset path. Indexed by NES palette byte & $3F.
  .db $15, $10, $20, $20, $11, $01, $01, $00, $00, $00, $04, $00, $00, $00, $00, $00
  .db $2A, $34, $30, $31, $22, $12, $02, $01, $05, $04, $04, $04, $14, $00, $00, $00
  .db $3F, $39, $35, $36, $37, $27, $17, $0B, $0A, $0D, $0D, $1C, $38, $00, $00, $00
  .db $3F, $3E, $3A, $3B, $3B, $3B, $2B, $2F, $1F, $1E, $2E, $2E, $3E, $2A, $00, $00

.ends
