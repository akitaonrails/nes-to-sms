//! Exercise the normal lifter/lowerer with explicit index and flag domains.
use super::*;

// The general harness deliberately uses FlatBus. These tests also cover NES
// RAM aliases, so supply mirroring at the bus boundary, not in CPU semantics.
struct MirroredRam(oracle_6502::FlatBus);

impl OracleBus for MirroredRam {
    fn read(&mut self, addr: u16) -> u8 {
        self.0.read(if addr < 0x2000 { addr & 0x7FF } else { addr })
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.0
            .write(if addr < 0x2000 { addr & 0x7FF } else { addr }, value);
    }
}

fn oracle_with_ram_mirrors(prg: &[u8], init: InitialState) -> FinalState {
    let mut bus = MirroredRam(oracle_6502::FlatBus::new());
    bus.0.load(0x8000, prg);
    let mut zp = vec![0; 0x100];
    let mut ram = vec![0; 0x600];
    fill_memory_with_seed(&mut zp, init.mem_seed);
    fill_memory_with_seed(&mut ram, init.mem_seed.wrapping_add(1));
    bus.0.load(0, &zp);
    bus.0.load(0x0200, &ram);
    bus.write(0x0100 + u16::from(init.sp), 0xFF);
    bus.write(0x0100 + u16::from(init.sp.wrapping_sub(1)), 0xFE);
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = 0xC000;
    cpu.a = init.a;
    cpu.x = init.x;
    cpu.y = init.y;
    cpu.p = init.p;
    cpu.sp = init.sp.wrapping_sub(2);
    cpu.run_until_rts(&mut bus, 100).unwrap();
    FinalState {
        a: cpu.a,
        x: cpu.x,
        y: cpu.y,
        p: cpu.p,
        sp: cpu.sp,
        zp: (0..0x100).map(|a| bus.read(a)).collect(),
        ram: (0x0200..0x0800).map(|a| bus.read(a)).collect(),
    }
}

fn check_indexed(opcode: u8, base: u16, tail: &[u8]) {
    let mut prg = vec![0; 0x8000];
    fill_memory_with_seed(&mut prg, 17);
    let mut code = vec![opcode, base as u8, (base >> 8) as u8];
    code.extend_from_slice(tail);
    code.push(0x60);
    // Keep the test's instructions outside the low-PRG data window.
    prg[0x4000..0x4000 + code.len()].copy_from_slice(&code);
    let routine = ir::lift_range(
        &prg,
        &ir::LiftOptions {
            start: 0xC000,
            end: 0xC000 + code.len() as u16,
            entry_name: "Indexed".into(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(classify_routine(&routine), None);
    let (bytes, entry) = lower_for_validation(&routine).unwrap();
    for index in 0..=255u8 {
        // All combinations of live N/V/Z/C, with I/U fixed as in the harness.
        for flags in 0..16u8 {
            let init = InitialState {
                a: index.wrapping_mul(73),
                x: index,
                y: index ^ 0xA5,
                p: 0x24 | ((flags & 8) << 4) | ((flags & 4) << 4) | (flags & 2) | (flags & 1),
                sp: 0xFD,
                mem_seed: u64::from(index) * 16 + u64::from(flags),
            };
            let oracle = oracle_with_ram_mirrors(&prg, init);
            let z80 = run_z80_with_prg(&bytes, entry, init, Some(&prg));
            let diffs = diff_state(&oracle, &z80);
            assert!(
                diffs.is_empty(),
                "opcode={opcode:02X} base={base:04X} init={init:?}: {diffs:?}"
            );
        }
    }
}

#[test]
fn aligned_load_store_all_indices_and_live_flags() {
    for base in [0x0000, 0x0200, 0x0700, 0x0A00, 0x1F00] {
        for opcode in [0xBD, 0xB9, 0x9D, 0x99] {
            check_indexed(opcode, base, &[]);
        }
    }
    // Direct low PRG, including the last safe aligned page.
    for base in [0x8000, 0xBF00] {
        for opcode in [0xBD, 0xB9] {
            check_indexed(opcode, base, &[]);
        }
    }
}

#[test]
fn aligned_index_register_loads_and_alu_preserve_live_state() {
    // LDX abs,Y; LDY abs,X; ADC/SBC/CMP/AND/ORA/EOR abs,X.
    for opcode in [0xBE, 0xBC, 0x7D, 0xFD, 0xDD, 0x3D, 0x1D, 0x5D] {
        check_indexed(opcode, 0x0200, &[]);
    }
    // Store must preserve carry for a following ADC and read its own write.
    check_indexed(0x9D, 0x0200, &[0x7D, 0x00, 0x02]);
}

#[test]
fn unaligned_indexed_page_crossing_stays_general() {
    for base in [0x0201, 0x02FF, 0x06FF] {
        for opcode in [0xBD, 0xB9, 0x9D, 0x99] {
            check_indexed(opcode, base, &[]);
        }
    }
}
