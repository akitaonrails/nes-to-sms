//! Standard MMC3 cartridge address decoding and register state.
//!
//! This is separate from the translator's [`crate::MapperPolicy`]: describing
//! a cartridge does not establish that the SMS runtime can execute it.
//! The supported board has CHR ROM, ordinary 0/8 KiB PRG RAM and Sharp IRQ
//! behavior (NES 2.0 mapper 4/submapper 0). MMC6, MC-ACC, mixed CHR ROM/RAM,
//! hard-wired/four-screen mirroring and extended banks are not accepted.
//!
//! Register and IRQ contracts: <https://www.nesdev.org/wiki/MMC3> and
//! <https://www.nesdev.org/wiki/NES_2.0_submappers#004:_MMC3>.
//! Checked 2026-09-07: NES 2.0 submapper 0 specifies Sharp behavior; this
//! identifies the modeled behavior, not the exact physical chip revision.
//! IRQ qualification consumes actual PPU address changes and M2 falling
//! edges; callers must not substitute one event per scanline.
//! Undocumented pathological consecutive-$C001 behavior is not modeled.

use crate::{Header, HeaderKind, Mirroring};
use std::fmt;

pub const PRG_WINDOW_SIZE: usize = 8 * 1024;
pub const CHR_WINDOW_SIZE: usize = 1024;

/// The explicit IRQ behavior selected by the consumer. Other revisions are
/// intentionally not represented as aliases of Sharp behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc3Revision {
    Sharp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mmc3Error {
    UnsupportedMapper { mapper: u16 },
    RequiresNes2,
    UnsupportedSubmapper { submapper: u8 },
    UnsupportedTrainer,
    UnsupportedFourScreen,
    InvalidPrgLayout { prg_len: usize },
    InvalidChrLayout { chr_len: usize },
    HeaderPayloadMismatch,
    UnsupportedChrRam,
    UnsupportedPrgRam,
}

impl fmt::Display for Mmc3Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => write!(f, "MMC3 requires mapper 4, got {mapper}"),
            Self::RequiresNes2 => write!(f, "MMC3 requires an explicit NES 2.0 board declaration"),
            Self::UnsupportedSubmapper { submapper } => write!(
                f,
                "unsupported MMC3 submapper {submapper}; only 0 (Sharp MMC3) is implemented"
            ),
            Self::UnsupportedTrainer => write!(f, "MMC3 trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => {
                write!(f, "MMC3 four-screen board wiring is unsupported")
            }
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "MMC3 requires power-of-two 32–512 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "MMC3 requires power-of-two 8–256 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "MMC3 header and payload lengths disagree"),
            Self::UnsupportedChrRam => {
                write!(f, "MMC3 CHR RAM and mixed CHR memory are unsupported")
            }
            Self::UnsupportedPrgRam => write!(
                f,
                "MMC3 requires zero or one 8 KiB PRG RAM chip with consistent battery metadata"
            ),
        }
    }
}

impl std::error::Error for Mmc3Error {}

/// Board state only; the bus owns ROM, RAM and its open-bus value.
///
/// Construction uses deterministic zeroed registers, disabled RAM/IRQ and
/// the header mirroring. These are reference initialization choices, not a
/// guarantee of physical power-on register contents. Software must initialize
/// the mapper before relying on switchable banks or RAM access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mmc3 {
    revision: Mmc3Revision,
    prg_bank_count: u8,
    chr_bank_count: u16,
    has_prg_ram: bool,
    bank_select: u8,
    banks: [u8; 8],
    mirroring: Mirroring,
    ram_protect: u8,
    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,
    a12_high: bool,
    a12_low_m2_edges: u8,
}

