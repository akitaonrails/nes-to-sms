; dispatch.s — Indirect jump and indexed memory access helpers.
;
; rt_indirect_jmp  — implements 6502 JMP ($xxxx).
; rt_unresolved_jsr — trap for JSR targets not resolved at translate time.
; rt_brk           — trap for 6502 BRK instruction.
; rt_read_indexed  — (HL+B) -> A.
; rt_write_indexed — A -> (HL+B).
; rt_read_zp_ptr_y — dereference zero-page pointer + Y.
; rt_write_zp_ptr_y — write through zero-page pointer + Y.
;
; NES-to-SMS address remapping rule (used in pointer dereferences):
;   NES addr < $0800 (NES RAM) → SMS addr = NES addr + $C000
;   NES addr $0000-$00FF (zero page) is covered by the above.
;   NES addr >= $0800 and < $2000 (mirrors) → remap to $C000 + (addr & $07FF)
;   NES addr $2000-$3FFF (PPU) → not valid to dereference as data; trap.
;   NES addr >= $8000 (PRG ROM) → address is already in SMS ROM space for NROM;
;     for NROM-256 the second bank ($C000-$FFFF) is visible at SMS $4000+.
;     TODO: For v1 we do not remap ROM addresses — translated code should not
;     be dereferencing PRG ROM pointers at runtime (it reads them as literals).
;
; NMOS 6502 page-crossing bug in JMP ($xxxx):
;   If the indirect address is at $xxFF, the high byte of the target is read
;   from $xx00 rather than $(xx+1)00. We do NOT reproduce this bug for v1
;   because SMB does not rely on it. Add a TODO comment in rt_indirect_jmp.

.section "dispatch" free

; ─── rt_far_call ──────────────────────────────────────────────────────────────
; Cross-bank call helper. Translated `JSR L_XXXX` becomes:
;     call rt_far_call
;     .dw <target_addr>      ; logical slot-1 address ($4000-$7FFF)
;     .db :<target>          ; bank number
;
; Saves the current slot-1 bank to the Z80 stack, switches slot 1 to the
; target bank, calls the target, restores the slot-1 bank, then returns.
;
; Bank shadow: we mirror the slot-1 bank value in RAM at $CB14 so that
; nested far-calls can see and restore it.  $CB15 is transient scratch for
; preserving the 6502 accumulator across the mapper write before control
; reaches the translated target.
;
; Trade-off: each cross-bank call costs ~50 Z80 cycles plus 3 bytes of
; inline data versus the original 3-byte `call`. For SMB this is a few
; hundred extra calls per frame, well within Z80 budget at 4 MHz.
; ─── rt_far_gate ──────────────────────────────────────────────────────────────
; Compact far dispatch (H2): the call site loads DE = target and A = bank
; as immediates (no data-block decode) and transfers here. This shim MUST
; live in slot 0: switching $FFFE from code running in slot 1 swaps the
; executing bank under the PC (found the hard way).
;   far CALL sites:  ld ($cb15),a / ld de,T / ld a,:T / call rt_far_gate
;   far JMP  sites:  ld ($cb15),a / ld de,T / ld a,:T / jp  rt_far_gate
; For calls, the site's return address is already on the stack; for jumps
; the original caller's is. Either way the target's RET unwinds through
; _far_after, which restores the previous bank.
rt_far_gate:
  ; Phase R: target arrives in BC (DE holds resident 6502 X/Y and must
  ; flow through untouched). A = target bank; ($cb15) = caller A.
  ld   ($cb2e), bc          ; park target (dedicated gate scratch word)
  ld   c, a
  ld   a, ($cb14)
  push af                   ; save previous slot-1 bank
  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a
  ld   bc, _far_after
  push bc
  ld   bc, ($cb2e)
  push bc                   ; target
  ld   a, ($cb15)           ; caller A (JSR/JMP preserve the accumulator)
  ret                       ; jump to target

rt_far_call:
  pop  hl                   ; HL = data block PC (just after the `call`)
  ld   ($cb15), a            ; preserve caller A for target entry
  ld   e, (hl)              ; E = target_lo
  inc  hl
  ld   d, (hl)              ; D = target_hi
  inc  hl
  ld   a, (hl)              ; A = target_bank
  inc  hl                   ; HL = pc to return to caller (past data)
  push hl                   ; final return PC

  ; Save target bank in C, then push current bank.
  ld   c, a                 ; C = target bank
  ld   a, ($cb14)           ; A = current bank
  push af                   ; stack: [final_ret, current_bank_in_AF]

  ; Switch slot 1 to target bank.
  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a

  ; "call DE" via push/jp trick. We need to return to _far_after.
  ld   bc, _far_after
  push bc                   ; stack: [final_ret, current_bank, _far_after]
  push de                   ; stack: [..., target]
  ld   a, ($cb15)            ; JSR preserves A; mapper writes used A as scratch
  ret                       ; jump to target

