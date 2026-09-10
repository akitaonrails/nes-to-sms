//! AxROM (mapper 7) cartridge decoding and register state.
//!
//! Separate from [`crate::MapperPolicy`]. AxROM is simple: a single write to
//! $8000-$FFFF selects a 32 KiB PRG bank (low bits) and the single-screen
//! nametable (bit 4). CHR is always RAM. Bus conflicts exist on AMROM/ANROM
//! but not AOROM; games that matter treat the port as conflict-free, so this
//! model applies the write value directly (the common, safe behavior).
//!
//! Register contract: <https://www.nesdev.org/wiki/AxROM>.

use crate::{Header, PRG_BANK_SIZE};
use std::fmt;

/// AxROM switches PRG in 32 KiB units.
pub const PRG_WINDOW_SIZE: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxromError {
    UnsupportedMapper {
        mapper: u16,
    },
    UnsupportedTrainer,
    /// AxROM is single-screen; explicit four-screen wiring is not a board.
    UnsupportedFourScreen,
    UnsupportedChrRom,
    InvalidPrgLayout {
        prg_len: usize,
    },
    HeaderPayloadMismatch,
}

impl fmt::Display for AxromError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => {
                write!(f, "AxROM requires mapper 7, got {mapper}")
            }
            Self::UnsupportedTrainer => write!(f, "AxROM trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => write!(f, "AxROM four-screen wiring is unsupported"),
            Self::UnsupportedChrRom => write!(f, "AxROM boards use CHR-RAM, not CHR-ROM"),
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "AxROM requires power-of-two 32–256 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "AxROM header and payload lengths disagree"),
        }
    }
}

impl std::error::Error for AxromError {}

/// Single-screen nametable selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxromNametable {
    Lower,
    Upper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axrom {
    bank_count: u8, // number of 32 KiB banks
    bank: u8,
    nametable: u8, // 0 = lower, 1 = upper
}

impl Axrom {
    pub fn new(header: &Header, prg_len: usize) -> Result<Self, AxromError> {
        if header.mapper != 7 {
            return Err(AxromError::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.has_trainer {
            return Err(AxromError::UnsupportedTrainer);
        }
        if header.mirroring == crate::Mirroring::FourScreen {
            return Err(AxromError::UnsupportedFourScreen);
        }
        if header.chr_len() != 0 {
            return Err(AxromError::UnsupportedChrRom);
        }
        if prg_len % PRG_WINDOW_SIZE != 0 || !prg_len.is_power_of_two() {
            return Err(AxromError::InvalidPrgLayout { prg_len });
        }
        let bank_count = (prg_len / PRG_WINDOW_SIZE) as u8;
        if !(1..=8).contains(&bank_count) {
            return Err(AxromError::InvalidPrgLayout { prg_len });
        }
        if header.prg_len() != prg_len {
            return Err(AxromError::HeaderPayloadMismatch);
        }
        Ok(Self {
            bank_count,
            bank: 0,
            nametable: 0,
        })
    }

    pub fn nametable(&self) -> AxromNametable {
        if self.nametable == 0 {
            AxromNametable::Lower
        } else {
            AxromNametable::Upper
        }
    }

    /// A CPU write to $8000-$FFFF selects the 32 KiB PRG bank (low bits) and
    /// the single-screen nametable (bit 4).
    pub fn write_register(&mut self, _cpu_addr: u16, value: u8) {
        self.bank = value % self.bank_count;
        self.nametable = (value >> 4) & 1;
    }

    /// Map a CPU address in $8000-$FFFF to a flat PRG-ROM offset. None below.
    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        if cpu_addr < 0x8000 {
            return None;
        }
        Some(self.bank as usize * PRG_WINDOW_SIZE + (cpu_addr as usize - 0x8000))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeaderKind, Mirroring};

    fn battletoads_header() -> Header {
        Header {
            kind: HeaderKind::INes,
            prg_banks: 16, // 256 KiB
            chr_banks: 0,  // CHR-RAM
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 8 * 1024,
            chr_nvram_size: 0,
            mapper: 7,
            submapper: 0,
            mirroring: Mirroring::Horizontal,
            has_trainer: false,
            has_battery: false,
        }
    }

    #[test]
    fn battletoads_board_parses_and_switches_banks() {
        let mut a = Axrom::new(&battletoads_header(), 16 * PRG_BANK_SIZE).unwrap();
        // 256 KiB = eight 32 KiB banks. Power-on bank 0.
        assert_eq!(a.cpu_to_prg_offset(0x8000), Some(0));
        assert_eq!(a.cpu_to_prg_offset(0xFFFF), Some(PRG_WINDOW_SIZE - 1));
        assert_eq!(a.cpu_to_prg_offset(0x7FFF), None);
        // Select bank 5, upper nametable.
        a.write_register(0x8000, 0x15);
        assert_eq!(a.cpu_to_prg_offset(0x8000), Some(5 * PRG_WINDOW_SIZE));
        assert_eq!(a.nametable(), AxromNametable::Upper);
        // Bank field wraps by bank_count.
        a.write_register(0x9ABC, 0x08); // 8 % 8 = 0
        assert_eq!(a.cpu_to_prg_offset(0x8000), Some(0));
        assert_eq!(a.nametable(), AxromNametable::Lower);
    }

    #[test]
    fn rejects_non_axrom_and_chr_rom() {
        let mut h = battletoads_header();
        h.mapper = 1;
        assert!(matches!(
            Axrom::new(&h, 16 * PRG_BANK_SIZE),
            Err(AxromError::UnsupportedMapper { mapper: 1 })
        ));
        let mut h = battletoads_header();
        h.chr_banks = 1;
        assert!(matches!(
            Axrom::new(&h, 16 * PRG_BANK_SIZE),
            Err(AxromError::UnsupportedChrRom)
        ));
    }
}
