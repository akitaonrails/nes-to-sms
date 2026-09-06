//! Source-backed banked entry regression; commercial input remains optional.
//! CV1_STAIRS_NES=/path/to/input.nes cargo test -p nes_to_sms --test cv1_stairs -- --ignored

fn cv1_profile() -> profile::Profile {
    profile::load_from_str(include_str!("../../../profiles/cv1.toml")).unwrap()
}

#[test]
fn profile_roots_the_banked_stair_tails() {
    for addr in [0x971d, 0x9ecb] {
        assert!(
            cv1_profile()
                .bank_entries
                .iter()
                .any(|entry| entry.bank == 6 && entry.addr == addr)
        );
    }
}

#[test]
#[ignore = "requires canonical CV1_STAIRS_NES, never a committed ROM"]
fn canonical_stair_modes_reach_live_tail_and_profile_discovers_it() {
    use oracle_6502::{FLAG_C, FLAG_N, FLAG_Z};

    let path =
        std::path::PathBuf::from(std::env::var("CV1_STAIRS_NES").expect("set CV1_STAIRS_NES"));
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
    let timeout = cpu6502::decode_at(window, 0x9729, 0x9729 - 0x8000).unwrap();
    assert_eq!(timeout.mnemonic, cpu6502::Mnemonic::BMI);
    assert_eq!(timeout.branch_target(), Some(0x971d));
    for pc in [0x9ed3u16, 0x9ed7] {
        let branch = cpu6502::decode_at(window, pc, (pc - 0x8000) as usize).unwrap();
        assert_eq!(branch.mnemonic, cpu6502::Mnemonic::BEQ);
        assert_eq!(branch.branch_target(), Some(0x9ecb));
    }

    for countdown in 0..=255u8 {
        for initial_flags in [0x24u8, 0xef] {
            let mut bus = oracle_6502::FlatBus::new();
            bus.load(0x8000, window);
            bus.ram[0x04dc] = countdown;
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0x9726;
            cpu.a = 37;
            cpu.x = 17;
            cpu.y = 29;
            cpu.p = initial_flags;
            let expected_countdown = countdown.wrapping_sub(1);
            let expired = expected_countdown & FLAG_N != 0;
            let destination = if expired { 0x9605 } else { 0x972b };
            let mut saw_tail = false;
            for _ in 0..4 {
                saw_tail |= cpu.pc == 0x971d;
                if cpu.pc == destination {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(cpu.pc, destination);
            assert_eq!(saw_tail, expired);
            assert_eq!((cpu.a, cpu.x, cpu.y, cpu.sp), (37, 17, 29, 0xfd));
            assert_eq!(bus.ram[0x04dc], expected_countdown);
            let expected_flags = (initial_flags & !(FLAG_N | FLAG_Z))
                | (expected_countdown & FLAG_N)
                | if expected_countdown == 0 { FLAG_Z } else { 0 };
            assert_eq!(cpu.p, expected_flags);
        }
    }

    for mode in 0..=255u8 {
        for initial_flags in [0x24u8, 0xef] {
            for collision in [0u8, 1, 0x80, 0xff] {
                let mut bus = oracle_6502::FlatBus::new();
                bus.load(0x8000, window);
                bus.ram[0x046c] = mode;
                bus.ram[0x0142] = collision;
                let mut cpu = oracle_6502::Cpu::new();
                cpu.pc = 0x9ece;
                cpu.x = 17;
                cpu.y = 29;
                cpu.p = initial_flags;
                let initial_sp = cpu.sp;
                let stairs = matches!(mode, 4 | 6);
                let destination = if stairs { 0x9f5f } else { 0x9ed9 };
                let mut saw_tail = false;
                for _ in 0..16 {
                    saw_tail |= cpu.pc == 0x9ecb;
                    if cpu.pc == destination {
                        break;
                    }
                    cpu.step(&mut bus).unwrap();
                }
                assert_eq!(cpu.pc, destination, "mode {mode}");
                assert_eq!(saw_tail, stairs);
                assert_eq!((cpu.x, cpu.y, cpu.sp), (17, 29, initial_sp));
                assert_eq!(bus.ram[0x046c], mode);
                let (value, carry, flags_value) = if stairs {
                    (collision & 0xfe, false, collision & 0xfe)
                } else {
                    (mode, mode >= 4, mode.wrapping_sub(4))
                };
                assert_eq!(cpu.a, value);
                assert_eq!(bus.ram[0x0142], if stairs { value } else { collision });
                let expected_flags = (initial_flags & !(FLAG_N | FLAG_Z | FLAG_C))
                    | (flags_value & FLAG_N)
                    | if flags_value == 0 { FLAG_Z } else { 0 }
                    | if carry { FLAG_C } else { 0 };
                assert_eq!(cpu.p, expected_flags, "mode {mode}");
                if stairs {
                    // Execute the original RTS with an owned caller return.
                    bus.ram[0x01fe] = 0x33;
                    bus.ram[0x01ff] = 0x12;
                    cpu.step(&mut bus).unwrap();
                    assert_eq!((cpu.pc, cpu.sp, cpu.p), (0x1234, 0xff, expected_flags));
                }
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
    view.extend_from_slice(&image.prg[7 * 0x4000..]);
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
    let tail = analyzed
        .functions
        .by_addr(0x9ecb)
        .expect("bank6 stair tail needs a translated owner");
    assert_eq!(tail.end, 0x9ece);
    assert!(tail.external_refs.contains(&0x9f56));
    let timeout = analyzed
        .functions
        .by_addr(0x971d)
        .expect("bank6 stair timeout needs a translated owner");
    assert_eq!(timeout.end, 0x9720);
    assert!(timeout.external_refs.contains(&0x9605));
}
