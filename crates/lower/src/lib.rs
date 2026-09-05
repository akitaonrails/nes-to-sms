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
    pub const LOWER_SAVED_A: u16 = 0xCB27; // lowerer spill byte; LD A,(nn) preserves flags
    pub const PPU_SCROLL_TOGGLE: u16 = 0xCB0B;
    pub const PPU_CTRL: u16 = 0xCB08;
    pub const PPU_SCROLL_X: u16 = 0xCB0C;
    pub const PPU_SCROLL_Y: u16 = 0xCB0D;
    pub const PPU_VBLANK_FLAG: u16 = 0xCB05;
    pub const PPU_MASK: u16 = 0xCB09;
    pub const PPUADDR_TOGGLE: u16 = 0xCB0E;
    pub const PPU_SPRITE0_PHASE: u16 = 0xCB12;
    pub const PPU_WRITE_VALUE: u16 = 0xCB18;
    pub const PPU_IFF_RESTORE: u16 = 0xCB19;
    pub const SPLIT_SCROLL_FLAGS: u16 = 0xCB20;
    pub const SPLIT_PRE_X: u16 = 0xCB21;
    pub const SPLIT_PRE_Y: u16 = 0xCB22;
    pub const SPLIT_POST_X: u16 = 0xCB23;
    pub const SPLIT_POST_Y: u16 = 0xCB24;
    pub const PENDING_VDP_REG1: u16 = 0xCB2D;
    pub const CHR_NT_REBUILD_DIRTY: u16 = 0xCB78;
    pub const CHR_VARIANT_FLUSH_PENDING: u16 = 0xCB7F;
    /// Sticky "8x16 sprites in use" latch (see runtime/ppu.s $CA39): once set,
    /// the 8x8 base-sprite copy-through disables itself so it cannot clobber
    /// the 8x16 pair resolver's VRAM slots.
    pub const SPRITE_8X16_SEEN: u16 = 0xCA39;
    pub const CHR_SCREEN_REBUILD_PENDING: u16 = 0xCA18;
}

// ---------------------------------------------------------------------------
// Runtime symbols
// ---------------------------------------------------------------------------

