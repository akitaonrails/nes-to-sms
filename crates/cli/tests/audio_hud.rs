//! Actual assembled audio/HUD boundary checks. No Rust audio implementation.
//! CV1_AUDIO_PROJECT=<candidate> CV1_AUDIO_BASELINE=<pre-poll-project>
//! cargo test -p nes_to_sms --test audio_hud -- --ignored --nocapture

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    labels: HashMap<String, u16>,
    ram: Box<[u8; 65536]>,
    sram: Box<[u8; 16384]>,
    psg: Vec<u8>,
    regs: [u8; 16],
    latch: Option<u8>,
    status: u8,
    counter: u8,
    clock: Option<u64>,
    next_vblank: u64,
    tstates: u64,
    status_reads: usize,
    polls: Vec<u64>,
    splits: Vec<u64>,
    min_sp: u16,
}

impl Machine {
    fn assembled(variable: &str) -> Self {
        let path = PathBuf::from(std::env::var(variable).expect(variable));
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
                && let Some(("00", address)) = location.split_once(':')
                && let Ok(address) = u16::from_str_radix(address, 16)
            {
                labels.insert(name.to_owned(), address);
            }
        }
        let mut m = Self {
            rom: std::fs::read(path.join("sms.sms")).unwrap(),
            labels,
            ram: Box::new([0; 65536]),
            sram: Box::new([0; 16384]),
            psg: vec![],
            regs: [0; 16],
            latch: None,
            status: 0,
            counter: 0,
            clock: None,
            next_vblank: 224 * 228,
            tstates: 0,
            status_reads: 0,
            polls: vec![],
            splits: vec![],
            min_sp: 0xffff,
        };
        m.ram[0xdffe] = 1;
        m.ram[0xdfff] = 2;
        m.ram[0xcb14] = 1;
        m.ram[0xcb62] = 3;
        m.ram[0xcb03] = 0x65;
        m.ram[0xcb7e] = 1;
        m.ram[0xdd80..0xde40].fill(0xa5);
        m.ram[0xc813..0xc81c].copy_from_slice(&[7, 0, 0, 0x57, 38, 4, 9, 9, 0]);
        m
    }

    fn call(&mut self, label: &str) -> Cpu {
        self.call_at(label, 0xde44)
    }

    fn call_at(&mut self, label: &str, entry_sp: u16) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.pc = self.labels[label];
        cpu.sp = entry_sp;
        self.ram[usize::from(entry_sp)] = 7;
        self.ram[usize::from(entry_sp) + 1] = 0;
        let mapping = [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f, 0xca11].map(|a| self.ram[a]);
        let shadows = [0xcb00, 0xcb01, 0xcb02, 0xcb03, 0xcb15, 0xcb18, 0xcb27].map(|a| self.ram[a]);
        for _ in 0..100_000 {
            if cpu.pc == 7 {
                assert_eq!(cpu.sp, entry_sp + 2);
                assert_eq!(
                    [0xdffc, 0xdffe, 0xdfff, 0xcb14, 0xcb62, 0xd47f, 0xca11].map(|a| self.ram[a]),
                    mapping
                );
                assert_eq!(
                    [0xcb00, 0xcb01, 0xcb02, 0xcb03, 0xcb15, 0xcb18, 0xcb27].map(|a| self.ram[a]),
                    shadows
                );
                assert!(self.ram[0xdd80..0xde40].iter().all(|b| *b == 0xa5));
                assert!(self.latch.is_none());
                return cpu;
            }
            assert!(
                !cpu.iff1 && !cpu.iff2,
                "audio must never enable nested writers"
            );
            assert!(!cpu.halted);
            self.step(&mut cpu);
            self.min_sp = self.min_sp.min(cpu.sp);
            assert!(cpu.sp >= 0xde40, "actual metadata floor, not old DD80");
        }
        panic!("{label} did not return");
    }

    // Exact opcode costs for code executed by the project's CPU. This models
    // no instruction semantics; unknown opcodes fail rather than guess a cost.
    fn step(&mut self, cpu: &mut Cpu) {
        let pc = cpu.pc;
        if self.labels.get("rt_cv1_hud_audio_poll") == Some(&pc) {
            self.polls.push(self.tstates);
        }
        let op = self.read(pc);
        let second = self.read(pc.wrapping_add(1));
        cpu.step(self).unwrap_or_else(|e| panic!("{pc:04x}: {e:?}"));
        let t = match op {
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
                _ => panic!("unbudgeted ED{second:02x}"),
            },
            _ => panic!("unbudgeted {op:02x}"),
        };
        self.tstates += t;
        if let Some(clock) = &mut self.clock {
            *clock += t;
            if *clock >= self.next_vblank {
                self.status |= 0x80;
                self.next_vblank += 262 * 228;
            }
        }
    }
}

