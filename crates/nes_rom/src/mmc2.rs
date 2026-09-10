//! Nintendo MMC2 (mapper 9) and MMC4 (mapper 10) board decoding.
//!
//! Separate from [`crate::MapperPolicy`]. The two chips are the same design
//! (PxROM): a switchable PRG window with the rest fixed at the top of ROM,
//! two 4 KiB CHR windows, mirroring, and the distinctive CHR *latch* — the PPU
//! fetching tile $FD or $FE from a pattern table flips which of two 4 KiB CHR
//! banks that table shows (Punch-Out's animating faces, Fire Emblem's portrait
//! blinks). MMC2 switches an 8 KiB PRG window at $8000 with the last three
//! 8 KiB banks fixed; MMC4 switches a 16 KiB window at $8000 with the last
//! 16 KiB fixed.
//!
//! The frame-granular oracle does not run per-tile PPU fetches, so the latch is
//! modeled as state (updated on demand via [`Mmc2::update_latch`]) but does not
//! self-trigger during rendering; PRG banking — what boot needs — is exact.
//!
//! Register contract: <https://www.nesdev.org/wiki/MMC2>, <https://www.nesdev.org/wiki/MMC4>.

use crate::{Header, Mirroring, NametableMirroring, windowed_bank_count};
use std::fmt;

pub const CHR_WINDOW_SIZE: usize = 4 * 1024;
const PRG_8K: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc2Error {
    UnsupportedMapper { mapper: u16 },
    UnsupportedTrainer,
    UnsupportedFourScreen,
    InvalidPrgLayout { prg_len: usize },
    InvalidChrLayout { chr_len: usize },
    HeaderPayloadMismatch,
}

impl fmt::Display for Mmc2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => {
                write!(f, "MMC2/MMC4 requires mapper 9 or 10, got {mapper}")
            }
            Self::UnsupportedTrainer => {
                write!(f, "MMC2/MMC4 trainer initialization is unsupported")
            }
            Self::UnsupportedFourScreen => {
                write!(f, "MMC2/MMC4 four-screen wiring is unsupported")
            }
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "MMC2/MMC4 requires power-of-two 32–256 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "MMC2/MMC4 requires power-of-two 16–256 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => {
                write!(f, "MMC2/MMC4 header and payload lengths disagree")
            }
        }
    }
}

impl std::error::Error for Mmc2Error {}

/// Which of the two latch states a pattern table is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Latch {
    Fd,
    Fe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mmc2 {
    is_mmc4: bool, // false = MMC2 (8 KiB PRG), true = MMC4 (16 KiB PRG)
    prg_window_banks: usize,
    prg_bank_count_8k: usize,
    chr_bank_count_4k: usize,
    prg_bank: u8, // the switchable window select
    // CHR banks: [table0 $FD, table0 $FE, table1 $FD, table1 $FE], 4 KiB each.
    chr: [u8; 4],
    latch0: Latch,
    latch1: Latch,
    mirroring: u8,
    has_wram: bool,
}

impl Mmc2 {
    pub fn new(header: &Header, prg_len: usize, chr_len: usize) -> Result<Self, Mmc2Error> {
        let is_mmc4 = match header.mapper {
            9 => false,
            10 => true,
            mapper => return Err(Mmc2Error::UnsupportedMapper { mapper }),
        };
        if header.has_trainer {
            return Err(Mmc2Error::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(Mmc2Error::UnsupportedFourScreen);
        }
        let prg_bank_count_8k = windowed_bank_count(prg_len, PRG_8K)
            .filter(|n| (4..=32).contains(n))
            .ok_or(Mmc2Error::InvalidPrgLayout { prg_len })?;
        let chr_bank_count_4k = windowed_bank_count(chr_len, CHR_WINDOW_SIZE)
            .filter(|n| (4..=64).contains(n))
            .ok_or(Mmc2Error::InvalidChrLayout { chr_len })?;
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(Mmc2Error::HeaderPayloadMismatch);
        }
        Ok(Self {
            is_mmc4,
            prg_window_banks: if is_mmc4 { 2 } else { 1 },
            prg_bank_count_8k,
            chr_bank_count_4k,
            prg_bank: 0,
            chr: [0; 4],
            latch0: Latch::Fe,
            latch1: Latch::Fe,
            mirroring: 0,
            has_wram: header.has_battery || header.prg_ram_size > 0 || header.prg_nvram_size > 0,
        })
    }

