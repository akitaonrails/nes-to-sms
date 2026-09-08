//! Synthetic fixed-PRG/CNROM bus contracts, not a rendered game or completion claim.
//!
//! Expected bytes and write sequences are literal fixture contracts. The
//! reference executes the real 6502 oracle without using `nes_rom::cnrom`.

use oracle_6502::Bus;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RESET: u16 = 0xc800;
const BANK_TAGS: [u8; 16] = [
    0x11, 0x21, 0x31, 0x41, 0x51, 0x61, 0x71, 0x81, 0x91, 0xa1, 0xb1, 0xc1, 0xd1, 0xe1, 0xf1, 0x01,
];

#[derive(Clone, Copy)]
struct Config {
    prg_banks: u8,
    chr_banks: u8,
    submapper: u8,
    ram: bool,
    vertical: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prg_banks: 2,
            chr_banks: 4,
            submapper: 2,
            ram: false,
            vertical: false,
        }
    }
}

struct Fixture {
    rom: Vec<u8>,
    profile: String,
    expected: Vec<(u16, u8)>,
    stop: u16,
}

struct Program {
    bytes: Vec<u8>,
    expected: Vec<(u16, u8)>,
}

impl Program {
    fn new() -> Self {
        Self {
            bytes: vec![0x78, 0xa2, 0x3f, 0x9a],
            expected: Vec::new(),
        }
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.emit(&[0xa9, value, 0x8d, addr as u8, (addr >> 8) as u8]);
    }

    fn save_a(&mut self, expected: u8) {
        let dest = 0x0300 + self.expected.len() as u16;
        assert!(dest < 0x0400);
        self.emit(&[0x8d, dest as u8, (dest >> 8) as u8]);
        self.expected.push((dest, expected));
    }

    fn read(&mut self, addr: u16, expected: u8) {
        self.emit(&[0xad, addr as u8, (addr >> 8) as u8]);
        self.save_a(expected);
    }

    fn ppu_addr(&mut self, addr: u16) {
        self.write(0x2006, (addr >> 8) as u8);
        self.write(0x2006, addr as u8);
    }

    fn chr_read(&mut self, addr: u16, expected: u8) {
        self.ppu_addr(addr);
        self.emit(&[0xad, 0x07, 0x20]); // Discard the old buffer explicitly.
        self.read(0x2007, expected);
    }

    fn finish(mut self, config: Config) -> Fixture {
        self.write(0x07ff, 0xa5);
        self.expected.push((0x07ff, 0xa5));
        let stop = RESET + self.bytes.len() as u16;
        self.emit(&[0x4c, stop as u8, (stop >> 8) as u8]);
        assert!(
            self.bytes.len() < 0x1000,
            "fixture code overlaps reserved data"
        );
        let mut prg = vec![0xea; usize::from(config.prg_banks) * 16384];
        let offset = usize::from(RESET - 0x8000) % prg.len();
        prg[offset..offset + self.bytes.len()].copy_from_slice(&self.bytes);
        for (addr, value) in [
            (0x8000u16, 0xff),
            (0xb800, 0xff),
            (0xb801, 2),
            (0xb802, 1),
            (0xb803, 0),
            (0xb804, 0x81),
        ] {
            let offset = usize::from(addr - 0x8000) % prg.len();
            prg[offset] = value;
        }
        if config.prg_banks == 2 {
            prg[0x7801] = 1; // $F801 is independent of $B801 only with 32 KiB.
        }
        for addr in [0xfffau16, 0xfffc, 0xfffe] {
            let offset = usize::from(addr - 0x8000) % prg.len();
            prg[offset..offset + 2].copy_from_slice(&RESET.to_le_bytes());
        }
        let mut rom = vec![0; 16];
        rom[..4].copy_from_slice(b"NES\x1a");
        rom[4] = config.prg_banks;
        rom[5] = config.chr_banks;
        rom[6] = 0x30 | u8::from(config.vertical);
        rom[7] = 0x08;
        rom[8] = config.submapper << 4;
        rom[10] = if config.ram { 5 } else { 0 }; // 64 << 5 = 2 KiB.
        rom.extend(prg);
        for &tag in &BANK_TAGS[..usize::from(config.chr_banks)] {
            let mut bank = vec![tag; 8192];
            for (offset, value) in [
                (1, tag.wrapping_add(1)),
                (2, tag.wrapping_add(2)),
                (32, tag.wrapping_add(3)),
                (64, tag.wrapping_add(4)),
                (0x0fff, tag.wrapping_add(5)),
                (0x1000, tag.wrapping_add(6)),
                (0x1fff, tag.wrapping_add(7)),
            ] {
                bank[offset] = value;
            }
            rom.extend(bank);
        }
        let profile = format!(
            "[rom]\nname='cnrom-bus-fixture'\nmapper=3\nprg_kib={}\nchr_kib={}\n\
             [vectors]\nreset={RESET}\nnmi={RESET}\nirq={RESET}\n\
             [translation]\nstack_discipline='software'\nruntime_defines=['CNROM_BUS_EXPERIMENT']\n",
            usize::from(config.prg_banks) * 16,
            usize::from(config.chr_banks) * 8
        );
        Fixture {
            rom,
            profile,
            expected: self.expected,
            stop,
        }
    }
}

