//! Execute the real Docker-assembled cache/converters, not a Rust cache model.
//! CV1_SPRITE_PROJECT=out/<candidate> cargo test -p nes_to_sms \
//!   --test sprite_commit -- --ignored
//! The ROM and extracted bytes stay in the ignored local project directory.

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

const FLAGS: usize = 0xc802;
const CURRENT: usize = 0xd440;
const NEXT: usize = 0xd448;

#[derive(Clone, Debug)]
struct Write {
    address: usize,
    value: u8,
    display: bool,
}

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    ram: Box<[u8; 65536]>,
    sram: Box<[u8; 16384]>,
    vram: Box<[u8; 16384]>,
    labels: HashMap<String, (u8, u16)>,
    regs: [u8; 16],
    latch: Option<u8>,
    address: usize,
    code: u8,
    writes: Vec<Write>,
    ports: Vec<(u8, u8)>,
    counters: Vec<u8>,
    counter_reads: usize,
    builds: usize,
    lookup_cycles: u64,
    b_scan_entries: usize,
    instructions: usize,
    min_sp: u16,
    stack_floor: u16,
}

impl Machine {
    fn new() -> Self {
        Self::from_project(PathBuf::from(
            std::env::var("CV1_SPRITE_PROJECT")
                .expect("set CV1_SPRITE_PROJECT to an assembled CV1 candidate"),
        ))
    }

