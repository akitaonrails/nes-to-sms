//! Execute Docker-assembled HUD helpers, not a parallel HUD state machine.
//! CV1_HUD_PROJECT=out/<candidate> cargo test -p nes_to_sms --test hud_commit -- --ignored

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

const FLAGS: usize = 0xc813;

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    ram: Box<[u8; 65536]>,
    sram: Box<[u8; 16384]>,
    labels: HashMap<String, (u8, u16)>,
    ports: Vec<(u8, u8)>,
    registers: [u8; 16],
    latch: Option<u8>,
    status: u8,
    status_reads: usize,
    counter: u8,
    counter_reads: usize,
    min_sp: u16,
}

impl Machine {
    fn assembled() -> Self {
        let path = PathBuf::from(std::env::var("CV1_HUD_PROJECT").expect("CV1_HUD_PROJECT"));
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
            let mut fields = line.split_whitespace();
            if let (Some(location), Some(name)) = (fields.next(), fields.next())
                && let Some((bank, address)) = location.split_once(':')
                && let (Ok(bank), Ok(address)) = (
                    u8::from_str_radix(bank, 16),
                    u16::from_str_radix(address, 16),
                )
            {
                labels.insert(name.to_owned(), (bank, address));
            }
        }
        let mut m = Self {
            rom: std::fs::read(path.join("sms.sms")).unwrap(),
            ram: Box::new([0; 65536]),
            sram: Box::new([0; 16384]),
            labels,
            ports: Vec::new(),
            registers: [0; 16],
            latch: None,
            status: 0x80,
            status_reads: 0,
            counter: 0xe0,
            counter_reads: 0,
            min_sp: 0xffff,
        };
        m.ram[0xdffe] = 1;
        m.ram[0xdfff] = m.labels["data_prg_bank_3"].0;
        m.ram[0xcb62] = 3;
        m.ram[0xcb14] = 1;
        m.ram[0xcb03] = 0x65;
        m.ram[0xdd80..0xde40].fill(0xa5);
        m
    }

    fn cpu(&mut self, label: &str) -> Cpu {
        let (bank, address) = self.labels[label];
        assert_eq!(bank, 0);
        let mut cpu = Cpu::new();
        cpu.pc = address;
        cpu.sp = 0xdff0;
        self.ram[0xdff0] = 7;
        self.ram[0xdff1] = 0;
        cpu
    }

    fn run(&mut self, mut cpu: Cpu) -> Cpu {
        let mapping = [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f].map(|a| self.ram[a]);
        let guard = self.ram[0xca19..0xca20].to_vec();
        let spills = [0xcb00, 0xcb01, 0xcb03, 0xcb15, 0xcb18, 0xcb27].map(|a| self.ram[a]);
        let entry_sp = cpu.sp;
        for _ in 0..10000 {
            assert!(!cpu.iff1 && !cpu.iff2);
            assert!(!cpu.halted);
            assert!(cpu.sp >= 0xde40);
            self.min_sp = self.min_sp.min(cpu.sp);
            if cpu.pc == 7 {
                assert_eq!(cpu.sp, entry_sp + 2);
                assert_eq!(
                    [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f].map(|a| self.ram[a]),
                    mapping
                );
                assert_eq!(&self.ram[0xca19..0xca20], guard);
                assert_eq!(
                    [0xcb00, 0xcb01, 0xcb03, 0xcb15, 0xcb18, 0xcb27].map(|a| self.ram[a]),
                    spills
                );
                assert!(self.ram[0xdd80..0xde40].iter().all(|b| *b == 0xa5));
                assert!(self.latch.is_none());
                return cpu;
            }
            cpu.step(self)
                .unwrap_or_else(|e| panic!("{:04x}: {e:?}", cpu.pc));
        }
        panic!("helper failed to return: {:04x}", cpu.pc);
    }

    fn call(&mut self, label: &str) -> Cpu {
        let cpu = self.cpu(label);
        self.run(cpu)
    }

    fn record(&mut self, flags: u8, base: u8) {
        self.ram[FLAGS..FLAGS + 6].copy_from_slice(&[flags, 0, 0, 0xd7, 38, base]);
    }

    // Tiny exact timing whitelist for the real assembled straight-line HUD
    // paths. Cpu.cycles deliberately remains unsuitable for beam deadlines.
    fn exact_step(&mut self, cpu: &mut Cpu) -> u32 {
        let opcode = self.read(cpu.pc);
        let cost = match opcode {
            0x3a | 0x32 => 13,
            0x21 => 10,
            0x34 => 11,
            0xc9 => 10,
            0x3e | 0xd6 | 0xfe | 0xf6 => 7,
            0xdb | 0xd3 => 11,
            0xc8 | 0xd8 | 0xd0 => {
                let taken = match opcode {
                    0xc8 => cpu.f & 0x40 != 0,
                    0xd8 => cpu.f & 1 != 0,
                    _ => cpu.f & 1 == 0,
                };
                if taken { 11 } else { 5 }
            }
            0xc3 => 10,
            0x79 | 0xb7 => 4,
            0xcd => 17,
            0xc2 | 0xca => 10,
            0xc5 | 0xe5 | 0xf5 => 11,
            0x40..=0x75 | 0x77..=0x7f => {
                if opcode & 7 == 6 || opcode & 0x38 == 0x30 {
                    7
                } else {
                    4
                }
            }
            0x28 => {
                if cpu.f & 0x40 != 0 {
                    12
                } else {
                    7
                }
            }
            0x20 => {
                if cpu.f & 0x40 == 0 {
                    12
                } else {
                    7
                }
            }
            0x30 => {
                if cpu.f & 1 == 0 {
                    12
                } else {
                    7
                }
            }
            0xcb => {
                let operation = self.read(cpu.pc + 1);
                if operation & 7 == 6 { 15 } else { 8 }
            }
            other => panic!("unbudgeted HUD opcode {other:02x} at {:04x}", cpu.pc),
        };
        cpu.step(self).unwrap();
        cost
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
        match address {
            0x8000..=0xbfff if self.ram[0xdffc] & 8 != 0 => {
                self.sram[address as usize - 0x8000] = value
            }
            0xc000..=0xffff => self.ram[0xc000 + (address as usize & 0x1fff)] = value,
            _ => panic!("unexpected write {address:04x}"),
        }
    }

    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0x7e => {
                self.counter_reads += 1;
                self.counter
            }
            0xbf => {
                assert!(self.latch.is_none(), "status read interrupted command pair");
                self.status_reads += 1;
                let status = self.status;
                self.status = 0;
                status
            }
            p => panic!("unexpected input {p:02x}"),
        }
    }

    fn out_port(&mut self, port: u8, value: u8) {
        assert_eq!(port, 0xbf);
        self.ports.push((0xbf, value));
        if let Some(first) = self.latch.take() {
            assert_eq!(value & 0xc0, 0x80);
            self.registers[usize::from(value & 15)] = first;
        } else {
            self.latch = Some(value);
        }
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn preparation_freezes_horizontal_controls_and_reports_unsupported_y() {
    for (split, mask, pre_y, post_y, expected_flags, expected_x) in [
        (7, 0x1e, 0, 0, 3, 0),
        (7, 0x1e, 3, 4, 17, 0xd7),
        (7, 0x10, 0, 0, 1, 0xd7),
        (5, 0x1e, 0, 0, 1, 0xd7),
        (0, 0x1e, 0, 0, 1, 0xf5),
    ] {
        let mut m = Machine::assembled();
        m.ram[0xdffc] = 8;
        m.ram[0xd47f] = 2;
        m.ram[0xc821..0xc82a].copy_from_slice(&[0xb0, mask, 11, 2, split, 0, pre_y, 41, post_y]);
        let frozen = m.ram[0xc821..0xc82a].to_vec();
        let mut cpu = m.cpu("rt_cv1_hud_prepare");
        cpu.h = 0xc8;
        cpu.l = 0x21;
        cpu.d = 0xb4;
        cpu.e = 0x60;
        m.run(cpu);
        let has_post = split & 4 != 0;
        assert_eq!(
            &m.sram[0x3460..0x3465],
            &[
                expected_flags,
                expected_x,
                if has_post { post_y } else { 2 },
                if has_post { 0xd7 } else { 0xf5 },
                38
            ]
        );
        assert_eq!(m.sram[0x3465] & 0x10, 0);
        assert_eq!(&m.ram[0xc821..0xc82a], frozen);
        assert!(m.ports.is_empty());
        assert_eq!(m.counter_reads + m.status_reads, 0);
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn every_physical_frame_rearms_committed_scroll_independent_of_live_ready_and_wrap() {
    for base in [0x46, 0x66] {
        let mut m = Machine::assembled();
        m.record(3, base);
        m.ram[0xc819] = 0xff;
        m.ram[0xc820] = 0;
        m.ram[0xcb20..0xcb25].fill(0x99);
        m.call("rt_cv1_hud_vblank");
        assert_eq!(m.ram[0xc819], 0);
        assert_eq!(m.ram[FLAGS], 7);
        assert_eq!(m.ram[0xc81a], 0);
        assert_eq!(m.registers[0], base | 0x10);
        assert_eq!(m.registers[8], 0);
        assert_eq!(m.registers[10], 38);
        assert_eq!(m.status_reads, 1);
        m.counter = 38;
        m.call("rt_cv1_hud_line");
        assert_eq!(m.registers[8], 0xd7);
        assert_eq!(m.registers[0], base);
        assert_eq!(m.registers[10], 0xff);
        assert_eq!(m.ram[FLAGS], 11);
        assert_eq!(m.ram[0xc81b], 0);
        let ports = m.ports.len();
        m.call("rt_cv1_hud_line");
        assert_eq!(m.ports.len(), ports, "at most one split per physical epoch");
        m.counter = 0xe0;
        m.status = 0x80;
        m.call("rt_cv1_hud_vblank");
        assert_eq!(m.ram[0xc819], 1);
        assert_eq!(m.ram[FLAGS], 7);
        assert_eq!(m.registers[8], 0);
        assert_eq!(m.status_reads, 2);
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn commit_reads_frozen_sram_record_without_advancing_physical_epoch() {
    let mut m = Machine::assembled();
    m.ram[0xdffc] = 8;
    m.ram[0xd47f] = 1;
    m.ram[0xc819] = 42;
    m.sram[0x3460..0x3466].copy_from_slice(&[3, 0xf0, 7, 0xc0, 38, 0x46]);
    let mut cpu = m.cpu("rt_cv1_hud_commit");
    cpu.h = 0xb4;
    cpu.l = 0x60;
    m.run(cpu);
    assert_eq!(m.ram[0xc819], 42);
    assert_eq!(m.ram[0xc81a], 42);
    assert_eq!(m.registers[8], 0xf0);
    assert_eq!(m.registers[9], 7);
    assert_eq!(m.ram[0xc816], 0xc0);
    assert_eq!(m.status_reads, 0, "commit must not acknowledge VINT");
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn guarded_di_poll_services_only_due_active_split_without_acknowledging_vint() {
    for depth in [0, 1, 2] {
        for sram in [0, 8] {
            for counter in [0, 37, 38, 47, 48, 0xdf, 0xe0, 0xff] {
                let mut m = Machine::assembled();
                m.record(7, 0x66);
                m.ram[0xd47f] = depth;
                m.ram[0xdffc] = sram;
                m.ram[0xdfff] = 2;
                m.counter = counter;
                m.call("rt_cv1_hud_poll_line");
                assert_eq!(m.status, 0x80);
                assert_eq!(m.status_reads, 0);
                let due = (38..0xe0).contains(&counter);
                assert_eq!(!m.ports.is_empty(), due);
                assert_eq!(m.ram[FLAGS], if due { 11 } else { 7 });
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn tail_acknowledgment_services_the_due_split_and_returns_original_status() {
    let mut m = Machine::assembled();
    m.record(7, 0x46);
    m.counter = 38;
    m.status = 0x20; // HINT/sprite status, not an unserviced VBlank.
    let cpu = m.call("rt_cv1_hud_tail_ack");
    assert_eq!(cpu.a, 0x20);
    assert_eq!(m.status_reads, 1);
    assert_eq!(m.registers[8], 0xd7);
    assert_eq!(m.ram[FLAGS], 11);
    assert_eq!(m.ram[0xc819], 0);
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn tail_acknowledgment_owns_consumed_vblank_without_running_a_producer() {
    for depth in [0, 1, 2] {
        for mapping in [0, 8] {
            for status in [0x80, 0xa0] {
                let mut m = Machine::assembled();
                m.record(11, 0x46); // Previous physical frame already split.
                m.ram[0xc819] = 0xff;
                m.ram[0xc81a..0xc81c].fill(0xff);
                m.ram[0xd47f] = depth;
                m.ram[0xdffc] = mapping;
                m.ram[0xc820] = 0; // No new presentation available.
                m.ram[0xc821..0xc82e].fill(0xa5);
                m.counter = 0xe0;
                m.status = status;
                let mut cpu = m.cpu("rt_cv1_hud_tail_ack");
                (cpu.b, cpu.c, cpu.d, cpu.e, cpu.f) = (0x12, 0x34, 0x56, 0x78, 0x95);
                let result = m.run(cpu);
                assert_eq!(result.a, status);
                assert_eq!(
                    (result.b, result.c, result.d, result.e, result.f),
                    (0x12, 0x34, 0x56, 0x78, 0x95)
                );
                assert_eq!(m.ram[0xc819], 0, "acknowledged VBlank owns a new epoch");
                assert_eq!(m.ram[0xc81a], 0);
                assert_eq!(m.ram[FLAGS], 7);
                assert_eq!(m.registers[0], 0x56);
                assert_eq!(m.registers[8], 0);
                assert_eq!(m.registers[10], 38);
                assert_eq!(m.ram[0xc820], 0);
                assert!(m.ram[0xc821..0xc82e].iter().all(|b| *b == 0xa5));
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn tail_late_vblank_advances_epoch_but_does_not_claim_a_repaired_hud() {
    for counter in [0, 38, 100, 0xdf, 0xfd, 0xff] {
        let mut m = Machine::assembled();
        m.record(11, 0x46);
        m.counter = counter;
        m.status = 0x80;
        let result = m.call("rt_cv1_hud_tail_ack");
        assert_eq!(result.a, 0x80);
        assert_eq!(m.ram[0xc819], 1);
        assert_eq!(m.ram[FLAGS], 3);
        assert_eq!(m.registers[8], 0xd7);
        assert_eq!(m.registers[0], 0x46);
        assert_eq!(m.registers[10], 0xff);
        assert_eq!(m.status_reads, 1);
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn late_rearm_never_restarts_pre_scroll_or_clears_pending_frame_status() {
    for counter in [0, 37, 38, 100, 0xdf, 0xfd, 0xff] {
        let mut m = Machine::assembled();
        m.record(3, 0x46);
        m.counter = counter;
        m.call("rt_cv1_hud_vblank");
        assert_eq!(m.status_reads, 0);
        assert_eq!(m.registers[8], 0xd7);
        assert_eq!(m.registers[0], 0x46);
        assert_eq!(m.registers[10], 0xff);
        assert_eq!(m.ram[FLAGS] & 4, 0);
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn exact_arm_deadline_accepts_late_combined_prefix_and_line_scroll_is_bounded() {
    let mut epoch = Machine::assembled();
    let mut cpu = epoch.cpu("rt_cv1_hud_epoch_begin");
    let mut cycles = 0;
    while cpu.pc != 7 {
        cycles += epoch.exact_step(&mut cpu);
    }
    assert_eq!(cycles, 71);
    assert!(epoch.ports.is_empty());

    for (counter, acknowledge) in [
        (0xe0, 1),
        (0xea, 1),
        (0xed, 0),
        (0xee, 0),
        (0xfc, 0),
        (0xfc, 1),
    ] {
        let mut m = Machine::assembled();
        m.record(3, 0x46);
        m.counter = counter;
        let mut cpu = m.cpu("_cv1_hud_arm");
        cpu.c = acknowledge;
        let mut cycles = 0;
        let mut before_sample = 0;
        while m.ports.len() < 10 {
            if m.read(cpu.pc) == 0xdb && m.read(cpu.pc + 1) == 0x7e {
                before_sample = cycles;
            }
            cycles += m.exact_step(&mut cpu);
        }
        assert_eq!(
            cycles - before_sample,
            if acknowledge == 0 { 363 } else { 369 }
        );
        // Latest FC = physical258. Even sampling its last instant leaves
        // complete lines259+260 before line261 reloads R10/latches R9.
        assert!(cycles - before_sample < 2 * 228);
        assert_eq!(m.registers[0], 0x56);
        assert_eq!(m.ram[FLAGS] & 4, 4);
        m.run(cpu);
    }

    let mut m = Machine::assembled();
    m.record(7, 0x46);
    let mut cpu = m.cpu("rt_cv1_hud_line");
    let mut cycles = 0;
    while m.ports.len() < 2 {
        cycles += m.exact_step(&mut cpu);
    }
    assert_eq!(cycles, 68);
    // This is helper-entry→lastR8 only, not complete IRQ or DI-chunk latency.
    assert_eq!(m.registers[8], 0xd7);
    m.run(cpu);

    let mut m = Machine::assembled();
    m.record(7, 0x46);
    m.counter = 38;
    let mut cpu = m.cpu("rt_cv1_hud_poll_line");
    let mut cycles = 0;
    while m.ports.len() < 2 {
        cycles += m.exact_step(&mut cpu);
    }
    assert_eq!(cycles, 113);
    assert_eq!(m.status_reads, 0);
    m.run(cpu);
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn chunk_admission_reserves_hud_and_next_frame_boundaries_without_io_side_effects() {
    let base = Machine::assembled();
    for (flags, cost, counter, admitted) in [
        (7, 27, 0, true),
        (7, 27, 8, true),
        (7, 27, 9, false),
        (7, 46, 0, false),
        (7, 27, 38, false),
        (11, 131, 38, true),
        (11, 131, 90, true),
        (11, 131, 91, false),
        (11, 46, 175, true),
        (11, 46, 176, false),
        (11, 27, 194, true),
        (11, 27, 195, false),
        (11, 27, 0xe0, false),
        (11, 27, 0xff, false),
        (11, 27, 0, false), // served flag alone cannot survive counter wrap
        (1, 27, 0, true),
        (1, 27, 0xe0, false),
        (0, 131, 0xe0, true),
        (7, 0, 0, false),
        (11, 255, 100, false),
    ] {
        let mut m = base.clone();
        m.record(flags, 0x46);
        m.counter = counter;
        let mut cpu = m.cpu("rt_cv1_hud_try_chunk");
        cpu.b = cost;
        let ram = m.ram.clone();
        let result = m.run(cpu);
        assert_eq!(
            result.f & 1 == 0,
            admitted,
            "flags={flags} cost={cost} VC={counter}"
        );
        assert_eq!(m.counter_reads, 1);
        assert_eq!(m.status_reads, 0);
        assert!(m.ports.is_empty());
        assert_eq!(m.ram, ram);
    }
    let mut m = base;
    m.record(7, 0x46);
    m.ram[0xc802] = 2; // explicit display-off fence
    let mut cpu = m.cpu("rt_cv1_hud_try_chunk");
    cpu.b = 131;
    assert_eq!(m.run(cpu).f & 1, 0);
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn short_blank_chunks_require_a_current_armed_split_and_leave_irq_status_untouched() {
    let base = Machine::assembled();
    for counter in [0xe0, 0xe5, 0xea, 0xfc, 0xff] {
        for (flags, epoch, armed_epoch, cost, expected) in [
            (7, 42, 42, 4, true),
            (7, 42, 42, 35, true),
            (7, 42, 42, 36, false),
            (7, 42, 42, 255, false),
            (7, 42, 42, 0, false),
            (7, 42, 41, 4, false),
            (7, 0, 255, 4, false),
            (7, 255, 0, 4, false),
            (7, 0, 0, 4, true),
            (11, 42, 42, 4, false),
            (15, 42, 42, 4, false),
            (3, 42, 42, 4, false),
            (1, 42, 42, 4, false),
        ] {
            let mut m = base.clone();
            m.record(flags, 0x46);
            m.ram[0xc819] = epoch;
            m.ram[0xc81a] = armed_epoch;
            m.counter = counter;
            let mut cpu = m.cpu("rt_cv1_hud_try_chunk");
            cpu.b = cost;
            let ram = m.ram.clone();
            let status = m.status;
            let result = m.run(cpu);
            assert_eq!(result.f & 1 == 0, expected);
            assert_eq!(m.ram, ram);
            assert_eq!(m.status, status);
            assert_eq!(m.counter_reads, 1);
            assert_eq!(m.status_reads, 0);
            assert!(m.ports.is_empty());
        }
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn serviced_old_split_requires_vblank_rearm_before_blank_work_can_resume() {
    let mut m = Machine::assembled();
    m.record(7, 0x46);
    m.ram[0xc819] = 255;
    m.ram[0xc81a] = 255;
    m.counter = 38;
    m.call("rt_cv1_hud_line"); // Caller serviced the previous pending HINT.
    assert_eq!(m.ram[FLAGS], 11);
    m.counter = 0xe0;
    m.status = 0x80;
    let mut cpu = m.cpu("rt_cv1_hud_try_chunk");
    cpu.b = 4;
    assert_eq!(m.run(cpu).f & 1, 1);
    assert_eq!(m.status, 0x80, "predicate cannot swallow the pending VINT");
    m.call("rt_cv1_hud_vblank"); // Real service boundary owns epoch and rearm.
    assert_eq!(m.ram[0xc819], 0);
    assert_eq!(m.ram[0xc81a], 0);
    let reads = m.status_reads;
    let mut cpu = m.cpu("rt_cv1_hud_try_chunk");
    cpu.b = 4;
    assert_eq!(m.run(cpu).f & 1, 0);
    assert_eq!(m.status_reads, reads);
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT assembled with Docker WLA-DX"]
fn helpers_respect_the_real_native_stack_floor_under_guarded_sram() {
    for (entry, extra) in [
        ("rt_cv1_hud_prepare", 4),
        ("rt_cv1_hud_commit", 0),
        ("rt_cv1_hud_vblank", 2),
        ("rt_cv1_hud_line", 0),
        ("rt_cv1_hud_poll_line", 0),
        ("rt_cv1_hud_tail_ack", 8),
    ] {
        let mut m = Machine::assembled();
        m.record(7, 0x46);
        m.ram[0xdffc] = 8;
        m.ram[0xd47f] = 2;
        m.ram[0xc821..0xc82a].copy_from_slice(&[0xb0, 0x1e, 0, 0, 7, 0, 0, 41, 0]);
        m.sram[0x3460..0x3466].copy_from_slice(&[3, 0, 0, 0xd7, 38, 0x46]);
        let mut cpu = m.cpu(entry);
        cpu.sp = 0xde40 + extra;
        m.ram[cpu.sp as usize] = 7;
        m.ram[cpu.sp as usize + 1] = 0;
        (cpu.h, cpu.l) = if entry == "rt_cv1_hud_prepare" {
            (0xc8, 0x21)
        } else {
            (0xb4, 0x60)
        };
        (cpu.d, cpu.e) = (0xb4, 0x60);
        m.run(cpu);
        assert_eq!(m.min_sp, 0xde40, "{entry}");
    }
}

#[test]
#[ignore = "requires CV1_HUD_PROJECT with CV1_COHERENT_BG assembled in Docker"]
fn complete_coherent_line_irq_meets_the_stock_deadline_and_preserves_guest_state() {
    let mut base = Machine::assembled();
    let irq = base.labels["irq_handler"].1;
    assert_eq!(&base.rom[0x38..0x3b], &[0xc3, irq as u8, (irq >> 8) as u8]);
    let line = base.labels["_irq_line_scroll_split"].1 as usize;
    let helper = base.labels["rt_cv1_hud_line"].1;
    assert_eq!(
        &base.rom[line..line + 3],
        &[0xcd, helper as u8, (helper >> 8) as u8],
        "this test requires the coherent IRQ branch"
    );
    base.record(7, 0x46);
    base.ram[0xcb28] = 1;
    base.status = 0; // actual line IRQ: no VINT bit
    base.counter = 38;
    for depth in [0, 1, 2] {
        for sram in [0, 8] {
            let mut m = base.clone();
            m.ram[0xd47f] = depth;
            m.ram[0xdffc] = sram;
            m.ram[0xdfff] = 2;
            let mut cpu = m.cpu("irq_handler");
            cpu.a = 0x95;
            cpu.f = 0x43;
            cpu.b = 0x37;
            cpu.c = 0xc8;
            cpu.h = 0x42;
            cpu.l = 0x69;
            let guest = (cpu.a, cpu.f, cpu.b, cpu.c, cpu.h, cpu.l);
            let mut cycles = 13 + 10; // IM1 acceptance plus actual vector JP
            while m.ports.len() < 2 {
                cycles += m.exact_step(&mut cpu);
            }
            assert_eq!(cycles, 348);
            // Conservatively include a longest23T interrupted instruction.
            // Source-anchored HINT is AFTER row38, leaving39..47 before48.
            assert!(cycles + 23 < 9 * 228);
            assert_eq!(m.registers[8], 0xd7);
            // Continue the real IRQ epilogue, including generic spill restores.
            // It ends with EI, unlike the DI-only helper runner.
            while cpu.pc != 7 {
                assert!(cpu.sp >= 0xde40);
                cpu.step(&mut m).unwrap();
            }
            assert_eq!((cpu.a, cpu.f, cpu.b, cpu.c, cpu.h, cpu.l), guest);
            assert_eq!(m.ram[0xd47f], depth);
            assert_eq!(m.ram[0xdffc], sram);
            assert_eq!(m.ram[0xdfff], 2);
            assert_eq!(m.ram[0xcb03], 0x65);
            assert!(m.latch.is_none());
            assert!(cpu.iff1 && cpu.iff2);
        }
    }
}
