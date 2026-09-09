//! Literal standard-controller serial and declared SMS Pause adaptation cases.
//! These invoke actual assembled peripherals, not the source bus integration;
//! source-cycle read timestamps and DMC arbitration need separate fixtures.

use super::*;

/// Test-only controllable physical port2: SmsBus's ordinary $DD is fixed FF.
struct PhysicalPads<'a> {
    bus: &'a mut SmsBus,
    dc: u8,
    dd: u8,
}

impl Bus for PhysicalPads<'_> {
    fn read(&mut self, address: u16) -> u8 {
        self.bus.read(address)
    }
    fn write(&mut self, address: u16, value: u8) {
        self.bus.write(address, value);
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port {
            0xdc => self.dc,
            0xdd => self.dd,
            _ => panic!("unexpected physical input port{port:02X}"),
        }
    }
    fn out_port(&mut self, port: u8, _value: u8) {
        panic!("physical sampling wrote port{port:02X}");
    }
}

fn input_fixture() -> (SmsBus, HashMap<String, (u8, u16)>) {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PACKET_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    input_call(&mut bus, &defs, "rt_source_input_init", 0, 0);
    bus.ram[0xa80..0xb00].fill(0x5a);
    bus.write(0xcb03, 0xa5);
    (bus, defs)
}

