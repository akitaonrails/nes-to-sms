//! Konami VRC2/VRC4 (mappers 22/23/25) board decoding and register state.
//!
//! Separate from [`crate::MapperPolicy`]. This models the banking common to
//! VRC2 and VRC4: two switchable 8 KiB PRG windows plus two fixed, eight
//! 1 KiB CHR windows loaded as low/high nibbles, and mirroring. The three
//! mapper numbers differ only in which CPU address lines carry the register's
//! low two selector bits; we OR the candidate lines (A0|A2, A1|A3) with a
//! per-mapper mask so one decoder serves 22/23/25 (the standard robust
//! approach). The VRC4 IRQ counter is not modeled yet (VRC2 games such as
//! Contra do not use it; VRC4 games that need it get it when queued).
//!
//! Register contract: <https://www.nesdev.org/wiki/VRC2_and_VRC4>.

use crate::{Header, Mirroring, NametableMirroring, windowed_bank_count};
use std::fmt;

pub const PRG_WINDOW_SIZE: usize = 8 * 1024;
pub const CHR_WINDOW_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vrc2Error {
    UnsupportedMapper { mapper: u16 },
    UnsupportedTrainer,
    UnsupportedFourScreen,
    InvalidPrgLayout { prg_len: usize },
    InvalidChrLayout { chr_len: usize },
    HeaderPayloadMismatch,
}

impl fmt::Display for Vrc2Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => {
                write!(f, "VRC2/4 requires mapper 22/23/25, got {mapper}")
            }
            Self::UnsupportedTrainer => write!(f, "VRC2/4 trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => write!(f, "VRC2/4 four-screen wiring is unsupported"),
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "VRC2/4 requires power-of-two 32–256 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "VRC2/4 requires power-of-two 16–512 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "VRC2/4 header and payload lengths disagree"),
        }
    }
}

impl std::error::Error for Vrc2Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vrc2 {
    prg_bank_count: u8,  // 8 KiB banks
    chr_bank_count: u16, // 1 KiB banks
    // The two selector-bit sources differ per mapper: 22 swaps A0/A1, 23 uses
    // A0/A1 direct, 25 uses A1/A0 (also via A2/A3 on VRC4). We normalize by
    // OR-ing the two candidate line pairs, matched by the common mask below.
    swap_lines: bool, // mapper 22/25: low selector bit from A1, high from A0
    prg_swap_mode: bool,
    prg0: u8, // $8000 switchable
    prg1: u8, // $A000 switchable
    chr: [u16; 8],
    mirroring: u8,
    // VRC2 boards carry a single bit of RAM ("microwire") at $6000-$6FFF
    // instead of 8 KiB WRAM. Games (e.g. Contra) write a bit and read it back
    // to detect the board; a wrong answer trips their self-test. VRC4 variants
    // that ship true 8 KiB WRAM are not modeled here yet.
    ram_latch: u8,
    // VRC4 scanline/cycle IRQ counter ($F000-$F003). VRC2 games leave it
    // disabled; VRC4 games (Gradius II, Goemon) drive their split-screen and
    // main-loop pacing off it, so it must run for them to progress.
    irq_latch: u8,
    irq_counter: u8,
    irq_enabled: bool,
    irq_enable_after_ack: bool,
    irq_cycle_mode: bool,
    irq_pending: bool,
    // Scanline mode divides the pixel clock by 341 dots (~113.667 CPU cycles);
    // this prescaler carries the fractional remainder between ticks.
    irq_prescaler: i16,
}

impl Vrc2 {
    pub fn new(header: &Header, prg_len: usize, chr_len: usize) -> Result<Self, Vrc2Error> {
        let swap_lines = match header.mapper {
            23 => false,
            22 | 25 => true,
            mapper => return Err(Vrc2Error::UnsupportedMapper { mapper }),
        };
        if header.has_trainer {
            return Err(Vrc2Error::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(Vrc2Error::UnsupportedFourScreen);
        }
        let prg_bank_count = windowed_bank_count(prg_len, PRG_WINDOW_SIZE)
            .filter(|n| (4..=32).contains(n))
            .ok_or(Vrc2Error::InvalidPrgLayout { prg_len })? as u8;
        let chr_bank_count = windowed_bank_count(chr_len, CHR_WINDOW_SIZE)
            .filter(|n| (16..=512).contains(n))
            .ok_or(Vrc2Error::InvalidChrLayout { chr_len })? as u16;
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(Vrc2Error::HeaderPayloadMismatch);
        }
        Ok(Self {
            prg_bank_count,
            chr_bank_count,
            swap_lines,
            prg_swap_mode: false,
            prg0: 0,
            prg1: 1,
            chr: [0; 8],
            mirroring: 0,
            ram_latch: 0,
            irq_latch: 0,
            irq_counter: 0,
            irq_enabled: false,
            irq_enable_after_ack: false,
            irq_cycle_mode: false,
            irq_pending: false,
            irq_prescaler: 0,
        })
    }

