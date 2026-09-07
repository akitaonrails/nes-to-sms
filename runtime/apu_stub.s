; apu_stub.s — NES APU register shim -> SMS PSG (SN76489).
;
; Architecture (docs/audio-plan.md): the translated NES sound engine writes
; APU registers exactly as on the NES; rt_apu_write stores them into a
; shadow and applies the write side effects (length reload, envelope
; restart, sweep reload). Once per frame, apu_frame_tick emulates the APU
; frame sequencer (envelopes and the triangle linear counter x4 quarter
; frames, length counters and sweeps x2 half frames), computes each
; channel's effective (period, volume), and writes only the *changes* to
; the PSG on port $7F.
;
; Pitch conversion (exact — the SMS PSG clock is 2x the NES CPU clock):
;   pulse:    N_psg = P_nes + 1
;   triangle: N_psg = 2 * (P_nes + 1)   (octave-fold while N > 1023)
;
; Channel map: pulse1->tone0, pulse2->tone1, triangle->tone2 (fixed
; attenuation), noise->PSG white noise (nearest of 3 rates), DMC silent.
;
; RAM (inside the free $CB26-$CB7F runtime window):
;   $CB30-$CB47  APU register shadow ($4000-$4017)
;   $CB48/$CB49  pulse1 envelope divider / decay level
;   $CB4A/$CB4B  pulse2 envelope divider / decay level
;   $CB4C/$CB4D  noise  envelope divider / decay level
;   $CB4E,$CB4F  pulse1/pulse2 length counters
;   $CB50,$CB51  triangle/noise length counters
;   $CB52,$CB53  pulse1/pulse2 sweep dividers
;   $CB54        flags: b0/b1/b2 env-start p1/p2/noise,
;                       b3/b4 sweep-reload p1/p2, b5 tri linear-reload
;   $CB55        triangle linear counter
;   $CB56-$CB5D  PSG cache: tone0 lo/hi, tone1 lo/hi, tone2 lo/hi,
;                noise ctrl byte, frame-loop temp
;   $CB5E-$CB61  PSG attenuation cache ch0..ch3
;
; rt_mapper_write owns mapper-2 writes; see its transaction notes below.

.define APU_SHADOW   $CB30
.define ENV_P1       $CB48
.define ENV_P2       $CB4A
.define ENV_NOISE    $CB4C
.define LEN_P1       $CB4E
.define LEN_P2       $CB4F
.define LEN_TRI      $CB50
.define LEN_NOISE    $CB51
.define SWEEP_P1     $CB52
.define SWEEP_P2     $CB53
.define APU_FLAGS    $CB54
.define TRI_LINEAR   $CB55
.define PSG_CACHE    $CB56
.define APU_FRAME_TMP $CB5D
.define PSG_ATTN     $CB5E
.define PSG_PORT     $7F
; Triangle loudness, derived from the NES APU mixer curves (nesdev):
; at SMB's typical pulse volumes (8-12), the full-scale triangle sits
; +5..+8 dB above a pulse after fundamental-amplitude scaling (25%-duty
; square 0.90x, triangle 0.81x) — i.e. it wants ~0 dB attenuation to
; match the NES balance. Rendering a triangle as a PSG square adds
; harmonic harshness, so back off 4 dB: attenuation step 2. (Was $04 =
; -8 dB: measurably too quiet vs the NES mix.) Final polish by ear.
.define TRI_ATTN     $02

.section "apu_shim" free

; ─── rt_apu_write ─────────────────────────────────────────────────────────────
; Entry: A = value, HL = NES register address ($4000-$4017).
; Stores to the shadow and applies NES write side effects.
; Preserves AF and DE. Clobbers BC and HL. Keep this shallow: translated code
; can reach APU writes while the native stack is at the frame/NMI low-water.
rt_apu_write:
  ld   b, a                 ; B = value
  ; Preserve caller AF without spending native stack. Like the PPU wrappers,
  ; capture the prior interrupt state and keep interrupts disabled while
  ; alternate AF owns caller flags; otherwise an IRQ-side PPU write could reuse
  ; alternate AF and corrupt the preserved store flags.
  ex   af, af'
  ld   a, i                 ; P/V := IFF2
  di
  jp   po, _aw_iff_disabled
  ld   a, $01
  jr   _aw_iff_recorded
_aw_iff_disabled:
  xor  a