impl Mmc3 {
    pub fn new(
        header: &Header,
        prg_len: usize,
        chr_len: usize,
        revision: Mmc3Revision,
    ) -> Result<Self, Mmc3Error> {
        if header.mapper != 4 {
            return Err(Mmc3Error::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.kind != HeaderKind::Nes2 {
            return Err(Mmc3Error::RequiresNes2);
        }
        if header.submapper != 0 {
            return Err(Mmc3Error::UnsupportedSubmapper {
                submapper: header.submapper,
            });
        }
        if header.has_trainer {
            return Err(Mmc3Error::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(Mmc3Error::UnsupportedFourScreen);
        }
        if !(32 * 1024..=512 * 1024).contains(&prg_len) || !prg_len.is_power_of_two() {
            return Err(Mmc3Error::InvalidPrgLayout { prg_len });
        }
        if !(8 * 1024..=256 * 1024).contains(&chr_len) || !chr_len.is_power_of_two() {
            return Err(Mmc3Error::InvalidChrLayout { chr_len });
        }
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(Mmc3Error::HeaderPayloadMismatch);
        }
        if header.chr_ram_size != 0 || header.chr_nvram_size != 0 {
            return Err(Mmc3Error::UnsupportedChrRam);
        }
        let has_prg_ram = match (
            header.prg_ram_size,
            header.prg_nvram_size,
            header.has_battery,
        ) {
            (0, 0, false) => false,
            (8192, 0, false) | (0, 8192, true) => true,
            _ => return Err(Mmc3Error::UnsupportedPrgRam),
        };
        Ok(Self {
            revision,
            prg_bank_count: (prg_len / PRG_WINDOW_SIZE) as u8,
            chr_bank_count: (chr_len / CHR_WINDOW_SIZE) as u16,
            has_prg_ram,
            bank_select: 0,
            banks: [0; 8],
            mirroring: header.mirroring,
            ram_protect: 0,
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_pending: false,
            a12_high: false,
            a12_low_m2_edges: 0,
        })
    }

    pub fn revision(&self) -> Mmc3Revision {
        self.revision
    }

    pub fn prg_bank_count(&self) -> u8 {
        self.prg_bank_count
    }

    pub fn chr_bank_count(&self) -> u16 {
        self.chr_bank_count
    }

    /// Physical 8 KiB banks at $8000, $A000, $C000 and $E000 respectively.
    pub fn prg_banks(&self) -> [u8; 4] {
        let mask = self.prg_bank_count - 1;
        let r6 = self.banks[6] & 0x3f & mask;
        let r7 = self.banks[7] & 0x3f & mask;
        let last = self.prg_bank_count - 1;
        if self.bank_select & 0x40 == 0 {
            [r6, r7, last - 1, last]
        } else {
            [last - 1, r7, r6, last]
        }
    }

    /// Physical 1 KiB CHR banks for the eight pattern-table windows.
    pub fn chr_banks(&self) -> [u8; 8] {
        let mask = (self.chr_bank_count - 1) as u8;
        let r0 = self.banks[0] & 0xfe;
        let r1 = self.banks[1] & 0xfe;
        let mut banks = [
            r0,
            r0 | 1,
            r1,
            r1 | 1,
            self.banks[2],
            self.banks[3],
            self.banks[4],
            self.banks[5],
        ];
        if self.bank_select & 0x80 != 0 {
            banks.rotate_left(4);
        }
        banks.map(|bank| bank & mask)
    }

    pub fn cpu_to_prg_offset(&self, addr: u16) -> Option<usize> {
        if addr < 0x8000 {
            return None;
        }
        let relative = usize::from(addr - 0x8000);
        let bank = self.prg_banks()[relative / PRG_WINDOW_SIZE];
        Some(usize::from(bank) * PRG_WINDOW_SIZE + relative % PRG_WINDOW_SIZE)
    }

    /// Map pattern-table addresses only. Address observation for the IRQ is
    /// deliberately separate: every PPU bus transition, including nametable
    /// access and $2006/$2007 effects, must reach `observe_ppu_address`.
    pub fn ppu_to_chr_offset(&self, addr: u16) -> Option<usize> {
        if addr >= 0x2000 {
            return None;
        }
        let bank = self.chr_banks()[usize::from(addr) / CHR_WINDOW_SIZE];
        Some(usize::from(bank) * CHR_WINDOW_SIZE + usize::from(addr) % CHR_WINDOW_SIZE)
    }

    pub fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// `None` means the cartridge does not drive PRG RAM; the bus must retain
    /// its open-bus behavior rather than manufacturing a zero byte.
    pub fn prg_ram_read_offset(&self, addr: u16) -> Option<usize> {
        (self.has_prg_ram && self.ram_protect & 0x80 != 0 && (0x6000..0x8000).contains(&addr))
            .then(|| usize::from(addr - 0x6000))
    }

    pub fn prg_ram_write_offset(&self, addr: u16) -> Option<usize> {
        if self.ram_protect & 0x40 != 0 {
            None
        } else {
            self.prg_ram_read_offset(addr)
        }
    }

    /// Apply a CPU register write. Returns false outside $8000–$FFFF. The
    /// full alias ranges decode only A15–A13 and A0; there are no bus conflicts.
    pub fn write_register(&mut self, addr: u16, value: u8) -> bool {
        if addr < 0x8000 {
            return false;
        }
        match addr & 0xe001 {
            0x8000 => self.bank_select = value,
            0x8001 => self.banks[usize::from(self.bank_select & 7)] = value,
            0xa000 => {
                self.mirroring = if value & 1 == 0 {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                };
            }
            0xa001 => self.ram_protect = value,
            0xc000 => self.irq_latch = value,
            0xc001 => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            0xe000 => {
                self.irq_enabled = false;
                self.irq_pending = false;
            }
            0xe001 => self.irq_enabled = true,
            _ => unreachable!("MMC3 register mask contains only eight possibilities"),
        }
        true
    }

    /// Observe an address change on the *external PPU bus*, including changes
    /// caused by CPU accesses. Call in chronological order with M2 events.
    /// A12 must remain low across three M2 falling edges before a rising edge
    /// clocks the counter. Repeated high samples and short low pulses do not.
    pub fn observe_ppu_address(&mut self, addr: u16) {
        let high = addr & 0x1000 != 0;
        if high && !self.a12_high && self.a12_low_m2_edges >= 3 {
            self.clock_irq_counter();
        }
        if high || self.a12_high {
            self.a12_low_m2_edges = 0;
        }
        self.a12_high = high;
    }

    /// Call exactly once per physical CPU M2 falling edge, not per instruction
    /// or scanline. This permits qualification even during rendering-off
    /// CPU-driven PPU accesses without assuming any particular PPU fetch mode.
    pub fn clock_m2_falling_edge(&mut self) {
        if !self.a12_high {
            self.a12_low_m2_edges = self.a12_low_m2_edges.saturating_add(1).min(3);
        }
    }

    fn clock_irq_counter(&mut self) {
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
        } else {
            self.irq_counter -= 1;
        }
        self.irq_reload = false;
        match self.revision {
            Mmc3Revision::Sharp => {
                if self.irq_counter == 0 && self.irq_enabled {
                    self.irq_pending = true;
                }
            }
        }
    }

    /// Latched IRQ line; CPU interrupt masking neither clears nor disables it.
    /// Only a write to an even $E000–$FFFF register acknowledges the line.
    pub fn irq_pending(&self) -> bool {
        self.irq_pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            kind: HeaderKind::Nes2,
            prg_banks: 16,
            chr_banks: 16,
            prg_ram_size: 8192,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mapper: 4,
            submapper: 0,
            mirroring: Mirroring::Horizontal,
            has_trainer: false,
            has_battery: false,
        }
    }

