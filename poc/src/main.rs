use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

mod ir6502;
mod sms_runtime;
mod z80_backend;

use z80_backend::Z80Program;

const DEFAULT_ROM: &str = "/mnt/terachad/Emulators/EmuDeck/roms/nes/Super Mario Bros. (World).nes";
const DEFAULT_OUT: &str = "out/smb";

#[derive(Debug)]
struct InesHeader {
    prg_banks: u8,
    chr_banks: u8,
    flags6: u8,
    flags7: u8,
    mapper: u16,
    has_trainer: bool,
    has_battery: bool,
    vertical_mirroring: bool,
    nes2: bool,
}

impl InesHeader {
    fn parse(rom: &[u8]) -> io::Result<Self> {
        if rom.len() < 16 || &rom[0..4] != b"NES\x1a" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not an iNES/NES 2.0 ROM",
            ));
        }

        let flags6 = rom[6];
        let flags7 = rom[7];
        let mapper = ((flags7 as u16 & 0xF0) | ((flags6 as u16) >> 4)) as u16;
        let nes2 = (flags7 & 0x0C) == 0x08;

        Ok(Self {
            prg_banks: rom[4],
            chr_banks: rom[5],
            flags6,
            flags7,
            mapper,
            has_trainer: flags6 & 0x04 != 0,
            has_battery: flags6 & 0x02 != 0,
            vertical_mirroring: flags6 & 0x01 != 0,
            nes2,
        })
    }

    fn prg_len(&self) -> usize {
        self.prg_banks as usize * 16 * 1024
    }

    fn chr_len(&self) -> usize {
        self.chr_banks as usize * 8 * 1024
    }
}

#[derive(Debug)]
struct Vectors {
    nmi: u16,
    reset: u16,
    irq: u16,
}

const WORLD_1_1_AREA: &[u8] = &[
    0x50, 0x21, 0x07, 0x81, 0x47, 0x24, 0x57, 0x00, 0x63, 0x01, 0x77, 0x01, 0xc9, 0x71, 0x68, 0xf2,
    0xe7, 0x73, 0x97, 0xfb, 0x06, 0x83, 0x5c, 0x01, 0xd7, 0x22, 0xe7, 0x00, 0x03, 0xa7, 0x6c, 0x02,
    0xb3, 0x22, 0xe3, 0x01, 0xe7, 0x07, 0x47, 0xa0, 0x57, 0x06, 0xa7, 0x01, 0xd3, 0x00, 0xd7, 0x01,
    0x07, 0x81, 0x67, 0x20, 0x93, 0x22, 0x03, 0xa3, 0x1c, 0x61, 0x17, 0x21, 0x6f, 0x33, 0xc7, 0x63,
    0xd8, 0x62, 0xe9, 0x61, 0xfa, 0x60, 0x4f, 0xb3, 0x87, 0x63, 0x9c, 0x01, 0xb7, 0x63, 0xc8, 0x62,
    0xd9, 0x61, 0xea, 0x60, 0x39, 0xf1, 0x87, 0x21, 0xa7, 0x01, 0xb7, 0x20, 0x39, 0xf1, 0x5f, 0x38,
    0x6d, 0xc1, 0xaf, 0x26, 0xfd,
];

const WORLD_1_1_ENEMIES: &[u8] = &[
    0x1e, 0xc2, 0x00, 0x6b, 0x06, 0x8b, 0x86, 0x63, 0xb7, 0x0f, 0x05, 0x03, 0x06, 0x23, 0x06, 0x4b,
    0xb7, 0xbb, 0x00, 0x5b, 0xb7, 0xfb, 0x37, 0x3b, 0xb7, 0x0f, 0x0b, 0x1b, 0x37, 0xff,
];

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let rom_path = args.get(1).map(String::as_str).unwrap_or(DEFAULT_ROM);
    let out_dir = PathBuf::from(args.get(2).map(String::as_str).unwrap_or(DEFAULT_OUT));

    fs::create_dir_all(&out_dir)?;

    let rom = fs::read(rom_path)?;
    let header = InesHeader::parse(&rom)?;
    let trainer_len = if header.has_trainer { 512 } else { 0 };
    let prg_start = 16 + trainer_len;
    let chr_start = prg_start + header.prg_len();
    let chr_end = chr_start + header.chr_len();

    if rom.len() < chr_end {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "ROM shorter than header-declared PRG+CHR data",
        ));
    }

    let prg = &rom[prg_start..chr_start];
    let chr = &rom[chr_start..chr_end];
    let vectors = read_vectors(prg)?;
    let sms_tiles = nes_chr_to_sms_4bpp(&chr[0x1000..0x2000]);
    let title = decode_smb_title_screen(chr);
    let world_1_1 = world_1_1_report();
    let world_1_1_trace = world_1_1_area_trace();
    let world_1_1_base_screen = build_world_1_1_screen(false);
    let world_1_1_screen = build_world_1_1_screen(true);
    let sms_build = build_sms_tile_demo(&sms_tiles, &title.sms_name_table, &world_1_1_screen)?;
    let translation = translate_reset_sketch(prg, vectors.reset);
    let ir_demo = ir6502::generate_smb_pointer_increment_demo(prg)?;
    let ir_branch_demo = ir6502::generate_smb_branch_store_demo(prg)?;
    let ir_compare_demo = ir6502::generate_smb_compare_branch_demo(prg)?;

    fs::write(
        out_dir.join("header.txt"),
        header_report(rom_path, rom.len(), &header),
    )?;
    fs::write(out_dir.join("prg.bin"), prg)?;
    fs::write(out_dir.join("chr.bin"), chr)?;
    fs::write(out_dir.join("vectors.txt"), vector_report(&vectors))?;
    fs::write(out_dir.join("map.json"), map_json(&header, &vectors))?;
    fs::write(out_dir.join("tiles.sms4bpp"), &sms_tiles)?;
    fs::write(out_dir.join("tiles.ppm"), tile_sheet_ppm(chr))?;
    fs::write(out_dir.join("title_nametable.bin"), &title.nes_name_table)?;
    fs::write(
        out_dir.join("title_sms_nametable.bin"),
        &title.sms_name_table,
    )?;
    fs::write(out_dir.join("title_decode.txt"), title.report)?;
    fs::write(
        out_dir.join("title_menu_port.asm"),
        title_menu_port_report(),
    )?;
    fs::write(out_dir.join("world_1_1_data.txt"), world_1_1)?;
    fs::write(out_dir.join("world_1_1_area_trace.txt"), world_1_1_trace)?;
    fs::write(
        out_dir.join("world_1_1_base_sms_nametable.bin"),
        &world_1_1_base_screen,
    )?;
    fs::write(
        out_dir.join("world_1_1_initial_screen_sms_nametable.bin"),
        &world_1_1_screen,
    )?;
    fs::write(
        out_dir.join("world_1_1_object_overlay.txt"),
        world_1_1_object_overlay_report(),
    )?;
    fs::write(out_dir.join("poc.sms"), sms_build.rom)?;
    fs::write(out_dir.join("generated.asm"), sms_build.asm)?;
    fs::write(out_dir.join("ir_lowering_demo.asm"), ir_demo.asm)?;
    fs::write(out_dir.join("ir_lowering_demo.bin"), ir_demo.bytes)?;
    fs::write(out_dir.join("ir_lowering_report.txt"), ir_demo.report)?;
    fs::write(
        out_dir.join("ir_lowering_validation.txt"),
        ir_demo.validation,
    )?;
    fs::write(out_dir.join("ir_branch_demo.asm"), ir_branch_demo.asm)?;
    fs::write(out_dir.join("ir_branch_demo.bin"), ir_branch_demo.bytes)?;
    fs::write(out_dir.join("ir_branch_report.txt"), ir_branch_demo.report)?;
    fs::write(
        out_dir.join("ir_branch_validation.txt"),
        ir_branch_demo.validation,
    )?;
    fs::write(out_dir.join("ir_compare_demo.asm"), ir_compare_demo.asm)?;
    fs::write(out_dir.join("ir_compare_demo.bin"), ir_compare_demo.bytes)?;
    fs::write(
        out_dir.join("ir_compare_report.txt"),
        ir_compare_demo.report,
    )?;
    fs::write(
        out_dir.join("ir_compare_validation.txt"),
        ir_compare_demo.validation,
    )?;
    fs::write(out_dir.join("z80_translation_experiment.asm"), translation)?;
    fs::write(
        out_dir.join("summary.txt"),
        summary_report(&header, &vectors),
    )?;

    println!("wrote PoC artifacts to {}", out_dir.display());
    Ok(())
}

