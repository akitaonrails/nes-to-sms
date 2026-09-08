//! Version-one descriptor ABI for the opt-in source-clock runtime.
use cpu6502::{
    AddrMode, Instruction, Operand,
    timing::{BusSequence, CyclePenalty},
};

pub(super) fn descriptor(i: Instruction) -> Option<[u8; 10]> {
    let t = i.timing()?;
    let operand = match i.operand {
        Operand::None => 0,
        Operand::Imm(n) => u16::from(n),
        Operand::Addr(n) => n,
        Operand::Relative(n) => u16::from(n as u8),
    };
    let bytes = [i.opcode, operand as u8, (operand >> 8) as u8];
    if cpu6502::decode_at(&bytes, i.pc, 0).ok()? != i {
        return None;
    }
    // Explicit wire values: never serialize Rust's enum representation.
    let sequence = match t.sequence {
        BusSequence::Read => 0,
        BusSequence::Write => 1,
        BusSequence::ReadModifyWrite => 2,
        BusSequence::Implied => 3,
        BusSequence::Branch => 4,
        BusSequence::Push => 5,
        BusSequence::Pull => 6,
        BusSequence::Jsr => 7,
        BusSequence::Rts => 8,
        BusSequence::Rti => 9,
        BusSequence::Brk => 10,
        BusSequence::JumpAbsolute => 11,
        BusSequence::JumpIndirect => 12,
    };
    let mode = match i.mode {
        AddrMode::Implied => 0,
        AddrMode::Accumulator => 1,
        AddrMode::Immediate => 2,
        AddrMode::ZeroPage => 3,
        AddrMode::ZeroPageX => 4,
        AddrMode::ZeroPageY => 5,
        AddrMode::Absolute => 6,
        AddrMode::AbsoluteX => 7,
        AddrMode::AbsoluteY => 8,
        AddrMode::Indirect => 9,
        AddrMode::IndirectX => 10,
        AddrMode::IndirectY => 11,
        AddrMode::Relative => 12,
    };
    let penalty = match t.penalty {
        CyclePenalty::None => 0,
        CyclePenalty::IndexedReadPageCross => 1,
        CyclePenalty::BranchTakenAndPageCross => 2,
    };
    Some([
        i.pc as u8,
        (i.pc >> 8) as u8,
        i.opcode,
        operand as u8,
        (operand >> 8) as u8,
        i.size,
        sequence,
        mode,
        t.base_cycles,
        penalty,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_uses_explicit_wire_values_and_original_operands() {
        for (bytes, expected) in [
            (
                [0x8d, 0x00, 0x20],
                [0x00, 0x80, 0x8d, 0, 0x20, 3, 1, 6, 4, 0],
            ),
            (
                [0xb1, 0xff, 0x00],
                [0x00, 0x80, 0xb1, 0xff, 0, 2, 0, 11, 5, 1],
            ),
            (
                [0xd0, 0xfe, 0x00],
                [0x00, 0x80, 0xd0, 0xfe, 0, 2, 4, 12, 2, 2],
            ),
            ([0x00, 0xa5, 0x00], [0x00, 0x80, 0x00, 0, 0, 1, 10, 0, 7, 0]),
        ] {
            let i = cpu6502::decode_at(&bytes, 0x8000, 0).unwrap();
            assert_eq!(descriptor(i), Some(expected));
        }
    }

    #[test]
    fn descriptor_rejects_inconsistent_or_unstable_source_identity() {
        let mut i = cpu6502::decode_at(&[0xa9, 7], 0x8000, 0).unwrap();
        i.cycles = 3;
        assert!(descriptor(i).is_none());
        let unstable = cpu6502::decode_at(&[0xab, 7], 0x8000, 0).unwrap();
        assert!(descriptor(unstable).is_none());
    }
}
