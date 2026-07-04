//! Semantic IR for the NES-to-SMS pipeline.
//!
//! Lifts 6502 instructions into an explicit intermediate representation that
//! carries memory-region tags, flag annotations, and structured control flow.
//! A separate `lower` crate transforms IR into Z80 output.

#![allow(clippy::upper_case_acronyms)]

use cpu6502::{AddrMode, Instruction, Mnemonic, Operand, decode_at, format_instruction};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// MemRegion
// ---------------------------------------------------------------------------

/// Region a memory access touches. Tagged at lift time so the lowering
/// pass can route hardware accesses to runtime shims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemRegion {
    ZeroPage,  // $0000-$00FF
    Stack,     // $0100-$01FF  (rarely used directly; PHA/PLA etc. handled by ops)
    Ram,       // $0200-$07FF (also mirrors via masking)
    RamMirror, // $0800-$1FFF — masked to $0000-$07FF
    PpuReg,    // $2000-$2007
    PpuMirror, // $2008-$3FFF — masked to $2000-$2007
    OamDma,    // $4014
    ApuIo,     // $4000-$4013, $4015-$4017
    Mapper,    // $4020-$5FFF
    PrgRam,    // $6000-$7FFF
    PrgRom,    // $8000-$FFFF
    Unknown,   // anything outside the above (shouldn't happen, but be safe)
}

pub fn classify_addr(addr: u16) -> MemRegion {
    match addr {
        0x0000..=0x00FF => MemRegion::ZeroPage,
        0x0100..=0x01FF => MemRegion::Stack,
        0x0200..=0x07FF => MemRegion::Ram,
        0x0800..=0x1FFF => MemRegion::RamMirror,
        0x2000..=0x2007 => MemRegion::PpuReg,
        0x2008..=0x3FFF => MemRegion::PpuMirror,
        0x4014 => MemRegion::OamDma,
        0x4000..=0x4013 => MemRegion::ApuIo,
        0x4015..=0x4017 => MemRegion::ApuIo,
        0x4018..=0x401F => MemRegion::ApuIo,
        0x4020..=0x5FFF => MemRegion::Mapper,
        0x6000..=0x7FFF => MemRegion::PrgRam,
        0x8000..=0xFFFF => MemRegion::PrgRom,
        #[allow(unreachable_patterns)]
        _ => MemRegion::Unknown,
    }
}

// ---------------------------------------------------------------------------
// AddrExpr
// ---------------------------------------------------------------------------

/// Effective addresses produced by the lifter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddrExpr {
    /// Constant 16-bit address.
    Const(u16),
    /// Zero-page constant (8-bit operand).
    ZpConst(u8),
    /// Zero-page indexed by X (wraps within zero page).
    ZpIndexedX(u8),
    /// Zero-page indexed by Y (wraps within zero page).
    ZpIndexedY(u8),
    /// Absolute indexed by X.
    AbsIndexedX(u16),
    /// Absolute indexed by Y.
    AbsIndexedY(u16),
    /// (zp,X): read16-zpwrap at zp+X, then dereference.
    IndirectX(u8),
    /// (zp),Y: read16-zpwrap at zp, then + Y.
    IndirectY(u8),
}

impl AddrExpr {
    /// If the address is a compile-time constant, return it. Otherwise None.
    pub fn const_addr(&self) -> Option<u16> {
        match self {
            AddrExpr::Const(a) => Some(*a),
            AddrExpr::ZpConst(a) => Some(*a as u16),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Flag / Cond
// ---------------------------------------------------------------------------

/// 6502 status flag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    C,
    Z,
    I,
    D,
    V,
    N,
}

/// Branch condition (relative to the 6502 flags shadow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Carry,
    NoCarry,
    Zero,
    NotZero,
    Negative,
    Positive,
    Overflow,
    NoOverflow,
}

// ---------------------------------------------------------------------------
// ValueSrc
// ---------------------------------------------------------------------------

/// Operand source for hardware writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueSrc {
    A,
    X,
    Y,
    Imm(u8),
    Mem { addr: AddrExpr, region: MemRegion },
}

// ---------------------------------------------------------------------------
// Op
// ---------------------------------------------------------------------------