impl Bus for Machine {
    fn read(&mut self, address: u16) -> u8 {
        match address {
            0..=0x3fff => self.rom[address as usize],
            0x4000..=0x7fff => {
                self.rom[usize::from(self.ram[0xdffe]) * 0x4000 + usize::from(address) - 0x4000]
            }
            0x8000..=0xbfff if self.ram[0xdffc] & 8 != 0 => {
                self.sram[usize::from(address) - 0x8000]
            }
            0x8000..=0xbfff => {
                self.rom[usize::from(self.ram[0xdfff]) * 0x4000 + usize::from(address) - 0x8000]
            }
            _ => self.ram[0xc000 + (usize::from(address) & 0x1fff)],
        }
    }
    fn write(&mut self, address: u16, value: u8) {
        if address >= 0xc000 {
            self.ram[0xc000 + (usize::from(address) & 0x1fff)] = value;
        } else if address >= 0x8000 && self.ram[0xdffc] & 8 != 0 {
            self.sram[usize::from(address) - 0x8000] = value;
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0xbf => {
                self.status_reads += 1;
                let status = self.status;
                self.status = 0;
                status
            }
            0x7e => self.clock.map_or(self.counter, |clock| {
                let line = (clock / 228) % 262;
                if line <= 234 {
                    line as u8
                } else {
                    (line - 6) as u8
                }
            }),
            _ => 0xff,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        match port {
            0x7f => self.psg.push(value),
            0xbf => {
                if let Some(low) = self.latch.take() {
                    assert_eq!(
                        value >> 6,
                        2,
                        "audio poll opened a non-register VDP command"
                    );
                    self.regs[usize::from(value & 15)] = low;
                    if value == 0x88 && low == 0x57 {
                        self.splits.push(self.clock.unwrap_or(self.tstates));
                    }
                } else {
                    self.latch = Some(value);
                }
            }
            _ => panic!("unexpected audio/HUD port{port:02x}"),
        }
    }
}

#[test]
#[ignore = "requires baseline and candidate Docker-assembled projects"]
fn staged_polls_preserve_audio_ram_and_psg_order_at_guarded_deep_entries() {
    let old = Machine::assembled("CV1_AUDIO_BASELINE");
    let new = Machine::assembled("CV1_AUDIO_PROJECT");
    let mut random = 0x7341_ab59u32;
    for vector in 0..128 {
        let mut state = [0u8; 50];
        for byte in &mut state {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *byte = random as u8;
        }
        for (status, counter) in [(0, 0), (0, 38), (0x80, 0xe0)] {
            for guard in 0..=2 {
                for mapper in [0, 8] {
                    for depth in [1, 2] {
                        let mut a = old.clone();
                        let mut b = new.clone();
                        for m in [&mut a, &mut b] {
                            m.ram[0xcb30..0xcb62].copy_from_slice(&state);
                            m.ram[0xd47f] = guard;
                            m.ram[0xdffc] = mapper;
                            m.ram[0xca11] = depth;
                            m.status = status;
                            m.counter = counter;
                        }
                        a.call("apu_frame_tick");
                        b.call("apu_frame_tick");
                        assert_eq!(
                            &a.ram[0xcb30..0xcb62],
                            &b.ram[0xcb30..0xcb62],
                            "audio RAM vector{vector}"
                        );
                        assert_eq!(a.psg, b.psg, "PSG write order vector{vector}");
                        assert_eq!(b.ram[0xd474], status & 0x80);
                        assert_eq!(b.ram[0xc819], 9 + u8::from(status & 0x80 != 0));
                        assert_eq!(
                            b.polls.len(),
                            14,
                            "all quarter/half/sweep/output boundaries"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires baseline and candidate Docker-assembled projects"]
fn real_audio_path_crossing_hint_is_red_before_polls_and_green_after() {
    for (variable, should_be_late) in [("CV1_AUDIO_BASELINE", true), ("CV1_AUDIO_PROJECT", false)] {
        let mut m = Machine::assembled(variable);
        // All channels enabled, active lengths, small periods, changed PSG
        // caches: real audio work, no synthetic delay or patched instruction.
        m.ram[0xcb30..0xcb62].fill(0);
        m.ram[0xcb45] = 15;
        m.ram[0xcb4e..0xcb52].fill(10);
        m.ram[0xcb55] = 10;
        for index in [0xcb32, 0xcb36, 0xcb3a] {
            m.ram[index] = 40;
        }
        m.ram[0xcb56..0xcb5d].fill(0xff);
        m.ram[0xcb5e..0xcb62].fill(0xff);
        m.clock = Some(37 * 228);
        m.call("apu_frame_tick");
        m.call("rt_cv1_hud_tail_ack");
        let line = m.splits.first().expect("committed HUD never serviced") / 228;
        assert_eq!(line >= 48, should_be_late, "{variable} lastR8line{line}");
        if !should_be_late {
            assert!(line < 48);
        }
        eprintln!("{variable} real audio crossing line37: postR8={line}");
    }
}

#[test]
#[ignore = "requires candidate Docker-assembled project"]
fn audio_poll_rearms_physical_epoch_and_accumulates_vint_without_reentry() {
    let base = Machine::assembled("CV1_AUDIO_PROJECT");
    let mut maximum = 0;
    for status in [0, 0x80] {
        for counter in 0..=255 {
            let mut m = base.clone();
            m.ram[0xc819] = 255;
            m.ram[0xc81a] = 255;
            m.counter = counter;
            m.status = status;
            let before = m.ram[0xcb30..0xcb62].to_vec();
            m.call("rt_cv1_hud_audio_poll");
            maximum = maximum.max(m.tstates);
            assert_eq!(&m.ram[0xcb30..0xcb62], before);
            assert_eq!(m.ram[0xd474], status);
            assert_eq!(m.ram[0xc819], if status == 0 { 255 } else { 0 });
            m.status = 0;
            m.counter = 0;
            m.call("rt_cv1_hud_audio_poll");
            assert_eq!(m.ram[0xd474], status, "later HINT poll lost prior VINT");
        }
    }
    eprintln!("audio poll maximum tested branch={maximum}T, entry-through-RET");
    assert_eq!(maximum, 566, "exhaustive status/VC branch maximum changed");
    let mut hint = base;
    hint.counter = 38;
    hint.call("rt_cv1_hud_audio_poll");
    let last_r8 = hint.splits[0] + 11; // Include the final OUT, not its start.
    eprintln!("audio poll entry through last postR8 OUT={last_r8}T");
    assert_eq!(last_r8, 154);
    // Worst prior poll + unchanged stage/glue + new poll through LAST OUT.
    // This deliberately combines incompatible longest branches, not just
    // the observed entry-gap maximum from the phase matrix below.
    assert!(566 + 778 + 50 + last_r8 < 9 * 228);
}

#[test]
#[ignore = "requires candidate Docker-assembled project"]
fn staged_audio_keeps_each_serviced_split_before_playfield_across_frame_wrap() {
    let base = Machine::assembled("CV1_AUDIO_PROJECT");
    let mut maximum_gap = 0;
    for line in 0..262 {
        for phase in [0, 227] {
            let mut m = base.clone();
            m.ram[0xcb30..0xcb62].fill(0);
            m.ram[0xcb45] = 15;
            m.ram[0xcb4e..0xcb52].fill(20);
            m.ram[0xcb55] = 20;
            for a in [0xcb30, 0xcb34, 0xcb3c] {
                m.ram[a] = 0x2f;
            }
            for a in [0xcb31, 0xcb35] {
                m.ram[a] = 0x8f;
            }
            for a in [0xcb32, 0xcb36, 0xcb3a] {
                m.ram[a] = 0xff;
                m.ram[a + 1] = 7;
            }
            m.ram[0xcb56..0xcb5d].fill(0xff);
            m.ram[0xcb5e..0xcb62].fill(0xff);
            if (38..224).contains(&line) {
                m.ram[0xc813] = 11;
                m.ram[0xc81b] = 9;
            }
            m.clock = Some(line * 228 + phase);
            if line >= 224 {
                m.next_vblank += 262 * 228;
            }
            m.call("apu_frame_tick");
            // This PRE-EXISTING helper preserves AF/BC and has an8-byte
            // VINT stack contract. The new audio path above stays DE44→DE40.
            m.call_at("rt_cv1_hud_tail_ack", 0xde48);
            for time in &m.splits {
                let actual_line = (time / 228) % 262;
                assert!(
                    (38..48).contains(&actual_line),
                    "entryline{line}/{phase}: split{actual_line}"
                );
            }
            for pair in m.polls.windows(2) {
                maximum_gap = maximum_gap.max(pair[1] - pair[0]);
            }
        }
    }
    // Independent unchanged chunk maxima: sweep778T, largest poll566T,
    // between-chunk tail/glue<=50T. This pessimistically sums unlike branches.
    assert!(maximum_gap <= 778 + 566 + 50);
    assert!(maximum_gap < 9 * 228);
    eprintln!("actual executed audio-poll entry gap max={maximum_gap}T across524 frame phases");
}

#[test]
#[ignore = "requires candidate Docker-assembled project"]
fn actual_irq_tail_keeps_overrun_status_and_stack_floor_after_audio_polls() {
    let base = Machine::assembled("CV1_AUDIO_PROJECT");
    for pending in [0, 0x80] {
        for guard in 0..=2 {
            for mapper in [0, 8] {
                for depth in [1, 2] {
                    let mut m = base.clone();
                    m.status = pending;
                    m.counter = 0xe0;
                    m.ram[0xd474] = 0x80; // Original classification is DEAD, not a new overrun.
                    m.ram[0xcb29] = 5;
                    m.ram[0xdffc] = mapper;
                    m.ram[0xd47f] = guard;
                    m.ram[0xca11] = depth;
                    m.ram[0xcb00] = 0x52;
                    m.ram[0xcb01] = 0xa9;
                    let mut cpu = Cpu::new();
                    cpu.pc = m.labels["_irq_skip_translated_nmi"];
                    cpu.sp = 0xde46;
                    // IRQ's five saved words plus its interrupted continuation.
                    let words = [0x2700u16, 0x1518, 0x3456, 0x3bc5, 0x789a, 7];
                    for (i, word) in words.into_iter().enumerate() {
                        m.ram[0xde46 + 2 * i..0xde48 + 2 * i].copy_from_slice(&word.to_le_bytes());
                    }
                    let mut enabled = 0;
                    for _ in 0..100_000 {
                        if cpu.pc == 7 {
                            break;
                        }
                        if m.read(cpu.pc) == 0xfb {
                            enabled += 1;
                        }
                        m.step(&mut cpu);
                        m.min_sp = m.min_sp.min(cpu.sp);
                        assert!(cpu.sp >= 0xde40);
                    }
                    assert_eq!(cpu.pc, 7);
                    assert_eq!(cpu.sp, 0xde52);
                    assert_eq!(enabled, 1, "only final IRQ exit may enable interrupts");
                    assert_eq!(
                        (cpu.a, cpu.f, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l),
                        (0x3b, 0xc5, 0x34, 0x56, 0x52, 0xa9, 0x78, 0x9a)
                    );
                    assert_eq!(m.ram[0xcb29], if pending == 0 { 4 } else { 60 });
                    assert_eq!(
                        [0xcb15, 0xcb18, 0xcb27].map(|a| m.ram[a]),
                        [0x15, 0x18, 0x27]
                    );
                    assert_eq!(m.ram[0xd474], pending);
                    assert_eq!(m.ram[0xdffc], mapper);
                    assert_eq!(m.ram[0xd47f], guard);
                    assert_eq!(m.ram[0xca11], depth);
                    assert!(m.ram[0xdd80..0xde40].iter().all(|b| *b == 0xa5));
                }
            }
        }
    }
}
