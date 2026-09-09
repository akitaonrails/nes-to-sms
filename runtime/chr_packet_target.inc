; Physical display/audio target only. Never change source NES timing here.
.ifndef CHR_PACKET_TARGET_DEFINED
.define CHR_PACKET_TARGET_DEFINED 1
.ifdef CNROM_SOURCE_HARDWARE_PAL240_EXPERIMENT
.ifndef CNROM_SOURCE_HARDWARE_EXPERIMENT
  .fail "PAL240 requires source hardware"
.endif
.define CNP_DISPLAY_LINES 240
.define CNP_DISPLAY_ROWS 30
.define CNP_SAT_HIDDEN $ef
.define CNP_REG1_DISPLAY $e8
.define CNP_REG1_BLANK $a8
.define CNP_DISPLAY_PAL 1
.define CNP_PSG_CLOCK_NUM 53203424
.else
.define CNP_DISPLAY_LINES 224
.define CNP_DISPLAY_ROWS 28
.define CNP_SAT_HIDDEN $e0
.define CNP_REG1_DISPLAY $f0
.define CNP_REG1_BLANK $b0
.define CNP_DISPLAY_PAL 0
.define CNP_PSG_CLOCK_NUM 53693175
.endif
.define CNP_PSG_CLOCK_DEN 15
.endif
