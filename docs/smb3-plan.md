# Super Mario Bros. 3 / MMC3 effort

Started 2026-09-07 at `80e30f6`. SMB3 is the user-selected next compatibility
target; SMB1 and Castlevania remain the regression floor. This supplements
the architecture in [master-plan.md](master-plan.md).

## Status: pinned compatibility baseline, not mapper support

`profiles/smb3.toml` identifies the local USA Rev 1 target. Conversion still
fails before analysis/output creation with `unsupported mapper 4`.
There is **no translated SMB3 boot, screenshot, or performance result yet**.
An assembled fixture or recognizable static image will not count as gameplay.

The checked local file has a NES 2.0 header, mapper 4/submapper 0, 256 KiB
PRG ROM, 128 KiB CHR ROM, 8 KiB volatile PRG RAM, no trainer, no battery,
and no four-screen flag. Submapper 0 does not establish an exact IRQ silicon
revision; record that uncertainty instead of selecting behavior silently.

- Whole-file SHA-256: `1ddc4b429490e298c05fa70389034107c0cb50f671c7c1baaa3f228224afc2e9`.
- PRG+CHR SHA-256, used by the profile:
  `959fdd32c71735d6fb2bd16a646d39f4ee65623273dd035e6a968e991bd13ef8`.
- Independently read final PRG bytes: `86 f4 40 ff 95 f7`, giving NMI `$F486`,
  RESET `$FF40`, IRQ `$F795`. The pipeline currently rejects mapper 4 **before**
  vector validation; its error alone does not verify these values.

Reproduce the compatibility probe with a legally obtained matching ROM:

```sh
cargo run --release -p nes_to_sms --bin nes-to-sms -- \
  /path/to/smb3.nes profiles/smb3.toml out/smb3 --runtime runtime
```

Failure is currently expected, including with `--debug-unresolved-stubs`.
Do not change the ROM header to masquerade as UxROM.

## Why this is a new architectural step

| Boundary | Current implementation | SMB3 requirement |
| --- | --- | --- |
| PRG identity | One selected 16 KiB UxROM bank | Independent 8 KiB windows and PRG inversion |
| Game memory | NES 2 KiB mirror plus runtime/render storage | Additional 8 KiB cartridge work RAM |
| Graphics | NROM CHR or CV1 CHR RAM | 128 KiB physical CHR, mapped in 1/2 KiB windows |
| Raster timing | Existing SMS split machinery | Qualified MMC3 IRQ events plus a separate SMS presentation strategy |

