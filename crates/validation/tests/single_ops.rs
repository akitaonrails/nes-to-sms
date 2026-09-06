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
            window_label_prefix: None,
            start: at,
            end: at + len,
            entry_name: name.into(),
            jump_engine_sites: Vec::new(),
            return_consume_sites: Vec::new(),
            materialized_call_sites: Vec::new(),
            return_escape_sites: Vec::new(),
            extra_label_pcs: Vec::new(),
        },
    )
    .expect("lift")
}

fn assert_green(prg: &[u8], at: u16, len: u16, name: &str) {
    let routine = lift(prg, at, len, name);
    let result = validation::validate_routine(prg, &routine, 8);
    assert!(
        result.is_green(),
        "{name}: {}",
        validation::format_report(&[result])
    );
}

#[test]
fn lda_fixed_high_direct_boundaries_validate() {
    for (addr, value) in [
        (0xC000, 0x11),
        (0xDFFF, 0x22),
        (0xE000, 0x33),
        (0xFFFF, 0x44),
    ] {
        let mut prg = slice(&[0xAD, addr as u8, (addr >> 8) as u8, 0x60], 0x8000);
        prg[addr as usize - 0x8000] = value;
        assert_green(&prg, 0x8000, 4, &format!("Lda{addr:04X}"));
    }
}

#[test]
fn lda_fixed_high_indexed_boundaries_validate() {
    for (base, value) in [(0xC000, 0x51), (0xFF00, 0x62)] {
        let mut prg = slice(
            &[0xA2, 0xFF, 0xBD, base as u8, (base >> 8) as u8, 0x60],
            0x8000,
        );
        prg[base as usize + 0xFF - 0x8000] = value;
        assert_green(&prg, 0x8000, 6, &format!("Lda{base:04X}X"));
    }
}

#[test]
fn lda_indirect_y_fixed_high_halves_validate() {
    for (ptr, y, value) in [(0xC123u16, 0x05u8, 0x73), (0xE234, 0x07, 0x84)] {
        // Encode the pointer and a nonzero Y in the routine so every vector uses
        // the same target. Poison pointer+pointer.low to catch a swapped-Y bug.
        let mut prg = slice(
            &[
                0xA9,
                ptr as u8,
                0x85,
                0x10,
                0xA9,
                (ptr >> 8) as u8,
                0x85,
                0x11,
                0xA0,
                y,
                0xB1,
                0x10,
                0x60,
            ],
            0x8000,
        );
        let expected = ptr.wrapping_add(y as u16);
        let wrong = ptr.wrapping_add(ptr as u8 as u16);
        prg[expected as usize - 0x8000] = value;
        prg[wrong as usize - 0x8000] = value ^ 0xFF;
        assert_green(&prg, 0x8000, 13, &format!("IndY{ptr:04X}"));
    }
}

#[test]
fn nrom128_fixed_high_mirrors_lower_bank() {
    let mut prg = vec![0u8; 0x4000];
    prg[..4].copy_from_slice(&[0xAD, 0x34, 0xC0, 0x60]);
    prg[0x034] = 0xA5;
    assert_green(&prg, 0x8000, 4, "Nrom128FixedMirror");
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
