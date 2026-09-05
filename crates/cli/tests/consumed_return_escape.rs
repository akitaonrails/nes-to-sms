//! Guarded stack-discard contract, plus an opt-in assembled CV1/NES check.
//! CV1_ESCAPE_PROJECT=out/<candidate> CV1_ESCAPE_NES=/path/to/input.nes \
//! cargo test -p nes_to_sms --test consumed_return_escape -- --ignored
//! The IRQ regression also needs CV1_ESCAPE_OLD_PROJECT pointing to the
//! preserved pre-fix artifact; it must reproduce the late freed-byte E5.

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu, FlatBus};

fn seed(bus: &mut FlatBus, ptr: u16, frame: u16, s: u8) {
    bus.mem[0xcb76..0xcb78].copy_from_slice(&ptr.to_le_bytes());
    bus.mem[frame as usize + 3] = 0x44; // owns two guest return bytes
    bus.mem[0xcb02] = s;
    bus.mem[0xcb03] = 0xa5;
    bus.mem[0xc100 + s.wrapping_add(2) as usize] = 0xe9;
    bus.mem[0xc100 + s.wrapping_add(1) as usize] = 0xe5;
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

fn step_to(bus: &mut impl Bus, cpu: &mut Cpu, stop: u16) -> u16 {
    let mut minimum_sp = cpu.sp;
    for _ in 0..10_000 {
        minimum_sp = minimum_sp.min(cpu.sp);
        assert!(cpu.sp >= 0xde40, "helper crossed the occupied BG metadata");
        if cpu.pc == stop || cpu.halted || bus.read(0xcb1d) != 0 {
            return minimum_sp;
        }
        cpu.step(bus).unwrap();
    }
    panic!("bounded helper did not finish: pc={:04x}", cpu.pc);
}

fn stub() -> (FlatBus, u16) {
    let mut program = z80_emit::Program::new();
    program.org(0);
    program.call("rt_translated_return_consume");
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
fn stub_rejects_malformed_frames_and_mismatched_live_bytes() {
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
        bus.mem[0xc1f6] = high;
        bus.mem[0xc1f5] = low;
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
        Self::at_project(dir)
    }

    fn at_project(dir: PathBuf) -> Self {
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

    fn diagnostic_counter_enabled(&self) -> bool {
        self.symbols.contains_key("_tr_escape_consumed_success")
    }

    fn seed_mapping(&mut self, sram: u8) -> Vec<u8> {
        for (address, value) in [
            (0xfffc, sram),
            (0xfffe, 1),
            (0xffff, 2),
            (0xcb14, 1),
            (0xcb62, 3),
            (0xd47f, 2),
        ] {
            self.bus.mem[address] = value;
        }
        self.bus.mem[0xca19..0xca20].copy_from_slice(&[1, 0, 3, 3, 0, 8, 2]);
        self.mapping()
    }

    fn mapping(&self) -> Vec<u8> {
        [0xfffc, 0xfffe, 0xffff, 0xcb14, 0xcb62, 0xd47f]
            .into_iter()
            .map(|a| self.bus.mem[a])
            .chain(self.bus.mem[0xca19..0xca20].iter().copied())
            .collect()
    }

    fn seed_canonical_path(&mut self) -> Cpu {
        seed(&mut self.bus, 0xd308, 0xd304, 0xf2);
        self.bus.mem[0xc049] = 1;
        self.bus.mem[0xc393] = 0xf7;
        let (bank, continuation) = self.symbols["L_E9E6"];
        self.bus.mem[0xd305..0xd307].copy_from_slice(&continuation.to_le_bytes());
        self.bus.mem[0xd307] = bank | 0x40;
        for (addr, value) in [
            (0xc01b, 1),
            (0xc07f, 1),
            (0xc0fe, 0x1e),
            (0xc0ff, 0xb0),
            (0xcb28, 1),
            (0xcb08, 0xb0),
            (0xcb09, 0x1e),
            (0xca11, 1),
            (0xcb1a, 1),
            (0xcb62, 3),
            (0xd47d, 0xc0),
            (0xd47e, 0xd4),
        ] {
            self.bus.mem[addr] = value;
        }
        self.bus.mem[0xffff] = self.symbols["data_prg_bank_3"].0;
        let mut cpu = self.enter("L_EE94", true);
        cpu.d = 7;
        cpu.e = 0;
        cpu
    }

    fn interrupt(&mut self, cpu: &mut Cpu) -> u16 {
        assert!(cpu.iff1 && cpu.ei_pending == 0);
        let (resume, sp, bank) = (cpu.pc, cpu.sp, self.bus.mem[0xfffe]);
        let shadow_bank = self.bus.mem[0xcb14];
        let (bc, hl) = ((cpu.b, cpu.c), (cpu.h, cpu.l));
        let ptr = self.bus.mem[0xcb76..0xcb78].to_vec();
        let (a, f, d, e, s, p) = (
            cpu.a,
            cpu.f,
            cpu.d,
            cpu.e,
            self.bus.mem[0xcb02],
            self.bus.mem[0xcb03],
        );
        cpu.sp -= 2;
        self.write(cpu.sp, resume as u8);
        self.write(cpu.sp + 1, (resume >> 8) as u8);
        cpu.pc = 0x38;
        cpu.iff1 = false;
        cpu.iff2 = false;
        let mut minimum = cpu.sp;
        for _ in 0..100_000 {
            minimum = minimum.min(cpu.sp);
            assert!(minimum >= 0xde40);
            assert_eq!(self.bus.mem[0xcb1d], 0, "nested IRQ trap at {:04x}", cpu.pc);
            if cpu.pc == resume && cpu.sp == sp {
                assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (a, f, d, e));
                assert_eq!(((cpu.b, cpu.c), (cpu.h, cpu.l)), (bc, hl));
                assert_eq!(self.bus.mem[0xcb14], shadow_bank);
                // The fixed gate publishes CB14 immediately before FFFE.
                // An IRQ in that legal interval restores the published bank;
                // its next resumed instruction writes that same bank again.
                if bank != shadow_bank {
                    let (gate_bank, gate) = self.symbols["rt_translated_tail_gate"];
                    assert_eq!((gate_bank, resume), (0, gate + 3));
                    assert_eq!(
                        &self.rom[gate as usize..gate as usize + 6],
                        &[0x32, 0x14, 0xcb, 0x32, 0xfe, 0xff]
                    );
                    assert_eq!(a, shadow_bank);
                }
                assert_eq!(self.bus.mem[0xfffe], shadow_bank);
                assert_eq!((self.bus.mem[0xcb02], self.bus.mem[0xcb03]), (s, p));
                assert_eq!(&self.bus.mem[0xcb76..0xcb78], ptr);
                assert!(cpu.iff1 && cpu.iff2);
                return minimum;
            }
            cpu.step(self).unwrap();
        }
        panic!(
            "IRQ did not return to {bank:02x}:{resume:04x}; pc={:04x} sp={:04x}/{sp:04x} bank={} depth={} S={:02x} PTR={:02x?}",
            cpu.pc,
            cpu.sp,
            self.bus.mem[0xfffe],
            self.bus.mem[0xca11],
            self.bus.mem[0xcb02],
            &self.bus.mem[0xcb76..0xcb78]
        );
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
    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0xbf => 0x80,
            0x7e => 0xe0,
            _ => 0xff,
        }
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
                for defect in 0..5 {
                    let mut machine = Assembled::new();
                    seed(&mut machine.bus, ptr, frame, s);
                    match defect {
                        1 => machine.bus.mem[frame as usize + 3] = 4,
                        2 => machine.bus.mem[0xc100 + s.wrapping_add(2) as usize] ^= 1,
                        3 => machine.bus.mem[0xcb76] |= 1,
                        4 => machine.bus.mem[0xc100 + s.wrapping_add(1) as usize] ^= 1,
                        _ => {}
                    }
                    let expected_ptr = machine.bus.mem[0xcb76..0xcb78].to_vec();
                    let mut cpu = machine.enter("rt_translated_return_consume", iff);
                    let mapping = machine.seed_mapping(if iff { 8 } else { 0 });
                    cpu.sp = 0xde44;
                    machine.bus.mem[0xde44] = 7;
                    machine.bus.mem[0xdd80..0xde40].fill(0x5a);
                    let minimum_sp = step_to(&mut machine, &mut cpu, 7);
                    assert_eq!(machine.bus.mem[0xcb02], s);
                    assert_eq!(machine.bus.mem[0xcb03], 0xa5);
                    assert_eq!(machine.mapping(), mapping);
                    assert!(machine.bus.mem[0xdd80..0xde40].iter().all(|v| *v == 0x5a));
                    assert_eq!(
                        machine.bus.mem[0xc81f],
                        u8::from(defect == 0 && machine.diagnostic_counter_enabled()),
                        "only a successful consumed pop may be counted"
                    );
                    if defect == 0 {
                        assert_eq!(cpu.pc, 7);
                        assert_eq!(machine.bus.mem[0xcb1d], 0);
                        assert_eq!(&machine.bus.mem[0xcb76..0xcb78], &frame.to_le_bytes());
                        assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (0x69, 0xa5, 7, 0xf9));
                        assert_eq!(cpu.sp, 0xde46);
                        assert_eq!(minimum_sp, 0xde40, "counter adds no native word");
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
#[ignore = "requires a Docker-assembled CV1_ESCAPE_PROJECT"]
fn assembled_ordinary_escape_never_counts_a_consumed_pop() {
    for iff in [false, true] {
        for sram in [0, 8] {
            let mut machine = Assembled::new();
            seed(&mut machine.bus, 0xd304, 0xd300, 0);
            machine.bus.mem[0xd303] = 4; // ordinary unmaterialized software frame
            let mut cpu = machine.enter("rt_translated_return_escape", iff);
            let mapping = machine.seed_mapping(sram);
            cpu.sp = 0xde42;
            machine.bus.mem[0xde42] = 7;
            assert_eq!(step_to(&mut machine, &mut cpu, 7), 0xde40);
            assert_eq!(cpu.pc, 7);
            assert_eq!(machine.bus.mem[0xcb1d], 0);
            assert_eq!(machine.bus.mem[0xc81f], 0);
            assert_eq!(machine.bus.mem[0xcb02], 0xfe);
            assert_eq!(machine.bus.mem[0xc100], 0xe9);
            assert_eq!(machine.bus.mem[0xc1ff], 0xe5);
            assert_eq!(&machine.bus.mem[0xcb76..0xcb78], &0xd300u16.to_le_bytes());
            assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (0x69, 0xa5, 7, 0xf9));
            assert_eq!((cpu.iff1, cpu.iff2, cpu.sp), (iff, iff, 0xde44));
            assert_eq!(machine.mapping(), mapping);
        }
    }
}

#[test]
#[ignore = "requires a Docker-assembled CV1_ESCAPE_PROJECT"]
fn assembled_success_counter_is_after_publication_and_wraps_without_extra_stack() {
    let mut machine = Assembled::new();
    seed(&mut machine.bus, 0xd304, 0xd300, 0xf4);
    let mut cpu = machine.enter("rt_translated_return_consume", true);
    let mapping = machine.seed_mapping(8);
    machine.bus.mem[0xc81f] = 0xff;
    machine.bus.mem[0xdfe0] = 7;
    if let Some(&(bank, site)) = machine.symbols.get("_tr_escape_consumed_success") {
        assert_eq!(bank, 0);
        step_to(&mut machine, &mut cpu, site);
        assert_eq!(cpu.pc, site);
        assert_eq!(&machine.bus.mem[0xcb76..0xcb78], &0xd300u16.to_le_bytes());
        assert_eq!(machine.bus.mem[0xc81f], 0xff);
        // Exactly LD A,(nn), INC A, LD(nn),A: 13+4+13T, no stack operation.
        assert_eq!(
            &machine.rom[site as usize..site as usize + 7],
            &[0x3a, 0x1f, 0xc8, 0x3c, 0x32, 0x1f, 0xc8]
        );
        assert!(!cpu.iff1 && !cpu.iff2);
        assert_eq!(cpu.sp, 0xdfde); // original AF still owns its existing word
    }
    step_to(&mut machine, &mut cpu, 7);
    assert_eq!(cpu.pc, 7);
    assert_eq!(
        machine.bus.mem[0xc81f],
        if machine.diagnostic_counter_enabled() {
            0
        } else {
            0xff
        }
    );
    assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), (0x69, 0xa5, 7, 0xf9));
    assert_eq!((cpu.iff1, cpu.iff2, cpu.sp), (true, true, 0xdfe2));
    assert_eq!(machine.mapping(), mapping);
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
            seed(&mut machine.bus, 0xd308, 0xd304, 0xf2);
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
            assert_eq!(
                machine.bus.mem[0xc81f],
                u8::from(machine.diagnostic_counter_enabled()),
                "the canonical PLA/PLA path must count exactly one consumed pop"
            );
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

#[test]
#[ignore = "requires the original CV1_ESCAPE_NES"]
fn canonical_nmi_may_reuse_return_bytes_after_each_consuming_pla() {
    let nes = std::fs::read(local_path("CV1_ESCAPE_NES")).unwrap();
    let image = nes_rom::parse(&nes).unwrap();
    for boundary in [0xee9f, 0xeea0, 0xeea1, 0xeea4] {
        let mut bus = oracle_6502::FlatBus::new();
        bus.load(0x8000, &image.prg[..0x4000]);
        bus.load(0xc000, &image.prg[7 * 0x4000..]);
        bus.ram[0x1b] = 1; // canonical lag NMI, not a second game-body entry
        bus.ram[0x7f] = 1; // canonical guard skips shared audio
        bus.ram[0x49] = 1;
        bus.ram[0x393] = 0xf7;
        bus.ram[0x1f3] = 0xe5;
        bus.ram[0x1f4] = 0xe9;
        let mut cpu = oracle_6502::Cpu::new();
        cpu.pc = 0xee94;
        cpu.sp = 0xf2;
        cpu.x = 7;
        cpu.p = 0xa5;
        for _ in 0..100 {
            if cpu.pc == boundary {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.pc, boundary);
        let resumed = cpu;
        cpu.nmi(&mut bus);
        assert_eq!(cpu.pc, 0xc052);
        for _ in 0..10_000 {
            if cpu.pc == resumed.pc && cpu.sp == resumed.sp {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(
            cpu, resumed,
            "canonical NMI preserves guest at {boundary:04x}"
        );
        for _ in 0..100 {
            if cpu.pc == 0xea33 {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!((cpu.pc, cpu.sp, cpu.a, cpu.x), (0xea33, 0xf4, 0xf7, 7));
        if boundary >= 0xeea1 {
            assert_eq!(bus.ram[0x1f4], (boundary >> 8) as u8);
            assert_eq!(bus.ram[0x1f3], boundary as u8);
        }
    }
}

#[test]
#[ignore = "requires old CV1_ESCAPE_OLD_PROJECT and new CV1_ESCAPE_PROJECT"]
fn every_legal_z80_boundary_preserves_live_transfer_and_reproduces_old_late_check() {
    let mut old = Assembled::at_project(local_path("CV1_ESCAPE_OLD_PROJECT"));
    let mut old_cpu = old.seed_canonical_path();
    let old_helper = old.symbols["rt_translated_return_discard_consumed"].1;
    step_to(&mut old, &mut old_cpu, old_helper);
    assert_eq!((old_cpu.pc, old.bus.mem[0xcb02]), (old_helper, 0xf4));
    assert_eq!(&old.bus.mem[0xc1f3..0xc1f5], &[0xe5, 0xe9]);
    old.interrupt(&mut old_cpu);
    assert_eq!(&old.bus.mem[0xc1f3..0xc1f5], &[0xff, 0xff]);
    let old_stop = old.symbols["L_EA33"].1;
    step_to(&mut old, &mut old_cpu, old_stop);
    assert_eq!(
        old.bus.mem[0xcb1d], 0xe5,
        "old artifact must reproduce the actual failure"
    );
    assert_eq!(&old.bus.mem[0xcb76..0xcb78], &0xd308u16.to_le_bytes());

    let mut reference = Assembled::new();
    let mut cpu = reference.seed_canonical_path();
    let stop = reference.symbols["L_EA33"].1;
    let live_helper = reference.symbols["rt_translated_return_consume"].1;
    step_to(&mut reference, &mut cpu, live_helper);
    let resume = u16::from_le_bytes([reference.read(cpu.sp), reference.read(cpu.sp + 1)]);
    let first = resume - 6; // emitted LD BC,expected / CALL helper before PLA
    assert_eq!(
        (reference.read(first), reference.read(first + 3)),
        (0x01, 0xcd)
    );
    let mut reference = Assembled::new();
    let mut cpu = reference.seed_canonical_path();
    let mut legal = Vec::new();
    let mut in_consuming_block = false;
    for step in 0..1000 {
        if cpu.pc == stop {
            break;
        }
        in_consuming_block |= cpu.pc == first;
        if in_consuming_block && cpu.iff1 && cpu.ei_pending == 0 {
            legal.push(step);
        }
        cpu.step(&mut reference).unwrap();
    }
    assert_eq!(cpu.pc, stop);
    let expected = (
        cpu.a,
        cpu.d,
        cpu.e,
        reference.bus.mem[0xcb02],
        reference.bus.mem[0xcb03],
    );
    let mut lowest = 0xffff;
    for &at in &legal {
        let mut machine = Assembled::new();
        let mut cpu = machine.seed_canonical_path();
        for _ in 0..at {
            cpu.step(&mut machine).unwrap();
        }
        lowest = lowest.min(machine.interrupt(&mut cpu));
        step_to(&mut machine, &mut cpu, stop);
        assert_eq!(cpu.pc, stop, "injection step {at}");
        assert_eq!(machine.bus.mem[0xcb1d], 0);
        assert_eq!(
            (
                cpu.a,
                cpu.d,
                cpu.e,
                machine.bus.mem[0xcb02],
                machine.bus.mem[0xcb03]
            ),
            expected
        );
        assert_eq!(&machine.bus.mem[0xcb76..0xcb78], &0xd304u16.to_le_bytes());
        assert_eq!(
            machine.bus.mem[0xc81f],
            u8::from(machine.diagnostic_counter_enabled())
        );
        assert_eq!(cpu.sp, 0xdfe0);
        assert_eq!(machine.bus.mem[0xcb14], machine.symbols["L_EA33"].0);
        assert_eq!(machine.bus.mem[0xfffe], machine.symbols["L_EA33"].0);
    }
    assert!(
        legal.len() >= 35,
        "exercise lowered helper/PLA/load/tail boundaries"
    );
    eprintln!(
        "actual IRQ boundary matrix: {} legal boundaries, min SP={lowest:04x}",
        legal.len()
    );
}
