//! Independent literal-packet evidence. Direct frozen memory is fixture input,
//! not a source PPU implementation or authentic game reset. SMS samples inspect
//! actual committed VRAM; source expectations are separately specified rows.

use super::*;

const LITERAL_PIXELS: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 3, 2, 1, 0],
    [3, 2, 1, 0, 0, 1, 2, 3],
    [1; 8],
    [2; 8],
    [3; 8],
    [0; 8],
    [0, 3, 0, 3, 0, 3, 0, 3],
    [3, 0, 3, 0, 3, 0, 3, 0],
];
const LITERAL_PLANES: [(u8, u8); 8] = [
    (0x5a, 0x3c),
    (0xa5, 0xc3),
    (0xff, 0x00),
    (0x00, 0xff),
    (0xff, 0xff),
    (0x00, 0x00),
    (0x55, 0x55),
    (0xaa, 0xaa),
];

/// Mode4 background palette index, not an NES compositor or physical beam.
/// The test packet fixes NT=$3700 and 224-line mode (32 NT rows). Deliberately
/// independent of dump_framebuffer_ppm's modulo224 approximation.
fn sms_background_index(bus: &SmsBus, x: usize, y: usize) -> u8 {
    sms_background_index_height(bus, x, y, 224)
}

fn sms_background_index_height(bus: &SmsBus, x: usize, y: usize, height: usize) -> u8 {
    assert!([224, 240].contains(&height));
    assert!(x < 256 && y < height);
    let source_x = x.wrapping_sub(usize::from(bus.vdp_regs[8])) & 255;
    let source_y = (y + usize::from(bus.vdp_regs[9])) & 255;
    let entry = 0x3700 + 2 * ((source_y / 8) * 32 + source_x / 8);
    let attributes = bus.vram[entry + 1];
    let tile = usize::from(bus.vram[entry]) + usize::from(attributes & 1) * 256;
    let px = if attributes & 2 == 0 {
        source_x & 7
    } else {
        7 - (source_x & 7)
    };
    let py = if attributes & 4 == 0 {
        source_y & 7
    } else {
        7 - (source_y & 7)
    };
    let offset = tile * 32 + py * 4;
    let color = (0..4).fold(0, |color, plane| {
        color | (((bus.vram[offset + plane] >> (7 - px)) & 1) << plane)
    });
    color + if attributes & 8 == 0 { 0 } else { 16 }
}

// Test-only PAL counter transport. One scripted sample is one observed input,
// not a claim about elapsed Z80 T or actual beam timing. Unscripted geometry
// fixtures advance one physical PAL line per counter read, repeating313.
struct PalPacketBus {
    inner: SmsBus,
    samples: Option<std::collections::VecDeque<u8>>,
    line: usize,
    ports: Vec<(u8, u8)>,
}

