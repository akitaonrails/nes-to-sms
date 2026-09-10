//! iNES / NES 2.0 ROM parsing.
//!
//! Lossless: keeps the original bytes around. Returns slices, not copies.

use std::fmt;

use sha2::{Digest, Sha256};

pub mod axrom;
pub mod cnrom;
pub mod fme7;
pub mod mmc1;
pub mod mmc2;
pub mod mmc3;
pub mod mmc5;
pub mod vrc2;

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
    /// Volatile PRG-RAM capacity in bytes.
    pub prg_ram_size: usize,
    /// Nonvolatile PRG-RAM capacity in bytes.
    pub prg_nvram_size: usize,
    /// Volatile CHR-RAM capacity in bytes.
    pub chr_ram_size: usize,
    /// Nonvolatile CHR-RAM capacity in bytes.
    pub chr_nvram_size: usize,
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

fn nes2_ram_size(nibble: u8) -> usize {
    if nibble == 0 { 0 } else { 64usize << nibble }
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
    let (
        prg_banks,
        chr_banks,
        submapper,
        prg_ram_size,
        prg_nvram_size,
        chr_ram_size,
        chr_nvram_size,
    ) = match kind {
        HeaderKind::Nes2 => {
            let prg_lo = rom[4] as u16;
            let chr_lo = rom[5] as u16;
            let size_msb = rom[9] as u16;
            let prg_hi = size_msb & 0x0f;
            let chr_hi = (size_msb >> 4) & 0x0f;
            let prg = (prg_hi << 8) | prg_lo;
            let chr = (chr_hi << 8) | chr_lo;
            let submapper = (rom[8] >> 4) & 0x0f;
            let prg_ram = nes2_ram_size(rom[10] & 0x0f);
            let prg_nvram = nes2_ram_size(rom[10] >> 4);
            let chr_ram = nes2_ram_size(rom[11] & 0x0f);
            let chr_nvram = nes2_ram_size(rom[11] >> 4);
            (prg, chr, submapper, prg_ram, prg_nvram, chr_ram, chr_nvram)
        }
        HeaderKind::INes => {
            let prg_ram = if rom[8] == 0 {
                8 * 1024
            } else {
                rom[8] as usize * 8 * 1024
            };
            let chr_ram = if rom[5] == 0 { 8 * 1024 } else { 0 };
            (rom[4] as u16, rom[5] as u16, 0, prg_ram, 0, chr_ram, 0)
        }
    };
    let mapper_lo = (flags6 >> 4) as u16;
    let mapper_hi = (flags7 & 0xf0) as u16;
    let mapper_ext = if kind == HeaderKind::Nes2 {
        u16::from(rom[8] & 0x0f) << 8
    } else {
        0
    };
    let mapper = mapper_ext | mapper_hi | mapper_lo;
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
        prg_ram_size,
        prg_nvram_size,
        chr_ram_size,
        chr_nvram_size,
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

/// Return the lowercase SHA-256 identity of the canonical ROM payload:
/// PRG bytes followed immediately by CHR-ROM bytes. Header and trainer data
/// are intentionally excluded.
pub fn payload_sha256_hex(prg: &[u8], chr: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prg);
    hasher.update(chr);
    format!("{:x}", hasher.finalize())
}