/// Symbolic runtime labels the lowering pass references.
pub mod runtime_symbols {
    pub const PPU_WRITE: &str = "rt_ppu_write";
    pub const PPU_WRITE_CONT: &str = "rt_ppu_write_cont";
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
    pub const READ_PRG_HIGH: &str = "rt_read_prg_high";
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
    pub const TRANSLATED_RTS: &str = "rt_translated_rts";
    pub const TRANSLATED_RETURN_ESCAPE: &str = "rt_translated_return_escape";
    pub const TRANSLATED_RETURN_CONSUME: &str = "rt_translated_return_consume";
    pub const BANKED_TAIL_DISPATCH: &str = "rt_banked_tail_dispatch";
    pub const BRK: &str = "rt_brk";
    pub const RTI: &str = "rt_rti";
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
    UnsupportedMapperStore { pc: Option<u16>, reason: String },
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
            LowerError::UnsupportedMapperStore {
                pc: Some(pc),
                reason,
            } => write!(f, "unsupported mapper store at ${pc:04X}: {reason}"),
            LowerError::UnsupportedMapperStore { pc: None, reason } => {
                write!(f, "unsupported mapper store: {reason}")
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

/// Phase R: which resident index register an emission uses (X=D, Y=E).
#[derive(Clone, Copy, PartialEq, Eq)]
enum IdxReg {
    X,
    Y,
}

impl IdxReg {
    fn load_into_a(self, p: &mut z80_emit::Program) {
        match self {
            IdxReg::X => p.ld_a_d(),
            IdxReg::Y => p.ld_a_e_reg(),
        }
    }
    fn store_from_a(self, p: &mut z80_emit::Program) {
        match self {
            IdxReg::X => p.ld_d_a_reg(),
            IdxReg::Y => p.ld_e_a_reg(),
        }
    }
}

// H.2 (optimizer plan): indexed-access specialization. When `base+idx`
// provably stays inside one flat window for every idx 0-255, the
// dispatcher's runtime range classification is dead weight — emit the
// direct add + access inline.

/// SMS base for a direct indexed access into plain NES RAM. The whole
/// $C000-$C7FF shadow qualifies: the runtime fallback (`rt_read_indexed` /
/// `rt_write_indexed`) performs the same unfolded 16-bit add with no
/// mirror handling, so the inline form is behavior-identical for every
/// base in the window — including bases whose `base+idx` could cross
/// $C7FF (both paths would read the same out-of-shadow byte).
fn indexed_plain_ram_base(base: u16, region: ir::MemRegion) -> Option<u16> {
    use ir::MemRegion as R;
    if !matches!(region, R::ZeroPage | R::Ram | R::RamMirror | R::Stack) {
        return None;
    }
    let sms = indexed_base_to_sms(base, region);
    ((0xC000..=0xC7FF).contains(&sms)).then_some(sms)
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

/// Fixed-high PRG read. Mapper 2 must restore the exact selected window through
/// the guarded helper. NROM has one immutable low window, so retain the proven
/// inline map/read/restore sequence used by the accepted SMB routes.
fn emit_prg_high_read_direct(
    p: &mut z80_emit::Program,
    nes_addr: u16,
    guarded_mapper_window: bool,
) {
    if guarded_mapper_window {
        p.ld_hl_imm(nes_addr);
        p.call(runtime_symbols::READ_PRG_HIGH);
    } else {
        // NROM: $FFFC is 0 at op boundaries and irq_handler restores the
        // interrupted slot-2 bank itself, so the restore is two plain
        // instructions — no helper call, no SRAM-control clear.
        p.ld_a_bank_imm("data_prg_high");
        p.ld_abs_a(0xFFFF);
        p.ld_a_abs(nes_addr - 0x4000);
        p.ld_c_a();
        p.ld_a_bank_imm("data_prg_low");
        p.ld_abs_a(0xFFFF);
        p.ld_a_c();
    }
}

/// H2 fixed-high indexed read. See `emit_prg_high_read_direct` for why mapper 2
/// uses the guarded helper while NROM keeps the accepted inline sequence.
fn emit_prg_high_indexed_direct(
    p: &mut z80_emit::Program,
    base: u16,
    idx: IdxReg,
    guarded_mapper_window: bool,
) {
    if guarded_mapper_window {
        p.ld_hl_imm(base);
        idx.load_into_a(p);
        p.ld_b_a();
        p.call(runtime_symbols::READ_PRG_HIGH_INDEXED);
    } else {
        p.ld_a_bank_imm("data_prg_high");
        p.ld_abs_a(0xFFFF);
        p.ld_hl_imm(base - 0x4000);
        idx.load_into_a(p);
        p.add_a_l();
        p.ld_l_a();
        p.ld_a_h();
        p.adc_a_imm0();
        p.ld_h_a();
        p.ld_a_hl_ptr();
        p.ld_c_a();
        p.ld_a_bank_imm("data_prg_low");
        p.ld_abs_a(0xFFFF);
        p.ld_a_c();
    }
}

/// A := (sms_base + idx). Clobbers HL/B/C and native flags.
fn emit_indexed_read_direct(p: &mut z80_emit::Program, sms_base: u16, idx: IdxReg) {
    // Phase R: index from the resident register; 16-bit add via A/L so
    // BC/DE stay untouched.
    p.ld_hl_imm(sms_base);
    idx.load_into_a(p);
    p.add_a_l();
    p.ld_l_a();
    p.ld_a_h();
    p.adc_a_imm0();
    p.ld_h_a();
    p.ld_a_hl_ptr();
}

/// H.1c: inline shadow-N/Z update from A via the pinned $3E00 lookup
/// table (see runtime/flags.s). Replaces `call rt_set_nz_a`. Clobbers
/// HL and native flags; preserves A. Callers must not hold live state
/// in HL across a shadow-NZ update (the lowering never carries HL
/// across IR ops; verified against the three-route oracle).
fn emit_set_nz_inline(p: &mut z80_emit::Program) {
    p.ld_l_a();
    p.ld_h_imm(0x3E);
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7D);
    p.or_hl_ptr();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_l();
}

/// H.10: inline flag-computing ALU bodies at flag-live sites. Mirrors the
/// branchless helper bodies in runtime/flags.s exactly, minus call/ret and
/// register-save overhead: at IR-op granularity only A (and the shadow
/// state) survive an op, so B/C/E/HL are free scratch. The Z80 F layout
/// maps to 6502 P with S->N and C->C in place; Z moves bit6->bit1 (3x
/// rlca), V (P/V) bit2->bit6 (4x rlca).

/// A = A + B + shadowC; shadow N/V/Z/C updated. Clobbers B?no(B kept as
/// operand input, untouched)/C/E/HL.
fn emit_adc_flags_inline(p: &mut z80_emit::Program) {
    p.ld_c_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.rrca();
    p.ld_a_c();
    p.adc_a_b();
    p.ld_c_a();
    p.push_af();
    p.pop_hl();
    p.ld_a_l();
    p.and_imm(0x81);
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x40);
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x04);
    p.rlca();
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x3C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// A = A - B - (1-shadowC); shadow N/V/Z/C updated (6502 borrow polarity).
fn emit_sbc_flags_inline(p: &mut z80_emit::Program) {
    p.ld_c_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.rrca();
    p.ccf();
    p.ld_a_c();
    p.sbc_a_b();
    p.ld_c_a();
    p.push_af();
    p.pop_hl();
    p.ld_a_l();
    p.and_imm(0x81);
    p.xor_imm(0x01);
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x40);
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x04);
    p.rlca();
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x3C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// CMP A vs B: shadow N/Z/C updated, A preserved.
fn emit_cmp_flags_inline(p: &mut z80_emit::Program) {
    p.ld_c_a();
    p.sub_b();
    p.push_af();
    p.pop_hl();
    p.ld_a_l();
    p.and_imm(0x81);
    p.xor_imm(0x01);
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x40);
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// CPX/CPY: like CMP but comparing a shadow register; A preserved via E.
fn emit_cpxy_flags_inline(p: &mut z80_emit::Program, idx: IdxReg) {
    p.ld_c_a();
    idx.load_into_a(p);
    p.sub_b();
    p.push_af();
    p.pop_hl();
    p.ld_a_l();
    p.and_imm(0x81);
    p.xor_imm(0x01);
    p.ld_h_a();
    p.ld_a_l();
    p.and_imm(0x40);
    p.rlca();
    p.rlca();
    p.rlca();
    p.or_h();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// LSR A with live flags: C = old bit0, N = 0, Z via the $3E00 table.
fn emit_lsr_a_flags_inline(p: &mut z80_emit::Program) {
    p.srl_a();
    p.ld_c_a();
    p.sbc_a_a();
    p.and_imm(0x01);
    p.ld_l_c();
    p.ld_h_imm(0x3E);
    p.or_hl_ptr();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// ASL A with live flags: C = old bit7, N/Z via the table.
fn emit_asl_a_flags_inline(p: &mut z80_emit::Program) {
    p.add_a_a();
    p.ld_c_a();
    p.sbc_a_a();
    p.and_imm(0x01);
    p.ld_l_c();
    p.ld_h_imm(0x3E);
    p.or_hl_ptr();
    p.ld_h_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_h();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// ROL A with live flags: result rotates through shadow C; new C is old bit 7.
fn emit_rol_a_flags_inline(p: &mut z80_emit::Program) {
    let have_c = p.fresh_label("rol_a_have_c");
    let no_n = p.fresh_label("rol_a_no_n");
    let no_z = p.fresh_label("rol_a_no_z");

    p.ld_b_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.rrca();
    p.ld_a_b();
    p.rl_a();
    p.ld_c_a();
    p.ld_b_imm(0x00);
    p.jr_nc(&have_c);
    p.ld_b_imm(0x01);
    p.label(&have_c);
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_b();
    p.ld_b_a();
    p.ld_a_c();
    p.bit_a(7);
    p.jr_z(&no_n);
    p.ld_a_b();
    p.or_imm(0x80);
    p.ld_b_a();
    p.label(&no_n);
    p.ld_a_c();
    p.or_a();
    p.jr_nz(&no_z);
    p.ld_a_b();
    p.or_imm(0x02);
    p.ld_b_a();
    p.label(&no_z);
    p.ld_a_b();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// ROR A with live flags: result rotates through shadow C; new C is old bit 0.
fn emit_ror_a_flags_inline(p: &mut z80_emit::Program) {
    let have_c = p.fresh_label("ror_a_have_c");
    let no_n = p.fresh_label("ror_a_no_n");
    let no_z = p.fresh_label("ror_a_no_z");

    p.ld_b_a();
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.rrca();
    p.ld_a_b();
    p.rr_a();
    p.ld_c_a();
    p.ld_b_imm(0x00);
    p.jr_nc(&have_c);
    p.ld_b_imm(0x01);
    p.label(&have_c);
    p.ld_a_abs(sms_layout::SHADOW_P);
    p.and_imm(0x7C);
    p.or_b();
    p.ld_b_a();
    p.ld_a_c();
    p.bit_a(7);
    p.jr_z(&no_n);
    p.ld_a_b();
    p.or_imm(0x80);
    p.ld_b_a();
    p.label(&no_n);
    p.ld_a_c();
    p.or_a();
    p.jr_nz(&no_z);
    p.ld_a_b();
    p.or_imm(0x02);
    p.ld_b_a();
    p.label(&no_z);
    p.ld_a_b();
    p.ld_abs_a(sms_layout::SHADOW_P);
    p.ld_a_c();
}

/// H.4: inline 6502 push

/// H.4: inline 6502 push

/// H.4: inline 6502 push — A to $C100+S, S decremented. A preserved.
/// Clobbers HL/D and native flags (same contract as rt_push6502).
fn emit_push6502_inline(p: &mut z80_emit::Program) {
    // RESERVE the slot (publish S-1) before writing the byte at old-S.
    // The write-then-publish order raced with the IRQ/NMI bridge: an
    // interrupt between the write and the S update pushes its 3-byte
    // frame at the same S and clobbers the in-flight byte (surfaced as
    // a garbage rt_rts_dispatch target mid-route).
    p.ld_c_a();
    p.ld_a_abs(sms_layout::SHADOW_S);
    p.dec_a();
    p.ld_abs_a(sms_layout::SHADOW_S);
    p.inc_a();
    p.ld_l_a();
    p.ld_h_imm(0xC1);
    p.ld_hl_ptr_c();
    p.ld_a_c();
}

/// H.4: inline 6502 pop — A from $C100+S+1, then S incremented. Read
/// BEFORE publishing: once S+1 is published, the IRQ/NMI bridge may
/// push its frame over the just-vacated slot; reading afterwards races.
/// Clobbers HL/B/C and native flags.
fn emit_pop6502_inline(p: &mut z80_emit::Program) {
    p.ld_a_abs(sms_layout::SHADOW_S);
    p.inc_a();
    p.ld_l_a();
    p.ld_h_imm(0xC1);
    p.ld_c_a();
    p.ld_a_hl_ptr();
    p.ld_b_a();
    p.ld_a_c();
    p.ld_abs_a(sms_layout::SHADOW_S);
    p.ld_a_b();
}

/// (sms_base + idx) := A; A preserved (6502 store contract). Clobbers
/// HL/C/DE and native flags.
fn emit_indexed_write_direct(p: &mut z80_emit::Program, sms_base: u16, idx: IdxReg) {
    // (sms_base + idx) := A; A preserved. Clobbers HL/C; DE untouched.
    p.ld_c_a();
    p.ld_hl_imm(sms_base);
    idx.load_into_a(p);
    p.add_a_l();
    p.ld_l_a();
    p.ld_a_h();
    p.adc_a_imm0();
    p.ld_h_a();
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
    target: IdxReg,
    guarded_mapper_window: bool,
) {
    use ir::{AddrExpr, MemRegion};
    // Keep translated LDX/LDY memory loads off the native Z80 stack. The old
    // `push af`/`pop bc` spill could cross the hard $DD80 stack guard in deep
    // NMI/update call chains. A RAM spill is safe here because Z80 `ld a,(nn)`
    // does not alter flags, so the N/Z result from emit_set_nz_inline remains
    // live for a following 6502 branch.
    program.ld_abs_a(sms_layout::LOWER_SAVED_A);
    match (addr, region) {
        (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
            program.ld_a_abs(sms_layout::NES_ZP_BASE + *z as u16);
        }
        (AddrExpr::Const(a), MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack) => {
            program.ld_a_abs(nes_ram_addr_to_sms(*a));
        }
        (AddrExpr::Const(a), MemRegion::PpuReg | MemRegion::PpuMirror) => {
            if (*a & 0x0007) == 2 {
                emit_ppu_status_read_inline(program);
            } else {
                program.ld_b_imm((*a & 0x0007) as u8);
                program.call(runtime_symbols::PPU_READ);
            }
        }
        (AddrExpr::Const(a), MemRegion::PrgRom) if *a < 0xC000 => {
            program.ld_a_abs(*a);
        }
        (AddrExpr::Const(a), MemRegion::PrgRom) => {
            emit_prg_high_read_direct(program, *a, guarded_mapper_window);
        }
        (AddrExpr::AbsIndexedX(base), _) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                emit_indexed_read_direct(program, sms, IdxReg::X);
            } else {
                if region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                    emit_prg_high_indexed_direct(program, *base, IdxReg::X, guarded_mapper_window);
                } else {
                    program.ld_hl_imm(indexed_base_to_sms(*base, region));
                    program.ld_a_d();
                    program.ld_b_a();
                    program.call(indexed_read_runtime(*base, region));
                }
            }
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                emit_indexed_read_direct(program, sms, IdxReg::Y);
            } else {
                if region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                    emit_prg_high_indexed_direct(program, *base, IdxReg::Y, guarded_mapper_window);
                } else {
                    program.ld_hl_imm(indexed_base_to_sms(*base, region));
                    program.ld_a_e_reg();
                    program.ld_b_a();
                    program.call(indexed_read_runtime(*base, region));
                }
            }
        }
        (AddrExpr::ZpIndexedX(zp), _) => {
            program.ld_a_d();
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            program.ld_a_hl_ptr();
        }
        (AddrExpr::ZpIndexedY(zp), _) => {
            program.ld_a_e_reg();
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
    target.store_from_a(program);
    emit_set_nz_inline(program);
    program.ld_a_abs(sms_layout::LOWER_SAVED_A);
}

fn restore_a_keep_flags_after_push_af(program: &mut z80_emit::Program) {
    program.pop_bc();
    program.ld_a_b();
}

/// Inline 6502 BIT with B = memory operand and A = accumulator.
/// Updates shadow N/V/Z, preserves A, and avoids the native-stack cost of a
/// runtime helper call in deep translated call chains. Clobbers C/H/native flags.
fn emit_bit_mem_inline(program: &mut z80_emit::Program) {
    let no_z = program.fresh_label("bit_no_z");
    let after_z = program.fresh_label("bit_after_z");

    program.ld_c_a(); // preserve accumulator
    program.and_b(); // Z := (A & M) == 0
    program.jr_nz(&no_z);
    program.ld_a_abs(sms_layout::SHADOW_P);
    program.and_imm(0b0011_1101); // clear N, V, Z
    program.or_imm(0b0000_0010); // set Z
    program.jr(&after_z);
    program.label(&no_z);
    program.ld_a_abs(sms_layout::SHADOW_P);
    program.and_imm(0b0011_1101); // clear N, V, Z
    program.label(&after_z);
    program.ld_h_a(); // H = P with updated Z
    program.ld_a_b();
    program.and_imm(0b1100_0000); // N/V come from memory operand bits 7/6
    program.or_h();
    program.ld_abs_a(sms_layout::SHADOW_P);
    program.ld_a_c(); // restore accumulator; LD does not alter flags
}

/// Common emission for STX/STY into any supported addressing mode.
/// `shadow_addr` is the SMS RAM address holding the X or Y shadow byte.
/// Preserves A and the caller's flags via push/pop AF.
fn emit_stxy_mem(
    program: &mut z80_emit::Program,
    addr: &ir::AddrExpr,
    region: ir::MemRegion,
    src: IdxReg,
) {
    use ir::{AddrExpr, MemRegion};
    program.push_af();
    match (addr, region) {
        (AddrExpr::ZpConst(z), MemRegion::ZeroPage) => {
            src.load_into_a(program);
            program.ld_abs_a(sms_layout::NES_ZP_BASE + *z as u16);
        }
        (AddrExpr::Const(a), MemRegion::Ram | MemRegion::RamMirror | MemRegion::Stack) => {
            src.load_into_a(program);
            program.ld_abs_a(nes_ram_addr_to_sms(*a));
        }
        (AddrExpr::AbsIndexedX(base), _) => {
            // Load X/Y into the value, also load X (index) — but the value
            // and the index can be the same shadow byte. Use C as scratch.
            if let Some(sms) = indexed_direct_base(*base, region) {
                src.load_into_a(program);
                emit_indexed_write_direct(program, sms, IdxReg::X);
            } else {
                src.load_into_a(program);
                program.ld_c_a(); // C = value (X or Y)
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_d();
                program.ld_b_a();
                program.ld_a_c();
                program.call(runtime_symbols::WRITE_INDEXED);
            }
        }
        (AddrExpr::AbsIndexedY(base), _) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                src.load_into_a(program);
                emit_indexed_write_direct(program, sms, IdxReg::Y);
            } else {
                src.load_into_a(program);
                program.ld_c_a();
                program.ld_hl_imm(indexed_base_to_sms(*base, region));
                program.ld_a_e_reg();
                program.ld_b_a();
                program.ld_a_c();
                program.call(runtime_symbols::WRITE_INDEXED);
            }
        }
        (AddrExpr::ZpIndexedX(zp), _) => {
            // (zp + X) & $FF wrap.
            program.ld_a_d();
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            src.load_into_a(program);
            program.ld_hl_ptr_a();
        }
        (AddrExpr::ZpIndexedY(zp), _) => {
            program.ld_a_e_reg();
            program.add_a_imm(*zp);
            program.ld_l_a();
            program.ld_h_imm((sms_layout::NES_ZP_BASE >> 8) as u8);
            src.load_into_a(program);
            program.ld_hl_ptr_a();
        }
        (AddrExpr::Const(a), MemRegion::ApuIo) => {
            // STX/STY to an APU register routes through the APU shim like
            // STA does (SMB's Dump_Squ1_Regs is `STY $4001 / STX $4000`;
            // these were silently dropped before, muting whole channels).
            src.load_into_a(program);
            if *a == 0x4016 {
                program.call(runtime_symbols::CONTROLLER_STROBE);
            } else {
                program.ld_hl_imm(*a);
                program.call(runtime_symbols::APU_WRITE);
            }
        }
        (AddrExpr::Const(a), MemRegion::PpuReg | MemRegion::PpuMirror) => {
            src.load_into_a(program);
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
    match src {
        ValueSrc::A => {}
        ValueSrc::X => {
            p.ld_a_d();
        }
        ValueSrc::Y => {
            p.ld_a_e_reg();
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

/// Inline the stackless `$2005 PPUSCROLL` write path.
///
/// Entry/exit: A is the 6502 accumulator value being stored and is restored on
/// exit. The emulated 6502 flags live in `SHADOW_P`; native Z80 flags are scratch.
/// This avoids a native `call rt_ppu_write` frame in SMB's deepest translated NMI
/// chains while keeping the same RAM latch/split-scroll side effects as
/// `runtime/ppu.s`'s `_ppu_w_scroll` body.
fn emit_ppu_scroll_write_inline(p: &mut z80_emit::Program) {
    use sms_layout::*;
    let scroll_y = p.fresh_label("ppu_scroll_y");
    let capture_post = p.fresh_label("ppu_scroll_post");
    let clear_toggle = p.fresh_label("ppu_scroll_clear_toggle");
    let restore_a = p.fresh_label("ppu_scroll_restore_a");

    p.comment("inline STA $2005 PPUSCROLL (stackless)");
    p.ld_abs_a(PPU_WRITE_VALUE);
    // The NES has one shared first/second-write latch for $2005 and $2006.
    // PPUADDR_TOGGLE is canonical; keep the older scroll shadow synchronized
    // because runtime diagnostics still expose it.
    p.ld_a_abs(PPUADDR_TOGGLE);
    p.or_a();
    p.jr_nz(&scroll_y);

    // First write = X scroll.
    p.ld_a_abs(PPU_WRITE_VALUE);
    p.ld_abs_a(PPU_SCROLL_X);
    p.ld_a_imm(1);
    p.ld_abs_a(PPU_SCROLL_TOGGLE);
    p.ld_abs_a(PPUADDR_TOGGLE);
    p.ld_a_abs(PPU_WRITE_VALUE);
    p.jr(&restore_a);

    // Second write = Y scroll, then capture the completed pair as pre/post split.
    p.label(&scroll_y);
    p.ld_a_abs(PPU_WRITE_VALUE);
    p.ld_abs_a(PPU_SCROLL_Y);
    p.ld_a_abs(SPLIT_SCROLL_FLAGS);
    p.and_imm(1);
    p.jr_nz(&capture_post);

    p.ld_a_abs(PPU_SCROLL_X);
    p.ld_abs_a(SPLIT_PRE_X);
    p.ld_a_abs(PPU_SCROLL_Y);
    p.ld_abs_a(SPLIT_PRE_Y);
    p.ld_hl_imm(SPLIT_SCROLL_FLAGS);
    p.set_n_hl_ptr(1);
    p.jr(&clear_toggle);

    p.label(&capture_post);
    p.ld_a_abs(PPU_SCROLL_X);
    p.ld_abs_a(SPLIT_POST_X);
    p.ld_a_abs(PPU_SCROLL_Y);
    p.ld_abs_a(SPLIT_POST_Y);
    p.ld_hl_imm(SPLIT_SCROLL_FLAGS);
    p.set_n_hl_ptr(2);

    p.label(&clear_toggle);
    p.xor_a();
    p.ld_abs_a(PPU_SCROLL_TOGGLE);
    p.ld_abs_a(PPUADDR_TOGGLE);

    p.label(&restore_a);
    p.ld_a_abs(PPU_WRITE_VALUE);
}

/// Inline the stackless `$2000 PPUCTRL` write path.
///
/// Entry/exit: A is the 6502 accumulator value being stored and is restored on
/// exit. DE is preserved because this inline path never touches it; BC/HL and
/// native flags are scratch. The VDP register-6 control-port pair keeps the
/// same DI/EI guard as `rt_ppu_write` without adding a native call frame.
/// A profile with a prepared-SAT runtime can defer that physical pair while
/// retaining the same guest shadows, dirty intent, registers and IFF behavior.
fn emit_ppu_ctrl_write_inline(
    p: &mut z80_emit::Program,
    chr_ram: bool,
    defer_sprite_registers: bool,
) {
    use sms_layout::*;

    let was_disabled = p.fresh_label("ppu_ctrl_was_disabled");
    let body = p.fresh_label("ppu_ctrl_body");
    let no_nt_flip = p.fresh_label("ppu_ctrl_no_nt_flip");
    let no_tile_flip = p.fresh_label("ppu_ctrl_no_tile_flip");
    let sprite_base_2000 = p.fresh_label("ppu_ctrl_sprite_base_2000");
    let sprite_base_0000 = p.fresh_label("ppu_ctrl_sprite_base_0000");
    let sprite_base_set = p.fresh_label("ppu_ctrl_sprite_base_set");
    let display_done = p.fresh_label("ppu_ctrl_reg1_display_done");
    let sprite_done = p.fresh_label("ppu_ctrl_reg1_sprite_done");
    let done_no_ei = p.fresh_label("ppu_ctrl_done_no_ei");
    let restore_a = p.fresh_label("ppu_ctrl_restore_a");

    p.comment("inline STA $2000 PPUCTRL (stackless)");
    p.ld_abs_a(PPU_WRITE_VALUE);
    p.ld_a_i();
    p.di();
    p.jp_po(&was_disabled);
    p.ld_a_imm(1);
    p.ld_abs_a(PPU_IFF_RESTORE);
    p.jr(&body);
    p.label(&was_disabled);
    p.xor_a();
    p.ld_abs_a(PPU_IFF_RESTORE);

    p.label(&body);
    p.ld_a_abs(PPU_WRITE_VALUE);
    p.ld_c_a();

    if chr_ram {
        // Runtime NES_CHR_RAM side effects: nametable-page flips and pattern-
        // table flips mark deferred rebuild/variant-flush work.
        p.ld_a_abs(PPU_CTRL);
        p.xor_c();
        p.and_imm(0x01);
        p.jr_z(&no_nt_flip);
        p.ld_a_imm(1);
        p.ld_abs_a(CHR_NT_REBUILD_DIRTY);
        p.label(&no_nt_flip);

        p.ld_a_abs(PPU_CTRL);
        p.xor_c();
        p.and_imm(0x10);
        p.jr_z(&no_tile_flip);
        p.ld_a_imm(1);
        p.ld_abs_a(CHR_VARIANT_FLUSH_PENDING);
        p.label(&no_tile_flip);
    }

    p.ld_a_c();
    p.ld_abs_a(PPU_CTRL);

    if chr_ram {
        // Sticky 8x16 latch: once the game selects 8x16 sprites, the pair
        // resolver owns the SMS $2000+ sprite region and the 8x8 base-sprite
        // copy-through must disable itself (see runtime/ppu.s $CA39 and the
        // $2007 pattern path). A is PPUCTRL here and must survive for the
        // sprite-base logic below, so bracket the store with push/pop af.
        let no_8x16_latch = p.fresh_label("ppu_ctrl_no_8x16_latch");
        p.bit_a(5);
        p.jr_z(&no_8x16_latch);
        p.push_af();
        p.ld_a_imm(1);
        p.ld_abs_a(SPRITE_8X16_SEEN);
        p.pop_af();
        p.label(&no_8x16_latch);
    }

    // Mirror NES PPUCTRL bit 3 into SMS VDP register 6 for 8x8 sprites. In
    // 8x16 mode the NES selects the pattern table per OAM tile bit, so the
    // CHR-RAM SAT resolver uses its pair cache at the SMS $2000 base.
    if !defer_sprite_registers {
        p.bit_a(5);
        p.jr_nz(&sprite_base_2000);
        p.bit_a(3);
        p.jr_nz(&sprite_base_0000);
        p.label(&sprite_base_2000);
        p.ld_a_imm(0xff);
        p.jr(&sprite_base_set);
        p.label(&sprite_base_0000);
        p.ld_a_imm(0xfb);
        p.label(&sprite_base_set);
        p.out_a(0xbf);
        p.ld_a_imm(0x86);
        p.out_a(0xbf);
    }

    emit_ppu_reg1_latch_inline(p, &display_done, &sprite_done);

    p.ld_a_abs(PPU_IFF_RESTORE);
    p.or_a();
    p.jr_z(&done_no_ei);
    p.ei();
    p.jr(&restore_a);
    p.label(&done_no_ei);
    p.label(&restore_a);
    p.ld_a_abs(PPU_WRITE_VALUE);
}

/// Inline the stackless `$2001 PPUMASK` write path.
///
/// Stores the mask shadow and refreshes the deferred SMS VDP register-1 latch.
/// The small DI/EI guard mirrors `rt_ppu_write`'s interrupt behavior without a
/// call frame.
fn emit_ppu_mask_write_inline(p: &mut z80_emit::Program, chr_ram: bool) {
    use sms_layout::*;

    let was_disabled = p.fresh_label("ppu_mask_was_disabled");
    let body = p.fresh_label("ppu_mask_body");
    let display_done = p.fresh_label("ppu_mask_reg1_display_done");
    let sprite_done = p.fresh_label("ppu_mask_reg1_sprite_done");
    let done_no_ei = p.fresh_label("ppu_mask_done_no_ei");
    let restore_a = p.fresh_label("ppu_mask_restore_a");
    let no_enable_edge = p.fresh_label("ppu_mask_no_enable_edge");
    let old_render_off = p.fresh_label("ppu_mask_old_render_off");

    p.comment("inline STA $2001 PPUMASK (stackless)");
    p.ld_abs_a(PPU_WRITE_VALUE);
    p.ld_a_i();
    p.di();
    p.jp_po(&was_disabled);
    p.ld_a_imm(1);
    p.ld_abs_a(PPU_IFF_RESTORE);
    p.jr(&body);
    p.label(&was_disabled);
    p.xor_a();
    p.ld_abs_a(PPU_IFF_RESTORE);

    p.label(&body);
    if chr_ram {
        p.ld_a_abs(PPU_MASK);
        p.and_imm(0x18);
        p.jr_z(&old_render_off);
        p.ld_a_abs(PPU_WRITE_VALUE);
        p.and_imm(0x18);
        p.jr_nz(&no_enable_edge);
        p.xor_a();
        p.ld_abs_a(CHR_SCREEN_REBUILD_PENDING);
        p.jr(&no_enable_edge);
        p.label(&old_render_off);
        p.ld_a_abs(PPU_WRITE_VALUE);
        p.and_imm(0x18);
        p.jr_z(&no_enable_edge);
        p.ld_a_abs(CHR_SCREEN_REBUILD_PENDING);
        p.cp_imm(0x40);
        p.jr_nc(&no_enable_edge);
        p.xor_a();
        p.ld_abs_a(CHR_SCREEN_REBUILD_PENDING);
        p.label(&no_enable_edge);
    }
    p.ld_a_abs(PPU_WRITE_VALUE);
    p.ld_abs_a(PPU_MASK);
    emit_ppu_reg1_latch_inline(p, &display_done, &sprite_done);

    p.ld_a_abs(PPU_IFF_RESTORE);
    p.or_a();
    p.jr_z(&done_no_ei);
    p.ei();
    p.jr(&restore_a);
    p.label(&done_no_ei);
    p.label(&restore_a);
    p.ld_a_abs(PPU_WRITE_VALUE);
}

fn emit_ppu_reg1_latch_inline(p: &mut z80_emit::Program, display_done: &str, sprite_done: &str) {
    use sms_layout::*;

    // Compose SMS VDP register 1 from PPUCTRL/PPUMASK and defer the actual VDP
    // write to the vblank-aligned presentation path, matching runtime/ppu.s.
    p.ld_a_imm(0xb0);
    p.ld_c_a();
    p.ld_a_abs(PPU_MASK);
    p.and_imm(0x18);
    p.jr_z(display_done);
    p.ld_a_c();
    p.or_imm(0x40);
    p.ld_c_a();
    p.label(display_done);
    p.ld_a_abs(PPU_CTRL);
    p.bit_a(5);
    p.jr_z(sprite_done);
    p.ld_a_c();
    p.or_imm(0x02);
    p.ld_c_a();
    p.label(sprite_done);
    p.ld_a_c();
    p.ld_abs_a(PENDING_VDP_REG1);
}

fn emit_ppu_write_callless(program: &mut z80_emit::Program, reg: u8, native_calls: bool) {
    use runtime_symbols::*;

    if native_calls {
        // The stackless-continuation scheme exists to keep deep translated
        // NMI chains off the native stack; native stack discipline uses the
        // native stack anyway, so a plain call is both cheaper (no cont-ptr
        // store, no exit dispatch) and simpler.
        program.ld_b_imm(reg);
        program.call(PPU_WRITE);
        return;
    }
    let cont = program.fresh_label("ppu_write_cont");
    program.ld_b_imm(reg);
    program.ld_hl_label(&cont);
    program.jp(PPU_WRITE_CONT);
    program.label(&cont);
}

/// Inline the stackless `$2002 PPUSTATUS` read path.
///
/// Entry/exit: A is replaced with the status byte. DE (resident translated X/Y)
/// is preserved; BC/HL/native flags are scratch. This mirrors
/// `runtime/ppu.s`'s `_ppu_r_status` side effects and keeps the `rt_ppu_read`
/// DI/EI critical-section semantics without placing another return address on
/// the native Z80 stack in the deepest translated-NMI chains.
fn emit_ppu_status_read_inline(p: &mut z80_emit::Program) {
    use sms_layout::*;

    let was_disabled = p.fresh_label("ppu_status_was_disabled");
    let body = p.fresh_label("ppu_status_body");
    let no_vblank = p.fresh_label("ppu_status_no_vblank");
    let sprite0 = p.fresh_label("ppu_status_sprite0");
    let not_stale = p.fresh_label("ppu_status_not_stale");
    let hit = p.fresh_label("ppu_status_hit");
    let report_hit = p.fresh_label("ppu_status_report_hit");
    let finish = p.fresh_label("ppu_status_finish");
    let done = p.fresh_label("ppu_status_done");

    p.comment("inline LDA/LDX/LDY $2002 PPUSTATUS (stackless)");
    p.ld_a_i();
    p.di();
    p.jp_po(&was_disabled);
    p.ld_a_imm(1);
    p.ld_abs_a(PPU_IFF_RESTORE);
    p.jr(&body);
    p.label(&was_disabled);
    p.xor_a();
    p.ld_abs_a(PPU_IFF_RESTORE);

    p.label(&body);
    p.ld_a_abs(PPU_VBLANK_FLAG);
    p.ld_c_a();
    p.xor_a();
    p.ld_abs_a(PPU_VBLANK_FLAG);
    p.ld_abs_a(PPU_SCROLL_TOGGLE);
    p.ld_abs_a(PPUADDR_TOGGLE);

    p.ld_a_c();
    p.or_a();
    p.jr_z(&no_vblank);
    p.ld_c_imm(0x80);
    p.jr(&sprite0);
    p.label(&no_vblank);
    p.ld_c_imm(0x00);

    p.label(&sprite0);
    p.ld_a_abs(PPU_MASK);
    p.and_imm(0x18);
    p.jr_z(&finish);
    // Three-phase stale -> clear -> hit sprite-0 timing; mirrors
    // `_ppu_r_status_sprite0` in runtime/ppu.s. Keep the two aligned.
    p.ld_a_abs(PPU_SPRITE0_PHASE);
    p.or_a();
    p.jr_nz(&not_stale);
    p.ld_a_imm(1);
    p.ld_abs_a(PPU_SPRITE0_PHASE);
    p.jr(&report_hit);

    p.label(&not_stale);
    p.cp_imm(1);
    p.jr_nz(&hit);
    p.ld_a_imm(2);
    p.ld_abs_a(PPU_SPRITE0_PHASE);
    p.jr(&finish); // the "clear" observation: no bit 6

    p.label(&hit);
    p.ld_hl_imm(SPLIT_SCROLL_FLAGS);
    p.set_n_hl_ptr(0);

    p.label(&report_hit);
    p.ld_a_c();
    p.or_imm(0x40);
    p.ld_c_a();

    p.label(&finish);
    p.ld_a_abs(PPU_IFF_RESTORE);
    p.or_a();
    p.ld_a_c();
    p.jr_z(&done);
    p.ei();
    p.label(&done);
}

/// Inline SMB-style `LDA $4016,X` controller polling.
///
/// X=0 reads controller 1 through the serial latch; X!=0 returns 0 without
/// advancing the controller-1 shift index. Mirrors `runtime/input.s` while
/// avoiding a call frame in deep translated update/NMI chains. Preserves DE;
/// clobbers AF/BC/native flags.
fn emit_controller_read_indexed_x_inline(p: &mut z80_emit::Program) {
    let read_p1 = p.fresh_label("ctrl_read_p1");
    let shift_done = p.fresh_label("ctrl_read_shift_done");
    let shift_loop = p.fresh_label("ctrl_read_shift");
    let open = p.fresh_label("ctrl_read_open");
    let done = p.fresh_label("ctrl_read_done");

    p.comment("inline LDA $4016,X controller read (stackless)");
    p.ld_a_d();
    p.or_a();
    p.jr_z(&read_p1);
    p.xor_a();
    p.jr(&done);

    p.label(&read_p1);
    p.ld_a_abs(0xCB07);
    p.cp_imm(8);
    p.jr_nc(&open);
    p.ld_b_a();
    p.ld_a_b();
    p.or_a();
    p.ld_a_abs(0xCB06);
    p.jr_z(&shift_done);
    p.label(&shift_loop);
    p.rrca();
    p.djnz(&shift_loop);
    p.label(&shift_done);
    p.and_imm(0x01);
    p.ld_c_a();
    p.ld_a_abs(0xCB07);
    p.inc_a();
    p.ld_abs_a(0xCB07);
    p.ld_a_c();
    p.jr(&done);

    p.label(&open);
    p.ld_a_imm(1);
    p.label(&done);
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
fn emit_mem_to_b_with_mode(
    p: &mut z80_emit::Program,
    addr: &ir::AddrExpr,
    region: ir::MemRegion,
    guarded_mapper_window: bool,
) {
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
            if guarded_mapper_window {
                p.ld_c_a();
                emit_prg_high_read_direct(p, *a, true);
                p.ld_b_a();
                p.ld_a_c();
            } else {
                p.push_af();
                emit_prg_high_read_direct(p, *a, false);
                p.ld_b_a();
                p.pop_af();
            }
        }
        AddrExpr::AbsIndexedX(base) => {
            // For indexed reads we must preserve A across the read (the
            // operand lands in B). Save A in C, read, restore.
            if let Some(sms) = indexed_direct_base(*base, region) {
                p.ld_c_a();
                p.ld_hl_imm(sms);
                p.ld_a_d();
                p.add_a_l();
                p.ld_l_a();
                p.ld_a_h();
                p.adc_a_imm0();
                p.ld_h_a();
                p.ld_b_hl_ptr();
                p.ld_a_c();
            } else if region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                if guarded_mapper_window {
                    p.ld_c_a();
                    emit_prg_high_indexed_direct(p, *base, IdxReg::X, true);
                    p.ld_b_a();
                    p.ld_a_c();
                } else {
                    p.push_af();
                    emit_prg_high_indexed_direct(p, *base, IdxReg::X, false);
                    p.ld_b_a();
                    p.pop_af();
                }
            } else {
                // rt_read_indexed returns the operand in A and clobbers BC, so
                // do not park the 6502 accumulator in C here. Flag-live ADC/SBC
                // call this path and need the original A after the indexed read
                // (SMB's DigitsMathRoutine: ADC $07D7,Y).
                p.push_af();
                p.ld_hl_imm(indexed_base_to_sms(*base, region));
                p.ld_a_d();
                p.ld_b_a();
                p.call(indexed_read_runtime(*base, region));
                p.ld_b_a();
                p.pop_af();
            }
        }
        AddrExpr::AbsIndexedY(base) => {
            if let Some(sms) = indexed_direct_base(*base, region) {
                p.ld_c_a();
                p.ld_hl_imm(sms);
                p.ld_a_e_reg();
                p.add_a_l();
                p.ld_l_a();
                p.ld_a_h();
                p.adc_a_imm0();
                p.ld_h_a();
                p.ld_b_hl_ptr();
                p.ld_a_c();
            } else if region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                if guarded_mapper_window {
                    p.ld_c_a();
                    emit_prg_high_indexed_direct(p, *base, IdxReg::Y, true);
                    p.ld_b_a();
                    p.ld_a_c();
                } else {
                    p.push_af();
                    emit_prg_high_indexed_direct(p, *base, IdxReg::Y, false);
                    p.ld_b_a();
                    p.pop_af();
                }
            } else {
                // rt_read_indexed clobbers BC; preserve the accumulator on the
                // native stack instead of in C.
                p.push_af();
                p.ld_hl_imm(indexed_base_to_sms(*base, region));
                p.ld_a_e_reg();
                p.ld_b_a();
                p.call(indexed_read_runtime(*base, region));
                p.ld_b_a();
                p.pop_af();
            }
        }
        AddrExpr::ZpIndexedX(zp) => {
            // 6502 zp,X wraps within zero page. Compute (zp+X) & $FF in A,
            // load value via HL=$C000 | offset into B (preserving caller A).
            p.ld_c_a(); // C = caller A
            p.ld_a_d();
            p.add_a_imm(*zp);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.ld_b_hl_ptr();
            p.ld_a_c(); // restore A
        }
        AddrExpr::ZpIndexedY(zp) => {
            p.ld_c_a();
            p.ld_a_e_reg();
            p.add_a_imm(*zp);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.ld_b_hl_ptr();
            p.ld_a_c();
        }
        AddrExpr::IndirectY(zp) => {
            // rt_read_zp_ptr_y also clobbers BC; preserve A across the helper.
            p.push_af();
            p.ld_b_imm(*zp);
            p.call(READ_ZP_PTR_Y);
            p.ld_b_a();
            p.pop_af();
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
            // Resident X is D: build the EA without touching A (native
            // flags are clobbered, as everywhere between IR ops).
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_c_d();
            p.ld_b_imm(0);
            p.add_hl_bc();
        }
        AddrExpr::AbsIndexedY(base) => {
            p.ld_hl_imm(indexed_base_to_sms(*base, region));
            p.ld_c_e();
            p.ld_b_imm(0);
            p.add_hl_bc();
        }
        AddrExpr::ZpIndexedX(z) => {
            p.push_af();
            p.ld_a_d();
            p.add_a_imm(*z);
            p.ld_l_a();
            p.ld_h_imm((NES_ZP_BASE >> 8) as u8);
            p.pop_af();
        }
        AddrExpr::ZpIndexedY(z) => {
            p.push_af();
            p.ld_a_e_reg();
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
            | Op::ReturnEscape { .. }
            | Op::JmpIndirect { .. }
            | Op::Brk { .. }
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
                | Op::ReturnEscape { .. }
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
            Op::Jmp { .. } | Op::ReturnEscape { .. } => return true,
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

/// Emit a branch on a Z80 native flag, handling cross-section translated
/// targets the same way `Op::BranchIf` does: for a far target, invert
/// the condition to skip past a translated tail jump. Conditional jumps
/// don't clobber flags, so chained native branches off one `cp` stay valid.
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
        program.translated_tail_jmp(target);
        program.label(&skip);
    }
}

/// Map a 6502 branch condition to the Z80 native condition that holds
/// after an op that sets S/Z from its result (e.g. `or a` after a load,
/// or `inc`/`dec`). Only N/Z conditions are derivable this way; C/V
/// branches aren't.
/// Condition map for producers whose 6502 carry has the SAME polarity
/// as the Z80 carry: ADC (carry-out) and ASL/LSR (shifted-out bit).
/// CMP/CPX/CPY/SBC use `cmp_cond_to_z80` (6502 C = !Z80 borrow).
fn direct_cond_to_z80(cond: &ir::Cond) -> Option<Z80Cond> {
    use ir::Cond;
    Some(match cond {
        Cond::Zero => Z80Cond::Z,
        Cond::NotZero => Z80Cond::Nz,
        Cond::Carry => Z80Cond::C,
        Cond::NoCarry => Z80Cond::Nc,
        Cond::Negative => Z80Cond::M,
        Cond::Positive => Z80Cond::P,
        Cond::Overflow | Cond::NoOverflow => return None,
    })
}

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
    target: IdxReg,
    is_inc: bool,
    fuse_end: Option<usize>,
    nz_live: bool,
    emit_comments: bool,
) {
    // Phase R: inc/dec the resident register directly (4T; sets S/Z,
    // preserves A and carry).
    match (target, is_inc) {
        (IdxReg::X, true) => program.inc_d(),
        (IdxReg::X, false) => program.dec_d(),
        (IdxReg::Y, true) => program.inc_e(),
        (IdxReg::Y, false) => program.dec_e(),
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
        // Non-adjacent flag reader: persist to shadow P from the register.
        program.push_af();
        target.load_into_a(program);
        emit_set_nz_inline(program);
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
        emit_set_nz_inline(program);
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
    idx: IdxReg,
    exit_idx: u8, // value the loop index holds on exit
    end: usize,   // one past the loop's back-branch
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
        idx: if want_x { IdxReg::X } else { IdxReg::Y },
        exit_idx,
        end: branch_idx + 1,
    })
}