fn read_vectors(prg: &[u8]) -> io::Result<Vectors> {
    let nmi = read_cpu_word(prg, 0xFFFA)?;
    let reset = read_cpu_word(prg, 0xFFFC)?;
    let irq = read_cpu_word(prg, 0xFFFE)?;
    Ok(Vectors { nmi, reset, irq })
}

fn cpu_to_prg_offset(prg: &[u8], addr: u16) -> Option<usize> {
    if addr < 0x8000 {
        return None;
    }
    let mut offset = addr as usize - 0x8000;
    if prg.len() == 16 * 1024 {
        offset %= 16 * 1024;
    }
    (offset + 1 < prg.len()).then_some(offset)
}

fn read_cpu_word(prg: &[u8], addr: u16) -> io::Result<u16> {
    let offset = cpu_to_prg_offset(prg, addr).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("CPU address ${addr:04X} is outside PRG ROM"),
        )
    })?;
    Ok(u16::from_le_bytes([prg[offset], prg[offset + 1]]))
}

fn nes_chr_to_sms_4bpp(chr: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chr.len() * 2);
    for tile in chr.chunks_exact(16) {
        for row in 0..8 {
            out.push(tile[row]);
            out.push(tile[row + 8]);
            out.push(0);
            out.push(0);
        }
    }
    out
}

struct SmsBuild {
    rom: Vec<u8>,
    asm: String,
}

fn build_sms_tile_demo(
    tiles: &[u8],
    sms_name_table: &[u8],
    world_1_1_screen: &[u8],
) -> io::Result<SmsBuild> {
    let mut z80 = Z80Program::new();

    z80.comment("SMS boot/title/world-screen PoC generated through z80_backend.");
    z80.label("start");
    z80.di();
    z80.ld_sp_imm(0xDFF0);

    for (reg, value) in [
        (0, 0x04), // Mode 4.
        (1, 0x00), // Display disabled while loading VRAM.
        (2, 0x0E), // Name table at $3800.
        (3, 0xFF),
        (4, 0x07),
        (5, 0x7E), // Sprite attribute table at $3F00.
        (6, 0x00),
        (7, 0x00),
        (8, 0x00),
        (9, 0x00),
        (10, 0xFF),
    ] {
        sms_runtime::emit_vdp_reg(&mut z80, reg, value);
    }

    sms_runtime::emit_copy_to_vram_label(&mut z80, 0x0000, tiles.len() as u16, "tiles_sms4bpp");
    sms_runtime::emit_copy_to_cram_label(&mut z80, 0x00, 32, "sms_palette");
    sms_runtime::emit_copy_to_vram_label(
        &mut z80,
        0x3800,
        sms_name_table.len() as u16,
        "title_sms_nametable",
    );
    sms_runtime::emit_copy_to_vram_label(&mut z80, 0x3F00, 64, "sprite_attribute_clear");

    sms_runtime::emit_vdp_reg(&mut z80, 1, 0x40); // Display on, interrupts off.
    z80.ld_a_imm(0);
    z80.ld_abs_a(0xC000); // Title menu selection: 0=1 player, 1=2 players.
    z80.ld_abs_a(0xC001); // Previous SMS joypad button state for debounce.
    z80.call("draw_cursor_one_player");

    z80.label("main_loop");
    z80.in_a(0xDC); // SMS controller port A/B, active low.
    z80.cpl();
    z80.and_imm(0x30); // SMS buttons 1/2 only. B1=NES Select, B2=NES Start stub.
    z80.ld_b_a();
    z80.ld_a_abs(0xC001);
    z80.cpl();
    z80.and_b();
    z80.ld_c_a();
    z80.ld_a_b();
    z80.ld_abs_a(0xC001);
    z80.ld_a_c();
    z80.and_imm(0x10);
    z80.call_nz("toggle_title_cursor");
    z80.ld_a_c();
    z80.and_imm(0x20);
    z80.call_nz("start_transition");
    z80.jp("main_loop");

    z80.label("draw_cursor_one_player");
    sms_runtime::emit_title_cursor_writes(&mut z80, [0xCE, 0x24, 0x24]);
    z80.ret();

    z80.label("draw_cursor_two_player");
    sms_runtime::emit_title_cursor_writes(&mut z80, [0x24, 0xCE, 0x24]);
    z80.ret();

    z80.label("toggle_title_cursor");
    z80.ld_a_abs(0xC000);
    z80.xor_imm(0x01);
    z80.ld_abs_a(0xC000);
    z80.or_a();
    z80.jp_z("draw_cursor_one_player");
    z80.jp("draw_cursor_two_player");

    z80.label("start_transition");
    sms_runtime::emit_copy_to_vram_label(
        &mut z80,
        0x3800,
        world_1_1_screen.len() as u16,
        "world_1_1_initial_screen",
    );
    sms_runtime::emit_world_lives_display_writes(&mut z80);
    z80.ret();

    z80.align(16);
    z80.data("tiles_sms4bpp", tiles);
    z80.align(16);
    z80.data("sms_palette", &sms_palette());
    z80.align(16);
    z80.data("title_sms_nametable", sms_name_table);
    z80.align(16);
    z80.data("sprite_attribute_clear", &vec![0xD0; 64]);
    z80.align(16);
    z80.data("world_1_1_initial_screen", world_1_1_screen);

    let output = z80.finish()?;

    if output.bytes.len() > 0x7FF0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SMS demo code/data overlaps header area",
        ));
    }

    let mut rom = vec![0x00; 32 * 1024];
    rom[..output.bytes.len()].copy_from_slice(&output.bytes);
    write_sms_header(&mut rom);
    Ok(SmsBuild {
        rom,
        asm: output.asm,
    })
}

fn sms_palette() -> [u8; 32] {
    fn rgb(r: u8, g: u8, b: u8) -> u8 {
        (r & 3) | ((g & 3) << 2) | ((b & 3) << 4)
    }

    let bg = [
        rgb(0, 0, 0),
        rgb(3, 3, 3),
        rgb(0, 2, 3),
        rgb(2, 1, 0),
        rgb(0, 3, 0),
        rgb(3, 2, 1),
        rgb(3, 0, 0),
        rgb(2, 2, 2),
        rgb(0, 0, 2),
        rgb(3, 3, 0),
        rgb(1, 1, 1),
        rgb(0, 3, 3),
        rgb(3, 0, 3),
        rgb(1, 2, 0),
        rgb(2, 3, 3),
        rgb(0, 0, 0),
    ];
    let mut palette = [0u8; 32];
    palette[..16].copy_from_slice(&bg);
    palette[16..].copy_from_slice(&bg);
    palette
}