    fn board() -> Mmc3 {
        let h = header();
        Mmc3::new(&h, h.prg_len(), h.chr_len(), Mmc3Revision::Sharp).unwrap()
    }

    fn bank(m: &mut Mmc3, select: u8, value: u8) {
        m.write_register(0x8000, select);
        m.write_register(0x8001, value);
    }

    fn qualified_edge(m: &mut Mmc3) {
        m.observe_ppu_address(0);
        for _ in 0..3 {
            m.clock_m2_falling_edge();
        }
        m.observe_ppu_address(0x1000);
    }

    fn irq_board(latch: u8) -> Mmc3 {
        let mut m = board();
        m.write_register(0xc000, latch);
        m.write_register(0xc001, 0);
        m.write_register(0xe001, 0);
        m
    }

    #[test]
    fn prg_windows_use_independent_banks_and_inversion() {
        let mut m = board();
        let prg: Vec<u8> = (0..32).flat_map(|b| vec![b; PRG_WINDOW_SIZE]).collect();
        bank(&mut m, 6, 5);
        bank(&mut m, 7, 9);
        assert_eq!(m.prg_banks(), [5, 9, 30, 31]);
        for (addr, expected) in [
            (0x8000, 5),
            (0x9fff, 5),
            (0xa000, 9),
            (0xbfff, 9),
            (0xc000, 30),
            (0xdfff, 30),
            (0xe000, 31),
            (0xffff, 31),
        ] {
            assert_eq!(prg[m.cpu_to_prg_offset(addr).unwrap()], expected);
        }
        m.write_register(0x8000, 0x46);
        assert_eq!(m.prg_banks(), [30, 9, 5, 31]);
        m.write_register(0x8001, 0xff);
        assert_eq!(m.prg_banks(), [30, 9, 31, 31]);
        assert_eq!(m.cpu_to_prg_offset(0xfffa), Some(prg.len() - 6));
        assert_eq!(m.cpu_to_prg_offset(0x7fff), None);
    }

