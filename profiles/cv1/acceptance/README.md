# CV1 Acceptance Inputs

`stage1-smoke.buttons` is a canonical-reference input timeline. Run it with
`frame-diff` and `FD_PAUSE_START=1` so its `start` event remains NES Start:

```sh
FD_PAUSE_START=1 target/release/frame-diff \
  .roms/cv1.nes out/cv1/sms.sms --frames 3600 \
  --buttons-script profiles/cv1/acceptance/stage1-smoke.buttons --ref-only
```

On SMS, NES Start is the hardware Pause NMI, so `trace-sms` must inject it
separately. The accepted smoke route waits for CV1 gameplay state `$0018=05`,
substate `$0019=06`, then holds Right. It asserts CV1's held-input byte:

```sh
SMS_DUMP_PPM=out/cv1/graphics-acceptance/stage.ppm \
  target/release/trace-sms out/cv1/sms.sms --steps 180000000 \
  --pause-at-frame 600 --buttons-at-frame 1850:right \
  --expect-no-trap --expect-ram 0x0018=05 --expect-ram 0x0019=06 \
  --expect-ram 0x00F7=01
```

The output framebuffer must show the Stage 1 playfield and sprites rather than
the earlier title-fill/random-pattern corruption. This is a boot/input/graphics
smoke route, not a full Stage 1 completion script.

## Candle/heart regression

`heart-smoke.buttons` records the reference-NES input intent. The translated
route is `heart-smoke.sms.buttons`; it uses hardware Pause for NES Start, walks
right, and whips across the first candle group. It must cross
`$E7D0 -> $EC60` without consuming the NMI frame or trapping at `$0000`, collect
the large heart (`$0071=$0A`), and remain in gameplay state `$0018=$05`,
substate `$0019=$06`:

```sh
target/release/trace-sms out/cv1/sms.sms --steps 116000000 \
  --pause-at-frame 600 \
  --buttons-script profiles/cv1/acceptance/heart-smoke.sms.buttons \
  --expect-no-trap --expect-ram 0x0071=0A \
  --expect-ram 0x0018=05 --expect-ram 0x0019=06
```

The 116M-step budget (previously 106.2M) covers the slower whip animation
since the banked-dispatch accumulator fix (`rt_banked_dispatch` parks entry A
in `$CB15`): the whip sound trigger now receives the correct sound ID, the
sound driver runs the SFX each frame, and game logic dilates roughly 2x
against the tracer's 60K-step injection frames while the effect is active.
The candle still breaks and the large heart still drops; Simon simply needs
more injection frames to walk into it.

The focused framebuffer must still show the Stage 1 playfield rather than the
former white/stalled frame. The longer soak command is recorded in
`docs/cv1-recovery-plan.md`.

For a real-emulator check, run `docker/run_gpgx.sh out/cv1/sms.sms`. The
launcher warns when the configured 8BitDo controller has no event device;
`GPGX_REQUIRE_GAMEPAD=1` makes that condition fatal. Keyboard fallback is
arrows, `Z`/`X`, and `Enter` for SMS Pause/NES Start. Stage setup remains slow:
the first background-only frame is not gameplay, so wait until Simon appears.
