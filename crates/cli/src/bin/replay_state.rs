//! Transplant a Mednafen SMS savestate (regs + work RAM + mapper banks)
//! into z80_emu and step it, logging the PC path. Diagnostic tool for
//! divergences between real-emulator behavior and the in-house harness:
//! if Mednafen is visibly stuck in a loop, replaying its exact state here
//! either reproduces the loop (mapping it) or diverges (exposing a
//! z80_emu semantics bug).
//!
//! Usage: replay-state <rom.sms> <ram.bin> <regs.json> [steps]

use z80_emu::{Bus, Cpu};

struct MiniSms {
    rom: Vec<u8>,
    ram: [u8; 0x2000],
    fcr: [u8; 4],
    io_log: Vec<(u16, u8, u8)>, // (pc-ish index, port, value)
}

impl MiniSms {
    fn rom_byte(&self, bank: usize, offset: usize) -> u8 {
        let idx = bank * 0x4000 + offset;
        if idx < self.rom.len() {
            self.rom[idx]
        } else {
            0xFF
        }
    }
}

impl Bus for MiniSms {
    fn read(&mut self, addr: u16) -> u8 {
        let a = addr as usize;
        match a {
            0x0000..=0x03FF => self.rom_byte(0, a),
            0x0400..=0x3FFF => self.rom_byte(self.fcr[1] as usize, a),
            0x4000..=0x7FFF => self.rom_byte(self.fcr[2] as usize, a - 0x4000),
            0x8000..=0xBFFF => self.rom_byte(self.fcr[3] as usize, a - 0x8000),
            _ => self.ram[(a - 0xC000) & 0x1FFF],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        let a = addr as usize;
        if a >= 0xC000 {
            self.ram[(a - 0xC000) & 0x1FFF] = value;
        }
        match a {
            0xFFFC => self.fcr[0] = value,
            0xFFFD => self.fcr[1] = value,
            0xFFFE => self.fcr[2] = value,
            0xFFFF => self.fcr[3] = value,
            _ => {}
        }
    }
    fn in_port(&mut self, _port: u8) -> u8 {
        // VDP status / controllers: return 0 (no vblank, no buttons).
        0x00
    }
    fn out_port(&mut self, port: u8, value: u8) {
        if self.io_log.len() < 64 {
            self.io_log.push((0, port, value));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom = std::fs::read(&args[1]).expect("rom");
    let ram_src = std::fs::read(&args[2]).expect("ram");
    // regs file: lines of `NAME=hex` (AF, BC, DE, HL, AF_, BC_, DE_, HL_,
    // SP, PC, IFF1, FCR0..FCR3).
    let mut regs = std::collections::HashMap::new();
    for line in std::fs::read_to_string(&args[3]).expect("regs").lines() {
        if let Some((k, v)) = line.split_once('=') {
            regs.insert(
                k.trim().to_string(),
                u16::from_str_radix(v.trim(), 16).expect("hex"),
            );
        }
    }
    let steps: usize = args.get(4).map(|s| s.parse().unwrap()).unwrap_or(400);

    let mut ram = [0u8; 0x2000];
    ram.copy_from_slice(&ram_src[..0x2000]);
    let g = |k: &str| regs[k];

    let mut bus = MiniSms {
        rom,
        ram,
        fcr: [
            regs["FCR0"] as u8,
            regs["FCR1"] as u8,
            regs["FCR2"] as u8,
            regs["FCR3"] as u8,
        ],
        io_log: Vec::new(),
    };
    let mut cpu = Cpu::new();
    let af = g("AF");
    cpu.a = (af >> 8) as u8;
    cpu.f = (af & 0xFF) as u8;
    let bc = g("BC");
    cpu.b = (bc >> 8) as u8;
    cpu.c = (bc & 0xFF) as u8;
    let de = g("DE");
    cpu.d = (de >> 8) as u8;
    cpu.e = (de & 0xFF) as u8;
    let hl = g("HL");
    cpu.h = (hl >> 8) as u8;
    cpu.l = (hl & 0xFF) as u8;
    cpu.sp = g("SP");
    cpu.pc = g("PC");
    cpu.af_shadow = g("AF_");
    cpu.bc_shadow = g("BC_");
    cpu.de_shadow = g("DE_");
    cpu.hl_shadow = g("HL_");
    cpu.iff1 = regs["IFF1"] != 0;
    cpu.iff2 = cpu.iff1;

    println!(
        "start: PC=${:04X} SP=${:04X} fcr={:02X?}",
        cpu.pc, cpu.sp, bus.fcr
    );
    let mut last_pcs: Vec<u16> = Vec::new();
    for i in 0..steps {
        let pc = cpu.pc;
        let op = bus.read(pc);
        if i < 200 || steps - i <= 40 {
            println!(
                "{i:5}  PC=${pc:04X} op={op:02X} SP=${:04X} A={:02X} BC={:02X}{:02X} HL={:02X}{:02X}",
                cpu.sp, cpu.a, cpu.b, cpu.c, cpu.h, cpu.l
            );
        }
        last_pcs.push(pc);
        if let Err(e) = cpu.step(&mut bus) {
            println!("STOP at step {i}: {e:?} (PC=${pc:04X} op={op:02X})");
            break;
        }
    }
    println!(
        "end: PC=${:04X} SP=${:04X} fcr={:02X?}",
        cpu.pc, cpu.sp, bus.fcr
    );
}