impl Bus for PalPacketBus {
    fn read(&mut self, address: u16) -> u8 {
        self.inner.read(address)
    }
    fn write(&mut self, address: u16, value: u8) {
        self.inner.write(address, value);
    }
    fn in_port(&mut self, port: u8) -> u8 {
        if port == 0x7e {
            if let Some(samples) = &mut self.samples {
                return samples.pop_front().expect("unexpected PAL counter sample");
            }
            let value = match self.line {
                0..=255 => self.line as u8,
                256..=266 => (self.line - 256) as u8,
                267..=312 => (self.line - 267 + 0xd2) as u8,
                _ => unreachable!(),
            };
            self.line = (self.line + 1) % 313;
            value
        } else {
            self.inner.in_port(port)
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.ports.push((port, value));
        self.inner.out_port(port, value);
    }
}

fn pal_invoke(cpu: &mut Cpu, bus: &mut PalPacketBus, entry: u16) {
    cpu.pc = entry;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    for _ in 0..6_000_000 {
        if cpu.pc == 7 || bus.inner.read(0xcb1d) != 0 {
            break;
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    assert!(
        cpu.pc == 7 || bus.inner.read(0xcb1d) != 0,
        "PAL helper did not terminate"
    );
}

#[test]
#[ignore = "requires TRACE_CNROM_PAL_PROJECT frozen PAL240 adapter"]
fn assembled_pal_counter_phases_anchor_and_final_reserve_match_literal_sequences() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PAL_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let rom = std::fs::read(path.join("sms.sms")).unwrap();
    // Legal previous phases only: impossible phase values additionally fail
    // closed. These are semantic observations, not the independent T budget.
    for (phase, last, sample, expected_phase, carry) in [
        (1, 0xf0, 0xf0, 1, false),
        (1, 0xf0, 0xff, 1, false),
        (1, 0xff, 0, 2, false),
        (1, 0xfe, 0x0a, 2, false),
        (1, 0xff, 0xd2, 3, false),
        (1, 0xff, 0xef, 3, false),
        (1, 0xff, 0xf0, 0, true),
        (1, 0xff, 0x0b, 0, true),
        (2, 0, 0x0a, 2, false),
        (2, 0x0a, 0xd2, 3, false),
        (2, 0x0a, 0xf0, 3, false),
        (2, 0x0a, 0xff, 3, false),
        (2, 9, 8, 0, true),
        (2, 0, 0x0b, 0, true),
        (3, 0xd2, 0xf0, 3, false),
        (3, 0xf0, 0xfa, 3, false),
        (3, 0xff, 0xff, 3, false),
        (3, 0xff, 0, 0, true),
        (3, 0xf0, 0xd2, 0, true),
        (0, 0, 0xf0, 0, true),
        (4, 0, 0xf0, 0, true),
    ] {
        let mut bus = PalPacketBus {
            inner: SmsBus::new(rom.clone(), 0xff),
            samples: Some([sample].into()),
            line: 0,
            ports: Vec::new(),
        };
        bus.write(0xfffe, defs["rt_chr_packet_pal_sample"].0);
        bus.write(0xcb14, 19);
        bus.write(0xd40c, phase);
        bus.write(0xd40d, last);
        let source = bus.inner.ram[..0x140c].to_vec();
        let mut cpu = Cpu::new();
        cpu.set_bc(0x2571);
        cpu.set_de(0x3692);
        cpu.set_hl(0x4796);
        pal_invoke(&mut cpu, &mut bus, defs["rt_chr_packet_pal_sample"].1);
        assert_eq!(bus.inner.read(0xcb1d), 0);
        assert_eq!(
            (cpu.a, cpu.f & 1 != 0),
            (sample, carry),
            "phase{phase} last{last:02X} sample{sample:02X}"
        );
        assert_eq!(bus.inner.read(0xd40c), expected_phase);
        assert_eq!(bus.inner.read(0xd40d), if carry { last } else { sample });
        assert_eq!(
            (cpu.bc(), cpu.de(), cpu.hl(), cpu.iff1, cpu.iff2),
            (0x2571, 0x3692, 0x4796, false, false)
        );
        assert_eq!(&bus.inner.ram[..0x140c], source);
        assert!(bus.ports.is_empty());
    }
    for samples in [
        vec![0xf0, 0xff, 0, 0x0a, 0xd2, 0xff, 0x0b, 0xf0],
        vec![0x20, 0xf2, 0xf0, 0x20, 0xf1],
        vec![0x20, 0, 0x20, 0xef, 0xf0],
    ] {
        let last = *samples.last().unwrap();
        let mut bus = PalPacketBus {
            inner: SmsBus::new(rom.clone(), 0xff),
            samples: Some(samples.into()),
            line: 0,
            ports: Vec::new(),
        };
        bus.write(0xfffe, defs["rt_chr_packet_pal_acquire"].0);
        let mut cpu = Cpu::new();
        pal_invoke(&mut cpu, &mut bus, defs["rt_chr_packet_pal_acquire"].1);
        assert_eq!((bus.inner.read(0xd40c), bus.inner.read(0xd40d)), (1, last));
        assert_eq!((cpu.iff1, cpu.iff2), (false, false));
        assert!(bus.samples.as_ref().unwrap().is_empty());
        assert!(bus.ports.is_empty());
    }
    for (phase, last, sample, carry) in [
        (1, 0xf0, 0xff, false),
        (2, 0, 0x0a, false),
        (3, 0xf0, 0xfa, false),
        (3, 0xfa, 0xfb, true),
        (3, 0xff, 0, true),
    ] {
        let mut bus = PalPacketBus {
            inner: SmsBus::new(rom.clone(), 0xff),
            samples: Some([sample].into()),
            line: 0,
            ports: Vec::new(),
        };
        bus.write(0xfffe, defs["rt_chr_packet_pal_final"].0);
        bus.write(0xd40c, phase);
        bus.write(0xd40d, last);
        let mut cpu = Cpu::new();
        pal_invoke(&mut cpu, &mut bus, defs["rt_chr_packet_pal_final"].1);
        assert_eq!(cpu.f & 1 != 0, carry);
        assert!(bus.ports.is_empty());
    }
    for (phase, last, sample, limit) in [
        (1, 0xf0, 0xff, 128),
        (2, 0, 0x0a, 128),
        (3, 0xd2, 0xf2, 128),
        (3, 0xf2, 0xf3, 64),
        (3, 0xf3, 0xf6, 64),
        (3, 0xf6, 0xf7, 16),
        (3, 0xf7, 0xf9, 16),
        (3, 0xf9, 0xfa, 1),
    ] {
        for remaining in [1u16, 16, 64, 128, 256] {
            let mut bus = PalPacketBus {
                inner: SmsBus::new(rom.clone(), 0xff),
                samples: Some([sample].into()),
                line: 0,
                ports: Vec::new(),
            };
            bus.write(0xfffe, defs["rt_chr_packet_pal_admit"].0);
            bus.write(0xd40c, phase);
            bus.write(0xd40d, last);
            let mut cpu = Cpu::new();
            cpu.set_bc(remaining);
            cpu.set_de(0x3692);
            cpu.set_hl(0x4796);
            pal_invoke(&mut cpu, &mut bus, defs["rt_chr_packet_pal_admit"].1);
            assert_eq!(u16::from(cpu.a), remaining.min(limit));
            assert_eq!((cpu.bc(), cpu.de(), cpu.hl()), (remaining, 0x3692, 0x4796));
            assert!(bus.ports.is_empty());
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PAL_PROJECT frozen PAL240 positive-queue guard"]
fn assembled_pal_zero_queue_descriptor_traps_before_mapping_or_vdp_output() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PAL_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut bus = PalPacketBus {
        inner: SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff),
        samples: Some([].into()),
        line: 0,
        ports: Vec::new(),
    };
    bus.write(0xfffe, defs["_m3p_flush"].0);
    bus.write(0xfffc, 12);
    bus.write(0xffff, 23);
    bus.write(0xcb14, 19);
    // One complete eight-byte descriptor with a deliberately invalid zero
    // length. Its requested SRAM bank differs from the actual mapped bank.
    for (offset, value) in [0, 0x80, 0, 0x77, 0, 0, 8, 0].into_iter().enumerate() {
        bus.write(0xda00 + offset as u16, value);
    }
    bus.write(0xd9f8, 8);
    bus.write(0xd9f9, 0xda);
    bus.write(0xd9f6, 1);
    let mapping = (bus.inner.mapper_control, bus.inner.slot_bank);
    let vram = bus.inner.vram;
    let cram = bus.inner.cram;
    let source = bus.inner.ram[0xa80..0xb00].to_vec();
    let mut cpu = Cpu::new();
    pal_invoke(&mut cpu, &mut bus, defs["_m3p_flush"].1);
    assert_eq!(bus.inner.read(0xcb1d), 0xea);
    assert_eq!((bus.inner.mapper_control, bus.inner.slot_bank), mapping);
    assert_eq!(bus.inner.read(0xcb14), 19);
    assert!(bus.ports.is_empty());
    assert_eq!(bus.inner.vram, vram);
    assert_eq!(bus.inner.cram, cram);
    assert_eq!(&bus.inner.ram[0xa80..0xb00], source);
}

#[test]
#[ignore = "requires TRACE_CNROM_PAL_PROJECT frozen PAL240 adapter"]
fn assembled_pal_packets_cover_all240_rows_seams_bottom_sprites_and_hidden_sat() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PAL_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    let mut chr = vec![0u8; 8192];
    for (tile, (lo, hi)) in [(0xff, 0), (0, 0xff), (0xff, 0xff)].into_iter().enumerate() {
        chr[tile * 16..tile * 16 + 8].fill(lo);
        chr[tile * 16 + 8..tile * 16 + 16].fill(hi);
    }
    // Distinct sprite rows in both pattern tables, both halves and flips.
    for base in [0, 0x1000] {
        for tile in 4..8 {
            for row in 0..8 {
                chr[base + tile * 16 + row] = 0xf0u8.rotate_right((row + tile) as u32);
                chr[base + tile * 16 + 8 + row] = 0x33u8.rotate_left((row + tile) as u32);
            }
        }
    }
    for bank in 0..4 {
        rom[31 * BANK_SIZE + bank * 8192..31 * BANK_SIZE + (bank + 1) * 8192].copy_from_slice(&chr);
    }
    let mut bus = PalPacketBus {
        inner: SmsBus::new(rom, 0xff),
        samples: None,
        line: 0,
        ports: Vec::new(),
    };
    let mut cpu = Cpu::new();
    pal_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
    let mut frame = 0u32;
    let mut present = |bus: &mut PalPacketBus,
                       cpu: &mut Cpu,
                       ctrl: u8,
                       mask: u8,
                       origin: u16,
                       fine_x: u8,
                       mirror: u8,
                       oam: &[[u8; 4]],
                       patterned: bool| {
        frame += 1;
        bus.write(0xfffc, 8);
        for copy in [0x8800u16, 0x9000] {
            for physical in 0..2usize {
                for offset in 0..1024usize {
                    let value = if offset >= 960 {
                        if patterned { 0xe4 } else { 0 }
                    } else if patterned {
                        ((physical * 2 + offset / 32 + offset % 32 + usize::from(fine_x)) % 3) as u8
                    } else {
                        0
                    };
                    bus.write(copy + (physical * 1024 + offset) as u16, value);
                }
            }
        }
        for offset in 0..256u16 {
            bus.write(0x9800 + offset, 0xff);
        }
        for (slot, sprite) in oam.iter().enumerate() {
            for (offset, &byte) in sprite.iter().enumerate() {
                bus.write(0x9800 + (slot * 4 + offset) as u16, byte);
            }
        }
        for record in [0x9900u16, 0x9940] {
            for offset in 0..64u16 {
                bus.write(record + offset, 0);
            }
            for page in 0..8u16 {
                bus.write(record + page, ((frame & 1) * 24) as u8 + page as u8);
            }
            bus.write(record + 8, ctrl);
            bus.write(record + 9, mask);
            bus.write(record + 12, origin as u8);
            bus.write(record + 13, (origin >> 8) as u8);
            bus.write(record + 14, fine_x);
            bus.write(record + 15, mirror);
            for offset in 0..32u16 {
                bus.write(
                    record + 16 + offset,
                    [0x0f, 0x30, 1, 0x16][usize::from(offset) & 3],
                );
            }
        }
        bus.write(0xd400, 2);
        for (i, byte) in frame.to_le_bytes().into_iter().enumerate() {
            bus.write(0xd402 + i as u16, byte);
        }
        let frozen = bus.inner.cart_ram[0x800..0x1980].to_vec();
        for address in 0xca80..0xcb00 {
            bus.write(address, 0x5a);
        }
        let source_clock = bus.inner.ram[0xa80..0xb00].to_vec();
        let guest = bus.inner.ram[..0x800].to_vec();
        let source_ppu = bus.inner.ram[0x1300..0x1400].to_vec();
        let source_audio_input = bus.inner.ram[0x1500..0x1600].to_vec();
        bus.write(0xfffe, 17);
        bus.write(0xcb14, 19);
        bus.write(0xffff, 23);
        bus.write(0xfffc, 12);
        let mapping = (bus.inner.mapper_control, bus.inner.slot_bank);
        cpu.a = 0x71;
        cpu.f = 0x95;
        cpu.set_bc(0x2571);
        cpu.set_de(0x3692);
        cpu.set_hl(0x4796);
        cpu.iff1 = frame & 1 != 0;
        cpu.iff2 = cpu.iff1;
        let iff = cpu.iff1;
        pal_invoke(cpu, bus, defs["rt_cnrom_packet_present"].1);
        assert_eq!(bus.inner.read(0xcb1d), 0, "PAL frame{frame}");
        assert_eq!(bus.inner.read(0xd400), 0);
        assert_eq!(&bus.inner.ram[0x1406..0x140a], frame.to_le_bytes());
        assert_eq!(&bus.inner.ram[0xa80..0xb00], source_clock);
        assert_eq!(&bus.inner.ram[..0x800], guest);
        assert_eq!(&bus.inner.ram[0x1300..0x1400], source_ppu);
        assert_eq!(&bus.inner.ram[0x1500..0x1600], source_audio_input);
        assert_eq!(&bus.inner.cart_ram[0x800..0x1980], frozen);
        assert_eq!((bus.inner.mapper_control, bus.inner.slot_bank), mapping);
        assert_eq!(bus.inner.read(0xcb14), 19);
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
            (0x71, 0x95, 0x2571, 0x3692, 0x4796, iff, iff)
        );
        assert_eq!(bus.inner.vdp_regs[1] & 0x58, 0x48, "240-line enabled Mode4");
        assert_eq!(
            bus.inner.vdp_regs[1] & 2,
            if ctrl & 0x20 != 0 { 2 } else { 0 }
        );
        assert_eq!(bus.inner.vdp_regs[0] & 0x66, 6);
        for slot in oam.len()..64 {
            assert_eq!(bus.inner.vram[0x3f00 + slot], 0xef, "hidden slot{slot}");
        }
        for y in 0..240usize {
            for x in 0..256usize {
                let bg = if patterned {
                    let logical_y = 232 + usize::from((origin >> 12) & 7) + y;
                    let row = (logical_y % 240) / 8;
                    let logical_x = 248 + usize::from(fine_x) + x;
                    let col = (logical_x % 256) / 8;
                    let nt = usize::from(logical_x >= 256) | (usize::from(logical_y >= 240) << 1);
                    let physical = if mirror == 0 { nt & 1 } else { nt >> 1 };
                    let color = ((physical * 2 + row + col + usize::from(fine_x)) % 3 + 1) as u8;
                    color + 4 * (((col >> 1) & 1) + 2 * ((row >> 1) & 1)) as u8
                } else {
                    1
                };
                let bg = if x < 8 && mask & 2 == 0 { 0 } else { bg };
                let expected = literal_priority_pixel(&chr, oam, ctrl, mask, bg, x, y);
                let actual = sms_sprite_index(&bus.inner, x, y)
                    .unwrap_or_else(|| sms_background_index_height(&bus.inner, x, y, 240));
                assert_eq!(
                    actual, expected,
                    "PAL frame{frame} ctrl{ctrl:02X} pixel{x},{y}"
                );
            }
        }
    };
    for mirror in [0, 1] {
        for fine_y in [0u16, 7] {
            for fine_x in 0..8 {
                present(
                    &mut bus,
                    &mut cpu,
                    0,
                    0x1e,
                    (fine_y << 12) | (29 << 5) | 31,
                    fine_x,
                    mirror,
                    &[],
                    true,
                );
            }
        }
    }
    // Cover all source Ys222..255 in groups of8 so no artificial ninth-sprite
    // rejection hides edge rows. Transparent/behind cases retain native slots.
    for ctrl in [0u8, 0x20] {
        for flip in [0u8, 0x40, 0x80, 0xc0] {
            for group in 0..5usize {
                let oam: Vec<_> = (0..8usize)
                    .filter_map(|slot| {
                        let y = 222 + group * 8 + slot;
                        (y <= 255).then_some([
                            y as u8,
                            if ctrl & 0x20 != 0 { 5 } else { 4 },
                            flip,
                            16 + (slot * 24) as u8,
                        ])
                    })
                    .collect();
                present(&mut bus, &mut cpu, ctrl, 0x1e, 0, 0, 0, &oam, false);
            }
        }
    }
    for ctrl in [0u8, 0x20] {
        for mode in 0..4 {
            let mut oam = vec![
                [
                    231,
                    if mode == 0 { 3 } else { 4 },
                    0x20,
                    if mode == 1 {
                        0
                    } else if mode == 2 {
                        255
                    } else {
                        16
                    }
                ];
                8
            ];
            oam.push([231, 4, 1, 16]);
            present(
                &mut bus,
                &mut cpu,
                ctrl,
                if mode == 1 { 0x18 } else { 0x1e },
                0,
                0,
                0,
                &oam,
                false,
            );
            // Priority→normal restores previously masked sprite patterns.
            present(
                &mut bus,
                &mut cpu,
                ctrl,
                0x1e,
                0,
                0,
                0,
                &[[231, 4, 0, 16]],
                false,
            );
        }
    }
    eprintln!("PAL literal packets={frame}, full240 pixel/state/mapping checks");
}

fn sms_sprite_index(bus: &SmsBus, x: usize, y: usize) -> Option<u8> {
    assert_eq!(
        bus.vdp_regs[1] & 1,
        0,
        "literal observer expects unzoomed sprites"
    );
    let tall = bus.vdp_regs[1] & 2 != 0;
    let height = if tall { 16 } else { 8 };
    let mut active = 0;
    for slot in 0..64 {
        let top = usize::from(bus.vram[0x3f00 + slot].wrapping_add(1));
        if y < top || y >= top + height {
            continue;
        }
        active += 1;
        if active > 8 {
            break;
        }
        let left = usize::from(bus.vram[0x3f80 + slot * 2]);
        if x < left || x >= left + 8 {
            continue;
        }
        let mut tile = usize::from(bus.vram[0x3f81 + slot * 2]);
        if tall {
            tile &= !1;
        }
        let offset = 0x2000 + tile * 32 + (y - top) * 4;
        let color = (0..4).fold(0, |color, plane| {
            color | (((bus.vram[offset + plane] >> (7 - (x - left))) & 1) << plane)
        });
        if color != 0 {
            return Some(16 + color);
        }
    }
    None
}

// Literal NES selector: select eight by Y, find the first opaque source pixel,
// then apply BG priority. This deliberately does not rescan earlier sprites
// per target or inspect the production staging/ownership/cache data.
fn literal_priority_pixel(
    chr: &[u8],
    oam: &[[u8; 4]],
    ctrl: u8,
    mask: u8,
    bg: u8,
    x: usize,
    y: usize,
) -> u8 {
    if mask & 0x10 == 0 || (x < 8 && mask & 4 == 0) {
        return bg;
    }
    let height = if ctrl & 0x20 != 0 { 16 } else { 8 };
    let mut selected = 0;
    for &[sprite_y, tile, attr, sprite_x] in oam {
        let top = usize::from(sprite_y) + 1;
        if y < top || y >= top + height {
            continue;
        }
        selected += 1;
        if selected > 8 {
            break;
        }
        let left = usize::from(sprite_x);
        if x < left || x >= left + 8 {
            continue;
        }
        let row = if attr & 0x80 != 0 {
            height - 1 - (y - top)
        } else {
            y - top
        };
        let address = if height == 16 {
            usize::from(tile & 1) * 0x1000
                + usize::from(tile & 0xfe) * 16
                + (row / 8) * 16
                + row % 8
        } else {
            usize::from(ctrl & 8 != 0) * 0x1000 + usize::from(tile) * 16 + row
        };
        let bit = if attr & 0x40 != 0 {
            x - left
        } else {
            7 - (x - left)
        };
        let color = ((chr[address] >> bit) & 1) | (((chr[address + 8] >> bit) & 1) << 1);
        if color == 0 {
            continue;
        }
        return if attr & 0x20 != 0 && bg & 3 != 0 {
            bg
        } else {
            16 + 4 * (attr & 3) + color
        };
    }
    bg
}

#[test]
fn literal_priority_selector_owns_before_background_and_counts_transparent_slots() {
    let mut chr = [0; 8192];
    chr[16..24].fill(0x80);
    chr[32..40].fill(0xff);
    assert_eq!(
        literal_priority_pixel(
            &chr,
            &[[31, 1, 0x20, 16], [31, 2, 1, 16]],
            0,
            0x1e,
            1,
            16,
            32
        ),
        1
    );
    assert_eq!(
        literal_priority_pixel(
            &chr,
            &[[31, 1, 0x20, 16], [31, 2, 1, 16]],
            0,
            0x1e,
            1,
            17,
            32
        ),
        21
    );
    let mut oam = vec![[31, 0, 0, 255]; 8];
    oam.push([31, 2, 0, 16]);
    assert_eq!(literal_priority_pixel(&chr, &oam, 0, 0x1e, 1, 16, 32), 1);
}

#[test]
#[ignore = "requires TRACE_CNROM_PRIORITY_PROJECT assembled behind-BG packet adapter"]
fn assembled_priority_packets_match_literal_first_opaque_selection_and_cache_transitions() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PRIORITY_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    let mut banks = Vec::new();
    for bank in 0..4usize {
        let mut chr = vec![0u8; 8192];
        // BG0 includes opaque pixels with exactly the backdrop's RGB. Testing
        // palette indices/opacity must distinguish them despite equal colors.
        for (row, byte) in chr[..8].iter_mut().enumerate() {
            *byte = if row & 1 == 0 { 0xaa } else { 0x55 };
        }
        for (tile, lo, hi) in [
            (2, 0xf0u8, 0x0cu8),
            (3, 0x33, 0xc0),
            (4, 0xff, 0),
            (5, 0, 0xff),
        ] {
            for base in [0, 0x1000] {
                for row in 0..8 {
                    chr[base + tile * 16 + row] = lo.rotate_right((row + bank) as u32);
                    chr[base + tile * 16 + 8 + row] = hi.rotate_left((row + bank) as u32);
                }
            }
        }
        let offset = 31 * BANK_SIZE + bank * 8192;
        rom[offset..offset + 8192].copy_from_slice(&chr);
        banks.push(chr);
    }
    let mut cases = vec![
        (0u8, 0x1eu8, 0usize, 0usize, vec![[31, 1, 0x20, 16]]),
        (0u8, 0x1eu8, 0usize, 0usize, vec![[31, 1, 0x20, 16]]),
    ];
    // Normal→priority→normal on identical target identities must not reuse
    // masked rows. Move BG fineX and predecessor overlap between packets.
    for behind in [false, true, false, true] {
        cases.push((
            0u8,
            0x1eu8,
            0usize,
            0usize,
            vec![
                [31, 2, if behind { 0x20 } else { 0 }, 16],
                [31, 4, 1, 16],
                [31, 5, 2, 18],
            ],
        ));
    }
    for bank in [0usize, 3] {
        for ctrl in [0u8, 8, 0x20] {
            for flip in [0u8, 0x40, 0x80, 0xc0] {
                let tile = if ctrl & 0x20 != 0 {
                    2 + (bank & 1) as u8
                } else {
                    2
                };
                cases.push((
                    ctrl,
                    0x1e,
                    bank,
                    3,
                    vec![[31, tile, flip | 0x20, 5], [31, 4, 1, 5], [31, 5, 2, 9]],
                ));
            }
        }
    }
    for mask in [0x18, 0x1a, 0x1c, 0x1e, 0x16, 0x0a] {
        for kind in [0, 1, 2] {
            let mut oam = vec![
                [
                    31,
                    if kind == 1 { 1 } else { 4 },
                    0x20,
                    if kind == 2 { 255 } else { 0 }
                ];
                8
            ];
            oam.push([31, 4, 1, 16]);
            cases.push((0, mask, 0, 7, oam));
        }
    }
    for x in 0..=8u8 {
        for flip in [0u8, 0x40] {
            cases.push((0, 0x18, 3, 3, vec![[31, 2, 0x20 | flip, x], [31, 4, 1, x]]));
        }
    }
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
    bus.vram[0x2000..0x3000].fill(0xff); // Unknown VRAM, equal-zero SRAM is not residency.
    for (frame, (ctrl, mask, bank, fine_x, oam)) in cases.into_iter().enumerate() {
        let fine_y = if frame & 1 == 0 { 0usize } else { 7 };
        bus.write(0xfffc, 8);
        for copy in [0x8800u16, 0x9000] {
            for physical in 0..2usize {
                for offset in 0..1024usize {
                    let value = if offset >= 960 {
                        0
                    } else {
                        ((physical + offset / 32 + offset % 32) & 1) as u8
                    };
                    bus.write(copy + (physical * 1024 + offset) as u16, value);
                }
            }
        }
        for offset in 0..256u16 {
            bus.write(0x9800 + offset, 0xe0);
        }
        for (slot, sprite) in oam.iter().enumerate() {
            for (offset, &byte) in sprite.iter().enumerate() {
                bus.write(0x9800 + (slot * 4 + offset) as u16, byte);
            }
        }
        for record in [0x9900u16, 0x9940] {
            for offset in 0..64u16 {
                bus.write(record + offset, 0);
            }
            for page in 0..8u16 {
                bus.write(record + page, (bank * 8) as u8 + page as u8);
            }
            bus.write(record + 8, ctrl);
            bus.write(record + 9, mask);
            bus.write(record + 12, 0xbf);
            bus.write(record + 13, (fine_y << 4) as u8 | 3); // CoarseY29/X31.
            bus.write(record + 14, fine_x as u8);
            bus.write(record + 15, 1);
            for offset in 0..32u16 {
                bus.write(
                    record + 16 + offset,
                    [0x0f, 0x0f, 1, 0x16][usize::from(offset) & 3],
                );
            }
        }
        bus.write(0xd400, 2);
        bus.write(0xd402, frame as u8);
        let resident_before = bus.ram[0x1410..0x1420].to_vec();
        let source_before = bus.ram[0xa80..0xb00].to_vec();
        let ppu_before = bus.ram[0x1300..0x1400].to_vec();
        let frozen_before = bus.cart_ram[0x800..0x1980].to_vec();
        let mut acknowledged = false;
        let mut sprite_writes = 0;
        let mut previous_writes = bus.vram_writes;
        let mut minimum_sp = 0xffff;
        packet_invoke_observed(
            &mut cpu,
            &mut bus,
            defs["rt_cnrom_packet_present"].1,
            |cpu, bus| {
                minimum_sp = minimum_sp.min(cpu.sp);
                if !acknowledged {
                    assert_eq!(
                        &bus.ram[0x1410..0x1420],
                        resident_before,
                        "residency before committed callback"
                    );
                }
                if cpu.pc == defs["rt_cnrom_packet_committed"].1 {
                    acknowledged = true;
                }
                if bus.vram_writes != previous_writes {
                    let next = (u16::from(bus.vdp_addr_high) << 8) | u16::from(bus.vdp_addr_low);
                    if (0x2000..0x3000).contains(&next.wrapping_sub(1)) {
                        sprite_writes += bus.vram_writes - previous_writes;
                    }
                    previous_writes = bus.vram_writes;
                }
            },
        );
        assert!(acknowledged);
        assert!(
            minimum_sp >= 0xde40,
            "priority nesting exceeded stack reservation"
        );
        assert_eq!(&bus.ram[0xa80..0xb00], source_before);
        assert_eq!(&bus.ram[0x1300..0x1400], ppu_before);
        assert_eq!(&bus.cart_ram[0x800..0x1980], frozen_before);
        for (byte, before) in resident_before.iter().enumerate() {
            assert_eq!(bus.ram[0x1410 + byte], before | bus.ram[0x1420 + byte]);
        }
        if frame == 0 {
            assert!(
                sprite_writes >= 32,
                "nonresident equal-zero pattern must upload"
            );
        }
        if frame == 1 {
            assert_eq!(
                sprite_writes, 0,
                "unchanged resident priority pattern stays clean"
            );
        }
        if bus.ram[0x140b] != 0 && mask & 0x10 != 0 {
            assert_eq!(&bus.ram[0x1800..0x1802], &[0xff, 0xff]);
        }
        for y in 30..50usize {
            for x in 0..256usize {
                let bg = if mask & 8 != 0 && (x >= 8 || mask & 2 != 0) {
                    let logical_y = 232 + fine_y + y;
                    let logical_x = 248 + fine_x + x;
                    let row = (logical_y % 240) / 8;
                    let col = (logical_x % 256) / 8;
                    let physical = usize::from(logical_y >= 240);
                    let low = if (row + col + physical) & 1 != 0 {
                        0
                    } else if logical_y & 1 == 0 {
                        0xaau8
                    } else {
                        0x55
                    };
                    (low >> (7 - ((x + fine_x) & 7))) & 1
                } else {
                    0
                };
                let expected = literal_priority_pixel(&banks[bank], &oam, ctrl, mask, bg, x, y);
                let actual = sms_sprite_index(&bus, x, y)
                    .unwrap_or_else(|| sms_background_index(&bus, x, y));
                assert_eq!(
                    actual, expected,
                    "frame{frame} ctrl{ctrl:02X} mask{mask:02X} bank{bank} fine{fine_x} pixel{x},{y}"
                );
            }
        }
    }
}