struct TitleDecode {
    nes_name_table: Vec<u8>,
    sms_name_table: Vec<u8>,
    report: String,
}

fn decode_smb_title_screen(chr: &[u8]) -> TitleDecode {
    const TITLE_DATA_OFFSET: usize = 0x1EC0;
    const TITLE_DATA_LEN: usize = 0x013A;

    let data = &chr[TITLE_DATA_OFFSET..TITLE_DATA_OFFSET + TITLE_DATA_LEN];
    let mut nametable = vec![0x24u8; 32 * 30];
    let mut attributes = [0u8; 64];
    let mut pos = 0usize;
    let mut commands = Vec::new();

    while pos < data.len() {
        let high = data[pos];
        if high == 0 {
            commands.push("terminator".to_string());
            break;
        }
        if pos + 2 >= data.len() {
            commands.push(format!("truncated command at title byte ${pos:04X}"));
            break;
        }

        let low = data[pos + 1];
        let control = data[pos + 2];
        let addr = u16::from_be_bytes([high, low]);
        let len = (control & 0x3f) as usize;
        let repeat = control & 0x40 != 0;
        let increment = if control & 0x80 != 0 { 32u16 } else { 1u16 };
        pos += 3;

        if len == 0 {
            commands.push(format!(
                "${addr:04X}: zero-length command control ${control:02X}"
            ));
            continue;
        }

        if repeat {
            if pos >= data.len() {
                commands.push(format!("truncated repeat command for ${addr:04X}"));
                break;
            }
            let value = data[pos];
            pos += 1;
            for i in 0..len {
                apply_title_ppu_write(
                    &mut nametable,
                    &mut attributes,
                    addr.wrapping_add(increment * i as u16),
                    value,
                );
            }
            commands.push(format!(
                "${addr:04X}: len {len:02} inc {increment:02} repeat ${value:02X}"
            ));
        } else {
            if pos + len > data.len() {
                commands.push(format!("truncated literal command for ${addr:04X}"));
                break;
            }
            for i in 0..len {
                let value = data[pos + i];
                apply_title_ppu_write(
                    &mut nametable,
                    &mut attributes,
                    addr.wrapping_add(increment * i as u16),
                    value,
                );
            }
            pos += len;
            commands.push(format!(
                "${addr:04X}: len {len:02} inc {increment:02} literal"
            ));
        }
    }

    let sms_name_table = sms_name_table_from_nes(&nametable, &attributes);
    let report = format!(
        "\
SMB title screen decode
=======================

Source:
- DrawTitleScreen reads a VRAM update buffer from CHR/PPU ${:04X}.
- The NES routine copies {:} bytes into VRAM_Buffer1.
- UpdateScreen then interprets the buffer as address/length/data commands.

This PoC decodes those real SMB commands and converts the resulting NES
nametable into an SMS Mode 4 name table. This replaces the earlier sequential
tile-number test pattern.

Current mapping:
- NES blank tile defaults to $24, matching InitializeNameTables.
- NES background pattern table is $1000-$1FFF; these 256 tiles are loaded as SMS
  tile indices 0-255.
- NES attribute table bytes are preserved in the decoded model, but SMS palette
  selection still uses a simple global palette for now.
- The SMS screen uses the top 24 NES nametable rows.

Commands decoded:
{}
",
        TITLE_DATA_OFFSET,
        TITLE_DATA_LEN,
        commands
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    TitleDecode {
        nes_name_table: nametable,
        sms_name_table,
        report,
    }
}

fn apply_title_ppu_write(nametable: &mut [u8], attributes: &mut [u8; 64], addr: u16, value: u8) {
    let mirrored = 0x2000 + ((addr.wrapping_sub(0x2000)) & 0x0fff);
    match mirrored {
        0x2000..=0x23bf => {
            let offset = (mirrored - 0x2000) as usize;
            if offset < nametable.len() {
                nametable[offset] = value;
            }
        }
        0x23c0..=0x23ff => {
            attributes[(mirrored - 0x23c0) as usize] = value;
        }
        _ => {}
    }
}

fn sms_name_table_from_nes(nametable: &[u8], attributes: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 * 24 * 2);
    for y in 0..24usize {
        for x in 0..32usize {
            let tile = nametable[y * 32 + x] as u16;
            let attr_index = (y / 4) * 8 + (x / 4);
            let attr = attributes[attr_index];
            let quadrant = ((y % 4) / 2) * 2 + ((x % 4) / 2);
            let subpalette = (attr >> (quadrant * 2)) & 0x03;
            let sms_flags = if subpalette != 0 { 0x08 } else { 0x00 };
            out.push((tile & 0xff) as u8);
            out.push(sms_flags);
        }
    }
    out
}

fn build_world_1_1_screen(include_objects: bool) -> Vec<u8> {
    const BACK_SCENERY_DATA: [u8; 48] = [
        0x97, 0x87, 0x88, 0x89, 0x99, 0x00, 0x00, 0x00, 0x11, 0x12, 0x13, 0xa4, 0xa5, 0xa5, 0xa5,
        0xa6, 0x97, 0x98, 0x99, 0x01, 0x02, 0x03, 0x00, 0xa4, 0xa5, 0xa6, 0x00, 0x11, 0x12, 0x12,
        0x12, 0x13, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x02, 0x03, 0x00, 0xa4, 0xa5, 0xa5, 0xa6,
        0x00, 0x00, 0x00,
    ];
    const BACK_SCENERY_METATILES: [u8; 36] = [
        0x80, 0x83, 0x00, 0x81, 0x84, 0x00, 0x82, 0x85, 0x00, 0x02, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x04, 0x00, 0x00, 0x00, 0x05, 0x06, 0x07, 0x06, 0x0a, 0x00, 0x08, 0x09, 0x4d, 0x00, 0x00,
        0x0d, 0x0f, 0x4e, 0x0e, 0x4e, 0x4e,
    ];
    const TERRAIN_RENDER_BITS: [u8; 32] = [
        0x00, 0x00, 0x00, 0x18, 0x01, 0x18, 0x07, 0x18, 0x0f, 0x18, 0xff, 0x18, 0x01, 0x1f, 0x07,
        0x1f, 0x0f, 0x1f, 0x81, 0x1f, 0x01, 0x00, 0x8f, 0x1f, 0xf1, 0x1f, 0xf9, 0x18, 0xf1, 0x18,
        0xff, 0x1f,
    ];
    const BITMASKS: [u8; 8] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80];

    let mut name_entries = vec![(0x24u8, 0x00u8); 32 * 24];
    for metatile_col in 0..16usize {
        let mut metatiles = [0u8; 13];

        // Header for World 1-1: BackgroundScenery=1, TerrainControl=1,
        // AreaType=ground. This mirrors AreaParserCore's background and terrain
        // path before level object overlays are applied.
        let scenery = BACK_SCENERY_DATA[metatile_col];
        if scenery != 0 {
            let low = (scenery & 0x0f).saturating_sub(1);
            let mut table_index = low as usize * 3;
            let mut row = (scenery >> 4) as usize;
            for _ in 0..3 {
                if row == 0x0b || table_index >= BACK_SCENERY_METATILES.len() {
                    break;
                }
                metatiles[row] = BACK_SCENERY_METATILES[table_index];
                table_index += 1;
                row += 1;
            }
        }

        let terrain_y = 1usize * 2;
        let terrain_metatile = 0x54u8;
        let mut row = 0usize;
        for byte in [
            TERRAIN_RENDER_BITS[terrain_y],
            TERRAIN_RENDER_BITS[terrain_y + 1],
        ] {
            for bit in BITMASKS {
                if row >= 13 {
                    break;
                }
                if byte & bit != 0 {
                    metatiles[row] = terrain_metatile;
                }
                row += 1;
            }
        }

        for (metatile_row, metatile) in metatiles.iter().copied().enumerate().take(12) {
            put_metatile_entry(&mut name_entries, metatile_col, metatile_row, metatile);
        }
    }

    if include_objects {
        apply_world_1_1_visible_object_subset(&mut name_entries);
    }

    let mut out = Vec::with_capacity(32 * 24 * 2);
    for (tile, flags) in name_entries {
        out.push(tile);
        out.push(flags);
    }
    out
}