    #[test]
    fn chr_banks_align_pairs_invert_and_mask_unwired_bits() {
        let mut m = board();
        let chr: Vec<u8> = (0..128).flat_map(|b| vec![b; CHR_WINDOW_SIZE]).collect();
        for (r, v) in [(0, 3), (1, 7), (2, 11), (3, 13), (4, 15), (5, 0xff)] {
            bank(&mut m, r, v);
        }
        assert_eq!(m.chr_banks(), [2, 3, 6, 7, 11, 13, 15, 127]);
        for (window, expected) in [2, 3, 6, 7, 11, 13, 15, 127].into_iter().enumerate() {
            for offset in [0, 1023] {
                let addr = (window * CHR_WINDOW_SIZE + offset) as u16;
                assert_eq!(chr[m.ppu_to_chr_offset(addr).unwrap()], expected);
            }
        }
        m.write_register(0x8000, 0x80);
        assert_eq!(m.chr_banks(), [11, 13, 15, 127, 2, 3, 6, 7]);
        assert_eq!(m.ppu_to_chr_offset(0x2000), None);
        assert_eq!(m.ppu_to_chr_offset(0xffff), None);
    }

    #[test]
    fn maximum_geometry_preserves_all_wired_address_bits() {
        let mut h = header();
        h.prg_banks = 32;
        h.chr_banks = 32;
        let mut m = Mmc3::new(&h, h.prg_len(), h.chr_len(), Mmc3Revision::Sharp).unwrap();
        assert_eq!((m.prg_bank_count(), m.chr_bank_count()), (64, 256));
        bank(&mut m, 6, 0xff);
        bank(&mut m, 7, 0x80);
        bank(&mut m, 0, 0xff);
        assert_eq!(m.prg_banks(), [63, 0, 62, 63]);
        assert_eq!(&m.chr_banks()[..2], &[254, 255]);
        assert_eq!(m.ppu_to_chr_offset(0x7ff), Some(256 * 1024 - 1));
    }

    #[test]
    fn all_register_aliases_decode_only_region_and_parity() {
        for canonical in [
            0x8000u16, 0x8001, 0xa000, 0xa001, 0xc000, 0xc001, 0xe000, 0xe001,
        ] {
            let mut expected = irq_board(1);
            qualified_edge(&mut expected);
            qualified_edge(&mut expected);
            let initial = expected.clone();
            expected.write_register(canonical, 0xd7);
            for offset in (0..0x2000).step_by(2) {
                let mut actual = initial.clone();
                assert!(actual.write_register(canonical + offset, 0xd7));
                assert_eq!(actual, expected, "alias ${:04x}", canonical + offset);
            }
        }
        let mut m = board();
        let original = m.clone();
        assert!(!m.write_register(0x7fff, 0xff));
        assert_eq!(m, original);
    }