    /// VRC2 microwire: a CPU write to $6000-$6FFF stores bit 0.
    pub fn microwire_write(&mut self, value: u8) {
        self.ram_latch = value & 1;
    }

    /// VRC2 microwire read: the stored bit surfaces in D0 (the upper bits are
    /// open bus on hardware; callers that matter mask to D0).
    pub fn microwire_read(&self) -> u8 {
        self.ram_latch & 1
    }

    pub fn mirroring(&self) -> NametableMirroring {
        match self.mirroring & 0x03 {
            0 => NametableMirroring::Vertical,
            1 => NametableMirroring::Horizontal,
            2 => NametableMirroring::OneScreenLower,
            _ => NametableMirroring::OneScreenUpper,
        }
    }

    /// The 2-bit sub-register within a $x000 block. OR the candidate address
    /// lines so one decoder serves all three mapper numbers; `swap_lines`
    /// exchanges the two selector bits for mappers 22/25.
    fn reg_select(&self, addr: u16) -> u8 {
        let a = ((addr & 1) | ((addr >> 2) & 1)) as u8; // A0 | A2
        let b = (((addr >> 1) & 1) | ((addr >> 3) & 1)) as u8; // A1 | A3
        if self.swap_lines {
            (b) | (a << 1)
        } else {
            (a) | (b << 1)
        }
    }

    pub fn write_register(&mut self, cpu_addr: u16, value: u8) {
        let sel = self.reg_select(cpu_addr);
        match cpu_addr & 0xF000 {
            0x8000 => self.prg0 = value & 0x1f,
            0xA000 => self.prg1 = value & 0x1f,
            0x9000 => {
                // sel 0/1: mirroring; sel 2/3: PRG swap mode (VRC4).
                if sel < 2 {
                    self.mirroring = value & 0x03;
                } else {
                    self.prg_swap_mode = value & 0x02 != 0;
                }
            }
            0xB000 | 0xC000 | 0xD000 | 0xE000 => {
                // Each $x000 block holds two CHR banks; sel picks bank and
                // low/high nibble. block index 0..3 -> CHR pair (2*idx).
                let idx = ((cpu_addr - 0xB000) >> 12) as usize; // 0..3
                let bank = idx * 2 + (sel as usize >> 1); // sel 0/1 -> low pair, 2/3 -> high pair
                let entry = &mut self.chr[bank];
                if sel & 1 == 0 {
                    *entry = (*entry & 0x1f0) | (value as u16 & 0x0f);
                } else {
                    *entry = (*entry & 0x00f) | ((value as u16 & 0x1f) << 4);
                }
            }
            0xF000 => match sel {
                0 => self.irq_latch = (self.irq_latch & 0xF0) | (value & 0x0F),
                1 => self.irq_latch = (self.irq_latch & 0x0F) | ((value & 0x0F) << 4),
                2 => {
                    // Control: bit0 = enable-after-ack (A), bit1 = enable (E),
                    // bit2 = mode (M: 0 scanline, 1 cycle).
                    self.irq_enable_after_ack = value & 0x01 != 0;
                    self.irq_enabled = value & 0x02 != 0;
                    self.irq_cycle_mode = value & 0x04 != 0;
                    self.irq_pending = false;
                    if self.irq_enabled {
                        self.irq_counter = self.irq_latch;
                        self.irq_prescaler = 0;
                    }
                }
                _ => {
                    // Acknowledge: clear pending, restore E from A.
                    self.irq_pending = false;
                    self.irq_enabled = self.irq_enable_after_ack;
                }
            },
            _ => {}
        }
    }