/// Test-only bus: no production mapper model supplies expected bank decisions.
/// No CPU timing/open-bus/raster/APU behavior is claimed by these fixtures.
struct ReferenceBus<'a> {
    prg: &'a [u8],
    chr: &'a [u8],
    and_conflicts: bool,
    bank_count: usize,
    bank: usize,
    ram: [u8; 2048],
    cart_ram: Option<[u8; 2048]>,
    vram: [u8; 2048],
    palette: [u8; 32],
    ppu_latch: u8,
    vertical: bool,
    ppu_addr: u16,
    ppu_hi: Option<u8>,
    ppu_buffer: u8,
    increment: u16,
    mapper_writes: Vec<(u16, u8, usize)>,
}

impl<'a> ReferenceBus<'a> {
    fn new(image: &'a nes_rom::Image<'a>) -> Self {
        Self {
            prg: image.prg,
            chr: image.chr,
            and_conflicts: image.header.submapper == 2,
            bank_count: image.chr.len() / 8192,
            bank: 0,
            ram: [0; 2048],
            cart_ram: (image.header.prg_ram_size == 2048).then_some([0; 2048]),
            vram: [0; 2048],
            palette: [0; 32],
            ppu_latch: 0,
            vertical: image.header.mirroring == nes_rom::Mirroring::Vertical,
            ppu_addr: 0,
            ppu_hi: None,
            ppu_buffer: 0,
            increment: 1,
            mapper_writes: Vec::new(),
        }
    }

    fn nt_offset(&self, addr: u16) -> usize {
        let offset = usize::from(addr - 0x2000) % 0x1000;
        let page = if self.vertical {
            [0, 1, 0, 1]
        } else {
            [0, 0, 1, 1]
        }[offset / 1024];
        page * 1024 + offset % 1024
    }

    fn palette_offset(addr: u16) -> usize {
        let offset = usize::from(addr) % 32;
        if matches!(offset, 0x10 | 0x14 | 0x18 | 0x1c) {
            offset - 16
        } else {
            offset
        }
    }
}

impl Bus for ReferenceBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0..=0x1fff => self.ram[usize::from(addr) % 2048],
            0x2000..=0x3fff => match addr % 8 {
                0 | 1 | 3 | 5 | 6 => self.ppu_latch,
                7 => {
                    let old = self.ppu_buffer;
                    let addr = self.ppu_addr % 0x4000;
                    let result = if addr >= 0x3f00 {
                        self.palette[Self::palette_offset(addr)] | (self.ppu_latch & 0xc0)
                    } else {
                        old
                    };
                    self.ppu_buffer = match addr {
                        0..=0x1fff => self.chr[self.bank * 8192 + usize::from(addr)],
                        0x2000..=0x3eff => self.vram[self.nt_offset(addr)],
                        _ => self.vram[self.nt_offset(addr - 0x1000)],
                    };
                    self.ppu_addr = (self.ppu_addr + self.increment) & 0x3fff;
                    self.ppu_latch = result;
                    result
                }
                _ => panic!("undriven PPU read ${addr:04X} outside reference"),
            },
            0x6000..=0x7fff => self
                .cart_ram
                .as_ref()
                .expect("absent RAM is not modeled as zero")[usize::from(addr - 0x6000) % 2048],
            0x8000..=0xffff => self.prg[usize::from(addr - 0x8000) % self.prg.len()],
            _ => panic!("unmodeled reference read ${addr:04X}"),
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        if (0x2000..0x4000).contains(&addr) {
            self.ppu_latch = value;
        }
        match addr {
            0..=0x1fff => self.ram[usize::from(addr) % 2048] = value,
            0x2000..=0x3fff => match addr % 8 {
                0 => {
                    assert_eq!(value & 0x80, 0);
                    self.increment = if value & 4 != 0 { 32 } else { 1 };
                }
                1 => assert_eq!(value & 0x18, 0, "rendering not part of bus fixture"),
                6 => {
                    if let Some(hi) = self.ppu_hi.take() {
                        self.ppu_addr = (u16::from(hi & 0x3f) << 8) | u16::from(value);
                    } else {
                        self.ppu_hi = Some(value);
                    }
                }
                7 => {
                    let ppu_addr = self.ppu_addr % 0x4000;
                    if (0x2000..=0x3eff).contains(&ppu_addr) {
                        let offset = self.nt_offset(ppu_addr);
                        self.vram[offset] = value;
                    } else if ppu_addr >= 0x3f00 {
                        self.palette[Self::palette_offset(ppu_addr)] = value & 0x3f;
                    }
                    self.ppu_addr = (self.ppu_addr + self.increment) & 0x3fff;
                }
                _ => panic!("unmodeled PPU write ${addr:04X}"),
            },
            0x6000..=0x7fff => {
                if let Some(ram) = &mut self.cart_ram {
                    ram[usize::from(addr - 0x6000) % 2048] = value;
                }
            }
            0x8000..=0xffff => {
                let rom = self.prg[usize::from(addr - 0x8000) % self.prg.len()];
                let effective = if self.and_conflicts {
                    value & rom
                } else {
                    value
                };
                self.bank = usize::from(effective) % self.bank_count;
                self.mapper_writes.push((addr, value, self.bank));
            }
            _ => panic!("unmodeled reference write ${addr:04X}"),
        }
    }
}

