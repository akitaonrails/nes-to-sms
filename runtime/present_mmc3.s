; Full-mode pending/committed display ownership. No CPU mapper state lives here.
; Cartridge bank1 A000..BFFF stages256 BG tiles; bank0 9980..9FFF/B600..BF7F
; stages128 fixed sprite tiles. Never map guest cartridge 8000..9FFF as scratch.
.ifdef MMC3_FULL_RUNTIME
.include "runtime/chr_packet_present.inc"
.else
.ifdef CNROM_SOURCE_HARDWARE_EXPERIMENT
.bank CHR_PACKET_CODE_BANK slot 1
.include "runtime/chr_packet_present.inc"
.bank 0 slot 0
.endif
.endif
