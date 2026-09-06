//! Original-ROM evidence for the profile's split-setup replacement.
//! CV1_SPLIT_NES=/path/to/input.nes cargo test -p nes_to_sms --test cv1_split -- --ignored

use std::collections::BTreeSet;

struct SourceBus {
    prg: Vec<u8>,
    ram: Box<[u8; 65536]>,
    bank: usize,
    io: Vec<(bool, u16, u8)>,
}

impl SourceBus {
    fn canonical() -> Self {
        let path =
            std::path::PathBuf::from(std::env::var("CV1_SPLIT_NES").expect("set CV1_SPLIT_NES"));
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
        Self {
            prg: image.prg.to_vec(),
            ram: Box::new([0; 65536]),
            bank: 6,
            io: Vec::new(),
        }
    }
}

impl oracle_6502::Bus for SourceBus {
    fn read(&mut self, addr: u16) -> u8 {
        let value = match addr {
            0x8000..=0xbfff => self.prg[self.bank * 0x4000 + addr as usize - 0x8000],
            0xc000..=0xffff => self.prg[7 * 0x4000 + addr as usize - 0xc000],
            _ => self.ram[addr as usize],
        };
        if (0x2000..=0x4017).contains(&addr) {
            self.io.push((false, addr, value));
        }
        value
    }

    fn write(&mut self, addr: u16, value: u8) {
        if addr >= 0x8000 {
            // C1D8 writes back the fixed-bank byte it just read, so UxROM
            // bus conflicts cannot change the bank value on these paths.
            let rom_byte = oracle_6502::Bus::read(self, addr);
            assert_eq!(value & rom_byte, value);
            self.bank = (value & 7) as usize;
        } else {
            self.ram[addr as usize] = value;
            if (0x2000..=0x4017).contains(&addr) {
                self.io.push((true, addr, value));
            }
        }
    }
}

