; hooks_smb.s — SMB-specific native replacement routines (Phase S2).
;
; Guarded by SMB_RUNTIME_HOOKS (emitted from profiles/smb.toml via
; `[translation] runtime_defines`); other games assemble this file to
; nothing. Each routine replaces a translated SMB routine through the
; profile `[[replacement]]` mechanism and must reproduce the original's
; observable effects exactly: RAM writes, and any register/flag exit
; state a caller consumes. Verified against the frame-diff byte-parity
; oracle on the acceptance routes.
;
; Register contract (Phase R): A = 6502 A, D = X, E = Y; replacements are
; entered by native `call` and return with `ret`.

.ifdef SMB_RUNTIME_HOOKS
.section "hooks_smb" free

; ─── rt_smb_top_score_pair ────────────────────────────────────────────────────
; Replaces NES $8F97: TopScoreCheck run twice — `LDX #$05 / JSR $8F9E /
; LDX #$0B` falling through into TopScoreCheck ($8F9E):
;   LDY #$05 / SEC
;   L: LDA $07DD,X / SBC $07D7,Y / DEX / DEY / BPL L
;   BCC done
;   INX / INY
;   C: LDA $07DD,X / STA $07D7,Y / INX / INY / CPY #$06 / BCC C
;   done: RTS
; The digit-wise SBC chain threads borrow across all six digits; the Z80
; sbc chain threads the same borrow with inverted carry polarity, so the
; 6502's exit BCC (C = 0, top score still higher) is Z80 `ret c`.
;
; Exit-state contract: the single caller (NMI $80ED) immediately runs
; `LDA $0776 / LSR` which overwrites A/N/Z/C, reloads X/Y before use
; ($8100 LDX #$14 / $811B LDX #$00, LDY #$07), and SMB contains no
; BVC/BVS, so the exit flags and A are architecturally dead. D/E are left
; at the no-copy exit shape of the second pass.
rt_smb_top_score_pair:
  ld   d, $05
  call _tsc_pass
  ld   d, $0b
  call _tsc_pass
  ld   d, $05                ; X = $0B - 6 (second pass, no-copy exit)
  ld   e, $ff
  ret

_tsc_pass:                   ; in: D = entry X; clobbers A/B/C/E/HL
  ld   a, $dd
  add  a, d
  ld   c, a                  ; C = player-digit ptr low ($C7DD+X), descending
  ld   e, $dc                ; E = top-digit ptr low ($C7D7+5), descending
  ld   h, $c7
  ld   b, $06
  or   a                     ; SEC: clear the Z80 borrow
_tsc_cmp:
  ld   l, c
  ld   a, (hl)               ; player digit (ld preserves carry)
  ld   l, e
  sbc  a, (hl)               ; borrow threads across the six digits
  dec  c
  dec  e                     ; dec r preserves carry
  djnz _tsc_cmp
  ret  c                     ; 6502 BCC: borrow — top score still higher
  ; New top score: copy the six digits ascending.
  inc  c
  inc  e
  ld   b, $06
_tsc_copy:
  ld   l, c
  ld   a, (hl)
  ld   l, e
  ld   (hl), a
  inc  c
  inc  e
  djnz _tsc_copy
  ret

; ─── rt_smb_sprite_shuffler ───────────────────────────────────────────────────
; Replaces NES $81C6 SpriteShuffler:
;   zp $00 = $28 (game-visible scratch)
;   pass 1: for X = $0E..0: if SprDataOffset[X] ($06E4+X) >= $28:
;             += SprShuffleAmt[$06E1 + ($06E0)]; if that carried, += $28.
;   $06E0 = ($06E0 + 1) mod 3
;   pass 2: X=8, Y=2 down to 0: base = ($06E9+Y);
;           $06F1+X = base; $06F2+X = base+8; $06F3+X = base+16; X -= 3.
; Exit state: A = last stored value (base+16 of the Y=0 group),
; X = Y = $FF, shadow N set, Z clear, C = carry of the final `+8`.
; (V is never read: SMB has no BVC/BVS.)
rt_smb_sprite_shuffler:
  ld   a, $28
  ld   ($c000), a            ; STA $00
  ld   h, $c6
  ld   a, ($c6e0)            ; SprShuffleAmtOffset
  add  a, $e1
  ld   l, a
  ld   c, (hl)               ; C = SprShuffleAmt[offset] (loop-invariant)
  ld   b, $0f
  ld   l, $f2                ; $06E4 + $0E
_ss_p1:
  ld   a, (hl)
  cp   $28
  jr   c, _ss_p1_next
  add  a, c                  ; CLC / ADC amt
  jr   nc, _ss_p1_store
  add  a, $28                ; CLC / ADC $00 (= $28)
_ss_p1_store:
  ld   (hl), a
_ss_p1_next:
  dec  l
  djnz _ss_p1
  ; SprShuffleAmtOffset = (SprShuffleAmtOffset + 1) mod 3
  ld   a, ($c6e0)
  inc  a
  cp   $03
  jr   nz, _ss_keep
  xor  a
_ss_keep:
  ld   ($c6e0), a
  ; Pass 2: misc sprite data offset groups.
  ld   e, $02                ; Y
  ld   c, $f9                ; $06F1 + 8
_ss_p2:
  ld   a, $e9
  add  a, e
  ld   l, a
  ld   a, (hl)               ; base = SprDataOffset group entry
  ld   l, c
  ld   (hl), a
  add  a, $08
  inc  l
  ld   (hl), a
  add  a, $08                ; final add: its carry is the 6502 exit C
  ld   b, $00
  rl   b                     ; B = exit carry bit (captured before inc/dec)
  inc  l
  ld   (hl), a
  dec  c
  dec  c
  dec  c
  dec  e
  jp   p, _ss_p2
  ; Exit: X = 8 - 9 = $FF, Y already $FF, A = last stored value.
  ld   d, $ff
  ld   c, a                  ; save A
  ld   a, ($cb03)
  and  $7c                   ; clear N/Z/C, keep V and the rest
  or   b                     ; C from the final add
  or   $80                   ; N set (final DEY), Z clear
  ld   ($cb03), a
  ld   a, c
  ret

; ─── rt_smb_read_joypads ──────────────────────────────────────────────────────
; Replaces NES $8E5C ReadJoypads: strobe $4016 high/low, then serially read
; 8 bits per port through ReadPortBits ($8E6A), building the NES button
; byte MSB-first (bit7 = A ... bit0 = Right), with the Select/Start
; debounce: if (new & $30) & prev($074A+X) then $06FC+X = new & $CF and
; $074A+X keeps its old value; otherwise both get `new`.
;
; This runtime latches the pad once per VBlank into $CB06 (bit0 = A ...
; bit7 = Right — the serial-read order), so the built byte is exactly the
; bit-reverse of the latch. Port 1 always reads 0 here. The strobe's only
; runtime effect is resetting the serial index; the translated routine
; leaves it at 8 (all port-0 bits consumed), which we set directly.
;
; Exit state: zp $00 = 0 (last raw port-1 read), X = 1, Y = 0, A = 0.
; Flags are dead: the caller (NMI $80E7) runs PauseRoutine next, which
; opens with LDA/CMP.
rt_smb_read_joypads:
  ld   a, ($cb06)
  ld   b, a
  xor  a
  .REPEAT 8
  rr   b
  rla
  .ENDR
  ld   c, a                  ; C = joy0, MSB-first NES order
  ld   a, ($c74a)            ; previous JoypadBitMask
  and  c
  and  $30                   ; Select/Start held since last frame?
  jr   z, _rj_fresh
  ld   a, c
  and  $cf                   ; strip Select/Start from the saved bits
  ld   ($c6fc), a
  jr   _rj_port1
_rj_fresh:
  ld   a, c
  ld   ($c6fc), a
  ld   ($c74a), a
_rj_port1:
  xor  a                     ; port 1 is unconnected: all bits 0
  ld   ($c6fd), a
  ld   ($c74b), a
  ld   ($c000), a            ; zp $00: last raw serial read (port 1) = 0
  ld   d, $01                ; X after the fall-through second pass
  ld   e, $00                ; Y counted down to zero
  ld   b, $08
  ld   a, b
  ld   ($cb07), a            ; serial index: port-0 bits fully consumed
  xor  a                     ; exit A = value last stored ($074B) = 0
  ret

; ─── rt_smb_block_buffer_collision ────────────────────────────────────────────
; Replaces NES $E3F0 BlockBufferCollision (and, through the JMP-replacement
; edge, the fall-through wrapper entries), including its inner JSR $9BE1
; GetBlockBufferAddr. Original semantics:
;   PHA (flag) / $04 = Y
;   xsum = XAdder[$E3B0+Y] + SprObject_X[$86+X]  -> $05  (carry K)
;   col  = (((PageLoc[$6D+X]+K) & 1) << 4) | (xsum >> 4)      ; 0..31
;   GetBlockBufferAddr: PHA(col); Y2 = col>>4;
;     $07 = [$9BDF+Y2]; $06 = (col & $0F) + [$9BDD+Y2]
;   ysum = (SprObject_Y[$CE+X] + YAdder[$E3CC+Y]) & $F0 - $20 -> $02 (SEC/SBC)
;   $03 = ($06),ysum   (block-buffer tile)
;   Y restored from $04; PLA(flag): pos = flag ? $86+X : $CE+X; $04 = pos & $0F
;   A = $03; RTS
; Byte-exact RAM effects reproduced: zp $02-$07, and the two emulated-stack
; slot bytes the PHAs leave behind ((S) = flag, (S-1) = col; S net-unchanged).
; Exit: A = block value, D untouched, E = entry Y; shadow N/Z from the block
; value, shadow C from the SEC/SBC (set iff (ysum & $F0) >= $20). V dead
; (SMB has no BVC/BVS). $CA23 scratch is safe: SMB game code runs only at
; translated-NMI depth 1 (nested entries are gated off by PPUCTRL bit 7),
; so this hook is never re-entered.
rt_smb_block_buffer_collision:
  ld   ($ca23), a            ; flag
  ld   a, e
  ld   ($c004), a            ; STY $04
  ; ROM adders live in fixed-high PRG: map, read both, restore.
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   h, $a3                ; NES $E3B0 -> slot-2 $A3B0
  ld   a, $b0
  add  a, e
  ld   l, a
  jr   nc, _bbc_xa
  inc  h
_bbc_xa:
  ld   c, (hl)               ; C = x adder
  ld   h, $a3
  ld   a, $cc                ; NES $E3CC -> $A3CC
  add  a, e
  ld   l, a
  jr   nc, _bbc_ya
  inc  h
_bbc_ya:
  ld   a, (hl)               ; y adder
  ld   ($c002), a            ; park in $02 (overwritten with its final value below)
  ld   a, :data_prg_low
  ld   ($ffff), a
  ; xsum = xpos + xadder (carry K); $05 = xsum
  ld   a, $86
  add  a, d
  ld   l, a
  ld   h, $c0
  ld   a, (hl)
  add  a, c                  ; A = xsum, CY = K
  ld   ($c005), a
  ld   c, a                  ; C = xsum (ld preserves CY)
  ld   b, $00
  rl   b                     ; B = K
  ; pagebit = (PageLoc + K) & 1
  ld   a, $6d
  add  a, d
  ld   l, a
  ld   a, (hl)
  add  a, b
  and  $01
  rlca
  rlca
  rlca
  rlca                       ; A = pagebit << 4
  ld   l, a
  ld   a, c
  rrca
  rrca
  rrca
  rrca
  and  $0f                   ; xsum >> 4
  or   l
  ld   c, a                  ; C = col (0..31)
  ; The two PHAs' emulated-stack bytes: (S) = flag, (S-1) = col.
  ld   a, ($cb02)
  ld   l, a
  ld   h, $c1
  ld   a, ($ca23)
  ld   (hl), a
  dec  l                     ; page-wrapping, like the 6502 stack
  ld   (hl), c
  ; Y2 = col >> 4 (0/1); block-buffer pointer $06/$07 from PRG-low tables.
  ld   a, c
  and  $10
  rrca
  rrca
  rrca
  rrca
  ld   b, a                  ; B = Y2
  ld   h, $9b
  ld   a, $df
  add  a, b
  ld   l, a
  ld   a, (hl)
  ld   ($c007), a            ; pointer high
  ld   a, $dd
  add  a, b
  ld   l, a
  ld   a, c
  and  $0f
  add  a, (hl)
  ld   ($c006), a            ; pointer low
  ; ysum = (ypos + yadder) & $F0 - $20; capture 6502 C (set iff no borrow).
  ld   a, $ce
  add  a, d
  ld   l, a
  ld   h, $c0
  ld   b, (hl)               ; ypos
  ld   hl, $c002
  ld   a, b
  add  a, (hl)
  and  $f0
  sub  $20                   ; CY = borrow
  ld   b, $00
  ccf                        ; CY = 6502 carry
  rl   b                     ; B = shadow-C bit
  ld   ($c002), a            ; $02 final
  ; block = (pointer),ysum — NES RAM pointer remapped to the $C000 shadow.
  ld   c, a                  ; C = index (the TAY value)
  ld   a, ($c006)
  add  a, c
  ld   l, a
  ld   a, ($c007)
  adc  a, $c0
  ld   h, a
  ld   a, (hl)
  ld   ($c003), a            ; $03 = block value
  ; LDY $04: restore entry Y.
  ld   a, ($c004)
  ld   e, a
  ; PLA(flag): $04 = (flag ? xpos : ypos) & $0F
  ld   a, ($ca23)
  or   a
  ld   a, $ce
  jr   z, _bbc_pos
  ld   a, $86
_bbc_pos:
  add  a, d
  ld   l, a
  ld   h, $c0
  ld   a, (hl)
  and  $0f
  ld   ($c004), a
  ; Exit: A = block; shadow N/Z from it, shadow C from B.
  ld   a, ($c003)
  ld   l, a
  ld   h, $3e
  ld   a, ($cb03)
  and  $7c
  or   b
  or   (hl)
  ld   ($cb03), a
  ld   a, l
  ret

; ─── rt_smb_render_attr_tables ────────────────────────────────────────────────
; Replaces NES $88AE (doppelganger import names it AreaParserNoOp; the body
; is the attribute-table renderer): builds a $9A-tagged VRAM_Buffer2 record
; of 13 metatile pairs from the attribute buffer ($06A1+X), packs the
; 2-bit palette groups into $03F9+Y, advances $0721/$0720, and tail-jumps
; to SetVRAMCtrl ($89BD: $0773 = 6, RTS). Reached via computed dispatch
; (the JMP ($06) JumpEngine), so the dispatch table redirects here.
;
; Byte-exact RAM effects: zp $00-$05 ($06/$07 = metatile gfx pointer),
; VRAM_Buffer2 header+pairs at $0341+, $03F9+Y attr packs, $0340, $0721,
; $0720, $0773. Exit: A = $06, X = $0D, Y = entry($0340)+29 & $FF,
; shadow C set (CPX #$0D at loop exit), N/Z clear (LDA #$06). V dead.
rt_smb_render_attr_tables:
  ld   a, ($c726)
  and  $01
  ld   ($c005), a
  ld   a, ($c340)            ; VRAM_Buffer2 offset
  ld   e, a
  ld   ($c000), a
  ld   c, a
  ; record header: ($0342+Y) = ($0721), ($0341+Y) = ($0720), ($0343+Y) = $9A
  ld   a, ($c721)
  ld   b, a
  ld   a, $42
  add  a, c
  ld   l, a
  ld   a, $c3
  adc  a, $00
  ld   h, a
  ld   (hl), b
  ld   a, ($c720)
  ld   b, a
  ld   a, $41
  add  a, c
  ld   l, a
  ld   a, $c3
  adc  a, $00
  ld   h, a
  ld   (hl), b
  ld   a, $43
  add  a, c
  ld   l, a
  ld   a, $c3
  adc  a, $00
  ld   h, a
  ld   (hl), $9a
  xor  a
  ld   ($c004), a
  ld   d, a                  ; X = 0
_rag_loop:
  ld   a, d
  ld   ($c001), a
  ; attr = ($06A1+X); $03 = attr & $C0; quadrant = attr >> 6
  ld   a, $a1
  add  a, d
  ld   l, a
  ld   h, $c6
  ld   b, (hl)               ; B = raw attr
  ld   a, b
  and  $c0
  ld   ($c003), a
  rlca
  rlca                       ; A = attr >> 6 (ASL/ROL/ROL on the 6502)
  ld   c, a
  ; metatile gfx pointer: $06 = ($8B08+q), $07 = ($8B0C+q) — PRG low, direct
  ld   h, $8b
  ld   a, $08
  add  a, c
  ld   l, a
  ld   a, (hl)
  ld   ($c006), a
  ld   a, $0c
  add  a, c
  ld   l, a
  ld   a, (hl)
  ld   ($c007), a
  ; $02 = attr << 2; Y = ((($071F)&1)^1)<<1 + $02
  ld   a, b
  add  a, a
  add  a, a
  ld   ($c002), a
  ld   c, a
  ld   a, ($c71f)
  and  $01
  xor  $01
  add  a, a
  add  a, c
  ld   e, a                  ; E = tile-pair index
  ; tile pair = (ptr+E), (ptr+E+1) — ROM pages, direct slot-2 reads
  ld   a, ($c006)
  add  a, e
  ld   l, a
  ld   a, ($c007)
  adc  a, $00
  ld   h, a
  ld   b, (hl)               ; tile 0
  inc  hl
  ld   a, (hl)               ; tile 1
  ; write to ($0344 + bufofs), ($0345 + bufofs)
  ld   c, a
  ld   a, ($c000)
  ld   l, a
  ld   h, $00
  ld   a, $44
  add  a, l
  ld   l, a
  ld   a, $c3
  adc  a, h
  ld   h, a
  ld   (hl), b
  inc  hl
  ld   (hl), c
  ; palette-group packing paths
  ld   a, ($c004)
  ld   e, a                  ; LDY $04
  ld   a, ($c005)
  or   a
  jr   nz, _rag_p1
  ld   a, ($c001)
  rrca                       ; CY = column & 1
  jr   c, _rag_lsr2
  ld   hl, $c003             ; ROL $03 x3 (carry-in 0 on this path)
  rl   (hl)
  rl   (hl)
  rl   (hl)
  jr   _rag_attr
_rag_p1:
  ld   a, ($c001)
  rrca
  jr   c, _rag_inc4
  ld   hl, $c003             ; LSR $03 x4
  srl  (hl)
  srl  (hl)
  srl  (hl)
  srl  (hl)
  jr   _rag_attr
_rag_lsr2:
  ld   hl, $c003             ; LSR $03 x2, then fall into INC $04
  srl  (hl)
  srl  (hl)
_rag_inc4:
  ld   hl, $c004
  inc  (hl)
_rag_attr:
  ; ($03F9+Y) |= $03
  ld   a, $f9
  add  a, e
  ld   l, a
  ld   a, $03
  adc  a, $c0
  ld   h, a
  ld   a, ($c003)
  or   (hl)
  ld   (hl), a
  ; $00 += 2; X = ($01)+1; loop while X < $0D
  ld   hl, $c000
  inc  (hl)
  inc  (hl)
  ld   a, ($c001)
  inc  a
  ld   d, a
  cp   $0d
  jp   c, _rag_loop
  ; trailer: Y = ($00)+3; ($0341+Y) = 0; ($0340) = Y
  ld   a, ($c000)
  add  a, $03
  ld   e, a
  ld   a, $41
  add  a, e
  ld   l, a
  ld   a, $c3
  adc  a, $00
  ld   h, a
  ld   (hl), $00
  ld   a, e
  ld   ($c340), a
  ; column counter: INC $0721; every 32 columns flip nametable select
  ld   hl, $c721
  inc  (hl)
  ld   a, (hl)
  and  $1f
  jr   nz, _rag_tail
  ld   (hl), $80
  ld   a, ($c720)
  xor  $04
  ld   ($c720), a
_rag_tail:
  ; SetVRAMCtrl tail: ($0773) = 6. Exit A = 6, X = $0D; shadow C set,
  ; N/Z clear.
  ld   a, $06
  ld   ($c773), a
  ld   d, $0d
  ld   a, ($cb03)
  and  $7c
  or   $01
  ld   ($cb03), a
  ld   a, $06
  ret

; ─── rt_smb_get_x_offscreen / rt_smb_get_y_offscreen ─────────────────────────
; Replace NES $F1F6 (GetXOffscreenBits), $F239 (GetYOffscreenBits) and
; their shared $F26D divider. Per axis, for Y = 1 then 0:
;   diff = (edge[$071A/$071C + Y] or the $F237 consts) - object position
;   (16-bit SEC/SBC chain; lo diff -> $07)
;   idx  = [in-bounds table + Y]; if page diff < 0 keep [base table + Y];
;   if page diff == 0: $06 = $38/$20, $05 = 8/4, and when the lo diff is
;   inside $06 the divider turns (diff >> 3) & 7 (+ $05 when Y = 0) into idx
;   mask = [mask table + idx]; loop while mask == 0
; ROM tables are fixed-high PRG: mapped once per hook, restored at exit
; (all other accesses are zp/RAM, unaffected by the slot-2 window).
;
; Byte-exact RAM: $04 (entry X), $07 every iteration, $05/$06 on the
; compute path. Exit: A = mask, D untouched, E = Y at exit (1/0, or $FF
; when both iterations produced mask 0); shadow C set (CMP #0), Z clear
; in every path (the exhausted path's DEY leaves Y=$FF), N = bit 7 of the
; mask, or set when exhausted. V dead (no BVC/BVS in SMB).
rt_smb_get_x_offscreen:
  ld   a, d
  ld   ($c004), a
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   e, $01
_gxo_loop:
  ; 16-bit diff: edge($071C+Y | $071A+Y) - pos($86+X | $6D+X)
  ld   a, $86
  add  a, d
  ld   l, a
  ld   h, $c0
  ld   c, (hl)               ; pos lo
  ld   a, $6d
  add  a, d
  ld   l, a
  ld   b, (hl)               ; pos hi (page)
  ld   a, $1c
  add  a, e
  ld   l, a
  ld   h, $c7
  ld   a, (hl)
  sub  c                     ; lo diff, CY = borrow
  ld   ($c007), a
  dec  l
  dec  l                     ; $071A + Y (dec preserves CY)
  ld   a, (hl)
  sbc  a, b                  ; A = page diff
  ld   c, a
  ; idx select from the fixed-high tables ($F1F3/$F1F4 -> $B1F3/$B1F4)
  ld   h, $b1
  ld   a, $f3
  add  a, e
  ld   l, a
  ld   b, (hl)               ; idx = [$F1F3+Y]
  ld   a, c
  or   a
  jp   m, _gxo_skip          ; page diff < 0
  inc  l
  ld   b, (hl)               ; idx = [$F1F4+Y]
  dec  a
  jp   p, _gxo_skip          ; page diff >= 1
  ld   a, $38
  ld   ($c006), a
  ld   a, $08
  call _osb_div
_gxo_skip:
  ld   h, $b1                ; masks $F1E3 -> $B1E3
  ld   a, $e3
  add  a, b
  ld   l, a
  ld   a, (hl)
  or   a
  jr   nz, _osb_done
  dec  e
  jp   p, _gxo_loop
  jr   _osb_exhaust

; Shared divider ($F26D DividePDiff): in A = $05 arg, B = current idx,
; E = current Y; out B = idx. Reads $06/$07, writes $05.
_osb_div:
  ld   ($c005), a
  ld   hl, $c006
  ld   a, ($c007)
  cp   (hl)
  ret  nc                    ; diff outside the span: keep idx
  srl  a
  srl  a
  srl  a
  and  $07
  ld   c, a
  ld   a, e
  cp   $01
  ld   a, c
  jr   nc, _osb_div_set      ; Y >= 1: no offset
  ld   hl, $c005
  add  a, (hl)
_osb_div_set:
  ld   b, a
  ret

_osb_exhaust:
  ; both iterations yielded 0: A = 0, Y = $FF, shadow N set.
  ld   a, ($cb03)
  and  $7c
  or   $81                   ; N + C
  ld   ($cb03), a
  jr   _osb_ret0
_osb_done:
  ; A = mask (non-zero): shadow N from bit 7, Z clear, C set.
  ld   c, a
  ld   l, a
  ld   h, $3e
  ld   a, ($cb03)
  and  $7c
  or   $01                   ; C
  or   (hl)                  ; N/Z bits (Z never set: mask != 0)
  ld   ($cb03), a
  ld   a, :data_prg_low
  ld   ($ffff), a
  ld   a, c
  ret
_osb_ret0:
  ld   a, :data_prg_low
  ld   ($ffff), a
  xor  a
  ret

rt_smb_get_y_offscreen:
  ld   a, d
  ld   ($c004), a
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   e, $01
_gyo_loop:
  ld   a, $ce
  add  a, d
  ld   l, a
  ld   h, $c0
  ld   c, (hl)               ; y pos lo
  ld   a, $b5
  add  a, d
  ld   l, a
  ld   b, (hl)               ; y hi
  ld   h, $b2                ; consts $F237 -> $B237
  ld   a, $37
  add  a, e
  ld   l, a
  ld   a, (hl)
  sub  c
  ld   ($c007), a
  ld   a, $01
  sbc  a, b                  ; A = page diff
  ld   c, a
  ld   h, $b2                ; tables $F234/$F235 -> $B234/$B235
  ld   a, $34
  add  a, e
  ld   l, a
  ld   b, (hl)
  ld   a, c
  or   a
  jp   m, _gyo_skip
  inc  l
  ld   b, (hl)
  dec  a
  jp   p, _gyo_skip
  ld   a, $20
  ld   ($c006), a
  ld   a, $04
  call _osb_div
_gyo_skip:
  ld   h, $b2                ; masks $F22B -> $B22B
  ld   a, $2b
  add  a, b
  ld   l, a
  ld   a, (hl)
  or   a
  jr   nz, _osb_done
  dec  e
  jp   p, _gyo_loop
  jr   _osb_exhaust

; ─── rt_smb_draw_sprite_pair ──────────────────────────────────────────────────
; Replaces NES $F282 (entered by tail JMP): writes one 8x16 sprite pair
; into OAM staging from zp inputs — $00/$01 tiles (order swapped when
; $03 bit1 = horizontal flip, attr base $40), $04 attr bits, $02 Y
; coordinate (both halves), $05 X (and X+8) — then $02 += 8, Y += 8,
; X += 2, RTS. All eight OAM writes stay within the $0200 page.
; Exit: A = Y+8 (the new Y), E = Y+8, D = X+2; shadow N/Z from the new X
; (final INX), shadow C = carry of Y+8. V dead.
rt_smb_draw_sprite_pair:
  ld   a, ($c003)
  rrca
  rrca                       ; CY = flip bit ($03 bit 1 — LSR LSR on the 6502)
  ld   a, ($c000)
  ld   b, a                  ; tile0 (ld preserves CY)
  ld   a, ($c001)
  ld   c, a                  ; tile1
  jr   c, _dsp_flip
  ld   a, $01
  add  a, e
  ld   l, a
  ld   h, $c2
  ld   (hl), b               ; $0201+Y = tile0
  ld   a, $05
  add  a, e
  ld   l, a
  ld   (hl), c               ; $0205+Y = tile1
  xor  a
  jr   _dsp_attr
_dsp_flip:
  ld   a, $05
  add  a, e
  ld   l, a
  ld   h, $c2
  ld   (hl), b               ; $0205+Y = tile0
  ld   a, $01
  add  a, e
  ld   l, a
  ld   (hl), c               ; $0201+Y = tile1
  ld   a, $40
_dsp_attr:
  ld   hl, $c004
  or   (hl)
  ld   b, a                  ; attr byte
  ld   a, $02
  add  a, e
  ld   l, a
  ld   h, $c2
  ld   (hl), b
  ld   a, $06
  add  a, e
  ld   l, a
  ld   (hl), b
  ld   a, ($c002)
  ld   b, a                  ; Y coordinate
  ld   l, e
  ld   (hl), b               ; $0200+Y
  ld   a, $04
  add  a, e
  ld   l, a
  ld   (hl), b
  ld   a, ($c005)
  ld   c, a                  ; X coordinate
  ld   a, $03
  add  a, e
  ld   l, a
  ld   (hl), c
  ld   a, c
  add  a, $08
  ld   c, a
  ld   a, $07
  add  a, e
  ld   l, a
  ld   (hl), c               ; X+8
  ld   hl, $c002
  ld   a, (hl)
  add  a, $08
  ld   (hl), a               ; $02 += 8
  ld   a, e
  add  a, $08
  ld   e, a                  ; Y += 8; CY = exit carry
  ld   b, $00
  rl   b
  inc  d
  inc  d                     ; X += 2 (exit N/Z source)
  ld   c, a
  ld   a, d
  ld   l, a
  ld   h, $3e
  ld   a, ($cb03)
  and  $7c
  or   b
  or   (hl)
  ld   ($cb03), a
  ld   a, c
  ret

; ─── rt_smb_sound_engine ──────────────────────────────────────────────────────
; Replaces NES $F2D0 SoundEngine's per-frame entry with a native shell.
; Strategy (Phase S sound stage 1): the steady-frame scaffolding is native;
; anything eventful delegates to the translated engine at a routine
; boundary, so rare paths keep translation-perfect fidelity:
;   - OperMode == 0: APU $4015 = 0, return (silence path).
;   - pause transitions ($07C6 != 0 or $FA == 1): tail-delegate the whole
;     engine to translated L_F2D0 (reads-only up to this point, so the
;     delegate re-runs the entry exactly once).
;   - normal frames: $4017 = $FF, $4015 = $0F; each SFX handler runs only
;     when its queue or active buffer is non-zero (the translated idle
;     path reads two zero bytes and returns without writes — verified
;     against $F45A/$F5C1/$F67F); the music handler always runs
;     (translated, stage 2 makes it native); then the queue clears and
;     the DMC throb ($07C0 ramp toward/away from $30, $4011 write).
; Exit A/flags are architecturally dead: the only caller (NMI $80E4)
; goes straight into ReadJoypads. D is preserved into the handlers
; (translated handlers see the caller's X exactly as the original entry
; left it); E ends as the original Y ($07C0 pre-ramp value).
rt_smb_sound_engine:
  ld   a, ($c770)            ; OperMode
  or   a
  jr   nz, _se_active
  ld   hl, $4015
  call rt_apu_write          ; A = 0: silence
  ret
_se_active:
  ld   a, ($c7c6)
  or   a
  jp   nz, _se_delegate
  ld   a, ($c0fa)
  cp   $01
  jp   z, _se_delegate
  ; normal frame
  ld   a, $ff
  ld   hl, $4017
  call rt_apu_write
  ld   a, $0f
  ld   hl, $4015
  call rt_apu_write
  ; Square 1 SFX: run only if queue ($FF) or buffer ($F1) is live.
  ld   a, ($c0ff)
  ld   hl, $c0f1
  or   (hl)
  jr   z, _se_sq2
  ld   bc, L_F41B
  ld   h, :L_F41B
  call rt_far_tail
_se_sq2:
  ld   a, ($c0fe)
  ld   hl, $c0f2
  or   (hl)
  jr   z, _se_noise
  ld   bc, L_F57C
  ld   h, :L_F57C
  call rt_far_tail
_se_noise:
  ld   a, ($c0fd)
  ld   hl, $c0f3
  or   (hl)
  jr   z, _se_music
  ld   bc, L_F667
  ld   h, :L_F667
  call rt_far_tail
_se_music:
  ld   bc, L_F694
  ld   h, :L_F694
  call rt_far_tail
  ; clear the frame's queues
  xor  a
  ld   ($c0fb), a
  ld   ($c0fc), a
  ld   ($c0ff), a
  ld   ($c0fe), a
  ld   ($c0fd), a
  ld   ($c0fa), a
  ; DMC throb: ramp $07C0 toward $30 while $F4 & 3, else back toward 0.
  ld   a, ($c7c0)
  ld   e, a
  ld   a, ($c0f4)
  and  $03
  jr   z, _se_tya
  ld   hl, $c7c0
  inc  (hl)
  ld   a, e
  cp   $30
  jr   c, _se_dmc
_se_tya:
  ld   a, e
  or   a
  jr   z, _se_dmc
  ld   hl, $c7c0
  dec  (hl)
_se_dmc:
  ld   a, e
  ld   hl, $4011
  call rt_apu_write
  ret
_se_delegate:
  ld   bc, L_F2D0
  ld   h, :L_F2D0
  jp   rt_far_tail

; ─── rt_smb_music_tick ────────────────────────────────────────────────────────
; Replaces NES $F73A (the per-frame music processor; entered by JMP from
; MusicHandler / the loaders, intercepted as call+ret). Sound stage 2:
; the every-frame steady tick is native — per channel, decrement the note
; length counter and run the envelope/sustain writes — while any frame on
; which a channel's counter would hit zero (a note fetch, with its stream
; reads and frequency setup) tail-delegates to the translated engine at
; that channel's block label, keeping fetch paths translation-perfect:
;   sq2 counter==1  -> L_F73A (whole routine)
;   sq1 counter==1  -> L_F7BC (sq1 block onward)
;   tri counter==1  -> L_F81A (triangle block onward)
;   noise counter==1-> L_F86D (noise block onward)
; Register contracts at the delegation labels were audited: no label
; reads A before writing it; X at L_F7BC must be $7F when the sq2
; envelope ran (mirrored via D), else the entry X (D untouched); Y at
; L_F7BC is the sq2 sustain index when the envelope ran (mirrored via E).
; Exit A/flags/X/Y are dead (the caller is rt_smb_sound_engine's
; queue-clear sequence).
rt_smb_music_tick:
  ; square 2
  ld   a, ($c7b4)
  cp   $01
  jp   z, _mt_deleg_all
  dec  a
  ld   ($c7b4), a
  ld   a, ($c0f2)            ; sq2 SFX active?
  or   a
  jr   nz, _mt_sq1
  ld   a, ($c7b1)
  and  $91
  jr   nz, _mt_sq1
  ld   a, ($c7b5)            ; sustain counter (pre-dec value indexes)
  ld   e, a
  or   a
  jr   z, +
  dec  a
  ld   ($c7b5), a
+:
  call _mt_env
  ld   hl, $4004
  call rt_apu_write
  ld   a, $7f
  ld   hl, $4005
  call rt_apu_write
  ld   d, $7f                ; 6502 LDX #$7F
_mt_sq1:
  ; square 1 (skipped entirely while its music track pointer is 0)
  ld   a, ($c0f8)
  or   a
  jr   z, _mt_tri
  ld   a, ($c7b6)
  cp   $01
  jp   z, _mt_deleg_sq1
  dec  a
  ld   ($c7b6), a
  ld   a, ($c0f1)            ; sq1 SFX active skips env AND the $4001 write
  or   a
  jr   nz, _mt_tri
  ld   a, ($c7b1)
  and  $91
  jr   nz, _mt_sq1_reg1
  ld   a, ($c7b7)
  ld   e, a
  or   a
  jr   z, +
  dec  a
  ld   ($c7b7), a
+:
  call _mt_env
  ld   hl, $4000
  call rt_apu_write
_mt_sq1_reg1:
  ld   a, ($c7ca)
  or   a
  jr   nz, +
  ld   a, $7f
+:
  ld   hl, $4001
  call rt_apu_write
_mt_tri:
  ld   a, ($c7b9)
  cp   $01
  jp   z, _mt_deleg_tri
  dec  a
  ld   ($c7b9), a
_mt_noise:
  ld   a, ($c0f4)
  and  $f3
  ret  z                     ; $F8C4 RTS
  ld   a, ($c7ba)
  cp   $01
  jp   z, _mt_deleg_noise
  dec  a
  ld   ($c7ba), a
  ret

_mt_env:                     ; in: E = index; out: A = envelope byte
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   h, $bf                ; $FF96/$FF9A/$FFA2 -> $BF96/$BF9A/$BFA2
  ld   a, ($c7b1)
  and  $08
  ld   a, $96
  jr   nz, _mt_env_rd
  ld   a, ($c0f4)
  and  $7d
  ld   a, $9a
  jr   nz, _mt_env_rd
  ld   a, $a2
_mt_env_rd:
  add  a, e
  ld   l, a
  jr   nc, +
  inc  h
+:
  ld   b, (hl)
  ld   a, :data_prg_low
  ld   ($ffff), a
  ld   a, b
  ret

; ─── rt_smb_flush_vram_buffer ─────────────────────────────────────────────────
; Replaces NES $8EDD UpdateScreen — the NMI's VRAM stripe-buffer flush.
; Original per stripe: two $2006 writes (address), a PPUCTRL write via
; WritePPUReg1 ($8EED: STA $2000 + STA $0778) selecting +1/+32 increment,
; then `count` bytes of LDA (ptr),Y / STA $2007 (bit6 of the length byte
; repeats one source byte), pointer advance ($00/$01 += Y+1), the
; $3F00/$0000 PPUADDR reset dance, an LDX $2002 latch reset, and loop
; while the next header byte is non-zero; two $2005 writes (A=0) close.
;
; Native version drives the SAME emulation primitives per stripe
; (rt_ppu_write/rt_ppu_read keep every latch/ctrl side effect exact) but
; the per-byte path drops the per-write guard entry, register dispatch,
; PPUADDR shadow load/increment/store, and the (zp),Y helper: the source
; pointer is normalized once per stripe (RAM or PRG-low ROM; anything
; else fail-safes by delegating the whole flush to translated L_8EDD),
; and each byte is a `di / call rt_ppudata_apply / ei` with a locally
; stepped NES VRAM address. EI is unconditional: the only caller is the
; translated NMI ($80C3) and the reset path (fall-through from the
; translated stripe writer); the IFF state is captured once at entry.
; Exit: A = 0, X(D) = last PPUSTATUS read, Y(E) = 0; flags dead (caller
; reloads via LDY/LDX/CPX). $CA25 is hook scratch (single-threaded game
; code — see rt_smb_block_buffer_collision's note).
rt_smb_flush_vram_buffer:
  ; Capture the caller's interrupt state once: the byte loops bracket each
  ; rt_ppudata_apply with DI, and must only re-enable when the caller had
  ; interrupts on (the reset path runs this flush with them off).
  ld   a, i
  ld   a, $00
  jp   po, +
  inc  a
+:
  ld   ($ca26), a
  ld   b, $02
  call rt_ppu_read           ; LDX $2002: latch reset + status side effects
  ld   d, a
_fvb_check:
  ld   a, ($c001)            ; normalize the zp $00/$01 pointer
  cp   $08
  jr   c, _fvb_ram
  cp   $80
  jr   c, _fvb_deleg
  cp   $c0
  jr   nc, _fvb_deleg
  ld   h, a                  ; PRG-low ROM: direct
  jr   _fvb_hi
_fvb_ram:
  add  a, $c0
  ld   h, a
_fvb_hi:
  ld   a, ($c000)
  ld   l, a
  ld   a, (hl)
  or   a
  jr   nz, _fvb_stripe
  ; empty buffer: A = 0 scroll clears, then return
  ld   b, $05
  call rt_ppu_write
  ld   b, $05
  call rt_ppu_write
  ld   e, $00
  xor  a
  ret
_fvb_deleg:
  ld   bc, L_8EDD
  ld   h, :L_8EDD
  jp   rt_far_tail

_fvb_stripe:                 ; A = VRAM addr hi, HL -> header byte 0
  push hl
  ld   b, $06
  call rt_ppu_write          ; $2006 high
  pop  hl
  inc  hl
  ld   a, (hl)
  push hl
  ld   b, $06
  call rt_ppu_write          ; $2006 low
  pop  hl
  inc  hl
  ld   c, (hl)               ; C = length byte (b7 vertical, b6 repeat)
  ld   a, ($c778)
  or   $04
  bit  7, c
  jr   nz, +
  and  $fb
+:
  push hl
  push bc
  ld   b, $00
  call rt_ppu_write          ; WritePPUReg1: $2000 semantics (A preserved)
  ld   ($c778), a            ; ... + STA $0778
  pop  bc
  pop  hl
  ld   a, c
  and  $3f
  ld   b, a                  ; B = count (0 means 256, like DEX/BNE)
  ld   a, $01
  bit  7, c
  jr   z, +
  ld   a, $20
+:
  bit  6, c
  ld   c, a                  ; C = address step (+1 / +32)
  jr   nz, _fvb_rep
  ; sequential: ptr advance = count + 3
  ld   a, b
  add  a, $03
  ld   ($ca25), a
  inc  hl                    ; first data byte
  call _fvb_seq_loop
  jr   _fvb_advance
_fvb_rep:
  ld   a, $04                ; repeat: ptr advance = 4
  ld   ($ca25), a
  inc  hl                    ; the single data byte
  call _fvb_rep_loop
_fvb_advance:
  ld   a, ($ca25)
  ld   c, a
  ld   a, ($c000)
  add  a, c
  ld   ($c000), a
  ld   a, ($c001)
  adc  a, $00
  ld   ($c001), a
  ; PPUADDR reset dance: $3F00 then $0000
  ld   a, $3f
  ld   b, $06
  call rt_ppu_write
  xor  a
  ld   b, $06
  call rt_ppu_write
  xor  a
  ld   b, $06
  call rt_ppu_write
  xor  a
  ld   b, $06
  call rt_ppu_write
  ld   b, $02
  call rt_ppu_read           ; LDX $2002
  ld   d, a
  jp   _fvb_check

_fvb_seq_loop:               ; B = count, C = step, HL = src
  ld   a, ($cb0f)            ; NES VRAM address from the $2006 pair
  ld   d, a
  ld   a, ($cb10)
  ld   e, a
_fvb_sl:
  ld   a, (hl)
  ld   ($cb18), a
  push hl
  push de
  push bc
  di
  call rt_ppudata_apply
  ld   a, ($ca26)
  or   a
  jr   z, +
  ei
+:
  pop  bc
  pop  de
  pop  hl
  inc  hl
  ld   a, e
  add  a, c
  ld   e, a
  jr   nc, +
  inc  d
+:
  djnz _fvb_sl
  ret

_fvb_rep_loop:               ; B = count, C = step, HL = the one src byte
  ld   a, ($cb0f)
  ld   d, a
  ld   a, ($cb10)
  ld   e, a
_fvb_rl:
  ld   a, (hl)
  ld   ($cb18), a
  push hl
  push de
  push bc
  di
  call rt_ppudata_apply
  ld   a, ($ca26)
  or   a
  jr   z, +
  ei
+:
  pop  bc
  pop  de
  pop  hl
  ld   a, e
  add  a, c
  ld   e, a
  jr   nc, +
  inc  d
+:
  djnz _fvb_rl
  ret

_mt_deleg_all:
  ld   bc, L_F73A
  ld   h, :L_F73A
  jp   rt_far_tail
_mt_deleg_sq1:
  ld   bc, L_F7BC
  ld   h, :L_F7BC
  jp   rt_far_tail
_mt_deleg_tri:
  ld   bc, L_F81A
  ld   h, :L_F81A
  jp   rt_far_tail
_mt_deleg_noise:
  ld   bc, L_F86D
  ld   h, :L_F86D
  jp   rt_far_tail

.ends
.endif
