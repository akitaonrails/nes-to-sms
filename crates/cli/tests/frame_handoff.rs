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
            self.ram[0xc000 + (addr as usize & 0x1fff)] = value;
        } else if addr >= 0x8000 && self.ram[0xdffc] & 8 != 0 {
            self.sram[addr as usize - 0x8000] = value;
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        if port == 0xbf { self.vdp_status } else { 0xff }
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