    /// Whether an 8 KiB WRAM window is present at $6000-$7FFF (MMC4 saves).
    pub fn prg_ram_enabled(&self) -> bool {
        self.has_wram
    }

    pub fn mirroring(&self) -> NametableMirroring {
        if self.mirroring & 1 != 0 {
            NametableMirroring::Horizontal
        } else {
            NametableMirroring::Vertical
        }
    }

    pub fn write_register(&mut self, cpu_addr: u16, value: u8) {
        match cpu_addr & 0xF000 {
            0xA000 => self.prg_bank = value & 0x0F,
            0xB000 => self.chr[0] = value & 0x1F, // table0, $FD state
            0xC000 => self.chr[1] = value & 0x1F, // table0, $FE state
            0xD000 => self.chr[2] = value & 0x1F, // table1, $FD state
            0xE000 => self.chr[3] = value & 0x1F, // table1, $FE state
            0xF000 => self.mirroring = value & 1,
            _ => {}
        }
    }

    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        if cpu_addr < 0x8000 {
            return None;
        }
        let window = (cpu_addr as usize - 0x8000) / PRG_8K; // 0..3 in 8 KiB units
        let switch_banks = self.prg_window_banks;
        let bank8k = if window < switch_banks {
            // Switchable window at $8000. MMC4 selects a 16 KiB bank (two 8 KiB).
            let base = if self.is_mmc4 {
                (self.prg_bank as usize) * 2
            } else {
                self.prg_bank as usize
            };
            base + window
        } else {
            // Fixed tail: the last banks of ROM fill $8000+switch .. $FFFF.
            let from_top = 4 - window; // banks counted back from the end
            self.prg_bank_count_8k - from_top
        };
        let bank8k = bank8k % self.prg_bank_count_8k;
        Some(bank8k * PRG_8K + (cpu_addr as usize & 0x1FFF))
    }

    /// The active 4 KiB CHR bank for pattern table `table` (0 or 1), honoring
    /// the current latch state.
    pub fn chr_bank_4k(&self, table: u8) -> u8 {
        let idx = if table == 0 {
            match self.latch0 {
                Latch::Fd => 0,
                Latch::Fe => 1,
            }
        } else {
            match self.latch1 {
                Latch::Fd => 2,
                Latch::Fe => 3,
            }
        };
        self.chr[idx] % self.chr_bank_count_4k.max(1) as u8
    }

    /// Update the CHR latch as the PPU would when it fetches a pattern byte at
    /// `ppu_addr`. Fetching tile $FD/$FE from either table flips that table's
    /// latch. Exposed for callers that do model PPU fetches; the frame-granular
    /// oracle leaves the latch at its reset/last-set state.
    pub fn update_latch(&mut self, ppu_addr: u16) {
        match ppu_addr & 0x3FF8 {
            0x0FD8 => self.latch0 = Latch::Fd,
            0x0FE8 => self.latch0 = Latch::Fe,
            0x1FD8 => self.latch1 = Latch::Fd,
            0x1FE8 => self.latch1 = Latch::Fe,
            _ => {}
        }
    }

    /// Flat CHR offset for a PPU pattern address under the current latch.
    pub fn chr_offset(&self, ppu_addr: u16) -> usize {
        let table = (ppu_addr >> 12) & 1;
        self.chr_bank_4k(table as u8) as usize * CHR_WINDOW_SIZE + (ppu_addr as usize & 0x0FFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HeaderKind;

    fn header(mapper: u16, prg_16k: u16, chr_8k: u16) -> Header {
        Header {
            kind: HeaderKind::INes,
            prg_banks: prg_16k,
            chr_banks: chr_8k,
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper,
            submapper: 0,
            mirroring: Mirroring::Vertical,
            has_trainer: false,
            has_battery: false,
        }
    }

    #[test]
    fn mmc2_switches_8k_and_fixes_tail() {
        // Punch-Out: 128 KiB PRG (sixteen 8 KiB), 128 KiB CHR.
        let mut m = Mmc2::new(&header(9, 8, 16), 128 * 1024, 128 * 1024).unwrap();
        // Fixed tail: last three 8 KiB banks at $A000/$C000/$E000 = 13/14/15.
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(13 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(14 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xE000), Some(15 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xFFFF), Some(16 * PRG_8K - 1));
        // Switchable $8000 window.
        m.write_register(0xA000, 5);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(5 * PRG_8K));
    }

    #[test]
    fn mmc4_switches_16k_and_fixes_last_16k() {
        // Fire Emblem: 256 KiB PRG (sixteen 16 KiB), 128 KiB CHR.
        let mut m = Mmc2::new(&header(10, 16, 16), 256 * 1024, 128 * 1024).unwrap();
        let banks8k = 256 * 1024 / PRG_8K; // 32
        // Fixed last 16 KiB = 8 KiB banks 30 and 31 at $C000/$E000.
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some((banks8k - 2) * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xE000), Some((banks8k - 1) * PRG_8K));
        // Switchable 16 KiB window at $8000: bank 3 -> 8 KiB banks 6 and 7.
        m.write_register(0xA000, 3);
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(6 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(7 * PRG_8K));
    }

    #[test]
    fn chr_latch_selects_bank() {
        let mut m = Mmc2::new(&header(9, 8, 16), 128 * 1024, 128 * 1024).unwrap();
        m.write_register(0xB000, 4); // table0 $FD -> bank 4
        m.write_register(0xC000, 7); // table0 $FE -> bank 7
        // Power-on latch is $FE.
        assert_eq!(m.chr_bank_4k(0), 7);
        m.update_latch(0x0FD8); // PPU fetched tile $FD from table 0
        assert_eq!(m.chr_bank_4k(0), 4);
        m.update_latch(0x0FE8);
        assert_eq!(m.chr_bank_4k(0), 7);
    }

    #[test]
    fn wram_tracks_battery_and_ram_header_fields() {
        // MMC4 with a battery (Fire Emblem) exposes $6000 WRAM.
        let mut h = header(10, 16, 16);
        h.has_battery = true;
        assert!(
            Mmc2::new(&h, 256 * 1024, 128 * 1024)
                .unwrap()
                .prg_ram_enabled()
        );
        // MMC2 (Punch-Out) with no RAM fields has none.
        assert!(
            !Mmc2::new(&header(9, 8, 16), 128 * 1024, 128 * 1024)
                .unwrap()
                .prg_ram_enabled()
        );
        // A declared PRG-RAM size also counts.
        let mut h = header(9, 8, 16);
        h.prg_ram_size = 8 * 1024;
        assert!(
            Mmc2::new(&h, 128 * 1024, 128 * 1024)
                .unwrap()
                .prg_ram_enabled()
        );
    }

    #[test]
    fn chr_offset_follows_latch_and_table() {
        let mut m = Mmc2::new(&header(9, 8, 16), 128 * 1024, 128 * 1024).unwrap();
        m.write_register(0xC000, 7); // table0 $FE -> bank 7 (power-on latch)
        m.write_register(0xE000, 9); // table1 $FE -> bank 9
        // Table 0 pattern ($0000-$0FFF) and table 1 ($1000-$1FFF).
        assert_eq!(m.chr_offset(0x0000), 7 * CHR_WINDOW_SIZE);
        assert_eq!(m.chr_offset(0x1000), 9 * CHR_WINDOW_SIZE);
    }

    #[test]
    fn rejects_wrong_mapper() {
        assert!(matches!(
            Mmc2::new(&header(4, 8, 16), 128 * 1024, 128 * 1024),
            Err(Mmc2Error::UnsupportedMapper { mapper: 4 })
        ));
    }
}
