//! IR → Z80 lowering pass.
//!
//! Translates IR routines into Z80 instructions emitted via `z80_emit::Program`.
//! 6502 registers are backed by Z80 A (for the accumulator) and SMS RAM shadow
//! locations for X, Y, S, and P.  Hardware accesses emit `call` to symbolic
//! runtime labels; the runtime itself is hand-written Z80 implemented elsewhere.

// ---------------------------------------------------------------------------
// SMS RAM layout
// ---------------------------------------------------------------------------

/// SMS RAM addresses for emulated 6502 state.
/// Matches docs/master-plan.md "SMS RAM layout" section.
pub mod sms_layout {
    pub const NES_ZP_BASE: u16 = 0xC000; // $0000-$00FF mirrors to $C000-$C0FF
    pub const NES_RAM_BASE: u16 = 0xC000; // $0000-$07FF mirrors to $C000-$C7FF
    pub const NES_STACK_BASE: u16 = 0xC100; // 6502 stack page
    pub const SHADOW_X: u16 = 0xCB00; // emulated X register
    pub const SHADOW_Y: u16 = 0xCB01; // emulated Y register
    pub const SHADOW_S: u16 = 0xCB02; // emulated 6502 stack pointer (byte)
    pub const SHADOW_P: u16 = 0xCB03; // emulated 6502 status byte
    pub const VRAM_BUFFER_HEAD: u16 = 0xC800;
    pub const SAT_STAGING: u16 = 0xC900;
    pub const FRAME_COUNTER: u16 = 0xCB04;
    pub const TEMP_W: u16 = 0xCB10; // 16-bit scratch
}

// ---------------------------------------------------------------------------
// Runtime symbols
// ---------------------------------------------------------------------------

/// Symbolic runtime labels the lowering pass references.
pub mod runtime_symbols {
    pub const PPU_WRITE: &str = "rt_ppu_write";
    pub const PPU_READ: &str = "rt_ppu_read";
    pub const OAM_DMA: &str = "rt_oam_dma";
    pub const APU_WRITE: &str = "rt_apu_write";
    pub const APU_READ: &str = "rt_apu_read";
    pub const CONTROLLER_STROBE: &str = "rt_controller_strobe";
    pub const CONTROLLER_READ: &str = "rt_controller_read";
    pub const CONTROLLER_READ_INDEXED_X: &str = "rt_controller_read_indexed_x";
    pub const MAPPER_WRITE: &str = "rt_mapper_write";
    pub const INDIRECT_JMP: &str = "rt_indirect_jmp";
    pub const PUSH_6502: &str = "rt_push6502";
    pub const POP_6502: &str = "rt_pop6502";
    pub const SET_NZ_A: &str = "rt_set_nz_a";
    pub const ADC_A_VIA_SHADOW: &str = "rt_adc_a";
    pub const SBC_A_VIA_SHADOW: &str = "rt_sbc_a";
    pub const CMP_A_VIA_SHADOW: &str = "rt_cmp_a";
    pub const ROUTE_INDEXED: &str = "rt_read_indexed";
    pub const READ_PRG_HIGH_INDEXED: &str = "rt_read_prg_high_indexed";
    pub const WRITE_INDEXED: &str = "rt_write_indexed";
    pub const READ_ZP_PTR_Y: &str = "rt_read_zp_ptr_y";
    pub const WRITE_ZP_PTR_Y: &str = "rt_write_zp_ptr_y";
    pub const ASL_A: &str = "rt_asl_a";
    pub const ASL_MEM: &str = "rt_asl_mem";
    pub const LSR_A: &str = "rt_lsr_a";
    pub const LSR_MEM: &str = "rt_lsr_mem";
    pub const ROL_A: &str = "rt_rol_a";
    pub const ROL_MEM: &str = "rt_rol_mem";
    pub const ROR_A: &str = "rt_ror_a";
    pub const ROR_MEM: &str = "rt_ror_mem";
    pub const BIT_MEM: &str = "rt_bit_mem";
    pub const INC_MEM: &str = "rt_inc_mem";
    pub const DEC_MEM: &str = "rt_dec_mem";
    pub const CPX_A: &str = "rt_cpx_a";
    pub const CPY_A: &str = "rt_cpy_a";
    pub const UNRESOLVED_JSR: &str = "rt_unresolved_jsr";
    pub const BRK: &str = "rt_brk";
    pub const FAR_CALL: &str = "rt_far_call";
    pub const FAR_JMP: &str = "rt_far_jmp";
}

// ---------------------------------------------------------------------------
// LowerOptions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LowerOptions<'p> {
    pub profile: Option<&'p profile::Profile>,
    /// If true, every IR Op emits a `; 6502 $XXXX: ...` comment in the Z80 listing.
    pub emit_source_comments: bool,
}

impl<'p> Default for LowerOptions<'p> {
    fn default() -> Self {
        Self {
            profile: None,
            emit_source_comments: true,
        }
    }
}

// ---------------------------------------------------------------------------
// LowerError
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LowerError {
    UnsupportedOp { pc: Option<u16>, reason: String },
    UnstableOpcode { pc: u16, opcode: u8 },
    IndirectAddrNotSupported { mode: String },
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::UnsupportedOp {
                pc: Some(pc),
                reason,
            } => {
                write!(f, "unsupported op at ${pc:04X}: {reason}")
            }
            LowerError::UnsupportedOp { pc: None, reason } => {
                write!(f, "unsupported op: {reason}")
            }
            LowerError::UnstableOpcode { pc, opcode } => {
                write!(f, "unstable opcode {opcode:#04X} at ${pc:04X}")
            }
            LowerError::IndirectAddrNotSupported { mode } => {
                write!(f, "indirect addressing not supported: {mode}")
            }
        }
    }
}

impl std::error::Error for LowerError {}

// ---------------------------------------------------------------------------
// Address translation helper
// ---------------------------------------------------------------------------

/// Map a NES RAM address to its SMS RAM equivalent.
/// Translate a NES "absolute" base address (the constant part of an
/// indexed address) into the SMS-side address the runtime helper should
/// receive. RAM/RamMirror/Stack/ZeroPage all map to the SMS $C000+ block;
/// PRG-ROM bases in the lower $8000-$BFFF window stay raw because the SMS
/// project mirrors that NES PRG window into slot 2 for data-table reads.
fn indexed_base_to_sms(base: u16, region: ir::MemRegion) -> u16 {
    use ir::MemRegion;
    match region {
        MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack => {
            if base < 0x2000 {
                nes_ram_addr_to_sms(base)
            } else {
                base
            }
        }
        MemRegion::ZeroPage => sms_layout::NES_ZP_BASE + (base & 0xFF),
        _ => base,
    }
}

fn indexed_read_runtime(base: u16, region: ir::MemRegion) -> &'static str {
    if region == ir::MemRegion::PrgRom && base >= 0xC000 {
        runtime_symbols::READ_PRG_HIGH_INDEXED
    } else {
        runtime_symbols::ROUTE_INDEXED
    }
}

