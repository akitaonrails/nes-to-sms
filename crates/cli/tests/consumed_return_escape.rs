//! Guarded stack-discard contract, plus an opt-in assembled CV1/NES check.
//! CV1_ESCAPE_PROJECT=out/<candidate> CV1_ESCAPE_NES=/path/to/input.nes \
//! cargo test -p nes_to_sms --test consumed_return_escape -- --ignored

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu, FlatBus};

fn seed(bus: &mut FlatBus, ptr: u16, frame: u16, s: u8) {
    bus.mem[0xcb76..0xcb78].copy_from_slice(&ptr.to_le_bytes());
    bus.mem[frame as usize + 3] = 0x44; // owns two guest return bytes
    bus.mem[0xcb02] = s;
    bus.mem[0xcb03] = 0xa5;
    bus.mem[0xc100 + s as usize] = 0xe9;
    bus.mem[0xc100 + s.wrapping_sub(1) as usize] = 0xe5;
}

fn cpu_at(pc: u16, iff: bool) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.pc = pc;
    cpu.sp = 0xdfe0;
    cpu.a = 0x69;
    cpu.f = 0xa5;
    cpu.b = 0xe9;
    cpu.c = 0xe5;
    cpu.d = 7;
    cpu.e = 0xf9;
    cpu.iff1 = iff;
    cpu.iff2 = iff;
    cpu
}

fn step_to(bus: &mut impl Bus, cpu: &mut Cpu, stop: u16) {
    for _ in 0..10_000 {
        if cpu.pc == stop || cpu.halted || bus.read(0xcb1d) != 0 {
            return;
        }
        cpu.step(bus).unwrap();
    }
    panic!("bounded helper did not finish: pc={:04x}", cpu.pc);
}

fn stub() -> (FlatBus, u16) {
    let mut program = z80_emit::Program::new();
    program.org(0);
    program.call("rt_translated_return_discard_consumed");
    program.halt();
    validation::emit_runtime_helpers(&mut program);
    let mut bus = FlatBus::new();
    bus.load(0, &program.finish().unwrap().bytes);
    (bus, 3)
}

#[test]
fn stub_preserves_guest_state_and_handles_stack_segment_and_page_wraps() {
    for (ptr, frame) in [
        (0xd304, 0xd300),
        (0xd500, 0xd3f8),
        (0xd504, 0xd500),
        (0xd600, 0xd5fc),
    ] {
        for s in [0, 1, 0xf4, 0xff] {
            for iff in [false, true] {
                let (mut bus, stop) = stub();
                seed(&mut bus, ptr, frame, s);
                let mut cpu = cpu_at(0, iff);
                step_to(&mut bus, &mut cpu, stop);
                assert_eq!(cpu.pc, stop);
                assert_eq!(bus.mem[0xcb1d], 0);
                assert_eq!(&bus.mem[0xcb76..0xcb78], &frame.to_le_bytes());
                assert_eq!(bus.mem[0xcb02], s);
                assert_eq!(bus.mem[0xcb03], 0xa5);
                assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (0x69, 0xa5, 7, 0xf9));
                assert_eq!(cpu.sp, 0xdfe0);
                assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
            }
        }
    }
}

#[test]
fn stub_rejects_malformed_frames_and_mismatched_consumed_bytes() {
    for (ptr, flags, high, low) in [
        (0xd300, 0x44, 0xe9, 0xe5),
        (0xd301, 0x44, 0xe9, 0xe5),
        (0xd3fc, 0x44, 0xe9, 0xe5),
        (0xd501, 0x44, 0xe9, 0xe5),
        (0xd601, 0x44, 0xe9, 0xe5),
        (0xe004, 0x44, 0xe9, 0xe5),
        (0xd304, 4, 0xe9, 0xe5),
        (0xd304, 0x44, 0xe8, 0xe5),
        (0xd304, 0x44, 0xe9, 0xe4),
    ] {
        let (mut bus, stop) = stub();
        seed(&mut bus, ptr, 0xd300, 0xf4);
        bus.mem[0xd303] = flags;
        bus.mem[0xc1f4] = high;
        bus.mem[0xc1f3] = low;
        let mut cpu = cpu_at(0, true);
        step_to(&mut bus, &mut cpu, stop);
        assert_eq!(bus.mem[0xcb1d], 0xe5, "ptr={ptr:04x} flags={flags:02x}");
        assert_eq!(&bus.mem[0xcb76..0xcb78], &ptr.to_le_bytes());
        assert_eq!(bus.mem[0xcb02], 0xf4);
    }
}

struct Assembled {
    rom: Vec<u8>,
    bus: FlatBus,
    symbols: HashMap<String, (u8, u16)>,
}

fn local_path(key: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(key).unwrap_or_else(|_| panic!("set {key}")));
    if path.is_absolute() {
        path
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }
}

impl Assembled {
    fn new() -> Self {
        let dir = local_path("CV1_ESCAPE_PROJECT");
        let mut symbols = HashMap::new();
        for line in std::fs::read_to_string(dir.join("sms.sym"))
            .unwrap()
            .lines()
        {
            let mut fields = line.split_whitespace();
            if let (Some(location), Some(name)) = (fields.next(), fields.next())
                && let Some((bank, addr)) = location.split_once(':')
                && let (Ok(bank), Ok(addr)) =
                    (u8::from_str_radix(bank, 16), u16::from_str_radix(addr, 16))
            {
                symbols.insert(name.into(), (bank, addr));
            }
        }
        Self {
            rom: std::fs::read(dir.join("sms.sms")).unwrap(),
            bus: FlatBus::new(),
            symbols,
        }
    }

