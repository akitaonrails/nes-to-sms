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

fn large_directory_expected_pcs() -> std::collections::BTreeSet<u16> {
    // Literal source layout from large_directory_fixture, not decoded output.
    let mut pcs = std::collections::BTreeSet::from([
        0x8000, 0x8002, 0x8003, 0x8005, 0x806c, 0x806f, 0x8071, 0x8074, 0x8077, 0xc000, 0xc002,
        0xc004, 0xc006,
    ]);
    pcs.extend(0x8008..=0x806b);
    for page in 0x81..=0x9fu16 {
        let head = page << 8;
        pcs.extend(head..head + 100);
        pcs.insert(head + 100);
        if page == 0x9f {
            pcs.extend([head + 102, head + 104, head + 106, head + 108]);
        } else {
            pcs.insert(head + 103);
        }
    }
    assert_eq!(pcs.len(), 3278);
    pcs
}

#[test]
#[ignore = "requires TRACE_CLOCK_LARGE_PROJECT assembled directory-large fixture"]
fn banked_directory_preserves_every_literal_source_pc_and_real_terminator() {
    let (rom, defs) = project("TRACE_CLOCK_LARGE_PROJECT");
    let (bank, directory) = defs["rt_dispatch_page_table"];
    assert_eq!(bank, 0);
    assert_eq!(defs["rt_dispatch_directory_end"], (0, directory + 384));
    let mut actual = std::collections::BTreeSet::new();
    let mut record_banks = std::collections::BTreeSet::new();
    for page in 0x80..=0xffu16 {
        let index = usize::from(directory + 3 * (page - 0x80));
        let bank = rom[index];
        let mut address = u16::from_le_bytes([rom[index + 1], rom[index + 2]]);
        assert_eq!(
            defs[&format!("rt_dispatch_page_{page:02X}")],
            (bank, address)
        );
        record_banks.insert(bank);
        for entry in 0..=256 {
            assert!((0x4000..=0x7ffe).contains(&address));
            let offset = usize::from(bank) * 0x4000 + usize::from(address - 0x4000);
            let source = u16::from_le_bytes([rom[offset], rom[offset + 1]]);
            if source == 0 {
                break; // Real two-byte terminator, including empty pages.
            }
            assert!(
                entry < 256,
                "one fixed-PRG high-byte group cannot exceed256 PCs"
            );
            assert!(address <= 0x7ffa, "record must not straddle its bank");
            assert_eq!(source >> 8, page);
            assert!(actual.insert(source), "no duplicate source keys");
            assert_eq!(rom[offset + 2], 0xff, "fixed PRG has no CHR-bank key");
            let destination = (
                rom[offset + 3],
                u16::from_le_bytes([rom[offset + 4], rom[offset + 5]]),
            );
            assert_eq!(defs[&format!("L_{source:04X}")], destination);
            assert!(usize::from(destination.0) * 0x4000 < rom.len());
            address += 6;
        }
    }
    assert!(
        record_banks.len() >= 2,
        "fixture must actually cross record banks"
    );
    assert_eq!(actual, large_directory_expected_pcs());
}

