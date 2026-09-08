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

    /// Original 6502 instruction identity and size, also used to derive guest
    /// return addresses. Optimizers must preserve this source metadata and its
    /// original-return semantics, not treat it as disposable debug text.
    Source {
        pc: u16,
        size: u8,
        text: String,
        /// Typed source identity for timing and bus sequencing. Pipeline lifts
        /// always retain it; hand-authored semantic-only fixtures may omit it.
        /// It must survive optimization even when source comments are disabled.
        instruction: Option<cpu6502::Instruction>,
    },

    /// Profile-verified idle poll. Signals scheduling intent without replacing
    /// the following original load, branch, or guest register effects.
    CooperativeWait {
        tick: u8,
        tick_enable: u8,
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
    /// Tail jump that first discards one translated-call continuation and
    /// either recreates the corresponding two 6502 JSR return bytes, or
    /// has already transferred ownership at ReturnEscapeConsume. The latter
    /// is an ordinary tail JMP; it must not read now-free guest stack bytes.
    ReturnEscape {
        target: String,
        return_addr: u16,
        stack_bytes_already_consumed: bool,
    },
    /// Validate and discard an owning software continuation before the first
    /// of its two guest PLAs, preserving all guest state and live stack bytes.
    ReturnEscapeConsume {
        return_addr: u16,
    },
    /// Retire one materialized ordinary-call owner before unchanged PLA/PLA.
    ReturnConsume {
        return_addrs: Vec<u16>,
    },
    JmpIndirect {
        addr: u16,
    },
    Jsr {
        target: String,
    },
    MaterializedJsr {
        target: String,
        return_addr: u16,
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
        return_target: Option<String>,
        tail_indices: Vec<usize>,
        stack_return_bytes: u8,
        target_entry_a: Vec<u8>,
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
    /// Store that targets mapper-adjacent space with unsupported semantics.
    /// This is distinct from generic unsupported instructions so pipeline
    /// generation can fail closed rather than emit a diagnostic stub.
    UnsupportedMapperStore {
        pc: u16,
        opcode: u8,
        mnemonic: String,
        reason: String,
    },
    /// RTS used as a computed jump (the 6502 `PHA hi / PHA lo / RTS`
    /// dispatch idiom): pops two bytes from the emulated 6502 stack and
    /// transfers to (target + 1) through the runtime banked dispatcher.
    /// Detected by the post-lift pass: an RTS whose basic block has two
    /// or more unmatched PHAs immediately before it.
    RtsDispatch,
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
    Brk {
        pc: u16,
    },
    Jam {
        pc: u16,
        opcode: u8,
    },
}

impl Op {
    /// Writes which may change an MMC3 PRG window. CHR, mirroring and IRQ
    /// registers do not change CPU instruction identity.
    pub fn may_remap_mmc3_prg(&self) -> bool {
        let address = match self {
            Op::MapperWrite { addr, .. } => return (0x8000..0xa000).contains(addr),
            Op::StaMem { addr, .. }
            | Op::StxMem { addr, .. }
            | Op::StyMem { addr, .. }
            | Op::SaxMem { addr, .. }
            | Op::AslMem { addr, .. }
            | Op::LsrMem { addr, .. }
            | Op::RolMem { addr, .. }
            | Op::RorMem { addr, .. }
            | Op::IncMem { addr, .. }
            | Op::DecMem { addr, .. } => addr,
            _ => return false,
        };
        match address {
            AddrExpr::Const(addr) => (0x8000..0xa000).contains(addr),
            AddrExpr::AbsIndexedX(base) | AddrExpr::AbsIndexedY(base) => {
                (0..=255).any(|index| (0x8000..0xa000).contains(&base.wrapping_add(index)))
            }
            AddrExpr::IndirectX(_) | AddrExpr::IndirectY(_) => true,
            _ => false,
        }
    }
    /// True when execution cannot fall through to the next translated op.
    pub fn is_hard_terminator(&self) -> bool {
        matches!(
            self,
            Op::Jmp { .. }
                | Op::ReturnEscape { .. }
                | Op::JmpIndirect { .. }
                | Op::JumpEngineCall { .. }
                | Op::Rts
                | Op::Rti
                | Op::RtsDispatch
                | Op::Brk { .. }
                | Op::Jam { .. }
        )
    }
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

impl Routine {
    /// Original next instruction, never an address inferred from Z80 size.
    pub fn next_source_pc(&self, op_index: usize) -> u16 {
        self.ops[..=op_index]
            .iter()
            .rev()
            .find_map(|op| match op {
                Op::Source { pc, size, .. } => Some(pc.wrapping_add(u16::from(*size))),
                _ => None,
            })
            .unwrap_or(self.end)
    }
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
    /// Profile-qualified tail edges that escape one translated call frame.
    pub return_escape_sites: Vec<ReturnEscapeSite>,
    pub return_consume_sites: Vec<ReturnConsumeSite>,
    pub materialized_call_sites: Vec<MaterializedCallSite>,
    /// When set (e.g. "b0_"), labels inside `window_label_range` become
    /// `L_b0_XXXX`: their identity includes a physical bank and CPU address.
    /// Other windows remain unqualified for fixed or runtime dispatch.
    pub window_label_prefix: Option<String>,
    /// CPU window whose physical bank is established by this analysis unit.
    /// Defaults to UxROM's 16 KiB window; MMC3 units use one 8 KiB window.
    pub window_label_range: std::ops::Range<u16>,
    /// Preserve cartridge/expansion memory operands for a complete runtime
    /// bus instead of applying the legacy mapper-store restrictions.
    pub dynamic_cpu_bus: bool,
    /// Profile-verified data holes in this physical analysis unit.
    pub data_regions: Vec<std::ops::RangeInclusive<u16>>,
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
    pub return_target: Option<String>,
    pub tail_indices: Vec<usize>,
    pub stack_return_bytes: u8,
    pub target_entry_a: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ReturnEscapeSite {
    pub caller: u16,
    pub target: u16,
    pub return_addr: u16,
    pub stack_bytes_already_consumed: bool,
    pub consume_at: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ReturnConsumeSite {
    pub at: u16,
    pub second_pla: Option<u16>,
    pub return_addrs: Vec<u16>,
}

#[derive(Debug, Clone)]
pub struct MaterializedCallSite {
    pub caller: u16,
    pub target: u16,
}

impl JumpEngineSite {
    fn table_start(&self) -> usize {
        usize::from(self.caller) + 3
    }

    fn table_end(&self) -> Option<usize> {
        self.targets
            .len()
            .checked_mul(2)
            .and_then(|len| self.table_start().checked_add(len))
    }

    fn contains_table_byte(&self, pc: u16) -> bool {
        let pc = usize::from(pc);
        self.table_end()
            .is_some_and(|end| pc >= self.table_start() && pc < end)
    }
}

impl Default for LiftOptions {
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            entry_name: String::new(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
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

/// Rewrite `PHA hi / PHA lo / RTS` computed-jump idioms: an `Op::Rts`
/// with >= 2 unmatched `Op::Pha` since the last label/branch boundary
/// becomes `Op::RtsDispatch` (the pushes stay — the dispatcher pops).
pub fn mark_rts_dispatch(ops: &mut [Op]) -> usize {
    let mut unmatched: i32 = 0;
    let mut hits = 0usize;
    for op in ops.iter_mut() {
        match op {
            Op::Pha => unmatched += 1,
            Op::Pla => unmatched -= 1,
            // Block boundaries reset the local push balance.
            Op::Label(_)
            | Op::BranchIf { .. }
            | Op::Jmp { .. }
            | Op::ReturnEscape { .. }
            | Op::Jsr { .. }
            | Op::MaterializedJsr { .. } => unmatched = 0,
            Op::Rts => {
                if unmatched >= 2 {
                    *op = Op::RtsDispatch;
                    hits += 1;
                }
                unmatched = 0;
            }
            _ => {}
        }
    }
    hits
}

fn label_for(addr: u16) -> String {
    format!("L_{addr:04X}")
}

fn label_for_prefixed(addr: u16, opts: &LiftOptions) -> String {
    match opts.window_label_prefix.as_deref() {
        Some(p) if opts.window_label_range.contains(&addr) => format!("L_{p}{addr:04X}"),
        _ => label_for(addr),
    }
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

    if opts
        .return_escape_sites
        .iter()
        .any(|site| site.caller == pc)
        && !(insn.mnemonic == Mnemonic::JMP && insn.mode == AddrMode::Absolute)
    {
        return vec![Op::Unsupported {
            pc,
            opcode: insn.opcode,
            mnemonic: format!("{:?}", insn.mnemonic),
            reason: "return_escape caller is not an absolute JMP".to_string(),
        }];
    }

    // Helper: record a label (internal or external)
    let record_target =
        |target: u16, branch_labels: &mut Vec<String>, external_calls: &mut Vec<String>| {
            let lbl = label_for_prefixed(target, opts);
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
        Mnemonic::BRK => return vec![Op::Brk { pc }],
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
                    if let Some(site) = opts.return_escape_sites.iter().find(|s| s.caller == pc) {
                        if site.target != target {
                            return vec![Op::Unsupported {
                                pc,
                                opcode: insn.opcode,
                                mnemonic: "JMP".to_string(),
                                reason: format!(
                                    "return_escape target mismatch: profile ${:04X}, ROM ${target:04X}",
                                    site.target
                                ),
                            }];
                        }
                        return vec![Op::ReturnEscape {
                            target: lbl,
                            return_addr: site.return_addr,
                            stack_bytes_already_consumed: site.stack_bytes_already_consumed,
                        }];
                    }
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
                    for target in &site.targets {
                        if !external_calls.contains(target) {
                            external_calls.push(target.clone());
                        }
                    }
                    if let Some(return_target) = &site.return_target
                        && !external_calls.contains(return_target)
                    {
                        external_calls.push(return_target.clone());
                    }
                    return vec![Op::JumpEngineCall {
                        targets: site.targets.clone(),
                        return_target: site.return_target.clone(),
                        tail_indices: site.tail_indices.clone(),
                        stack_return_bytes: site.stack_return_bytes,
                        target_entry_a: site.target_entry_a.clone(),
                    }];
                }
                let lbl = record_target(target, branch_labels, external_calls);
                if let Some(site) = opts.materialized_call_sites.iter().find(|s| s.caller == pc) {
                    return vec![Op::MaterializedJsr {
                        target: lbl,
                        return_addr: site.caller + 2,
                    }];
                }
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
                if opts.dynamic_cpu_bus {
                    return vec![Op::LdaMem {
                        addr,
                        region: classify_addr(base),
                    }];
                }
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
                if opts.dynamic_cpu_bus {
                    return vec![Op::StaMem { addr, region }];
                }
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
                    MemRegion::PrgRom => {
                        if let Some(ca) = addr.const_addr() {
                            vec![Op::MapperWrite {
                                addr: ca,
                                value: ValueSrc::A,
                            }]
                        } else {
                            // Keep indexed ROM stores intact: lowering computes
                            // their exact effective mapper-register address.
                            vec![Op::StaMem { addr, region }]
                        }
                    }
                    MemRegion::Mapper | MemRegion::PrgRam => vec![Op::UnsupportedMapperStore {
                        pc,
                        opcode: insn.opcode,
                        mnemonic: "STA".to_string(),
                        reason: match region {
                            MemRegion::Mapper => {
                                "STA to expansion space ($4020-$5FFF) is not a supported UxROM mapper register"
                            }
                            MemRegion::PrgRam => {
                                "STA to PRG RAM ($6000-$7FFF) is not a supported UxROM mapper register"
                            }
                            _ => unreachable!(),
                        }
                        .to_string(),
                    }],
                    _ => vec![Op::StaMem { addr, region }],
                };
            }
        }

        // ---- STX ----
        Mnemonic::STX => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                if opts.dynamic_cpu_bus {
                    return vec![Op::StxMem { addr, region }];
                }
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::X,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::X,
                    }],
                    MemRegion::OamDma => vec![Op::OamDmaWrite { value: ValueSrc::X }],
                    MemRegion::Mapper | MemRegion::PrgRam | MemRegion::PrgRom => {
                        vec![Op::UnsupportedMapperStore {
                            pc,
                            opcode: insn.opcode,
                            mnemonic: "STX".to_string(),
                            reason: "STX to expansion space, PRG RAM, or PRG ROM is unsupported"
                                .to_string(),
                        }]
                    }
                    _ => vec![Op::StxMem { addr, region }],
                };
            }
        }

        // ---- STY ----
        Mnemonic::STY => {
            if let Some((addr, base)) = make_addr_expr(insn) {
                let region = classify_addr(base);
                if opts.dynamic_cpu_bus {
                    return vec![Op::StyMem { addr, region }];
                }
                return match region {
                    MemRegion::PpuReg => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::Y,
                    }],
                    MemRegion::PpuMirror => vec![Op::PpuWrite {
                        reg: (base & 0x07) as u8,
                        value: ValueSrc::Y,
                    }],
                    MemRegion::OamDma => vec![Op::OamDmaWrite { value: ValueSrc::Y }],
                    MemRegion::Mapper | MemRegion::PrgRam | MemRegion::PrgRom => {
                        vec![Op::UnsupportedMapperStore {
                            pc,
                            opcode: insn.opcode,
                            mnemonic: "STY".to_string(),
                            reason: "STY to expansion space, PRG RAM, or PRG ROM is unsupported"
                                .to_string(),
                        }]
                    }
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
    let is_jump_engine_table_byte = |pc: u16| {
        opts.data_regions.iter().any(|range| range.contains(&pc))
            || opts
                .jump_engine_sites
                .iter()
                .any(|site| site.contains_table_byte(pc))
    };
    {
        let mut pc = opts.start;
        while pc < opts.end {
            if let Some(range) = opts.data_regions.iter().find(|range| range.contains(&pc)) {
                pc = range.end().saturating_add(1);
                continue;
            }
            let offset = cpu_to_prg_offset(pc).ok_or(LiftError::OutsideRange { pc })?;
            let insn = decode_at(prg, pc, offset).map_err(|_| LiftError::Truncated { pc })?;

            if insn.is_branch() {
                if let Some(target) = insn.branch_target() {
                    if target >= opts.start
                        && target < opts.end
                        && !is_jump_engine_table_byte(target)
                    {
                        internal_targets.insert(target);
                    }
                }
            }
            // Also mark JMP absolute targets inside range
            if insn.mnemonic == Mnemonic::JMP {
                if let (AddrMode::Absolute, Operand::Addr(target)) = (insn.mode, insn.operand) {
                    if target >= opts.start
                        && target < opts.end
                        && !is_jump_engine_table_byte(target)
                    {
                        internal_targets.insert(target);
                    }
                }
            }

            if insn.mnemonic == Mnemonic::JSR
                && insn.mode == AddrMode::Absolute
                && let Some(site) = opts.jump_engine_sites.iter().find(|site| site.caller == pc)
            {
                let Some(table_end) = site.table_end() else {
                    break;
                };
                if table_end >= usize::from(opts.end) {
                    break;
                }
                let Ok(table_end) = u16::try_from(table_end) else {
                    break;
                };
                pc = table_end;
                continue;
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
        if pc >= opts.start && pc < opts.end && !is_jump_engine_table_byte(pc) {
            all_targets.insert(pc);
        }
    }

    for site in &opts.materialized_call_sites {
        if site.caller >= opts.start && site.caller < opts.end {
            let insn = decode_at(prg, site.caller, cpu_to_prg_offset(site.caller).unwrap())
                .map_err(|_| LiftError::Truncated { pc: site.caller })?;
            if insn.mnemonic != Mnemonic::JSR
                || insn.operand != Operand::Addr(site.target)
                || site.caller.checked_add(3).is_none_or(|end| end > opts.end)
            {
                return Err(LiftError::Decode(format!(
                    "materialized call ${:04X}: expected complete JSR/target",
                    site.caller
                )));
            }
        }
    }
    for site in &opts.return_consume_sites {
        let last = site
            .second_pla
            .or_else(|| site.at.checked_add(1))
            .ok_or_else(|| LiftError::Decode("return_consume pair wraps".into()))?;
        if site.at >= opts.end || last < opts.start {
            continue;
        }
        if site.at < opts.start
            || last >= opts.end
            || site.return_addrs.is_empty()
            || last <= site.at
            || all_targets.iter().any(|pc| *pc > site.at && *pc <= last)
        {
            return Err(LiftError::Decode(format!(
                "return_consume ${:04X}: incomplete pair or bypass entry",
                site.at
            )));
        }
        for pc in [site.at, last] {
            let insn = decode_at(prg, pc, cpu_to_prg_offset(pc).unwrap())
                .map_err(|_| LiftError::Truncated { pc })?;
            if insn.mnemonic != Mnemonic::PLA || insn.size != 1 {
                return Err(LiftError::Decode(format!(
                    "return_consume ${:04X}: expected PLA/PLA",
                    site.at
                )));
            }
        }
        // The gap may expose the first pulled byte (e.g. STA zp) but may
        // neither alter the stack nor branch around the second pull.
        let mut gap = site.at + 1;
        while gap < last {
            let insn = decode_at(prg, gap, cpu_to_prg_offset(gap).unwrap())
                .map_err(|_| LiftError::Truncated { pc: gap })?;
            if !matches!(
                insn.mnemonic,
                Mnemonic::STA | Mnemonic::STX | Mnemonic::STY | Mnemonic::NOP
            ) || gap + u16::from(insn.size) > last
                || (insn.mnemonic != Mnemonic::NOP && insn.mode != AddrMode::ZeroPage)
            {
                return Err(LiftError::Decode(format!(
                    "return_consume ${:04X}: gap must be stack-neutral zero-page stores or NOP",
                    site.at
                )));
            }
            gap += u16::from(insn.size);
        }
        if opts.jump_engine_sites.iter().any(|engine| {
            (site.at + 1..=last).any(|pc| {
                let label = label_for_prefixed(pc, opts);
                engine.targets.contains(&label) || engine.return_target.as_ref() == Some(&label)
            })
        }) {
            return Err(LiftError::Decode(format!(
                "return_consume ${:04X}: dispatch bypasses first PLA",
                site.at
            )));
        }
        // JSR destinations do not normally become intra-routine labels.
        // Inspect them without changing default discovery or generated bytes.
        let mut scan = opts.start;
        while scan < opts.end {
            if let Some(range) = opts.data_regions.iter().find(|range| range.contains(&scan)) {
                scan = range.end().saturating_add(1);
                continue;
            }
            let insn = decode_at(prg, scan, cpu_to_prg_offset(scan).unwrap())
                .map_err(|_| LiftError::Truncated { pc: scan })?;
            if insn.mnemonic == Mnemonic::JSR
                && matches!(insn.operand, Operand::Addr(target) if target > site.at && target <= last)
            {
                return Err(LiftError::Decode(format!(
                    "return_consume ${:04X}: JSR bypasses first PLA",
                    site.at
                )));
            }
            scan = if let Some(engine) = opts.jump_engine_sites.iter().find(|s| s.caller == scan) {
                u16::try_from(engine.table_end().unwrap_or(usize::from(opts.end)))
                    .map_err(|_| LiftError::Decode("return_consume dispatch range wraps".into()))?
            } else {
                scan.checked_add(u16::from(insn.size))
                    .ok_or_else(|| LiftError::Decode("return_consume range wraps".into()))?
            };
        }
    }

    // An early software pop is safe only when every route through the
    // consuming block enters before its first PLA. Matching bytes alone is
    // insufficient: a cross-routine/dispatch entry could skip the transfer.
    for site in opts
        .return_escape_sites
        .iter()
        .filter(|site| site.stack_bytes_already_consumed)
    {
        let start = site.consume_at.ok_or_else(|| {
            LiftError::Decode("consumed return_escape requires consume_at".into())
        })?;
        if !(start < opts.end && site.caller >= opts.start) {
            continue;
        }
        let invalid = |reason: &str| {
            LiftError::Decode(format!("return_escape consume_at ${start:04X}: {reason}"))
        };
        if start < opts.start
            || site.caller >= opts.end
            || start.checked_add(2).is_none_or(|p| p > site.caller)
        {
            return Err(invalid("consume block must belong to one lifted routine"));
        }
        if all_targets
            .iter()
            .any(|pc| *pc > start && *pc <= site.caller)
            || opts.jump_engine_sites.iter().any(|engine| {
                (start + 1..=site.caller).any(|pc| {
                    let label = label_for_prefixed(pc, opts);
                    engine.targets.contains(&label) || engine.return_target.as_ref() == Some(&label)
                })
            })
        {
            return Err(invalid("alternate entry bypasses the first PLA"));
        }
        // Internal JSR targets are not branch labels in the normal lifter.
        // Check them without changing default routine discovery/emission.
        let mut scan = opts.start;
        while scan < opts.end {
            if let Some(range) = opts.data_regions.iter().find(|range| range.contains(&scan)) {
                scan = range.end().saturating_add(1);
                continue;
            }
            let insn = decode_at(prg, scan, cpu_to_prg_offset(scan).unwrap())
                .map_err(|_| LiftError::Truncated { pc: scan })?;
            if insn.mnemonic == Mnemonic::JSR
                && let Operand::Addr(target) = insn.operand
                && target > start
                && target <= site.caller
            {
                return Err(invalid("JSR entry bypasses the first PLA"));
            }
            if let Some(engine) = opts.jump_engine_sites.iter().find(|s| s.caller == scan) {
                scan = u16::try_from(engine.table_end().unwrap_or(usize::from(opts.end)))
                    .map_err(|_| invalid("dispatch table exceeds lift range"))?;
            } else {
                scan = scan
                    .checked_add(u16::from(insn.size))
                    .ok_or_else(|| invalid("range wraps"))?;
            }
        }
        let mut pc = start;
        while pc <= site.caller {
            let insn = decode_at(prg, pc, cpu_to_prg_offset(pc).unwrap())
                .map_err(|_| LiftError::Truncated { pc })?;
            if pc < start + 2 {
                if insn.mnemonic != Mnemonic::PLA || insn.size != 1 {
                    return Err(invalid("expected consecutive PLA/PLA"));
                }
            } else if pc == site.caller {
                if insn.mnemonic != Mnemonic::JMP
                    || insn.mode != AddrMode::Absolute
                    || insn.operand != Operand::Addr(site.target)
                {
                    return Err(invalid("final JMP/target mismatch"));
                }
                break;
            } else if !matches!(
                insn.mnemonic,
                Mnemonic::LDA | Mnemonic::LDX | Mnemonic::LDY | Mnemonic::NOP
            ) {
                // Deliberately narrow: other suffixes need an explicit proof
                // before expanding this read-only, stack-neutral contract.
                return Err(invalid("suffix supports only loads/NOP before the JMP"));
            }
            pc = pc
                .checked_add(u16::from(insn.size))
                .ok_or_else(|| invalid("range wraps"))?;
            if pc > site.caller {
                return Err(invalid("caller is not an instruction boundary"));
            }
        }
    }

    let mut pc = opts.start;
    let mut lifted_end = opts.start;
    while pc < opts.end {
        if let Some(range) = opts.data_regions.iter().find(|range| range.contains(&pc)) {
            // A taken branch may skip embedded data. The untaken path must
            // trap, not quietly fall through into code after the table.
            let target = label_for_prefixed(pc, opts);
            external_calls.push(target.clone());
            ops.push(Op::Jmp { target });
            let after = range.end().saturating_add(1);
            if let Some(&target) = internal_targets
                .iter()
                .filter(|&&target| target >= after)
                .min()
            {
                pc = target;
                continue;
            }
            break;
        }
        // Emit internal label if this PC is a branch target
        if all_targets.contains(&pc) {
            let lbl = label_for_prefixed(pc, opts);
            // The entry label is already emitted as op 0 (entry_name); a
            // self-targeting entry must not define the same string twice.
            if lbl != opts.entry_name {
                ops.push(Op::Label(lbl.clone()));
            }
            if !branch_labels.contains(&lbl) {
                branch_labels.push(lbl);
            }
        }

        let offset = cpu_to_prg_offset(pc).ok_or(LiftError::OutsideRange { pc })?;
        let insn = decode_at(prg, pc, offset).map_err(|_| LiftError::Truncated { pc })?;

        // Source marker
        ops.push(Op::Source {
            pc,
            size: insn.size,
            text: format_instruction(&insn),
            instruction: Some(insn),
        });

        if let Some(site) = opts
            .return_escape_sites
            .iter()
            .find(|site| site.consume_at == Some(pc))
        {
            ops.push(Op::ReturnEscapeConsume {
                return_addr: site.return_addr,
            });
        }
        if let Some(site) = opts.return_consume_sites.iter().find(|site| site.at == pc) {
            ops.push(Op::ReturnConsume {
                return_addrs: site.return_addrs.clone(),
            });
        }

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

        // JumpEngine consumes the JSR return address as its inline table
        // pointer and tail-dispatches to the selected target. Continue only
        // when an earlier branch has established a reachable code target
        // beyond the table.
        if insn.mnemonic == Mnemonic::JSR
            && insn.mode == AddrMode::Absolute
            && let Some(site) = opts.jump_engine_sites.iter().find(|site| site.caller == pc)
        {
            let table_end = site.table_end().unwrap_or(usize::from(next_pc));
            if let Some(&target) = internal_targets
                .iter()
                .filter(|&&target| {
                    usize::from(target) >= table_end && !is_jump_engine_table_byte(target)
                })
                .min()
            {
                pc = target;
                continue;
            }
            break;
        }

        // If this is a hard terminator, continue at the lowest pending forward
        // internal target; otherwise there is nothing reachable to emit.
        if insn.is_terminator() {
            if let Some(&target) = internal_targets
                .iter()
                .filter(|&&target| target >= next_pc)
                .min()
            {
                pc = target;
                continue;
            }
            break;
        }

        pc = next_pc;
    }

    for site in opts
        .return_escape_sites
        .iter()
        .filter(|site| site.stack_bytes_already_consumed)
    {
        if site
            .consume_at
            .is_some_and(|start| start >= opts.start && start < opts.end)
            && (!ops
                .iter()
                .any(|op| matches!(op, Op::Source { pc, .. } if Some(*pc) == site.consume_at))
                || !ops
                    .iter()
                    .any(|op| matches!(op, Op::Source { pc, .. } if *pc == site.caller)))
        {
            return Err(LiftError::Decode(
                "return_escape consuming block was not lifted".into(),
            ));
        }
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
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            start: cpu_start,
            end: cpu_start + bytes.len() as u16,
            entry_name: format!("L_{cpu_start:04X}"),
            extra_label_pcs: Vec::new(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
        };
        lift_range(&prg, &opts).expect("lift failed")
    }

    // ------ classify_addr tests ------

    #[test]
    fn eight_kib_units_qualify_only_their_established_cpu_window() {
        for window in [0x8000..0xa000, 0xa000..0xc000, 0xc000..0xe000] {
            // Calls into all four CPU windows: only the current unit's bank
            // is known. Other switchable targets must be runtime-dispatched.
            let code = [
                0x20, 0x00, 0x81, 0x20, 0x00, 0xa1, 0x20, 0x00, 0xc1, 0x20, 0x00, 0xe1, 0x60,
            ];
            let prg = make_prg_at(window.start, &code);
            let opts = LiftOptions {
                start: window.start,
                end: window.start + code.len() as u16,
                window_label_prefix: Some("b7_".to_owned()),
                window_label_range: window.clone(),
                dynamic_cpu_bus: false,
                data_regions: Vec::new(),
                ..LiftOptions::default()
            };
            let routine = lift_range(&prg, &opts).unwrap();
            let expected: Vec<_> = [0x8100, 0xa100, 0xc100, 0xe100]
                .into_iter()
                .map(|pc| {
                    if window.contains(&pc) {
                        format!("L_b7_{pc:04X}")
                    } else {
                        format!("L_{pc:04X}")
                    }
                })
                .collect();
            assert_eq!(routine.external_calls, expected);
        }
    }

    #[test]
    fn uxrom_default_still_qualifies_both_lower_eight_kib_halves() {
        let opts = LiftOptions {
            window_label_prefix: Some("b2_".to_owned()),
            ..LiftOptions::default()
        };
        assert_eq!(label_for_prefixed(0x8000, &opts), "L_b2_8000");
        assert_eq!(label_for_prefixed(0xbfff, &opts), "L_b2_BFFF");
        assert_eq!(label_for_prefixed(0xc000, &opts), "L_C000");
    }

    #[test]
    fn materialized_calls_and_adjacent_consumption_preserve_original_control_flow() {
        let prg = make_prg_at(0x8000, &[0x20, 0, 0x81, 0x60]);
        let mut opts = LiftOptions {
            start: 0x8000,
            end: 0x8004,
            materialized_call_sites: vec![MaterializedCallSite {
                caller: 0x8000,
                target: 0x8100,
            }],
            ..LiftOptions::default()
        };
        let call = lift_range(&prg, &opts).unwrap();
        assert!(call.ops.iter().any(|op| matches!(op, Op::MaterializedJsr { target, return_addr: 0x8002 } if target == "L_8100")));
        opts.materialized_call_sites[0].target = 0x8200;
        assert!(lift_range(&prg, &opts).is_err());
        opts.materialized_call_sites[0].target = 0x8100;
        opts.end = 0x8002;
        assert!(lift_range(&prg, &opts).is_err());

        let code = [0x68, 0x68, 0xa9, 0, 0xd0, 2, 0xa9, 1, 0x60];
        let prg = make_prg_at(0x8100, &code);
        let mut opts = LiftOptions {
            start: 0x8100,
            end: 0x8109,
            return_consume_sites: vec![ReturnConsumeSite {
                at: 0x8100,
                second_pla: None,
                return_addrs: vec![0x8002],
            }],
            ..LiftOptions::default()
        };
        let pair = lift_range(&prg, &opts).unwrap();
        assert_eq!(
            pair.ops
                .iter()
                .filter(|op| matches!(op, Op::ReturnConsume { .. }))
                .count(),
            1
        );
        assert_eq!(
            pair.ops.iter().filter(|op| matches!(op, Op::Pla)).count(),
            2
        );
        assert!(pair.ops.iter().any(|op| matches!(op, Op::BranchIf { .. })));
        assert!(pair.ops.iter().any(|op| matches!(op, Op::Rts)));
        assert!(
            !pair
                .ops
                .iter()
                .any(|op| matches!(op, Op::ReturnEscape { .. }))
        );
        opts.extra_label_pcs = vec![0x8102]; // A suffix entry still returns normally.
        assert!(lift_range(&prg, &opts).is_ok());
        opts.extra_label_pcs = vec![0x8101];
        assert!(lift_range(&prg, &opts).is_err());
        opts.extra_label_pcs.clear();
        let mut bad = prg.clone();
        bad[0x106..0x109].copy_from_slice(&[0x20, 1, 0x81]);
        assert!(lift_range(&bad, &opts).is_err());
        bad = prg.clone();
        bad[0x101] = 0xea;
        assert!(lift_range(&bad, &opts).is_err());
        opts.start = 0x8101;
        assert!(lift_range(&prg, &opts).is_err());
    }

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
    fn lift_stx_sty_oam_dma() {
        // 8E/8C 14 40 — STX/STY $4014
        let stx = lift(0x8000, &[0x8E, 0x14, 0x40]);
        assert!(stx.ops.contains(&Op::OamDmaWrite { value: ValueSrc::X }));

        let sty = lift(0x8000, &[0x8C, 0x14, 0x40]);
        assert!(sty.ops.contains(&Op::OamDmaWrite { value: ValueSrc::Y }));
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
    fn lift_forward_branch_past_external_jmp() {
        // $8000: D0 04     BNE $8006
        // $8002: 4C 00 90  JMP $9000
        // $8006: 60        RTS
        let r = lift(0x8000, &[0xD0, 0x04, 0x4C, 0x00, 0x90, 0xEA, 0x60]);

        assert!(r.branch_labels.contains(&"L_8006".to_string()));
        let jmp_pos = r
            .ops
            .iter()
            .position(|op| matches!(op, Op::Jmp { target } if target == "L_9000"))
            .unwrap();
        let label_pos = r
            .ops
            .iter()
            .position(|op| op == &Op::Label("L_8006".to_string()))
            .unwrap();
        let rts_pos = r.ops.iter().rposition(|op| op == &Op::Rts).unwrap();
        assert!(jmp_pos < label_pos && label_pos < rts_pos);
    }

    #[test]
    fn lift_profiled_return_escape() {
        let prg = make_prg_at(0xE7D0, &[0x4C, 0x60, 0xEC]);
        let opts = LiftOptions {
            start: 0xE7D0,
            end: 0xE7D3,
            entry_name: "L_E7D0".to_string(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: vec![ReturnEscapeSite {
                caller: 0xE7D0,
                target: 0xEC60,
                return_addr: 0xEA79,
                stack_bytes_already_consumed: false,
                consume_at: None,
            }],
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            extra_label_pcs: Vec::new(),
        };
        let routine = lift_range(&prg, &opts).expect("lift return escape");
        assert!(routine.ops.contains(&Op::ReturnEscape {
            target: "L_EC60".to_string(),
            return_addr: 0xEA79,
            stack_bytes_already_consumed: false,
        }));

        let mut consumed = opts.clone();
        consumed.return_escape_sites[0].stack_bytes_already_consumed = true;
        assert!(
            lift_range(&prg, &consumed)
                .unwrap_err()
                .to_string()
                .contains("consume_at")
        );
        let mut stale = opts;
        stale.return_escape_sites[0].target = 0xEC61;
        let routine = lift_range(&prg, &stale).expect("stale fact becomes unsupported IR");
        assert!(routine.ops.iter().any(|op| matches!(
            op,
            Op::Unsupported { reason, .. } if reason.contains("return_escape target mismatch")
        )));

        let prg = make_prg_at(0xE7D0, &[0xEA]);
        stale.end = 0xE7D1;
        let routine = lift_range(&prg, &stale).expect("stale opcode becomes unsupported IR");
        assert!(routine.ops.iter().any(|op| matches!(
            op,
            Op::Unsupported { reason, .. }
                if reason.contains("return_escape caller is not an absolute JMP")
        )));
    }

    #[test]
    fn consumed_escape_requires_exact_live_pair_and_single_entry_block() {
        let code = [0x68, 0x68, 0xbd, 0x80, 0x03, 0x4c, 0x00, 0x90];
        let prg = make_prg_at(0x8000, &code);
        let opts = LiftOptions {
            start: 0x8000,
            end: 0x8008,
            entry_name: "L_8000".into(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: vec![ReturnEscapeSite {
                caller: 0x8005,
                target: 0x9000,
                return_addr: 0x8fff,
                stack_bytes_already_consumed: true,
                consume_at: Some(0x8000),
            }],
            ..LiftOptions::default()
        };
        let valid = lift_range(&prg, &opts).unwrap();
        assert!(valid.ops.windows(2).any(|ops| matches!(
            ops,
            [
                Op::ReturnEscapeConsume {
                    return_addr: 0x8fff
                },
                Op::Pla
            ]
        )));
        assert_eq!(
            valid.ops.iter().filter(|op| matches!(op, Op::Pla)).count(),
            2
        );
        for boundary in 0x8001..=0x8005 {
            let mut alternate = opts.clone();
            alternate.extra_label_pcs.push(boundary);
            assert!(
                lift_range(&prg, &alternate)
                    .unwrap_err()
                    .to_string()
                    .contains("entry")
            );
            let mut split = opts.clone();
            split.start = boundary;
            assert!(
                lift_range(&prg, &split).is_err(),
                "interior routine root {boundary:04x}"
            );
        }
        for (index, byte) in [
            (0, 0xea),
            (1, 0x48),
            (2, 0x9a),
            (2, 0x20),
            (2, 0xd0),
            (2, 0x60),
            (2, 0x8d),
            (5, 0xea),
            (6, 1),
        ] {
            let mut changed = prg.clone();
            changed[index] = byte;
            assert!(
                lift_range(&changed, &opts).is_err(),
                "stale {index}/{byte:02x}"
            );
        }
        // Known internal branch, JMP, JSR and profiled dispatch entries all
        // fail, even though their target's bytes remain a valid PLA suffix.
        for prefix in [vec![0xd0, 1], vec![0x4c, 4, 0x80], vec![0x20, 4, 0x80]] {
            let start = 0x8000 + prefix.len() as u16;
            let mut body = prefix;
            body.extend(code);
            let mut prefixed = opts.clone();
            prefixed.end = start + code.len() as u16;
            prefixed.return_escape_sites[0].consume_at = Some(start);
            prefixed.return_escape_sites[0].caller = start + 5;
            assert!(lift_range(&make_prg_at(0x8000, &body), &prefixed).is_err());
        }
        let mut dispatch = opts.clone();
        dispatch.jump_engine_sites.push(JumpEngineSite {
            caller: 0x9000,
            targets: vec!["L_8001".into()],
            return_target: None,
            tail_indices: vec![],
            stack_return_bytes: 0,
            target_entry_a: vec![],
        });
        assert!(lift_range(&prg, &dispatch).is_err());
        // A valid pair after an unreachable terminator must not turn the
        // eventual JMP into a silent no-pop escape.
        let unreachable = make_prg_at(
            0x7fff + 1,
            &[0x60, 0x68, 0x68, 0xbd, 0x80, 3, 0x4c, 0, 0x90],
        );
        let mut missing = opts;
        missing.end += 1;
        missing.return_escape_sites[0].consume_at = Some(0x8001);
        missing.return_escape_sites[0].caller += 1;
        assert!(
            lift_range(&unreachable, &missing)
                .unwrap_err()
                .to_string()
                .contains("not lifted")
        );
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
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            start: 0xAEF9,
            end: 0xAEFE,
            entry_name: "L_AEF9".to_string(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
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
    fn lift_jump_engine_skips_inline_table_and_tail_dispatches() {
        // The branch makes $8009 reachable without executing the dispatch;
        // the bytes in between are a two-entry pointer table, not code.
        let prg = make_prg_at(
            0x8000,
            &[0xD0, 0x07, 0x20, 0x00, 0x90, 0x32, 0x12, 0x02, 0x80, 0x60],
        );
        let r = lift_range(
            &prg,
            &LiftOptions {
                start: 0x8000,
                end: 0x800A,
                entry_name: "L_8000".into(),
                jump_engine_sites: vec![JumpEngineSite {
                    caller: 0x8002,
                    targets: vec!["First".into(), "Second".into()],
                    return_target: None,
                    tail_indices: Vec::new(),
                    stack_return_bytes: 0,
                    target_entry_a: Vec::new(),
                }],
                return_consume_sites: Vec::new(),
                materialized_call_sites: Vec::new(),
                return_escape_sites: Vec::new(),
                window_label_prefix: None,
                window_label_range: 0x8000..0xC000,
                dynamic_cpu_bus: false,
                data_regions: Vec::new(),
                extra_label_pcs: Vec::new(),
            },
        )
        .expect("lift");

        assert_eq!(r.end, 0x800A);
        assert!(r.ops.contains(&Op::JumpEngineCall {
            targets: vec!["First".into(), "Second".into()],
            return_target: None,
            tail_indices: Vec::new(),
            stack_return_bytes: 0,
            target_entry_a: Vec::new(),
        }));
        assert!(r.ops.contains(&Op::Rts));
        assert!(r.external_calls.contains(&"First".to_string()));
        assert!(r.external_calls.contains(&"Second".to_string()));
        assert!(!r.ops.iter().any(|op| matches!(op, Op::Jam { .. })));
        assert!(
            !r.ops
                .iter()
                .any(|op| matches!(op, Op::Source { pc, .. } if (0x8005..0x8009).contains(pc)))
        );
    }

    #[test]
    fn lift_jump_engine_without_bypass_ends_after_jsr() {
        let prg = make_prg_at(0x8000, &[0x20, 0x00, 0x90, 0x32, 0x12, 0x02, 0x80, 0x60]);
        let r = lift_range(
            &prg,
            &LiftOptions {
                start: 0x8000,
                end: 0x8008,
                entry_name: "L_8000".into(),
                jump_engine_sites: vec![JumpEngineSite {
                    caller: 0x8000,
                    targets: vec!["First".into(), "Second".into()],
                    return_target: None,
                    tail_indices: Vec::new(),
                    stack_return_bytes: 0,
                    target_entry_a: Vec::new(),
                }],
                return_consume_sites: Vec::new(),
                materialized_call_sites: Vec::new(),
                return_escape_sites: Vec::new(),
                window_label_prefix: None,
                window_label_range: 0x8000..0xC000,
                dynamic_cpu_bus: false,
                data_regions: Vec::new(),
                extra_label_pcs: Vec::new(),
            },
        )
        .expect("lift");

        assert_eq!(r.end, 0x8003);
        assert!(r.ops.last().is_some_and(Op::is_hard_terminator));
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
                window_label_prefix: None,
                window_label_range: 0x8000..0xC000,
                dynamic_cpu_bus: false,
                data_regions: Vec::new(),
                start: 0xE3E9,
                end: 0xE3EC,
                entry_name: "L_E3E9".into(),
                jump_engine_sites: Vec::new(),
                return_consume_sites: Vec::new(),
                materialized_call_sites: Vec::new(),
                return_escape_sites: Vec::new(),
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
    fn lift_mapper_stores_are_fail_closed_except_prg_rom_sta() {
        // STA $8000 remains a constant-address mapper write.
        let r = lift(0x8000, &[0x8D, 0x00, 0x80]);
        assert!(r.ops.contains(&Op::MapperWrite {
            addr: 0x8000,
            value: ValueSrc::A,
        }));

        // STA $9000,X / STA $C000,Y retain their indexed address expressions
        // so lowering can pass the effective mapper register address.
        let r = lift(0x8000, &[0x9D, 0x00, 0x90, 0x99, 0x00, 0xC0]);
        assert!(r.ops.contains(&Op::StaMem {
            addr: AddrExpr::AbsIndexedX(0x9000),
            region: MemRegion::PrgRom,
        }));
        assert!(r.ops.contains(&Op::StaMem {
            addr: AddrExpr::AbsIndexedY(0xC000),
            region: MemRegion::PrgRom,
        }));

        for (bytes, mnemonic, range) in [
            (&[0x8D, 0x20, 0x40][..], "STA", "expansion space"),
            (&[0x8D, 0x00, 0x60][..], "STA", "PRG RAM"),
            (&[0x8E, 0x00, 0x80][..], "STX", "PRG ROM"),
            (&[0x8C, 0x00, 0x60][..], "STY", "PRG RAM"),
        ] {
            let r = lift(0x8000, bytes);
            assert!(r.ops.iter().any(|op| matches!(op,
                Op::UnsupportedMapperStore { mnemonic: actual, reason, .. }
                if actual == mnemonic && reason.contains(range)
            )));
        }
    }

    #[test]
    fn lift_sta_indirect_y_zero_page_remains_supported() {
        // STA ($10),Y has a zero-page pointer base, not a mapper base.
        let r = lift(0x8000, &[0x91, 0x10]);
        assert!(r.ops.contains(&Op::StaMem {
            addr: AddrExpr::IndirectY(0x10),
            region: MemRegion::ZeroPage,
        }));
    }

    #[test]
    fn lift_error_on_empty_range() {
        let prg = vec![0u8; 0x8000];
        let opts = LiftOptions {
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            start: 0x8000,
            end: 0x8000,
            entry_name: "test".to_string(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        };
        assert!(lift_range(&prg, &opts).is_err());
    }

    #[test]
    fn lift_error_start_gt_end() {
        let prg = vec![0u8; 0x8000];
        let opts = LiftOptions {
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            start: 0x8010,
            end: 0x8000,
            entry_name: "test".to_string(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
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
    #[test]
    fn separated_return_plas_keep_stores_and_reject_bypasses_or_stack_changes() {
        let prg = make_prg_at(0x8100, &[0x68, 0x85, 0, 0x68, 0x60]);
        let mut opts = LiftOptions {
            start: 0x8100,
            end: 0x8105,
            return_consume_sites: vec![ReturnConsumeSite {
                at: 0x8100,
                second_pla: Some(0x8103),
                return_addrs: vec![0x8002],
            }],
            ..LiftOptions::default()
        };
        let routine = lift_range(&prg, &opts).unwrap();
        assert_eq!(
            routine
                .ops
                .iter()
                .filter(|op| matches!(op, Op::Pla))
                .count(),
            2
        );
        assert!(routine.ops.iter().any(|op| matches!(
            op,
            Op::StaMem {
                addr: AddrExpr::ZpConst(0),
                ..
            }
        )));
        for entry in 0x8101..=0x8103 {
            opts.extra_label_pcs = vec![entry];
            assert!(lift_range(&prg, &opts).is_err());
        }
        opts.extra_label_pcs.clear();
        for gap in [[0x48, 0xea], [0xd0, 0], [0x9a, 0xea]] {
            let mut bytes = vec![0x68];
            bytes.extend(gap);
            bytes.extend([0x68, 0x60]);
            assert!(lift_range(&make_prg_at(0x8100, &bytes), &opts).is_err());
        }
    }

    #[test]
    fn explicit_data_hole_preserves_branch_but_traps_fallthrough() {
        let prg = make_prg_at(0x8000, &[0xa9, 1, 0xd0, 2, 0x02, 0x02, 0xa9, 2, 0x60]);
        let routine = lift_range(
            &prg,
            &LiftOptions {
                start: 0x8000,
                end: 0x8009,
                data_regions: std::iter::once(0x8004..=0x8005).collect(),
                ..LiftOptions::default()
            },
        )
        .unwrap();
        assert!(!routine.ops.iter().any(|op| matches!(
            op,
            Op::Source {
                pc: 0x8004 | 0x8005,
                ..
            }
        )));
        assert!(routine.ops.contains(&Op::Jmp {
            target: "L_8004".into()
        }));
        assert!(routine.ops.contains(&Op::Label("L_8006".into())));
        let index = routine
            .ops
            .iter()
            .position(|op| matches!(op, Op::BranchIf { .. }))
            .unwrap();
        assert_eq!(routine.next_source_pc(index), 0x8004);
    }
}