    /// Advance the VRC4 IRQ counter by one scanline and report whether the IRQ
    /// line is (still) asserted. The frame-granular oracle calls this 262 times
    /// per frame in place of cycle-exact timing. Scanline mode ticks the
    /// counter once per scanline; cycle mode ticks per CPU cycle, ~113.67 per
    /// scanline, so the prescaler carries the fraction across calls.
    pub fn vrc_irq_scanline(&mut self) -> bool {
        if self.irq_enabled {
            if self.irq_cycle_mode {
                // 341 dots / 3 dots-per-cycle = 113.67 CPU cycles per scanline.
                self.irq_prescaler += 341;
                while self.irq_prescaler >= 3 {
                    self.irq_prescaler -= 3;
                    self.irq_step_counter();
                }
            } else {
                self.irq_step_counter();
            }
        }
        self.irq_asserted()
    }

    fn irq_step_counter(&mut self) {
        if self.irq_counter == 0xFF {
            self.irq_counter = self.irq_latch;
            self.irq_pending = true;
        } else {
            self.irq_counter += 1;
        }
    }

    /// True while the mapper is holding the CPU IRQ line low.
    pub fn irq_asserted(&self) -> bool {
        self.irq_enabled && self.irq_pending
    }

    pub fn cpu_to_prg_offset(&self, cpu_addr: u16) -> Option<usize> {
        if cpu_addr < 0x8000 {
            return None;
        }
        let last = self.prg_bank_count - 1;
        let bank = match cpu_addr & 0xE000 {
            // In swap mode the fixed/switchable ends exchange for $8000/$C000.
            0x8000 => {
                if self.prg_swap_mode {
                    (last - 1) % self.prg_bank_count
                } else {
                    self.prg0 % self.prg_bank_count
                }
            }
            0xA000 => self.prg1 % self.prg_bank_count,
            0xC000 => {
                if self.prg_swap_mode {
                    self.prg0 % self.prg_bank_count
                } else {
                    (last - 1) % self.prg_bank_count
                }
            }
            _ => last, // $E000 always the last 8 KiB bank
        };
        Some(bank as usize * PRG_WINDOW_SIZE + (cpu_addr as usize & 0x1FFF))
    }

    /// The selected 1 KiB CHR bank for pattern window 0..7.
    pub fn chr_bank_1k(&self, window: u8) -> u16 {
        self.chr[(window & 7) as usize] % self.chr_bank_count.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HeaderKind, PRG_BANK_SIZE};

    fn contra_header() -> Header {
        Header {
            kind: HeaderKind::INes,
            prg_banks: 8,  // 128 KiB
            chr_banks: 16, // 128 KiB
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper: 23,
            submapper: 0,
            mirroring: Mirroring::Vertical,
            has_trainer: false,
            has_battery: false,
        }
    }

    fn gradius_header_mapper25() -> Header {
        let mut h = contra_header();
        h.mapper = 25; // VRC4b/e: A0/A1 selector lines swapped
        h
    }

