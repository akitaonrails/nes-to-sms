; input.s — SMS controller read with NES button mapping.
;
; SMS controller ports (active-low, read from I/O):
;   Port $DC (port 1 + port 2 mixed):
;     bit 0 = Port 1 Up
;     bit 1 = Port 1 Down
;     bit 2 = Port 1 Left
;     bit 3 = Port 1 Right
;     bit 4 = Port 1 Button 1  (mapped to NES A)
;     bit 5 = Port 1 Button 2  (mapped to NES B)
;     bit 6 = Port 2 Up
;     bit 7 = Port 2 Down
;   Port $DD:
;     bit 0 = Port 2 Left
;     bit 1 = Port 2 Right
;     bit 2 = Port 2 Button 1
;     bit 3 = Port 2 Button 2
;
; NES button bit order (as SMB reads from $4016 serially):
;   read 1 = A
;   read 2 = B
;   read 3 = Select
;   read 4 = Start
;   read 5 = Up
;   read 6 = Down
;   read 7 = Left
;   read 8 = Right
;
; SMB button remap (per master plan):
;   Title mode ($0770 == 0): SMS Button 1 -> NES Select, Button 2 -> NES Start.
;   Gameplay modes:         SMS Button 1 -> NES A,      Button 2 -> NES B.
;   Direction buttons map directly.
;
; Implementation strategy (v1):
;   rt_controller_latch  — called once per VBlank from irq_handler.
;                          Reads $DC, inverts (active-high), stores NES byte at $CB06.
;                          Also resets the bit-read index at $CB07 to 0.
;   rt_controller_strobe — called when translated code writes $4016.
;                          Resets $CB07 to 0 so the next read returns A.
;   rt_controller_read   — called for each $4016 read by translated SMB.
;                          Returns bit ($CB07) of ($CB06) in A bit 0.
;                          Increments $CB07 (wraps at 8).
;
; Resulting NES byte layout stored at $CB06:
;   bit 7 = Right
;   bit 6 = Left
;   bit 5 = Down
;   bit 4 = Up
;   bit 3 = Start  (SMS Button 2 in title mode)
;   bit 2 = Select (SMS Button 1 in title mode)
;   bit 1 = B      (SMS Button 2 in gameplay modes)
;   bit 0 = A      (SMS Button 1 in gameplay modes)
;
; SMB reads in order: A, B, Select, Start, Up, Down, Left, Right.
; The bit counter at $CB07 goes 0,1,2,...,7 across successive reads.
; Bit 0 of the returned byte = button state (1 = pressed).

.section "input" free

; ─── rt_controller_latch ──────────────────────────────────────────────────────
; Reads SMS port $DC, converts to NES button byte, stores at $CB06.
; Resets the serial read counter at $CB07 to 0.
; Called from irq_handler once per VBlank. irq_handler already preserves AF/BC;
; keep this helper stackless on the hot frame path.
rt_controller_latch:
  in   a, ($dc)             ; read SMS controller port 1 (active-low)
  cpl                       ; invert: now 1 = pressed, 0 = released

  ; a = raw SMS byte (inverted):
  ;   bit 0 = Up pressed
  ;   bit 1 = Down pressed
  ;   bit 2 = Left pressed
  ;   bit 3 = Right pressed
  ;   bit 4 = Button 1 pressed (NES A + Select)
  ;   bit 5 = Button 2 pressed (NES B + Start)

  ; Build NES byte:
  ;   NES bit 0 (A)      = SMS bit 4
  ;   NES bit 1 (B)      = SMS bit 5
  ;   NES bit 2 (Select) = SMS bit 4  (same physical button as A per master plan)
  ;   NES bit 3 (Start)  = SMS bit 5  (same physical button as B per master plan)
  ;   NES bit 4 (Up)     = SMS bit 0
  ;   NES bit 5 (Down)   = SMS bit 1
  ;   NES bit 6 (Left)   = SMS bit 2
  ;   NES bit 7 (Right)  = SMS bit 3

  ld   b, a                 ; B = raw inverted SMS byte
  ld   c, $00               ; C = NES byte being built