The [US PRG1 disassembly](https://github.com/captainsouthbird/smb3) is useful
for labels and control flow, not a replacement for translating the ROM.
Its [memory definitions](https://raw.githubusercontent.com/captainsouthbird/smb3/master/smb3.asm)
place 6,480 bytes of level tile data in cartridge RAM, followed by other game
state. With PRG inversion, bank 30 occupies `$8000`, bank 31 `$E000`, and
`$A000`/`$C000` switch independently. Its
[interrupt code](https://raw.githubusercontent.com/captainsouthbird/smb3/master/PRG/prg031.asm)
temporarily maps audio into both switchable windows and restores them; raster
handlers change CHR and scrolling for HUD and other partitions. Verify any
imported address against the pinned payload; some hardware comments in the
disassembly are inaccurate.

[MMC3 IRQ behavior](https://www.nesdev.org/wiki/MMC3) depends on qualified
PPU A12 edges, not simply a frame's scanline number. Reload, acknowledge,
enable, and chip-revision behavior need explicit tests. Hardware-derived
[MMC3 test sources](https://github.com/christopherpow/nes-test-roms/tree/master/mmc3_test_2)
provide independent cases; they validate NES mapper emulation, not SMS output.

The [SMS VDP](https://www.smspower.org/uploads/Development/msvdp-20021112.txt)
has 16 KiB VRAM and cannot change vertical scroll during active display.
Thus a literal port of SMB3's raster handler is insufficient. CHR expansion
and palette variants need bounded residency and coherent frame presentation.

[Sega cartridge RAM](https://www.smspower.org/Development/Mappers) overlays
slot-2 ROM, so RAM helpers must execute outside that window and restore mapper
state safely. Cartridge RAM must not overlap the runtime's existing render
storage. The older plan's universal 512 KiB ROM ceiling is incorrect: capacity
depends on mapper implementation. Measure output expansion and declare a
specific tested cartridge/emulator contract before claiming feasibility.

## Ordered implementation and acceptance gates

1. **Compatibility baseline (this delivery).** Pin identity and vectors;
   test mapper-4 rejection without creating or modifying output. Publish this
   plan and preserve SMB1/CV1 artifacts.
2. **Executable 8 KiB banking foundation (next code milestone).** Define
   physical-bank/window identity and mapper state separately from the UxROM
   policy. Integrate it with a real consumer and synthetic executable fixture;
   do not land an unused board model. Cover both PRG modes, independent R6/R7,
   address aliases, masked bank selection, fixed vectors, data reads, and
   cross-window calls/returns. Then extend profile/discovery/dispatch while
   trapping unknown dynamic targets. Reuse `AnalysisWindow`; remove concrete
   16 KiB assumptions in orchestration rather than globally changing its size.
3. **Memory and CHR contract.** Prove non-overlapping cartridge work RAM,
   enable/protect behavior and interrupt-safe bank restoration. Track physical
   CHR identity, 2 KiB even alignment, inversion, bank-change invalidation,
   dynamic mirroring, cache residency and complete-frame commits. Measure
   emitted ROM size before committing to a cartridge capacity.
4. **Reference timing and SMS IRQ adaptation.** Obtain actual-core NES
   traces/captures; verify A12 qualification, deferred reload, zero-latch
   variants, acknowledge and interrupt masking. Separately implement SMS
   HUD/playfield composition. Neither a synthetic scanline clock nor one
   correct screenshot establishes raster fidelity.
5. **First visible conversion.** Reset → animated title → controller-driven
   world map, with no permissive stubs or injected game state. Gate moving
   frames, input response, bank restoration and stack bounds.
6. **First playable route.** Enter 1-1, move/jump, collect a power-up, break a
   block, finish the level and return to the map. Compare game/cart-RAM state
   and consecutive HUD/playfield frames with NES evidence. Then expand to
   vertical scrolling, inventory, transformations, roulette and other splits.

Measure logical game ticks/second at stock clock and explicit overclock,
Z80 cycles/tick, mapper overhead, CHR bytes uploaded/frame, peak tile residency,
and dropped/repeated presentations. SMB3 full speed is an experiment, not a
promise. Game-specific facts stay in profiles/runtime, not Rust hand-ports.

## Evidence and preservation

Local evidence is under ignored `out/smb3-baseline.bo9UiK/`. Original-NES
capture remains open: the existing FCEUX 2.5.0 Docker image aborted at startup
under Xvfb, including with audio disabled. Offscreen attempts did not yield
captures. No original-NES or SMS screenshot is claimed. The temporary Lua
input schedule and failure logs are retained; no emulator dependency was added.

Current `frame-diff` is **not** an MMC3 ground-truth oracle: it has no PPU
fetch/A12 stream, uses an instruction-based frame allowance, and does not
deliver mapper IRQs. Extending cartridge registers alone cannot fix that.

Before every mapper-phase commit: workspace tests, formatting/lint, all three
SMB differential routes and route expectations, isolated SMB/CV1 generation
and byte comparison, and Alter Ego assembly. Preserve moving-HUD evidence;
render changes require new consecutive real-core captures, not just static
checkpoints. Do not overwrite `out/smb` or `out/cv1` during experiments.

Preserved ROM SHA-256 values:

- SMB1: `b3e14180f904182882a40da5c1d7be4ff778e4f0518e4ddcf8de250ee23211b0`.
- CV1: `d4e3e0fe60b5b0df48531b2e48ec5134c3a551e284ecd03d3477ae36d06b4497`.

Phase 0 verification:

- Workspace: **530 passed, 114 ignored**. Formatting and Clippy completed;
  existing lint warnings remain visible in the retained log.
- All **13** assembled NROM/UxROM IRQ tests passed, including the moving-HUD
  split contracts.
- Isolated SMB1 and CV1 rebuilds match the hashes above byte-for-byte;
  canonical projects were not regenerated or modified.
- The 301-million-step SMB clear trace passes no-trap, terminal RAM and
  zero-stale-BG gates. All seven PPM checkpoints match the accepted HUD-fix
  baseline byte-for-byte.
- SMB clear (4,500 frames), pipe (3,000 frames) and death/game-over
  (3,300 frames) all report **NO DIVERGENCE** against the NES oracle.
- Alter Ego's existing generated project assembles in an isolated copy.
  Its original input was not located: this establishes assembly, not fresh
  pipeline-generation or gameplay parity for Alter Ego.
- Independent plan and implementation reviews accepted this as a compatibility
  baseline; the required regression gates subsequently passed. No production
  module boundaries or runtime code changed.