    #[test]
    fn prg_swap_mode_exchanges_8000_and_c000() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        v.write_register(0x8000, 5); // prg0 = bank 5
        // Default (swap mode off): $8000 switchable, $C000 fixed second-to-last.
        assert_eq!(v.cpu_to_prg_offset(0x8000), Some(5 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xC000), Some(14 * PRG_WINDOW_SIZE));
        // $9002 = sel 2 on mapper 23 -> PRG swap mode (bit1 set).
        v.write_register(0x9002, 0x02);
        // Now the ends exchange: $8000 fixed second-to-last, $C000 switchable.
        assert_eq!(v.cpu_to_prg_offset(0x8000), Some(14 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xC000), Some(5 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xE000), Some(15 * PRG_WINDOW_SIZE));
    }

    #[test]
    fn mapper25_swaps_selector_lines_for_irq_registers() {
        // Gradius II (mapper 25) reaches IRQ control at $F001 and the latch
        // high nibble at $F002 — the A0/A1 swap. Latch $F6 fires after 10 ticks.
        let mut v =
            Vrc2::new(&gradius_header_mapper25(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        v.write_register(0xF000, 0x06); // sel 0: latch low nibble = 6
        v.write_register(0xF002, 0x0F); // sel 1 (swapped): latch high -> $F6
        v.write_register(0xF001, 0x02); // sel 2 (swapped): enable, scanline mode
        for _ in 0..9 {
            assert!(!v.vrc_irq_scanline());
        }
        assert!(
            v.vrc_irq_scanline(),
            "mapper-25 IRQ fires after 10 scanlines"
        );
    }

    #[test]
    fn cycle_mode_irq_counts_faster_than_scanline() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        v.write_register(0xF000, 0x00);
        v.write_register(0xF001, 0x0F); // latch $F0 -> 16 counts to overflow
        v.write_register(0xF002, 0x06); // control: enable (bit1) + cycle mode (bit2)
        // Cycle mode advances ~114 counts per scanline call, so 16 counts to the
        // overflow happen within the first scanline tick.
        assert!(
            v.vrc_irq_scanline(),
            "cycle mode overflows within one scanline"
        );
    }

    #[test]
    fn contra_board_parses_and_banks_prg() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        // 128 KiB PRG = sixteen 8 KiB banks. Power-on: $C000/$E000 fixed at the
        // last two banks (14, 15).
        assert_eq!(v.cpu_to_prg_offset(0xC000), Some(14 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xE000), Some(15 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xFFFF), Some(16 * PRG_WINDOW_SIZE - 1));
        assert_eq!(v.cpu_to_prg_offset(0x7FFF), None);
        // Select $8000 bank 5, $A000 bank 9.
        v.write_register(0x8000, 5);
        v.write_register(0xA000, 9);
        assert_eq!(v.cpu_to_prg_offset(0x8000), Some(5 * PRG_WINDOW_SIZE));
        assert_eq!(v.cpu_to_prg_offset(0xA000), Some(9 * PRG_WINDOW_SIZE));
    }

    #[test]
    fn chr_low_high_nibbles_and_mirroring() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        // CHR bank 0 low nibble at $B000 sel0, high nibble at $B000 sel1.
        v.write_register(0xB000, 0x0A); // low nibble = A
        v.write_register(0xB001, 0x01); // high nibble = 1 -> bank 0x1A
        assert_eq!(v.chr_bank_1k(0), 0x1A % (128));
        // Mirroring via $9000 sel0.
        v.write_register(0x9000, 1);
        assert_eq!(v.mirroring(), NametableMirroring::Horizontal);
    }

    #[test]
    fn microwire_latch_roundtrips_bit0() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        v.microwire_write(1);
        assert_eq!(v.microwire_read(), 1);
        v.microwire_write(0);
        assert_eq!(v.microwire_read(), 0);
        // Only bit 0 is stored.
        v.microwire_write(0xFE);
        assert_eq!(v.microwire_read(), 0);
        v.microwire_write(0xFF);
        assert_eq!(v.microwire_read(), 1);
    }

    #[test]
    fn scanline_irq_fires_after_reload_period() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        // Latch $F6 -> counter reloads at $F6, so it overflows after
        // 256 - 0xF6 = 10 scanline ticks. Write low then high nibble.
        v.write_register(0xF000, 0x06); // low nibble
        v.write_register(0xF001, 0x0F); // high nibble -> latch $F6
        // Control: enable (E=1, bit1), scanline mode (M=0). sel 2 on mapper 23.
        v.write_register(0xF002, 0x02);
        for _ in 0..9 {
            assert!(!v.vrc_irq_scanline(), "should not fire before 10 ticks");
        }
        assert!(v.vrc_irq_scanline(), "fires on the 10th tick");
        assert!(v.irq_asserted());
        // Acknowledge clears the line.
        v.write_register(0xF003, 0);
        assert!(!v.irq_asserted());
    }

    #[test]
    fn disabled_irq_never_fires() {
        let mut v = Vrc2::new(&contra_header(), 8 * PRG_BANK_SIZE, 16 * 8 * 1024).unwrap();
        v.write_register(0xF000, 0x0F);
        v.write_register(0xF001, 0x0F); // latch $FF -> would fire next tick
        for _ in 0..300 {
            assert!(!v.vrc_irq_scanline(), "disabled counter stays quiet");
        }
    }

    #[test]
    fn rejects_wrong_mapper() {
        let mut h = contra_header();
        h.mapper = 4;
        assert!(matches!(
            Vrc2::new(&h, 8 * PRG_BANK_SIZE, 16 * 8 * 1024),
            Err(Vrc2Error::UnsupportedMapper { mapper: 4 })
        ));
    }
}
