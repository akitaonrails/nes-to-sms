//! CNROM cartridge address decoding and CHR bank latch.
//!
//! This board model does not admit mapper 3 to the translation pipeline. It
//! models plain NES 2.0 submapper 1/2 boards, including the documented oversize
//! 128 KiB CHR wiring and optional mirrored 2 KiB PRG RAM. It does not model
//! security-diode electrical effects or the M50805 speech peripheral. Those
//! require independent board identification; mapper/submapper alone cannot
//! establish their absence. Mapper 185 CHR-disable wiring is a different mapper.
//!
//! Contracts: <https://www.nesdev.org/wiki/CNROM>,
//! <https://www.nesdev.org/wiki/NES_2.0_submappers#003:_0,_1,_2_CNROM>, and
//! <https://www.nesdev.org/wiki/Implementing_Mappers_In_Hardware>.

use crate::{CHR_BANK_SIZE, Header, HeaderKind, Mirroring, PRG_BANK_SIZE};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CnromBusConflicts {
    None,
    And,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CnromError {
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

impl fmt::Display for CnromError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedMapper { mapper } => {
                write!(f, "CNROM requires mapper 3, got {mapper}")
            }
            Self::RequiresNes2 => write!(f, "CNROM requires an explicit NES 2.0 board declaration"),
            Self::UnsupportedSubmapper { submapper } => write!(
                f,
                "unsupported CNROM submapper {submapper}; specify 1 (no conflict) or 2 (AND conflict)"
            ),
            Self::UnsupportedTrainer => write!(f, "CNROM trainer initialization is unsupported"),
            Self::UnsupportedFourScreen => {
                write!(f, "CNROM requires fixed horizontal or vertical mirroring")
            }
            Self::InvalidPrgLayout { prg_len } => write!(
                f,
                "CNROM requires exactly 16 or 32 KiB PRG ROM, got {prg_len} bytes"
            ),
            Self::InvalidChrLayout { chr_len } => write!(
                f,
                "CNROM requires power-of-two 8–128 KiB CHR ROM, got {chr_len} bytes"
            ),
            Self::HeaderPayloadMismatch => write!(f, "CNROM header and payload lengths disagree"),
            Self::UnsupportedChrRam => {
                write!(f, "CNROM CHR RAM and mixed CHR memory are unsupported")
            }
            Self::UnsupportedPrgRam => write!(
                f,
                "CNROM requires zero or 2 KiB volatile PRG RAM and no battery"
            ),
        }
    }
}

impl std::error::Error for CnromError {}

/// Board state only: the caller owns ROM, RAM and open-bus values.
///
/// The power-on latch is intentionally supplied by the caller; plain CNROM
/// hardware does not guarantee an initial bank. A deterministic emulator may
/// choose zero, but initialization coverage should also exercise other values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cnrom {
    prg_len: usize,
    chr_bank_count: u8,
    bus_conflicts: CnromBusConflicts,
    has_prg_ram: bool,
    mirroring: Mirroring,
    chr_bank: u8,
}

impl Cnrom {
    pub fn new(
        header: &Header,
        prg_len: usize,
        chr_len: usize,
        power_on_latch: u8,
    ) -> Result<Self, CnromError> {
        if header.mapper != 3 {
            return Err(CnromError::UnsupportedMapper {
                mapper: header.mapper,
            });
        }
        if header.kind != HeaderKind::Nes2 {
            return Err(CnromError::RequiresNes2);
        }
        let bus_conflicts = match header.submapper {
            1 => CnromBusConflicts::None,
            2 => CnromBusConflicts::And,
            submapper => return Err(CnromError::UnsupportedSubmapper { submapper }),
        };
        if header.has_trainer {
            return Err(CnromError::UnsupportedTrainer);
        }
        if header.mirroring == Mirroring::FourScreen {
            return Err(CnromError::UnsupportedFourScreen);
        }
        if prg_len != PRG_BANK_SIZE && prg_len != 2 * PRG_BANK_SIZE {
            return Err(CnromError::InvalidPrgLayout { prg_len });
        }
        if !(CHR_BANK_SIZE..=16 * CHR_BANK_SIZE).contains(&chr_len) || !chr_len.is_power_of_two() {
            return Err(CnromError::InvalidChrLayout { chr_len });
        }
        if header.prg_len() != prg_len || header.chr_len() != chr_len {
            return Err(CnromError::HeaderPayloadMismatch);
        }
        if header.chr_ram_size != 0 || header.chr_nvram_size != 0 {
            return Err(CnromError::UnsupportedChrRam);
        }
        if !matches!(header.prg_ram_size, 0 | 2048)
            || header.prg_nvram_size != 0
            || header.has_battery
        {
            return Err(CnromError::UnsupportedPrgRam);
        }
        let chr_bank_count = (chr_len / CHR_BANK_SIZE) as u8;
        Ok(Self {
            prg_len,
            chr_bank_count,
            bus_conflicts,
            has_prg_ram: header.prg_ram_size != 0,
            mirroring: header.mirroring,
            chr_bank: power_on_latch & (chr_bank_count - 1),
        })
    }