_aw_iff_recorded:
  ld   (APU_FRAME_TMP), a
  ; Bounds: only $4000-$4017.
  ld   a, h
  cp   $40
  jr   nz, _aw_done
  ld   a, l
  cp   $18
  jr   nc, _aw_done
  ; Shadow store: ($CB30 + reg index).
  ld   c, a                 ; C = reg index 0..$17
  ld   h, >APU_SHADOW
  ld   a, <APU_SHADOW
  add  a, c
  ld   l, a
  ld   (hl), b
  ; Side effects by register index.
  ld   a, c
  cp   $01
  jr   z, _aw_sweep1
  cp   $05
  jr   z, _aw_sweep2
  cp   $03
  jr   z, _aw_len1
  cp   $07
  jr   z, _aw_len2
  cp   $0B
  jr   z, _aw_len_tri
  cp   $0F
  jp   z, _aw_len_noise
  cp   $15
  jp   z, _aw_enable
_aw_done:
  ld   a, (APU_FRAME_TMP)
  or   a
  jr   z, _aw_done_no_ei
  ex   af, af'
  ei
  ret
_aw_done_no_ei:
  ex   af, af'
  ret

_aw_sweep1:
  ; $4001 write: set p1 sweep reload flag.
  ld   hl, APU_FLAGS
  set  3, (hl)
  jp   _aw_done
_aw_sweep2:
  ld   hl, APU_FLAGS
  set  4, (hl)
  jp   _aw_done

_aw_len1:
  ; $4003 write: reload p1 length (if channel enabled), restart envelope.
  ld   a, (APU_SHADOW+$15)
  bit  0, a
  jr   z, _aw_len1_env
  ; Inline B(value)>>3 -> length-table lookup. A helper call spends the word
  ; saved by dropping BC preservation and can cross the stack guard.
  ld   a, b
  rrca
  rrca
  rrca
  and  $1f
  ld   hl, _apu_length_table
  add  a, l
  ld   l, a
  jr   nc, _aw_len1_no_carry
  inc  h
_aw_len1_no_carry:
  ld   a, (hl)
  ld   (LEN_P1), a
_aw_len1_env:
  ld   hl, APU_FLAGS
  set  0, (hl)
  jp   _aw_done

_aw_len2:
  ld   a, (APU_SHADOW+$15)
  bit  1, a
  jr   z, _aw_len2_env
  ld   a, b
  rrca
  rrca
  rrca
  and  $1f
  ld   hl, _apu_length_table
  add  a, l
  ld   l, a
  jr   nc, _aw_len2_no_carry
  inc  h
_aw_len2_no_carry:
  ld   a, (hl)
  ld   (LEN_P2), a
_aw_len2_env:
  ld   hl, APU_FLAGS
  set  1, (hl)
  jp   _aw_done

_aw_len_tri:
  ; $400B: reload tri length (if enabled), set linear reload flag.
  ld   a, (APU_SHADOW+$15)
  bit  2, a
  jr   z, _aw_len_tri_lin
  ld   a, b
  rrca
  rrca
  rrca
  and  $1f
  ld   hl, _apu_length_table
  add  a, l
  ld   l, a
  jr   nc, _aw_len_tri_no_carry
  inc  h
_aw_len_tri_no_carry:
  ld   a, (hl)
  ld   (LEN_TRI), a
_aw_len_tri_lin:
  ld   hl, APU_FLAGS
  set  5, (hl)
  jp   _aw_done

_aw_len_noise:
  ld   a, (APU_SHADOW+$15)
  bit  3, a
  jr   z, _aw_len_noise_env
  ld   a, b
  rrca
  rrca
  rrca
  and  $1f
  ld   hl, _apu_length_table
  add  a, l
  ld   l, a
  jr   nc, _aw_len_noise_no_carry
  inc  h
_aw_len_noise_no_carry:
  ld   a, (hl)
  ld   (LEN_NOISE), a
_aw_len_noise_env:
  ld   hl, APU_FLAGS
  set  2, (hl)
  jp   _aw_done

_aw_enable:
  ; $4015 write: clearing a channel's enable bit zeroes its length counter.
  xor  a
  bit  0, b
  jr   nz, _aw_en1
  ld   (LEN_P1), a
_aw_en1:
  bit  1, b
  jr   nz, _aw_en2
  ld   (LEN_P2), a
_aw_en2:
  bit  2, b
  jr   nz, _aw_en3
  ld   (LEN_TRI), a
