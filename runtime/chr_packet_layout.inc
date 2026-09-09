; Definition-only immutable CHR packet layout, shared by guarded adapters.
.ifndef CHR_PACKET_LAYOUT_DEFINED
.define CHR_PACKET_LAYOUT_DEFINED 1
.define M3G_DIRTY       $c830
.define M3G_BUSY        $c831
.define M3G_READY       $c833
.define M3G_TEMP_HI     $c834
.define M3G_TEMP_LO     $c835
.define M3G_FINE_X      $c836
.define M3G_SPLIT       $c837
.define M3G_PRESENT_X   $c838
.define M3G_PRESENT_Y   $c839
.define M3G_HUD_X       $c83a
.define M3G_HUD_Y       $c83b
.define M3G_PPU_BUS     $c83f
.define M3G_CHR_MAP     $c850 ; eight resolved physical 1 KiB pages
.define M3G_PALETTE     $c860 ; raw NES palette, including aliased entries
.define M3G_TILE        $c880 ; sixteen raw pattern bytes
.define M3G_RECORD     $9900 ; playfield record, HUD record at $9940
.define M3T_PF        $dd40
.define M3T_HUD       $dd50
.define M3G_PACKET_CHANGED $c8f4
.define M3G_BG_CHANGED $c8ec
.define M3G_COMPARE_BG $c8ed
.define M3C_DIRTY $dc00
.define M3C_OLD $dd00
.define M3C_VALID $dd33
.define M3C_OLD_SPLIT $dd34
.define M3C_FRAME $dd35
.define M3C_ROW $dd36
.define M3C_OLD_X $dd37
.define M3C_OLD_PAGE $dd38
.define M3C_COPY_CURSOR $dd39
.define M3C_CAPTURE_READY $dd3e
.define M3C_ANCHORED $dd3f ;frozen PF source matches completed committed NT
.endif