    fn from_project(dir: PathBuf) -> Self {
        let dir = if dir.is_absolute() {
            dir
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(dir)
        };
        let mut labels = HashMap::new();
        for line in std::fs::read_to_string(dir.join("sms.sym"))
            .unwrap()
            .lines()
        {
            let mut fields = line.split_whitespace();
            let (Some(location), Some(name)) = (fields.next(), fields.next()) else {
                continue;
            };
            let Some((bank, addr)) = location.split_once(':') else {
                continue;
            };
            if let (Ok(bank), Ok(addr)) =
                (u8::from_str_radix(bank, 16), u16::from_str_radix(addr, 16))
            {
                labels.insert(name.to_owned(), (bank, addr));
            }
        }
        assert!(labels.contains_key("rt_sat_upload"));
        let mut m = Self {
            rom: std::fs::read(dir.join("sms.sms")).unwrap(),
            ram: Box::new([0; 65536]),
            sram: Box::new([0; 16384]),
            vram: Box::new([0; 16384]),
            labels,
            regs: [0; 16],
            latch: None,
            address: 0,
            code: 0,
            writes: Vec::new(),
            ports: Vec::new(),
            counters: Vec::new(),
            counter_reads: 0,
            builds: 0,
            lookup_cycles: 0,
            b_scan_entries: 0,
            instructions: 0,
            min_sp: 0xffff,
            stack_floor: 0xdfd0,
        };
        m.ram[0xdffe] = 1;
        m.ram[0xdfff] = m.labels["data_prg_bank_3"].0;
        m.ram[0xcb62] = 3;
        m.ram[0xcb14] = 1;
        m.ram[0xcb03] = 0x65;
        m.ram[0xcb08] = 0xb0;
        m.ram[0xcb09] = 0x1e;
        m.ram[0xca39] = 1;
        m.regs[1] = 0xf0;
        m.regs[6] = 0xff;
        for (i, byte) in m.sram[0x800..0x2800].iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(29).rotate_left((i >> 8) as u32 & 7) ^ (i >> 4) as u8;
        }
        m.oam(&[]);
        m
    }

    fn oam(&mut self, entries: &[[u8; 4]]) {
        self.ram[0xc900..0xca00].fill(0xff);
        for (i, entry) in entries.iter().enumerate() {
            self.ram[0xc900 + i * 4..0xc904 + i * 4].copy_from_slice(entry);
        }
    }

    fn call(&mut self, label: &str) -> Cpu {
        let mut cpu = Cpu::new();
        let (bank, address) = self.labels[label];
        assert_eq!(bank, 0, "runtime helper must stay fixed-bank");
        cpu.pc = address;
        cpu.sp = 0xdff0;
        self.ram[0xdff0] = 7;
        self.ram[0xdff1] = 0;
        cpu.a = 0x69;
        cpu.d = 0x52;
        cpu.e = 0xa9;
        self.execute(cpu)
    }

    fn execute(&mut self, mut cpu: Cpu) -> Cpu {
        let entry_sp = cpu.sp;
        let mapping: Vec<_> = [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f]
            .into_iter()
            .map(|a| self.ram[a])
            .collect();
        for _ in 0..2_000_000 {
            self.min_sp = self.min_sp.min(cpu.sp);
            assert!(
                cpu.sp >= self.stack_floor,
                "sprite helper crossed its stack floor"
            );
            assert!(!cpu.iff1 && !cpu.iff2, "preparation must not admit an IRQ");
            if cpu.pc == 7 {
                assert_eq!(cpu.sp, entry_sp + 2, "unbalanced helper native stack");
                assert_eq!(self.ram[0xcb03], 0x65, "guest flags changed");
                assert_eq!(
                    [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f]
                        .map(|a| self.ram[a])
                        .as_slice(),
                    mapping
                );
                return cpu;
            }
            assert!(
                !cpu.halted,
                "runtime trap {:02x} at {:04x}",
                self.ram[0xcb1d], cpu.pc
            );
            if cpu.pc == self.labels["_sat_build_pair_8x16"].1
                || cpu.pc == self.labels["do_sprite_variant"].1
            {
                self.builds += 1;
            }
            let pc = cpu.pc;
            if self
                .labels
                .get("_cv1_sat_b_loop")
                .is_some_and(|p| p.1 == pc)
            {
                self.b_scan_entries += 1;
            }
            self.instructions += 1;
            let before = cpu.cycles;
            cpu.step(self)
                .unwrap_or_else(|e| panic!("{:04x}: {e:?}", cpu.pc));
            if let (Some(start), Some(end)) = (
                self.labels.get("_cv1_sat_lookup"),
                self.labels.get("_cv1_sat_bit"),
            ) && (start.1..end.1).contains(&pc)
            {
                self.lookup_cycles += cpu.cycles - before;
            }
        }
        panic!("runtime did not return, PC={:04x}", cpu.pc);
    }

    fn present(&mut self) {
        self.call("rt_cv1_sat_prepare");
        self.call("rt_cv1_sat_commit");
    }

    fn displayed_patterns(&self) -> Vec<(usize, Vec<u8>)> {
        (0..64)
            .filter(|slot| self.ram[CURRENT + slot / 8] & (1 << (slot % 8)) != 0)
            .map(|slot| {
                (
                    slot,
                    self.vram[0x2000 + slot * 64..0x2040 + slot * 64].to_vec(),
                )
            })
            .collect()
    }

    fn assert_patterns_unchanged(&self, before: &[(usize, Vec<u8>)]) {
        for (slot, bytes) in before {
            assert_eq!(&self.vram[0x2000 + slot * 64..0x2040 + slot * 64], bytes);
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
        match port {
            0x7e => {
                let value = self
                    .counters
                    .get(self.counter_reads)
                    .copied()
                    .unwrap_or(0xe0);
                self.counter_reads += 1;
                value
            }
            0xbf => {
                self.latch = None;
                0x80
            }
            _ => 0xff,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.ports.push((port, value));
        match port {
            0xbf => {
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
            }
            0xbe if self.code != 3 => {
                if self.regs[1] & 0x40 != 0 && (0x2000..0x3000).contains(&self.address) {
                    let slot = (self.address - 0x2000) / 64;
                    assert_eq!(
                        self.ram[CURRENT + slot / 8] & (1 << (slot % 8)),
                        0,
                        "wrote displayed pair slot {slot} before SAT commit"
                    );
                }
                self.vram[self.address] = value;
                self.writes.push(Write {
                    address: self.address,
                    value,
                    display: self.regs[1] & 0x40 != 0,
                });
                self.address = (self.address + 1) & 0x3fff;
            }
            _ => {}
        }
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn duplicate_keys_and_oam_reorder_reuse_the_same_complete_pair() {
    let mut m = Machine::new();
    m.oam(&[[40, 3, 0x41, 20], [48, 3, 0x61, 28], [60, 8, 0x82, 50]]);
    m.present();
    assert_eq!(m.builds, 2, "priority is excluded from the key");
    assert_eq!(m.vram[0x3f81], m.vram[0x3f83]);
    let old = m.displayed_patterns();
    let pins = m.ram[CURRENT..CURRENT + 8].to_vec();
    m.oam(&[[60, 8, 0x82, 50], [48, 3, 0x61, 28], [40, 3, 0x41, 20]]);
    m.writes.clear();
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.builds, 2);
    assert!(m.writes.is_empty(), "hits must not touch VRAM");
    assert_eq!(&m.ram[CURRENT..CURRENT + 8], pins);
    m.assert_patterns_unchanged(&old);
    m.call("rt_cv1_sat_commit");
    assert_eq!(&m.vram[0x3f00..0x3f03], &[61, 49, 41]);
    assert!(m.vram[0x3f03..0x3f40].iter().all(|b| *b == 0xe0));
    assert_eq!(m.regs[1], 0xf2);
    assert_eq!(m.regs[6], 0xff);
    assert!(m.min_sp >= 0xdfd0);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn stepped_preparation_matches_blocking_payload_and_returns_with_closed_ownership() {
    let mut stepped = Machine::new();
    stepped.oam(&[[40, 3, 0x41, 20], [48, 3, 0x61, 28], [60, 8, 0x82, 50]]);
    let mut blocking = stepped.clone();
    blocking.call("rt_cv1_sat_prepare");
    stepped.call("rt_cv1_sat_prepare_begin");
    let mut calls = 0;
    loop {
        let reads = stepped.counter_reads;
        let phase = stepped.ram[FLAGS] & 0xf0;
        let index = stepped.ram[0xc806];
        let mut next = usize::from(index) + 1;
        if [0x20, 0x30].contains(&phase) && stepped.ram[0xc900 + usize::from(index) * 4] >= 0xcf {
            while next < 64
                && next < usize::from(index) + 8
                && stepped.ram[0xc900 + next * 4] >= 0xcf
            {
                next += 1;
            }
        }
        let result = stepped.call("rt_cv1_sat_prepare_step");
        assert_eq!(stepped.counter_reads, reads, "bounded step must never wait");
        assert!(stepped.latch.is_none());
        calls += 1;
        assert!(calls <= 132);
        if [0x20, 0x30].contains(&phase) && result.a != 2 {
            let expected = if next == 64 && stepped.ram[FLAGS] & 0xf0 == 0x30 {
                0
            } else {
                next as u8
            };
            assert_eq!(stepped.ram[0xc806], expected, "bounded hidden batching");
        }
        match result.a {
            0 => break,
            1 => {}
            2 => {
                stepped.call("rt_cv1_sat_blank");
            }
            status => panic!("unexpected step status {status}"),
        }
    }
    assert_eq!(&stepped.ram[0xc840..0xc900], &blocking.ram[0xc840..0xc900]);
    assert_eq!(
        &stepped.ram[CURRENT..NEXT + 8],
        &blocking.ram[CURRENT..NEXT + 8]
    );
    assert_eq!(stepped.vram, blocking.vram);
    assert_eq!(stepped.ram[FLAGS], blocking.ram[FLAGS]);
    assert_eq!(stepped.ram[FLAGS] & 0xf0, 0);
    stepped.call("rt_cv1_sat_commit");
    stepped.call("rt_cv1_sat_prepare_begin");
    // One initialization, three visible entries and eight hidden batches.
    // Every call starts with fresh native registers, proving RAM-owned cursors.
    for n in 0..12 {
        let cpu = stepped.call("rt_cv1_sat_prepare_step");
        assert_eq!(cpu.a, u8::from(n != 11));
    }
    assert_eq!(stepped.builds, 2);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn stepped_overflow_requests_external_blank_without_waiting_or_reusing_current_slots() {
    let mut m = Machine::new();
    let entries: Vec<_> = (0..64).map(|i| [40, i, 0, i]).collect();
    m.oam(&entries);
    m.present();
    let old = m.displayed_patterns();
    m.oam(&[[40, 128, 0, 20]]);
    m.call("rt_cv1_sat_prepare_begin");
    for _ in 0..10 {
        assert_eq!(m.call("rt_cv1_sat_prepare_step").a, 1);
    }
    m.ports.clear();
    let reads = m.counter_reads;
    let result = m.call("rt_cv1_sat_prepare_step");
    assert_eq!(result.a, 2);
    assert!(m.ports.is_empty());
    assert_eq!(m.counter_reads, reads);
    assert_eq!(m.ram[FLAGS] & 0xf0, 0x10);
    assert_eq!(m.regs[1], 0xf2);
    m.assert_patterns_unchanged(&old);
    m.call("rt_cv1_sat_blank");
    for n in 0..19 {
        let result = m.call("rt_cv1_sat_prepare_step");
        assert_eq!(result.a, u8::from(n != 18));
    }
    assert_eq!(m.ram[FLAGS] & 0xf0, 0);
    m.call("rt_cv1_sat_commit");
    assert_eq!(m.regs[1], 0xf2);
    assert_eq!(m.ram[CURRENT], 1);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn same_tile_different_attributes_remain_distinct_after_reordering() {
    let mut m = Machine::new();
    let mut entries: Vec<_> = (0..16)
        .map(|i| [40, 0xff, ((i & 12) << 4) | (i & 3), i * 8])
        .collect();
    m.oam(&entries);
    m.present();
    assert_eq!(m.builds, 16);
    for i in 0..16 {
        assert_eq!(m.vram[0x3f81 + i * 2], i as u8 * 2);
    }
    entries.reverse();
    m.oam(&entries);
    m.writes.clear();
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.builds, 16);
    assert!(m.writes.is_empty());
    for i in 0..16 {
        assert_eq!(m.ram[0xc881 + i * 2], (15 - i) as u8 * 2);
    }
    m.call("rt_cv1_sat_commit");
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn lookup_reports_slot_and_carry_without_exposing_other_registers_as_an_abi() {
    for slot in [0, 31, 63] {
        let mut m = Machine::new();
        m.ram[0xd480..0xd4c0].fill(0xff);
        m.ram[0xd400 + slot] = 0xff;
        m.ram[0xd480 + slot] = 0xc2;
        m.ram[0xc807] = 0xff;
        m.ram[0xc808] = 0xc2;
        let hit = m.call("_cv1_sat_lookup");
        assert_eq!(hit.c, slot as u8);
        assert_eq!(hit.f & 1, 0, "hit must clear carry");
        m.ram[0xc808] = 0xc3;
        let miss = m.call("_cv1_sat_lookup");
        assert_ne!(miss.f & 1, 0, "same tile, missing attr must set carry");
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn read_only_step_budgets_cover_stable_modes_hidden_entries_and_fallbacks() {
    fn classify(m: &mut Machine, expected: u8) -> u64 {
        let mut cpu = Cpu::new();
        cpu.pc = m.labels["rt_cv1_sat_step_lines"].1;
        cpu.sp = 0xde40;
        (cpu.d, cpu.e) = (0x52, 0xa9);
        m.ram[0xde40] = 7;
        m.ram[0xde41] = 0;
        let ram = m.ram.clone();
        let mut t = 0;
        for _ in 0..100 {
            if cpu.pc == 7 {
                assert_eq!(cpu.b, expected);
                assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
                assert_eq!(cpu.sp, 0xde42);
                assert_eq!(m.ram, ram, "classification changed producer/cache state");
                assert!(m.ports.is_empty());
                assert!(m.writes.is_empty());
                assert_eq!(m.counter_reads, 0);
                return t;
            }
            assert!(!cpu.iff1 && !cpu.iff2);
            assert!(cpu.sp >= 0xde40);
            let pc = cpu.pc;
            let sp = cpu.sp;
            let opcode = m.read(pc);
            cpu.step(m).unwrap();
            // Exact executed classifier instructions, not Cpu.cycles.
            t += match opcode {
                0x3a => 13,
                0x06 | 0x26 | 0x3e | 0xe6 | 0xfe => 7,
                0x18 => 12,
                0x20 | 0x28 | 0x30 | 0x38 => {
                    if cpu.pc == pc + 2 {
                        7
                    } else {
                        12
                    }
                }
                0xc0 | 0xc8 | 0xd0 | 0xd8 => {
                    if cpu.sp == sp {
                        5
                    } else {
                        11
                    }
                }
                0xc9 => 10,
                0xcb => 8,
                0x7e => 7,
                0x0f | 0x4f | 0x6f | 0x87 | 0xb7 | 0xb9 => 4,
                _ => panic!("unbudgeted classifier opcode {opcode:02x}"),
            };
        }
        panic!("classifier did not return");
    }
    let base = Machine::new();
    let mut max_t = 0;
    for ctrl in 0u8..=255 {
        for mode in [0, 1, 2] {
            for flags in [0x10, 0x14, 0x16] {
                for dirty in [0, 1] {
                    let mut m = base.clone();
                    m.ram[0xcb08] = ctrl;
                    m.ram[0xd468] = mode;
                    m.ram[FLAGS] = flags;
                    m.ram[0xc801] = dirty;
                    let expected_mode = if ctrl & 0x20 != 0 {
                        1
                    } else if ctrl & 8 != 0 {
                        2
                    } else {
                        0
                    };
                    let expected = if flags != 0x14 || mode != expected_mode {
                        46
                    } else if dirty == 0 {
                        23
                    } else {
                        30
                    };
                    max_t = max_t.max(classify(&mut m, expected));
                }
            }
        }
    }
    for (phase, fallback) in [(0x20, 27), (0x30, 131)] {
        for index in [0, 63, 64, 255] {
            for y in 0u8..=255 {
                let mut m = base.clone();
                m.ram[FLAGS] = phase | 4;
                m.ram[0xc806] = index;
                m.ram[0xc900 + usize::from(index & 63) * 4] = y;
                let expected = if index < 64 && y >= 0xcf { 7 } else { fallback };
                max_t = max_t.max(classify(&mut m, expected));
            }
        }
    }
    assert!(max_t <= 256, "classifier used {max_t} exact T states");
    eprintln!("SAT read-only budget classifier maximum {max_t} exact T states");
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT; optional CV1_SPRITE_COST_INPUT and CV1_SPRITE_COST_REFERENCE"]
fn assembled_helper_cost_probe_reports_approximate_cpu_attribution() {
    let mut frames = [
        (0..21)
            .map(|i| [40, (i % 14) * 2, 0, i * 8])
            .collect::<Vec<_>>(),
        (0..21)
            .rev()
            .map(|i| [40, (i % 14) * 2, 0, i * 8])
            .collect::<Vec<_>>(),
    ];
    let source = if let Ok(dir) = std::env::var("CV1_SPRITE_COST_INPUT") {
        let dir = PathBuf::from(dir);
        let dir = if dir.is_absolute() {
            dir
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(dir)
        };
        for (index, tick) in [180, 181].into_iter().enumerate() {
            let bytes = std::fs::read(dir.join(format!("oam-{tick}.bin"))).unwrap();
            assert_eq!(bytes.len(), 256);
            frames[index] = bytes.as_chunks::<4>().0.to_vec();
        }
        "actual-core OAM tick180/181; synthetic CHR bytes; frame180-warmed cache"
    } else {
        "synthetic21-sprite14-key reorder; synthetic CHR bytes"
    };
    let mut projects = vec![("candidate", Machine::new())];
    if let Ok(project) = std::env::var("CV1_SPRITE_COST_REFERENCE") {
        projects.push(("reference", Machine::from_project(PathBuf::from(project))));
    }
    for (name, mut warmed) in projects {
        let prepared = warmed.labels.contains_key("rt_cv1_sat_prepare");
        warmed.oam(&frames[0]);
        if prepared {
            warmed.present();
        } else {
            warmed.call("rt_sat_upload");
        }
        for (scenario, oam) in [("unchanged", &frames[0]), ("next_oam", &frames[1])] {
            let mut m = warmed.clone();
            m.oam(oam);
            m.builds = 0;
            m.lookup_cycles = 0;
            m.b_scan_entries = 0;
            m.instructions = 0;
            // The legacy index-owned resolver is allowed to rewrite displayed
            // patterns; it has no shared-cache pins. The fixture starts them0.
            let cpu = m.call(if prepared {
                "rt_cv1_sat_prepare"
            } else {
                "rt_sat_upload"
            });
            eprintln!(
                "helper_cost source={source:?} project={name} scenario={scenario} kind={} approximate_cpu_cycles={} lookup_approximate_cpu_cycles={} builds={} instructions={} second_pass_entries={}",
                if prepared {
                    "prepare_without_wait_or_commit"
                } else {
                    "legacy_resolve_and_upload"
                },
                cpu.cycles,
                m.lookup_cycles,
                m.builds,
                m.instructions,
                m.b_scan_entries
            );
        }
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn repeated_byte_fill_matches_all_fixed_counts_without_crossing_boundaries() {
    for count in [2u16, 8, 16, 64, 128] {
        for value in [0, 0xe0, 0xff] {
            let mut m = Machine::new();
            m.ram[0xc83f..0xc901].fill(0x5a);
            let mut cpu = Cpu::new();
            cpu.pc = m.labels["_cv1_sat_fill"].1;
            cpu.sp = 0xdff0;
            m.ram[0xdff0] = 7;
            cpu.a = value;
            [cpu.b, cpu.c] = count.to_be_bytes();
            cpu.h = 0xc8;
            cpu.l = 0x40;
            m.execute(cpu);
            assert_eq!(m.ram[0xc83f], 0x5a);
            assert!(
                m.ram[0xc840..0xc840 + count as usize]
                    .iter()
                    .all(|b| *b == value)
            );
            assert_eq!(m.ram[0xc840 + count as usize], 0x5a);
        }
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn all_hits_skip_second_pass_and_dirty_generation_resets_the_miss_flag() {
    let mut m = Machine::new();
    m.oam(&[[40, 2, 0, 20], [50, 3, 0xc3, 30]]);
    m.present();
    assert_eq!(m.builds, 2);
    m.ram[FLAGS] |= 8; // A previous generation's misses must not force pass B.
    m.b_scan_entries = 0;
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.b_scan_entries, 0);
    assert_eq!(m.ram[FLAGS] & 9, 1);
    assert_eq!(m.builds, 2);
    assert_eq!(&m.ram[0xc880..0xc884], &[20, 0, 30, 2]);
    m.call("rt_cv1_sat_commit");
    m.ram[0xc801] = 1;
    m.sram[0x820] ^= 0xff;
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.b_scan_entries, 10); // two visible entries + eight hidden batches
    assert_eq!(m.ram[FLAGS] & 9, 9);
    assert_eq!(m.builds, 4);
    assert_ne!(m.ram[0xc881], 0xff);
    assert_ne!(m.ram[0xc883], 0xff);
    m.call("rt_cv1_sat_commit");
    assert_eq!(m.ram[FLAGS], 4);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn hidden_batches_stop_before_visible_work_and_have_exact_whole_step_bounds() {
    let base = Machine::new();
    let mut maximum = [0u64; 2];
    for (kind, phase) in [0x20, 0x30].into_iter().enumerate() {
        for index in 0..64usize {
            for run in 1..=9 {
                for misses in [0, 8] {
                    for hidden_y in [0xcf, 0xff] {
                        let mut m = base.clone();
                        m.ram[FLAGS] = phase | 4 | misses;
                        m.ram[0xc806] = index as u8;
                        m.ram[0xc805] = 17;
                        m.ram[0xc840..0xc900].fill(0x5a);
                        m.ram[CURRENT..NEXT + 8].fill(0xa5);
                        m.ram[0xc900 + index * 4] = hidden_y;
                        if index + run < 64 {
                            m.ram[0xc900 + (index + run) * 4] = 0xce;
                        }
                        let mut cpu = Cpu::new();
                        cpu.pc = m.labels["rt_cv1_sat_prepare_step"].1;
                        cpu.sp = 0xde42; // CALL skip-helper reaches the real DE40 floor.
                        m.ram[0xde42] = 7;
                        m.ram[0xde43] = 0;
                        let before = m.ram.clone();
                        let mut t = 0;
                        for _ in 0..1000 {
                            if cpu.pc == 7 {
                                break;
                            }
                            assert!(!cpu.iff1 && !cpu.iff2);
                            assert!(cpu.sp >= 0xde40);
                            let pc = cpu.pc;
                            let opcode = m.read(pc);
                            let second = m.read(pc + 1);
                            cpu.step(&mut m).unwrap();
                            // Actual hidden-only path. Any lookup/converter or
                            // unexpected instruction fails this narrow observer.
                            t += match opcode {
                                0x3a | 0x32 => 13,
                                0x21 => 10,
                                0x06 | 0x26 | 0x3e | 0xe6 | 0xfe | 0xf6 => 7,
                                0x34 => 11,
                                0x7e => 7,
                                0x6f | 0x87 | 0xaf | 0xb7 | 0x37 => 4,
                                0x18 => 12,
                                0x10 => {
                                    if cpu.b == 0 {
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
                                0xc3 | 0xca | 0xda | 0xd2 => 10,
                                0xcd => 17,
                                0xc9 => 10,
                                0xcb if second & 0xc0 == 0x40 && second & 7 == 6 => 12,
                                _ => {
                                    panic!("unbudgeted hidden-step opcode {opcode:02x} at {pc:04x}")
                                }
                            };
                        }
                        assert_eq!(cpu.pc, 7);
                        assert_eq!(cpu.sp, 0xde44);
                        let next = (index + run.min(8)).min(64);
                        let restarts_b = next == 64 && phase == 0x20 && misses != 0;
                        assert_eq!(m.ram[0xc806], if restarts_b { 0 } else { next as u8 });
                        assert_eq!(m.ram[0xc805], if restarts_b { 0 } else { 17 });
                        assert_eq!(cpu.a, u8::from(next != 64 || restarts_b));
                        assert_eq!(
                            m.ram[FLAGS] & 0xf0,
                            if restarts_b {
                                0x30
                            } else if next == 64 {
                                0
                            } else {
                                phase
                            }
                        );
                        let mut expected = before;
                        for field in [FLAGS, 0xc805, 0xc806] {
                            expected[field] = m.ram[field];
                        }
                        assert_eq!(&m.ram[..0xde40], &expected[..0xde40]);
                        assert!(m.ports.is_empty() && m.writes.is_empty());
                        assert_eq!(m.counter_reads, 0);
                        assert!(m.latch.is_none());
                        maximum[kind] = maximum[kind].max(t);
                    }
                }
            }
        }
    }
    assert_eq!(maximum, [1053, 1003]);
    for cost in maximum {
        assert!(
            cost + 512 <= 7 * 228,
            "whole hidden batch exceeds its admission"
        );
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn pass_a_pins_a_late_hit_before_the_first_miss_allocates() {
    let mut m = Machine::new();
    m.ram[FLAGS] = 4;
    m.ram[0xd468] = 1;
    m.ram[0xd480..0xd4c0].fill(0xff);
    m.ram[0xd400] = 9;
    m.ram[0xd480] = 0x40;
    m.ram[0xc80a] = 0;
    m.vram[0x2000..0x2040].fill(0xa5);
    m.oam(&[[20, 4, 0, 10], [20, 9, 0x40, 30]]);
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.builds, 1);
    assert_eq!(m.ram[0xc883], 0, "late hit must retain slot zero");
    assert_eq!(m.ram[0xc881], 2, "miss allocates outside all pass-A hits");
    assert!(m.vram[0x2000..0x2040].iter().all(|b| *b == 0xa5));
    assert_eq!(m.ram[NEXT], 3);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn dirty_source_keeps_displayed_pairs_immutable_until_sat_commit() {
    let mut m = Machine::new();
    m.oam(&[[40, 2, 0, 20]]);
    m.present();
    let old = m.displayed_patterns();
    let old_tile = m.vram[0x3f81];
    m.sram[0x820] ^= 0xff; // one raw byte, not an entire tile upload
    m.ram[0xc801] = 1;
    m.writes.clear();
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.ram[0xc801], 0);
    assert_eq!(m.builds, 2);
    assert_ne!(m.ram[0xc881], old_tile);
    m.assert_patterns_unchanged(&old);
    assert_eq!(m.vram[0x3f81], old_tile);
    assert!(
        m.writes
            .iter()
            .all(|w| (0x2040..0x2080).contains(&w.address) && w.display)
    );
    m.call("rt_cv1_sat_commit");
    assert_eq!(m.vram[0x3f81], 2);
    assert_eq!(m.ram[CURRENT], 2, "old pins release only after commit");
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn all_64_pairs_fit_and_union_overflow_blanks_before_reusing_old_slots() {
    let mut m = Machine::new();
    let mut entries: Vec<_> = (0..64).map(|i| [40, i, 0, i]).collect();
    m.oam(&entries);
    m.present();
    assert_eq!(m.builds, 64);
    assert!(m.ram[CURRENT..CURRENT + 8].iter().all(|b| *b == 0xff));
    entries[0][1] = 128;
    m.oam(&entries);
    m.writes.clear();
    m.ports.clear();
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.builds, 128, "exactly one empty-cache rebuild");
    assert_eq!(m.ram[FLAGS] & 9, 9, "retry must record its own misses");
    assert!(m.writes.iter().all(|w| !w.display));
    assert_eq!(&m.ports[..2], &[(0xbf, 0xb2), (0xbf, 0x81)]);
    assert_eq!(m.regs[1], 0xb2);
    assert!(
        m.ram[0xc880..0xc900]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|xt| xt[1] != 0xff)
    );
    m.call("rt_cv1_sat_commit");
    assert_eq!(m.regs[1], 0xf2);
    assert_eq!(m.ram[FLAGS], 4);
    m.b_scan_entries = 0;
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.b_scan_entries, 0, "retry misses must not leak forward");
    assert_eq!(m.ram[FLAGS] & 9, 1);
    assert_eq!(m.builds, 128);
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn an_existing_bg_blank_releases_old_pins_before_preparation() {
    let mut m = Machine::new();
    let entries: Vec<_> = (0..64).map(|i| [40, i, 0, i]).collect();
    m.oam(&entries);
    m.present();
    m.call("rt_cv1_sat_blank");
    m.oam(&[[40, 128, 0, 20]]);
    m.writes.clear();
    m.call("rt_cv1_sat_prepare");
    assert_eq!(m.builds, 65);
    assert!(m.writes.iter().all(|w| !w.display));
    m.call("rt_cv1_sat_commit");
    assert_eq!(m.regs[1], 0xf2);
    assert_eq!(m.ram[CURRENT], 1);
    assert!(m.ram[CURRENT + 1..CURRENT + 8].iter().all(|b| *b == 0));
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn disabled_sprites_commit_hidden_sat_and_preserve_frozen_display_enable() {
    let mut m = Machine::new();
    m.oam(&[[40, 2, 0, 20]]);
    m.present();
    let builds = m.builds;
    for mask in [8, 0] {
        m.ram[0xcb09] = mask;
        m.present();
        assert_eq!(m.builds, builds);
        assert!(m.vram[0x3f00..0x3f40].iter().all(|b| *b == 0xe0));
        assert!(m.vram[0x3f80..0x4000].iter().all(|b| *b == 0));
        assert!(m.ram[CURRENT..CURRENT + 8].iter().all(|b| *b == 0));
        assert_eq!(m.regs[1], if mask == 0 { 0xb2 } else { 0xf2 });
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn mode_transitions_use_source_aware_8x8_pixels_without_touching_bg() {
    let mut m = Machine::new();
    m.oam(&[[40, 3, 0xc3, 20]]);
    m.present();
    m.vram[..0x2000].fill(0x5a);
    for control in [0x80, 0x88, 0xa0] {
        m.ram[0xcb08] = control;
        m.writes.clear();
        m.call("rt_cv1_sat_prepare");
        assert!(
            m.writes
                .iter()
                .all(|w| !w.display && (0x2000..0x3000).contains(&w.address))
        );
        assert!(m.vram[..0x2000].iter().all(|b| *b == 0x5a));
        if control & 0x20 == 0 {
            // Direct planar pixel oracle, not a parallel cache/allocator model.
            let source = 0x800 + usize::from(control & 8 != 0) * 0x1000 + 3 * 16;
            for row in 0..8 {
                let p0 = m.sram[source + 7 - row].reverse_bits();
                let p1 = m.sram[source + 15 - row].reverse_bits();
                assert_eq!(
                    &m.vram[0x2000 + row * 4..0x2004 + row * 4],
                    &[p0, p1, p0 | p1, p0 | p1]
                );
            }
        }
        m.call("rt_cv1_sat_commit");
        assert_eq!(m.regs[1], if control & 0x20 == 0 { 0xf0 } else { 0xf2 });
        assert_eq!(m.regs[6], 0xff);
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn cached_pair_pixels_match_original_converter_for_all_flip_palette_keys() {
    for tile in [2, 3, 254, 255] {
        for flip in [0, 0x40, 0x80, 0xc0] {
            for palette in 0..4 {
                let attr = flip | palette;
                let mut cached = Machine::new();
                cached.oam(&[[40, tile, attr, 20]]);
                let mut original = cached.clone();
                // Execute the unchanged real pair converter at destination0.
                let mut cpu = Cpu::new();
                cpu.pc = original.labels["rt_sat_build_pair_8x16"].1;
                cpu.sp = 0xdff0;
                original.ram[0xdff0] = 7;
                cpu.a = tile;
                cpu.b = attr;
                cpu.c = 0;
                original.execute(cpu);
                cached.call("rt_cv1_sat_prepare");
                assert_eq!(
                    &cached.vram[0x2000..0x2040],
                    &original.vram[0x2000..0x2040],
                    "tile={tile:02x}, attr={attr:02x}"
                );
                assert_eq!(cached.builds, 1);
                assert!(cached.min_sp >= 0xdfd0);
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn late_try_has_one_sample_and_no_memory_or_port_side_effects() {
    let mut prepared = Machine::new();
    prepared.oam(&[[40, 2, 0, 20]]);
    prepared.call("rt_cv1_sat_prepare");
    prepared.ram[0xc801] = 1;
    for late in [0, 0xdf, 0xed, 0xff] {
        let mut m = prepared.clone();
        m.ports.clear();
        m.writes.clear();
        m.counters = vec![late, 0xe0]; // a forbidden retry would then succeed
        m.counter_reads = 0;
        let ram = m.ram.clone();
        let sram = m.sram.clone();
        let vram = m.vram.clone();
        let regs = m.regs;
        let cpu = m.call("rt_cv1_sat_try_commit");
        assert_ne!(cpu.f & 1, 0, "late try must report carry");
        assert_eq!(m.counter_reads, 1, "try must never poll again");
        assert!(m.ports.is_empty() && m.writes.is_empty());
        assert_eq!(m.ram, ram, "failed try mutated RAM or pins");
        assert_eq!(m.sram, sram);
        assert_eq!(m.vram, vram);
        assert_eq!(m.regs, regs);
        assert!(m.latch.is_none());
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn admitted_commit_preserves_guarded_mappings_at_the_real_stack_floor() {
    let mut prepared = Machine::new();
    prepared.oam(&[[40, 2, 0, 20]]);
    prepared.call("rt_cv1_sat_prepare");
    for entry in [
        "rt_cv1_sat_try_commit",
        "rt_cv1_sat_commit",
        "rt_cv1_sat_commit_admitted",
    ] {
        for depth in [0, 1, 2] {
            for sram_control in [0, 8] {
                let mut m = prepared.clone();
                m.ram[0xd47f] = depth;
                m.ram[0xca19..0xca20].copy_from_slice(&[1, 0, 3, 3, 0, 8, 2]);
                m.ram[0xd3ff] = 3;
                m.ram[0xdffc] = sram_control;
                m.ram[0xdfff] = 2;
                m.ram[0xc801] = 1; // later CHR dirtiness is never consumed here
                m.ram[0xcb2d] = 0xa5; // newer producer enable intent
                m.ram[0xdd80..0xde40].fill(0x5a); // BGV_REFCNT, not stack space
                m.stack_floor = 0xde40;
                m.min_sp = 0xffff;
                let composed = entry == "rt_cv1_sat_commit_admitted";
                // Internal composition trusts its caller's earlier admission:
                // another sample here would either fail or spin forever.
                m.counters = if composed {
                    vec![0x20]
                } else {
                    vec![0xec, 0xed, 0xed]
                };
                m.counter_reads = 0;
                m.ports.clear();
                m.writes.clear();
                let guard = m.ram[0xca19..0xca20].to_vec();
                let sram = m.sram.clone();
                let mut cpu = Cpu::new();
                cpu.pc = m.labels[entry].1;
                // Blocking poll needs one call word. Other entries need none.
                cpu.sp = if entry == "rt_cv1_sat_commit" {
                    0xde42
                } else {
                    0xde40
                };
                m.ram[cpu.sp as usize] = 7;
                m.ram[cpu.sp as usize + 1] = 0;
                let result = m.execute(cpu);
                assert_eq!(result.f & 1, 0, "successful commit must clear carry");
                assert_eq!(m.min_sp, 0xde40);
                assert_eq!(&m.ram[0xca19..0xca20], guard);
                assert_eq!(m.ram[0xd3ff], 3);
                assert_eq!(m.sram, sram);
                assert_eq!(m.ram[0xc801], 1);
                assert_eq!(
                    m.ram[0xcb2d],
                    if m.labels.contains_key("rt_cv1_bg_init") {
                        0xa5
                    } else {
                        0
                    },
                    "coherent commit must not consume newer producer intent"
                );
                assert!(m.ram[0xdd80..0xde40].iter().all(|b| *b == 0x5a));
                assert_eq!(m.writes.len(), 192);
                assert_eq!(m.ram[FLAGS], 4);
                assert!(m.latch.is_none());
                let diagnostic = m.rom[m.labels["_cv1_sat_commit_admitted"].1 as usize] == 0xdb;
                let admission_reads = usize::from(!composed);
                assert_eq!(
                    m.counter_reads,
                    admission_reads + if diagnostic { 2 } else { 0 }
                );
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_SPRITE_PROJECT assembled with Docker WLA-DX"]
fn admitted_commit_is_bounded_and_rejects_late_blank() {
    let mut m = Machine::new();
    m.oam(&[[40, 2, 0, 20]]);
    m.call("rt_cv1_sat_prepare");
    m.writes.clear();
    m.counters = vec![0xed, 0xff, 0, 0xdf, 0xec];
    m.counter_reads = 0;
    m.call("rt_cv1_sat_commit");
    assert!(m.counter_reads >= 5, "late blank was incorrectly admitted");
    assert_eq!(m.writes.len(), 192);
    assert!(
        m.writes[..64]
            .iter()
            .enumerate()
            .all(|(i, w)| w.address == 0x3f00 + i)
    );
    assert!(
        m.writes[64..]
            .iter()
            .enumerate()
            .all(|(i, w)| w.address == 0x3f80 + i)
    );
    assert_eq!(m.writes[0].value, 41);
    assert!(m.latch.is_none(), "all control-port pairs complete");
    // E5..EA occurs twice on NTSC224. The byte alone cannot distinguish
    // those phases; the static bound below uses the later physical phase.
    for accepted in [0xe0, 0xe3, 0xe5, 0xea, 0xec] {
        m.call("rt_cv1_sat_prepare");
        m.counters = vec![accepted];
        m.counter_reads = 0;
        m.call("rt_cv1_sat_commit");
        assert!(m.counter_reads <= 3, "valid blank counter rejected");
    }

    // Exact straight-line opcode sum, deliberately NOT Cpu.cycles (approximate).
    let mut pc = m.labels["_cv1_sat_commit_admitted"].1;
    let end = m.labels["_cv1_sat_commit_finished"].1;
    let mut cycles = 0;
    let mut outi = 0;
    while pc != end {
        let opcode = m.read(pc);
        let (length, cost) = match opcode {
            0xaf => (1, 4),
            0x3e => (2, 7),
            0xd3 | 0xdb => (2, 11),
            0x21 | 0x01 => (3, 10),
            0x32 | 0x3a => (3, 13),
            0xed => {
                assert_eq!(m.read(pc + 1), 0xa3);
                outi += 1;
                (2, 16)
            }
            _ => panic!("unexpected/unbounded burst opcode {opcode:02x} at {pc:04x}"),
        };
        pc += length;
        cycles += cost;
    }
    assert_eq!(outi, 192);
    // The try path must branch directly into this burst after ONE sample.
    // Assert the assembled bytes, including its target and the failed RET.
    let sample = m.labels["_cv1_sat_try_sample"].1;
    let code: Vec<_> = (0..10).map(|i| m.read(sample + i)).collect();
    assert_eq!(&code[..7], &[0xdb, 0x7e, 0xd6, 0xe0, 0xfe, 13, 0x38]);
    assert_eq!(&code[8..], &[0x37, 0xc9]);
    assert_eq!(
        sample
            .wrapping_add(8)
            .wrapping_add_signed(i16::from(code[7] as i8)),
        m.labels["_cv1_sat_commit_admitted"].1
    );
    // Conservatively include the entire accepted poll: IN(11), SUB(7),
    // CP(7), JR-not-taken(7), RET(10), rather than only the admission label.
    assert!(
        cycles + 42 <= 4096,
        "admitted burst is {cycles}+42 T states"
    );
    assert!(
        cycles + 42 < (262 - 243) * 228,
        "measured burst exceeds the latest-EC deadline"
    );
    // Single try: IN11 + SUB7 + CP7 + JR-taken12 =37 T before the burst.
    assert!(cycles + 37 <= 4096);
    assert!(cycles + 37 < (262 - 243) * 228);
    eprintln!(
        "SAT admission-to-last-register bounds: blocking={} try={} real Z80 T states",
        cycles + 42,
        cycles + 37
    );
}
