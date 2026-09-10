//! Nintendo MMC5 / ExROM (mapper 5) board decoding — focused subset.
//!
//! Separate from [`crate::MapperPolicy`]. MMC5 is the most configurable NES
//! mapper; this models the parts a game needs to boot and render: the four PRG
//! modes ($5100 + $5113-$5117, each PRG register carrying a ROM/RAM select),
//! the four CHR modes ($5101 + the sprite bank set $5120-$5127 and the
//! background set $5128-$512B), WRAM banking at $6000, nametable/fill control
//! (stored), and the scanline IRQ ($5203/$5204). Not modeled yet: ExRAM as a
//! nametable/attribute source, the vertical split window, the multiplier, and
//! the expansion audio — none are needed to bring Castlevania III up.
//!
//! Register contract: <https://www.nesdev.org/wiki/MMC5>.

use crate::{Header, Mirroring};
use std::fmt;

const PRG_8K: usize = 8 * 1024;
pub const CHR_1K: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc5Error {
    UnsupportedMapper { mapper: u16 },
    UnsupportedTrainer,
    InvalidPrgLayout { prg_len: usize },
    InvalidChrLayout { chr_len: usize },
    HeaderPayloadMismatch,
}

impl fmt::Display for Mmc5Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => write!(f, "MMC5 requires mapper 5, got {mapper}"),
            Self::UnsupportedTrainer => write!(f, "MMC5 trainer initialization is unsupported"),
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "MMC5 requires power-of-two 32–1024 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "MMC5 requires power-of-two 8–1024 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "MMC5 header and payload lengths disagree"),
        }
    }
}

impl std::error::Error for Mmc5Error {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mmc5 {
    prg_bank_count_8k: usize,
    chr_bank_count_1k: usize,
    prg_mode: u8,
    chr_mode: u8,
    // $5113 WRAM bank; $5114-$5117 PRG banks (bit7 = ROM select on 14..16).
    wram_bank: u8,
    prg: [u8; 4],
    // CHR: sprite set $5120-$5127 (8), background set $5128-$512B (4).
    chr_sprite: [u8; 8],
    chr_bg: [u8; 4],
    // Which CHR set the PPU should use for background fetches. MMC5 switches to
    // the background set during 8x16 sprite rendering; approximated by "last
    // written set" for the frame-granular oracle.
    last_chr_bg: bool,
    nametable_ctrl: u8,
    // Scanline IRQ.
    irq_scanline_cmp: u8,
    irq_enabled: bool,
    irq_pending: bool,
    scanline: u16,
    has_wram: bool,
}

impl Mmc5 {
    pub fn new(header: &Header, prg_len: usize, chr_len: usize) -> Result<Self, Mmc5Error> {
        if header.mapper != 5 {
            return Err(Mmc5Error::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.has_trainer {
            return Err(Mmc5Error::UnsupportedTrainer);
        }
        if prg_len % PRG_8K != 0 || !prg_len.is_power_of_two() {
            return Err(Mmc5Error::InvalidPrgLayout { prg_len });
        }
        let prg_bank_count_8k = prg_len / PRG_8K;
        if !(4..=128).contains(&prg_bank_count_8k) {
            return Err(Mmc5Error::InvalidPrgLayout { prg_len });
        }
        if chr_len % CHR_1K != 0 || !chr_len.is_power_of_two() {
            return Err(Mmc5Error::InvalidChrLayout { chr_len });
        }
        let chr_bank_count_1k = chr_len / CHR_1K;
        if !(8..=1024).contains(&chr_bank_count_1k) {
            return Err(Mmc5Error::InvalidChrLayout { chr_len });
        }
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(Mmc5Error::HeaderPayloadMismatch);
        }
        let last = (prg_bank_count_8k - 1) as u8;
        Ok(Self {
            prg_bank_count_8k,
            chr_bank_count_1k,
            // Power-on: PRG mode 3 (all 8 KiB) with the last bank fixed at
            // $E000, which is where the reset vector must resolve.
            prg_mode: 3,
            chr_mode: 0,
            wram_bank: 0,
            prg: [0xFF, 0xFF, 0xFF, 0x80 | last],
            chr_sprite: [0; 8],
            chr_bg: [0; 4],
            last_chr_bg: false,
            nametable_ctrl: 0,
            irq_scanline_cmp: 0,
            irq_enabled: false,
            irq_pending: false,
            scanline: 0,
            has_wram: header.has_battery
                || header.prg_ram_size > 0
                || header.prg_nvram_size > 0
                || true, // MMC5 boards ship WRAM; games rely on $6000 always mapping it.
        })
    }

    pub fn mirroring(&self) -> Mirroring {
        // $5105 is a per-quadrant nametable map, not a simple mirroring mode.
        // The oracle renderer resolves nametables itself, so report the two
        // common wirings: $44 => vertical, $50 => horizontal, else vertical.
        match self.nametable_ctrl {
            0x50 => Mirroring::Horizontal,
            _ => Mirroring::Vertical,
        }
    }

    pub fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            0x5100 => self.prg_mode = value & 0x03,
            0x5101 => self.chr_mode = value & 0x03,
            0x5105 => self.nametable_ctrl = value,
            0x5113 => self.wram_bank = value & 0x0F,
            0x5114..=0x5117 => self.prg[(addr - 0x5114) as usize] = value,
            0x5120..=0x5127 => {
                self.chr_sprite[(addr - 0x5120) as usize] = value;
                self.last_chr_bg = false;
            }
            0x5128..=0x512B => {
                self.chr_bg[(addr - 0x5128) as usize] = value;
                self.last_chr_bg = true;
            }
            0x5203 => self.irq_scanline_cmp = value,
            0x5204 => self.irq_enabled = value & 0x80 != 0,
            _ => {}
        }
    }

