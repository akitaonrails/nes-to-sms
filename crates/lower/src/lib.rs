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
    /// Interprocedural flag liveness: routine entry-label → mask of flags
    /// (F_N/F_Z/F_C/F_V) it may read before writing. Lets `flags_live_after`
    /// see past a JSR/JMP to a callee that doesn't read the flags in
    /// question, instead of conservatively assuming every call reads them.
    /// `None` (e.g. unit tests) keeps the conservative behavior.
    pub routine_flag_reads: Option<&'p std::collections::HashMap<String, u8>>,
}

impl<'p> Default for LowerOptions<'p> {
    fn default() -> Self {
        Self {
            profile: None,
            emit_source_comments: true,
            routine_flag_reads: None,
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

// H.2 (optimizer plan): indexed-access specialization. When `base+idx`
// provably stays inside one flat window for every idx 0-255, the
// dispatcher's runtime range classification is dead weight — emit the
// direct add + access inline.

/// SMS base for a direct indexed access into plain NES RAM: the folded
/// base must leave `base+$FF` inside the $C000-$C7FF shadow (no NES
/// hardware window, no mirror-fold crossing).
fn indexed_plain_ram_base(base: u16, region: ir::MemRegion) -> Option<u16> {
    use ir::MemRegion as R;
    if !matches!(region, R::ZeroPage | R::Ram | R::RamMirror | R::Stack) {
        return None;
    }
    let sms = indexed_base_to_sms(base, region);
    ((0xC000..=0xC700).contains(&sms)).then_some(sms)
}

/// PRG bases whose whole `base+$FF` span stays inside the always-mapped
/// low window ($8000-$BFFF in slot 2): direct read, no banking.
fn indexed_plain_prg_low(base: u16, region: ir::MemRegion) -> Option<u16> {
    (region == ir::MemRegion::PrgRom && (0x8000..=0xBF00).contains(&base)).then_some(base)
}

/// Either of the two direct-read windows.
fn indexed_direct_base(base: u16, region: ir::MemRegion) -> Option<u16> {
    indexed_plain_ram_base(base, region).or_else(|| indexed_plain_prg_low(base, region))
}

/// A := (sms_base + idx). Clobbers HL/B/C and native flags.
fn emit_indexed_read_direct(p: &mut z80_emit::Program, sms_base: u16, shadow_idx: u16) {
    p.ld_hl_imm(sms_base);
    p.ld_a_abs(shadow_idx);
    p.ld_c_a();
    p.ld_b_imm(0);
    p.add_hl_bc();
    p.ld_a_hl_ptr();
}

/// (sms_base + idx) := A; A preserved (6502 store contract). Clobbers
/// HL/C/DE and native flags.
fn emit_indexed_write_direct(p: &mut z80_emit::Program, sms_base: u16, shadow_idx: u16) {
    p.ld_c_a();
    p.ld_hl_imm(sms_base);
    p.ld_a_abs(shadow_idx);
    p.ld_e_a();
    p.ld_d_imm(0);
    p.add_hl_de();
    p.ld_hl_ptr_c();
    p.ld_a_c();
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
            if let Some(sms) = indexed_direct_base(*base, region) {
                emit_indexed_read_direct(program, sms, sms_layout::SHADOW_X);
            } else {
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_abs(sms_layout::SHADOW_X);
                program.ld_b_a();
                program.call(indexed_read_runtime(*base, region));
            }
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                emit_indexed_read_direct(program, sms, sms_layout::SHADOW_Y);
            } else {
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_abs(sms_layout::SHADOW_Y);
                program.ld_b_a();
                program.call(indexed_read_runtime(*base, region));
            }
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
            if let Some(sms) = indexed_direct_base(*base, region) {
                program.ld_a_abs(shadow_addr);
                emit_indexed_write_direct(program, sms, sms_layout::SHADOW_X);
            } else {
                program.ld_a_abs(shadow_addr);
                program.ld_c_a(); // C = value (X or Y)
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_abs(sms_layout::SHADOW_X);
                program.ld_b_a();
                program.ld_a_c();
                program.call(runtime_symbols::WRITE_INDEXED);
            }
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                program.ld_a_abs(shadow_addr);
                emit_indexed_write_direct(program, sms, sms_layout::SHADOW_Y);
            } else {
                program.ld_a_abs(shadow_addr);
                program.ld_c_a();
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_abs(sms_layout::SHADOW_Y);
                program.ld_b_a();
                program.ld_a_c();
                program.call(runtime_symbols::WRITE_INDEXED);
            }
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
        (AddrExpr::Const(a), MemRegion::ApuIo) => {
            // STX/STY to an APU register routes through the APU shim like
            // STA does (SMB's Dump_Squ1_Regs is `STY $4001 / STX $4000`;
            // these were silently dropped before, muting whole channels).
            program.ld_a_abs(shadow_addr);
            if *a == 0x4016 {
                program.call(runtime_symbols::CONTROLLER_STROBE);
            } else {
                program.ld_hl_imm(*a);
                program.call(runtime_symbols::APU_WRITE);
            }
        }
        (AddrExpr::Const(a), MemRegion::PpuReg | MemRegion::PpuMirror) => {
            program.ld_a_abs(shadow_addr);
            program.ld_b_imm((*a & 7) as u8);
            program.call(runtime_symbols::PPU_WRITE);
        }
        _ => {
            program.comment("WARN: unresolved STX/STY addressing mode");
        }
    }
    program.pop_af();
}

/// Set or clear a single shadow-P bit. `mask` is the OR mask (set) or the
/// AND mask (clear, i.e. the complement). Emits `ld hl,SHADOW_P; set/res
/// n,(hl)` — 2 ops, preserves A, no push/pop af dance.
fn emit_flag_update(program: &mut z80_emit::Program, mask: u8, set: bool) {
    let bit = if set {
        mask.trailing_zeros()
    } else {
        (!mask).trailing_zeros()
    } as u8;
    program.ld_hl_imm(sms_layout::SHADOW_P);
    if set {
        program.set_n_hl_ptr(bit);
    } else {
        program.res_n_hl_ptr(bit);
    }
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
            // For indexed reads we must preserve A across the read (the
            // operand lands in B). Save A in C, read, restore.
            if let Some(sms) = indexed_direct_base(*base, region) {
                p.ld_c_a();
                p.ld_hl_imm(sms);
                p.ld_a_abs(SHADOW_X);
                p.ld_e_a();
                p.ld_d_imm(0);
                p.add_hl_de();
                p.ld_b_hl_ptr();
                p.ld_a_c();
            } else {
                p.ld_c_a();
                p.ld_hl_imm(indexed_base_to_sms(*base, region));
                p.ld_a_abs(SHADOW_X);
                p.ld_b_a();
                p.call(indexed_read_runtime(*base, region));
                p.ld_b_a();
                p.ld_a_c();
            }
        }
        AddrExpr::AbsIndexedY(base) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                p.ld_c_a();
                p.ld_hl_imm(sms);
                p.ld_a_abs(SHADOW_Y);
                p.ld_e_a();
                p.ld_d_imm(0);
                p.add_hl_de();
                p.ld_b_hl_ptr();
                p.ld_a_c();
            } else {
                p.ld_c_a();
                p.ld_hl_imm(indexed_base_to_sms(*base, region));
                p.ld_a_abs(SHADOW_Y);
                p.ld_b_a();
                p.call(indexed_read_runtime(*base, region));
                p.ld_b_a();
                p.ld_a_c();
            }
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
        if is_flag_boundary(op) {
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
            | Op::PpuRead { .. }
            | Op::ApuRead { .. }
            | Op::ControllerRead { .. }
    )
}

