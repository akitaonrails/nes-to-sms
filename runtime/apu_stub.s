; apu_stub.s — APU and mapper write stubs for v1.
;
; All APU writes are discarded for v1. They are optionally logged to a
; ring buffer at $CE00-$CFFF (512 bytes) for debugging. The ring buffer
; uses a write pointer at $CB1E-$CB1F (16-bit, little-endian).
;
; Ring buffer record format (5 bytes):
;   [addr_hi, addr_lo, value, frame_lo, sentinel=$AA]
;
; The buffer wraps at $CFFF back to $CE00. When full, older entries are
; silently overwritten. This is debug-only; disable by setting
; APU_LOG_ENABLE = 0 below.
;
; rt_mapper_write discards all writes (NROM has no mapper registers).
; For banked mappers (MMC1, MMC3) a mapper.s module will replace this.

.define APU_LOG_ENABLE 1    ; set to 0 to disable ring-buffer logging

.section "apu_stub" free

; ─── rt_apu_write ─────────────────────────────────────────────────────────────
; Entry: A = value, HL = NES register address.
; Discards the write (no PSG output for v1).
; Optionally logs to the ring buffer.
rt_apu_write:
.if APU_LOG_ENABLE == 1
  push af
  push hl
  push bc
  push de
  ; Load ring-buffer write pointer from $CB1E-$CB1F.
  ld   de, ($cb1e)          ; little-endian 16-bit load
  ; Clamp pointer to range $CE00-$CFFF.
  ld   a, d
  cp   $ce
  jr   nc, _apu_log_ptr_ok
  ld   de, $ce00
_apu_log_ptr_ok:
  ; Write record: [HL_hi, HL_lo, value, frame_lo].
  ; HL = NES APU address.
  ld   a, h
  ld   (de), a
  inc  de
  ld   a, l
  ld   (de), a
  inc  de
  ; value is still on stack (from push af).
  pop  af
  push af
  ld   (de), a              ; value
  inc  de
  ld   a, ($cb04)           ; frame counter low byte
  ld   (de), a
  inc  de
  ld   a, $aa               ; sentinel
  ld   (de), a
  inc  de
  ; Wrap pointer: if DE > $CFFF, reset to $CE00.
  ld   a, d
  cp   $d0
  jr   c, _apu_log_no_wrap
  ld   de, $ce00
_apu_log_no_wrap:
  ; Store updated write pointer.
  ld   ($cb1e), de
  pop  de
  pop  bc
  pop  hl
  pop  af
.endif
  ret

; ─── rt_apu_read ──────────────────────────────────────────────────────────────
; Entry: (HL = NES register address — ignored for v1).
; Returns A = 0.
; The only meaningful NES APU read-back is $4015 (channel status).
; Returning 0 means "no channels active" which is safe for SMB's use.
rt_apu_read:
  xor  a
  ret

; ─── rt_sound_stub ────────────────────────────────────────────────────────────
; Profile replacement for game sound engines while audio is out of scope.
; Returning immediately keeps NMI/frame work bounded and avoids spending large
; amounts of trace time in translated APU/music code whose writes are discarded.
rt_sound_stub:
  ret

; ─── rt_mapper_write ──────────────────────────────────────────────────────────
; Entry: A = value, HL = NES bus address.
; NROM has no mapper registers; all writes are discarded.
; TODO: Replace with mapper.s for banked mappers (MMC1, MMC3, etc.).
rt_mapper_write:
  ret

.ends
