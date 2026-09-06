//! Z80 implementations of the runtime helpers, emitted via `z80_emit`.
//!
//! These are functionally equivalent to the hand-written assembly in
//! `runtime/*.s`. The harness emits these alongside the lowered routine
//! so it runs in isolation under `z80_emu` — no link to `runtime/` is
//! required (and indeed cannot easily be done from a unit test).
//!
//! Both implementations should agree behaviorally. When they don't, one
//! of them is wrong and the diff harness will catch it.

use lower::sms_layout::{NES_RAM_BASE, NES_STACK_BASE, SHADOW_P, SHADOW_S, SHADOW_X, SHADOW_Y};
use z80_emit::Program;

// Helpers were originally placed at a fixed offset; the validator now
// queries `Program::current_addr()` after emitting them, so no compile-
// time size constant is needed.

/// Required helper names — the harness expects each to be defined.
pub const REQUIRED_HELPERS: &[&str] = &[
    "rt_set_nz_a",
    "rt_adc_a",
    "rt_sbc_a",
    "rt_cmp_a",
    "rt_cpx_a",
    "rt_cpy_a",
    "rt_push6502",
    "rt_pop6502",
    "rt_ppu_write",
    "rt_ppu_read",
    "rt_oam_dma",
    "rt_apu_write",
    "rt_apu_read",
    "rt_controller_read",
    "rt_mapper_write",
    "rt_indirect_jmp",
    "rt_translated_rts",
    "rt_translated_return_escape",
    "rt_translated_return_consume",
    "rt_translated_call_materialize",
    "rt_translated_call_gate",
    "rt_translated_tail_gate",
    "rt_unresolved_jsr",
    "rt_brk",
    "rt_rti",
    "rt_asl_a",
    "rt_asl_mem",
    "rt_lsr_a",
    "rt_lsr_mem",
    "rt_rol_a",
    "rt_rol_mem",
    "rt_ror_a",
    "rt_ror_mem",
    "rt_bit_mem",
    "rt_inc_mem",
    "rt_dec_mem",
    "rt_read_indexed",
    "rt_read_prg_high",
    "rt_read_prg_high_indexed",
    "rt_write_indexed",
    "rt_read_zp_ptr_y",
    "rt_write_zp_ptr_y",
];

/// Emit all runtime helpers into `program`. The helpers must come before
/// any code that calls them so the label patches resolve cleanly.
pub fn emit_runtime_helpers(p: &mut Program) {
    p.comment("─── validation runtime helpers (Rust-emitted) ───");
    emit_set_nz_a(p);
    emit_adc_a(p);
    emit_sbc_a(p);
    emit_cmp_a(p);
    emit_cpx_a(p);
    emit_cpy_a(p);
    emit_push6502(p);
    emit_pop6502(p);
    emit_asl_a(p);
    emit_lsr_a(p);
    emit_rol_a(p);
    emit_ror_a(p);
    emit_asl_mem(p);
    emit_lsr_mem(p);
    emit_rol_mem(p);
    emit_ror_mem(p);
    emit_inc_mem(p);
    emit_dec_mem(p);
    emit_bit_mem(p);
    emit_read_indexed(p);
    emit_read_prg_high(p);
    emit_read_prg_high_indexed(p);
    emit_write_indexed(p);
    emit_read_zp_ptr_y(p);
    emit_write_zp_ptr_y(p);
    emit_translated_rts(p);
    emit_translated_return_escape(p);
    emit_halt_stub(p, "rt_translated_call_gate");
    emit_halt_stub(p, "rt_translated_tail_gate");
    // Hardware helpers — RET stubs. Routines that touch hardware are
    // skipped by `classify_routine` and never reach these.
    emit_ret_stub(p, "rt_ppu_write");
    emit_ret_stub(p, "rt_ppu_read");
    emit_ret_stub(p, "rt_oam_dma");
    emit_ret_stub(p, "rt_apu_write");
    emit_ret_stub(p, "rt_apu_read");
    emit_ret_stub(p, "rt_controller_read");
    emit_ret_stub(p, "rt_mapper_write");
    // Trap helpers — HALT so the harness notices.
    emit_halt_stub(p, "rt_indirect_jmp");
    emit_halt_stub(p, "rt_unresolved_jsr");
    emit_halt_stub(p, "rt_brk");
    // rt_rti pops a 3-byte interrupt frame and dispatches; no interrupt
    // frame exists in the single-routine harness, so reaching it is a
    // mis-execution — HALT like the other trap helpers.
    emit_halt_stub(p, "rt_rti");
    p.comment("─── end runtime helpers ───");
}

