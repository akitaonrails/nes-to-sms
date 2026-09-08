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
;   $CA08-$CA0E  Banked-dispatch MRU cache (dispatch.s)
;   $CA0F        Beacon: first IRQ
;   $CA10        Beacon: first translated NMI
;   $CA11        Translated-NMI nesting depth
;   $CA12        Presentation-in-progress guard
;   $CA18        CHR-RAM off-render nametable-write count / rebuild pending
;   $CC00-$D2FF  SMS visible nametable high-byte shadow ($3700-$3DFF + $9500)
;   $D300-$D3FB  Translated-call continuation frames (dispatch.s)
;   $D500-$D5FF  Translated-call continuation segment 1 (dispatch.s)
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
;   $CB28        Runtime-ready flag: 0 during boot; 1 once irq_handler may work
;   $CA39        Sticky "8x16 sprites in use" latch (ppu.s): disables the 8x8
;                base-sprite copy-through once the pair resolver owns $2000+
;   $CB29        Previous-frame overrun flag ($80 = overran; split suppressed)
;   $CB2A        Projected window start column; $CB2B/$CB2C projector scratch
;   $CB2D        Deferred VDP reg-1 latch (0 = none; see ppu.s reg1 sync)
;   $CB2E/$CB2F  rt_far_gate target park (Phase R: BC carries far targets)
;   $CB20-$CB24  Split-scroll scheduler state (see runtime/ppu.s)
;   $CB25-$CB26  Translated-NMI interrupted slot-1 bank by nesting depth
;   $CB27        Lowerer temporary A spill for stackless LDX/LDY memory loads
;   $CB30-$CB61  APU->PSG shim state (see runtime/apu_stub.s)
;   $CB73-$CB74  Translated-call diagnostics/scratch (dispatch.s)
;   $CB76-$CB77  Translated-call return stack next-free pointer (dispatch.s)
;   $D3FC-$D3FD  rt_ppu_write_cont continuation pointer; $D3FE cont-mode flag
;   $D46C-$D471  Stackless rotate-memory helper scratch (runtime/flags.s)
;   $D472-$D473  SMB frozen playfield X / pending HUD split (otherwise reserved)
;   $D474        IRQ VDP status scratch (keeps ready check off native stack)
;   $D475-$D476  IRQ saved HL (keeps one word off native stack)
;   $D477-$D478  IRQ saved AF (keeps one word off native stack after save)
;   $D479-$D47C  rt_oam_dma AF/HL save (sat.s)
;   $D47D-$D47E  Far slot-1 bank stack next-free pointer (dispatch.s)
;   $D47F        Slot-2 transaction guard depth (maximum 2)
;   $CA19-$CA1C Slot-2 guard frame 0: IFF2, $FFFC, $FFFF, $CB62
;   $CA1D-$CA1F/$D3FF Slot-2 guard frame 1: IFF2, $FFFC, $FFFF, $CB62
;   $D3FC-$D3FE  PPU continuation pointer/mode; $D3FF is guard frame-1 CB62
;   $D4C0-$D4FF  Far slot-1 bank/continuation stack entries (dispatch.s)
;   $DD80-$DE3F  BG variant ring-slot NT refcounts (chrmap.s BGV_REFCNT;
;                below the native stack: SP low-water measured $DFC4)
;   $CB80-$CBFF  Raw mirrored NES attribute shadow (2 CIRAM pages × 64 bytes)
;   $CB13-$CB1F  13-byte scratch ("temp w")
;   $CB1D        Runtime trap marker for trace-sms diagnostics
;   Z80 SP starts at $DFFC and grows down — never touches $C100-$C1FF.
;   Z80 push/call pre-decrements SP, so the first write lands at $DFFA-$DFFB.
;   The Sega mapper registers $FFFC-$FFFF are RAM-mirrored at $DFFC-$DFFF;
;   stack writes must never reach those bytes or they reprogram banking under
;   the running code.

.define VDP_R0_BASE            $46   ; Mode 4 + top-row hscroll lock
.define VDP_R0_LINE_IRQ_ON     $56   ; VDP_R0_BASE + IE1 line IRQ enable
.define RT_GUARD_OVERFLOW       $F1
.define RT_GUARD_UNDERFLOW      $F2
.define RT_GUARD_CORRUPT        $F3
.define RT_MAPPER_BAD_ADDRESS   $F4
.define RT_MAPPER_NESTED        $F5
.define RT_MAPPER_COMMIT_BAD    $F6
.define RT_PRG_HIGH_BAD_ADDRESS $F7
.define RT_PRESENT_SLOT2_BAD    $F8
.define RT_BTD_SLOT2_BAD        $F9
.ifdef CV1_RUNTIME_HOOKS
.define RT_CV1_VBUF_ACTIVE      $FA   ; dormant C800 header must stay zero
.endif

.bank 0 slot 0
.org $0000

reset_entry:
  ; Must fit in $0000-$0007: the RST $08 trap is at $0008, and a 9-byte
  ; reset block overlaps it (wla-z80's MEM_INSERT warning; the collision
  ; corrupted the RST-08 trap bytes in earlier builds). SP init moved to
  ; boot_main to keep this block at 6 bytes.
.ifdef DIAG_WILDJUMP
  ; Diagnostic: route through the reset probe, which counts reboots and
  ; freezes with full arrival context on the first anomalous re-entry.
  di
  jp diag_reset_probe
.else
  di
  im 1
  jp boot_main
.endif

; Padding bytes between $0006 and $0008 are handled by the linker filling
; with $FF (ROM erased value).  WLA-DX will fill the gap automatically.

.org $0008
.ifdef DIAG_WILDJUMP
  jp rt_wildjump_diag        ; record the wild source, then freeze red
.else
  ; RST 1 — unused. Trap immediately so bugs surface.
  di
  halt
.endif

.org $0010
.ifdef DIAG_WILDJUMP
  jp rt_wildjump_diag        ; record the wild source, then freeze red
.else
  ; RST 2 — unused. Trap immediately so bugs surface.
  di
  halt
.endif

.org $0018
.ifdef DIAG_WILDJUMP
  jp rt_wildjump_diag        ; record the wild source, then freeze red
.else
  ; RST 3 — unused. Trap immediately so bugs surface.
  di
  halt
.endif

.org $0020
.ifdef DIAG_WILDJUMP
  jp rt_wildjump_diag        ; record the wild source, then freeze red
.else
  ; RST 4 — unused. Trap immediately so bugs surface.
  di
  halt
.endif

.org $0028
.ifdef DIAG_WILDJUMP
  jp rt_wildjump_diag        ; record the wild source, then freeze red
.else
  ; RST 5 — unused. Trap immediately so bugs surface.
  di
  halt
.endif

.org $0030
.ifdef DIAG_WILDJUMP
  ; Wild-jump canary: scratch RAM is filled with $F7 (RST $30), so any
  ; execution that strays into it lands here with the stray address on
  ; the native stack. See rt_wildjump_diag.
  jp rt_wildjump_diag
.else
  ; RST 6 — unused.
  di
  halt
.endif

.org $0038
  jp irq_handler

