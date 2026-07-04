# Audio Plan — NES APU → SMS PSG (Phase F)

Researched 2026-07-04. This is the F.0 "sound boundary decision" plus the
execution plan. Decision: **(a) translate the NES sound engine through the
normal pipeline and emulate the APU at the register level in the runtime**
("APU shim"), not (b) a game-specific PSG replacement score. Rationale at
the end.

## 1. The two sound systems

| | NES APU (2A03) | SMS PSG (SN76489, VDP-integrated) |
|---|---|---|
| Channels | 2 pulse + 1 triangle + 1 noise + 1 DMC | 3 square tones + 1 noise |
| Tone period | 11-bit; f = 1789773/(16(P+1)) pulse, /(32(P+1)) triangle | 10-bit; f = 3579545/(32·N) |
| Duty | pulses: 12.5/25/50/75% | fixed 50% squares |
| Volume | 4-bit linear, per channel; hardware envelope decay | 4-bit attenuation in 2 dB steps; no envelopes |
| Sweep | hardware sweep units on both pulses | none |
| Length | hardware length counters, 240 Hz frame sequencer | none |
| Noise | 15-bit LFSR, 16 timer rates, two feedback modes | 15/16-stage LFSR, **3 fixed rates** (clock/512/1024/2048) or tone-2-driven; white or periodic (1/16-duty) |
| Samples | DMC delta modulation | none (PCM only via CPU-timed volume writes — not applicable) |

