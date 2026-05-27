//! Validate the smallest possible routines, one IR op at a time.
//! Used to localize bugs in the harness or the runtime stubs.

use ir::{LiftOptions, lift_range};

fn slice(bytes: &[u8], at: u16) -> Vec<u8> {
    let mut prg = vec![0u8; 32 * 1024];
    let off = at as usize - 0x8000;
    prg[off..off + bytes.len()].copy_from_slice(bytes);
    prg
}

fn lift(prg: &[u8], at: u16, len: u16, name: &str) -> ir::Routine {
    lift_range(
        prg,
        &LiftOptions {
            start: at,
            end: at + len,
            entry_name: name.into(),
            jump_engine_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift")
}

#[test]
fn just_rts() {
    // 60  RTS
    let prg = slice(&[0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 1, "JustRts");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "RTS only should validate");
}

#[test]
fn lda_imm_then_rts() {
    // A9 42  LDA #$42
    // 60     RTS
    let prg = slice(&[0xA9, 0x42, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 3, "LdaImmRts");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "LDA #imm + RTS should validate");
}

#[test]
fn lda_imm_zero_sets_z() {
    let prg = slice(&[0xA9, 0x00, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 3, "LdaImm0Rts");
    let r = validation::validate_routine(&prg, &routine, 4);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "LDA #$00 sets Z; expecting green");
}

#[test]
fn lda_zp_then_rts() {
    // A5 E7  LDA $E7
    // 60     RTS
    let prg = slice(&[0xA5, 0xE7, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 3, "LdaZpRts");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "LDA $zp + RTS should validate");
}

#[test]
fn sta_zp_then_rts() {
    // A9 42  LDA #$42
    // 85 0E  STA $0E
    // 60     RTS
    let prg = slice(&[0xA9, 0x42, 0x85, 0x0E, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 5, "LdaStaRts");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "LDA imm + STA zp + RTS should validate");
}

#[test]
fn clc_only() {
    // 18     CLC
    // 60     RTS
    let prg = slice(&[0x18, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 2, "ClcRts");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "CLC + RTS should validate");
}

#[test]
fn sta_abs_indexed_y() {
    // A0 04  LDY #$04
    // A9 F8  LDA #$F8
    // 99 00 02  STA $0200,Y
    // 60     RTS
    let prg = slice(&[0xA0, 0x04, 0xA9, 0xF8, 0x99, 0x00, 0x02, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 8, "StaAbsY");
    let r = validation::validate_routine(&prg, &routine, 8);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "STA $abs,Y should validate");
}

#[test]
fn iny_loop_writes_ram() {
    // Same shape as SMB's $8223 OAM-clear loop. BNE target = the STA, so
    // rel = STA_addr - (BNE_addr+2) = $8004 - $800D = $F7.
    // A0 04           LDY #$04
    // A9 F8           LDA #$F8
    // 99 00 02        STA $0200,Y
    // C8 C8 C8 C8     INY × 4
    // D0 F7           BNE $8004
    // 60              RTS
    let prg = slice(
        &[
            0xA0, 0x04, 0xA9, 0xF8, 0x99, 0x00, 0x02, 0xC8, 0xC8, 0xC8, 0xC8, 0xD0, 0xF7, 0x60,
        ],
        0x8000,
    );
    let routine = lift(&prg, 0x8000, 14, "InyLoop");
    let r = validation::validate_routine(&prg, &routine, 4);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "OAM-clear-style loop should validate");
}

#[test]
fn adc_imm_no_carry() {
    // 18     CLC
    // 69 02  ADC #$02
    // 60     RTS
    let prg = slice(&[0x18, 0x69, 0x02, 0x60], 0x8000);
    let routine = lift(&prg, 0x8000, 4, "ClcAdcRts");
    let r = validation::validate_routine(&prg, &routine, 16);
    println!("{}", validation::format_report(&[r.clone()]));
    assert!(r.is_green(), "CLC + ADC + RTS should validate");
}