fn apply_world_1_1_visible_object_subset(entries: &mut [(u8, u8)]) {
    render_world_1_1_object_page(entries, 1);
}

struct ActiveRenderObject {
    object: ParsedAreaObject,
    remaining_columns: u8,
    column_offset: u8,
}

fn render_world_1_1_object_page(entries: &mut [(u8, u8)], page: u8) {
    let objects = parsed_area_objects(WORLD_1_1_AREA);
    let mut active: Vec<ActiveRenderObject> = Vec::new();

    for local_column in 0..16u8 {
        for object in objects
            .iter()
            .copied()
            .filter(|object| object.page == page && object.column == local_column)
        {
            if let Some(remaining_columns) = render_duration_columns(object) {
                active.push(ActiveRenderObject {
                    object,
                    remaining_columns,
                    column_offset: 0,
                });
            }
        }

        for active_object in &mut active {
            render_active_object_column(entries, local_column, active_object);
            active_object.remaining_columns = active_object.remaining_columns.saturating_sub(1);
            active_object.column_offset = active_object.column_offset.saturating_add(1);
        }
        active.retain(|object| object.remaining_columns > 0);
    }
}

fn render_duration_columns(object: ParsedAreaObject) -> Option<u8> {
    match object.class.handler {
        "QuestionBlock(power-up)" | "QuestionBlock(coin)" => Some(1),
        "RowOfBricks" => Some(object.len.saturating_add(1)),
        "VerticalPipe(decoration)" | "VerticalPipe(warp)" => Some(2),
        _ => None,
    }
}