Sources: [SMS Power SN76489](https://www.smspower.org/Development/SN76489),
[nesdev APU reference], [nesdev forum on SN76489](https://forums.nesdev.org/viewtopic.php?t=15562),
[SMS Power NES→SMS conversion thread](https://www.smspower.org/forums/15896-HowDoIConvertNesMusicToSmsMusicWithMod2psg2).

## 2. The lucky break: exact pulse pitch conversion

The SMS Z80/PSG clock (3,579,545 Hz) is **exactly 2×** the NES CPU clock
(1,789,773 Hz) — both derive from the NTSC colorburst. Setting the pulse
and tone formulas equal:

```
3579545/(32·N) = 1789773/(16·(P+1))   ⇒   N = P + 1
```

**NES pulse period P maps to PSG tone value N = P+1, bit-exact.** No
tuning tables, no cents error. The 11-bit → 10-bit width difference only
matters below ~109 Hz (P ≥ 1023), which pulse melodies rarely reach.

Triangle: f = 1789773/(32(P+1)) ⇒ **N = 2(P+1)** — same pitch, one octave
lower formula. Bass notes below ~109 Hz overflow the 10-bit register and
must octave-fold (halve N until ≤ 1023).

## 3. Channel mapping (v1)

| NES | SMS PSG | Loss / approximation |
|---|---|---|
| Pulse 1 ($4000-$4003) | Tone 0 | duty → 50% square (timbre only) |
| Pulse 2 ($4004-$4007) | Tone 1 | duty → 50% square |
| Triangle ($4008-$400B) | Tone 2 | timbre (triangle→square); fixed mid attenuation (NES triangle has no volume); bass below ~109 Hz octave-folds up |
| Noise ($400C-$400F) | Noise, white mode | 16 NES rates → nearest of 3 PSG rates (index 0-7→clock/512, 8-11→/1024, 12-15→/2048) |
| DMC ($4010-$4013) | silent | documented; SMB uses DMC only for incidental thuds |

Volume: NES 4-bit linear → PSG 4-bit 2 dB attenuation via a 16-entry LUT
(`atten = round(-10·log10(v/15))`, v=0 → $F/off). Perceptually close
enough at this resolution.

Deliberately **not** used in v1: the tone-2-driven periodic-noise bass
trick (4 octaves below tone 2, the classic SMS bass sound). It would give
the triangle its real bass register, but it couples the noise channel to
tone 2 and SMB plays percussion constantly. Revisit as a v2 polish item
with a per-frame arbitration heuristic (use periodic-noise bass only on
frames where the NES noise channel is silent or its volume is low).

## 4. Architecture: translated engine + APU register shim

SMB's sound engine ($F2D0-$F7xx) is ordinary 6502 code that writes APU
registers. It is currently the **only** profile `[[replacement]]`
(`rt_sound_stub`), with 22 deferred unresolved labels behind it. The plan
is to delete the replacement, let the engine translate like all other
code, and make the runtime's `rt_apu_write` a real APU model:

```
translated SoundEngine ──rt_apu_write──▶ APU register shadow ($4000-$4017)
                                              │
irq_handler, once per frame ──▶ apu_frame_tick:
    · envelope decay ×4 quarter-frames (240 Hz ≈ 4/frame)
    · length counters + sweep units ×2 half-frames
    · compute effective (period, volume) per channel
    · diff against PSG cache, write only changes to port $7F
```

- **Register shadow + sequencer state** (~48 bytes) in the free
  `$CB26-$CB7F` runtime RAM window: $4000-$4017 shadow, per-channel
  envelope dividers/decay levels, length counters, sweep dividers/reload
  flags, and a 8-byte "last written to PSG" cache to suppress redundant
  port writes (~12 OUT bytes per frame worst case — negligible even
  against our tight budget).
- **Hardware semantics emulated**: envelope decay (looping and one-shot),
  length-counter channel gating, sweep pitch bends (SMB uses these for
  SFX), $4015 enable bits and length-status reads, $4017 sequencer-mode
  write (approximated: 4-step and 5-step both tick 4/2 per frame).
- **Reads**: $4015 returns length-counter status from the shadow (SMB
  polls it rarely, but fail-open here means wrong SFX arbitration).

Why (a) and not (b) (PSG-native replacement score): (b) needs per-game
authoring — exactly the hand-port anti-pattern this project forbids — and
the parity harness cannot validate it. (a) is fully generic: any NROM
game's sound engine translates the same way, and the engine's RAM state
becomes byte-comparable against the NES reference.

## 5. Validation strategy

1. **RAM parity (automatic, strongest):** removing the stub means the
   sound engine's working RAM ($F0-$FF, $07B0-$07CF) must evolve exactly
   like the NES's. Drop `FD_EXCLUDE_AUDIO` from the frame-diff route runs
   — the existing 4900-frame NO-DIVERGENCE gate then covers the entire
   translated sound engine's logic for free.
2. **Pitch-trajectory diff (new, objective):** frame-diff already sees
   every ref APU write; teach trace-sms's bus to log PSG port writes.
   Convert both sides to Hz per channel per frame and compare with a
   small tolerance — catches shim bugs (wrong envelope rate, sweep sign,
   octave-fold errors) without ears.
3. **Listening test:** Mednafen with `-sound 1`; acceptance per the
   master plan is "the SMB theme is recognizable", plus the coin/jump/
   stomp SFX being identifiable.

## 6. Execution steps

- **F.1 Promote the sound engine.** Remove the `rt_sound_stub`
  replacement; the 22 deferred labels resolve; expect ~1.5-2 KiB of new
  translated code (also moves PRG coverage toward the 95% goal). Route
  must stay green with audio RAM included in the diff.
- **F.2 APU shim state + writes.** Replace `apu_stub.s` ring buffer with
  the register shadow; implement the volume LUT and the N=P+1 / N=2(P+1)
  period conversions with octave-fold.
- **F.3 Frame tick.** Envelope/length/sweep sequencer in `apu_frame_tick`
  called from `irq_handler` after the translated NMI; PSG write-back with
  change suppression.
- **F.4 Noise mapping.** Rate table + white mode; volume from the noise
  envelope.
- **F.5 Validation.** PSG write logging in trace-sms; pitch-trajectory
  comparison in frame-diff; drop FD_EXCLUDE_AUDIO from the acceptance
  documentation once green.
- **F.6 Listen.** Mednafen/GPGX audio on; tune the triangle attenuation
  constant and the noise rate table by ear.

## 7. Risks

- **Frame budget:** the sound engine adds real per-frame NMI work (SMB's
  engine is a few thousand cycles on the 6502 → ~5-10× that in Z80). We
  are already ~8× over real-time budget; this worsens wall-clock speed in
  stock Mednafen slightly but changes nothing structurally (overclocked
  GPGX/MAME remains the full-speed path).
- **Envelope/sweep approximation:** ticking 4/2 per video frame instead
  of the true 240 Hz sequencer skews decay/bend rates by <2%; inaudible
  at "recognizable" fidelity.
- **Triangle bass register:** octave-folded bass will sound thin in
  underground/castle themes; the periodic-noise bass trick is the known
  fix, deferred to v2 arbitration.
- **Mednafen SMS PSG variant:** Sega's integrated PSG differs slightly
  from TI's (16- vs 15-stage periodic LFSR, tone=0 behavior). We only use
  white noise and N≥1 in v1, avoiding both quirks.
