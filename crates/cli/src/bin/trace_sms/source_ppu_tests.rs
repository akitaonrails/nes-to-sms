//! Independent literal PPU dot/fetch expectations. Direct state setup is a
//! peripheral fixture, not a translated game frame or source CPU oracle.

use super::*;

#[derive(Debug, PartialEq, Eq)]
struct DomainBusEvent {
    cycle: u32,
    completed: bool,
    write: bool,
    address: u16,
    value: u8,
    external: u8,
    position: (u16, u16),
    // Independent reconstruction uses only the declared event-free interval:
    // three PPU dots and one sequencer count per pending source CPU cycle.
    domains: Vec<u8>,
}

fn domain_word(bus: &SmsBus, address: usize) -> u16 {
    u16::from_le_bytes(
        bus.ram[address - 0xc000..address - 0xbffe]
            .try_into()
            .unwrap(),
    )
}

fn domain_cycle(bus: &SmsBus) -> u32 {
    u32::from_le_bytes(bus.ram[0xa80..0xa84].try_into().unwrap())
}

fn virtual_domains(bus: &SmsBus, deferred: bool) -> ((u16, u16), Vec<u8>) {
    let pending = if deferred {
        domain_word(bus, 0xd380)
    } else {
        0
    };
    let dot = domain_word(bus, 0xca84) + 3 * pending;
    assert!(dot <= 340, "deferred span crossed a scanline");
    let mut domains = bus.ram[0x1300..0x1380].to_vec();
    if pending != 0 {
        assert_eq!(bus.ram[0xb09] & 0x18, 0, "only blank PPU may defer");
        assert_eq!(domains[0x1a], 0, "pending loopy load is not quiet");
        domains[5] = 0; // SP_ACTIVE, cleared by every precise blank cycle.
        domains[0x30] = 0; // SP_ODD_SKIP, likewise.
    }
    let mut apu = bus.ram[0x1500..0x15ee].to_vec();
    if pending != 0 {
        let counter = u16::from_le_bytes([apu[4], apu[5]]) + pending;
        assert!(counter < u16::from_le_bytes([apu[6], apu[7]]));
        apu[4..6].copy_from_slice(&counter.to_le_bytes());
        for offset in [9, 11, 12] {
            if apu[offset] != 0 {
                assert!(u16::from(apu[offset]) > pending, "countdown event crossed");
                apu[offset] -= pending as u8;
            }
        }
        apu[0x1b] = 0; // No quarter/half/IRQ event exists inside this span.
    }
    domains.extend_from_slice(&apu);
    domains.extend_from_slice(&bus.ram[0x1400..0x140b]);
    domains.extend_from_slice(&bus.ram[0xb30..0xb62]);
    ((domain_word(bus, 0xca86), dot), domains)
}