fn reference(fixture: &Fixture) -> Vec<(u16, u8, usize)> {
    let image = nes_rom::parse(&fixture.rom).unwrap();
    let mut bus = ReferenceBus::new(&image);
    let mut cpu = oracle_6502::Cpu::new();
    cpu.reset(&mut bus);
    for step in 0..20_000 {
        if cpu.pc == fixture.stop {
            break;
        }
        cpu.step(&mut bus).unwrap();
        assert!(step < 19_999, "reference never reached explicit stop");
    }
    for &(addr, expected) in &fixture.expected {
        assert_eq!(bus.ram[usize::from(addr)], expected, "source ${addr:04X}");
    }
    assert_eq!(bus.ram[0x7ff], 0xa5, "source completion sentinel");
    bus.mapper_writes
}

fn bank_fixture(config: Config) -> Fixture {
    let mut p = Program::new();
    for bank in 0..config.chr_banks {
        p.write(0xb800, bank);
        p.chr_read(0, BANK_TAGS[usize::from(bank)]);
        p.read(0xb801, 2);
        p.read(0xf801, if config.prg_banks == 1 { 2 } else { 1 });
    }
    p.write(0xb800, 0xff);
    p.chr_read(0, BANK_TAGS[usize::from(config.chr_banks - 1)]);
    for (addr, rom) in [(0xb801, 2), (0xb802, 1), (0xb803, 0)] {
        p.write(addr, 3);
        let bank = if config.submapper == 2 { rom } else { 3 } % config.chr_banks;
        p.chr_read(0, BANK_TAGS[usize::from(bank)]);
    }
    p.read(0xfffa, 0);
    p.read(0xfffb, 0xc8);
    p.read(0xfffc, 0);
    p.read(0xfffd, 0xc8);
    p.read(0xfffe, 0);
    p.read(0xffff, 0xc8);
    p.finish(config)
}

fn buffered_fixture() -> Fixture {
    let mut p = Program::new();
    p.write(0xb800, 0);
    p.ppu_addr(0);
    p.emit(&[0xad, 7, 0x20]);
    p.write(0xb800, 1);
    p.read(0x2007, 0x11);
    p.read(0x2007, 0x22);
    p.ppu_addr(0);
    p.read(0x2007, 0x23);
    p.read(0x2007, 0x21);
    // Mirrors of PPUCTRL, PPUADDR and PPUDATA: +32 increments.
    p.write(0x3ff8, 4);
    p.write(0x3ffe, 0);
    p.write(0x3ffe, 0);
    p.emit(&[0xad, 0xff, 0x3f]);
    p.read(0x3fff, 0x21);
    p.read(0x3fff, 0x24);
    p.read(0x3fff, 0x25);
    p.write(0x2000, 0);
    p.ppu_addr(0x0fff);
    p.emit(&[0xad, 7, 0x20]);
    p.read(0x2007, 0x26);
    p.read(0x2007, 0x27);
    // CHR writes increment address but cannot modify ROM, including after rebank.
    p.ppu_addr(0);
    p.write(0x2007, 0xa5);
    p.emit(&[0xad, 7, 0x20]);
    p.read(0x2007, 0x22);
    p.write(0xb800, 0);
    p.write(0xb800, 1);
    p.chr_read(0, 0x21);
    // Write-only PPU registers return the PPU I/O latch, not CPU open bus.
    p.write(0x2000, 0x40);
    p.read(0x2000, 0x40);
    p.read(0x2001, 0x40);
    p.read(0x2006, 0x40);
    p.finish(Config::default())
}

