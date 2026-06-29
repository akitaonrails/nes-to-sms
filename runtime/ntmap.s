; ntmap.s — NES nametable/CIRAM address helpers.
;
; This is a scaffold for the runtime nametable-shadow materializer. It does not
; write a full CIRAM tile shadow yet because chrmap.s still uses $CC00-$D2FF
; as the authoritative folded SMS per-cell subpalette state. The old compact
; $D300-$D3DF duplicate has been retired, and trace diagnostics currently
; reserve $D300-$D3FF only as metadata space. Callers may use the full helper
; only once the folded-shadow storage collision is removed.

.define NT_ATTR_SHADOW $cb80

.section "ntmap" free

; Exactly one mirroring mode must be provided by the generated top-level asm.
.ifndef NES_MIRRORING_VERTICAL
.ifndef NES_MIRRORING_HORIZONTAL
.fail "missing NES nametable mirroring define"
.endif
.endif

.ifdef NES_MIRRORING_VERTICAL
.ifdef NES_MIRRORING_HORIZONTAL
.fail "conflicting NES nametable mirroring defines"
.endif
.endif

; Convert a NES PPU nametable address to a mirrored 2 KiB CIRAM shadow pointer.
;
; Entry: DE = NES PPU address $2000-$2FFF.
; Exit:  HL = $CC00 + mirrored CIRAM offset.
; Preserves: DE.
; Clobbers: AF, HL.
;
; Mirroring:
;   vertical:   pages 0,1,0,1 -> raw & $07FF
;   horizontal: pages 0,0,1,1 -> (raw & $03FF) | ((raw & $0800) >> 1)
rt_nt_ppuaddr_to_ciram:
  ld   a, d

.ifdef NES_MIRRORING_VERTICAL
  and  $07                   ; raw high byte within 2 KiB CIRAM
  add  a, $cc
  ld   h, a
  ld   l, e
  ret
.endif

.ifdef NES_MIRRORING_HORIZONTAL
  and  $03                   ; raw low 1 KiB offset high bits
  ld   h, a
  ld   a, d
  and  $08                   ; raw bit 11 selects CIRAM page 1
  srl  a                     ; move bit 3 -> bit 2
  or   h
  add  a, $cc
  ld   h, a
  ld   l, e
  ret
.endif

; Map a NES PPU attribute-table address to the compact mirrored attribute
; shadow. The shadow stores only the 64 attribute bytes for each of the two
; physical CIRAM pages, leaving the existing $CC00 folded SMS subpalette shadow
; untouched for current rendering.
;
; Entry: DE = NES PPU attribute address ($23C0/$27C0/$2BC0/$2FC0 mirrors).
; Exit:  HL = $CB80 + mirrored_attr_index (0..127).
; Preserves: DE.
; Clobbers: AF, HL.
rt_nt_attr_shadow_addr:
  call rt_nt_ppuaddr_to_ciram   ; HL = $CC00 + mirrored CIRAM offset

  ld   a, l
  and  $3f                      ; attribute byte within CIRAM page
  ld   l, a

  ld   a, h
  sub  $cc
  and  $04                      ; mirrored CIRAM page bit
  add  a, a
  add  a, a
  add  a, a
  add  a, a                     ; $04 -> $40
  or   l
  add  a, $80                  ; base low byte of NT_ATTR_SHADOW
  ld   l, a
  ld   h, $cb
  ret

; Write one NES attribute byte into the compact mirrored attribute shadow.
; Entry: DE = NES PPU attribute address, A = attr byte.
; Preserves: DE.
; Clobbers: AF, HL.
rt_nt_write_attr_shadow:
  push af
  call rt_nt_attr_shadow_addr
  pop  af
  ld   (hl), a
  ret

; Derive the NES background subpalette S for a tile from the compact mirrored
; attribute shadow. Helper-only scaffold for the later materializer; current
; rendering paths still use the folded $CC00 SMS subpalette state.
;
; Entry: DE = NES PPU tile address $2000-$2FBF.
; Exit:  A = S (0..3).
; Preserves: DE.
; Clobbers: AF, BC, HL.
rt_nt_attr_s_from_attr_shadow:
  call rt_nt_ppuaddr_to_ciram   ; HL = $CC00 + mirrored tile offset

  ld   a, h
  sub  $cc
  ld   b, a                    ; B = mirrored offset high byte (0..7)
  ld   c, l                    ; C = mirrored offset low byte

  ; HL = $CB80 + page*64 + attr_index.
  ld   a, b
  and  $04
  add  a, a
  add  a, a
  add  a, a
  add  a, a                    ; $04 -> $40
  ld   l, a
  ld   a, c
  and  $80
  rrca
  rrca
  rrca
  rrca                         ; coarse_y attr bit from offset bit 7
  add  a, l
  ld   l, a
  ld   a, b
  and  $03
  add  a, a
  add  a, a
  add  a, a
  add  a, a                    ; offset bits 8..9 -> attr bits 4..5
  add  a, l
  ld   l, a
  ld   a, c
  and  $1c
  srl  a
  srl  a                       ; coarse_x >> 2
  add  a, l
  add  a, $80                  ; base low byte of NT_ATTR_SHADOW
  ld   l, a
  ld   h, $cb

  ; B = shift = ((coarse_y & 2) << 1) | (coarse_x & 2), i.e. 0/2/4/6.
  ld   b, $00
  ld   a, c
  and  $40
  jr   z, _nt_attr_shadow_shift_y_done
  ld   b, $04
_nt_attr_shadow_shift_y_done:
  ld   a, c
  and  $02
  jr   z, _nt_attr_shadow_shift_ready
  inc  b
  inc  b
_nt_attr_shadow_shift_ready:
  ld   a, b
  or   a
  ld   a, (hl)
  jr   z, _nt_attr_shadow_shift_done
_nt_attr_shadow_shift_apply:
  srl  a
  djnz _nt_attr_shadow_shift_apply
_nt_attr_shadow_shift_done:
  and  $03
  ret

.ends
