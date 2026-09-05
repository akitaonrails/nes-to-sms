//! Profile regression and optional original-ROM proof for the dagger initializer.
//! CV1_DAGGER_NES=/path/to/input.nes cargo test -p nes_to_sms --test cv1_dagger -- --ignored

fn cv1_profile() -> profile::Profile {
    profile::load_from_str(include_str!("../../../profiles/cv1.toml")).unwrap()
}

#[test]
fn profile_roots_the_shared_dagger_initializer() {
    assert!(cv1_profile().functions.iter().any(|f| f.addr == 0xdb8d));
}

#[test]
#[ignore = "requires the canonical CV1_DAGGER_NES, never a committed ROM"]
fn canonical_dagger_branch_reaches_live_code_and_profile_discovers_it() {
    let path =
        std::path::PathBuf::from(std::env::var("CV1_DAGGER_NES").expect("set CV1_DAGGER_NES"));
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
    let fixed = &image.prg[7 * 0x4000..];
    let branch = cpu6502::decode_at(fixed, 0xdba4, 0xdba4 - 0xc000).unwrap();
    assert_eq!(branch.mnemonic, cpu6502::Mnemonic::BEQ);
    assert_eq!(branch.branch_target(), Some(0xdb8d));

    for x in [1u8, 7, 15] {
        for state in [0u8, 1, 0xff] {
            let mut bus = oracle_6502::FlatBus::new();
            bus.load(0xc000, fixed);
            bus.ram[0x046c + x as usize] = state;
            bus.ram[0x0584 + x as usize] = 0; // dagger dispatch selector
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0xdba1;
            cpu.x = x;
            let initial_sp = cpu.sp;
            let target = if state == 0 { 0xec72 } else { 0xee48 };
            let mut saw_initializer = false;
            for _ in 0..16 {
                saw_initializer |= cpu.pc == 0xdb8d;
                if cpu.pc == target {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert!(saw_initializer);
            assert_eq!(cpu.pc, target);
            assert_eq!((cpu.x, cpu.sp), (x, initial_sp));
            if state == 0 {
                assert_eq!((cpu.a, cpu.y, bus.ram[0x046c + x as usize]), (4, 0, 1));
            } else {
                assert_eq!(bus.ram[0x046c + x as usize], state);
            }
        }
    }

    let mut view = image.prg[..0x8000].to_vec();
    view[0x4000..].copy_from_slice(fixed);
    let vectors = analysis::nes_rom_like::Vectors {
        nmi: 0xc052,
        reset: 0xc008,
        irq: 0xc0bf,
    };
    let prof = cv1_profile();
    let analyzed = analysis::analyze_in_window(
        &view,
        vectors,
        &prof,
        analysis::AnalysisWindow::FIXED_16K,
        None,
    );
    let initializer = analyzed
        .functions
        .by_addr(0xdb8d)
        .expect("dagger branch needs a translated owner");
    assert_eq!(initializer.end, 0xdba1);
}