fn ram_fixture() -> Fixture {
    let mut p = Program::new();
    p.write(0x67ab, 0x42);
    for addr in [0x67ab, 0x6fab, 0x77ab, 0x7fab] {
        p.read(addr, 0x42);
    }
    p.write(0x6000, 0x5a);
    p.write(0x7fff, 0xa6);
    for addr in [0x6000, 0x6800, 0x7000, 0x7800] {
        p.read(addr, 0x5a);
    }
    for addr in [0x67ff, 0x6fff, 0x77ff, 0x7fff] {
        p.read(addr, 0xa6);
    }
    // Pointer high byte wraps from $FF to $00; effective address crosses into RAM.
    p.write(0xff, 0xff);
    p.write(0, 0x5f);
    p.emit(&[0xa0, 1, 0xb1, 0xff]);
    p.save_a(0x5a);
    p.write(0xff, 0);
    p.write(0, 0x60);
    p.emit(&[0xa2, 1, 0xa1, 0xfe]);
    p.save_a(0x5a);
    // All dynamic store forms reach mapper space across $7FFF, not cart RAM.
    p.emit(&[0xa9, 2, 0x9d, 0xff, 0x7f]);
    p.chr_read(0, 0x31);
    p.emit(&[0xa9, 1, 0x99, 0xff, 0x7f]);
    p.chr_read(0, 0x21);
    p.write(0xff, 0xff);
    p.write(0, 0x7f);
    p.emit(&[0xa9, 3, 0x91, 0xff]);
    p.chr_read(0, 0x41);
    p.write(0xff, 0);
    p.write(0, 0x80);
    p.emit(&[0xa9, 2, 0x81, 0xfe]);
    p.chr_read(0, 0x31);
    // STX/STY preserve source flags as well as their registers across helpers.
    p.emit(&[0xa2, 1, 0xa0, 2, 0x38, 0x8e, 0, 0xb8, 0x08, 0x68]);
    p.save_a(0x35);
    p.chr_read(0, 0x21);
    p.emit(&[0x8c, 0, 0xb8]);
    p.chr_read(0, 0x31);
    p.emit(&[0x8a]);
    p.save_a(1);
    p.emit(&[0x98]);
    p.save_a(2);
    // RAM mirror and full 16-bit wrapping reads remain source-addressed.
    p.write(0x0010, 0x6a);
    p.read(0x1810, 0x6a);
    p.write(0, 0x5c);
    p.emit(&[0xbd, 0xff, 0xff]);
    p.save_a(0x5c);
    p.finish(Config {
        ram: true,
        ..Config::default()
    })
}

fn rmw_fixture(submapper: u8) -> Fixture {
    let mut p = Program::new();
    p.emit(&[0xee, 1, 0xb8]); // INC ROM $02: original $02 then modified $03.
    p.chr_read(0, if submapper == 2 { 0x31 } else { 0x41 });
    p.emit(&[0x0e, 3, 0xb8]); // ASL zero must still emit two zero writes.
    p.chr_read(0, 0x11);
    p.finish(Config {
        submapper,
        ..Config::default()
    })
}

fn rmw_family_fixture(submapper: u8) -> Fixture {
    let mut p = Program::new();
    // Absolute NMOS opcodes; ROM operand $81, A=$53, C=1, V=0, I=1.
    // Expected memory result and pushed status are independent literal values.
    for (opcode, result, flags, accumulator) in [
        (0xee, 0x82u8, 0xb5, 0x53),
        (0xce, 0x80, 0xb5, 0x53), // INC, DEC
        (0x0e, 0x02, 0x35, 0x53),
        (0x4e, 0x40, 0x35, 0x53), // ASL, LSR
        (0x2e, 0x03, 0x35, 0x53),
        (0x6e, 0xc0, 0xb5, 0x53), // ROL, ROR
        (0xef, 0x82, 0xf4, 0xd1),
        (0xcf, 0x80, 0xb4, 0x53), // ISC, DCP
        (0x0f, 0x02, 0x35, 0x53),
        (0x4f, 0x40, 0x35, 0x13), // SLO, SRE
        (0x2f, 0x03, 0x35, 0x03),
        (0x6f, 0xc0, 0x35, 0x14), // RLA, RRA
    ] {
        p.emit(&[0xb8, 0x38, 0xa9, 0x53, opcode, 4, 0xb8]);
        p.save_a(accumulator);
        p.emit(&[0x08, 0x68]);
        p.save_a(flags);
        let bank = if submapper == 2 {
            result & 0x81
        } else {
            result
        } & 3;
        p.chr_read(0, BANK_TAGS[usize::from(bank)]);
        p.read(0xb804, 0x81); // RMW cannot modify PRG ROM itself.
    }
    p.finish(Config {
        submapper,
        ..Config::default()
    })
}

