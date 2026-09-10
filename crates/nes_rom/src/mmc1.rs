//! Standard MMC1 (mapper 1) cartridge decoding and register state.
//!
//! Separate from [`crate::MapperPolicy`]: describing a cartridge does not
//! establish that the SMS runtime can execute it. This models the common
//! SxROM behavior — a 5-bit serial shift register feeding four internal
//! registers (control, CHR bank 0/1, PRG bank), the three PRG banking modes,
//! the 8/4+4 KiB CHR mode, and an optional 8 KiB (battery-backed) PRG-RAM at
//! $6000-$7FFF. Zelda (mapper 1, 128 KiB PRG, 8 KiB CHR-RAM, battery) is the
//! first target.
//!
//! Not modeled: SxROM variants that repurpose CHR bank bits for extended PRG
//! or PRG-RAM banking (SUROM/SOROM/SXROM 512 KiB, multiple RAM chips), the
//! MMC1A/MMC1B consecutive-write quirk timing, and CHR-ROM boards (added
//! when a CHR-ROM MMC1 game enters the queue).
//!
//! Register contract: <https://www.nesdev.org/wiki/MMC1>.

use crate::{Header, HeaderKind, Mirroring, PRG_BANK_SIZE};
use std::fmt;

/// 16 KiB PRG window; 8 KiB PRG-RAM window; 4 KiB CHR window.
pub const PRG_WINDOW_SIZE: usize = 16 * 1024;
pub const PRG_RAM_SIZE: usize = 8 * 1024;
pub const CHR_WINDOW_SIZE: usize = 4 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc1Error {
    UnsupportedMapper {
        mapper: u16,
    },
    RequiresNes2,
    UnsupportedSubmapper {
        submapper: u8,
    },
    UnsupportedTrainer,
    UnsupportedFourScreen,
    InvalidPrgLayout {
        prg_len: usize,
    },
    /// Only CHR-RAM boards are modeled so far (Zelda); CHR-ROM is deferred.
    UnsupportedChrRom,
    HeaderPayloadMismatch,
    /// SUROM/SXROM 512 KiB PRG needs CHR-bit PRG extension, not yet modeled.
    UnsupportedLargePrg {
        prg_len: usize,
    },
}

impl fmt::Display for Mmc1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => write!(f, "MMC1 requires mapper 1, got {mapper}"),
            Self::RequiresNes2 => write!(f, "MMC1 requires an explicit NES 2.0 board declaration"),
            Self::UnsupportedSubmapper { submapper } => {
                write!(
                    f,
                    "unsupported MMC1 submapper {submapper}; only 0/5 modeled"
                )
            }
            Self::UnsupportedTrainer => write!(f, "MMC1 trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => {
                write!(f, "MMC1 four-screen board wiring is unsupported")
            }
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "MMC1 requires power-of-two 32–256 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::UnsupportedChrRom => {
                write!(f, "only CHR-RAM MMC1 boards are modeled so far")
            }
            Self::HeaderPayloadMismatch => write!(f, "MMC1 header and payload lengths disagree"),
            Self::UnsupportedLargePrg { prg_len } => write!(
                f,
                "MMC1 512 KiB PRG (SUROM/SXROM extension) is not modeled, got {prg_len} bytes"
            ),
        }
    }
}

impl std::error::Error for Mmc1Error {}

/// One of the four MMC1 nametable mirroring modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc1Mirroring {
    OneScreenLower,
    OneScreenUpper,
    Vertical,
    Horizontal,
}

/// Board register state. The bus owns ROM/RAM; this owns only mapper state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mmc1 {
    prg_bank_count: u8, // number of 16 KiB PRG banks
    has_battery: bool,
    // Serial load port.
    shift: u8,
    shift_count: u8,
    // Internal registers (5 bits each).
    control: u8, // bit0-1 mirroring, bit2-3 PRG mode, bit4 CHR mode
    chr0: u8,
    chr1: u8,
    prg: u8, // bit0-3 PRG bank, bit4 PRG-RAM disable (1 = disabled)
}