.org $0066
.ifdef INPUT_PAUSE_START
  ; NMI = SMS pause button -> arm a four-count NES Start press at $CB2E.
  ; Legacy input consumes one count per host frame; full MMC3 consumes
  ; one per completed port-1 byte, then releases at the next host latch.
  ; DIAG: also capture the interrupted PC (the NMI pushed it) into
  ; $CA36/37 and count NMIs at $CA35 — a non-maskable probe that works
  ; even when the machine is interrupt-dead (press pause, read RAM).
  push af
.ifdef DIAG_WILDJUMP
  push hl
  ld  hl, $0004
  add hl, sp
  ld  a, (hl)
  ld  ($ca36), a             ; interrupted PC low
  inc hl
  ld  a, (hl)
  ld  ($ca37), a             ; interrupted PC high
  ld  a, ($ca35)
  inc a
  ld  ($ca35), a             ; pause-NMI count
  pop hl
.endif
  ld  a, $04
  ld  ($cb2e), a
  pop af
  retn
.else
  ; NMI = SMS pause button.  Ignored for v1; RETN returns to the
  ; interrupted context without servicing.
  retn
.endif

; ─── boot_main ────────────────────────────────────────────────────────────────
.org $0068

.section "boot_main" free

boot_main:
  ld  sp, $dffc               ; native Z80 stack; push pre-decrements below
                              ; the $DFFC-$DFFF mapper mirror (see header note)

  ; Mark the runtime NOT ready: until boot finishes, any IRQ-handler entry
  ; (spurious RST $38, emulator power-on IFF quirks, stray VDP INT) must be
  ; acknowledged and ignored WITHOUT re-enabling interrupts. Mednafen was
  ; observed accepting an interrupt a few instructions into boot despite the
  ; reset DI; the handler's unconditional `ei` exit then kept interrupts
  ; enabled for the whole boot, and the per-frame handler starved boot and
  ; translated init forever (black screen). See docs/completion-plan.md.
  xor a
  ld  ($cb28), a
  ld  ($cb29), a            ; overrun flag (see irq_handler pacing read)
  ld  ($cb2a), a            ; projected window start column (ntmap.s)
  ld  ($cb25), a            ; translated-NMI saved slot-1 bank depth 0
  ld  ($cb26), a            ; translated-NMI saved slot-1 bank depth 1
  ld  ($cb62), a            ; NES PRG bank shadow (banked mappers; 0 = bank 0)
  ld  ($cb78), a            ; band-dirty flag (rows 0-3 re-materialization)
  ld  ($cb7e), a            ; in-handler flag starts clear
  ld  ($cb7f), a            ; deferred FC-flush flag
  ld  ($ca0e), a            ; dispatch MRU invalid
  ld  ($ca11), a            ; translated-NMI nesting depth
  ld  ($ca12), a            ; presentation-in-progress guard
  ld  ($ca18), a            ; no CHR-RAM full-screen rebuild pending
  ld  ($d47f), a            ; slot-2 transaction guard depth
  ld  a, $ff
  ld  ($ca13), a            ; last-materialized BG table (force first flush)
  ld  hl, $d4c0
  ld  ($d47d), hl           ; far-bank stack next-free pointer
  ld  hl, $d300
  ld  ($cb76), hl           ; translated-call return stack next-free pointer

  ; I/O port control: configure both controller ports as inputs (TR/TH
  ; lines included). Real SMS games write $3F=$FF at boot; without it,
  ; emulators that model the I/O control register (Mednafen) can return
  ; forced-output levels on port $DC reads and controller input is dead.
  ; trace-sms does not model port $3F, which is why this never showed
  ; in the harness.
  ld  a, $ff
  out ($3f), a

  ; Initialize standard Sega mapper registers explicitly. This keeps emulators
  ; on the Sega mapper path before any optional slot-2 SRAM use.
  xor a
  ld  ($fffc), a              ; slot-2 SRAM disabled, no bank shift
  ld  ($fffd), a              ; slot 0 bank 0
  ld  a, $01
  ld  ($fffe), a              ; slot 1 bank 1 until translated_reset remap
  ld  a, $02
  ld  ($ffff), a              ; slot 2 bank 2 until asset/data remaps

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

.ifdef RAW_CIRAM_BACKEND_SRAM
  ; 6b. Clear external raw-CIRAM SRAM backend ($8000-$87FF in slot-2 SRAM
  ; bank 0). Rendering is still driven by the folded internal shadows; this is
  ; only storage scaffolding for later parity/materializer phases.
.ifndef MMC3_FULL_RUNTIME
  call rt_raw_ciram_sram_clear
.endif
.endif

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

  ; 8b. Init the APU->PSG shim (shadow, sequencer state, silence PSG).
  call apu_psg_init

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
.ifdef NES_MMC3
  ; Mapper state reuses the inactive legacy update-buffer body, so initialize
  ; only AFTER its startup clear. C800 remains the empty-buffer sentinel.
  call rt_mmc3_init
.endif
.ifdef CNROM_BUS_EXPERIMENT
  call rt_cnrom_init
.endif
.ifdef CNROM_SOURCE_CLOCK_EXPERIMENT
  call rt_source_init
.endif

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

  ; 11d. Init the background sub-palette variant cache to "unassigned" ($FF)
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
  ld  ($ca39), a            ; 8x16-sprite-mode-seen latch (ppu.s) clear
  ld  hl, $dd80             ; ring-slot NT refcounts (chrmap.s BGV_REFCNT)
  ld  bc, $00c0             ; 192 entries for slots 64-255
  xor a
  call mem_fill
.ifdef NES_CHR_RAM
  ; 8x16 sprite cache: tile key per OAM entry at $D400 and attribute key per
  ; entry at $D480. $D400 is shared with the ordinary 8x8 resolved table, so
  ; the resolver invalidates it again whenever 8x16 mode is re-entered.
  ld  hl, $d400
  ld  bc, $0040
  ld  a, $ff
  call mem_fill
  ld  hl, $d480
  ld  bc, $0040
  call mem_fill
  xor a
  ld  ($d468), a            ; last SAT mode was 8x8
.endif
.ifdef CV1_COHERENT_BG
  ; SRAM metadata and palette shadow match the boot assets before the first
  ; producer write; no internal OAM or continuation storage is repurposed.
  call rt_cv1_bg_init
.endif
.ifdef MMC3_FULL_RUNTIME
  call rt_mmc3_graphics_init
.endif

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
  ld   a, $01
  ld   ($cb28), a            ; runtime ready: irq_handler may do real work
  ; SRAM self-test: the CHR mirror needs SRAM beyond the CIRAM 2 KiB.
  ; Write/read-back at $8800; RED border + halt on failure.
.ifdef NES_CHR_RAM
  call rt_raw_ciram_sram_enable
  ld   a, $5A
  ld   ($8800), a
  ld   a, ($8800)
  cp   $5A
  push af
  call rt_raw_ciram_sram_disable
  pop  af
  jr   z, _sram_ok
  ld   a, $03                ; RED: SRAM $8800 not usable
  call rt_boot_beacon
_sram_halt:
  jr   _sram_halt