fn emit_translated_rts(p: &mut Program) {
    p.label("rt_translated_rts");
    p.ld_c_a();
    p.ld_hl_abs(0xCB76);
    p.ld_abs_hl(0xCB73);
    p.ld_a_h();
    p.cp_imm(0xD3);
    p.jr_z("_validation_tr_rts_check_seg0");
    p.cp_imm(0xD5);
    p.jr_z("_validation_tr_rts_check_seg1");
    p.cp_imm(0xD6);
    p.jr_nz("_validation_tr_rts_underflow");
    p.ld_a_l();
    p.or_a();
    p.jr_nz("_validation_tr_rts_underflow");
    p.ld_hl_imm(0xD5FC);
    p.jr("_validation_tr_rts_pop_frame");
    p.label("_validation_tr_rts_check_seg0");
    p.ld_a_l();
    p.cp_imm(0x01);
    p.jr_c("_validation_tr_rts_underflow");
    p.cp_imm(0xF9);
    p.jr_nc("_validation_tr_rts_underflow");
    p.and_imm(0x03);
    p.jr_nz("_validation_tr_rts_underflow");
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.jr("_validation_tr_rts_pop_frame");
    p.label("_validation_tr_rts_check_seg1");
    p.ld_a_l();
    p.or_a();
    p.jr_z("_validation_tr_rts_bridge");
    p.and_imm(0x03);
    p.jr_nz("_validation_tr_rts_underflow");
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.jr("_validation_tr_rts_pop_frame");
    p.label("_validation_tr_rts_bridge");
    p.ld_hl_imm(0xD3F8);
    p.label("_validation_tr_rts_pop_frame");
    p.ld_hl_ptr_c();
    p.inc_hl();
    p.ld_c_hl_ptr();
    p.inc_hl();
    p.ld_b_hl_ptr();
    p.inc_hl();
    p.ld_a_hl_ptr();
    p.bit_a(6);
    p.jr_z("_validation_tr_rts_bank_ready");
    p.ld_a_abs(SHADOW_S);
    p.inc_a();
    p.inc_a();
    p.ld_abs_a(SHADOW_S);
    p.ld_a_hl_ptr();
    p.and_imm(0x1F);
    p.label("_validation_tr_rts_bank_ready");
    p.ld_abs_a(0xCB14);
    p.ld_abs_a(0xFFFE);
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.ld_a_hl_ptr();
    p.ld_abs_hl(0xCB76);
    p.ld_h_b();
    p.ld_l_c();
    p.jp_hl();
    p.label("_validation_tr_rts_underflow");
    p.ld_a_imm(0xE3);
    p.ld_abs_a(0xCB1D);
    p.halt();
}

