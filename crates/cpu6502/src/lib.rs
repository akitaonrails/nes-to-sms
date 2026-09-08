//! NMOS 6502 / Ricoh 2A03 instruction decoder.
//!
//! Covers all 151 official opcodes and the stable, commonly-used unofficial
//! opcodes (LAX, SAX, DCP, ISC, SLO, RLA, SRE, RRA, NOP variants).
//! Unstable unofficial opcodes (XAA, AHX, TAS, LAS, SHX, SHY, ANE) are
//! decoded as their mnemonics so the pipeline can fail closed if it sees one.
//!
//! Decimal mode flag exists in the status byte but has no arithmetic effect
//! on the 2A03 (no decimal correction). The decoder is platform-neutral;
//! callers can decide whether to honor decimal mode.

#![allow(clippy::upper_case_acronyms)]

pub mod table;
pub mod timing;

pub use table::{ADDR_MODE_NAME, MNEMONIC_NAME, OPCODE_TABLE, OpInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrMode {
    Implied,
    Accumulator,
    Immediate,
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteX,
    AbsoluteY,
    Indirect,
    IndirectX,
    IndirectY,
    Relative,
}

impl AddrMode {
    pub fn operand_bytes(self) -> u8 {
        match self {
            AddrMode::Implied | AddrMode::Accumulator => 0,
            AddrMode::Immediate
            | AddrMode::ZeroPage
            | AddrMode::ZeroPageX
            | AddrMode::ZeroPageY
            | AddrMode::IndirectX
            | AddrMode::IndirectY
            | AddrMode::Relative => 1,
            AddrMode::Absolute | AddrMode::AbsoluteX | AddrMode::AbsoluteY | AddrMode::Indirect => {
                2
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mnemonic {
    // Load / store
    LDA,
    LDX,
    LDY,
    STA,
    STX,
    STY,
    // Transfer
    TAX,
    TAY,
    TXA,
    TYA,
    TSX,
    TXS,
    // Stack
    PHA,
    PHP,
    PLA,
    PLP,
    // Logical
    AND,
    ORA,
    EOR,
    BIT,
    // Arithmetic
    ADC,
    SBC,
    CMP,
    CPX,
    CPY,
    // Shifts / rotates
    ASL,
    LSR,
    ROL,
    ROR,
    // Increment / decrement
    INC,
    INX,
    INY,
    DEC,
    DEX,
    DEY,
    // Branches
    BCC,
    BCS,
    BEQ,
    BNE,
    BMI,
    BPL,
    BVC,
    BVS,
    // Jumps / calls
    JMP,
    JSR,
    RTS,
    RTI,
    // Flags
    CLC,
    SEC,
    CLI,
    SEI,
    CLV,
    CLD,
    SED,
    // Misc
    NOP,
    BRK,
    // Unofficial stable
    LAX,
    SAX,
    DCP,
    ISC,
    SLO,
    RLA,
    SRE,
    RRA,
    // Unofficial unstable (decoded so we can refuse them cleanly)
    XAA,
    AHX,
    TAS,
    LAS,
    SHX,
    SHY,
    ANE,
    // KIL / JAM (halts CPU)
    JAM,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operand {
    None,
    Imm(u8),
    Addr(u16),
    Relative(i8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction {
    pub pc: u16,
    pub opcode: u8,
    pub mnemonic: Mnemonic,
    pub mode: AddrMode,
    pub operand: Operand,
    pub size: u8,
    pub cycles: u8,
    pub official: bool,
}

impl Instruction {
    /// Computed target for branches/absolute jumps when statically known.
    pub fn branch_target(&self) -> Option<u16> {
        match (self.mnemonic, self.mode, self.operand) {
            (Mnemonic::JMP, AddrMode::Absolute, Operand::Addr(a)) => Some(a),
            (Mnemonic::JSR, AddrMode::Absolute, Operand::Addr(a)) => Some(a),
            (
                Mnemonic::BCC
                | Mnemonic::BCS
                | Mnemonic::BEQ
                | Mnemonic::BNE
                | Mnemonic::BMI
                | Mnemonic::BPL
                | Mnemonic::BVC
                | Mnemonic::BVS,
                AddrMode::Relative,
                Operand::Relative(rel),
            ) => Some(
                self.pc
                    .wrapping_add(self.size as u16)
                    .wrapping_add(rel as i16 as u16),
            ),
            _ => None,
        }
    }

    pub fn is_terminator(&self) -> bool {
        matches!(
            self.mnemonic,
            Mnemonic::RTS | Mnemonic::RTI | Mnemonic::JMP | Mnemonic::BRK | Mnemonic::JAM
        )
    }

    pub fn is_call(&self) -> bool {
        matches!(self.mnemonic, Mnemonic::JSR)
    }

    pub fn is_branch(&self) -> bool {
        matches!(
            self.mnemonic,
            Mnemonic::BCC
                | Mnemonic::BCS
                | Mnemonic::BEQ
                | Mnemonic::BNE
                | Mnemonic::BMI
                | Mnemonic::BPL
                | Mnemonic::BVC
                | Mnemonic::BVS
        )
    }

    pub fn is_return(&self) -> bool {
        matches!(self.mnemonic, Mnemonic::RTS | Mnemonic::RTI)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "truncated operand"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// Decode a single instruction at the given PC.
pub fn decode_at(bytes: &[u8], pc: u16, offset: usize) -> Result<Instruction, DecodeError> {
    let opcode = *bytes.get(offset).ok_or(DecodeError::Truncated)?;
    let info = OPCODE_TABLE[opcode as usize];
    let operand_bytes = info.mode.operand_bytes() as usize;
    if offset + 1 + operand_bytes > bytes.len() {
        return Err(DecodeError::Truncated);
    }
    let operand = match info.mode {
        AddrMode::Implied | AddrMode::Accumulator => Operand::None,
        AddrMode::Immediate => Operand::Imm(bytes[offset + 1]),
        AddrMode::ZeroPage
        | AddrMode::ZeroPageX
        | AddrMode::ZeroPageY
        | AddrMode::IndirectX
        | AddrMode::IndirectY => Operand::Addr(bytes[offset + 1] as u16),
        AddrMode::Absolute | AddrMode::AbsoluteX | AddrMode::AbsoluteY | AddrMode::Indirect => {
            Operand::Addr(u16::from_le_bytes([bytes[offset + 1], bytes[offset + 2]]))
        }
        AddrMode::Relative => Operand::Relative(bytes[offset + 1] as i8),
    };
    Ok(Instruction {
        pc,
        opcode,
        mnemonic: info.mnemonic,
        mode: info.mode,
        operand,
        size: 1 + operand_bytes as u8,
        cycles: info.cycles,
        official: info.official,
    })
}

/// Format an instruction as 6502 assembly text.
pub fn format_instruction(insn: &Instruction) -> String {
    let m = MNEMONIC_NAME[insn.mnemonic as usize];
    match (insn.mode, insn.operand) {
        (AddrMode::Implied, _) => m.to_string(),
        (AddrMode::Accumulator, _) => format!("{m} A"),
        (AddrMode::Immediate, Operand::Imm(v)) => format!("{m} #${v:02X}"),
        (AddrMode::ZeroPage, Operand::Addr(a)) => format!("{m} ${a:02X}"),
        (AddrMode::ZeroPageX, Operand::Addr(a)) => format!("{m} ${a:02X},X"),
        (AddrMode::ZeroPageY, Operand::Addr(a)) => format!("{m} ${a:02X},Y"),
        (AddrMode::Absolute, Operand::Addr(a)) => format!("{m} ${a:04X}"),
        (AddrMode::AbsoluteX, Operand::Addr(a)) => format!("{m} ${a:04X},X"),
        (AddrMode::AbsoluteY, Operand::Addr(a)) => format!("{m} ${a:04X},Y"),
        (AddrMode::Indirect, Operand::Addr(a)) => format!("{m} (${a:04X})"),
        (AddrMode::IndirectX, Operand::Addr(a)) => format!("{m} (${a:02X},X)"),
        (AddrMode::IndirectY, Operand::Addr(a)) => format!("{m} (${a:02X}),Y"),
        (AddrMode::Relative, Operand::Relative(rel)) => {
            let target = insn
                .pc
                .wrapping_add(insn.size as u16)
                .wrapping_add(rel as i16 as u16);
            format!("{m} ${target:04X}")
        }
        _ => format!("{m} ???"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_lda_imm() {
        let bytes = [0xA9, 0x42];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(insn.mnemonic, Mnemonic::LDA);
        assert_eq!(insn.mode, AddrMode::Immediate);
        assert_eq!(insn.operand, Operand::Imm(0x42));
        assert_eq!(insn.size, 2);
    }

    #[test]
    fn decodes_jsr_absolute() {
        let bytes = [0x20, 0x34, 0x12];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(insn.mnemonic, Mnemonic::JSR);
        assert_eq!(insn.operand, Operand::Addr(0x1234));
        assert!(insn.is_call());
        assert_eq!(insn.branch_target(), Some(0x1234));
    }

    #[test]
    fn decodes_branch_relative() {
        let bytes = [0xD0, 0x0E];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(insn.mnemonic, Mnemonic::BNE);
        assert_eq!(insn.branch_target(), Some(0x8010));
    }

    #[test]
    fn decodes_indirect_jmp() {
        let bytes = [0x6C, 0x00, 0x30];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(insn.mnemonic, Mnemonic::JMP);
        assert_eq!(insn.mode, AddrMode::Indirect);
        assert_eq!(insn.operand, Operand::Addr(0x3000));
        assert_eq!(insn.branch_target(), None);
    }

    #[test]
    fn formats_immediate_and_branches() {
        let bytes = [0xA9, 0x42];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(format_instruction(&insn), "LDA #$42");
        let bytes = [0xD0, 0xFE];
        let insn = decode_at(&bytes, 0x8000, 0).unwrap();
        assert_eq!(format_instruction(&insn), "BNE $8000");
    }

    #[test]
    fn all_256_opcodes_have_table_entry() {
        for op in 0..=255u8 {
            let _ = OPCODE_TABLE[op as usize];
        }
    }

    #[test]
    fn known_official_count() {
        let official = OPCODE_TABLE.iter().filter(|i| i.official).count();
        assert_eq!(official, 151);
    }
}