_sram_ok:
.endif
.ifdef DIAG_WILDJUMP
 .ifndef MMC3_FULL_RUNTIME
  ; Arm the wild-jump canary: fill the nametable sub-palette shadow (the
  ; region a real-emulator run was caught executing, PC $CE0A) with $F7 =
  ; RST $30. Legitimate writers overwrite their cells; readers only use
  ; the low 2 bits (renders with wrong sub-palettes — diagnostic build).
  ld   hl, $cc00
  ld   bc, $0700
  ld   a, $f7
  call mem_fill
 .endif
.endif
  ; Phase R: establish X/Y residency (D = X, E = Y) from the shadows.
  ld   a, ($cb00)
  ld   d, a
  ld   a, ($cb01)
  ld   e, a
  ei                          ; now safe: slot 1 has translated code
  jp   $4000                  ; logical slot-1 address of translated_reset

.ends

; ─── irq_handler ──────────────────────────────────────────────────────────────
.section "irq_handler" free

.ifdef DIAG_WILDJUMP
; Reset-arrival probe. Confirmed: on Mednafen the machine re-enters $0000
; ~80 times/minute (endless reboot cycle) without ever executing the RAM
; canary — so the arrival is a native `ret` through a zeroed stack word or
; a computed `jp` with a zeroed pointer. Freeze on the FIRST re-entry
; (count 2; power-on is 1) with the arrival context:
;   $CA20  reboot counter        $CA22  HL at entry (jp (hl) source)
;   $CA24  SP at entry           $CA27  A at entry
;   $CA28  16 stack bytes from SP-8 (the word a `ret` just popped sits
;          at SP-2/SP-1)         $CA38  BC   $CA3A  DE at entry
diag_reset_probe:
  ld  ($ca27), a             ; A at entry, before anything clobbers it
  ld  ($ca22), hl
  ld  hl, $0000
  add hl, sp
  ld  ($ca24), hl
  ld  a, ($ca20)
  inc a
  ld  ($ca20), a
  cp  $02
  jr  nz, _drp_boot
  ld  ($ca38), bc
  ld  ($ca3a), de
  ld  hl, ($ca24)
  ld  bc, $fff8
  add hl, bc                 ; HL = SP - 8
  ld  de, $ca28
  ld  bc, $0010
  ldir
  im  1
  ld  a, $03                 ; RED backdrop: reboot arrival captured
  call rt_boot_beacon
_drp_halt:
  di
  halt
  jr  _drp_halt
_drp_boot:
  im  1
  jp  boot_main

; Wild-jump breadcrumb trap. Reached via the $F7 (RST $30) canary fill:
; execution strayed into scratch RAM. Record everything a savestate needs
; to reconstruct the jump, paint the backdrop red, and freeze — a Mednafen
; F5 state then carries the whole story:
;   $CA20  reboot counter (incremented at each boot_main entry)
;   $CA21  wild-jump counter
;   $CA22  wild-exec address + 1 (the RST return address)
;   $CA24  native SP at trap time
;   $CA28  top 16 bytes of the native stack (return-address chain)
rt_wildjump_diag:
  di
  pop  hl
  ld   ($ca22), hl
  ld   hl, $0000
  add  hl, sp
  ld   ($ca24), hl
  ld   de, $ca28
  ld   bc, $0010
  ldir
  ld   a, ($ca21)
  inc  a
  ld   ($ca21), a
  ld   a, $03                ; RED backdrop: wild jump caught
  call rt_boot_beacon
_wjd_halt:
  di
  halt
  jr   _wjd_halt
.endif

; Boot beacon: A = border color (CRAM format); writes CRAM index 17 and
; points VDP reg 7 (backdrop) at it. Diagnostic use only: halt paths (SRAM
; self-test failure) and DIAG_WILDJUMP builds. It must NEVER run during
; normal play — clobbering sprite palette entry 1 and the backdrop showed
; up as full-screen white blinks on display-off frames (the SMS fills a
; disabled display with the backdrop color).
rt_boot_beacon:
  push af
  ld   a, $11                ; CRAM index 17 (sprite palette entry 1)
  out  ($bf), a
  ld   a, $c0
  out  ($bf), a              ; CRAM write command
  pop  af
  out  ($be), a              ; the color itself
  ld   a, $01
  out  ($bf), a
  ld   a, $87                ; reg 7 = backdrop -> entry 17
  out  ($bf), a
  ret

irq_handler:
.ifdef CNROM_BUS_EXPERIMENT
  ; Bus-only synthetic contract: acknowledge host IRQ, never invent a guest
  ; frame/NMI or run legacy graphics over CNROM source state.
  push af
  in a, ($bf)
  pop af
  ei
  reti
.endif
.ifdef MMC3_FULL_RUNTIME
  jp rt_mmc3_sms_interrupt
.else
  ; Save the interrupted context on the NATIVE STACK. The handler is
  ; re-entrant (nested translated-NMI entries, skip entries, the line
  ; split); the old fixed save words ($D472/$D475/$D477) were single-slot,
  ; so ANY second entry before the first exit clobbered the outer
  ; context's registers — the outer thread then resumed with the inner's
  ; BC/HL (observed as garbage rt_rti dispatches in SMB free-run and as
  ; CV1's jp-$0000 reboot loop on Mednafen, where skip entries carry
  ; zeroed BC/HL).
  push hl
  push af
  push bc
  ; All profiles have interruptible live A spills: banked dispatch uses
  ; CB15 before DI, inline PPU stores use CB18, and stackless LDX/LDY uses
  ; CB27. A translated NMI can reuse each. Two native words make the save
  ; re-entrant; the second word's flags byte is padding, not live state.
  ld  a, ($cb15)
  ld  b, a
  ld  a, ($cb18)
  ld  c, a
  push bc
  ld  a, ($cb27)
  push af
.ifndef NES_PRG_BANK_BASE
  ; NROM fixed-high reads temporarily map data_prg_high inline. An IRQ may land
  ; between that map and its restore, so preserve the interrupted slot-2 bank
  ; on the re-entrant native stack and present/NMI from the canonical low bank.
  ; Mapper 2 cannot use this shortcut: its guarded transactions also own SRAM
  ; control and the live NES-bank shadow.
  ld  a, ($ffff)
  push af
  ld  a, :data_prg_low
  ld  ($ffff), a
.endif
  ld  a, $01
  ld  ($cb7e), a            ; in-handler flag (nesting-aware ei gating)
  ; Phase R: DE carries the resident 6502 X/Y of the interrupted thread.
  ; Sync to the RAM shadows now; exits restore DE from those shadows instead of
  ; spending native stack on a saved DE word. The translated NMI may update the
  ; shadows below, matching 6502 interrupt-visible X/Y behavior.
  ld  a, d
  ld  ($cb00), a
  ld  a, e
  ld  ($cb01), a

  ; Acknowledge the VDP interrupt by reading the status port. Frame and line
  ; interrupts share the Z80 IM1 vector; status bit 7 identifies frame IRQs.
  ; Bit 7 clear means a non-frame VDP IRQ; the only one we enable is the
  ; one-shot line split below.
  in  a, ($bf)
  ld  ($d474), a

  ; Runtime-ready gate: if boot has not finished, this entry is spurious
  ; (stray RST $38 through linker fill bytes, emulator power-on IFF quirks,
  ; or a VDP INT that predates our setup). The status read above already
  ; acknowledged the VDP; leave WITHOUT `ei` so a spurious entry cannot
  ; enable interrupts behind boot's back.
  ld  a, ($cb28)
  or  a
  jr  nz, _irq_runtime_ready
  ld  a, ($cb00)
  ld  d, a
  ld  a, ($cb01)
  ld  e, a