/// Emit a lifted copy loop: `ld hl,src; ld de,dst; ld bc,count; ldir`,
/// then restore the loop index's exit value. (A is dead; flags dead.)
fn emit_copy_loop(program: &mut z80_emit::Program, plan: &CopyLoopPlan) {
    program.comment(format!("[lifted copy loop: {} bytes via ldir]", plan.count));
    // Phase R: LDIR uses DE as the destination pointer — save the resident
    // X/Y pair around it, then set the loop register's exit value.
    program.push_de();
    program.ld_hl_imm(plan.src_sms);
    program.ld_de_imm(plan.dst_sms);
    program.ld_bc_imm(plan.count);
    program.ldir();
    program.pop_de();
    match plan.idx {
        IdxReg::X => {
            program.data(None, &[0x16, plan.exit_idx]); // ld d,exit
        }
        IdxReg::Y => {
            program.data(None, &[0x1E, plan.exit_idx]); // ld e,exit
        }
    }
}

/// S1.3a: a maximal run of consecutive accumulator shifts/rotates
/// (ASL/LSR/ROL/ROR A, Source-transparent). The 6502 carry threads
/// natively through the run, so shadow-P is touched at most twice: read
/// once before the run when the first element consumes carry (ROL/ROR),
/// and written once after it — only when a flag reader survives and the
/// trailing branches couldn't be fused natively.
#[derive(Clone)]
struct ShiftRunPlan {
    shifts: Vec<ir::Op>,
    last_idx: usize,
    /// Exclusive end of a fused trailing branch run (None = no fusion).
    fuse_end: Option<usize>,
    consumes_carry: bool,
    write_back: bool,
}