_aw_en3:
  bit  3, b
  jr   nz, _aw_en4
  ld   (LEN_NOISE), a
_aw_en4:
  jp   _aw_done

; B = raw register value; returns A = length_table[B >> 3]. Clobbers HL.
_len_lookup:
  ld   a, b
  rrca
  rrca
  rrca
  and  $1F
  ld   hl, _apu_length_table
  add  a, l
  ld   l, a
  jr   nc, _ll_no_carry
  inc  h
_ll_no_carry:
  ld   a, (hl)
  ret

_apu_length_table:
.db 10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14
.db 12, 16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30

; ─── rt_apu_read ──────────────────────────────────────────────────────────────
; Entry: HL = NES register address. Only $4015 is meaningful: returns the
; per-channel length>0 status bits, like the NES.
rt_apu_read:
  push bc
  ld   a, h
  cp   $40
  jr   nz, _ar_zero
  ld   a, l
  cp   $15
  jr   nz, _ar_zero
  ld   b, $00
  ld   a, (LEN_P1)
  or   a
  jr   z, _ar_p2
  set  0, b
_ar_p2:
  ld   a, (LEN_P2)
  or   a
  jr   z, _ar_tri
  set  1, b
_ar_tri:
  ld   a, (LEN_TRI)
  or   a
  jr   z, _ar_noise
  set  2, b
_ar_noise:
  ld   a, (LEN_NOISE)
  or   a
  jr   z, _ar_ret
  set  3, b
_ar_ret:
  ld   a, b
  pop  bc
  ret
_ar_zero:
  xor  a
  pop  bc
  ret

; ─── apu_frame_tick ───────────────────────────────────────────────────────────
; Called once per video frame from irq_handler. Approximates the NES 240 Hz
; frame sequencer with 4 quarter-frame and 2 half-frame ticks, then updates
; the PSG.
; Clobbers AF, BC, DE, HL. irq_handler already saved the interrupted register
; set, so keeping a second outer save set here only burns scarce nested stack.
apu_frame_tick:
  ; ---- 4 quarter frames: envelopes + triangle linear counter ----
  ld   a, 4
  ld   (APU_FRAME_TMP), a
_qf_loop:
.ifdef CV1_COHERENT_BG
  ; Audio remains DI: poll the committed HUD only where every register is
  ; dead. All sequencer state is RAM-backed; no tick or PSG write is skipped.
  call rt_cv1_hud_audio_poll
.endif
  ld   a, (APU_SHADOW+$00)
  ld   d, a
  ; Inline _env_tick for pulse 1 (env-start bit 0). The call frame alone can
  ; cross the native stack guard in deep translated NMI chains.
  ld   hl, APU_FLAGS
  bit  0, (hl)
  jr   z, _qf_p1_run
  res  0, (hl)
  ld   hl, ENV_P1
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   (hl), 15
  jr   _qf_p1_done
_qf_p1_run:
  ld   hl, ENV_P1
  ld   a, (hl)
  or   a
  jr   z, _qf_p1_fire
  dec  (hl)
  jr   _qf_p1_done
_qf_p1_fire:
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   a, (hl)
  or   a
  jr   z, _qf_p1_loop
  dec  (hl)
  jr   _qf_p1_done
_qf_p1_loop:
  bit  5, d
  jr   z, _qf_p1_done
  ld   (hl), 15
_qf_p1_done:

  ld   a, (APU_SHADOW+$04)
  ld   d, a
  ; Inline _env_tick for pulse 2 (env-start bit 1).
  ld   hl, APU_FLAGS
  bit  1, (hl)
  jr   z, _qf_p2_run
  res  1, (hl)
  ld   hl, ENV_P2
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   (hl), 15
  jr   _qf_p2_done
_qf_p2_run:
  ld   hl, ENV_P2
  ld   a, (hl)
  or   a
  jr   z, _qf_p2_fire
  dec  (hl)
  jr   _qf_p2_done
_qf_p2_fire:
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   a, (hl)
  or   a
  jr   z, _qf_p2_loop
  dec  (hl)
  jr   _qf_p2_done
_qf_p2_loop:
  bit  5, d
  jr   z, _qf_p2_done
  ld   (hl), 15
_qf_p2_done:

  ld   a, (APU_SHADOW+$0C)
  ld   d, a
  ; Inline _env_tick for noise (env-start bit 2).
  ld   hl, APU_FLAGS
  bit  2, (hl)
  jr   z, _qf_n_run
  res  2, (hl)
  ld   hl, ENV_NOISE
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   (hl), 15
  jr   _qf_n_done