_far_after:
  ; Target's RET landed here. Stack: [final_ret, current_bank_in_AF].
  ; Preserve the target's returned AF while restoring the mapper.
  pop  bc                   ; B = previous slot-1 bank
  push af                   ; save target return A/F
  ld   a, b
  ld   ($cb14), a
  ld   ($fffe), a
  pop  af                   ; restore target return A/F
  ret                       ; return to caller (past data)

; ─── rt_far_jmp ───────────────────────────────────────────────────────────────
; Bank-aware cross-bank JMP. Translated `JMP L_XXXX` becomes:
;     call rt_far_jmp
;     .dw <target_addr>
;     .db :<target>
;
; Switches slot 1 to the target bank and tail-jumps. Because Z80 return
; addresses are bankless, this also pushes a restore trampoline: when the
; target eventually RETs, the previous slot-1 bank is restored before
; returning to the original caller.
rt_far_jmp:
  pop  hl                   ; HL = data block PC
  ld   ($cb15), a            ; preserve caller A for target entry
  ld   e, (hl)              ; E = target_lo
  inc  hl
  ld   d, (hl)              ; D = target_hi
  inc  hl
  ld   c, (hl)              ; C = target_bank
  ld   a, ($cb14)           ; A = current bank
  push af                   ; save previous bank
  ld   a, c
  ld   ($cb14), a
  ld   ($fffe), a

  ld   bc, _far_jmp_after
  push bc                   ; target RET lands here
  push de
  ld   a, ($cb15)            ; JMP preserves A; mapper writes used A as scratch
  ret

_far_jmp_after:
  ; Target's RET landed here. Stack: [original_return, previous_bank_in_AF].
  ; Preserve the target's returned AF while restoring mapper state.
  pop  bc                   ; B = previous slot-1 bank
  push af                   ; save target return A/F
  ld   a, b
  ld   ($cb14), a
  ld   ($fffe), a
  pop  af                   ; restore target return A/F
  ret

; ─── rt_indirect_jmp ──────────────────────────────────────────────────────────
; 6502 JMP ($ptr): reads the 16-bit jump target from (ptr) and (ptr+1).
; Entry: HL = address of the pointer (already remapped to SMS address space).
; Exit:  jumps to the target address.
; Clobbers: all (it does not return to caller).
;
; The "push DE / ret" trick: push target onto the Z80 stack and execute RET,
; which pops it into PC.  This avoids needing to know which register holds PC.
;
; TODO: The NMOS page-crossing bug ($xxFF high byte read from $xx00) is not
; reproduced here. If a future target relies on it, add a check: if L == $FF,
; set H unchanged and L = 0 for the second read.
rt_indirect_jmp:
  ; Phase R note: DE is the resident X/Y pair — this helper is a JMP
  ; (control transfer), so X/Y must SURVIVE into the target. Use BC for
  ; the pointer instead.
  ld   c, (hl)              ; low byte of target
  inc  hl
  ld   b, (hl)              ; high byte of target
  ; ROM targets ($8000+) dispatch through the generated (bank, addr)
  ; table — never execute raw NES bytes (mapper plan M1).
  ld   a, b
  cp   $80
  jp   nc, rt_banked_dispatch
  ; RAM targets: remap NES RAM -> SMS RAM and jump.
  push de
  ld   d, b
  ld   e, c
  call _dispatch_remap_de
  ld   b, d
  ld   c, e
  pop  de
  push bc
  ret

; ─── rt_banked_dispatch ───────────────────────────────────────────────────────
; BC = NES ROM target address. Look it up in the generated dispatch
; table: entries of .dw nes_addr / .db nes_bank / .db sms_bank / .dw label,
; terminated by addr $0000. Fixed-bank entries carry nes_bank $FF and
; match any window state; window entries ($8000-$BFFF) also require the
; current UxROM bank shadow ($CB62) to match. Hit -> far-gate jump (bank
; restore on return included). Miss -> loud trap ($CB1D=$E2, target in
; $CB1B/1C) — fail closed, never run raw NES bytes.
rt_banked_dispatch:
  push de                   ; preserve resident X/Y through the search
  ld   a, ($cb14)
  push af                   ; caller's slot-1 bank (restored before far-gate)
  ld   a, :rt_dispatch_table
  ld   ($fffe), a
  ld   hl, rt_dispatch_table