.ifndef NES_PRG_BANK_BASE
  pop af
  ld  ($ffff), a
.endif
  pop af
  ld  ($cb27), a
  pop bc
  ld  a, b
  ld  ($cb15), a
  ld  a, c
  ld  ($cb18), a
  pop bc
  pop af
  pop hl
  ret

_irq_runtime_ready:
  ld  a, ($d474)
  bit 7, a
  jp  z, _irq_line_scroll_split

.ifdef CV1_COHERENT_BG
  ; Physical epoch advances even when no new producer packet is ready. The
  ; coherent consumer may commit first; otherwise it rearms the old HUD.
  call rt_cv1_hud_epoch_begin
  ; Already-built output can admit before old HUD rearm/controller overhead.
  call rt_cv1_frame_try_present
  or a
  call z, rt_cv1_hud_rearm
.endif

  ; Latch controller state before any translated code reads it.
  call rt_controller_latch

  ; Increment frame counter (wraps at 256, sufficient for v1 timing).
  ld  a, ($cb04)
  inc a
  ld  ($cb04), a

  ; Signal "VBlank pending" to translated code that polls $2002.
  ld  a, $01
  ld  ($cb05), a

.ifdef DEBUG_BORDER_HEARTBEAT
  ; Diagnostic (assemble with -D DEBUG_BORDER_HEARTBEAT): cycle the border
  ; color (VDP reg 7) with the frame counter so even an all-black screen
  ; proves in a real emulator that the frame IRQ handler is alive. Used to
  ; bisect the 2026-07-03 Mednafen black-screen investigation.
  ld  a, ($cb04)
  and $0f
  out ($bf), a
  ld  a, $87
  out ($bf), a
.endif

.ifdef CV1_COHERENT_BG
  ; The early consumer owns every visible write. Never enter legacy waits,
  ; in-place projection or mutable control installation for this backend.
  jp _present_skip_all
.endif
  ; Present before delivering the next translated NMI. The CV1 opt-in
  ; requires a completed-prologue packet; the legacy path uses the latest
  ; available staging. Frozen control does NOT make direct $2007/CHR/CRAM
  ; writes coherent or guarantee that conversion/upload fits VBlank.
  ; The wait below aligns the start of work, but that work can still race
  ; the beam and delay game computation. DI excludes CPU re-entry, not VDP
  ; scanning; bounded preparation/commit remains separate work.
  ; An overlong translated NMI may receive a nested lag handler (CV1's
  ; full handler normally RTIs). Never restart an in-progress presentation
  ; or its VBlank wait from a nested IRQ: repeated waits can starve the
  ; interrupted work. Its outer presentation must finish instead.
  ld  a, ($ca12)
  or  a
  jp  nz, _present_skip_all
.ifdef CV1_RUNTIME_HOOKS
  ; C801-C8FF belongs to prepared graphics, never the legacy vbuf parser.
  ld  a, ($c800)
  or  a
  jp  nz, _cv1_vbuf_bad
  ; A prepared generation has priority over any later READY packet. Retrying
  ; final ports does not rebuild BG/SAT or wait for another global VBlank.
  ld  a, (CV1_FRAME_PENDING)
  or  a
  jr  z, _cv1_no_pending_commit
  ld  a, 1
  ld  ($ca12), a
  call rt_cv1_try_finish_pending
  jp  _present_skip_all
_cv1_no_pending_commit:
  ; An IRQ during a slow producer is not a new completed graphics frame.
  ; In particular, do not burn another VBlank wait resolving the same OAM.
  ld  a, (CV1_FRAME_READY)
  or  a
  jp  z, _present_skip_all
  call rt_cv1_frame_begin
.endif
  ld  a, $01
  ld  ($ca12), a
.ifdef DIAG_WILDJUMP
  ld  a, ($ca30)
  inc a
  ld  ($ca30), a             ; entered vblank wait
.endif
_present_wait_vblank:
.ifdef DIAG_WILDJUMP
  ld  hl, ($ca32)
  inc hl
  ld  ($ca32), hl            ; spin iterations (running)
.endif
  in  a, ($7e)               ; V-counter
  cp  $e0
  jr  c, _present_wait_vblank
.ifdef DIAG_WILDJUMP
  ld  a, ($ca31)
  inc a
  ld  ($ca31), a             ; exited vblank wait
.endif
  ; Single outer presentation invariant: all materializer/SAT slot-2 traffic
  ; runs under DI with no slot-2 guard and the current PRG window visible.
  ; Do not add per-byte checks below this boundary.
  ld  a, i
  jp  pe, _present_slot2_bad
  ld  a, ($d47f)
  or  a
  jp  nz, _present_slot2_bad
  ld  a, ($fffc)
  or  a
  jp  nz, _present_slot2_bad
  ld  b, a
  ld  a, ($ffff)
.ifdef NES_PRG_BANK_BASE
  ld  b, a
  ld  a, ($cb62)
  and NES_PRG_BANK_MASK
  add a, NES_PRG_BANK_BASE
  cp  b
.else
  cp  :data_prg_low
.endif
  jp  nz, _present_slot2_bad
.ifdef SMB_RUNTIME_HOOKS
  ; Before variable-cost SAT/column work, while the beam is still in blank.
  call _apply_frame_scroll
.endif
  ; A CHR-RAM game can replace an entire screen in raw CIRAM while rendering
  ; is disabled. Rebuild the folded SMS nametable before applying the deferred
  ; display-enable write, so no stale cells from the previous scene are shown.
.ifdef NES_CHR_RAM
  ld  a, ($ca18)
  or  a
  jr  z, _present_no_screen_rebuild
  ld  a, ($cb09)
  and $18
  jr  z, _present_no_screen_rebuild
  xor a
  ld  ($ca18), a
.ifndef DIAG_NO_PROJECTION
  ; A complete screen build replaces every visible cell. Reclaim variant slots
  ; owned by the previous scene before projecting the new working set.
.ifdef CV1_RUNTIME_HOOKS
  ; The new scene must remain hidden during a destructive BG rebuild. The
  ; frozen SAT commit alone restores its requested display enable afterward.
  call rt_cv1_sat_blank
.endif
  call rt_bg_reset_variant_cache
  call rt_nt_materialize_window
.endif
_present_no_screen_rebuild:
.endif
  ; Apply a deferred PPUMASK-driven VDP reg-1 write (see ppu.s): display
  ; enable changes only ever land here, inside VBlank.
.ifndef CV1_RUNTIME_HOOKS
  ld  a, ($cb2d)
  or  a
  jr  z, _present_no_reg1
  ld  b, 1
  call vdp_set_register
  xor a
  ld  ($cb2d), a
.ifdef DIAG_WILDJUMP
  ld  a, $01
  ld  ($ca34), a
