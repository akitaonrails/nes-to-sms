//! Actual Docker-assembled CV1 background ownership, not a renderer model.
//! CV1_BG_PROJECT=out/<isolated-project> cargo test -p nes_to_sms --test bg_commit -- --ignored
use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    ram: Box<[u8; 65536]>,
    sram: Box<[u8; 16384]>,
    vram: Box<[u8; 16384]>,
    cram: [u8; 32],
    regs: [u8; 16],
    labels: HashMap<String, (u8, u16)>,
    latch: Option<u8>,
    address: usize,
    code: u8,
    ports: usize,
    commit: bool,
    max_step: u64,
    blank_requests: usize,
    counters: Vec<u8>,
    counter_reads: usize,
    allow_service: bool,
    last_tstates: u64,
    phase_max: [u64; 16],
    physical_clock: Option<u64>,
    vdp_status: u8,
}
impl Machine {
    fn new() -> Self {
        let path = PathBuf::from(std::env::var("CV1_BG_PROJECT").expect("set CV1_BG_PROJECT"));
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let mut labels = HashMap::new();
        for line in std::fs::read_to_string(path.join("sms.sym"))
            .unwrap()
            .lines()
        {
            let mut words = line.split_whitespace();
            if let (Some(address), Some(label)) = (words.next(), words.next())
                && let Some((bank, offset)) = address.split_once(':')
                && let (Ok(bank), Ok(offset)) = (
                    u8::from_str_radix(bank, 16),
                    u16::from_str_radix(offset, 16),
                )
            {
                labels.insert(label.to_owned(), (bank, offset));
            }
        }
        let mut m = Self {
            rom: std::fs::read(path.join("sms.sms")).unwrap(),
            ram: Box::new([0; 65536]),
            sram: Box::new([0; 16384]),
            vram: Box::new([0; 16384]),
            cram: [0; 32],
            regs: [0; 16],
            labels,
            latch: None,
            address: 0,
            code: 0,
            ports: 0,
            commit: false,
            max_step: 0,
            blank_requests: 0,
            counters: Vec::new(),
            counter_reads: 0,
            allow_service: false,
            last_tstates: 0,
            phase_max: [0; 16],
            physical_clock: None,
            vdp_status: 0x80,
        };
        m.ram[0xdfff] = m.labels["data_prg_bank_3"].0;
        m.ram[0xcb62] = 3;
        m.ram[0xcb03] = 0x65;
        m.ram[0xc821] = 0x90;
        m.ram[0xc822] = 0x1e;
        m.ram[0xcb08] = 0x90;
        m.ram[0xcb09] = 0x1e;
        m.ram[0xca13] = 0xff;
        m.call("rt_cv1_bg_init", 0, 0, 0);
        m.cram.copy_from_slice(&m.sram[0x33a0..0x33c0]);
        for (i, byte) in m.sram[0x800..0x2800].iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(29) ^ (i >> 4) as u8;
        }
        m
    }
    fn call(&mut self, name: &str, a: u8, bc: u16, de: u16) -> Cpu {
        self.call_hl(name, a, bc, de, 0)
    }
    fn call_hl(&mut self, name: &str, a: u8, bc: u16, de: u16, hl: u16) -> Cpu {
        let (bank, address) = self.labels[name];
        assert_eq!(bank, 0, "fixed code required: {name}");
        let mut cpu = Cpu::new();
        cpu.pc = address;
        cpu.sp = 0xdff0;
        cpu.a = a;
        cpu.b = (bc >> 8) as u8;
        cpu.c = bc as u8;
        cpu.d = (de >> 8) as u8;
        cpu.e = de as u8;
        cpu.h = (hl >> 8) as u8;
        cpu.l = hl as u8;
        self.ram[0xdff0] = 7;
        self.ram[0xdff1] = 0;
        self.last_tstates = 0;
        let phase = self.sram[0x33e1] as usize;
        let mapping = [0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| self.ram[p]);
        for _ in 0..1_000_000 {
            if cpu.pc == 7 {
                assert_eq!(cpu.sp, 0xdff2);
                assert_eq!(self.ram[0xcb03], 0x65);
                assert_eq!(
                    [0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| self.ram[p]),
                    mapping,
                    "{name} mapping"
                );
                assert!(self.latch.is_none(), "{name} left VDP half command");
                if name == "rt_cv1_bg_prepare_step" {
                    self.phase_max[phase] = self.phase_max[phase].max(self.last_tstates);
                }
                return cpu;
            }
            assert!(
                !cpu.halted,
                "{name}: trap{:02x}, pc{:04x}",
                self.ram[0xcb1d], cpu.pc
            );
            assert!(cpu.sp >= 0xde40, "native floor");
            if !self.allow_service {
                assert!(!cpu.iff1 && !cpu.iff2);
            }
            self.last_tstates += exact_step(self, &mut cpu);
        }
        panic!("{name} did not return at{:04x}", cpu.pc);
    }
    fn prepare(&mut self) {
        self.ram[0xdffc] = 8;
        self.call_hl("rt_cv1_hud_prepare", 0, 0, 0xb460, 0xc821);
        self.ram[0xdffc] = 0;
        self.call("rt_cv1_bg_prepare_begin", 0, 0, 0);
        for _ in 0..20000 {
            let cpu = self.call("rt_cv1_bg_prepare_step", 0, 0, 0);
            self.max_step = self.max_step.max(cpu.cycles);
            match cpu.a {
                0 => return,
                1 => {}
                2 => {
                    self.blank_requests += 1;
                    self.call("rt_cv1_sat_blank", 0, 0, 0);
                    self.call("rt_cv1_bg_prepare_blanked", 0, 0, 0);
                }
                value => panic!("invalid step return {value}"),
            }
        }
        panic!(
            "preparation stuck phase{} scan{:02x}{:02x}",
            self.sram[0x33e1], self.sram[0x33f4], self.sram[0x33f3]
        );
    }
    fn publish(&mut self) {
        self.ram[0xdffc] = 8;
        self.commit = true;
        self.call("rt_cv1_bg_commit_admitted", 0, 0, 0);
        self.call_hl("rt_cv1_hud_commit", 0, 0, 0, 0xb460);
        self.commit = false;
        self.ram[0xdffc] = 0;
        self.regs[1] = 0xf0;
        self.ram[0xc802] &= !2;
        self.assert_counts();
    }
    fn assert_counts(&self) {
        let selector = self.sram[0x33e0];
        let (shadow, counts) = if selector == 0 {
            (0x2a00, 0x3100)
        } else {
            (0x3900, 0x3700)
        };
        let mut expected = [0u16; 256];
        for cell in 0..896 {
            let slot = self.sram[shadow + cell];
            assert_ne!(slot, 255);
            assert_eq!(self.vram[0x3700 + cell * 2], slot);
            assert_eq!(self.vram[0x3701 + cell * 2], 0);
            expected[slot as usize] += 1;
        }
        for (slot, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                u16::from_le_bytes([
                    self.sram[counts + slot * 2],
                    self.sram[counts + slot * 2 + 1]
                ]),
                expected,
                "slot{slot}"
            );
        }
    }
}
impl Bus for Machine {
    fn read(&mut self, address: u16) -> u8 {
        match address {
            0..=0x3fff => self.rom[address as usize],
            0x4000..=0x7fff => {
                self.rom[self.ram[0xdffe] as usize * 0x4000 + address as usize - 0x4000]
            }
            0x8000..=0xbfff if self.ram[0xdffc] & 8 != 0 => self.sram[address as usize - 0x8000],
            0x8000..=0xbfff => {
                self.rom[self.ram[0xdfff] as usize * 0x4000 + address as usize - 0x8000]
            }
            _ => self.ram[0xc000 + (address as usize & 0x1fff)],
        }
    }
    fn write(&mut self, address: u16, value: u8) {
        if address >= 0xc000 {
            self.ram[0xc000 + (address as usize & 0x1fff)] = value;
        } else if address >= 0x8000 && self.ram[0xdffc] & 8 != 0 {
            self.sram[address as usize - 0x8000] = value;
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        if port == 0x7e {
            if let Some(clock) = self.physical_clock {
                let line = (224 + clock / 228) % 262;
                return if line <= 234 {
                    line as u8
                } else {
                    (line - 6) as u8
                };
            }
            let value = self
                .counters
                .get(self.counter_reads)
                .copied()
                .unwrap_or(0xe0);
            self.counter_reads += 1;
            value
        } else if port == 0xbf {
            self.vdp_status
        } else {
            0x80
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.ports += 1;
        if port == 0xbf {
            if let Some(low) = self.latch.take() {
                self.code = value >> 6;
                if self.code == 2 {
                    self.regs[value as usize & 15] = low;
                } else {
                    self.address = ((value as usize & 63) << 8) | low as usize;
                }
            } else {
                self.latch = Some(value);
            }
        } else if port == 0xbe {
            if self.regs[1] & 0x40 != 0 && !self.commit {
                assert_ne!(self.code, 3, "active CRAM mutation during preparation");
                if self.address < 0x2000 {
                    let slot = self.address / 32;
                    let base = if self.sram[0x33e0] == 0 {
                        0x3100
                    } else {
                        0x3700
                    };
                    assert_eq!(
                        &self.sram[base + slot * 2..base + slot * 2 + 2],
                        &[0, 0],
                        "overwrote active slot{slot}"
                    );
                } else {
                    assert!(
                        self.address < 0x3000,
                        "visible NT mutation during preparation"
                    );
                    let slot = (self.address - 0x2000) / 64;
                    assert_eq!(
                        self.ram[0xd440 + slot / 8] & (1 << (slot % 8)),
                        0,
                        "overwrote active sprite{slot}"
                    );
                }
            }
            if self.code == 3 {
                self.cram[self.address & 31] = value;
            } else {
                self.vram[self.address] = value;
            }
            self.address = (self.address + 1) & 0x3fff;
        }
    }
}

// Real Z80 timings for the executed subset. Cpu.cycles is approximate for
// block I/O; reject unknown opcodes instead of inventing a deadline estimate.
fn exact_step(m: &mut Machine, cpu: &mut Cpu) -> u64 {
    let pc = cpu.pc;
    let op = m.read(pc);
    let second = m.read(pc.wrapping_add(1));
    cpu.step(m)
        .unwrap_or_else(|error| panic!("{pc:04x}: {error:?}"));
    match op {
        0x00 | 0x07 | 0x0f | 0x17 | 0x1f | 0x2f | 0x37 | 0x3f | 0xeb | 0xf3 | 0xfb => 4,
        0x01 | 0x11 | 0x21 | 0x31 => 10,
        0x02 | 0x12 | 0x0a | 0x1a => 7,
        0x03 | 0x13 | 0x23 | 0x33 | 0x0b | 0x1b | 0x2b | 0x3b => 6,
        0x09 | 0x19 | 0x29 | 0x39 => 11,
        0x22 | 0x2a => 16,
        0x32 | 0x3a => 13,
        0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x3c | 0x05 | 0x0d | 0x15 | 0x1d | 0x25
        | 0x2d | 0x3d => 4,
        0x34 | 0x35 => 11,
        0x06 | 0x0e | 0x16 | 0x1e | 0x26 | 0x2e | 0x3e => 7,
        0x36 => 10,
        0x40..=0x7f if op != 0x76 => {
            if op & 7 == 6 || (op >> 3) & 7 == 6 {
                7
            } else {
                4
            }
        }
        0x80..=0xbf => {
            if op & 7 == 6 {
                7
            } else {
                4
            }
        }
        0xc6 | 0xce | 0xd6 | 0xde | 0xe6 | 0xee | 0xf6 | 0xfe => 7,
        0xc5 | 0xd5 | 0xe5 | 0xf5 => 11,
        0xc1 | 0xd1 | 0xe1 | 0xf1 => 10,
        0x18 => 12,
        0x10 => {
            if cpu.pc == pc + 2 {
                8
            } else {
                13
            }
        }
        0x20 | 0x28 | 0x30 | 0x38 => {
            if cpu.pc == pc + 2 {
                7
            } else {
                12
            }
        }
        0xc3 | 0xc2 | 0xca | 0xd2 | 0xda | 0xe2 | 0xea | 0xf2 | 0xfa => 10,
        0xe9 => 4,
        0xcd => 17,
        0xc4 | 0xcc | 0xd4 | 0xdc | 0xe4 | 0xec | 0xf4 | 0xfc => {
            if cpu.pc == pc + 3 {
                10
            } else {
                17
            }
        }
        0xc9 => 10,
        0xc0 | 0xc8 | 0xd0 | 0xd8 | 0xe0 | 0xe8 | 0xf0 | 0xf8 => {
            if cpu.pc == pc + 1 {
                5
            } else {
                11
            }
        }
        0xd3 | 0xdb => 11,
        0xcb => {
            if second & 7 != 6 {
                8
            } else if (0x40..0x80).contains(&second) {
                12
            } else {
                15
            }
        }
        0xed => match second {
            0x42 | 0x52 | 0x62 | 0x72 | 0x4a | 0x5a | 0x6a | 0x7a => 15,
            0x43 | 0x53 | 0x63 | 0x73 | 0x4b | 0x5b | 0x6b | 0x7b => 20,
            0x44 => 8,
            0x47 | 0x4f | 0x57 | 0x5f => 9,
            0xa0 | 0xa3 => 16,
            0xb0 | 0xb3 => {
                if cpu.pc == pc {
                    21
                } else {
                    16
                }
            }
            _ => panic!("unbudgeted ED{second:02x} at{pc:04x}"),
        },
        _ => panic!("unbudgeted opcode{op:02x} at{pc:04x}"),
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn raw_writers_are_deferred_and_partial_palette_keeps_boot_colors() {
    let mut m = Machine::new();
    let palette = m.cram;
    assert!(palette.iter().any(|v| *v != 0));
    let oam = m.ram[0xc900..0xca00].to_vec();
    let ports = m.ports;
    m.call("rt_cv1_bg_raw_write", 0x57, 0, 0x2413);
    assert_eq!(m.sram[0x413], 0x57);
    assert_ne!(m.sram[0x2800 + 0x413 / 8] & (1 << (0x413 % 8)), 0);
    for i in 0..300 {
        m.call("rt_cv1_bg_chr_write", i as u8, 0, 0x1003);
    }
    assert_eq!(m.ram[0xc801], 1);
    assert_ne!(m.sram[0x3320 + 0x100 / 8] & 1, 0);
    m.call("rt_cv1_bg_palette_write", 0, 0x013a, 0);
    for (i, color) in palette.into_iter().enumerate() {
        assert_eq!(m.sram[0x33a0 + i], if i == 1 { 0x3a } else { color });
    }
    m.call("rt_cv1_bg_palette_write", 0, 0x1027, 0);
    for i in [0, 4, 8, 12] {
        assert_eq!(m.sram[0x33a0 + i], 0x27);
    }
    assert_eq!(m.cram, palette);
    assert_eq!(m.ports, ports);
    assert_eq!(&m.ram[0xc900..0xca00], oam);
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn full_scene_then_one_cell_keeps_exact_wide_counts_and_visible_patterns() {
    let mut m = Machine::new();
    m.prepare();
    m.publish();
    assert_eq!(m.blank_requests, 1);
    let before = m.vram.to_vec();
    let selector = m.sram[0x33e0];
    m.call("rt_cv1_bg_raw_write", 7, 0, 0x2164);
    m.prepare();
    assert_eq!(m.sram[0x33e2], 1);
    assert_eq!(m.sram[0x33e0], selector, "metadata published early");
    assert_eq!(m.blank_requests, 1);
    assert_eq!(&m.vram[0x3700..0x3e00], &before[0x3700..0x3e00]);
    m.publish();
    assert_ne!(m.vram[0x3700 + 0x164 * 2], before[0x3700 + 0x164 * 2]);
    let unchanged = m.ports;
    m.prepare();
    assert_eq!(m.sram[0x33e2], 0);
    assert_eq!(
        m.sram[0x33e3], 0,
        "unchanged frame copied1408 metadata bytes"
    );
    assert_eq!(m.ports, unchanged);
    eprintln!(
        "bg approximate max-step cycles={} (not exact timing proof)",
        m.max_step
    );
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn one_column_scroll_both_directions_and_page_wrap_keeps_six_hud_rows() {
    let mut m = Machine::new();
    m.ram[0xc825] = 6; // immutable complete pre/post pair, same Y
    for world in 0..64 {
        for row in 0..28 {
            m.sram[(world / 32) * 0x400 + row * 32 + world % 32] = world as u8 + 1;
        }
    }
    m.prepare();
    m.publish();
    for position in (1..=64).chain((0..64).rev()) {
        let window = position & 63;
        m.ram[0xc821] = 0x90 | (window / 32) as u8;
        m.ram[0xc828] = (window as u8).wrapping_mul(8);
        let previous = m.vram[0x3700..0x3e00].to_vec();
        m.prepare();
        assert_eq!(m.blank_requests, 1, "routine column blanked at{position}");
        assert!(m.sram[0x33e2] <= 22);
        assert_eq!(&m.vram[0x3700..0x3e00], previous);
        m.publish();
        assert_eq!(m.ram[0xcb2a], window as u8);
        for cell in 0..896 {
            let column = cell % 32;
            let world = if cell < 192 {
                column
            } else {
                (window + (column + 64 - window) % 32) % 64
            };
            let slot = m.vram[0x3700 + cell * 2] as usize;
            // The canonical profile separately remaps transition-fill37/38
            // in eight rows; this is not the six-row fixed HUD policy.
            let expected = if cell < 256 && [0x37, 0x38].contains(&(world as u8 + 1)) {
                0
            } else {
                world as u8 + 1
            };
            assert_eq!(m.sram[0x3500 + slot], expected, "window{window} cell{cell}");
        }
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn partial_chr_is_copy_on_write_and_other_source_table_does_not_repaint() {
    let mut m = Machine::new();
    m.sram[0x164] = 7;
    m.prepare();
    m.publish();
    let old_slot = m.vram[0x3700 + 0x164 * 2] as usize;
    let old_pattern = m.vram[old_slot * 32..old_slot * 32 + 32].to_vec();
    m.call("rt_cv1_bg_chr_write", 0xff, 0, 0x1073);
    m.prepare();
    assert_eq!(m.blank_requests, 1);
    assert_eq!(m.sram[0x33e2], 1);
    assert_eq!(&m.vram[old_slot * 32..old_slot * 32 + 32], old_pattern);
    m.publish();
    assert_ne!(m.vram[0x3700 + 0x164 * 2] as usize, old_slot);
    let before = m.vram.to_vec();
    m.call("rt_cv1_bg_chr_write", 0x44, 0, 0x0073);
    m.prepare();
    assert_eq!(m.sram[0x33e2], 0);
    assert_eq!(m.vram.as_slice(), before);
    m.publish();
    m.ram[0xc821] = 0x80; // true source table change: explicit hidden rebuild
    m.prepare();
    assert_eq!(m.ram[0xca13], 0x10, "table latch moved before commit");
    assert_eq!(m.blank_requests, 2);
    m.publish();
    assert_eq!(m.ram[0xca13], 0);
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn queue_and_slot_union_overflow_blank_before_reuse_instead_of_stealing() {
    for exhaust_slots in [false, true] {
        let mut m = Machine::new();
        m.prepare();
        m.publish();
        if exhaust_slots {
            let counts = if m.sram[0x33e0] == 0 { 0x3100 } else { 0x3700 };
            for slot in 0..255 {
                m.sram[counts + slot * 2] = 1;
            }
        }
        for cell in 0..if exhaust_slots { 1 } else { 33 } {
            m.call("rt_cv1_bg_raw_write", 7, 0, 0x2140 + cell);
        }
        m.prepare();
        assert_eq!(m.blank_requests, 2);
        assert_eq!(m.sram[0x33e2], 0, "full rebuild must not truncate a packet");
        m.publish();
        assert!(
            m.vram[0x3700..0x3e00]
                .iter()
                .step_by(2)
                .all(|slot| *slot != 255)
        );
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn direct_title_without_hint_can_leave_vblank_and_admit_preparation() {
    let mut m = Machine::new();
    m.ram[0xc813] = 1;
    m.ram[0xc83b] = 1;
    m.ram[0xc811] = 0xa9;
    m.ram[0xc812] = 0x52;
    m.allow_service = true;
    m.counter_reads = 0;
    m.counters = vec![0xe0, 0xe0, 0];
    let cpu = m.call("_cv1_prepare_admit", 0, 38 << 8, 0);
    assert_eq!(m.counter_reads, 3);
    assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
    assert!(!cpu.iff1 && !cpu.iff2);
    // The preserved p3 HALT instruction fails before reaching sample3.
}

fn complete_packet() -> Machine {
    let mut m = Machine::new();
    m.ram[0xc825] = 6;
    m.prepare();
    m.publish();
    for cell in 0..32 {
        m.call("rt_cv1_bg_raw_write", 7, 0, 0x2140 + cell);
    }
    m.call("rt_cv1_bg_palette_write", 0, 0x012a, 0);
    m.prepare();
    assert_eq!(m.sram[0x33e2], 32);
    m.ram[0xc900..0xca00].fill(0xff);
    m.call("rt_cv1_sat_prepare", 0, 0, 0);
    m.ram[0xc820] = 1;
    m.ram[0xcb2d] = 0xa5;
    m.commit = true;
    m
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn complete_maximum_commit_meets_latest_e3_and_preserves_all_guard_mappings() {
    let base = complete_packet();
    let mut maximum = 0;
    for depth in 0..=2 {
        for sram in [0, 8] {
            for hud in [1, 3, 17] {
                let mut m = base.clone();
                m.sram[0x3460] = hud;
                m.ram[0xd47f] = depth;
                m.ram[0xdffc] = sram;
                m.counters = vec![0xe3, 0xf4];
                m.counter_reads = 0;
                let mapping = [0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| m.ram[p]);
                let mut cpu = Cpu::new();
                cpu.pc = m.labels["rt_cv1_frame_try_present"].1;
                cpu.sp = 0xde44;
                m.ram[0xde44] = 7;
                m.ram[0xde45] = 0;
                let mut t = 0;
                let mut first = None;
                let mut last = 0;
                while cpu.pc != 7 {
                    assert!(cpu.sp >= 0xde40);
                    if m.read(cpu.pc) == 0xdb && m.read(cpu.pc + 1) == 0x7e && first.is_none() {
                        first = Some(t);
                    }
                    let ports = m.ports;
                    t += exact_step(&mut m, &mut cpu);
                    if m.ports != ports {
                        last = t;
                    }
                }
                let bound = last - first.unwrap();
                maximum = maximum.max(bound);
                assert!(bound <= 7752, "whole maxpacket exceeded latestE3: {bound}");
                assert_eq!(cpu.a, 1);
                assert_eq!(cpu.sp, 0xde46);
                assert_eq!(
                    m.counter_reads, 2,
                    "only consumer admission + HUD arming sample"
                );
                assert_eq!([0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| m.ram[p]), mapping);
                assert_eq!(m.ram[0xcb2d], 0xa5, "newer producer intent lost");
                assert_eq!(m.ram[0xc820], 0);
                assert_eq!(m.ram[0xc810], 0);
                assert_eq!(m.ram[0xc83d], 1);
                assert!(m.latch.is_none());
            }
        }
    }
    eprintln!("whole_BG32_CRAM32_HUD_SAT192_max_last_visible={maximum}T latestE3_remaining=7752T");
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn actual_irq_prefix_admits_maximum_packet_at_stock_clock() {
    let mut m = complete_packet();
    m.ram[0xcb28] = 1;
    m.physical_clock = Some(23); // hardware IM1 acknowledgment13 + vectorJP10
    let mut cpu = Cpu::new();
    cpu.pc = m.labels["irq_handler"].1;
    cpu.sp = 0xdff0;
    let mut return_pc = None;
    let mut first_sample = None;
    let mut last = 0;
    for _ in 0..5000 {
        if return_pc.is_some_and(|p| cpu.pc == p) {
            break;
        }
        if cpu.pc == m.labels["rt_cv1_frame_try_present"].1 {
            return_pc = Some(u16::from_le_bytes([m.read(cpu.sp), m.read(cpu.sp + 1)]));
        }
        if m.read(cpu.pc) == 0xdb && m.read(cpu.pc + 1) == 0x7e && first_sample.is_none() {
            first_sample = m.physical_clock;
        }
        let ports = m.ports;
        let elapsed = exact_step(&mut m, &mut cpu);
        *m.physical_clock.as_mut().unwrap() += elapsed;
        if m.ports != ports {
            last = m.physical_clock.unwrap();
        }
    }
    assert_eq!(Some(cpu.pc), return_pc);
    assert_eq!(cpu.a, 1, "stock IRQ retries must not starve forever");
    assert_eq!(m.ram[0xc820], 0);
    assert!(last < 38 * 228, "last visible OUT left the physical blank");
    eprintln!(
        "stock_IRQ_admission_sample={}T_from_VINT last_OUT={last}T_from_VINT",
        first_sample.unwrap()
    );
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn c11f_control_capture_and_prepare_prefix_has_a_separate_exact_budget() {
    let mut base = Machine::new();
    base.prepare();
    base.publish();
    let mut maxima = [0; 2];
    for control in 0..=255 {
        for (chunk, label) in ["_cv1_hook_ppu_begin", "_cv1_hook_capture_begin"]
            .iter()
            .enumerate()
        {
            let target = base.labels[if chunk == 0 {
                "_cv1_prepare_admit"
            } else {
                "_cv1_prepare_service"
            }]
            .1;
            let mut m = base.clone();
            m.ram[0xc0ff] = control;
            m.ram[0xc0fd] = 0x98;
            m.ram[0xc0fc] = 0;
            m.ram[0xcb20] = 6;
            m.ram[0xca11] = 1;
            m.ram[0xc83b] = 1;
            m.ram[0xc811] = 0xa9;
            m.ram[0xc812] = 0x52;
            let mut cpu = Cpu::new();
            cpu.pc = m.labels[*label].1;
            cpu.sp = 0xdff0;
            cpu.d = 0x52;
            cpu.e = 0xa9;
            let mut t = 0;
            for _ in 0..2000 {
                if cpu.pc == target {
                    break;
                }
                t += exact_step(&mut m, &mut cpu);
            }
            assert_eq!(cpu.pc, target);
            maxima[chunk] = maxima[chunk].max(t);
        }
    }
    eprintln!(
        "C11F_original_PPU_ops={}T capture_HUD_BGbegin={}T",
        maxima[0], maxima[1]
    );
    for (maximum, lines) in maxima.into_iter().zip([17, 16]) {
        assert!(
            maximum + 512 <= lines * 228,
            "chunk plus coordinator glue exceeds its admission"
        );
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn bg_admission_uses_measured_phase_bounds_and_preserves_mapping() {
    // Independent exhaustive assembled phase matrix is recorded in the local
    // phase-cost evidence. These are entry-to-RET maxima, not Cpu.cycles.
    let maxima = [
        154 + 194,
        3482,
        1426,
        8460,
        2850,
        2647,
        10431,
        1073,
        9471,
        3992,
        2014,
        1271,
        387,
        3804,
        1337,
        668,
    ];
    let mut m = Machine::new();
    m.sram[0x2900..0x2a00].fill(0xff); // exercise the conservative nonzero classes
    for (phase, maximum) in maxima.into_iter().enumerate() {
        m.sram[0x33e1] = phase as u8;
        let before = m.sram.clone();
        let cpu = m.call("rt_cv1_bg_chunk_lines", 0, 0, 0);
        assert_eq!(m.sram, before, "budget classification changed preparation");
        assert_eq!(u64::from(cpu.b), (maximum + 512_u64).div_ceil(228));
        assert!(
            m.last_tstates + 17 < 352,
            "classification exceeds its entry reserve share: phase{phase} {}T",
            m.last_tstates + 17
        );
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn sparse_bitmap_steps_skip_only_clean_cells_and_have_short_exact_bounds() {
    let base = Machine::new();
    let mut scan_max = 0;
    let mut attribute_max = 0;
    for start_byte in 0..256 {
        for distance in 1..=17 {
            let mut m = base.clone();
            m.sram[0x33e1] = 6;
            m.sram[0x33f3..0x33f5].copy_from_slice(&((start_byte * 8) as u16).to_le_bytes());
            let next = (start_byte + distance).min(256);
            if next < 256 {
                m.sram[0x2900 + next] = 0xa5;
            }
            let frozen = m.sram[0x2900..0x2a00].to_vec();
            let budget = m.call("rt_cv1_bg_chunk_lines", 0, 0, 0).b;
            assert_eq!(budget, 8);
            let ports = m.ports;
            m.call("rt_cv1_bg_prepare_step", 0, 0, 0);
            scan_max = scan_max.max(m.last_tstates);
            let end = (start_byte + 16).min(next);
            if end == 256 {
                assert_eq!(m.sram[0x33e1], 15);
            } else {
                assert_eq!(m.sram[0x33e1], 6);
                assert_eq!(
                    u16::from_le_bytes([m.sram[0x33f3], m.sram[0x33f4]]),
                    (end * 8) as u16
                );
            }
            assert_eq!(&m.sram[0x2900..0x2a00], frozen);
            assert_eq!(m.ports, ports);
        }
    }
    for cursor in (0x3c0u16..0x400)
        .step_by(8)
        .chain((0x7c0..0x800).step_by(8))
    {
        let mut m = base.clone();
        m.sram[0x33e1] = 5;
        m.sram[0x33e9..0x33eb].copy_from_slice(&cursor.to_le_bytes());
        m.sram[0x3467] = 3;
        assert_eq!(m.call("rt_cv1_bg_chunk_lines", 0, 0, 0).b, 6);
        let frozen = m.sram[0x2900..0x2a00].to_vec();
        m.call("rt_cv1_bg_prepare_step", 0, 0, 0);
        attribute_max = attribute_max.max(m.last_tstates);
        if cursor == 0x7f8 {
            assert_eq!(m.sram[0x33e1], 6);
        } else {
            let expected = if cursor == 0x3f8 { 0x7c0 } else { cursor + 8 };
            assert_eq!(
                u16::from_le_bytes([m.sram[0x33e9], m.sram[0x33ea]]),
                expected
            );
        }
        assert_eq!(&m.sram[0x2900..0x2a00], frozen);
    }
    eprintln!("sparse_scan_max={scan_max}T clean_attribute_max={attribute_max}T");
    assert!(scan_max + 512 <= 8 * 228);
    assert!(attribute_max + 512 <= 6 * 228);
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn full_producer_rejects_reentry_and_invalid_yield_context_before_mutation() {
    for invalid in 0..7 {
        let mut m = Machine::new();
        m.ram[0xca11] = 1;
        match invalid {
            0 => m.ram[0xca11] = 0,
            1 => m.ram[0xca11] = 2,
            2 => m.ram[0xd47f] = 1,
            3 => m.ram[0xdffc] = 8,
            4 => m.ram[0xdfff] ^= 1,
            5 => m.ram[0xc820] = 1,
            6 => m.ram[0xc810] = 1,
            _ => unreachable!(),
        }
        let frozen = m.sram.clone();
        let packet = m.ram[0xc821..0xc83a].to_vec();
        let ports = m.ports;
        let mut cpu = Cpu::new();
        cpu.pc = m.labels["rt_cv1_scroll_publish"].1;
        cpu.sp = 0xdff0;
        cpu.iff1 = true;
        cpu.iff2 = true;
        let stop = m.labels["_cv1_present_halt"].1;
        for _ in 0..200 {
            if cpu.pc == stop {
                break;
            }
            cpu.step(&mut m).unwrap();
        }
        assert_eq!(cpu.pc, stop, "invalid context{invalid} was not rejected");
        assert_eq!(m.ram[0xcb1d], 0xf8);
        assert_eq!(m.ram[0xc83c], 0);
        assert_eq!(m.sram, frozen);
        assert_eq!(&m.ram[0xc821..0xc83a], packet);
        assert_eq!(m.ports, ports);
    }
    let mut m = Machine::new();
    m.ram[0xca11] = 1;
    let mut cpu = Cpu::new();
    cpu.pc = m.labels["rt_cv1_scroll_publish"].1;
    cpu.sp = 0xdff0;
    cpu.iff1 = true;
    cpu.iff2 = true;
    cpu.d = 0x52;
    cpu.e = 0xa9;
    let stop = m.labels["_cv1_prepare_admit"].1;
    let mut t = 0;
    for _ in 0..200 {
        if cpu.pc == stop {
            break;
        }
        t += exact_step(&mut m, &mut cpu);
    }
    assert_eq!(cpu.pc, stop);
    assert!(
        t <= 512,
        "unadmitted guard/park prelude exceeds latency reserve: {t}T"
    );
    assert_eq!(&m.ram[0xc811..0xc813], &[0xa9, 0x52]);
    assert_eq!(cpu.b, 17);
    eprintln!("C11F_before_first_admit={t}T");
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn preparation_service_window_parks_guest_xy_and_preserves_actual_irq_state() {
    for status in [0, 0x80] {
        for pending_before_ei in [false, true] {
            let mut m = Machine::new();
            m.ram[0xcb28] = 1;
            m.ram[0xcb1a] = 1;
            m.ram[0xca11] = 1;
            m.ram[0xca12] = 1;
            m.ram[0xc01a] = 0x4f;
            m.ram[0xc811..0xc813].copy_from_slice(&[0xa9, 0x52]);
            m.ram[0xc83b] = 1;
            m.ram[0xc813..0xc819].copy_from_slice(&[7, 0, 0, 0x61, 38, 4]);
            m.ram[0xcb15] = 0x35;
            m.ram[0xcb18] = 0x68;
            m.ram[0xcb27] = 0x79;
            m.vdp_status = status;
            let mapping = [0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| m.ram[p]);
            let mut cpu = Cpu::new();
            cpu.pc = m.labels["_cv1_prepare_service"].1;
            cpu.sp = 0xde80;
            m.ram[0xde80] = 7;
            cpu.d = 0x12; // scratch converter registers must never become guest X/Y
            cpu.e = 0x34;
            let mut injected = false;
            let mut min_sp = cpu.sp;
            for _ in 0..2000 {
                if cpu.pc == 7 {
                    break;
                }
                if cpu.iff1 && cpu.ei_pending == 0 && !injected {
                    // Both a previously pending edge and one raised at the service
                    // boundary are delivered only after EI's delayed instruction.
                    assert_eq!(m.read(cpu.pc), 0xf3);
                    assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
                    let resume = cpu.pc;
                    let sp = cpu.sp;
                    cpu.sp -= 2;
                    m.write(cpu.sp, resume as u8);
                    m.write(cpu.sp + 1, (resume >> 8) as u8);
                    cpu.iff1 = false;
                    cpu.iff2 = false;
                    cpu.pc = 0x38;
                    for _ in 0..10000 {
                        min_sp = min_sp.min(cpu.sp);
                        if cpu.pc == resume && cpu.sp == sp {
                            break;
                        }
                        cpu.step(&mut m).unwrap();
                    }
                    assert_eq!((cpu.pc, cpu.sp), (resume, sp));
                    injected = true;
                }
                if pending_before_ei && m.read(cpu.pc) == 0xfb {
                    assert!(!cpu.iff1, "pending edge cannot enter before guest parking");
                }
                cpu.step(&mut m).unwrap();
            }
            assert_eq!(cpu.pc, 7);
            assert!(injected);
            assert!(!cpu.iff1 && !cpu.iff2);
            assert!(min_sp >= 0xde40);
            assert_eq!(
                (cpu.d, cpu.e, m.ram[0xcb00], m.ram[0xcb01]),
                (0x52, 0xa9, 0x52, 0xa9)
            );
            assert_eq!(
                [0xcb15, 0xcb18, 0xcb27, 0xcb03].map(|p| m.ram[p]),
                [0x35, 0x68, 0x79, 0x65]
            );
            assert_eq!([0xdffc, 0xdfff, 0xcb62, 0xd47f].map(|p| m.ram[p]), mapping);
            assert_eq!((m.ram[0xc01a], m.ram[0xc820], m.ram[0xc83c]), (0x4f, 0, 0));
            assert_eq!((m.ram[0xca11], m.ram[0xca12]), (1, 1));
            assert_eq!(m.ram[0xcb04], u8::from(status == 0x80));
        }
    }
}

#[test]
#[ignore = "requires CV1_BG_PROJECT assembled using Docker WLA-DX"]
fn coordinator_bounds_admission_through_next_guest_service_boundary() {
    let mut maximum = 0;
    let mut entry_maximum = 0;
    for (bg_phase, flags, index, dirty) in [
        (None, 0x14, 0, 0),
        (None, 0x14, 0, 1),
        (None, 0x24, 0, 0),
        (None, 0x2c, 63, 0),
        (None, 0x24, 63, 0),
        (None, 0x34, 0, 0),
        (Some(0), 4, 0, 0),
        (Some(1), 4, 0, 0),
        (Some(2), 4, 0, 0),
        (Some(5), 4, 0, 0),
        (Some(6), 4, 0, 0),
        (Some(5), 4, 0, 1),
        (Some(6), 4, 0, 1),
    ] {
        let mut m = Machine::new();
        m.ram[0xc802] = flags;
        m.ram[0xc801] = dirty;
        m.ram[0xc806] = index;
        m.ram[0xd468] = 1;
        m.ram[0xcb08] = 0xb0;
        m.ram[0xcb09] = 0x18;
        m.ram[0xc900..0xca00].fill(0xff);
        m.ram[0xc813] = 7;
        if let Some(phase) = bg_phase {
            m.sram[0x33e1] = phase;
            if dirty != 0 {
                m.sram[0x2900..0x2a00].fill(0xff);
            }
            m.ram[0xc813] = 11;
            m.counters = vec![50];
        }
        m.ram[0xc83b] = 1;
        m.ram[0xc811..0xc813].copy_from_slice(&[0xa9, 0x52]);
        let mut cpu = Cpu::new();
        cpu.pc = m.labels[if bg_phase.is_some() {
            "_cv1_prepare_bg"
        } else {
            "_cv1_prepare_sat"
        }]
        .1;
        cpu.sp = 0xdff0;
        let post_prepare = m
            .labels
            .get("_cv1_hook_prepared")
            .map(|p| p.1)
            .unwrap_or_else(|| {
                let begin = m.labels["_cv1_hook_capture_begin"].1;
                let helper = m.labels["rt_cv1_frame_prepare_all"].1;
                (begin..begin + 64)
                    .find(|p| {
                        m.read(*p) == 0xcd
                            && u16::from_le_bytes([m.read(*p + 1), m.read(*p + 2)]) == helper
                    })
                    .unwrap()
                    + 3
            });
        m.ram[0xdff0..0xdff6].copy_from_slice(&[
            post_prepare as u8,
            (post_prepare >> 8) as u8,
            0xa9,
            0x52,
            7,
            0,
        ]);
        let mut elapsed = 0;
        let mut sampled = None;
        let mut previous_service = None;
        let mut step_start = None;
        let mut step_end = None;
        let mut step_return = 0;
        for _ in 0..10000 {
            if sampled.is_none() && cpu.iff1 && cpu.ei_pending == 0 {
                previous_service = Some(elapsed);
            }
            if sampled.is_none() && m.read(cpu.pc) == 0xdb && m.read(cpu.pc + 1) == 0x7e {
                sampled = Some(elapsed);
            }
            if cpu.pc
                == m.labels[if bg_phase.is_some() {
                    "rt_cv1_bg_prepare_step"
                } else {
                    "rt_cv1_sat_prepare_step"
                }]
                .1
                && step_start.is_none()
            {
                step_start = Some(elapsed);
                step_return = u16::from_le_bytes([m.read(cpu.sp), m.read(cpu.sp + 1)]);
            }
            if step_start.is_some() && step_end.is_none() && cpu.pc == step_return {
                step_end = Some(elapsed);
            }
            if step_end.is_some() && cpu.iff1 && cpu.ei_pending == 0 {
                break;
            }
            elapsed += exact_step(&mut m, &mut cpu);
        }
        assert!(cpu.iff1 && cpu.ei_pending == 0);
        assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
        entry_maximum = entry_maximum.max(sampled.unwrap() - previous_service.unwrap());
        if bg_phase.is_none() && flags == 0x24 && index == 63 {
            assert_eq!((m.ram[0xca12], m.ram[0xc820], m.ram[0xc83c]), (0, 1, 1));
        }
        let glue = elapsed
            - sampled.unwrap()
            - (step_end.unwrap() - step_start.unwrap())
            - if bg_phase == Some(0) { 194 } else { 0 };
        eprintln!("coordinator phase={bg_phase:?}/SAT{flags:02x} glue={glue}T");
        maximum = maximum.max(glue);
    }
    eprintln!("SAT_sample_through_next_service_excluding_step={maximum}T");
    eprintln!("service_boundary_through_classification_to_sample={entry_maximum}T");
    assert!(
        entry_maximum <= 2 * 228,
        "read-only classifier exceeded the entry IRQ-latency reserve"
    );
    assert!(
        maximum <= 512,
        "admitted step omitted its trailing classifier/service cost"
    );
}