// ---------------------------------------------------------------------------
// Per-flag liveness (N/Z/C/V) — foundation for native-flag fusion.
// ---------------------------------------------------------------------------
// Internal flag bitmask (NOT the 6502 P layout — just a set).
const F_N: u8 = 1;
const F_Z: u8 = 2;
const F_C: u8 = 4;
const F_V: u8 = 8;

/// Which of N/Z/C/V the op *reads*. Branches read their condition flag.
/// ADC/SBC read carry-in; ROL/ROR rotate through carry; PHP reads all.
fn flags_read(op: &ir::Op) -> u8 {
    use ir::{Cond, Op};
    match op {
        Op::BranchIf { cond, .. } => match cond {
            Cond::Carry | Cond::NoCarry => F_C,
            Cond::Zero | Cond::NotZero => F_Z,
            Cond::Negative | Cond::Positive => F_N,
            Cond::Overflow | Cond::NoOverflow => F_V,
        },
        Op::AdcImm(_)
        | Op::AdcMem { .. }
        | Op::SbcImm(_)
        | Op::SbcMem { .. }
        | Op::RolA
        | Op::RolMem { .. }
        | Op::RorA
        | Op::RorMem { .. } => F_C,
        Op::Php => F_N | F_Z | F_C | F_V,
        _ => 0,
    }
}

/// Which of N/Z/C/V the op *overwrites*.
fn flags_written(op: &ir::Op) -> u8 {
    use ir::Op;
    match op {
        Op::LdaImm(_)
        | Op::LdaMem { .. }
        | Op::LdxImm(_)
        | Op::LdxMem { .. }
        | Op::LdyImm(_)
        | Op::LdyMem { .. }
        | Op::AndImm(_)
        | Op::AndMem { .. }
        | Op::OraImm(_)
        | Op::OraMem { .. }
        | Op::EorImm(_)
        | Op::EorMem { .. }
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
        | Op::PpuRead { .. }
        | Op::ApuRead { .. }
        | Op::ControllerRead { .. } => F_N | F_Z,
        Op::AdcImm(_) | Op::AdcMem { .. } | Op::SbcImm(_) | Op::SbcMem { .. } => {
            F_N | F_Z | F_C | F_V
        }
        Op::CmpImm(_)
        | Op::CmpMem { .. }
        | Op::CpxImm(_)
        | Op::CpxMem { .. }
        | Op::CpyImm(_)
        | Op::CpyMem { .. } => F_N | F_Z | F_C,
        Op::AslA
        | Op::AslMem { .. }
        | Op::LsrA
        | Op::LsrMem { .. }
        | Op::RolA
        | Op::RolMem { .. }
        | Op::RorA
        | Op::RorMem { .. } => F_N | F_Z | F_C,
        Op::BitMem { .. } => F_N | F_Z | F_V,
        Op::Plp => F_N | F_Z | F_C | F_V,
        Op::Sec | Op::Clc => F_C,
        Op::Clv => F_V,
        _ => 0,
    }
}

/// Ops that end a basic block / cross a routine boundary where we must be
/// conservative (downstream code we can't see here may read the flags via
/// PHP/PLP or fall-through).
fn is_flag_boundary(op: &ir::Op) -> bool {
    use ir::Op;
    // H.1a (optimizer plan): hardware WRITES (PpuWrite/ApuWrite/
    // OamDmaWrite/MapperWrite) and PHA neither read nor write the 6502
    // P register — a STA $2007 between an ALU op and its branch must
    // not force the shadow update. Hardware READS (PpuRead/ApuRead/
    // ControllerRead) load A and therefore OVERWRITE N/Z — expressed in
    // flags_written/overwrites_nz, which is strictly better than a
    // boundary (it kills pending N/Z liveness instead of preserving it).
    matches!(
        op,
        Op::Rts
            | Op::Rti
            | Op::Jsr { .. }
            | Op::JsrUnknown { .. }
            | Op::JumpEngineCall { .. }
            | Op::Jmp { .. }
            | Op::JmpIndirect { .. }
            | Op::Brk
            | Op::Php
            | Op::Unsupported { .. }
            | Op::Jam { .. }
    )
}

/// Flags routine R may read before writing, scanning from its entry. A
/// safe over-approximation: writes are only credited on the guaranteed
/// straight-line entry path (crediting stops at the first control-flow
/// divergence/join); calls and computed jumps are treated as reading any
/// not-yet-written flag. So a flag is excluded only when R provably
/// writes it before any read on every path — sound for the use below.
pub fn routine_incoming_flag_reads(ops: &[ir::Op]) -> u8 {
    use ir::Op;
    let mut incoming = 0u8;
    let mut written = 0u8;
    let mut frozen = false;
    for op in ops {
        incoming |= flags_read(op) & !written;
        // A call/computed-jump may read any flag the callee reads; without
        // its mask here, assume it reads all not-yet-written flags.
        if matches!(
            op,
            Op::Jsr { .. }
                | Op::JsrUnknown { .. }
                | Op::JumpEngineCall { .. }
                | Op::JmpIndirect { .. }
                | Op::Php
        ) {
            incoming |= (F_N | F_Z | F_C | F_V) & !written;
        }
        if !frozen {
            written |= flags_written(op);
        }
        if matches!(
            op,
            Op::Label(_)
                | Op::BranchIf { .. }
                | Op::Jmp { .. }
                | Op::JmpIndirect { .. }
                | Op::Jsr { .. }
                | Op::JsrUnknown { .. }
                | Op::JumpEngineCall { .. }
                | Op::Rts
                | Op::Rti
        ) {
            frozen = true;
        }
    }
    incoming
}

