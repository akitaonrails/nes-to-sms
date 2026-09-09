; Coherent, deliberately conservative full-frame MMC3 bring-up renderer.
; Pattern conversion targets cartridge SRAM; present_mmc3 owns guarded
; uploads/publication while this module builds an immutable next packet.
; No visible cache entry is overwritten during preparation. Pattern exhaustion
; traps instead of silently substituting another physical CHR tile.
;
; Bank0 SRAM: raw8000..87FF, frozen8800..97FF, OAM9800..98FF,
; records9900..997F, full32-row SMS NT A000..A7FF, BGkeysA800..AAFF,
; preparedSAT AB00..ABBF, hashheadsAC00..ADFF, chainsAE00..AFFF,
; mixed-raster secondary keys/palette/cut/row offsets B000..B5FF.
; The extra NT row is required for fine-Y scroll in 224-line mode's
; 256-pixel-high tilemap. BG uses256 slots at $0000..$1fff; fixed sprites
; use128 at $2000..$2fff. NT starts at $3700; the intervening VRAM is unused.
.ifdef MMC3_FULL_RUNTIME
.define CHR_PACKET_RAW_BASE MMC3_CHR_DATA_BASE
.include "runtime/chr_packet_render.inc"
.else
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.define CHR_PACKET_RAW_BASE CNROM_CHR_DATA_BASE
.bank CHR_PACKET_CODE_BANK slot 1
.include "runtime/chr_packet_render.inc"
.bank 0 slot 0
.endif
.endif