_bd_loop:
  ld   e, (hl)              ; entry addr lo
  inc  hl
  ld   d, (hl)              ; entry addr hi
  inc  hl
  ld   a, d
  or   e
  jr   z, _bd_miss
  ; address match?
  ld   a, d
  cp   b
  jr   nz, _bd_skip
  ld   a, e
  cp   c
  jr   nz, _bd_skip
  ; bank constraint: entry nes_bank $FF matches anything; else compare
  ; with the mapper shadow (only meaningful for window targets).
  ld   a, (hl)
  cp   $ff
  jr   z, _bd_hit
  ld   e, a
  ld   a, ($cb62)
  cp   e
  jr   z, _bd_hit
_bd_skip:
  inc  hl                   ; skip nes_bank
  inc  hl                   ; skip sms bank
  inc  hl                   ; skip label lo
  inc  hl                   ; skip label hi
  jr   _bd_loop
_bd_hit:
  inc  hl                   ; -> sms bank byte
  ld   a, (hl)
  inc  hl
  ld   c, (hl)              ; label lo
  inc  hl
  ld   b, (hl)              ; label hi
  ld   e, a                 ; park sms bank
  pop  af                   ; caller's slot-1 bank
  ld   ($cb14), a
  ld   ($fffe), a
  ld   a, e                 ; A = target's sms bank, BC = label
  pop  de                   ; restore resident X/Y
  jp   rt_far_gate
_bd_miss:
  pop  af
  ld   ($cb14), a
  ld   ($fffe), a
  pop  de
  ld   a, c
  ld   ($cb1b), a
  ld   a, b
  ld   ($cb1c), a
  ld   a, ($cb62)
  ld   ($cb1a), a           ; live NES bank at miss time (diagnostics)
  ld   hl, $0000
  add  hl, sp
  ld   a, (hl)
  ld   ($cb73), a           ; Z80 caller return address (diagnostics)
  inc  hl
  ld   a, (hl)
  ld   ($cb74), a
  ld   a, $e2               ; distinct marker: banked-dispatch miss
  ld   ($cb1d), a
  jp   rt_unresolved_jsr_flash