#[test]
fn literal_packet_observer_reads_planar_bits_flips_and_full_32_row_wrap() {
    let mut bus = SmsBus::new(vec![0; 0x8000], 0xff);
    for (row, &(low, high)) in LITERAL_PLANES.iter().enumerate() {
        bus.vram[32 + row * 4..36 + row * 4].copy_from_slice(&[low, high, 0, 0]);
    }
    // Only NT row31 uses the nonblank tile. Y=223 + scroll25 selects row31,
    // not row3 as a modulo224 observer would. X=0 with reg8=1 wraps to255.
    bus.vdp_regs[8] = 1;
    for attributes in [0, 2, 4, 6, 8, 14] {
        for col in 0..32 {
            let entry = 0x3700 + 2 * (31 * 32 + col);
            bus.vram[entry] = 1;
            bus.vram[entry + 1] = attributes;
        }
        for row in 0..8 {
            bus.vdp_regs[9] = 25 + row as u8;
            for x in 0usize..16 {
                let column = x.wrapping_sub(1) & 7;
                let expected = LITERAL_PIXELS[if attributes & 4 == 0 { row } else { 7 - row }]
                    [if attributes & 2 == 0 {
                        column
                    } else {
                        7 - column
                    }]
                    + if attributes & 8 == 0 { 0 } else { 16 };
                assert_eq!(sms_background_index(&bus, x, 223), expected);
                assert_eq!(sms_background_index(&bus, x, 222 - row), 0);
            }
        }
    }
    bus.vdp_regs[9] = 33; // 223+33 wraps to row0, not row31.
    assert_eq!(sms_background_index(&bus, 0, 223), 0);
}