.endif
_present_no_reg1:
.endif
.ifdef NES_CHR_RAM
  ; Deferred variant-cache flush (BG table switched; see ppu.s).
  ld  a, ($cb7f)
  or  a
  jr  z, _present_no_fcflush
  xor a
  ld  ($cb7f), a
  ; Damping: games toggle PPUCTRL bit 4 dozens of times per frame
  ; during upload bursts; only the value PRESENTED matters. If the
  ; table now selected is the one the current variants were generated
  ; against ($CA13 latch), the toggles cancelled out — skip the flush
  ; entirely. Without this the full-window re-projection below ran
  ; every frame (avg budget 0.45x -> 6.6x).
  ld  a, ($cb08)
  and $10
  ld  hl, $ca13
  cp  (hl)
  jr  z, _present_no_fcflush
  ld  (hl), a
  push de
  ; PPUCTRL selects the source table globally. Keep every existing variant's
  ; slot number (and therefore every nametable reference) stable, and refresh
  ; only its pixels from the newly presented table. A full reset/reprojection
  ; here renumbered slots and took far beyond one real VBlank, exposing torn
  ; columns even though the final trace framebuffer looked coherent.
  call rt_bg_refresh_variant_cache
  pop de
.ifdef DIAG_WILDJUMP
  ld  a, $02
  ld  ($ca34), a
.endif
_present_no_fcflush:
  ; Dirty band (rows 0-3): re-materialize from the selected page's raw
  ; CIRAM (write-time page gating is unsound; see ntmap.s).
  ld  a, ($cb78)
  or  a
  jr  z, _present_band_clean
  call rt_nt_materialize_band
.ifdef DIAG_WILDJUMP
  ld  a, $03
  ld  ($ca34), a
.endif
_present_band_clean:
.endif
.ifdef CV1_RUNTIME_HOOKS
  ; Always prepare, including sprites-disabled MASK: SMS has no independent
  ; sprite enable, so an explicit hidden SAT must replace the old one.
  call rt_cv1_sat_prepare
.else
  ld  a, ($cb09)             ; PPUMASK
  bit 4, a                   ; sprites enabled?
  call nz, rt_sat_upload
.endif
.ifdef DIAG_WILDJUMP
  ld  a, $04
  ld  ($ca34), a
.endif
  ; Project columns entering the visible window (E.5c) with the same
  ; playfield scroll the apply below will present: the last post pair when
  ; one exists (bit2 is sticky), else the live latch.
  ld  a, ($cb20)
  bit 2, a
  jr  z, _irq_proj_live
  ld  a, ($cb23)
  ld  c, a
  jr  _irq_proj_go
_irq_proj_live:
  ld  a, ($cb0c)
  ld  c, a
_irq_proj_go:
  call rt_nt_project_scroll
.ifdef CV1_RUNTIME_HOOKS
  ; All destructive BG preparation precedes the bounded SAT/base/size commit.
  ; Its local early-blank admission does not assume the older global wait
  ; survived the intervening BG work.
  call rt_cv1_sat_try_commit
  jr  nc, _cv1_committed_now
  call rt_cv1_frame_suspend
  xor a
  ld ($ca12), a
  jp _present_skip_all
_cv1_committed_now:
.endif
.ifndef SMB_RUNTIME_HOOKS
  call _apply_frame_scroll
.else
  call rt_smb_hud_poll
.endif
.ifdef CV1_RUNTIME_HOOKS
  call rt_cv1_frame_end
.else
  call vbuf_flush
.endif
  xor a
  ld  ($ca12), a            ; presentation complete (re-entrancy guard clear)
  jp  _present_skip_all

_present_slot2_bad:
  di
  ld  a, RT_PRESENT_SLOT2_BAD
  ld  ($cb1d), a
_present_slot2_halt:
  halt
  jr  _present_slot2_halt

.ifdef CV1_RUNTIME_HOOKS
; Final commit is internal-RAM/fixed-bank/port-only. Unlike preparation, it
; preserves any valid slot2 guard depth and SRAM/PRG mapping, including an
; interrupted PPUDATA writer's depth1/2. Every try restores live control.
rt_cv1_try_finish_pending:
  call _cv1_pending_resume_checked
  call rt_cv1_sat_try_commit
  jr c, _cv1_pending_restore
  jr _cv1_pending_committed

; Explicit writer barrier: block here if necessary, before its first raw byte.
rt_cv1_finish_pending:
  call _cv1_pending_resume_checked
  call rt_cv1_sat_commit
_cv1_pending_committed:
  call _apply_frame_scroll
  xor a
  ld (CV1_FRAME_PENDING), a
_cv1_pending_restore:
  xor a
  ld ($ca12), a
  jp rt_cv1_frame_end

_cv1_pending_resume_checked:
  ld a, i
  jp pe, _present_slot2_bad
  ld a, ($d47f)
  cp 3
  jp nc, rt_ppu_guard_overflow
  ld a, ($c800)
  or a
  jp nz, _cv1_vbuf_bad
  jp rt_cv1_frame_resume

_cv1_vbuf_bad:
  di
  ld a, RT_CV1_VBUF_ACTIVE
  ld ($cb1d), a
_cv1_vbuf_halt:
  halt
  jr _cv1_vbuf_halt
.endif

_present_skip_all:
.ifndef CV1_RUNTIME_HOOKS
  ; Start each translated NMI before the approximated sprite-0 hit point.
  ; SMB first waits for PPUSTATUS bit 6 to clear, then waits for it to set.
  ; The status reader advances this phase on polling so those barriers can
  ; complete without scanline-level NES PPU emulation.
  xor a
  ld  ($cb12), a
  ; Clear the per-frame split flags (bit0 hit phase, bit1 pre pair) but keep
  ; bit2 STICKY: once a post-split pair exists it remains the playfield
  ; scroll of record. On frames where the game's scroll sequence has not
  ; completed (preempted resident NMI, rendering-off transitions that run
  ; their sprite-0 wait to timeout), presentation must show the last post
  ; pair rather than a status-bar pair sampled from the live latch — the
  ; latch mid-sequence reads as a scroll-0 teleport, which both flashes the
  ; origin on screen and tricks rt_nt_project_scroll into a full-window
  ; re-materialization (~500K-step handler bursts).
  ld  a, ($cb20)
  and $04
  ld  ($cb20), a
.endif

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
  jp  z, _irq_skip_translated_nmi
  ; Once the translated NMI has started, keep the frame driver alive for
  ; top-level IRQs even if the game temporarily clears PPUCTRL.NMI (SMB does
  ; this during its VRAM update). Nested frame IRQs are different: NES would not
  ; re-enter its NMI while bit 7 is clear, and doing so grows the native Z80
  ; call stack without bound. If we are already inside translated_nmi and the
  ; bit is clear, skip this translated NMI body.
  ld  a, ($ca11)
  or  a
  jp  nz, _irq_skip_translated_nmi
  jr  _irq_call_translated_nmi

_irq_mark_nmi_started:
.ifdef CV1_RUNTIME_HOOKS
  ; The reset routine enables PPUCTRL.NMI at C106 before its epilogue sets
  ; the saved PRG context ($24=6 at C02B). That epilogue fits before the next
  ; NES VBlank, but translated timing can interrupt it. The first full NMI
  ; would restore the still-zero $24 and dispatch task B7DC into bank0 data.
  ; Keep driving IRQ/input while reset finishes; only the FIRST translated
  ; NMI needs this profile-specific boot-context condition.
  ld  a, ($cb1a)
  or  a
  jr  nz, _cv1_first_nmi_ready
  ld  a, ($c024)
  cp  6
  jp  nz, _irq_skip_translated_nmi