.ifdef INPUT_MODE_ACTION
  ; Profile input mode "action": fixed button 1 -> NES A, button 2 ->
  ; NES B. Start comes from the SMS PAUSE button (INPUT_PAUSE_START).
  ; The heuristic below reads SMB-specific RAM ($0770) and mis-mapped
  ; buttons for other games (CV1's $0770 is ordinary game RAM).
  jr   _latch_game_buttons
.else
  ; SMB title mode needs Select/Start; gameplay needs A/B. Use $0770 as the
  ; coarse mode discriminator: 0 = title/menu, nonzero = in-game modes.
  ld   a, ($c770)
  or   a
  jr   nz, _latch_game_buttons
.endif

  ; Select (NES bit 2) = SMS bit 4 in title mode.
  bit  4, b
  jr   z, _latch_no_sel
  ld   a, c
  or   %00000100
  ld   c, a
_latch_no_sel:

  ; Start (NES bit 3) = SMS bit 5 in title mode.
  bit  5, b
  jr   z, _latch_no_start
  ld   a, c
  or   %00001000
  ld   c, a
_latch_no_start:
  jr   _latch_buttons_done

_latch_game_buttons:
  ; A (NES bit 0) = SMS bit 4 in gameplay modes.
  bit  4, b
  jr   z, _latch_no_a
  ld   a, c
  or   %00000001
  ld   c, a
_latch_no_a:

  ; B (NES bit 1) = SMS bit 5 in gameplay modes.
  bit  5, b
  jr   z, _latch_no_b
  ld   a, c
  or   %00000010
  ld   c, a
_latch_no_b:

_latch_buttons_done:
.ifdef INPUT_PAUSE_START
  ; SMS PAUSE pressed recently: the pause NMI ($0066) armed a small
  ; countdown at $CB2E; while it runs, hold NES Start down. The counter
  ; makes the press a clean multi-frame edge (press then release), which
  ; is what new-press detectors need.
  ld   a, ($cb2e)
  or   a
  jr   z, _latch_no_pause_start
  dec  a
  ld   ($cb2e), a
  ld   a, c
  or   %00001000            ; NES Start
  ld   c, a
_latch_no_pause_start:
.endif

  ; Up (NES bit 4) = SMS bit 0.
  bit  0, b
  jr   z, _latch_no_up
  ld   a, c
  or   %00010000
  ld   c, a
_latch_no_up:

  ; Down (NES bit 5) = SMS bit 1.
  bit  1, b
  jr   z, _latch_no_down
  ld   a, c
  or   %00100000
  ld   c, a
_latch_no_down:

  ; Left (NES bit 6) = SMS bit 2.
  bit  2, b
  jr   z, _latch_no_left
  ld   a, c
  or   %01000000
  ld   c, a
_latch_no_left:

  ; Right (NES bit 7) = SMS bit 3.
  bit  3, b
  jr   z, _latch_no_right
  ld   a, c
  or   %10000000
  ld   c, a
_latch_no_right:

  ; Store NES button byte and reset serial index.
  ld   a, c
  ld   ($cb06), a
  xor  a
  ld   ($cb07), a           ; reset bit-read index to 0
  ret

; ─── rt_controller_strobe ─────────────────────────────────────────────────────
; Called when translated code writes $4016 (NES latch strobe).
; Resets the serial counter so the next read returns button A again.
rt_controller_strobe:
  ; STA $4016 preserves A and flags. Use an immediate memory store rather than
  ; `xor a` plus AF save, because controller strobes can occur at native-stack
  ; low-water inside translated update chains.
  ld   hl, $cb07
  ld   (hl), $00
  ret

; ─── rt_controller_read ───────────────────────────────────────────────────────
; Returns one bit of the latched controller state in A bit 0.
; Entry: (none — uses $CB06 latch and $CB07 index).
; Exit:  A = 0 or 1 (button released or pressed).
; The bit-read index wraps at 8; reads 9+ return 1 (NES open-bus behavior).
; Preserves DE (resident translated X/Y). Clobbers AF/BC. Keep stackless:
; controller polling can happen inside nested translated-NMI work.
rt_controller_read:
  ; Entry: A = port low byte ($16 = controller 1, $17 = controller 2).
  ; Controller 2 is unconnected for v1: return 0 WITHOUT touching the
  ; controller-1 shift index — games like CV1 interleave $4016/$4017
  ; reads, and a shared index de-serializes port 1 (phantom input).
  cp   $17
  jr   nz, _ctrl_read_p1
  xor  a
  ret
_ctrl_read_p1:
  ld   a, ($cb07)           ; current bit index (0..7)
  cp   8
  jr   nc, _ctrl_read_open  ; index >= 8: return 1 (open-bus)

  ; Shift the latched byte right by index, take bit 0.
  ld   b, a                 ; B = shift count
  ld   a, ($cb06)           ; latched NES button byte
  ld   c, a
  ld   a, b
  or   a
  jr   z, _ctrl_read_shift_done
_ctrl_read_shift:
  rrc  c                    ; rotate right; bit 0 comes out at each step
  djnz _ctrl_read_shift
_ctrl_read_shift_done:
  ld   a, c
  and  %00000001            ; isolate bit 0
  ld   c, a                 ; preserve return bit while updating the index

  ; Increment index for the next read. Avoid `inc (hl)` because the local
  ; trace emulator does not implement that Z80 opcode yet.
  ld   a, ($cb07)
  inc  a
  ld   ($cb07), a
  ld   a, c                 ; return sampled button bit, not incremented index
  ret

; ─── rt_controller_read_indexed_x ──────────────────────────────────────────────
; Implements SMB's `LDA $4016,X` joypad loop. X=0 reads controller 1 through
; the serial latch; X=1 reads controller 2, which is unconnected for v1 and
; returns 0. Other X values also return 0.
rt_controller_read_indexed_x:
  ld   a, d                   ; Phase R: resident X
  or   a
  jp   z, rt_controller_read
  xor  a
  ret

_ctrl_read_open:
  ; NES open-bus: reads past 8 return 1.
  ld   a, 1
  ret

.ends