_qf_n_run:
  ld   hl, ENV_NOISE
  ld   a, (hl)
  or   a
  jr   z, _qf_n_fire
  dec  (hl)
  jr   _qf_n_done
_qf_n_fire:
  ld   a, d
  and  $0F
  ld   (hl), a
  inc  hl
  ld   a, (hl)
  or   a
  jr   z, _qf_n_loop
  dec  (hl)
  jr   _qf_n_done
_qf_n_loop:
  bit  5, d
  jr   z, _qf_n_done
  ld   (hl), 15
_qf_n_done:
  ; Triangle linear counter.
  ld   hl, APU_FLAGS
  bit  5, (hl)
  jr   z, _lin_no_reload
  ld   a, (APU_SHADOW+$08)
  and  $7F
  ld   (TRI_LINEAR), a
  jr   _lin_ctrl
_lin_no_reload:
  ld   a, (TRI_LINEAR)
  or   a
  jr   z, _lin_ctrl
  dec  a
  ld   (TRI_LINEAR), a
_lin_ctrl:
  ; Control bit clear -> clear the reload flag.
  ld   a, (APU_SHADOW+$08)
  bit  7, a
  jr   nz, _lin_done
  ld   hl, APU_FLAGS
  res  5, (hl)
_lin_done:
  ld   hl, APU_FRAME_TMP
  dec  (hl)
  jr   z, _qf_done
  jp   _qf_loop
_qf_done:

  ; ---- 2 half frames: length counters + sweeps ----
  ld   a, 2
  ld   (APU_FRAME_TMP), a
_hf_loop:
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ld   a, (APU_SHADOW+$00)
  bit  5, a
  ld   hl, LEN_P1
  call z, _len_tick
  ld   a, (APU_SHADOW+$04)
  bit  5, a
  ld   hl, LEN_P2
  call z, _len_tick
  ld   a, (APU_SHADOW+$08)
  bit  7, a
  ld   hl, LEN_TRI
  call z, _len_tick
  ld   a, (APU_SHADOW+$0C)
  bit  5, a
  ld   hl, LEN_NOISE
  call z, _len_tick
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ld   c, 0
  call _sweep_tick
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ld   c, 1
  call _sweep_tick
  ld   hl, APU_FRAME_TMP
  dec  (hl)
  jr   nz, _hf_loop

  ; ---- Output stage ----
  jp   _psg_update             ; tail-call: save one native frame in IRQ path

; HL -> length counter byte; decrement if > 0.
_len_tick:
  ld   a, (hl)
  or   a
  ret  z
  dec  (hl)
  ret

; Envelope tick. HL -> {divider, level}, D = channel volume register value,
; C = channel's env-start flag bit (0=p1, 1=p2, 2=noise).
_env_tick:
  ; Stackless: save envelope pointer in B:E. apu_frame_tick owns/clobbers
  ; BC/DE/HL, and D must keep the volume register value.
  ld   b, h
  ld   e, l
  ld   hl, APU_FLAGS
  ld   a, c
  or   a
  jr   nz, _et_not0
  bit  0, (hl)
  jr   nz, _et_start0
  jr   _et_run
_et_not0:
  cp   1
  jr   nz, _et_not1
  bit  1, (hl)
  jr   nz, _et_start1
  jr   _et_run
_et_not1:
  bit  2, (hl)
  jr   nz, _et_start2
  jr   _et_run
_et_start0:
  res  0, (hl)
  jr   _et_restart
_et_start1:
  res  1, (hl)
  jr   _et_restart
_et_start2:
  res  2, (hl)
_et_restart:
  ld   h, b
  ld   l, e
  ld   a, d
  and  $0F
  ld   (hl), a              ; divider = V
  inc  hl
  ld   (hl), 15             ; level = 15
  ret
_et_run:
  ld   h, b
  ld   l, e
  ld   a, (hl)
  or   a
  jr   z, _et_fire
  dec  (hl)
  ret
_et_fire:
  ld   a, d
  and  $0F
  ld   (hl), a              ; divider = V
  inc  hl
  ld   a, (hl)
  or   a
  jr   z, _et_loop
  dec  (hl)
  ret
_et_loop:
  bit  5, d                 ; loop flag
  ret  z
  ld   (hl), 15
  ret