fn input_call(
    bus: &mut SmsBus,
    defs: &HashMap<String, (u8, u16)>,
    label: &str,
    a: u8,
    port: u8,
) -> u8 {
    let mut cpu = Cpu::new();
    cpu.a = a;
    cpu.f = 0x95;
    cpu.set_bc(u16::from_be_bytes([port, 0x71]));
    cpu.set_de(0x3852);
    cpu.set_hl(0x1796);
    cpu.iff1 = true;
    cpu.iff2 = true;
    cpu.pc = defs[label].1;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    let mapping = (bus.mapper_control, bus.slot_bank);
    for _ in 0..10_000 {
        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
            break;
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    assert_eq!(cpu.pc, 7, "{label}");
    assert_eq!(bus.read(0xcb1d), 0, "{label}");
    assert_eq!((bus.mapper_control, bus.slot_bank), mapping);
    assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (true, true, 0));
    if !matches!(
        label,
        "rt_source_input_init" | "rt_source_input_sample_physical"
    ) {
        assert_eq!(cpu.bc(), u16::from_be_bytes([port, 0x71]));
        assert_eq!((cpu.de(), cpu.hl()), (0x3852, 0x1796));
        assert_eq!(bus.read(0xcb03), 0xa5);
        assert_eq!(&bus.ram[0xa80..0xb00], &[0x5a; 128]);
        if label != "rt_source_input_read" {
            assert_eq!((cpu.a, cpu.f), (a, 0x95));
        }
    }
    cpu.a
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled source input"]
fn assembled_source_input_freezes_both_serial_bytes_and_merges_only_driven_bits() {
    let (mut bus, defs) = input_fixture();
    bus.write(0xd5e2, 0xa5);
    bus.write(0xd5e3, 0x3c);
    input_call(&mut bus, &defs, "rt_source_input_strobe", 7, 0);
    input_call(&mut bus, &defs, "rt_source_input_strobe", 6, 0);
    assert_eq!(bus.read(0xd5e8), 6, "all three output bits retained");
    let prior = [0xff, 0, 0xa6, 0x5a, 0xe0, 0x80, 0x20, 0x60, 0x1f];
    let high = [0xe0, 0, 0xa0, 0x40, 0xe0, 0x80, 0x20, 0x60, 0];
    let serial = [[1, 0, 1, 0, 0, 1, 0, 1, 1], [0, 0, 1, 1, 1, 1, 0, 0, 1]];
    for bit in 0..9 {
        if bit == 3 {
            bus.write(0xd5e0, 0);
            bus.write(0xd5e1, 0xff);
            input_call(&mut bus, &defs, "rt_source_input_frame", 0x55, 0);
            // Repeated low write must not rewind either in-progress cursor.
            input_call(&mut bus, &defs, "rt_source_input_strobe", 6, 0);
            assert_eq!(&bus.ram[0x15e2..0x15e4], &[0, 0xff]);
            assert_eq!(&bus.ram[0x15e4..0x15e6], &[0xa5, 0x3c]);
        }
        for port in [0, 1] {
            assert_eq!(
                input_call(&mut bus, &defs, "rt_source_input_read", prior[bit], port),
                high[bit] | serial[port as usize][bit]
            );
        }
    }
    assert_eq!(&bus.ram[0x15e6..0x15e8], &[8, 8]);
    for _ in 0..3 {
        assert_eq!(
            input_call(&mut bus, &defs, "rt_source_input_read", 0x9f, 0),
            0x81
        );
    }
    bus.write(0xd5e9, 1); // Explicitly disconnected second official port.
    assert_eq!(
        input_call(&mut bus, &defs, "rt_source_input_read", 0x7f, 1),
        0x60
    );
    assert_eq!(bus.read(0xd5e7), 8);
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled source input"]
fn assembled_source_input_high_strobe_reads_live_a_without_serial_advance() {
    let (mut bus, defs) = input_fixture();
    input_call(&mut bus, &defs, "rt_source_input_strobe", 1, 0);
    for (live, expected) in [(0, 0xa0), (1, 0xa1), (0xfe, 0xa0), (0xff, 0xa1)] {
        bus.write(0xd5e0, live);
        bus.write(0xd5e1, live ^ 1);
        input_call(&mut bus, &defs, "rt_source_input_frame", 0x62, 0);
        for _ in 0..3 {
            assert_eq!(
                input_call(&mut bus, &defs, "rt_source_input_read", 0xbf, 0),
                expected
            );
            assert_eq!(
                input_call(&mut bus, &defs, "rt_source_input_read", 0xbf, 1),
                expected ^ 1
            );
        }
        assert_eq!(&bus.ram[0x15e6..0x15e8], &[0, 0]);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled source input"]
fn assembled_pause_start_select_are_four_source_frames_and_leave_serial_bytes_frozen() {
    for (chord, promoted) in [(0, 8), (1, 9), (3, 4)] {
        let (mut bus, defs) = input_fixture();
        bus.write(0xd5e2, 0xa5);
        input_call(&mut bus, &defs, "rt_source_input_strobe", 1, 0);
        input_call(&mut bus, &defs, "rt_source_input_strobe", 0, 0);
        assert_eq!(input_call(&mut bus, &defs, "rt_source_input_read", 0, 0), 1);
        input_call(&mut bus, &defs, "rt_source_input_pause", 0x71, 0);
        for _ in 0..17 {
            input_call(&mut bus, &defs, "rt_source_input_sample_physical", 0, 0);
        }
        assert_eq!(bus.read(0xd5ea), 1);
        assert_eq!(bus.read(0xd5eb), 0, "host callbacks do not age pulse");
        assert_eq!(&bus.ram[0x15e2..0x15e8], &[0xa5, 0, 0xa5, 0, 1, 0]);
        bus.write(0xd5e0, chord);
        input_call(&mut bus, &defs, "rt_source_input_frame", 0x71, 0);
        assert_eq!(bus.read(0xd5e2), promoted);
        assert_eq!(bus.read(0xd5eb), 3);
        // Releasing or changing the chord cannot change the selected button.
        bus.write(0xd5e0, if chord == 3 { 0 } else { 3 });
        for remaining in [2, 1, 0] {
            input_call(&mut bus, &defs, "rt_source_input_frame", 0x71, 0);
            assert_eq!(bus.read(0xd5e2), if chord == 3 { 4 } else { 11 });
            assert_eq!(bus.read(0xd5eb), remaining);
        }
        input_call(&mut bus, &defs, "rt_source_input_frame", 0x71, 0);
        assert_eq!(bus.read(0xd5e2), if chord == 3 { 0 } else { 3 });
        assert_eq!(input_call(&mut bus, &defs, "rt_source_input_read", 0, 0), 0);
        assert_eq!(&bus.ram[0x15e4..0x15e8], &[0xa5, 0, 2, 0]);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled source input"]
fn assembled_physical_two_pad_mapping_changes_only_pending_samples() {
    let (mut bus, defs) = input_fixture();
    bus.ram[0x15e2..0x15ee].fill(0x6a);
    // Literal SMS U/D/L/R/1/2 wire positions, not the producer mapper formula.
    for (dc, dd, first, second) in [
        (0xff, 0xff, 0, 0),
        (0xfe, 0xff, 0x10, 0),
        (0xfd, 0xff, 0x20, 0),
        (0xfb, 0xff, 0x40, 0),
        (0xf7, 0xff, 0x80, 0),
        (0xef, 0xff, 1, 0),
        (0xdf, 0xff, 2, 0),
        (0xbf, 0xff, 0, 0x10),
        (0x7f, 0xff, 0, 0x20),
        (0xff, 0xfe, 0, 0x40),
        (0xff, 0xfd, 0, 0x80),
        (0xff, 0xfb, 0, 1),
        (0xff, 0xf7, 0, 2),
        (0, 0, 0xf3, 0xf3),
    ] {
        let mut cpu = Cpu::new();
        cpu.set_de(0x2579);
        cpu.set_hl(0x6348);
        cpu.pc = defs["rt_source_input_sample_physical"].1;
        cpu.sp = 0xdff0;
        bus.write(0xdff0, 7);
        bus.write(0xdff1, 0);
        let mut ports = PhysicalPads {
            bus: &mut bus,
            dc,
            dd,
        };
        for _ in 0..200 {
            if cpu.pc == 7 {
                break;
            }
            cpu.step(&mut ports).unwrap();
        }
        assert_eq!(cpu.pc, 7);
        assert_eq!((cpu.de(), cpu.hl()), (0x2579, 0x6348));
        assert_eq!(
            &bus.ram[0x15e0..0x15e2],
            &[first, second],
            "DC{dc:02X}/DD{dd:02X}"
        );
        assert_eq!(&bus.ram[0x15e2..0x15ee], &[0x6a; 12]);
        assert_eq!(&bus.ram[0xa80..0xb00], &[0x5a; 128]);
    }
}