fn render_active_object_column(
    entries: &mut [(u8, u8)],
    local_column: u8,
    active_object: &ActiveRenderObject,
) {
    let object = active_object.object;
    match object.class.handler {
        "QuestionBlock(power-up)" => {
            put_metatile_entry(entries, local_column as usize, object.row as usize, 0xc1)
        }
        "QuestionBlock(coin)" => {
            put_metatile_entry(entries, local_column as usize, object.row as usize, 0xc0)
        }
        "RowOfBricks" => {
            put_metatile_entry(entries, local_column as usize, object.row as usize, 0x51)
        }
        "VerticalPipe(decoration)" | "VerticalPipe(warp)" => {
            let height = object.len.max(1);
            match active_object.column_offset {
                0 => {
                    put_metatile_entry(entries, local_column as usize, object.row as usize, 0x13);
                    for pipe_row in object.row.saturating_add(1)..=object.row.saturating_add(height)
                    {
                        put_metatile_entry(entries, local_column as usize, pipe_row as usize, 0x15);
                    }
                }
                1 => {
                    put_metatile_entry(entries, local_column as usize, object.row as usize, 0x12);
                    for pipe_row in object.row.saturating_add(1)..=object.row.saturating_add(height)
                    {
                        put_metatile_entry(entries, local_column as usize, pipe_row as usize, 0x14);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn put_metatile_entry(
    entries: &mut [(u8, u8)],
    metatile_x: usize,
    metatile_y: usize,
    metatile: u8,
) {
    let [tl, tr, bl, br] = metatile_tiles(metatile);
    let tile_x = metatile_x * 2;
    let tile_y = metatile_y * 2;
    put_name_entry(entries, tile_x, tile_y, tl);
    put_name_entry(entries, tile_x + 1, tile_y, tr);
    put_name_entry(entries, tile_x, tile_y + 1, bl);
    put_name_entry(entries, tile_x + 1, tile_y + 1, br);
}

fn put_name_entry(entries: &mut [(u8, u8)], x: usize, y: usize, tile: u8) {
    if x < 32 && y < 24 {
        entries[y * 32 + x] = (tile, 0);
    }
}

fn metatile_tiles(metatile: u8) -> [u8; 4] {
    let palette = metatile >> 6;
    let index = (metatile & 0x3f) as usize;
    let table = match palette {
        0 => PALETTE0_MTILES,
        1 => PALETTE1_MTILES,
        2 => PALETTE2_MTILES,
        _ => PALETTE3_MTILES,
    };
    let base = index * 4;
    if base + 3 < table.len() {
        [
            table[base],
            table[base + 1],
            table[base + 2],
            table[base + 3],
        ]
    } else {
        [0x24, 0x24, 0x24, 0x24]
    }
}

const PALETTE0_MTILES: &[u8] = &[
    0x24, 0x24, 0x24, 0x24, 0x27, 0x27, 0x27, 0x27, 0x24, 0x24, 0x24, 0x35, 0x36, 0x25, 0x37, 0x25,
    0x24, 0x38, 0x24, 0x24, 0x24, 0x30, 0x30, 0x26, 0x26, 0x26, 0x34, 0x26, 0x24, 0x31, 0x24, 0x32,
    0x33, 0x26, 0x24, 0x33, 0x34, 0x26, 0x26, 0x26, 0x26, 0x26, 0x26, 0x26,
];
const PALETTE1_MTILES: &[u8] = &[
    0xa2, 0xa2, 0xa3, 0xa3, 0x99, 0x24, 0x99, 0x24, 0x24, 0xa2, 0x3e, 0x3f, 0x5b, 0x5c, 0x24, 0xa3,
    0x24, 0x24, 0x24, 0x24, 0x9d, 0x47, 0x9e, 0x47, 0x47, 0x47, 0x27, 0x27, 0x47, 0x47, 0x47, 0x47,
    0x27, 0x27, 0x47, 0x47, 0xa9, 0x47, 0xaa, 0x47, 0x9b, 0x27, 0x9c, 0x27, 0x27, 0x27, 0x27, 0x27,
    0x52, 0x52, 0x52, 0x52, 0x80, 0xa0, 0x81, 0xa1, 0xbe, 0xbe, 0xbf, 0xbf, 0x75, 0xba, 0x76, 0xbb,
    0xba, 0xba, 0xbb, 0xbb, 0x45, 0x47, 0x45, 0x47, 0x47, 0x47, 0x47, 0x47, 0x45, 0x47, 0x45, 0x47,
    0xb4, 0xb6, 0xb5, 0xb7,
];
const PALETTE2_MTILES: &[u8] = &[
    0x24, 0x24, 0x24, 0x35, 0x36, 0x25, 0x37, 0x25, 0x24, 0x38, 0x24, 0x24, 0x24, 0x24, 0x39, 0x24,
    0x3a, 0x24, 0x3b, 0x24, 0x3c, 0x24, 0x24, 0x24, 0x41, 0x26, 0x41, 0x26, 0x26, 0x26, 0x26, 0x26,
    0xb0, 0xb1, 0xb2, 0xb3, 0x77, 0x79, 0x77, 0x79,
];
const PALETTE3_MTILES: &[u8] = &[
    0x53, 0x55, 0x54, 0x56, 0x53, 0x55, 0x54, 0x56, 0xa5, 0xa7, 0xa6, 0xa8, 0xc2, 0xc4, 0xc3, 0xc5,
    0x57, 0x59, 0x58, 0x5a, 0x7b, 0x7d, 0x7c, 0x7e,
];

fn write_sms_header(rom: &mut [u8]) {
    let header = 0x7FF0;
    rom[header..header + 8].copy_from_slice(b"TMR SEGA");
    rom[0x7FF8] = 0x00;
    rom[0x7FF9] = 0x00;
    rom[0x7FFA] = 0x00;
    rom[0x7FFB] = 0x00;
    rom[0x7FFC] = 0x00;
    rom[0x7FFD] = 0x00;
    rom[0x7FFE] = 0x00;
    rom[0x7FFF] = 0x4C; // Export SMS, 32 KB ROM.

    let checksum = sms_checksum_32k(rom);
    let [lo, hi] = checksum.to_le_bytes();
    rom[0x7FFA] = lo;
    rom[0x7FFB] = hi;
}

fn sms_checksum_32k(rom: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for (i, byte) in rom.iter().take(32 * 1024).enumerate() {
        if i == 0x7FFA || i == 0x7FFB {
            continue;
        }
        sum = sum.wrapping_add(*byte as u16);
    }
    sum
}

fn tile_sheet_ppm(chr: &[u8]) -> Vec<u8> {
    let tiles = chr.len() / 16;
    let columns = 16usize;
    let rows = tiles.div_ceil(columns);
    let width = columns * 8;
    let height = rows * 8;
    let colors = [
        [0u8, 0, 0],
        [255u8, 255, 255],
        [80u8, 160, 255],
        [192u8, 96, 32],
    ];

    let mut pixels = vec![[0u8, 0, 0]; width * height];
    for tile_index in 0..tiles {
        let tile = &chr[tile_index * 16..tile_index * 16 + 16];
        let tile_x = (tile_index % columns) * 8;
        let tile_y = (tile_index / columns) * 8;
        for row in 0..8 {
            let p0 = tile[row];
            let p1 = tile[row + 8];
            for col in 0..8 {
                let bit = 7 - col;
                let value = ((p0 >> bit) & 1) | (((p1 >> bit) & 1) << 1);
                pixels[(tile_y + row) * width + tile_x + col] = colors[value as usize];
            }
        }
    }

    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    for pixel in pixels {
        out.extend_from_slice(&pixel);
    }
    out
}

fn translate_reset_sketch(prg: &[u8], reset: u16) -> String {
    let mut out = String::new();
    out.push_str("; Rough 6502-to-Z80 translation sketch generated by poc.\n");
    out.push_str("; This is not expected to assemble yet; it exposes remapping decisions.\n\n");
    out.push_str("NES_ZP_BASE  equ $C000\n");
    out.push_str("NES_RAM_BASE equ $C000\n");
    out.push_str("NES_STACK    equ $C100\n\n");
    out.push_str(&format!("L_{reset:04X}:\n"));

    let Some(mut pc_offset) = cpu_to_prg_offset(prg, reset) else {
        out.push_str("; reset vector is outside PRG\n");
        return out;
    };
    let mut pc = reset;

    for _ in 0..128 {
        if pc_offset >= prg.len() {
            break;
        }
        let (asm, len, z80, terminal) = decode_6502_to_z80(prg, pc_offset, pc);
        out.push_str(&format!("; ${pc:04X}: {asm}\n"));
        for line in z80 {
            out.push_str("    ");
            out.push_str(&line);
            out.push('\n');
        }
        out.push('\n');
        if terminal {
            break;
        }
        pc = pc.wrapping_add(len as u16);
        pc_offset += len;
    }

    out
}

fn title_menu_port_report() -> &'static str {
    r#"; SMB title menu port slice
; =========================
;
; This file documents the runnable title-menu behavior currently emitted
; directly as Z80 machine code into poc.sms.
;
; Source 6502 routines/data:
; - GameMenuRoutine
; - DrawMushroomIcon
; - MushroomIconData
; - GameText::WorldLivesDisplay
;
; Controller mapping:
; - SMS button 1 -> NES Select
; - SMS button 2 -> NES Start
;
; Why this mapping exists:
; - Standard SMS controllers have directional input plus two buttons.
; - NES has Select and Start in addition to A/B.
; - This PoC uses the two SMS buttons to exercise the title menu path first.
;
; Ported behavior:
; - Button 1 toggles the one-player/two-player mushroom cursor.
; - Cursor writes are based on MushroomIconData:
;     .db $07, $22, $49, $83, $ce, $24, $24, $00
; - The original command writes three vertical nametable tiles at NES $2249.
; - The PoC maps those to SMS name table entries in VRAM.
;
; Runtime Z80 shape:
;
; main_loop:
;     in   a,($dc)       ; SMS controller port, active low
;     cpl
;     and  $30           ; buttons 1/2
;     debounce against previous state in $c001
;     call nz,toggle_cursor_for_button_1
;     call nz,start_transition_for_button_2
;     jp   main_loop
;
; toggle_cursor_for_button_1:
;     xor selection byte at $c000
;     if zero: write [$ce,$24,$24]
;     else:    write [$24,$ce,$24]
;
; start_transition_for_button_2:
;     writes a first real GameText::WorldLivesDisplay slice:
;       $21cd len 7: $24,$24,$29,$24,$24,$24,$24
;       $214b len 9: $20,$18,$1b,$15,$0d,$24,$24,$28,$24
;
; Current limitation:
; - This is a hand-port of a small behavior slice, not a full generated
;   6502-to-Z80 translation of GameMenuRoutine yet.
; - The next step is to emit this Z80 from a decoded 6502 routine model.
"#
}

fn world_1_1_report() -> String {
    let header1 = WORLD_1_1_AREA[0];
    let header2 = WORLD_1_1_AREA[1];
    let foreground_scenery = if header1 & 0x07 < 4 {
        header1 & 0x07
    } else {
        0
    };
    let background_color_ctrl = if header1 & 0x07 >= 4 {
        header1 & 0x07
    } else {
        0
    };
    let player_entrance_ctrl = (header1 & 0x38) >> 3;
    let game_timer_setting = (header1 & 0xc0) >> 6;
    let terrain_control = header2 & 0x0f;
    let background_scenery = (header2 & 0x30) >> 4;
    let mut area_style = (header2 & 0xc0) >> 6;
    let mut cloud_override = 0;
    if area_style == 3 {
        cloud_override = 3;
        area_style = 0;
    }

    let mut objects = Vec::new();
    let mut i = 2usize;
    while i < WORLD_1_1_AREA.len() {
        let first = WORLD_1_1_AREA[i];
        if first == 0xfd {
            objects.push(format!("${i:02X}: terminator $fd"));
            i += 1;
            continue;
        }
        if i + 1 >= WORLD_1_1_AREA.len() {
            objects.push(format!("${i:02X}: truncated object ${first:02X}"));
            break;
        }
        let second = WORLD_1_1_AREA[i + 1];
        let page_or_column = first >> 4;
        let row = first & 0x0f;
        let object_id = second >> 4;
        let len_or_data = second & 0x0f;
        objects.push(format!(
            "${i:02X}: bytes ${first:02X} ${second:02X} -> page/col {page_or_column:02}, row {row:02}, object ${object_id:X}, len/data ${len_or_data:X}"
        ));
        i += 2;
    }

    let mut enemies = Vec::new();
    let mut j = 0usize;
    while j < WORLD_1_1_ENEMIES.len() {
        let first = WORLD_1_1_ENEMIES[j];
        if first == 0xff {
            enemies.push(format!("${j:02X}: terminator $ff"));
            break;
        }
        if j + 1 >= WORLD_1_1_ENEMIES.len() {
            enemies.push(format!("${j:02X}: truncated enemy ${first:02X}"));
            break;
        }
        let second = WORLD_1_1_ENEMIES[j + 1];
        enemies.push(format!(
            "${j:02X}: bytes ${first:02X} ${second:02X} -> page/col {}, row {}, id/data ${second:02X}",
            first >> 4,
            first & 0x0f
        ));
        j += 2;
    }

    format!(
        "\
World 1-1 data extraction
=========================

Source disassembly labels:
- Area pointer: World1Areas[0] = $25
- Area type: $25 & $60 -> 1, ground
- Area data: L_GroundArea6
- Enemy data: E_GroundArea6

Header bytes:
- Raw: ${header1:02X} ${header2:02X}
- ForegroundScenery: {foreground_scenery}
- BackgroundColorCtrl: {background_color_ctrl}
- PlayerEntranceCtrl: {player_entrance_ctrl}
- GameTimerSetting: {game_timer_setting}
- TerrainControl: {terrain_control}
- BackgroundScenery: {background_scenery}
- AreaStyle: {area_style}
- CloudTypeOverride: {cloud_override}

Area objects, rough first-pass field split:
{}

Enemy objects, rough first-pass field split:
{}

Current status:
- This report proves the PoC is using the real World 1-1 level and enemy byte streams.
- `world_1_1_area_trace.txt` contains a more faithful DecodeAreaData-style
  classification trace for this same stream.
",
        objects
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
        enemies
            .iter()
            .map(|line| format!("- {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn world_1_1_area_trace() -> String {
    let mut out = String::new();
    out.push_str("World 1-1 area parser trace\n");
    out.push_str("===========================\n\n");
    out.push_str("Source: SMB disassembly `L_GroundArea6` / `E_GroundArea6`.\n");
    out.push_str("This trace follows the object classification path in `DecodeAreaData`.\n");
    out.push_str(
        "It is not yet a full column renderer; it is the parser input to that renderer.\n\n",
    );

    let header1 = WORLD_1_1_AREA[0];
    let header2 = WORLD_1_1_AREA[1];
    out.push_str(&format!(
        "Header ${header1:02X} ${header2:02X}: terrain={}, background_scenery={}, area_style={}\n\n",
        header2 & 0x0f,
        (header2 & 0x30) >> 4,
        (header2 & 0xc0) >> 6
    ));

    out.push_str("Area object stream:\n");
    for line in trace_area_objects(WORLD_1_1_AREA) {
        out.push_str("- ");
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str("\nEnemy stream:\n");
    for line in trace_enemy_objects(WORLD_1_1_ENEMIES) {
        out.push_str("- ");
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str("\nNext renderer step:\n");
    out.push_str(
        "\n- Feed these classified objects into a metatile-column renderer that maintains \
         `AreaObjectLength`, `AreaObjOffsetBuffer`, `CurrentPageLoc`, and `CurrentColumnPos`.\n",
    );
    out
}

fn world_1_1_object_overlay_report() -> String {
    let mut out = String::new();
    out.push_str("World 1-1 initial object overlay\n");
    out.push_str("================================\n\n");
    out.push_str("Target screen: parser page 1, the first foreground object page used by the current static Start transition.\n");
    out.push_str("Implemented handlers: question blocks, row of bricks, and vertical pipe.\n");
    out.push_str("Rendering model: a small active-object column renderer. Objects start at their decoded page/column and render for their handler duration.\n");
    out.push_str("Deferred handlers: hidden 1-up state, enemies, full three-slot object buffers, collisions, and block interactions.\n\n");

    for object in parsed_area_objects(WORLD_1_1_AREA) {
        if object.page != 1 {
            continue;
        }
        let status = match object.class.handler {
            "QuestionBlock(power-up)"
            | "QuestionBlock(coin)"
            | "RowOfBricks"
            | "VerticalPipe(decoration)"
            | "VerticalPipe(warp)" => "rendered",
            _ => "deferred",
        };
        out.push_str(&format!(
            "- {status}: offset ${:02X}, ${:02X} ${:02X} page {}, col {:X}, row {:X}, {}, len ${:X}, handler {}\n",
            object.offset,
            object.first,
            object.second,
            object.page,
            object.column,
            object.row,
            object.class.group,
            object.len,
            object.class.handler
        ));
    }

    out
}

#[derive(Clone, Copy)]
struct ParsedAreaObject {
    offset: usize,
    first: u8,
    second: u8,
    page: u8,
    column: u8,
    row: u8,
    len: u8,
    class: AreaObjectClass,
}

fn parsed_area_objects(area: &[u8]) -> Vec<ParsedAreaObject> {
    let mut objects = Vec::new();
    let mut page_loc = 0u8;
    let mut offset = 2usize;

    while offset + 1 < area.len() {
        let first = area[offset];
        if first == 0xfd {
            break;
        }

        let second = area[offset + 1];
        if second & 0x80 != 0 {
            page_loc = page_loc.wrapping_add(1);
        }

        let row = first & 0x0f;
        if row == 0x0d && second & 0x40 == 0 {
            page_loc = second & 0x1f;
            offset += 2;
            continue;
        }

        objects.push(ParsedAreaObject {
            offset,
            first,
            second,
            page: page_loc,
            column: first >> 4,
            row,
            len: second & 0x0f,
            class: classify_area_object(row, second),
        });
        offset += 2;
    }

    objects
}

fn trace_area_objects(area: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut page_loc = 0u8;
    let mut offset = 2usize;

    while offset < area.len() {
        let first = area[offset];
        if first == 0xfd {
            lines.push(format!("${offset:02X}: $FD end-of-area"));
            break;
        }
        if offset + 1 >= area.len() {
            lines.push(format!("${offset:02X}: truncated byte ${first:02X}"));
            break;
        }

        let second = area[offset + 1];
        let mut events = Vec::new();
        if second & 0x80 != 0 {
            page_loc = page_loc.wrapping_add(1);
            events.push(format!("d7 page-select -> page {page_loc}"));
        }

        let column = first >> 4;
        let row = first & 0x0f;
        if row == 0x0d && second & 0x40 == 0 {
            page_loc = second & 0x1f;
            events.push(format!("row-13 page-control -> page {page_loc}"));
            lines.push(format!(
                "${offset:02X}: ${first:02X} ${second:02X} page-control column {column:X}, {}",
                events.join(", ")
            ));
            offset += 2;
            continue;
        }

        let decoded = classify_area_object(row, second);
        let event_text = if events.is_empty() {
            String::from("no page event")
        } else {
            events.join(", ")
        };
        lines.push(format!(
            "${offset:02X}: ${first:02X} ${second:02X} page {page_loc}, col {column:X}, row {row:X}, {} id ${:02X}, len/data ${:X}, handler {}, {event_text}",
            decoded.group,
            decoded.id,
            second & 0x0f,
            decoded.handler
        ));
        offset += 2;
    }

    lines
}

#[derive(Clone, Copy)]
struct AreaObjectClass {
    group: &'static str,
    id: u8,
    handler: &'static str,
}

fn classify_area_object(row: u8, second: u8) -> AreaObjectClass {
    const LARGE: [&str; 8] = [
        "VerticalPipe(warp)",
        "AreaStyleObject",
        "RowOfBricks",
        "RowOfSolidBlocks",
        "RowOfCoins",
        "ColumnOfBricks",
        "ColumnOfSolidBlocks",
        "VerticalPipe(decoration)",
    ];
    const ROW_12: [&str; 8] = [
        "Hole_Empty",
        "PulleyRopeObject",
        "Bridge_High",
        "Bridge_Middle",
        "Bridge_Low",
        "Hole_Water",
        "QuestionBlockRow_High",
        "QuestionBlockRow_Low",
    ];
    const ROW_15: [&str; 6] = [
        "EndlessRope",
        "BalancePlatRope",
        "CastleObject",
        "StaircaseObject",
        "ExitPipe",
        "FlagBalls_Residual",
    ];
    const SMALL: [&str; 12] = [
        "QuestionBlock(power-up)",
        "QuestionBlock(coin)",
        "QuestionBlock(hidden coin)",
        "Hidden1UpBlock",
        "BrickWithItem(power-up)",
        "BrickWithItem(vine)",
        "BrickWithItem(star)",
        "BrickWithCoins",
        "BrickWithItem(1-up)",
        "WaterPipe",
        "EmptyBlock",
        "Jumpspring",
    ];
    const ROW_13: [&str; 12] = [
        "IntroPipe",
        "FlagpoleObject",
        "AxeObj",
        "ChainObj",
        "CastleBridgeObj",
        "ScrollLockObject_Warp",
        "ScrollLockObject",
        "ScrollLockObject",
        "AreaFrenzy(flying cheep-cheeps)",
        "AreaFrenzy(bullet bills/swimming cheep-cheeps)",
        "AreaFrenzy(stop)",
        "LoopCmdE",
    ];

    match row {
        0x0f => {
            let id = (second & 0x70) >> 4;
            AreaObjectClass {
                group: "special-row-15",
                id,
                handler: ROW_15.get(id as usize).copied().unwrap_or("unknown-row-15"),
            }
        }
        0x0c => {
            let id = (second & 0x70) >> 4;
            AreaObjectClass {
                group: "special-row-12",
                id,
                handler: ROW_12.get(id as usize).copied().unwrap_or("unknown-row-12"),
            }
        }
        0x0e => AreaObjectClass {
            group: "special-row-14",
            id: 0x2e,
            handler: "AlterAreaAttributes",
        },
        0x0d => {
            let id = second & 0x3f;
            AreaObjectClass {
                group: "special-row-13",
                id,
                handler: ROW_13.get(id as usize).copied().unwrap_or("unknown-row-13"),
            }
        }
        _ => {
            let large_bits = second & 0x70;
            if large_bits != 0 {
                let normalized = if large_bits == 0x70 && second & 0x08 != 0 {
                    0
                } else {
                    large_bits
                };
                let id = normalized >> 4;
                AreaObjectClass {
                    group: "large",
                    id,
                    handler: LARGE.get(id as usize).copied().unwrap_or("unknown-large"),
                }
            } else {
                let id = second & 0x0f;
                AreaObjectClass {
                    group: "small",
                    id,
                    handler: SMALL.get(id as usize).copied().unwrap_or("unknown-small"),
                }
            }
        }
    }
}

fn trace_enemy_objects(enemies: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut offset = 0usize;
    let mut page = 0u8;

    while offset < enemies.len() {
        let first = enemies[offset];
        if first == 0xff {
            lines.push(format!("${offset:02X}: $FF end-of-enemies"));
            break;
        }
        if first == 0x0f && offset + 1 < enemies.len() {
            page = enemies[offset + 1] & 0x0f;
            lines.push(format!(
                "${offset:02X}: ${first:02X} ${:02X} page-control -> page {page}",
                enemies[offset + 1]
            ));
            offset += 2;
            continue;
        }
        if offset + 1 >= enemies.len() {
            lines.push(format!("${offset:02X}: truncated byte ${first:02X}"));
            break;
        }

        let second = enemies[offset + 1];
        if first & 0x80 != 0 {
            page = page.wrapping_add(1);
        }
        let column = first & 0x0f;
        let y = (second & 0xf0) >> 4;
        let id = second & 0x0f;
        lines.push(format!(
            "${offset:02X}: ${first:02X} ${second:02X} page {page}, col {column:X}, y {y:X}, id ${id:X}"
        ));
        offset += 2;
    }

    lines
}

fn decode_6502_to_z80(prg: &[u8], offset: usize, pc: u16) -> (String, usize, Vec<String>, bool) {
    let op = prg[offset];
    let b1 = prg.get(offset + 1).copied().unwrap_or(0);
    let b2 = prg.get(offset + 2).copied().unwrap_or(0);
    let word = u16::from_le_bytes([b1, b2]);
    let rel = pc.wrapping_add(2).wrapping_add((b1 as i8) as i16 as u16);

    match op {
        0x78 => ("SEI".into(), 1, vec!["di".into()], false),
        0xD8 => (
            "CLD".into(),
            1,
            vec!["; decimal mode absent on NES CPU".into()],
            false,
        ),
        0xA9 => (
            format!("LDA #${b1:02X}"),
            2,
            vec![format!("ld a,${b1:02X}")],
            false,
        ),
        0xA2 => (
            format!("LDX #${b1:02X}"),
            2,
            vec![format!("ld b,${b1:02X} ; emulated 6502 X")],
            false,
        ),
        0xA0 => (
            format!("LDY #${b1:02X}"),
            2,
            vec![format!("ld c,${b1:02X} ; emulated 6502 Y")],
            false,
        ),
        0xA5 => (
            format!("LDA ${b1:02X}"),
            2,
            vec![format!("ld a,(NES_ZP_BASE+${b1:02X})")],
            false,
        ),
        0xAD => (
            format!("LDA ${word:04X}"),
            3,
            translate_load_abs(word),
            false,
        ),
        0xBD => (
            format!("LDA ${word:04X},X"),
            3,
            vec![
                format!("; TODO indexed load from NES address ${word:04X}+X"),
                "ld a,(hl) ; placeholder after address remap".into(),
            ],
            false,
        ),
        0xAA => (
            "TAX".into(),
            1,
            vec!["ld b,a ; emulated 6502 X".into()],
            false,
        ),
        0x8A => (
            "TXA".into(),
            1,
            vec!["ld a,b ; emulated 6502 X".into()],
            false,
        ),
        0x9A => (
            "TXS".into(),
            1,
            vec!["; TODO: map 6502 stack pointer X to SMS RAM stack model".into()],
            false,
        ),
        0x85 => (
            format!("STA ${b1:02X}"),
            2,
            vec![format!("ld (NES_ZP_BASE+${b1:02X}),a")],
            false,
        ),
        0x86 => (
            format!("STX ${b1:02X}"),
            2,
            vec![format!("ld a,b"), format!("ld (NES_ZP_BASE+${b1:02X}),a")],
            false,
        ),
        0x84 => (
            format!("STY ${b1:02X}"),
            2,
            vec![format!("ld a,c"), format!("ld (NES_ZP_BASE+${b1:02X}),a")],
            false,
        ),
        0x8D => (
            format!("STA ${word:04X}"),
            3,
            translate_store_abs(word, "a"),
            false,
        ),
        0x8E => (
            format!("STX ${word:04X}"),
            3,
            translate_store_abs(word, "b"),
            false,
        ),
        0x8C => (
            format!("STY ${word:04X}"),
            3,
            translate_store_abs(word, "c"),
            false,
        ),
        0x4C => (
            format!("JMP ${word:04X}"),
            3,
            vec![format!("jp L_{word:04X}")],
            true,
        ),
        0x20 => (
            format!("JSR ${word:04X}"),
            3,
            vec![format!("call L_{word:04X}")],
            false,
        ),
        0xC9 => (
            format!("CMP #${b1:02X}"),
            2,
            vec![
                format!("cp ${b1:02X}"),
                "; after CP, 6502 BCS maps to Z80 JP NC for this pattern".into(),
            ],
            false,
        ),
        0x09 => (
            format!("ORA #${b1:02X}"),
            2,
            vec![format!("or ${b1:02X}")],
            false,
        ),
        0xCA => (
            "DEX".into(),
            1,
            vec!["dec b ; emulated 6502 X".into()],
            false,
        ),
        0xEE => (
            format!("INC ${word:04X}"),
            3,
            translate_inc_abs(word),
            false,
        ),
        0x60 => ("RTS".into(), 1, vec!["ret".into()], true),
        0xD0 => (
            format!("BNE ${rel:04X}"),
            2,
            vec![format!(
                "jp nz,L_{rel:04X} ; only valid if Z flag is preserved"
            )],
            false,
        ),
        0xF0 => (
            format!("BEQ ${rel:04X}"),
            2,
            vec![format!(
                "jp z,L_{rel:04X} ; only valid if Z flag is preserved"
            )],
            false,
        ),
        0x10 => (
            format!("BPL ${rel:04X}"),
            2,
            vec![format!("jp p,L_{rel:04X} ; 6502 N flag approximation")],
            false,
        ),
        0x30 => (
            format!("BMI ${rel:04X}"),
            2,
            vec![format!("jp m,L_{rel:04X} ; 6502 N flag approximation")],
            false,
        ),
        0xB0 => (
            format!("BCS ${rel:04X}"),
            2,
            vec![format!("jp nc,L_{rel:04X} ; valid after translated CMP/CP")],
            false,
        ),
        0xEA => ("NOP".into(), 1, vec!["nop".into()], false),
        _ => (
            format!(".db ${op:02X}"),
            1,
            vec![format!("; TODO unsupported opcode ${op:02X}")],
            false,
        ),
    }
}

fn translate_load_abs(addr: u16) -> Vec<String> {
    match addr {
        0x0000..=0x07FF => vec![format!("ld a,(NES_RAM_BASE+${addr:04X})")],
        0x2000..=0x2007 => vec![format!("call nes_ppu_read_{addr:04X}_shim")],
        0x4000..=0x4017 => vec![format!(
            "; input/audio read ${addr:04X} needs a platform shim"
        )],
        _ => vec![format!("; TODO load A from NES address ${addr:04X}")],
    }
}

fn translate_store_abs(addr: u16, reg: &str) -> Vec<String> {
    let value = if reg == "a" {
        vec![]
    } else {
        vec![format!("ld a,{reg}")]
    };
    let mut out = value;

    match addr {
        0x0000..=0x07FF => out.push(format!("ld (NES_RAM_BASE+${addr:04X}),a")),
        0x2000..=0x2007 => out.push(format!("call nes_ppu_write_{addr:04X}_shim")),
        0x4014 => out.push("call nes_oam_dma_to_sms_sprite_shim".into()),
        0x4000..=0x4017 => out.push(format!(
            "; audio/input register ${addr:04X} stubbed in this PoC"
        )),
        _ => out.push(format!("; TODO store A to NES address ${addr:04X}")),
    }

    out
}

fn translate_inc_abs(addr: u16) -> Vec<String> {
    match addr {
        0x0000..=0x07FF => vec![format!(
            "inc (NES_RAM_BASE+${addr:04X}) ; pseudo-Z80, needs HL form"
        )],
        _ => vec![format!("; TODO increment NES address ${addr:04X}")],
    }
}

fn header_report(path: &str, len: usize, h: &InesHeader) -> String {
    format!(
        "\
ROM: {path}
File size: {len} bytes
Format: {}
PRG ROM: {} banks, {} bytes
CHR ROM: {} banks, {} bytes
Mapper: {}
Flags6: ${:02X}
Flags7: ${:02X}
Trainer: {}
Battery: {}
Mirroring: {}
",
        if h.nes2 { "NES 2.0" } else { "iNES" },
        h.prg_banks,
        h.prg_len(),
        h.chr_banks,
        h.chr_len(),
        h.mapper,
        h.flags6,
        h.flags7,
        h.has_trainer,
        h.has_battery,
        if h.vertical_mirroring {
            "vertical"
        } else {
            "horizontal"
        },
    )
}

fn vector_report(v: &Vectors) -> String {
    format!(
        "\
NMI:   ${:04X}
Reset: ${:04X}
IRQ:   ${:04X}
",
        v.nmi, v.reset, v.irq
    )
}

fn map_json(h: &InesHeader, v: &Vectors) -> String {
    format!(
        r#"{{
  "format": "{}",
  "mapper": {},
  "prg_bytes": {},
  "chr_bytes": {},
  "trainer": {},
  "battery": {},
  "mirroring": "{}",
  "cpu_map": {{
    "prg_rom": "$8000-$FFFF",
    "nrom_prg_offset": "cpu_address - $8000",
    "zero_page_to_sms": "$0000-$00FF -> $C000-$C0FF",
    "ram_to_sms": "$0000-$07FF -> $C000-$C7FF",
    "ppu_registers": "$2000-$2007 -> SMS VDP shim calls",
    "apu_registers": "$4000-$4017 -> stubbed"
  }},
  "vectors": {{
    "nmi": "${:04X}",
    "reset": "${:04X}",
    "irq": "${:04X}"
  }}
}}
"#,
        if h.nes2 { "NES 2.0" } else { "iNES" },
        h.mapper,
        h.prg_len(),
        h.chr_len(),
        h.has_trainer,
        h.has_battery,
        if h.vertical_mirroring {
            "vertical"
        } else {
            "horizontal"
        },
        v.nmi,
        v.reset,
        v.irq
    )
}

fn summary_report(h: &InesHeader, v: &Vectors) -> String {
    format!(
        "\
PoC artifact summary
====================

Input classification:
- Mapper {} / {}
- PRG: {} bytes
- CHR: {} bytes
- Reset vector: ${:04X}

Generated files:
- prg.bin: raw NES PRG ROM
- chr.bin: raw NES CHR ROM
- tiles.sms4bpp: CHR expanded to SMS Mode 4 4bpp tile format
- title_nametable.bin: NES title screen nametable reconstructed from real SMB VRAM commands
- title_sms_nametable.bin: reconstructed title screen converted to SMS name table format
- title_decode.txt: decoded DrawTitleScreen/UpdateScreen command stream
- tiles.ppm: quick visual tile-sheet preview
- poc.sms: 32 KB SMS ROM that loads SMB background CHR and reconstructed title data
- generated.asm: readable Z80 listing emitted from the same backend as poc.sms
- ir_lowering_demo.asm/bin: first 6502 IR lowered to real Z80 bytes from SMB $9CA6
- ir_lowering_report.txt: source/IR/lowering notes for the $9CA6 demo
- ir_lowering_validation.txt: local Z80 state-oracle validation for the IR demo
- ir_branch_demo.asm/bin: second IR lowering, from SMB $B1B4 branch/store slice
- ir_branch_report.txt: source/IR/lowering notes for the branch/store demo
- ir_branch_validation.txt: local Z80 state-oracle validation for branch behavior
- ir_compare_demo.asm/bin: third IR lowering, from SMB $AEF9 compare/carry branch slice
- ir_compare_report.txt: source/IR/lowering notes for the compare/carry demo
- ir_compare_validation.txt: local Z80 validation for CMP/BCS behavior
- z80_translation_experiment.asm: rough reset-routine translation sketch

Current limitations:
- The SMS ROM displays a reconstructed title-screen nametable, not gameplay yet.
- Audio is skipped.
- The translation sketch is intentionally partial and not ready to assemble.
- NES PPU writes are represented as shim calls, because they cannot be translated
  as ordinary memory stores.
",
        h.mapper,
        if h.mapper == 0 { "NROM" } else { "non-NROM" },
        h.prg_len(),
        h.chr_len(),
        v.reset
    )
}