fn packet_invoke(cpu: &mut Cpu, bus: &mut SmsBus, entry: u16) {
    packet_invoke_observed(cpu, bus, entry, |_, _| {});
}

fn packet_invoke_observed(
    cpu: &mut Cpu,
    bus: &mut SmsBus,
    entry: u16,
    mut observe: impl FnMut(&Cpu, &mut SmsBus),
) {
    cpu.pc = entry;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    for _ in 0..6_000_000 {
        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
            break;
        }
        observe(cpu, bus);
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    assert_eq!(bus.read(0xcb1d), 0, "packet trap PC={:04X}", cpu.pc);
    assert_eq!(cpu.pc, 7, "packet helper did not return");
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled hardware-capability packet fixture"]
fn assembled_cnrom_literal_packet_is_immutable_and_retires_after_publication() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PACKET_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    // This is literal test art injected into the test-owned ROM, not extracted
    // commercial art or a claim that source6502 produced the packet.
    let raw = 31 * BANK_SIZE;
    for (row, &(low, high)) in LITERAL_PLANES.iter().enumerate() {
        rom[raw + row] = low;
        rom[raw + row + 8] = high;
    }
    rom[raw + 16..raw + 24].fill(0xff); // Tile1: solid sprite color1.
    rom[raw + 24..raw + 32].fill(0);
    // Later live bank3 would show a different solid background.
    rom[raw + 3 * 8192..raw + 3 * 8192 + 8].fill(0xff);
    rom[raw + 3 * 8192 + 8..raw + 3 * 8192 + 16].fill(0xff);
    for iff in [false, true] {
        let mut bus = SmsBus::new(rom.clone(), 0xff);
        let mut cpu = Cpu::new();
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
        bus.write(0xfffc, 8);
        for base in [0x8800u16, 0x9000] {
            for offset in 0..0x800 {
                bus.write(
                    base + offset,
                    if offset & 0x3ff >= 0x3c0 { 0xe4 } else { 0 },
                );
            }
        }
        for offset in 0..256 {
            bus.write(0x9800 + offset, 0xe0);
        }
        for (offset, value) in [31, 1, 2, 24].into_iter().enumerate() {
            bus.write(0x9800 + offset as u16, value);
        }
        for base in [0x9900u16, 0x9940] {
            for offset in 0..64 {
                bus.write(base + offset, 0);
            }
            for page in 0..8 {
                bus.write(base + page, page as u8);
            }
            bus.write(base + 9, 0x1e); // Both layers and both left edges.
            for offset in 0..32 {
                bus.write(
                    base + 16 + offset,
                    [0x0f, 0x30, 0x01, 0x16][offset as usize & 3],
                );
            }
        }
        let frozen = bus.cart_ram[0x800..0x1980].to_vec();
        bus.write(0xd400, 2); // Explicitly sealed literal fixture, not PPU proof.
        for (offset, value) in [0x78, 0x56, 0x34, 0x12].into_iter().enumerate() {
            bus.write(0xd402 + offset as u16, value);
        }
        // Mutate every relevant LIVE source. None belongs to frozen packet.
        bus.write(0xc810, 3);
        bus.write(0xcb08, 0x38);
        bus.write(0xcb09, 0);
        for offset in 0..0x800 {
            bus.write(0x8000 + offset, 0xff);
        }
        for offset in 0..256 {
            bus.write(0xc900 + offset, 0xff);
        }
        for offset in 0..32 {
            bus.write(0xc860 + offset, 0x30);
        }
        for offset in 0..128 {
            bus.write(0xca80 + offset, 0x5a);
        }
        let source_clock = bus.ram[0xa80..0xb00].to_vec();
        bus.write(0xfffe, 17);
        bus.write(0xcb14, 19); // Intentionally different shadow/hardware bank.
        bus.write(0xffff, 23);
        bus.write(0xfffc, 12);
        bus.write(0xcb15, 0xa6);
        bus.write(0xcb18, 0x59);
        bus.write(0xcb27, 0x62);
        cpu.a = 0x71;
        cpu.f = 0x95;
        cpu.set_bc(0x2486);
        cpu.set_de(0x53a9);
        cpu.set_hl(0x1278);
        cpu.iff1 = iff;
        cpu.iff2 = iff;
        let mapping = (bus.mapper_control, bus.slot_bank);
        let mut retirement_observed = false;
        packet_invoke_observed(
            &mut cpu,
            &mut bus,
            defs["rt_cnrom_packet_present"].1,
            |cpu, bus| {
                if cpu.pc == defs["rt_cnrom_packet_committed"].1 {
                    retirement_observed = true;
                    assert_eq!(bus.read(0xd400), 3, "backpressure until final callback");
                    for offset in 0..0x800 {
                        let shadow = if offset < 0x700 {
                            0xc00 + offset
                        } else {
                            0x1600 + offset - 0x700
                        };
                        assert_eq!(
                            bus.ram[shadow],
                            bus.vram[0x3700 + offset],
                            "retirement NT shadow{offset:03X}"
                        );
                    }
                    assert_eq!(&bus.ram[0x1700..0x1740], &bus.vram[0x3f00..0x3f40]);
                    assert_eq!(&bus.ram[0x1740..0x17c0], &bus.vram[0x3f80..0x4000]);
                }
            },
        );
        assert!(retirement_observed);
        assert_eq!(
            (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
            (0x71, 0x95, 0x2486, 0x53a9, 0x1278)
        );
        assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
        assert_eq!((bus.mapper_control, bus.slot_bank), mapping);
        assert_eq!(
            (
                bus.read(0xcb14),
                bus.read(0xcb15),
                bus.read(0xcb18),
                bus.read(0xcb27)
            ),
            (19, 0xa6, 0x59, 0x62)
        );
        assert_eq!(bus.read(0xd400), 0);
        assert_eq!(&bus.ram[0x1406..0x140a], &[0x78, 0x56, 0x34, 0x12]);
        assert_eq!(&bus.ram[0xa80..0xb00], source_clock);
        assert_eq!(&bus.cart_ram[0x800..0x1980], frozen);
        assert_ne!(bus.vdp_regs[1] & 0x40, 0, "whole display committed");
        assert_eq!(
            bus.vdp_regs[0] & 0x66,
            0x06,
            "Mode4 without legacy scroll locks"
        );
        assert_eq!(bus.vdp_regs[1] & 0x18, 0x10, "224-line Mode4 selection");
        for y in 0..224 {
            for x in 0..256 {
                let color = LITERAL_PIXELS[y & 7][x & 7];
                let quadrant = ((x >> 4) & 1) + 2 * ((y >> 4) & 1);
                let expected = if color == 0 {
                    0
                } else {
                    color + quadrant as u8 * 4
                };
                assert_eq!(
                    sms_background_index(&bus, x, y),
                    expected,
                    "pixel{x},{y} IFF{iff}"
                );
            }
        }
        // Existing explicit coarse palette adaptation, not analog NES color
        // fidelity. These literal entries are fixed in the fixture contract.
        for index in 0..32 {
            assert_eq!(bus.cram[index], [0, 0x3f, 0x10, 2][index & 3]);
        }
        assert_eq!(bus.vram[0x3f00], 31);
        assert_eq!(bus.vram[0x3f80], 24);
        let sprite = 0x2000 + usize::from(bus.vram[0x3f81]) * 32;
        for row in 0..8 {
            assert_eq!(
                &bus.vram[sprite + row * 4..sprite + row * 4 + 4],
                &[0xff, 0, 0, 0xff]
            );
        }
        // A later explicitly blank packet changes its frozen universal color.
        // No old visible tile may be modified merely to publish a backdrop.
        let displayed_tiles = bus.vram;
        bus.write(0xfffc, 8);
        for record in [0x9900u16, 0x9940] {
            bus.write(record + 9, 0);
            bus.write(record + 16, 0x30);
        }
        bus.write(0xd400, 2);
        bus.write(0xd402, 0x79);
        bus.write(0xfffc, 12);
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1);
        assert_eq!(bus.read(0xd400), 0);
        assert_eq!(&bus.ram[0x1406..0x140a], &[0x79, 0x56, 0x34, 0x12]);
        assert_eq!(bus.vdp_regs[1] & 0x40, 0);
        assert_eq!(bus.vdp_regs[7] & 15, 0);
        assert_eq!(bus.cram[16], 0x3f);
        assert_eq!(bus.vram, displayed_tiles);
        assert_eq!(&bus.ram[0xa80..0xb00], source_clock);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_PACKET_PROJECT assembled hardware-capability packet fixture"]
fn assembled_cnrom_capture_and_host_irq_keep_source_and_banked_caller_ownership() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_PACKET_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let rom = std::fs::read(path.join("sms.sms")).unwrap();
    for iff in [false, true] {
        let mut bus = SmsBus::new(rom.clone(), 0xff);
        let mut cpu = Cpu::new();
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
        packet_invoke(&mut cpu, &mut bus, defs["rt_source_input_init"].1);
        bus.write(0xfffc, 8);
        for offset in 0..0x800u16 {
            bus.write(0x8000 + offset, (offset ^ (offset >> 5)) as u8);
        }
        for offset in 0..256u16 {
            bus.write(0xc900 + offset, offset as u8 ^ 0x69);
        }
        for offset in 0..32u16 {
            bus.write(0xc860 + offset, offset as u8);
        }
        bus.write(0xc810, 2);
        bus.write(0xc816, 5); // Fine X, distinct from raw scroll metadata.
        bus.write(0xcb08, 0x20);
        bus.write(0xcb09, 0x1e);
        bus.write(0xcb0c, 0x93);
        bus.write(0xcb0d, 0xb7);
        for offset in 0..128u16 {
            bus.write(0xca80 + offset, offset as u8 ^ 0xa5);
        }
        let source_clock = bus.ram[0xa80..0xb00].to_vec();
        let live = bus.cart_ram[..0x800].to_vec();
        let oam = bus.ram[0x900..0xa00].to_vec();
        bus.write(0xfffe, 17);
        bus.write(0xcb14, 19);
        bus.write(0xffff, 23);
        bus.write(0xfffc, 12);
        let maps = (bus.mapper_control, bus.slot_bank);
        cpu.a = 0x71;
        cpu.f = 0x95;
        cpu.set_bc(0x2486);
        cpu.set_de(0x53a9);
        cpu.set_hl(0x2bde); // Resolved visible origin, not CB0C/CB0D.
        cpu.iff1 = iff;
        cpu.iff2 = iff;
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_capture"].1);
        assert_eq!(
            (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
            (0x71, 0x95, 0x2486, 0x53a9, 0x2bde)
        );
        assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
        assert_eq!((bus.mapper_control, bus.slot_bank), maps);
        assert_eq!(bus.read(0xcb14), 19);
        assert_eq!(&bus.cart_ram[0x800..0x1000], live);
        assert_eq!(&bus.cart_ram[0x1000..0x1800], live);
        assert_eq!(&bus.cart_ram[0x1800..0x1900], oam);
        let mut record = [0; 64];
        record[..15].copy_from_slice(&[
            16, 17, 18, 19, 20, 21, 22, 23, 0x20, 0x1e, 0x93, 0xb7, 0xde, 0x2b, 5,
        ]);
        record[15] = 1; // Literal fixture NES header is horizontal mirroring.
        for offset in 0..32 {
            record[16 + offset] = offset as u8;
        }
        assert_eq!(&bus.cart_ram[0x1900..0x1940], record);
        assert_eq!(&bus.cart_ram[0x1940..0x1980], record);
        assert_eq!(bus.read(0xd400), 1);
        assert_eq!(&bus.ram[0x1402..0x1406], &source_clock[8..12]);
        assert_eq!(&bus.ram[0xa80..0xb00], source_clock);

        // Direct host interrupt service uses an actual VBlank status read.
        // It may sample physical pending buttons, but cannot advance source
        // clock/APU, serial cursor/latch, packet state or guest registers.
        let envelope = bus.ram[0x1400..0x1440].to_vec();
        let apu = bus.ram[0x1500..0x15e0].to_vec();
        let serial = bus.ram[0x15e2..0x15ee].to_vec();
        bus.vdp_status_override = Some(0x80);
        cpu.iff1 = false;
        cpu.iff2 = false;
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_sms_interrupt"].1);
        assert_eq!(
            (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
            (0x71, 0x95, 0x2486, 0x53a9, 0x2bde)
        );
        assert_eq!((bus.mapper_control, bus.slot_bank), maps);
        assert_eq!(bus.read(0xcb14), 19);
        assert_eq!((cpu.iff1, cpu.iff2), (true, true));
        assert_eq!(&bus.ram[0xa80..0xb00], source_clock);
        assert_eq!(&bus.ram[0x1400..0x1440], envelope);
        assert_eq!(&bus.ram[0x1500..0x15e0], apu);
        assert_eq!(&bus.ram[0x15e2..0x15ee], serial);
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_RENDERED_PROJECT genuine translated hardware-rendered-frame fixture"]
fn assembled_translated_source_frame_matches_every_literal_pixel_and_frozen_packet() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_RENDERED_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    let mut cpu = Cpu::new();
    let mut rendered_commit = None;
    let mut captures = Vec::new();
    let mut validations = Vec::new();
    for step in 0..45_000_000 {
        if bus.read(0xcb1d) != 0 {
            break;
        }
        if cpu.pc == defs["rt_cnrom_packet_committed"].1 && bus.read(0xd402) == 2 {
            rendered_commit = Some((
                step,
                u32::from_le_bytes(bus.ram[0xa80..0xa84].try_into().unwrap()),
            ));
        }
        let cycle = u32::from_le_bytes(bus.ram[0xa80..0xa84].try_into().unwrap());
        if cpu.pc == defs["rt_cnrom_packet_capture"].1 {
            captures.push(cycle);
        }
        if cpu.pc == defs["rt_cnrom_packet_validate_complete"].1 {
            validations.push(cycle);
        }
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(bus.read(0xcb1d), 0xe8);
    assert_eq!(bus.read(0xc7ff), 0xa5);
    assert!(
        rendered_commit.is_some(),
        "whole source interval2 was published"
    );
    assert_eq!(
        u32::from_le_bytes(bus.ram[0xa80..0xa84].try_into().unwrap()),
        87531
    );
    assert_eq!(&bus.ram[0x1406..0x140a], &[2, 0, 0, 0]);
    assert_eq!(captures, vec![0, 29781, 59561]);
    assert_eq!(validations, vec![27280, 57061, 86841]);
    // Frame1 has rendering enabled before pre-render and skips one dot.
    // 87531*3 -(89342+89341) =83910 =246*341+24, not the blank-frame23.
    assert_eq!(&bus.ram[0xa84..0xa88], &[24, 0, 246, 0]);
    assert_eq!(bus.vdp_regs[0] & 0x66, 6);
    assert_eq!(bus.vdp_regs[1] & 0x58, 0x50);
    for y in 0..224 {
        for x in 0..256 {
            assert_eq!(
                sms_background_index(&bus, x, y),
                LITERAL_PIXELS[y & 7][x & 7],
                "translated pixel{x},{y}"
            );
        }
    }
    for index in 0..32 {
        assert_eq!(bus.cram[index], [0, 0x3f, 0x10, 2][index & 3]);
    }
    assert_eq!(
        bus.vram[0x3f00], 0xe0,
        "all source sprites were hidden via2004"
    );
    assert!(bus.cart_ram[0x800..0x1800].iter().all(|&byte| byte == 0));
    assert_eq!(&bus.cart_ram[0x1900..0x1908], &[0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(&bus.cart_ram[0x1908..0x190f], &[0, 0x1e, 0, 0, 0, 0, 0]);
    eprintln!(
        "translated frame2 commit step/cycle: {:?}",
        rendered_commit.unwrap()
    );
}

#[test]
#[ignore = "requires TRACE_CNROM_COMPOSED_PROJECT assembled horizontal packet compositor"]
fn assembled_horizontal_packets_cover_all_shifts_neighbor_palettes_and_mirroring_seams() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_COMPOSED_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    // Three literal solid tiles, NES colors1/2/3. The expected pixel uses the
    // independently filled logical nametable; never renderer key/slot state.
    let raw = 31 * BANK_SIZE;
    for (tile, (low, high)) in [(0xff, 0), (0, 0xff), (0xff, 0xff)].into_iter().enumerate() {
        rom[raw + tile * 16..raw + tile * 16 + 8].fill(low);
        rom[raw + tile * 16 + 8..raw + tile * 16 + 16].fill(high);
    }
    for mirror in [0u8, 1] {
        //0vertical,1horizontal.
        for fine_y in [0u16, 7] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            let mut cpu = Cpu::new();
            packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
            // Reuse this exact presenter/cache through all eight shifts, while
            // changing every neighbor's logical tile identity between frames.
            for fine_x in 0..8usize {
                bus.write(0xfffc, 8);
                for copy in [0x8800u16, 0x9000] {
                    for physical in 0..2usize {
                        for offset in 0..1024usize {
                            let byte = if offset >= 960 {
                                0xe4
                            } else {
                                ((physical * 2 + offset / 32 + offset % 32 + fine_x) % 3) as u8
                            };
                            bus.write(copy + (physical * 1024 + offset) as u16, byte);
                        }
                    }
                }
                for offset in 0..256u16 {
                    bus.write(0x9800 + offset, 0xe0);
                }
                for record in [0x9900u16, 0x9940] {
                    for offset in 0..64u16 {
                        bus.write(record + offset, 0);
                    }
                    for page in 0..8u16 {
                        bus.write(record + page, page as u8);
                    }
                    bus.write(record + 9, 0x1e);
                    let origin = (fine_y << 12) | (29 << 5) | 31;
                    bus.write(record + 12, origin as u8);
                    bus.write(record + 13, (origin >> 8) as u8);
                    bus.write(record + 14, fine_x as u8);
                    bus.write(record + 15, mirror);
                    for offset in 0..32u16 {
                        bus.write(
                            record + 16 + offset,
                            [0x0f, 0x30, 1, 0x16][usize::from(offset) & 3],
                        );
                    }
                }
                bus.write(0xd400, 2);
                bus.write(0xd402, fine_x as u8);
                packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1);
                assert_eq!(
                    bus.vdp_regs[8] & 7,
                    0,
                    "composition must not expose an unrefreshed SMS left edge"
                );
                for y in 0..224usize {
                    let logical_y = 232 + usize::from(fine_y) + y;
                    let row = (logical_y % 240) / 8;
                    for x in 0..256usize {
                        let logical_x = 248 + fine_x + x;
                        let col = (logical_x % 256) / 8;
                        let nt =
                            usize::from(logical_x >= 256) | (usize::from(logical_y >= 240) << 1);
                        let physical = if mirror == 0 { nt & 1 } else { nt >> 1 };
                        let color = ((physical * 2 + row + col + fine_x) % 3 + 1) as u8;
                        let palette = ((col >> 1) & 1) + 2 * ((row >> 1) & 1);
                        assert_eq!(
                            sms_background_index(&bus, x, y),
                            color + 4 * palette as u8,
                            "mirror{mirror} fine{fine_x},{fine_y} pixel{x},{y}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_LAYERS_PROJECT assembled independent layer/left clipping adapter"]
fn assembled_packet_layers_and_independent_left_clips_preserve_literal_pixels_and_sat_occupancy() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_LAYERS_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    let raw = 31 * BANK_SIZE;
    rom[raw..raw + 8].fill(0xff); // BGtile0 opaquecolor1.
    for (base, low, high) in [(16, 0x96, 0x69), (0x1000, 0x96, 0x69), (0x1010, 0xcc, 0x33)] {
        rom[raw + base..raw + base + 8].fill(low);
        rom[raw + base + 8..raw + base + 16].fill(high);
    }
    let mut cases = Vec::new();
    for layers in [0, 8, 16, 24] {
        for clipping in [0, 2, 4, 6] {
            cases.push((layers | clipping, 3usize, 0u8, false));
        }
    }
    for x in 0..=8 {
        for (flip, tall) in [(0, false), (0x40, false), (0x80, true), (0xc0, true)] {
            cases.push((0x18, x, flip, tall));
        }
    }
    let mut bus = SmsBus::new(rom, 0xff);
    let mut cpu = Cpu::new();
    packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
    for (frame, (mask, x, flip, tall)) in cases.into_iter().enumerate() {
        bus.write(0xfffc, 8);
        for offset in 0..0x1000u16 {
            bus.write(0x8800 + offset, 0);
        }
        for offset in 0..256u16 {
            bus.write(0x9800 + offset, 0xe0);
        }
        for (offset, byte) in [31, 1, flip, x as u8].into_iter().enumerate() {
            bus.write(0x9800 + offset as u16, byte);
        }
        for record in [0x9900u16, 0x9940] {
            for offset in 0..64u16 {
                bus.write(record + offset, 0);
            }
            for page in 0..8u16 {
                bus.write(record + page, page as u8);
            }
            bus.write(record + 8, if tall { 0x20 } else { 0 });
            bus.write(record + 9, mask);
            bus.write(record + 12, 31); // CoarseX31 plus fineX3 crosses right seam.
            bus.write(record + 14, 3);
            bus.write(record + 15, 1);
            for offset in 0..32u16 {
                bus.write(
                    record + 16 + offset,
                    [0x0f, 0x30, 1, 0x16][usize::from(offset) & 3],
                );
            }
        }
        bus.write(0xd400, 2);
        bus.write(0xd402, frame as u8);
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1);
        if mask & 0x18 == 0 {
            assert_eq!(bus.vdp_regs[1] & 0x40, 0);
            assert_eq!(bus.cram[16], 0);
            continue;
        }
        assert_eq!(
            bus.vdp_regs[0] & 0x26,
            6,
            "no global SMS clip hides both layers"
        );
        if mask & 0x10 != 0 {
            assert_eq!(
                (bus.vram[0x3f00], bus.vram[0x3f80]),
                (31, x as u8),
                "clipped sprite must retain SAT occupancy"
            );
        }
        for y in 32..48usize {
            for screen_x in 0..24usize {
                let bg = if mask & 8 != 0 && (mask & 2 != 0 || screen_x >= 8) {
                    1
                } else {
                    0
                };
                let height = if tall { 16 } else { 8 };
                let visible = mask & 16 != 0
                    && y < 32 + height
                    && screen_x >= x
                    && screen_x < x + 8
                    && (mask & 4 != 0 || screen_x >= 8);
                let expected = if visible {
                    let row = if flip & 0x80 != 0 {
                        height - 1 - (y - 32)
                    } else {
                        y - 32
                    };
                    let bit = if flip & 0x40 != 0 {
                        screen_x - x
                    } else {
                        7 - (screen_x - x)
                    };
                    let low = if row < 8 { 0x96u8 } else { 0xcc };
                    16 + if low & (1 << bit) != 0 { 1 } else { 2 }
                } else {
                    bg
                };
                let actual = sms_sprite_index(&bus, screen_x, y)
                    .unwrap_or_else(|| sms_background_index(&bus, screen_x, y));
                assert_eq!(
                    actual, expected,
                    "mask{mask:02X} x{x} flip{flip:02X} tall{tall} pixel{screen_x},{y}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_LAYERS_PROJECT assembled independent layer/left clipping adapter"]
fn assembled_packet_transparent_clipped_and_right_edge_sprites_still_hide_the_ninth() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_LAYERS_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    let raw = 31 * BANK_SIZE;
    rom[raw..raw + 8].fill(0xff);
    rom[raw + 16..raw + 24].fill(0xff);
    for leading_kind in ["clipped", "transparent", "right-edge"] {
        let mut bus = SmsBus::new(rom.clone(), 0xff);
        let mut cpu = Cpu::new();
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1);
        bus.write(0xfffc, 8);
        for offset in 0..0x1000u16 {
            bus.write(0x8800 + offset, 0);
        }
        for offset in 0..256u16 {
            bus.write(0x9800 + offset, 0xe0);
        }
        for slot in 0..9u16 {
            let tile = if leading_kind == "transparent" && slot < 8 {
                2
            } else {
                1
            };
            let x = if slot == 8 {
                16
            } else if leading_kind == "right-edge" {
                255
            } else {
                0
            };
            for (offset, byte) in [31, tile, 0, x].into_iter().enumerate() {
                bus.write(0x9800 + slot * 4 + offset as u16, byte);
            }
        }
        for record in [0x9900u16, 0x9940] {
            for offset in 0..64u16 {
                bus.write(record + offset, 0);
            }
            for page in 0..8u16 {
                bus.write(record + page, page as u8);
            }
            bus.write(
                record + 9,
                if leading_kind == "clipped" {
                    0x1a
                } else {
                    0x1e
                },
            );
            bus.write(record + 15, 1);
            for offset in 0..32u16 {
                bus.write(
                    record + 16 + offset,
                    [0x0f, 0x30, 1, 0x16][usize::from(offset) & 3],
                );
            }
        }
        bus.write(0xd400, 2);
        packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1);
        for slot in 0..9 {
            assert_eq!(
                bus.vram[0x3f00 + slot],
                31,
                "{leading_kind} slot{slot} retains occupancy"
            );
        }
        assert_eq!(bus.vram[0x3f80 + 8 * 2], 16);
        for y in 32..40 {
            assert_eq!(
                sms_sprite_index(&bus, 16, y),
                None,
                "{leading_kind} eighth-slot cutoff"
            );
            assert_eq!(sms_background_index(&bus, 16, y), 1);
        }
    }
}
