# Unresolved Trap Triage

Snapshot from `out/smb/reports/unresolved_labels.txt` after the 1-1 scripted
acceptance route passed. The unresolved set is still useful, but it should be
treated as a reachability/classification queue, not as the primary success
metric. SMB-specific facts here belong in `profiles/smb.toml` or runtime data;
converter changes should remain generic.

After correcting the `SetupVictoryMode` profile address (`$83A8` table entry →
`$83B0` routine start), naming the `$BDD8` block dispatch target as
`ExtraLifeMushBlock`, adding piranha init/run roots, and adding platform/lift
handler roots, and adding named enemy variant roots, the regenerated project
plus eight local gameplay entrypoints, the regenerated project reports 22
unresolved labels, 631/631 lifted routines, and 0 current lift/lower failures.

## Current acceptance route

The profile-owned route in `profiles/smb/acceptance/1-1-clear.buttons` reaches
the expected post-1-1 transition state without the unresolved trap marker:

```sh
target/release/trace-sms out/smb/sms.sms --steps 300000000 \
  --buttons-script profiles/smb/acceptance/1-1-clear.buttons \
  --expect-no-trap \
  --expect-ram 0x0760=01 \
  --expect-ram 0x075C=01 \
  --expect-ram 0x000E=07
```

That means the labels below are not reached by this one scripted path, or are
only present behind cold/deferred dispatch entries. They still matter for broad
SMB playability and for general converter coverage.

## Buckets

### Deferred sound / APU

Current policy defers audio. `profiles/smb.toml` skips `SoundEngine`, and
`validation.txt` classifies `$F2D0 SoundEngine` as hardware-accessing. These
labels are likely sound-engine internals and should stay deferred until the APU
shim/PSG plan is active:

- `L_F3BF`, `L_F3CD`, `L_F3D1`, `L_F3DF`, `L_F3F9`, `L_F3FF`, `L_F40D`
- `L_F4B6`, `L_F4BB`, `L_F518`, `L_F51E`
- `L_F5C8`, `L_F5D1`, `L_F5D3`, `L_F5E2`, `L_F5E7`, `L_F5FC`, `L_F600`,
  `L_F60F`, `L_F63B`, `L_F640`, `L_F64D`

**Action:** do not promote these as gameplay roots now. Handle them when adding
a generic APU-to-PSG/audio-runtime phase.

### Enemy initialization and run dispatch

The named table-dispatched enemy handlers are now profile roots, including
Piranha plants, platform/lift handlers, Cheep-cheeps, flying/swimming enemies,
Paratroopas, firebars, and the enemy-init terminator.

The remaining enemy/player local labels were checked against the disassembly and
added as profile roots where they are used as external transfer targets:

- `KillLakitu`, `SpawnFromMouth`, `HammerBroJumpCode`, `BowserControl`,
  `MakeBJump`, `UnderHammerBro`, `NoUnderHammerBro`, `ShrinkPlayer`

No non-sound unresolved labels remain in the current report.

### Platform / lift handlers

The high-value platform roots `InitDropPlatform`, `InitHoriPlatform`,
`InitVertPlatform`, and `MoveLargeLiftPlat` are now profile roots and no longer
unresolved. They are still broader-SMB coverage items rather than part of the
1-1 scripted acceptance path.

### End-level / victory

`SetupVictoryMode` was removed from the unresolved set by correcting its profile
root from the jump-table word at `$83A8` to the real routine at `$83B0`. It is
still cold for the 1-1 acceptance route, which transitions to 1-2 rather than
world victory, but it now discovers and lifts as `$83B0-$83BD`.

### Player / miscellaneous gameplay

No player/misc gameplay labels remain unresolved in the current report.

## Illegal-op / data-artifact suspects

The regenerated project currently has no lift/lower failures. Earlier stale
`lower_failures.txt` content listed dispatch/table-adjacent illegal-op decodes;
the report writer now removes stale optional reports on zero-failure runs.

Still worth watching in this bucket:

- `BumpBlock`'s `$BDD8` table entry is now named `ExtraLifeMushBlock` and lifts
  as `$BDD8-$BDDF`; future changes near that range should still verify
  code-vs-data boundaries.
- Remaining enemy dispatch entries around the `$C2xx` tables should be promoted
  only after confirming the bytes are code entrypoints, not table data.

**Action:** before promoting labels near these ranges, verify whether the bytes
are code or dispatch/data tables. Fail closed: data misclassification should
become profile data/range metadata, not unsupported-op lowering.

## Suggested next order

1. **World/castle victory route.** `SetupVictoryMode` now lifts, but the broader
   victory mode still needs an explicit route/test outside 1-1.
2. **Sound phase.** Keep `$Fxxx` sound labels deferred until the APU/PSG runtime
   plan is active.

Sound labels stay deferred until the audio phase.