/// Supported CPU-to-PRG mapping policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperPolicy {
    /// Mapper 0 with either one mirrored or two directly mapped 16 KiB banks.
    Nrom { prg_len: usize },
    /// Mapper 2 with a switchable lower bank and a fixed final upper bank.
    Uxrom {
        bank_count: u8,
        bus_conflicts: UxromBusConflicts,
    },
    /// Mapper 3 (CNROM): fixed PRG, CHR-bank select on $8000-$FFFF writes.
    Cnrom {
        prg_len: usize,
        chr_bank_count: u8,
        bus_conflicts: UxromBusConflicts,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UxromBusConflicts {
    None,
    And,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapperPolicyError {
    UnsupportedMapper { mapper: u16 },
    UxromRequiresNes2,
    UnsupportedUxromSubmapper { submapper: u8 },
    InvalidNromPrgLayout { prg_len: usize },
    InvalidUxromPrgLayout { prg_len: usize },
    SelectedBankOutOfRange { bank: u8, bank_count: u8 },
}

impl fmt::Display for MapperPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper: 4 } => write!(
                f,
                "unsupported mapper 4 (MMC3); translation requires independently mapped 8-KiB PRG windows, banked CHR, cartridge-RAM mapping, and PPU-A12-qualified IRQ support; only mapper 0 (NROM) and mapper 2 (UxROM) are supported"
            ),
            Self::UnsupportedMapper { mapper } => {
                write!(
                    f,
                    "unsupported mapper {mapper}; only mapper 0 (NROM) and mapper 2 (UxROM) are supported"
                )
            }
            Self::UxromRequiresNes2 => {
                write!(f, "mapper 2 requires a NES 2.0 submapper declaration")
            }
            Self::UnsupportedUxromSubmapper { submapper } => write!(
                f,
                "unsupported mapper 2 NES 2.0 submapper {submapper}; only 1 (no conflict) and 2 (AND conflict) are supported"
            ),
            Self::InvalidNromPrgLayout { prg_len } => write!(
                f,
                "invalid NROM PRG layout: expected exactly 16384 or 32768 bytes, got {prg_len}"
            ),
            Self::InvalidUxromPrgLayout { prg_len } => write!(
                f,
                "invalid UxROM PRG layout: expected 2 through 16 16-KiB banks, got {prg_len} bytes"
            ),
            Self::SelectedBankOutOfRange { bank, bank_count } => write!(
                f,
                "UxROM selected bank {bank} is out of range for {bank_count} banks"
            ),
        }
    }
}

impl std::error::Error for MapperPolicyError {}

/// Resolve the supported mapper policy for a parsed ROM's PRG payload.
pub fn resolve_mapper_policy(
    header: &Header,
    prg_len: usize,
) -> Result<MapperPolicy, MapperPolicyError> {
    match header.mapper {
        0 if prg_len == PRG_BANK_SIZE || prg_len == 2 * PRG_BANK_SIZE => {
            Ok(MapperPolicy::Nrom { prg_len })
        }
        0 => Err(MapperPolicyError::InvalidNromPrgLayout { prg_len }),
        2 if header.kind != HeaderKind::Nes2 => Err(MapperPolicyError::UxromRequiresNes2),
        2 if prg_len % PRG_BANK_SIZE == 0 => {
            let bank_count = prg_len / PRG_BANK_SIZE;
            let bus_conflicts = match header.submapper {
                1 => UxromBusConflicts::None,
                2 => UxromBusConflicts::And,
                submapper => {
                    return Err(MapperPolicyError::UnsupportedUxromSubmapper { submapper });
                }
            };
            if matches!(bank_count, 2 | 4 | 8 | 16) {
                Ok(MapperPolicy::Uxrom {
                    bank_count: bank_count as u8,
                    bus_conflicts,
                })
            } else {
                Err(MapperPolicyError::InvalidUxromPrgLayout { prg_len })
            }
        }
        2 => Err(MapperPolicyError::InvalidUxromPrgLayout { prg_len }),
        mapper => Err(MapperPolicyError::UnsupportedMapper { mapper }),
    }
}

/// Oracle/analysis resolver that additionally admits CNROM (mapper 3). Kept
/// SEPARATE from `resolve_mapper_policy` so the SMS conversion pipeline's
/// default path still rejects CNROM and forces explicit board opt-in; only
/// the reference oracle (frame-diff) uses this wider set.
pub fn resolve_mapper_policy_oracle(
    header: &Header,
    prg_len: usize,
) -> Result<MapperPolicy, MapperPolicyError> {
    if header.mapper == 3 {
        if header.kind != HeaderKind::Nes2 {
            return Err(MapperPolicyError::UxromRequiresNes2);
        }
        if prg_len != PRG_BANK_SIZE && prg_len != 2 * PRG_BANK_SIZE {
            return Err(MapperPolicyError::InvalidNromPrgLayout { prg_len });
        }
        let bus_conflicts = match header.submapper {
            // CNROM submapper 2 requires AND bus conflicts, like UxROM.
            0 | 1 => UxromBusConflicts::None,
            2 => UxromBusConflicts::And,
            submapper => return Err(MapperPolicyError::UnsupportedUxromSubmapper { submapper }),
        };
        let chr_bank_count = (header.chr_len() / CHR_BANK_SIZE).max(1) as u8;
        return Ok(MapperPolicy::Cnrom {
            prg_len,
            chr_bank_count,
            bus_conflicts,
        });
    }
    if header.mapper == 2 && header.kind != HeaderKind::Nes2 {
        // iNES UxROM has no submapper; default to AND bus conflicts (the
        // conservative common case) so the oracle can boot old-header UxROM
        // dumps like Metal Gear. The strict SMS pipeline still requires NES 2.0.
        if prg_len % PRG_BANK_SIZE == 0 {
            let bank_count = prg_len / PRG_BANK_SIZE;
            if matches!(bank_count, 2 | 4 | 8 | 16) {
                return Ok(MapperPolicy::Uxrom {
                    bank_count: bank_count as u8,
                    bus_conflicts: UxromBusConflicts::And,
                });
            }
        }
        return Err(MapperPolicyError::InvalidUxromPrgLayout { prg_len });
    }
    resolve_mapper_policy(header, prg_len)
}

