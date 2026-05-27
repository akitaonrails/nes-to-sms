//! NES→SMS asset conversion utilities.
//!
//! Converts NES graphics assets (CHR tiles, palettes, nametables) into
//! SMS Mode 4-compatible formats and produces diagnostic PPM tile sheets.

// Canonical 2C02 NES master palette in 24-bit RGB (64 entries, index 0x00 first).
const NES_MASTER_PALETTE: [(u8, u8, u8); 64] = [
    (84, 84, 84),
    (0, 30, 116),
    (8, 16, 144),
    (48, 0, 136),
    (68, 0, 100),
    (92, 0, 48),
    (84, 4, 0),
    (60, 24, 0),
    (32, 42, 0),
    (8, 58, 0),
    (0, 64, 0),
    (0, 60, 0),
    (0, 50, 60),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (152, 150, 152),
    (8, 76, 196),
    (48, 50, 236),
    (92, 30, 228),
    (136, 20, 176),
    (160, 20, 100),
    (152, 34, 32),
    (120, 60, 0),
    (84, 90, 0),
    (40, 114, 0),
    (8, 124, 0),
    (0, 118, 40),
    (0, 102, 120),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (236, 238, 236),
    (76, 154, 236),
    (120, 124, 236),
    (176, 98, 236),
    (228, 84, 236),
    (236, 88, 180),
    (236, 106, 100),
    (212, 136, 32),
    (160, 170, 0),
    (116, 196, 0),
    (76, 208, 32),
    (56, 204, 108),
    (56, 180, 204),
    (60, 60, 60),
    (0, 0, 0),
    (0, 0, 0),
    (236, 238, 236),
    (168, 204, 236),
    (188, 188, 236),
    (212, 178, 236),
    (236, 174, 236),
    (236, 174, 212),
    (236, 180, 176),
    (228, 196, 144),
    (204, 210, 120),
    (180, 222, 120),
    (168, 226, 144),
    (152, 226, 180),
    (160, 214, 228),
    (160, 162, 160),
    (0, 0, 0),
    (0, 0, 0),
];

/// Convert NES 2bpp 8x8 tiles into SMS Mode 4 4bpp 8x8 tiles.
///
/// NES tile layout: 16 bytes per tile.
///   bytes 0..=7  are bitplane 0 (one byte per row, bit7 = leftmost pixel)
///   bytes 8..=15 are bitplane 1
///
/// SMS Mode 4 tile layout: 32 bytes per tile, interleaved by row.
///   For each row Y in 0..8: 4 bytes are emitted, [p0, p1, p2, p3] where
///   each plane byte has bit7 = leftmost pixel.
///
/// This conservative conversion copies NES bitplane 0 to SMS plane 0,
/// NES bitplane 1 to SMS plane 1, and clears SMS planes 2 and 3.
pub fn nes_chr_to_sms_4bpp(chr: &[u8]) -> Vec<u8> {
    let tile_count = chr.len() / 16;
    let mut out = vec![0u8; tile_count * 32];
    for t in 0..tile_count {
        let nes_base = t * 16;
        let sms_base = t * 32;
        for y in 0..8 {
            let p0 = chr[nes_base + y];
            let p1 = chr[nes_base + 8 + y];
            let row_base = sms_base + y * 4;
            out[row_base] = p0;
            out[row_base + 1] = p1;
            // planes 2 and 3 remain 0
        }
    }
    out
}

/// Compute the SMS Mode 4 4bpp tile count produced for a given CHR byte length.
pub fn sms_tile_count(chr_len: usize) -> usize {
    chr_len / 16
}

/// Grayscale palette for PPM rendering: pixel value → (R, G, B).
/// 0=white, 1=light gray, 2=dark gray, 3=black.
fn pixel_to_rgb(pixel: u8) -> (u8, u8, u8) {
    let v = match pixel & 0x3 {
        0 => 255u8,
        1 => 170,
        2 => 85,
        _ => 0,
    };
    (v, v, v)
}

