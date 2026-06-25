# Unresolved Trap Triage

Snapshot from `out/smb/reports/unresolved_labels.txt` after the 1-1 scripted
acceptance route passed. The unresolved set is still useful, but it should be
treated as a reachability/classification queue, not as the primary success
metric. SMB-specific facts here belong in `profiles/smb.toml` or runtime data;
converter changes should remain generic.

After correcting the `SetupVictoryMode` profile address (`$83A8` table entry →
`$83B0` routine start), naming the `$BDD8` block dispatch target as
`ExtraLifeMushBlock`, and adding piranha init/run roots, the regenerated project
reports 43 unresolved labels, 606/606 lifted routines, and 0 current lift/lower
failures.

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

These are table-dispatched enemy handlers already named in the SMB jump-engine
tables. They are broad-SMB reachable, but not required by the current 1-1 clear
route if the relevant object types do not spawn or are not interacted with.

- Init: `EndOFEnemyInitCode`, `InitHorizFlySwimEnemy`, `InitJumpGPTroopa`,
  `InitShortFirebar`
- Run/move: `MoveFlyGreenPTroopa`, `MoveFlyingCheepCheep`, `MoveJumpingEnemy`,
  `MoveSwimmingCheepCheep`, `ProcMoveRedPTroopa`

**Action:** promote/repair through profile-owned jump-engine/data metadata, not
Rust-side SMB logic. Piranha init/run are now profile roots; prioritize
platform/lift handlers, then later water/flying enemy variants.

### Platform / lift handlers

These are high-value for broad playability because platform levels need them,
even if the 1-1 script avoids them:

- Init: `InitDropPlatform`, `InitHoriPlatform`, `InitVertPlatform`
- Run: `MoveLargeLiftPlat`

**Action:** likely next gameplay bucket after piranha/victory checks. Keep the
fix profile-driven: ensure table entries are discovered/lifted and any data
regions near platform dispatch are classified correctly.

### End-level / victory

`SetupVictoryMode` was removed from the unresolved set by correcting its profile
root from the jump-table word at `$83A8` to the real routine at `$83B0`. It is
still cold for the 1-1 acceptance route, which transitions to 1-2 rather than
world victory, but it now discovers and lifts as `$83B0-$83BD`.

### Player / miscellaneous gameplay

- `L_F0D7` — player/control-adjacent cold path in the high `$F0xx` region.
- `L_C395`, `L_C5EC`, `L_CA12`, `L_D07F`, `L_D13C`, `L_E196`, `L_E1A7` —
  unnamed labels inside enemy/platform/player-adjacent regions.

**Action:** classify each against the disassembly before promoting. If a label
is only a local branch target inside an already lifted routine, the better fix
may be range ownership/fallthrough repair rather than adding more roots.

## Illegal-op / data-artifact suspects

The regenerated project currently has no lift/lower failures. Earlier stale
`lower_failures.txt` content listed dispatch/table-adjacent illegal-op decodes;
the report writer now removes stale optional reports on zero-failure runs.

Still worth watching in this bucket:

- `BumpBlock`'s `$BDD8` table entry is now named `ExtraLifeMushBlock` and lifts
  as `$BDD8-$BDDF`; future changes near that range should still verify
  code-vs-data boundaries.
- Enemy/platform dispatch entries around the `$C2xx` tables should be promoted
  only after confirming the bytes are code entrypoints, not table data.

**Action:** before promoting labels near these ranges, verify whether the bytes
are code or dispatch/data tables. Fail closed: data misclassification should
become profile data/range metadata, not unsupported-op lowering.

## Suggested next order

1. **Platform/lift handlers.** Needed for broad level coverage; keep fixes in
   profile/runtime semantics, not SMB-specific Rust branches.
2. **Enemy variants after platform coverage.** Cheep-cheep/flying/paratroopa
   movement should follow once common ground/platform play is stable.
3. **World/castle victory route.** `SetupVictoryMode` now lifts, but the broader
   victory mode still needs an explicit route/test outside 1-1.
4. **Named local labels in enemy/player regions.** Resolve `L_C395`, `L_C5EC`,
   `L_CA12`, `L_D07F`, `L_D13C`, `L_E196`, `L_E1A7`, and `L_F0D7` only after
   checking each against the disassembly for true entrypoint vs local branch.
5. **Sound phase.** Keep `$Fxxx` sound labels deferred until the APU/PSG runtime
   plan is active.

Sound labels stay deferred until the audio phase.
