//! Returning-weapon tail discovered through original code, not a guessed stub.
//! CV1_WEAPON_NES=/path/to/input.nes cargo test -p nes_to_sms \
//!   --test cv1_returning_weapon -- --include-ignored

fn profile() -> profile::Profile {
    profile::load_from_str(include_str!("../../../profiles/cv1.toml")).unwrap()
}

#[test]
fn profile_roots_the_returning_weapon_acceleration_tail() {
    assert!(profile().functions.iter().any(|f| f.addr == 0xdc37));
}

#[test]
fn profile_covers_every_original_weapon_capacity_escape_caller() {
    let prof = profile();
    let site = prof
        .return_consume_at(0xda9d, None)
        .expect("capacity escape needs ownership transfer");
    let calls: Vec<_> = site
        .calls
        .iter()
        .map(|call| (call.caller, call.target))
        .collect();
    assert_eq!(
        calls,
        [
            (0xda72, 0xda90),
            (0xdaa3, 0xda90),
            (0xdaac, 0xda90),
            (0xdab5, 0xda90),
            (0xdae9, 0xda90),
            (0xdaec, 0xda7b)
        ]
    );
}

#[test]
fn projectile_cleanup_drops_only_its_arranged_dispatch_return() {
    let prof = profile();
    for edge in [0xdb08, 0xdb4c, 0xdc34] {
        let escape = prof.return_escape_at(edge, None).unwrap();
        assert_eq!((escape.target, escape.return_addr), (0xec60, 0xe9e5));
        assert!(!escape.stack_bytes_already_consumed);
    }
    let dispatcher = prof
        .jump_engines
        .iter()
        .find(|site| site.caller == 0xe959)
        .unwrap();
    assert!(
        !dispatcher.tail_indices.contains(&23),
        "active projectiles still return normally"
    );
    assert_eq!(dispatcher.stack_return_bytes, 2);
}

