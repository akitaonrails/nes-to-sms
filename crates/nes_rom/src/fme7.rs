//! Sunsoft FME-7 / 5B (mapper 69) board decoding and register state.
//!
//! Separate from [`crate::MapperPolicy`]. FME-7 exposes a command/parameter
//! pair: a write to $8000-$9FFF picks one of sixteen internal registers, and
//! a write to $A000-$BFFF supplies its value. Commands $0-$7 bank the eight
//! 1 KiB CHR windows; $8-$B bank the four switchable 8 KiB PRG windows
//! ($6000, $8000, $A000, $C000) while $E000 stays fixed at the last bank;
//! $C sets mirroring; $D-$F run a 16 KiB-wide, cycle-decrementing IRQ counter.
//! The 5B audio extension (command register mirror at $C000/$E000) is not
//! modeled — Batman: Return of the Joker uses only the banking and IRQ.
//!
//! Register contract: <https://www.nesdev.org/wiki/Sunsoft_FME-7>.

use crate::{Header, Mirroring};
use std::fmt;

pub const PRG_WINDOW_SIZE: usize = 8 * 1024;
pub const CHR_WINDOW_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fme7Error {
    UnsupportedMapper { mapper: u16 },
    UnsupportedTrainer,
    UnsupportedFourScreen,
    InvalidPrgLayout { prg_len: usize },
    InvalidChrLayout { chr_len: usize },
    HeaderPayloadMismatch,
}

impl fmt::Display for Fme7Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => {
                write!(f, "FME-7 requires mapper 69, got {mapper}")
            }
            Self::UnsupportedTrainer => write!(f, "FME-7 trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => write!(f, "FME-7 four-screen wiring is unsupported"),
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "FME-7 requires power-of-two 32–512 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "FME-7 requires power-of-two 16–512 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "FME-7 header and payload lengths disagree"),
        }
    }
}