_cv1_first_nmi_ready:
.endif
  ld  a, $01
  ld  ($cb1a), a

_irq_call_translated_nmi:
  ; Bound translated-NMI nesting. An overlong game handler may receive one
  ; light nested handler, but deeper re-entry exhausts the native stack.
  ; CV1's full handler normally clears $1B and RTIs after its game work.
  ld  a, ($ca11)
  cp  2
  jp  nc, _irq_skip_translated_nmi
.ifdef CV1_RUNTIME_HOOKS
  ; Pending output backpressures a NEW full producer, not an already-running
  ; busy game body or its valid lag NMI. Do this before CB12/CB20 phase resets.
  ld  a, (CV1_FRAME_PENDING)
  or  a
  jr  z, _cv1_pending_body_allowed
  ld  a, ($c01b)
  or  a
  jp  z, _irq_skip_translated_nmi
_cv1_pending_body_allowed:
.ifdef CV1_COHERENT_BG
  ; READY owns the double buffer even before its first deferred IRQ retry.
  ld a, (CV1_FRAME_READY)
  or a
  jr z, _cv1_ready_body_allowed
  ld a, ($c01b)
  or a
  jp z, _irq_skip_translated_nmi
_cv1_ready_body_allowed:
.endif
  ld  a, ($ca11)
  ; CV1 sets $1B only AFTER its DMA/stripe/split prologue. While that
  ; prologue (or the busy-clear/RTI epilogue) is interrupted, a second full
  ; handler would consume partial producer state. Only its real busy lag
  ; path may nest. Skipped bodies must also leave CB12/CB20 untouched.
  or  a
  jr  z, _cv1_deliver_nmi
  ld  a, ($c01b)
  or  a
  jp  z, _irq_skip_translated_nmi
_cv1_deliver_nmi:
  xor a
  ld ($cb12), a
  ld a, ($cb20)
  and $04
  ld ($cb20), a
.endif

  ; Phase R: reload resident X/Y for the translated NMI.
  ld  a, ($cb00)
  ld  d, a
  ld  a, ($cb01)
  ld  e, a

  ; Per-frame game logic runs through the translated NES NMI handler.
  ; Per NES NMI semantics, hardware pushes PCH, PCL, P on the 6502 stack
  ; and jumps via $FFFA. Synthesize the full 3-byte frame with the
  ; sentinel PC $FFFF: rt_rti pops P + PC and, on the sentinel, returns
  ; natively to this bridge — while a game that REWRITES the stacked PC
  ; before RTI (BRK-recovery idiom) gets dispatched to the rewritten
  ; address, matching hardware. Inline (stackless): the IRQ/NMI bridge
  ; only needs DE (resident X/Y) preserved at the deepest native point.
  ld  a, ($cb02)            ; shadow S
  ld  l, a
  ld  h, $c1
  ld  (hl), $ff             ; sentinel PCH at S
  dec l                     ; page-wrapping, like the 6502 stack
  ld  (hl), $ff             ; sentinel PCL at S-1
  dec l
  ld  a, ($cb03)            ; shadow P
  ld  (hl), a               ; P at S-2
  ld  a, ($cb02)
  sub 3
  ld  ($cb02), a            ; push decrements S by the frame size
  ; The frame interrupt can arrive while translated code has bank-switched
  ; slot 1 for a far call/jump. `translated_nmi` lives in its own generated
  ; bank, so save the current slot-1 bank by NMI nesting depth, map the NMI
  ; bank, call it, then restore the interrupted bank before returning. Keep
  ; this off the native stack: the translated NMI can run at stack low-water.
  ld  a, ($ca11)
  ld  hl, $cb25
  or  a
  jr  z, _irq_nmi_save_bank_slot
  inc hl
_irq_nmi_save_bank_slot:
  ld  a, ($cb14)
  ld  (hl), a
.ifdef NATIVE_CALLS
  ; The native far shim's scratch pair may hold live main-thread state
  ; (an IRQ can land mid-shim); the translated NMI's own far transfers
  ; reuse it. Save per depth, restore after the NMI body.
  ld  a, ($ca11)
  or  a
  ld  a, ($ca2a)
  jr  nz, _irq_native_save_d1
  ld  ($ca2c), a
  ld  a, ($ca2b)
  ld  ($ca2e), a
  jr  _irq_native_save_done
_irq_native_save_d1:
  ld  ($ca2d), a
  ld  a, ($ca2b)
  ld  ($ca2f), a
_irq_native_save_done:
.endif
  ld  a, :translated_nmi
  ld  ($cb14), a
  ld  ($fffe), a
  ; NES NMIs are edge-triggered: a long-running NMI still receives later
  ; VBlank edges. CV1's $1B busy flag routes these to its lag handler;
  ; $7F separately guards shared audio work. Run translated code with
  ; interrupts ENABLED so the
  ; next frame INT can nest through this same handler. Games that gate
  ; re-entry via PPUCTRL bit 7 (SMB clears it first thing) are skipped
  ; by the $CB08 check on the nested entry — NES-equivalent either way.
  xor a
  ld  ($cb7e), a            ; leaving handler context (helpers may ei)
  ld  a, ($ca11)
  inc a
  ld  ($ca11), a
.ifdef DIAG_WILDJUMP
  ld  a, $05
  ld  ($ca34), a
.endif
  ei
  call translated_nmi       ; jumps to the profile/ROM NMI vector
  di
.ifdef DIAG_WILDJUMP
  ld  a, $06
  ld  ($ca34), a
.endif
  ld  a, ($ca11)
  dec a
  ld  ($ca11), a
  ld  hl, $cb25
  or  a
  jr  z, _irq_nmi_restore_bank_slot
  inc hl
_irq_nmi_restore_bank_slot:
.ifdef NATIVE_CALLS
  ; Restore the far-shim scratch pair for the interrupted thread. A holds
  ; the post-decrement depth (0 or 1) from above; it is zero exactly when
  ; HL stayed at the depth-0 slot.
  or  a
  ld  a, ($ca2c)
  jr  z, _irq_native_restore_store
  ld  a, ($ca2d)
_irq_native_restore_store:
  ld  ($ca2a), a
  ld  a, ($ca11)
  or  a
  ld  a, ($ca2e)
  jr  z, _irq_native_restore_bank
  ld  a, ($ca2f)
_irq_native_restore_bank:
  ld  ($ca2b), a
.endif
  ld  a, $01
  ld  ($cb7e), a            ; back in handler context
  ; Phase R: the NMI may have changed X/Y — new truth back to the shadows.
  ld  a, d
  ld  ($cb00), a
  ld  a, e
  ld  ($cb01), a
  ld  a, (hl)
  ld  ($cb14), a
  ld  ($fffe), a

_irq_skip_translated_nmi:

.ifdef CV1_COHERENT_BG
  ; Entry status was consumed by the frame/line classification above. Audio
  ; stays DI, so D474 can now collect any VINT acknowledged by its HUD polls.
  ; The final pacing read must retain those edges even if its own status is0.
  xor a
  ld ($d474), a
