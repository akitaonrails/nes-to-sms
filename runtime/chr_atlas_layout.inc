; New cold-only canonical physical atlas. All AT storage below is SRAM1.
.ifndef CHR_ATLAS_LAYOUT_DEFINED
.define CHR_ATLAS_LAYOUT_DEFINED 1
.define AT_CAPACITY 378
.define AT_PAYLOAD $8800
.define AT_FLAGS $b740
.define AT_HANDLES $b8ba
.define AT_HEADS $baba
.define AT_LINKS $bcba
.define AT_ACTIVE_NT $bfae
.define AT_PHASE $bfaf
.define AT_FAULT $bfb0
.define AT_KIND $bfb1
.define AT_LOGICAL_COUNT $bfb2
.define AT_ALLOCATED_COUNT $bfb4
.define AT_SCAN $bfb6
.define AT_CANDIDATE $bfb8
.define AT_PREDECESSOR $bfba
.define AT_SOURCE $bfbc
.define AT_DESTINATION $bfbe
.define AT_REMAIN $bfc0
.define AT_UPLOAD_ORDINAL $bfc2
.define AT_NT_CURSOR $bfc4
.define AT_RESULT $bfc6
.define AT_HASH $bfc8
.define AT_HALF $bfc9
.define AT_COMPARE_OFFSET $bfca
.define AT_FINAL_BUDGET $bfcc
.define AT_PUBLISH_PROGRESS $bfce
.define AT_BRIDGE_KIND $bfcf
.define AT_WORK0 $bfd0
.define AT_WORK1 $bfd2
.define AT_WORK2 $bfd4
.define AT_WORK3 $bfd6
.define AT_RESERVED $bfd8
.define AT_END $c000
.define AT_BG_CANDIDATE $abc0
.define AT_BG_CANDIDATE_END $abe0
.define AT_ALLOCATED $01
.define AT_OLD $02
.define AT_PENDING $04
.define AT_DIRTY $08
.define AT_PAIR_FIRST $10
.define AT_PAIR_SECOND $20
.define AT_PERMANENT_ZERO $40
.define AT_PROTECTED (AT_OLD | AT_PENDING | AT_PERMANENT_ZERO)
.if AT_PAYLOAD != $8800 || AT_END != $c000
  .fail "Atlas must preserve optional guest SRAM8000..87FF"
.endif
.if AT_PAYLOAD+AT_CAPACITY*32 != AT_FLAGS
  .fail "Atlas payload/flags overlap"
.endif
.if AT_FLAGS+AT_CAPACITY != AT_HANDLES
  .fail "Atlas flags/handles overlap"
.endif
.if AT_HANDLES+512 != AT_HEADS || AT_HEADS+512 != AT_LINKS
  .fail "Atlas handle/hash arrays overlap"
.endif
.if AT_LINKS+AT_CAPACITY*2 != AT_ACTIVE_NT || AT_ACTIVE_NT+82 != AT_END
  .fail "Atlas links/state budget mismatch"
.endif
.if AT_RESERVED+40 != AT_END
  .fail "Atlas reserved tail mismatch"
.endif
.if AT_BG_CANDIDATE != $abc0 || AT_BG_CANDIDATE_END-AT_BG_CANDIDATE != 32
  .fail "Atlas candidate overlaps prepared SAT or reserved SRAM"
.endif
.ifdef CNROM_SOURCE_HARDWARE_ATLAS_EXPERIMENT
.if CHR_ATLAS_CODE_BANK != 2
  .fail "Atlas code requires independently reserved bank2"
.endif
.endif
.endif
