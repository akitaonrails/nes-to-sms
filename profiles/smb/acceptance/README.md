# SMB Acceptance Inputs

This directory contains profile-owned input scripts for `trace-sms` acceptance
runs. They are regression data for the SMB stress target, not converter logic.

## Format

Each non-comment line is:

```text
FRAME:buttons
```

`trace-sms` holds that button set from `FRAME` until the next event. Blank
lines and `#` comments are ignored. Button names match `--buttons-at-frame`,
for example `right`, `a`, `start`, or comma-separated combinations such as
`right,a`.

## 1-1 clear smoke test

After generating and assembling `out/smb/sms.sms`, run:

```sh
target/release/trace-sms out/smb/sms.sms --steps 300000000 \
  --buttons-script profiles/smb/acceptance/1-1-clear.buttons \
  --expect-no-trap \
  --expect-ram 0x0760=01 \
  --expect-ram 0x075C=01 \
  --expect-ram 0x000E=07
```

The expectations assert that the route did not hit the unresolved-runtime trap
and reached the expected post-1-1 transition state (`area=1`, `level=1`, player
state `$0E=07`).