/// One IR operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Block label / branch target.
    Label(String),

    /// 6502 source debug marker.
    Source {
        pc: u16,
        text: String,
    },

    // Loads / stores
    LdaImm(u8),
    LdaMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    LdxImm(u8),
    LdxMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    LdyImm(u8),
    LdyMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    StaMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    StxMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    StyMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    /// SAX: write (A & X) to memory; no flag changes; A and X preserved.
    SaxMem {
        addr: AddrExpr,
        region: MemRegion,
    },

    // Transfers
    Tax,
    Tay,
    Txa,
    Tya,
    Tsx,
    Txs,

    // Stack
    Pha,
    Php,
    Pla,
    Plp,

    // ALU on A with immediate or memory
    AdcImm(u8),
    AdcMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    SbcImm(u8),
    SbcMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    AndImm(u8),
    AndMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    OraImm(u8),
    OraMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    EorImm(u8),
    EorMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    CmpImm(u8),
    CmpMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    CpxImm(u8),
    CpxMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    CpyImm(u8),
    CpyMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    BitMem {
        addr: AddrExpr,
        region: MemRegion,
    },

    // Shifts / rotates
    AslA,
    AslMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    LsrA,
    LsrMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    RolA,
    RolMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    RorA,
    RorMem {
        addr: AddrExpr,
        region: MemRegion,
    },

    // Increment / decrement
    IncMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    DecMem {
        addr: AddrExpr,
        region: MemRegion,
    },
    Inx,
    Iny,
    Dex,
    Dey,

    // Flag operations
    Clc,
    Sec,
    Cli,
    Sei,
    Clv,
    Cld,
    Sed,

    // Branches / jumps
    BranchIf {
        cond: Cond,
        target: String,
    },
    Jmp {
        target: String,
    },
    JmpIndirect {
        addr: u16,
    },
    Jsr {
        target: String,
    },
    JsrUnknown {
        addr: u16,
    },
    /// `JSR JumpEngine`-style dispatch: pick a target by `(A * 2)` into
    /// a 2-byte pointer table and jump. Replaces `JSR` + inline `.dd2`
    /// targets so the lowered Z80 doesn't depend on the emulated 6502
    /// stack having the return address (which JumpEngine pops as data).
    JumpEngineCall {
        targets: Vec<String>,
    },
    Rts,
    Rti,

    // Hardware-tagged ops
    PpuWrite {
        reg: u8,
        value: ValueSrc,
    },
    PpuRead {
        reg: u8,
    },
    OamDmaWrite {
        value: ValueSrc,
    },
    ApuWrite {
        reg: u16,
        value: ValueSrc,
    },
    ApuRead {
        reg: u16,
    },
    MapperWrite {
        addr: u16,
        value: ValueSrc,
    },
    ControllerRead {
        port: u16,
    },

    // Catch-all for unhandled instructions
    Unsupported {
        pc: u16,
        opcode: u8,
        mnemonic: String,
        reason: String,
    },

    // Misc
    Nop,
    Brk,
    Jam {
        pc: u16,
        opcode: u8,
    },
}

// ---------------------------------------------------------------------------
// Routine
// ---------------------------------------------------------------------------

/// A lifted IR routine.
#[derive(Debug, Clone)]
pub struct Routine {
    pub entry: u16,
    /// CPU address immediately after the last decoded instruction in this
    /// lifted routine. This can be greater than `LiftOptions::end` when the
    /// final decoded instruction overlaps the next known root (for example
    /// SMB's `BIT abs` skip idiom where the alternate entry is inside the
    /// BIT operand bytes).
    pub end: u16,
    pub name: String,
    pub ops: Vec<Op>,
    pub branch_labels: Vec<String>,
    pub external_calls: Vec<String>,
    pub unresolved: Vec<u16>,
}

// ---------------------------------------------------------------------------
// LiftOptions / LiftError
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LiftOptions {
    pub start: u16,
    pub end: u16,
    pub entry_name: String,
    /// Optional set of JumpEngine call sites. When the lifter sees a
    /// `JSR` at one of these PCs, it substitutes a `JumpEngineCall`
    /// op carrying the listed targets instead of a plain `Jsr`.
    pub jump_engine_sites: Vec<JumpEngineSite>,
    /// PCs that other routines branch to which fall inside our range.
    /// We emit `Op::Label(L_<pc>)` at each so the cross-routine
    /// reference resolves. Without this, a `BEQ $85C8` from one
    /// routine would land at an unresolved-stub even when `$85C8`
    /// sits inside the next routine's body.
    pub extra_label_pcs: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct JumpEngineSite {
    pub caller: u16,
    pub targets: Vec<String>,
}

impl Default for LiftOptions {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            entry_name: String::new(),
            jump_engine_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum LiftError {
    Decode(String),
    OutsideRange { pc: u16 },
    Truncated { pc: u16 },
}

impl std::fmt::Display for LiftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiftError::Decode(s) => write!(f, "decode error: {s}"),
            LiftError::OutsideRange { pc } => write!(f, "PC ${pc:04X} outside lift range"),
            LiftError::Truncated { pc } => write!(f, "truncated instruction at ${pc:04X}"),
        }
    }
}

impl std::error::Error for LiftError {}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a CPU address to a byte offset into the PRG slice (NROM: $8000..$FFFF).
fn cpu_to_prg_offset(addr: u16) -> Option<usize> {
    if addr >= 0x8000 {
        Some((addr - 0x8000) as usize)
    } else {
        None
    }
}

fn label_for(addr: u16) -> String {
    format!("L_{addr:04X}")
}

fn branch_cond(mnem: Mnemonic) -> Cond {
    match mnem {
        Mnemonic::BCC => Cond::NoCarry,
        Mnemonic::BCS => Cond::Carry,
        Mnemonic::BEQ => Cond::Zero,
        Mnemonic::BNE => Cond::NotZero,
        Mnemonic::BMI => Cond::Negative,
        Mnemonic::BPL => Cond::Positive,
        Mnemonic::BVC => Cond::NoOverflow,
        Mnemonic::BVS => Cond::Overflow,
        _ => unreachable!("not a branch mnemonic"),
    }
}

