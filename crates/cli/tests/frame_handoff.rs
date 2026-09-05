//! Execute the assembled runtime, not a Rust model of its handoff protocol.
//! Build CV1 with the Docker toolchain, then run explicitly:
//! CV1_HANDOFF_PROJECT=out/<candidate> cargo test -p nes_to_sms \
//!   --test frame_handoff -- --ignored
//! The commercial input/assembled project remains local and gitignored.

use std::{collections::HashMap, path::PathBuf};
use z80_emu::{Bus, Cpu};

const READY: usize = 0xc820;
const PACKET: usize = 0xc821;
const PUBLISH: usize = 0xc83c;
const CONSUME: usize = 0xc83d;
const PENDING: usize = 0xc810;
const FIELDS: [usize; 13] = [
    0xcb08, 0xcb09, 0xcb0c, 0xcb0d, 0xcb20, 0xcb21, 0xcb22, 0xcb23, 0xcb24, 0xcb2d, 0xcb7f, 0xcb78,
    0xca18,
];

#[derive(Clone)]
struct Machine {
    rom: Vec<u8>,
    ram: Box<[u8; 65536]>,
    sram: Box<[u8; 16384]>,
    labels: HashMap<String, (u8, u16)>,
    ports: Vec<(u8, u8)>,
    vdp_status: u8,
    vcounters: Vec<u8>,
    vcounter_reads: usize,
    raw_writes: Vec<(u16, u8, usize)>,
}

