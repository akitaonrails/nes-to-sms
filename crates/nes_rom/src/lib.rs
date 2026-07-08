//! iNES / NES 2.0 ROM parsing.
//!
//! Lossless: keeps the original bytes around. Returns slices, not copies.

use std::fmt;

pub const INES_MAGIC: [u8; 4] = [b'N', b'E', b'S', 0x1a];
pub const PRG_BANK_SIZE: usize = 16 * 1024;
pub const CHR_BANK_SIZE: usize = 8 * 1024;
pub const TRAINER_SIZE: usize = 512;
pub const HEADER_SIZE: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirroring {
    Horizontal,
    Vertical,
    FourScreen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderKind {
    INes,
    Nes2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: HeaderKind,
    pub prg_banks: u16,
    pub chr_banks: u16,
    pub mapper: u16,
    pub submapper: u8,
    pub mirroring: Mirroring,
    pub has_trainer: bool,
    pub has_battery: bool,
}

impl Header {
    pub fn prg_len(&self) -> usize {
        self.prg_banks as usize * PRG_BANK_SIZE
    }
    pub fn chr_len(&self) -> usize {
        self.chr_banks as usize * CHR_BANK_SIZE
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    TooShort,
    BadMagic,
    Truncated { expected: usize, actual: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooShort => write!(f, "ROM is shorter than 16-byte header"),
            ParseError::BadMagic => write!(f, "missing NES<EOF> magic"),
            ParseError::Truncated { expected, actual } => write!(
                f,
                "ROM truncated: expected {expected} bytes after header, got {actual}"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse the 16-byte iNES / NES 2.0 header. Does not validate body length.
pub fn parse_header(rom: &[u8]) -> Result<Header, ParseError> {
    if rom.len() < HEADER_SIZE {
        return Err(ParseError::TooShort);
    }
    if rom[0..4] != INES_MAGIC {
        return Err(ParseError::BadMagic);
    }
    let flags6 = rom[6];
    let flags7 = rom[7];
    let kind = if (flags7 & 0x0c) == 0x08 {
        HeaderKind::Nes2
    } else {
        HeaderKind::INes
    };
    let (prg_banks, chr_banks, submapper) = match kind {
        HeaderKind::Nes2 => {
            let prg_lo = rom[4] as u16;
            let chr_lo = rom[5] as u16;
            let size_msb = rom[9] as u16;
            let prg_hi = size_msb & 0x0f;
            let chr_hi = (size_msb >> 4) & 0x0f;
            let prg = (prg_hi << 8) | prg_lo;
            let chr = (chr_hi << 8) | chr_lo;
            let submapper = (rom[8] >> 4) & 0x0f;
            (prg, chr, submapper)
        }
        HeaderKind::INes => (rom[4] as u16, rom[5] as u16, 0),
    };
    let mapper_lo = (flags6 >> 4) as u16;
    let mapper_hi = (flags7 & 0xf0) as u16;
    let mapper = mapper_hi | mapper_lo;
    let mirroring = if flags6 & 0x08 != 0 {
        Mirroring::FourScreen
    } else if flags6 & 0x01 != 0 {
        Mirroring::Vertical
    } else {
        Mirroring::Horizontal
    };
    Ok(Header {
        kind,
        prg_banks,
        chr_banks,
        mapper,
        submapper,
        mirroring,
        has_trainer: flags6 & 0x04 != 0,
        has_battery: flags6 & 0x02 != 0,
    })
}

/// A parsed iNES image with borrowed PRG/CHR slices.
#[derive(Debug)]
pub struct Image<'a> {
    pub header: Header,
    pub trainer: Option<&'a [u8]>,
    pub prg: &'a [u8],
    pub chr: &'a [u8],
}

pub fn parse<'a>(rom: &'a [u8]) -> Result<Image<'a>, ParseError> {
    let header = parse_header(rom)?;
    let trainer_len = if header.has_trainer { TRAINER_SIZE } else { 0 };
    let prg_start = HEADER_SIZE + trainer_len;
    let prg_end = prg_start + header.prg_len();
    let chr_end = prg_end + header.chr_len();
    if rom.len() < chr_end {
        return Err(ParseError::Truncated {
            expected: chr_end,
            actual: rom.len(),
        });
    }
    let trainer = if header.has_trainer {
        Some(&rom[HEADER_SIZE..HEADER_SIZE + TRAINER_SIZE])
    } else {
        None
    };
    let prg = &rom[prg_start..prg_end];
    let chr = &rom[prg_end..chr_end];
    Ok(Image {
        header,
        trainer,
        prg,
        chr,
    })
}

/// Read a 16-bit little-endian value from PRG at the given CPU address.
/// Assumes NROM-256 layout (PRG mapped at $8000..=$FFFF).
pub fn read_word_at_cpu_addr(prg: &[u8], cpu_addr: u16) -> Option<u16> {
    let off = cpu_to_prg_offset(prg.len(), cpu_addr)?;
    let lo = *prg.get(off)?;
    let hi = *prg.get(off + 1)?;
    Some(u16::from_le_bytes([lo, hi]))
}

/// CPU address → PRG byte offset, NROM only. Mirrors $C000..=$FFFF onto $8000..=$BFFF
/// when PRG is 16 KB (NROM-128).
/// CPU address → PRG byte offset for the FIXED region of the layout.
/// NROM: whole window (16 KB mirrored or flat 32 KB). Banked mappers
/// (PRG > 32 KB; UxROM/MMC1-typical/MMC3 fix the top): $C000-$FFFF maps
/// to the LAST 16 KB of PRG; the switchable window returns None
/// (callers need a bank: `banked_prg_offset`).
pub fn cpu_to_prg_offset(prg_len: usize, cpu_addr: u16) -> Option<usize> {
    if cpu_addr < 0x8000 {
        return None;
    }
    let off = (cpu_addr - 0x8000) as usize;
    if prg_len == 16 * 1024 {
        Some(off & 0x3fff)
    } else if prg_len == 32 * 1024 {
        Some(off)
    } else if prg_len > 32 * 1024 && cpu_addr >= 0xC000 {
        Some(prg_len - 0x4000 + (cpu_addr as usize - 0xC000))
    } else {
        None
    }
}

/// CPU address in the switchable window ($8000-$BFFF for 16 KB-banked
/// mappers) → PRG offset given the selected bank.
pub fn banked_prg_offset(prg_len: usize, bank: u8, cpu_addr: u16) -> Option<usize> {
    if !(0x8000..0xC000).contains(&cpu_addr) {
        return None;
    }
    let off = bank as usize * 0x4000 + (cpu_addr as usize - 0x8000);
    (off < prg_len).then_some(off)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vectors {
    pub nmi: u16,
    pub reset: u16,
    pub irq: u16,
}

pub fn read_vectors(prg: &[u8]) -> Option<Vectors> {
    Some(Vectors {
        nmi: read_word_at_cpu_addr(prg, 0xfffa)?,
        reset: read_word_at_cpu_addr(prg, 0xfffc)?,
        irq: read_word_at_cpu_addr(prg, 0xfffe)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_nrom256() -> Vec<u8> {
        let mut rom = vec![0u8; HEADER_SIZE + 32 * 1024 + 8 * 1024];
        rom[0..4].copy_from_slice(&INES_MAGIC);
        rom[4] = 2;
        rom[5] = 1;
        // mapper 0, vertical mirroring
        rom[6] = 0x01;
        // vectors at end of PRG: nmi $8082, reset $8000, irq $fff0
        let prg_end = HEADER_SIZE + 32 * 1024;
        rom[prg_end - 6..prg_end - 4].copy_from_slice(&0x8082u16.to_le_bytes());
        rom[prg_end - 4..prg_end - 2].copy_from_slice(&0x8000u16.to_le_bytes());
        rom[prg_end - 2..prg_end].copy_from_slice(&0xfff0u16.to_le_bytes());
        rom
    }

    #[test]
    fn parses_nrom256_header() {
        let rom = build_nrom256();
        let h = parse_header(&rom).unwrap();
        assert_eq!(h.prg_banks, 2);
        assert_eq!(h.chr_banks, 1);
        assert_eq!(h.mapper, 0);
        assert_eq!(h.mirroring, Mirroring::Vertical);
        assert!(!h.has_trainer);
        assert!(!h.has_battery);
    }

    #[test]
    fn parses_image_and_vectors() {
        let rom = build_nrom256();
        let img = parse(&rom).unwrap();
        assert_eq!(img.prg.len(), 32 * 1024);
        assert_eq!(img.chr.len(), 8 * 1024);
        let v = read_vectors(img.prg).unwrap();
        assert_eq!(v.nmi, 0x8082);
        assert_eq!(v.reset, 0x8000);
        assert_eq!(v.irq, 0xfff0);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut rom = vec![0u8; 64];
        assert_eq!(parse_header(&rom), Err(ParseError::BadMagic));
        rom[0..4].copy_from_slice(&INES_MAGIC);
        // header parses but body is too short for the declared sizes:
        rom[4] = 2;
        rom[5] = 1;
        assert!(matches!(parse(&rom), Err(ParseError::Truncated { .. })));
    }

    #[test]
    fn nrom128_mirrors_high_bank() {
        assert_eq!(cpu_to_prg_offset(16 * 1024, 0x8000), Some(0));
        assert_eq!(cpu_to_prg_offset(16 * 1024, 0xc000), Some(0));
        assert_eq!(cpu_to_prg_offset(16 * 1024, 0xffff), Some(0x3fff));
        assert_eq!(cpu_to_prg_offset(32 * 1024, 0xffff), Some(0x7fff));
        assert_eq!(cpu_to_prg_offset(32 * 1024, 0x7fff), None);
    }
}
