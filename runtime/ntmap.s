; ntmap.s — NES nametable/CIRAM address helpers.
;
; This is a scaffold for the runtime nametable-shadow materializer. It does not
; write the CIRAM shadow yet because chrmap.s still uses $CC00-$D3FF as folded
; SMS per-cell subpalette state. Callers may use the helper once that storage
; collision is removed.

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

.ends