fn emit_translated_return_escape(p: &mut Program) {
    p.label("rt_translated_return_consume");
    p.push_af();
    p.ld_a_i();
    p.di();
    p.jp_po("_validation_tr_consumed_was_disabled");
    p.ld_a_imm(0x81);
    p.jr("_validation_tr_consumed_save_mode");
    p.label("_validation_tr_consumed_was_disabled");
    p.ld_a_imm(0x80);
    p.label("_validation_tr_consumed_save_mode");
    p.ld_abs_a(0xD471);
    p.jr("_validation_tr_escape_find_frame");
    p.label("rt_translated_call_materialize");
    p.push_af();
    p.ld_a_i();
    p.di();
    p.jp_po("_validation_tr_materialize_disabled");
    p.ld_a_imm(1);
    p.jr("_validation_tr_materialize_mode");
    p.label("_validation_tr_materialize_disabled");
    p.xor_a();
    p.label("_validation_tr_materialize_mode");
    p.ld_abs_a(0xD471);
    p.jp("_validation_tr_escape_materialize");
    p.label("rt_translated_return_escape");
    p.push_af();
    p.ld_a_i();
    p.di();
    p.jp_po("_validation_tr_escape_was_disabled");
    p.ld_a_imm(1);
    p.ld_abs_a(0xD471);
    p.jr("_validation_tr_escape_find_frame");
    p.label("_validation_tr_escape_was_disabled");
    p.xor_a();
    p.ld_abs_a(0xD471);
    p.label("_validation_tr_escape_find_frame");
    p.ld_hl_abs(0xCB76);
    p.ld_abs_hl(0xCB73);
    p.ld_a_h();
    p.cp_imm(0xD3);
    p.jr_z("_validation_tr_escape_check_seg0");
    p.cp_imm(0xD5);
    p.jr_z("_validation_tr_escape_check_seg1");
    p.cp_imm(0xD6);
    p.jr_nz("_validation_tr_escape_underflow");
    p.ld_a_l();
    p.or_a();
    p.jr_nz("_validation_tr_escape_underflow");
    p.ld_hl_imm(0xD5FC);
    p.jr("_validation_tr_escape_pop_frame");
    p.label("_validation_tr_escape_check_seg0");
    p.ld_a_l();
    p.cp_imm(0x01);
    p.jr_c("_validation_tr_escape_underflow");
    p.cp_imm(0xF9);
    p.jr_nc("_validation_tr_escape_underflow");
    p.and_imm(0x03);
    p.jr_nz("_validation_tr_escape_underflow");
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.jr("_validation_tr_escape_pop_frame");
    p.label("_validation_tr_escape_check_seg1");
    p.ld_a_l();
    p.or_a();
    p.jr_z("_validation_tr_escape_bridge");
    p.and_imm(0x03);
    p.jr_nz("_validation_tr_escape_underflow");
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.jr("_validation_tr_escape_pop_frame");
    p.label("_validation_tr_escape_bridge");
    p.ld_hl_imm(0xD3F8);
    p.label("_validation_tr_escape_pop_frame");
    p.ld_a_abs(0xD471);
    p.bit_a(7);
    p.jp_nz("_validation_tr_escape_discard_consumed");
    p.ld_abs_hl(0xCB76);
    p.label("_validation_tr_escape_materialize");
    p.ld_a_abs(SHADOW_S);
    p.ld_l_a();
    p.ld_h_imm(0xC1);
    p.ld_hl_ptr_b();
    p.dec_l();
    p.ld_hl_ptr_c();
    p.sub_imm(2);
    p.ld_abs_a(SHADOW_S);
    p.label("_validation_tr_escape_restore_iff");
    p.ld_a_abs(0xD471);
    p.and_imm(1);
    p.or_a();
    p.jr_z("_validation_tr_escape_return_disabled");
    p.pop_af();
    p.ei();
    p.ret();
    p.label("_validation_tr_escape_return_disabled");
    p.pop_af();
    p.ret();
    p.label("_validation_tr_escape_underflow");
    p.ld_a_imm(0xE5);
    p.ld_abs_a(0xCB1D);
    p.halt();
    p.label("_validation_tr_escape_discard_consumed");
    p.inc_hl();
    p.inc_hl();
    p.inc_hl();
    p.ld_a_hl_ptr();
    p.bit_a(6);
    p.jp_z("_validation_tr_escape_underflow");
    p.dec_hl();
    p.dec_hl();
    p.dec_hl();
    p.push_hl();
    p.ld_a_abs(SHADOW_S);
    p.ld_l_a();
    p.inc_l();
    p.inc_l();
    p.ld_h_imm(0xC1);
    p.ld_a_hl_ptr();
    p.cp_b();
    p.jp_nz("_validation_tr_escape_underflow");
    p.dec_l();
    p.ld_a_hl_ptr();
    p.cp_c();
    p.jp_nz("_validation_tr_escape_underflow");
    p.pop_hl();
    p.ld_abs_hl(0xCB76);
    p.jp("_validation_tr_escape_restore_iff");
}

fn emit_ret_stub(p: &mut Program, name: &str) {
    p.label(name);
    p.ret();
}

fn emit_halt_stub(p: &mut Program, name: &str) {
    p.label(name);
    p.halt();
}

// ─── Flag helpers ────────────────────────────────────────────────────────────

/// Update shadow N and Z bits from A. A preserved.
///
/// Strategy: save A; load shadow P; clear N & Z bits; if A==0 set Z; if
/// bit 7 of A set, set N; write back; restore A.
fn emit_set_nz_a(p: &mut Program) {
    p.label("rt_set_nz_a");
    p.push_af(); // save caller flags + A
    // C := A (save A in C since we need A for shadow load)
    p.ld_c_a();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1101); // clear bits 7 (N) and 1 (Z)
    p.ld_b_a(); // B = new P (with N,Z cleared)
    // Test A == 0
    p.ld_a_c();
    p.or_a();
    p.jr_nz("_set_nz_a_nz");
    p.ld_a_b();
    p.or_imm(0b0000_0010); // set Z
    p.ld_b_a();
    p.label("_set_nz_a_nz");
    // Test bit 7 of A
    p.ld_a_c();
    p.bit_a(7);
    p.jr_z("_set_nz_a_done");
    p.ld_a_b();
    p.or_imm(0b1000_0000); // set N
    p.ld_b_a();
    p.label("_set_nz_a_done");
    p.ld_a_b();
    p.ld_abs_a(SHADOW_P);
    p.ld_a_c(); // restore A
    p.pop_af(); // restore caller flags+A
    p.ret();
}