    fn enter(&mut self, name: &str, iff: bool) -> Cpu {
        let (bank, pc) = self.symbols[name];
        self.bus.mem[0xfffe] = bank;
        self.bus.mem[0xcb14] = bank;
        cpu_at(pc, iff)
    }
}

impl Bus for Assembled {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0..=0x3fff => self.rom[addr as usize],
            0x4000..=0x7fff => {
                self.rom[self.bus.mem[0xfffe] as usize * 0x4000 + addr as usize - 0x4000]
            }
            0x8000..=0xbfff => {
                self.rom[self.bus.mem[0xffff] as usize * 0x4000 + addr as usize - 0x8000]
            }
            _ => self.bus.mem[addr as usize],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        assert!(addr >= 0xc000, "unexpected ROM/SRAM write {addr:04x}");
        self.bus.mem[addr as usize] = value;
    }
}

#[test]
#[ignore = "requires a Docker-assembled CV1_ESCAPE_PROJECT"]
fn assembled_helper_matches_stub_guards_and_preservation() {
    for (ptr, frame) in [
        (0xd304, 0xd300),
        (0xd500, 0xd3f8),
        (0xd504, 0xd500),
        (0xd600, 0xd5fc),
    ] {
        for s in [0, 1, 0xf4, 0xff] {
            for iff in [false, true] {
                for defect in 0..4 {
                    let mut machine = Assembled::new();
                    seed(&mut machine.bus, ptr, frame, s);
                    match defect {
                        1 => machine.bus.mem[frame as usize + 3] = 4,
                        2 => machine.bus.mem[0xc100 + s as usize] ^= 1,
                        3 => machine.bus.mem[0xcb76] |= 1,
                        _ => {}
                    }
                    let expected_ptr = machine.bus.mem[0xcb76..0xcb78].to_vec();
                    let mut cpu = machine.enter("rt_translated_return_discard_consumed", iff);
                    machine.bus.mem[0xdfe0] = 7;
                    step_to(&mut machine, &mut cpu, 7);
                    assert_eq!(machine.bus.mem[0xcb02], s);
                    if defect == 0 {
                        assert_eq!(cpu.pc, 7);
                        assert_eq!(machine.bus.mem[0xcb1d], 0);
                        assert_eq!(&machine.bus.mem[0xcb76..0xcb78], &frame.to_le_bytes());
                        assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (0x69, 0xa5, 7, 0xf9));
                        assert_eq!(cpu.sp, 0xdfe2);
                        assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                    } else {
                        assert_eq!(machine.bus.mem[0xcb1d], 0xe5);
                        assert_eq!(&machine.bus.mem[0xcb76..0xcb78], expected_ptr);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_ESCAPE_PROJECT and the original CV1_ESCAPE_NES"]
fn actual_ee94_path_matches_canonical_6502_and_discards_continuation_once() {
    let nes = std::fs::read(local_path("CV1_ESCAPE_NES")).unwrap();
    let image = nes_rom::parse(&nes).unwrap();
    assert_eq!(image.prg.len(), 128 * 1024);
    for slot in [4u8, 7, 0x16] {
        for scroll in [0, 1, 5] {
            let mut oracle_bus = oracle_6502::FlatBus::new();
            oracle_bus.load(0xc000, &image.prg[7 * 0x4000..]);
            oracle_bus.ram[0x3c] = scroll;
            oracle_bus.ram[0x49] = 1;
            oracle_bus.ram[0x038c + slot as usize] = 0xf7;
            oracle_bus.ram[0x1f3] = 0xe5;
            oracle_bus.ram[0x1f4] = 0xe9;
            let mut oracle = oracle_6502::Cpu::new();
            oracle.pc = 0xee94;
            oracle.sp = 0xf2;
            oracle.x = slot;
            oracle.p = 0xa5;
            for _ in 0..100 {
                if oracle.pc == 0xea33 {
                    break;
                }
                oracle.step(&mut oracle_bus).unwrap();
            }
            assert_eq!((oracle.pc, oracle.sp), (0xea33, 0xf4));

            let mut machine = Assembled::new();
            seed(&mut machine.bus, 0xd308, 0xd304, 0xf4);
            machine.bus.mem[0xcb02] = 0xf2;
            machine.bus.mem[0xc03c] = scroll;
            machine.bus.mem[0xc049] = 1;
            machine.bus.mem[0xc38c + slot as usize] = 0xf7;
            // This is the E959 continuation that the real PLA/PLA cancels.
            let (bank, continuation) = machine.symbols["L_E9E6"];
            machine.bus.mem[0xd305..0xd307].copy_from_slice(&continuation.to_le_bytes());
            machine.bus.mem[0xd307] = bank | 0x40;
            let mut cpu = machine.enter("L_EE94", true);
            cpu.d = slot;
            let (bank, stop) = machine.symbols["L_EA33"];
            step_to(&mut machine, &mut cpu, stop);
            assert_eq!((cpu.pc, machine.bus.mem[0xcb14]), (stop, bank));
            assert_eq!(machine.bus.mem[0xcb1d], 0);
            assert_eq!(machine.bus.mem[0xcb02], oracle.sp);
            assert_eq!((cpu.a, cpu.d, cpu.e), (oracle.a, oracle.x, oracle.y));
            assert_eq!(
                &machine.bus.mem[0xcb76..0xcb78],
                &0xd304u16.to_le_bytes(),
                "the E959 software continuation must be gone before EA33"
            );
            for addr in [
                0x16,
                0x17,
                0x4f,
                0x300 + slot as usize,
                0x38c + slot as usize,
            ] {
                assert_eq!(
                    machine.bus.mem[0xc000 + addr],
                    oracle_bus.ram[addr],
                    "RAM {addr:04x}"
                );
            }
        }
    }
}