    /// MMC5 exposes status at $5204 (IRQ pending / in-frame). Reading it clears
    /// the pending flag.
    pub fn read_status(&mut self, addr: u16) -> Option<u8> {
        if addr == 0x5204 {
            let v = if self.irq_pending { 0x80 } else { 0x00 };
            self.irq_pending = false;
            Some(v)
        } else {
            None
        }
    }

    pub fn wram_bank(&self) -> u8 {
        self.wram_bank
    }

    pub fn prg_ram_enabled(&self) -> bool {
        self.has_wram
    }

    /// Map a CPU address to a flat PRG-ROM offset, or None when the window is
    /// RAM (the $6000 WRAM window, or an $8000-$DFFF window whose register
    /// selects RAM via bit 7 cleared).
    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        if cpu_addr < 0x8000 {
            return None;
        }
        // Resolve (bank_8k, is_rom) for the window containing cpu_addr.
        let (reg_val, is_rom): (u8, bool) = match self.prg_mode {
            0 => {
                // 32 KiB: $5117 selects, bank aligned to 4x8 KiB.
                let b = (self.prg[3] & 0x7F) & !0x03;
                (b + ((cpu_addr - 0x8000) / 0x2000 as u16) as u8, true)
            }
            1 => {
                // Two 16 KiB windows: $5115 ($8000-$BFFF), $5117 ($C000-$FFFF).
                if cpu_addr < 0xC000 {
                    let b = (self.prg[1] & 0x7E) + ((cpu_addr - 0x8000) / 0x2000) as u8;
                    (b, self.prg[1] & 0x80 != 0)
                } else {
                    let b = (self.prg[3] & 0x7E) + ((cpu_addr - 0xC000) / 0x2000) as u8;
                    (b, true)
                }
            }
            2 => {
                // 16 KiB ($5115) + 8 KiB ($5116) + 8 KiB ($5117).
                if cpu_addr < 0xC000 {
                    let b = (self.prg[1] & 0x7E) + ((cpu_addr - 0x8000) / 0x2000) as u8;
                    (b, self.prg[1] & 0x80 != 0)
                } else if cpu_addr < 0xE000 {
                    (self.prg[2] & 0x7F, self.prg[2] & 0x80 != 0)
                } else {
                    (self.prg[3] & 0x7F, true)
                }
            }
            _ => {
                // 8 KiB windows: $5114/$5115/$5116/$5117.
                let i = ((cpu_addr - 0x8000) / 0x2000) as usize; // 0..3
                let is_rom = i == 3 || self.prg[i] & 0x80 != 0;
                (self.prg[i] & 0x7F, is_rom)
            }
        };
        if !is_rom {
            return None; // PRG-RAM window; the WRAM path handles it.
        }
        let bank = (reg_val as usize) % self.prg_bank_count_8k;
        Some(bank * PRG_8K + (cpu_addr as usize & 0x1FFF))
    }

    /// The selected 1 KiB CHR bank for background pattern window 0..7.
    pub fn chr_bank_1k(&self, window: u8) -> u16 {
        let w = (window & 7) as usize;
        // Background fetches use the BG set in 8x16 mode; approximate by the
        // last-written set. The BG set has four registers ($5128-$512B) that
        // mirror to fill 0..7.
        let raw = if self.last_chr_bg {
            self.bg_bank_for_window(w)
        } else {
            self.sprite_bank_for_window(w)
        };
        raw % self.chr_bank_count_1k.max(1) as u16
    }

    fn sprite_bank_for_window(&self, w: usize) -> u16 {
        match self.chr_mode {
            0 => self.chr_sprite[7] as u16 * 8 + w as u16,
            1 => {
                let reg = if w < 4 { 3 } else { 7 };
                self.chr_sprite[reg] as u16 * 4 + (w & 3) as u16
            }
            2 => {
                let reg = (w & !1) | 1; // 1,3,5,7
                self.chr_sprite[reg] as u16 * 2 + (w & 1) as u16
            }
            _ => self.chr_sprite[w] as u16,
        }
    }

    fn bg_bank_for_window(&self, w: usize) -> u16 {
        // The BG set only has four registers; they tile across the 8 windows.
        match self.chr_mode {
            0 => self.chr_bg[3] as u16 * 8 + w as u16,
            1 => self.chr_bg[3] as u16 * 4 + (w & 3) as u16,
            2 => {
                let reg = ((w & !1) | 1) & 3;
                self.chr_bg[reg] as u16 * 2 + (w & 1) as u16
            }
            _ => self.chr_bg[w & 3] as u16,
        }
    }

    pub fn chr_offset(&self, ppu_addr: u16) -> usize {
        let window = ((ppu_addr >> 10) & 7) as u8;
        self.chr_bank_1k(window) as usize * CHR_1K + (ppu_addr as usize & 0x3FF)
    }

    /// Advance one scanline for the IRQ compare; returns whether IRQ is asserted.
    /// The oracle calls this 262 times per frame in place of PPU scanline timing.
    pub fn irq_scanline(&mut self) -> bool {
        self.scanline = self.scanline.wrapping_add(1);
        if self.scanline >= 262 {
            self.scanline = 0;
        }
        if self.scanline as u8 == self.irq_scanline_cmp && self.irq_scanline_cmp != 0 {
            self.irq_pending = true;
        }
        self.irq_enabled && self.irq_pending
    }

    pub fn irq_asserted(&self) -> bool {
        self.irq_enabled && self.irq_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HeaderKind;

    fn cv3_header() -> Header {
        Header {
            kind: HeaderKind::INes,
            prg_banks: 16, // 256 KiB
            chr_banks: 16, // 128 KiB
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper: 5,
            submapper: 0,
            mirroring: Mirroring::Vertical,
            has_trainer: false,
            has_battery: false,
        }
    }

    fn cv3() -> Mmc5 {
        Mmc5::new(&cv3_header(), 256 * 1024, 128 * 1024).unwrap()
    }

    #[test]
    fn reset_maps_last_bank_at_e000() {
        let m = cv3();
        // 256 KiB = 32 8-KiB banks; the reset vector window $E000 must map the
        // last bank at power-on (PRG mode 3, $5117 = last | ROM).
        assert_eq!(m.cpu_to_prg_offset(0xE000), Some(31 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xFFFF), Some(32 * PRG_8K - 1));
    }

    #[test]
    fn prg_mode3_switches_8k_windows() {
        let mut m = cv3();
        m.write_register(0x5100, 3); // 8 KiB mode
        m.write_register(0x5114, 0x80 | 5); // $8000 = ROM bank 5
        m.write_register(0x5115, 0x80 | 9); // $A000 = ROM bank 9
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(5 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(9 * PRG_8K));
        // Bit 7 clear -> RAM window (None; WRAM path handles it).
        m.write_register(0x5114, 5);
        assert_eq!(m.cpu_to_prg_offset(0x8000), None);
    }

    #[test]
    fn prg_mode1_16k_window() {
        let mut m = cv3();
        m.write_register(0x5100, 1); // 16 KiB mode
        m.write_register(0x5115, 0x80 | 4); // $8000-$BFFF = 16 KiB bank (8k 4/5)
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(4 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(5 * PRG_8K));
    }

    #[test]
    fn chr_mode3_1k_banks() {
        let mut m = cv3();
        m.write_register(0x5101, 3); // 1 KiB mode
        m.write_register(0x5120, 2);
        m.write_register(0x5121, 3);
        assert_eq!(m.chr_bank_1k(0), 2);
        assert_eq!(m.chr_bank_1k(1), 3);
    }

    #[test]
    fn scanline_irq_fires_at_compare() {
        let mut m = cv3();
        m.write_register(0x5203, 4); // compare scanline 4
        m.write_register(0x5204, 0x80); // enable
        for _ in 0..3 {
            assert!(!m.irq_scanline());
        }
        assert!(m.irq_scanline(), "fires at scanline 4");
        // Reading $5204 clears pending.
        assert_eq!(m.read_status(0x5204), Some(0x80));
        assert!(!m.irq_asserted());
    }

    #[test]
    fn prg_mode0_32k_bank() {
        let mut m = cv3();
        m.write_register(0x5100, 0); // 32 KiB mode
        m.write_register(0x5117, 0x80 | 8); // aligned to 8 KiB banks 8..11
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(8 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(9 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(10 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xE000), Some(11 * PRG_8K));
    }

    #[test]
    fn prg_mode2_16k_plus_two_8k() {
        let mut m = cv3();
        m.write_register(0x5100, 2); // 16 KiB ($8000) + 8 KiB ($C000) + 8 KiB ($E000)
        m.write_register(0x5115, 0x80 | 4); // 16 KiB window -> 8 KiB banks 4/5
        m.write_register(0x5116, 0x80 | 20); // $C000 8 KiB bank 20
        m.write_register(0x5117, 0x80 | 31); // $E000 8 KiB bank 31
        assert_eq!(m.cpu_to_prg_offset(0x8000), Some(4 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xA000), Some(5 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xC000), Some(20 * PRG_8K));
        assert_eq!(m.cpu_to_prg_offset(0xE000), Some(31 * PRG_8K));
    }

    #[test]
    fn chr_mode0_8k_sprite_set() {
        let mut m = cv3();
        m.write_register(0x5101, 0); // 8 KiB CHR mode
        m.write_register(0x5127, 2); // sprite-set 8 KiB bank 2 -> 1 KiB banks 16..23
        assert_eq!(m.chr_bank_1k(0), 16);
        assert_eq!(m.chr_bank_1k(7), 23);
    }

    #[test]
    fn chr_background_set_takes_over_after_bg_write() {
        let mut m = cv3();
        m.write_register(0x5101, 3); // 1 KiB mode
        m.write_register(0x5120, 4); // sprite set window 0
        assert_eq!(m.chr_bank_1k(0), 4);
        // A write to the background set ($5128-$512B) switches the source.
        m.write_register(0x5128, 9); // bg set window 0
        assert_eq!(m.chr_bank_1k(0), 9);
    }

    #[test]
    fn rejects_wrong_mapper() {
        let mut h = cv3_header();
        h.mapper = 4;
        assert!(matches!(
            Mmc5::new(&h, 256 * 1024, 128 * 1024),
            Err(Mmc5Error::UnsupportedMapper { mapper: 4 })
        ));
    }
}