/// Build AddrExpr + base u16 from instruction mode/operand.
/// Returns (expr, base_addr) where base_addr is used for MemRegion classification.
fn make_addr_expr(insn: &Instruction) -> Option<(AddrExpr, u16)> {
    match (insn.mode, insn.operand) {
        (AddrMode::ZeroPage, Operand::Addr(a)) => Some((AddrExpr::ZpConst(a as u8), a)),
        (AddrMode::ZeroPageX, Operand::Addr(a)) => Some((AddrExpr::ZpIndexedX(a as u8), a)),
        (AddrMode::ZeroPageY, Operand::Addr(a)) => Some((AddrExpr::ZpIndexedY(a as u8), a)),
        (AddrMode::Absolute, Operand::Addr(a)) => Some((AddrExpr::Const(a), a)),
        (AddrMode::AbsoluteX, Operand::Addr(a)) => Some((AddrExpr::AbsIndexedX(a), a)),
        (AddrMode::AbsoluteY, Operand::Addr(a)) => Some((AddrExpr::AbsIndexedY(a), a)),
        (AddrMode::IndirectX, Operand::Addr(a)) => Some((AddrExpr::IndirectX(a as u8), a)),
        (AddrMode::IndirectY, Operand::Addr(a)) => Some((AddrExpr::IndirectY(a as u8), a)),
        _ => None,
    }
}