/// 6502 ADC: A = A + M + C; sets N,V,Z,C. (Decimal mode ignored.)
/// Entry: A = A, B = M, shadow P bit 0 = C.
fn emit_adc_a(p: &mut Program) {
    p.label("rt_adc_a");
    // Save inputs.
    p.push_de(); // free DE for scratch
    p.ld_d_a(); // D = A_orig
    p.ld_e_a(); // E = A_orig (used twice for V calculation)

    // 16-bit sum: A + M + C → store in scratch.
    // Compute A + B first using Z80 native add (carry out is bit 8).
    // Then add C if shadow C is set, propagating carry.
    // Simpler: use Z80 ADC after setting native carry from shadow C.
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0000_0001); // isolate shadow C
    // If shadow C == 0, clear native carry (and a clears C). If shadow C == 1, set native carry.
    p.jr_z("_adc_clear_c");
    p.scf(); // set carry
    p.jr("_adc_have_c");
    p.label("_adc_clear_c");
    p.and_a(); // clears carry
    p.label("_adc_have_c");
    // Now native carry = shadow C. Perform ADC A,B.
    p.ld_a_d(); // restore A_orig
    p.adc_a_b();

    // Now A = result, native flags reflect the add.
    // We need: result→A (done), new C (native C), new V (native PV after add),
    //          new N (bit 7 of result), new Z (result == 0).
    // Compose new shadow P.
    p.push_af(); // save result + native flags

    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0011_1100); // clear N, V, Z, C. Keep I, D, B, U.
    p.ld_c_a(); // C = new P so far

    p.pop_af(); // restore native flags + result
    p.push_af();

    // C (carry)
    p.jr_nc("_adc_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_adc_no_c");

    p.pop_af();
    p.push_af();

    // V (overflow) — native PV
    p.jp_po("_adc_no_v");
    p.ld_a_c();
    p.or_imm(0b0100_0000);
    p.ld_c_a();
    p.label("_adc_no_v");

    p.pop_af();
    p.push_af();

    // N (bit 7 of A)
    p.bit_a(7);
    p.jr_z("_adc_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_adc_no_n");

    p.pop_af();
    p.push_af();
    // Z (A == 0) — must save A first because the set-Z path clobbers A.
    p.or_a();
    p.jr_nz("_adc_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_adc_no_z");

    // Restore result; write shadow P; preserve result through the write.
    p.pop_af(); // A = result
    p.push_af(); // save result+flags
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af(); // A = result
    p.pop_de();
    p.ret();
}

/// 6502 SBC: A = A - M - (1 - C); sets N,V,Z,C.
///
/// Identity used: SBC(A, M) ≡ ADC(A, M XOR 0xFF). Both 6502 and Z80
/// share this identity (the carry direction works out: 6502 SBC borrows
/// when C=0, Z80 ADC carries in when C=1; XOR-flip makes them line up).
fn emit_sbc_a(p: &mut Program) {
    p.label("rt_sbc_a");
    p.push_de();
    p.ld_d_a(); // D = original A
    // Invert B in place.
    p.ld_a_b();
    p.xor_imm(0xFF);
    p.ld_b_a();
    // Native carry := shadow C.
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0000_0001);
    p.jr_z("_sbc_clear_c");
    p.scf();
    p.jr("_sbc_have_c");
    p.label("_sbc_clear_c");
    p.and_a();
    p.label("_sbc_have_c");
    // A := D + B + C (== orig_A + ~M + C == orig_A - M - 1 + C == orig_A - M - (1-C))
    p.ld_a_d();
    p.adc_a_b();
    // Now A = result, native flags reflect the ADC.
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0011_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    // C from native carry (6502 SBC C=1 means no borrow, same as Z80 ADC C out).
    p.jr_nc("_sbc_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_sbc_no_c");
    p.pop_af();
    p.push_af();
    // V from native PV.
    p.jp_po("_sbc_no_v");
    p.ld_a_c();
    p.or_imm(0b0100_0000);
    p.ld_c_a();
    p.label("_sbc_no_v");
    p.pop_af();
    p.push_af();
    // N from bit 7 of result.
    p.bit_a(7);
    p.jr_z("_sbc_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_sbc_no_n");
    p.pop_af();
    p.push_af();
    // Z from result == 0 — save A across the set-Z path.
    p.or_a();
    p.jr_nz("_sbc_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_sbc_no_z");
    p.pop_af();
    p.push_af();
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
    p.pop_de();
    p.ret();
}