/// Render a tile sheet PPM (P6 binary, RGB) showing all tiles in the input
/// CHR, 16 tiles per row, using a fixed 4-color grayscale palette.
/// Returns the complete PPM file bytes.
pub fn tile_sheet_ppm(chr: &[u8]) -> Vec<u8> {
    let tile_count = sms_tile_count(chr.len());
    let cols = 16usize;
    let rows = tile_count.div_ceil(cols);
    let width = cols * 8;
    let height = rows * 8;

    let header = format!("P6\n{width} {height}\n255\n");
    let pixel_bytes = width * height * 3;
    let mut out = Vec::with_capacity(header.len() + pixel_bytes);
    out.extend_from_slice(header.as_bytes());

    for row in 0..rows {
        for py in 0..8usize {
            for col in 0..cols {
                let tile_idx = row * cols + col;
                for px in 0..8usize {
                    let (r, g, b) = if tile_idx < tile_count {
                        let nes_base = tile_idx * 16;
                        let p0 = (chr[nes_base + py] >> (7 - px)) & 1;
                        let p1 = (chr[nes_base + 8 + py] >> (7 - px)) & 1;
                        pixel_to_rgb(p0 | (p1 << 1))
                    } else {
                        (0, 0, 0)
                    };
                    out.push(r);
                    out.push(g);
                    out.push(b);
                }
            }
        }
    }
    out
}

/// Approximate-map a NES 64-color master palette index to an SMS 6-bit
/// `%00BBGGRR` color byte. Uses a fixed lookup table; not perceptually
/// optimized — good enough for v1.
pub fn nes_palette_to_sms_color(nes_index: u8) -> u8 {
    let idx = (nes_index & 0x3F) as usize;
    let (r, g, b) = NES_MASTER_PALETTE[idx];
    let rr = r >> 6;
    let gg = g >> 6;
    let bb = b >> 6;
    rr | (gg << 2) | (bb << 4)
}

/// Build a full 32-byte SMS CRAM bank by mapping a 32-byte NES palette
/// array (each byte 0..=63) through nes_palette_to_sms_color. Background
/// palette gets the first 16 bytes, sprite palette gets the next 16.
pub fn nes_palettes_to_sms_cram(nes_palettes: &[u8; 32]) -> [u8; 32] {
    let mut cram = [0u8; 32];
    for i in 0..32 {
        cram[i] = nes_palette_to_sms_color(nes_palettes[i]);
    }
    cram
}

/// Convert a single NES 32x30 nametable (960 bytes) into an SMS Mode 4
/// name-table tile-number array. The top and bottom NES rows are dropped
/// to fit SMS Mode 4's 224-pixel visible region (28 rows remain).
/// Output is 32 * 28 * 2 = 1792 bytes; two bytes per entry:
/// [tile_index, 0x00] (attribute byte is 0 — background palette).
pub fn nes_nametable_to_sms(nes_nt: &[u8; 960]) -> Vec<u8> {
    // NES nametable: 32 columns × 30 rows, row-major.
    // Drop row 0 and row 29; keep rows 1..=28 (28 rows).
    const COLS: usize = 32;
    const KEEP_ROWS: usize = 28;
    let mut out = Vec::with_capacity(COLS * KEEP_ROWS * 2);
    for row in 1..=28usize {
        for col in 0..COLS {
            let tile_index = nes_nt[row * COLS + col];
            out.push(tile_index);
            out.push(0x00);
        }
    }
    out
}