/// Emit a flag-bit set or clear on the shadow status byte without
/// modifying A or any other emulated 6502 state.
///
/// `mask` is applied to the loaded shadow-P byte. `set` chooses OR vs AND.
/// Common emission for LDX/LDY from any supported addressing mode.
/// Loads memory into A (transient), stores A into `shadow_addr` (the
/// SMS RAM byte holding the X or Y shadow), updates shadow N/Z based
/// on A. Caller's A is preserved without restoring the old flags.
fn emit_ldxy_mem(
    program: &mut z80_emit::Program,
    addr: &ir::AddrExpr,
    region: ir::MemRegion,
    shadow_addr: u16,
) {
    use ir::{AddrExpr, MemRegion};
    program.push_af();
    match (addr, region) {
        (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
            program.ld_a_abs(sms_layout::NES_ZP_BASE + *z as u16);
        }
        (AddrExpr::Const(a), MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack) => {
            program.ld_a_abs(nes_ram_addr_to_sms(*a));
        }
        (AddrExpr::Const(a), MemRegion::PpuReg | MemRegion::PpuMirror) => {
            program.ld_b_imm((*a & 0x0007) as u8);
            program.call(runtime_symbols::PPU_READ);
        }
        (AddrExpr::Const(a), MemRegion::PrgRom) if *a < 0xC000 => {
            program.ld_a_abs(*a);
        }
        (AddrExpr::Const(a), MemRegion::PrgRom) => {
            program.ld_hl_imm(*a);
            program.ld_b_imm(0);
            program.call(runtime_symbols::READ_PRG_HIGH_INDEXED);
        }
        (AddrExpr::AbsIndexedX(base), _) => {
            program.ld_hl_imm(indexed_base_to_sms(*base, region));
            program.ld_a_abs(sms_layout::SHADOW_X);
            program.ld_b_a();
            program.call(indexed_read_runtime(*base, region));
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            program.ld_hl_imm(indexed_base_to_sms(*base, region));
            program.ld_a_abs(sms_layout::SHADOW_Y);
            program.ld_b_a();
            program.call(indexed_read_runtime(*base, region));
        }
        (AddrExpr::ZpIndexedX(zp), _) => {
            program.ld_a_abs(sms_layout::SHADOW_X);
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            program.ld_a_hl_ptr();
        }
        (AddrExpr::ZpIndexedY(zp), _) => {
            program.ld_a_abs(sms_layout::SHADOW_Y);
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            program.ld_a_hl_ptr();
        }
        _ => {
            program.comment("WARN: unresolved LDX/LDY addressing mode");
            program.ld_a_imm(0x00);
        }
    }
    program.ld_abs_a(shadow_addr);
    program.call(runtime_symbols::SET_NZ_A);
    // `push af` saved caller A in the high byte. Pop into BC and restore
    // only A, leaving the N/Z flags produced by SET_NZ_A live for a
    // following 6502 branch (e.g. LDY mem; BEQ).
    program.pop_bc();
    program.ld_a_b();
}

fn restore_a_keep_flags_after_push_af(program: &mut z80_emit::Program) {
    program.pop_bc();
    program.ld_a_b();
}

/// Common emission for STX/STY into any supported addressing mode.
/// `shadow_addr` is the SMS RAM address holding the X or Y shadow byte.
/// Preserves A and the caller's flags via push/pop AF.
fn emit_stxy_mem(
    program: &mut z80_emit::Program,
    addr: &ir::AddrExpr,
    region: ir::MemRegion,
    shadow_addr: u16,
) {
    use ir::{AddrExpr, MemRegion};
    program.push_af();
    match (addr, region) {
        (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
            program.ld_a_abs(shadow_addr);
            program.ld_abs_a(sms_layout::NES_ZP_BASE + *z as u16);
        }
        (AddrExpr::Const(a), MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack) => {
            program.ld_a_abs(shadow_addr);
            program.ld_abs_a(nes_ram_addr_to_sms(*a));
        }
        (AddrExpr::AbsIndexedX(base), _) => {
            // Load X/Y into the value, also load X (index) — but the value
            // and the index can be the same shadow byte. Use C as scratch.
            program.ld_a_abs(shadow_addr);
            program.ld_c_a(); // C = value (X or Y)
            program.ld_hl_imm(indexed_base_to_sms(*base, region));
            program.ld_a_abs(sms_layout::SHADOW_X);
            program.ld_b_a();
            program.ld_a_c();
            program.call(runtime_symbols::WRITE_INDEXED);
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            program.ld_a_abs(shadow_addr);
            program.ld_c_a();
            program.ld_hl_imm(indexed_base_to_sms(*base, region));
            program.ld_a_abs(sms_layout::SHADOW_Y);
            program.ld_b_a();
            program.ld_a_c();
            program.call(runtime_symbols::WRITE_INDEXED);
        }
        (AddrExpr::ZpIndexedX(zp), _) => {
            // (zp + X) & $FF wrap.
            program.ld_a_abs(sms_layout::SHADOW_X);
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            program.ld_a_abs(shadow_addr);
            program.ld_hl_ptr_a();
        }
        (AddrExpr::ZpIndexedY(zp), _) => {
            program.ld_a_abs(sms_layout::SHADOW_Y);
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            program.ld_a_abs(shadow_addr);
            program.ld_hl_ptr_a();
        }
        _ => {
            program.comment("WARN: unresolved STX/STY addressing mode");
        }
    }
    program.pop_af();
}

fn emit_flag_update(program: &mut z80_emit::Program, mask: u8, set: bool) {
    program.push_af();
    program.ld_a_abs(sms_layout::SHADOW_P);
    if set {
        program.or_imm(mask);
    } else {
        program.and_imm(mask);
    }
    program.ld_abs_a(sms_layout::SHADOW_P);
    program.pop_af();
}