impl Mmc1 {
    pub fn new(header: &Header, prg_len: usize) -> Result<Self, Mmc1Error> {
        if header.mapper != 1 {
            return Err(Mmc1Error::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.kind != HeaderKind::Nes2 {
            return Err(Mmc1Error::RequiresNes2);
        }
        // Submapper 0 (unspecified) and 5 (fixed PRG-RAM/no conflicts) behave
        // identically for this board model; others are not established.
        if !matches!(header.submapper, 0 | 5) {
            return Err(Mmc1Error::UnsupportedSubmapper {
                submapper: header.submapper,
            });
        }
        if header.has_trainer {
            return Err(Mmc1Error::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(Mmc1Error::UnsupportedFourScreen);
        }
        if header.chr_len() != 0 {
            return Err(Mmc1Error::UnsupportedChrRom);
        }
        if prg_len % PRG_BANK_SIZE != 0 || !prg_len.is_power_of_two() {
            return Err(Mmc1Error::InvalidPrgLayout { prg_len });
        }
        let prg_bank_count = (prg_len / PRG_BANK_SIZE) as u8;
        if !(2..=16).contains(&prg_bank_count) {
            return Err(Mmc1Error::InvalidPrgLayout { prg_len });
        }
        if prg_len > 16 * PRG_BANK_SIZE {
            return Err(Mmc1Error::UnsupportedLargePrg { prg_len });
        }
        if header.prg_len() != prg_len {
            return Err(Mmc1Error::HeaderPayloadMismatch);
        }
        Ok(Self {
            prg_bank_count,
            has_battery: header.has_battery,
            shift: 0,
            shift_count: 0,
            // Power-on: PRG mode 3 (fix last 16 KiB at $C000), the reset state.
            control: 0x0c,
            chr0: 0,
            chr1: 0,
            prg: 0,
        })
    }

    pub fn has_battery(&self) -> bool {
        self.has_battery
    }

    pub fn mirroring(&self) -> Mmc1Mirroring {
        match self.control & 0x03 {
            0 => Mmc1Mirroring::OneScreenLower,
            1 => Mmc1Mirroring::OneScreenUpper,
            2 => Mmc1Mirroring::Vertical,
            _ => Mmc1Mirroring::Horizontal,
        }
    }

    /// A CPU write to $8000-$FFFF drives the serial port. Bit 7 set resets the
    /// shift register and forces PRG mode 3; otherwise bit 0 is shifted in and
    /// the fifth write commits to the register selected by address bits 13-14.
    pub fn write_register(&mut self, cpu_addr: u16, value: u8) {
        debug_assert!(cpu_addr >= 0x8000);
        if value & 0x80 != 0 {
            self.shift = 0;
            self.shift_count = 0;
            self.control |= 0x0c;
            return;
        }
        self.shift = (self.shift >> 1) | ((value & 1) << 4);
        self.shift_count += 1;
        if self.shift_count < 5 {
            return;
        }
        let data = self.shift & 0x1f;
        match (cpu_addr >> 13) & 0x03 {
            0 => self.control = data,
            1 => self.chr0 = data,
            2 => self.chr1 = data,
            _ => self.prg = data,
        }
        self.shift = 0;
        self.shift_count = 0;
    }

    /// Map a CPU address in $8000-$FFFF to a flat PRG-ROM offset per the
    /// current PRG banking mode. Returns None below $8000.
    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        if cpu_addr < 0x8000 {
            return None;
        }
        let last = self.prg_bank_count - 1;
        let bank = self.prg & 0x0f;
        let mode = (self.control >> 2) & 0x03;
        let bank16 = match mode {
            // 32 KiB switch: low bit ignored, two consecutive 16 KiB banks.
            0 | 1 => {
                let base = (bank & !1) % self.prg_bank_count;
                return Some(base as usize * PRG_BANK_SIZE + (cpu_addr as usize - 0x8000));
            }
            // Fix first bank at $8000, switch 16 KiB at $C000.
            2 => {
                if cpu_addr < 0xC000 {
                    0
                } else {
                    bank % self.prg_bank_count
                }
            }
            // Switch 16 KiB at $8000, fix last bank at $C000 (power-on).
            _ => {
                if cpu_addr < 0xC000 {
                    bank % self.prg_bank_count
                } else {
                    last
                }
            }
        };
        let window = if cpu_addr < 0xC000 {
            cpu_addr as usize - 0x8000
        } else {
            cpu_addr as usize - 0xC000
        };
        Some(bank16 as usize * PRG_BANK_SIZE + window)
    }

    /// The 4 KiB CHR window (0 = $0000-$0FFF, 1 = $1000-$1FFF) selected bank.
    /// In 8 KiB CHR mode both windows come from `chr0 & !1` and its successor.
    /// For CHR-RAM boards this indexes 4 KiB windows of the CHR-RAM.
    pub fn chr_bank_4k(&self, window: u8) -> u8 {
        if self.control & 0x10 == 0 {
            // 8 KiB mode: chr0 low bit ignored; window picks the half.
            (self.chr0 & !1) | (window & 1)
        } else if window == 0 {
            self.chr0
        } else {
            self.chr1
        }
    }

    /// PRG-RAM at $6000-$7FFF is enabled unless the PRG register's bit 4 is set.
    pub fn prg_ram_enabled(&self) -> bool {
        self.prg & 0x10 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zelda_header() -> Header {
        Header {
            kind: HeaderKind::Nes2,
            prg_banks: 8, // 128 KiB
            chr_banks: 0, // CHR-RAM
            prg_ram_size: 0,
            prg_nvram_size: 8 * 1024,
            chr_ram_size: 8 * 1024,
            chr_nvram_size: 0,
            mapper: 1,
            submapper: 0,
            mirroring: Mirroring::Horizontal,
            has_trainer: false,
            has_battery: true,
        }
    }

    fn write5(m: &mut Mmc1, addr: u16, value: u8) {
        for i in 0..5 {
            m.write_register(addr, (value >> i) & 1);
        }
    }

    #[test]
    fn zelda_board_parses_and_powers_on_in_mode3() {
        let m = Mmc1::new(&zelda_header(), 8 * PRG_BANK_SIZE).unwrap();
        assert!(m.has_battery());
        // Power-on PRG mode 3: $8000 switches (bank 0), $C000 fixes last (7).
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(0));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(7 * PRG_BANK_SIZE));
        assert_eq!(m.cpu_to_prg_offset(0xFFFF), Some(8 * PRG_BANK_SIZE - 1));
        assert_eq!(m.cpu_to_prg_offset(0x7FFF), None);
        assert!(m.prg_ram_enabled());
    }

    #[test]
    fn serial_shift_commits_on_fifth_write_and_selects_bank() {
        let mut m = Mmc1::new(&zelda_header(), 8 * PRG_BANK_SIZE).unwrap();
        // Select PRG bank 3 in mode 3 ($E000 register).
        write5(&mut m, 0xE000, 3);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(3 * PRG_BANK_SIZE));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(7 * PRG_BANK_SIZE));
        // Fewer than five bits: no commit.
        m.write_register(0xE000, 1);
        m.write_register(0xE000, 0);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(3 * PRG_BANK_SIZE));
    }

    #[test]
    fn reset_bit_forces_mode3() {
        let mut m = Mmc1::new(&zelda_header(), 8 * PRG_BANK_SIZE).unwrap();
        // Switch to mode 2 (fix first, switch last), then reset.
        write5(&mut m, 0x8000, 0b0_1000); // control: PRG mode 2
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(0)); // first fixed
        m.write_register(0x8000, 0x80); // reset -> mode 3
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(7 * PRG_BANK_SIZE));
    }

    #[test]
    fn prg_modes_map_windows() {
        let mut m = Mmc1::new(&zelda_header(), 8 * PRG_BANK_SIZE).unwrap();
        // Mode 0/1: 32 KiB switch. Select bank 5 -> uses bank 4 (low bit clear).
        write5(&mut m, 0x8000, 0b0_0000); // PRG mode 0
        write5(&mut m, 0xE000, 5);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(4 * PRG_BANK_SIZE));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(5 * PRG_BANK_SIZE));
        // Mode 2: fix first at $8000, switch at $C000.
        write5(&mut m, 0x8000, 0b0_1000);
        write5(&mut m, 0xE000, 6);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(0));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(6 * PRG_BANK_SIZE));
    }

    #[test]
    fn chr_windows_and_mirroring() {
        let mut m = Mmc1::new(&zelda_header(), 8 * PRG_BANK_SIZE).unwrap();
        // 8 KiB CHR mode (control bit4 = 0): chr0 low bit ignored.
        write5(&mut m, 0xA000, 3); // chr0
        assert_eq!(m.chr_bank_4k(0), 2);
        assert_eq!(m.chr_bank_4k(1), 3);
        // 4 KiB mode.
        write5(&mut m, 0x8000, 0b1_0000); // control: CHR mode 1
        write5(&mut m, 0xA000, 5);
        write5(&mut m, 0xC000, 6);
        assert_eq!(m.chr_bank_4k(0), 5);
        assert_eq!(m.chr_bank_4k(1), 6);
        // Mirroring select.
        write5(&mut m, 0x8000, 0b1_0010); // vertical (bits0-1 = 2) + CHR mode 1
        assert_eq!(m.mirroring(), Mmc1Mirroring::Vertical);
    }

    #[test]
    fn rejects_non_mmc1_and_chr_rom() {
        let mut h = zelda_header();
        h.mapper = 4;
        assert!(matches!(
            Mmc1::new(&h, 8 * PRG_BANK_SIZE),
            Err(Mmc1Error::UnsupportedMapper { mapper: 4 })
        ));
        let mut h = zelda_header();
        h.chr_banks = 1; // CHR-ROM
        assert!(matches!(
            Mmc1::new(&h, 8 * PRG_BANK_SIZE),
            Err(Mmc1Error::UnsupportedChrRom)
        ));
        let mut h = zelda_header();
        h.kind = HeaderKind::INes;
        assert!(matches!(
            Mmc1::new(&h, 8 * PRG_BANK_SIZE),
            Err(Mmc1Error::RequiresNes2)
        ));
    }
}
