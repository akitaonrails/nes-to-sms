//! Source-backed bank6 interior entry reached after the first upper exit.
//! CV1_NEXT_ROOM_NES=/path/to/input.nes cargo test -p nes_to_sms \
//!   --test cv1_next_room -- --ignored

fn cv1_profile() -> profile::Profile {
    profile::load_from_str(include_str!("../../../profiles/cv1.toml")).unwrap()
}

#[test]
fn profile_roots_the_room_two_interior_entry_in_bank_six_only() {
    assert!(
        cv1_profile()
            .bank_entries
            .iter()
            .any(|entry| entry.bank == 6 && entry.addr == 0x80e7)
    );
}

#[test]
#[ignore = "requires canonical CV1_NEXT_ROOM_NES, never a committed ROM"]
fn original_area_dispatch_reaches_the_live_interior_and_discovery_exports_it() {
    use oracle_6502::{FLAG_C, FLAG_N, FLAG_Z};

    let path = std::path::PathBuf::from(
        std::env::var("CV1_NEXT_ROOM_NES").expect("set CV1_NEXT_ROOM_NES"),
    );
    let path = if path.is_absolute() {
        path
    } else {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    };
    let bytes = std::fs::read(path).unwrap();
    let image = nes_rom::parse(&bytes).unwrap();
    assert_eq!(image.prg.len(), 128 * 1024);
    let window = &image.prg[6 * 0x4000..7 * 0x4000];
    let fixed = &image.prg[7 * 0x4000..];
    let jump = cpu6502::decode_at(fixed, 0xf552, 0xf552 - 0xc000).unwrap();
    assert_eq!(
        (jump.mnemonic, jump.operand),
        (cpu6502::Mnemonic::JMP, cpu6502::Operand::Addr(0x80e7))
    );
    let entry = cpu6502::decode_at(window, 0x80e7, 0x80e7 - 0x8000).unwrap();
    assert_eq!(
        (entry.mnemonic, entry.mode, entry.operand),
        (
            cpu6502::Mnemonic::LDA,
            cpu6502::AddrMode::AbsoluteY,
            cpu6502::Operand::Addr(0x0184)
        )
    );

    for area in 0..=255u8 {
        let mut bus = oracle_6502::FlatBus::new();
        bus.load(0x8000, window);
        bus.load(0xc000, fixed);
        bus.ram[0x28] = area;
        let mut cpu = oracle_6502::Cpu::new();
        cpu.pc = 0xf54c;
        cpu.x = 17;
        cpu.y = 29;
        cpu.p = 0x7d;
        for _ in 0..4 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.pc, if area == 2 { 0x80e7 } else { 0x87e6 });
        assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), (area, 17, 29, 0xfd));
        let compared = area.wrapping_sub(2);
        assert_eq!(
            cpu.p,
            (0x7d & !(FLAG_N | FLAG_Z | FLAG_C))
                | (compared & FLAG_N)
                | if compared == 0 { FLAG_Z } else { 0 }
                | if area >= 2 { FLAG_C } else { 0 }
        );
    }
    for y in [0u8, 5, 0xff] {
        for value in [0, 1, 0x80, 0xff] {
            let mut bus = oracle_6502::FlatBus::new();
            bus.load(0x8000, window);
            bus.load(0xc000, fixed);
            bus.ram[0x2b] = 0; // The earlier80E3 entry would incorrectly return.
            bus.ram[0x0184 + y as usize] = value;
            bus.ram[0x01fe] = 0x33;
            bus.ram[0x01ff] = 0x12;
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0x80e7;
            cpu.y = y;
            cpu.x = 17;
            cpu.p = 0x7d;
            cpu.step(&mut bus).unwrap();
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.pc, if value == 0 { 0x80ed } else { 0x80ec });
            assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), (value, 17, y, 0xfd));
            assert_eq!(
                cpu.p,
                (0x7d & !(FLAG_N | FLAG_Z))
                    | (value & FLAG_N)
                    | if value == 0 { FLAG_Z } else { 0 }
            );
            if value == 0 {
                for _ in 0..4 {
                    cpu.step(&mut bus).unwrap();
                }
                assert_eq!(
                    (cpu.pc, cpu.x, cpu.sp, bus.ram[0x4b]),
                    (0xf41f, 9, 0xfb, 12)
                );
            } else {
                cpu.step(&mut bus).unwrap();
                assert_eq!((cpu.pc, cpu.sp), (0x1234, 0xff));
            }
        }
    }

    let mut prof = cv1_profile();
    prof.functions = prof
        .bank_entries
        .iter()
        .filter(|entry| entry.bank == 6)
        .map(|entry| profile::Function {
            addr: entry.addr,
            name: format!("L_b6_{:04X}", entry.addr),
            note: None,
        })
        .collect();
    prof.jump_tables.clear();
    prof.jump_engines.retain(|entry| entry.bank == Some(6));
    let mut view = window.to_vec();
    view.extend_from_slice(fixed);
    let analyzed = analysis::analyze_in_window(
        &view,
        analysis::nes_rom_like::Vectors {
            nmi: 0,
            reset: 0,
            irq: 0,
        },
        &prof,
        analysis::AnalysisWindow::SWITCHABLE_16K,
        Some(6),
    );
    let owner = analyzed
        .functions
        .by_addr(0x80e7)
        .expect("bank6 interior needs an exported translated owner");
    assert_eq!(owner.name, "L_b6_80E7");
    assert!(
        owner.end >= 0x80ed,
        "both original first branches must remain code"
    );
}