    #[test]
    fn ram_gates_reads_writes_and_dynamic_mirroring() {
        let mut m = board();
        assert_eq!(m.prg_ram_read_offset(0x6000), None);
        m.write_register(0xa001, 0x80);
        assert_eq!(m.prg_ram_read_offset(0x6000), Some(0));
        assert_eq!(m.prg_ram_write_offset(0x7fff), Some(8191));
        assert_eq!(m.prg_ram_read_offset(0x5fff), None);
        assert_eq!(m.prg_ram_write_offset(0x8000), None);
        m.write_register(0xa001, 0xc0);
        assert_eq!(m.prg_ram_read_offset(0x7fff), Some(8191));
        assert_eq!(m.prg_ram_write_offset(0x6000), None);
        m.write_register(0xa001, 0x40);
        assert_eq!(m.prg_ram_read_offset(0x6000), None);
        m.write_register(0xa000, 0xfe);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        m.write_register(0xa000, 0xff);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);
        let mut h = header();
        h.prg_ram_size = 0;
        let mut absent = Mmc3::new(&h, h.prg_len(), h.chr_len(), Mmc3Revision::Sharp).unwrap();
        absent.write_register(0xa001, 0x80);
        assert_eq!(absent.prg_ram_read_offset(0x6000), None);
        assert_eq!(absent.prg_ram_write_offset(0x6000), None);
    }

    #[test]
    fn irq_requires_three_low_m2_edges_and_a_rising_a12() {
        let mut m = irq_board(0);
        m.observe_ppu_address(0x1000);
        assert!(!m.irq_pending());
        for short in 0..3 {
            m.observe_ppu_address(0);
            for _ in 0..short {
                m.clock_m2_falling_edge();
            }
            m.observe_ppu_address(0x1000);
            assert!(!m.irq_pending());
        }
        for _ in 0..10 {
            m.clock_m2_falling_edge();
        }
        m.observe_ppu_address(0x1fff);
        assert!(!m.irq_pending());
        m.observe_ppu_address(0x2000);
        for _ in 0..3 {
            m.clock_m2_falling_edge();
        }
        m.observe_ppu_address(0x2fff);
        assert!(!m.irq_pending());
        m.observe_ppu_address(0x3000);
        assert!(m.irq_pending());
    }

    #[test]
    fn irq_reload_is_deferred_and_ack_does_not_stop_counter() {
        // Hardware-derived cases correspond to blargg mmc3_test_2/2-details.s.
        let mut m = irq_board(2);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 2);
        m.write_register(0xe000, 0);
        qualified_edge(&mut m);
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 0);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        qualified_edge(&mut m);
        m.write_register(0xe001, 0);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert!(m.irq_pending());
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 2);
        assert!(m.irq_pending());
        m.write_register(0xe000, 0);
        m.write_register(0xe001, 0);
        qualified_edge(&mut m);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert!(m.irq_pending());
        m.write_register(0xe000, 0);
        m.write_register(0xe001, 0);
        m.write_register(0xc000, 0);
        m.write_register(0xc001, 0);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert!(m.irq_pending());
    }

    #[test]
    fn latch_write_does_not_reload_and_enable_does_not_assert() {
        let mut m = irq_board(2);
        qualified_edge(&mut m);
        m.write_register(0xc000, 9);
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 1);
        qualified_edge(&mut m);
        assert!(m.irq_pending());
        m.write_register(0xe000, 0);
        m.write_register(0xe001, 0);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 9);
        assert!(!m.irq_pending());
        m.write_register(0xc001, 0);
        assert!(!m.irq_pending());
        qualified_edge(&mut m);
        assert_eq!(m.irq_counter, 9);
    }

    #[test]
    fn sharp_zero_latch_asserts_on_each_qualified_clock() {
        let mut m = irq_board(0);
        for _ in 0..3 {
            assert!(!m.irq_pending());
            qualified_edge(&mut m);
            assert!(m.irq_pending());
            m.write_register(0xe000, 0);
            m.write_register(0xe001, 0);
            // Acknowledging while A12 remains high cannot create a new edge.
            for _ in 0..4 {
                m.clock_m2_falling_edge();
                m.observe_ppu_address(0x1fff);
            }
            assert!(!m.irq_pending());
        }
    }

    #[test]
    fn battery_ram_uses_the_same_enable_and_write_protection() {
        let mut h = header();
        h.prg_ram_size = 0;
        h.prg_nvram_size = 8192;
        h.has_battery = true;
        let mut m = Mmc3::new(&h, h.prg_len(), h.chr_len(), Mmc3Revision::Sharp).unwrap();
        assert_eq!(m.prg_ram_read_offset(0x6000), None);
        m.write_register(0xa001, 0x80);
        assert_eq!(m.prg_ram_write_offset(0x6000), Some(0));
        m.write_register(0xa001, 0xc0);
        assert_eq!(m.prg_ram_read_offset(0x6000), Some(0));
        assert_eq!(m.prg_ram_write_offset(0x6000), None);
    }

    #[test]
    fn maximum_irq_latch_takes_256_qualified_clocks() {
        let mut m = irq_board(255);
        for _ in 0..255 {
            qualified_edge(&mut m);
            assert!(!m.irq_pending());
        }
        qualified_edge(&mut m);
        assert!(m.irq_pending());
    }

    #[test]
    fn rejects_other_boards_and_ambiguous_memory_layouts() {
        let check = |h: Header, expected| {
            assert_eq!(
                Mmc3::new(&h, h.prg_len(), h.chr_len(), Mmc3Revision::Sharp),
                Err(expected)
            );
        };
        let mut h = header();
        h.mapper = 119;
        check(h, Mmc3Error::UnsupportedMapper { mapper: 119 });
        let mut h = header();
        h.kind = HeaderKind::INes;
        check(h, Mmc3Error::RequiresNes2);
        for submapper in 1..=15 {
            let mut h = header();
            h.submapper = submapper;
            check(h, Mmc3Error::UnsupportedSubmapper { submapper });
        }
        let mut h = header();
        h.has_trainer = true;
        check(h, Mmc3Error::UnsupportedTrainer);
        let mut h = header();
        h.mirroring = Mirroring::FourScreen;
        check(h, Mmc3Error::UnsupportedFourScreen);
        for prg_banks in [0, 1, 3, 33, 64] {
            let mut h = header();
            h.prg_banks = prg_banks;
            check(
                h,
                Mmc3Error::InvalidPrgLayout {
                    prg_len: h.prg_len(),
                },
            );
        }
        for chr_banks in [0, 3, 33, 64] {
            let mut h = header();
            h.chr_banks = chr_banks;
            check(
                h,
                Mmc3Error::InvalidChrLayout {
                    chr_len: h.chr_len(),
                },
            );
        }
        let mut h = header();
        h.chr_ram_size = 8192;
        check(h, Mmc3Error::UnsupportedChrRam);
        let mut h = header();
        h.chr_nvram_size = 8192;
        check(h, Mmc3Error::UnsupportedChrRam);
        for ram in [1024, 4096, 16384] {
            let mut h = header();
            h.prg_ram_size = ram;
            check(h, Mmc3Error::UnsupportedPrgRam);
        }
        let mut h = header();
        h.prg_nvram_size = 8192;
        h.has_battery = true;
        check(h, Mmc3Error::UnsupportedPrgRam);
        let mut h = header();
        h.has_battery = true;
        check(h, Mmc3Error::UnsupportedPrgRam);
        let h = header();
        assert_eq!(
            Mmc3::new(&h, 32 * 1024, h.chr_len(), Mmc3Revision::Sharp),
            Err(Mmc3Error::HeaderPayloadMismatch)
        );
    }
}