/// 6502 CMP: shadow_C = A >= M; shadow_Z = A == M; shadow_N = bit7(A - M). A unchanged.
fn emit_cmp_a(p: &mut Program) {
    p.label("rt_cmp_a");
    emit_compare_inner(p, "cmp_a");
    p.ret();
}

fn emit_cpx_a(p: &mut Program) {
    p.label("rt_cpx_a");
    // CPX compares (shadow_X) with B. We need to preserve caller's A.
    p.push_af();
    p.ld_a_abs(SHADOW_X);
    emit_compare_inner(p, "cpx_a");
    p.pop_af();
    p.ret();
}

fn emit_cpy_a(p: &mut Program) {
    p.label("rt_cpy_a");
    p.push_af();
    p.ld_a_e();
    emit_compare_inner(p, "cpy_a");
    p.pop_af();
    p.ret();
}

/// Compare A vs B, write shadow flags. A unchanged. Clobbers BC/F.
///
/// `prefix` makes local labels unique per call site so emitting this
/// helper into the same `Program` multiple times doesn't duplicate labels.
fn emit_compare_inner(p: &mut Program, prefix: &str) {
    let lbl = |s: &str| format!("__{prefix}_{s}");
    p.push_af();
    p.cp_b();
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();

    p.pop_af();
    p.push_af();
    // 6502 C := A >= B  ==  NOT(Z80 carry after CP).
    p.jr_c(&lbl("no_c"));
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label(&lbl("no_c"));

    p.pop_af();
    p.push_af();
    p.jr_nz(&lbl("no_z"));
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label(&lbl("no_z"));

    p.pop_af();
    p.jp_p(&lbl("no_n"));
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label(&lbl("no_n"));

    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
}

// ─── Stack (6502 emulated stack at $C100-$C1FF) ──────────────────────────────

fn emit_push6502(p: &mut Program) {
    p.label("rt_push6502");
    // Compute address = $C100 + SHADOW_S; store A there; SHADOW_S -= 1.
    p.push_hl();
    p.push_bc();
    p.ld_b_a(); // B = value to push
    p.ld_a_abs(SHADOW_S);
    p.ld_l_a();
    p.ld_h_imm((NES_STACK_BASE >> 8) as u8);
    p.ld_a_b();
    p.ld_hl_ptr_a();
    p.ld_a_abs(SHADOW_S);
    p.dec_a();
    p.ld_abs_a(SHADOW_S);
    p.ld_a_b(); // restore A
    p.pop_bc();
    p.pop_hl();
    p.ret();
}

fn emit_pop6502(p: &mut Program) {
    p.label("rt_pop6502");
    p.push_hl();
    p.ld_a_abs(SHADOW_S);
    p.inc_a();
    p.ld_abs_a(SHADOW_S);
    p.ld_l_a();
    p.ld_h_imm((NES_STACK_BASE >> 8) as u8);
    p.ld_a_hl_ptr();
    p.pop_hl();
    p.ret();
}

// ─── Shifts and rotates on A ─────────────────────────────────────────────────

fn emit_asl_a(p: &mut Program) {
    p.label("rt_asl_a");
    // shadow_C = bit 7 of A before shift; A <<= 1; set N,Z from result.
    p.push_bc();
    p.ld_b_a(); // B = original A
    // Compute new shadow P.
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100); // clear N,Z,C
    p.ld_c_a();
    // Set C if bit 7 of B was set.
    p.ld_a_b();
    p.bit_a(7);
    p.jr_z("_asl_a_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_asl_a_no_c");
    // Shift A.
    p.ld_a_b();
    p.add_a_a(); // A = A + A == A << 1
    p.push_af();
    // Set N (bit 7 of result)
    p.bit_a(7);
    p.jr_z("_asl_a_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_asl_a_no_n");
    p.pop_af();
    p.push_af();
    // Set Z (result == 0)
    p.or_a();
    p.jr_nz("_asl_a_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_asl_a_no_z");
    // Write shadow P.
    p.push_af();
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
    p.pop_af(); // discard saved (we restored above)
    p.pop_bc();
    p.ret();
}

