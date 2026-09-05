//! Execute assembled IRQ bridges and emitted victim instructions, not an IRQ
//! model. Both projects must be generated and assembled with the Docker toolchain:
//! IRQ_NROM_PROJECT=out/<smb> IRQ_UXROM_PROJECT=out/<cv1> cargo test \
//!   -p nes_to_sms --test generic_irq_spills -- --ignored --nocapture
//! Commercial ROM inputs and assembled projects stay local and ignored.

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

const SPILLS: [usize; 3] = [0xcb15, 0xcb18, 0xcb27];
const FLOOR: u16 = 0xde40;

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    ram: Box<[u8; 65536]>,
    labels: HashMap<String, (u8, u16)>,
    uxrom: bool,
    status: u8,
    pad1: u8,
    min_sp: u16,
    post_save: u16,
}

impl Machine {
    fn assembled(uxrom: bool) -> Self {
        let variable = if uxrom {
            "IRQ_UXROM_PROJECT"
        } else {
            "IRQ_NROM_PROJECT"
        };
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
            let mut words = line.split_whitespace();
            if let (Some(location), Some(name)) = (words.next(), words.next())
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
            labels,
            uxrom,
            status: 0x80,
            pad1: 0xff,
            min_sp: 0xffff,
            post_save: 0,
        };
        assert_eq!(m.labels.contains_key("data_prg_bank_0"), uxrom);
        m.ram[0xdffe] = 1;
        m.ram[0xcb14] = 1;
        m.ram[0xcb62] = if uxrom { 3 } else { 0 };
        m.ram[0xdfff] = m.labels[if uxrom {
            "data_prg_bank_3"
        } else {
            "data_prg_low"
        }]
        .0;
        m.ram[0xcb28] = 1;
        m.ram[0xca12] = 1; // skip presentation, not the actual IRQ/NMI bridge
        m.ram[0xcb02] = 0xf0;
        m.ram[0xcb03] = 0x65;
        m.ram[0xcb76] = 0;
        m.ram[0xcb77] = 0xd3;
        m.ram[0xd47d] = 0xc0;
        m.ram[0xd47e] = 0xd4;
        let entry = m.labels["irq_handler"].1 as usize;
        // Real first status read is after the reentrant saves on every exit.
        m.post_save = (entry..entry + 100)
            .find(|&p| m.rom[p..p + 2] == [0xdb, 0xbf])
            .unwrap() as u16
            + 2;
        m
    }

    fn cpu(&mut self, label: &str) -> Cpu {
        let (bank, pc) = self.labels[label];
        if pc >= 0x4000 {
            self.ram[0xdffe] = bank;
            self.ram[0xcb14] = bank;
        }
        let mut cpu = Cpu::new();
        cpu.pc = pc;
        cpu.sp = 0xdff4;
        cpu.a = 0x69;
        cpu.f = 0x95;
        cpu.b = 0x43;
        cpu.c = 0x87;
        cpu.d = 0x52;
        cpu.e = 0xa9;
        cpu.h = 0x23;
        cpu.l = 0x46;
        cpu.af_shadow = 0x1020;
        cpu.bc_shadow = 0x3040;
        cpu.de_shadow = 0x5060;
        cpu.hl_shadow = 0x7080;
        cpu.iff1 = true;
        cpu.iff2 = true;
        self.ram[cpu.sp as usize] = 7;
        cpu
    }

    fn step(&mut self, cpu: &mut Cpu) {
        assert_eq!(self.ram[0xcb1d], 0, "runtime trap at {:04x}", cpu.pc);
        assert!(!cpu.halted, "halt at {:04x}", cpu.pc);
        cpu.step(self)
            .unwrap_or_else(|e| panic!("{:04x}: {e:?}", cpu.pc));
        self.min_sp = self.min_sp.min(cpu.sp);
        assert!(cpu.sp >= FLOOR, "native stack collided at {:04x}", cpu.sp);
    }

    fn run_until(&mut self, cpu: &mut Cpu, stop: u16) {
        for _ in 0..200_000 {
            if cpu.pc == stop {
                return;
            }
            self.step(cpu);
        }
        panic!("did not reach {stop:04x}; PC={:04x}", cpu.pc);
    }

    fn spills(&self) -> [u8; 3] {
        SPILLS.map(|a| self.ram[a])
    }
    fn set_spills(&mut self, values: [u8; 3]) {
        for (a, value) in SPILLS.into_iter().zip(values) {
            self.ram[a] = value;
        }
    }

    fn interrupt(&mut self, cpu: &mut Cpu, mut hook: impl FnMut(&mut Self, &mut Cpu)) {
        assert!(cpu.iff1 && cpu.ei_pending == 0);
        let resume = (cpu.pc, cpu.sp, self.ram[0xdffe]);
        cpu.sp -= 2;
        self.write(cpu.sp, resume.0 as u8);
        self.write(cpu.sp + 1, (resume.0 >> 8) as u8);
        cpu.pc = 0x38;
        cpu.iff1 = false;
        cpu.iff2 = false;
        for _ in 0..200_000 {
            if (cpu.pc, cpu.sp, self.ram[0xdffe]) == resume {
                return;
            }
            hook(self, cpu);
            self.step(cpu);
        }
        panic!("IRQ did not resume {resume:?}; PC={:04x}", cpu.pc);
    }

    fn clobbering_irq(&mut self, cpu: &mut Cpu) {
        let before = self.spills();
        let before_registers = registers(cpu);
        let mapping = self.mapping();
        let enabled_on_return = self.ram[0xcb28] != 0;
        let mut injected = false;
        self.interrupt(cpu, |m, c| {
            if c.pc == m.post_save {
                // Deliberate adversarial scratch reuse after the real prologue;
                // NOT evidence that a particular game route reaches this write.
                m.set_spills(before.map(|v| v ^ 0xff));
                injected = true;
            }
        });
        assert!(injected);
        assert_eq!(self.spills(), before, "live spills changed across IRQ");
        assert_eq!(registers(cpu), before_registers);
        assert_eq!(self.mapping(), mapping);
        assert_eq!((cpu.iff1, cpu.iff2), (enabled_on_return, enabled_on_return));
        assert_eq!(cpu.ei_pending, 0);
    }

    fn mapping(&self) -> [u8; 5] {
        [0xcb14, 0xcb62, 0xdffc, 0xdffe, 0xdfff].map(|a| self.ram[a])
    }

    fn busy_lag(&mut self) {
        assert!(self.uxrom);
        self.ram[0xc01b] = 1;
        self.ram[0xc07f] = 1; // real lag body, skip shared audio
        self.ram[0xc0fe] = 0x1e;
        self.ram[0xc0ff] = 0xb0;
        self.ram[0xcb08] = 0xb0;
        self.ram[0xcb09] = 0x1e;
        self.ram[0xca11] = 1;
        self.ram[0xcb1a] = 1;
    }
}

