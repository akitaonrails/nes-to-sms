//! FULL-profile lowering, not the profile-less validation/stub harness.
use ir::{LiftOptions, lift_range};

// (absolute opcode, zero-page opcode); final six are NMOS RMW operations.
const OPS: [(u8, u8); 22] = [
    (0xad, 0xa5),
    (0xae, 0xa6),
    (0xac, 0xa4),
    (0x8d, 0x85),
    (0x8e, 0x86),
    (0x8c, 0x84),
    (0x8f, 0x87),
    (0x6d, 0x65),
    (0xed, 0xe5),
    (0x2d, 0x25),
    (0x0d, 0x05),
    (0x4d, 0x45),
    (0xcd, 0xc5),
    (0xec, 0xe4),
    (0xcc, 0xc4),
    (0x2c, 0x24),
    (0x0e, 0x06),
    (0x4e, 0x46),
    (0x2e, 0x26),
    (0x6e, 0x66),
    (0xee, 0xe6),
    (0xce, 0xc6),
];
const ADDRESSES: [(u16, bool); 12] = [
    (0, true),
    (255, true),
    (0, false),
    (255, false),
    (0x100, false),
    (0x7ff, false),
    (0x800, false),
    (0xfff, false),
    (0x1000, false),
    (0x17ff, false),
    (0x1800, false),
    (0x1fff, false),
];

struct SourceBus {
    ram: [u8; 0x800],
    code: Vec<u8>,
    writes: Vec<(u16, u8)>,
}
impl oracle_6502::Bus for SourceBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0..=0x1fff => self.ram[usize::from(addr & 0x7ff)],
            0x8000..=0x8002 => self
                .code
                .get(usize::from(addr - 0x8000))
                .copied()
                .unwrap_or(0),
            _ => panic!("unexpected source read {addr:04X}"),
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        assert!(addr < 0x2000);
        self.ram[usize::from(addr & 0x7ff)] = value;
        self.writes.push((addr & 0x7ff, value));
    }
}
struct TargetBus {
    mem: Box<[u8; 65536]>,
    writes: Vec<(u16, u8)>,
}
impl z80_emu::Bus for TargetBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[usize::from(addr)]
    }
    fn write(&mut self, addr: u16, value: u8) {
        if (0xc000..=0xc7ff).contains(&addr) {
            self.writes.push((addr - 0xc000, value));
        } else {
            assert!(
                addr == 0xcb03 || (0xdfc0..=0xdfff).contains(&addr),
                "unexpected write{addr:04X}"
            );
        }
        self.mem[usize::from(addr)] = value;
    }
    fn in_port(&mut self, port: u8) -> u8 {
        panic!("unexpected IN {port:02X}")
    }
    fn out_port(&mut self, port: u8, _value: u8) {
        panic!("unexpected OUT {port:02X}")
    }
}

