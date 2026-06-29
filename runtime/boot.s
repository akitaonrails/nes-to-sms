; boot.s — SMS cartridge entry points and boot sequence.
;
; Lands at $0000 (reset), provides $0038 (mode 1 IRQ) and $0066 (NMI/pause).
; After hardware init, jumps to translated_reset (emitted by the Rust lower
; crate as the target-ROM's reset vector entry point).
;
; SMS RAM layout (all routines refer to this):
;   $C000-$C0FF  NES zero-page mirror
;   $C100-$C1FF  Emulated 6502 stack page
;   $C200-$C7FF  NES RAM mirror ($0200-$07FF)
;   $C800-$C8FF  VRAM update buffer
;   $C900-$C9FF  Sprite attribute staging (Y at $C900, X/tile at $C940)
;   $CC00-$D2FF  SMS visible nametable high-byte shadow ($3700-$3DFF + $9500)
;   $D300-$D3FF  Clean metadata reserve: 240 tile-dirty bits + 16 attr bits
;   $CB00        Shadow X
;   $CB01        Shadow Y
;   $CB02        Shadow S (init $FD)
;   $CB03        Shadow P (NV-BDIZC, init $24)
;   $CB04        Frame counter (low byte)
;   $CB05        VBlank pending flag
;   $CB06        Latched controller state (port 1)
;   $CB07        Controller bit-read index
;   $CB08        PPU ctrl shadow ($2000)
;   $CB09        PPU mask shadow ($2001)
;   $CB0A        OAM address ($2003 latch)
;   $CB0B        Scroll write toggle (0=first, 1=second)
;   $CB0C        Scroll X latch
;   $CB0D        Scroll Y latch
;   $CB0E        VRAM addr write toggle (0=first, 1=second)
;   $CB0F        VRAM addr high latch ($2006 first write)
;   $CB10        VRAM addr low latch ($2006 second write)
;   $CB11        PPUDATA read buffer
;   $CB12        Synthetic sprite-0 phase for PPUSTATUS bit 6
;   $CB1A        Translated NMI has been enabled at least once
;   $CB20-$CB24  Split-scroll scheduler state (see runtime/ppu.s)
;   $CB80-$CBFF  Raw mirrored NES attribute shadow (2 CIRAM pages × 64 bytes)
;   $CB13-$CB1F  13-byte scratch ("temp w")
;   $CB1D        Runtime trap marker for trace-sms diagnostics
;   Z80 SP lives at $DFFE, grows down — never touches $C100-$C1FF.

.define VDP_R0_BASE            $66   ; Mode 4 + top-row hscroll lock + left blank
.define VDP_R0_LINE_IRQ_ON     $76   ; VDP_R0_BASE + IE1 line IRQ enable

.bank 0 slot 0
.org $0000

reset_entry:
  di
  im 1
  ld sp, $dffe
  jp boot_main

; Padding bytes between $0003 and $0008 are handled by the linker filling
; with $FF (ROM erased value).  WLA-DX will fill the gap automatically.

.org $0008
  ; RST 1 — unused for v1.  Trap immediately so bugs surface.
  di
  halt

.org $0010
  ; RST 2 — unused.
  di
  halt

.org $0018
  ; RST 3 — unused.
  di
  halt

.org $0020
  ; RST 4 — unused.
  di
  halt

.org $0028
  ; RST 5 — unused.
  di
  halt

.org $0030
  ; RST 6 — unused.
  di
  halt

.org $0038
  jp irq_handler

.org $0066
  ; NMI = SMS pause button.  Ignored for v1; RETN returns to the
  ; interrupted context without servicing.
  retn

; ─── boot_main ────────────────────────────────────────────────────────────────
.org $0068

.section "boot_main" free

boot_main:
  ; 1. Init VDP to Mode 4 defaults.
  call vdp_init

  ; 2. Clear all 16 KB of VRAM.
  call vdp_clear_vram

  ; 3. Clear CRAM (32 bytes palette RAM).
  call vdp_clear_cram

  ; 4. Load static palette from data_palette (32 bytes) into CRAM at addr 0.
  ;    Asset blobs are pinned to dedicated banks in slot 2 by sms.asm, so
  ;    each symbol is already a $8000-range logical address. We just need
  ;    to map the right bank into slot 2 before reading.
  ld  a, $00
  call vdp_set_cram_addr
  ld  a, :data_palette
  ld  ($ffff), a
  ld  hl, data_palette
  ld  bc, $0020
  call vdp_write_block

  ; 5. Load CHR tiles from data_chr into VRAM at $0000.
  ;    VRAM Mode 4 layout (224-line mode): $0000-$36FF tile patterns
  ;    (~13.75 KiB), $3700-$3EFF name table (2 KiB, 32x32), $3F00-$3FFF
  ;    SAT. SMB converted CHR is 16 KiB; clamp the upload to $3700 bytes.
  ld  a, $00
  ld  d, $00
  call vdp_set_vram_addr
  ld  a, :data_chr
  ld  ($ffff), a
  ld  hl, data_chr
  ld  bc, $3700
  call vdp_write_block

  ; 6. Load nametable from data_nametable into VRAM at $3700 (224-line
  ;    mode base = (R2 & $0C)<<10 | $700, R2=$FF -> $3700).
  ;    32*28*2 = 1792 ($0700) bytes covers the visible rows 0-27.
  ld  a, $00
  ld  d, $37
  call vdp_set_vram_addr
  ld  a, :data_nametable
  ld  ($ffff), a
  ld  hl, data_nametable
  ld  bc, $0700
  call vdp_write_block

  ; Keep the lower NES PRG window mapped in slot 2 for translated data-table
  ; reads ($8000-$BFFF). The startup asset uploads above temporarily map CHR,
  ; palette, and nametable banks here; translated code expects SMB tables such
  ; as $805A/$806D/$8080 to be readable at their original addresses.
  ld  a, :data_prg_low
  ld  ($ffff), a

  ; 7. Init emulated 6502 CPU state.
  xor a
  ld  ($cb00), a            ; Shadow X = 0
  ld  ($cb01), a            ; Shadow Y = 0
  ld  a, $fd
  ld  ($cb02), a            ; Shadow S = $FD (6502 reset convention)
  ld  a, $24
  ld  ($cb03), a            ; Shadow P = $24  (I=1, unused=1)

  ; 8. Init controller state.
  xor a
  ld  ($cb06), a            ; latched controller state = all released
  ld  ($cb07), a            ; bit-read index = 0

  ; 9. Init PPU shadow registers.
  xor a
  ld  ($cb08), a            ; ppu_ctrl = 0
  ld  ($cb09), a            ; ppu_mask = 0
  ld  ($cb0a), a            ; OAM addr = 0
  ld  ($cb0b), a            ; scroll write toggle = 0
  ld  ($cb0c), a            ; scroll X = 0
  ld  ($cb0d), a            ; scroll Y = 0
  ld  ($cb0e), a            ; VRAM addr toggle = 0
  ld  ($cb0f), a            ; VRAM addr high = 0
  ld  ($cb10), a            ; VRAM addr low = 0
  ld  ($cb11), a            ; PPUDATA read buffer = 0
  ld  ($cb12), a            ; sprite-0 phase = clear/not-yet-hit
  ld  ($cb1a), a            ; translated NMI not enabled yet

  ; 9b. Init split-scroll scheduler state.
  ld  hl, $cb20
  ld  bc, $0005
  xor a
  call mem_fill

  ; 10. Clear the VRAM update buffer.
  ld  hl, $c800
  ld  bc, $0100
  xor a
  call mem_fill

  ; 11. Clear sprite staging area.
  ld  hl, $c900
  ld  bc, $0100
  ld  a, $d0                ; Y=$D0 hides sprites below the visible area
  call mem_fill

  ; 11b. Clear raw mirrored NES attribute shadow (2 CIRAM pages × 64 bytes).
  ; This is scaffolding for later mirroring-aware materialization; current
  ; folded rendering still uses the SMS nametable high-byte shadow below.
  ld  hl, $cb80
  ld  bc, $0080
  xor a
  call mem_fill

  ; 11c. Clear SMS nametable high-byte shadow. Runtime attribute writes update
  ; this shadow so they can preserve profile-mapped CHR tile high bits without
  ; reading back from buffered VDP VRAM.
  ld  hl, $cc00
  ld  bc, $0700
  xor a
  call mem_fill

  ; 11d. Build the horizontal-flip byte LUT (software sprite flipping; the SMS
  ; VDP has no per-sprite flip bit). See runtime/sat.s.
  call rt_build_hflip_lut

  ; 11e. Init the background sub-palette variant cache to "unassigned" ($FF)
  ; and reset the variant pool allocator. See runtime/chrmap.s.
  ld  hl, $d600
  ld  bc, $0400             ; 1024 cache entries
  ld  a, $ff
  call mem_fill
  ld  hl, $da00             ; per-cell base-slot shadow
  ld  bc, $0380             ; 896 cells
  xor a
  call mem_fill
  xor a
  ld  ($ca00), a            ; bg variant pool next-free slot = 0
  ld  ($ca07), a            ; bg variant ring has not wrapped yet

  ; 12. Enable display and frame interrupts (VDP reg 1).
  ;     %11110000: display on, frame INT enabled, M1=1 (224-line mode),
  ;     8×8 sprites. 224 lines (28 tile rows) vs 192 so the NES 30-row
  ;     playfield's lower rows (e.g. the ground) aren't clipped.
  ld  a, %11110000
  ld  b, 1
  call vdp_set_register
  ; NOTE: do NOT `ei` here. If an IRQ fires between this point and the
  ; bank switch below, irq_handler would `call L_8082` into the wrong
  ; bank's $40FB and crash. The translated reset itself opens with
  ; `SEI` (which we lower to a shadow-P bit set, not a Z80 `di`), so
  ; the IFF state is meaningful only inside translated code. We `ei`
  ; AFTER the bank switch, then the jump immediately enters translated
  ; code which can decide when to enable the frame interrupt.

  ; 13. Map the translated_reset bank into slot 1 and jump to $4000.
  ;     translated_reset lives in a superfree section, so the symbol
  ;     value is the in-bank offset ($0000) — `jp translated_reset`
  ;     would resolve to `jp $0000` (= back to reset_entry). Instead,
  ;     bank-switch slot 1 to the right bank and jump to $4000.
  ld   a, :translated_reset
  ld   ($fffe), a            ; map slot 1 ($4000-$7FFF) to this bank
  ld   ($cb14), a            ; mirror in bank shadow for rt_far_call
  ei                          ; now safe: slot 1 has translated code
  jp   $4000                  ; logical slot-1 address of translated_reset

.ends

; ─── irq_handler ──────────────────────────────────────────────────────────────
.section "irq_handler" free

irq_handler:
  push af
  push hl
  push bc
  push de

  ; Acknowledge the VDP interrupt by reading the status port. Frame and line
  ; interrupts share the Z80 IM1 vector; status bit 7 identifies frame IRQs.
  ; Bit 7 clear means a non-frame VDP IRQ; the only one we enable is the
  ; one-shot line split below.
  in  a, ($bf)
  bit 7, a
  jp  z, _irq_line_scroll_split

  ; Latch controller state before any translated code reads it.
  call rt_controller_latch

  ; Increment frame counter (wraps at 256, sufficient for v1 timing).
  ld  a, ($cb04)
  inc a
  ld  ($cb04), a

  ; Signal "VBlank pending" to translated code that polls $2002.
  ld  a, $01
  ld  ($cb05), a

  ; Start each translated NMI before the approximated sprite-0 hit point.
  ; SMB first waits for PPUSTATUS bit 6 to clear, then waits for it to set.
  ; The status reader advances this phase on polling so those barriers can
  ; complete without scanline-level NES PPU emulation.
  xor a
  ld  ($cb12), a
  ld  ($cb20), a            ; clear per-frame split flags; keep last latches

  ; NOTE: the VDP flushes (SAT upload, scroll apply, VRAM buffer) run AFTER the
  ; translated NMI below, not before. SMB's NMI is what writes THIS frame's OAM
  ; ($4014 -> $C900), scroll ($2005 -> $CB0C) and background tiles ($2007). The
  ; background tile writes go straight to VRAM during the NMI, so if we flushed
  ; sprites + scroll before it we'd show this frame's background with last
  ; frame's sprite positions and scroll -- the few-pixel sprite/background
  ; misalignment ("blocks rendered too early"). Flushing after keeps all three
  ; in sync for the frame.

  ; Do not invoke the translated NMI handler until NES PPUCTRL bit 7 has enabled
  ; NMI at least once. The SMS frame IRQ is our timing source, but NES reset code
  ; expects PPUSTATUS polling to work before NMIs are enabled; calling
  ; translated_nmi early would read/clear the synthetic VBlank flag and starve
  ; reset's wait loops. After startup, SMB may temporarily clear PPUCTRL bit 7
  ; while still relying on the ported frame driver to keep running, so keep
  ; calling once the game has crossed the first enable.
  ld  a, ($cb08)
  bit 7, a
  jr  nz, _irq_mark_nmi_started
  ld  a, ($cb1a)
  or  a
  jr  z, _irq_skip_translated_nmi
  jr  _irq_call_translated_nmi

_irq_mark_nmi_started:
  ld  a, $01
  ld  ($cb1a), a

_irq_call_translated_nmi:

  ; Per-frame game logic runs through the translated NES NMI handler.
  ; Per NES NMI semantics, hardware would push PC + P on the 6502 stack
  ; and jump via $FFFA. We synthesize the P push here so the translated
  ; RTI at the end pops a matching byte. PC isn't pushed because the
  ; translated routine returns via Z80 ret to this irq_handler, not via
  ; an emulated jump-via-popped-PC.
  ld  a, ($cb03)            ; shadow P
  call rt_push6502          ; push P onto emulated 6502 stack
  ; The frame interrupt can arrive while translated code has bank-switched
  ; slot 1 for a far call/jump. `translated_nmi` lives in its own generated
  ; bank, so save the current slot-1 bank, map the NMI bank, call it, then
  ; restore the interrupted bank before returning.
  ld  a, ($cb14)
  push af
  ld  a, :translated_nmi
  ld  ($cb14), a
  ld  ($fffe), a
  call translated_nmi       ; jumps to the profile/ROM NMI vector
  pop af
  ld  ($cb14), a
  ld  ($fffe), a

_irq_skip_translated_nmi:

  ; Flush this frame's prepared state to the VDP (see note above): sprites from
  ; the OAM staging, the scheduled scroll, and the queued VRAM buffer.
  call rt_sat_upload
  call _apply_frame_scroll
  call vbuf_flush

  pop de
  pop bc
  pop hl
  pop af
  ei
  ret

_irq_line_scroll_split:
  ; Mid-frame line IRQ: switch from the pre/top scroll to the captured post-hit
  ; playfield scroll, then disable further line IRQs until the next frame IRQ
  ; explicitly schedules one.
  call _apply_post_scroll
  call _disable_line_irq

  pop de
  pop bc
  pop hl
  pop af
  ei
  ret

_apply_frame_scroll:
  ; Write latched scroll X to VDP reg 8, scroll Y to VDP reg 9.
  ; The SMS horizontal scroll is the OPPOSITE direction of the NES: a
  ; larger reg8 shifts the background right (camera left), whereas a
  ; larger NES PPUSCROLL-X moves the camera right. So negate X
  ; (reg8 = -scrollX) — otherwise walking right scrolls backwards.
  ;
  ; Frame IRQ applies the pre/top scroll first. If a complete post-hit pair was
  ; captured, schedule one SMS line IRQ at NES sprite 0 Y + 8 to switch to the
  ; post/playfield scroll during active display.
  ld  a, ($cb20)
  bit 2, a
  jr  nz, _apply_frame_split_scroll
  call _apply_pre_or_live_scroll
  call _disable_line_irq
  ret

_apply_frame_split_scroll:
  call _apply_pre_or_live_scroll

  ; Use NES sprite 0's Y coordinate as the generic split marker. The interrupt
  ; counter value is approximately the target scanline minus one; sprite0_y+8
  ; puts the switch just after the 8px marker sprite used by split-screen games.
  ld  a, ($c900)
  cp  $c0
  jr  nc, _disable_line_irq
  add a, 7
  ld  b, 10
  call vdp_set_register
  call _enable_line_irq
  ret

_apply_pre_or_live_scroll:
  ld  a, ($cb20)
  bit 1, a
  jr  nz, _apply_pre_scroll
  ld  a, ($cb0c)
  ld  c, a
  ld  a, ($cb0d)
  jr  _apply_scroll_pair_cx_ay
_apply_pre_scroll:
  ld  a, ($cb21)
  ld  c, a
  ld  a, ($cb22)
  jr  _apply_scroll_pair_cx_ay

_apply_post_scroll:
  ld  a, ($cb20)
  bit 2, a
  ret z
  ld  a, ($cb23)
  ld  c, a
  ld  a, ($cb24)
  jr  _apply_scroll_pair_cx_ay

_apply_scroll_pair_cx_ay:
  ; Entry: C = NES scroll X, A = NES scroll Y.
  ld  e, a
  ld  a, c
  neg
  ld  b, 8
  call vdp_set_register
  ld  a, e
  ld  b, 9
  call vdp_set_register
  ret

_enable_line_irq:
  ; VDP reg0 bit 4 enables line interrupts on top of the base display mode.
  ld  a, VDP_R0_LINE_IRQ_ON
  ld  b, 0
  call vdp_set_register
  ret

_disable_line_irq:
  ; Restore the base R0 mode with line interrupts disabled.
  ld  a, VDP_R0_BASE
  ld  b, 0
  call vdp_set_register
  ret

.ends

; ─── mem_fill ─────────────────────────────────────────────────────────────────
; Entry: HL = destination, BC = byte count, A = fill value.
; Clobbers: HL, BC.  Preserves AF.
.section "mem_fill" free

mem_fill:
  push af
_mem_fill_loop:
  ld  (hl), a
  inc hl
  dec bc
  ld  d, a                  ; stash fill byte so we can test BC without clobbering
  ld  a, b
  or  c
  ld  a, d                  ; restore fill byte
  jr  nz, _mem_fill_loop
  pop af
  ret

.ends