impl MapperPolicy {
    pub fn is_banked(self) -> bool {
        matches!(self, Self::Uxrom { .. })
    }

    pub fn bank_count(self) -> u8 {
        match self {
            Self::Nrom { prg_len } | Self::Cnrom { prg_len, .. } => (prg_len / PRG_BANK_SIZE) as u8,
            Self::Uxrom { bank_count, .. } => bank_count,
        }
    }

    fn checked_bank_offset(self, bank: u8) -> Result<usize, MapperPolicyError> {
        match self {
            Self::Nrom { .. } | Self::Cnrom { .. } => Ok(0),
            Self::Uxrom { bank_count, .. } if bank < bank_count => {
                Ok(bank as usize * PRG_BANK_SIZE)
            }
            Self::Uxrom { bank_count, .. } => {
                Err(MapperPolicyError::SelectedBankOutOfRange { bank, bank_count })
            }
        }
    }

    /// Map a CPU address using the UxROM lower-window selection when needed.
    pub fn cpu_to_prg_offset(
        self,
        cpu_addr: u16,
        selected_bank: u8,
    ) -> Result<Option<usize>, MapperPolicyError> {
        if cpu_addr < 0x8000 {
            return Ok(None);
        }
        match self {
            Self::Nrom { prg_len } | Self::Cnrom { prg_len, .. } => {
                let offset = (cpu_addr - 0x8000) as usize;
                Ok(Some(if prg_len == PRG_BANK_SIZE {
                    offset & (PRG_BANK_SIZE - 1)
                } else {
                    offset
                }))
            }
            Self::Uxrom { bank_count, .. } => {
                let bank_offset = self.checked_bank_offset(selected_bank)?;
                if cpu_addr < 0xC000 {
                    Ok(Some(bank_offset + (cpu_addr as usize - 0x8000)))
                } else {
                    Ok(Some(
                        (bank_count as usize - 1) * PRG_BANK_SIZE + (cpu_addr as usize - 0xC000),
                    ))
                }
            }
        }
    }