fn combined_rmw_corner_fixture(submapper: u8) -> Fixture {
    // Opcode, ROM operand, input A/C, modified operand, final A, pushed P.
    // These literal corner cases distinguish the modified operand from both
    // the unchanged ROM and accumulator, including each arithmetic flag edge.
    let cases = [
        (0x0f, 0x00, 0x00, true, 0x00u8, 0x00, 0x36),
        (0x0f, 0x40, 0x80, false, 0x80, 0x80, 0xb4),
        (0x0f, 0x80, 0x01, false, 0x00, 0x01, 0x35),
        (0x4f, 0x00, 0x00, true, 0x00, 0x00, 0x36),
        (0x4f, 0x01, 0x00, false, 0x00, 0x00, 0x37),
        (0x4f, 0xfe, 0x80, false, 0x7f, 0xff, 0xb4),
        (0x2f, 0x00, 0x00, true, 0x01, 0x00, 0x36),
        (0x2f, 0x80, 0xff, false, 0x00, 0x00, 0x37),
        (0x2f, 0x40, 0x80, false, 0x80, 0x80, 0xb4),
        (0x6f, 0x00, 0x00, false, 0x00, 0x00, 0x36),
        (0x6f, 0x01, 0x7f, false, 0x00, 0x80, 0xf4),
        (0x6f, 0x00, 0x80, true, 0x80, 0x00, 0x77),
        (0x6f, 0xff, 0x7f, true, 0xff, 0x7f, 0x35),
    ];
    let mut p = Program::new();
    for (index, &(opcode, old, input_a, carry, modified, result_a, status)) in
        cases.iter().enumerate()
    {
        p.emit(&[
            0xb8,
            if carry { 0x38 } else { 0x18 },
            0xa9,
            input_a,
            opcode,
            index as u8,
            0xb9,
        ]);
        p.save_a(result_a);
        p.emit(&[0x08, 0x68]);
        p.save_a(status);
        let bank = if submapper == 2 {
            modified & old
        } else {
            modified
        } & 3;
        p.chr_read(0, BANK_TAGS[usize::from(bank)]);
    }
    let mut fixture = p.finish(Config {
        submapper,
        ..Config::default()
    });
    for (index, &(_, operand, _, _, _, _, _)) in cases.iter().enumerate() {
        fixture.rom[16 + 0x3900 + index] = operand;
    }
    fixture
}

fn nametable_fixture(vertical: bool) -> Fixture {
    let mut p = Program::new();
    p.write(0xb800, 0);
    for (addr, value) in [
        (0x2000, 0x44),
        (0x2400, 0x55),
        (0x2800, 0x66),
        (0x2c00, 0x77),
    ] {
        p.ppu_addr(addr);
        p.write(0x2007, value);
    }
    let expected = if vertical {
        [0x66, 0x77, 0x66, 0x77]
    } else {
        [0x55, 0x55, 0x77, 0x77]
    };
    for (page, value) in expected.into_iter().enumerate() {
        p.chr_read(0x2000 + page as u16 * 1024, value);
        p.chr_read(0x3000 + page as u16 * 1024, value);
    }
    p.ppu_addr(0x1fff);
    p.emit(&[0xad, 7, 0x20]);
    p.read(0x2007, 0x18);
    p.read(0x2007, expected[0]);
    p.write(0xb800, 3);
    p.chr_read(0x2000, expected[0]); // CHR bank changes cannot change mirroring.
    // Palette aliases and immediate return still refill buffer from underlying NT.
    p.ppu_addr(0x2f00);
    p.write(0x2007, 0x5d);
    p.ppu_addr(0x3f10);
    p.write(0x2007, 0x2a);
    p.ppu_addr(0x3f00);
    p.read(0x2007, 0x2a);
    p.ppu_addr(0x1000);
    p.read(0x2007, 0x5d);
    p.read(0x2007, 0x47);
    p.finish(Config {
        vertical,
        ..Config::default()
    })
}

fn install_function(fixture: &mut Fixture, addr: u16, bytes: &[u8]) {
    let prg_len = usize::from(fixture.rom[4]) * 16384;
    let offset = 16 + usize::from(addr - 0x8000) % prg_len;
    fixture.rom[offset..offset + bytes.len()].copy_from_slice(bytes);
    fixture.profile.push_str(&format!(
        "[[function]]\naddr={addr}\nname='fixture_{addr:04x}'\n"
    ));
}