.endif

  ; APU frame sequencer + PSG write-back (envelopes, lengths, sweeps).
  call apu_frame_tick
.ifdef DIAG_WILDJUMP
  ld  a, $07
  ld  ($ca34), a
.endif

  ; Frame-overrun pacing. The VDP frame interrupt is level-held: if this
  ; handler ran longer than one video frame (heavy translated NMIs do), the
  ; next frame INT is already pending and would re-enter the handler on the
  ; very next instruction after `ei; ret`, starving the main thread forever
  ; (observed as SMB's reset code never finishing init under Mednafen).
  ; Reading the status port acknowledges any pending frame INT, guaranteeing
  ; the main thread one full frame of CPU between handler runs. When the
  ; handler fits its frame budget this read happens during VBlank before a
  ; line INT can be pending, so nothing is lost.
  ;
  ; The read also tells us whether THIS handler overran (bit 7 = a frame INT
  ; is already pending again). $CB29 is a STICKY overrun counter: an overrun
  ; sets it to 60; a fit frame decrements it. Legacy scroll presentation only
  ; arms the sprite-0 split when the counter is zero. Without hysteresis,
  ; frames alternating right at the budget line flip-flopped between split
  ; mode (whose swallowed line IRQ rendered the whole frame at the status-
  ; bar scroll) and direct mode — a fast blink between two scroll states
  ; (field report: 'interleaving frames from way behind/ahead').
  ; SMB now services delayed splits before acknowledgment and rearms consumed
  ; VBlanks below, so it no longer needs the legacy split suppression.
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_tail_ack
  ld hl, $d474
  or (hl)                     ; audio polls may already have acknowledged VINT
.else
.ifdef SMB_RUNTIME_HOOKS
  ; Service a split delayed by DI audio before the pacing read clears HINT.
  call rt_smb_hud_poll
.endif
  in  a, ($bf)
.endif
  and $80
  jr  z, _pace_fit
.ifdef SMB_RUNTIME_HOOKS
  ; The pacing acknowledgment consumes this physical VBlank without another
  ; frame IRQ. Repeat the committed HUD now; otherwise its next top band
  ; inherits the previous playfield scroll for one whole video frame.
  call rt_smb_hud_repeat
.endif
  ld  a, 60
  ld  ($cb29), a
  jr  _pace_done
_pace_fit:
  ld  a, ($cb29)
  or  a
  jr  z, _pace_done
  dec a
  ld  ($cb29), a
_pace_done:

  ; Phase R: X/Y may have changed in the translated NMI; the interrupted
  ; thread resumes with the new values (6502 semantics).
  ld  a, ($cb00)
  ld  d, a
  ld  a, ($cb01)
  ld  e, a
  xor a
  ld  ($cb7e), a            ; leaving handler
.ifndef NES_PRG_BANK_BASE
  pop af
  ld  ($ffff), a            ; resume an interrupted inline fixed-high read
.endif
  pop af
  ld  ($cb27), a
  pop bc
  ld  a, b
  ld  ($cb15), a
  ld  a, c
  ld  ($cb18), a
  pop bc
  pop af
  pop hl
  ei
  ret

_irq_line_scroll_split:
  ; Mid-frame line IRQ: switch from the pre/top scroll to the captured post-hit
  ; playfield scroll, then disable further line IRQs until the next frame IRQ
  ; explicitly schedules one.
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_line
.else
.ifdef SMB_RUNTIME_HOOKS
  call rt_smb_hud_line
.else
  call _apply_post_scroll
  call _disable_line_irq
.endif
.endif

  ld  a, ($cb00)
  ld  d, a
  ld  a, ($cb01)
  ld  e, a
  xor a
  ld  ($cb7e), a            ; leaving handler
.ifndef NES_PRG_BANK_BASE
  pop af
  ld  ($ffff), a
.endif
  pop af
  ld  ($cb27), a
  pop bc
  ld  a, b
  ld  ($cb15), a
  ld  a, c
  ld  ($cb18), a
  pop bc
  pop af
  pop hl
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
  ;
  ; OVERRUN GUARD (2026-07-05, field report): the split protocol writes the
  ; pre/status-bar scroll (0 for SMB) and relies on the line IRQ to switch to
  ; the playfield scroll. On frames where the handler overran, presentation
  ; runs mid-display and the armed line IRQ gets swallowed by the pacing
  ; read — those frames rendered END-TO-END with the status-bar scroll,
  ; alternating with correct frames ("two levels on top of each other,
  ; shifted"). Read the V-counter: outside VBlank, skip the split and write
  ; the playfield scroll directly — the HUD wobbles on those frames instead
  ; of the whole level double-imaging.
  in  a, ($7e)              ; V-counter
  cp  $e0
  jr  c, _apply_playfield_direct
  ; Legacy adaptive split suppression: if the previous handler overran, this one
  ; almost certainly will too, and its armed split would be swallowed.
  ; Present the playfield scroll directly instead (HUD scrolls with the
  ; camera on those frames — stable, no double image). At full speed the
  ; flag stays clear and the fixed-HUD split path returns automatically.
  ; SMB instead polls blocked splits and repeats its HUD on consumed VBlanks.
.ifndef SMB_RUNTIME_HOOKS
  ld  a, ($cb29)
  or  a
  jr  nz, _apply_playfield_direct
.endif
  ld  a, ($cb20)
  bit 2, a
  jr  nz, _apply_frame_split_scroll
  call _apply_pre_or_live_scroll
  jp  _disable_line_irq

_apply_playfield_direct:
  ; Out-of-vblank presentation: apply the last post/playfield pair when one
  ; exists (bit2 is sticky — see _present_skip_all), else the live latch
  ; pair; never arm the line IRQ.
  ld  a, ($cb20)
  bit 2, a
  jr  z, _apd_live
  ld  a, ($cb23)
  ld  c, a
  ld  a, ($cb24)
  jp  _apply_scroll_pair_then_disable
_apd_live:
  ld  a, ($cb0c)
  ld  c, a
  ld  a, ($cb0d)
  jp  _apply_scroll_pair_then_disable

_apply_frame_split_scroll:
.ifdef NO_SCROLL_SPLIT
  ; Profile disabled the split: no status band is presented, so the pre/top
  ; pair has no on-screen meaning — show the playfield scroll (post pair
  ; when one exists, else the live latch) and keep line IRQs off.
  ; Line-counter/pending semantics differ across emulators (GPGX latched
  ; pending line IRQs into a storm that starved the frame handler on CV1).
  jp  _apply_playfield_direct
.else
.ifdef SMB_RUNTIME_HOOKS
  ; SMB's first playfield row is 32. Presentation remains DI; service the
  ; horizontal split at closed VRAM transactions, or via IRQ after EI.
  ; This avoids both late-arm HUD wobble and a swallowed hardware line IRQ.
  ; Freeze the scroll that column projection and SAT are presenting together.
  ld a, ($cb23)
  ld ($d472), a
_smb_hud_rearm:
  call _disable_line_irq
  ; Bit1 is cleared before each translated NMI. A physical frame can arrive
  ; before that NMI has rewritten its pre pair; the live latch is then still
  ; the playfield scroll. Sticky post-valid implies SMB already supplied a
  ; complete split, so retain its previous pre pair instead of the live latch.
  call _apply_pre_scroll
  ld a, 1
  ld ($d473), a
  ld a, 31
  ld b, 10
  call vdp_set_register
  jp _enable_line_irq
.else
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
  jp  _enable_line_irq
.endif
.endif

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
  ; Stackless: IRQ presentation can run at native-stack low water.
  ld  e, a
  ld  a, c
  neg
  out ($bf), a
  ld  a, $88                ; VDP reg 8 = horizontal scroll
  out ($bf), a
  ld  a, e
  out ($bf), a
  ld  a, $89                ; VDP reg 9 = vertical scroll
  out ($bf), a
  ld  a, e                  ; keep old helper's returned A = scroll Y
  ret

_apply_scroll_pair_then_disable:
  ; Entry: C = NES scroll X, A = NES scroll Y.
  ; Tail target for direct/out-of-vblank presentation: write the playfield
  ; scroll pair and restore VDP reg0 without spending another return slot.
  ld  e, a
.ifdef SMB_RUNTIME_HOOKS
  xor a
  ld ($d473), a
.endif
  ld  a, c
  neg
  out ($bf), a
  ld  a, $88
  out ($bf), a
  ld  a, e
  out ($bf), a
  ld  a, $89
  out ($bf), a
  ld  a, VDP_R0_BASE
  out ($bf), a
  ld  a, $80                ; VDP reg 0
  out ($bf), a
  ld  a, e
  ret

_enable_line_irq:
  ; VDP reg0 bit 4 enables line interrupts on top of the base display mode.
  ld  a, VDP_R0_LINE_IRQ_ON
  out ($bf), a
  ld  a, $80                ; VDP reg 0
  out ($bf), a
  ret

_disable_line_irq:
  ; Restore the base R0 mode with line interrupts disabled, and park the
  ; line counter at $FF: the VDP decrements it every scanline regardless
  ; of IE1 and LATCHES pending on underflow — a small parked value made
  ; some emulators (GPGX) re-fire immediately at the next enable.
.ifdef SMB_RUNTIME_HOOKS
  xor a
  ld ($d473), a
.endif
  ld  a, VDP_R0_BASE
  out ($bf), a
  ld  a, $80                ; VDP reg 0
  out ($bf), a
  ld  a, $ff
  out ($bf), a
  ld  a, $8a                ; VDP reg 10 = line counter reload
  out ($bf), a
  ret

.ifdef SMB_RUNTIME_HOOKS
; DI, closed VDP transaction, no live VRAM stream. Clobbers AF only: callers
; poll between cells/attributes or sprite variants, before setting an address.
; R8 written during line31 takes effect for the playfield starting at line32.
rt_smb_hud_poll:
  ld a, ($d473)
  or a
  ret z
  in a, ($7e)
  cp 31
  ret c
  cp $e0
  ret nc
rt_smb_hud_line:
  ld a, ($d473)
  or a
  ret z
_smb_hud_post:
  ld a, ($d472)
  neg
  out ($bf), a
  ld a, $88
  out ($bf), a
  jp _disable_line_irq

; Pacing consumed a VINT, but no new SAT/columns have been presented. Rearm
; only in blank and retain D472: installing a newer live post pair would tear
; that old playfield. Never restart pre-scroll halfway through active video.
rt_smb_hud_repeat:
.ifndef NO_SCROLL_SPLIT
  in a, ($7e)
  cp $e0
  ret c
  ld a, ($cb20)
  bit 2, a
  ret z
  jp _smb_hud_rearm
.else
  ret
.endif
.endif
.endif ; legacy IRQ/presentation policy

.ends

; ─── Slot-2 transaction guard ────────────────────────────────────────────────
; Cold outer-transaction guard for routines that temporarily map slot 2.  The
; hot loops remain inline; callers enter once and exit once.  Frames save IFF2,
; SRAM control ($FFFC), slot-2 bank ($FFFF), and the NES PRG-bank shadow
; ($CB62).  Preserves BC/DE/HL, clobbers AF, and uses no native pushes.
.section "slot2_guard" free
rt_slot2_guard_enter:
  ld  a, i
  di
  ld  a, ($d47f)
  jp  po, _slot2_guard_enter_di
  cp  2
  jp  nc, _slot2_guard_overflow
  or  a
  jr  nz, _slot2_guard_enter_1_ei
  ld  a, $01
  ld  ($ca19), a
  jr  _slot2_guard_enter_snapshot_0
_slot2_guard_enter_1_ei:
  ld  a, $01
  ld  ($ca1d), a
  jr  _slot2_guard_enter_snapshot_1
_slot2_guard_enter_di:
  cp  2
  jp  nc, _slot2_guard_overflow
  or  a
  jr  nz, _slot2_guard_enter_1_di
  xor a
  ld  ($ca19), a
  jr  _slot2_guard_enter_snapshot_0
_slot2_guard_enter_1_di:
  xor a
  ld  ($ca1d), a
  jr  _slot2_guard_enter_snapshot_1
_slot2_guard_enter_snapshot_0:
  ld  a, ($fffc)
  ld  ($ca1a), a
  ld  a, ($ffff)
  ld  ($ca1b), a
  ld  a, ($cb62)
  ld  ($ca1c), a
  jr  _slot2_guard_enter_done
_slot2_guard_enter_snapshot_1:
  ld  a, ($fffc)
  ld  ($ca1e), a
  ld  a, ($ffff)
  ld  ($ca1f), a
  ld  a, ($cb62)
  ld  ($d3ff), a
_slot2_guard_enter_done:
  ld  a, ($d47f)
  inc a
  ld  ($d47f), a
  ret

; Exact exit: forces ROM visibility before restoring the saved slot-2 state;
; returns saved IFF2 in A and deliberately leaves interrupts disabled.
rt_slot2_guard_exit_exact_di:
  ld  a, ($d47f)
  or  a
  jp  z, _slot2_guard_underflow
  cp  3
  jp  nc, _slot2_guard_corrupt
  dec a
  ld  ($d47f), a
  jr  nz, _slot2_guard_exit_1
  xor a
  ld  ($fffc), a
  ld  a, ($ca1c)
  ld  ($cb62), a
  ld  a, ($ca1b)
  ld  ($ffff), a
  ld  a, ($ca1a)
  ld  ($fffc), a
  ld  a, ($ca19)
  ret
_slot2_guard_exit_1:
  xor a
  ld  ($fffc), a
  ld  a, ($d3ff)
  ld  ($cb62), a
  ld  a, ($ca1f)
  ld  ($ffff), a
  ld  a, ($ca1e)
  ld  ($fffc), a
  ld  a, ($ca1d)
  ret
_slot2_guard_overflow:
  ld  a, RT_GUARD_OVERFLOW
  jr  _slot2_guard_trap
_slot2_guard_underflow:
  ld  a, RT_GUARD_UNDERFLOW
  jr  _slot2_guard_trap
_slot2_guard_corrupt:
  ld  a, RT_GUARD_CORRUPT
_slot2_guard_trap:
  di
  ld  ($cb1d), a
_slot2_guard_halt:
  halt
  jr  _slot2_guard_halt
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