    pub fn bus_conflicts(&self) -> CnromBusConflicts {
        self.bus_conflicts
    }

    pub fn chr_bank_count(&self) -> u8 {
        self.chr_bank_count
    }

    /// Physical 8 KiB bank identity, not a tile index or converted SMS slot.
    pub fn chr_bank(&self) -> u8 {
        self.chr_bank
    }

    pub fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    /// Fixed PRG wiring; a 16 KiB chip is mirrored in both CPU windows.
    pub fn cpu_to_prg_offset(&self, addr: u16) -> Option<usize> {
        (addr >= 0x8000).then(|| usize::from(addr - 0x8000) & (self.prg_len - 1))
    }

    /// Map reads and writes to the optional 2 KiB chip at $6000–$7FFF.
    /// `None` means no RAM is driven; the bus supplies its open-bus value.
    pub fn cpu_to_prg_ram_offset(&self, addr: u16) -> Option<usize> {
        (self.has_prg_ram && (0x6000..0x8000).contains(&addr))
            .then(|| usize::from(addr - 0x6000) & 0x07ff)
    }

    /// Pattern-table reads only; writes cannot modify CHR ROM. Nametable
    /// and palette decoding belong to the PPU bus, not this bank latch.
    pub fn ppu_to_chr_offset(&self, addr: u16) -> Option<usize> {
        (addr < 0x2000).then(|| usize::from(self.chr_bank) * CHR_BANK_SIZE + usize::from(addr))
    }

    /// All CPU writes at $8000–$FFFF address the same latch. The bus must
    /// supply the PRG byte at the *written address*, before the write, and
    /// call this for both write cycles of a 6502 read/modify/write operation.
    /// Returns whether this was a mapper-register write, even if unchanged.
    pub fn write_register(&mut self, addr: u16, value: u8, pre_write_rom_byte: u8) -> bool {
        if addr < 0x8000 {
            return false;
        }
        let effective = match self.bus_conflicts {
            CnromBusConflicts::None => value,
            CnromBusConflicts::And => value & pre_write_rom_byte,
        };
        self.chr_bank = effective & (self.chr_bank_count - 1);
        true
    }

