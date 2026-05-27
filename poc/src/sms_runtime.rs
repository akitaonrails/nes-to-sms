use crate::z80_backend::Z80Program;

pub fn emit_vdp_reg(z80: &mut Z80Program, reg: u8, value: u8) {
    z80.ld_a_imm(value);
    z80.out_a(0xBF);
    z80.ld_a_imm(0x80 | reg);
    z80.out_a(0xBF);
}

pub fn emit_copy_to_vram_label(z80: &mut Z80Program, vram_addr: u16, len: u16, label: &str) {
    emit_vdp_addr(z80, 0x4000 | vram_addr);
    z80.ld_hl_label(label);
    z80.ld_bc_imm(len);
    emit_copy_loop(z80);
}

pub fn emit_copy_to_cram_label(z80: &mut Z80Program, cram_addr: u8, len: u16, label: &str) {
    emit_vdp_addr(z80, 0xC000 | cram_addr as u16);
    z80.ld_hl_label(label);
    z80.ld_bc_imm(len);
    emit_copy_loop(z80);
}

pub fn emit_title_cursor_writes(z80: &mut Z80Program, tiles: [u8; 3]) {
    // Original SMB DrawMushroomIcon writes tile $CE vertically at NES name
    // table $2249. These are SMS name-table entries at $3800 + offset * 2.
    for (vram_addr, tile) in [
        (nes_addr_to_sms_name_vram(0x2249), tiles[0]),
        (nes_addr_to_sms_name_vram(0x2269), tiles[1]),
        (nes_addr_to_sms_name_vram(0x2289), tiles[2]),
    ] {
        emit_sms_name_entry(z80, vram_addr, tile, 0x00);
    }
}

pub fn emit_world_lives_display_writes(z80: &mut Z80Program) {
    // Real SMB GameText::WorldLivesDisplay data:
    //   $21CD len 7  -> spaces/cross/lives placeholder
    //   $214B len 9  -> "WORLD  - " placeholder
    //   $220C repeat -> clear "TIME UP", skipped here because this is only
    //   the first visible Start transition slice.
    emit_nes_horizontal_text(z80, 0x21CD, &[0x24, 0x24, 0x29, 0x24, 0x24, 0x24, 0x24]);
    emit_nes_horizontal_text(
        z80,
        0x214B,
        &[0x20, 0x18, 0x1B, 0x15, 0x0D, 0x24, 0x24, 0x28, 0x24],
    );
}

pub fn emit_sms_name_entry(z80: &mut Z80Program, vram_addr: u16, tile: u8, flags: u8) {
    emit_vdp_addr(z80, 0x4000 | vram_addr);
    z80.ld_a_imm(tile);
    z80.out_a(0xBE);
    z80.ld_a_imm(flags);
    z80.out_a(0xBE);
}

pub fn nes_addr_to_sms_name_vram(nes_addr: u16) -> u16 {
    let offset = (nes_addr - 0x2000) & 0x03ff;
    0x3800 + offset * 2
}

fn emit_vdp_addr(z80: &mut Z80Program, command: u16) {
    z80.ld_a_imm((command & 0xFF) as u8);
    z80.out_a(0xBF);
    z80.ld_a_imm((command >> 8) as u8);
    z80.out_a(0xBF);
}

fn emit_copy_loop(z80: &mut Z80Program) {
    let loop_label = z80.fresh_label("copy_loop");
    z80.label(&loop_label);
    z80.ld_a_hl_ptr();
    z80.out_a(0xBE);
    z80.inc_hl();
    z80.dec_bc();
    z80.ld_a_b();
    z80.or_c();
    z80.jp_nz(&loop_label);
}

fn emit_nes_horizontal_text(z80: &mut Z80Program, nes_addr: u16, tiles: &[u8]) {
    let base = nes_addr_to_sms_name_vram(nes_addr);
    for (i, tile) in tiles.iter().copied().enumerate() {
        emit_sms_name_entry(z80, base + (i as u16 * 2), tile, 0x00);
    }
}