impl Bus for Machine {
    fn read(&mut self, address: u16) -> u8 {
        match address {
            0..=0x3fff => self.rom[address as usize],
            0x4000..=0x7fff => {
                self.rom[self.ram[0xdffe] as usize * 0x4000 + address as usize - 0x4000]
            }
            0x8000..=0xbfff if self.ram[0xdffc] & 8 != 0 => self.ram[address as usize],
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
            self.ram[address as usize] = value;
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0xbf => self.status,
            0x7e => 0xe0,
            0xdc => self.pad1,
            _ => 0xff,
        }
    }
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT and IRQ_UXROM_PROJECT assembled with Docker"]
fn physical_irq_does_not_rewind_serial_reads_but_guest_strobe_does() {
    for uxrom in [false, true] {
        for raw_pad in [0xff, 0xf7, 0xef, 0xdf, 0xc0] {
            for index in 0..=9 {
                let mut m = Machine::assembled(uxrom);
                m.pad1 = raw_pad;
                m.ram[0xcb07] = index;
                let mut cpu = m.cpu("rt_controller_read");
                let registers_before = registers(&cpu);
                m.interrupt(&mut cpu, |_, _| {});
                assert_eq!(registers(&cpu), registers_before);
                assert_eq!(m.ram[0xcb07], index, "physical poll rewound serial input");

                // The disconnected second controller must not consume port1.
                cpu.a = 0x17;
                m.run_until(&mut cpu, 7);
                assert_eq!(cpu.a, 0);
                assert_eq!(m.ram[0xcb07], index);

                let mut cpu = m.cpu("rt_controller_strobe");
                let before = (cpu.a, cpu.f, cpu.d, cpu.e);
                m.run_until(&mut cpu, 7);
                assert_eq!(m.ram[0xcb07], 0);
                assert_eq!((cpu.a, cpu.f, cpu.d, cpu.e), before);
            }
        }
    }
}

#[test]
#[ignore = "requires IRQ_UXROM_PROJECT assembled with Docker"]
fn actual_cv1_controller_loop_retains_held_right_across_each_lag_irq_boundary() {
    for partial_reads in 0..8 {
        let mut m = Machine::assembled(true);
        m.busy_lag();
        m.pad1 = 0xf7; // physical SMS Right held, active-low
        m.ram[0xcb06] = 0x80;
        let mut cpu = m.cpu("L_C8CD");
        let loop_pc = m.labels["L_C8D8"].1;
        let stop = m.labels["rt_translated_rts"].1;
        let mut injected = false;
        for _ in 0..20_000 {
            // First translated RTS is C90C: the original C917 call has
            // completed controller1's eight reads and published held $F7.
            if cpu.pc == stop {
                break;
            }
            if partial_reads != 0
                && !injected
                && cpu.pc == loop_pc
                && m.ram[0xcb07] == partial_reads
            {
                let tick = m.ram[0xc01a];
                m.interrupt(&mut cpu, |_, _| {});
                assert_eq!(m.ram[0xc01a], tick, "lag NMI advanced game logic");
                assert_eq!(m.ram[0xcb06], 0x80);
                assert_eq!(m.ram[0xcb07], partial_reads);
                injected = true;
            }
            m.step(&mut cpu);
        }
        assert_eq!(cpu.pc, stop);
        assert_eq!(injected, partial_reads != 0);
        assert_eq!(m.ram[0xc0f7], 1, "held Right was lost during polling");
        assert_eq!(m.ram[0xcb07], 8);
    }
}

fn registers(c: &Cpu) -> [u16; 13] {
    [
        c.a as u16,
        c.f as u16,
        c.b as u16,
        c.c as u16,
        c.d as u16,
        c.e as u16,
        c.h as u16,
        c.l as u16,
        c.sp,
        c.af_shadow,
        c.bc_shadow,
        c.de_shadow,
        c.hl_shadow,
    ]
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT and IRQ_UXROM_PROJECT assembled with Docker"]
fn every_irq_exit_restores_all_spills_registers_and_mapping() {
    for uxrom in [false, true] {
        for (ready, status) in [(0, 0x80), (1, 0), (1, 0x80)] {
            for guard in [0, 1, 2] {
                for control in [0, 8] {
                    let mut m = Machine::assembled(uxrom);
                    let mut cpu = m.cpu("rt_banked_dispatch");
                    m.ram[0xcb28] = ready;
                    m.status = status;
                    m.ram[0xd47f] = guard;
                    m.ram[0xdffc] = control;
                    if !uxrom {
                        m.ram[0xdfff] = m.labels["data_prg_high"].0;
                    }
                    m.set_spills([0x37, 0x5a, 0xa5]);
                    m.clobbering_irq(&mut cpu);
                    assert_eq!(m.ram[0xd47f], guard);
                }
            }
        }
    }
}

#[test]
#[ignore = "requires IRQ_UXROM_PROJECT assembled with Docker"]
fn real_lag_body_and_nested_exits_preserve_each_depth() {
    for (ready, status) in [(0, 0x80), (1, 0), (1, 0x80)] {
        let mut m = Machine::assembled(true);
        m.busy_lag();
        m.set_spills([0x37, 0x5a, 0xa5]);
        let mut cpu = m.cpu("rt_banked_dispatch");
        let before = registers(&cpu);
        let mapping = m.mapping();
        let lag = m.labels["L_C0C0"].1;
        let mut nested = false;
        m.interrupt(&mut cpu, |m, c| {
            if c.pc == lag && !nested {
                m.step(c); // real LDA $FE
                m.step(c); // real LDX spills A into CB27
                assert_eq!(m.ram[0xcb27], 0x1e);
                assert_eq!(m.ram[0xca11], 2);
                m.ram[0xcb28] = ready;
                m.status = status;
                m.clobbering_irq(c);
                assert_eq!(m.ram[0xca11], 2);
                m.ram[0xcb28] = 1;
                m.status = 0x80;
                nested = true;
            }
        });
        assert!(nested);
        assert_eq!(m.spills(), [0x37, 0x5a, 0xa5]);
        assert_eq!(registers(&cpu), before);
        assert_eq!(m.mapping(), mapping);
        assert_eq!(m.ram[0xca11], 1);
    }
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT assembled with Docker"]
fn real_nrom_full_nmi_returns_with_the_same_guest_and_native_context() {
    let mut m = Machine::assembled(false);
    m.ram[0xcb08] = 0x80;
    m.ram[0xcb1a] = 1;
    m.ram[0xc774] = 1; // canonical DisableScreenFlag
    m.ram[0xc776] = 1; // pause gameplay, still execute the actual NMI body
    m.set_spills([0x37, 0x5a, 0xa5]);
    let mut cpu = m.cpu("rt_banked_dispatch");
    let mut reference = m.clone();
    let mut reference_cpu = cpu;
    reference.interrupt(&mut reference_cpu, |_, _| {});
    let mut entered_body = false;
    m.interrupt(&mut cpu, |m, c| {
        if c.pc == m.post_save {
            m.set_spills([0xc8, 0xa5, 0x5a]);
        }
        if c.pc == m.labels["translated_nmi"].1 {
            assert_eq!(m.ram[0xca11], 1);
            entered_body = true;
        }
    });
    assert!(entered_body);
    assert_eq!(m.spills(), [0x37, 0x5a, 0xa5]);
    assert_eq!(registers(&cpu), registers(&reference_cpu));
    assert_eq!(m.mapping(), reference.mapping());
    assert_eq!((m.ram[0xcb02], m.ram[0xca11]), (0xf0, 0));
    assert_eq!(&m.ram[0xc000..0xc800], &reference.ram[0xc000..0xc800]);
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT and IRQ_UXROM_PROJECT assembled with Docker"]
fn banked_dispatch_accumulator_survives_every_legal_mru_and_slow_boundary() {
    for uxrom in [false, true] {
        for mru in [false, true] {
            let mut m = Machine::assembled(uxrom);
            // First real fixed-bank dispatch entry: actual generated table,
            // not a target invented from a trap. Stop before its game body.
            let (bank, address) = m.labels["rt_dispatch_page_C0"];
            let offset = bank as usize * 0x4000 + (address as usize & 0x3fff);
            let record = &m.rom[offset..offset + 6];
            let target = u16::from_le_bytes([record[0], record[1]]);
            assert!((0xc000..0xc100).contains(&target));
            assert_eq!(record[2], 0xff);
            let destination = (record[3], u16::from_le_bytes([record[4], record[5]]));
            let mut cpu = m.cpu("rt_banked_dispatch");
            cpu.b = (target >> 8) as u8;
            cpu.c = target as u8;
            if mru {
                m.ram[0xca08] = target as u8;
                m.ram[0xca09] = (target >> 8) as u8;
                m.ram[0xca0a] = m.ram[0xcb62];
                m.ram[0xca0b] = destination.0;
                m.ram[0xca0c] = destination.1 as u8;
                m.ram[0xca0d] = (destination.1 >> 8) as u8;
                m.ram[0xca0e] = 1;
            }
            let mut boundaries = 0;
            for _ in 0..1000 {
                if (m.ram[0xdffe], cpu.pc) == destination {
                    break;
                }
                if cpu.iff1 && cpu.ei_pending == 0 {
                    let mut interrupted = m.clone();
                    let mut victim = cpu;
                    // Let the victim reload CB15 and transfer before comparing
                    // A: this makes the old ROM fail on an actual live value,
                    // rather than merely on an unused scratch-byte difference.
                    let spills = interrupted.spills();
                    let mapping = interrupted.mapping();
                    let before_registers = registers(&victim);
                    interrupted.interrupt(&mut victim, |m, c| {
                        if c.pc == m.post_save {
                            m.set_spills(spills.map(|v| v ^ 0xff));
                        }
                    });
                    assert_eq!(registers(&victim), before_registers);
                    assert_eq!(interrupted.mapping(), mapping);
                    interrupted.run_until(&mut victim, destination.1);
                    assert_eq!(victim.a, 0x69, "dispatch A at boundary {:04x}", cpu.pc);
                    assert_eq!(interrupted.ram[0xdffe], destination.0);
                    boundaries += 1;
                }
                m.step(&mut cpu);
            }
            assert_eq!((m.ram[0xdffe], cpu.pc), destination);
            assert_eq!(cpu.a, 0x69);
            assert!(boundaries >= 15, "insufficient dispatch coverage");
            eprintln!("uxrom={uxrom} mru={mru}: {boundaries} legal dispatch boundaries");
        }
    }
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT assembled with Docker"]
fn nrom_fixed_high_read_restores_mapping_at_every_emitted_boundary() {
    let mut m = Machine::assembled(false);
    let mut cpu = m.cpu("L_C0D8"); // canonical SMB CMP $C06B,Y sequence
    cpu.e = 1;
    // Stop immediately after the data read/map-back, before guest branching.
    let start = cpu.pc;
    let mut stop = None;
    for _ in 0..100 {
        let pc = cpu.pc;
        if [m.read(pc), m.read(pc + 1), m.read(pc + 2)] == [0x32, 0xff, 0xff]
            && cpu.a == m.labels["data_prg_low"].0
        {
            stop = Some(pc + 3);
            break;
        }
        m.step(&mut cpu);
    }
    let stop = stop.expect("actual fixed-high read must map low back");
    let mut m = Machine::assembled(false);
    let mut cpu = m.cpu("L_C0D8");
    cpu.e = 1;
    assert_eq!(cpu.pc, start);
    let mut mapped_boundaries = 0;
    while cpu.pc != stop {
        if cpu.iff1 && cpu.ei_pending == 0 {
            let mut interrupted = m.clone();
            let mut victim = cpu;
            interrupted.interrupt(&mut victim, |_, _| {});
            interrupted.run_until(&mut victim, stop);
            let mut uninterrupted = m.clone();
            let mut reference = cpu;
            uninterrupted.run_until(&mut reference, stop);
            assert_eq!(registers(&victim), registers(&reference));
            assert_eq!(interrupted.mapping(), uninterrupted.mapping());
            if m.ram[0xdfff] == m.labels["data_prg_high"].0 {
                mapped_boundaries += 1;
            }
        }
        m.step(&mut cpu);
    }
    assert!(mapped_boundaries >= 8);
    eprintln!("fixed-high mapped boundaries: {mapped_boundaries}");
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT assembled with Docker"]
fn nrom_emitted_ppuctrl_and_ldy_keep_live_accumulator_at_every_legal_boundary() {
    for spill in [0x18, 0x27] {
        let mut m = Machine::assembled(false);
        m.ram[0xc778] = 0x69;
        m.ram[0xc779] = 0x51;
        m.ram[0xc774] = 0xa9;
        let mut cpu = m.cpu("L_8082");
        // Derive the live interval from actual emitted store/reload opcodes,
        // including CTRL's pre-DI and post-EI windows. No handwritten victim.
        let mut found = false;
        for _ in 0..1000 {
            let p = cpu.pc;
            if [m.read(p), m.read(p + 1), m.read(p + 2)] == [0x32, spill, 0xcb] {
                found = true;
                break;
            }
            m.step(&mut cpu);
        }
        assert!(found);
        let mut reference_machine = m.clone();
        let mut reference = cpu;
        let mut stop = None;
        for _ in 0..1000 {
            let p = reference.pc;
            if reference.iff1
                && reference.ei_pending == 0
                && [
                    reference_machine.read(p),
                    reference_machine.read(p + 1),
                    reference_machine.read(p + 2),
                ] == [0x3a, spill, 0xcb]
            {
                reference_machine.step(&mut reference);
                stop = Some(reference.pc);
                break;
            }
            reference_machine.step(&mut reference);
        }
        let stop = stop.expect("enabled final accumulator reload");
        let mut boundaries = 0;
        while cpu.pc != stop {
            if cpu.iff1 && cpu.ei_pending == 0 {
                let mut interrupted = m.clone();
                let mut victim = cpu;
                let spills = m.spills();
                interrupted.interrupt(&mut victim, |m, c| {
                    if c.pc == m.post_save {
                        m.set_spills(spills.map(|v| v ^ 0xff));
                    }
                });
                interrupted.run_until(&mut victim, stop);
                assert_eq!(
                    registers(&victim),
                    registers(&reference),
                    "emitted CB{spill:02x} boundary {:04x}",
                    cpu.pc
                );
                assert_eq!(interrupted.ram[0xcb03], reference_machine.ram[0xcb03]);
                boundaries += 1;
            }
            m.step(&mut cpu);
        }
        assert!(boundaries >= 4);
        eprintln!("NROM CB{spill:02x}: {boundaries} legal emitted boundaries");
    }
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT and IRQ_UXROM_PROJECT assembled with Docker"]
fn first_status_read_latency_is_bounded_by_actual_prologue_opcodes() {
    for uxrom in [false, true] {
        let mut m = Machine::assembled(uxrom);
        let mut cpu = m.cpu("irq_handler");
        cpu.pc = 0x38;
        cpu.iff1 = false;
        cpu.iff2 = false;
        let mut tstates = 13; // real IM1 hardware acceptance, before vector JP
        while cpu.pc != m.post_save {
            let op = m.read(cpu.pc);
            // Exact timings only for the tiny straight-line actual prologue;
            // Cpu.cycles remains approximate and is deliberately not used.
            tstates += match op {
                0xc3 => 10,                     // JP nn
                0xe5 | 0xf5 | 0xc5 => 11,       // PUSH
                0x3a | 0x32 => 13,              // LD A,(nn) / LD (nn),A
                0x3e => 7,                      // LD A,n
                0x47 | 0x4f | 0x7a | 0x7b => 4, // LD r,r
                0xdb => {
                    assert_eq!(m.read(cpu.pc + 1), 0xbf);
                    11
                }
                _ => panic!("unbudgeted prologue opcode {op:02x} at {:04x}", cpu.pc),
            };
            m.step(&mut cpu);
        }
        assert_eq!(tstates, if uxrom { 190 } else { 234 });
        eprintln!("uxrom={uxrom}: IM1 through status read {tstates} exact T-states");
    }
}

#[test]
#[ignore = "requires IRQ_NROM_PROJECT and IRQ_UXROM_PROJECT assembled with Docker"]
fn save_block_has_explicit_four_byte_budget_and_respects_de40() {
    for uxrom in [false, true] {
        let mut m = Machine::assembled(uxrom);
        let mut cpu = m.cpu("rt_banked_dispatch");
        // IM1 PC word + original HL/AF/BC + two scratch words; NROM adds bank.
        let stack_bytes = if uxrom { 12 } else { 14 };
        cpu.sp = FLOOR + stack_bytes;
        m.ram[0xcb28] = 0; // leaf not-ready path reaches exactly the save floor
        m.ram[FLOOR as usize - 1] = 0x96;
        m.clobbering_irq(&mut cpu);
        assert_eq!(m.min_sp, FLOOR);
        assert_eq!(m.ram[FLOOR as usize - 1], 0x96);
        assert_eq!(cpu.sp, FLOOR + stack_bytes);
    }
}
