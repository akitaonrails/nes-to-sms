# NES ground-truth capture tools

See `docs/visual-parity.md` for the full workflow.

- `compare_frames.py` — cell-level diff between a `frame-diff FD_NES_DUMP`
  ground-truth PPM and a `trace-sms` checkpoint PPM (quantizes NES colors to
  the SMS 2-bit space; flags cells beyond quantization distance).
- `gd2png.py` — converts FCEUX `gui.gdscreenshot()` truecolor gd dumps to PNG.
- `gen_capture_lua.py` — generates a state-keyed FCEUX Lua capture script from
  a trace-sms buttons file (for the `nesref` compose service). NOTE: Ubuntu
  jammy's fceux 2.5.0 currently crashes under xvfb + SDL dummy audio; the
  primary, frame-exact oracle is `FD_NES_DUMP` in frame-diff.
