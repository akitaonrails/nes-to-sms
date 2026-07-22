# Castlevania 1 Mapper-2 Completion Record

This document records the completed mapper-2 recovery and the reproducible
acceptance floor. It supersedes the old CV1 blocker descriptions in
`HANDOFF-cv1.md` and historical deep-work notes where they conflict.

## Scope and canonical input

The target is Castlevania (USA) (Rev 1), NES 2.0 mapper 2/submapper 2, with
eight 16 KiB PRG banks, CHR-RAM, vertical mirroring, and AND bus conflicts.
Use the gitignored `.roms/cv1.nes`; `profiles/cv1.toml` binds its PRG/CHR
payload SHA-256. Do not derive profile facts from a divergent SMS trace.

The present acceptance bar follows the project owner's SMB precedent: boot,
Start, recognizable first-stage rendering, responsive gameplay input, and a
trap-free soak. It is not a pixel-perfect, real-time, or full-game claim.

## Completed course correction

The earlier work stalled because mapper banks were treated as interchangeable
32 KiB views and many inline `$CA6D` dispatch tables were decoded as code.
The completed implementation now provides:

- a generic eight-bank UxROM policy with bus-conflict-effective writes;
- independent fixed-window and physical `(bank, PC)` analysis;
- bank-qualified calls, live-bank tail dispatch, and mapper-aware raw PRG reads;
- profile-driven inline JumpEngine tables, including stack-aware returns;
- profile-driven `[[return_escape]]` edges for 6502 callees that discard a
  JSR return address as stack data before returning through an older caller;
- executable targets that deliberately overlap a pointer-table suffix;
- fixed-point roots for branch continuations stranded by embedded routines;
- a compact 512 KiB SMS layout with 17 translated-code banks and all eight raw
  PRG banks preserved;
- fail-closed unsupported mapper stores and unresolved dispatches.
- generic STX/STY OAM-DMA lowering and OAMADDR-aware DMA wrapping;
- shared NES `$2005`/`$2006` latch semantics;
- CHR-RAM nametable rematerialization across render-off screen builds;
- scene-scoped CHR-RAM background variants: complete screen rebuilds reclaim
  stale slots before applying the new screen's attribute palettes;
- CHR-RAM 8x16 sprite-pair generation with per-sprite table selection,
  palette selection, and horizontal/vertical flips;
- accumulator-safe mapped background writes: the mapped-tile helper formerly
  stored a 16-bit VDP address at `$CB17`, whose high byte overwrote
  `rt_ppu_write`'s saved accumulator at `$CB18`. Castlevania's RLE decoder
  therefore wrote one correct zero followed by mapped slot numbers (`$37`,
  `$38`, `$3A`...) into CIRAM. The helper now parks the address bytes
  separately and preserves 6502 `A` across `STA $2007` continuations.

The executable synthetic UxROM fixture selects and dispatches all eight banks.
Its bus-conflict and unsupported-store tests are part of
`crates/cli/tests/synthetic_pipeline.rs`.

## Current canonical result

Generation reports 416 discovered fixed-view functions, 898 lifted routines,
zero lift failures, one lower failure, and 80 strict unresolved traps. The one
lower failure is bank 5 `$BD8A`: an unreachable JumpEngine sentinel pointing
into an all-`$FF` filler region. The canonical NES reference does not execute it
across the 3,600-frame Start/move/jump/whip route, so it correctly remains a
fail-closed trap rather than being translated as bogus code.

The old 115,000,000-step smoke stopped during Stage 1 setup at system
state/substate `$05/$01`; it proved only boot and no-trap behavior. The current
180,000,000-step route reaches gameplay `$05/$06`, holds Right after frame 1850,
and records CV1's held-input byte `$00F7=01` without a trap. Mednafen cold-boots
to the title and accepts Pause/Start. Genesis Plus GX at 500% Z80 overclock now
shows a clean sword/logo title and coherent entrance scene instead of the
former repeated-pattern tile field.
The trace framebuffer reaches a coherent, recognizable Stage 1 playfield with
8x16 sprites. The NES oracle reports 234 live `(tile, subpalette)` pairs on the
dense title and 54 in Stage 1, both within the 256-slot SMS background table.
Reclaiming the allocator on complete screen rebuilds therefore preserves the
NES attribute palettes without recycling visible patterns. The Stage 1 trace
now matches the reference's blue sky, green canopy, dark lower backdrop, and
gray fence/ground layout. The canonical ROM SHA-256 after the accumulator,
performance, and candle-route stack fixes is
`d7c6ed256619b9f68b23c95324329a9d46418b0ecfd6e1cf3dfee04297b74c11`.
Stock-timing emulation remains slow.