    /// Return the requested UxROM switchable bank.
    pub fn prg_bank<'a>(self, prg: &'a [u8], bank: u8) -> Result<&'a [u8], MapperPolicyError> {
        let offset = self.checked_bank_offset(bank)?;
        Ok(&prg[offset..offset + PRG_BANK_SIZE])
    }

    /// Return the PRG bytes visible in the fixed $C000-$FFFF window.
    pub fn fixed_prg<'a>(self, prg: &'a [u8]) -> &'a [u8] {
        match self {
            Self::Nrom { prg_len } | Self::Cnrom { prg_len, .. } if prg_len == PRG_BANK_SIZE => prg,
            Self::Nrom { .. } | Self::Cnrom { .. } => &prg[PRG_BANK_SIZE..],
            Self::Uxrom { bank_count, .. } => {
                let offset = (bank_count as usize - 1) * PRG_BANK_SIZE;
                &prg[offset..offset + PRG_BANK_SIZE]
            }
        }
    }

    /// Return the bytes in NROM's lower $8000-$BFFF window.
    pub fn lower_prg<'a>(self, prg: &'a [u8]) -> &'a [u8] {
        &prg[..PRG_BANK_SIZE]
    }

    /// Construct the NROM-shaped analysis view for a selected UxROM bank.
    pub fn analysis_view(
        self,
        prg: &[u8],
        selected_bank: u8,
    ) -> Result<Vec<u8>, MapperPolicyError> {
        match self {
            Self::Nrom { .. } | Self::Cnrom { .. } => Ok(prg.to_vec()),
            Self::Uxrom { .. } => {
                let mut view = Vec::with_capacity(2 * PRG_BANK_SIZE);
                view.extend_from_slice(self.prg_bank(prg, selected_bank)?);
                view.extend_from_slice(self.fixed_prg(prg));
                Ok(view)
            }
        }
    }

    /// CNROM CHR bank selected by a $8000-$FFFF write, honoring submapper-2
    /// AND bus conflicts. Returns None for non-CNROM policies. The result
    /// indexes an 8 KiB CHR window; it does not affect PRG/CPU reads.
    pub fn cnrom_chr_bank_from_write(self, raw_write: u8, pre_write_rom_byte: u8) -> Option<u8> {
        match self {
            Self::Cnrom {
                chr_bank_count,
                bus_conflicts,
                ..
            } => {
                let effective = match bus_conflicts {
                    UxromBusConflicts::None => raw_write,
                    UxromBusConflicts::And => raw_write & pre_write_rom_byte,
                };
                Some(if chr_bank_count == 0 {
                    0
                } else {
                    effective % chr_bank_count
                })
            }
            _ => None,
        }
    }

    pub fn uxrom_bus_conflicts(self) -> Option<UxromBusConflicts> {
        match self {
            Self::Nrom { .. } | Self::Cnrom { .. } => None,
            Self::Uxrom { bus_conflicts, .. } => Some(bus_conflicts),
        }
    }

    /// Apply a mapper write with its pre-write ROM bus byte.
    pub fn selected_bank_from_write(
        self,
        raw_write: u8,
        pre_write_rom_byte: u8,
    ) -> Result<u8, MapperPolicyError> {
        match self {
            Self::Nrom { .. } | Self::Cnrom { .. } => Ok(0),
            Self::Uxrom {
                bank_count,
                bus_conflicts,
            } => {
                let effective = match bus_conflicts {
                    UxromBusConflicts::None => raw_write,
                    UxromBusConflicts::And => raw_write & pre_write_rom_byte,
                };
                let bank = effective & (bank_count - 1);
                self.checked_bank_offset(bank)?;
                Ok(bank)
            }
        }
    }
}