impl std::error::Error for Fme7Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fme7Mirroring {
    Vertical,
    Horizontal,
    OneScreenLower,
    OneScreenUpper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fme7 {
    prg_bank_count: u8,  // 8 KiB banks
    chr_bank_count: u16, // 1 KiB banks
    command: u8,
    chr: [u16; 8],
    // Switchable PRG windows: [0]=$6000, [1]=$8000, [2]=$A000, [3]=$C000.
    prg: [u8; 4],
    // Command $8 bit6 selects RAM (1) or ROM (0) at $6000; bit7 gates RAM only
    // (1 = RAM mapped, 0 = open bus). In ROM mode the window is always mapped.
    prg6000_ram: bool,
    prg6000_ram_enabled: bool,
    mirroring: u8,
    // IRQ: 16-bit counter decrements every CPU cycle while the counter is
    // enabled; wrapping past $0000 asserts IRQ when the IRQ enable bit is set.
    irq_counter_enabled: bool,
    irq_enabled: bool,
    irq_counter: u16,
    irq_pending: bool,
}

impl Fme7 {
    pub fn new(header: &Header, prg_len: usize, chr_len: usize) -> Result<Self, Fme7Error> {
        if header.mapper != 69 {
            return Err(Fme7Error::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.has_trainer {
            return Err(Fme7Error::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(Fme7Error::UnsupportedFourScreen);
        }
        if prg_len % PRG_WINDOW_SIZE != 0 || !prg_len.is_power_of_two() {
            return Err(Fme7Error::InvalidPrgLayout { prg_len });
        }
        let prg_bank_count = (prg_len / PRG_WINDOW_SIZE) as u8;
        if !(4..=64).contains(&prg_bank_count) {
            return Err(Fme7Error::InvalidPrgLayout { prg_len });
        }
        if chr_len % CHR_WINDOW_SIZE != 0 || !chr_len.is_power_of_two() {
            return Err(Fme7Error::InvalidChrLayout { chr_len });
        }
        let chr_bank_count = (chr_len / CHR_WINDOW_SIZE) as u16;
        if !(16..=512).contains(&chr_bank_count) {
            return Err(Fme7Error::InvalidChrLayout { chr_len });
        }
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(Fme7Error::HeaderPayloadMismatch);
        }
        let last = prg_bank_count - 1;
        Ok(Self {
            prg_bank_count,
            chr_bank_count,
            command: 0,
            chr: [0; 8],
            // Power-on: switchable windows walk the first banks; $E000 is fixed.
            prg: [0, 0, 1, last.saturating_sub(1)],
            prg6000_ram: false,
            prg6000_ram_enabled: false,
            mirroring: 0,
            irq_counter_enabled: false,
            irq_enabled: false,
            irq_counter: 0,
            irq_pending: false,
        })
    }

    pub fn mirroring(&self) -> Fme7Mirroring {
        match self.mirroring & 0x03 {
            0 => Fme7Mirroring::Vertical,
            1 => Fme7Mirroring::Horizontal,
            2 => Fme7Mirroring::OneScreenLower,
            _ => Fme7Mirroring::OneScreenUpper,
        }
    }

    /// A write to $8000-$9FFF selects the command; $A000-$BFFF supplies the
    /// parameter for the currently selected command.
    pub fn write_register(&mut self, cpu_addr: u16, value: u8) {
        match cpu_addr & 0xE000 {
            0x8000 => self.command = value & 0x0F,
            0xA000 => self.apply_parameter(value),
            _ => {}
        }
    }

    fn apply_parameter(&mut self, value: u8) {
        match self.command {
            0x0..=0x7 => self.chr[self.command as usize] = value as u16,
            0x8 => {
                self.prg6000_ram = value & 0x40 != 0;
                self.prg6000_ram_enabled = value & 0x80 != 0;
                self.prg[0] = value & 0x3F;
            }
            0x9 => self.prg[1] = value & 0x3F,
            0xA => self.prg[2] = value & 0x3F,
            0xB => self.prg[3] = value & 0x3F,
            0xC => self.mirroring = value & 0x03,
            0xD => {
                // IRQ control: bit7 counter-enable (C), bit0 IRQ-enable (T).
                self.irq_counter_enabled = value & 0x80 != 0;
                self.irq_enabled = value & 0x01 != 0;
                // Writing the control register acknowledges a pending IRQ.
                self.irq_pending = false;
            }
            0xE => self.irq_counter = (self.irq_counter & 0xFF00) | value as u16,
            _ => self.irq_counter = (self.irq_counter & 0x00FF) | ((value as u16) << 8),
        }
    }

    /// Whether the $6000-$7FFF window currently maps writable cartridge RAM.
    /// The caller supplies the RAM; ROM mode is served by `cpu_to_prg_offset`.
    pub fn prg6000_is_ram(&self) -> bool {
        self.prg6000_ram && self.prg6000_ram_enabled
    }

    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        let bank = match cpu_addr {
            0x6000..=0x7FFF => {
                if self.prg6000_ram {
                    // RAM (or open bus) — not a ROM offset.
                    return None;
                }
                // ROM mode: the window is always mapped.
                self.prg[0]
            }
            0x8000..=0x9FFF => self.prg[1],
            0xA000..=0xBFFF => self.prg[2],
            0xC000..=0xDFFF => self.prg[3],
            0xE000..=0xFFFF => self.prg_bank_count - 1,
            _ => return None,
        };
        let bank = (bank % self.prg_bank_count) as usize;
        Some(bank * PRG_WINDOW_SIZE + (cpu_addr as usize & 0x1FFF))
    }

    /// The selected 1 KiB CHR bank for pattern window 0..7.
    pub fn chr_bank_1k(&self, window: u8) -> u16 {
        self.chr[(window & 7) as usize] % self.chr_bank_count.max(1)
    }

    /// Advance the IRQ counter by one scanline (~113.67 CPU cycles) and report
    /// whether the IRQ line is asserted. The frame-granular oracle calls this
    /// 262 times per frame in place of the cycle-exact down-counter.
    pub fn irq_scanline(&mut self) -> bool {
        if self.irq_counter_enabled {
            // ~113.67 CPU cycles per scanline; the down-counter wraps past 0.
            let before = self.irq_counter;
            self.irq_counter = self.irq_counter.wrapping_sub(114);
            if self.irq_counter > before {
                // Wrapped past $0000.
                self.irq_pending = true;
            }
        }
        self.irq_asserted()
    }

    /// True while the mapper holds the CPU IRQ line low.
    pub fn irq_asserted(&self) -> bool {
        self.irq_enabled && self.irq_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeaderKind, PRG_BANK_SIZE};

    fn batman_header() -> Header {
        Header {
            kind: HeaderKind::INes,
            prg_banks: 8,  // 128 KiB
            chr_banks: 32, // 256 KiB
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper: 69,
            submapper: 0,
            mirroring: Mirroring::Vertical,
            has_trainer: false,
            has_battery: false,
        }
    }

    fn batman() -> Fme7 {
        Fme7::new(&batman_header(), 8 * PRG_BANK_SIZE, 256 * 1024).unwrap()
    }

    #[test]
    fn command_parameter_banks_prg() {
        let mut f = batman();
        // 128 KiB PRG = sixteen 8 KiB banks; $E000 fixed at bank 15.
        assert_eq!(f.cpu_to_prg_offset(0xE000), Some(15 * PRG_WINDOW_SIZE));
        assert_eq!(f.cpu_to_prg_offset(0xFFFF), Some(16 * PRG_WINDOW_SIZE - 1));
        // Command $9 sets the $8000 window to bank 5.
        f.write_register(0x8000, 0x09);
        f.write_register(0xA000, 5);
        assert_eq!(f.cpu_to_prg_offset(0x8000), Some(5 * PRG_WINDOW_SIZE));
        // Command $B sets the $C000 window to bank 12.
        f.write_register(0x8000, 0x0B);
        f.write_register(0xA000, 12);
        assert_eq!(f.cpu_to_prg_offset(0xC000), Some(12 * PRG_WINDOW_SIZE));
    }

    #[test]
    fn prg6000_rom_vs_ram_modes() {
        let mut f = batman();
        // ROM mode by default (bit6=0): the window is always mapped, bank 0.
        assert_eq!(f.cpu_to_prg_offset(0x6000), Some(0));
        assert!(!f.prg6000_is_ram());
        // ROM bank 3 (bit6=0); bit7 is ignored in ROM mode.
        f.write_register(0x8000, 0x08);
        f.write_register(0xA000, 3);
        assert_eq!(f.cpu_to_prg_offset(0x6000), Some(3 * PRG_WINDOW_SIZE));
        assert!(!f.prg6000_is_ram());
        // RAM enabled (bit6=1, bit7=1): the WRAM path handles it, not a ROM off.
        f.write_register(0xA000, 0xC0);
        assert_eq!(f.cpu_to_prg_offset(0x6000), None);
        assert!(f.prg6000_is_ram());
        // RAM selected but not enabled (bit6=1, bit7=0): open bus, no RAM.
        f.write_register(0xA000, 0x40);
        assert_eq!(f.cpu_to_prg_offset(0x6000), None);
        assert!(!f.prg6000_is_ram());
    }

    #[test]
    fn chr_banks_and_mirroring() {
        let mut f = batman();
        f.write_register(0x8000, 0x02); // command CHR window 2
        f.write_register(0xA000, 0x11);
        assert_eq!(f.chr_bank_1k(2), 0x11);
        f.write_register(0x8000, 0x0C); // mirroring command
        f.write_register(0xA000, 0x01);
        assert_eq!(f.mirroring(), Fme7Mirroring::Horizontal);
    }

    #[test]
    fn irq_counter_wraps_and_fires() {
        let mut f = batman();
        // Load counter with 200, enable counter + IRQ.
        f.write_register(0x8000, 0x0E);
        f.write_register(0xA000, 200); // low = 200
        f.write_register(0x8000, 0x0F);
        f.write_register(0xA000, 0); // high = 0 -> counter 200
        f.write_register(0x8000, 0x0D);
        f.write_register(0xA000, 0x81); // counter-enable + IRQ-enable
        // 200 cycles / ~114 per scanline -> wraps within two scanline ticks.
        assert!(!f.irq_asserted());
        f.irq_scanline();
        let fired = f.irq_scanline();
        assert!(fired, "counter should wrap past zero and assert IRQ");
        // Writing the control register acknowledges.
        f.write_register(0x8000, 0x0D);
        f.write_register(0xA000, 0x80);
        assert!(!f.irq_asserted());
    }

    #[test]
    fn rejects_wrong_mapper() {
        let mut h = batman_header();
        h.mapper = 4;
        assert!(matches!(
            Fme7::new(&h, 8 * PRG_BANK_SIZE, 256 * 1024),
            Err(Fme7Error::UnsupportedMapper { mapper: 4 })
        ));
    }
}