#[test]
#[ignore = "requires TRACE_CLOCK_LARGE_PROJECT assembled directory-large fixture"]
fn banked_dispatch_hits_distant_decoded_pcs_and_strictly_rejects_missing_ones() {
    let (rom, defs) = project("TRACE_CLOCK_LARGE_PROJECT");
    // First/last decoded boundaries in near/far groups plus actual NMI entry.
    for target in [
        0x8000, 0x8077, 0x9900, 0x9967, 0x9f00, 0x9f6c, 0xc000,
        0x806d, // JSR operand: populated page, but not a source instruction.
        0xa000, // Entire high-byte group empty.
    ] {
        for iff in [false, true] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            let mut cpu = Cpu::new();
            cpu.pc = defs["rt_banked_tail_dispatch"].1;
            cpu.sp = 0xdff0;
            cpu.a = 0x62;
            cpu.set_de(0x52a9);
            cpu.set_bc(target);
            cpu.iff1 = iff;
            cpu.iff2 = iff;
            bus.write(0xcb7e, u8::from(!iff)); // Existing tail-dispatch IFF policy.
            bus.write(0xcb02, 0x3f);
            bus.write(0xcb03, 0xa5);
            put16(&mut bus, 0xcb76, 0x725a); // No TR_RET frame may be allocated.
            bus.write(0xfffe, 17);
            bus.write(0xcb14, 17);
            bus.write(0xffff, 23);
            bus.write(0xfffc, 12);
            let expected = defs.get(&format!("L_{target:04X}")).copied();
            for step in 0..20_000 {
                if bus.read(0xcb1d) != 0
                    || expected.is_some_and(|(bank, pc)| bus.slot_bank[1] == bank && cpu.pc == pc)
                {
                    break;
                }
                cpu.step(&mut bus).unwrap();
                assert!(step < 19_999, "dispatch did not resolve/trap ${target:04X}");
            }
            assert_eq!(bus.slot_bank[2], 23);
            assert_eq!(bus.mapper_control, 12);
            assert_eq!(cpu.de(), 0x52a9);
            assert_eq!(cpu.sp, 0xdff0);
            assert_eq!(bus.read(0xcb02), 0x3f);
            assert_eq!(bus.read(0xcb03), 0xa5);
            assert_eq!(read16(&mut bus, 0xcb76), 0x725a);
            if let Some((bank, pc)) = expected {
                assert_eq!((bus.slot_bank[1], cpu.pc), (bank, pc));
                assert_eq!(bus.read(0xcb14), bank);
                assert_eq!(cpu.a, 0x62);
                assert_eq!(cpu.iff1, iff);
                assert_eq!(bus.read(0xcb1d), 0);
            } else {
                assert_eq!(bus.read(0xcb1d), 0xe2);
                assert_eq!(read16(&mut bus, 0xcb1b), target);
                assert_eq!(bus.slot_bank[1], 17, "strict miss restores code mapping");
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct SourceTransfer(u32, bool, u16, u8);

struct LoopRun {
    transfers: Vec<SourceTransfer>,
    ram: Vec<u8>,
    source: Vec<u8>,
    guest: (u8, u16, u8, u8),
    coordinates: (u32, u16, u16, u32),
    spans: usize,
    approximate_z80: u64,
    host_elapsed: std::time::Duration,
}

fn snapshot(bus: &mut SmsBus, start: u16, end: u16) -> Vec<u8> {
    (start..end).map(|a| bus.read(a)).collect()
}

fn loop_entry(variable: &str, visit: usize) -> (Cpu, SmsBus, HashMap<String, (u8, u16)>) {
    let (rom, defs) = project(variable);
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    let (bank, pc) = defs["L_8100"];
    let mut found = 0;
    for step in 0..1_000_000 {
        if cpu.pc == pc && bus.slot_bank[1] == bank {
            found += 1;
            if found == visit {
                return (cpu, bus, defs);
            }
        }
        assert_eq!(bus.read(0xcb1d), 0);
        cpu.step(&mut bus).unwrap();
        assert!(step < 999_999, "did not reach original loop head");
    }
    unreachable!()
}

fn observe_loop(mut cpu: Cpu, mut bus: SmsBus, defs: &HashMap<String, (u8, u16)>) -> LoopRun {
    let (stop_bank, stop_pc) = defs["L_8104"];
    let read_tap = defs["rt_source_bus_read_event"].1;
    let write_tap = defs["rt_source_bus_write_event"].1;
    let begin_tap = defs["rt_source_quiet_span_begin"].1;
    let end_tap = defs["rt_source_quiet_span_end"].1;
    let mut transfers = Vec::new();
    let mut pending_read = None;
    let mut span_state = None;
    let mut spans = 0;
    let host_start = std::time::Instant::now();
    let z80_start = cpu.cycles;
    for step in 0..8_000_000 {
        if let Some((return_pc, return_sp, cycle, address)) = pending_read
            && cpu.pc == return_pc
            && cpu.sp == return_sp
        {
            transfers.push(SourceTransfer(cycle, false, address, bus.read(0xcaae)));
            pending_read = None;
        }
        if cpu.pc == stop_pc && bus.slot_bank[1] == stop_bank {
            break;
        }
        assert_eq!(bus.read(0xcb1d), 0, "unexpected source trap");
        if cpu.pc == read_tap {
            assert!(pending_read.is_none());
            pending_read = Some((
                read16(&mut bus, cpu.sp),
                cpu.sp + 2,
                read32(&mut bus, CYCLES),
                cpu.hl(),
            ));
        } else if cpu.pc == write_tap {
            assert!(pending_read.is_none());
            transfers.push(SourceTransfer(
                read32(&mut bus, CYCLES),
                true,
                cpu.hl(),
                cpu.a,
            ));
        } else if cpu.pc == begin_tap {
            assert!(pending_read.is_none() && span_state.is_none());
            let start = read32(&mut bus, 0xcabe);
            let count = read16(&mut bus, 0xcac2);
            let head = read16(&mut bus, 0xcac4);
            let zp = bus.read(0xcac6);
            let value = bus.read(0xcac7);
            assert_eq!((head, zp), (0x8100, 0x20));
            assert_ne!(value, 0);
            assert_eq!(value, bus.read(0xc020));
            assert!((1..=1820).contains(&count));
            assert_eq!(read16(&mut bus, 0xcac9), count * 6);
            assert_eq!(
                bus.read(0xcac8),
                0xa9,
                "last branch dummy read is next opcode"
            );
            assert_eq!(read32(&mut bus, CYCLES), start);
            for iteration in 0..u32::from(count) {
                // Expand literal original reads, never production template logic.
                for (index, (address, byte)) in [
                    (0x8100, 0xa5),
                    (0x8101, 0x20),
                    (0x0020, value),
                    (0x8102, 0xd0),
                    (0x8103, 0xfc),
                    (0x8104, 0xa9),
                ]
                .into_iter()
                .enumerate()
                {
                    transfers.push(SourceTransfer(
                        start.wrapping_add(iteration * 6 + index as u32 + 1),
                        false,
                        address,
                        byte,
                    ));
                }
            }
            let dots = u32::from(read16(&mut bus, LINE)) * 341 + u32::from(read16(&mut bus, DOT));
            let elapsed = u32::from(count) * 18;
            // No declared PPU guard-window start can be crossed by a summary.
            for boundary in [240 * 341 + 338, 260 * 341 + 338] {
                let distance = (boundary + 89342 - dots) % 89342;
                assert!(distance > elapsed, "span crosses/lands on guarded boundary");
            }
            span_state = Some((
                snapshot(&mut bus, 0xca8c, 0xcabe),
                start,
                dots + elapsed,
                read32(&mut bus, FRAME),
                count,
                snapshot(&mut bus, 0xc000, 0xc800),
                bus.read(0xcb03),
            ));
            spans += 1;
        } else if cpu.pc == end_tap
            && let Some((state, start, dots, frame, count, ram, p)) = span_state.take()
        {
            assert_eq!(
                snapshot(&mut bus, 0xca8c, 0xcabe),
                state,
                "entire old branch state preserved"
            );
            assert_eq!(
                snapshot(&mut bus, 0xc000, 0xc800),
                ram,
                "summary cannot write guest memory"
            );
            assert_eq!(bus.read(0xcb03), p);
            assert_eq!(
                read32(&mut bus, CYCLES),
                start.wrapping_add(u32::from(count) * 6)
            );
            assert_eq!(read32(&mut bus, FRAME), frame.wrapping_add(dots / 89342));
            assert_eq!(read16(&mut bus, LINE), ((dots % 89342) / 341) as u16);
            assert_eq!(read16(&mut bus, DOT), (dots % 341) as u16);
        }
        cpu.step(&mut bus).unwrap();
        assert!(step < 7_999_999, "wait did not exit");
    }
    assert!(pending_read.is_none() && span_state.is_none());
    let elapsed = host_start.elapsed();
    // Native descriptor resume pointer differs across emitted off/on layouts.
    // Compare all remaining prior-instruction bytes, including effective/fetch,
    // bus/result, interrupt, DMA and guest-target state.
    let source = (0xca8c..0xcabe)
        .filter(|a| ![0xcab2, 0xcab3].contains(a))
        .map(|a| bus.read(a))
        .collect();
    LoopRun {
        transfers,
        ram: snapshot(&mut bus, 0xc000, 0xca00),
        source,
        guest: (cpu.a, cpu.de(), bus.read(0xcb02), bus.read(0xcb03)),
        coordinates: (
            read32(&mut bus, CYCLES),
            read16(&mut bus, LINE),
            read16(&mut bus, DOT),
            read32(&mut bus, FRAME),
        ),
        spans,
        approximate_z80: cpu.cycles - z80_start,
        host_elapsed: elapsed,
    }
}

fn assert_loop_equivalent(off: &LoopRun, on: &LoopRun) {
    assert_eq!(
        off.transfers.len(),
        on.transfers.len(),
        "summaries cannot hide reads"
    );
    for (index, (left, right)) in off.transfers.iter().zip(&on.transfers).enumerate() {
        assert_eq!(left, right, "source transfer {index}");
    }
    assert_eq!(off.ram, on.ram);
    assert_eq!(off.source, on.source);
    assert_eq!(off.guest, on.guest);
    assert_eq!(off.coordinates, on.coordinates);
    assert_eq!(
        off.spans, 0,
        "off mode must be a precise executable reference"
    );
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_OFF_PROJECT and TRACE_CLOCK_QUIET_ON_PROJECT"]
fn quiet_wait_expands_every_transfer_and_measures_matched_cost() {
    let (cpu, bus, defs) = loop_entry("TRACE_CLOCK_QUIET_OFF_PROJECT", 1);
    let off = observe_loop(cpu, bus, &defs);
    let (cpu, bus, defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 1);
    let on = observe_loop(cpu, bus, &defs);
    assert_loop_equivalent(&off, &on);
    assert!(on.spans > 0, "test must actually execute accelerated spans");
    assert_eq!(off.coordinates.0, 27546);
    assert_eq!(
        off.transfers.len(),
        27524,
        "start C22, stop before terminal C27546"
    );
    println!(
        "finite_wait_cost: source_cycles=27524 precise_z80_approx={} accelerated_z80_approx={} ratio={:.3} precise_host_ms={:.3} accelerated_host_ms={:.3} expanded_transfers={} spans={} boot_excluded=true host_irq_injection=false; NOT SMS FPS",
        off.approximate_z80,
        on.approximate_z80,
        off.approximate_z80 as f64 / on.approximate_z80 as f64,
        off.host_elapsed.as_secs_f64() * 1000.0,
        on.host_elapsed.as_secs_f64() * 1000.0,
        on.transfers.len(),
        on.spans
    );
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_OFF_PROJECT and TRACE_CLOCK_QUIET_ON_PROJECT"]
fn quiet_wait_deadline_poll_positions_keep_literal_nmi_bus_timeline() {
    let (off_cpu, off_bus, off_defs) = loop_entry("TRACE_CLOCK_QUIET_OFF_PROJECT", 2);
    let (on_cpu, on_bus, on_defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 2);
    for position in 1..=6u32 {
        // Ten whole iterations followed by each position in the six-read loop.
        let before = 241 * 341 + 1 - 3 * (60 + position);
        let run = |cpu, mut bus: SmsBus, defs: &HashMap<String, (u8, u16)>| {
            put16(&mut bus, LINE, (before / 341) as u16);
            put16(&mut bus, DOT, (before % 341) as u16);
            observe_loop(cpu, bus, defs)
        };
        let off = run(off_cpu, off_bus.clone(), &off_defs);
        let on = run(on_cpu, on_bus.clone(), &on_defs);
        assert_loop_equivalent(&off, &on);
        assert!(
            on.spans > 0,
            "position{position} must exercise a preceding summary"
        );
        let (retired, low_pc, elapsed) = match position {
            1 | 2 => (63, 2, 98),
            3 | 4 => (66, 0, 98),
            5 | 6 => (69, 2, 104),
            _ => unreachable!(),
        };
        assert_eq!(off.coordinates.0, 28 + elapsed);
        let stack: Vec<_> = off
            .transfers
            .iter()
            .filter(|event| event.1 && (0x100..0x200).contains(&event.2))
            .collect();
        assert_eq!(
            stack,
            [
                &SourceTransfer(28 + retired + 3, true, 0x13f, 0x81),
                &SourceTransfer(28 + retired + 4, true, 0x13e, low_pc),
                &SourceTransfer(28 + retired + 5, true, 0x13d, 0x24),
            ],
            "NMI entry/poll position{position}"
        );
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_OFF_PROJECT and TRACE_CLOCK_QUIET_ON_PROJECT"]
fn quiet_wait_wrap_pending_dma_and_latched_interrupts_match_precise_execution() {
    let (off_cpu, off_bus, off_defs) = loop_entry("TRACE_CLOCK_QUIET_OFF_PROJECT", 2);
    let (on_cpu, on_bus, on_defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 2);
    for case in 0..6 {
        let run = |cpu, mut bus: SmsBus, defs: &HashMap<String, (u8, u16)>| {
            match case {
                0 => {
                    put32(&mut bus, CYCLES, u32::MAX - 30);
                    let before = 241 * 341 + 1 - 3 * 62;
                    put16(&mut bus, LINE, (before / 341) as u16);
                    put16(&mut bus, DOT, (before % 341) as u16);
                }
                1 => {
                    put32(&mut bus, CYCLES, u32::MAX - 30);
                    put32(&mut bus, FRAME, u32::MAX);
                    put16(&mut bus, LINE, 261);
                    put16(&mut bus, DOT, 5);
                }
                2 | 3 => {
                    put16(&mut bus, LINE, 230);
                    put16(&mut bus, DOT, 0);
                    bus.write(0xcaa8, 3);
                    bus.write(0xcaa9, 1);
                    bus.write(0xcaaa, case - 2);
                    bus.write(0xc300, 0x12);
                    bus.write(0xc3ff, 0x34);
                    bus.write(0xcb0a, 0xf9);
                }
                4 | 5 => {
                    put16(&mut bus, LINE, 241);
                    put16(&mut bus, DOT, 10);
                    bus.write(0xcaa3, 1);
                    bus.write(0xcaa4, 1);
                    bus.write(if case == 4 { 0xcaa5 } else { 0xcaa6 }, 1);
                }
                _ => unreachable!(),
            }
            observe_loop(cpu, bus, defs)
        };
        let off = run(off_cpu, off_bus.clone(), &off_defs);
        let on = run(on_cpu, on_bus.clone(), &on_defs);
        assert_loop_equivalent(&off, &on);
        match case {
            0 => assert_eq!(on.coordinates.0, 67, "98 cycles wrap FFFFFFE1 to43"),
            1 => assert_eq!(on.coordinates.3, 0, "independent frame wrap"),
            2 | 3 => {
                let oam: Vec<_> = on
                    .transfers
                    .iter()
                    .filter(|event| event.1 && event.2 == 0x2004)
                    .collect();
                assert_eq!(oam.len(), 256);
                assert_eq!((oam[0].3, oam[255].3), (0x12, 0x34));
            }
            4 | 5 => assert_eq!(on.spans, 0, "pending interrupt cannot be summarized away"),
            _ => unreachable!(),
        }
    }
}

fn invoke_poll_loop(cpu: &mut Cpu, bus: &mut SmsBus, defs: &HashMap<String, (u8, u16)>) {
    // Physical inline descriptor in inactive SMS staging RAM, not a guest I/O
    // pseudo-register or a source-code patch. Stop after the helper returns.
    put16(bus, 0xd000, 0x8100);
    bus.write(0xd002, 0x20);
    cpu.sp = 0xdff0;
    put16(bus, cpu.sp, 0xd000);
    cpu.pc = defs["rt_source_poll_loop"].1;
    for step in 0..50_000 {
        if cpu.pc == 0xd003 {
            return;
        }
        assert_eq!(bus.read(0xcb1d), 0);
        cpu.step(bus).unwrap();
        assert!(step < 49_999, "poll helper did not return");
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_ON_PROJECT"]
fn quiet_helper_stops_before_literal_guard_limits_and_preserves_branch_abi() {
    let (original_cpu, original_bus, defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 2);
    for (line, dot, iterations, end_line, end_dot, frames) in [
        (240, 318, 1, 240, 336, 0),
        (240, 319, 1, 240, 337, 0),
        (240, 320, 0, 240, 320, 0),
        (240, 337, 0, 240, 337, 0),
        (240, 338, 0, 240, 338, 0),
        (240, 340, 0, 240, 340, 0),
        (241, 0, 0, 241, 0, 0),
        (241, 4, 0, 241, 4, 0),
        (241, 5, 378, 260, 330, 0),
        (260, 318, 1, 260, 336, 0),
        (260, 319, 1, 260, 337, 0),
        (260, 320, 0, 260, 320, 0),
        (260, 338, 0, 260, 338, 0),
        (261, 0, 0, 261, 0, 0),
        (261, 4, 0, 261, 4, 0),
        (261, 5, 1820, 95, 29, 1),
        (100, 0, 1820, 196, 24, 0),
        (144, 340, 1818, 240, 328, 0),
    ] {
        let mut cpu = original_cpu;
        let mut bus = original_bus.clone();
        put16(&mut bus, LINE, line);
        put16(&mut bus, DOT, dot);
        put32(&mut bus, CYCLES, u32::MAX - 2);
        put32(&mut bus, FRAME, u32::MAX);
        let previous = snapshot(&mut bus, 0xca8c, 0xcabe);
        let mapping = (bus.mapper_control, bus.slot_bank);
        invoke_poll_loop(&mut cpu, &mut bus, &defs);
        assert_eq!(
            read32(&mut bus, CYCLES),
            (u32::MAX - 2).wrapping_add(iterations * 6),
            "PPU{line}:{dot}"
        );
        assert_eq!(
            (read16(&mut bus, LINE), read16(&mut bus, DOT)),
            (end_line, end_dot)
        );
        assert_eq!(read32(&mut bus, FRAME), u32::MAX.wrapping_add(frames));
        assert_eq!(snapshot(&mut bus, 0xca8c, 0xcabe), previous);
        assert_eq!(
            (
                cpu.a,
                cpu.f,
                cpu.bc(),
                cpu.de(),
                cpu.hl(),
                cpu.iff1,
                cpu.iff2
            ),
            (
                original_cpu.a,
                original_cpu.f,
                original_cpu.bc(),
                original_cpu.de(),
                original_cpu.hl(),
                original_cpu.iff1,
                original_cpu.iff2
            )
        );
        assert_eq!(cpu.sp, 0xdff2);
        assert_eq!((bus.mapper_control, bus.slot_bank), mapping);
        assert_eq!(bus.read(0xcb03), 0x24);
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_ON_PROJECT"]
fn quiet_helper_rejects_every_pending_or_unproved_state_without_advancing_time() {
    let (original_cpu, original_bus, defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 2);
    for (address, value) in [
        (0xcaa5, 1),
        (0xcaa6, 1),
        (0xcaa7, 1),
        (0xcaa9, 1),
        (0xcaab, 1),
        (0xcb09, 8),
        (0xcb09, 16),
        (0xc020, 0),
        (0xc020, 2),
        (0xcb03, 0xa4), // A1 but inconsistent N1: alternate-entry guard.
        (0xca96, 2),
        (0xca97, 4),
        (0xca8e, 0xea),
        (0xca8f, 0xfb),
        (0xcab0, 0),
        (0xca8c, 0),
    ] {
        let mut cpu = original_cpu;
        let mut bus = original_bus.clone();
        bus.write(address, value);
        let before = snapshot(&mut bus, 0xca80, 0xcabe);
        invoke_poll_loop(&mut cpu, &mut bus, &defs);
        assert_eq!(
            snapshot(&mut bus, 0xca80, 0xcabe),
            before,
            "guard${address:04X}={value:02X}"
        );
    }
}

#[test]
#[ignore = "requires TRACE_CLOCK_QUIET_OFF_PROJECT and TRACE_CLOCK_QUIET_ON_PROJECT; run negative/alternate/zero variants"]
fn quiet_wait_variant_transfers_match_without_erasing_loop_exit_or_flag_changes() {
    let (cpu, mut bus, defs) = loop_entry("TRACE_CLOCK_QUIET_OFF_PROJECT", 1);
    let value = bus.read(0xc020);
    let off = observe_loop(cpu, bus, &defs);
    let (cpu, bus, defs) = loop_entry("TRACE_CLOCK_QUIET_ON_PROJECT", 1);
    let on = observe_loop(cpu, bus, &defs);
    assert_loop_equivalent(&off, &on);
    assert_eq!(on.spans == 0, value == 0);
}

#[test]
#[ignore = "requires TRACE_CLOCK_DENSE_PROJECT assembled directory-dense fixture"]
fn full_256_entry_page_and_high_code_bank_dispatch_remain_addressable() {
    let (rom, defs) = project("TRACE_CLOCK_DENSE_PROJECT");
    assert!(
        defs["L_BF00"].0 >= 36,
        "executed target must use extended code placement"
    );
    let directory = usize::from(defs["rt_dispatch_page_table"].1);
    let index = directory + 3 * (0xe0 - 0x80);
    let bank = rom[index];
    let address = u16::from_le_bytes([rom[index + 1], rom[index + 2]]);
    let base = usize::from(bank) * 0x4000 + usize::from(address - 0x4000);
    assert!(
        address + 256 * 6 + 2 <= 0x8000,
        "complete page remains in one bank"
    );
    for index in 0..256u16 {
        let offset = base + usize::from(index) * 6;
        assert_eq!(
            u16::from_le_bytes([rom[offset], rom[offset + 1]]),
            0xe000 + index
        );
    }
    assert_eq!(&rom[base + 256 * 6..base + 256 * 6 + 2], &[0, 0]);
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    cpu.pc = defs["rt_banked_tail_dispatch"].1;
    cpu.sp = 0xdff0;
    cpu.set_bc(0xe0ff);
    cpu.a = 0x55;
    let (bank, target) = defs["L_E0FF"];
    for step in 0..30_000 {
        if cpu.pc == target && bus.slot_bank[1] == bank {
            break;
        }
        assert_eq!(bus.read(0xcb1d), 0, "256th record cannot be truncated");
        cpu.step(&mut bus).unwrap();
        assert!(step < 29_999, "last full-page record did not dispatch");
    }
    assert_eq!(
        bus.read(0xcb7d),
        0,
        "8-bit diagnostic scan counter wraps, lookup does not"
    );
    assert_eq!(cpu.a, 0x55);
    assert_eq!(cpu.sp, 0xdff0);
}