/// Lift a single instruction into one or more IR ops.
fn lift_insn(
    insn: &Instruction,
    opts: &LiftOptions,
    branch_labels: &mut Vec<String>,
    external_calls: &mut Vec<String>,
    unresolved: &mut Vec<u16>,
    _internal_targets: &HashSet<u16>,
) -> Vec<Op> {
    let pc = insn.pc;

    // Helper: record a label (internal or external)
    let record_target =
        |target: u16, branch_labels: &mut Vec<String>, external_calls: &mut Vec<String>| {
            let lbl = label_for(target);
            if target >= opts.start && target < opts.end {
                if !branch_labels.contains(&lbl) {
                    branch_labels.push(lbl.clone());
                }
            } else {
                if !external_calls.contains(&lbl) {
                    external_calls.push(lbl.clone());
                }
            }
            lbl
        };

    match insn.mnemonic {
        // ---- Nop / Brk / Jam ----
        Mnemonic::NOP => return vec![Op::Nop],
        Mnemonic::BRK => return vec![Op::Brk],
        Mnemonic::JAM => {
            return vec![Op::Jam {
                pc,
                opcode: insn.opcode,
            }];
        }

        // ---- RTS / RTI ----
        Mnemonic::RTS => return vec![Op::Rts],
        Mnemonic::RTI => return vec![Op::Rti],

        // ---- Flag ops ----
        Mnemonic::CLC => return vec![Op::Clc],
        Mnemonic::SEC => return vec![Op::Sec],
        Mnemonic::CLI => return vec![Op::Cli],
        Mnemonic::SEI => return vec![Op::Sei],
        Mnemonic::CLV => return vec![Op::Clv],
        Mnemonic::CLD => return vec![Op::Cld],
        Mnemonic::SED => return vec![Op::Sed],

        // ---- Transfers ----
        Mnemonic::TAX => return vec![Op::Tax],
        Mnemonic::TAY => return vec![Op::Tay],
        Mnemonic::TXA => return vec![Op::Txa],
        Mnemonic::TYA => return vec![Op::Tya],
        Mnemonic::TSX => return vec![Op::Tsx],
        Mnemonic::TXS => return vec![Op::Txs],

        // ---- Stack ----
        Mnemonic::PHA => return vec![Op::Pha],
        Mnemonic::PHP => return vec![Op::Php],
        Mnemonic::PLA => return vec![Op::Pla],
        Mnemonic::PLP => return vec![Op::Plp],

        // ---- Inc / Dec register ----
        Mnemonic::INX => return vec![Op::Inx],
        Mnemonic::INY => return vec![Op::Iny],
        Mnemonic::DEX => return vec![Op::Dex],
        Mnemonic::DEY => return vec![Op::Dey],

        // ---- Unstable unofficials ----
        Mnemonic::XAA
        | Mnemonic::AHX
        | Mnemonic::TAS
        | Mnemonic::LAS
        | Mnemonic::SHX
        | Mnemonic::SHY
        | Mnemonic::ANE => {
            return vec![Op::Unsupported {
                pc,
                opcode: insn.opcode,
                mnemonic: format!("{:?}", insn.mnemonic),
                reason: "unstable opcode".to_string(),
            }];
        }

        // ---- Stable unofficial: SAX, DCP, ISC, SLO, RLA, SRE, RRA ----
        // Each decomposes to an existing memory-modify + ALU sequence.
        // SAX is the only one that doesn't fit the existing op set; for
        // it we emit Unsupported until we add a dedicated op.
        Mnemonic::DCP
        | Mnemonic::ISC
        | Mnemonic::SLO
        | Mnemonic::RLA
        | Mnemonic::SRE
        | Mnemonic::RRA => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return match insn.mnemonic {
                    Mnemonic::DCP => vec![
                        Op::DecMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::CmpMem { addr, region },
                    ],
                    Mnemonic::ISC => vec![
                        Op::IncMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::SbcMem { addr, region },
                    ],
                    Mnemonic::SLO => vec![
                        Op::AslMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::OraMem { addr, region },
                    ],
                    Mnemonic::RLA => vec![
                        Op::RolMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::AndMem { addr, region },
                    ],
                    Mnemonic::SRE => vec![
                        Op::LsrMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::EorMem { addr, region },
                    ],
                    Mnemonic::RRA => vec![
                        Op::RorMem {
                            addr: addr.clone(),
                            region,
                        },
                        Op::AdcMem { addr, region },
                    ],
                    _ => unreachable!(),
                };
            }
            return vec![Op::Unsupported {
                pc,
                opcode: insn.opcode,
                mnemonic: format!("{:?}", insn.mnemonic),
                reason: "unofficial opcode with unhandled addressing mode".to_string(),
            }];
        }
        // SAX = M := A & X. No flag changes.
        Mnemonic::SAX => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::SaxMem { addr, region }];
            }
            return vec![Op::Unsupported {
                pc,
                opcode: insn.opcode,
                mnemonic: format!("{:?}", insn.mnemonic),
                reason: "SAX with unsupported addressing mode".to_string(),
            }];
        }

        // ---- LAX: LDA then TAX (semantically A = X = load) ----
        Mnemonic::LAX => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![
                    Op::LdaMem {
                        addr: addr.clone(),
                        region,
                    },
                    Op::Tax,
                ];
            } else {
                return vec![Op::Unsupported {
                    pc,
                    opcode: insn.opcode,
                    mnemonic: "LAX".to_string(),
                    reason: "unexpected addressing mode".to_string(),
                }];
            }
        }

        // ---- Branches ----
        Mnemonic::BCC
        | Mnemonic::BCS
        | Mnemonic::BEQ
        | Mnemonic::BNE
        | Mnemonic::BMI
        | Mnemonic::BPL
        | Mnemonic::BVC
        | Mnemonic::BVS => {
            let target = insn.branch_target().expect("branch always has target");
            let cond = branch_cond(insn.mnemonic);
            let lbl = record_target(target, branch_labels, external_calls);
            return vec![Op::BranchIf { cond, target: lbl }];
        }

        // ---- JMP ----
        Mnemonic::JMP => match insn.mode {
            AddrMode::Absolute => {
                if let Operand::Addr(target) = insn.operand {
                    let lbl = record_target(target, branch_labels, external_calls);
                    return vec![Op::Jmp { target: lbl }];
                }
            }
            AddrMode::Indirect => {
                if let Operand::Addr(addr) = insn.operand {
                    if !unresolved.contains(&addr) {
                        unresolved.push(addr);
                    }
                    return vec![Op::JmpIndirect { addr }];
                }
            }
            _ => {}
        },

        // ---- JSR ----
        Mnemonic::JSR => {
            if let Operand::Addr(target) = insn.operand {
                // Profile may declare this JSR as a JumpEngine-style
                // dispatch site. In that case substitute the JSR with
                // a JumpEngineCall op carrying the target list — the
                // lowering picks a target by A and jumps directly,
                // bypassing the 6502-stack-trick the real JumpEngine
                // routine would use.
                if let Some(site) = opts.jump_engine_sites.iter().find(|s| s.caller == pc) {
                    return vec![Op::JumpEngineCall {
                        targets: site.targets.clone(),
                    }];
                }
                let lbl = record_target(target, branch_labels, external_calls);
                return vec![Op::Jsr { target: lbl }];
            }
        }

        // ---- ASL ----
        Mnemonic::ASL => match insn.mode {
            AddrMode::Accumulator | AddrMode::Implied => return vec![Op::AslA],
            _ => {
                if let Some((addr, base)) = make_addr_expr(insn) {
                    let region = classify_addr(base);
                    return vec![Op::AslMem { addr, region }];
                }
            }
        },

        // ---- LSR ----
        Mnemonic::LSR => match insn.mode {
            AddrMode::Accumulator | AddrMode::Implied => return vec![Op::LsrA],
            _ => {
                if let Some((addr, base)) = make_addr_expr(insn) {
                    let region = classify_addr(base);
                    return vec![Op::LsrMem { addr, region }];
                }
            }
        },

        // ---- ROL ----
        Mnemonic::ROL => match insn.mode {
            AddrMode::Accumulator | AddrMode::Implied => return vec![Op::RolA],
            _ => {
                if let Some((addr, base)) = make_addr_expr(insn) {
                    let region = classify_addr(base);
                    return vec![Op::RolMem { addr, region }];
                }
            }
        },

        // ---- ROR ----
        Mnemonic::ROR => match insn.mode {
            AddrMode::Accumulator | AddrMode::Implied => return vec![Op::RorA],
            _ => {
                if let Some((addr, base)) = make_addr_expr(insn) {
                    let region = classify_addr(base);
                    return vec![Op::RorMem { addr, region }];
                }
            }
        },

        // ---- LDA ----
        Mnemonic::LDA => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::LdaImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                // Controller reads must be checked before the generic ApuIo arm.
                if let Some(ca) = addr.const_addr() {
                    if ca == 0x4016 || ca == 0x4017 {
                        return vec![Op::ControllerRead { port: ca }];
                    }
                }
                let region = classify_addr(base);
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuRead {
                        reg: (base & 0x07) as u8,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuRead {
                        reg: (base & 0x07) as u8,
                    }],
                    // Indexed controller reads (`LDA $4016,X`) need the X
                    // value at runtime to choose port 1 vs port 2, so keep
                    // the address expression instead of collapsing to a raw
                    // APU read at the base address.
                    MemRegion::ApuIo if matches!(addr, AddrExpr::AbsIndexedX(0x4016)) => {
                        vec![Op::LdaMem { addr, region }]
                    }
                    MemRegion::ApuIo => vec![Op::ApuRead { reg: base }],
                    MemRegion::OamDma => vec![Op::ApuRead { reg: base }],
                    _ => vec![Op::LdaMem { addr, region }],
                };
            }
        }

        // ---- LDX ----
        Mnemonic::LDX => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::LdxImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::LdxMem { addr, region }];
            }
        }

        // ---- LDY ----
        Mnemonic::LDY => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::LdyImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::LdyMem { addr, region }];
            }
        }

        // ---- STA ----
        Mnemonic::STA => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::A,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::A,
                    }],
                    MemRegion::OamDma => vec![Op::OamDmaWrite { value: ValueSrc::A }],
                    MemRegion::ApuIo => {
                        if let Some(ca) = addr.const_addr() {
                            if ca == 0x4016 || ca == 0x4017 {
                                // Controller strobe write
                                return vec![Op::ApuWrite {
                                    reg: ca,
                                    value: ValueSrc::A,
                                }];
                            }
                        }
                        if let Some(ca) = addr.const_addr() {
                            return vec![Op::ApuWrite {
                                reg: ca,
                                value: ValueSrc::A,
                            }];
                        }
                        // Indexed APU store (e.g. SMB's `STA $4002,X` with
                        // X = channel offset): keep the address expression —
                        // folding to the base register silently redirected
                        // square-2/triangle/noise writes onto square 1. The
                        // lowered indexed store goes through
                        // rt_write_indexed, which forwards $4000-$4017
                        // targets to the APU shim at runtime.
                        vec![Op::StaMem { addr, region }]
                    }
                    MemRegion::Mapper | MemRegion::PrgRam => {
                        if let Some(ca) = addr.const_addr() {
                            vec![Op::MapperWrite {
                                addr: ca,
                                value: ValueSrc::A,
                            }]
                        } else {
                            vec![Op::StaMem { addr, region }]
                        }
                    }
                    _ => vec![Op::StaMem { addr, region }],
                };
            }
        }

        // ---- STX ----
        Mnemonic::STX => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::X,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::X,
                    }],
                    _ => vec![Op::StxMem { addr, region }],
                };
            }
        }

        // ---- STY ----
        Mnemonic::STY => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::Y,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::Y,
                    }],
                    _ => vec![Op::StyMem { addr, region }],
                };
            }
        }

        // ---- ADC ----
        Mnemonic::ADC => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::AdcImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::AdcMem { addr, region }];
            }
        }

        // ---- SBC ----
        Mnemonic::SBC => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::SbcImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::SbcMem { addr, region }];
            }
        }

        // ---- AND ----
        Mnemonic::AND => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::AndImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::AndMem { addr, region }];
            }
        }

        // ---- ORA ----
        Mnemonic::ORA => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::OraImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::OraMem { addr, region }];
            }
        }

        // ---- EOR ----
        Mnemonic::EOR => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::EorImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::EorMem { addr, region }];
            }
        }

        // ---- CMP ----
        Mnemonic::CMP => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::CmpImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::CmpMem { addr, region }];
            }
        }

        // ---- CPX ----
        Mnemonic::CPX => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::CpxImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::CpxMem { addr, region }];
            }
        }

        // ---- CPY ----
        Mnemonic::CPY => {
            if let Operand::Imm(v) = insn.operand {
                return vec![Op::CpyImm(v)];
            }
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::CpyMem { addr, region }];
            }
        }

        // ---- BIT ----
        Mnemonic::BIT => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::BitMem { addr, region }];
            }
        }

        // ---- INC / DEC memory ----
        Mnemonic::INC => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::IncMem { addr, region }];
            }
        }
        Mnemonic::DEC => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                return vec![Op::DecMem { addr, region }];
            }
        }
    }

    // Fallback: emit unsupported
    vec![Op::Unsupported {
        pc,
        opcode: insn.opcode,
        mnemonic: format!("{:?}", insn.mnemonic),
        reason: "unhandled addressing mode or operand".to_string(),
    }]
}