fn emit_shift_run(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    ops_slice: &[ir::Op],
    plan: &ShiftRunPlan,
    emit_comments: bool,
) {
    use ir::Op;
    if plan.consumes_carry {
        // Shadow C -> native carry, preserving A.
        program.ld_b_a();
        program.ld_a_abs(sms_layout::SHADOW_P);
        program.rrca();
        program.ld_a_b();
    }
    let n = plan.shifts.len();
    for (k, op) in plan.shifts.iter().enumerate() {
        // Intermediate elements only need the carry chain (4-cycle rla/rra);
        // the final element uses the CB forms so S/Z are native for fusion.
        let last = k + 1 == n;
        match op {
            Op::AslA => program.add_a_a(),
            Op::LsrA => program.srl_a(),
            Op::RolA if last => program.rl_a(),
            Op::RolA => program.rla(),
            Op::RorA if last => program.rr_a(),
            Op::RorA => program.rra(),
            _ => unreachable!("non-shift op in shift run"),
        }
    }
    if let Some(end) = plan.fuse_end {
        emit_fused_branches(
            program,
            routine,
            ops_slice,
            plan.last_idx + 1,
            end,
            emit_comments,
            direct_cond_to_z80,
        );
    } else if plan.write_back {
        // Materialize shadow N/Z/C once: C from native carry, N/Z from the
        // pinned $3E00 lookup table. V is untouched by rotates.
        program.ld_c_a();
        program.ld_b_imm(0);
        program.rl_b(); // B = new 6502 carry bit
        program.ld_a_abs(sms_layout::SHADOW_P);
        program.and_imm(0x7C); // clear N, Z, C; keep V and the rest
        program.or_b();
        program.ld_b_a();
        program.ld_a_c();
        program.ld_l_a();
        program.ld_h_imm(0x3E);
        program.ld_a_b();
        program.or_hl_ptr();
        program.ld_abs_a(sms_layout::SHADOW_P);
        program.ld_a_c();
    }
}

/// S1.3c: strided fill-until-wrap loop:
///   L: STA base,idx ; INX/INY × stride ; BNE L
/// (SMB's MoveSpritesOffscreen shape: fill every `stride`-th byte of the
/// 256-byte window at `base` from the current index up to the wrap.)
/// The fill value is whatever A holds at loop entry — no constant needed.
/// Exit state is architectural: idx = 0, Z set, N clear (the final INY/INX
/// wrapped to zero), C/V untouched, A preserved.
///
/// Soundness: the loop terminates on the 6502 only when the entry index is
/// a multiple of `stride` (otherwise it never hits zero and hangs); the
/// lifted form computes count = (256 - idx) / stride, which matches every
/// terminating entry state.
#[derive(Clone)]
struct FillLoopPlan {
    /// Exclusive end (index just past the back-branch).
    end: usize,
    sms_base: u16,
    stride: u8,
    use_x: bool,
}

fn match_fill_loop(ops: &[ir::Op], i: usize, routine: &ir::Routine) -> Option<FillLoopPlan> {
    use ir::{Cond, Op};
    let label = match ops.get(i)? {
        Op::Label(l) => l.clone(),
        _ => return None,
    };
    let sta = skip_source(ops, i + 1);
    let (base, region, use_x) = match ops.get(sta)? {
        Op::StaMem {
            addr: ir::AddrExpr::AbsIndexedY(b),
            region,
        } => (*b, *region, false),
        Op::StaMem {
            addr: ir::AddrExpr::AbsIndexedX(b),
            region,
        } => (*b, *region, true),
        _ => return None,
    };
    if !matches!(
        region,
        ir::MemRegion::Ram | ir::MemRegion::RamMirror | ir::MemRegion::Stack
    ) {
        return None;
    }
    let sms_base = indexed_base_to_sms(base, region);
    // The whole 256-byte index window must stay inside the RAM shadow.
    if sms_base < 0xC000 || sms_base.checked_add(0xFF)? > 0xC7FF {
        return None;
    }
    // Count the INY/INX run.
    let mut j = skip_source(ops, sta + 1);
    let mut stride = 0u16;
    loop {
        match (ops.get(j)?, use_x) {
            (Op::Iny, false) | (Op::Inx, true) => {
                stride += 1;
                j = skip_source(ops, j + 1);
            }
            _ => break,
        }
    }
    if stride == 0 || 256 % stride != 0 {
        return None;
    }
    match ops.get(j)? {
        Op::BranchIf {
            cond: Cond::NotZero,
            target,
        } if *target == label => {}
        _ => return None,
    }
    // The loop head must have no referrer besides the back-branch.
    let refs = routine
        .ops
        .iter()
        .filter(|op| match op {
            Op::BranchIf { target, .. } | Op::Jmp { target } => *target == label,
            _ => false,
        })
        .count();
    if refs != 1 {
        return None;
    }
    Some(FillLoopPlan {
        end: j + 1,
        sms_base,
        stride: stride as u8,
        use_x,
    })
}

fn emit_fill_loop(program: &mut z80_emit::Program, plan: &FillLoopPlan) {
    let max_count = (256 / plan.stride as u16) as u8; // 256/stride, mod-256 for stride 1
    let loop_top = program.fresh_label("fill_loop");
    let count_ok = program.fresh_label("fill_count_ok");
    // C = fill value (A preserved for the 6502), HL = base + idx,
    // B = (256 - idx) / stride with the idx=0 case meaning a full window.
    program.ld_c_a();
    program.ld_hl_imm(plan.sms_base);
    if plan.use_x {
        program.ld_a_d();
    } else {
        program.ld_a_e_reg();
    }
    program.ld_b_a();
    program.ld_a_imm(0);
    program.sub_b();
    for _ in 0..plan.stride.trailing_zeros() {
        program.rrca();
    }
    if plan.stride > 1 {
        program.and_imm((0xFFu16 >> plan.stride.trailing_zeros()) as u8);
    }
    program.or_a();
    program.jr_nz(&count_ok);
    program.ld_a_imm(max_count);
    program.label(&count_ok);
    program.ld_b_a();
    if plan.use_x {
        program.ld_a_d();
    } else {
        program.ld_a_e_reg();
    }
    program.add_a_l();
    program.ld_l_a();
    program.ld_a_h();
    program.adc_a_imm0();
    program.ld_h_a();
    program.label(&loop_top);
    program.ld_hl_ptr_c();
    for _ in 0..plan.stride {
        program.inc_hl();
    }
    program.djnz(&loop_top);
    // Architectural exit state: idx = 0; Z set, N clear; A restored.
    if plan.use_x {
        program.ld_d_imm(0);
    } else {
        program.ld_e_imm(0);
    }
    program.ld_a_abs(sms_layout::SHADOW_P);
    program.and_imm(0x7D); // clear N, keep the rest
    program.or_imm(0x02); // set Z
    program.ld_abs_a(sms_layout::SHADOW_P);
    program.ld_a_c();
}

/// S1.3d: conditional strided decrement loop (SMB's NMI timer loop shape):
///   L: LDA base,idx ; BEQ S ; DEC base,idx ; S: DEX/DEY ; BPL L
/// Lifted to a pointer walk: one EA computation, `dec (hl)` in place, and
/// a 16-bit pointer decrement (page-crossing safe for any entry index).
/// Exit state is architectural: idx = $FF, A = the last byte read, N set,
/// Z clear (the final DEX/DEY wrapped), C/V untouched.
#[derive(Clone)]
struct CondDecLoopPlan {
    end: usize,
    sms_base: u16,
    use_x: bool,
    write_flags: bool,
}