#[test]
#[ignore = "requires canonical CV1_SPLIT_NES, never a committed ROM"]
fn original_split_setup_has_the_retained_ppu_protocol_and_early_rts() {
    use oracle_6502::{FLAG_N, FLAG_Z};
    let mut bus = SourceBus::canonical();
    // The replacement covers exactly the true JSR callee; the adjacent scene
    // selector still owns branches to its final RTS, not to the new helper.
    let fixed = &bus.prg[7 * 0x4000..];
    let rts = cpu6502::decode_at(fixed, 0xf87c, 0xf87c - 0xc000).unwrap();
    assert_eq!(rts.mnemonic, cpu6502::Mnemonic::RTS);
    for pc in [0xf8ae, 0xf8b4, 0xf8bb] {
        assert_eq!(
            cpu6502::decode_at(fixed, pc, (pc - 0xc000) as usize)
                .unwrap()
                .branch_target(),
            Some(0xf87c)
        );
    }
    let caller = cpu6502::decode_at(fixed, 0xf8bd, 0xf8bd - 0xc000).unwrap();
    assert_eq!(caller.mnemonic, cpu6502::Mnemonic::JSR);
    assert_eq!(caller.operand, cpu6502::Operand::Addr(0xf868));
    for control in 0..=255u8 {
        for flags in [0, 0x7d, 0xff] {
            bus.io.clear();
            bus.ram[0xff] = control;
            bus.ram[0x2002] = 0xc0;
            bus.ram[0x1fe] = 6;
            bus.ram[0x1ff] = 0;
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0xf868;
            cpu.x = 0x52;
            cpu.y = 0xa9;
            cpu.p = flags;
            for _ in 0..9 {
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(
                (cpu.pc, cpu.sp, cpu.a, cpu.x, cpu.y),
                (7, 0xff, control & 0xfe, 0x52, 0xa9)
            );
            let result = control & 0xfe;
            assert_eq!(
                cpu.p,
                (flags & !(FLAG_N | FLAG_Z))
                    | (result & FLAG_N)
                    | if result == 0 { FLAG_Z } else { 0 }
            );
            assert_eq!(
                bus.io,
                [
                    (false, 0x2002, 0xc0),
                    (true, 0x2005, 0),
                    (true, 0x2005, 0),
                    (true, 0x2000, result)
                ]
            );
        }
    }
}

#[test]
#[ignore = "requires canonical CV1_SPLIT_NES, never a committed ROM"]
fn intervening_original_hud_and_sound_paths_never_consume_the_clear_observation() {
    let mut bus = SourceBus::canonical();
    let fixed = &bus.prg[7 * 0x4000..];
    // CC95's only callers in this closure supply constant stripe IDs. Even
    // an 8-bit Y wrap reads fixed ROM, never mirrored PPU registers.
    for (id, expected) in [(0x19, 0xcd6a), (0x1a, 0xcd6d), (0x2b, 0xcd65)] {
        let offset = 0xcf9 + id * 2;
        let pointer = u16::from_le_bytes([fixed[offset], fixed[offset + 1]]);
        assert_eq!(pointer, expected);
        assert!((0xc000..=0xff00).contains(&pointer));
    }
    // C1A7 receives sound ID22 only. Bank0 8187 forms table879F+3*22;
    // its first record has one channel and a ROM-only data pointer88E6.
    assert_eq!(&bus.prg[0x185..0x187], &[0x9f, 0x87]);
    assert_eq!(&bus.prg[0x805..0x808], &[0x11, 0xe6, 0x88]);
    let mut visited = BTreeSet::new();
    let mut saw_audio = false;
    // Exercise every timer phase, disabled/enabled sound and gameplay gates,
    // timer zero/low/carry, and rising/falling/disabled bars. Execute original
    // banked 6502 code including the actual sound setup, never a stubbed JSR.
    for tick in 0..=255u8 {
        for gates in 0..5 {
            for timer in [0, 1, 0x29, 0x30, 0x100u16] {
                for bars in [(0, 64), (64, 0), (0xff, 0xff)] {
                    bus.ram.fill(0);
                    bus.bank = 6;
                    bus.io.clear();
                    bus.ram[0x1a] = tick;
                    bus.ram[0x18] = if gates == 3 { 4 } else { 5 };
                    bus.ram[0x19] = 6;
                    bus.ram[0x22] = u8::from(gates == 2);
                    bus.ram[0x560] = u8::from(gates == 1);
                    bus.ram[0x7f] = u8::from(gates == 4);
                    bus.ram[0x27] = 6;
                    bus.ram[0x42] = timer as u8;
                    bus.ram[0x43] = (timer >> 8) as u8;
                    bus.ram[0x44] = bars.0;
                    bus.ram[0x45] = bars.1;
                    bus.ram[0x1aa] = bars.0;
                    bus.ram[0x1a9] = bars.1;
                    bus.ram[0x15b] = 8;
                    bus.ram[0x1fe] = 6;
                    let mut cpu = oracle_6502::Cpu::new();
                    cpu.pc = 0xa08a;
                    for _ in 0..10_000 {
                        if cpu.pc == 7 {
                            break;
                        }
                        visited.insert((if cpu.pc >= 0xc000 { 7 } else { bus.bank }, cpu.pc));
                        cpu.step(&mut bus).unwrap();
                    }
                    assert_eq!((cpu.pc, cpu.sp, bus.bank), (7, 0xff, 6));
                    for &(write, addr, _) in &bus.io {
                        assert!(
                            write && (0x4000..=0x4010).contains(&addr),
                            "unexpected I/O {write}/{addr:04x}"
                        );
                        saw_audio = true;
                    }
                }
            }
        }
    }
    for entry in [
        (6, 0xa08a),
        (6, 0xa2af),
        (6, 0xa23b),
        (7, 0xcc95),
        (7, 0xc1a7),
        (0, 0x8187),
        (0, 0x8288),
    ] {
        assert!(visited.contains(&entry), "unexercised {entry:?}");
    }
    assert!(saw_audio);
}