// ---------------------------------------------------------------------------
// lift_range
// ---------------------------------------------------------------------------

/// Lift a contiguous byte range as one routine.
///
/// The PRG slice is the full PRG (assumed to live at $8000..=$FFFF).
/// `start` and `end` are CPU addresses.
pub fn lift_range(prg: &[u8], opts: &LiftOptions) -> Result<Routine, LiftError> {
    if opts.start >= opts.end {
        return Err(LiftError::OutsideRange { pc: opts.start });
    }

    // ---- Pass 1: collect internal branch targets ----
    let mut internal_targets: HashSet<u16> = HashSet::new();
    {
        let mut pc = opts.start;
        while pc < opts.end {
            let offset = cpu_to_prg_offset(pc).ok_or(LiftError::OutsideRange { pc })?;
            let insn = decode_at(prg, pc, offset).map_err(|_| LiftError::Truncated { pc })?;

            if insn.is_branch() {
                if let Some(target) = insn.branch_target() {
                    if target >= opts.start && target < opts.end {
                        internal_targets.insert(target);
                    }
                }
            }
            // Also mark JMP absolute targets inside range
            if insn.mnemonic == Mnemonic::JMP {
                if let (AddrMode::Absolute, Operand::Addr(target)) = (insn.mode, insn.operand) {
                    if target >= opts.start && target < opts.end {
                        internal_targets.insert(target);
                    }
                }
            }

            pc = pc.wrapping_add(insn.size as u16);
        }
    }

    // ---- Pass 2: emit ops ----
    let mut ops: Vec<Op> = Vec::new();
    let mut branch_labels: Vec<String> = Vec::new();
    let mut external_calls: Vec<String> = Vec::new();
    let mut unresolved: Vec<u16> = Vec::new();

    // Emit entry label
    ops.push(Op::Label(opts.entry_name.clone()));

    // Combine the lifter's own intra-routine branch targets with any
    // cross-routine PCs the pipeline asked us to expose as labels.
    let mut all_targets = internal_targets.clone();
    for &pc in &opts.extra_label_pcs {
        if pc >= opts.start && pc < opts.end {
            all_targets.insert(pc);
        }
    }

    let mut pc = opts.start;
    let mut lifted_end = opts.start;
    while pc < opts.end {
        // Emit internal label if this PC is a branch target
        if all_targets.contains(&pc) {
            let lbl = label_for(pc);
            ops.push(Op::Label(lbl.clone()));
            if !branch_labels.contains(&lbl) {
                branch_labels.push(lbl);
            }
        }

        let offset = cpu_to_prg_offset(pc).ok_or(LiftError::OutsideRange { pc })?;
        let insn = decode_at(prg, pc, offset).map_err(|_| LiftError::Truncated { pc })?;

        // Source marker
        ops.push(Op::Source {
            pc,
            text: format_instruction(&insn),
        });

        // Lift
        let lifted = lift_insn(
            &insn,
            opts,
            &mut branch_labels,
            &mut external_calls,
            &mut unresolved,
            &internal_targets,
        );
        ops.extend(lifted);

        let next_pc = pc.wrapping_add(insn.size as u16);
        lifted_end = next_pc;

        // If this is a hard terminator and there's nothing reachable after,
        // stop early (unless the next PC is a branch target we still need to emit).
        if insn.is_terminator() && !internal_targets.contains(&next_pc) {
            break;
        }

        pc = next_pc;
    }

    Ok(Routine {
        entry: opts.start,
        end: lifted_end,
        name: opts.entry_name.clone(),
        ops,
        branch_labels,
        external_calls,
        unresolved,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: build a PRG slice with bytes starting at a given CPU address.
    // Pads the rest with 0xFF (unofficial nop-ish / jam territory, but
    // we don't care since we bound the range).
    fn make_prg_at(cpu_start: u16, bytes: &[u8]) -> Vec<u8> {
        let prg_size = 0x8000usize; // 32 KiB
        let mut prg = vec![0xFFu8; prg_size];
        let offset = (cpu_start as usize).saturating_sub(0x8000);
        let end = (offset + bytes.len()).min(prg_size);
        prg[offset..end].copy_from_slice(&bytes[..end - offset]);
        prg
    }

    fn lift(cpu_start: u16, bytes: &[u8]) -> Routine {
        let prg = make_prg_at(cpu_start, bytes);
        let opts = LiftOptions {
            start: cpu_start,
            end: cpu_start + bytes.len() as u16,
            entry_name: format!("L_{cpu_start:04X}"),
            extra_label_pcs: Vec::new(),
            jump_engine_sites: Vec::new(),
        };
        lift_range(&prg, &opts).expect("lift failed")
    }

    // ------ classify_addr tests ------

    #[test]
    fn classify_zero_page() {
        assert_eq!(classify_addr(0x0050), MemRegion::ZeroPage);
    }

    #[test]
    fn classify_stack() {
        assert_eq!(classify_addr(0x0150), MemRegion::Stack);
    }

    #[test]
    fn classify_ram() {
        assert_eq!(classify_addr(0x0300), MemRegion::Ram);
    }

    #[test]
    fn classify_ram_mirror() {
        assert_eq!(classify_addr(0x1000), MemRegion::RamMirror);
    }

    #[test]
    fn classify_ppu_reg() {
        assert_eq!(classify_addr(0x2004), MemRegion::PpuReg);
    }

    #[test]
    fn classify_ppu_mirror() {
        assert_eq!(classify_addr(0x2010), MemRegion::PpuMirror);
    }

    #[test]
    fn classify_oam_dma() {
        assert_eq!(classify_addr(0x4014), MemRegion::OamDma);
    }

    #[test]
    fn classify_apu_io() {
        assert_eq!(classify_addr(0x4000), MemRegion::ApuIo);
        assert_eq!(classify_addr(0x4015), MemRegion::ApuIo);
        assert_eq!(classify_addr(0x4016), MemRegion::ApuIo);
    }

    #[test]
    fn classify_mapper() {
        assert_eq!(classify_addr(0x5000), MemRegion::Mapper);
    }

    #[test]
    fn classify_prg_ram() {
        assert_eq!(classify_addr(0x6800), MemRegion::PrgRam);
    }

    #[test]
    fn classify_prg_rom() {
        assert_eq!(classify_addr(0xC000), MemRegion::PrgRom);
    }

    // ------ Lift single-instruction tests ------

    #[test]
    fn lift_lda_imm() {
        // A9 42 — LDA #$42
        let r = lift(0x8000, &[0xA9, 0x42]);
        assert!(r.ops.contains(&Op::LdaImm(0x42)));
    }

    #[test]
    fn lift_lda_ppu_reg() {
        // AD 03 20 — LDA $2003
        let r = lift(0x8000, &[0xAD, 0x03, 0x20]);
        assert!(r.ops.contains(&Op::PpuRead { reg: 3 }));
    }

    #[test]
    fn lift_sta_ppu_reg() {
        // 8D 06 20 — STA $2006
        let r = lift(0x8000, &[0x8D, 0x06, 0x20]);
        assert!(r.ops.contains(&Op::PpuWrite {
            reg: 6,
            value: ValueSrc::A
        }));
    }

    #[test]
    fn lift_sta_oam_dma() {
        // 8D 14 40 — STA $4014
        let r = lift(0x8000, &[0x8D, 0x14, 0x40]);
        assert!(r.ops.contains(&Op::OamDmaWrite { value: ValueSrc::A }));
    }

    #[test]
    fn lift_lda_controller() {
        // AD 16 40 — LDA $4016
        let r = lift(0x8000, &[0xAD, 0x16, 0x40]);
        assert!(r.ops.contains(&Op::ControllerRead { port: 0x4016 }));
    }

    #[test]
    fn lift_sta_apu() {
        // 8D 00 40 — STA $4000
        let r = lift(0x8000, &[0x8D, 0x00, 0x40]);
        assert!(r.ops.contains(&Op::ApuWrite {
            reg: 0x4000,
            value: ValueSrc::A
        }));
    }

    #[test]
    fn lift_bne_with_branch_label() {
        // D0 04 A9 06 85 0E 60
        // BNE $8006, LDA #$06, STA $0E (zp), RTS
        // $8000: D0 04  -> BNE $8006
        // $8002: A9 06  -> LDA #$06
        // $8004: 85 0E  -> STA $0E
        // $8006: 60     -> RTS
        let bytes = &[0xD0, 0x04, 0xA9, 0x06, 0x85, 0x0E, 0x60];
        let r = lift(0x8000, bytes);

        assert!(r.branch_labels.contains(&"L_8006".to_string()));
        assert!(r.ops.contains(&Op::BranchIf {
            cond: Cond::NotZero,
            target: "L_8006".to_string()
        }));
        assert!(r.ops.contains(&Op::LdaImm(6)));
        assert!(r.ops.contains(&Op::StaMem {
            addr: AddrExpr::ZpConst(0x0E),
            region: MemRegion::ZeroPage,
        }));
        // Label L_8006 should appear before Rts
        let label_pos = r
            .ops
            .iter()
            .position(|o| o == &Op::Label("L_8006".to_string()))
            .unwrap();
        let rts_pos = r.ops.iter().position(|o| o == &Op::Rts).unwrap();
        assert!(label_pos < rts_pos);
        assert!(r.ops.contains(&Op::Rts));
    }

    #[test]
    fn lift_external_branch_target() {
        // C9 03 B0 01 60
        // $AEF9: CMP #$03
        // $AEFB: BCS $AEFD (inside range is $AEF9..$AEFE so $AEFD is inside)
        // Actually: range $AEF9..$AEFE is 5 bytes, target = $AEFB+2+1 = $AEFE which is outside
        // Let's recalculate: BCS with rel=01 at $AEFB -> target = $AEFB+2+1 = $AEFE
        // Range is $AEF9..($AEF9+5)=$AEFE so $AEFE is NOT in [start, end) → external
        let bytes = &[0xC9, 0x03, 0xB0, 0x01, 0x60];
        let prg = make_prg_at(0xAEF9, bytes);
        let opts = LiftOptions {
            start: 0xAEF9,
            end: 0xAEFE,
            entry_name: "L_AEF9".to_string(),
            jump_engine_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        };
        let r = lift_range(&prg, &opts).unwrap();
        assert!(r.external_calls.contains(&"L_AEFE".to_string()));
    }

    #[test]
    fn lift_jsr_external() {
        // 20 34 12 60 — JSR $1234, RTS
        let r = lift(0x8000, &[0x20, 0x34, 0x12, 0x60]);
        assert!(r.external_calls.contains(&"L_1234".to_string()));
        assert!(r.ops.contains(&Op::Jsr {
            target: "L_1234".to_string()
        }));
    }

    #[test]
    fn lift_jmp_indirect() {
        // 6C 00 30 — JMP ($3000)
        let r = lift(0x8000, &[0x6C, 0x00, 0x30]);
        assert!(r.ops.contains(&Op::JmpIndirect { addr: 0x3000 }));
        assert!(r.unresolved.contains(&0x3000));
    }

    #[test]
    fn lift_jmp_absolute_internal() {
        // 4C 00 80 — JMP $8000 (self-loop, target is inside range)
        let bytes = &[0x4C, 0x00, 0x80];
        let r = lift(0x8000, bytes);
        assert!(r.ops.contains(&Op::Jmp {
            target: "L_8000".to_string()
        }));
    }

    #[test]
    fn lift_instruction_can_overlap_trimmed_end() {
        // SMB uses `BIT abs` as a 3-byte skip where an alternate entry starts
        // inside the BIT operand bytes:
        //   E3E9: A9 00     LDA #$00
        //   E3EB: 2C A9 01  BIT $01A9   ; consumes the side-entry LDA bytes
        //   E3EC: A9 01     LDA #$01    ; alternate entry, not fallthrough
        //   E3EE: A2 00     LDX #$00    ; real fallthrough for E3E9
        let prg = make_prg_at(0xE3E9, &[0xA9, 0x00, 0x2C, 0xA9, 0x01, 0xA2, 0x00]);
        let r = lift_range(
            &prg,
            &LiftOptions {
                start: 0xE3E9,
                end: 0xE3EC,
                entry_name: "L_E3E9".into(),
                jump_engine_sites: Vec::new(),
                extra_label_pcs: Vec::new(),
            },
        )
        .expect("lift");

        assert_eq!(r.end, 0xE3EE);
        assert!(r.ops.contains(&Op::LdaImm(0x00)));
        assert!(r.ops.contains(&Op::BitMem {
            addr: AddrExpr::Const(0x01A9),
            region: MemRegion::Stack,
        }));
        assert!(!r.ops.contains(&Op::LdaImm(0x01)));
    }

    #[test]
    fn lift_countdown_loop() {
        // A2 0A     LDX #$0A   ($8000)
        // CA        DEX        ($8002)
        // D0 FD     BNE $8002  ($8003) — rel=-3, target=$8002
        // 60        RTS        ($8005)
        let bytes = &[0xA2, 0x0A, 0xCA, 0xD0, 0xFD, 0x60];
        let r = lift(0x8000, bytes);
        assert!(r.ops.contains(&Op::LdxImm(0x0A)));
        assert!(r.ops.contains(&Op::Dex));
        assert!(r.ops.contains(&Op::BranchIf {
            cond: Cond::NotZero,
            target: "L_8002".to_string()
        }));
        assert!(r.branch_labels.contains(&"L_8002".to_string()));
        // The label should appear in the ops at pc $8002
        let label_pos = r
            .ops
            .iter()
            .position(|o| o == &Op::Label("L_8002".to_string()))
            .unwrap();
        let dex_source = r
            .ops
            .iter()
            .position(|o| matches!(o, Op::Source { pc: 0x8002, .. }))
            .unwrap();
        // label appears before (or at) the DEX source marker at $8002
        assert!(label_pos <= dex_source);
    }

    #[test]
    fn lift_official_nop() {
        // EA — NOP
        let r = lift(0x8000, &[0xEA]);
        assert!(r.ops.contains(&Op::Nop));
    }

    #[test]
    fn lift_unofficial_nop_1a() {
        // 1A — unofficial 1-byte NOP
        let r = lift(0x8000, &[0x1A]);
        assert!(r.ops.contains(&Op::Nop));
    }

    #[test]
    fn lift_jam() {
        // 02 — JAM
        let r = lift(0x8000, &[0x02]);
        assert!(r.ops.contains(&Op::Jam {
            pc: 0x8000,
            opcode: 0x02
        }));
    }

    #[test]
    fn lift_xaa_unsupported() {
        // 8B FF — XAA/ANE imm
        let r = lift(0x8000, &[0x8B, 0xFF]);
        let has_unsupported = r
            .ops
            .iter()
            .any(|o| matches!(o, Op::Unsupported { reason, .. } if reason == "unstable opcode"));
        assert!(has_unsupported);
    }

    #[test]
    fn lift_abs_indexed_x_prg_rom() {
        // BD 00 90 — LDA $9000,X  (base $9000 → PrgRom)
        let r = lift(0x8000, &[0xBD, 0x00, 0x90]);
        assert!(r.ops.contains(&Op::LdaMem {
            addr: AddrExpr::AbsIndexedX(0x9000),
            region: MemRegion::PrgRom,
        }));
    }

    #[test]
    fn lift_error_on_empty_range() {
        let prg = vec![0u8; 0x8000];
        let opts = LiftOptions {
            start: 0x8000,
            end: 0x8000,
            entry_name: "test".to_string(),
            jump_engine_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        };
        assert!(lift_range(&prg, &opts).is_err());
    }

    #[test]
    fn lift_error_start_gt_end() {
        let prg = vec![0u8; 0x8000];
        let opts = LiftOptions {
            start: 0x8010,
            end: 0x8000,
            entry_name: "test".to_string(),
            jump_engine_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        };
        assert!(lift_range(&prg, &opts).is_err());
    }

    #[test]
    fn addr_expr_const_addr() {
        assert_eq!(AddrExpr::Const(0x1234).const_addr(), Some(0x1234));
        assert_eq!(AddrExpr::ZpConst(0x42).const_addr(), Some(0x42));
        assert_eq!(AddrExpr::AbsIndexedX(0x5000).const_addr(), None);
        assert_eq!(AddrExpr::IndirectY(0x10).const_addr(), None);
    }
}