#[test]
fn all_static_ram_variants_match_original_6502_without_runtime_stubs() {
    let profile=profile::load_from_str("[rom]\nname='static-test'\nmapper=4\nprg_kib=64\nchr_kib=8\n[translation]\nruntime_defines=['MMC3_FULL_RUNTIME']\n").unwrap();
    let mut vectors = 0;
    for (op_index, &(absolute, zp)) in OPS.iter().enumerate() {
        for (address_index, &(address, zero_page)) in ADDRESSES.iter().enumerate() {
            let mut code = vec![if zero_page { zp } else { absolute }, address as u8];
            if !zero_page {
                code.push((address >> 8) as u8);
            }
            let mut prg = vec![0; 32768];
            prg[..code.len()].copy_from_slice(&code);
            let routine = lift_range(
                &prg,
                &LiftOptions {
                    start: 0x8000,
                    end: 0x8000 + code.len() as u16,
                    entry_name: "static_test".into(),
                    jump_engine_sites: vec![],
                    return_escape_sites: vec![],
                    return_consume_sites: vec![],
                    materialized_call_sites: vec![],
                    window_label_prefix: None,
                    window_label_range: 0x8000..0xa000,
                    dynamic_cpu_bus: true,
                    data_regions: vec![],
                    extra_label_pcs: vec![],
                },
            )
            .unwrap();
            let mut program = z80_emit::Program::new();
            program.section("test");
            program.org(0x4000);
            lower::lower_routine(
                &mut program,
                &routine,
                &lower::LowerOptions {
                    profile: Some(&profile),
                    emit_source_comments: false,
                    routine_flag_reads: None,
                },
            )
            .unwrap();
            assert!(
                program.unresolved_labels().is_empty(),
                "op{absolute:02X}: {:?}",
                program.unresolved_labels()
            );
            let end = program.current_addr();
            let build = program.finish().unwrap();
            assert!(
                !build.asm.contains("rt_mmc3_read_bus") && !build.asm.contains("rt_mmc3_write_bus")
            );
            let mut bus = TargetBus {
                mem: Box::new([0; 65536]),
                writes: vec![],
            };
            bus.mem[0x4000..0x4000 + build.bytes.len()].copy_from_slice(&build.bytes);
            for value in 0..256 {
                bus.mem[0x3e00 + value] = (value as u8 & 0x80) | if value == 0 { 2 } else { 0 };
            }
            let mut states: Vec<_> = (0..16u8)
                .map(|seed| {
                    let edges = [0, 1, 2, 0x7f, 0x80, 0x81, 0xfe, 0xff];
                    (
                        edges[usize::from(seed & 7)],
                        seed.wrapping_mul(73),
                        edges[usize::from((seed + 3) & 7)],
                        seed,
                    )
                })
                .collect();
            if address_index == 0 {
                states.extend((0..=255u8).map(|p| (p ^ 0x96, p, p.rotate_left(3), p)));
                if op_index >= 16 {
                    states.extend(
                        (0..=255u8)
                            .flat_map(|value| [0, 1].map(move |c| (0x69, 0x3c | c, value, value))),
                    );
                }
            }
            for (a, p, value, seed) in states {
                let mut source = SourceBus {
                    ram: [0; 0x800],
                    code: code.clone(),
                    writes: vec![],
                };
                for (i, byte) in source.ram.iter_mut().enumerate() {
                    *byte = (i as u8).wrapping_mul(37) ^ seed;
                }
                source.ram[usize::from(address & 0x7ff)] = value;
                bus.mem[0xc000..0xc800].copy_from_slice(&source.ram);
                bus.writes.clear();
                let x = seed.wrapping_mul(17) ^ 0xa5;
                let y = seed.wrapping_mul(31) ^ 0x5a;
                let s = seed.wrapping_mul(19);
                bus.mem[0xcb02] = s;
                bus.mem[0xcb03] = p;
                let mut oracle = oracle_6502::Cpu::new();
                oracle.pc = 0x8000;
                oracle.a = a;
                oracle.x = x;
                oracle.y = y;
                oracle.sp = s;
                oracle.p = p;
                oracle.step(&mut source).unwrap();
                let mut cpu = z80_emu::Cpu::new();
                cpu.pc = 0x4000;
                cpu.sp = 0xdff0;
                cpu.a = a;
                cpu.f = p ^ 0xff;
                cpu.set_de(u16::from(x) << 8 | u16::from(y));
                cpu.iff1 = seed & 1 != 0;
                cpu.iff2 = cpu.iff1;
                for _ in 0..512 {
                    if cpu.pc == end {
                        break;
                    }
                    cpu.step(&mut bus).unwrap();
                }
                assert_eq!(cpu.pc, end, "op{absolute:02X} addr{address:04X}");
                assert_eq!(
                    (cpu.a, cpu.d, cpu.e, bus.mem[0xcb02], bus.mem[0xcb03]),
                    (oracle.a, oracle.x, oracle.y, oracle.sp, oracle.p),
                    "op{absolute:02X} addr{address:04X} A{a:02X} P{p:02X} data{value:02X}"
                );
                assert_eq!(&bus.mem[0xc000..0xc800], source.ram);
                // The instruction-accurate oracle omits the NMOS dummy store.
                // Add that separate source bus contract, not a claimed trace.
                let mut expected = source.writes;
                if op_index >= 16 {
                    expected.insert(0, (address & 0x7ff, value));
                }
                assert_eq!(bus.writes, expected, "op{absolute:02X} addr{address:04X}");
                assert_eq!(cpu.sp, 0xdff0);
                assert_eq!((cpu.iff1, cpu.iff2), (seed & 1 != 0, seed & 1 != 0));
                vectors += 1;
            }
        }
    }
    assert_eq!(vectors, 12928);
    eprintln!(
        "{vectors} FULL dynamic-lift original6502 vectors;22 operations;12 operand forms;no runtime stubs"
    );
}
