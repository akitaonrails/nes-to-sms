//! Typed NMOS source instruction timing classification.
//!
//! This describes the family of bus sequence, not merely elapsed cycles.
//! Conditional penalties, dummy accesses, interrupt polling and DMA still need
//! their ordered runtime implementation; this metadata alone proves none of
//! those behaviors. Keep it independent of target CPU execution duration.

use crate::{AddrMode, Instruction, Mnemonic};

/// Families with distinct ordered source bus sequences. Addressing mode remains
/// in the decoded instruction: e.g. indexed reads and writes have different
/// dummy-read behavior even when they end at the same effective address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusSequence {
    Read,
    Write,
    ReadModifyWrite,
    Implied,
    Branch,
    Push,
    Pull,
    Jsr,
    Rts,
    Rti,
    Brk,
    JumpAbsolute,
    JumpIndirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CyclePenalty {
    None,
    /// Indexed reads add one cycle if the effective address crosses a page.
    IndexedReadPageCross,
    /// Taken branches add one cycle, plus another for a page crossing relative
    /// to the PC after the displacement byte, not relative to the opcode PC.
    BranchTakenAndPageCross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstructionTiming {
    pub sequence: BusSequence,
    pub base_cycles: u8,
    pub penalty: CyclePenalty,
}

impl Instruction {
    /// Classify only decoded official and supported stable unofficial source
    /// encodings. Missing timing templates must fail closed, not inherit a
    /// plausible base-cycle count from an unstable opcode's decoder entry.
    pub fn timing(&self) -> Option<InstructionTiming> {
        use Mnemonic::*;
        // Immediate LAX depends on unstable internal bus behavior; retaining
        // its decoder identity is not permission to admit a timing template.
        if self.opcode == 0xab {
            return None;
        }
        let sequence = match self.mnemonic {
            LDA | LDX | LDY | LAX | ADC | SBC | AND | ORA | EOR | BIT | CMP | CPX | CPY => {
                BusSequence::Read
            }
            STA | STX | STY | SAX => BusSequence::Write,
            ASL | LSR | ROL | ROR if self.mode == AddrMode::Accumulator => BusSequence::Implied,
            ASL | LSR | ROL | ROR | INC | DEC | DCP | ISC | SLO | RLA | SRE | RRA => {
                BusSequence::ReadModifyWrite
            }
            BCC | BCS | BEQ | BNE | BMI | BPL | BVC | BVS => BusSequence::Branch,
            PHA | PHP => BusSequence::Push,
            PLA | PLP => BusSequence::Pull,
            JSR => BusSequence::Jsr,
            RTS => BusSequence::Rts,
            RTI => BusSequence::Rti,
            BRK => BusSequence::Brk,
            JMP if self.mode == AddrMode::Absolute => BusSequence::JumpAbsolute,
            JMP if self.mode == AddrMode::Indirect => BusSequence::JumpIndirect,
            NOP if self.mode != AddrMode::Implied => BusSequence::Read,
            NOP | TAX | TAY | TXA | TYA | TSX | TXS | INX | INY | DEX | DEY | CLC | SEC | CLI
            | SEI | CLV | CLD | SED => BusSequence::Implied,
            _ => return None,
        };
        let penalty = match sequence {
            BusSequence::Branch => CyclePenalty::BranchTakenAndPageCross,
            BusSequence::Read
                if matches!(
                    self.mode,
                    AddrMode::AbsoluteX | AddrMode::AbsoluteY | AddrMode::IndirectY
                ) =>
            {
                CyclePenalty::IndexedReadPageCross
            }
            _ => CyclePenalty::None,
        };
        Some(InstructionTiming {
            sequence,
            base_cycles: self.cycles,
            penalty,
        })
    }
}