; ─── _dispatch_remap_de ───────────────────────────────────────────────────────
; Remaps a NES address in DE to the SMS equivalent if it falls in NES RAM.
; NES $0000-$07FF → SMS $C000-$C7FF (add $C000).
; NES $0800-$1FFF → SMS $C000-$C7FF (mirror: add $C000 and mask to $07FF).
; Other addresses are returned unchanged (ROM, PPU — caller's problem).
; Clobbers: AF.
_dispatch_remap_de:
  ld   a, d
  cp   $08                  ; is high byte < $08? (i.e. NES addr < $0800)
  jr   c, _remap_ram        ; yes: simple $C000 offset
  cp   $20                  ; is high byte < $20? (i.e. addr $0800-$1FFF = mirrors)
  jr   c, _remap_mirror
  ; $2000+ — return unchanged.
  ret
_remap_ram:
  ; NES $0000-$07FF → add $C000.
  ld   a, d
  add  a, $c0
  ld   d, a
  ret
_remap_mirror:
  ; NES $0800-$1FFF — mask to $07FF, then add $C000.
  ld   a, e                 ; keep low byte as-is
  ld   e, a
  ld   a, d
  and  $07                  ; mask high bits to stay within 2KB
  add  a, $c0
  ld   d, a
  ret

; ─── rt_rts_dispatch ──────────────────────────────────────────────────────────
; 6502 `PHA hi / PHA lo / RTS` computed jump. Pop lo, then hi from the
; emulated 6502 stack ($C100 + S), add 1, and transfer through
; rt_banked_dispatch. The Z80 return address of the caller (pushed by
; the `call rt_rts_dispatch` in translated code) is discarded — the
; 6502 semantics transfer control, they don't return.
rt_rts_dispatch:
  pop  hl                   ; discard translated-code return address
  push de
  ld   a, ($cb02)           ; 6502 S
  inc  a
  ld   l, a
  ld   h, $c1
  ld   e, (hl)              ; lo (S+1)
  inc  a
  ld   l, a
  ld   d, (hl)              ; hi (S+2)
  ld   ($cb02), a           ; S += 2
  ; BC = target + 1
  inc  de
  ld   b, d
  ld   c, e
  pop  de
  ; RAM-target computed jumps would need translated RAM code — trap via
  ; the dispatcher's miss path ($E2) if the table has no entry.
  jp   rt_banked_dispatch

; ─── rt_unresolved_jsr ────────────────────────────────────────────────────────
; Trap: called when the Rust back end emitted a JSR to an address that could
; not be resolved to a translated label at compile time.
; This halts the Z80 with a visible pattern: continuously writes $FF to CRAM
; addr 0 to make the border flash, then halts.
;
; TODO: In a later phase, rt_unresolved_jsr should look up the target in a
; runtime dispatch table (for indirect JSR through profile-annotated jump tables).
rt_unresolved_jsr:
  di
  ld   a, $e1
  ld   ($cb1d), a            ; trace-sms runtime trap marker
  ; Flash screen: write $FF (bright white) to CRAM palette 0.
rt_unresolved_jsr_flash:
  di
_ujsr_flash:
  xor  a
  out  ($bf), a             ; CRAM addr 0 low
  ld   a, $c0
  out  ($bf), a             ; CRAM addr command
  ld   a, $ff
  out  ($be), a             ; white
  xor  a
  out  ($be), a             ; black
  jr   _ujsr_flash          ; loop forever

; ─── rt_brk ───────────────────────────────────────────────────────────────────
; Trap: BRK is used in NES programs to trigger the IRQ/BRK vector.
; For v1 we treat it as a fatal error (SMB never intentionally BRKs).
; Same trap as rt_unresolved_jsr.
rt_brk:
  jp   rt_unresolved_jsr

; ─── rt_read_indexed ──────────────────────────────────────────────────────────
; Read a byte at (HL + B).
; Entry: HL = base address, B = unsigned offset.
; Exit:  A = byte at (HL + B).  HL and B preserved.
rt_read_indexed:
  push hl
  push bc
  ld   c, b
  ld   b, 0
  add  hl, bc               ; HL = base + offset (unsigned 8-bit offset)
  ld   a, (hl)
  pop  bc
  pop  hl
  ret

; ─── rt_read_prg_high_indexed ─────────────────────────────────────────────────
; Read a byte from the original NES fixed PRG window ($C000-$FFFF).
; Entry: HL = NES base address in $C000-$FFFF, B = unsigned offset.
; Exit:  A = byte at (HL + B). HL and B preserved. Slot 2 restored to
;        data_prg_low because most translated PRG table reads expect it there.
rt_read_prg_high_indexed:
  push hl
  push bc
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, h
  sub  $40                   ; $C000->$8000 within slot 2
  ld   h, a
  ld   c, b
  ld   b, 0
  add  hl, bc
  ld   a, (hl)
  push af
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  pop  af
  pop  bc
  pop  hl
  ret

; ─── rt_write_indexed ─────────────────────────────────────────────────────────
; Write C to (HL + B).
; Entry: HL = base address, B = unsigned offset, C = value.
; Exit:  (HL + B) = C.  HL, B, C preserved.  A clobbered.
rt_write_indexed:
  push hl
  push bc
  ld   a, c                 ; save value
  ld   c, b
  ld   b, 0
  add  hl, bc
  pop  bc
  ; Hardware windows must not be written as plain memory: indexed stores
  ; like SMB's `STA $4000,X` (X = channel offset) target APU registers,
  ; and `STA $2000,X` targets PPU registers. Forward them to the shims;
  ; plain-memory writes fall through.
  ld   a, h
  cp   $40
  jr   z, _wi_maybe_apu
  cp   $20
  jr   c, _wi_plain
  cp   $40
  jr   c, _wi_ppu           ; $2000-$3FFF: PPU register mirrors
_wi_plain:
  ld   (hl), c              ; write value
  ld   a, c                 ; STA leaves the 6502 accumulator intact: the
                            ; range checks above clobbered A, restore it
                            ; (returning the address byte in A corrupted
                            ; every store that followed an indexed store)
  pop  hl
  ret
_wi_maybe_apu:
  ld   a, l
  cp   $18
  jr   nc, _wi_plain        ; $4018+: not an APU register
  cp   $16
  jr   z, _wi_strobe
  push bc
  ld   a, c
  call rt_apu_write         ; A = value, HL = $40xx (preserves A)
  pop  bc
  pop  hl
  ret
_wi_strobe:
  push bc
  ld   a, c
  call rt_controller_strobe
  pop  bc
  pop  hl
  ret
_wi_ppu:
  push bc
  ld   a, l
  and  $07
  ld   b, a
  ld   a, c
  call rt_ppu_write         ; A = value, B = register index
  ld   a, c                 ; body may clobber A; restore the accumulator
  pop  bc
  pop  hl
  ret

; ─── rt_read_zp_ptr_y ─────────────────────────────────────────────────────────
; 6502 (zp),Y addressing mode read.
; Reads a 16-bit pointer from zero-page at B and B+1, adds Y, dereferences.
; Entry: B = zero-page address (0..255).
; Exit:  A = byte at ((zp[B+1] << 8) | zp[B]) + Y.
; Clobbers: AF.  Preserves HL, BC, DE.
;
; Zero page is mirrored at SMS $C000-$C0FF.
; Pointer target is remapped to SMS space if it falls in NES RAM.
rt_read_zp_ptr_y:
  push hl
  push de
  push bc
  ld   c, e                 ; Phase R: capture resident Y before DE is reused
  ; Read pointer from zero page.
  ld   l, b                 ; zero-page offset
  ld   h, $c0               ; SMS base for zero page = $C000
  ld   e, (hl)              ; low byte of pointer
  ; Wrap within zero page for high byte (6502 ZP wraps, not 6502 page-cross bug).
  inc  l                    ; L wraps within $00-$FF automatically (no carry to H)
  ld   d, (hl)              ; high byte of pointer
  ; DE = NES pointer value.
  ; Add Y to form effective address.
  ld   a, c                 ; resident Y (captured at entry)
  ld   l, a
  ld   h, 0
  add  hl, de               ; HL = pointer + Y (16-bit)
  ex   de, hl               ; DE = effective NES address
  ; NES $C000-$FFFF is fixed high PRG: read it through the data_prg_high
  ; copy in slot 2 (SMB's music note streams live at $F800-$FFFF and are
  ; dereferenced via (zp),Y — reading the SMS RAM mirror here fed garbage
  ; notes to the translated sound engine).
  ld   a, d
  cp   $c0
  jr   nc, _rzpy_prg_high
  ; Remap to SMS address.
  call _dispatch_remap_de
  ; Dereference.
  ld   h, d
  ld   l, e
  ld   a, (hl)
  pop  bc
  pop  de
  pop  hl
  ret
_rzpy_prg_high:
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, d
  sub  $40                  ; $C000-$FFFF -> $8000-$BFFF in slot 2
  ld   h, a
  ld   l, e
  ld   a, (hl)
  push af
  call rt_restore_prg_window   ; current NES PRG window (banked-aware)
  pop  af
  pop  bc
  pop  de
  pop  hl
  ret

; ─── rt_write_zp_ptr_y ────────────────────────────────────────────────────────
; 6502 (zp),Y addressing mode write.
; Reads pointer from zero page at B and B+1, adds Y, writes A there.
; Entry: B = zero-page address, A = value to write.
; Clobbers: AF (carries through — A still holds the written value on return).
; Preserves HL, BC, DE.
rt_write_zp_ptr_y:
  push hl
  push de
  push bc
  ld   c, e                 ; Phase R: capture resident Y before DE is reused
  push af                   ; save value to write
  ; Read pointer from zero page.
  ld   l, b
  ld   h, $c0
  ld   e, (hl)
  inc  l
  ld   d, (hl)
  ; Add Y.
  ld   a, c                 ; resident Y (captured at entry)
  ld   l, a
  ld   h, 0
  add  hl, de
  ex   de, hl               ; DE = effective NES address
  ; Hardware windows: forward APU/PPU targets to the shims (see
  ; rt_write_indexed).
  ld   a, d
  cp   $40
  jr   z, _wzy_maybe_apu
  cp   $20
  jr   c, _wzy_plain
  cp   $40
  jr   c, _wzy_ppu
_wzy_plain:
  call _dispatch_remap_de
  ld   h, d
  ld   l, e
  pop  af                   ; restore value
  ld   (hl), a              ; write
  pop  bc
  pop  de
  pop  hl
  ret

_wzy_maybe_apu:
  ld   a, e
  cp   $18
  jr   nc, _wzy_plain
  cp   $16
  jr   z, _wzy_strobe
  ld   h, d
  ld   l, e
  pop  af
  call rt_apu_write
  pop  bc
  pop  de
  pop  hl
  ret
_wzy_strobe:
  pop  af
  call rt_controller_strobe
  pop  bc
  pop  de
  pop  hl
  ret
_wzy_ppu:
  ld   a, e
  and  $07
  ld   b, a
  pop  af
  call rt_ppu_write
  pop  bc
  pop  de
  pop  hl
  ret


.ends