/// Read vectors using an explicit supported mapper policy.
pub fn read_vectors_with_policy(
    policy: MapperPolicy,
    prg: &[u8],
) -> Result<Option<Vectors>, MapperPolicyError> {
    let read_word = |cpu_addr| -> Result<Option<u16>, MapperPolicyError> {
        let Some(offset) = policy.cpu_to_prg_offset(cpu_addr, 0)? else {
            return Ok(None);
        };
        Ok(prg
            .get(offset..offset + 2)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_le_bytes))
    };
    Ok(Some(Vectors {
        nmi: match read_word(0xfffa)? {
            Some(value) => value,
            None => return Ok(None),
        },
        reset: match read_word(0xfffc)? {
            Some(value) => value,
            None => return Ok(None),
        },
        irq: match read_word(0xfffe)? {
            Some(value) => value,
            None => return Ok(None),
        },
    }))
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

    fn header(mapper: u16) -> Header {
        Header {
            kind: if mapper == 2 {
                HeaderKind::Nes2
            } else {
                HeaderKind::INes
            },
            prg_banks: 0,
            chr_banks: 0,
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper,
            submapper: if mapper == 2 { 2 } else { 0 },
            mirroring: Mirroring::Horizontal,
            has_trainer: false,
            has_battery: false,
        }
    }

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
        assert_eq!(h.prg_ram_size, 8 * 1024);
        assert_eq!(h.chr_ram_size, 0);
        assert_eq!(h.prg_nvram_size, 0);
        assert_eq!(h.chr_nvram_size, 0);
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

    #[test]
    fn nrom_policy_maps_boundaries() {
        let nrom128 = resolve_mapper_policy(&header(0), PRG_BANK_SIZE).unwrap();
        assert_eq!(nrom128.cpu_to_prg_offset(0x7fff, 0).unwrap(), None);
        assert_eq!(nrom128.cpu_to_prg_offset(0x8000, 0).unwrap(), Some(0));
        assert_eq!(
            nrom128.cpu_to_prg_offset(0xbfff, 0).unwrap(),
            Some(PRG_BANK_SIZE - 1)
        );
        assert_eq!(nrom128.cpu_to_prg_offset(0xc000, 0).unwrap(), Some(0));
        assert_eq!(
            nrom128.cpu_to_prg_offset(0xffff, 0).unwrap(),
            Some(PRG_BANK_SIZE - 1)
        );

        let nrom256 = resolve_mapper_policy(&header(0), 2 * PRG_BANK_SIZE).unwrap();
        assert_eq!(nrom256.cpu_to_prg_offset(0x8000, 0).unwrap(), Some(0));
        assert_eq!(
            nrom256.cpu_to_prg_offset(0xffff, 0).unwrap(),
            Some(2 * PRG_BANK_SIZE - 1)
        );
    }

    #[test]
    fn uxrom_policy_maps_all_cv1_banks_and_fixed_bank() {
        let policy = resolve_mapper_policy(&header(2), 8 * PRG_BANK_SIZE).unwrap();
        assert!(policy.is_banked());
        assert_eq!(policy.bank_count(), 8);
        for bank in 0..8 {
            assert_eq!(
                policy.cpu_to_prg_offset(0x8000, bank).unwrap(),
                Some(bank as usize * PRG_BANK_SIZE)
            );
            assert_eq!(
                policy.cpu_to_prg_offset(0xbfff, bank).unwrap(),
                Some((bank as usize + 1) * PRG_BANK_SIZE - 1)
            );
            assert_eq!(
                policy.cpu_to_prg_offset(0xc000, bank).unwrap(),
                Some(7 * PRG_BANK_SIZE)
            );
            assert_eq!(
                policy.cpu_to_prg_offset(0xffff, bank).unwrap(),
                Some(8 * PRG_BANK_SIZE - 1)
            );
        }
    }

    #[test]
    fn cnrom_policy_fixes_prg_and_selects_chr_bank() {
        let mut h = header(3);
        h.kind = HeaderKind::Nes2;
        h.submapper = 2; // AND bus conflicts
        h.prg_banks = 2; // 32 KiB PRG
        h.chr_banks = 4; // 32 KiB CHR = four 8 KiB banks
        let policy = resolve_mapper_policy_oracle(&h, 2 * PRG_BANK_SIZE).unwrap();
        assert!(matches!(policy, MapperPolicy::Cnrom { .. }));
        // PRG behaves exactly like NROM-32: fixed, unbanked.
        assert!(!policy.is_banked());
        assert_eq!(policy.cpu_to_prg_offset(0x8000, 0).unwrap(), Some(0));
        assert_eq!(
            policy.cpu_to_prg_offset(0xffff, 0).unwrap(),
            Some(2 * PRG_BANK_SIZE - 1)
        );
        // A $8000+ write selects the CHR bank (mod 4), AND'd with the bus byte.
        assert_eq!(policy.cnrom_chr_bank_from_write(0x02, 0xff), Some(2));
        assert_eq!(policy.cnrom_chr_bank_from_write(0x07, 0x02), Some(2)); // 7 & 2 = 2
        assert_eq!(policy.cnrom_chr_bank_from_write(0x05, 0xff), Some(1)); // 5 % 4 = 1
        // Non-CNROM policies do not select a CHR bank.
        let uxrom = resolve_mapper_policy(&header(2), 8 * PRG_BANK_SIZE).unwrap();
        assert_eq!(uxrom.cnrom_chr_bank_from_write(3, 0xff), None);
    }

    #[test]
    fn cnrom_requires_nes2_and_valid_prg() {
        let mut ines = header(3);
        ines.kind = HeaderKind::INes;
        ines.prg_banks = 2;
        assert_eq!(
            resolve_mapper_policy_oracle(&ines, 2 * PRG_BANK_SIZE),
            Err(MapperPolicyError::UxromRequiresNes2)
        );
        let mut bad = header(3);
        bad.kind = HeaderKind::Nes2;
        assert_eq!(
            resolve_mapper_policy_oracle(&bad, 3 * PRG_BANK_SIZE),
            Err(MapperPolicyError::InvalidNromPrgLayout {
                prg_len: 3 * PRG_BANK_SIZE
            })
        );
    }

    #[test]
    fn uxrom_policy_rejects_invalid_selected_bank() {
        let policy = resolve_mapper_policy(&header(2), 8 * PRG_BANK_SIZE).unwrap();
        assert_eq!(
            policy.cpu_to_prg_offset(0x8000, 8),
            Err(MapperPolicyError::SelectedBankOutOfRange {
                bank: 8,
                bank_count: 8,
            })
        );
        assert_eq!(
            policy.cpu_to_prg_offset(0xc000, 9),
            Err(MapperPolicyError::SelectedBankOutOfRange {
                bank: 9,
                bank_count: 8,
            })
        );
    }

    #[test]
    fn uxrom_submappers_select_bus_conflict_mode() {
        let mut no_conflict = header(2);
        no_conflict.submapper = 1;
        let no_conflict = resolve_mapper_policy(&no_conflict, 8 * PRG_BANK_SIZE).unwrap();
        assert_eq!(
            no_conflict.uxrom_bus_conflicts(),
            Some(UxromBusConflicts::None)
        );
        assert_eq!(no_conflict.selected_bank_from_write(3, 0), Ok(3));
        assert_eq!(no_conflict.selected_bank_from_write(0xf9, 0), Ok(1));

        let conflict = resolve_mapper_policy(&header(2), 8 * PRG_BANK_SIZE).unwrap();
        assert_eq!(conflict.uxrom_bus_conflicts(), Some(UxromBusConflicts::And));
        assert_eq!(conflict.selected_bank_from_write(7, 3), Ok(3));
        assert_eq!(conflict.selected_bank_from_write(0xff, 0xfa), Ok(2));
    }

    #[test]
    fn uxrom_rejects_legacy_and_unknown_submappers() {
        let mut legacy = header(2);
        legacy.kind = HeaderKind::INes;
        assert_eq!(
            resolve_mapper_policy(&legacy, 8 * PRG_BANK_SIZE),
            Err(MapperPolicyError::UxromRequiresNes2)
        );
        for submapper in [0, 3] {
            let mut unknown = header(2);
            unknown.submapper = submapper;
            assert_eq!(
                resolve_mapper_policy(&unknown, 8 * PRG_BANK_SIZE),
                Err(MapperPolicyError::UnsupportedUxromSubmapper { submapper })
            );
        }
    }

    #[test]
    fn nes2_extended_mapper_bits_do_not_alias_supported_boards() {
        let mut bytes = [0; HEADER_SIZE];
        bytes[..4].copy_from_slice(&INES_MAGIC);
        bytes[4] = 2;
        bytes[5] = 4;
        bytes[6] = 0x30;
        bytes[7] = 0x08;
        bytes[8] = 0x21; // Submapper 2, mapper $103, not CNROM $003.
        let h = parse_header(&bytes).unwrap();
        assert_eq!(h.mapper, 0x103);
        assert_eq!(h.submapper, 2);
        assert_eq!(
            cnrom::Cnrom::new(&h, h.prg_len(), h.chr_len(), 0),
            Err(cnrom::CnromError::UnsupportedMapper { mapper: 0x103 })
        );
        for low_mapper in [0, 2, 3, 4] {
            bytes[6] = low_mapper << 4;
            let h = parse_header(&bytes).unwrap();
            let extended_mapper = 0x100 | u16::from(low_mapper);
            assert_eq!(h.mapper, extended_mapper);
            assert_eq!(
                resolve_mapper_policy(&h, h.prg_len()),
                Err(MapperPolicyError::UnsupportedMapper {
                    mapper: extended_mapper
                })
            );
        }
        bytes[6] = 0xf0;
        bytes[7] = 0xf8;
        bytes[8] = 0xaf;
        let h = parse_header(&bytes).unwrap();
        assert_eq!(h.mapper, 0xfff);
        assert_eq!(h.submapper, 10);
        bytes[7] = 0xf0; // Legacy byte 8 is RAM size, never mapper bits.
        assert_eq!(parse_header(&bytes).unwrap().mapper, 0xff);
    }

    #[test]
    fn mapper_policy_rejects_invalid_layouts_and_unsupported_mapper() {
        assert_eq!(
            resolve_mapper_policy(&header(0), 3 * PRG_BANK_SIZE),
            Err(MapperPolicyError::InvalidNromPrgLayout {
                prg_len: 3 * PRG_BANK_SIZE,
            })
        );
        assert_eq!(
            resolve_mapper_policy(&header(2), PRG_BANK_SIZE),
            Err(MapperPolicyError::InvalidUxromPrgLayout {
                prg_len: PRG_BANK_SIZE,
            })
        );
        assert_eq!(
            resolve_mapper_policy(&header(2), 3 * PRG_BANK_SIZE),
            Err(MapperPolicyError::InvalidUxromPrgLayout {
                prg_len: 3 * PRG_BANK_SIZE,
            })
        );
        assert_eq!(
            resolve_mapper_policy(&header(2), 17 * PRG_BANK_SIZE),
            Err(MapperPolicyError::InvalidUxromPrgLayout {
                prg_len: 17 * PRG_BANK_SIZE,
            })
        );
        assert_eq!(
            resolve_mapper_policy(&header(1), 2 * PRG_BANK_SIZE),
            Err(MapperPolicyError::UnsupportedMapper { mapper: 1 })
        );
    }

    #[test]
    fn hashes_canonical_prg_then_chr_payload() {
        assert_eq!(
            payload_sha256_hex(b"ab", b"c"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parses_nes2_cv1_chr_ram_header() {
        let mut rom = vec![0u8; HEADER_SIZE];
        rom[0..4].copy_from_slice(&INES_MAGIC);
        rom[4] = 8;
        rom[6] = 0x21;
        rom[7] = 0x08;
        rom[8] = 0x20;
        rom[11] = 0x07;

        let h = parse_header(&rom).unwrap();
        assert_eq!(h.kind, HeaderKind::Nes2);
        assert_eq!(h.prg_banks, 8);
        assert_eq!(h.mapper, 2);
        assert_eq!(h.submapper, 2);
        assert_eq!(h.mirroring, Mirroring::Vertical);
        assert_eq!(h.chr_len(), 0);
        assert_eq!(h.prg_ram_size, 0);
        assert_eq!(h.prg_nvram_size, 0);
        assert_eq!(h.chr_ram_size, 8 * 1024);
        assert_eq!(h.chr_nvram_size, 0);
    }

    #[test]
    fn parses_nes2_volatile_and_nonvolatile_ram_sizes() {
        let mut rom = vec![0u8; HEADER_SIZE];
        rom[0..4].copy_from_slice(&INES_MAGIC);
        rom[7] = 0x08;
        rom[10] = 0x32;
        rom[11] = 0x54;

        let h = parse_header(&rom).unwrap();
        assert_eq!(h.prg_ram_size, 64 << 2);
        assert_eq!(h.prg_nvram_size, 64 << 3);
        assert_eq!(h.chr_ram_size, 64 << 4);
        assert_eq!(h.chr_nvram_size, 64 << 5);
    }

    #[test]
    fn ines_uses_chr_ram_fallback_without_chr_rom() {
        let mut rom = vec![0u8; HEADER_SIZE];
        rom[0..4].copy_from_slice(&INES_MAGIC);
        rom[4] = 1;
        rom[8] = 2;

        let h = parse_header(&rom).unwrap();
        assert_eq!(h.prg_ram_size, 16 * 1024);
        assert_eq!(h.chr_len(), 0);
        assert_eq!(h.chr_ram_size, 8 * 1024);
        assert_eq!(h.prg_nvram_size, 0);
        assert_eq!(h.chr_nvram_size, 0);
    }

    #[test]
    fn ines_nonzero_chr_rom_does_not_imply_chr_ram() {
        let mut rom = vec![0u8; HEADER_SIZE];
        rom[0..4].copy_from_slice(&INES_MAGIC);
        rom[5] = 1;

        let h = parse_header(&rom).unwrap();
        assert_eq!(h.chr_len(), CHR_BANK_SIZE);
        assert_eq!(h.chr_ram_size, 0);
    }
}