fn stack_fixture() -> Fixture {
    let mut p = Program::new();
    // JSR at $C804 pushes literal source return $C806 onto guest stack.
    p.emit(&[0x20, 0x00, 0xe0]);
    p.save_a(0x7c);
    p.emit(&[0xba, 0x8a]);
    p.save_a(0x3f);
    p.read(0x013f, 0xc8);
    p.read(0x013e, 0x06);
    // A hardware stack-page wrap, not a native Z80 continuation stack.
    p.emit(&[0xa2, 0, 0x9a]);
    let return_word = RESET + p.bytes.len() as u16 + 2;
    p.emit(&[0x20, 0, 0xe0]);
    p.save_a(0x7c);
    p.emit(&[0xba, 0x8a]);
    p.save_a(0);
    p.read(0x0100, (return_word >> 8) as u8);
    p.read(0x01ff, return_word as u8);
    p.emit(&[0xa2, 0x3f, 0x9a]);
    // Software-produced return pair transfers to $E100; no preceding JSR.
    p.emit(&[0xa9, 0xe0, 0x48, 0xa9, 0xff, 0x48, 0x60]);
    let resume = RESET + p.bytes.len() as u16;
    p.read(0x0400, 0x5e);
    p.emit(&[0xba, 0x8a]);
    p.save_a(0x3f);
    // Outer calls inner; inner restores S=$3D, abandoning its return frame.
    p.emit(&[0x20, 0, 0xe2]);
    p.save_a(0x6d);
    p.emit(&[0xba, 0x8a]);
    p.save_a(0x3f);
    let mut fixture = p.finish(Config::default());
    install_function(&mut fixture, 0xe000, &[0xa9, 0x7c, 0x60]);
    install_function(
        &mut fixture,
        0xe100,
        &[
            0xa9,
            0x5e,
            0x8d,
            0,
            4,
            0x4c,
            resume as u8,
            (resume >> 8) as u8,
        ],
    );
    install_function(&mut fixture, resume, &[]); // Deliberate post-synthetic-RTS entry.
    install_function(&mut fixture, 0xe200, &[0x20, 0, 0xe3, 0xa9, 0xee, 0x60]);
    install_function(&mut fixture, 0xe300, &[0xa2, 0x3d, 0x9a, 0xa9, 0x6d, 0x60]);
    fixture
}

#[test]
fn cnrom_original_6502_covers_fixed_prg_conflicts_and_full_chr_geometry() {
    for prg_banks in [1, 2] {
        for submapper in [1, 2] {
            for chr_banks in [1, 2, 4, 8, 16] {
                reference(&bank_fixture(Config {
                    prg_banks,
                    submapper,
                    chr_banks,
                    ..Config::default()
                }));
            }
        }
    }
}

#[test]
fn cnrom_original_6502_buffered_chr_readback_and_dynamic_ram_bus() {
    reference(&buffered_fixture());
    reference(&ram_fixture());
}

#[test]
fn cnrom_original_6502_rmw_has_literal_ordered_mapper_writes() {
    for submapper in [1, 2] {
        let events = reference(&rmw_fixture(submapper));
        assert_eq!(
            events,
            [
                (0xb801, 2, 2),
                (0xb801, 3, if submapper == 2 { 2 } else { 3 }),
                (0xb803, 0, 0),
                (0xb803, 0, 0)
            ]
        );
        let family_events = reference(&rmw_family_fixture(submapper));
        assert_eq!(family_events.len(), 24);
        for (pair, result) in family_events.as_chunks::<2>().0.iter().zip([
            0x82, 0x80, 0x02, 0x40, 0x03, 0xc0, 0x82, 0x80, 0x02, 0x40, 0x03, 0xc0,
        ]) {
            assert_eq!((pair[0].0, pair[0].1), (0xb804, 0x81));
            assert_eq!((pair[1].0, pair[1].1), (0xb804, result));
        }
        reference(&combined_rmw_corner_fixture(submapper));
    }
}

#[test]
fn cnrom_original_6502_nametables_palette_and_real_guest_stack() {
    reference(&nametable_fixture(false));
    reference(&nametable_fixture(true));
    reference(&stack_fixture());
}

fn generated(name: &str, fixture: &Fixture) -> (PathBuf, Output) {
    generated_with_options(name, fixture, &[])
}