#[test]
#[ignore = "requires TRACE_CNROM_DOMAIN_PRECISE and TRACE_CNROM_DOMAIN_DEFERRED paired source projects"]
fn assembled_deferred_domains_preserve_every_source_bus_event_and_logical_state() {
    let mut results = Vec::new();
    for (env, deferred) in [
        ("TRACE_CNROM_DOMAIN_PRECISE", false),
        ("TRACE_CNROM_DOMAIN_DEFERRED", true),
    ] {
        let project = PathBuf::from(std::env::var(env).unwrap());
        let defs = load_wla_symbol_defs(&project.join("sms.sym"));
        let mut bus = SmsBus::new(std::fs::read(project.join("sms.sms")).unwrap(), 0xff);
        let mut cpu = Cpu::new();
        let mut events = Vec::new();
        let mut pending_read = None;
        let mut active_span = None;
        let mut spans = 0;
        let mut publication = Vec::new();
        let started = std::time::Instant::now();
        for _ in 0..70_000_000 {
            if bus.read(0xcb1d) != 0 {
                break;
            }
            if let Some((return_pc, cycle, address, position, domains)) = pending_read.take() {
                if cpu.pc == return_pc {
                    events.push(DomainBusEvent {
                        cycle,
                        completed: true,
                        write: false,
                        address,
                        value: cpu.a,
                        external: bus.read(0xcaae),
                        position,
                        domains,
                    });
                } else {
                    pending_read = Some((return_pc, cycle, address, position, domains));
                }
            }
            if cpu.pc == defs["rt_source_bus_read_event"].1
                || cpu.pc == defs["rt_source_bus_write_event"].1
            {
                let address = cpu.hl();
                let write = cpu.pc == defs["rt_source_bus_write_event"].1;
                let (position, domains) = virtual_domains(&bus, deferred);
                if write {
                    events.push(DomainBusEvent {
                        cycle: domain_cycle(&bus),
                        completed: true,
                        write,
                        address,
                        value: cpu.a,
                        external: bus.read(0xcaae),
                        position,
                        domains,
                    });
                } else {
                    assert!(pending_read.is_none());
                    let ret = u16::from_le_bytes([bus.read(cpu.sp), bus.read(cpu.sp + 1)]);
                    pending_read = Some((ret, domain_cycle(&bus), address, position, domains));
                }
            }
            // The outer event label precedes its flush prologue. Check the
            // actual CPU-bus callee, not the entry to that prologue.
            if deferred
                && (cpu.pc == defs["rt_cpu_read_bus"].1 || cpu.pc == defs["rt_cpu_write_bus"].1)
            {
                let address = cpu.hl();
                let write = cpu.pc == defs["rt_cpu_write_bus"].1;
                if (0x2000..0x8000).contains(&address) || (write && address >= 0x2000) {
                    assert_eq!(
                        domain_word(&bus, 0xd380),
                        0,
                        "actual hardware transfer before flush"
                    );
                }
            }
            for label in [
                "rt_cnrom_packet_capture",
                "rt_cnrom_packet_validate_complete",
                "rt_cnrom_packet_committed",
            ] {
                if cpu.pc == defs[label].1 {
                    if deferred {
                        assert_eq!(domain_word(&bus, 0xd380), 0);
                    }
                    publication.push((label, domain_cycle(&bus)));
                }
            }
            if deferred && cpu.pc == defs["rt_source_domain_span_begin"].1 {
                assert!(active_span.is_none());
                let count = domain_word(&bus, 0xd388);
                let start = u32::from_le_bytes(bus.ram[0x1384..0x1388].try_into().unwrap());
                assert!(count > 0);
                assert_eq!(start.wrapping_add(u32::from(count)), domain_cycle(&bus));
                assert_eq!(count, domain_word(&bus, 0xd380));
                assert_eq!(domain_word(&bus, 0xd38a), domain_word(&bus, 0xca84));
                assert_eq!(domain_word(&bus, 0xd38c), domain_word(&bus, 0xca86));
                active_span = Some(virtual_domains(&bus, true));
            }
            if deferred && cpu.pc == defs["rt_source_domain_span_end"].1 {
                assert_eq!(domain_word(&bus, 0xd380), 0);
                assert_eq!(active_span.take().unwrap(), virtual_domains(&bus, false));
                spans += 1;
            }
            if active_span.is_some() {
                for label in [
                    "rt_source_ppu_fetch_address_ready",
                    "rt_source_ppu_fetch_read",
                    "rt_cnrom_packet_capture",
                    "rt_cnrom_packet_validate_complete",
                    "rt_source_apu_cycle",
                ] {
                    assert_ne!(cpu.pc, defs[label].1, "quiet summary contained {label}");
                }
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(bus.read(0xcb1d), 0xe8, "finite intentional endpoint");
        assert_eq!(bus.read(0xc7ff), 0xa5);
        // Unsupported final LDA$5000 transfers never return a read value.
        // Retain that attempted access explicitly instead of dropping it.
        let (_, cycle, address, position, domains) = pending_read.take().unwrap();
        assert_eq!(address, 0x5000);
        assert_eq!(cycle, domain_cycle(&bus));
        events.push(DomainBusEvent {
            cycle,
            completed: false,
            write: false,
            address,
            value: 0,
            external: bus.read(0xcaae),
            position,
            domains,
        });
        if deferred {
            assert_eq!(
                domain_word(&bus, 0xd380),
                0,
                "diagnostic marker before flush"
            );
            assert!(spans > 0);
        }
        assert!(active_span.is_none());
        let mut final_state = bus.ram[..0x800].to_vec();
        final_state.extend_from_slice(&bus.ram[0xa80..0xa9e]); // omit native inline-pointer scratch
        final_state.extend_from_slice(&bus.ram[0xaa0..0xabe]);
        final_state.extend_from_slice(&bus.ram[0xb02..0xb04]); // genuine guest S/P
        final_state.extend_from_slice(&bus.ram[0xb08..0xb11]);
        final_state.extend_from_slice(&bus.cart_ram);
        let (position, domains) = virtual_domains(&bus, deferred);
        eprintln!(
            "{env}={} events={} spans={spans} source={} approximate_z80={} wall={:?}",
            project.display(),
            events.len(),
            domain_cycle(&bus),
            cpu.cycles,
            started.elapsed()
        );
        results.push((
            events,
            publication,
            final_state,
            position,
            domains,
            bus.psg_log,
            bus.vram,
            bus.cram,
        ));
    }
    assert_eq!(results[0].0.len(), results[1].0.len());
    for (index, (precise, deferred)) in results[0].0.iter().zip(&results[1].0).enumerate() {
        assert_eq!(precise, deferred, "source transfer{index}");
    }
    assert_eq!(
        results[0].1, results[1].1,
        "packet source publication epochs"
    );
    assert_eq!(
        results[0].2, results[1].2,
        "guest/source/frozen packet final state"
    );
    assert_eq!(results[0].3, results[1].3);
    assert_eq!(results[0].4, results[1].4);
    assert_eq!(results[0].5, results[1].5, "PSG publication values");
    assert_eq!(results[0].6, results[1].6, "VRAM");
    assert_eq!(results[0].7, results[1].7, "CRAM");
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT deferred hardware project"]
fn assembled_deferred_tick_deadlines_wrap_and_pending_interrupt_dma_guards() {
    // Event-free zero-page/ROM intervals must stop BEFORE the scanline or
    // sequencer event, regardless of 32-bit source-counter wrap.
    for (dot, apu_counter, next, cycles, ticks, expected_pending) in [
        (100, 100, 7457, 100u32, 10, 10),
        (335, 100, 7457, 100, 2, 0), // second tick crosses line, precise
        (0, 7455, 7457, 100, 2, 0),  // actual quarter event on second tick
        (100, 100, 7457, u32::MAX - 3, 10, 10),
    ] {
        let mut probe = PpuProbe::new();
        probe.call("rt_source_apu_init", 0, 0);
        probe.set_word(0xca84, dot);
        probe.set_word(0xca86, 10);
        probe.set_word(0xd504, apu_counter);
        probe.set_word(0xd506, next);
        for (index, byte) in cycles.to_le_bytes().into_iter().enumerate() {
            probe.bus.write(0xca80 + index as u16, byte);
        }
        for _ in 0..ticks {
            probe.call("rt_source_tick", 0x71, 0x4796);
        }
        assert_eq!(domain_cycle(&probe.bus), cycles.wrapping_add(ticks));
        assert_eq!(probe.word(0xd380), expected_pending);
        let virtual_before = virtual_blank_live(&probe.bus);
        let state = probe.call_abi("rt_source_domain_flush", 0x71, 0x4796, 0x2571, true);
        assert_eq!(
            (state.a, state.f, state.bc(), state.de(), state.hl()),
            (0x71, 0x95, 0x2571, 0x3692, 0x4796)
        );
        assert!(state.iff1 && state.iff2);
        assert_eq!(probe.word(0xd380), 0);
        assert_eq!(virtual_before, virtual_blank_live(&probe.bus));
        assert_eq!(domain_cycle(&probe.bus), cycles.wrapping_add(ticks));
        if apu_counter == 7455 {
            assert_eq!(probe.bus.read(0xd51b), 1);
        }
    }
    for (address, value) in [
        (0xcaa5, 1),
        (0xcaa6, 1),
        (0xcaa6, 2),
        (0xcaa7, 1),
        (0xcaa9, 1),
        (0xcaab, 1),
        (0xd400, 2),
    ] {
        let mut probe = PpuProbe::new();
        probe.call("rt_source_apu_init", 0, 0);
        probe.set_word(0xca84, 100);
        probe.set_word(0xca86, 10);
        probe.set_word(0xd504, 100);
        probe.bus.write(address, value);
        probe.call("rt_source_tick", 0, 0);
        assert_eq!(probe.word(0xd380), 0, "guard{address:04X}={value}");
        assert_eq!(probe.word(0xca84), 103);
        assert_eq!(probe.word(0xd504), 101);
        assert_eq!(domain_cycle(&probe.bus), 1);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT deferred hardware project"]
fn assembled_deferred_diagnostics_flush_before_observable_marker() {
    for (label, input_a, expected) in [
        ("rt_source_phase_error", 0, 0xe9),
        ("rt_cnrom_unsupported", 0, 0xe8),
        ("rt_unresolved_jsr", 0, 0xe1),
        ("rt_cnrom_packet_graphics_trap", 0xea, 0xea),
    ] {
        let mut probe = PpuProbe::new();
        probe.call("rt_source_apu_init", 0, 0);
        probe.set_word(0xca84, 100);
        probe.set_word(0xca86, 10);
        probe.set_word(0xd504, 100);
        for _ in 0..7 {
            probe.call("rt_source_tick", 0, 0);
        }
        assert_eq!(probe.word(0xd380), 7);
        let before = virtual_blank_live(&probe.bus);
        probe.call_abi(label, input_a, 0, 0, false);
        assert_eq!(probe.bus.read(0xcb1d), expected, "{label}");
        assert_eq!(probe.word(0xd380), 0, "{label} marker is normalized");
        assert_eq!(before, virtual_blank_live(&probe.bus));
        assert_eq!(domain_cycle(&probe.bus), 7);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fetch {
    read: bool,
    line: u16,
    dot: u16,
    kind: u8,
    address: u16,
    bank: u8,
    value: u8,
}

fn literal_bg_interval_fetches(bus: &SmsBus, count: u16, vertical: bool) -> (Vec<Fetch>, u16) {
    let mut v = u16::from_be_bytes([bus.ram[0xb0f], bus.ram[0xb10]]);
    let mut tile = bus.ram[0x1313];
    let mut address = domain_word(bus, 0xd307);
    let mut kind = bus.ram[0x1306];
    let bank = bus.ram[0x810];
    let mut events = Vec::new();
    for relative in 1..=count * 3 {
        let dot = domain_word(bus, 0xca84) + relative;
        assert!(dot < 256);
        if dot & 1 != 0 {
            (kind, address) = match dot & 7 {
                1 => (1, 0x2000 | (v & 0x0fff)),
                3 => (
                    2,
                    0x23c0 | (v & 0x0c00) | ((v >> 4) & 0x38) | ((v >> 2) & 7),
                ),
                5 => (
                    3,
                    u16::from(bus.ram[0xb08] & 0x10) * 0x100
                        + u16::from(tile) * 16
                        + ((v >> 12) & 7),
                ),
                7 => (
                    4,
                    u16::from(bus.ram[0xb08] & 0x10) * 0x100
                        + u16::from(tile) * 16
                        + ((v >> 12) & 7)
                        + 8,
                ),
                _ => unreachable!(),
            };
            events.push(Fetch {
                read: false,
                line: domain_word(bus, 0xca86),
                dot,
                kind,
                address,
                bank: 0,
                value: 0,
            });
        } else {
            let value = if address < 0x2000 {
                bus.rom[31 * BANK_SIZE + usize::from(bank) * 8192 + usize::from(address)]
            } else {
                let nt = usize::from((address - 0x2000) & 0x0fff);
                let physical = if vertical {
                    (nt / 1024) & 1
                } else {
                    (nt / 1024) >> 1
                };
                bus.cart_ram[physical * 1024 + (nt & 1023)]
            };
            events.push(Fetch {
                read: true,
                line: domain_word(bus, 0xca86),
                dot,
                kind,
                address,
                bank,
                value,
            });
            if kind == 1 {
                tile = value;
            }
            if dot & 7 == 0 {
                v = if v & 31 == 31 {
                    (v & !31) ^ 0x0400
                } else {
                    v + 1
                };
            }
        }
    }
    (events, v)
}

fn literal_sprite_interval_fetches(bus: &SmsBus, count: u16, vertical: bool) -> Vec<Fetch> {
    let start = domain_word(bus, 0xca84);
    let line = domain_word(bus, 0xca86);
    let ctrl = bus.ram[0xb08];
    let bank = bus.ram[0x810];
    let v = u16::from_be_bytes([bus.ram[0xb0f], bus.ram[0xb10]]);
    let mut address = domain_word(bus, 0xd307);
    let mut kind = bus.ram[0x1306];
    let mut result = Vec::new();
    for dot in start + 1..=start + count * 3 {
        assert!((259..321).contains(&dot));
        let slot = usize::from((dot - 257) / 8);
        let phase = (dot - 257) & 7;
        if dot & 1 != 0 {
            if phase == 0 || phase == 2 {
                kind = 7;
                address = 0x2000 | (v & 0x0fff);
            } else {
                kind = if phase == 4 { 5 } else { 6 };
                let sprite = &bus.ram[0x1340 + slot * 4..0x1344 + slot * 4];
                let height = if ctrl & 0x20 != 0 { 16u8 } else { 8 };
                let mut row = (line as u8).wrapping_sub(sprite[0]) & (height - 1);
                if sprite[2] & 0x80 != 0 {
                    row ^= height - 1;
                }
                let (base, tile) = if height == 16 {
                    (
                        u16::from(sprite[1] & 1) * 0x1000,
                        (sprite[1] & 0xfe) + row / 8,
                    )
                } else {
                    (u16::from(ctrl & 8 != 0) * 0x1000, sprite[1])
                };
                address = base
                    + u16::from(tile) * 16
                    + u16::from(row & 7)
                    + if phase == 6 { 8 } else { 0 };
            }
            result.push(Fetch {
                read: false,
                line,
                dot,
                kind,
                address,
                bank: 0,
                value: 0,
            });
        } else {
            let value = if address < 0x2000 {
                bus.rom[31 * BANK_SIZE + usize::from(bank) * 8192 + usize::from(address)]
            } else {
                let nt = usize::from((address - 0x2000) & 0x0fff);
                let physical = if vertical {
                    (nt / 1024) & 1
                } else {
                    (nt / 1024) >> 1
                };
                bus.cart_ram[physical * 1024 + (nt & 1023)]
            };
            result.push(Fetch {
                read: true,
                line,
                dot,
                kind,
                address,
                bank,
                value,
            });
        }
    }
    result
}

// Logical state excludes predictor/materializer scratch, whose purpose is to
// differ under optimization. The 104-byte live PPU and loopy v remain exact.
fn interval_live(bus: &SmsBus) -> Vec<u8> {
    let mut state = bus.ram[0x1300..0x1368].to_vec();
    state.extend_from_slice(&bus.ram[0xb0f..0xb11]);
    state.extend_from_slice(&bus.ram[0xa84..0xa8c]);
    state
}

fn interval_context(bus: &SmsBus) -> Vec<u8> {
    let mut context = bus.cart_ram[..2048].to_vec();
    context.extend_from_slice(&bus.ram[0x900..0xa00]);
    context.extend_from_slice(&bus.ram[0x860..0x880]);
    context.extend_from_slice(&bus.ram[0xb08..0xb0b]);
    context.extend_from_slice(&bus.ram[0x814..0x817]);
    context.push(bus.ram[0x810]);
    context
}

fn interval_other_domains(bus: &SmsBus, pending: u16) -> Vec<u8> {
    let mut apu = bus.ram[0x1500..0x15ee].to_vec();
    if pending != 0 {
        let counter = u16::from_le_bytes([apu[4], apu[5]]) + pending;
        assert!(counter < u16::from_le_bytes([apu[6], apu[7]]));
        apu[4..6].copy_from_slice(&counter.to_le_bytes());
        for offset in [9, 11, 12] {
            if apu[offset] != 0 {
                assert!(u16::from(apu[offset]) > pending);
                apu[offset] -= pending as u8;
            }
        }
        apu[0x1b] = 0;
    }
    apu.extend_from_slice(&bus.ram[0x1400..0x140b]);
    apu.extend_from_slice(&bus.ram[0xb30..0xb62]);
    // Interrupt line/latch/acceptance and genuine guest stack/status at every
    // bus event, rather than only eventual register equality.
    apu.extend_from_slice(&bus.ram[0xaa3..0xaac]);
    apu.extend_from_slice(&bus.ram[0xb02..0xb04]);
    apu
}

fn virtual_blank_live(bus: &SmsBus) -> (Vec<u8>, Vec<u8>) {
    let pending = domain_word(bus, 0xd380);
    let mut ppu = interval_live(bus);
    if pending != 0 {
        assert_eq!(bus.ram[0xb09] & 0x18, 0);
        ppu[5] = 0;
        ppu[0x30] = 0;
        let dot = domain_word(bus, 0xca84) + 3 * pending;
        assert!(dot <= 340);
        ppu[106..108].copy_from_slice(&dot.to_le_bytes());
    }
    (ppu, interval_other_domains(bus, pending))
}

#[test]
#[ignore = "requires frozen precise/v2 deferred projects; complete source stream comparison"]
fn assembled_typed_intervals_preserve_source_events_live_endpoints_and_expanded_fetches() {
    observe_typed_source_streams(false);
}

#[test]
#[ignore = "requires precise/ordinary/CPU-wait v3 rendered source projects"]
fn assembled_poll_composition_expands_all_cpu_reads_and_nested_physical_fetches() {
    observe_typed_source_streams(true);
}

fn observe_typed_source_streams(cpu_wait_mode: bool) {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    let mut starts = BTreeMap::<u32, (Vec<u8>, Arc<Vec<u8>>)>::new();
    let mut endpoints = BTreeMap::<u32, Vec<u8>>::new();
    let mut results = Vec::new();
    let subjects = if cpu_wait_mode {
        vec![
            ("TRACE_CNROM_DOMAIN_PRECISE", false, false),
            ("TRACE_CNROM_DOMAIN_DEFERRED", true, false),
            ("TRACE_CNROM_DOMAIN_CPU_WAIT", true, true),
        ]
    } else {
        vec![
            ("TRACE_CNROM_DOMAIN_PRECISE", false, false),
            ("TRACE_CNROM_DOMAIN_DEFERRED", true, false),
        ]
    };
    for (env, deferred, cpu_wait) in subjects {
        let project = PathBuf::from(std::env::var(env).unwrap());
        let defs = load_wla_symbol_defs(&project.join("sms.sym"));
        assert_eq!(
            defs.contains_key("rt_source_domain_format_v2"),
            deferred && !cpu_wait_mode
        );
        assert_eq!(
            defs.contains_key("rt_source_domain_format_v3"),
            deferred && cpu_wait_mode,
            "CPU summaries require the explicit v3 observer"
        );
        let vertical = std::fs::read_to_string(project.join("sms.asm"))
            .unwrap()
            .contains(".define NES_MIRRORING_VERTICAL");
        let mut bus = SmsBus::new(std::fs::read(project.join("sms.sms")).unwrap(), 0xff);
        let mut cpu = Cpu::new();
        let mut events = Vec::new();
        let mut fetches = Vec::new();
        let mut pending_read: Option<(u16, DomainBusEvent)> = None;
        let mut active_span = None;
        let mut spans = [0usize; 4];
        let mut cpu_spans = 0;
        let mut cpu_span = None;
        let mut publication = Vec::new();
        let mut boundaries = Vec::new();
        let mut cached_context = Arc::new(Vec::new());
        let started = std::time::Instant::now();
        let mut steps = 0;
        for step in 0..70_000_000 {
            steps = step;
            if bus.read(0xcb1d) != 0 {
                break;
            }
            let cycle = domain_cycle(&bus);
            if !deferred && cpu.pc == defs["rt_source_tick"].1 {
                let context = interval_context(&bus);
                if *cached_context != context {
                    cached_context = Arc::new(context);
                }
                starts.insert(cycle, (interval_live(&bus), Arc::clone(&cached_context)));
            }
            if cpu.pc == defs["rt_source_begin"].1 {
                boundaries.push((
                    cycle,
                    cpu.a,
                    bus.ram[0xabd],
                    bus.ram[0xabc],
                    bus.ram[0xb02],
                    bus.ram[0xb03],
                ));
            }
            if cpu.pc == defs["rt_source_quiet_span_begin"].1 {
                assert!(cpu_wait, "undeclared CPU summary");
                assert!(cpu_span.is_none() && pending_read.is_none() && active_span.is_none());
                assert_eq!(domain_word(&bus, 0xd380), 0);
                let start = u32::from_le_bytes(bus.ram[0xabe..0xac2].try_into().unwrap());
                let repeats = domain_word(&bus, 0xcac2);
                let head = domain_word(&bus, 0xcac4);
                let zp = bus.ram[0xac6];
                let value = bus.ram[0xac7];
                assert_eq!((start, head, zp), (cycle, 0x8200, 0x20));
                assert!((1..=18).contains(&repeats));
                assert_eq!(value, 1);
                assert_eq!(bus.ram[0x20], value);
                assert_eq!(bus.ram[0xac8], 0xa9);
                assert_eq!(domain_word(&bus, 0xcac9), 6 * repeats);
                assert_eq!(bus.ram[0xb03] & 0x82, value & 0x80);
                let count = 6 * repeats;
                for iteration in 0..repeats {
                    for offset in [0u16, 3] {
                        boundaries.push((
                            start + u32::from(iteration * 6 + offset),
                            value,
                            bus.ram[0xabd],
                            bus.ram[0xabc],
                            bus.ram[0xb02],
                            bus.ram[0xb03],
                        ));
                    }
                    for (offset, (address, byte)) in [
                        (head, 0xa5),
                        (head + 1, zp),
                        (u16::from(zp), value),
                        (head + 2, 0xd0),
                        (head + 3, 0xfc),
                        (head + 4, 0xa9),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let elapsed = iteration * 6 + offset as u16 + 1;
                        events.push(DomainBusEvent {
                            cycle: start + u32::from(elapsed),
                            completed: true,
                            write: false,
                            address,
                            value: byte,
                            external: byte,
                            position: (
                                domain_word(&bus, 0xca86),
                                domain_word(&bus, 0xca84) + 3 * elapsed,
                            ),
                            domains: interval_other_domains(&bus, elapsed),
                        });
                    }
                }
                cpu_span = Some((
                    start + u32::from(count),
                    bus.ram[0xa8c..0xabe].to_vec(),
                    bus.ram[..0x800].to_vec(),
                ));
            }
            // End aliases the common fallback return; only an opened summary
            // owns a positive retirement event at this label.
            if cpu_span.is_some() && cpu.pc == defs["rt_source_quiet_span_end"].1 {
                let (end, descriptor, guest) = cpu_span.take().expect("unopened CPU summary");
                assert_eq!(cycle, end);
                assert_eq!(domain_word(&bus, 0xd380), 0);
                assert!(active_span.is_none());
                assert_eq!(
                    &bus.ram[0xa8c..0xabe],
                    descriptor,
                    "retained branch metadata"
                );
                assert_eq!(&bus.ram[..0x800], guest, "summary guest RAM");
                cpu_spans += 1;
            }
            if let Some((ret, mut event)) = pending_read.take() {
                if cpu.pc == ret {
                    event.value = cpu.a;
                    event.external = bus.read(0xcaae);
                    events.push(event);
                } else {
                    pending_read = Some((ret, event));
                }
            }
            if cpu.pc == defs["rt_source_bus_read_event"].1
                || cpu.pc == defs["rt_source_bus_write_event"].1
            {
                let pending = if deferred {
                    domain_word(&bus, 0xd380)
                } else {
                    0
                };
                if !deferred {
                    endpoints.insert(cycle, interval_live(&bus));
                }
                let event = DomainBusEvent {
                    cycle,
                    completed: true,
                    write: cpu.pc == defs["rt_source_bus_write_event"].1,
                    address: cpu.hl(),
                    value: cpu.a,
                    external: bus.read(0xcaae),
                    position: (
                        domain_word(&bus, 0xca86),
                        domain_word(&bus, 0xca84) + 3 * pending,
                    ),
                    domains: interval_other_domains(&bus, pending),
                };
                if event.write {
                    events.push(event);
                } else {
                    assert!(pending_read.is_none());
                    let ret = u16::from_le_bytes([bus.read(cpu.sp), bus.read(cpu.sp + 1)]);
                    pending_read = Some((ret, event));
                }
            }
            if deferred
                && (cpu.pc == defs["rt_cpu_read_bus"].1 || cpu.pc == defs["rt_cpu_write_bus"].1)
            {
                let address = cpu.hl();
                if (0x2000..0x8000).contains(&address)
                    || (cpu.pc == defs["rt_cpu_write_bus"].1 && address >= 0x2000)
                {
                    assert_eq!(
                        domain_word(&bus, 0xd380),
                        0,
                        "hardware access must materialize"
                    );
                }
            }
            if deferred && cpu.pc == defs["rt_source_domain_span_begin"].1 {
                assert!(active_span.is_none());
                let start = u32::from_le_bytes(bus.ram[0x1384..0x1388].try_into().unwrap());
                let count = domain_word(&bus, 0xd388);
                let kind = bus.ram[0x138f];
                assert!((1..=3).contains(&kind));
                assert!(count > 0);
                assert_eq!(start.wrapping_add(u32::from(count)), cycle);
                assert_eq!(count, domain_word(&bus, 0xd380));
                let (live, context) = &starts[&start];
                assert_eq!(&interval_live(&bus), live, "span START {start} kind{kind}");
                assert_eq!(
                    &interval_context(&bus),
                    context.as_ref(),
                    "immutable source context START {start}"
                );
                let expanded = match kind {
                    1 => Vec::new(),
                    2 => literal_bg_interval_fetches(&bus, count, vertical).0,
                    3 => literal_sprite_interval_fetches(&bus, count, vertical),
                    _ => unreachable!(),
                };
                let dot = domain_word(&bus, 0xca84);
                fetches.extend(
                    expanded
                        .into_iter()
                        .map(|fetch| (start + u32::from((fetch.dot - dot).div_ceil(3)), fetch)),
                );
                active_span = Some((
                    cycle,
                    kind,
                    interval_context(&bus),
                    interval_other_domains(&bus, count),
                ));
            }
            if deferred && cpu.pc == defs["rt_source_domain_span_end"].1 {
                let (end, kind, context, other) = active_span.take().unwrap();
                assert_eq!(cycle, end);
                assert_eq!(domain_word(&bus, 0xd380), 0);
                assert_eq!(bus.ram[0x13fa], 0);
                assert_eq!(
                    interval_live(&bus),
                    endpoints[&end],
                    "full endpoint {end} kind{kind}"
                );
                assert_eq!(
                    interval_context(&bus),
                    context,
                    "context changed inside span"
                );
                assert_eq!(
                    interval_other_domains(&bus, 0),
                    other,
                    "APU/event-free span"
                );
                spans[usize::from(kind)] += 1;
            }
            let is_read = cpu.pc == defs["rt_source_ppu_fetch_read"].1;
            if (is_read || cpu.pc == defs["rt_source_ppu_fetch_address_ready"].1)
                && bus.ram[0x13fa] == 0
            {
                assert!(active_span.is_none(), "real-time fetch inside summary");
                fetches.push((
                    cycle,
                    Fetch {
                        read: is_read,
                        line: domain_word(&bus, 0xca86),
                        dot: domain_word(&bus, 0xca84),
                        kind: bus.ram[0x1306],
                        address: domain_word(&bus, 0xd307),
                        bank: if is_read { bus.ram[0x1309] } else { 0 },
                        value: if is_read { bus.ram[0x130a] } else { 0 },
                    },
                ));
            }
            for label in [
                "rt_cnrom_packet_capture",
                "rt_cnrom_packet_validate_complete",
                "rt_cnrom_packet_committed",
            ] {
                if cpu.pc == defs[label].1 {
                    if deferred {
                        assert_eq!(domain_word(&bus, 0xd380), 0);
                    }
                    publication.push((label, cycle));
                }
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(bus.read(0xcb1d), 0xe8);
        assert_eq!(bus.read(0xc7ff), 0xa5);
        assert!(active_span.is_none());
        assert!(cpu_span.is_none());
        assert_eq!(cpu_spans > 0, cpu_wait);
        let (_, mut terminal) = pending_read.take().unwrap();
        assert_eq!(terminal.address, 0x5000);
        terminal.completed = false;
        terminal.value = 0;
        terminal.external = bus.read(0xcaae);
        events.push(terminal);
        if deferred {
            assert_eq!(domain_word(&bus, 0xd380), 0, "diagnostic before flush");
            assert!(spans[1] > 0);
            if !fetches.is_empty() {
                assert!(spans[2] > 0 && spans[3] > 0);
            }
        }
        let mut final_state = bus.ram[..0x800].to_vec();
        final_state.extend_from_slice(&bus.ram[0xa80..0xa9e]);
        let final_label = format!("L_{:04X}", domain_word(&bus, 0xca8c));
        // SC_RESUME is a native pointer after CALL+ten inline descriptor
        // bytes, not a6502 PC. An inserted poll call legitimately relocates
        // it; prove its exact subject-local target before semantic comparison.
        assert_eq!(domain_word(&bus, 0xcab2), defs[&final_label].1 + 13);
        final_state.extend_from_slice(&bus.ram[0xaa0..0xab2]);
        final_state.extend_from_slice(&bus.ram[0xab4..0xabe]);
        final_state.extend_from_slice(&bus.ram[0xb02..0xb04]);
        final_state.extend_from_slice(&interval_live(&bus));
        final_state.extend_from_slice(&interval_other_domains(&bus, 0));
        final_state.extend_from_slice(&bus.cart_ram);
        eprintln!(
            "typed {env}={} events={} fetches={} spans={spans:?} cpu_spans={cpu_spans} source={} steps={steps} approximate_z80={} wall={:?}",
            project.display(),
            events.len(),
            fetches.len(),
            domain_cycle(&bus),
            cpu.cycles,
            started.elapsed()
        );
        results.push((
            events,
            fetches,
            boundaries,
            publication,
            final_state,
            bus.psg_log,
            bus.vram,
            bus.cram,
        ));
    }
    for compared in &results[1..] {
        assert_eq!(results[0].0.len(), compared.0.len());
        for (index, (a, b)) in results[0].0.iter().zip(&compared.0).enumerate() {
            assert_eq!(a, b, "CPU bus event{index}");
        }
        assert_eq!(results[0].1.len(), compared.1.len());
        for (index, (a, b)) in results[0].1.iter().zip(&compared.1).enumerate() {
            assert_eq!(a, b, "physical PPU fetch{index}");
        }
        assert_eq!(results[0].2, compared.2, "source A/X/Y/S/P boundaries");
        assert_eq!(results[0].3, compared.3, "packet lifecycle");
        assert_eq!(results[0].4.len(), compared.4.len());
        for (index, (a, b)) in results[0].4.iter().zip(&compared.4).enumerate() {
            assert_eq!(a, b, "terminal semantic-state offset{index:04X}");
        }
        assert_eq!(results[0].5, compared.5, "PSG events");
        assert_eq!(results[0].6, compared.6, "VRAM");
        assert_eq!(results[0].7, compared.7, "CRAM");
    }
}

fn hardware_poll_checkpoint() -> (Cpu, SmsBus, HashMap<String, (u8, u16)>) {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_DOMAIN_CPU_WAIT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    assert!(defs.contains_key("rt_source_domain_format_v3"));
    let mut bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    let mut cpu = Cpu::new();
    for _ in 0..35_000_000 {
        if cpu.pc == defs["rt_source_poll_loop"].1
            && domain_word(&bus, 0xca8c) == 0x8202
            && bus.ram[0xa96] == 3
        {
            assert_eq!(cpu.a, 1);
            assert_eq!(bus.ram[0xb03], 0x24);
            return (cpu, bus, defs);
        }
        assert_eq!(bus.read(0xcb1d), 0);
        cpu.step(&mut bus).unwrap();
    }
    panic!("did not reach genuine completed wait branch");
}

fn invoke_hardware_poll(
    cpu: &mut Cpu,
    bus: &mut SmsBus,
    defs: &HashMap<String, (u8, u16)>,
) -> usize {
    // Native inline bytes above the test's stack start; never source guest
    // RAM or a hardware pseudo-register. The return stops before execution.
    for (address, value) in [
        (0xdff0, 0xf4),
        (0xdff1, 0xdf),
        (0xdff4, 0),
        (0xdff5, 0x82),
        (0xdff6, 0x20),
    ] {
        bus.write(address, value);
    }
    cpu.pc = defs["rt_source_poll_loop"].1;
    cpu.sp = 0xdff0;
    let mut spans = 0;
    for _ in 0..2_000_000 {
        if cpu.pc == 0xdff7 {
            return spans;
        }
        assert_eq!(bus.read(0xcb1d), 0, "poll helper trapped");
        if cpu.pc == defs["rt_source_quiet_span_begin"].1 {
            spans += 1;
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    panic!("hardware poll helper did not return");
}

#[test]
#[ignore = "requires TRACE_CNROM_DOMAIN_CPU_WAIT genuine v3 rendered wait project"]
fn assembled_hardware_poll_guards_deadline_residues_wrap_and_boundary_service() {
    let (original_cpu, original_bus, defs) = hardware_poll_checkpoint();
    let mut probe = PpuProbe {
        bus: original_bus,
        defs: defs.clone(),
        fetches: Vec::new(),
        status_changes: Vec::new(),
    };
    probe.call("rt_source_domain_flush", 0, 0);
    probe.call("rt_source_apu_publish", 0, 0);
    probe.set_word(0xca86, 10);
    probe.set_word(0xca84, 100);
    probe.set_word(0xd504, 100);
    probe.set_word(0xd506, 7457);
    probe.bus.write(0xcb09, 0);
    probe.set_v(0);
    for address in [0xd509, 0xd50b, 0xd50c, 0xd50d, 0xd51b, 0xd31a, 0xd400] {
        probe.bus.write(address, 0);
    }
    let base = probe.bus;
    // Wrong proof must return without service or any source-time advancement.
    for (address, value) in [
        (0xcaa5, 1),
        (0xcaa6, 1),
        (0xcaa6, 2),
        (0xcaa7, 1),
        (0xcaa9, 1),
        (0xcaab, 1),
        (0xc020, 0),
        (0xc020, 2),
        (0xcb03, 0xa4),
        (0xcb03, 0x26),
        (0xca96, 2),
        (0xca97, 4),
        (0xca8e, 0xea),
        (0xca8f, 0xfb),
        (0xcab0, 0),
        (0xca8c, 0),
    ] {
        let mut bus = base.clone();
        let mut cpu = original_cpu;
        bus.write(address, value);
        bus.write(0xd50d, 1);
        let before = bus.ram[..0xabe].to_vec();
        assert_eq!(
            invoke_hardware_poll(&mut cpu, &mut bus, &defs),
            0,
            "guard{address:04X}"
        );
        assert_eq!(&bus.ram[..0xabe], before);
        assert_eq!(bus.read(0xd50d), 1, "unproved branch must not publish");
    }
    // First forbidden distance1..6 allows no complete6-cycle iteration;7
    // permits exactlyone. All positions are literal same-row/event bounds.
    for domain in 0..3 {
        for distance in 1..=7u16 {
            let mut bus = base.clone();
            let mut cpu = original_cpu;
            match domain {
                0 => {
                    bus.ram[0x1504..0x1506].copy_from_slice(&(7457 - distance).to_le_bytes());
                }
                1 => {
                    bus.ram[0xa86..0xa88].copy_from_slice(&240u16.to_le_bytes());
                    bus.ram[0xa84..0xa86].copy_from_slice(&(341 - 3 * distance).to_le_bytes());
                }
                2 => bus.write(0xd509, distance as u8),
                _ => unreachable!(),
            }
            let start = domain_cycle(&bus);
            let dot = domain_word(&bus, 0xca84);
            let expected = if distance == 7 { 6 } else { 0 };
            assert_eq!(
                invoke_hardware_poll(&mut cpu, &mut bus, &defs),
                usize::from(expected != 0)
            );
            assert_eq!(
                domain_cycle(&bus),
                start + expected,
                "domain{domain}/distance{distance}"
            );
            assert_eq!(domain_word(&bus, 0xca84), dot + 3 * expected as u16);
            assert_eq!(domain_word(&bus, 0xd380), 0);
        }
    }
    for (irq, masked, wrap, pending, dirty, ready) in [
        (true, false, false, false, false, false),
        (true, true, false, false, false, false),
        (false, true, true, false, false, false),
        (false, true, false, true, false, false),
        (false, true, false, false, true, false),
        (false, true, false, false, false, true),
    ] {
        let mut bus = base.clone();
        let mut cpu = original_cpu;
        cpu.a = 1;
        cpu.f = 0x95;
        cpu.set_bc(0x2571);
        cpu.set_de(0x3692);
        cpu.set_hl(0x4796);
        cpu.iff1 = true;
        cpu.iff2 = true;
        bus.write(0xfffe, 17);
        bus.write(0xcb14, 19);
        bus.write(0xffff, 23);
        bus.write(0xfffc, 8);
        bus.write(0xd500, u8::from(irq));
        bus.write(0xcb03, if masked { 0x24 } else { 0x20 });
        if wrap {
            bus.ram[0xa80..0xa84].copy_from_slice(&(u32::MAX - 3).to_le_bytes());
        }
        if pending {
            let mut p = PpuProbe {
                bus,
                defs: defs.clone(),
                fetches: Vec::new(),
                status_changes: Vec::new(),
            };
            for _ in 0..3 {
                p.call("rt_source_tick", 1, 0);
            }
            assert_eq!(domain_word(&p.bus, 0xd380), 3);
            bus = p.bus;
        }
        if dirty {
            bus.write(0xd50d, 1);
        }
        if ready {
            // A validated blank native packet is an explicit service fixture,
            // not an injected source-frame completion.
            for record in [0x9900u16, 0x9940] {
                for offset in 0..64u16 {
                    bus.write(record + offset, 0);
                }
            }
            bus.write(0xd400, 2);
            bus.write(0xd402, 0x67);
        }
        let start = domain_cycle(&bus);
        let pending_before = domain_word(&bus, 0xd380);
        let dot = domain_word(&bus, 0xca84) + 3 * pending_before;
        let cycles = if irq && !masked {
            0
        } else {
            ((340 - dot) / 18) * 6
        };
        let before = bus.ram[0xa8c..0xabe].to_vec();
        let mapping = (bus.mapper_control, bus.slot_bank);
        let spans = invoke_hardware_poll(&mut cpu, &mut bus, &defs);
        assert_eq!(spans, usize::from(cycles != 0));
        assert_eq!(domain_cycle(&bus), start.wrapping_add(u32::from(cycles)));
        assert_eq!(domain_word(&bus, 0xca84), dot + 3 * cycles);
        assert_eq!(domain_word(&bus, 0xd380), 0);
        assert_eq!(&bus.ram[0xa8c..0xabe], before);
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
            (1, 0x95, 0x2571, 0x3692, 0x4796, true, true)
        );
        assert_eq!((bus.mapper_control, bus.slot_bank), mapping);
        assert_eq!(bus.read(0xcb14), 19);
        if dirty {
            assert_eq!(bus.read(0xd50d), 0);
        }
        if ready {
            assert_eq!(bus.read(0xd400), 0);
            assert_eq!(bus.read(0xd406), 0x67);
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated visible materializer"]
fn assembled_visible_intervals_match_literal_fetches_and_all_live_pipeline_state() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PPU_PROJECT").unwrap());
    let vertical = std::fs::read_to_string(path.join("sms.asm"))
        .unwrap()
        .contains(".define NES_MIRRORING_VERTICAL");
    let mut costs = Vec::new();
    for fine_x in 0..8u8 {
        for fine_y in 0..8u16 {
            let start = [3u16, 6, 9, 12, 21, 24, 63, 66][usize::from(fine_x)];
            let wanted = [1u16, 5, 16, 50][usize::from(fine_y & 3)];
            let overflow = match fine_y {
                2 => 130u16,
                3 => 132,
                _ => 256,
            };
            let count = wanted.min((overflow - start).div_ceil(3) - 1);
            let mut precise = PpuProbe::new();
            let mut materialized = PpuProbe::new();
            for probe in [&mut precise, &mut materialized] {
                for offset in 0..32768usize {
                    probe.bus.rom[31 * BANK_SIZE + offset] = (offset as u8)
                        .wrapping_mul(13)
                        .wrapping_add(((offset >> 8) as u8).wrapping_mul(7));
                }
                for offset in 0..2048usize {
                    probe.bus.cart_ram[offset] = (offset as u8)
                        .wrapping_mul(7)
                        .wrapping_add(((offset >> 10) as u8) * 17);
                }
                for offset in 0..256u16 {
                    probe.bus.write(
                        0xc900 + offset,
                        if fine_y == 3 && offset & 3 != 0 {
                            10
                        } else {
                            0xff
                        },
                    );
                }
                let sprites = match fine_y {
                    1 | 3 | 7 => 8,
                    2 => 9,
                    _ => 0,
                };
                for sprite in 0..sprites {
                    for (offset, byte) in [10, sprite as u8 + 1, 0, sprite as u8 * 8]
                        .into_iter()
                        .enumerate()
                    {
                        probe.bus.write(0xc900 + sprite * 4 + offset as u16, byte);
                    }
                }
                probe
                    .bus
                    .write(0xcb09, if fine_x & 1 == 0 { 0x1e } else { 0x0a });
                probe
                    .bus
                    .write(0xcb08, if fine_y & 1 == 0 { 0 } else { 0x10 });
                probe.bus.write(0xc810, (fine_y & 3) as u8);
                probe.bus.write(0xc816, fine_x);
                probe.set_word(0xca86, 10);
                probe.set_v((fine_y << 12) | if fine_y & 1 == 0 { 31 } else { 0 });
                for _ in 0..start / 3 {
                    probe.cycle();
                }
                probe.fetches.clear();
            }
            let before = materialized.bus.ram[..0x1368].to_vec();
            let deadline =
                materialized.call_abi("rt_source_ppu_interval_deadline", 0, 0, 0x2571, true);
            assert_eq!(
                (deadline.a, deadline.hl()),
                (2, (overflow - start).div_ceil(3))
            );
            assert!(count < deadline.hl());
            assert_eq!(&materialized.bus.ram[..0x1368], before);
            let (expected_fetches, expected_v) =
                literal_bg_interval_fetches(&precise.bus, count, vertical);
            let mut precise_cost = 0;
            for _ in 0..count {
                precise_cost += precise.call("rt_source_ppu_cycle", 0, 0).cycles;
            }
            assert_eq!(
                precise.fetches, expected_fetches,
                "literal fetches fine{fine_x},{fine_y}"
            );
            let result =
                materialized.call_abi("rt_source_ppu_interval_advance", 2, 0x4796, count, true);
            assert_eq!(materialized.bus.read(0xcb1d), 0);
            assert_eq!(
                (
                    result.a,
                    result.bc(),
                    result.de(),
                    result.hl(),
                    result.iff1,
                    result.iff2
                ),
                (2, count, 0x3692, 0x4796, true, true)
            );
            assert_eq!(
                &materialized.bus.ram[..0x1368],
                &precise.bus.ram[..0x1368],
                "live state fine{fine_x},{fine_y} start{start} count{count}"
            );
            assert_eq!(materialized.v(), expected_v);
            assert_eq!(
                &materialized.bus.ram[0x1400..0x1600],
                &precise.bus.ram[0x1400..0x1600]
            );
            assert_eq!(materialized.bus.cart_ram, precise.bus.cart_ram);
            assert_eq!(materialized.bus.read(0xd3fa), 0);
            if count == 50 {
                costs.push((fine_x, fine_y, count, precise_cost, result.cycles));
            }
        }
    }
    eprintln!(
        "long BG helper-body approximateZ80 costs (fineX,fineY,sourcecycles,precise,materialized): {costs:?}; excludes caller/CPU/APU work"
    );
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT typed materializer deadline"]
fn assembled_sprite_zero_candidate_deadlines_and_typed_rejections_are_literal() {
    for x in [0u8, 5, 16, 254, 255] {
        for mask in [0x1eu8, 0x18, 0x1a, 0x1c] {
            for start in [1u16, 8, 16, 24, 254] {
                let mut probe = PpuProbe::new();
                probe.bus.write(0xcb09, mask);
                probe.bus.write(0xd324, 1);
                probe.bus.write(0xd325, x);
                probe.set_word(0xca84, start);
                for offset in 0..256u16 {
                    probe.bus.write(0xc900 + offset, 0xff);
                }
                let first = (u16::from(x) + 1).max(if mask & 6 == 6 { 1 } else { 9 });
                let last = (u16::from(x) + 8).min(255);
                let (kind, deadline) = if x == 255 || first > last || start > last {
                    (2, (256 - start).div_ceil(3))
                } else if start >= first {
                    (0, 0)
                } else {
                    (2, (first - start).div_ceil(3))
                };
                let before = probe.bus.ram[..0x1368].to_vec();
                let result = probe.call("rt_source_ppu_interval_deadline", 0, 0);
                assert_eq!(
                    (result.a, result.hl()),
                    (kind, deadline),
                    "X{x} mask{mask:02X} dot{start}"
                );
                assert_eq!(&probe.bus.ram[..0x1368], before);
            }
        }
    }
    for (start, kind, pending, count, error) in [
        (1, 2, 0, 85, true),
        (1, 3, 0, 1, true),
        (1, 0, 0, 1, true),
        (0, 2, 0, 1, true),
        (256, 3, 0, 1, true),
        (257, 3, 0, 1, true),
        (258, 3, 0, 21, true),
        (320, 3, 0, 1, true),
        (321, 3, 0, 1, true),
        (1, 2, 1, 1, true),
        (341, 99, 1, 0, false),
    ] {
        let mut probe = PpuProbe::new();
        probe.bus.write(0xcb09, 0x1e);
        probe.set_word(0xca84, start);
        probe.bus.write(0xd31a, pending);
        for offset in 0..256u16 {
            probe.bus.write(0xc900 + offset, 0xff);
        }
        let mut before = probe.bus.ram[..0x1368].to_vec();
        if error {
            before[0xb1d] = 0xe8;
            before[0x1333] = 2;
        }
        let cpu = probe.call_abi("rt_source_ppu_interval_advance", kind, 0x4796, count, true);
        assert_eq!(probe.bus.read(0xcb1d), if error { 0xe8 } else { 0 });
        assert_eq!(&probe.bus.ram[..0x1368], before);
        if !error {
            assert_eq!(
                (cpu.a, cpu.bc(), cpu.hl(), cpu.iff1),
                (kind, 0, 0x4796, true)
            );
        }
    }
    for (address, value) in [(0xcb0a, 1), (0xd32f, 1)] {
        let mut probe = PpuProbe::new();
        probe.bus.write(0xcb09, 0x1e);
        probe.set_word(0xca84, 3);
        probe.bus.write(address, value);
        let result = probe.call("rt_source_ppu_interval_deadline", 0, 0);
        assert_eq!((result.a, result.hl()), (0, 0));
    }
    for (mask, status, active) in [(0x0a, 0, 1), (0x16, 0, 1), (0x1e, 0x40, 1), (0x1e, 0, 0)] {
        let mut probe = PpuProbe::new();
        probe.bus.write(0xcb09, mask);
        probe.bus.write(0xd300, status);
        probe.bus.write(0xd324, active);
        probe.bus.write(0xd325, 16);
        probe.set_word(0xca84, 17);
        for offset in 0..256u16 {
            probe.bus.write(0xc900 + offset, 0xff);
        }
        let result = probe.call("rt_source_ppu_interval_deadline", 0, 0);
        assert_eq!((result.a, result.hl()), (2, 80));
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated sprite-fetch materializer"]
fn assembled_sprite_intervals_expand_literal_half_phase_banked_pattern_reads() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PPU_PROJECT").unwrap());
    let vertical = std::fs::read_to_string(path.join("sms.asm"))
        .unwrap()
        .contains(".define NES_MIRRORING_VERTICAL");
    for ctrl in [0u8, 8, 0x20, 0x28] {
        for flip in [0u8, 0x40, 0x80, 0xc0] {
            for start in [258u16, 261] {
                let count = if start == 258 { 20 } else { 19 };
                let mut precise = PpuProbe::new();
                let mut interval = PpuProbe::new();
                for probe in [&mut precise, &mut interval] {
                    for offset in 0..32768usize {
                        probe.bus.rom[31 * BANK_SIZE + offset] = (offset as u8)
                            .wrapping_mul(13)
                            .wrapping_add(((offset >> 8) as u8).wrapping_mul(7));
                    }
                    for offset in 0..2048usize {
                        probe.bus.cart_ram[offset] = (offset as u8)
                            .wrapping_mul(7)
                            .wrapping_add(((offset >> 10) as u8) * 17);
                    }
                    probe.set_word(0xca86, 10);
                    probe.set_word(0xca84, 255);
                    probe.set_v(0x701f);
                    probe.bus.write(0xcb09, 0x1e);
                    probe.bus.write(0xcb08, ctrl);
                    probe.bus.write(0xc810, flip >> 6);
                    probe.bus.write(0xd321, 1); // Evaluator done; frozen secondary OAM.
                    probe.bus.write(0xd323, 1); // Secondary slot0 is sprite0.
                    for slot in 0..8u16 {
                        let values = if slot < 4 {
                            [10 - slot as u8 * 3, 2 + slot as u8, flip, slot as u8 * 16]
                        } else {
                            [0xff; 4]
                        };
                        for (offset, value) in values.into_iter().enumerate() {
                            probe.bus.write(0xd340 + slot * 4 + offset as u16, value);
                        }
                    }
                    for _ in 0..(start - 255) / 3 {
                        probe.cycle();
                    }
                    probe.fetches.clear();
                }
                let mut expected = Vec::new();
                let mut address = precise.word(0xd307);
                let mut kind = precise.bus.ram[0x1306];
                let v = precise.v();
                for dot in start + 1..=start + count * 3 {
                    let slot = usize::from((dot - 257) / 8);
                    let phase = (dot - 257) & 7;
                    if dot & 1 != 0 {
                        if phase == 0 || phase == 2 {
                            kind = 7;
                            address = 0x2000 | (v & 0x0fff);
                        } else {
                            kind = if phase == 4 { 5 } else { 6 };
                            let sprite = &precise.bus.ram[0x1340 + slot * 4..0x1344 + slot * 4];
                            let height = if ctrl & 0x20 != 0 { 16u8 } else { 8 };
                            let mut row = 10u8.wrapping_sub(sprite[0]) & (height - 1);
                            if sprite[2] & 0x80 != 0 {
                                row ^= height - 1;
                            }
                            let (base, tile) = if height == 16 {
                                (
                                    u16::from(sprite[1] & 1) * 0x1000,
                                    (sprite[1] & 0xfe) + row / 8,
                                )
                            } else {
                                (u16::from(ctrl & 8 != 0) * 0x1000, sprite[1])
                            };
                            address = base
                                + u16::from(tile) * 16
                                + u16::from(row & 7)
                                + if phase == 6 { 8 } else { 0 };
                        }
                        expected.push(Fetch {
                            read: false,
                            line: 10,
                            dot,
                            kind,
                            address,
                            bank: 0,
                            value: 0,
                        });
                    } else {
                        let bank = flip >> 6;
                        let value = if address < 0x2000 {
                            precise.bus.rom
                                [31 * BANK_SIZE + usize::from(bank) * 8192 + usize::from(address)]
                        } else {
                            let nt = usize::from((address - 0x2000) & 0x0fff);
                            let physical = if vertical {
                                (nt / 1024) & 1
                            } else {
                                (nt / 1024) >> 1
                            };
                            precise.bus.cart_ram[physical * 1024 + (nt & 1023)]
                        };
                        expected.push(Fetch {
                            read: true,
                            line: 10,
                            dot,
                            kind,
                            address,
                            bank,
                            value,
                        });
                    }
                }
                let query = interval.call("rt_source_ppu_interval_deadline", 0, 0);
                assert_eq!((query.a, query.hl()), (3, (321 - start).div_ceil(3)));
                assert_eq!(
                    literal_sprite_interval_fetches(&precise.bus, count, vertical),
                    expected
                );
                for _ in 0..count {
                    precise.cycle();
                }
                assert_eq!(
                    precise.fetches, expected,
                    "literal sprite fetch ctrl{ctrl:02X} flip{flip:02X} start{start}"
                );
                let result =
                    interval.call_abi("rt_source_ppu_interval_advance", 3, 0x4796, count, true);
                assert_eq!(interval.bus.read(0xcb1d), 0);
                assert_eq!(
                    (
                        result.a,
                        result.f,
                        result.bc(),
                        result.de(),
                        result.hl(),
                        result.iff1,
                        result.iff2
                    ),
                    (3, 0x95, count, 0x3692, 0x4796, true, true)
                );
                assert_eq!(
                    &interval.bus.ram[..0x1368],
                    &precise.bus.ram[..0x1368],
                    "live sprite state ctrl{ctrl:02X} flip{flip:02X} start{start}"
                );
                assert_eq!(
                    &interval.bus.ram[0x1400..0x1600],
                    &precise.bus.ram[0x1400..0x1600]
                );
                assert_eq!(interval.bus.cart_ram, precise.bus.cart_ram);
                assert_eq!(interval.bus.read(0xd3fa), 0);
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated overflow predictor"]
fn assembled_overflow_prediction_preserves_live_state_and_literal_first_dot() {
    for (count, hidden_rest, expected) in [
        (0u8, 0xff, 0xffff),
        (8, 0xff, 0xffff),
        (8, 0, 132),
        (9, 0xff, 130),
    ] {
        for ingress in [0u16, 63, 66, 126, 129] {
            for iff in [false, true] {
                let mut probe = PpuProbe::new();
                probe.bus.write(0xcb09, 0x1e);
                for sprite in 0..64u16 {
                    probe.bus.write(0xc900 + sprite * 4, 0xff);
                    for offset in 1..4 {
                        probe.bus.write(0xc900 + sprite * 4 + offset, hidden_rest);
                    }
                }
                for sprite in 0..u16::from(count) {
                    for (offset, byte) in [0, sprite as u8 + 1, 0, sprite as u8 * 8]
                        .into_iter()
                        .enumerate()
                    {
                        probe.bus.write(0xc900 + sprite * 4 + offset as u16, byte);
                    }
                }
                for _ in 0..ingress / 3 {
                    probe.cycle();
                }
                assert_eq!(probe.word(0xca84), ingress);
                probe.fetches.clear();
                let before = probe.bus.ram[..0x1368].to_vec();
                let later = probe.bus.ram[0x1400..0x1600].to_vec();
                let mapper = (probe.bus.mapper_control, probe.bus.slot_bank);
                let sram = probe.bus.cart_ram;
                for _ in 0..2 {
                    // miss then exact same-row cache hit
                    let cpu =
                        probe.call_abi("rt_source_ppu_predict_overflow", 0x71, 0x4796, 0x2571, iff);
                    assert_eq!(
                        (cpu.a, cpu.hl()),
                        (0, expected),
                        "count{count} rest{hidden_rest} ingress{ingress}"
                    );
                    assert_eq!((cpu.bc(), cpu.de()), (0x2571, 0x3692));
                    assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                    assert_eq!(&probe.bus.ram[..0x1368], before);
                    assert_eq!(&probe.bus.ram[0x1400..0x1600], later);
                    assert_eq!((probe.bus.mapper_control, probe.bus.slot_bank), mapper);
                    assert_eq!(probe.bus.cart_ram, sram);
                    assert_eq!(probe.bus.read(0xd3fa), 0);
                    assert!(
                        probe.fetches.is_empty(),
                        "lookahead must not issue source PPU fetches"
                    );
                }
                if expected != 0xffff {
                    while probe.word(0xca84) < expected {
                        probe.cycle();
                    }
                    assert_eq!(probe.bus.read(0xd300) & 0x20, 0x20);
                    assert_eq!(
                        probe.call("rt_source_ppu_predict_overflow", 0, 0).hl(),
                        0xffff,
                        "cached past event is not a future transition"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated overflow predictor"]
fn assembled_prediction_cache_hardware_writes_invalidate_status_read_retains() {
    let mut probe = PpuProbe::new();
    for sprite in 0..64u16 {
        for offset in 0..4u16 {
            probe.bus.write(
                0xc900 + sprite * 4 + offset,
                if sprite < 9 { 0 } else { 0xff },
            );
        }
    }
    probe.bus.write(0xcb09, 0x1e);
    assert_eq!(probe.call("rt_source_ppu_predict_overflow", 0, 0).hl(), 130);
    assert_eq!(probe.bus.read(0xd368), 1);
    probe.call("rt_cpu_read_bus", 0, 0x2002);
    assert_eq!(
        probe.bus.read(0xd368),
        1,
        "status read preserves overflow prediction"
    );
    // Peripheral mutation fixture: rendering is disabled before legal raw OAM
    // writes. Restore the controlled rendering context only after those writes.
    probe.bus.write(0xcb09, 0);
    probe.call("rt_cpu_write_bus", 32, 0x2003);
    assert_eq!(probe.bus.read(0xd368), 0);
    probe.call("rt_cpu_write_bus", 0xff, 0x2004);
    probe.call("rt_cpu_write_bus", 0, 0x2003);
    probe.bus.write(0xcb09, 0x1e);
    assert_eq!(
        probe.call("rt_source_ppu_predict_overflow", 0, 0).hl(),
        0xffff
    );
    probe.set_word(0xca86, 10); // Row key must miss; eight-high sprites no longer qualify.
    assert_eq!(
        probe.call("rt_source_ppu_predict_overflow", 0, 0).hl(),
        0xffff
    );
    probe.bus.write(0xcb09, 0);
    probe.call("rt_cpu_write_bus", 32, 0x2003);
    probe.call("rt_cpu_write_bus", 0, 0x2004); // Restore ninth sprite Y.
    probe.call("rt_cpu_write_bus", 0, 0x2003);
    probe.call("rt_cpu_write_bus", 0x20, 0x2000); // Sixteen-high makes nine qualify.
    assert_eq!(probe.bus.read(0xd368), 0);
    probe.bus.write(0xcb09, 0x1e);
    assert_eq!(probe.call("rt_source_ppu_predict_overflow", 0, 0).hl(), 130);
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated overflow predictor"]
fn assembled_prediction_invalid_geometry_has_no_live_or_private_side_effect() {
    for (line, dot, internal) in [
        (240, 0, 0),
        (261, 129, 0),
        (262, 0, 0),
        (0, 256, 0),
        (0, 341, 0),
        (0, 0, 2),
    ] {
        let mut probe = PpuProbe::new();
        probe.set_word(0xca86, line);
        probe.set_word(0xca84, dot);
        probe.bus.write(0xd3fa, internal);
        let before = probe.bus.ram[..0x1600].to_vec();
        let result = probe.call_abi("rt_source_ppu_predict_overflow", 0x71, 0x4796, 0x2571, true);
        assert_eq!(
            (
                result.a,
                result.hl(),
                result.bc(),
                result.de(),
                result.iff1,
                result.iff2
            ),
            (1, 0, 0x2571, 0x3692, true, true)
        );
        assert_eq!(&probe.bus.ram[..0x1600], before);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT isolated inactive interval helper"]
fn assembled_inactive_intervals_match_precise_dots_and_stop_before_every_event() {
    for line in [0u16, 239, 240, 241, 260, 261] {
        for dot in [0u16, 1, 337, 338, 340] {
            for mask in [0u8, 0x1e] {
                let expected = if mask != 0 && !(240..=260).contains(&line) {
                    0
                } else if (line == 241 || line == 261) && dot == 0 {
                    1
                } else {
                    (341 - dot).div_ceil(3)
                };
                let mut precise = PpuProbe::new();
                let mut interval = PpuProbe::new();
                for probe in [&mut precise, &mut interval] {
                    probe.set_word(0xca86, line);
                    probe.set_word(0xca84, dot);
                    probe.bus.write(0xcb09, mask);
                    probe.bus.write(0xd305, 1);
                    probe.bus.write(0xd330, 1);
                }
                let result =
                    interval.call_abi("rt_source_ppu_inactive_deadline", 0x71, 0, 0x2571, true);
                assert_eq!(result.hl(), expected, "line{line} dot{dot} mask{mask:02X}");
                assert_eq!(
                    (
                        result.a,
                        result.f,
                        result.bc(),
                        result.de(),
                        result.iff1,
                        result.iff2
                    ),
                    (0x71, 0x95, 0x2571, 0x3692, true, true)
                );
                assert_eq!(&interval.bus.ram[..0x1380], &precise.bus.ram[..0x1380]);
                if expected == 0 {
                    continue;
                }
                let count = expected - 1;
                for _ in 0..count {
                    precise.cycle();
                }
                let result =
                    interval.call_abi("rt_source_ppu_inactive_advance", 0x71, 0x4796, count, false);
                assert_eq!(
                    (
                        result.a,
                        result.f,
                        result.bc(),
                        result.de(),
                        result.hl(),
                        result.iff1,
                        result.iff2
                    ),
                    (0x71, 0x95, count, 0x3692, 0x4796, false, false)
                );
                assert_eq!(
                    &interval.bus.ram[..0x1380],
                    &precise.bus.ram[..0x1380],
                    "line{line} dot{dot} mask{mask:02X}"
                );
                assert_eq!(
                    &interval.bus.ram[0x1400..0x1600],
                    &precise.bus.ram[0x1400..0x1600]
                );
                assert!(interval.fetches.is_empty() && precise.fetches.is_empty());
                assert!(interval.status_changes.is_empty() && precise.status_changes.is_empty());
            }
        }
    }
    for (line, dot, mask, pending, count, trap) in [
        (240, 0, 0x1e, 0, 114, true),
        (241, 0, 0x1e, 0, 1, true),
        (260, 340, 0x1e, 0, 1, true),
        (239, 100, 0x1e, 0, 1, true),
        (261, 100, 0x1e, 0, 1, true),
        (240, 100, 0x1e, 1, 1, true),
        (262, 341, 0x1e, 1, 0, false),
    ] {
        let mut probe = PpuProbe::new();
        probe.set_word(0xca86, line);
        probe.set_word(0xca84, dot);
        probe.bus.write(0xcb09, mask);
        probe.bus.write(0xd31a, pending);
        let mut before = probe.bus.ram[..0x1380].to_vec();
        if trap {
            before[0xb1d] = 0xe8;
            before[0x1333] = 1;
        }
        probe.call_abi("rt_source_ppu_inactive_advance", 0x71, 0x4796, count, true);
        assert_eq!(probe.bus.read(0xcb1d), if trap { 0xe8 } else { 0 });
        assert_eq!(&probe.bus.ram[..0x1380], before);
    }
}

struct PpuProbe {
    bus: SmsBus,
    defs: HashMap<String, (u8, u16)>,
    fetches: Vec<Fetch>,
    status_changes: Vec<(u16, u16, u8)>,
}

impl PpuProbe {
    fn new() -> Self {
        let path = PathBuf::from(std::env::var("TRACE_CNROM_PPU_PROJECT").unwrap());
        let mut probe = Self {
            bus: SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff),
            defs: load_wla_symbol_defs(&path.join("sms.sym")),
            fetches: Vec::new(),
            status_changes: Vec::new(),
        };
        probe.call("rt_cnrom_packet_init", 0, 0);
        probe.bus.write(0xfffc, 8);
        probe
    }

    fn word(&self, address: u16) -> u16 {
        u16::from_le_bytes([
            self.bus.ram[usize::from(address - 0xc000)],
            self.bus.ram[usize::from(address - 0xbfff)],
        ])
    }

    fn set_word(&mut self, address: u16, value: u16) {
        self.bus.write(address, value as u8);
        self.bus.write(address + 1, (value >> 8) as u8);
    }

    fn set_v(&mut self, value: u16) {
        self.bus.write(0xcb0f, (value >> 8) as u8);
        self.bus.write(0xcb10, value as u8);
    }

    fn v(&mut self) -> u16 {
        u16::from_be_bytes([self.bus.read(0xcb0f), self.bus.read(0xcb10)])
    }

    fn call(&mut self, label: &str, a: u8, hl: u16) -> Cpu {
        let cpu = self.call_abi(label, a, hl, 0x2571, false);
        assert_eq!(self.bus.read(0xcb1d), 0, "{label} PC={:04X}", cpu.pc);
        cpu
    }

    fn call_abi(&mut self, label: &str, a: u8, hl: u16, bc: u16, iff: bool) -> Cpu {
        let mut cpu = Cpu::new();
        cpu.pc = self.defs[label].1;
        cpu.a = a;
        cpu.f = 0x95;
        cpu.set_hl(hl);
        cpu.set_bc(bc);
        cpu.set_de(0x3692);
        cpu.iff1 = iff;
        cpu.iff2 = iff;
        cpu.sp = 0xdff0;
        self.bus.write(0xdff0, 7);
        self.bus.write(0xdff1, 0);
        for _ in 0..200_000 {
            if cpu.pc == 7 || self.bus.read(0xcb1d) != 0 {
                break;
            }
            if self.defs.contains_key("rt_source_ppu_predict_overflow") && self.bus.ram[0x13fa] == 1
            {
                assert!(
                    !cpu.iff1,
                    "lookahead's temporary source state must stay under DI"
                );
            }
            let read = cpu.pc == self.defs["rt_source_ppu_fetch_read"].1;
            if read || cpu.pc == self.defs["rt_source_ppu_fetch_address_ready"].1 {
                self.fetches.push(Fetch {
                    read,
                    line: self.word(0xca86),
                    dot: self.word(0xca84),
                    kind: self.bus.read(0xd306),
                    address: self.word(0xd307),
                    bank: if read { self.bus.read(0xd309) } else { 0 },
                    value: if read { self.bus.read(0xd30a) } else { 0 },
                });
            }
            let prior_status = self.bus.read(0xd300);
            cpu.step(&mut self.bus).unwrap();
            let status = self.bus.read(0xd300);
            if prior_status != status {
                self.status_changes
                    .push((self.word(0xca86), self.word(0xca84), status));
            }
        }
        if self.bus.read(0xcb1d) != 0 {
            return cpu;
        }
        assert_eq!(cpu.pc, 7, "{label} return");
        cpu
    }

    fn cycle(&mut self) {
        // This helper's ABI is three PPU dots only; it does not own SC_CYCLES.
        self.call("rt_source_ppu_cycle", 0, 0);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT freshly assembled source hardware project"]
fn assembled_ppu_loopy_literal_x_y_wrap_cases() {
    let mut probe = PpuProbe::new();
    for (before, after) in [
        (0x0000, 0x0001),
        (0x001f, 0x0400),
        (0x041f, 0x0000),
        (0x73bf, 0x77a0),
    ] {
        probe.set_v(before);
        probe.call("rt_source_ppu_increment_x", 0, 0);
        assert_eq!(probe.v(), after, "X {before:04X}");
    }
    for (before, after) in [
        (0x0000, 0x1000),
        (0x6000, 0x7000),
        (0x7000, 0x0020),
        (0x73a0, 0x0800),
        (0x73e0, 0x0000),
        (0x7ba0, 0x0000),
        (0x73bf, 0x081f),
    ] {
        probe.set_v(before);
        probe.call("rt_source_ppu_increment_y", 0, 0);
        assert_eq!(probe.v(), after, "Y {before:04X}");
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT freshly assembled source hardware project"]
fn assembled_ppu_prerender_prefetch_keeps_origin_and_even_dot_plane_reads() {
    let mut probe = PpuProbe::new();
    probe.bus.write(0xcb09, 0x1e);
    probe.bus.write(0xc816, 5);
    probe.set_word(0xca86, 261);
    probe.set_word(0xca84, 320);
    probe.set_v(0x201f); // Fine Y2, coarse X31, first nametable.
    probe.bus.write(0x801f, 3);
    probe.bus.write(0x8000, 4); // Horizontal mirror: $2400 aliases $2000.
    probe.bus.write(0x83c7, 0xe4);
    probe.bus.write(0x83c0, 0x1b);
    let raw = 31 * BANK_SIZE;
    for (offset, value) in [(0x32, 0xa5), (0x3a, 0x5a), (0x42, 0xf0), (0x4a, 0x0f)] {
        probe.bus.rom[raw + offset] = value;
    }
    for _ in 0..6 {
        probe.cycle();
    }
    assert_eq!(
        probe.word(0xd302),
        0x201f,
        "origin before both prefetched tiles"
    );
    assert_eq!(
        probe.v(),
        0x2401,
        "two coarse-X increments, not packet origin"
    );
    assert_eq!((probe.word(0xca86), probe.word(0xca84)), (261, 338));
    let expected = [
        (321, 1, 0x201f, 3),
        (323, 2, 0x23c7, 0xe4),
        (325, 3, 0x0032, 0xa5),
        (327, 4, 0x003a, 0x5a),
        (329, 1, 0x2400, 4),
        (331, 2, 0x27c0, 0x1b),
        (333, 3, 0x0042, 0xf0),
        (335, 4, 0x004a, 0x0f),
        (337, 7, 0x2401, 0),
    ];
    let mut literal = Vec::new();
    for (dot, kind, address, value) in expected {
        literal.push(Fetch {
            read: false,
            line: 261,
            dot,
            kind,
            address,
            bank: 0,
            value: 0,
        });
        literal.push(Fetch {
            read: true,
            line: 261,
            dot: dot + 1,
            kind,
            address,
            bank: 0,
            value,
        });
    }
    assert_eq!(probe.fetches, literal);
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT freshly assembled source hardware project"]
fn assembled_ppu_chr_identity_is_resolved_on_read_without_cpu_bus_side_effects() {
    let mut probe = PpuProbe::new();
    let raw = 31 * BANK_SIZE;
    probe.bus.rom[raw + 0x32] = 0xaa;
    probe.bus.rom[raw + 8192 + 0x32] = 0x55;
    probe.bus.write(0xcaae, 0x69); // CPU external bus is not a PPU fetch latch.
    probe.bus.write(0xfffe, 17);
    probe.bus.write(0xcb14, 19);
    probe.bus.write(0xffff, 23);
    probe.bus.write(0xfffc, 12);
    let maps = (probe.bus.mapper_control, probe.bus.slot_bank);
    probe.call("rt_source_ppu_fetch_address", 3, 0x0032);
    probe.bus.write(0xc810, 1); // Bank switches between address and read.
    probe.call("rt_source_ppu_fetch_complete", 0, 0);
    assert_eq!((probe.bus.read(0xd309), probe.bus.read(0xd30a)), (1, 0x55));
    assert_eq!(probe.bus.read(0xcaae), 0x69);
    assert_eq!((probe.bus.mapper_control, probe.bus.slot_bank), maps);
    assert_eq!(probe.bus.read(0xcb14), 19);
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT freshly assembled source hardware project"]
fn assembled_ppu_sprite_zero_first_dot_clipping_priority_and_right_edge_are_literal() {
    for (mask, attr, fine_x, start_dot, x, background, expected) in [
        (0x1e, 0, 0, 0, 0, 0x8000, vec![(1, 2, 0x40)]),
        (0x1e, 0x20, 0, 0, 0, 0x8000, vec![(1, 2, 0x40)]),
        (0x18, 0, 0, 0, 0, 0x8000, vec![]),
        (0x1e, 0, 7, 0, 0, 0x0100, vec![(1, 2, 0x40)]),
        (0x1e, 0, 0, 0, 0, 0, vec![]),
        (0x1e, 0, 0, 254, 255, 0xffff, vec![]),
    ] {
        let mut probe = PpuProbe::new();
        probe.bus.write(0xcb09, mask);
        probe.bus.write(0xc816, fine_x);
        probe.set_word(0xca86, 1);
        probe.set_word(0xca84, start_dot);
        probe.set_word(0xd30b, background);
        probe.bus.write(0xd324, 1);
        probe.bus.write(0xd325, x);
        probe.bus.write(0xd326, attr);
        probe.bus.write(0xd327, 0x80);
        probe.cycle();
        assert_eq!(
            probe.status_changes, expected,
            "mask={mask:02X} attr={attr:02X} fineX={fine_x} x={x}"
        );
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT freshly assembled source hardware project"]
fn assembled_ppu_ninth_sprite_sets_overflow_at_dot130_and_hidden_y_does_not_wrap() {
    // Hardware diagonal scan: after eight sprites, an out-of-range ninth Y
    // makes the tenth sprite's tile byte the next Y probe (dot132). This can
    // raise overflow with only eight actual in-range sprites.
    for (count, hidden_rest) in [(0u8, 0xff), (8, 0xff), (8, 0), (9, 0xff)] {
        let mut probe = PpuProbe::new();
        probe.bus.write(0xcb09, 0x1e);
        for sprite in 0..64u16 {
            probe.bus.write(0xc900 + sprite * 4, 0xff);
            for offset in 1..4 {
                probe.bus.write(0xc900 + sprite * 4 + offset, hidden_rest);
            }
        }
        for sprite in 0..u16::from(count) {
            for (offset, value) in [0, sprite as u8 + 1, 0, sprite as u8 * 8]
                .into_iter()
                .enumerate()
            {
                probe.bus.write(0xc900 + sprite * 4 + offset as u16, value);
            }
        }
        for _ in 0..43 {
            probe.cycle();
        }
        assert_eq!(probe.word(0xca84), 129);
        assert_eq!(probe.bus.read(0xd300) & 0x20, 0);
        probe.cycle();
        assert_eq!(
            probe.status_changes,
            if count == 9 {
                vec![(0, 130, 0x20)]
            } else if count == 8 && hidden_rest == 0 {
                vec![(0, 132, 0x20)]
            } else {
                vec![]
            }
        );
        for _ in 0..41 {
            probe.cycle();
        }
        assert_eq!(probe.word(0xca84), 255);
        assert_eq!(probe.bus.read(0xd320), count.min(8));
        assert_eq!(probe.bus.read(0xd323), u8::from(count != 0));
        for sprite in 0..usize::from(count.min(8)) {
            assert_eq!(
                &probe.bus.ram[0x1340 + sprite * 4..0x1344 + sprite * 4],
                &[0, sprite as u8 + 1, 0, sprite as u8 * 8]
            );
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_IRQ_PROJECT genuine translated hardware-irq-cli fixture"]
fn assembled_source_cold_reset_and_irq_status_keep_literal_bus_and_frame_lifecycle() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_IRQ_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    let mut cpu = Cpu::new();
    let read32 = |bus: &SmsBus, offset: usize| {
        u32::from_le_bytes(bus.ram[offset..offset + 4].try_into().unwrap())
    };
    let mut pending = None;
    let mut reset = Vec::new();
    let mut status = Vec::new();
    let mut capture = Vec::new();
    let mut validate = Vec::new();
    let mut committed = Vec::new();
    for _ in 0..20_000_000 {
        if bus.read(0xcb1d) != 0 {
            break;
        }
        let cycles = read32(&bus, 0xa80);
        if let Some((return_pc, at, address)) = pending
            && cpu.pc == return_pc
        {
            if at <= 7 {
                reset.push((at, address, cpu.a));
            }
            if address == 0x4015 {
                status.push((at, cpu.a, bus.read(0xcaae)));
            }
            pending = None;
        }
        if cpu.pc == defs["rt_source_bus_read_event"].1 {
            let return_pc = u16::from_le_bytes([bus.read(cpu.sp), bus.read(cpu.sp + 1)]);
            pending = Some((return_pc, cycles, cpu.hl()));
        }
        if cpu.pc == defs["rt_cnrom_packet_capture"].1 {
            capture.push((cycles, read32(&bus, 0xa88), cpu.hl()));
        }
        if cpu.pc == defs["rt_cnrom_packet_validate_complete"].1 {
            validate.push((cycles, read32(&bus, 0xa88)));
        }
        if cpu.pc == defs["rt_cnrom_packet_committed"].1 {
            committed.push((cycles, read32(&bus, 0xa88)));
        }
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(bus.read(0xcb1d), 0xe8, "bounded source endpoint");
    assert_eq!(
        reset,
        vec![
            (1, 0x0000, 0),
            (2, 0x0000, 0),
            (3, 0x0100, 0),
            (4, 0x01ff, 0),
            (5, 0x01fe, 0),
            (6, 0xfffc, 0),
            (7, 0xfffd, 0x80)
        ]
    );
    assert_eq!(
        status,
        vec![(29869, 0x41, 0x40)],
        "internal4015 returns status but never drives external CPU bus"
    );
    assert_eq!(capture, vec![(0, 0, 0), (29781, 1, 0)]);
    assert_eq!(validate, vec![(27280, 0)]);
    // Outer iteration22 begins at27031; LDX retires27033, 49 DEX/BNE pairs
    // retire27278, and the next DEX retires27280. Publication occurs at that
    // source boundary, without charging any source cycle for SMS conversion.
    assert_eq!(committed, vec![(27280, 0)]);
    assert_eq!(read32(&bus, 0xa80), 29889);
    assert_eq!(
        (cpu.d, cpu.e, bus.read(0xcb02), bus.read(0xcb03)),
        (0x3c, 0, 0x3f, 0xa0)
    );
    assert_eq!(&bus.ram[0x500..0x504], &[0x22, 0x8a, 0x80, 0x41]);
    assert_eq!(bus.read(0xd400), 1);
    assert_eq!(&bus.ram[0x1406..0x140a], &[0; 4]);
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT hardware status-suppression implementation"]
fn assembled_ppu_status_literal_set_window_suppresses_only_new_unaccepted_edge() {
    // Source: PPU_frame_timing oldid19961, least-special-case CPU/PPU alignment.
    // Relative to241:1, -3/-2 normal, -1 suppresses set,0/+1 cancels only the
    // too-short new NMI pulse,+2 normal. Each read follows three REAL PPU dots.
    for (start_line, start_dot, read_line, read_dot, returned, future_flag, new_edge) in [
        (240, 336, 240, 339, 0x7b, 1, 1),
        (240, 337, 240, 340, 0x7b, 1, 1),
        (240, 338, 241, 0, 0x7b, 0, 0),
        (240, 339, 241, 1, 0xfb, 0, 0),
        (240, 340, 241, 2, 0xfb, 0, 0),
        (241, 0, 241, 3, 0xfb, 0, 1),
    ] {
        for (older_edge, accepted) in [(0, 0), (1, 0), (0, 1), (0, 2)] {
            let mut probe = PpuProbe::new();
            probe.set_word(0xca86, start_line);
            probe.set_word(0xca84, start_dot);
            probe.bus.write(0xcb08, 0x80);
            probe.bus.write(0xcb0e, 1);
            probe.bus.write(0xc811, 0x9b);
            probe.bus.write(0xd300, 0x60);
            probe.bus.write(0xcaa5, older_edge);
            probe.bus.write(0xcaa6, accepted);
            probe.cycle();
            assert_eq!(
                (probe.word(0xca86), probe.word(0xca84)),
                (read_line, read_dot)
            );
            let result = probe.call("rt_source_status_read", 0, 0).a;
            assert_eq!(result, returned, "read{read_line}:{read_dot}");
            assert_eq!(probe.bus.read(0xcaa3), 0);
            assert_eq!(probe.bus.read(0xcb0e), 0);
            assert_eq!(probe.bus.read(0xd300), 0x60);
            assert_eq!(probe.bus.read(0xcaa6), accepted);
            probe.cycle();
            assert_eq!(probe.bus.read(0xcaa3), future_flag);
            assert_eq!(
                probe.bus.read(0xcaa5),
                older_edge | new_edge,
                "read{read_line}:{read_dot} older{older_edge} accepted{accepted}"
            );
            assert_eq!(probe.bus.read(0xcaa6), accepted);
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT hardware status-suppression implementation"]
fn assembled_ppu_status_prerender_clear_has_normal_order_and_no_old_clock_guard() {
    for (start_line, start_dot, returned) in [(260, 336, 0xfb), (260, 340, 0x1b)] {
        let mut probe = PpuProbe::new();
        probe.set_word(0xca86, start_line);
        probe.set_word(0xca84, start_dot);
        probe.bus.write(0xc811, 0x9b);
        probe.bus.write(0xd300, 0x60);
        probe.bus.write(0xcaa3, 1);
        probe.bus.write(0xcaa5, 1);
        probe.cycle();
        assert_eq!(probe.call("rt_source_status_read", 0, 0).a, returned);
        assert_eq!(
            probe.bus.read(0xcaa5),
            1,
            "old edge survives PPU flag clear"
        );
        if start_dot == 336 {
            probe.cycle();
        }
        assert_eq!(probe.bus.read(0xd300), 0);
        assert_eq!(probe.bus.read(0xcaa3), 0);
        assert_eq!((probe.bus.read(0xd331), probe.bus.read(0xd332)), (0, 0));
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT strict blank same-row interval helpers"]
fn assembled_ppu_blank_leaf_matches_every_precise_dot_and_literal_deadline() {
    for line in [0u16, 239, 240, 241, 260, 261] {
        for dot in [0u16, 1, 2, 3, 337, 338, 339, 340] {
            let deadline = if (line == 241 || line == 261) && dot == 0 {
                1
            } else {
                (341 - dot).div_ceil(3)
            };
            for iff in [false, true] {
                let mut precise = PpuProbe::new();
                let mut quiet = PpuProbe::new();
                for probe in [&mut precise, &mut quiet] {
                    probe.set_word(0xca86, line);
                    probe.set_word(0xca84, dot);
                    probe.set_word(0xca80, 0xfffe);
                    probe.bus.write(0xd305, 1); // Both paths clear active/stale skip.
                    probe.bus.write(0xd330, 1);
                    probe.bus.write(0xcaa5, 1); // Pending CPU events do not belong to this leaf.
                    probe.bus.write(0xcaa6, 2);
                    probe.bus.write(0xd501, 1);
                    probe.bus.write(0xd5e4, 0xa5);
                    probe.bus.write(0xd5e6, 3);
                }
                let cpu = quiet.call_abi("rt_source_ppu_blank_deadline", 0x71, 0x4796, 0x2571, iff);
                assert_eq!(cpu.hl(), deadline, "line{line} dot{dot}");
                assert_eq!(
                    (cpu.bc(), cpu.de(), cpu.iff1, cpu.iff2),
                    (0x2571, 0x3692, iff, iff)
                );
                let span = deadline - 1;
                for _ in 0..span {
                    precise.cycle();
                }
                let cpu = quiet.call_abi("rt_source_ppu_blank_advance", 0x71, 0x4796, span, iff);
                assert_eq!(
                    (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
                    (0x71, 0x95, span, 0x3692, 0x4796)
                );
                assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                assert_eq!(&quiet.bus.ram[0xa80..0xb00], &precise.bus.ram[0xa80..0xb00]);
                assert_eq!(
                    &quiet.bus.ram[0x1300..0x1600],
                    &precise.bus.ram[0x1300..0x1600],
                    "domains line{line} dot{dot} span{span}"
                );
                assert_eq!(&quiet.bus.cart_ram, &precise.bus.cart_ram);
                assert!(quiet.fetches.is_empty() && precise.fetches.is_empty());
                assert!(quiet.status_changes.is_empty() && precise.status_changes.is_empty());
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PPU_PROJECT strict blank same-row interval helpers"]
fn assembled_ppu_blank_leaf_rejects_events_or_ineligible_states_without_other_mutation() {
    for (line, dot, mask, pending, v, span, rejected) in [
        (0u16, 0u16, 0, 0, 0u16, 114u16, true),
        (0, 338, 0, 0, 0, 1, true),
        (241, 0, 0, 0, 0, 1, true),
        (261, 0, 0, 0, 0, 1, true),
        (0, 0, 8, 0, 0, 1, true),
        (0, 0, 16, 0, 0, 1, true),
        (0, 0, 0, 1, 0, 1, true),
        (262, 0, 0, 0, 0, 1, true),
        (0, 341, 0, 0, 0, 1, true),
        (0, 0, 0, 0, 0x3f00, 1, true),
        (0, 0, 0, 0, 0x7f12, 1, true),
        (262, 341, 24, 1, 0x3f00, 0, false), // Zero bypasses all guards.
    ] {
        let mut probe = PpuProbe::new();
        probe.set_word(0xca86, line);
        probe.set_word(0xca84, dot);
        probe.bus.write(0xcb09, mask);
        probe.bus.write(0xd31a, pending);
        probe.set_v(v);
        probe.bus.write(0xd305, 1);
        probe.bus.write(0xd330, 1);
        let before_clock = probe.bus.ram[0xa80..0xb00].to_vec();
        let mut expected = probe.bus.ram[0x1300..0x1600].to_vec();
        if rejected {
            expected[0x33] = 1;
        }
        probe.call_abi("rt_source_ppu_blank_advance", 0x71, 0x4796, span, true);
        assert_eq!(probe.bus.read(0xcb1d), if rejected { 0xe8 } else { 0 });
        assert_eq!(&probe.bus.ram[0xa80..0xb00], before_clock);
        assert_eq!(&probe.bus.ram[0x1300..0x1600], expected);
        assert!(probe.fetches.is_empty());
    }
}