    /// Plain CNROM has no console-reset input connected to its bank latch.
    /// A soft reset preserves selection; power cycling creates a new board.
    pub fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            kind: HeaderKind::Nes2,
            mapper: 3,
            submapper: 2,
            prg_banks: 2,
            chr_banks: 4,
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
            mirroring: Mirroring::Horizontal,
            has_trainer: false,
            has_battery: false,
        }
    }

    fn board(h: &Header) -> Result<Cnrom, CnromError> {
        Cnrom::new(h, h.prg_len(), h.chr_len(), 0)
    }

    #[test]
    fn fixed_prg_vectors_and_16k_aliases_ignore_chr_selection() {
        for banks in [1, 2] {
            let h = Header {
                prg_banks: banks,
                ..header()
            };
            let mut m = board(&h).unwrap();
            for bank in 0..4 {
                m.write_register(0x8000, bank, 0xff);
                for addr in 0x8000..=0xffffu16 {
                    let expected = usize::from(addr - 0x8000) % h.prg_len();
                    assert_eq!(m.cpu_to_prg_offset(addr), Some(expected));
                }
                assert_eq!(m.cpu_to_prg_offset(0xfffa), Some(h.prg_len() - 6));
                assert_eq!(m.cpu_to_prg_offset(0xfffc), Some(h.prg_len() - 4));
                assert_eq!(m.cpu_to_prg_offset(0xfffe), Some(h.prg_len() - 2));
            }
            assert_eq!(m.cpu_to_prg_offset(0x7fff), None);
        }
    }

    #[test]
    fn submappers_apply_conflicts_before_masking_for_every_data_byte() {
        for submapper in [1, 2] {
            for chr_banks in [1, 2, 4, 8, 16] {
                let h = Header {
                    submapper,
                    chr_banks,
                    ..header()
                };
                let mut m = board(&h).unwrap();
                assert_eq!(m.chr_bank_count(), chr_banks as u8);
                for value in 0..=255u8 {
                    for rom in [0, 1, 2, 3, 5, 10, 15, 0x55, 0xaa, 255] {
                        assert!(m.write_register(0xbeef, value, rom));
                        let effective = if submapper == 2 { value & rom } else { value };
                        let expected = effective % chr_banks as u8;
                        assert_eq!(m.chr_bank(), expected);
                        for addr in [0, 0x0fff, 0x1000, 0x1fff] {
                            assert_eq!(
                                m.ppu_to_chr_offset(addr),
                                Some(usize::from(expected) * 8192 + usize::from(addr))
                            );
                        }
                    }
                }
                assert_eq!(m.ppu_to_chr_offset(0x2000), None);
                assert_eq!(m.ppu_to_chr_offset(0xffff), None);
            }
        }
    }

    #[test]
    fn every_high_cpu_address_is_a_register_alias_low_addresses_are_not() {
        let mut m = board(&header()).unwrap();
        for addr in 0..0x8000 {
            assert!(!m.write_register(addr, 3, 0xff));
            assert_eq!(m.chr_bank(), 0);
        }
        for addr in 0x8000..=0xffff {
            let bank = addr as u8 & 3;
            assert!(m.write_register(addr, bank, 0xff));
            assert_eq!(m.chr_bank(), bank);
        }
    }

    #[test]
    fn conflict_uses_rom_byte_at_written_address_including_mirrors() {
        let h = Header {
            prg_banks: 1,
            ..header()
        };
        let mut m = board(&h).unwrap();
        let mut prg = vec![0; h.prg_len()];
        prg[0x1234] = 2;
        prg[0x1235] = 1;
        for (addr, expected) in [(0x9234, 2), (0xd234, 2), (0xd235, 1)] {
            m.write_register(addr, 3, prg[m.cpu_to_prg_offset(addr).unwrap()]);
            assert_eq!(m.chr_bank(), expected);
        }
        // An INC $D234 writes the original byte and then its increment.
        m.write_register(0xd234, 2, 2);
        assert_eq!(m.chr_bank(), 2);
        m.write_register(0xd234, 3, 2);
        assert_eq!(m.chr_bank(), 2);
    }

    #[test]
    fn physical_chr_identity_keeps_same_tile_in_different_banks_distinct() {
        let h = header();
        let mut m = board(&h).unwrap();
        let chr: Vec<u8> = (0..4).flat_map(|b| vec![b; CHR_BANK_SIZE]).collect();
        for bank in 0..4 {
            m.write_register(0xffff, bank, 0xff);
            for addr in [0, 0x123, 0x1000, 0x1fff] {
                assert_eq!(chr[m.ppu_to_chr_offset(addr).unwrap()], bank);
            }
        }
    }

    #[test]
    fn reset_preserves_latch_and_fixed_mirroring_for_all_initial_banks() {
        for mirroring in [Mirroring::Horizontal, Mirroring::Vertical] {
            let h = Header {
                mirroring,
                ..header()
            };
            for latch in 0..=255 {
                let mut m = Cnrom::new(&h, h.prg_len(), h.chr_len(), latch).unwrap();
                assert_eq!(m.chr_bank(), latch & 3);
                m.reset();
                assert_eq!(m.chr_bank(), latch & 3);
                m.write_register(0xffff, 2, 0xff);
                m.reset();
                assert_eq!(m.chr_bank(), 2);
                assert_eq!(m.mirroring(), mirroring);
            }
        }
    }

    #[test]
    fn optional_2k_ram_is_mirrored_four_times_without_bank_effects() {
        let empty = board(&header()).unwrap();
        let h = Header {
            prg_ram_size: 2048,
            ..header()
        };
        let mut m = board(&h).unwrap();
        let mut ram = [0; 2048];
        ram[m.cpu_to_prg_ram_offset(0x67ab).unwrap()] = 0x42;
        for addr in [0x67ab, 0x6fab, 0x77ab, 0x7fab] {
            assert_eq!(ram[m.cpu_to_prg_ram_offset(addr).unwrap()], 0x42);
            assert_eq!(empty.cpu_to_prg_ram_offset(addr), None);
            assert!(!m.write_register(addr, 3, 0xff));
        }
        assert_eq!(m.chr_bank(), 0);
        for addr in [0, 0x5fff, 0x8000, 0xffff] {
            assert_eq!(m.cpu_to_prg_ram_offset(addr), None);
        }
    }

    #[test]
    fn rejects_ambiguous_variants_and_unsupported_board_metadata() {
        for (h, expected) in [
            (
                Header {
                    mapper: 185,
                    ..header()
                },
                CnromError::UnsupportedMapper { mapper: 185 },
            ),
            (
                Header {
                    kind: HeaderKind::INes,
                    ..header()
                },
                CnromError::RequiresNes2,
            ),
            (
                Header {
                    has_trainer: true,
                    ..header()
                },
                CnromError::UnsupportedTrainer,
            ),
            (
                Header {
                    mirroring: Mirroring::FourScreen,
                    ..header()
                },
                CnromError::UnsupportedFourScreen,
            ),
            (
                Header {
                    chr_ram_size: 8192,
                    ..header()
                },
                CnromError::UnsupportedChrRam,
            ),
            (
                Header {
                    chr_nvram_size: 8192,
                    ..header()
                },
                CnromError::UnsupportedChrRam,
            ),
            (
                Header {
                    prg_ram_size: 8192,
                    ..header()
                },
                CnromError::UnsupportedPrgRam,
            ),
            (
                Header {
                    prg_nvram_size: 2048,
                    ..header()
                },
                CnromError::UnsupportedPrgRam,
            ),
            (
                Header {
                    has_battery: true,
                    ..header()
                },
                CnromError::UnsupportedPrgRam,
            ),
        ] {
            assert_eq!(board(&h), Err(expected));
        }
        for submapper in [0, 3, 15] {
            assert_eq!(
                board(&Header {
                    submapper,
                    ..header()
                }),
                Err(CnromError::UnsupportedSubmapper { submapper })
            );
        }
    }

    #[test]
    fn rejects_bad_geometry_and_payload_mismatch_without_modulo_guessing() {
        let h = header();
        for prg_len in [0, 8192, 16385, 49152, 65536] {
            assert_eq!(
                Cnrom::new(&h, prg_len, h.chr_len(), 0),
                Err(CnromError::InvalidPrgLayout { prg_len })
            );
        }
        for chr_len in [0, 4096, 8193, 24576, 262144] {
            assert_eq!(
                Cnrom::new(&h, h.prg_len(), chr_len, 0),
                Err(CnromError::InvalidChrLayout { chr_len })
            );
        }
        assert_eq!(
            Cnrom::new(&h, 16384, h.chr_len(), 0),
            Err(CnromError::HeaderPayloadMismatch)
        );
        assert_eq!(
            Cnrom::new(&h, h.prg_len(), 8192, 0),
            Err(CnromError::HeaderPayloadMismatch)
        );
    }

    #[test]
    fn board_model_does_not_enable_pipeline_admission() {
        let h = header();
        assert!(board(&h).is_ok());
        assert_eq!(
            crate::resolve_mapper_policy(&h, h.prg_len()),
            Err(crate::MapperPolicyError::UnsupportedMapper { mapper: 3 })
        );
    }
}