/// Shadow-P N/Z liveness for a producer whose result is in **A** (LDA,
/// AND/ORA/EOR, ADC/SBC, TXA/TYA, PLA, shifts-on-A). In the native-flag
/// model an N/Z branch is lowered as `or a; jp cc` (reading A) **iff A
/// still holds this value** — exactly the `a_holds_nz` tracker's state.
/// So such branches are NOT shadow-P readers and don't keep the producer's
/// shadow write alive. Returns true only if a genuine shadow reader (a
/// non-native N/Z branch, PHP, or an opaque boundary) is reachable before
/// the N/Z are overwritten. Sound: any uncertainty returns true.
///
/// `a_clean` here tracks the same transitions as the lowering tracker
/// (`op_nz_effect`): it starts true at the producer, a flag-writer ends
/// the scan (overwrite → dead), and an A-clobbering op clears it — so the
/// analysis and the branch lowering always agree on native-vs-shadow.
fn nz_shadow_live_after(
    ops: &[ir::Op],
    i: usize,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> bool {
    use ir::{Cond, Op};
    let mut a_clean = true;
    for op in ops.iter().skip(i + 1) {
        // Shadow-P readers of N/Z:
        match op {
            Op::BranchIf { cond, .. }
                if matches!(
                    cond,
                    Cond::Zero | Cond::NotZero | Cond::Negative | Cond::Positive
                ) =>
            {
                if !a_clean {
                    return true; // lowered as `ld hl,SHADOW_P; bit n,(hl)`
                }
                // else native `or a; jp` — not a shadow read; keep scanning.
            }
            Op::Php => return true,
            _ => {}
        }
        // N/Z overwritten before any shadow read → producer's write is dead.
        if flags_written(op) & (F_N | F_Z) != 0 {
            return false;
        }
        // Track A cleanliness (mirrors the a_holds_nz tracker).
        if matches!(op_nz_effect(op), Some(false)) {
            a_clean = false;
        }
        // Opaque boundaries: a JSR to a callee that doesn't read N/Z is
        // transparent (we already cleared a_clean); anything else may hide
        // a downstream shadow reader / flag-return convention → live.
        if is_flag_boundary(op) {
            match op {
                Op::Jsr { target } if callee_flag_reads(target, reads) & (F_N | F_Z) == 0 => {}
                _ => return true,
            }
        }
    }
    true
}

/// Mask of flags a JSR/JMP target may read. Known routine → its computed
/// mask; unknown/computed target → all flags (conservative).
fn callee_flag_reads(target: &str, map: Option<&std::collections::HashMap<String, u8>>) -> u8 {
    match map.and_then(|m| m.get(target)) {
        Some(&mask) => mask,
        None => F_N | F_Z | F_C | F_V,
    }
}

/// Are any of the `which` flags live after op index `i` — i.e. read by a
/// later op before being overwritten? Conservative (live) at boundaries
/// and at end-of-routine. With `reads` (interprocedural map), a JSR/JMP to
/// a callee that doesn't read a pending flag is transparent rather than a
/// hard boundary, letting liveness see the real downstream overwrite.
fn flags_live_after(
    ops: &[ir::Op],
    i: usize,
    which: u8,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> bool {
    use ir::Op;
    let mut pending = which;
    for op in ops.iter().skip(i + 1) {
        if flags_read(op) & pending != 0 {
            return true;
        }
        match op {
            // A direct call is transparent if the callee reads none of the
            // pending flags: execution returns and continues past it.
            Op::Jsr { target } => {
                if callee_flag_reads(target, reads) & pending != 0 {
                    return true;
                }
            }
            // A conditional branch has a taken path this linear scan does
            // not follow. If any pending flag could be read there — the
            // classic case is a routine returning its answer in carry via
            // `CMP ...; BEQ done; ...; CLC; done: RTS`, where the taken
            // path reaches RTS with the CMP's carry as the return value —
            // eliding the shadow write would be unsound. Found the hard way
            // in SMB's BlockBumpedChk (coin blocks silently not paying
            // out). Conservative: pending flags stay live across any
            // conditional branch.
            Op::BranchIf { .. } => return true,
            // A tail jump transfers control with the pending flags intact;
            // the target routine's RTS returns them to OUR caller as a
            // potential flag return value. Same soundness rule as RTS:
            // treat pending flags as live.
            Op::Jmp { .. } => return true,
            _ if is_flag_boundary(op) => return true,
            _ => {}
        }
        pending &= !flags_written(op);
        if pending == 0 {
            return false;
        }
    }
    true
}

/// Tracks, while lowering a routine, whether Z80 A currently holds the
/// value whose N/Z are the live 6502 N/Z. Returns:
///   Some(true)  — after this op, A holds the N/Z-determining value
///   Some(false) — after this op, it does not (N/Z come from elsewhere,
///                 A was reloaded by something opaque, or a join point)
///   None        — op preserves A and the 6502 N/Z (e.g. STA, CLC)
fn op_nz_effect(op: &ir::Op) -> Option<bool> {
    use ir::Op;
    match op {
        // A := result; 6502 N/Z computed from A.
        Op::LdaImm(_)
        | Op::LdaMem { .. }
        | Op::AndImm(_)
        | Op::AndMem { .. }
        | Op::OraImm(_)
        | Op::OraMem { .. }
        | Op::EorImm(_)
        | Op::EorMem { .. }
        | Op::AdcImm(_)
        | Op::AdcMem { .. }
        | Op::SbcImm(_)
        | Op::SbcMem { .. }
        | Op::Txa
        | Op::Tya
        | Op::Pla
        | Op::AslA
        | Op::LsrA
        | Op::RolA
        | Op::RorA => Some(true),
        // Preserve A and the 6502 N/Z.
        Op::StaMem { .. }
        | Op::StxMem { .. }
        | Op::StyMem { .. }
        | Op::SaxMem { .. }
        | Op::Clc
        | Op::Sec
        | Op::Cli
        | Op::Sei
        | Op::Clv
        | Op::Cld
        | Op::Sed
        | Op::Txs
        | Op::Pha
        | Op::Php
        | Op::Nop
        | Op::Source { .. }
        | Op::BranchIf { .. } => None,
        // Everything else (LDX/LDY/INC/DEC/transfers-to-XY/compares/BIT,
        // labels = join points, calls, jumps, IO reads, unknown) sets N/Z
        // from non-A or makes A's relationship unknown → conservative.
        _ => Some(false),
    }
}

/// A Z80 native branch condition.
#[derive(Clone, Copy)]
enum Z80Cond {
    Z,
    Nz,
    C,
    Nc,
    M,
    P,
}

impl Z80Cond {
    fn invert(self) -> Z80Cond {
        match self {
            Z80Cond::Z => Z80Cond::Nz,
            Z80Cond::Nz => Z80Cond::Z,
            Z80Cond::C => Z80Cond::Nc,
            Z80Cond::Nc => Z80Cond::C,
            Z80Cond::M => Z80Cond::P,
            Z80Cond::P => Z80Cond::M,
        }
    }
    fn jp(self, p: &mut z80_emit::Program, target: &str) {
        match self {
            Z80Cond::Z => p.jp_z(target),
            Z80Cond::Nz => p.jp_nz(target),
            Z80Cond::C => p.jp_c(target),
            Z80Cond::Nc => p.jp_nc(target),
            Z80Cond::M => p.jp_m(target),
            Z80Cond::P => p.jp_p(target),
        }
    }
}

/// Map a 6502 branch condition to the Z80 native condition that holds
/// after a `cp` (A - operand). Carry is inverted: 6502 C=1 (A>=operand,
/// no borrow) is Z80 NC. Overflow conditions can't come from `cp` (CMP
/// doesn't set V), so they aren't fusable.
fn cmp_cond_to_z80(cond: &ir::Cond) -> Option<Z80Cond> {
    use ir::Cond;
    Some(match cond {
        Cond::Zero => Z80Cond::Z,
        Cond::NotZero => Z80Cond::Nz,
        Cond::Carry => Z80Cond::Nc,
        Cond::NoCarry => Z80Cond::C,
        Cond::Negative => Z80Cond::M,
        Cond::Positive => Z80Cond::P,
        Cond::Overflow | Cond::NoOverflow => return None,
    })
}

/// Emit a branch on a Z80 native flag, handling cross-section (far)
/// targets the same way `Op::BranchIf` does: for a far target, invert
/// the condition to skip past a `far_jmp`. Conditional jumps don't
/// clobber flags, so chained native branches off one `cp` stay valid.
fn emit_native_branch(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    cond: Z80Cond,
    target: &str,
) {
    let local = routine.branch_labels.iter().any(|l| l.as_str() == target)
        || routine.name.as_str() == target
        || program.label_section_idx(target) == Some(program.current_section_idx());
    if local {
        cond.jp(program, target);
    } else {
        let skip = program.fresh_label("br_skip");
        cond.invert().jp(program, &skip);
        program.far_jmp(target);
        program.label(&skip);
    }
}

/// Map a 6502 branch condition to the Z80 native condition that holds
/// after an op that sets S/Z from its result (e.g. `or a` after a load,
/// or `inc`/`dec`). Only N/Z conditions are derivable this way; C/V
/// branches aren't.
fn nz_cond_to_z80(cond: &ir::Cond) -> Option<Z80Cond> {
    use ir::Cond;
    Some(match cond {
        Cond::Zero => Z80Cond::Z,
        Cond::NotZero => Z80Cond::Nz,
        Cond::Negative => Z80Cond::M,
        Cond::Positive => Z80Cond::P,
        _ => return None,
    })
}

/// Lower INX/INY/DEX/DEY. Instead of the old `push af; ld a,(shadow);
/// inc a; ld (shadow),a; pop af` dance, modify the shadow byte in place
/// with `inc/dec (hl)` — 2 ops, preserves A, and sets Z80 native S/Z so
/// the common `DEX;BNE` loop idiom fuses to a native jump.
#[allow(clippy::too_many_arguments)]
fn emit_inc_dec_xy(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    ops: &[ir::Op],
    op_idx: usize,
    shadow_addr: u16,
    is_inc: bool,
    fuse_end: Option<usize>,
    nz_live: bool,
    emit_comments: bool,
) {
    program.ld_hl_imm(shadow_addr);
    if is_inc {
        program.inc_hl_ptr(); // inc (hl): sets S/Z, preserves A
    } else {
        program.dec_hl_ptr();
    }
    if let Some(end) = fuse_end {
        emit_fused_branches(
            program,
            routine,
            ops,
            op_idx + 1,
            end,
            emit_comments,
            nz_cond_to_z80,
        );
    } else if nz_live {
        // Non-adjacent reader of the flags: persist to shadow P. The new
        // value is at (HL); preserve the caller's A across the helper.
        program.push_af();
        program.ld_a_hl_ptr();
        program.call(runtime_symbols::SET_NZ_A);
        program.pop_af();
    }
}

/// Tail for an N/Z producer whose result is already in A with Z80 S/Z
/// set (AND/ORA/EOR): emit the fused native branch run if fusable, else
/// the rt_set_nz_a call if the flags are live.
#[allow(clippy::too_many_arguments)]
fn emit_nz_producer_tail(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    ops: &[ir::Op],
    op_idx: usize,
    fuse_end: Option<usize>,
    nz_live: bool,
    emit_comments: bool,
) {
    if let Some(end) = fuse_end {
        emit_fused_branches(
            program,
            routine,
            ops,
            op_idx + 1,
            end,
            emit_comments,
            nz_cond_to_z80,
        );
    } else if nz_live {
        program.call(runtime_symbols::SET_NZ_A);
    }
}

/// Emit the branch run that follows a fused flag-producer (ops
/// `[start, end)` are `Source` comments and fusable `BranchIf`s). The
/// producer already set the Z80 native flags; conditional jumps preserve
/// them, so the chain stays valid. `map` translates each 6502 condition
/// to the native condition.
fn emit_fused_branches(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    ops: &[ir::Op],
    start: usize,
    end: usize,
    emit_comments: bool,
    map: fn(&ir::Cond) -> Option<Z80Cond>,
) {
    use ir::Op;
    for op in ops.iter().take(end).skip(start) {
        match op {
            Op::Source { pc, text } => {
                if emit_comments {
                    program.comment(format!("6502 ${pc:04X}: {text}"));
                }
            }
            Op::BranchIf { cond, target } => {
                let z = map(cond).expect("fusion run only holds fusable conds");
                emit_native_branch(program, routine, z, target);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// 16-bit add idiom lifting
// ---------------------------------------------------------------------------
// The 6502 has no 16-bit add, so it does:
//     LDA lo ; CLC ; ADC loOp ; STA lo ; LDA hi ; ADC hiOp ; STA hi
// threading the carry between the two ADCs through the (emulated) status
// byte — which costs two rt_adc_a calls + the CLC dance. The Z80 has the
// same carry flag, so we thread it *natively* between a real `add` and
// `adc`, dropping all the shadow-flag machinery. SMB does this constantly
// (player/object 16-bit positions, camera scroll), so it's a hot path.
//
// We lift only when the resulting flags are dead afterward (the native
// carry/sign/zero after `adc` would otherwise need to be reflected into
// the shadow byte) and all addresses are constant (non-indexed), so the
// observable result — the two stored bytes — is provably identical.

#[derive(Clone, Copy)]
enum Val16 {
    Imm(u8),
    Mem(u16), // SMS address
}

#[derive(Clone, Copy)]
struct Add16Plan {
    lo_src: u16,
    lo_op: Val16,
    lo_dst: u16,
    hi_src: Val16,
    hi_op: Val16,
    hi_dst: u16,
    end: usize, // one past the last consumed op
}

/// Next op index at/after `i` skipping `Source` comments.
fn skip_source(ops: &[ir::Op], mut i: usize) -> usize {
    while i < ops.len() && matches!(ops[i], ir::Op::Source { .. }) {
        i += 1;
    }
    i
}

/// `LdaMem`/`StaMem` with a constant (non-indexed) address → SMS address.
fn lda_const(op: &ir::Op) -> Option<u16> {
    match op {
        ir::Op::LdaMem { addr, region } => const_addr_to_sms(addr, *region),
        _ => None,
    }
}
fn sta_const(op: &ir::Op) -> Option<u16> {
    match op {
        ir::Op::StaMem { addr, region } => const_addr_to_sms(addr, *region),
        _ => None,
    }
}
/// An LDA source (immediate or constant memory) as a Val16.
fn lda_src_val(op: &ir::Op) -> Option<Val16> {
    match op {
        ir::Op::LdaImm(v) => Some(Val16::Imm(*v)),
        ir::Op::LdaMem { addr, region } => const_addr_to_sms(addr, *region).map(Val16::Mem),
        _ => None,
    }
}
/// An ADC operand (immediate or constant memory) as a Val16.
fn adc_val(op: &ir::Op) -> Option<Val16> {
    match op {
        ir::Op::AdcImm(v) => Some(Val16::Imm(*v)),
        ir::Op::AdcMem { addr, region } => const_addr_to_sms(addr, *region).map(Val16::Mem),
        _ => None,
    }
}

/// Recognize the 16-bit ADD idiom starting at op `i`. Returns a plan if
/// the full shape matches with constant addresses and the result flags
/// are dead afterward.
fn match_add16(
    ops: &[ir::Op],
    i: usize,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> Option<Add16Plan> {
    use ir::Op;
    let lo_src = lda_const(&ops[i])?; // LDA lo
    let i2 = skip_source(ops, i + 1);
    if !matches!(ops.get(i2)?, Op::Clc) {
        return None; // CLC
    }
    let i3 = skip_source(ops, i2 + 1);
    let lo_op = adc_val(ops.get(i3)?)?; // ADC loOp
    let i4 = skip_source(ops, i3 + 1);
    let lo_dst = sta_const(ops.get(i4)?)?; // STA lo
    let i5 = skip_source(ops, i4 + 1);
    let hi_src = lda_src_val(ops.get(i5)?)?; // LDA hi
    let i6 = skip_source(ops, i5 + 1);
    // The high ADC must NOT be preceded by another CLC (that would reset
    // the carry and break the 16-bit chain). adc_val rejects anything but
    // ADC, and i6 is the op right after the high LDA.
    let hi_op = adc_val(ops.get(i6)?)?; // ADC hiOp
    let i7 = skip_source(ops, i6 + 1);
    let hi_dst = sta_const(ops.get(i7)?)?; // STA hi
    // Result flags must be dead — otherwise the shadow byte would be stale.
    if flags_live_after(ops, i7, F_N | F_Z | F_C | F_V, reads) {
        return None;
    }
    Some(Add16Plan {
        lo_src,
        lo_op,
        lo_dst,
        hi_src,
        hi_op,
        hi_dst,
        end: i7 + 1,
    })
}

/// Emit the lifted 16-bit add: native `add`/`adc` with the carry threaded
/// in the Z80 carry flag (no shadow-P traffic). Leaves A = high byte of
/// the result, matching the faithful idiom.
// ---------------------------------------------------------------------------
// LDIR copy-loop lifting — the Z80's block-transfer advantage. SMB copies
// tables→buffers byte-at-a-time; in our translation each byte pays
// rt_read_indexed + rt_write_indexed. `ldir` does the whole block.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct CopyLoopPlan {
    src_sms: u16, // SMS address of the first copied source byte
    dst_sms: u16, // SMS address of the first copied dest byte
    count: u16,
    idx_shadow: u16, // SHADOW_X or SHADOW_Y
    exit_idx: u8,    // value the loop index holds on exit
    end: usize,      // one past the loop's back-branch
}

/// Is A read (used) before being overwritten after op `i`? Conservative
/// (live) at boundaries.
fn a_live_after(ops: &[ir::Op], i: usize) -> bool {
    use ir::Op;
    for op in ops.iter().skip(i + 1) {
        match op {
            Op::StaMem { .. }
            | Op::SaxMem { .. }
            | Op::CmpImm(_)
            | Op::CmpMem { .. }
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
            | Op::BitMem { .. }
            | Op::Tax
            | Op::Tay
            | Op::Pha
            | Op::AslA
            | Op::LsrA
            | Op::RolA
            | Op::RorA => return true,
            Op::LdaImm(_) | Op::LdaMem { .. } | Op::Pla | Op::Txa | Op::Tya => return false,
            _ => {}
        }
        if is_flag_boundary(op) {
            return true;
        }
    }
    true
}

/// SMS address of an indexed base `b` (the un-indexed table address) for a
/// copy source: low PRG ($8000-$BFFF) is mapped directly; RAM mirrors to
/// $C000+. High PRG / other → None (would need a bank swap).
fn src_base_sms(b: u16, region: ir::MemRegion) -> Option<u16> {
    use ir::MemRegion::*;
    match region {
        PrgRom if b < 0xC000 => Some(b),
        Ram | RamMirror | Stack => Some(nes_ram_addr_to_sms(b)),
        ZeroPage => Some(sms_layout::NES_ZP_BASE + (b & 0xFF)),
        _ => None,
    }
}
/// SMS address of an indexed RAM destination base.
fn dst_base_sms(b: u16, region: ir::MemRegion) -> Option<u16> {
    use ir::MemRegion::*;
    match region {
        Ram | RamMirror | Stack => Some(nes_ram_addr_to_sms(b)),
        ZeroPage => Some(sms_layout::NES_ZP_BASE + (b & 0xFF)),
        _ => None,
    }
}
/// Extract (base_addr, region) if `op` is an LDA/STA indexed by the given
/// register (X if `want_x`, else Y).
fn indexed_mem(op: &ir::Op, want_x: bool, is_load: bool) -> Option<(u16, ir::MemRegion)> {
    use ir::{AddrExpr, Op};
    let (addr, region) = match (op, is_load) {
        (Op::LdaMem { addr, region }, true) => (addr, region),
        (Op::StaMem { addr, region }, false) => (addr, region),
        _ => return None,
    };
    match addr {
        AddrExpr::AbsIndexedX(b) if want_x => Some((*b, *region)),
        AddrExpr::AbsIndexedY(b) if !want_x => Some((*b, *region)),
        _ => None,
    }
}

/// Recognize a same-index copy loop beginning with `LDX/LDY #init` at `i`:
///   LDX/LDY #init ; L: LDA src,i ; STA dst,i ; INC i ; CPX/CPY #N ; B?? L
///   (ascending, count = N-init)  — or —
///   LDX/LDY #init ; L: LDA src,i ; STA dst,i ; DEX/DEY ; BPL L
///   (descending, count = init+1)
/// Lifts to `ldir` only when the loop body is exactly the copy, the label
/// has no other referrer, and A / N / Z / C are dead afterward (the index
/// is restored to its exit value explicitly).
fn match_copy_loop(
    ops: &[ir::Op],
    i: usize,
    routine: &ir::Routine,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> Option<CopyLoopPlan> {
    use ir::{Cond, Op};
    let (init, want_x) = match &ops[i] {
        Op::LdxImm(v) => (*v, true),
        Op::LdyImm(v) => (*v, false),
        _ => return None,
    };
    let lbl_idx = skip_source(ops, i + 1);
    let label = match ops.get(lbl_idx)? {
        Op::Label(l) => l.clone(),
        _ => return None,
    };
    let a = skip_source(ops, lbl_idx + 1);
    let b = skip_source(ops, a + 1);
    let c = skip_source(ops, b + 1);
    // LDA src,idx ; STA dst,idx
    let (src_b, src_r) = indexed_mem(ops.get(a)?, want_x, true)?;
    let (dst_b, dst_r) = indexed_mem(ops.get(b)?, want_x, false)?;
    let src_sms_base = src_base_sms(src_b, src_r)?;
    let dst_sms_base = dst_base_sms(dst_b, dst_r)?;
    // Step op (INC/DEC matching idx) then optional CMP then branch.
    let ascending = matches!((ops.get(c)?, want_x), (Op::Inx, true) | (Op::Iny, false));
    let descending = matches!((ops.get(c)?, want_x), (Op::Dex, true) | (Op::Dey, false));
    if !ascending && !descending {
        return None;
    }
    let (count, exit_idx, branch_idx) = if ascending {
        // CPX/CPY #N then loop-while-below branch.
        let d = skip_source(ops, c + 1);
        let n = match (ops.get(d)?, want_x) {
            (Op::CpxImm(n), true) | (Op::CpyImm(n), false) => *n,
            _ => return None,
        };
        let e = skip_source(ops, d + 1);
        match ops.get(e)? {
            Op::BranchIf { cond, target }
                if target == &label
                    && matches!(cond, Cond::NoCarry | Cond::Negative | Cond::NotZero) => {}
            _ => return None,
        }
        if n <= init {
            return None;
        }
        ((n - init) as u16, n, e)
    } else {
        // DEX/DEY ; BPL L  (copies init..0)
        let d = skip_source(ops, c + 1);
        match ops.get(d)? {
            Op::BranchIf {
                cond: Cond::Positive,
                target,
            } if target == &label => {}
            _ => return None,
        }
        ((init as u16) + 1, 0xFFu8, d)
    };
    if count == 0 {
        return None;
    }
    let lo = if ascending { init } else { 0 };
    let src_start = src_sms_base.wrapping_add(lo as u16);
    let dst_start = dst_sms_base.wrapping_add(lo as u16);
    // Non-overlap: ROM source never overlaps RAM dest; for RAM→RAM require
    // separation ≥ count.
    let src_is_rom = matches!(src_r, ir::MemRegion::PrgRom);
    if !src_is_rom {
        let lo16 = src_start.min(dst_start);
        let hi16 = src_start.max(dst_start);
        if hi16 - lo16 < count {
            return None;
        }
    }
    // The label must have no referrer other than this loop's back-branch.
    let refs = routine
        .ops
        .iter()
        .filter(|op| match op {
            Op::BranchIf { target, .. } | Op::Jmp { target } => target == &label,
            _ => false,
        })
        .count();
    if refs != 1 {
        return None;
    }
    // A and N/Z/C must be dead after the loop (index restored explicitly).
    if a_live_after(ops, branch_idx) || flags_live_after(ops, branch_idx, F_N | F_Z | F_C, reads) {
        return None;
    }
    Some(CopyLoopPlan {
        src_sms: src_start,
        dst_sms: dst_start,
        count,
        idx_shadow: if want_x {
            sms_layout::SHADOW_X
        } else {
            sms_layout::SHADOW_Y
        },
        exit_idx,
        end: branch_idx + 1,
    })
}

/// Emit a lifted copy loop: `ld hl,src; ld de,dst; ld bc,count; ldir`,
/// then restore the loop index's exit value. (A is dead; flags dead.)
fn emit_copy_loop(program: &mut z80_emit::Program, plan: &CopyLoopPlan) {
    program.comment(format!("[lifted copy loop: {} bytes via ldir]", plan.count));
    program.ld_hl_imm(plan.src_sms);
    program.ld_de_imm(plan.dst_sms);
    program.ld_bc_imm(plan.count);
    program.ldir();
    // Restore the 6502 index register to its post-loop value (preserves A).
    program.ld_hl_imm(plan.idx_shadow);
    program.data(None, &[0x36, plan.exit_idx]); // ld (hl),exit_idx
}

fn emit_add16(program: &mut z80_emit::Program, plan: &Add16Plan) {
    program.comment("[lifted 16-bit add]");
    // low byte: A = lo_src + lo_op  (CLC absorbed → plain add)
    program.ld_a_abs(plan.lo_src);
    match plan.lo_op {
        Val16::Imm(v) => program.add_a_imm(v),
        Val16::Mem(a) => {
            program.ld_hl_imm(a);
            program.add_a_hl_ptr();
        }
    }
    program.ld_abs_a(plan.lo_dst);
    // high byte: A = hi_src + hi_op + carry
    match plan.hi_src {
        Val16::Imm(v) => program.ld_a_imm(v),
        Val16::Mem(a) => program.ld_a_abs(a),
    }
    match plan.hi_op {
        Val16::Imm(v) => program.adc_a_imm(v),
        Val16::Mem(a) => {
            program.ld_hl_imm(a);
            program.adc_a_hl_ptr();
        }
    }
    program.ld_abs_a(plan.hi_dst);
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
        .map(|(i, op)| {
            // A-result producers (N/Z derivable from A) get the native-flag
            // aware liveness: branches that read A natively don't keep the
            // shadow write alive. Other producers (N/Z from X/Y/mem) keep
            // the conservative shadow liveness.
            if matches!(op_nz_effect(op), Some(true)) {
                nz_shadow_live_after(&routine.ops, i, opts.routine_flag_reads)
            } else {
                nz_flags_live_after(&routine.ops, i)
            }
        })
        .collect();

    // CMP→branch fusion plan. `fuse_cmp_end[i] = Some(j)` marks a CMP at
    // `i` whose flags are consumed only by the branch run `[i+1, j)` and
    // are dead afterward — lowered with a native `cp` + native jumps
    // instead of rt_cmp_a + shadow-P bit tests. Ops in the run are marked
    // `fuse_consumed` and skipped by the main loop (the CMP emits them).
    let ops_slice = &routine.ops;
    // A flag producer is fusable if, walking forward over Source-transparent
    // ops, it is immediately followed by ≥1 BranchIf reading flags it sets
    // (per `map`), and those flags are dead after the run (so skipping the
    // shadow-P write is sound). Helper: returns the run end if fusable.
    // In-routine label positions, for checking flag liveness at the taken
    // path of each fused branch (not just the fall-through).
    let label_index: std::collections::HashMap<&str, usize> = ops_slice
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| match op {
            Op::Label(name) => Some((name.as_str(), idx)),
            _ => None,
        })
        .collect();
    let scan_run = |i: usize,
                    map: fn(&ir::Cond) -> Option<Z80Cond>,
                    dead_mask: u8|
     -> Option<usize> {
        let mut j = i + 1;
        let mut saw_branch = false;
        while j < ops_slice.len() {
            match &ops_slice[j] {
                Op::Source { .. } => j += 1,
                Op::BranchIf { cond, target } if map(cond).is_some() => {
                    // The taken path continues with the producer's flags
                    // intact. If the target is outside this routine, or
                    // the flags are live there, eliding the shadow write
                    // is unsound (e.g. SMB's PlayerInjuryBlink: `CMP
                    // #$F0; BCS t; ...; t: BNE` — the target's BNE reads
                    // the CMP's Z while the fall-through overwrites it).
                    match label_index.get(target.as_str()) {
                        Some(&t) => {
                            if flags_live_after(ops_slice, t, dead_mask, opts.routine_flag_reads) {
                                return None;
                            }
                        }
                        None => return None,
                    }
                    saw_branch = true;
                    j += 1;
                }
                _ => break,
            }
        }
        if saw_branch && !flags_live_after(ops_slice, j - 1, dead_mask, opts.routine_flag_reads) {
            Some(j)
        } else {
            None
        }
    };

    // `fuse_cmp_end`/`fuse_nz_end`: producer index → run end (exclusive).
    // CMP fuses to a native `cp`; LDA fuses to the load + `or a`. Both
    // emit their branch run natively and mark the run `fuse_consumed`.
    let mut fuse_cmp_end: Vec<Option<usize>> = vec![None; ops_slice.len()];
    let mut fuse_nz_end: Vec<Option<usize>> = vec![None; ops_slice.len()];
    let mut add16_plans: Vec<Option<Add16Plan>> = vec![None; ops_slice.len()];
    let mut copy_loop_plans: Vec<Option<CopyLoopPlan>> = vec![None; ops_slice.len()];
    let mut fuse_consumed: Vec<bool> = vec![false; ops_slice.len()];
    // Copy loops first — they span the most ops (init + label + body +
    // back-branch) and subsume the inner LDA/STA/INC fusions.
    for i in 0..ops_slice.len() {
        if let Some(plan) = match_copy_loop(ops_slice, i, routine, opts.routine_flag_reads) {
            for slot in fuse_consumed.iter_mut().take(plan.end).skip(i + 1) {
                *slot = true;
            }
            copy_loop_plans[i] = Some(plan);
        }
    }
    // 16-bit add idiom next (it spans 7 ops and subsumes the LDA/CLC/ADC
    // fusions that would otherwise match its pieces).
    for i in 0..ops_slice.len() {
        if fuse_consumed[i] || copy_loop_plans[i].is_some() {
            continue;
        }
        if let Some(plan) = match_add16(ops_slice, i, opts.routine_flag_reads) {
            for slot in fuse_consumed.iter_mut().take(plan.end).skip(i + 1) {
                *slot = true;
            }
            add16_plans[i] = Some(plan);
        }
    }
    for i in 0..ops_slice.len() {
        if fuse_consumed[i] || add16_plans[i].is_some() {
            continue;
        }
        let (end, target) = match &ops_slice[i] {
            // CMP sets N/Z/C; all three must be dead after the run.
            Op::CmpImm(_) | Op::CmpMem { .. } => (
                scan_run(i, cmp_cond_to_z80, F_N | F_Z | F_C),
                &mut fuse_cmp_end,
            ),
            // LDA/AND/ORA/EOR (imm) set only N/Z (C/V untouched, stay valid
            // in shadow P). AND/ORA/EOR set Z80 flags directly; LDA needs a
            // trailing `or a` (added at emit time).
            Op::LdaImm(_)
            | Op::LdaMem { .. }
            | Op::AndImm(_)
            | Op::AndMem { .. }
            | Op::OraImm(_)
            | Op::OraMem { .. }
            | Op::EorImm(_)
            | Op::EorMem { .. }
            | Op::Inx
            | Op::Iny
            | Op::Dex
            | Op::Dey => (scan_run(i, nz_cond_to_z80, F_N | F_Z), &mut fuse_nz_end),
            _ => continue,
        };
        if let Some(j) = end {
            target[i] = Some(j);
            for slot in fuse_consumed.iter_mut().take(j).skip(i + 1) {
                *slot = true;
            }
        }
    }

    // Native-flag branch tracking: does Z80 A currently hold the value
    // whose N/Z are the live 6502 N/Z? When true at an N/Z branch we test
    // the flag natively (`or a; jp cc`) instead of `ld hl,SHADOW_P; bit
    // n,(hl)`. Independent of the shadow update (re-derives from A), so
    // it's correctness-safe; the producer still maintains shadow-P.
    let mut a_holds_nz = false;
    for (op_idx, op) in routine.ops.iter().enumerate() {
        if fuse_consumed[op_idx] {
            continue;
        }
        if let Some(plan) = &copy_loop_plans[op_idx] {
            emit_copy_loop(program, plan);
            a_holds_nz = false; // A is dead post-loop; index restored explicitly
            continue;
        }
        if let Some(plan) = &add16_plans[op_idx] {
            emit_add16(program, plan);
            a_holds_nz = false; // lifted add's flags are dead; don't claim A's N/Z
            continue;
        }
        // Native N/Z branch: re-derive the flag from A instead of reading
        // shadow-P, when A provably holds the N/Z-determining value.
        if let Op::BranchIf { cond, target } = op {
            if a_holds_nz {
                if let Some(z) = nz_cond_to_z80(cond) {
                    program.or_a(); // set Z80 S/Z from A
                    emit_native_branch(program, routine, z, target);
                    continue; // a_holds_nz unchanged (branch preserves A)
                }
            }
        }
        // Update the A-holds-N/Z tracker by op type (before the match, so
        // arms that `continue` still update it). Op type, not emit shape,
        // determines the effect.
        if let Some(v) = op_nz_effect(op) {
            a_holds_nz = v;
        }
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
                if let Some(end) = fuse_nz_end[op_idx] {
                    program.or_a(); // set Z80 S/Z from A
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        nz_cond_to_z80,
                    );
                } else if nz_live[op_idx] {
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
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_read_direct(program, sms, SHADOW_X);
                        } else {
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_abs(SHADOW_X);
                            program.ld_b_a();
                            program.call(indexed_read_runtime(*base, *region));
                        }
                    }
                    (AddrExpr::AbsIndexedY(base), _) => {
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_read_direct(program, sms, SHADOW_Y);
                        } else {
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_abs(SHADOW_Y);
                            program.ld_b_a();
                            program.call(indexed_read_runtime(*base, *region));
                        }
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
                if let Some(end) = fuse_nz_end[op_idx] {
                    program.or_a(); // set Z80 S/Z from the loaded value in A
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        nz_cond_to_z80,
                    );
                } else if nz_live[op_idx] {
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
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_write_direct(program, sms, SHADOW_X);
                        } else {
                            program.ld_c_a(); // save value in C
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_abs(SHADOW_X);
                            program.ld_b_a();
                            program.ld_a_c();
                            program.call(WRITE_INDEXED);
                        }
                    }
                    (AddrExpr::AbsIndexedY(base), _) => {
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_write_direct(program, sms, SHADOW_Y);
                        } else {
                            program.ld_c_a();
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_abs(SHADOW_Y);
                            program.ld_b_a();
                            program.ld_a_c();
                            program.call(WRITE_INDEXED);
                        }
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
                program.and_imm(*v); // sets Z80 S/Z
                if let Some(end) = fuse_nz_end[op_idx] {
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        nz_cond_to_z80,
                    );
                } else if nz_live[op_idx] {
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
                program.data(None, &[0xA0]); // and b (sets Z80 S/Z)
                emit_nz_producer_tail(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    fuse_nz_end[op_idx],
                    nz_live[op_idx],
                    opts.emit_source_comments,
                );
            }

            Op::OraImm(v) => {
                program.or_imm(*v); // sets Z80 S/Z
                emit_nz_producer_tail(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    fuse_nz_end[op_idx],
                    nz_live[op_idx],
                    opts.emit_source_comments,
                );
            }

            Op::OraMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.comment("or b  ; A = A | B");
                program.data(None, &[0xB0]); // or b (sets Z80 S/Z)
                emit_nz_producer_tail(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    fuse_nz_end[op_idx],
                    nz_live[op_idx],
                    opts.emit_source_comments,
                );
            }

            Op::EorImm(v) => {
                program.xor_imm(*v); // sets Z80 S/Z
                emit_nz_producer_tail(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    fuse_nz_end[op_idx],
                    nz_live[op_idx],
                    opts.emit_source_comments,
                );
            }

            Op::EorMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                program.comment("xor b  ; A = A ^ B");
                program.data(None, &[0xA8]); // xor b (sets Z80 S/Z)
                emit_nz_producer_tail(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    fuse_nz_end[op_idx],
                    nz_live[op_idx],
                    opts.emit_source_comments,
                );
            }

            // ------------------------------------------------------------------
            // ALU: CMP / CPX / CPY
            // ------------------------------------------------------------------
            Op::CmpImm(v) => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    program.cp_imm(*v);
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        cmp_cond_to_z80,
                    );
                } else {
                    program.ld_b_imm(*v);
                    program.call(CMP_A_VIA_SHADOW);
                }
            }

            Op::CmpMem { addr, region } => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    emit_mem_to_b(program, addr, *region);
                    program.cp_b();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        cmp_cond_to_z80,
                    );
                } else {
                    emit_mem_to_b(program, addr, *region);
                    program.call(CMP_A_VIA_SHADOW);
                }
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
            Op::Inx => emit_inc_dec_xy(
                program,
                routine,
                ops_slice,
                op_idx,
                SHADOW_X,
                true,
                fuse_nz_end[op_idx],
                nz_live[op_idx],
                opts.emit_source_comments,
            ),

            Op::Iny => emit_inc_dec_xy(
                program,
                routine,
                ops_slice,
                op_idx,
                SHADOW_Y,
                true,
                fuse_nz_end[op_idx],
                nz_live[op_idx],
                opts.emit_source_comments,
            ),

            Op::Dex => emit_inc_dec_xy(
                program,
                routine,
                ops_slice,
                op_idx,
                SHADOW_X,
                false,
                fuse_nz_end[op_idx],
                nz_live[op_idx],
                opts.emit_source_comments,
            ),

            Op::Dey => emit_inc_dec_xy(
                program,
                routine,
                ops_slice,
                op_idx,
                SHADOW_Y,
                false,
                fuse_nz_end[op_idx],
                nz_live[op_idx],
                opts.emit_source_comments,
            ),

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
    // 16-bit add idiom: LDA $86; CLC; ADC #$01; STA $86; LDA $6D;
    // ADC #$00; STA $6D  (player X += 1, 16-bit). Flags killed afterward
    // (CLV + CMP) so it lifts to native add/adc with no rt_adc_a.
    #[test]
    fn add16_lifts_to_native() {
        let zp = |z| ir::AddrExpr::ZpConst(z);
        let build = lower_and_finish(vec![
            Op::LdaMem {
                addr: zp(0x86),
                region: MemRegion::ZeroPage,
            },
            Op::Clc,
            Op::AdcImm(0x01),
            Op::StaMem {
                addr: zp(0x86),
                region: MemRegion::ZeroPage,
            },
            Op::LdaMem {
                addr: zp(0x6D),
                region: MemRegion::ZeroPage,
            },
            Op::AdcImm(0x00),
            Op::StaMem {
                addr: zp(0x6D),
                region: MemRegion::ZeroPage,
            },
            Op::Clv,       // kills V
            Op::CmpImm(0), // kills N/Z/C
            Op::Rts,
        ]);
        // native add a,$01 (C6 01) and adc a,$00 (CE 00); no rt_adc_a.
        assert!(build.bytes.windows(2).any(|w| w == [0xC6, 0x01]));
        assert!(build.bytes.windows(2).any(|w| w == [0xCE, 0x00]));
        assert!(!build.asm.contains("call rt_adc_a"));
        assert!(build.asm.contains("[lifted 16-bit add]"));
    }

    #[test]
    fn cmp_imm_3() {
        let build = lower_and_finish(vec![Op::CmpImm(3)]);
        // ld b,$03 = 06 03
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x03]));
        assert!(build.asm.contains("call rt_cmp_a"));
    }

    // CMP #$10 / BEQ ; a later CMP overwrites N/Z/C before any boundary,
    // so the first compare's flags are dead after the branch -> fuse to a
    // native `cp $10` + native `jp z` (no rt_cmp_a, no shadow-P bit test).
    #[test]
    fn cmp_beq_fuses_to_native() {
        // Fusable: both the fall-through AND the branch's taken path
        // overwrite N/Z/C before any read or routine exit.
        let build = lower_and_finish(vec![
            Op::CmpImm(0x10),
            Op::BranchIf {
                cond: Cond::Zero,
                target: "L_x".into(),
            },
            Op::CmpImm(0x30), // kills flags on the fall-through
            Op::Label("L_x".into()),
            Op::CmpImm(0x20), // kills flags on the taken path
            Op::Rts,
        ]);
        // native cp $10 = FE 10
        assert!(build.bytes.windows(2).any(|w| w == [0xFE, 0x10]));
        // fused branch is a native jp z
        assert!(build.asm.contains("jp z,L_x"));

        // NOT fusable: the taken path reaches RTS with the CMP's flags
        // intact — 6502 code returns results in flags (SMB's
        // BlockBumpedChk/PlayerInjuryBlink), so the shadow write stays.
        let build = lower_and_finish(vec![
            Op::CmpImm(0x10),
            Op::BranchIf {
                cond: Cond::Zero,
                target: "L_r".into(),
            },
            Op::CmpImm(0x30),
            Op::Label("L_r".into()),
            Op::Rts,
        ]);
        assert!(build.asm.contains("call rt_cmp_a"));
        assert!(!build.bytes.windows(2).any(|w| w == [0xFE, 0x10]));
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
        // ld hl,$CB00 = 21 00 CB
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x00, 0xCB]));
        // inc (hl) = 34 (modifies shadow X in place, preserves A)
        assert!(build.bytes.contains(&0x34));
        // flags live across the routine end -> still persists to shadow P
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
            routine_flag_reads: None,
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
        // ld hl,$CB03 (21 03 CB) ; set 0,(hl) (CB C6)
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x03, 0xCB]));
        assert!(build.bytes.windows(2).any(|w| w == [0xCB, 0xC6]));
    }

    #[test]
    fn clc_clears_carry_bit() {
        let build = lower_and_finish(vec![Op::Clc]);
        // ld hl,$CB03 (21 03 CB) ; res 0,(hl) (CB 86)
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x03, 0xCB]));
        assert!(build.bytes.windows(2).any(|w| w == [0xCB, 0x86]));
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
