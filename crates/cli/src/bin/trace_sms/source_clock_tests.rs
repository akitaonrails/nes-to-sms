//! Actual assembled source-clock probes. No production timing model supplies
//! expected values. Test-only injected phases establish boundary/ABI behavior,
//! not a commercial game's reset state or a complete native playthrough.

use super::*;

const CYCLES: u16 = 0xca80;
const DOT: u16 = 0xca84;
const LINE: u16 = 0xca86;
const FRAME: u16 = 0xca88;

fn project(variable: &str) -> (Vec<u8>, HashMap<String, (u8, u16)>) {
    let path = PathBuf::from(std::env::var(variable).expect("assembled clock fixture path"));
    let rom = std::fs::read(path.join("sms.sms")).unwrap();
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    for label in [
        "rt_source_read_bus",
        "rt_source_bus_read_event",
        "rt_source_bus_write_event",
    ] {
        assert_eq!(defs[label].0, 0, "source event code must be fixed slot0");
    }
    (rom, defs)
}

fn put16(bus: &mut SmsBus, address: u16, value: u16) {
    for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
        bus.write(address + offset as u16, byte);
    }
}

fn put32(bus: &mut SmsBus, address: u16, value: u32) {
    for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
        bus.write(address + offset as u16, byte);
    }
}

fn read16(bus: &mut SmsBus, address: u16) -> u16 {
    u16::from_le_bytes([bus.read(address), bus.read(address + 1)])
}

fn read32(bus: &mut SmsBus, address: u16) -> u32 {
    u32::from_le_bytes(std::array::from_fn(|index| {
        bus.read(address + index as u16)
    }))
}