impl Machine {
    fn assembled() -> Self {
        let dir = PathBuf::from(
            std::env::var("CV1_HANDOFF_PROJECT")
                .expect("set CV1_HANDOFF_PROJECT to a Docker-assembled candidate"),
        );
        let dir = if dir.is_absolute() {
            dir
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(dir)
        };
        let symbols = std::fs::read_to_string(dir.join("sms.sym")).unwrap();
        let mut labels = HashMap::new();
        for line in symbols.lines() {
            let mut words = line.split_whitespace();
            let (Some(location), Some(name)) = (words.next(), words.next()) else {
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
        assert!(
            labels.contains_key("rt_cv1_scroll_publish"),
            "candidate lacks the handoff"
        );
        let mut result = Self {
            rom: std::fs::read(dir.join("sms.sms")).unwrap(),
            ram: Box::new([0; 65536]),
            sram: Box::new([0; 16384]),
            labels,
            ports: Vec::new(),
            vdp_status: 0x80,
            vcounters: Vec::new(),
            vcounter_reads: 0,
            raw_writes: Vec::new(),
        };
        result.ram[0xdffd] = 0;
        result.ram[0xdffe] = 1;
        result.ram[0xdfff] = result.labels["data_prg_bank_3"].0;
        result.ram[0xcb62] = 3;
        result.ram[0xcb14] = 1;
        result
    }

    fn cpu(&mut self, label: &str, iff: bool) -> Cpu {
        let (bank, pc) = self.labels[label];
        if pc >= 0x4000 {
            self.ram[0xdffe] = bank;
            self.ram[0xcb14] = bank;
        }
        let mut cpu = Cpu::new();
        cpu.pc = pc;
        cpu.sp = 0xdff4;
        self.ram[0xdff4] = 7; // native return sentinel (never executed)
        cpu.a = 0x69;
        cpu.d = 0x52;
        cpu.e = 0xa9;
        cpu.iff1 = iff;
        cpu.iff2 = iff;
        cpu
    }

    fn run_until(&mut self, cpu: &mut Cpu, stop: u16) {
        for _ in 0..100_000 {
            if cpu.pc == stop {
                return;
            }
            assert!(
                !cpu.halted,
                "halt at {:04x}, trap={:02x}",
                cpu.pc, self.ram[0xcb1d]
            );
            cpu.step(self)
                .unwrap_or_else(|e| panic!("{:04x}: {e:?}", cpu.pc));
        }
        panic!("did not reach {stop:04x}; PC={:04x}", cpu.pc);
    }

    fn call(&mut self, label: &str, iff: bool) -> Cpu {
        let mut cpu = self.cpu(label, iff);
        self.run_until(&mut cpu, 7);
        cpu
    }

    fn seed_ppu(&mut self, busy: u8, value: u8) {
        self.ram[0xc01b] = busy;
        self.ram[0xc0fd] = 0x39;
        self.ram[0xc0fc] = 0x04;
        self.ram[0xc0ff] = value;
        self.ram[0xcb03] = 0x7d;
        self.ram[0xcb05] = 1;
        self.ram[0xcb09] = 0x1e;
        self.ram[0xcb0b] = 1;
        self.ram[0xcb0e] = 1;
        self.ram[0xcb12] = 2;
        self.ram[0xcb20] = 7;
        self.ram[0xca18] = 0x40;
    }

    fn seed_busy_lag_irq(&mut self) {
        self.seed_ppu(1, 0xb0);
        self.ram[0xcb28] = 1;
        self.ram[0xcb08] = 0xb0;
        self.ram[0xca11] = 1;
        self.ram[0xcb1a] = 1;
        self.ram[0xc07f] = 1; // execute the real lag body, skip shared audio
        self.ram[0xc0fe] = 0x1e;
        self.ram[0xcb02] = 0xf0;
        self.ram[0xcb76] = 0;
        self.ram[0xcb77] = 0xd3;
        self.ram[0xd47d] = 0xc0;
        self.ram[0xd47e] = 0xd4;
    }

    fn interrupt(&mut self, cpu: &mut Cpu) {
        assert!(cpu.iff1 && cpu.ei_pending == 0);
        let resume = cpu.pc;
        let resume_sp = cpu.sp;
        let resume_bank = self.ram[0xdffe];
        // Hardware IM1 entry; execute the ROM's vector and complete handler.
        cpu.sp = cpu.sp.wrapping_sub(2);
        self.write(cpu.sp, resume as u8);
        self.write(cpu.sp + 1, (resume >> 8) as u8);
        cpu.iff1 = false;
        cpu.iff2 = false;
        cpu.pc = 0x38;
        for _ in 0..100_000 {
            if cpu.pc == resume && cpu.sp == resume_sp && self.ram[0xdffe] == resume_bank {
                return;
            }
            assert!(!cpu.halted, "IRQ halted at {:04x}", cpu.pc);
            cpu.step(self).unwrap();
        }
        panic!("IRQ did not return to {resume_bank:02x}:{resume:04x}");
    }
}

impl Bus for Machine {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3fff => self.rom[addr as usize],
            0x4000..=0x7fff => {
                self.rom[self.ram[0xdffe] as usize * 0x4000 + addr as usize - 0x4000]
            }
            0x8000..=0xbfff if self.ram[0xdffc] & 8 != 0 => self.sram[addr as usize - 0x8000],
            0x8000..=0xbfff => {
                self.rom[self.ram[0xdfff] as usize * 0x4000 + addr as usize - 0x8000]
            }
            _ => self.ram[0xc000 + (addr as usize & 0x1fff)],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        if addr >= 0xc000 {
            let physical = 0xc000 + (addr as usize & 0x1fff);
            if (0xcb80..=0xcbff).contains(&physical) {
                self.raw_writes
                    .push((physical as u16, value, self.ports.len()));
            }
            self.ram[physical] = value;
        } else if addr >= 0x8000 && self.ram[0xdffc] & 8 != 0 {
            self.raw_writes.push((addr, value, self.ports.len()));
            self.sram[addr as usize - 0x8000] = value;
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0xbf => self.vdp_status,
            0x7e => {
                let value = self
                    .vcounters
                    .get(self.vcounter_reads)
                    .copied()
                    .unwrap_or(0xe0);
                self.vcounter_reads += 1;
                value
            }
            _ => 0xff,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.ports.push((port, value));
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn replacement_matches_actual_translated_scroll_and_preserves_mapper_iff() {
    for iff in [false, true] {
        for busy in [0, 1] {
            for control in [0x00, 0x81, 0xb0, 0xff] {
                for sram_control in [0, 8] {
                    let mut original = Machine::assembled();
                    original.seed_ppu(busy, control);
                    original.ram[0xdffc] = sram_control;
                    let mut replacement = original.clone();
                    let mut cpu = original.cpu("L_C11F", iff);
                    let original_bank = original.ram[0xdffe];
                    replacement.ram[0xdffe] = original_bank;
                    replacement.ram[0xcb14] = original_bank;
                    let stop = original.labels["rt_translated_rts"].1;
                    original.run_until(&mut cpu, stop);
                    let hook = replacement.call("rt_cv1_scroll_publish", iff);
                    assert_eq!((hook.a, hook.d, hook.e), (cpu.a, cpu.d, cpu.e));
                    assert_eq!((hook.iff1, hook.iff2), (cpu.iff1, cpu.iff2));
                    for address in [
                        0xcb03, 0xcb05, 0xcb0b, 0xcb0e, 0xcb12, 0xcb62, 0xcb14, 0xd47f, 0xdffc,
                        0xdffe, 0xdfff, 0xca39,
                    ] {
                        assert_eq!(
                            replacement.ram[address], original.ram[address],
                            "field {address:04x}, busy={busy}, ctrl={control:02x}"
                        );
                    }
                    for (index, address) in FIELDS.into_iter().enumerate() {
                        let actual = if busy == 0 {
                            replacement.ram[PACKET + index]
                        } else {
                            replacement.ram[address]
                        };
                        assert_eq!(actual, original.ram[address], "field {address:04x}");
                    }
                    assert_eq!(replacement.ports, original.ports);
                    assert!(
                        replacement.ports.is_empty(),
                        "producer CTRL must defer physical sprite registers"
                    );
                    assert_eq!(replacement.ram[READY], u8::from(busy == 0));
                    assert_eq!(replacement.ram[PUBLISH], u8::from(busy == 0));
                }
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn lag_does_not_republish_and_consumption_preserves_newer_intent() {
    let mut m = Machine::assembled();
    m.seed_ppu(0, 0xb0);
    m.call("rt_cv1_scroll_publish", true);
    let packet = m.ram[PACKET..PACKET + 13].to_vec();
    m.seed_ppu(1, 0x81);
    m.ram[0xc0fd] = 0xe8;
    m.call("rt_cv1_scroll_publish", true);
    assert_eq!(&m.ram[PACKET..PACKET + 13], packet);
    assert_eq!(m.ram[PUBLISH], 1);
    m.ram[0xcb7f] = 1;
    m.ram[0xcb78] = 1;
    m.ram[0xca18] = 250;
    let live: Vec<_> = FIELDS.iter().map(|a| m.ram[*a]).collect();
    m.call("rt_cv1_frame_begin", false);
    assert_eq!(m.ram[READY], 0);
    assert_eq!(m.ram[CONSUME], 1);
    assert_eq!(FIELDS.iter().map(|a| m.ram[*a]).collect::<Vec<_>>(), packet);
    m.ram[0xcb2d] = 0; // consumer completed its older reg1 write
    m.ram[0xcb7f] = 0;
    m.ram[0xcb78] = 0;
    m.ram[0xca13] = 0x10; // committed state must not be rolled back
    m.ram[0xcb2a] = 23;
    m.call("rt_cv1_frame_end", false);
    for (i, address) in FIELDS.into_iter().enumerate() {
        assert_eq!(
            m.ram[address],
            if i == 12 { 255 } else { live[i] },
            "field {address:04x}"
        );
    }
    assert_eq!((m.ram[0xca13], m.ram[0xcb2a]), (0x10, 23));
    // No new READY packet: actual IRQ must not consume/present it again.
    m.ram[0xcb28] = 1;
    m.ram[0xca11] = 1;
    m.ram[0xcb1a] = 1;
    m.ram[0xc01b] = 0;
    m.call("irq_handler", false);
    assert_eq!(m.ram[CONSUME], 1);
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn suppressed_nested_prologue_and_epilogue_preserve_ppu_phase() {
    // Before DMA, after F87D/before C11F, and busy-clear/RTI each have busy0
    // but differing synthetic PPU phase. Exercise the real IRQ for all phases.
    for phase in [0, 1, 2] {
        for split in [0, 3, 7] {
            let mut m = Machine::assembled();
            m.ram[0xcb28] = 1;
            m.ram[0xcb08] = 0xb0;
            m.ram[0xca11] = 1;
            m.ram[0xcb1a] = 1;
            m.ram[0xc01b] = 0;
            m.ram[0xcb12] = phase;
            m.ram[0xcb20] = split;
            let cpu = m.call("irq_handler", false);
            assert_eq!((m.ram[0xcb12], m.ram[0xcb20]), (phase, split));
            assert_eq!(m.ram[0xcb04], 1, "physical IRQ obligation");
            assert_eq!(m.ram[0xcb05], 1, "physical VBlank pending");
            assert_eq!((m.ram[0xca11], m.ram[READY], m.ram[CONSUME]), (1, 0, 0));
            assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
        }
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn actual_irq_consumes_ready_once_and_delivers_busy_lag_body() {
    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[READY] = 1;
    // Empty rendering packet: exercise the actual begin/present/end path
    // without relying on initialized game graphics or fixtures of its pixels.
    m.call("irq_handler", false);
    m.call("irq_handler", false);
    assert_eq!((m.ram[READY], m.ram[CONSUME], m.ram[0xcb04]), (0, 1, 2));

    let mut m = Machine::assembled();
    m.seed_ppu(1, 0xb0);
    m.ram[0xcb28] = 1;
    m.ram[0xcb08] = 0xb0;
    m.ram[0xca11] = 1;
    m.ram[0xcb1a] = 1;
    m.ram[0xc07f] = 1; // audio guard: real lag body skips its shared audio call
    m.ram[0xc0fe] = 0x1e;
    m.ram[0xcb02] = 0xf0;
    m.ram[0xcb76] = 0;
    m.ram[0xcb77] = 0xd3;
    m.ram[0xd47d] = 0xc0;
    m.ram[0xd47e] = 0xd4;
    let cpu = m.call("irq_handler", false);
    assert_eq!((m.ram[0xca11], m.ram[0xcb02]), (1, 0xf0));
    assert_eq!((m.ram[READY], m.ram[PUBLISH], m.ram[CONSUME]), (0, 0, 0));
    assert_eq!(m.ram[0xc01a], 0, "lag path must not run game logic");
    assert_eq!(
        m.ram[0xcb05], 0,
        "delivered NMI acknowledges synthetic VBlank"
    );
    assert_eq!((m.ram[0xcb0c], m.ram[0xcb0d]), (0x39, 4));
    assert_eq!((cpu.d, cpu.e), (0x52, 0xa9));
    assert_eq!(m.ram[0xcb1d], 0);
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn first_nmi_waits_for_reset_bank_context_in_actual_irq() {
    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[0xcb08] = 0xb0; // C102 enabled NMI, C02B has not initialized $24
    m.ram[0xcb02] = 0xf0;
    m.ram[0xcb12] = 2;
    m.ram[0xcb20] = 7;
    m.ram[0xcb62] = 0;
    m.ram[0xdfff] = m.labels["data_prg_bank_0"].0;
    let delivery = m.labels["_cv1_deliver_nmi"].1;
    // Both post-C102/pre-C029 and post-C029 LDA #6/pre-C02B STA $24
    // must defer. A live accumulator of6 is not initialized saved bank RAM.
    for accumulator in [0, 6] {
        let mut cpu = m.cpu("irq_handler", false);
        cpu.a = accumulator;
        for _ in 0..100_000 {
            assert_ne!(
                cpu.pc, delivery,
                "entered first NMI before saved PRG bank initialization"
            );
            if cpu.pc == 7 {
                break;
            }
            cpu.step(&mut m).unwrap();
        }
        assert_eq!(cpu.pc, 7);
    }
    assert_eq!(m.ram[0xcb1a], 0, "first NMI must remain deferred");
    assert_eq!(m.ram[0xcb04], 2, "physical IRQ still delivered");
    assert_eq!((m.ram[0xcb02], m.ram[0xcb12], m.ram[0xcb20]), (0xf0, 2, 7));
    assert_eq!((m.ram[0xc01a], m.ram[0xca11], m.ram[PUBLISH]), (0, 0, 0));

    m.ram[0xc024] = 6; // reset C02B completed: now a full NMI may begin
    // C02B has set saved context; C02D may still have bank0 mapped. The
    // full prologue explicitly restores $24, so either physical mapping
    // must become eligible. Later saved-bank0 frames must remain eligible.
    for (saved, mapped, started) in [(6, 0, 0), (6, 6, 0), (0, 0, 1)] {
        let mut scenario = m.clone();
        scenario.ram[0xc024] = saved;
        scenario.ram[0xcb62] = mapped;
        scenario.ram[0xdfff] = scenario.labels[&format!("data_prg_bank_{mapped}")].0;
        scenario.ram[0xcb1a] = started;
        let mut cpu = scenario.cpu("irq_handler", false);
        scenario.run_until(&mut cpu, delivery);
        assert_eq!(scenario.ram[0xcb1a], 1);
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn nested_lag_preserves_stripe_terminator_at_every_ldx_boundary() {
    // CCEE appends zero at the current stripe-buffer cursor. CCF0's LDX
    // spills that A to CB27; an IRQ inside this instruction used to replace
    // the terminator with the lag NMI's own LDX accumulator. The next stripe
    // upload then interpreted stale timer digits as vertical nametable data.
    let mut original = Machine::assembled();
    original.seed_busy_lag_irq();
    original.ram[0xc020] = 12;
    original.ram[0xc70c] = 0xe0;
    let mut cpu = original.cpu("L_CCEE", true);
    let ldx = original.labels["L_CCF0"].1;
    let store = original.labels["L_CCF2"].1;
    let stop = original.labels["L_CCF6"].1;
    original.run_until(&mut cpu, ldx);
    let mut boundaries = 0;
    while cpu.pc != store {
        let mut interrupted = original.clone();
        let mut interrupted_cpu = cpu;
        interrupted.interrupt(&mut interrupted_cpu);
        interrupted.run_until(&mut interrupted_cpu, stop);
        assert_eq!(
            interrupted.ram[0xc70c], 0,
            "stripe terminator corrupted by IRQ at {:04x}",
            cpu.pc
        );
        assert_eq!((interrupted_cpu.a, interrupted_cpu.d), (0, 13));
        assert_eq!(interrupted.ram[0xcb1d], 0);
        boundaries += 1;
        cpu.step(&mut original).unwrap();
    }
    assert!(boundaries >= 10, "must cover the whole generated LDX");
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn nested_lag_preserves_live_ppu_spills_at_every_interruptible_boundary() {
    // The emitted original C11F contains both unguarded STA $2005 and
    // guarded STA $2000. C0C8 contains STA $2001. Cover the pre-DI stores
    // and post-EI/JR reloads too, respecting the real EI acceptance delay.
    for entry in ["L_C11F", "L_C0C8"] {
        let mut original = Machine::assembled();
        original.seed_busy_lag_irq();
        let mut cpu = original.cpu(entry, true);
        let stop = if entry == "L_C11F" {
            original.labels["rt_translated_rts"].1
        } else {
            // End just after STA $2001 restores A, before the following
            // translated JSR. Derive it from the assembled instruction path
            // instead of depending on a generated continuation's ordinal.
            let mut probe = original.clone();
            let mut probe_cpu = cpu;
            let mut end = None;
            for _ in 0..1000 {
                let pc = probe_cpu.pc;
                if probe_cpu.iff1
                    && probe_cpu.ei_pending == 0
                    && [probe.read(pc), probe.read(pc + 1), probe.read(pc + 2)]
                        == [0x3a, 0x18, 0xcb]
                {
                    end = Some(pc + 3);
                    break;
                }
                probe_cpu.step(&mut probe).unwrap();
            }
            end.expect("inline MASK must restore A after EI")
        };
        let mut boundaries = 0;
        for _ in 0..10_000 {
            if cpu.pc == stop {
                break;
            }
            if cpu.iff1 && cpu.ei_pending == 0 {
                let mut interrupted = original.clone();
                let mut interrupted_cpu = cpu;
                let before = (original.ram[0xcb18], original.ram[0xcb27]);
                interrupted.interrupt(&mut interrupted_cpu);
                assert_eq!(
                    (interrupted.ram[0xcb18], interrupted.ram[0xcb27]),
                    before,
                    "live lowerer spills changed by IRQ at {entry}/{:04x}",
                    cpu.pc
                );
                assert_eq!(
                    (
                        interrupted_cpu.a,
                        interrupted_cpu.f,
                        interrupted_cpu.b,
                        interrupted_cpu.c,
                        interrupted_cpu.d,
                        interrupted_cpu.e,
                        interrupted_cpu.h,
                        interrupted_cpu.l,
                        interrupted_cpu.sp
                    ),
                    (
                        cpu.a, cpu.f, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l, cpu.sp
                    ),
                    "interrupted CPU context at {entry}/{:04x}",
                    cpu.pc
                );
                for address in [0xcb03, 0xcb14, 0xcb62, 0xdffc, 0xdffe, 0xdfff] {
                    assert_eq!(
                        interrupted.ram[address], original.ram[address],
                        "field {address:04x} at {entry}/{:04x}",
                        cpu.pc
                    );
                }
                boundaries += 1;
            }
            cpu.step(&mut original).unwrap();
        }
        assert_eq!(cpu.pc, stop);
        assert!(boundaries >= 3, "no meaningful IRQ boundaries for {entry}");
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn spill_pair_is_reentrant_and_balanced_on_all_irq_exits() {
    for (ready, status) in [(0, 0x80), (1, 0), (1, 0x80)] {
        let mut m = Machine::assembled();
        m.ram[0xcb28] = ready;
        m.vdp_status = status;
        m.ram[0xcb18] = 0x5a;
        m.ram[0xcb27] = 0xa5;
        let cpu = m.call("irq_handler", false);
        assert_eq!((m.ram[0xcb18], m.ram[0xcb27]), (0x5a, 0xa5));
        assert_eq!(cpu.sp, 0xdff6, "one native return, balanced IRQ saves");
        assert_eq!(cpu.iff1, ready != 0, "boot-not-ready must stay DI");
    }

    let mut m = Machine::assembled();
    m.seed_busy_lag_irq();
    m.ram[0xcb18] = 0x5a;
    m.ram[0xcb27] = 0xa5;
    let mut cpu = m.cpu("irq_handler", false);
    let lag = m.labels["L_C0C0"].1;
    m.run_until(&mut cpu, lag);
    cpu.step(&mut m).unwrap(); // LDA $FE
    cpu.step(&mut m).unwrap(); // LDX $1F: spill A
    assert_eq!(m.ram[0xcb27], 0x1e, "the actual lag body reused the spill");
    assert_eq!(m.ram[0xca11], 2);
    let inner_spills = (m.ram[0xcb18], m.ram[0xcb27]);
    m.interrupt(&mut cpu); // depth limit skips the third translated body
    assert_eq!((m.ram[0xcb18], m.ram[0xcb27]), inner_spills);
    m.run_until(&mut cpu, 7);
    assert_eq!((m.ram[0xcb18], m.ram[0xcb27]), (0x5a, 0xa5));
    assert_eq!(m.ram[0xca11], 1);
    assert_eq!(cpu.sp, 0xdff6);
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn every_partial_chr_write_marks_sprite_source_dirty_without_copy_through() {
    let mut m = Machine::assembled();
    m.seed_ppu(1, 0); // actual 8x8, before the sticky 8x16 latch
    m.ram[0xcb08] = 0;
    m.ram[0xca13] = 0xff; // before first BG presentation: no live BG variant
    for n in 0..300 {
        m.ram[0xcb0f] = 0;
        m.ram[0xcb10] = 3; // partial row, not the conventional final tile byte
        let mut cpu = m.cpu("rt_ppu_write", true);
        cpu.b = 7;
        cpu.a = n as u8;
        m.run_until(&mut cpu, 7);
        assert_eq!(m.sram[0x803], n as u8, "raw CHR stays authoritative");
        assert_eq!(m.ram[0xc801], 1, "sticky dirty must not wrap at write {n}");
        assert_eq!((cpu.a, cpu.d, cpu.e, cpu.iff1), (n as u8, 0x52, 0xa9, true));
        assert!(
            m.ports.is_empty(),
            "raw sprite updates must not alter displayed pattern slots"
        );
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn actual_irq_blanks_before_scene_rebuild_and_commits_mask_off_hidden_sat() {
    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[READY] = 1;
    m.ram[PACKET] = 0xb0;
    m.ram[PACKET + 1] = 0x18;
    m.ram[PACKET + 12] = 0x40; // frozen large rendering-off screen upload
    let mut cpu = m.cpu("irq_handler", false);
    let destructive_rebuild = m.labels["rt_bg_reset_variant_cache"].1;
    m.run_until(&mut cpu, destructive_rebuild);
    let last_reg1 = m
        .ports
        .windows(2)
        .rev()
        .find_map(|pair| (pair[0].0 == 0xbf && pair[1] == (0xbf, 0x81)).then_some(pair[0].1));
    assert_eq!(
        last_reg1,
        Some(0xb2),
        "display must stay off before reclaiming visible BG slots; hidden sprite preparation may follow the blank write"
    );
    assert_ne!(m.ram[0xc802] & 2, 0, "SAT backend owns the blanking fence");

    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[READY] = 1;
    m.ram[PACKET] = 0xb0;
    m.ram[PACKET + 1] = 8; // BG enabled, NES sprites disabled
    // Zero OAM would otherwise produce64 visible sprites; the real bridge
    // must invoke the prepared backend instead of retaining the old SAT.
    m.call("irq_handler", false);
    let payload: Vec<_> = m
        .ports
        .iter()
        .filter_map(|(port, value)| (*port == 0xbe).then_some(*value))
        .collect();
    assert_eq!(payload.len(), 192);
    assert_eq!(&payload[..64], &[0xe0; 64]);
    assert_eq!(&payload[64..], &[0; 128]);
    assert_eq!(m.ram[CONSUME], 1);
}

fn pending_from_actual_irq() -> Machine {
    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[0xcb1a] = 1;
    m.ram[0xcb08] = 0xb8;
    m.ram[0xcb12] = 2;
    m.ram[0xcb20] = 7;
    m.ram[0xca18] = 3;
    m.ram[READY] = 1;
    m.ram[PACKET] = 0xb0;
    m.ram[PACKET + 1] = 0; // keep an unconsumed rendering-off count
    m.ram[PACKET + 9] = 0xb2;
    m.ram[PACKET + 12] = 7;
    m.ram[0xc802] = 4; // initialized 8x16 cache, no mode-change blank
    m.ram[0xd468] = 1;
    m.vcounters = vec![0xe0, 0x20]; // old BG wait admits; final try misses
    m.call("irq_handler", false);
    assert_eq!((m.ram[PENDING], m.ram[READY], m.ram[CONSUME]), (1, 0, 1));
    assert_eq!((m.ram[0xcb12], m.ram[0xcb20]), (2, 7));
    assert_eq!(m.ram[0xca12], 0);
    assert_eq!(
        m.ram[0xca18], 10,
        "unconsumed old count merges exactly once"
    );
    assert_eq!(&m.ram[PACKET + 9..PACKET + 13], &[0; 4]);
    assert!(
        m.ports.iter().all(|p| p.0 == 0x7f),
        "late upload must not write video; physical audio still runs"
    );
    m.ports.clear();
    m.vcounters.clear();
    m.vcounter_reads = 0;
    m
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn pending_retries_restore_live_intent_without_double_consumption_or_count() {
    let mut m = pending_from_actual_irq();
    let packet = m.ram[PACKET..PACKET + 13].to_vec();
    m.ram[0xcb2d] = 0xf2;
    m.ram[0xcb7f] = 0x40;
    m.ram[0xcb78] = 3;
    m.ram[0xca18] = 21;
    m.ram[0xc801] = 1;
    for _ in 0..3 {
        let before: Vec<_> = FIELDS.iter().map(|address| m.ram[*address]).collect();
        let pins = m.ram[0xd440..0xd450].to_vec();
        m.vcounters = vec![0x20, 0xe0];
        m.vcounter_reads = 0;
        m.call("irq_handler", false);
        assert_eq!(m.vcounter_reads, 1, "failed IRQ try must not spin");
        assert_eq!(FIELDS.iter().map(|a| m.ram[*a]).collect::<Vec<_>>(), before);
        assert_eq!(&m.ram[0xd440..0xd450], pins);
        assert_eq!(&m.ram[PACKET..PACKET + 13], packet);
        assert_eq!((m.ram[PENDING], m.ram[READY], m.ram[CONSUME]), (1, 0, 1));
        assert_eq!(m.ram[0xca12], 0);
        assert!(m.ports.iter().all(|p| p.0 == 0x7f));
        m.ports.clear();
    }
    // Keep the next full producer disabled after the successful commit so
    // this test ends at the IRQ boundary, not inside new translated work.
    m.ram[0xcb08] = 0;
    m.ram[0xcb1a] = 0;
    m.vcounters.clear();
    m.vcounter_reads = 0;
    m.call("irq_handler", false);
    assert_eq!((m.ram[PENDING], m.ram[READY], m.ram[CONSUME]), (0, 0, 1));
    assert_eq!(
        (m.ram[0xcb2d], m.ram[0xcb7f], m.ram[0xcb78], m.ram[0xca18]),
        (0xf2, 0x40, 3, 21)
    );
    assert_eq!(m.ram[0xc801], 1, "commit cannot consume future CHR writes");
    assert_eq!(m.ports.iter().filter(|p| p.0 == 0xbe).count(), 192);
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn pending_blocks_new_full_producers_but_delivers_the_real_busy_lag() {
    let pending = pending_from_actual_irq();
    for busy in [0, 1] {
        for depth in [0, 1] {
            let mut m = pending.clone();
            m.ram[0xc01b] = busy;
            m.ram[0xca11] = depth;
            m.ram[0xc07f] = 1; // real lag, skip shared audio
            m.ram[0xc0fe] = 0x1e;
            m.ram[0xc0ff] = 0xb0;
            m.ram[0xcb02] = 0xf0;
            m.ram[0xcb76] = 0;
            m.ram[0xcb77] = 0xd3;
            m.ram[0xd47d] = 0xc0;
            m.ram[0xd47e] = 0xd4;
            m.vcounters = vec![0x20];
            m.vcounter_reads = 0;
            let packet = m.ram[PACKET..PACKET + 13].to_vec();
            m.call("irq_handler", false);
            assert_eq!((m.ram[PENDING], m.ram[CONSUME], m.ram[PUBLISH]), (1, 1, 0));
            assert_eq!(&m.ram[PACKET..PACKET + 13], packet);
            assert_eq!(m.ram[0xca11], depth);
            assert_eq!(m.ram[0xc01b], busy, "do not fake a NES busy flag");
            if busy == 0 {
                assert_eq!((m.ram[0xcb12], m.ram[0xcb20]), (2, 7));
            } else {
                assert_eq!(m.ram[0xcb09], 0x1e, "translated C0C0 lag executed");
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn pending_ppudata_barrier_precedes_all_raw_classes_and_preserves_guarded_guest_abi() {
    let pending = pending_from_actual_irq();
    for address in [0x0003u16, 0x2413, 0x27c1, 0x3f01] {
        for increment in [1, 32] {
            for iff in [false, true] {
                for outer_depth in [0, 1] {
                    for sram_control in [0, 8] {
                        let mut m = pending.clone();
                        m.ram[0xcb08] = if increment == 32 { 4 } else { 0 };
                        m.ram[0xcb09] = 0;
                        m.ram[0xcb0f] = (address >> 8) as u8;
                        m.ram[0xcb10] = address as u8;
                        m.ram[0xcb03] = 0x7d;
                        m.ram[0xca13] = 0xff; // no live BG variants in this fixture
                        m.ram[0xd47f] = outer_depth;
                        m.ram[0xca19..0xca20].copy_from_slice(&[1, 8, 2, 2, 0, 0, 3]);
                        m.ram[0xd3ff] = 3;
                        m.ram[0xdffc] = sram_control;
                        m.ram[0xdfff] = m.labels["data_prg_bank_2"].0;
                        m.ram[0xcb62] = 2;
                        let outer_guard = m.ram[0xca19..0xca1d].to_vec();
                        let mapping = (m.ram[0xdffc], m.ram[0xdfff], m.ram[0xcb62]);
                        let mut cpu = m.cpu("rt_ppu_write", iff);
                        cpu.a = 0x21;
                        cpu.b = 7;
                        let sp = cpu.sp;
                        m.run_until(&mut cpu, 7);
                        assert_eq!((m.ram[PENDING], m.ram[CONSUME]), (0, 1));
                        assert_eq!(
                            (cpu.a, cpu.d, cpu.e, cpu.iff1, cpu.sp),
                            (0x21, 0x52, 0xa9, iff, sp + 2)
                        );
                        assert_eq!(m.ram[0xcb03], 0x7d);
                        assert_eq!(m.ram[0xd47f], outer_depth);
                        assert_eq!((m.ram[0xdffc], m.ram[0xdfff], m.ram[0xcb62]), mapping);
                        if outer_depth == 1 {
                            assert_eq!(&m.ram[0xca19..0xca1d], outer_guard);
                        }
                        let next = address + increment;
                        assert_eq!(
                            (m.ram[0xcb0f], m.ram[0xcb10]),
                            ((next >> 8) as u8, next as u8)
                        );
                        let payload: Vec<_> = m.ports.iter().filter(|p| p.0 == 0xbe).collect();
                        assert!(payload.len() >= 192);
                        assert!(payload[..64].iter().all(|p| p.1 == 0xe0));
                        assert!(payload[64..192].iter().all(|p| p.1 == 0));
                        if address < 0x3f00 {
                            let first = m.raw_writes.first().unwrap_or_else(|| {
                                panic!("raw source byte must be stored at {address:04x}")
                            });
                            assert_eq!(first.1, 0x21);
                            assert_eq!(
                                m.ports[..first.2].iter().filter(|p| p.0 == 0xbe).count(),
                                192,
                                "source write cannot precede the old complete SAT"
                            );
                        } else {
                            assert_eq!(
                                payload.len(),
                                193,
                                "palette write follows the complete old SAT"
                            );
                        }
                        if address < 0x2000 {
                            assert_eq!(m.ram[0xc801], 1, "new CHR write becomes next dirty intent");
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn barrier_has_eight_byte_headroom_from_actual_apply_entry_and_keeps_continuation() {
    let pending = pending_from_actual_irq();
    for depth in [1, 2] {
        let mut m = pending.clone();
        m.ram[0xd47f] = depth;
        m.ram[0xcb18] = 0x21;
        m.ram[0xcb1e] = 0x52;
        m.ram[0xcb1f] = 0xa9;
        m.ram[0xd3fc..0xd400].copy_from_slice(&[0x34, 0x56, 1, 6]);
        m.ram[0xdd80..0xde40].fill(0x5a);
        let mut cpu = m.cpu("rt_ppudata_apply", false);
        cpu.sp = 0xde48; // apply already owns its caller's return frame
        cpu.d = 0x24;
        cpu.e = 0x13;
        let stop = m.labels["_cv1_ppudata_unblocked"].1;
        let mut min_sp = cpu.sp;
        for _ in 0..100_000 {
            min_sp = min_sp.min(cpu.sp);
            if cpu.pc == stop {
                break;
            }
            cpu.step(&mut m).unwrap();
        }
        assert_eq!(cpu.pc, stop);
        assert_eq!(
            min_sp, 0xde40,
            "budget from apply, not an isolated DFF0 helper"
        );
        assert_eq!(cpu.sp, 0xde48);
        assert_eq!((cpu.d, cpu.e), (0x24, 0x13));
        assert_eq!(&m.ram[0xd3fc..0xd400], &[0x34, 0x56, 1, 6]);
        assert_eq!(
            (m.ram[0xcb18], m.ram[0xcb1e], m.ram[0xcb1f]),
            (0x21, 0x52, 0xa9)
        );
        assert!(m.ram[0xdd80..0xde40].iter().all(|b| *b == 0x5a));
    }

    for iff in [false, true] {
        let mut m = pending.clone();
        m.ram[0xcb0f] = 0x3f;
        m.ram[0xcb10] = 1;
        let mut cpu = m.cpu("rt_ppu_write_cont", iff);
        cpu.a = 0x21;
        cpu.b = 7;
        cpu.h = 0;
        cpu.l = 7;
        let sp = cpu.sp;
        m.run_until(&mut cpu, 7);
        assert_eq!(
            (cpu.a, cpu.d, cpu.e, cpu.sp, cpu.iff1),
            (0x21, 0x52, 0xa9, sp, iff)
        );
        assert_eq!(m.ram[PENDING], 0);
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn cv1_rejects_nonempty_dormant_vbuf_before_any_video_write() {
    let mut m = Machine::assembled();
    m.ram[0xcb28] = 1;
    m.ram[0xc800] = 1;
    m.ram[READY] = 1;
    let mut cpu = m.cpu("irq_handler", false);
    let halt = m.labels["_cv1_vbuf_halt"].1;
    m.run_until(&mut cpu, halt);
    assert_eq!(
        m.ram[0xcb1d], 0xfa,
        "FA means CV1 dormant vbuf became active"
    );
    assert!(m.ports.is_empty());
}

// Exact Z80 timing for the deliberately small admitted SAT/scroll path.
// Cpu.cycles is approximate (notably OUTI), so reject unknown opcodes here.
fn step_commit_tstates(m: &mut Machine, cpu: &mut Cpu) -> u32 {
    let pc = cpu.pc;
    let op = m.read(pc);
    let second = m.read(pc + 1);
    cpu.step(m).unwrap();
    match op {
        0xd3 | 0xdb => 11,
        0x32 | 0x3a => 13,
        0x01 | 0x11 | 0x21 => 10,
        0x3e | 0xd6 | 0xe6 | 0xf6 | 0xfe => 7,
        0xaf | 0xb7 => 4,
        0x40..=0x7f if op != 0x76 && op & 7 != 6 && (op >> 3) & 7 != 6 => 4,
        0x18 => 12,
        0x20 | 0x28 | 0x30 | 0x38 => {
            if cpu.pc == pc + 2 {
                7
            } else {
                12
            }
        }
        0xc3 | 0xc2 | 0xca | 0xd2 | 0xda | 0xe2 | 0xea | 0xf2 | 0xfa => 10,
        0xcd => 17,
        0xc9 => 10,
        0xcb if (0x40..0x80).contains(&second) && second & 7 != 6 => 8,
        0xed => match second {
            0xa3 => 16,
            0xb0 => {
                if cpu.pc == pc {
                    21
                } else {
                    16
                }
            }
            0x44 => 8,
            _ => panic!("unbudgeted ED {second:02x} at {pc:04x}"),
        },
        _ => panic!("unbudgeted commit opcode {op:02x} at {pc:04x}"),
    }
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn whole_pending_commit_including_scroll_finishes_inside_latest_admitted_blank() {
    let pending = pending_from_actual_irq();
    let mut maximum = 0;
    for entry in ["rt_cv1_try_finish_pending", "rt_cv1_finish_pending"] {
        for split_flags in 0..8 {
            for overrun in [0, 1] {
                let mut m = pending.clone();
                m.ram[PACKET + 4] = split_flags;
                m.ram[PACKET + 5..PACKET + 9].copy_from_slice(&[0x10, 0, 0x91, 0x18]);
                m.ram[0xcb29] = overrun;
                m.vcounters = vec![0xec, 0xed]; // latest valid admission, then later blank
                m.vcounter_reads = 0;
                let mut cpu = m.cpu(entry, false);
                let sample = if entry == "rt_cv1_try_finish_pending" {
                    m.labels["_cv1_sat_try_sample"].1
                } else {
                    m.labels["_cv1_sat_early_blank"].1
                };
                m.run_until(&mut cpu, sample);
                let end = m.labels["rt_cv1_frame_end"].1;
                let mut cycles = 0;
                let mut last_visible = 0;
                for _ in 0..1000 {
                    if cpu.pc == end {
                        break;
                    }
                    let before = m.ports.len();
                    cycles += step_commit_tstates(&mut m, &mut cpu);
                    if m.ports[before..].iter().any(|p| p.0 == 0xbe || p.0 == 0xbf) {
                        last_visible = cycles;
                    }
                }
                assert_eq!(cpu.pc, end);
                assert_eq!(m.ram[PENDING], 0);
                assert_eq!(
                    m.vcounter_reads, 2,
                    "one admission plus scroll's VCounter read"
                );
                assert_eq!(m.ports.iter().filter(|p| p.0 == 0xbe).count(), 192);
                assert!(
                    last_visible <= 4096,
                    "whole commit is {last_visible}T: {entry}/{split_flags}/{overrun}"
                );
                assert!(
                    last_visible < 19 * 228,
                    "latest EC leaves19 full stock scanlines"
                );
                maximum = maximum.max(last_visible);
            }
        }
    }
    eprintln!(
        "whole_pending_commit_last_visible_max_tstates={maximum} stock_remaining_after_latest_EC=4332"
    );
}

#[test]
#[ignore = "requires CV1_HANDOFF_PROJECT assembled with Docker WLA-DX"]
fn pending_mode_and_full_pool_fences_keep_prepared_pixels_and_pins_until_commit() {
    for (control, old_mode, full_pool) in [(0x00u8, 1, false), (0xb0, 0, false), (0xb0, 1, true)] {
        let mut m = Machine::assembled();
        m.ram[0xcb28] = 1;
        m.ram[READY] = 1;
        m.ram[PACKET] = control;
        m.ram[PACKET + 1] = 0x18;
        m.ram[0xc802] = 4;
        m.ram[0xd468] = old_mode;
        m.ram[0xc900..0xca00].fill(0xff);
        m.ram[0xc900..0xc904].copy_from_slice(&[40, 2, 0, 20]);
        m.ram[0xd480..0xd4c0].fill(0xff);
        if full_pool {
            m.ram[0xd440..0xd448].fill(0xff);
        }
        m.vcounters = vec![0xe0, 0xe0, 0x20]; // BG wait, required blank, late final try
        m.call("irq_handler", false);
        assert_eq!(m.ram[PENDING], 1);
        let blank_reg1 = 0xb0 | ((control & 0x20) >> 4);
        assert_eq!(
            m.ports
                .windows(2)
                .rev()
                .find_map(|p| { (p[0].0 == 0xbf && p[1] == (0xbf, 0x81)).then_some(p[0].1) }),
            Some(blank_reg1)
        );
        let staged = m.ram[0xc840..0xc900].to_vec();
        let pins = m.ram[0xd440..0xd450].to_vec();
        let keys = m.ram[0xd400..0xd440].to_vec();
        let attrs = m.ram[0xd480..0xd4c0].to_vec();
        let prepared_writes = m.ports.iter().filter(|p| p.0 == 0xbe).count();
        assert_eq!(prepared_writes, if control & 0x20 == 0 { 32 } else { 64 });
        m.vcounters = vec![0x20];
        m.vcounter_reads = 0;
        m.call("irq_handler", false);
        assert_eq!(&m.ram[0xc840..0xc900], staged);
        assert_eq!(&m.ram[0xd440..0xd450], pins);
        assert_eq!(&m.ram[0xd400..0xd440], keys);
        assert_eq!(&m.ram[0xd480..0xd4c0], attrs);
        assert_eq!(
            m.ports.iter().filter(|p| p.0 == 0xbe).count(),
            prepared_writes
        );
        m.vcounters.clear();
        m.vcounter_reads = 0;
        m.call("rt_cv1_finish_pending", false);
        assert_eq!(m.ram[PENDING], 0);
        assert_eq!(&m.ram[0xd440..0xd448], &pins[8..]);
        assert_eq!(
            m.ports.iter().filter(|p| p.0 == 0xbe).count(),
            prepared_writes + 192
        );
        assert_eq!(
            m.ports
                .windows(2)
                .rev()
                .find_map(|p| { (p[0].0 == 0xbf && p[1] == (0xbf, 0x81)).then_some(p[0].1) }),
            Some(blank_reg1 | 0x40)
        );
    }
}