fn match_cond_dec_loop(
    ops: &[ir::Op],
    i: usize,
    routine: &ir::Routine,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> Option<CondDecLoopPlan> {
    use ir::{AddrExpr, Cond, Op};
    let head = match ops.get(i)? {
        Op::Label(l) => l.clone(),
        _ => return None,
    };
    let lda = skip_source(ops, i + 1);
    let (base, region, use_x) = match ops.get(lda)? {
        Op::LdaMem {
            addr: AddrExpr::AbsIndexedX(b),
            region,
        } => (*b, *region, true),
        Op::LdaMem {
            addr: AddrExpr::AbsIndexedY(b),
            region,
        } => (*b, *region, false),
        _ => return None,
    };
    if !matches!(
        region,
        ir::MemRegion::Ram | ir::MemRegion::RamMirror | ir::MemRegion::Stack
    ) {
        return None;
    }
    let sms_base = indexed_base_to_sms(base, region);
    if !(0xC000..=0xC7FF).contains(&sms_base) {
        return None;
    }
    let beq = skip_source(ops, lda + 1);
    if base == 0x0780 {}
    let skip_lbl = match ops.get(beq)? {
        Op::BranchIf {
            cond: Cond::Zero,
            target,
        } => target.clone(),
        _ => return None,
    };
    let dec = skip_source(ops, beq + 1);
    match ops.get(dec)? {
        Op::DecMem {
            addr: AddrExpr::AbsIndexedX(b),
            ..
        } if use_x && *b == base => {}
        Op::DecMem {
            addr: AddrExpr::AbsIndexedY(b),
            ..
        } if !use_x && *b == base => {}
        _ => return None,
    }
    let skip_at = skip_source(ops, dec + 1);
    match ops.get(skip_at)? {
        Op::Label(l) if *l == skip_lbl => {}
        _ => return None,
    }
    let step = skip_source(ops, skip_at + 1);
    match (ops.get(step)?, use_x) {
        (Op::Dex, true) | (Op::Dey, false) => {}
        _ => return None,
    }
    let bpl = skip_source(ops, step + 1);
    match ops.get(bpl)? {
        Op::BranchIf {
            cond: Cond::Positive,
            target,
        } if *target == head => {}
        _ => return None,
    }
    // The interior skip label must belong to this loop alone. The HEAD may
    // have outside referrers: they enter at the label, which precedes the
    // lifted loop, and the lift computes its pointer from the live index —
    // identical semantics for any entry (SMB's NMI enters the timer loop
    // from two BPL sites with different X).
    {
        let lbl = &skip_lbl;
        let refs = routine
            .ops
            .iter()
            .filter(|op| match op {
                Op::BranchIf { target, .. } | Op::Jmp { target } => target == lbl,
                _ => false,
            })
            .count();
        if refs != 1 {
            return None;
        }
    }
    let write_flags = flags_live_after(ops, bpl, F_N | F_Z, reads);
    Some(CondDecLoopPlan {
        end: bpl + 1,
        sms_base,
        use_x,
        write_flags,
    })
}

fn emit_cond_dec_loop(program: &mut z80_emit::Program, plan: &CondDecLoopPlan) {
    let loop_top = program.fresh_label("cdec_loop");
    let skip = program.fresh_label("cdec_skip");
    program.ld_hl_imm(plan.sms_base);
    if plan.use_x {
        program.ld_c_d();
    } else {
        program.ld_c_e();
    }
    program.ld_b_imm(0);
    program.add_hl_bc();
    program.label(&loop_top);
    program.ld_a_hl_ptr();
    program.or_a();
    program.jr_z(&skip);
    program.dec_hl_ptr();
    program.label(&skip);
    program.dec_hl();
    if plan.use_x {
        program.dec_d();
    } else {
        program.dec_e();
    }
    program.jp_p(&loop_top);
    if plan.write_flags {
        // Exit flags are constant: the final DEX/DEY wrapped to $FF.
        program.ld_b_a();
        program.ld_a_abs(sms_layout::SHADOW_P);
        program.and_imm(0xFD); // clear Z
        program.or_imm(0x80); // set N
        program.ld_abs_a(sms_layout::SHADOW_P);
        program.ld_a_b();
    }
}

/// S1.7: inline (zp),Y read for a constant zero-page pointer (the common
/// LDA (zp),Y). Mirrors rt_read_zp_ptr_y: 16-bit pointer from the zp pair,
/// + Y, then RAM/mirror remap or a fixed-high PRG helper read. `zp = $FF`
/// (page-wrap pair) keeps the helper. A := byte; clobbers BC/HL/flags.
fn emit_read_zp_ptr_y_inline(p: &mut z80_emit::Program, zp: u8) {
    use runtime_symbols::READ_ZP_PTR_Y;
    if zp == 0xFF {
        p.ld_b_imm(zp);
        p.call(READ_ZP_PTR_Y);
        return;
    }
    let high = p.fresh_label("rzpy_high");
    let deref = p.fresh_label("rzpy_deref");
    let mirror = p.fresh_label("rzpy_mirror");
    let done = p.fresh_label("rzpy_done");
    p.ld_hl_abs(sms_layout::NES_ZP_BASE + zp as u16);
    p.ld_c_e();
    p.ld_b_imm(0);
    p.add_hl_bc();
    p.ld_a_h();
    p.cp_imm(0xC0);
    p.jr_nc(&high);
    p.cp_imm(0x08);
    p.jr_nc(&mirror);
    p.add_a_imm(0xC0);
    p.ld_h_a();
    p.jr(&deref);
    p.label(&mirror);
    p.cp_imm(0x20);
    p.jr_nc(&deref);
    p.and_imm(0x07);
    p.add_a_imm(0xC0);
    p.ld_h_a();
    p.label(&deref);
    p.ld_a_hl_ptr();
    p.jr(&done);
    p.label(&high);
    p.call("rt_read_prg_high");
    p.label(&done);
}

/// S1.3e: memory rotate chain (SMB's PRNG shape):
///   L: ROR base,X ; INX ; DEY ; BNE L
/// The 6502 carry threads through the chain of bytes; the lift loads
/// shadow C once, runs `rr (hl)` across Y bytes, and materializes the
/// final carry only if a reader survives. The head label may have other
/// referrers (the seed's CLC/SEC paths converge there): external entries
/// read the shadow carry those paths just wrote.
/// Exit: X += entry Y, Y = 0, shadow Z set / N clear (the DEY), C = the
/// final rotate's carry-out.
#[derive(Clone)]
struct RorChainPlan {
    end: usize,
    sms_base: u16,
    write_flags: bool,
}

fn match_ror_chain(
    ops: &[ir::Op],
    i: usize,
    routine: &ir::Routine,
    reads: Option<&std::collections::HashMap<String, u8>>,
) -> Option<RorChainPlan> {
    use ir::{AddrExpr, Cond, Op};
    let head = match ops.get(i)? {
        Op::Label(l) => l.clone(),
        _ => return None,
    };
    let ror = skip_source(ops, i + 1);
    let (base, region) = match ops.get(ror)? {
        Op::RorMem {
            addr: AddrExpr::AbsIndexedX(b),
            region,
        } => (*b, *region),
        _ => return None,
    };
    if !matches!(
        region,
        ir::MemRegion::Ram | ir::MemRegion::RamMirror | ir::MemRegion::Stack
    ) {
        return None;
    }
    let sms_base = indexed_base_to_sms(base, region);
    if !(0xC000..=0xC7FF).contains(&sms_base) {
        return None;
    }
    let inx = skip_source(ops, ror + 1);
    if !matches!(ops.get(inx)?, Op::Inx) {
        return None;
    }
    let dey = skip_source(ops, inx + 1);
    if !matches!(ops.get(dey)?, Op::Dey) {
        return None;
    }
    let bne = skip_source(ops, dey + 1);
    match ops.get(bne)? {
        Op::BranchIf {
            cond: Cond::NotZero,
            target,
        } if *target == head => {}
        _ => return None,
    }
    let _ = routine;
    let write_flags = flags_live_after(ops, bne, F_N | F_Z | F_C, reads);
    Some(RorChainPlan {
        end: bne + 1,
        sms_base,
        write_flags,
    })
}

fn emit_ror_chain(program: &mut z80_emit::Program, plan: &RorChainPlan) {
    let loop_top = program.fresh_label("rorc_loop");
    let done = program.fresh_label("rorc_done");
    program.ld_hl_imm(plan.sms_base);
    program.ld_c_d();
    program.ld_b_imm(0);
    program.add_hl_bc();
    program.ld_c_a(); // park A: the 6502 loop never touches it
    program.ld_a_d();
    program.add_a_e();
    program.ld_d_a_reg(); // X += Y up front (memory walk uses HL)
    program.ld_b_e(); // count
    program.ld_a_e();
    program.or_a();
    program.jr_z(&done); // Y = 0 would mean 256 DEYs on the 6502; SMB never does
    program.ld_a_abs(sms_layout::SHADOW_P);
    program.rrca(); // shadow C -> native carry
    program.label(&loop_top);
    program.rr_hl_ptr();
    program.inc_hl();
    program.djnz(&loop_top);
    program.label(&done);
    program.ld_e_imm(0);
    if plan.write_flags {
        program.ld_b_imm(0);
        program.rl_b(); // B = final carry
        program.ld_a_abs(sms_layout::SHADOW_P);
        program.and_imm(0x7C); // clear N/Z/C
        program.or_imm(0x02); // Z set (the final DEY hit zero)
        program.or_b();
        program.ld_abs_a(sms_layout::SHADOW_P);
    }
    program.ld_a_c(); // restore the parked accumulator
}

/// S1.3b: flag-aware memory shift/rotate (ASL/LSR/ROL/ROR on memory).
/// The CB (hl) forms produce native S/Z/C directly; shadow-P is read only
/// when the op consumes carry (ROL/ROR) and written only when a flag
/// reader survives an unfused site. A (untouched by 6502 memory shifts)
/// is preserved without the native stack.
#[allow(clippy::too_many_arguments)]
fn emit_shift_mem(
    program: &mut z80_emit::Program,
    routine: &ir::Routine,
    ops_slice: &[ir::Op],
    op_idx: usize,
    op: &ir::Op,
    fused: Option<usize>,
    opts: &LowerOptions,
) {
    use ir::Op;
    let (addr, region, consumes_carry) = match op {
        Op::AslMem { addr, region } => (addr, *region, false),
        Op::LsrMem { addr, region } => (addr, *region, false),
        Op::RolMem { addr, region } => (addr, *region, true),
        Op::RorMem { addr, region } => (addr, *region, true),
        _ => unreachable!("emit_shift_mem on non-shift op"),
    };
    emit_hl_for_rw_mem(program, addr, region);
    if consumes_carry {
        program.ld_b_a();
        program.ld_a_abs(sms_layout::SHADOW_P);
        program.rrca();
        program.ld_a_b();
    }
    match op {
        Op::AslMem { .. } => program.sla_hl_ptr(),
        Op::LsrMem { .. } => program.srl_hl_ptr(),
        Op::RolMem { .. } => program.rl_hl_ptr(),
        Op::RorMem { .. } => program.rr_hl_ptr(),
        _ => unreachable!(),
    }
    if let Some(end) = fused {
        emit_fused_branches(
            program,
            routine,
            ops_slice,
            op_idx + 1,
            end,
            opts.emit_source_comments,
            direct_cond_to_z80,
        );
        return;
    }
    if !flags_live_after(ops_slice, op_idx, F_N | F_Z | F_C, opts.routine_flag_reads) {
        return;
    }
    // Materialize shadow N/Z/C once from the result and the native carry.
    program.ld_c_a();
    program.ld_b_imm(0);
    program.rl_b(); // B = new 6502 carry bit
    program.ld_a_hl_ptr(); // shifted result
    program.ld_l_a();
    program.ld_h_imm(0x3E);
    program.ld_a_abs(sms_layout::SHADOW_P);
    program.and_imm(0x7C);
    program.or_b();
    program.or_hl_ptr();
    program.ld_abs_a(sms_layout::SHADOW_P);
    program.ld_a_c();
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

    // A profile-less lowering pass must remain conservative. Mapper 0 is the
    // only layout whose low PRG window is immutable and can use the accepted
    // inline fixed-high read sequence; mapper 2 restores an exact live bank.
    let guarded_mapper_window = opts.profile.is_none_or(|profile| profile.rom.mapper != 0);
    let emit_mem_to_b =
        |program: &mut z80_emit::Program, addr: &ir::AddrExpr, region: ir::MemRegion| {
            emit_mem_to_b_with_mode(program, addr, region, guarded_mapper_window)
        };

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
    // H.5: direct-polarity producers (ADC carry-out, ASL/LSR shifted-out
    // bit) fuse through `direct_cond_to_z80`.
    let mut fuse_direct_end: Vec<Option<usize>> = vec![None; ops_slice.len()];
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
    // S1.3c: strided fill-until-wrap loops (lifted whole).
    let mut fill_loop_plans: Vec<Option<FillLoopPlan>> = vec![None; ops_slice.len()];
    for i in 0..ops_slice.len() {
        if fuse_consumed[i] || copy_loop_plans[i].is_some() {
            continue;
        }
        if let Some(plan) = match_fill_loop(ops_slice, i, routine) {
            for slot in fuse_consumed.iter_mut().take(plan.end).skip(i + 1) {
                *slot = true;
            }
            fill_loop_plans[i] = Some(plan);
        }
    }
    // S1.3d: conditional strided decrement loops (lifted whole).
    let mut cond_dec_plans: Vec<Option<CondDecLoopPlan>> = vec![None; ops_slice.len()];
    for i in 0..ops_slice.len() {
        if fuse_consumed[i] || copy_loop_plans[i].is_some() || fill_loop_plans[i].is_some() {
            continue;
        }
        if let Some(plan) = match_cond_dec_loop(ops_slice, i, routine, opts.routine_flag_reads) {
            for slot in fuse_consumed.iter_mut().take(plan.end).skip(i + 1) {
                *slot = true;
            }
            cond_dec_plans[i] = Some(plan);
        }
    }
    // S1.3e: memory rotate chains (lifted whole).
    let mut ror_chain_plans: Vec<Option<RorChainPlan>> = vec![None; ops_slice.len()];
    for i in 0..ops_slice.len() {
        if fuse_consumed[i]
            || copy_loop_plans[i].is_some()
            || fill_loop_plans[i].is_some()
            || cond_dec_plans[i].is_some()
        {
            continue;
        }
        if let Some(plan) = match_ror_chain(ops_slice, i, routine, opts.routine_flag_reads) {
            for slot in fuse_consumed.iter_mut().take(plan.end).skip(i + 1) {
                *slot = true;
            }
            ror_chain_plans[i] = Some(plan);
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
    // S1.3a: carry-threaded shift/rotate runs. Matched before the generic
    // single-producer fusions so a run's head isn't claimed as a lone
    // ASL/LSR fusion.
    let mut shift_run_plans: Vec<Option<ShiftRunPlan>> = vec![None; ops_slice.len()];
    {
        let is_shift = |op: &Op| matches!(op, Op::AslA | Op::LsrA | Op::RolA | Op::RorA);
        let mut i = 0;
        while i < ops_slice.len() {
            if fuse_consumed[i]
                || copy_loop_plans[i].is_some()
                || add16_plans[i].is_some()
                || !is_shift(&ops_slice[i])
            {
                i += 1;
                continue;
            }
            let mut shifts = vec![ops_slice[i].clone()];
            let mut last_idx = i;
            let mut j = i + 1;
            while j < ops_slice.len() {
                match &ops_slice[j] {
                    Op::Source { .. } => j += 1,
                    op if is_shift(op) && !fuse_consumed[j] => {
                        shifts.push(op.clone());
                        last_idx = j;
                        j += 1;
                    }
                    _ => break,
                }
            }
            // Lone ASL/LSR keep their existing tuned paths; runs and lone
            // ROL/ROR (whose only path was the full shadow body) plan here.
            if shifts.len() == 1 && matches!(ops_slice[i], Op::AslA | Op::LsrA) {
                i = j;
                continue;
            }
            let fuse_end = scan_run(last_idx, direct_cond_to_z80, F_N | F_Z | F_C);
            let write_back = fuse_end.is_none()
                && flags_live_after(
                    ops_slice,
                    last_idx,
                    F_N | F_Z | F_C,
                    opts.routine_flag_reads,
                );
            let consumes_carry = matches!(ops_slice[i], Op::RolA | Op::RorA);
            let plan_end = fuse_end.unwrap_or(last_idx + 1);
            for slot in fuse_consumed.iter_mut().take(plan_end).skip(i + 1) {
                *slot = true;
            }
            shift_run_plans[i] = Some(ShiftRunPlan {
                shifts,
                last_idx,
                fuse_end,
                consumes_carry,
                write_back,
            });
            i = j;
        }
    }
    for i in 0..ops_slice.len() {
        if fuse_consumed[i] || add16_plans[i].is_some() || shift_run_plans[i].is_some() {
            continue;
        }
        let (end, target) = match &ops_slice[i] {
            // CMP/CPX/CPY set N/Z/C; all three must be dead after the run.
            // SBC additionally sets V (mask includes it; Overflow branches
            // map to None so V-consuming runs never fuse) and writes A.
            Op::CmpImm(_)
            | Op::CmpMem { .. }
            | Op::CpxImm(_)
            | Op::CpxMem { .. }
            | Op::CpyImm(_)
            | Op::CpyMem { .. } => (
                scan_run(i, cmp_cond_to_z80, F_N | F_Z | F_C),
                &mut fuse_cmp_end,
            ),
            Op::SbcImm(_) | Op::SbcMem { .. } => (
                scan_run(i, cmp_cond_to_z80, F_N | F_Z | F_C | F_V),
                &mut fuse_cmp_end,
            ),
            // ADC and accumulator shifts: 6502 carry has Z80 polarity.
            Op::AdcImm(_) | Op::AdcMem { .. } => (
                scan_run(i, direct_cond_to_z80, F_N | F_Z | F_C | F_V),
                &mut fuse_direct_end,
            ),
            Op::AslA | Op::LsrA => (
                scan_run(i, direct_cond_to_z80, F_N | F_Z | F_C),
                &mut fuse_direct_end,
            ),
            // S1.3b: memory shifts via the CB (hl) forms — native S/Z/C.
            Op::AslMem { .. } | Op::LsrMem { .. } | Op::RolMem { .. } | Op::RorMem { .. } => (
                scan_run(i, direct_cond_to_z80, F_N | F_Z | F_C),
                &mut fuse_direct_end,
            ),
            // INC/DEC memory set only N/Z; `inc/dec (hl)` gives native S/Z.
            Op::IncMem { .. } | Op::DecMem { .. } => {
                (scan_run(i, nz_cond_to_z80, F_N | F_Z), &mut fuse_nz_end)
            }
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
        if let Some(plan) = &fill_loop_plans[op_idx] {
            // The loop-head label still exists for the (single) back-branch
            // reference bookkeeping; emit it, then the lifted fill.
            if let Op::Label(name) = op {
                program.label(name);
            }
            emit_fill_loop(program, plan);
            a_holds_nz = false; // exit N/Z are the INY/INX wrap, not A's
            continue;
        }
        if let Some(plan) = &cond_dec_plans[op_idx] {
            if let Op::Label(name) = op {
                program.label(name);
            }
            emit_cond_dec_loop(program, plan);
            a_holds_nz = false; // exit N/Z are the DEX/DEY wrap, not A's
            continue;
        }
        if let Some(plan) = &ror_chain_plans[op_idx] {
            if let Op::Label(name) = op {
                program.label(name);
            }
            emit_ror_chain(program, plan);
            a_holds_nz = false;
            continue;
        }
        if let Some(plan) = &add16_plans[op_idx] {
            emit_add16(program, plan);
            a_holds_nz = false; // lifted add's flags are dead; don't claim A's N/Z
            continue;
        }
        if let Some(plan) = &shift_run_plans[op_idx] {
            emit_shift_run(program, routine, ops_slice, plan, opts.emit_source_comments);
            a_holds_nz = true; // A holds the run result; its N/Z are the live N/Z
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
            Op::Brk { pc } => {
                // NES BRK is a 2-byte software interrupt: the pushed return
                // PC is the BRK site + 2. rt_brk builds the 3-byte frame on
                // the emulated stack and vectors to translated_irq; control
                // resumes wherever the handler's RTI frame says.
                program.comment("BRK");
                program.ld_hl_imm(pc.wrapping_add(2));
                program.jp(BRK);
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

            Op::UnsupportedMapperStore {
                pc,
                mnemonic,
                reason,
                ..
            } => {
                return Err(LowerError::UnsupportedMapperStore {
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
                    emit_set_nz_inline(program);
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
                        emit_prg_high_read_direct(program, *a, guarded_mapper_window);
                    }
                    (AddrExpr::AbsIndexedX(0x4016), MemRegion::ApuIo) => {
                        emit_controller_read_indexed_x_inline(program);
                    }
                    (AddrExpr::AbsIndexedX(base), _) => {
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_read_direct(program, sms, IdxReg::X);
                        } else if *region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                            emit_prg_high_indexed_direct(
                                program,
                                *base,
                                IdxReg::X,
                                guarded_mapper_window,
                            );
                        } else {
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_d();
                            program.ld_b_a();
                            program.call(indexed_read_runtime(*base, *region));
                        }
                    }
                    (AddrExpr::AbsIndexedY(base), _) => {
                        if let Some(sms) = indexed_direct_base(*base, *region) {
                            emit_indexed_read_direct(program, sms, IdxReg::Y);
                        } else if *region == ir::MemRegion::PrgRom && *base >= 0xC000 {
                            emit_prg_high_indexed_direct(
                                program,
                                *base,
                                IdxReg::Y,
                                guarded_mapper_window,
                            );
                        } else {
                            program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                            program.ld_a_e_reg();
                            program.ld_b_a();
                            program.call(indexed_read_runtime(*base, *region));
                        }
                    }
                    (AddrExpr::ZpIndexedX(zp), _) => {
                        // 6502 zp,X wraps within zero page: (zp + X) & $FF.
                        // rt_read_indexed adds 16-bit, no wrap, so do the
                        // wrap inline.
                        program.ld_a_d();
                        program.add_a_imm(*zp);
                        program.ld_l_a();
                        program.ld_h_imm((NES_ZP_BASE >> 8) as u8);
                        program.ld_a_hl_ptr();
                    }
                    (AddrExpr::ZpIndexedY(zp), _) => {
                        program.ld_a_e_reg();
                        program.add_a_imm(*zp);
                        program.ld_l_a();
                        program.ld_h_imm((NES_ZP_BASE >> 8) as u8);
                        program.ld_a_hl_ptr();
                    }
                    (AddrExpr::IndirectY(zp), _) => {
                        emit_read_zp_ptr_y_inline(program, *zp);
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
                    emit_set_nz_inline(program);
                }
            }

            // LDX/LDY (any form) load shadow X or Y and update N/Z, but
            // must NOT modify A. Save AF, call rt_set_nz_a while A holds
            // the new register value, then restore only A so the new flags
            // remain live for a following branch.
            Op::LdxImm(v) => {
                if nz_live[op_idx] {
                    program.push_af();
                    program.ld_a_imm(*v);
                    program.ld_d_a_reg();
                    emit_set_nz_inline(program);
                    restore_a_keep_flags_after_push_af(program);
                } else {
                    program.ld_d_imm(*v);
                }
            }

            Op::LdxMem { addr, region } => {
                emit_ldxy_mem(program, addr, *region, IdxReg::X, guarded_mapper_window);
            }

            Op::LdyImm(v) => {
                if nz_live[op_idx] {
                    program.push_af();
                    program.ld_a_imm(*v);
                    program.ld_e_a_reg();
                    emit_set_nz_inline(program);
                    restore_a_keep_flags_after_push_af(program);
                } else {
                    program.ld_e_imm(*v);
                }
            }

            Op::LdyMem { addr, region } => {
                emit_ldxy_mem(program, addr, *region, IdxReg::Y, guarded_mapper_window);
            }

            // ------------------------------------------------------------------
            // Stores
            // ------------------------------------------------------------------
            Op::StaMem { addr, region } => {
                // PRG-ROM stores are mapper writes only when the indexed base
                // cannot wrap out of the ROM window. The lifter preserves these
                // forms so we can pass their exact effective address to runtime.
                if *region == MemRegion::PrgRom {
                    let (base, idx) = match addr {
                        AddrExpr::AbsIndexedX(base) if (0x8000..=0xFF00).contains(base) => {
                            (*base, IdxReg::X)
                        }
                        AddrExpr::AbsIndexedY(base) if (0x8000..=0xFF00).contains(base) => {
                            (*base, IdxReg::Y)
                        }
                        _ => {
                            return Err(LowerError::UnsupportedMapperStore {
                                pc: None,
                                reason: "PRG-ROM STA requires AbsIndexedX/AbsIndexedY with base $8000-$FF00"
                                    .to_string(),
                            });
                        }
                    };
                    program.ld_hl_imm(base);
                    program.ld_c_a();
                    idx.load_into_a(program);
                    program.add_a_l();
                    program.ld_l_a();
                    program.ld_a_h();
                    program.adc_a_imm0();
                    program.ld_h_a();
                    program.ld_a_c();
                    program.call(MAPPER_WRITE);
                } else {
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
                                emit_indexed_write_direct(program, sms, IdxReg::X);
                            } else {
                                program.ld_c_a(); // save value in C
                                program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                                program.ld_a_d();
                                program.ld_b_a();
                                program.ld_a_c();
                                program.call(WRITE_INDEXED);
                            }
                        }
                        (AddrExpr::AbsIndexedY(base), _) => {
                            if let Some(sms) = indexed_direct_base(*base, *region) {
                                emit_indexed_write_direct(program, sms, IdxReg::Y);
                            } else {
                                program.ld_c_a();
                                program.ld_hl_imm(indexed_base_to_sms(*base, *region));
                                program.ld_a_e_reg();
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
                            program.ld_a_d();
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
            }

            Op::StxMem { addr, region } => {
                if matches!(
                    *region,
                    MemRegion::Mapper | MemRegion::PrgRam | MemRegion::PrgRom
                ) {
                    return Err(LowerError::UnsupportedMapperStore {
                        pc: None,
                        reason: format!(
                            "STX to expansion space, PRG RAM, or PRG ROM is unsupported \
                             (addr {addr:?}, region {region:?})"
                        ),
                    });
                }
                // STX must NOT modify A. Bracket with push/pop AF, mirror
                // StaMem's addressing-mode coverage.
                emit_stxy_mem(program, addr, *region, IdxReg::X);
            }

            Op::StyMem { addr, region } => {
                if matches!(
                    *region,
                    MemRegion::Mapper | MemRegion::PrgRam | MemRegion::PrgRom
                ) {
                    return Err(LowerError::UnsupportedMapperStore {
                        pc: None,
                        reason: "STY to expansion space, PRG RAM, or PRG ROM is unsupported"
                            .to_string(),
                    });
                }
                emit_stxy_mem(program, addr, *region, IdxReg::Y);
            }

            Op::SaxMem { addr, region } => {
                // SAX: M := A & X. No flag changes. A and X both preserved.
                // Strategy: save A in scratch (C), AND A with shadow X, write
                // to memory using the same emit_stxy_mem path (which already
                // handles all addressing modes), then restore A.
                program.push_af();
                program.ld_c_a(); // C = original A
                program.ld_a_d();
                program.and_c(); // A = A & C = X & A
                // Stash the AND result in shadow X temporarily so we can
                // reuse emit_stxy_mem; restore X after.
                program.push_bc(); // save B,C; C still holds orig A
                program.ld_b_a(); // B = (A & X) value to write
                program.ld_a_d();
                program.push_af(); // save shadow X on Z80 stack
                program.ld_a_b();
                program.ld_d_a_reg(); // SHADOW_X = (A & X) value temporarily
                emit_stxy_mem(program, addr, *region, IdxReg::X);
                program.pop_af();
                program.ld_d_a_reg(); // restore real X
                program.pop_bc();
                program.ld_a_c(); // restore A
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Transfers
            // ------------------------------------------------------------------
            Op::Tax => {
                program.ld_d_a_reg();
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Tay => {
                program.ld_e_a_reg();
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Txa => {
                program.ld_a_d();
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Tya => {
                program.ld_a_e_reg();
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Tsx => {
                program.ld_a_abs(SHADOW_S);
                program.ld_d_a_reg();
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Txs => {
                // TXS sets S := X with no flag effect AND no change to A.
                program.push_af();
                program.ld_a_d();
                program.ld_abs_a(SHADOW_S);
                program.pop_af();
            }

            // ------------------------------------------------------------------
            // Stack
            // ------------------------------------------------------------------
            Op::Pha => {
                emit_push6502_inline(program);
            }

            Op::Pla => {
                emit_pop6502_inline(program);
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::Php => {
                // PHP pushes shadow P onto the emulated 6502 stack;
                // A must be preserved.
                program.push_af();
                program.ld_a_abs(SHADOW_P);
                emit_push6502_inline(program);
                program.pop_af();
            }

            Op::Plp => {
                // PLP pops a value from the emulated 6502 stack into
                // shadow P; A must be preserved.
                program.push_af();
                emit_pop6502_inline(program);
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
                if !flags_live_after(
                    ops_slice,
                    op_idx,
                    F_N | F_Z | F_C | F_V,
                    opts.routine_flag_reads,
                ) {
                    // H.8: result-only ADC — no consumer reads any flag
                    // before overwrite, so skip the whole shadow update.
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ld_a_c();
                    program.adc_a_imm(*v);
                } else if let Some(end) = fuse_direct_end[op_idx] {
                    // Native ADC: shadow C -> Z80 carry (RRCA on shadow P,
                    // A parked in C; LD doesn't touch flags), result in A.
                    // Scanner guarantees N/Z/C/V all dead after the run,
                    // so the stale shadow-C byte is never read.
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ld_a_c();
                    program.adc_a_imm(*v);
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        direct_cond_to_z80,
                    );
                } else {
                    program.ld_b_imm(*v);
                    emit_adc_flags_inline(program);
                }
            }

            Op::AdcMem { addr, region } => {
                if !flags_live_after(
                    ops_slice,
                    op_idx,
                    F_N | F_Z | F_C | F_V,
                    opts.routine_flag_reads,
                ) {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ld_a_c();
                    program.adc_a_b();
                } else if let Some(end) = fuse_direct_end[op_idx] {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ld_a_c();
                    program.adc_a_b();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        direct_cond_to_z80,
                    );
                } else {
                    emit_mem_to_b(program, addr, *region);
                    emit_adc_flags_inline(program);
                }
            }

            Op::SbcImm(v) => {
                if !flags_live_after(
                    ops_slice,
                    op_idx,
                    F_N | F_Z | F_C | F_V,
                    opts.routine_flag_reads,
                ) {
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ccf();
                    program.ld_a_c();
                    program.sbc_a_imm(*v);
                } else if let Some(end) = fuse_cmp_end[op_idx] {
                    // Native SBC: Z80 carry-in = !shadow C (CCF), and the
                    // 6502 carry-out = !borrow — cmp_cond_to_z80 handles
                    // the inverted branch polarity.
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ccf();
                    program.ld_a_c();
                    program.sbc_a_imm(*v);
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
                    emit_sbc_flags_inline(program);
                }
            }

            Op::SbcMem { addr, region } => {
                if !flags_live_after(
                    ops_slice,
                    op_idx,
                    F_N | F_Z | F_C | F_V,
                    opts.routine_flag_reads,
                ) {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ccf();
                    program.ld_a_c();
                    program.sbc_a_b();
                } else if let Some(end) = fuse_cmp_end[op_idx] {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_abs(SHADOW_P);
                    program.rrca();
                    program.ccf();
                    program.ld_a_c();
                    program.sbc_a_b();
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
                    emit_sbc_flags_inline(program);
                }
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
                    emit_set_nz_inline(program);
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
                if !flags_live_after(ops_slice, op_idx, F_N | F_Z | F_C, opts.routine_flag_reads) {
                    // H.8: CMP only produces flags; with no consumer it is
                    // a complete no-op.
                    let _ = v;
                } else if let Some(end) = fuse_cmp_end[op_idx] {
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
                    emit_cmp_flags_inline(program);
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
                    emit_cmp_flags_inline(program);
                }
            }

            Op::CpxImm(v) => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    // Native compare of shadow X; the 6502 accumulator in
                    // Z80 A survives in C (LD does not touch flags).
                    program.ld_c_a();
                    program.ld_a_d();
                    program.cp_imm(*v);
                    program.ld_a_c();
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
                    emit_cpxy_flags_inline(program, IdxReg::X);
                }
            }

            Op::CpxMem { addr, region } => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_d();
                    program.cp_b();
                    program.ld_a_c();
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
                    emit_cpxy_flags_inline(program, IdxReg::X);
                }
            }

            Op::CpyImm(v) => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    program.ld_c_a();
                    program.ld_a_e_reg();
                    program.cp_imm(*v);
                    program.ld_a_c();
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
                    emit_cpxy_flags_inline(program, IdxReg::Y);
                }
            }

            Op::CpyMem { addr, region } => {
                if let Some(end) = fuse_cmp_end[op_idx] {
                    emit_mem_to_b(program, addr, *region);
                    program.ld_c_a();
                    program.ld_a_e_reg();
                    program.cp_b();
                    program.ld_a_c();
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
                    emit_cpxy_flags_inline(program, IdxReg::Y);
                }
            }

            // ------------------------------------------------------------------
            // BIT
            // ------------------------------------------------------------------
            Op::BitMem { addr, region } => {
                emit_mem_to_b(program, addr, *region);
                emit_bit_mem_inline(program);
            }

            // ------------------------------------------------------------------
            // Shifts / rotates
            // ------------------------------------------------------------------
            Op::AslA => {
                if !flags_live_after(ops_slice, op_idx, F_N | F_Z | F_C, opts.routine_flag_reads) {
                    program.add_a_a();
                } else if let Some(end) = fuse_direct_end[op_idx] {
                    // Native shift: carry = shifted-out bit, same polarity
                    // as the 6502; Z native; N (=0 after LSR, bit7 after
                    // ASL) matches Z80 S.
                    program.add_a_a();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        direct_cond_to_z80,
                    );
                } else {
                    emit_asl_a_flags_inline(program);
                }
            }

            Op::AslMem { .. } => {
                emit_shift_mem(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    op,
                    fuse_direct_end[op_idx],
                    opts,
                );
            }

            Op::LsrA => {
                if !flags_live_after(ops_slice, op_idx, F_N | F_Z | F_C, opts.routine_flag_reads) {
                    program.srl_a();
                } else if let Some(end) = fuse_direct_end[op_idx] {
                    program.srl_a();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        direct_cond_to_z80,
                    );
                } else {
                    emit_lsr_a_flags_inline(program);
                }
            }

            Op::LsrMem { .. } => {
                emit_shift_mem(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    op,
                    fuse_direct_end[op_idx],
                    opts,
                );
            }

            Op::RolA => {
                emit_rol_a_flags_inline(program);
            }

            Op::RolMem { .. } => {
                emit_shift_mem(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    op,
                    fuse_direct_end[op_idx],
                    opts,
                );
            }

            Op::RorA => {
                emit_ror_a_flags_inline(program);
            }

            Op::RorMem { .. } => {
                emit_shift_mem(
                    program,
                    routine,
                    ops_slice,
                    op_idx,
                    op,
                    fuse_direct_end[op_idx],
                    opts,
                );
            }

            // ------------------------------------------------------------------
            // INC / DEC memory
            // ------------------------------------------------------------------
            Op::IncMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                if let Some(end) = fuse_nz_end[op_idx] {
                    // `inc (hl)` sets native S/Z: fuse the trailing branches.
                    program.inc_hl_ptr();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        nz_cond_to_z80,
                    );
                } else if !flags_live_after(ops_slice, op_idx, F_N | F_Z, opts.routine_flag_reads) {
                    // H.8: no flag consumer — the helper's only extra work
                    // is the shadow N/Z update.
                    program.inc_hl_ptr();
                } else {
                    program.call(INC_MEM);
                }
            }

            Op::DecMem { addr, region } => {
                emit_hl_for_rw_mem(program, addr, *region);
                if let Some(end) = fuse_nz_end[op_idx] {
                    program.dec_hl_ptr();
                    emit_fused_branches(
                        program,
                        routine,
                        ops_slice,
                        op_idx + 1,
                        end,
                        opts.emit_source_comments,
                        nz_cond_to_z80,
                    );
                } else if !flags_live_after(ops_slice, op_idx, F_N | F_Z, opts.routine_flag_reads) {
                    program.dec_hl_ptr();
                } else {
                    program.call(DEC_MEM);
                }
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
                IdxReg::X,
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
                IdxReg::Y,
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
                IdxReg::X,
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
                IdxReg::Y,
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
                    program.translated_tail_jmp(target);
                    program.label(&skip);
                }
            }

            // ------------------------------------------------------------------
            // Jumps / calls
            // ------------------------------------------------------------------
            Op::Jmp { target } => {
                // Translated-label JMPs can cross banks (e.g., JMP $8745
                // from InitScreen lands in IncSubtask which may be
                // pinned to a different code bank). Use the translated tail
                // gate so no native far-gate return frame is left behind.
                // Runtime helpers (rt_*) live in bank 0 and are reachable via
                // plain jp.
                // A tail JMP into a replaced routine behaves like the
                // replacement followed by the original's RTS: call the
                // hook, then return to this routine's caller.
                if let Some(profile) = opts.profile
                    && let Some(replacement) =
                        parse_label_addr(target).and_then(|a| profile.replacement_for(a))
                {
                    program.call(&replacement.runtime_label.clone());
                    if profile.native_calls() {
                        program.ret();
                    } else {
                        program.jp(TRANSLATED_RTS);
                    }
                    continue;
                }
                if target.starts_with("rt_") {
                    program.jp(target);
                } else if opts.profile.is_some_and(|p| p.native_calls()) {
                    program.native_tail_jmp(target);
                } else {
                    program.translated_tail_jmp(target);
                }
            }

            Op::ReturnEscape {
                target,
                return_addr,
                stack_bytes_already_consumed,
            } => {
                if opts.profile.is_some_and(|p| p.native_calls()) {
                    return Err(LowerError::UnsupportedMapperStore {
                        pc: None,
                        reason: "return-escape sites are incompatible with \
                                 stack_discipline = \"native\""
                            .to_string(),
                    });
                }
                // Consumed mode transferred ownership before its PLA pair.
                // Freed guest bytes may now contain a nested NMI's resume PC.
                if !stack_bytes_already_consumed {
                    program.ld_bc_imm(*return_addr);
                    program.call(TRANSLATED_RETURN_ESCAPE);
                }
                program.translated_tail_jmp(target);
            }

            Op::ReturnEscapeConsume { return_addr } => {
                if opts.profile.is_some_and(|p| p.native_calls()) {
                    return Err(LowerError::UnsupportedMapperStore {
                        pc: None,
                        reason: "return-escape consumption is incompatible with native calls"
                            .into(),
                    });
                }
                program.ld_bc_imm(*return_addr);
                program.call(TRANSLATED_RETURN_CONSUME);
            }

            Op::JmpIndirect { addr } => {
                program.ld_hl_imm(*addr);
                program.jp(INDIRECT_JMP);
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
                } else if opts.profile.is_some_and(|p| p.native_calls()) {
                    program.native_call(target);
                } else {
                    program.translated_call(target);
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
            // On Z80 this must be a software tail bank jump: do not leave a
            // native far-call helper return frame behind. The target's own
            // `RTS` unwinds via the translated-call continuation stack.
            Op::JumpEngineCall {
                targets,
                return_target,
                tail_indices,
                stack_return_bytes,
                target_entry_a,
            } => {
                let banked_mapper = opts.profile.is_some_and(|p| p.rom.mapper == 2);
                let native_calls = opts.profile.is_some_and(|p| p.native_calls());
                if targets.is_empty() {
                    program.comment("JumpEngineCall with empty targets — unreachable".to_string());
                    program.call(UNRESOLVED_JSR);
                } else {
                    // Each target is a translated label that may live in a
                    // different bank. We emit a chain of:
                    //   cp $i
                    //   jp nz, <skip_label>
                    //   same-section: jp target
                    //   cross-section: switch bank; jp target
                    //   skip_label:
                    // For the final entry, we drop the cp/jp_nz and just
                    // dispatch unconditionally.
                    let n = targets.len();
                    for (i, target) in targets.iter().enumerate() {
                        if i + 1 == n {
                            emit_jump_engine_target(
                                program,
                                i,
                                target,
                                return_target.as_deref(),
                                tail_indices,
                                *stack_return_bytes,
                                target_entry_a,
                                banked_mapper,
                                native_calls,
                                opts.profile,
                            );
                        } else {
                            let skip = program.fresh_label("je_skip");
                            program.cp_imm(i as u8);
                            program.jp_nz(&skip);
                            emit_jump_engine_target(
                                program,
                                i,
                                target,
                                return_target.as_deref(),
                                tail_indices,
                                *stack_return_bytes,
                                target_entry_a,
                                banked_mapper,
                                native_calls,
                                opts.profile,
                            );
                            program.label(&skip);
                        }
                    }
                }
            }

            Op::Rts => {
                if opts.profile.is_some_and(|p| p.native_calls()) {
                    program.ret();
                } else {
                    program.jp(TRANSLATED_RTS);
                }
            }

            Op::Rti => {
                // rt_rti pops P + PC from the emulated 6502 stack. The
                // sentinel PC pushed by the runtime's NMI bridge returns
                // natively; a game-written PC (BRK return, or a recovery
                // that rewrote the frame) dispatches through the banked
                // dispatcher. Tail transfer: control does not come back.
                program.jp(RTI);
            }

            // ------------------------------------------------------------------
            // Hardware ops
            // ------------------------------------------------------------------
            Op::PpuWrite { reg, value } => {
                emit_value_src_to_a(program, value);
                match *reg {
                    0 => {
                        let chr_ram = opts.profile.map(|p| p.rom.chr_kib == 0).unwrap_or(false);
                        let deferred = opts
                            .profile
                            .is_some_and(|p| p.translation.defer_sprite_registers);
                        emit_ppu_ctrl_write_inline(program, chr_ram, deferred);
                    }
                    1 => {
                        let chr_ram = opts.profile.map(|p| p.rom.chr_kib == 0).unwrap_or(false);
                        emit_ppu_mask_write_inline(program, chr_ram);
                    }
                    5 => emit_ppu_scroll_write_inline(program),
                    _ => emit_ppu_write_callless(
                        program,
                        *reg,
                        opts.profile.is_some_and(|p| p.native_calls()),
                    ),
                }
            }

            Op::PpuRead { reg } => {
                if *reg == 2 {
                    emit_ppu_status_read_inline(program);
                } else {
                    program.ld_b_imm(*reg);
                    program.call(PPU_READ);
                }
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
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
                    emit_set_nz_inline(program);
                }
            }

            Op::ControllerRead { port } => {
                program.ld_a_imm(*port as u8);
                program.call(CONTROLLER_READ);
                if nz_live[op_idx] {
                    emit_set_nz_inline(program);
                }
            }

            Op::RtsDispatch => {
                // Computed jump via the 6502 stack (PHA hi / PHA lo / RTS):
                // pop lo then hi from the shadow stack, target+1, and go
                // through the runtime banked dispatcher (fail-closed).
                // Tail helper entry avoids leaving this translated routine's
                // native helper return frame; runtime dispatches through the
                // tail-only banked dispatcher rather than rt_far_gate.
                program.jp("rt_rts_dispatch");
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

fn emit_jump_engine_target(
    program: &mut z80_emit::Program,
    index: usize,
    target: &str,
    return_target: Option<&str>,
    tail_indices: &[usize],
    stack_return_bytes: u8,
    target_entry_a: &[u8],
    banked_mapper: bool,
    native_calls: bool,
    profile: Option<&profile::Profile>,
) {
    if let Some(&entry_a) = target_entry_a.get(index) {
        program.ld_a_imm(entry_a);
    }
    if native_calls {
        // Native stack discipline: tail entries transfer directly (the
        // handler's RET unwinds to the outer caller through the native
        // stack); stack-aware entries native-call the handler, then consume
        // the emulated-stack return bytes the NES caller pushed before
        // resuming the statically resolved continuation. Equivalent to the
        // software frame's bit-6 consumption as long as the handler never
        // reads S — part of the `stack_discipline = "native"` assertion.
        //
        // Jump-engine targets may be profile display NAMES rather than
        // L_XXXX labels; resolve both forms so `[[replacement]]` hooks
        // intercept dispatched handlers too.
        let hook = profile
            .and_then(|p| {
                parse_label_addr(target)
                    .or_else(|| {
                        p.functions
                            .iter()
                            .find(|f| f.name == *target)
                            .map(|f| f.addr)
                    })
                    .and_then(|a| p.replacement_for(a))
            })
            .map(|r| r.runtime_label.clone());
        if let Some(continuation) = return_target
            && !tail_indices.contains(&index)
        {
            match &hook {
                Some(h) => program.call(h),
                None => program.native_call(target),
            }
            if stack_return_bytes > 0 {
                program.ld_c_a();
                program.ld_a_abs(sms_layout::SHADOW_S);
                program.add_a_imm(stack_return_bytes);
                program.ld_abs_a(sms_layout::SHADOW_S);
                program.ld_a_c();
            }
            program.native_tail_jmp(continuation);
        } else {
            match &hook {
                Some(h) => {
                    program.call(h);
                    program.ret();
                }
                None => program.native_tail_jmp(target),
            }
        }
        return;
    }
    // Unqualified `L_8xxx`-style labels on a banked mapper name an address in
    // the switchable window: they resolve against the live UxROM bank through
    // the runtime's bank-aware dispatcher instead of a translated label.
    let banked_window_addr = if banked_mapper {
        target
            .strip_prefix("L_")
            .filter(|hex| !hex.contains('_'))
            .and_then(|hex| u16::from_str_radix(hex, 16).ok())
            .filter(|addr| (0x8000..0xC000).contains(addr))
    } else {
        None
    };
    if let Some(continuation) = return_target
        && !tail_indices.contains(&index)
    {
        match banked_window_addr {
            Some(addr) => {
                program.translated_banked_call_with_continuation(
                    addr,
                    continuation,
                    stack_return_bytes,
                );
            }
            None => {
                program.translated_call_with_continuation(target, continuation, stack_return_bytes)
            }
        }
    } else if let Some(addr) = banked_window_addr {
        program.translated_banked_tail_dispatch(addr);
    } else {
        program.translated_tail_jmp(target);
    }
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
            PPU_WRITE_CONT,
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
            "rt_unresolved_jsr_flash",
            TRANSLATED_RTS,
            TRANSLATED_RETURN_ESCAPE,
            TRANSLATED_RETURN_CONSUME,
            BANKED_TAIL_DISPATCH,
            "rt_translated_call_gate",
            "rt_translated_tail_gate",
            "rt_read_prg_high",
            "rt_read_prg_high_indexed",
            BRK,
            RTI,
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

    fn ppu_ctrl_inline_asm(chr_ram: bool) -> String {
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        prog.section("test");
        emit_ppu_ctrl_write_inline(&mut prog, chr_ram, false);
        prog.finish().unwrap().asm
    }

    /// The inline STA $2000 path must latch the sticky "8x16 sprites in use"
    /// flag ($CA39) for CHR-RAM builds. Without it, the 8x8 base-sprite
    /// copy-through in the $2007 path keeps clobbering the 8x16 pair
    /// resolver's VRAM slots (Castlevania medusa/enemy sprites rendered with
    /// a corrupted per-row palette). Runtime _ppu_w_ctrl is bypassed by this
    /// inline path, so the latch must live here too.
    #[test]
    fn chr_ram_ppu_ctrl_latches_8x16_sprite_mode() {
        let asm = ppu_ctrl_inline_asm(true);
        assert!(
            asm.contains("ld ($ca39),a") || asm.contains("ld ($CA39),a"),
            "CHR-RAM inline PPUCTRL must set the $CA39 8x16 latch; got:\n{asm}"
        );
        // The latch must be conditioned on PPUCTRL bit 5 (8x16 sprite size).
        assert!(asm.contains("bit 5,a"), "latch must test PPUCTRL bit 5");
    }

    /// CHR-ROM builds (SMB) resolve sprites differently and must not carry the
    /// CHR-RAM-only latch, so the shared runtime byte stays untouched.
    #[test]
    fn chr_rom_ppu_ctrl_has_no_8x16_latch() {
        let asm = ppu_ctrl_inline_asm(false);
        assert!(
            !asm.contains("$ca39") && !asm.contains("$CA39"),
            "CHR-ROM inline PPUCTRL must not touch the $CA39 latch; got:\n{asm}"
        );
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
        // translated RTS tails through the software continuation helper.
        assert!(build.asm.contains("jp rt_translated_rts"));
        // call rt_set_nz_a present
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
        assert!(build.asm.contains("jp rt_translated_rts"));
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
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
    }

    #[test]
    fn fixed_high_reads_use_guarded_helpers_without_mapper_sequences() {
        for (op, helper) in [
            (
                Op::LdaMem {
                    addr: AddrExpr::Const(0xC000),
                    region: MemRegion::PrgRom,
                },
                "call rt_read_prg_high",
            ),
            (
                Op::LdaMem {
                    addr: AddrExpr::AbsIndexedX(0xFF01),
                    region: MemRegion::PrgRom,
                },
                "call rt_read_prg_high_indexed",
            ),
            (
                Op::AdcMem {
                    addr: AddrExpr::AbsIndexedY(0xC000),
                    region: MemRegion::PrgRom,
                },
                "call rt_read_prg_high_indexed",
            ),
        ] {
            let build = lower_and_finish(vec![op]);
            let helper_pos = build.asm.find(helper).expect("guarded helper call");
            assert!(
                !build.asm[..helper_pos].contains("push af"),
                "fixed-high operand fetch must not stack AF"
            );
            assert!(!build.asm.contains("ld ($FFFF),a"));
            assert!(!build.asm.contains("data_prg_high"));
            assert!(!build.asm.contains("rt_restore_prg_window"));
        }
    }

    #[test]
    fn nrom_profile_keeps_inline_fixed_high_reads() {
        let prof = profile::load_from_str(
            r#"
[rom]
name = "nrom-test"
mapper = 0
prg_kib = 32
chr_kib = 8
"#,
        )
        .unwrap();
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
            routine_flag_reads: None,
        };
        let routine = make_routine(
            "test_routine",
            vec![
                Op::LdaMem {
                    addr: AddrExpr::Const(0xC123),
                    region: MemRegion::PrgRom,
                },
                Op::LdaMem {
                    addr: AddrExpr::AbsIndexedX(0xC200),
                    region: MemRegion::PrgRom,
                },
            ],
        );
        let mut program = z80_emit::Program::new();
        define_runtime_stubs(&mut program);
        program.label("rt_restore_prg_window");
        program.ret();
        program.label("data_prg_high");
        program.ret();
        program.label("data_prg_low");
        program.ret();
        lower_routine(&mut program, &routine, &opts).unwrap();
        let build = program.finish().unwrap();

        assert_eq!(build.asm.matches("ld a,:data_prg_high").count(), 2);
        // The restore is inlined (two instructions); no helper call: $FFFC
        // is 0 at op boundaries and irq_handler restores the slot-2 bank.
        assert_eq!(build.asm.matches("ld a,:data_prg_low").count(), 2);
        assert!(!build.asm.contains("call rt_restore_prg_window"));
        assert!(!build.asm.contains("call rt_read_prg_high"));
        assert!(!build.asm.contains("call rt_read_prg_high_indexed"));
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
        assert!(
            !build.asm.contains("and $7D"),
            "unexpected inline NZ update"
        );
    }

    #[test]
    fn indexed_prg_rom_sta_passes_exact_mapper_address() {
        let build = lower_and_finish(vec![Op::StaMem {
            addr: AddrExpr::AbsIndexedY(0xFF00),
            region: MemRegion::PrgRom,
        }]);
        assert!(build.asm.contains("ld hl,$FF00"));
        assert!(build.asm.contains("add a,l"));
        assert!(build.asm.contains("adc a,$00"));
        assert!(build.asm.contains("call rt_mapper_write"));
    }

    #[test]
    fn constant_mapper_write_keeps_its_exact_address() {
        let build = lower_and_finish(vec![Op::MapperWrite {
            addr: 0x8000,
            value: ValueSrc::A,
        }]);
        assert!(build.asm.contains("ld hl,$8000"));
        assert!(build.asm.contains("call rt_mapper_write"));
    }

    #[test]
    fn invalid_prg_rom_sta_forms_fail_closed_without_8000_fallback() {
        for addr in [
            AddrExpr::Const(0x8000),
            AddrExpr::AbsIndexedX(0xFF01),
            AddrExpr::IndirectY(0x10),
        ] {
            let routine = make_routine(
                "test_routine",
                vec![Op::StaMem {
                    addr,
                    region: MemRegion::PrgRom,
                }],
            );
            let mut prog = z80_emit::Program::new();
            let err = lower_routine(&mut prog, &routine, &LowerOptions::default())
                .expect_err("invalid PRG-ROM STA must fail");
            assert!(matches!(err, LowerError::UnsupportedMapperStore { .. }));
        }
    }

    #[test]
    fn stx_sty_mapper_related_regions_fail_closed() {
        for (op, mnemonic) in [
            (
                Op::StxMem {
                    addr: AddrExpr::Const(0x4020),
                    region: MemRegion::Mapper,
                },
                "STX",
            ),
            (
                Op::StyMem {
                    addr: AddrExpr::Const(0x6000),
                    region: MemRegion::PrgRam,
                },
                "STY",
            ),
            (
                Op::StxMem {
                    addr: AddrExpr::Const(0x8000),
                    region: MemRegion::PrgRom,
                },
                "STX",
            ),
        ] {
            let routine = make_routine("test_routine", vec![op]);
            let mut prog = z80_emit::Program::new();
            let err = lower_routine(&mut prog, &routine, &LowerOptions::default())
                .expect_err("mapper-related STX/STY must fail");
            assert!(
                matches!(err, LowerError::UnsupportedMapperStore { reason, .. } if reason.starts_with(mnemonic))
            );
        }
    }

    #[test]
    fn structured_mapper_store_op_returns_structured_error() {
        let routine = make_routine(
            "test_routine",
            vec![Op::UnsupportedMapperStore {
                pc: 0x8123,
                opcode: 0x8D,
                mnemonic: "STA".to_string(),
                reason: "expansion space".to_string(),
            }],
        );
        let mut prog = z80_emit::Program::new();
        let err = lower_routine(&mut prog, &routine, &LowerOptions::default())
            .expect_err("mapper-store violation must fail");
        assert!(matches!(
            err,
            LowerError::UnsupportedMapperStore { pc: Some(0x8123), reason }
                if reason.contains("STA") && reason.contains("expansion space")
        ));
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
    fn ppu_write_reg0_inline() {
        let build = lower_and_finish(vec![Op::PpuWrite {
            reg: 0,
            value: ValueSrc::A,
        }]);
        assert!(build.asm.contains("inline STA $2000 PPUCTRL"));
        assert!(!build.asm.contains("call rt_ppu_write"));
        assert!(build.asm.contains("ld ($CB08),a"));
        assert!(build.asm.contains("out ($BF),a"));
    }

    #[test]
    fn ppu_write_reg1_inline() {
        let build = lower_and_finish(vec![Op::PpuWrite {
            reg: 1,
            value: ValueSrc::A,
        }]);
        assert!(build.asm.contains("inline STA $2001 PPUMASK"));
        assert!(!build.asm.contains("call rt_ppu_write"));
        assert!(build.asm.contains("ld ($CB09),a"));
        assert!(build.asm.contains("ld ($CB2D),a"));
    }

    #[test]
    fn ppu_scroll_uses_the_shared_2005_2006_latch() {
        let build = lower_and_finish(vec![Op::PpuWrite {
            reg: 5,
            value: ValueSrc::A,
        }]);
        assert!(build.asm.contains("ld a,($CB0E)"));
        assert!(build.asm.matches("ld ($CB0E),a").count() >= 2);
        assert!(build.asm.contains("ld ($CB0B),a"));
    }

    #[test]
    fn chr_ram_ppumask_inline_damps_full_screen_rebuilds() {
        let prof = profile::load_from_str(
            r#"
[rom]
name = "chr-ram-test"
mapper = 0
prg_kib = 32
chr_kib = 0
"#,
        )
        .unwrap();
        let routine = make_routine(
            "test_routine",
            vec![Op::PpuWrite {
                reg: 1,
                value: ValueSrc::A,
            }],
        );
        let mut program = z80_emit::Program::new();
        define_runtime_stubs(&mut program);
        program.org(0x0000);
        lower_routine(
            &mut program,
            &routine,
            &LowerOptions {
                profile: Some(&prof),
                ..LowerOptions::default()
            },
        )
        .unwrap();
        let build = program.finish().unwrap();

        assert!(build.asm.contains("ld a,($CA18)"));
        assert!(build.asm.contains("cp $40"));
        assert!(build.asm.contains("ld ($CA18),a"));
    }

    #[test]
    fn ppu_write_reg6_a() {
        let build = lower_and_finish(vec![Op::PpuWrite {
            reg: 6,
            value: ValueSrc::A,
        }]);
        // ld b,$06 = 06 06
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x06]));
        assert!(build.asm.contains("jp rt_ppu_write_cont"));
        assert!(!build.asm.contains("ld ($D3FC),hl"));
        assert!(!build.asm.contains("call rt_ppu_write"));
    }

    #[test]
    fn rol_a_inlines_without_runtime_call() {
        let build = lower_and_finish(vec![Op::RolA]);
        assert!(build.asm.contains("rl a"));
        assert!(build.asm.contains("ld ($CB03),a"));
        assert!(!build.asm.contains("call rt_rol_a"));
    }

    #[test]
    fn ror_a_inlines_without_runtime_call() {
        let build = lower_and_finish(vec![Op::RorA]);
        assert!(build.asm.contains("rr a"));
        assert!(build.asm.contains("ld ($CB03),a"));
        assert!(!build.asm.contains("call rt_ror_a"));
    }

    // -------------------------------------------------------------------
    // PpuRead
    // -------------------------------------------------------------------
    #[test]
    fn ppu_read_reg2() {
        let build = lower_and_finish(vec![Op::PpuRead { reg: 2 }]);
        assert!(build.asm.contains("ld a,i"));
        assert!(!build.asm.contains("call rt_ppu_read"));
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
    }

    #[test]
    fn ppu_read_reg7_still_calls_runtime() {
        let build = lower_and_finish(vec![Op::PpuRead { reg: 7 }]);
        // ld b,$07 = 06 07
        assert!(build.bytes.windows(2).any(|w| w == [0x06, 0x07]));
        assert!(build.asm.contains("call rt_ppu_read"));
    }

    // -------------------------------------------------------------------
    // OamDmaWrite
    // -------------------------------------------------------------------
    #[test]
    fn oam_dma_write_from_all_registers() {
        let build = lower_and_finish(vec![Op::OamDmaWrite { value: ValueSrc::A }]);
        assert!(build.asm.contains("call rt_oam_dma"));

        let build = lower_and_finish(vec![Op::OamDmaWrite { value: ValueSrc::X }]);
        assert!(build.asm.contains("ld a,d"));
        assert!(build.asm.contains("call rt_oam_dma"));

        let build = lower_and_finish(vec![Op::OamDmaWrite { value: ValueSrc::Y }]);
        assert!(build.asm.contains("ld a,e"));
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
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
    }

    #[test]
    fn lda_4016_indexed_x_inlines_controller_read() {
        let build = lower_and_finish(vec![Op::LdaMem {
            addr: AddrExpr::AbsIndexedX(0x4016),
            region: MemRegion::ApuIo,
        }]);
        assert!(build.asm.contains("inline LDA $4016,X controller read"));
        assert!(!build.asm.contains("call rt_controller_read_indexed_x"));
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
        // H.10: flag-live ADC emits the branchless body inline (adc a,b
        // + the F-map merge into shadow P), no helper call.
        assert!(build.asm.contains("adc a,b"));
        assert!(build.asm.contains("and $3C"));
    }

    #[test]
    fn adc_abs_indexed_y_direct_read_preserves_accumulator() {
        let build = lower_and_finish(vec![Op::AdcMem {
            // $07D7,Y sits at the top of the directly folded $C000-$C7FF RAM
            // window and takes the inline read. Historical guarantee (from the
            // rt_read_indexed era, which corrupted SMB's DigitsMathRoutine by
            // parking A in a helper-clobbered register): the accumulator must
            // survive the operand fetch, with the operand landing in B.
            addr: AddrExpr::AbsIndexedY(0x07D7),
            region: MemRegion::Ram,
        }]);

        assert!(
            !build.asm.contains("call rt_read_indexed"),
            "in-window indexed ADC should read inline, not via helper"
        );
        // Inline shape: A parked in C, EA built in HL from the resident Y (E),
        // operand into B, A restored from C before the add.
        assert!(build.asm.contains("ld c,a"), "A must be parked in C");
        assert!(build.asm.contains("ld b,(hl)"), "operand should land in B");
        assert!(build.asm.contains("adc a,b"), "operand should remain in B");
        let park = build.asm.find("ld c,a").unwrap();
        let restore = build.asm[park..]
            .find("ld a,c")
            .expect("A must be restored from C after the operand fetch");
        let between = &build.asm[park..park + restore];
        assert!(
            !between.contains("ld c,") || between.find("ld c,").unwrap() == 0,
            "nothing may clobber C while A is parked there"
        );
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
        // H.10: flag-live CMP emits the branchless body inline.
        assert!(build.asm.contains("sub b"));
        assert!(build.asm.contains("and $7C"));
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
        // H.10: the unfusable compare keeps the full shadow update,
        // now emitted inline rather than via rt_cmp_a.
        assert!(build.asm.contains("sub b"));
        assert!(build.asm.contains("and $7C"));
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
        assert!(!build.asm.contains("call rt_bit_mem"));
        assert!(build.asm.contains("ld c,a"));
        assert!(build.asm.contains("and b"));
        assert!(build.asm.contains("ld ($CB03),a"));
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

        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
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
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
        assert!(build.asm.contains("ld ($CB27),a"));
        assert!(build.asm.contains("ld a,($CB27)"));
        assert!(!build.asm.contains("pop bc"));
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
    fn jump_engine_call_uses_tail_bank_jump_without_ret() {
        let routine = make_routine(
            "test_routine",
            vec![Op::JumpEngineCall {
                targets: vec!["L_a".to_string(), "L_b".to_string()],
                return_target: None,
                tail_indices: Vec::new(),
                stack_return_bytes: 0,
                target_entry_a: Vec::new(),
            }],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        prog.label("L_a");
        prog.label("L_b");
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();
        let test_asm = build.asm.split(".org $0000").last().unwrap_or(&build.asm);

        assert!(test_asm.contains("jp L_a"));
        assert!(test_asm.contains("jp L_b"));
        assert!(!test_asm.contains("jp rt_far_gate_cont"));
        assert!(!test_asm.contains("call rt_far_call"));
        assert!(!test_asm.contains("ret"));
        assert!(!test_asm.contains("call rt_far_jmp"));
    }

    #[test]
    fn mapper_jump_engine_uses_live_bank_for_unqualified_window_target() {
        let prof = profile::load_from_str(
            r#"
[rom]
name = "uxrom-test"
mapper = 2
prg_kib = 128
chr_kib = 0
"#,
        )
        .unwrap();
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
            routine_flag_reads: None,
        };
        let routine = make_routine(
            "test_routine",
            vec![Op::JumpEngineCall {
                targets: vec!["L_8123".to_string()],
                return_target: None,
                tail_indices: Vec::new(),
                stack_return_bytes: 0,
                target_entry_a: vec![0x81],
            }],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &opts).unwrap();
        let build = prog.finish().unwrap();

        assert!(build.asm.contains("ld a,$81"));
        assert!(build.asm.contains("ld bc,$8123"));
        assert!(build.asm.contains("jp rt_banked_tail_dispatch"));
        assert!(!build.asm.contains("jp rt_translated_tail_gate"));
    }

    #[test]
    fn stack_aware_jump_engine_emits_continuation_and_tail_exception() {
        let routine = make_routine(
            "test_routine",
            vec![Op::JumpEngineCall {
                targets: vec!["L_returning".to_string(), "L_tail".to_string()],
                return_target: Some("L_cont".to_string()),
                tail_indices: vec![1],
                stack_return_bytes: 2,
                target_entry_a: vec![0xAA, 0xBB],
            }],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        for label in ["L_returning", "L_tail", "L_cont"] {
            prog.label(label);
            prog.ret();
        }
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        let build = prog.finish().unwrap();

        assert!(build.asm.contains("ld a,$AA"));
        assert!(build.asm.contains("ld a,$BB"));
        assert!(build.asm.contains("ld bc,L_cont"));
        assert!(build.asm.contains("or $40"));
        assert!(build.asm.contains("jp L_returning"));
        assert!(build.asm.contains("jp L_tail"));
    }

    #[test]
    fn stack_aware_jump_engine_uses_banked_dispatch_for_window_target() {
        let prof = profile::load_from_str(
            r#"
[rom]
name = "uxrom-test"
mapper = 2
prg_kib = 128
chr_kib = 0
"#,
        )
        .unwrap();
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
            routine_flag_reads: None,
        };
        let routine = make_routine(
            "test_routine",
            vec![Op::JumpEngineCall {
                targets: vec!["L_8123".to_string()],
                return_target: Some("L_cont".to_string()),
                tail_indices: Vec::new(),
                stack_return_bytes: 2,
                target_entry_a: vec![0x81],
            }],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        prog.label("L_cont");
        prog.ret();
        lower_routine(&mut prog, &routine, &opts).unwrap();
        let build = prog.finish().unwrap();

        // Continuation frame (bit 6 = consume 2 emulated-stack bytes on the
        // handler's RTS) followed by the live-bank literal dispatch.
        assert!(build.asm.contains("ld a,$81"));
        assert!(build.asm.contains("ld bc,L_cont"));
        assert!(build.asm.contains("or $40"));
        assert!(build.asm.contains("ld bc,$8123"));
        assert!(build.asm.contains("jp rt_banked_tail_dispatch"));
        assert!(!build.asm.contains("jp rt_translated_tail_gate"));
    }

    // -------------------------------------------------------------------
    // Tax
    // -------------------------------------------------------------------
    #[test]
    fn tax_stores_to_shadow_x() {
        let build = lower_and_finish(vec![Op::Tax]);
        // ld ($CB00),a = 32 00 CB
        // Phase R: X lives in D — `ld d,a`.
        assert!(build.bytes.contains(&0x57));
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
    }

    // -------------------------------------------------------------------
    // Inx
    // -------------------------------------------------------------------
    #[test]
    fn inx_sequence() {
        let build = lower_and_finish(vec![Op::Inx]);
        // ld hl,$CB00 = 21 00 CB
        // Phase R: INX is `inc d` on the resident register.
        assert!(build.bytes.contains(&0x14));
        // flags live across the routine end -> still persists to shadow P
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
    }

    // -------------------------------------------------------------------
    // Jsr with label (no profile)
    // -------------------------------------------------------------------
    #[test]
    fn jsr_emits_software_translated_call() {
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
        assert!(build.asm.contains("ld bc,_tr_cont_"));
        assert!(build.asm.contains("ld bc,L_8200"));
        assert!(build.asm.contains("ld a,:L_8200"));
        assert!(build.asm.contains("jp rt_translated_call_gate"));
        assert!(!build.asm.contains("call L_8200"));
        assert!(!build.asm.contains("jp rt_far_gate_cont"));
        assert!(!build.asm.contains("$cb15"));
        assert!(!build.asm.contains("$fffe"));
        assert!(build.asm.contains("ld a,$E4"));
    }

    #[test]
    fn translated_jmp_uses_tail_gate_not_far_jmp() {
        let routine = make_routine(
            "test_routine",
            vec![Op::Jmp {
                target: "L_9000".to_string(),
            }],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        prog.label("L_9000");
        let build = prog.finish().unwrap();
        assert!(build.asm.contains("jp rt_translated_tail_gate"));
        assert!(!build.asm.contains("jp rt_far_gate"));
        assert!(!build.asm.contains("call rt_far_call"));
        assert!(!build.asm.contains("$cb15"));
        assert!(!build.asm.contains("$fffe"));
    }

    #[test]
    fn native_cross_branch_uses_tail_gate_not_far_jmp() {
        let routine = make_routine(
            "test_routine",
            vec![
                Op::LdaImm(0x80),
                Op::BranchIf {
                    cond: Cond::Negative,
                    target: "L_9000".to_string(),
                },
                Op::Rts,
            ],
        );
        let mut prog = z80_emit::Program::new();
        define_runtime_stubs(&mut prog);
        prog.org(0x0000);
        lower_routine(&mut prog, &routine, &LowerOptions::default()).unwrap();
        prog.label("L_9000");
        let build = prog.finish().unwrap();
        assert!(build.asm.contains("jp rt_translated_tail_gate"));
        assert!(!build.asm.contains("jp rt_far_gate"));
        assert!(!build.asm.contains("$cb15"));
        assert!(!build.asm.contains("$fffe"));
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

    #[test]
    fn runtime_helper_jsr_still_native_call() {
        let build = lower_and_finish(vec![Op::Jsr {
            target: "rt_push6502".to_string(),
        }]);
        assert!(build.asm.contains("call rt_push6502"));
        assert!(!build.asm.contains("ld bc,_tr_cont_"));
    }

    #[test]
    fn rti_tail_transfers_to_rt_rti() {
        // RTI pops the 3-byte interrupt frame in rt_rti: sentinel PC =
        // native ret (NMI bridge), game-written PC = banked dispatch
        // (BRK return / recovery rewrite). Not an RTS, not inline.
        let build = lower_and_finish(vec![Op::Rti]);
        assert!(build.asm.contains("jp rt_rti"));
        assert!(!build.asm.contains("jp rt_translated_rts"));
    }

    #[test]
    fn return_escape_pops_translated_frame_before_tail_jump() {
        let build = lower_and_finish(vec![
            Op::ReturnEscape {
                target: "escape_target".to_string(),
                return_addr: 0xEA79,
                stack_bytes_already_consumed: false,
            },
            Op::Label("escape_target".to_string()),
        ]);
        assert!(build.asm.contains("ld bc,$EA79"));
        assert!(build.asm.contains("call rt_translated_return_escape"));
        assert!(build.asm.contains("ld bc,escape_target"));
        assert!(build.asm.contains("jp rt_translated_tail_gate"));
    }

    #[test]
    fn already_consumed_return_escape_uses_guarded_discard_helper() {
        let build = lower_and_finish(vec![
            Op::ReturnEscapeConsume {
                return_addr: 0x8FFF,
            },
            Op::Pla,
            Op::Pla,
            Op::ReturnEscape {
                target: "escape_target".into(),
                return_addr: 0x8FFF,
                stack_bytes_already_consumed: true,
            },
            Op::Label("escape_target".into()),
        ]);
        assert!(build.asm.contains("ld bc,$8FFF"));
        assert!(build.asm.contains("call rt_translated_return_consume"));
        assert!(!build.asm.contains("call rt_translated_return_escape\n"));
        assert_eq!(
            build
                .asm
                .matches("call rt_translated_return_consume")
                .count(),
            1
        );
    }

    // -------------------------------------------------------------------
    // JmpIndirect
    // -------------------------------------------------------------------
    #[test]
    fn jmp_indirect_3000() {
        let build = lower_and_finish(vec![Op::JmpIndirect { addr: 0x3000 }]);
        // ld hl,$3000 = 21 00 30
        assert!(build.bytes.windows(3).any(|w| w == [0x21, 0x00, 0x30]));
        assert!(build.asm.contains("jp rt_indirect_jmp"));
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
        // Phase R: X lives in D — `ld d,a`.
        assert!(build.bytes.contains(&0x57));
        // H.1c: shadow-NZ update is inlined (table at $3E00).
        assert!(build.asm.contains("and $7D"), "inline NZ sequence missing");
        assert!(build.asm.contains("or (hl)"), "inline NZ sequence missing");
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
