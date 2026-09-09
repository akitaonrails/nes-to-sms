//! Hardware-derived literal APU register/sequencer tests. The assembled APU
//! is the subject; its tables/algorithms never supply expected values. Direct
//! cycle calls obey the peripheral ABI, not proof of source bus integration.

use super::*;

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT frozen PAL240 subject"]
fn assembled_pal_period_lookup_matches_all_literal_clock_ratios_and_restores_banks() {
    let mut probe = ApuProbe::new();
    probe.bus.write(0xfffe, 17);
    probe.bus.write(0xcb14, 19);
    probe.bus.write(0xffff, 23);
    probe.bus.write(0xfffc, 12);
    probe.set_cycle(0x12345678);
    let source = probe.bus.ram[..0x1e40].to_vec();
    let cartridge = probe.bus.cart_ram;
    let psg = probe.bus.psg_log.clone();
    for n in 1..=4096u16 {
        // Integer comparison derived directly from the two crystal/divider
        // ratios. Rounding precedes octave folding; no produced LUT is read
        // to decide the expected result.
        let numerator = u64::from(n) * 585_237_664;
        let mut expected = ((numerator + 295_312_500) / 590_625_000).max(1);
        while expected >= 1024 {
            expected /= 2;
        }
        let iff = n & 1 != 0;
        let result = probe.call_abi("rt_source_apu_pal_period", 0x71, n, 0x2571, iff);
        assert_eq!(probe.bus.read(0xcb1d), 0);
        assert_eq!(result.hl(), expected as u16, "raw NES period {n}");
        assert_eq!((result.bc(), result.de()), (0x2571, 0x3692));
        assert_eq!(probe.bus.read(0xcb14), 19);
        assert_eq!(
            &probe.bus.ram[..0x1e40],
            source,
            "lookup changed source RAM at {n}"
        );
        assert_eq!(probe.bus.cart_ram, cartridge);
        assert_eq!(probe.bus.psg_log, psg);
    }
    for n in [0u16, 4097, 0xffff] {
        let mut invalid = ApuProbe::new();
        invalid.bus.write(0xfffe, 17);
        invalid.bus.write(0xcb14, 19);
        let mapping = (invalid.bus.mapper_control, invalid.bus.slot_bank);
        invalid.call("rt_source_apu_pal_period", 0x71, n);
        assert_eq!(invalid.bus.read(0xcb1d), 0xe9);
        assert_eq!(invalid.bus.read(0xd51a), 6);
        assert_eq!((invalid.bus.mapper_control, invalid.bus.slot_bank), mapping);
        assert_eq!(invalid.bus.read(0xcb14), 19);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT frozen PAL240 source APU"]
fn assembled_pal_pulse_triangle_publish_calibrates_before_fold_and_keeps_source_time() {
    let mut p = ApuProbe::new();
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    p.bus.psg_log.clear(); // Observation starts after the explicit initial mute.
    p.bus.write(0xfffe, 17);
    p.bus.write(0xcb14, 19);
    for (address, value) in [
        (0x4000, 0x3f),
        (0x4001, 7),
        (0x4002, 0xff),
        (0x4015, 1),
        (0x4003, 3),
    ] {
        p.write(address, value);
    }
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(p.bus.psg_log, vec![0x87, 0x3f, 0x90]); // Raw1024 ->1015, not an octave shift.
    assert_eq!(p.cycle(), 0);
    p.write(0x4002, 8);
    p.write(0x4003, 4); // Raw1033 ->512 only AFTER calibration.
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(p.bus.psg_log, vec![0x87, 0x3f, 0x90, 0x80, 0x20]);
    for (address, value) in [(0x4015, 5), (0x4008, 0xff), (0x400a, 0xff), (0x400b, 7)] {
        p.write(address, value);
    }
    for _ in 0..7457 {
        p.tick();
    } // Real synthetic quarter-frame reloads triangle linear.
    let units = p.bus.ram[0xb30..0xb56].to_vec();
    let cycle = p.cycle();
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    // Raw4096 ->1014. The existing documented square-wave triangle
    // adaptation retains its -4dB (attenuation2) setting on this target.
    assert_eq!(&p.bus.psg_log[5..], &[0xc6, 0x3f, 0xd2]);
    assert_eq!(p.cycle(), cycle);
    assert_eq!(&p.bus.ram[0xb30..0xb56], units);
    let writes = p.bus.psg_log.clone();
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(
        p.bus.psg_log, writes,
        "clean publication must not reprogram PSG"
    );
    p.write(0x4015, 0);
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(&p.bus.psg_log[writes.len()..], &[0x9f, 0xdf]);
    assert_eq!(p.cycle(), cycle);
    assert_eq!(p.bus.read(0xcb14), 19);
}

#[test]
#[ignore = "requires TRACE_CNROM_SOURCE_AUDIO_PROJECT genuine translated two-note fixture"]
fn assembled_source_audio_writes_publish_two_literal_periods_without_charging_time() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_SOURCE_AUDIO_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    assert!(
        !defs.contains_key("rt_source_domain_span_begin"),
        "frozen precise subject"
    );
    let mut bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    let mut cpu = Cpu::new();
    let cycle = |bus: &SmsBus| u32::from_le_bytes(bus.ram[0xa80..0xa84].try_into().unwrap());
    let mut writes = Vec::new();
    let mut markers = Vec::new();
    let mut psg = Vec::new();
    let mut publication = None;
    let mut publications = 0;
    for _ in 0..25_000_000 {
        if bus.read(0xcb1d) != 0 {
            break;
        }
        if let Some((return_pc, at)) = publication
            && cpu.pc == return_pc
        {
            assert_eq!(
                cycle(&bus),
                at,
                "PSG publication must not charge source time"
            );
            publication = None;
            publications += 1;
        }
        if cpu.pc == defs["rt_source_apu_publish"].1 {
            assert!(publication.is_none());
            let ret = u16::from_le_bytes([bus.read(cpu.sp), bus.read(cpu.sp + 1)]);
            publication = Some((ret, cycle(&bus)));
        }
        if cpu.pc == defs["rt_source_bus_write_event"].1 {
            if (0x4000..=0x4017).contains(&cpu.hl()) {
                writes.push((cycle(&bus), cpu.hl(), cpu.a));
            }
            if cpu.hl() == 0x07fe {
                markers.push((cycle(&bus), cpu.a));
            }
        }
        let prior = bus.psg_log.len();
        cpu.step(&mut bus).unwrap();
        for &byte in &bus.psg_log[prior..] {
            psg.push((cycle(&bus), byte));
        }
    }
    assert_eq!(bus.read(0xcb1d), 0xe8);
    assert_eq!(bus.read(0xc7ff), 0xa5);
    assert_eq!(cycle(&bus), 55384);
    assert_eq!(&bus.ram[0xa84..0xa88], &[85, 0, 225, 0]);
    assert_eq!(bus.read(0xca88), 1);
    assert_eq!((bus.read(0xcb02), bus.read(0xcb03)), (0xfd, 0xa4));
    assert!(publication.is_none());
    assert!(publications > 1000);
    assert_eq!(
        writes,
        vec![
            (13, 0x4017, 0x40),
            (29598, 0x4000, 0x3f),
            (29604, 0x4001, 0),
            (29610, 0x4002, 0xfd),
            (29616, 0x4015, 1),
            (29622, 0x4003, 0),
            (42495, 0x4002, 0x7e),
            (55368, 0x4015, 0)
        ]
    );
    assert_eq!(markers, vec![(29628, 0xa1), (42501, 0xa2), (55374, 0xa3)]);
    let active: Vec<_> = psg.iter().copied().filter(|&(at, _)| at > 7).collect();
    // SN76489 channel0 period254: latch8E/data0F; period127:8F/07;
    // attenuation0 is90 and mute15 is9F. This checks the literal wire format,
    // not production cache contents or a waveform estimated by this harness.
    assert_eq!(
        active,
        vec![
            (29622, 0x8e),
            (29622, 0x0f),
            (29622, 0x90),
            (42495, 0x8f),
            (42495, 7),
            (55368, 0x9f)
        ]
    );
    assert!(
        psg.iter()
            .filter(|&&(at, _)| at <= 7)
            .all(|&(_, value)| [0x9f, 0xbf, 0xdf, 0xff].contains(&value)),
        "startup must emit mute only"
    );
    eprintln!(
        "source audio: writes={writes:?} markers={markers:?} PSG={psg:?} publications={publications} source=55384 approximate_z80={}",
        cpu.cycles
    );
}

struct ApuProbe {
    bus: SmsBus,
    defs: HashMap<String, (u8, u16)>,
}

impl ApuProbe {
    fn new() -> Self {
        let path = PathBuf::from(std::env::var("TRACE_CNROM_APU_PROJECT").unwrap());
        let mut probe = Self {
            bus: SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff),
            defs: load_wla_symbol_defs(&path.join("sms.sym")),
        };
        probe.call("rt_source_apu_init", 0, 0);
        probe.bus.write(0xcb03, 0xa5);
        probe
    }

    fn cycle(&self) -> u32 {
        u32::from_le_bytes(self.bus.ram[0xa80..0xa84].try_into().unwrap())
    }

    fn set_cycle(&mut self, value: u32) {
        self.bus.ram[0xa80..0xa84].copy_from_slice(&value.to_le_bytes());
    }

    fn word(&self, address: usize) -> u16 {
        u16::from_le_bytes(
            self.bus.ram[address - 0xc000..address - 0xbffe]
                .try_into()
                .unwrap(),
        )
    }

    fn call(&mut self, label: &str, a: u8, hl: u16) -> Cpu {
        self.call_bc(label, a, hl, 0x2571)
    }

    fn call_bc(&mut self, label: &str, a: u8, hl: u16, bc: u16) -> Cpu {
        self.call_abi(label, a, hl, bc, true)
    }

    fn call_abi(&mut self, label: &str, a: u8, hl: u16, bc: u16, iff: bool) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.a = a;
        cpu.f = 0x95;
        cpu.set_bc(bc);
        cpu.set_de(0x3692);
        cpu.set_hl(hl);
        cpu.iff1 = iff;
        cpu.iff2 = iff;
        cpu.pc = self.defs[label].1;
        cpu.sp = 0xdff0;
        self.bus.write(0xdff0, 7);
        self.bus.write(0xdff1, 0);
        let maps = (self.bus.mapper_control, self.bus.slot_bank);
        for _ in 0..20_000 {
            if cpu.pc == 7 || self.bus.read(0xcb1d) != 0 {
                break;
            }
            cpu.step(&mut self.bus).unwrap();
            assert!(cpu.sp >= NATIVE_STACK_FLOOR);
        }
        if self.bus.read(0xcb1d) != 0 {
            return cpu;
        }
        assert_eq!(cpu.pc, 7, "{label}");
        assert_eq!((self.bus.mapper_control, self.bus.slot_bank), maps);
        assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
        if matches!(
            label,
            "rt_source_apu_cycle"
                | "rt_source_apu_write"
                | "rt_source_apu_read"
                | "rt_source_apu_publish"
                | "rt_source_apu_deadline"
                | "rt_source_apu_quiet_advance"
        ) {
            assert_eq!((cpu.bc(), cpu.de()), (bc, 0x3692), "{label}");
            assert_eq!(self.bus.read(0xcb03), 0xa5);
            if label != "rt_source_apu_deadline" {
                assert_eq!(cpu.hl(), hl, "{label}");
            }
            if label != "rt_source_apu_read" {
                assert_eq!((cpu.a, cpu.f), (a, 0x95), "{label}");
            }
        }
        cpu
    }

    fn write(&mut self, address: u16, value: u8) {
        self.call("rt_source_apu_write", value, address);
        assert_eq!(
            self.bus.read(0xcb1d),
            0,
            "APU write{address:04X}={value:02X}"
        );
    }

    fn tick(&mut self) -> u8 {
        self.set_cycle(self.cycle().wrapping_add(1));
        self.call("rt_source_apu_cycle", 0x71, 0x4796);
        assert_eq!(self.bus.read(0xcb1d), 0, "APU cycle{}", self.cycle());
        self.bus.read(0xd51b)
    }

    fn status(&mut self, previous_bus: u8) -> u8 {
        let cpu = self.call("rt_source_apu_read", previous_bus, 0x4015);
        assert_eq!(self.bus.read(0xcb1d), 0, "APU status cycle{}", self.cycle());
        cpu.a
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_literal_length_table_enable_and_internal_status_bus() {
    const LENGTHS: [u8; 32] = [
        10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96,
        22, 192, 24, 72, 26, 16, 28, 32, 30,
    ];
    let mut p = ApuProbe::new();
    assert_eq!(p.status(0xff), 0x20);
    p.write(0x4015, 15);
    for (index, length) in LENGTHS.into_iter().enumerate() {
        for address in [0x4003, 0x4007, 0x400b, 0x400f] {
            p.write(address, (index as u8) << 3);
        }
        assert_eq!(
            &p.bus.ram[0xb4e..0xb52],
            &[length; 4],
            "length index{index}"
        );
        assert_eq!(p.status(0xff), 0x2f);
        assert_eq!(p.status(0xdf), 0x0f);
    }
    for (enable, expected) in [(1, 1), (2, 2), (4, 4), (8, 8), (0, 0)] {
        p.write(0x4015, enable);
        for address in [0x4003, 0x4007, 0x400b, 0x400f] {
            p.write(address, 0);
        }
        assert_eq!(p.status(0), expected);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_four_step_events_lengths_irq_and_source_counter_wrap() {
    for start in [0, u32::MAX - 14912] {
        let mut p = ApuProbe::new();
        p.set_cycle(start);
        p.write(0x4015, 1);
        p.write(0x4000, 0);
        p.write(0x4003, 0x18); // Literal length2.
        let mut events = Vec::new();
        for elapsed in 1..=29834 {
            let event = p.tick();
            if event != 0 {
                events.push((elapsed, event));
            }
            if matches!(elapsed, 14912 | 14913 | 29828 | 29829) {
                assert_eq!(
                    p.bus.read(0xcb4e),
                    match elapsed {
                        14912 => 2,
                        14913 | 29828 => 1,
                        _ => 0,
                    }
                );
            }
            if elapsed == 29827 {
                assert_eq!(p.status(0xff), 0x21);
            }
        }
        assert_eq!(
            events,
            [
                (7457, 1),
                (14913, 3),
                (22371, 1),
                (29828, 4),
                (29829, 7),
                (29830, 4)
            ]
        );
        assert_eq!(p.word(0xd504), 4);
        assert_eq!(p.status(0xff), 0x60);
        assert_eq!(p.bus.read(0xd501), 0);
        assert_eq!(p.bus.read(0xd500), 0);
        for _ in 0..3 {
            p.tick();
        }
        assert_eq!(p.status(0), 0);
        assert_eq!(p.cycle(), start.wrapping_add(29837));
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_4017_both_delays_and_five_step_timeline() {
    for (write_cycle, delay) in [(100u32, 3u32), (101, 4)] {
        let mut p = ApuProbe::new();
        for _ in 0..write_cycle {
            p.tick();
        }
        p.write(0x4015, 1);
        p.write(0x4000, 0);
        p.write(0x4003, 0x18); // Length2 ->1 only when mode1 reset arrives.
        p.write(0x4017, 0x80);
        assert_eq!(
            p.call("rt_source_apu_deadline", 0x71, 0x4796).hl(),
            delay as u16
        );
        for elapsed in 1..=delay {
            let event = p.tick();
            assert_eq!(event, if elapsed == delay { 3 } else { 0 });
            assert_eq!(p.bus.read(0xcb4e), if elapsed == delay { 1 } else { 2 });
        }
        assert_eq!(p.word(0xd504), 0);
        let mut events = Vec::new();
        for elapsed in 1..=37282 {
            let event = p.tick();
            if event != 0 {
                events.push((elapsed, event));
            }
        }
        assert_eq!(events, [(7457, 1), (14913, 3), (22371, 1), (37281, 3)]);
        assert_eq!(p.word(0xd504), 0);
        assert_eq!(p.status(0xff), 0x20);
        assert_eq!(p.bus.read(0xd501), 0);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_dmc_admission_and_event_collision_guards_are_explicit() {
    for (address, value, reason) in [
        (0x4015, 0x10, 1),
        (0x4011, 1, 1),
        (0x4016, 0, 4),
        (0x4014, 0, 4),
    ] {
        let mut p = ApuProbe::new();
        p.call("rt_source_apu_write", value, address);
        assert_eq!((p.bus.read(0xcb1d), p.bus.read(0xd51a)), (0xe9, reason));
    }
    let mut p = ApuProbe::new();
    for address in 0x4010..=0x4013 {
        p.write(address, 0);
    }
    p.bus.write(0xd51b, 4); // Explicit event-phase fixture, not arbitrary quiet read.
    p.call("rt_source_apu_read", 0xff, 0x4015);
    assert_eq!((p.bus.read(0xcb1d), p.bus.read(0xd51a)), (0xe9, 2));
    let mut p = ApuProbe::new();
    p.bus.write(0xd51b, 2); // Same-cycle half-clock length write.
    p.call("rt_source_apu_write", 0x18, 0x4003);
    assert_eq!((p.bus.read(0xcb1d), p.bus.read(0xd51a)), (0xe9, 3));
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_envelopes_triangle_reload_and_sweep_negate_are_literal() {
    let mut p = ApuProbe::new();
    p.write(0x4015, 15);
    for (address, value) in [
        (0x4000, 2),
        (0x4004, 0x20),
        (0x400c, 0x10),
        (0x4003, 0),
        (0x4007, 0),
        (0x400f, 0),
    ] {
        p.write(address, value);
    }
    for (quarter, expected) in [
        (1, [2, 15, 0, 15, 0, 15]),
        (2, [1, 15, 0, 14, 0, 14]),
        (3, [0, 15, 0, 13, 0, 13]),
        (4, [2, 14, 0, 12, 0, 12]),
    ] {
        p.call("rt_source_apu_quarter", 0, 0);
        assert_eq!(&p.bus.ram[0xb48..0xb4e], &expected, "quarter{quarter}");
    }
    for _ in 4..17 {
        p.call("rt_source_apu_quarter", 0, 0);
    }
    assert_eq!(&p.bus.ram[0xb48..0xb4e], &[1, 10, 0, 15, 0, 0]);

    let mut p = ApuProbe::new();
    p.write(0x4015, 4);
    p.write(0x4008, 3);
    p.write(0x400b, 0);
    for expected in [3, 2, 1, 0, 0] {
        p.call("rt_source_apu_quarter", 0, 0);
        assert_eq!(p.bus.read(0xcb55), expected);
        assert_eq!(p.bus.read(0xcb54) & 0x20, 0);
    }
    p.write(0x4008, 0x83);
    p.write(0x400b, 0);
    for _ in 0..4 {
        p.call("rt_source_apu_quarter", 0, 0);
        assert_eq!(p.bus.read(0xcb55), 3);
        assert_eq!(p.bus.read(0xcb54) & 0x20, 0x20);
    }
    p.write(0x4008, 2);
    for expected in [2, 1] {
        p.call("rt_source_apu_quarter", 0, 0);
        assert_eq!(p.bus.read(0xcb55), expected);
    }

    for (sweep, pulse1, pulse2) in [(0x89, 499u16, 500u16), (0x81, 1500, 1500)] {
        let mut p = ApuProbe::new();
        p.write(0x4015, 3);
        for base in [0x4000, 0x4004] {
            p.write(base, 0x1f);
            p.write(base + 2, 0xe8);
            p.write(base + 3, 3); // Period1000.
            p.write(base + 1, sweep); // Reload pending with divider0.
        }
        p.call("rt_source_apu_half", 0, 0);
        assert_eq!(p.word(0xcb32) & 0x7ff, pulse1);
        assert_eq!(p.word(0xcb36) & 0x7ff, pulse2);
        assert_eq!(p.bus.read(0xcb54) & 0x18, 0);
        assert_eq!(&p.bus.ram[0xb4e..0xb50], &[9, 9]);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU"]
fn assembled_apu_publish_is_timeless_with_continuous_sweep_mute_and_octave_fold() {
    let mut p = ApuProbe::new();
    p.write(0x4015, 3);
    for base in [0x4000, 0x4004] {
        p.write(base, 0x1f);
        p.write(base + 2, 0);
        p.write(base + 3, 4); // Period1024.
    }
    p.write(0x4001, 0); // Disabled positive shift0 still overflows target2048.
    p.write(0x4005, 8); // Disabled negative shift0 does not overflow.
    p.set_cycle(0x12345678);
    let units = p.bus.ram[0xb48..0xb56].to_vec();
    let sequence = p.bus.ram[0x1501..0x150d].to_vec();
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(p.bus.read(0xd513), 1);
    assert_eq!(&p.bus.ram[0xb5e..0xb60], &[15, 0]);
    assert_eq!(&p.bus.ram[0xb48..0xb56], units);
    assert_eq!(&p.bus.ram[0x1501..0x150d], sequence);
    assert_eq!(p.cycle(), 0x12345678);
    let psg_writes = p.bus.psg_writes;
    p.call("rt_source_apu_publish", 0x71, 0x4796);
    assert_eq!(
        p.bus.psg_writes, psg_writes,
        "clean publication writes nothing"
    );
    for (before, after) in [
        (0, 0),
        (1, 1),
        (1023, 1023),
        (1024, 512),
        (2047, 1023),
        (2048, 512),
    ] {
        assert_eq!(p.call("rt_source_apu_fold_tone", 0, before).hl(), after);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled source APU with cold-reset helper"]
fn assembled_apu_declared_cold_virtual_write_resets_at_cycle_one() {
    let mut p = ApuProbe::new();
    p.call("rt_source_apu_cold_reset", 0, 0);
    assert_eq!(p.call("rt_source_apu_deadline", 0x71, 0x4796).hl(), 1);
    assert_eq!(p.tick(), 0);
    assert_eq!(p.word(0xd504), 0);
    for _ in 1..7 {
        p.tick();
    }
    assert_eq!((p.cycle(), p.word(0xd504)), (7, 6));
    let mut events = Vec::new();
    while p.cycle() < 29831 {
        let event = p.tick();
        if event != 0 {
            events.push((p.cycle(), event));
        }
    }
    assert_eq!(
        events,
        [
            (7458, 1),
            (14914, 3),
            (22372, 1),
            (29829, 4),
            (29830, 7),
            (29831, 4)
        ]
    );
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled strict quiet-span helper"]
fn assembled_apu_quiet_spans_equal_precise_cycles_and_preserve_source_time() {
    // Each tuple is an independent legal interior: source-counter, sequencer
    // counter, next event, write-delay, duplicate-clock block, status-race
    // countdown, span. No event boundary may be summarized.
    for (cycle, counter, next, delay, block, recent, span) in [
        (0, 0, 7457, 0, 0, 0, 7456),
        (7457, 7457, 14913, 0, 0, 0, 7455),
        (u32::MAX - 3, 22371, 29828, 0, 0, 0, 17),
        (29829, 29829, 37281, 0, 0, 0, 7451),
        (100, 100, 7457, 3, 0, 0, 2),
        (101, 101, 7457, 4, 0, 0, 3),
        (100, 100, 7457, 0, 3, 0, 2),
        (100, 100, 7457, 0, 0, 2, 1),
        (100, 100, 7457, 4, 3, 2, 1),
    ] {
        let mut precise = ApuProbe::new();
        let mut quiet = ApuProbe::new();
        for p in [&mut precise, &mut quiet] {
            p.set_cycle(cycle);
            p.bus.ram[0x1504..0x1506].copy_from_slice(&(counter as u16).to_le_bytes());
            p.bus.ram[0x1506..0x1508].copy_from_slice(&(next as u16).to_le_bytes());
            p.bus.write(0xd509, delay);
            p.bus.write(0xd50b, block);
            p.bus.write(0xd50c, recent);
            p.bus.write(0xd51b, 0xa7); // A prior event marker must be cleared.
            p.bus.write(0xd501, 1); // Existing IRQ/dirty state must survive.
            p.bus.write(0xd500, 1);
            p.bus.write(0xd50d, 1);
        }
        let source = quiet.bus.ram[0xa80..0xb00].to_vec();
        for _ in 0..span {
            precise.tick();
        }
        quiet.call_abi(
            "rt_source_apu_quiet_advance",
            0x71,
            0x4796,
            span,
            span & 1 != 0,
        );
        assert_eq!(quiet.bus.read(0xcb1d), 0);
        assert_eq!(
            &quiet.bus.ram[0x1500..0x15e0],
            &precise.bus.ram[0x1500..0x15e0],
            "counter{counter} span{span}"
        );
        assert_eq!(&quiet.bus.ram[0xb30..0xb62], &precise.bus.ram[0xb30..0xb62]);
        assert_eq!(&quiet.bus.ram[0xa80..0xb00], source);
        assert_eq!(quiet.bus.psg_log, precise.bus.psg_log);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_APU_PROJECT assembled strict quiet-span helper"]
fn assembled_apu_quiet_zero_is_exact_and_deadline_crossings_reject_before_mutation() {
    for (counter, next, delay, block, recent, span, rejected) in [
        (7457u16, 7457u16, 0, 0, 0, 0, false), // Zero bypasses even invalid epoch.
        (0, 7457, 0, 0, 0, 7457, true),
        (0, 7457, 0, 0, 0, 7458, true),
        (100, 7457, 3, 0, 0, 3, true),
        (100, 7457, 0, 2, 0, 2, true),
        (100, 7457, 0, 0, 1, 1, true),
        (7458, 7457, 0, 0, 0, 1, true),
    ] {
        let mut p = ApuProbe::new();
        p.bus.ram[0x1504..0x1506].copy_from_slice(&counter.to_le_bytes());
        p.bus.ram[0x1506..0x1508].copy_from_slice(&next.to_le_bytes());
        p.bus.write(0xd509, delay);
        p.bus.write(0xd50b, block);
        p.bus.write(0xd50c, recent);
        p.bus.write(0xd51b, 0xa7);
        let before = p.bus.ram[0x1500..0x15e0].to_vec();
        let source = p.bus.ram[0xa80..0xb00].to_vec();
        p.call_bc("rt_source_apu_quiet_advance", 0x71, 0x4796, span);
        let mut expected = before;
        if rejected {
            expected[0x1a] = 5;
        }
        assert_eq!(p.bus.read(0xcb1d), if rejected { 0xe9 } else { 0 });
        assert_eq!(&p.bus.ram[0x1500..0x15e0], expected);
        assert_eq!(&p.bus.ram[0xa80..0xb00], source);
    }
}