fn invoke(cpu: &mut Cpu, bus: &mut SmsBus, entry: u16) {
    cpu.pc = entry;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    for _ in 0..500_000 {
        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
            return;
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    panic!(
        "helper did not return or explicitly trap, PC={:04X}",
        cpu.pc
    );
}

fn cpu_for_read(bus: &mut SmsBus, address: u16) -> Cpu {
    put16(bus, 0xca9a, address); // Prepared effective address.
    bus.write(0xca97, 1); // One remaining semantic transfer in this direct probe.
    let mut cpu = Cpu::new();
    cpu.set_bc(0x1787);
    cpu.set_de(0x52a9);
    cpu.set_hl(address);
    cpu.a = 0x62;
    cpu.f = 0x95;
    cpu
}

#[test]
#[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled source-clock fixture"]
fn assembled_clock_wrap_preserves_ppu_edges_and_independent_frame_counter() {
    let (rom, defs) = project("TRACE_FUNCTIONAL_PROJECT");
    let mut bus = SmsBus::new(rom.clone(), 0xff);
    let mut cpu = cpu_for_read(&mut bus, 0x55);
    bus.write(0xc055, 0x5a);
    put32(&mut bus, CYCLES, u32::MAX - 1);
    put16(&mut bus, LINE, 240);
    put16(&mut bus, DOT, 338);
    bus.write(0xca97, 2);
    bus.write(0xca98, 2); // Accept NMI on the second read only.
    bus.write(0xcb08, 0x80);
    invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
    assert_eq!(cpu.pc, 7);
    assert_eq!(read32(&mut bus, CYCLES), u32::MAX);
    assert_eq!((read16(&mut bus, LINE), read16(&mut bus, DOT)), (241, 0));
    assert_eq!(bus.read(0xcaa3), 0);
    invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
    assert_eq!(cpu.pc, 7);
    assert_eq!(read32(&mut bus, CYCLES), 0);
    assert_eq!((read16(&mut bus, LINE), read16(&mut bus, DOT)), (241, 3));
    assert_eq!(
        (bus.read(0xcaa3), bus.read(0xcaa4), bus.read(0xcaa6)),
        (1, 1, 1)
    );
    assert_eq!(bus.read(0xcaa5), 0, "accepted edge consumed only once");
    assert_eq!(cpu.a, 0x5a);

    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = cpu_for_read(&mut bus, 0);
    put32(&mut bus, CYCLES, u32::MAX);
    put32(&mut bus, FRAME, u32::MAX);
    put16(&mut bus, LINE, 261);
    put16(&mut bus, DOT, 339);
    invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
    assert_eq!(cpu.pc, 7);
    assert_eq!((read32(&mut bus, CYCLES), read32(&mut bus, FRAME)), (0, 0));
    assert_eq!((read16(&mut bus, LINE), read16(&mut bus, DOT)), (0, 1));
}

#[test]
#[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled source-clock fixture"]
fn assembled_clock_dma_wrap_and_both_declared_alignments_preserve_abi() {
    let (rom, defs) = project("TRACE_FUNCTIONAL_PROJECT");
    for alignment in [0, 1] {
        for iff in [false, true] {
            for mapping in [0, 8, 12] {
                let mut bus = SmsBus::new(rom.clone(), 0xff);
                let mut cpu = cpu_for_read(&mut bus, 0x55);
                cpu.iff1 = iff;
                cpu.iff2 = iff;
                bus.write(0xc055, 0x5a);
                bus.write(0xc300, 0x12);
                bus.write(0xc3ff, 0x34);
                bus.write(0xcb0a, 0xf9);
                bus.write(0xcb03, 0xa5);
                bus.write(0xcaa8, 3);
                bus.write(0xcaa9, 1);
                bus.write(0xcaaa, alignment);
                put32(&mut bus, CYCLES, u32::MAX - 3);
                put16(&mut bus, LINE, 10);
                bus.write(0xfffe, 17);
                bus.write(0xcb14, 17);
                bus.write(0xffff, 23);
                bus.write(0xfffc, mapping);
                let maps = (bus.mapper_control, bus.slot_bank);
                invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
                assert_eq!(cpu.pc, 7, "alignment {alignment} IFF{iff} mapping{mapping}");
                // Starting C=FFFFFFFC: 514/513 stalls, then one actual read.
                let elapsed = if alignment == 0 { 515 } else { 514 };
                assert_eq!(read32(&mut bus, CYCLES), elapsed - 4);
                assert_eq!(
                    (read16(&mut bus, LINE), read16(&mut bus, DOT)),
                    (14, (elapsed * 3 - 1364) as u16)
                );
                assert_eq!(
                    (bus.read(0xc9f9), bus.read(0xc9f8), bus.read(0xcb0a)),
                    (0x12, 0x34, 0xf9)
                );
                assert_eq!((bus.read(0xcaa9), bus.read(0xcaab)), (0, 0));
                assert_eq!(
                    (cpu.bc(), cpu.de(), cpu.hl(), cpu.a),
                    (0x1787, 0x52a9, 0x55, 0x5a)
                );
                assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                assert_eq!(cpu.sp, 0xdff2);
                assert_eq!(bus.read(0xcb03), 0xa5);
                assert_eq!((bus.mapper_control, bus.slot_bank), maps);
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled source-clock fixture"]
fn assembled_status_collision_neighborhoods_are_explicit_rejections() {
    let (rom, defs) = project("TRACE_FUNCTIONAL_PROJECT");
    for (line, dots) in [
        (240u32, &[337u32, 338, 339, 340][..]),
        (241, &[0, 1, 2, 3, 4, 5][..]),
        (260, &[337, 338, 339, 340][..]),
        (261, &[0, 1, 2, 3, 4, 5][..]),
    ] {
        for &dot in dots {
            let rejected = if line == 240 || line == 260 {
                dot >= 338
            } else {
                dot < 5
            };
            let before = line * 341 + dot - 3;
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            let mut cpu = cpu_for_read(&mut bus, 0x2002);
            put16(&mut bus, LINE, (before / 341) as u16);
            put16(&mut bus, DOT, (before % 341) as u16);
            invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
            assert_eq!(read32(&mut bus, CYCLES), 1, "actual read phase is charged");
            assert_eq!(
                bus.read(0xcb1d),
                if rejected { 0xe8 } else { 0 },
                "PPU {line}:{dot}"
            );
            if !rejected {
                assert_eq!(cpu.pc, 7);
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled source-clock fixture"]
fn assembled_nmi_during_vector_entry_or_dma_is_guarded() {
    let (rom, defs) = project("TRACE_FUNCTIONAL_PROJECT");
    for active in [0xcaa7, 0xcaab] {
        let mut bus = SmsBus::new(rom.clone(), 0xff);
        let mut cpu = cpu_for_read(&mut bus, 0);
        put16(&mut bus, LINE, 240);
        put16(&mut bus, DOT, 340);
        bus.write(0xcb08, 0x80);
        bus.write(active, 1);
        invoke(&mut cpu, &mut bus, defs["rt_source_read_bus"].1);
        assert_eq!(bus.read(0xcb1d), 0xe8, "active marker {active:04X}");
        assert_eq!(read32(&mut bus, CYCLES), 1);
        assert_eq!(bus.read(0xcaa6), 0, "no fabricated handler acceptance");
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_RMW_PROJECT assembled rmw-ordered fixture"]
fn assembled_rmw_events_have_literal_original_and_final_cycle_timestamps() {
    let (rom, defs) = project("TRACE_CLOCK_RMW_PROJECT");
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    let mut events = Vec::new();
    for step in 0..1_500_000 {
        if bus.read(0xcb1d) != 0 {
            break;
        }
        let writing = cpu.pc == defs["rt_source_bus_write_event"].1;
        if writing || cpu.pc == defs["rt_source_bus_read_event"].1 {
            events.push((read32(&mut bus, CYCLES), writing, cpu.hl(), cpu.a));
        }
        cpu.step(&mut bus).unwrap();
        assert!(
            step < 1_499_999,
            "source fixture never reached terminal trap"
        );
    }
    assert_eq!(bus.read(0xc7ff), 0xa5);
    assert_eq!(bus.read(0xcb1d), 0xe8);
    assert_eq!(read32(&mut bus, CYCLES), 32);
    let reads: Vec<_> = events
        .iter()
        .filter(|(_, w, a, _)| !w && *a == 0xb801)
        .map(|(c, _, _, _)| *c)
        .collect();
    let writes: Vec<_> = events
        .iter()
        .filter(|(_, w, a, _)| *w && *a == 0xb801)
        .map(|(c, _, _, v)| (*c, *v))
        .collect();
    assert_eq!(reads, [10, 16]);
    assert_eq!(writes, [(11, 2), (12, 3), (17, 2), (18, 4)]);
    assert_eq!(events.len(), 32, "one tap per admitted source bus cycle");
    for (index, (cycle, _, _, _)) in events.iter().enumerate() {
        assert_eq!(*cycle, index as u32 + 1);
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_LOOP_PROJECT assembled nmi-restored-flags fixture"]
fn bounded_source_loop_reports_intrinsic_clock_cost_without_boot() {
    let (rom, defs) = project("TRACE_CLOCK_LOOP_PROJECT");
    let (bank, entry) = defs["L_8008"];
    let (_, after) = defs["L_800B"];
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    let mut start = None;
    let mut iterations = 0;
    for _ in 0..1_500_000 {
        assert_eq!(bus.read(0xcb1d), 0);
        if cpu.pc == entry && bus.slot_bank[1] == bank {
            if let Some((source_start, host_start)) = start {
                iterations += 1;
                if iterations == 64 {
                    let source_delta = read32(&mut bus, CYCLES) - source_start;
                    let host_delta = cpu.cycles - host_start;
                    assert_eq!(source_delta, 320, "64*(DEX2 + taken BNE3)");
                    println!(
                        "source_loop_cost: iterations=64 source_cycles={source_delta} z80_approx_cycles={host_delta} z80_per_source={:.3} emitted_loop_bytes={} boot_excluded=true host_irq_injection=false; NOT real SMS frame speed",
                        host_delta as f64 / f64::from(source_delta),
                        after - entry
                    );
                    return;
                }
            } else {
                start = Some((read32(&mut bus, CYCLES), cpu.cycles));
            }
        }
        cpu.step(&mut bus).unwrap();
    }
    panic!("did not finish bounded source loop measurement");
}
