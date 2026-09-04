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

.ends
.endif
