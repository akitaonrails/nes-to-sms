//! Canonical upper-exit stack ownership; ROMs and gameplay captures stay local.
//! CV1_EXIT_NES=/path/to/input.nes \
//! cargo test -p nes_to_sms --test cv1_upper_exit -- --ignored --nocapture

use std::path::{Path, PathBuf};

fn local_path(key: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(key).unwrap_or_else(|_| panic!("set {key}")));
    if path.is_absolute() {
        path
    } else {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path)
    }
}

struct SourceBus {
    prg: Vec<u8>,
    ram: Box<[u8; 65536]>,
    bank: usize,
}

impl SourceBus {
    fn canonical() -> Self {
        let bytes = std::fs::read(local_path("CV1_EXIT_NES")).unwrap();
        let image = nes_rom::parse(&bytes).unwrap();
        assert_eq!(image.prg.len(), 128 * 1024);
        let mut ram = Box::new([0; 65536]);
        // Minimal original exit collision state, established from the
        // from-boot transition and the original routine's input reads.
        ram[0x28] = 1; // Area used to select the original exit record.
        ram[0x3f] = 96; // Player Y; area's upper exit is at Y96.
        ram[0x38c] = 236; // Screen X inside the exit collision box.
        ram[0x42] = 1; // Timer nonzero: not the timeout/death path.
        ram[0x45] = 64; // Health positive: not the death path.
        Self {
            prg: image.prg.to_vec(),
            ram,
            bank: 6,
        }
    }
}

impl oracle_6502::Bus for SourceBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x8000..=0xbfff => self.prg[self.bank * 0x4000 + addr as usize - 0x8000],
            0xc000..=0xffff => self.prg[7 * 0x4000 + addr as usize - 0xc000],
            _ => self.ram[addr as usize],
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            self.bank = (value & self.read(addr) & 7) as usize;
        } else {
            self.ram[addr as usize] = value;
        }
    }
}

#[test]
#[ignore = "requires canonical CV1_EXIT_NES, never a committed ROM"]
fn original_upper_exit_discards_the_real_return_of_each_ordinary_caller() {
    for caller in [0xcefa, 0xc604, 0xc61a] {
        for held in [0, 1] {
            let mut bus = SourceBus::canonical();
            let fixed = &bus.prg[7 * 0x4000..];
            let jsr = cpu6502::decode_at(fixed, caller, (caller - 0xc000) as usize).unwrap();
            assert_eq!(jsr.mnemonic, cpu6502::Mnemonic::JSR);
            assert_eq!(jsr.operand, cpu6502::Operand::Addr(0x934c));
            // Give this original call a real caller beneath it, not a native
            // Z80/software-stack representation. The exit must bypass the
            // instruction following the tested JSR and return through it.
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = caller;
            cpu.sp = 0xfd;
            cpu.a = 0;
            cpu.p = 0x24;
            bus.ram[0x1fe] = 0x25;
            bus.ram[0x1ff] = 0x07;
            bus.ram[0x18] = 5;
            bus.ram[0x19] = 6;
            bus.ram[0xf7] = held;
            let mut pair = Vec::new();
            for step in 0..20_000 {
                if cpu.pc == 0x726 {
                    break;
                }
                assert_ne!(cpu.pc, caller + 3, "exit returned to the discarded caller");
                if cpu.pc == 0x9397 || cpu.pc == 0x9398 {
                    pair.push((
                        cpu.pc,
                        cpu.sp,
                        bus.ram[0x100 + cpu.sp.wrapping_add(1) as usize],
                    ));
                }
                if step == 19_999 {
                    panic!(
                        "bounded canonical caller={caller:04x} held={held} pc={:04x} bank={} pair={pair:?}",
                        cpu.pc, bus.bank
                    );
                }
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(
                pair,
                [
                    (0x9397, 0xfb, (caller + 2) as u8),
                    (0x9398, 0xfc, ((caller + 2) >> 8) as u8)
                ]
            );
            assert_eq!(
                (cpu.pc, cpu.sp, bus.bank, bus.ram[0x18], bus.ram[0x19]),
                (0x726, 0xff, 6, 8, 0)
            );
            eprintln!(
                "caller={caller:04x} held={held} original live return={:04x}, state08, outerRTS=0726",
                caller + 2
            );
        }
    }
}

#[test]
#[ignore = "requires canonical CV1_EXIT_NES"]
fn original_non_exit_path_returns_to_each_ordinary_caller() {
    for caller in [0xcefa, 0xc604, 0xc61a] {
        let mut bus = SourceBus::canonical();
        bus.ram[0x38c] = 128; // Outside the original right exit collision box.
        let mut cpu = oracle_6502::Cpu::new();
        cpu.pc = caller;
        cpu.sp = 0xfd;
        for _ in 0..20_000 {
            if cpu.pc == caller + 3 {
                break;
            }
            assert_ne!(cpu.pc, 0x9397);
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!((cpu.pc, cpu.sp), (caller + 3, 0xfd));
    }
}

#[test]
#[ignore = "requires canonical CV1_EXIT_NES"]
fn original_transition_explicitly_restarts_the_tick_counter_once() {
    let mut bus = SourceBus::canonical();
    let fixed = &bus.prg[7 * 0x4000..];
    let store = cpu6502::decode_at(fixed, 0xc3f4, 0x3f4).unwrap();
    assert_eq!(store.mnemonic, cpu6502::Mnemonic::STA);
    assert_eq!(store.operand, cpu6502::Operand::Addr(0x1a));
    bus.ram[0x18] = 8;
    bus.ram[0x19] = 1;
    bus.ram[0x25] = 3;
    bus.ram[0x1a] = 32;
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = 0xc3f2;
    cpu.sp = 0xfd;
    bus.ram[0x1fe] = 0x25;
    bus.ram[0x1ff] = 7;
    for _ in 0..100 {
        if cpu.pc == 0x726 {
            break;
        }
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.pc, 0x726);
    assert_eq!(
        (bus.ram[0x18], bus.ram[0x19], bus.ram[0x25], bus.ram[0x1a]),
        (8, 2, 2, 1)
    );
}

#[test]
#[ignore = "requires canonical CV1_EXIT_NES"]
fn original_pair_is_safe_with_nmi_before_between_and_after_its_plas() {
    for caller in [0xcefa_u16, 0xc604, 0xc61a] {
        for boundary in [0x9397, 0x9398, 0x9399] {
            let mut bus = SourceBus::canonical();
            bus.ram[0x1b] = 1;
            bus.ram[0x7f] = 1;
            bus.ram[0x1fc] = (caller + 2) as u8;
            bus.ram[0x1fd] = ((caller + 2) >> 8) as u8;
            bus.ram[0x1fe] = 0x25;
            bus.ram[0x1ff] = 7;
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0x9397;
            cpu.sp = 0xfb;
            while cpu.pc != boundary {
                cpu.step(&mut bus).unwrap();
            }
            let saved = cpu;
            cpu.nmi(&mut bus);
            for _ in 0..10_000 {
                if cpu.pc == saved.pc && cpu.sp == saved.sp {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(cpu, saved);
            for _ in 0..200 {
                if cpu.pc == 0x726 {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(
                (cpu.pc, cpu.sp, bus.bank, bus.ram[0x18], bus.ram[0x19]),
                (0x726, 0xff, 6, 8, 0)
            );
        }
    }
}