fn emit_lsr_a(p: &mut Program) {
    p.label("rt_lsr_a");
    // shadow_C = bit 0 of A; A >>= 1; N always 0; Z from result.
    p.push_bc();
    p.ld_b_a();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.ld_a_b();
    p.bit_a(0);
    p.jr_z("_lsr_a_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_lsr_a_no_c");
    p.ld_a_b();
    p.srl_a(); // logical shift right
    p.push_af();
    p.or_a();
    p.jr_nz("_lsr_a_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_lsr_a_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
    p.pop_bc();
    p.ret();
}

fn emit_rol_a(p: &mut Program) {
    p.label("rt_rol_a");
    // new_C = bit 7 of A; A = (A << 1) | old_C; N,Z from result.
    p.push_bc();
    p.ld_b_a(); // B = orig
    // Get shadow C into native carry.
    p.ld_a_abs(SHADOW_P);
    p.rrca(); // bit 0 → native carry
    p.ld_a_b();
    p.rl_a(); // rotate left through carry
    p.push_af();
    // Now native carry = old bit 7 of B; A = (B<<1)|old_C.
    // Build shadow P.
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_rol_a_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_rol_a_no_c");
    p.pop_af();
    p.push_af();
    p.bit_a(7);
    p.jr_z("_rol_a_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_rol_a_no_n");
    p.pop_af();
    p.push_af();
    p.or_a();
    p.jr_nz("_rol_a_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_rol_a_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
    p.pop_bc();
    p.ret();
}

fn emit_ror_a(p: &mut Program) {
    p.label("rt_ror_a");
    p.push_bc();
    p.ld_b_a();
    p.ld_a_abs(SHADOW_P);
    p.rrca(); // shadow C → native carry
    p.ld_a_b();
    p.rr_a(); // rotate right through carry
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_ror_a_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_ror_a_no_c");
    p.pop_af();
    p.push_af();
    p.bit_a(7);
    p.jr_z("_ror_a_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_ror_a_no_n");
    p.pop_af();
    p.push_af();
    p.or_a();
    p.jr_nz("_ror_a_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_ror_a_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_af();
    p.pop_bc();
    p.ret();
}

// ─── Memory variants — placeholders for now (TODO: implement in Phase D) ─────

/// rt_asl_mem — HL = SMS address. M := M << 1; shadow C = old bit 7;
/// shadow N,Z from result. A preserved.
fn emit_asl_mem(p: &mut Program) {
    p.label("rt_asl_mem");
    p.push_af();
    p.push_bc();
    p.ld_a_hl_ptr(); // A = M
    p.ld_b_a(); // B = M_orig (for C extraction)
    p.add_a_a(); // A = M << 1; Z80 C reflects bit-7 carry-out
    p.ld_hl_ptr_a(); // store back

    // Build shadow P like emit_asl_a.
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_asl_mem_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_asl_mem_no_c");
    p.pop_af();
    p.push_af();
    p.bit_a(7);
    p.jr_z("_asl_mem_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_asl_mem_no_n");
    p.pop_af();
    p.or_a();
    p.jr_nz("_asl_mem_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_asl_mem_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_bc();
    p.pop_af();
    p.ret();
}

/// rt_lsr_mem — M >>= 1; shadow C = old bit 0; N=0; Z from result.
fn emit_lsr_mem(p: &mut Program) {
    p.label("rt_lsr_mem");
    p.push_af();
    p.push_bc();
    p.ld_a_hl_ptr();
    p.srl_a(); // logical shift right; native C = old bit 0
    p.ld_hl_ptr_a();
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100); // clear N,Z,C
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_lsr_mem_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_lsr_mem_no_c");
    p.pop_af();
    p.or_a();
    p.jr_nz("_lsr_mem_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_lsr_mem_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_bc();
    p.pop_af();
    p.ret();
}

/// rt_rol_mem — M = (M << 1) | shadow_C; new shadow C = old bit 7.
fn emit_rol_mem(p: &mut Program) {
    p.label("rt_rol_mem");
    p.push_af();
    p.push_bc();
    // Native C := shadow C
    p.ld_a_abs(SHADOW_P);
    p.rrca(); // bit 0 → native carry
    p.ld_a_hl_ptr();
    p.rl_a(); // rotate left through carry
    p.ld_hl_ptr_a();
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_rol_mem_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_rol_mem_no_c");
    p.pop_af();
    p.push_af();
    p.bit_a(7);
    p.jr_z("_rol_mem_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_rol_mem_no_n");
    p.pop_af();
    p.or_a();
    p.jr_nz("_rol_mem_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_rol_mem_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_bc();
    p.pop_af();
    p.ret();
}

/// rt_ror_mem — M = (M >> 1) | (shadow_C << 7); new shadow C = old bit 0.
fn emit_ror_mem(p: &mut Program) {
    p.label("rt_ror_mem");
    p.push_af();
    p.push_bc();
    p.ld_a_abs(SHADOW_P);
    p.rrca(); // bit 0 → native carry
    p.ld_a_hl_ptr();
    p.rr_a(); // rotate right through carry
    p.ld_hl_ptr_a();
    p.push_af();
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0111_1100);
    p.ld_c_a();
    p.pop_af();
    p.push_af();
    p.jr_nc("_ror_mem_no_c");
    p.ld_a_c();
    p.or_imm(0b0000_0001);
    p.ld_c_a();
    p.label("_ror_mem_no_c");
    p.pop_af();
    p.push_af();
    p.bit_a(7);
    p.jr_z("_ror_mem_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_ror_mem_no_n");
    p.pop_af();
    p.or_a();
    p.jr_nz("_ror_mem_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_ror_mem_no_z");
    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);
    p.pop_bc();
    p.pop_af();
    p.ret();
}

fn emit_inc_mem(p: &mut Program) {
    p.label("rt_inc_mem");
    // HL = SMS addr. Read, inc, write, set NZ.
    p.push_af();
    p.push_bc();
    p.ld_a_hl_ptr();
    p.inc_a();
    p.ld_hl_ptr_a();
    p.call("rt_set_nz_a");
    p.pop_bc();
    p.pop_af();
    p.ret();
}

fn emit_dec_mem(p: &mut Program) {
    p.label("rt_dec_mem");
    p.push_af();
    p.push_bc();
    p.ld_a_hl_ptr();
    p.dec_a();
    p.ld_hl_ptr_a();
    p.call("rt_set_nz_a");
    p.pop_bc();
    p.pop_af();
    p.ret();
}

fn emit_bit_mem(p: &mut Program) {
    p.label("rt_bit_mem");
    // B = M; Z = (A & B) == 0; N = bit 7 of B; V = bit 6 of B. A unchanged.
    p.push_af();
    p.push_bc();

    // Compute Z = (A & B) == 0.
    p.push_af();
    p.and_b();
    p.pop_af(); // restore A; but native flags from `and a,b` lost
    // Recompute Z = (A & B) == 0 by saving the AND result first.
    // Simpler: do AND into C scratch.
    // Reorganize:
    p.pop_bc();
    p.pop_af();

    // Restart cleanly:
    p.push_af();
    p.push_bc();
    p.push_de();

    // E = A AND B
    p.ld_d_a(); // D = A (preserve)
    p.and_b(); // A = A & B
    p.ld_e_a(); // E = A & B
    p.ld_a_d(); // restore A

    // Now compute shadow P.
    p.ld_a_abs(SHADOW_P);
    p.and_imm(0b0011_1101); // clear N, V, Z. Keep others.
    p.ld_c_a();

    // Z = (E == 0)
    p.ld_a_e();
    p.or_a();
    p.jr_nz("_bit_no_z");
    p.ld_a_c();
    p.or_imm(0b0000_0010);
    p.ld_c_a();
    p.label("_bit_no_z");

    // N = bit 7 of B
    p.ld_a_b();
    p.bit_a(7);
    p.jr_z("_bit_no_n");
    p.ld_a_c();
    p.or_imm(0b1000_0000);
    p.ld_c_a();
    p.label("_bit_no_n");

    // V = bit 6 of B
    p.ld_a_b();
    p.bit_a(6);
    p.jr_z("_bit_no_v");
    p.ld_a_c();
    p.or_imm(0b0100_0000);
    p.ld_c_a();
    p.label("_bit_no_v");

    p.ld_a_c();
    p.ld_abs_a(SHADOW_P);

    p.pop_de();
    p.pop_bc();
    p.pop_af();
    p.ret();
}

// ─── Indexed access ──────────────────────────────────────────────────────────

fn emit_read_indexed(p: &mut Program) {
    p.label("rt_read_indexed");
    // HL = base, B = offset → A = (HL + B). Read only; no flag updates here
    // (lower emits a separate rt_set_nz_a call after the load).
    p.push_hl();
    p.push_bc();
    p.ld_c_b();
    p.ld_b_imm(0);
    p.add_hl_bc();
    p.ld_a_hl_ptr();
    p.pop_bc();
    p.pop_hl();
    p.ret();
}

fn emit_write_indexed(p: &mut Program) {
    p.label("rt_write_indexed");
    // HL = base, B = offset, C = value → (HL + B) = C.
    p.push_hl();
    p.push_bc();
    p.push_de();
    p.ld_e_c(); // E = value
    p.ld_c_b();
    p.ld_b_imm(0);
    p.add_hl_bc();
    p.ld_a_e();
    p.ld_hl_ptr_a();
    p.pop_de();
    p.pop_bc();
    p.pop_hl();
    p.ret();
}

fn emit_read_zp_ptr_y(p: &mut Program) {
    p.label("rt_read_zp_ptr_y");
    // B = zp addr (0..0xFF). Read pointer at $C000+B (low) and $C000+(B+1) (high,
    // with zp wrap). Then dereference at pointer + shadow Y, remapping NES RAM
    // addresses (<$0800) to SMS RAM ($C000..$C7FF).
    p.push_hl();
    p.push_bc();
    p.push_de();
    p.ld_c_e(); // preserve resident Y before DE becomes the decoded pointer
    // HL := $C000 + B
    p.ld_a_b();
    p.ld_l_a();
    p.ld_h_imm((NES_RAM_BASE >> 8) as u8);
    p.ld_e_hl_ptr();
    // Increment L only (zp wrap)
    p.inc_l();
    p.ld_d_hl_ptr();
    // DE = pointer; add Y
    p.ld_a_c();
    p.ld_l_a();
    p.ld_h_imm(0);
    p.add_hl_de();
    // Fixed high PRG is split outside SMS RAM; use the common helper.
    p.ld_a_h();
    p.cp_imm(0xC0);
    p.jr_c("_rzpy_not_high");
    p.call("rt_read_prg_high");
    p.pop_de();
    p.pop_bc();
    p.pop_hl();
    p.ret();
    p.label("_rzpy_not_high");
    // If HL < $0800, remap by adding $C000.
    p.ld_a_h();
    p.cp_imm(0x08);
    p.jr_nc("_rzpy_no_remap");
    p.ld_a_h();
    p.add_a_imm((NES_RAM_BASE >> 8) as u8);
    p.ld_h_a();
    p.label("_rzpy_no_remap");
    p.ld_a_hl_ptr();
    p.pop_de();
    p.pop_bc();
    p.pop_hl();
    p.ret();
}

fn emit_read_prg_high(p: &mut Program) {
    p.label("rt_read_prg_high");
    p.push_bc();
    p.push_de();
    p.ld_a_h();
    p.cp_imm(0xC0);
    p.jr_nc("_rph_valid");
    p.halt();
    p.label("_rph_valid");
    p.cp_imm(0xE0);
    p.jr_nc("_rph_upper");
    p.sub_imm(0xC0);
    p.ld_h_a();
    p.label("_rph_upper");
    p.ld_a_hl_ptr();
    p.pop_de();
    p.pop_bc();
    p.ret();
}

fn emit_read_prg_high_indexed(p: &mut Program) {
    p.label("rt_read_prg_high_indexed");
    p.ld_a_l();
    p.add_a_b();
    p.ld_l_a();
    p.ld_a_h();
    p.adc_a_imm0();
    p.ld_h_a();
    p.jp("rt_read_prg_high");
}

fn emit_write_zp_ptr_y(p: &mut Program) {
    p.label("rt_write_zp_ptr_y");
    // B = zp addr, A = value. Same pointer-resolve as read, then store.
    p.push_hl();
    p.push_bc();
    p.push_de();
    p.ld_c_a(); // save value in C
    p.ld_a_b();
    p.ld_l_a();
    p.ld_h_imm((NES_RAM_BASE >> 8) as u8);
    p.ld_e_hl_ptr();
    p.inc_l();
    p.ld_d_hl_ptr();
    p.ld_a_abs(SHADOW_Y);
    p.ld_l_a();
    p.ld_h_imm(0);
    p.add_hl_de();
    p.ld_a_h();
    p.cp_imm(0x08);
    p.jr_nc("_wzpy_no_remap");
    p.ld_a_h();
    p.add_a_imm((NES_RAM_BASE >> 8) as u8);
    p.ld_h_a();
    p.label("_wzpy_no_remap");
    p.ld_a_c();
    p.ld_hl_ptr_a();
    p.pop_de();
    p.pop_bc();
    p.pop_hl();
    p.ret();
}