#[test]
#[ignore = "requires canonical CV1_WEAPON_NES; no ROM is committed"]
fn original_weapon_dispatch_reaches_tail_and_discovery_keeps_both_exits() {
    let path = std::path::PathBuf::from(std::env::var("CV1_WEAPON_NES").unwrap());
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
    // Enter the real projectile selector with the live return arranged by
    // E959. Catch/expiry must reach E92B after discarding exactly that pair.
    for (selector, edge) in [(1u8, 0xdc34), (6, 0xdb08), (7, 0xdb4c)] {
        for x in [20u8, 21, 22] {
            let mut bus = oracle_6502::FlatBus::new();
            bus.load(0xc000, fixed);
            bus.ram[0x584 + x as usize] = selector;
            bus.ram[0x46c + x as usize] = if selector == 1 { 4 } else { 1 };
            bus.ram[0x568 + x as usize] = 1;
            bus.ram[0x434 + x as usize] = 0x17;
            bus.ram[0x1fc] = 0xe5;
            bus.ram[0x1fd] = 0xe9;
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0xdba1;
            cpu.x = x;
            cpu.sp = 0xfb;
            let mut saw_edge = false;
            for _ in 0..500 {
                saw_edge |= cpu.pc == edge;
                if cpu.pc == 0xe92b {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert!(saw_edge, "selector={selector}");
            assert_eq!((cpu.pc, cpu.sp, cpu.x, cpu.a), (0xe92b, 0xfd, x, 0xe9));
            assert_eq!(bus.ram[0x434 + x as usize], 0);
            assert_eq!(bus.ram[0x354 + x as usize], 0xf4);
        }
    }
    for caller in [0xda72u16, 0xdaa3, 0xdaac, 0xdab5, 0xdae9, 0xdaec] {
        let jsr = cpu6502::decode_at(fixed, caller, usize::from(caller - 0xc000)).unwrap();
        assert_eq!(jsr.mnemonic, cpu6502::Mnemonic::JSR);
        assert_eq!(
            jsr.operand,
            cpu6502::Operand::Addr(if caller == 0xdaec { 0xda7b } else { 0xda90 })
        );
        for limit in [0u8, 1, 2] {
            for status in [0x24u8, 0xa5, 0x67] {
                let mut bus = oracle_6502::FlatBus::new();
                bus.load(0xc000, fixed);
                bus.ram[0x64] = limit;
                bus.ram[0x434 + 20..0x434 + 23].fill(0x17);
                bus.ram[0x71] = limit; // DA7B: fewer than its five-heart cost.
                bus.ram[0x1fe] = 0x25;
                bus.ram[0x1ff] = 7;
                let mut cpu = oracle_6502::Cpu::new();
                cpu.pc = caller;
                cpu.p = status;
                let mut consumed = Vec::new();
                for _ in 0..80 {
                    if cpu.pc == 0x726 {
                        break;
                    }
                    assert_ne!(cpu.pc, caller + 3, "failed allocation must skip its caller");
                    if matches!(cpu.pc, 0xda9d | 0xda9e) {
                        consumed.push(bus.ram[0x100 + cpu.sp.wrapping_add(1) as usize]);
                    }
                    cpu.step(&mut bus).unwrap();
                }
                assert_eq!((cpu.pc, cpu.sp), (0x726, 0xff));
                assert_eq!(consumed, (caller + 2).to_le_bytes());
            }
        }
    }
    let branch = cpu6502::decode_at(fixed, 0xdc84, 0x1c84).unwrap();
    assert_eq!(branch.mnemonic, cpu6502::Mnemonic::BEQ);
    assert_eq!(branch.branch_target(), Some(0xdc37));

    for x in [20u8, 21, 22] {
        for low in [0u8, 0xdf, 0xe0, 0xff] {
            for high in [0u8, 1] {
                for carry in [0u8, 1] {
                    let mut bus = oracle_6502::FlatBus::new();
                    bus.load(0xc000, fixed);
                    bus.ram[0x584 + x as usize] = 1;
                    bus.ram[0x46c + x as usize] = 3;
                    bus.ram[0x418 + x as usize] = low;
                    bus.ram[0x3fc + x as usize] = high;
                    bus.ram[0x4dc + x as usize] = 0x55;
                    let mut cpu = oracle_6502::Cpu::new();
                    cpu.pc = 0xdba1;
                    cpu.x = x;
                    cpu.p = 0x24 | carry;
                    let mut saw_tail = false;
                    for _ in 0..80 {
                        saw_tail |= cpu.pc == 0xdc37;
                        if matches!(cpu.pc, 0xee48 | 0xec72) {
                            break;
                        }
                        cpu.step(&mut bus).unwrap();
                    }
                    assert!(saw_tail);
                    let velocity =
                        (u16::from(high) << 8 | u16::from(low)) + 0x20 + u16::from(carry);
                    assert_eq!(bus.ram[0x418 + x as usize], velocity as u8);
                    assert_eq!(bus.ram[0x3fc + x as usize], (velocity >> 8) as u8);
                    assert_eq!(cpu.x, x);
                    assert_eq!(bus.ram[0x46c + x as usize], 3);
                    if velocity < 0x200 {
                        assert_eq!((cpu.pc, cpu.sp), (0xee48, 0xfd));
                        assert_eq!(bus.ram[0x4dc + x as usize], 0x55);
                    } else {
                        assert_eq!((cpu.pc, cpu.sp, cpu.a, cpu.y), (0xec72, 0xfb, 2, 0));
                        assert_eq!(bus.ram[0x4dc + x as usize], 0);
                    }
                }
            }
        }
    }

    let mut view = image.prg[..0x8000].to_vec();
    view[0x4000..].copy_from_slice(fixed);
    let analyzed = analysis::analyze_in_window(
        &view,
        analysis::nes_rom_like::Vectors {
            nmi: 0xc052,
            reset: 0xc008,
            irq: 0xc0bf,
        },
        &profile(),
        analysis::AnalysisWindow::FIXED_16K,
        None,
    );
    let tail = analyzed
        .functions
        .by_addr(0xdc37)
        .expect("live conditional tail must be rooted");
    assert_eq!(tail.end, 0xdc5f, "both acceleration exits must remain code");
}