; Sweep tick for pulse channel C (0 or 1). Updates the period shadow in
; place when the sweep unit fires. Pulse 1 uses ones'-complement negate.
_sweep_tick:
  ; D = sweep register value.
  ld   a, c
  or   a
  jr   nz, _st_reg2
  ld   a, (APU_SHADOW+$01)
  jr   _st_have
_st_reg2:
  ld   a, (APU_SHADOW+$05)
_st_have:
  ld   d, a
  ; HL -> divider byte.
  ld   hl, SWEEP_P1
  ld   a, c
  or   a
  jr   z, _st_div
  ld   hl, SWEEP_P2
_st_div:
  ; Reload pending?
  ; Stackless: save divider pointer in B:E while testing APU_FLAGS.
  ld   b, h
  ld   e, l
  ld   hl, APU_FLAGS
  ld   a, c
  or   a
  jr   nz, _st_rel2
  bit  3, (hl)
  jr   _st_relq
_st_rel2:
  bit  4, (hl)
_st_relq:
  ld   h, b
  ld   l, e
  jr   z, _st_no_reload
  ; Reload: divider = sweep period; clear flag; skip applying this tick.
  ld   a, d
  rrca
  rrca
  rrca
  rrca
  and  $07
  ld   (hl), a
  ld   hl, APU_FLAGS
  ld   a, c
  or   a
  jr   nz, _st_clr2
  res  3, (hl)
  ret
_st_clr2:
  res  4, (hl)
  ret
_st_no_reload:
  ld   a, (hl)
  or   a
  jr   z, _st_fire
  dec  (hl)
  ret
_st_fire:
  ; Divider expired: reload it, then apply if enabled && shift > 0.
  ld   a, d
  rrca
  rrca
  rrca
  rrca
  and  $07
  ld   (hl), a
  bit  7, d
  ret  z
  ld   a, d
  and  $07
  ret  z
  push bc
  ld   b, a                 ; B = shift count
  ; HL = current 11-bit period.
  ld   a, c
  or   a
  jr   nz, _st_p2
  ld   a, (APU_SHADOW+$02)
  ld   l, a
  ld   a, (APU_SHADOW+$03)
  jr   _st_pl
_st_p2:
  ld   a, (APU_SHADOW+$06)
  ld   l, a
  ld   a, (APU_SHADOW+$07)
_st_pl:
  and  $07
  ld   h, a
  ; Sweep units skip when period < 8.
  or   a
  jr   nz, _st_ge8
  ld   a, l
  cp   8
  jr   c, _st_pop_ret
_st_ge8:
  ; DE = period >> shift.
  ld   d, h
  ld   e, l
_st_shift:
  srl  d
  rr   e
  djnz _st_shift
  ; Refetch the sweep register (D was consumed by the shift).
  ld   a, c
  or   a
  jr   nz, _st_ref2
  ld   a, (APU_SHADOW+$01)
  jr   _st_refd
_st_ref2:
  ld   a, (APU_SHADOW+$05)