fn nes_ram_addr_to_sms(nes_addr: u16) -> u16 {
    let masked = if nes_addr < 0x2000 {
        nes_addr & 0x07FF
    } else {
        nes_addr
    };
    if masked < 0x0100 {
        0xC000 + masked
    } else if masked < 0x0200 {
        0xC100 + (masked - 0x0100)
    } else if masked < 0x0800 {
        0xC200 + (masked - 0x0200)
    } else {
        panic!("not a RAM addr: ${masked:04X}")
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Parse `L_XXXX` back to a u16 address for profile lookups.
fn parse_label_addr(label: &str) -> Option<u16> {
    let hex = label.strip_prefix("L_")?;
    u16::from_str_radix(hex, 16).ok()
}

/// Emit a load of a ValueSrc into Z80 A.  Used by hardware write ops.
fn emit_value_src_to_a(p: &mut z80_emit::Program, src: &ir::ValueSrc) {
    use ir::ValueSrc;
    use sms_layout::*;
    match src {
        ValueSrc::A => {}
        ValueSrc::X => {
            p.ld_a_abs(SHADOW_X);
        }
        ValueSrc::Y => {
            p.ld_a_abs(SHADOW_Y);
        }
        ValueSrc::Imm(v) => {
            p.ld_a_imm(*v);
        }
        ValueSrc::Mem { addr, region } => {
            // Best-effort: for constant addresses we translate; otherwise load A.
            if let Some(sms) = const_addr_to_sms(addr, *region) {
                p.ld_a_abs(sms);
            } else {
                // Complex addressing: just load 0 as a placeholder and comment.
                p.comment("WARN: complex ValueSrc::Mem not fully resolved; loading 0");
                p.ld_a_imm(0x00);
            }
        }
    }
}

/// Attempt to resolve a constant AddrExpr to an SMS RAM address.
fn const_addr_to_sms(addr: &ir::AddrExpr, region: ir::MemRegion) -> Option<u16> {
    use ir::{AddrExpr, MemRegion};
    match (addr, region) {
        (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => Some(sms_layout::NES_ZP_BASE + *z as u16),
        (AddrExpr::Const(a), MemRegion::Ram) => Some(nes_ram_addr_to_sms(*a)),
        (AddrExpr::Const(a), MemRegion::RamMirror) => Some(nes_ram_addr_to_sms(*a)),
        (AddrExpr::Const(a), MemRegion::Stack) => Some(nes_ram_addr_to_sms(*a)),
        (AddrExpr::ZpConst(z), MemRegion::Stack) => Some(nes_ram_addr_to_sms(*z as u16)),
        _ => None,
    }
}

/// Emit instructions that load a memory operand into Z80 B, ready for an
/// ALU runtime call.  Falls back to a comment for unresolvable modes.
fn emit_mem_to_b(p: &mut z80_emit::Program, addr: &ir::AddrExpr, region: ir::MemRegion) {
    use ir::{AddrExpr, MemRegion};
    use runtime_symbols::*;
    use sms_layout::*;

    if let Some(sms) = const_addr_to_sms(addr, region) {
        // CRITICAL: must NOT clobber A. ALU ops use the current A as
        // the LHS, so loading M into B has to go through HL→B.  This
        // covers RAM, mirrors, zero page, and stack-page absolute forms
        // such as SMB's `BIT $01A9` in the metatile collision path.
        p.ld_hl_imm(sms);
        p.ld_b_hl_ptr();
        return;
    }

    match addr {
        AddrExpr::Const(a) if region == MemRegion::PrgRom && *a < 0xC000 => {
            p.ld_hl_imm(*a);
            p.ld_b_hl_ptr();
        }
        AddrExpr::Const(a) if region == MemRegion::PrgRom => {
            p.ld_c_a();
            p.ld_hl_imm(*a);
            p.ld_b_imm(0);
            p.call(READ_PRG_HIGH_INDEXED);
            p.ld_b_a();
            p.ld_a_c();
        }
        AddrExpr::AbsIndexedX(base) => {
            // For indexed reads we must preserve A across rt_read_indexed
            // (which returns its result in A). Save A in C, do the read,
            // move the result to B, restore A from C.
            p.ld_c_a();
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_a_abs(SHADOW_X);
            p.ld_b_a();
            p.call(indexed_read_runtime(*base, region));
            p.ld_b_a();
            p.ld_a_c();
        }
        AddrExpr::AbsIndexedY(base) => {
            p.ld_c_a();
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_a_abs(SHADOW_Y);
            p.ld_b_a();
            p.call(indexed_read_runtime(*base, region));
            p.ld_b_a();
            p.ld_a_c();
        }
        AddrExpr::ZpIndexedX(zp) => {
            // 6502 zp,X wraps within zero page. Compute (zp+X) & $FF in A,
            // load value via HL=$C000 | offset into B (preserving caller A).
            p.ld_c_a(); // C = caller A
            p.ld_a_abs(SHADOW_X);
            p.add_a_imm(*zp);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.ld_b_hl_ptr();
            p.ld_a_c(); // restore A
        }
        AddrExpr::ZpIndexedY(zp) => {
            p.ld_c_a();
            p.ld_a_abs(SHADOW_Y);
            p.add_a_imm(*zp);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.ld_b_hl_ptr();
            p.ld_a_c();
        }
        AddrExpr::IndirectY(zp) => {
            p.ld_c_a();
            p.ld_b_imm(*zp);
            p.call(READ_ZP_PTR_Y);
            p.ld_b_a();
            p.ld_a_c();
        }
        AddrExpr::IndirectX(_zp) => {
            p.comment("WARN: IndirectX mem read not yet fully implemented");
            p.ld_b_imm(0x00);
        }
        _ => {
            p.comment("WARN: unresolved mem-to-B mode");
            p.ld_b_imm(0x00);
        }
    }
}

/// Emit HL = SMS address for INC/DEC mem operations.
///
/// Note: this function is permitted to clobber A for indexed modes
/// (caller brackets with push/pop AF).  IndirectX/Y are not yet
/// implemented; they fall to a no-op marker that writes nothing.
/// TODO: many SMB routines use `INC $xxxx,X` for in-RAM counters and
/// the indexed lowering here makes the NMI ~10x slower than the
/// unfixed version because translated code now does the real work.
/// Need to investigate why — possibly a regression in scope of effects.
fn emit_hl_for_rw_mem(p: &mut z80_emit::Program, addr: &ir::AddrExpr, region: ir::MemRegion) {
    use ir::{AddrExpr, MemRegion};
    use sms_layout::*;
    match addr {
        AddrExpr::ZpConst(z) if region == MemRegion::ZeroPage => {
            p.ld_hl_imm(NES_ZP_BASE + *z as u16);
        }
        AddrExpr::Const(a) if region == MemRegion::Ram || region == MemRegion::RamMirror => {
            p.ld_hl_imm(nes_ram_addr_to_sms(*a));
        }
        AddrExpr::AbsIndexedX(base) => {
            p.push_af();
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_a_abs(SHADOW_X);
            p.ld_c_a();
            p.ld_b_imm(0);
            p.add_hl_bc();
            p.pop_af();
        }
        AddrExpr::AbsIndexedY(base) => {
            p.push_af();
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_a_abs(SHADOW_Y);
            p.ld_c_a();
            p.ld_b_imm(0);
            p.add_hl_bc();
            p.pop_af();
        }
        AddrExpr::ZpIndexedX(z) => {
            p.push_af();
            p.ld_a_abs(SHADOW_X);
            p.add_a_imm(*z);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.pop_af();
        }
        AddrExpr::ZpIndexedY(z) => {
            p.push_af();
            p.ld_a_abs(SHADOW_Y);
            p.add_a_imm(*z);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.pop_af();
        }
        _ => {
            p.comment("WARN: complex addr for rw-mem operation");
            p.ld_hl_imm(0x0000);
        }
    }
}

// ---------------------------------------------------------------------------
// Flag-liveness analysis
// ---------------------------------------------------------------------------

/// Returns true if the N/Z flags set by `ops[i]` are read by a later
/// op before being overwritten. Used to skip redundant `rt_set_nz_a`
/// calls — SMB does many `LDA / STA` sequences where no branch reads
/// the flags between, so the flag-update is dead code.
///
/// Conservative on routine boundaries: any op that ends the routine
/// (Jmp/Jsr/Rts/Rti/BranchIf with NZ-reading cond) keeps the flag
/// update live, since downstream code in another routine may rely on
/// it (the 6502 callee may see the caller's NZ state via PHP/PLP).
fn nz_flags_live_after(ops: &[ir::Op], i: usize) -> bool {
    use ir::Op;
    for op in ops.iter().skip(i + 1) {
        // Reads NZ? → live.
        if reads_nz(op) {
            return true;
        }
        // Routine boundary: be conservative and keep flags live.
        if matches!(
            op,
            Op::Rts
                | Op::Rti
                | Op::Jsr { .. }
                | Op::JsrUnknown { .. }
                | Op::JumpEngineCall { .. }
                | Op::Jmp { .. }
                | Op::JmpIndirect { .. }
                | Op::Brk
                | Op::Pha
                | Op::Php
                | Op::PpuWrite { .. }
                | Op::PpuRead { .. }
                | Op::ApuWrite { .. }
                | Op::ApuRead { .. }
                | Op::OamDmaWrite { .. }
                | Op::MapperWrite { .. }
                | Op::ControllerRead { .. }
                | Op::Unsupported { .. }
                | Op::Jam { .. }
        ) {
            return true;
        }
        // Overwrites NZ? → previous flags are dead.
        if overwrites_nz(op) {
            return false;
        }
        // Anything else (Source, Label, StaMem, StxMem, StyMem,
        // SaxMem, Sec/Clc/Sei/Cli/Clv/Cld/Sed, Txs, Nop, Brk) doesn't
        // touch NZ — keep scanning.
    }
    // End of routine reached without a read or overwrite. Conservative:
    // assume the caller (or fall-through) might read.
    true
}

fn reads_nz(op: &ir::Op) -> bool {
    use ir::{Cond, Op};
    match op {
        Op::BranchIf { cond, .. } => matches!(
            cond,
            Cond::Zero | Cond::NotZero | Cond::Negative | Cond::Positive
        ),
        // Php reads the whole P register, so it reads NZ.
        Op::Php => true,
        _ => false,
    }
}

fn overwrites_nz(op: &ir::Op) -> bool {
    use ir::Op;
    matches!(
        op,
        Op::LdaImm(_)
            | Op::LdaMem { .. }
            | Op::LdxImm(_)
            | Op::LdxMem { .. }
            | Op::LdyImm(_)
            | Op::LdyMem { .. }
            | Op::AdcImm(_)
            | Op::AdcMem { .. }
            | Op::SbcImm(_)
            | Op::SbcMem { .. }
            | Op::AndImm(_)
            | Op::AndMem { .. }
            | Op::OraImm(_)
            | Op::OraMem { .. }
            | Op::EorImm(_)
            | Op::EorMem { .. }
            | Op::CmpImm(_)
            | Op::CmpMem { .. }
            | Op::CpxImm(_)
            | Op::CpxMem { .. }
            | Op::CpyImm(_)
            | Op::CpyMem { .. }
            | Op::BitMem { .. }
            | Op::AslA
            | Op::AslMem { .. }
            | Op::LsrA
            | Op::LsrMem { .. }
            | Op::RolA
            | Op::RolMem { .. }
            | Op::RorA
            | Op::RorMem { .. }
            | Op::IncMem { .. }
            | Op::DecMem { .. }
            | Op::Inx
            | Op::Iny
            | Op::Dex
            | Op::Dey
            | Op::Tax
            | Op::Tay
            | Op::Txa
            | Op::Tya
            | Op::Tsx
            | Op::Pla
            | Op::Plp
    )
}

// ---------------------------------------------------------------------------
// lower_routine
// ---------------------------------------------------------------------------

/// Lower one IR routine into a `z80_emit::Program`. The routine's entry
/// label is `routine.name`. Internal labels (`L_XXXX`) become Z80 labels.
/// External `L_XXXX` references are emitted as `call`/`jp` to a label
/// that the linker (the cli, later) will resolve.
pub fn lower_routine(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    opts: &LowerOptions,
) -> Result<(), LowerError> {
    use ir::{AddrExpr, Cond, MemRegion, Op};
    use runtime_symbols::*;
    use sms_layout::*;

    // Pre-compute flag liveness: for each op whose result sets N/Z,
    // is the flag read by a later op before being overwritten?
    // Cuts ~60% of SET_NZ_A invocations on SMB by eliding the call
    // when no downstream branch / PHP / read-modify uses the flags.
    let nz_live: Vec<bool> = routine
        .ops
        .iter()
        .enumerate()
        .map(|(i, _)| nz_flags_live_after(&routine.ops, i))
        .collect();

    for (op_idx, op) in routine.ops.iter().enumerate() {
        match op {
            // ------------------------------------------------------------------
            Op::Label(name) => {
                program.label(name);
            }

            // ------------------------------------------------------------------
            Op::Source { pc, text } => {
                if opts.emit_source_comments {
                    program.comment(format!("6502 ${pc:04X}: {text}"));
                }
            }

            // ------------------------------------------------------------------
            Op::Nop => {
                program.nop();
            }

            // ------------------------------------------------------------------
            Op::Brk => {
                program.comment("BRK");
                program.call(BRK);
            }

            // ------------------------------------------------------------------
            Op::Jam { pc, opcode } => {
                return Err(LowerError::UnsupportedOp {
                    pc: Some(*pc),
                    reason: format!("JAM opcode {opcode:#04X}"),
                });
            }

            // ------------------------------------------------------------------
            Op::Unsupported {
                pc,
                mnemonic,
                reason,
                ..
            } => {
                return Err(LowerError::UnsupportedOp {
                    pc: Some(*pc),
                    reason: format!("{mnemonic}: {reason}"),
                });
            }

            // ------------------------------------------------------------------
            // Loads
            // ------------------------------------------------------------------
            Op::LdaImm(v) => {
                program.ld_a_imm(*v);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::LdaMem { addr, region } => {
                match (addr, region) {
                    (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
                        program.ld_a_abs(NES_ZP_BASE + *z as u16);
                    }
                    (
                        AddrExpr::Const(a),
                        MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack,
                    ) => {
                        program.ld_a_abs(nes_ram_addr_to_sms(*a));
                    }
                    (AddrExpr::Const(a), MemRegion::PrgRom) if *a < 0xC000 => {
                        program.ld_a_abs(*a);
                    }
                    (AddrExpr::Const(a), MemRegion::PrgRom) => {
                        program.ld_hl_imm(*a);
                        program.ld_b_imm(0);
                        program.call(READ_PRG_HIGH_INDEXED);
                    }
                    (AddrExpr::AbsIndexedX(0x4016), MemRegion::ApuIo) => {
                        program.call(CONTROLLER_READ_INDEXED_X);
                    }
                    (AddrExpr::AbsIndexedX(base), _) => {
                        program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                        program.ld_a_abs(SHADOW_X);
                        program.ld_b_a();
                        program.call(indexed_read_runtime(*base, *region));
                    }
                    (AddrExpr::AbsIndexedY(base), _) => {
                        program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                        program.ld_a_abs(SHADOW_Y);
                        program.ld_b_a();
                        program.call(indexed_read_runtime(*base, *region));
                    }
                    (AddrExpr::ZpIndexedX(zp), _) => {
                        // 6502 zp,X wraps within zero page: (zp + X) & $FF.
                        // rt_read_indexed adds 16-bit, no wrap, so do the
                        // wrap inline.
                        program.ld_a_abs(SHADOW_X);
                        program.add_a_imm(*zp);
                        program.ld_l_a();
                        program.ld_h_imm((NES_ZP_BASE >> 8) as u8);
                        program.ld_a_hl_ptr();
                    }
                    (AddrExpr::ZpIndexedY(zp), _) => {
                        program.ld_a_abs(SHADOW_Y);
                        program.add_a_imm(*zp);
                        program.ld_l_a();
                        program.ld_h_imm((NES_ZP_BASE >> 8) as u8);
                        program.ld_a_hl_ptr();
                    }
                    (AddrExpr::IndirectY(zp), _) => {
                        program.ld_b_imm(*zp);
                        program.call(READ_ZP_PTR_Y);
                    }
                    (AddrExpr::IndirectX(zp), _) => {
                        program.comment("WARN: IndirectX LDA not fully implemented");
                        program.ld_b_imm(*zp);
                        program.call(READ_ZP_PTR_Y);
                    }
                    _ => {
                        program.comment("WARN: unresolved LdaMem addressing mode");
                        program.ld_a_imm(0x00);
                    }
                }
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            // LDX/LDY (any form) load shadow X or Y and update N/Z, but
            // must NOT modify A. Save AF, call rt_set_nz_a while A holds
            // the new register value, then restore only A so the new flags
            // remain live for a following branch.
            Op::LdxImm(v) => {
                program.push_af();
                program.ld_a_imm(*v);
                program.ld_abs_a(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                    restore_a_keep_flags_after_push_af(program);
                } else {
                    program.pop_af();
                }
            }

            Op::LdxMem { addr, region } => {
                emit_ldxy_mem(program, addr, *region, sms_layout::SHADOW_X);
            }

            Op::LdyImm(v) => {
                program.push_af();
                program.ld_a_imm(*v);
                program.ld_abs_a(SHADOW_Y);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                    restore_a_keep_flags_after_push_af(program);
                } else {
                    program.pop_af();
                }
            }

            Op::LdyMem { addr, region } => {
                emit_ldxy_mem(program, addr, *region, sms_layout::SHADOW_Y);
            }

            // ------------------------------------------------------------------
            // Stores
            // ------------------------------------------------------------------
            Op::StaMem { addr, region } => {
                match (addr, region) {
                    (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
                        program.ld_abs_a(NES_ZP_BASE + *z as u16);
                    }
                    (
                        AddrExpr::Const(a),
                        MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack,
                    ) => {
                        program.ld_abs_a(nes_ram_addr_to_sms(*a));
                    }
                    (AddrExpr::AbsIndexedX(base), _) => {
                        program.ld_c_a(); // save value in C
                        program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                        program.ld_a_abs(SHADOW_X);
                        program.ld_b_a();
                        program.ld_a_c();
                        program.call(WRITE_INDEXED);
                    }
                    (AddrExpr::AbsIndexedY(base), _) => {
                        program.ld_c_a();
                        program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                        program.ld_a_abs(SHADOW_Y);
                        program.ld_b_a();
                        program.ld_a_c();
                        program.call(WRITE_INDEXED);
                    }
                    (AddrExpr::ZpIndexedX(zp), _) => {
                        // 6502 zp,X wraps within zero page: addr = (zp + X) & $FF.
                        // Compute the wrapped offset in A, then build HL = $C000 + offset.
                        // Save value first, since A is the value to write.
                        program.ld_c_a(); // C = value
                        program.ld_a_abs(SHADOW_X);
                        program.add_a_imm(*zp);
                        program.ld_l_a();
                        program.ld_h_imm((NES_ZP_BASE >> 8) as u8);
                        program.ld_a_c();
                        program.ld_hl_ptr_a();
                    }
                    (AddrExpr::IndirectY(zp), _) => {
                        // entry: B=zp, A=value
                        program.ld_b_imm(*zp);
                        program.call(WRITE_ZP_PTR_Y);
                    }
                    _ => {
                        program.comment("WARN: unresolved StaMem addressing mode");
                    }
                }
            }

            Op::StxMem { addr, region } => {
                // STX must NOT modify A. Bracket with push/pop AF, mirror
                // StaMem's addressing-mode coverage.
                emit_stxy_mem(program, addr, *region, sms_layout::SHADOW_X);
            }

            Op::StyMem { addr, region } => {
                emit_stxy_mem(program, addr, *region, sms_layout::SHADOW_Y);
            }

            Op::SaxMem { addr, region } => {
                // SAX: M := A & X. No flag changes. A and X both preserved.
                // Strategy: save A in scratch (C), AND A with shadow X, write
                // to memory using the same emit_stxy_mem path (which already
                // handles all addressing modes), then restore A.
                program.push_af();
                program.ld_c_a(); // C = original A
                program.ld_a_abs(sms_layout::SHADOW_X);
                program.and_c(); // A = A & C = X & A
                // Stash the AND result in shadow X temporarily so we can
                // reuse emit_stxy_mem; restore X after.
                program.push_bc(); // save B,C; C still holds orig A
                program.ld_b_a(); // B = (A & X) value to write
                program.ld_a_abs(sms_layout::SHADOW_X);
                program.push_af(); // save shadow X on Z80 stack
                program.ld_a_b();
                program.ld_abs_a(sms_layout::SHADOW_X); // SHADOW_X = (A & X) value temporarily
                emit_stxy_mem(program, addr, *region, sms_layout::SHADOW_X);
                program.pop_af();
                program.ld_abs_a(sms_layout::SHADOW_X); // restore real X
                program.pop_bc();
                program.ld_a_c(); // restore A
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Transfers
            // ------------------------------------------------------------------
            Op::Tax => {
                program.ld_abs_a(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Tay => {
                program.ld_abs_a(SHADOW_Y);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Txa => {
                program.ld_a_abs(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Tya => {
                program.ld_a_abs(SHADOW_Y);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Tsx => {
                program.ld_a_abs(SHADOW_S);
                program.ld_abs_a(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Txs => {
                // TXS sets S := X with no flag effect AND no change to A.
                program.push_af();
                program.ld_a_abs(SHADOW_X);
                program.ld_abs_a(SHADOW_S);
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Stack
            // ------------------------------------------------------------------
            Op::Pha => {
                program.call(PUSH_6502);
            }

            Op::Pla => {
                program.call(POP_6502);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::Php => {
                // PHP pushes shadow P onto the emulated 6502 stack;
                // A must be preserved.
                program.push_af();
                program.ld_a_abs(SHADOW_P);
                program.call(PUSH_6502);
                program.pop_af();
            }

            Op::Plp => {
                // PLP pops a value from the emulated 6502 stack into
                // shadow P; A must be preserved.
                program.push_af();
                program.call(POP_6502);
                program.ld_abs_a(SHADOW_P);
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Flag operations
            // ------------------------------------------------------------------

            // Flag-clear/set ops must preserve A. 6502 CLC/SEC/CLI/SEI/CLV/CLD/SED
            // all leave the accumulator unchanged; the naive `ld a,(SHADOW_P);
            // and/or imm; ld (SHADOW_P),a` sequence destroys A, so we bracket
            // with push/pop.
            Op::Clc => emit_flag_update(program, 0xFE, false),
            Op::Sec => emit_flag_update(program, 0x01, true),
            Op::Cli => emit_flag_update(program, 0xFB, false),
            Op::Sei => emit_flag_update(program, 0x04, true),
            Op::Clv => emit_flag_update(program, 0xBF, false),
            Op::Cld => emit_flag_update(program, 0xF7, false),
            Op::Sed => emit_flag_update(program, 0x08, true),

            // ------------------------------------------------------------------
            // ALU: ADC / SBC
            // ------------------------------------------------------------------
            Op::AdcImm(v) => {
                program.ld_b_imm(*v);
                program.call(ADC_A_VIA_SHADOW);
            }

            Op::AdcMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(ADC_A_VIA_SHADOW);
            }

            Op::SbcImm(v) => {
                program.ld_b_imm(*v);
                program.call(SBC_A_VIA_SHADOW);
            }

            Op::SbcMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(SBC_A_VIA_SHADOW);
            }

            // ------------------------------------------------------------------
            // ALU: AND / ORA / EOR
            // ------------------------------------------------------------------
            Op::AndImm(v) => {
                program.and_imm(*v);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::AndMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                // A & B via: push af, save A in C, get B, AND
                // Simpler: A already in A, B already in B after emit_mem_to_b.
                // Use and_a then... but z80_emit has no `and b`. Use add_a_b trick? No.
                // Emit raw byte: and b = 0xA0
                program.comment("and b  ; A = A & B");
                // Use the Program's raw emit1 isn't public. Use data() workaround:
                // Actually we can note that z80_emit doesn't expose `and b` directly.
                // Use: `push bc; pop hl; ... ` — too complex.
                // Simplest: load B back as an immediate isn't possible.
                // Solution: re-read from memory. For constant addresses, re-read.
                // For indexed: call rt_set_nz_a after the indexed read already gave us the value.
                // Restructure: emit_mem_operand_into_a then use and_imm(0xFF) won't work.
                //
                // We need `and b`. z80_emit doesn't have it. Add a data() hack isn't clean.
                // Best path: load the memory value back into A via a separate load, then
                // the AND needs to be done differently.
                //
                // We'll push AF, get mem into B, pop AF, and apply the AND inline.
                // But we already called emit_mem_to_b above. Let's redo this via a helper
                // that returns the value in A (not B), and use the and_a pattern.
                //
                // For correctness, abandon the B-loading approach for logical ops;
                // instead push A, load mem into A, save as temp, pop A, then... no.
                //
                // Simplest clean solution: for AND/OR/EOR immediate-like forms,
                // if memory address is constant just load it and use a temp path.
                // For now emit data byte 0xA0 = `and b` directly via .db.
                program.data(None, &[0xA0]); // and b
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::OraImm(v) => {
                program.or_imm(*v);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::OraMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.comment("or b  ; A = A | B");
                program.data(None, &[0xB0]); // or b
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::EorImm(v) => {
                program.xor_imm(*v);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::EorMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.comment("xor b  ; A = A ^ B");
                program.data(None, &[0xA8]); // xor b
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            // ------------------------------------------------------------------
            // ALU: CMP / CPX / CPY
            // ------------------------------------------------------------------
            Op::CmpImm(v) => {
                program.ld_b_imm(*v);
                program.call(CMP_A_VIA_SHADOW);
            }

            Op::CmpMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(CMP_A_VIA_SHADOW);
            }

            Op::CpxImm(v) => {
                program.ld_b_imm(*v);
                program.call(CPX_A);
            }

            Op::CpxMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(CPX_A);
            }

            Op::CpyImm(v) => {
                program.ld_b_imm(*v);
                program.call(CPY_A);
            }

            Op::CpyMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(CPY_A);
            }

            // ------------------------------------------------------------------
            // BIT
            // ------------------------------------------------------------------
            Op::BitMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.call(BIT_MEM);
            }

            // ------------------------------------------------------------------
            // Shifts / rotates
            // ------------------------------------------------------------------
            Op::AslA => {
                program.call(ASL_A);
            }

            Op::AslMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(ASL_MEM);
            }

            Op::LsrA => {
                program.call(LSR_A);
            }

            Op::LsrMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(LSR_MEM);
            }

            Op::RolA => {
                program.call(ROL_A);
            }

            Op::RolMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(ROL_MEM);
            }

            Op::RorA => {
                program.call(ROR_A);
            }

            Op::RorMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(ROR_MEM);
            }

            // ------------------------------------------------------------------
            // INC / DEC memory
            // ------------------------------------------------------------------
            Op::IncMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(INC_MEM);
            }

            Op::DecMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                program.call(DEC_MEM);
            }

            // ------------------------------------------------------------------
            // INX / INY / DEX / DEY
            // ------------------------------------------------------------------

            // INX/INY/DEX/DEY update shadow X or Y plus N/Z. A is NOT
            // touched on the 6502, so bracket with push/pop AF.
            Op::Inx => {
                program.push_af();
                program.ld_a_abs(SHADOW_X);
                program.inc_a();
                program.ld_abs_a(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
                program.pop_af();
            }

            Op::Iny => {
                program.push_af();
                program.ld_a_abs(SHADOW_Y);
                program.inc_a();
                program.ld_abs_a(SHADOW_Y);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
                program.pop_af();
            }

            Op::Dex => {
                program.push_af();
                program.ld_a_abs(SHADOW_X);
                program.dec_a();
                program.ld_abs_a(SHADOW_X);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
                program.pop_af();
            }

            Op::Dey => {
                program.push_af();
                program.ld_a_abs(SHADOW_Y);
                program.dec_a();
                program.ld_abs_a(SHADOW_Y);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Branches
            // ------------------------------------------------------------------
            Op::BranchIf { cond, target } => {
                // Use `bit n,(hl)` so the branch test doesn't clobber A.
                // Caller's HL is sacrificed (HL has no 6502 analogue), but
                // A and the other emulated registers are preserved.
                program.ld_hl_imm(SHADOW_P);
                let (bit, jump_if_set) = match cond {
                    Cond::Carry => (0u8, true),
                    Cond::NoCarry => (0, false),
                    Cond::Zero => (1, true),
                    Cond::NotZero => (1, false),
                    Cond::Overflow => (6, true),
                    Cond::NoOverflow => (6, false),
                    Cond::Negative => (7, true),
                    Cond::Positive => (7, false),
                };
                program.bit_n_hl_ptr(bit);
                // `bit n,(hl)` sets native Z if the bit is CLEAR.
                // For intra-routine branches (target is one of our own
                // `branch_labels` or matches the routine entry), a plain
                // jp_z/jp_nz works because the routine occupies one
                // section and slot 1 won't change mid-execution.
                // For cross-routine branches we check the label-section
                // map (seeded by pass-1 dry lowering): if the target
                // lives in the same section as we're currently emitting
                // into, slot 1 will hold the same bank when the branch
                // is taken and a plain jp_z/jp_nz works. Only truly
                // cross-section branches need the skip-around far_jmp.
                let local = routine.branch_labels.contains(target)
                    || target == &routine.name
                    || program.label_section_idx(target) == Some(program.current_section_idx());
                if local {
                    if jump_if_set {
                        program.jp_nz(target);
                    } else {
                        program.jp_z(target);
                    }
                } else {
                    let skip = program.fresh_label("br_skip");
                    if jump_if_set {
                        // Branch taken when bit was 1 → Z80 Z=0.
                        // Skip the far_jmp if branch NOT taken: jp z skip.
                        program.jp_z(&skip);
                    } else {
                        program.jp_nz(&skip);
                    }
                    program.far_jmp(target);
                    program.label(&skip);
                }
            }

            // ------------------------------------------------------------------
            // Jumps / calls
            // ------------------------------------------------------------------
            Op::Jmp { target } => {
                // Translated-label JMPs can cross banks (e.g., JMP $8745
                // from InitScreen lands in IncSubtask which may be
                // pinned to a different code bank). Use far_jmp so slot
                // 1 is mapped correctly. Runtime helpers (rt_*) live in
                // bank 0 and are reachable via plain jp.
                if target.starts_with("rt_") {
                    program.jp(target);
                } else {
                    program.far_jmp(target);
                }
            }

            Op::JmpIndirect { addr } => {
                program.ld_hl_imm(*addr);
                program.call(INDIRECT_JMP);
            }

            Op::Jsr { target } => {
                // Check for profile replacement (e.g., $8082 NMI vector
                // gets remapped to `rt_vblank`).
                if let Some(profile) = opts.profile {
                    if let Some(replacement) =
                        parse_label_addr(target).and_then(|a| profile.replacement_for(a))
                    {
                        program.call(&replacement.runtime_label.clone());
                        continue;
                    }
                }
                // Translated labels (L_XXXX, func_XXXX, named SMB
                // routines) live in arbitrary banks. Use the bank-aware
                // far_call helper so the call works regardless of which
                // bank is currently in slot 1. Runtime helpers (rt_*)
                // live in bank 0 (always mapped to slot 0) so a direct
                // `call` is correct and faster.
                if target.starts_with("rt_") {
                    program.call(target);
                } else {
                    program.far_call(target);
                }
            }

            Op::JsrUnknown { addr } => {
                program.comment(format!("UNRESOLVED JSR at ${addr:04X}"));
                program.call(UNRESOLVED_JSR);
            }

            // SMB-style `JSR JumpEngine`. The original pulls the return
            // address (= pointer to the inline `.dd2` table) off the 6502
            // stack, indexes by A*2, and JMPs to the chosen target. The
            // chosen target's RTS returns to the caller of the routine that
            // invoked JumpEngine, not to the inline table.
            //
            // On Z80 we cannot tail `far_jmp` across generated banks here:
            // if the target RTS returns to the caller, slot 1 would still be
            // mapped to the target bank and the return PC would execute in
            // the wrong bank. Emit `far_call target; ret` for each selected
            // arm instead. `far_call` restores the caller bank, and the extra
            // ret consumes this routine's call frame, matching JumpEngine's
            // tail-dispatch semantics.
            Op::JumpEngineCall { targets } => {
                if targets.is_empty() {
                    program.comment("JumpEngineCall with empty targets — unreachable".to_string());
                    program.call(UNRESOLVED_JSR);
                } else {
                    // Each target is a translated label that may live in a
                    // different bank. We emit a chain of:
                    //   cp $i
                    //   jp nz, <skip_label>
                    //   far_call target
                    //   ret
                    //   skip_label:
                    // For the final entry, we drop the cp/jp_nz and just
                    // dispatch unconditionally.
                    let n = targets.len();
                    for (i, target) in targets.iter().enumerate() {
                        if i + 1 == n {
                            program.far_call(target);
                            program.ret();
                        } else {
                            let skip = program.fresh_label("je_skip");
                            program.cp_imm(i as u8);
                            program.jp_nz(&skip);
                            program.far_call(target);
                            program.ret();
                            program.label(&skip);
                        }
                    }
                }
            }

            Op::Rts => {
                program.ret();
            }

            Op::Rti => {
                program.call(POP_6502);
                program.ld_abs_a(SHADOW_P);
                program.ret();
            }

            // ------------------------------------------------------------------
            // Hardware ops
            // ------------------------------------------------------------------
            Op::PpuWrite { reg, value } => {
                emit_value_src_to_a(program, value);
                program.ld_b_imm(*reg);
                program.call(PPU_WRITE);
            }

            Op::PpuRead { reg } => {
                program.ld_b_imm(*reg);
                program.call(PPU_READ);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::OamDmaWrite { value } => {
                emit_value_src_to_a(program, value);
                program.call(OAM_DMA);
            }

            Op::ApuWrite { reg, value } => {
                emit_value_src_to_a(program, value);
                if *reg == 0x4016 {
                    program.call(CONTROLLER_STROBE);
                } else {
                    program.ld_hl_imm(*reg);
                    program.call(APU_WRITE);
                }
            }

            Op::ApuRead { reg } => {
                program.ld_hl_imm(*reg);
                program.call(APU_READ);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::ControllerRead { port } => {
                program.ld_a_imm(*port as u8);
                program.call(CONTROLLER_READ);
                if nz_live[op_idx] {
                    program.call(SET_NZ_A);
                }
            }

            Op::MapperWrite { addr, value } => {
                emit_value_src_to_a(program, value);
                program.ld_hl_imm(*addr);
                program.call(MAPPER_WRITE);
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// lower_routines
// ---------------------------------------------------------------------------

/// Convenience: lower an entire batch of routines into one Program.
pub fn lower_routines(
    program: &mut z80_emit::Program,
    routines: &[ir::Routine],
    opts: &LowerOptions,
) -> Result<(), LowerError> {
    for r in routines {
        lower_routine(program, r, opts)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ir::{AddrExpr, Cond, MemRegion, Op, Routine, ValueSrc};

    fn make_routine(name: &str, ops: Vec<Op>) -> Routine {
        // Collect any Op::Label names so BranchIf knows they are
        // intra-routine and can emit plain jp_z/jp_nz instead of the
        // cross-bank skip-around pattern.
        let branch_labels: Vec<String> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Label(name) => Some(name.clone()),
                _ => None,
            })
            .collect();
        Routine {
            entry: 0x8000,
            end: 0x8000,
            name: name.to_string(),
            ops,
            branch_labels,
            external_calls: vec![],
            unresolved: vec![],
        }
    }

    /// All runtime labels the lowering pass may `call`. Pre-defining them as
    /// stubs lets `Program::finish()` resolve every patch in unit tests without
    /// needing a real runtime section.
    fn define_runtime_stubs(prog: &mut z80_emit::Program) {
        use runtime_symbols::*;
        let labels: &[&str] = &[
            SET_NZ_A,
            ADC_A_VIA_SHADOW,
            SBC_A_VIA_SHADOW,
            CMP_A_VIA_SHADOW,
            CPX_A,
            CPY_A,
            PUSH_6502,
            POP_6502,
            INDIRECT_JMP,
            PPU_WRITE,
            PPU_READ,
            OAM_DMA,
            APU_WRITE,
            APU_READ,
            CONTROLLER_STROBE,
            CONTROLLER_READ,
            CONTROLLER_READ_INDEXED_X,
            MAPPER_WRITE,
            ROUTE_INDEXED,
            WRITE_INDEXED,
            READ_ZP_PTR_Y,
            WRITE_ZP_PTR_Y,
            ASL_A,
            ASL_MEM,
            LSR_A,
            LSR_MEM,
            ROL_A,
            ROL_MEM,
            ROR_A,
            ROR_MEM,
            BIT_MEM,
            INC_MEM,
            DEC_MEM,
            UNRESOLVED_JSR,
            BRK,
            FAR_CALL,
            FAR_JMP,
        ];
        prog.section("rt_stubs");
        for &lbl in labels {
            prog.label(lbl);
            prog.ret();
        }
        prog.section("test");
    }

    fn lower_and_finish(ops: Vec<Op>) -> z80_emit::Build {
        let routine = make_routine("test_routine", ops);
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).expect("lower failed");
        prog.finish().unwrap()
    }

    // -------------------------------------------------------------------
    // LdaImm + Rts
    // -------------------------------------------------------------------
    #[test]
    fn lda_imm_rts_bytes() {
        let build = lower_and_finish(vec![Op::LdaImm(0x42), Op::Rts]);
        // ld a,$42 = 3E 42
        assert!(build.bytes.contains(&0x3E));
        let idx = build.bytes.iter().position(|&b| b == 0x3E).unwrap();
        assert_eq!(build.bytes[idx + 1], 0x42);
        // ret = C9
        assert!(build.bytes.contains(&0xC9));
        // call rt_set_nz_a present
        assert!(build.asm.contains("call rt_set_nz_a"));
        assert!(build.asm.contains("ret"));
    }

    // -------------------------------------------------------------------
    // LdaMem ZeroPage
    // -------------------------------------------------------------------
    #[test]
    fn lda_mem_zp() {
        let build = lower_and_finish(vec![Op::LdaMem {
            addr: AddrExpr::ZpConst(0x0E),
            region: MemRegion::ZeroPage,
        }]);
        // ld a,($C00E) = 3A 0E C0
        assert!(build.bytes.windows(3).any(|w| w == [0x3A, 0x0E, 0xC0]));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // StaMem ZeroPage — no NZ update
    // -------------------------------------------------------------------
    #[test]
    fn sta_mem_zp_no_nz() {
        let build = lower_and_finish(vec![Op::StaMem {
            addr: AddrExpr::ZpConst(0x0E),
            region: MemRegion::ZeroPage,
        }]);
        // ld ($C00E),a = 32 0E C0
        assert!(build.bytes.windows(3).any(|w| w == [0x32, 0x0E, 0xC0]));
        assert!(!build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // LdaMem Const RAM
    // -------------------------------------------------------------------
    #[test]
    fn lda_mem_ram_const() {
        let build = lower_and_finish(vec![Op::LdaMem {
            addr: AddrExpr::Const(0x0700),
            region: MemRegion::Ram,
        }]);
        // $0700 → nes_ram_addr_to_sms($0700): mask=0x0700, >=0x200 & <0x0800 → 0xC200 + 0x500 = 0xC700
        assert!(build.bytes.windows(3).any(|w| w == [0x3A, 0x00, 0xC7]));
    }

    // -------------------------------------------------------------------
    // LdaMem RamMirror — mask $0808 → $0008 → $C008
    // -------------------------------------------------------------------
    #[test]
    fn lda_mem_ram_mirror_masked() {
        let build = lower_and_finish(vec![Op::LdaMem {
            addr: AddrExpr::Const(0x0808),
            region: MemRegion::RamMirror,
        }]);
        // $0808 & $07FF = $0008 → $C008
        assert!(build.bytes.windows(3).any(|w| w == [0x3A, 0x08, 0xC0]));
    }

    // -------------------------------------------------------------------
    // PpuWrite
    // -------------------------------------------------------------------
    #[test]
    fn ppu_write_reg6_a() {
        let build = lower_and_finish(vec![Op::PpuWrite {
            reg: 6,
            value: ValueSrc::A,
        }]);
        // ld b,$06 = 06 06
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x06]));
        assert!(build.asm.contains("call rt_ppu_write"));
    }

    // -------------------------------------------------------------------
    // PpuRead
    // -------------------------------------------------------------------
    #[test]
    fn ppu_read_reg2() {
        let build = lower_and_finish(vec![Op::PpuRead { reg: 2 }]);
        // ld b,$02 = 06 02
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x02]));
        assert!(build.asm.contains("call rt_ppu_read"));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // OamDmaWrite
    // -------------------------------------------------------------------
    #[test]
    fn oam_dma_write_a() {
        let build = lower_and_finish(vec![Op::OamDmaWrite { value: ValueSrc::A }]);
        assert!(build.asm.contains("call rt_oam_dma"));
    }

    // -------------------------------------------------------------------
    // ControllerRead
    // -------------------------------------------------------------------
    #[test]
    fn controller_read_4016() {
        let build = lower_and_finish(vec![Op::ControllerRead { port: 0x4016 }]);
        // ld a,$16 = 3E 16
        assert!(build.bytes.windows(2).any(|w| w == [0x3E, 0x16]));
        assert!(build.asm.contains("call rt_controller_read"));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // ApuWrite
    // -------------------------------------------------------------------
    #[test]
    fn apu_write_4000_a() {
        let build = lower_and_finish(vec![Op::ApuWrite {
            reg: 0x4000,
            value: ValueSrc::A,
        }]);
        // ld hl,$4000 = 21 00 40
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x00, 0x40]));
        assert!(build.asm.contains("call rt_apu_write"));
    }

    #[test]
    fn apu_write_4016_uses_controller_strobe() {
        let build = lower_and_finish(vec![Op::ApuWrite {
            reg: 0x4016,
            value: ValueSrc::A,
        }]);

        assert!(build.asm.contains("call rt_controller_strobe"));
        assert!(!build.asm.contains("call rt_apu_write"));
    }

    // -------------------------------------------------------------------
    // AdcImm
    // -------------------------------------------------------------------
    #[test]
    fn adc_imm_2() {
        let build = lower_and_finish(vec![Op::AdcImm(2)]);
        // ld b,$02 = 06 02
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x02]));
        assert!(build.asm.contains("call rt_adc_a"));
    }

    // -------------------------------------------------------------------
    // CmpImm
    // -------------------------------------------------------------------
    #[test]
    fn cmp_imm_3() {
        let build = lower_and_finish(vec![Op::CmpImm(3)]);
        // ld b,$03 = 06 03
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x03]));
        assert!(build.asm.contains("call rt_cmp_a"));
    }

    #[test]
    fn bit_abs_stack_page_loads_memory_operand() {
        let build = lower_and_finish(vec![Op::BitMem {
            addr: AddrExpr::Const(0x01A9),
            region: MemRegion::Stack,
        }]);

        // $01A9 is CPU RAM, mirrored to SMS RAM at $C1A9. BIT must load
        // the memory operand into B while preserving A; loading zero here
        // breaks SMB's metatile collision lookup.
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0xA9, 0xC1]));
        assert!(build.asm.contains("ld b,(hl)"));
        assert!(!build.asm.contains("WARN: unresolved mem-to-B mode"));
        assert!(build.asm.contains("call rt_bit_mem"));
    }

    // -------------------------------------------------------------------
    // BranchIf Carry
    // -------------------------------------------------------------------
    #[test]
    fn branch_if_carry() {
        let routine = make_routine(
            "test_routine",
            vec![
                Op::BranchIf {
                    cond: Cond::Carry,
                    target: "L_8010".to_string(),
                },
                Op::Label("L_8010".to_string()),
                Op::Rts,
            ],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();
        // Branch is lowered via `bit n,(hl)` so A is preserved.
        // ld hl,$CB03 = 21 03 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x03, 0xCB]));
        // bit 0,(hl) = CB 46
        assert!(build.bytes.windows(2).any(|w| w == [0xCB, 0x46]));
        // jp nz opcode = C2 (Carry: branch when shadow C set → bit was set → Z80 Z=0)
        assert!(build.bytes.contains(&0xC2));
        assert!(build.asm.contains("jp nz,L_8010"));
    }

    #[test]
    fn branch_if_not_zero() {
        let routine = make_routine(
            "test_routine",
            vec![
                Op::BranchIf {
                    cond: Cond::NotZero,
                    target: "L_8010".to_string(),
                },
                Op::Label("L_8010".to_string()),
                Op::Nop,
            ],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();
        // bit 1,(hl) = CB 4E
        assert!(build.bytes.windows(2).any(|w| w == [0xCB, 0x4E]));
        // jp z opcode = CA (NotZero: branch when shadow Z clear → bit was clear → Z80 Z=1)
        assert!(build.bytes.contains(&0xCA));
        assert!(build.asm.contains("jp z,L_8010"));
    }

    #[test]
    fn ldx_imm_restores_a_without_clobbering_new_flags() {
        let build = lower_and_finish(vec![
            Op::LdxImm(0x00),
            Op::BranchIf {
                cond: Cond::Zero,
                target: "L_done".to_string(),
            },
            Op::Label("L_done".to_string()),
            Op::Rts,
        ]);

        assert!(build.asm.contains("call rt_set_nz_a"));
        assert!(build.asm.contains("pop bc"));
        assert!(build.asm.contains("ld a,b"));
    }

    #[test]
    fn ldy_mem_restores_a_without_clobbering_new_flags() {
        let build = lower_and_finish(vec![
            Op::LdyMem {
                addr: AddrExpr::Const(0x07A2),
                region: MemRegion::Ram,
            },
            Op::BranchIf {
                cond: Cond::Zero,
                target: "L_done".to_string(),
            },
            Op::Label("L_done".to_string()),
            Op::Rts,
        ]);

        assert!(build.asm.contains("ld a,($C7A2)"));
        assert!(build.asm.contains("call rt_set_nz_a"));
        assert!(build.asm.contains("pop bc"));
        assert!(build.asm.contains("ld a,b"));
    }

    #[test]
    fn ldy_stack_page_abs_reads_mapped_ram() {
        let build = lower_and_finish(vec![Op::LdyMem {
            addr: AddrExpr::Const(0x010F),
            region: MemRegion::Stack,
        }]);

        assert!(build.asm.contains("ld a,($C10F)"));
        assert!(
            !build
                .asm
                .contains("WARN: unresolved LDX/LDY addressing mode")
        );
    }

    #[test]
    fn jump_engine_call_uses_bank_restoring_call_then_ret() {
        let build = lower_and_finish(vec![Op::JumpEngineCall {
            targets: vec!["L_a".to_string(), "L_b".to_string()],
        }]);

        assert!(build.asm.contains("call rt_far_call"));
        assert!(build.asm.contains("; → L_a"));
        assert!(build.asm.contains("; → L_b"));
        assert!(build.asm.contains("ret"));
        assert!(!build.asm.contains("call rt_far_jmp"));
    }

    // -------------------------------------------------------------------
    // Tax
    // -------------------------------------------------------------------
    #[test]
    fn tax_stores_to_shadow_x() {
        let build = lower_and_finish(vec![Op::Tax]);
        // ld ($CB00),a = 32 00 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x32, 0x00, 0xCB]));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // Inx
    // -------------------------------------------------------------------
    #[test]
    fn inx_sequence() {
        let build = lower_and_finish(vec![Op::Inx]);
        // ld a,($CB00) = 3A 00 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x3A, 0x00, 0xCB]));
        // inc a = 3C
        assert!(build.bytes.contains(&0x3C));
        // ld ($CB00),a = 32 00 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x32, 0x00, 0xCB]));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // Jsr with label (no profile)
    // -------------------------------------------------------------------
    #[test]
    fn jsr_emits_call_label() {
        let routine = make_routine(
            "test_routine",
            vec![
                Op::Jsr {
                    target: "L_8200".to_string(),
                },
                Op::Label("L_8200".to_string()),
                Op::Rts,
            ],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();
        // Translated-label JSRs go through the bank-aware trampoline so
        // they work regardless of which bank holds the target. The asm
        // listing should mention both the trampoline call and the target
        // (in a `; → L_8200` comment).
        assert!(build.asm.contains("call rt_far_call"));
        assert!(build.asm.contains("L_8200"));
    }

    // -------------------------------------------------------------------
    // Jsr with profile replacement
    // -------------------------------------------------------------------
    #[test]
    fn jsr_with_profile_replacement() {
        let profile_toml = r#"
[rom]
name = "test"
mapper = 0
prg_kib = 32
chr_kib = 8

[[replacement]]
addr = 0x8200
runtime_label = "rt_replacement"
"#;
        let prof = profile::load_from_str(profile_toml).unwrap();
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
        };
        let routine = make_routine(
            "test_routine",
            vec![
                Op::Jsr {
                    target: "L_8200".to_string(),
                },
                Op::Rts,
            ],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        // rt_replacement is a custom label not in the standard stub set.
        prog.section("rt_stubs");
        prog.label("rt_replacement");
        prog.ret();
        prog.section("test");
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &opts).unwrap();
        let build = prog.finish().unwrap();
        assert!(build.asm.contains("call rt_replacement"));
        assert!(!build.asm.contains("call L_8200"));
    }

    // -------------------------------------------------------------------
    // JmpIndirect
    // -------------------------------------------------------------------
    #[test]
    fn jmp_indirect_3000() {
        let build = lower_and_finish(vec![Op::JmpIndirect { addr: 0x3000 }]);
        // ld hl,$3000 = 21 00 30
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x00, 0x30]));
        assert!(build.asm.contains("call rt_indirect_jmp"));
    }

    // -------------------------------------------------------------------
    // Jam returns LowerError
    // -------------------------------------------------------------------
    #[test]
    fn jam_returns_error() {
        let routine = make_routine(
            "test_routine",
            vec![Op::Jam {
                pc: 0x8000,
                opcode: 0x02,
            }],
        );
        let mut prog = z80_emit::Program::new();
        lower_routine(&mut prog, &routine, &LowerOptions::default())
            .expect_err("should be an error");
    }

    // -------------------------------------------------------------------
    // Unsupported returns LowerError
    // -------------------------------------------------------------------
    #[test]
    fn unsupported_returns_error() {
        let routine = make_routine(
            "test_routine",
            vec![Op::Unsupported {
                pc: 0x8001,
                opcode: 0x8B,
                mnemonic: "XAA".to_string(),
                reason: "unstable opcode".to_string(),
            }],
        );
        let mut prog = z80_emit::Program::new();
        let result = lower_routine(&mut prog, &routine, &LowerOptions::default());
        assert!(result.is_err());
        if let Err(LowerError::UnsupportedOp { pc, reason }) = result {
            assert_eq!(pc, Some(0x8001));
            assert!(reason.contains("XAA"));
        }
    }

    // -------------------------------------------------------------------
    // LdxImm
    // -------------------------------------------------------------------
    #[test]
    fn ldx_imm_stores_to_shadow_x() {
        let build = lower_and_finish(vec![Op::LdxImm(0x0A)]);
        // ld a,$0A = 3E 0A
        assert!(build.bytes.windows(2).any(|w| w == [0x3E, 0x0A]));
        // ld ($CB00),a = 32 00 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x32, 0x00, 0xCB]));
        assert!(build.asm.contains("call rt_set_nz_a"));
    }

    // -------------------------------------------------------------------
    // Sec / Clc flag ops
    // -------------------------------------------------------------------
    #[test]
    fn sec_sets_carry_bit() {
        let build = lower_and_finish(vec![Op::Sec]);
        // ld a,(SHADOW_P): 3A 03 CB, or $01: F6 01, ld (SHADOW_P),a: 32 03 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x3A, 0x03, 0xCB]));
        assert!(build.bytes.windows(2).any(|w| w == [0xF6, 0x01]));
        assert!(build.bytes.windows(3).any(|w| w == [0x32, 0x03, 0xCB]));
    }

    #[test]
    fn clc_clears_carry_bit() {
        let build = lower_and_finish(vec![Op::Clc]);
        assert!(build.bytes.windows(2).any(|w| w == [0xE6, 0xFE]));
    }

    // -------------------------------------------------------------------
    // lower_routines batch convenience
    // -------------------------------------------------------------------
    #[test]
    fn lower_routines_batch() {
        let r1 = make_routine("routine_a", vec![Op::LdaImm(0x01), Op::Rts]);
        let r2 = make_routine("routine_b", vec![Op::LdaImm(0x02), Op::Rts]);
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.section("code");
        prog.org(0x0000);
        lower_routines(&mut prog, &[r1, r2], &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();
        // Both ld a,$01 and ld a,$02 should be present.
        assert!(build.bytes.windows(2).any(|w| w == [0x3E, 0x01]));
        assert!(build.bytes.windows(2).any(|w| w == [0x3E, 0x02]));
    }
}