/// Decode a single NES 8x8 tile into a 64-entry pixel array (0..=3) for
/// inspection or palette planning.
pub fn decode_nes_tile(tile: &[u8; 16]) -> [u8; 64] {
    let mut pixels = [0u8; 64];
    for y in 0..8usize {
        let p0 = tile[y];
        let p1 = tile[y + 8];
        for x in 0..8usize {
            let bit0 = (p0 >> (7 - x)) & 1;
            let bit1 = (p1 >> (7 - x)) & 1;
            pixels[y * 8 + x] = bit0 | (bit1 << 1);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- nes_chr_to_sms_4bpp ---

    #[test]
    fn empty_chr_produces_empty_output() {
        assert_eq!(nes_chr_to_sms_4bpp(&[]), Vec::<u8>::new());
    }

    #[test]
    fn all_zero_tile_becomes_32_zero_bytes() {
        let input = [0u8; 16];
        let out = nes_chr_to_sms_4bpp(&input);
        assert_eq!(out.len(), 32);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn solid_plane0_row0_produces_correct_first_four_bytes() {
        // p0 row0 = 0xFF, rest zeros
        let mut input = [0u8; 16];
        input[0] = 0xFF;
        let out = nes_chr_to_sms_4bpp(&input);
        assert_eq!(out.len(), 32);
        // row 0: [p0=0xFF, p1=0x00, 0x00, 0x00]
        assert_eq!(&out[0..4], &[0xFF, 0x00, 0x00, 0x00]);
        // remaining rows are zero
        assert!(out[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn bit7_plane0_row0_only() {
        let mut input = [0u8; 16];
        input[0] = 0x80;
        let out = nes_chr_to_sms_4bpp(&input);
        assert_eq!(&out[0..4], &[0x80, 0x00, 0x00, 0x00]);
        assert!(out[4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn two_tile_roundtrip_layout() {
        // Two distinct tiles; verify interleaving is correct per tile.
        let mut input = [0u8; 32];
        // tile 0: p0[row0]=0xAA, p1[row0]=0x55
        input[0] = 0xAA;
        input[8] = 0x55;
        // tile 1: p0[row3]=0xFF
        input[16 + 3] = 0xFF;

        let out = nes_chr_to_sms_4bpp(&input);
        assert_eq!(out.len(), 64);

        // tile 0, row 0
        assert_eq!(&out[0..4], &[0xAA, 0x55, 0x00, 0x00]);
        // tile 0, remaining rows zero
        assert!(out[4..32].iter().all(|&b| b == 0));

        // tile 1, row 3: offset = 32 + 3*4 = 44
        assert_eq!(&out[44..48], &[0xFF, 0x00, 0x00, 0x00]);
        // other rows of tile 1 are zero
        let t1 = &out[32..64];
        assert!(t1[..12].iter().all(|&b| b == 0)); // rows 0-2
        assert!(t1[16..].iter().all(|&b| b == 0)); // rows 4-7
    }

    // --- sms_tile_count ---

    #[test]
    fn sms_tile_count_correct() {
        assert_eq!(sms_tile_count(0), 0);
        assert_eq!(sms_tile_count(16), 1);
        assert_eq!(sms_tile_count(256), 16);
        assert_eq!(sms_tile_count(4096), 256);
    }

    // --- decode_nes_tile ---

    #[test]
    fn decode_nes_tile_all_zeros() {
        let tile = [0u8; 16];
        let pixels = decode_nes_tile(&tile);
        assert!(pixels.iter().all(|&p| p == 0));
    }

    #[test]
    fn decode_nes_tile_known_pattern() {
        let mut tile = [0u8; 16];
        // Row 0, plane 0 = 0b10000001 → pixels at x=0 and x=7 are set in bit0
        tile[0] = 0b10000001;
        // Row 0, plane 1 = 0b11000000 → pixels at x=0 and x=1 have bit1 set
        tile[8] = 0b11000000;
        let pixels = decode_nes_tile(&tile);
        // x=0: bit0=1, bit1=1 → pixel=3
        assert_eq!(pixels[0], 3);
        // x=1: bit0=0, bit1=1 → pixel=2
        assert_eq!(pixels[1], 2);
        // x=7: bit0=1, bit1=0 → pixel=1
        assert_eq!(pixels[7], 1);
        // other pixels in row 0 are 0
        for x in 2..7 {
            assert_eq!(pixels[x], 0);
        }
        // rows 1..7 all zero
        assert!(pixels[8..].iter().all(|&p| p == 0));
    }

    // --- nes_palette_to_sms_color ---

    #[test]
    fn palette_index_0f_black_maps_to_zero() {
        // NES 0x0F = (0,0,0) → SMS 0x00
        assert_eq!(nes_palette_to_sms_color(0x0F), 0x00);
    }

    #[test]
    fn palette_index_30_white_maps_to_0x3f() {
        // NES 0x30 = (236,238,236); each channel >> 6 = 3 → 0b00_11_11_11 = 0x3F
        assert_eq!(nes_palette_to_sms_color(0x30), 0x3F);
    }

    #[test]
    fn palette_index_21_blue() {
        // NES 0x21 = (8,76,196); R>>6=0, G>>6=1, B>>6=3 → 0b00_11_01_00 = 0x34
        let sms = nes_palette_to_sms_color(0x21);
        let (r, g, b) = NES_MASTER_PALETTE[0x21];
        let expected = (r >> 6) | ((g >> 6) << 2) | ((b >> 6) << 4);
        assert_eq!(sms, expected);
    }

    #[test]
    fn palette_index_16_red() {
        // NES 0x16 = (152,34,32); R>>6=2, G>>6=0, B>>6=0 → 0b00_00_00_10 = 0x02
        let sms = nes_palette_to_sms_color(0x16);
        let (r, g, b) = NES_MASTER_PALETTE[0x16];
        let expected = (r >> 6) | ((g >> 6) << 2) | ((b >> 6) << 4);
        assert_eq!(sms, expected);
    }

    // --- nes_palettes_to_sms_cram ---

    #[test]
    fn cram_is_32_bytes_and_maps_correctly() {
        let mut nes_pals = [0u8; 32];
        nes_pals[0] = 0x0F; // black
        nes_pals[31] = 0x30; // white
        let cram = nes_palettes_to_sms_cram(&nes_pals);
        assert_eq!(cram.len(), 32);
        assert_eq!(cram[0], 0x00);
        assert_eq!(cram[31], 0x3F);
        // entries 1..31 come from NES index 0 = (84,84,84); each >>6 = 1
        // → 0b00_01_01_01 = 0x15
        for i in 1..31 {
            assert_eq!(cram[i], nes_palette_to_sms_color(0));
        }
    }

    // --- tile_sheet_ppm ---

    #[test]
    fn ppm_starts_with_magic_bytes() {
        let chr = [0u8; 16]; // 1 tile
        let ppm = tile_sheet_ppm(&chr);
        assert_eq!(&ppm[..2], b"P6");
    }

    #[test]
    fn ppm_dimensions_match_tile_count() {
        // 4 tiles: 16 per row → 1 row of tiles → height=8, width=128
        let chr = [0u8; 64]; // 4 tiles
        let ppm = tile_sheet_ppm(&chr);

        // Find the end of the PPM header (three newlines: after "P6", after "W H", after "255").
        let mut newlines = 0usize;
        let mut header_len = 0usize;
        for (i, &b) in ppm.iter().enumerate() {
            if b == b'\n' {
                newlines += 1;
                if newlines == 3 {
                    header_len = i + 1;
                    break;
                }
            }
        }
        let header = std::str::from_utf8(&ppm[..header_len]).unwrap();
        // header is "P6\nW H\n255\n"
        let lines: Vec<&str> = header.lines().collect();
        assert_eq!(lines[0], "P6");
        let parts: Vec<&str> = lines[1].split_whitespace().collect();
        let w: usize = parts[0].parse().unwrap();
        let h: usize = parts[1].parse().unwrap();
        assert_eq!(w, 128);
        assert_eq!(h, 8);
        let pixel_bytes = w * h * 3;
        assert_eq!(ppm.len(), header_len + pixel_bytes);
    }

    #[test]
    fn ppm_empty_chr_produces_zero_pixel_header() {
        // 0 tiles → 0 rows → height = 0
        let ppm = tile_sheet_ppm(&[]);
        let header = std::str::from_utf8(&ppm).unwrap();
        assert!(header.starts_with("P6\n0 0\n255\n") || header.starts_with("P6\n128 0\n255\n"));
    }

    // --- nes_nametable_to_sms ---

    #[test]
    fn nametable_all_zero_produces_1792_zero_bytes() {
        let nt = [0u8; 960];
        let out = nes_nametable_to_sms(&nt);
        assert_eq!(out.len(), 1792);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn nametable_all_0x42_produces_alternating_pattern() {
        let nt = [0x42u8; 960];
        let out = nes_nametable_to_sms(&nt);
        assert_eq!(out.len(), 1792);
        for i in 0..out.len() {
            if i % 2 == 0 {
                assert_eq!(out[i], 0x42, "tile byte at offset {i}");
            } else {
                assert_eq!(out[i], 0x00, "attr byte at offset {i}");
            }
        }
    }

    #[test]
    fn nametable_top_and_bottom_rows_dropped() {
        let mut nt = [0u8; 960];
        // Set top row (row 0) to 0xFF — should not appear in output
        for col in 0..32 {
            nt[col] = 0xFF;
        }
        // Set bottom row (row 29) to 0xFE — should not appear in output
        for col in 0..32 {
            nt[29 * 32 + col] = 0xFE;
        }
        // Set row 1 to 0x01 — should appear first in output
        for col in 0..32 {
            nt[32 + col] = 0x01;
        }
        let out = nes_nametable_to_sms(&nt);
        assert_eq!(out.len(), 1792);
        // First 64 bytes are row 1 entries: [0x01, 0x00] × 32
        for i in 0..32 {
            assert_eq!(out[i * 2], 0x01);
            assert_eq!(out[i * 2 + 1], 0x00);
        }
        // No 0xFF or 0xFE in output
        assert!(!out.iter().any(|&b| b == 0xFF));
        assert!(!out.iter().any(|&b| b == 0xFE));
    }
}