_st_refd:
  bit  3, a
  jr   z, _st_add
  ; Negate: HL -= DE (pulse 1: one extra -1, ones' complement).
  or   a
  sbc  hl, de
  ld   a, c
  or   a
  jr   nz, _st_store
  dec  hl
  jr   _st_store
_st_add:
  add  hl, de
  ; Target > $7FF: the NES mutes rather than writes; skip the store.
  ld   a, h
  cp   $08
  jr   nc, _st_pop_ret
_st_store:
  bit  7, h
  jr   nz, _st_pop_ret      ; underflow guard
  ld   a, c
  or   a
  jr   nz, _st_store2
  ld   a, l
  ld   (APU_SHADOW+$02), a
  ld   a, (APU_SHADOW+$03)
  and  $F8
  or   h
  ld   (APU_SHADOW+$03), a
  jr   _st_pop_ret
_st_store2:
  ld   a, l
  ld   (APU_SHADOW+$06), a
  ld   a, (APU_SHADOW+$07)
  and  $F8
  or   h
  ld   (APU_SHADOW+$07), a
_st_pop_ret:
  pop  bc
  ret

; ─── _psg_update ─────────────────────────────────────────────────────────────
; Computes each channel's PSG tone/attenuation and writes only changes.
_psg_update:
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ; ---- pulse 1 -> tone 0 ----
  ld   a, (APU_SHADOW+$15)
  bit  0, a
  jr   z, _pu_p1_off
  ld   a, (LEN_P1)
  or   a
  jr   z, _pu_p1_off
  ld   a, (APU_SHADOW+$02)
  ld   l, a
  ld   a, (APU_SHADOW+$03)
  and  $07
  ld   h, a
  or   a
  jr   nz, _pu_p1_on
  ld   a, l
  cp   8
  jr   c, _pu_p1_off        ; period < 8 mutes on the NES
_pu_p1_on:
  inc  hl                   ; N = P + 1
  ld   c, 0
  call _psg_tone
  ld   a, (APU_SHADOW+$00)
  bit  4, a
  jr   z, _pu_p1_env
  and  $0F
  jr   _pu_p1_vol
_pu_p1_env:
  ld   a, (ENV_P1+1)
_pu_p1_vol:
  ld   c, 0
  call _psg_attn_vol
  jr   _pu_p2
_pu_p1_off:
  ld   c, 0
  call _psg_attn_off

_pu_p2:
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ; ---- pulse 2 -> tone 1 ----
  ld   a, (APU_SHADOW+$15)
  bit  1, a
  jr   z, _pu_p2_off
  ld   a, (LEN_P2)
  or   a
  jr   z, _pu_p2_off
  ld   a, (APU_SHADOW+$06)
  ld   l, a
  ld   a, (APU_SHADOW+$07)
  and  $07
  ld   h, a
  or   a
  jr   nz, _pu_p2_on
  ld   a, l
  cp   8
  jr   c, _pu_p2_off
_pu_p2_on:
  inc  hl
  ld   c, 1
  call _psg_tone
  ld   a, (APU_SHADOW+$04)
  bit  4, a
  jr   z, _pu_p2_env
  and  $0F
  jr   _pu_p2_vol
_pu_p2_env:
  ld   a, (ENV_P2+1)
_pu_p2_vol:
  ld   c, 1
  call _psg_attn_vol
  jr   _pu_tri
_pu_p2_off:
  ld   c, 1
  call _psg_attn_off

_pu_tri:
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ; ---- triangle -> tone 2 ----
  ld   a, (APU_SHADOW+$15)
  bit  2, a
  jr   z, _pu_tri_off
  ld   a, (LEN_TRI)
  or   a
  jr   z, _pu_tri_off
  ld   a, (TRI_LINEAR)
  or   a
  jr   z, _pu_tri_off
  ld   a, (APU_SHADOW+$0A)
  ld   l, a
  ld   a, (APU_SHADOW+$0B)
  and  $07
  ld   h, a
  or   a
  jr   nz, _pu_tri_on
  ld   a, l
  cp   2
  jr   c, _pu_tri_off       ; ultrasonic on the NES; treat as silent
_pu_tri_on:
  inc  hl
  add  hl, hl               ; N = 2 * (P + 1)
_pu_tri_fold:
  ld   a, h
  cp   $04
  jr   c, _pu_tri_fits      ; fits 10 bits
  srl  h
  rr   l
  jr   _pu_tri_fold
_pu_tri_fits:
  ld   c, 2
  call _psg_tone
  ld   a, TRI_ATTN
  ld   c, 2
  call _psg_attn_raw
  jr   _pu_noise
_pu_tri_off:
  ld   c, 2
  call _psg_attn_off

_pu_noise:
.ifdef CV1_COHERENT_BG
  call rt_cv1_hud_audio_poll
.endif
  ; ---- noise -> PSG white noise ----
  ld   a, (APU_SHADOW+$15)
  bit  3, a
  jr   z, _pu_noise_off
  ld   a, (LEN_NOISE)
  or   a
  jr   z, _pu_noise_off
  ; NES period index 0-7 -> PSG rate clock/512, 8-11 -> /1024, 12-15 -> /2048.
  ld   a, (APU_SHADOW+$0E)
  and  $0F
  cp   8
  jr   c, _pu_nr0
  cp   12
  jr   c, _pu_nr1
  ld   a, $E6               ; white, clock/2048
  jr   _pu_nwr
_pu_nr1:
  ld   a, $E5               ; white, clock/1024
  jr   _pu_nwr
_pu_nr0:
  ld   a, $E4               ; white, clock/512
_pu_nwr:
  ; Only write when changed: a noise-register write resets the LFSR phase.
  ld   hl, PSG_CACHE+6
  cp   (hl)
  jr   z, _pu_nvol
  ld   (hl), a
  out  (PSG_PORT), a
_pu_nvol:
  ld   a, (APU_SHADOW+$0C)
  bit  4, a
  jr   z, _pu_noise_env
  and  $0F
  jr   _pu_noise_vol
_pu_noise_env:
  ld   a, (ENV_NOISE+1)
_pu_noise_vol:
  ld   c, 3
  call _psg_attn_vol
  ret
_pu_noise_off:
  ld   c, 3
  call _psg_attn_off
  ret

; Write 10-bit tone HL to PSG channel C (0-2) if changed.
; Preserves C; clobbers A, D, E, HL.
_psg_tone:
  ; Clamp N to 1..1023.
  ld   a, h
  and  $03
  ld   h, a
  or   l
  jr   nz, _pt_nz
  inc  l
_pt_nz:
  ; DE -> cache entry (PSG_CACHE + 2*C; same page).
  ld   a, c
  add  a, a
  add  a, <PSG_CACHE
  ld   e, a
  ld   d, >PSG_CACHE
  ld   a, (de)
  cp   l
  jr   nz, _pt_write
  inc  de
  ld   a, (de)
  dec  de
  cp   h
  ret  z
_pt_write:
  ld   a, l
  ld   (de), a
  inc  de
  ld   a, h
  ld   (de), a
  ; Latch byte: %1 cc 0 dddd (tone low 4 bits).
  ld   a, c
  rrca
  rrca
  rrca
  and  $60
  or   $80
  ld   d, a
  ld   a, l
  and  $0F
  or   d
  out  (PSG_PORT), a
  ; Data byte: %00 dddddd (tone bits 9-4).
  ld   a, l
  rrca
  rrca
  rrca
  rrca
  and  $0F
  ld   d, a
  ld   a, h
  rlca
  rlca
  rlca
  rlca
  and  $30
  or   d
  out  (PSG_PORT), a
  ret

; A = NES linear volume 0-15, C = PSG channel: map through the dB LUT.
; Clobbers A, D, E, HL; preserves C.
_psg_attn_vol:
  ld   hl, _vol_to_attn
  and  $0F
  add  a, l
  ld   l, a
  jr   nc, _pav_nc
  inc  h
_pav_nc:
  ld   a, (hl)
  ; fall through
; A = raw attenuation 0-15, C = channel: write if changed.
; Clobbers A, D, E, HL; preserves C.
_psg_attn_raw:
  ld   e, a
  ld   a, c
  add  a, <PSG_ATTN
  ld   l, a
  ld   h, >PSG_ATTN
  ld   a, (hl)
  cp   e
  jr   z, _par_done
  ld   (hl), e
  ; %1 cc 1 vvvv
  ld   a, c
  rrca
  rrca
  rrca
  and  $60
  or   $90
  or   e
  out  (PSG_PORT), a
_par_done:
  ret

_psg_attn_off:
  ld   a, $0F
  jr   _psg_attn_raw

_vol_to_attn:
.db 15, 12, 9, 7, 6, 5, 4, 3, 3, 2, 2, 1, 1, 1, 0, 0

; ─── apu_psg_init ─────────────────────────────────────────────────────────────
; Called from boot: clear shim state, invalidate the PSG cache, silence all
; PSG channels.
apu_psg_init:
  push af
  push bc
  push hl
  ; Zero $CB30-$CB55 (shadow + sequencer state).
  ld   hl, APU_SHADOW
  ld   b, $26
  xor  a
_ai_clear:
  ld   (hl), a
  inc  hl
  djnz _ai_clear
  ; Invalidate the PSG caches so the first tick writes everything.
  ld   hl, PSG_CACHE
  ld   b, 12
  ld   a, $FF
_ai_inval:
  ld   (hl), a
  inc  hl
  djnz _ai_inval
  ; Silence all four channels.
  ld   a, $9F
  out  (PSG_PORT), a
  ld   a, $BF
  out  (PSG_PORT), a
  ld   a, $DF
  out  (PSG_PORT), a
  ld   a, $FF
  out  (PSG_PORT), a
  pop  hl
  pop  bc
  pop  af
  ret

; ─── rt_sound_stub ────────────────────────────────────────────────────────────
; Retained for profiles that replace a game's sound engine outright.
rt_sound_stub:
  ret

; ─── rt_mapper_write ──────────────────────────────────────────────────────────
; Entry: A = value written, HL = NES address ($8000-$FFFF).
; Preserves AF, DE, and shadow P ($CB03); clobbers BC/HL. Mapper writes are
; outer slot-2 transactions only: nested guards trap rather than silently
; restoring a stale mapper snapshot. The one AF push is intentionally bounded.
; NROM: valid writes are discarded.
; UxROM (mapper 2): any write selects the 16 KiB PRG bank at the
; $8000-$BFFF window. The shim stores the NES bank shadow ($CB62) and
; maps the matching SMS data bank into slot 2 immediately — every
; existing PRG-window read path then sees the right bytes with zero
; per-read cost.
; MMC3 uses a separate register decoder and keeps slot 2 unchanged.
rt_mapper_write:
.ifdef NES_MMC3
  jp rt_mmc3_write
.else
.ifndef NES_PRG_BANK_BASE
  ret
.else
  push af
  ld   b, a                  ; raw write; caller AF remains on the native stack
  ld   a, h
  cp   $80
  jp   c, _mw_bad_address
  ld   a, ($d47f)
  or   a
  jp   nz, _mw_nested
  ; Inline frame-0 entry; mapper writes require depth zero above.
  ld   a, i
  di
  jp   po, _mw_enter_di
  ld   a, $01
  jr   _mw_enter_iff
_mw_enter_di:
  xor  a
_mw_enter_iff:
  ld   ($ca19), a
  ld   a, ($fffc)
  ld   ($ca1a), a
  ld   a, ($ffff)
  ld   ($ca1b), a
  ld   a, ($cb62)
  ld   ($ca1c), a
  ld   a, $01
  ld   ($d47f), a
  xor  a
  ld   ($fffc), a
  ; Fetch the ROM bus byte before changing the selected-bank shadow.
.if NES_PRG_BUS_CONFLICTS == 1
  ld   a, h
  cp   $c0
  jr   nc, _mw_conflict_upper
  ld   a, ($cb62)
  and  NES_PRG_BANK_MASK
  add  a, NES_PRG_BANK_BASE
  ld   ($ffff), a
  ld   a, (hl)
  jr   _mw_conflict_have_byte
_mw_conflict_upper:
  ld   a, :data_prg_high
  ld   ($ffff), a
  ld   a, h
  sub  $40                   ; $C000-$FFFF -> slot-2 $8000-$BFFF
  ld   h, a
  ld   a, (hl)
_mw_conflict_have_byte:
  and  b
  and  NES_PRG_BANK_MASK
  jr   _mw_selected
.else
  ld   a, b
  and  NES_PRG_BANK_MASK
.endif
_mw_selected:
  ld   ($cb62), a           ; NES PRG bank shadow
  xor  a
  ld   ($fffc), a           ; force ROM visibility before the final mapping
  ld   a, ($cb62)
  add  a, NES_PRG_BANK_BASE
  ld   ($ffff), a           ; slot 2 = selected NES bank's data image
  ; Commit: the new mapping is intentional, so discard (do not restore) the
  ; guard frame. Its entry IFF2 controls the final EI only.
  ld   a, ($d47f)
  cp   $01
  jp   nz, _mw_depth_corrupt
  ld   a, ($ca19)
  ld   c, a
  xor  a
  ld   ($d47f), a
  ld   a, c
  or   a
  jr   z, _mw_return_di
  pop  af
  ei
  ret
_mw_return_di:
  pop  af
  ret
_mw_bad_address:
  ld   a, RT_MAPPER_BAD_ADDRESS
  jr   _mw_trap
_mw_nested:
  ld   a, RT_MAPPER_NESTED
  jr   _mw_trap
_mw_depth_corrupt:
  ld   a, RT_MAPPER_COMMIT_BAD
_mw_trap:
  di
  ld   ($cb1d), a
_mw_halt:
  halt
  jr   _mw_halt
.endif
.endif

; ─── rt_restore_prg_window ────────────────────────────────────────────────────
; Restore slot 2 to the CURRENT NES PRG window after a temporary remap
; (CHR maps, chr data, prg_high). NROM: the single data_prg_low bank.
; Banked: the bank selected by the mapper shadow. Clobbers A.
rt_restore_prg_window:
  xor  a
  ld   ($fffc), a
.ifdef NES_PRG_BANK_BASE
  ld   a, ($cb62)
  add  a, NES_PRG_BANK_BASE
  ld   ($ffff), a
.else
  ld   a, :data_prg_low
  ld   ($ffff), a
.endif
  ret

.ends