fn generated_with_options(name: &str, fixture: &Fixture, options: &[&str]) -> (PathBuf, Output) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let work = root
        .join("out/tests")
        .join(format!("cnrom-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("fixture.nes"), &fixture.rom).unwrap();
    std::fs::write(work.join("profile.toml"), &fixture.profile).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nes-to-sms"))
        .arg(work.join("fixture.nes"))
        .arg(work.join("profile.toml"))
        .arg(work.join("sms"))
        .arg("--runtime")
        .arg(root.join("runtime"))
        .args(options)
        .output()
        .unwrap();
    std::fs::write(
        work.join("generate.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )
    .unwrap();
    (work, output)
}

#[test]
fn cnrom_pipeline_rejects_ambiguous_or_unimplemented_metadata_before_output() {
    for case in [
        "no-capability",
        "native-stack",
        "legacy",
        "submapper-zero",
        "submapper-three",
        "extended-mapper",
        "four-screen",
        "chr-ram",
        "large-prg-ram",
        "battery",
        "non-power-two-chr",
        "brk-without-scheduler",
        "rti-without-scheduler",
        "other-console",
        "other-timing",
        "misc-rom",
        "expansion-device",
        "reserved-metadata",
        "trailing-payload",
        "debug-stubs",
    ] {
        let mut fixture = bank_fixture(Config::default());
        match case {
            "no-capability" => {
                fixture.profile = fixture.profile.replace("['CNROM_BUS_EXPERIMENT']", "[]")
            }
            "native-stack" => {
                fixture.profile = fixture
                    .profile
                    .replace("stack_discipline='software'", "stack_discipline='native'")
            }
            "legacy" => {
                fixture.rom[7] = 0;
                fixture.rom[8] = 0;
            }
            "submapper-zero" => fixture.rom[8] = 0,
            "submapper-three" => fixture.rom[8] = 0x30,
            "extended-mapper" => fixture.rom[8] |= 1,
            "four-screen" => fixture.rom[6] |= 8,
            "chr-ram" => fixture.rom[11] = 7,
            "large-prg-ram" => fixture.rom[10] = 7,
            "battery" => fixture.rom[6] |= 2,
            "non-power-two-chr" => {
                fixture.rom[5] = 3;
                fixture.rom.truncate(16 + 32768 + 3 * 8192);
                fixture.profile = fixture.profile.replace("chr_kib=32", "chr_kib=24");
            }
            "brk-without-scheduler" | "rti-without-scheduler" => {
                let mut p = Program::new();
                p.emit(&[if case == "brk-without-scheduler" {
                    0x00
                } else {
                    0x40
                }]);
                fixture = p.finish(Config::default());
            }
            "other-console" => fixture.rom[7] |= 1,
            "other-timing" => fixture.rom[12] = 1,
            "misc-rom" => fixture.rom[14] = 1,
            "expansion-device" => fixture.rom[15] = 1,
            "reserved-metadata" => fixture.rom[13] = 1,
            "trailing-payload" => fixture.rom.push(0),
            "debug-stubs" => {}
            _ => unreachable!(),
        }
        let name = format!("reject-{case}");
        let options: &[&str] = if case == "debug-stubs" {
            &["--debug-unresolved-stubs"]
        } else {
            &[]
        };
        let (work, output) = generated_with_options(&name, &fixture, options);
        assert!(!output.status.success(), "must reject {case}");
        assert!(
            !work.join("sms").exists(),
            "{case} created output before rejection"
        );
        std::fs::create_dir(work.join("sms")).unwrap();
        std::fs::write(work.join("sms/keep.txt"), "preexisting output").unwrap();
        let (_, output) = generated_with_options(&name, &fixture, options);
        assert!(
            !output.status.success(),
            "must reject {case} with existing output"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("sms/keep.txt")).unwrap(),
            "preexisting output"
        );
        assert_eq!(
            std::fs::read_dir(work.join("sms")).unwrap().count(),
            1,
            "{case} modified existing project"
        );
    }
}

fn assembled(name: &str, fixture: &Fixture) -> PathBuf {
    let (work, output) = generated(name, fixture);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let uid = Command::new("id").arg("-u").output().unwrap();
    let gid = Command::new("id").arg("-g").output().unwrap();
    assert!(uid.status.success() && gid.status.success());
    let user = format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    );
    let output = Command::new("docker")
        .args(["run", "--rm", "--network", "none", "--user", &user, "-v"])
        .arg(format!("{}:/work", root.display()))
        .args(["nes-to-sms-poc", "make", "-B", "-C"])
        .arg(
            PathBuf::from("/work")
                .join(work.strip_prefix(&root).unwrap())
                .join("sms"),
        )
        .output()
        .expect("existing Docker toolchain");
    std::fs::write(
        work.join("assemble.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    work
}

fn trace_command(work: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    command
        .arg(work.join("sms/sms.sms"))
        .args(["--steps", "1000000", "--no-irq"]);
    command
}

fn assert_assembled(name: &str, fixture: &Fixture) {
    reference(fixture);
    let work = assembled(name, fixture);
    // Ignored handoff evidence for an independent real-core smoke runner.
    let observations = fixture
        .expected
        .iter()
        .map(|(addr, value)| format!("{{\"addr\":{},\"value\":{value}}}", 0xc000 + addr))
        .collect::<Vec<_>>()
        .join(",");
    std::fs::write(
        work.join("expected.json"),
        format!("{{\"expect_no_trap\":true,\"ram\":[{observations}]}}\n"),
    )
    .unwrap();
    let mut trace = trace_command(&work);
    trace.arg("--expect-no-trap");
    for &(addr, value) in &fixture.expected {
        trace
            .arg("--expect-ram")
            .arg(format!("{:04X}={value:02X}", 0xc000 + addr));
    }
    let output = trace.output().unwrap();
    std::fs::write(
        work.join("trace.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}\n{}",
        work.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn cnrom_assembled_fixed_prg_conflicts_and_full_chr_geometry() {
    for prg_banks in [1, 2] {
        for submapper in [1, 2] {
            for chr_banks in [1, 2, 4, 8, 16] {
                let name = format!("p{prg_banks}-s{submapper}-c{chr_banks}");
                assert_assembled(
                    &name,
                    &bank_fixture(Config {
                        prg_banks,
                        submapper,
                        chr_banks,
                        ..Config::default()
                    }),
                );
            }
        }
    }
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn cnrom_assembled_buffered_chr_and_dynamic_ram_bus() {
    assert_assembled("buffered", &buffered_fixture());
    assert_assembled("ram", &ram_fixture());
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn cnrom_assembled_nametables_palette_and_real_guest_stack() {
    assert_assembled("nt-horizontal", &nametable_fixture(false));
    assert_assembled("nt-vertical", &nametable_fixture(true));
    assert_assembled("guest-stack", &stack_fixture());
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn cnrom_assembled_rmw_records_both_mapper_writes_even_when_unchanged() {
    for submapper in [1, 2] {
        assert_assembled(
            &format!("rmw-families-s{submapper}"),
            &rmw_family_fixture(submapper),
        );
        assert_assembled(
            &format!("rmw-corners-s{submapper}"),
            &combined_rmw_corner_fixture(submapper),
        );
        let fixture = rmw_fixture(submapper);
        let events = reference(&fixture);
        let work = assembled(&format!("rmw-s{submapper}"), &fixture);
        let symbols = std::fs::read_to_string(work.join("sms/sms.sym")).unwrap();
        let address = symbols
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                let address = fields.next()?;
                (fields.next()? == "rt_cnrom_write_register")
                    .then(|| address.split(':').next_back().unwrap().to_owned())
            })
            .expect("assembled CNROM register entry symbol");
        let mut trace = trace_command(&work);
        trace.arg("--expect-no-trap").env("SMS_WATCH_PC", &address);
        for &(addr, value) in &fixture.expected {
            trace
                .arg("--expect-ram")
                .arg(format!("{:04X}={value:02X}", 0xc000 + addr));
        }
        let output = trace.output().unwrap();
        std::fs::write(
            work.join("trace.log"),
            [&output.stdout[..], &output.stderr[..]].concat(),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // At the actual register entry B holds the raw written byte. Observe
        // every entry, not just changes to the effective latch bank.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let observed: Vec<u8> = stdout
            .lines()
            .filter(|line| line.trim_start().starts_with("step "))
            .filter_map(|line| {
                line.split_once("bc=$")
                    .map(|(_, value)| u8::from_str_radix(&value[..2], 16).unwrap())
            })
            .collect();
        let expected: Vec<u8> = events.iter().map(|(_, value, _)| *value).collect();
        assert_eq!(expected, [2, 3, 0, 0]);
        assert_eq!(
            observed, expected,
            "actual assembled register-write sequence"
        );
    }
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn cnrom_assembled_unsupported_bus_reads_and_scheduling_fail_closed() {
    for (name, program) in [
        ("no-ram", &[0xad, 0, 0x60][..]),
        ("expansion-read", &[0xad, 0, 0x50][..]),
        ("status-timing", &[0xad, 2, 0x20][..]),
        ("controller-openbus", &[0xad, 0x16, 0x40][..]),
        ("rendering", &[0xa9, 0x18, 0x8d, 1, 0x20][..]),
        ("nmi", &[0xa9, 0x80, 0x8d, 0, 0x20][..]),
    ] {
        let mut p = Program::new();
        p.emit(program);
        let fixture = p.finish(Config::default());
        let work = assembled(name, &fixture);
        let output = trace_command(&work)
            .args(["--expect-ram", "CB1D=E8", "--expect-ram", "C7FF=00"])
            .output()
            .unwrap();
        std::fs::write(
            work.join("trace.log"),
            [&output.stdout[..], &output.stderr[..]].concat(),
        )
        .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
