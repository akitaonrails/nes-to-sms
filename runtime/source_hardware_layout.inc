; Exclusive new hardware-mode aliases of inactive legacy continuation/SAT RAM.
.ifndef SOURCE_HARDWARE_LAYOUT
.define SOURCE_HARDWARE_LAYOUT 1
.define SP_BASE $d300
.define SP_STATUS $d300
.define SP_ODD $d301
.define SP_ORIGIN $d302
.define SP_PREFETCH_DIRTY $d304
.define SP_ACTIVE $d305
.define SP_FETCH_KIND $d306
.define SP_FETCH_ADDR $d307
.define SP_FETCH_BANK $d309
.define SP_FETCH_VALUE $d30a
.define SP_BG_LOW $d30b
.define SP_BG_HIGH $d30d
.define SP_ATTR_LOW $d30f
.define SP_ATTR_HIGH $d311
.define SP_TILE $d313
.define SP_ATTRIBUTE $d314
.define SP_PATTERN_LOW $d315
.define SP_PATTERN_HIGH $d316
.define SP_STARTUP $d317
.define SP_FETCH_DEST $d318 ; completion latch: 0 dummy,1 tile,2 attr,3 low,4 high
.define SP_PREFETCH_BANK $d319
.define SP_ADDR_PENDING $d31a
.define SP_ADDR_VALUE $d31b ; word, delayed v load from second $2006
.define SP_OAM_LATCH $d31d
.define SP_EVAL_N $d31e
.define SP_EVAL_M $d31f
.define SP_EVAL_COUNT $d320
.define SP_EVAL_DONE $d321
.define SP_EVAL_COPY $d322
.define SP_ZERO_NEXT $d323
.define SP_ZERO_ACTIVE $d324
.define SP_ZERO_X $d325
.define SP_ZERO_ATTR $d326
.define SP_ZERO_LOW $d327
.define SP_ZERO_HIGH $d328
.define SP_ZERO_NEXT_LOW $d329
.define SP_ZERO_NEXT_HIGH $d32a
.define SP_ZERO_NEXT_X $d32b
.define SP_ZERO_NEXT_ATTR $d32c
.define SP_FETCH_SLOT $d32d
.define SP_FETCH_ROW $d32e
.define SP_HIT_PENDING $d32f
.define SP_ODD_SKIP $d330
.define SP_VBL_SUPPRESS $d331
.define SP_VBL_EDGE_NEW $d332 ; only the edge raised by this frame's VBlank set
.define SP_INTERVAL_ERROR $d333 ; 1=invalid blank-domain interval, before mutation
.define SP_SECONDARY $d340 ; 32-byte secondary OAM
.define SP_SECONDARY_INDEX $d360 ; eight original OAM indices
.define SP_LIVE_END $d368
.define SP_PRED_VALID $d368
.define SP_PRED_LINE $d369
.define SP_OVERFLOW_DOT $d36b
.define SP_INTERVAL_KIND $d36d
.define SP_INTERVAL_START $d36e
.define SP_INTERVAL_END $d370
.define SP_INTERVAL_V $d372
.define SP_INTERVAL_SEED_V $d374
.define SP_INTERVAL_SAVED_DOT $d376
.define SP_INTERVAL_SUFFIX $d378
.define SP_INTERVAL_LIMIT $d37a
.define SP_INTERVAL_COUNT $d37c
.define SP_INTERVAL_SCRATCH $d37e
.define SHW_PENDING $d380 ; materialized PPU/APU time = SC_CYCLES - this word
.define SHW_REMAIN $d382 ; further cycles strictly before the domain deadline
.define SHW_START $d384 ; completed source cycle before the first deferred cycle
.define SHW_SPAN_COUNT $d388
.define SHW_START_DOT $d38a
.define SHW_START_LINE $d38c
.define SHW_FLUSH_ACTIVE $d38e ; diagnostics must not recursively materialize
.define SHW_KIND $d38f ; typed materialization span:1 inactive,2 BG,3 sprite fetch
.define SP_BACKUP $d390
.define SP_BACKUP_END (SP_BACKUP + SP_LIVE_END-SP_BASE)
.define SP_PRED_SAVED_DOT $d3f8
.define SP_INTERNAL_MODE $d3fa ; 0=source,1=lookahead,2=materialization (not events)
.define SP_PRED_RESULT $d3fb
.define SP_USED_END (SP_PRED_RESULT + 2)
.define SP_END $d400
.define SH_BASE $d500
.define SH_IRQ_LINE $d500
.define SH_END $d600
.if SP_USED_END > SP_END
  .fail "Source PPU overlaps packet envelope"
.endif
.if SP_BACKUP_END > SP_PRED_SAVED_DOT
  .fail "Source PPU lookahead backup overlaps saved beam state"
.endif
.if SP_INTERVAL_SCRATCH+2 > SHW_PENDING
  .fail "Source PPU interval metadata overlaps deferred coordinator"
.endif
.if SH_END > $d600
  .fail "Source I/O overlaps committed presentation shadow"
.endif
.endif
