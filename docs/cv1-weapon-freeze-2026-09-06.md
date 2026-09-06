# CV1 weapon freezes — September 6, 2026

The new white-screen/held-note report is consistent with the runtime's fatal
trap loop, which disables IRQs and writes a diagnostic palette. No save state
or input recording survived the user's force quit, so this is **not a claim
that the exact reported play sequence was reproduced**.

## Confirmed defects and fixes

1. **Occupied projectile slots / insufficient hearts.** Original `$DA90`
   checks the allowed slots; `$DA7B` rejects a five-heart cost. Their shared
   `$DA9D/$DA9E` PLA pair discards the calling JSR return and bypasses the
   spawning caller. The profile now materializes and transfers ownership for
   all six source-proven callers (`DA72`, `DAA3`, `DAAC`, `DAB5`, `DAE9`,
   `DAEC`), using the existing `return_consume` contract.
2. **Returning-cross acceleration.** `$DC84` branches to `$DC37`, which was
   unresolved. Rooting the original tail discovers both exits through `$DC5F`;
   no guessed recovery address or replacement game logic is involved.
3. **Projectile catch/expiry.** `$DB08/$DB4C/$DC34` tail-jump to `$EC60`,
   which removes the object and consumes the dispatcher's arranged `$E9E5`
   return. These edges now discard the software continuation. The generic
   escape helper validates and retains already-materialized guest bytes
   instead of pushing a duplicate pair. Ordinary unmaterialized escapes still
   synthesize their original return bytes. Runtime and validation stubs agree.

The engine remains game-agnostic. Invalid return ownership still traps; no
unresolved stubs, game logic, or failure thresholds were relaxed.

## Evidence

Local, ignored evidence directory: `out/cv1-weapon-freeze.3Kcyx3/`.
Candidate: `candidate-r3/sms.sms`, SHA-256
`50485bc3903625b4286cb3bb79c403cf234769b983f05155bc6311b276b604df`.
Baseline: `a1ee7106fb3211c69809e3363f0d3b3d40df1d2bbd156072f99cdda8eb6733cb`.
Both use the installed Genesis Plus GX `a7985a9` core, numeric overclock 500,
NTSC-U and frameskip disabled.

Controlled inventory fixtures change only `$015B` (weapon) and `$0141` (HUD
update) after 200 indoor updates. Subsequent spawning, motion and cleanup use
the translated original code. They are not natural-pickup equivalence tests.

| Check | Before | After |
|---|---|---|
| Axe, six-tick throws every 40 updates | E2 at physical frame 5326 | 2,400 indoor updates; world X=1135; no trap |
| Axe, every 12 updates until indoor update 600, then normal movement | E2 at frame 5226 | 2,400 indoor updates; world X=1112; no trap |
| Cross, every 120 updates | E1 at frame 5492; acceleration-only fix exposed E2 at frame 5632 | 2,400 indoor updates; world X=1331; no trap |
| Naturally acquired dagger, every 40 updates | Existing regression | 2,400 indoor updates; hearts exhausted; world X=1050; no trap |
| Frozen walk / heart 420-update routes | 26.188920 / 29.264596 updates/s | Identical rates and all compared landmark values |

Continuous twelve-/twenty-four-update axe spam without releasing input did
not trap after the capacity fix, but did not finish the traversal within its
frame bound. Those exploratory runs are **not** counted as route passes.

The frozen upper-exit input sequence reached area 2 without a trap, but its
strict reset oracle correctly remained inconclusive: sampled `255→1` could
also be natural wrap. The runner/profile were left unchanged. A separately
labeled input-only variant inserts 32 neutral updates after the first climb;
it retains every gate, observes the unambiguous `47→1` reset and completes
122 next-room updates with 56 pixels of movement and positive health.

Source-oracle tests cover the actual caller returns, conditional tail and
three cleanup selectors. Assembled tests cover successful allocation as well
as failure, and inject real IRQ handling at 3,423 allocation-failure and 1,623
projectile-cleanup instruction boundaries. Existing upper-exit/return tests
also pass. Workspace tests, formatting and Clippy pass (existing warnings remain).

SMB's fresh ROM differs from the accepted artifact only at the SDSC build-day
byte (`$7FE6`) and Sega checksum (`$7FFA`). All other bytes are identical.
Its 301-million-step 1-1-clear route passes no-trap, zero stale BG variants and
the post-transition 1-2/lives checks.

## Reproduction

Generate and Docker-assemble a candidate normally, then run:

```sh
CV1_WEAPON_NES=/path/to/input.nes cargo test -p nes_to_sms \
  --test cv1_returning_weapon -- --include-ignored
CV1_ESCAPE_PROJECT=out/cv1 cargo test -p nes_to_sms \
  --test consumed_return_escape weapon_ -- --include-ignored
CV1_ESCAPE_PROJECT=out/cv1 cargo test -p nes_to_sms \
  --test consumed_return_escape projectile_cleanup -- --ignored
```

The ignored `probe.py` and `exit_phase.py` preserve the controlled actual-core
experiments and write their own input/fixture manifests. Commercial ROMs and
captures are not committed. The known weapon paths are fixed; broader level
coverage and the user's exact interrupted session are not certified by this work.
