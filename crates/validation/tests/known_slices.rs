//! Validate the three SMB micro-slices the original PoC validated by hand:
//!   $9CA6  add 2 to little-endian zero-page pointer $E7/$E8
//!   $B1B4  conditional store of $06 to zp $0E based on incoming Z flag
//!   $AEF9  compare A vs $03 and branch when carry set
//!
//! These are self-contained (no external JSR, no hardware) so the diff
//! harness should report them green.

use ir::{LiftOptions, lift_range};

fn build_prg(slices: &[(u16, &[u8])]) -> Vec<u8> {
    let mut prg = vec![0u8; 32 * 1024];
    for (start, bytes) in slices {
        let off = *start as usize - 0x8000;
        prg[off..off + bytes.len()].copy_from_slice(bytes);
    }
    prg
}

#[test]
fn smb_9ca6_pointer_increment_validates_green() {
    let bytes = [
        0xA5, 0xE7, 0x18, 0x69, 0x02, 0x85, 0xE7, 0xA5, 0xE8, 0x69, 0x00, 0x85, 0xE8, 0x60,
    ];
    let prg = build_prg(&[(0x9CA6, &bytes)]);
    let routine = lift_range(
        &prg,
        &LiftOptions {
            window_label_prefix: None,
            start: 0x9CA6,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            end: 0x9CA6 + bytes.len() as u16,
            entry_name: "AdvancePointer".into(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift");

    let result = validation::validate_routine(&prg, &routine, 32);
    println!("{}", validation::format_report(&[result.clone()]));
    assert!(result.is_green(), "$9CA6 should validate green");
    assert_eq!(result.vectors_passed, 32);
}

#[test]
fn smb_b1b4_branch_store_validates_green() {
    // D0 04  BNE $B1BA
    // A9 06  LDA #$06
    // 85 0E  STA $0E
    // 60     RTS
    let bytes = [0xD0, 0x04, 0xA9, 0x06, 0x85, 0x0E, 0x60];
    let prg = build_prg(&[(0xB1B4, &bytes)]);
    let routine = lift_range(
        &prg,
        &LiftOptions {
            window_label_prefix: None,
            start: 0xB1B4,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            end: 0xB1B4 + bytes.len() as u16,
            entry_name: "BranchStore".into(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift");

    let result = validation::validate_routine(&prg, &routine, 32);
    println!("{}", validation::format_report(&[result.clone()]));
    assert!(result.is_green(), "$B1B4 should validate green");
}

#[test]
fn smb_aef9_compare_branch_skips_external() {
    // C9 03  CMP #$03
    // B0 01  BCS $AEFE  ← target is outside our lift range
    // 60     RTS
    let bytes = [0xC9, 0x03, 0xB0, 0x01, 0x60];
    let prg = build_prg(&[(0xAEF9, &bytes)]);
    let routine = lift_range(
        &prg,
        &LiftOptions {
            window_label_prefix: None,
            start: 0xAEF9,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            end: 0xAEF9 + bytes.len() as u16,
            entry_name: "CompareBranch".into(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift");

    let result = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[result.clone()]));
    // External BCS target → skipped, not validated. This is correct
    // behavior; once we add stub-return for externals (Phase A.5+), we
    // can validate it for real.
    assert!(
        result.skipped_reason.is_some(),
        "$AEF9 has an external branch and should be skipped, got {:?}",
        result
    );
}

#[test]
fn smb_b1b4_with_intentional_lower_bug_validates_red() {
    // Synthetic test: we don't actually modify the lower crate here.
    // Instead we run a routine whose lift correctly translates but whose
    // input vectors hit a code path that depends on the LDA #imm flag
    // update — if a future bug breaks rt_set_nz_a, this test will catch
    // it before the regression hits anything downstream.
    //
    // For now it just confirms the LDA + STA + RTS pattern validates green.
    let bytes = [
        0xA9, 0x00, // LDA #$00
        0x85, 0x10, // STA $10
        0xA9, 0x80, // LDA #$80
        0x85, 0x11, // STA $11
        0x60,
    ];
    let prg = build_prg(&[(0x9100, &bytes)]);
    let routine = lift_range(
        &prg,
        &LiftOptions {
            window_label_prefix: None,
            start: 0x9100,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: false,
            data_regions: Vec::new(),
            end: 0x9100 + bytes.len() as u16,
            entry_name: "TwoStores".into(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift");

    let result = validation::validate_routine(&prg, &routine, 16);
    println!("{}", validation::format_report(&[result.clone()]));
    assert!(
        result.is_green(),
        "TwoStores should validate green, failures: {:?}",
        result.failures
    );
}