## Performance hardening and candle-route repair

The July 21 profile found linear mapper dispatch, not rendering, as the largest
avoidable CPU cost: `_bd_loop` accounted for 24.11% of 180 million sampled
instructions while scanning as many as 897 records. Generated projects now
sort dispatch records by NES address, preserve fixed-bank precedence at
duplicate addresses, and emit a 128-entry high-byte page directory. The same
route attributes about 0.80% to `_bd_loop`; its Stage 1 completion upload moves
from synthetic IRQ frame 1904 to 1370. CHR-RAM 8x16 bit reversal is now a
constant-time permutation, and depth-0 fixed-PRG reads use a checked canonical
mapper-window fast path while nested mappings retain the exact snapshot path.

A repeated Right/whip route isolated the reported broken-candle hang in two
steps. The first failure was a valid fixed-bank branch from `$DE32` to `$DE04`;
the CV1 profile roots that statically proven shared tail, removing strict stub
`$0040`. The remaining `$0000` RTI failure came from `$E7D0 -> $EC60`:
`$EC60` deliberately executes `PLA; PLA` to discard the return bytes of
`$EA77: JSR $E4D9`, then returns through the caller below it. Translated calls
store that frame in `TR_RET`, not on `$0100+S`, so the PLAs consumed a live NMI
frame and raised S by two.

The generic `[[return_escape]]` annotation now identifies that tail edge. Its
runtime bridge discards one translated continuation, recreates the original
6502 JSR bytes (`$EA79`, high then low) on the emulated stack, and fail-closes
with marker `$E5` if no valid frame exists. The separate `$E959` JumpEngine
route into `$EC60` remains unchanged because it already arranges real emulated
stack bytes. The canonical route crosses the former failure at step
105,468,688, reaches it again at step 127,976,128, and completes a
180,000,000-step soak with no trap. S is balanced at `$F8` after the event, and
the near-event framebuffer remains in Stage 1 rather than blinking white and
stalling. The focused route also collects the large heart: the heart counter at
`$0071` finishes at `$0A` (the initial `$05` plus five) while system state
`$0018=$05`, substate `$0019=$06` confirms gameplay continues.

## Reproduce CV1 acceptance

```sh
cargo run --release -p nes_to_sms --bin nes-to-sms -- \
  .roms/cv1.nes profiles/cv1.toml out/cv1 --runtime runtime
docker compose run --rm --user "$(id -u):$(id -g)" --workdir /work poc \
  bash -lc 'make -C out/cv1'
SMS_DUMP_PPM=out/cv1/graphics-acceptance/stage.ppm \
  target/release/trace-sms out/cv1/sms.sms --steps 180000000 \
  --pause-at-frame 600 --buttons-at-frame 1850:right \
  --expect-no-trap --expect-ram 0x0018=05 --expect-ram 0x0019=06 \
  --expect-ram 0x00F7=01

# Candle/heart regression: crosses the formerly fatal return escape, collects
# the large heart, remains in gameplay, and soaks after the event.
target/release/trace-sms out/cv1/sms.sms --steps 180000000 \
  --pause-at-frame 600 \
  --buttons-script profiles/cv1/acceptance/heart-smoke.sms.buttons \
  --expect-no-trap --expect-ram 0x0071=0A \
  --expect-ram 0x0018=05 --expect-ram 0x0019=06
```

For canonical-reference checks, use
`profiles/cv1/acceptance/stage1-smoke.buttons` with `FD_PAUSE_START=1`.
For GPGX, `docker/run_gpgx.sh` reports whether the configured 8BitDo event
device is actually present; set `GPGX_REQUIRE_GAMEPAD=1` to fail instead of
falling back to the keyboard.

## Regression floor

SMB must remain green before mapper work is accepted. The current build has
657/657 lifted functions, no lift/lower failures, no unresolved labels, and
passes the documented 301-million-step 1-1 clear route with `$075C=01`,
`$0760=02`, `$075A=02`, and no trap. Run `cargo test --workspace`, format, and
Clippy before handoff.

## Deferred quality work

These are follow-on improvements, not mapper-2 blockers:

- improve palette quantization and localized scene/window seams;
- expand return-escape annotations only when a canonical route proves another
  stack-unwinding edge; stale target metadata must continue to fail closed;
- continue stock-timing work on fixed-PRG reads and CHR-RAM sprite builds;
- author a human route through the Stage 1 boss and Stage 2 entry;
- resolve additional strict stubs only when canonical execution reaches them;
- extend real-emulator soak coverage and audio quality.

Never raise routine limits, translate filler/data walks, add Rust-side CV1
gameplay, or harvest roots from SMS traps to make these items appear green.
