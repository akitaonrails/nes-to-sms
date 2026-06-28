//! `trace-sms <rom.sms> [--steps N]` — run a built SMS ROM under
//! `z80_emu` and report execution path. Used to diagnose why a ROM
//! produces unexpected output (e.g. all-black screen).
//!
//! Implements minimal SMS hardware:
//! - Sega mapper: writes to $FFFC-$FFFF control banks for slot 0/1/2.
//! - I/O ports: $BE/$BF (VDP), $DC/$DD (controller) — VDP writes are
//!   logged, reads return sensible defaults so the CPU doesn't stall
//!   waiting forever (vblank flag toggles every "frame").
//! - Memory: 8 KB SMS RAM at $C000-$DFFF, mirrored at $E000-$FFFF.
//!   ROM banks live in `rom_banks[bank][offset]`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use z80_emu::{Bus, Cpu, StepError};

const BANK_SIZE: usize = 0x4000;
const RAM_SIZE: usize = 0x2000;
const IRQ_PERIOD: usize = 60_000;
const RT_PPU_WRITE_ADDR: u16 = 0x0068;

#[derive(Clone)]
struct SmsBus {
    rom: Vec<u8>,
    /// Current bank mapped into each slot. slot[0] = bank for $0000-$3FFF, etc.
    slot_bank: [u8; 3],
    ram: [u8; RAM_SIZE],
    /// Log of (frame, op, port, value).
    io_log: Vec<String>,
    /// VDP status reads. Real frame/line IRQ kind is supplied through
    /// `vdp_status_override` when the tracer injects an interrupt; fallback
    /// toggling keeps non-IRQ polling loops from stalling.
    vdp_status_reads: u32,
    /// Status byte returned by the next $BF read, used to distinguish injected
    /// frame IRQs (bit 7 set) from line IRQs (bit 7 clear).
    vdp_status_override: Option<u8>,
    /// VRAM 16 KiB and CRAM 32 B (for inspection if needed).
    vram: [u8; 0x4000],
    cram: [u8; 0x20],
    /// Last written VDP register values, for framebuffer interpretation.
    vdp_regs: [u8; 16],
    /// Approximate per-frame line-scroll split for checkpoint rendering:
    /// (screen line, top/pre reg8, top/pre reg9). After the injected line IRQ
    /// runs, the live VDP regs hold the bottom/post scroll values.
    render_scroll_split: Option<(usize, u8, u8)>,
    /// VDP address latch state (toggles between low/high byte).
    vdp_addr_high: u8,
    vdp_addr_low: u8,
    vdp_addr_latched: bool,
    /// VDP control / data port mode: 0 = next data write goes to VRAM, 1 = CRAM.
    vdp_code: u8,
    /// Total number of VRAM writes (diagnostic).
    vram_writes: u32,
    /// Total number of CRAM writes (diagnostic).
    cram_writes: u32,
    /// Total number of VDP data-port writes.
    vdp_data_writes: u32,
    /// Total number of VDP control-port writes.
    vdp_control_writes: u32,
    /// Total number of controller port reads.
    controller_reads: u32,
    /// Bitmask of NES nametable pages observed writing each folded SMS cell.
    nt_fold_cell_pages: [u8; 1024],
    /// Trace-only reconstruction of raw NES CIRAM writes observed at the
    /// `rt_ppu_write` call boundary, interpreted with vertical mirroring.
    nt_trace_ciram_vertical: [u8; 0x800],
    /// Same trace-only raw CIRAM reconstruction, interpreted with horizontal
    /// mirroring. Keeping both avoids baking SMB's mirroring mode into the
    /// diagnostic and lets future profiles compare the expected mode.
    nt_trace_ciram_horizontal: [u8; 0x800],
    /// Count of trace-observed PPUDATA writes into $2000-$2FFF.
    nt_trace_ciram_writes: u32,
    /// Count of trace-observed PPUDATA writes into tile bytes ($2000-$2FBF).
    nt_trace_ciram_tile_writes: u32,
    /// Count of trace-observed PPUDATA writes into attribute bytes.
    nt_trace_ciram_attr_writes: u32,
    /// Tile writes where the folded $CC00 subpalette disagrees with the compact
    /// attribute shadow, interpreted as horizontal NES mirroring.
    nt_explicit_s_mismatch_horizontal: u32,
    nt_explicit_s_mismatch_horizontal_examples: Vec<NtExplicitSExample>,
    /// Same diagnostic, interpreted as vertical NES mirroring.
    nt_explicit_s_mismatch_vertical: u32,
    nt_explicit_s_mismatch_vertical_examples: Vec<NtExplicitSExample>,
    /// Raw SMS port $DC value for controller 1. Active-low; default $FF = released.
    controller_port_dc: u8,
    /// Log of every mapper write (port, value). Lets the trace report
    /// when a translated routine surprises us by re-banking a slot.
    bank_writes: Vec<(u16, u8)>,
    /// Per-address write tap. If `watch_addr` is set, every write to it
    /// pushes (step, value) into `watch_log`. Use to confirm whether a
    /// specific RAM byte ever gets touched.
    watch_addr: Option<u16>,
    watch_write_range: Option<(u16, u16)>,
    watch_log: Vec<WatchWrite>,
    /// Per-address read tap. If `watch_read_addr` is set, every RAM read from
    /// it pushes (step, value) into `watch_read_log`. This is useful for
    /// collision paths that indirect through block buffers.
    watch_read_addr: Option<u16>,
    watch_read_range: Option<(u16, u16)>,
    watch_read_log: Vec<WatchWrite>,
    watch_step: usize,
    watch_pc: u16,
    watch_bank1: u8,
    watch_sp: u16,
    watch_ret: u16,
}

#[derive(Clone, Copy, Debug)]
struct WatchWrite {
    step: usize,
    addr: u16,
    pc: u16,
    bank1: u8,
    sp: u16,
    ret: u16,
    value: u8,
    ppage: u8,
    px: u8,
    ypage: u8,
    py: u8,
    player_state: u8,
    x_shadow: u8,
    y_shadow: u8,
    zp02: u8,
    zp03: u8,
    zp04: u8,
    zp05: u8,
    zp06: u8,
    zp07: u8,
    zp08: u8,
    yspeed: u8,
    eb: u8,
    vertical_force: u8,
}

#[derive(Clone, Debug)]
struct NtExplicitSExample {
    ppu_addr: u16,
    sms_addr: u16,
    folded_s: u8,
    explicit_s: u8,
    attr_index: usize,
    attr_byte: u8,
}

#[derive(Clone, Copy, Debug)]
struct WatchExecHit {
    step: usize,
    pc: u16,
    bank1: u8,
    sp: u16,
    ret: u16,
    op: u8,
    a: u8,
    f: u8,
    p_shadow: u8,
    x_shadow: u8,
    y_shadow: u8,
    zp00: u8,
    zp02: u8,
    zp03: u8,
    zp04: u8,
    zp05: u8,
    zp06: u8,
    zp07: u8,
    zp08: u8,
    ppage: u8,
    px: u8,
    ypage: u8,
    py: u8,
    yspeed: u8,
    eb: u8,
    vertical_force: u8,
    area_obj_dispatch: u8,
}

#[derive(Clone)]
struct FallSnapshot {
    step: usize,
    frame: usize,
    ram: [u8; RAM_SIZE],
    recent_reads: Vec<WatchWrite>,
    recent_writes: Vec<WatchWrite>,
}

impl SmsBus {
    fn new(rom: Vec<u8>, controller_port_dc: u8) -> Self {
        Self {
            rom,
            slot_bank: [0, 1, 2],
            ram: [0; RAM_SIZE],
            io_log: Vec::new(),
            vdp_status_reads: 0,
            vdp_status_override: None,
            vram: [0; 0x4000],
            cram: [0; 0x20],
            vdp_regs: [0; 16],
            render_scroll_split: None,
            vdp_addr_high: 0,
            vdp_addr_low: 0,
            bank_writes: Vec::new(),
            watch_addr: std::env::var("SMS_WATCH_ADDR")
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            watch_write_range: std::env::var("SMS_WATCH_WRITE_RANGE")
                .ok()
                .and_then(|s| parse_addr_range(&s)),
            watch_log: Vec::new(),
            watch_read_addr: std::env::var("SMS_WATCH_READ_ADDR")
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            watch_read_range: std::env::var("SMS_WATCH_READ_RANGE")
                .ok()
                .and_then(|s| parse_addr_range(&s)),
            watch_read_log: Vec::new(),
            watch_step: 0,
            watch_pc: 0,
            watch_bank1: 1,
            watch_sp: 0,
            watch_ret: 0,
            vdp_addr_latched: false,
            vdp_code: 0,
            vram_writes: 0,
            cram_writes: 0,
            vdp_data_writes: 0,
            vdp_control_writes: 0,
            controller_reads: 0,
            nt_fold_cell_pages: [0; 1024],
            nt_trace_ciram_vertical: [0; 0x800],
            nt_trace_ciram_horizontal: [0; 0x800],
            nt_trace_ciram_writes: 0,
            nt_trace_ciram_tile_writes: 0,
            nt_trace_ciram_attr_writes: 0,
            nt_explicit_s_mismatch_horizontal: 0,
            nt_explicit_s_mismatch_horizontal_examples: Vec::new(),
            nt_explicit_s_mismatch_vertical: 0,
            nt_explicit_s_mismatch_vertical_examples: Vec::new(),
            controller_port_dc,
        }
    }
    fn rom_byte(&self, bank: u8, offset: u16) -> u8 {
        let i = bank as usize * BANK_SIZE + offset as usize;
        *self.rom.get(i).unwrap_or(&0xFF)
    }

    fn watches_read(&self, addr: u16) -> bool {
        Some(addr) == self.watch_read_addr
            || self
                .watch_read_range
                .is_some_and(|(start, end)| (start..=end).contains(&addr))
    }

    fn watches_write(&self, addr: u16) -> bool {
        Some(addr) == self.watch_addr
            || self
                .watch_write_range
                .is_some_and(|(start, end)| (start..=end).contains(&addr))
    }

    fn watch_entry(&self, addr: u16, value: u8) -> WatchWrite {
        WatchWrite {
            step: self.watch_step,
            addr,
            pc: self.watch_pc,
            bank1: self.watch_bank1,
            sp: self.watch_sp,
            ret: self.watch_ret,
            value,
            ppage: self.ram[0x006D],
            px: self.ram[0x0086],
            ypage: self.ram[0x00B5],
            py: self.ram[0x00CE],
            player_state: self.ram[0x000E],
            x_shadow: self.ram[0x0B00],
            y_shadow: self.ram[0x0B01],
            zp02: self.ram[0x0002],
            zp03: self.ram[0x0003],
            zp04: self.ram[0x0004],
            zp05: self.ram[0x0005],
            zp06: self.ram[0x0006],
            zp07: self.ram[0x0007],
            zp08: self.ram[0x0008],
            yspeed: self.ram[0x009F],
            eb: self.ram[0x00EB],
            vertical_force: self.ram[0x070E],
        }
    }

    fn record_nt_fold_write(&mut self, vram_addr: u16) {
        let masked = vram_addr & 0x3FFF;
        if !(0x3700..=0x3EFF).contains(&masked) {
            return;
        }

        let ppu_addr = ((self.ram[0x0B0F] as u16) << 8) | self.ram[0x0B10] as u16;
        if !(0x2000..=0x2FBF).contains(&ppu_addr) || (ppu_addr & 0x03FF) >= 0x03C0 {
            return;
        }

        let cell = ((masked - 0x3700) / 2) as usize;
        if cell < self.nt_fold_cell_pages.len() {
            let page = ((ppu_addr - 0x2000) >> 10) as u8;
            self.nt_fold_cell_pages[cell] |= 1 << page;
        }

        if masked & 1 == 0 {
            self.record_nt_explicit_s_mismatch(masked, ppu_addr, false);
            self.record_nt_explicit_s_mismatch(masked, ppu_addr, true);
        }
    }

    fn record_trace_ppu_write_call(&mut self, reg: u8, value: u8) {
        if reg != 7 {
            return;
        }

        let ppu_addr = ((self.ram[0x0B0F] as u16) << 8) | self.ram[0x0B10] as u16;
        if !(0x2000..=0x2FFF).contains(&ppu_addr) {
            return;
        }

        let vertical = nt_ciram_index(ppu_addr, true);
        let horizontal = nt_ciram_index(ppu_addr, false);
        self.nt_trace_ciram_vertical[vertical] = value;
        self.nt_trace_ciram_horizontal[horizontal] = value;
        self.nt_trace_ciram_writes += 1;
        if (ppu_addr & 0x03FF) >= 0x03C0 {
            self.nt_trace_ciram_attr_writes += 1;
        } else {
            self.nt_trace_ciram_tile_writes += 1;
        }
    }

    fn record_nt_explicit_s_mismatch(
        &mut self,
        sms_addr: u16,
        ppu_addr: u16,
        vertical_mirroring: bool,
    ) {
        let Some(folded_s) = self.nt_folded_shadow_s(sms_addr) else {
            return;
        };
        let (explicit_s, attr_index, attr_byte) =
            self.nt_attr_shadow_s(ppu_addr, vertical_mirroring);
        if folded_s == explicit_s {
            return;
        }

        let example = NtExplicitSExample {
            ppu_addr,
            sms_addr,
            folded_s,
            explicit_s,
            attr_index,
            attr_byte,
        };

        let (count, examples) = if vertical_mirroring {
            (
                &mut self.nt_explicit_s_mismatch_vertical,
                &mut self.nt_explicit_s_mismatch_vertical_examples,
            )
        } else {
            (
                &mut self.nt_explicit_s_mismatch_horizontal,
                &mut self.nt_explicit_s_mismatch_horizontal_examples,
            )
        };
        *count += 1;
        if examples.len() < 6 {
            examples.push(example);
        }
    }

    fn nt_folded_shadow_s(&self, sms_addr: u16) -> Option<u8> {
        let masked = sms_addr & 0x3FFF;
        if !(0x3700..=0x3EFF).contains(&masked) {
            return None;
        }

        // Runtime _bgv_sub_palette maps the SMS high-byte nametable address to
        // folded shadow storage by adding $9500: $3701 -> $CC01.
        let shadow_addr = (masked | 1).wrapping_add(0x9500);
        (0xC000..=0xDFFF)
            .contains(&shadow_addr)
            .then(|| self.ram[(shadow_addr - 0xC000) as usize] & 0x03)
    }

    fn nt_attr_shadow_s(&self, ppu_addr: u16, vertical_mirroring: bool) -> (u8, usize, u8) {
        let ciram = nt_ciram_index(ppu_addr, vertical_mirroring) as u16;
        let ciram_page = (ciram >> 10) as usize;
        let tile_offset = (ciram & 0x03FF) as usize;
        let coarse_y = tile_offset / 32;
        let coarse_x = tile_offset % 32;
        let attr_index = ciram_page * 64 + (coarse_y / 4) * 8 + coarse_x / 4;
        let attr_byte = self.ram[0x0B80 + attr_index];
        let shift = ((coarse_y & 0x02) << 1) | (coarse_x & 0x02);
        ((attr_byte >> shift) & 0x03, attr_index, attr_byte)
    }
}

fn nt_ciram_index(ppu_addr: u16, vertical_mirroring: bool) -> usize {
    let raw = ppu_addr.wrapping_sub(0x2000) & 0x0FFF;
    if vertical_mirroring {
        (raw & 0x07FF) as usize
    } else {
        ((raw & 0x03FF) | ((raw & 0x0800) >> 1)) as usize
    }
}

fn parse_hex_addr(s: &str) -> Option<u16> {
    u16::from_str_radix(
        s.trim().trim_start_matches("0x").trim_start_matches('$'),
        16,
    )
    .ok()
}

fn parse_addr_range(s: &str) -> Option<(u16, u16)> {
    let (start, end) = s.split_once('-')?;
    Some((parse_hex_addr(start)?, parse_hex_addr(end)?))
}

impl Bus for SmsBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.rom_byte(0, addr), // first 1 KB always bank 0
            0x0400..=0x3FFF => self.rom_byte(self.slot_bank[0], addr),
            0x4000..=0x7FFF => self.rom_byte(self.slot_bank[1], addr - 0x4000),
            0x8000..=0xBFFF => self.rom_byte(self.slot_bank[2], addr - 0x8000),
            0xC000..=0xDFFF => {
                let value = self.ram[(addr - 0xC000) as usize];
                if self.watches_read(addr) {
                    self.watch_read_log.push(self.watch_entry(addr, value));
                }
                value
            }
            0xE000..=0xFFFB => {
                let value = self.ram[(addr - 0xE000) as usize];
                if self.watches_read(addr) {
                    self.watch_read_log.push(self.watch_entry(addr, value));
                }
                value
            } // mirror
            0xFFFC => 0x00, // bank-mapping control, returns 0
            0xFFFD => self.slot_bank[0],
            0xFFFE => self.slot_bank[1],
            0xFFFF => self.slot_bank[2],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0xBFFF => {
                // Writes to ROM area are ignored (real SMS hardware).
            }
            0xC000..=0xDFFF => {
                self.ram[(addr - 0xC000) as usize] = value;
                if self.watches_write(addr) {
                    self.watch_log.push(self.watch_entry(addr, value));
                }
            }
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize] = value,
            0xFFFC => {
                self.io_log.push(format!("mapper ctrl=${value:02X}"));
                self.bank_writes.push((0xFFFC, value));
            }
            0xFFFD => {
                self.slot_bank[0] = value;
                self.bank_writes.push((0xFFFD, value));
            }
            0xFFFE => {
                self.slot_bank[1] = value;
                self.bank_writes.push((0xFFFE, value));
            }
            0xFFFF => {
                self.slot_bank[2] = value;
                self.bank_writes.push((0xFFFF, value));
            }
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port & 0xC1 {
            // VDP data port $BE — read VRAM through latch (not implemented).
            0x80 => 0x00,
            // VDP status / control port $BF — return VBlank flag bit toggling.
            0x81 => {
                self.vdp_status_reads += 1;
                if let Some(status) = self.vdp_status_override.take() {
                    self.vdp_addr_latched = false;
                    return status;
                }
                // Make bit 7 toggle every 1000 reads so polling loops advance.
                let vblank = if (self.vdp_status_reads / 100) & 1 == 0 {
                    0x80
                } else {
                    0x00
                };
                // Reading also resets the VDP address latch toggle.
                self.vdp_addr_latched = false;
                vblank
            }
            // V counter $7E / H counter $7F.
            0x40 => 0xFF,
            // I/O port $DC/$DD (controllers) — all buttons released.
            0xC0 => {
                self.controller_reads += 1;
                self.controller_port_dc
            }
            _ => 0xFF,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        match port & 0xC1 {
            // VDP data port $BE. SMS VDP codes after address-set:
            //   0=VRAM read, 1=VRAM write, 2=register write, 3=CRAM write.
            0x80 => {
                self.vdp_data_writes += 1;
                let addr = ((self.vdp_addr_high as u16) << 8) | self.vdp_addr_low as u16;
                match self.vdp_code {
                    0 | 1 => {
                        // VRAM write (code 0 is read-mode but real HW writes
                        // anyway in some cases; SMB doesn't rely on this).
                        let masked = (addr & 0x3FFF) as usize;
                        self.vram[masked] = value;
                        self.vram_writes += 1;
                        self.record_nt_fold_write(addr);
                    }
                    3 => {
                        let masked = (addr & 0x1F) as usize;
                        self.cram[masked] = value;
                        self.cram_writes += 1;
                    }
                    _ => {}
                }
                let new = addr.wrapping_add(1);
                self.vdp_addr_high = (new >> 8) as u8;
                self.vdp_addr_low = (new & 0xFF) as u8;
            }
            // VDP control port $BF — address/register write protocol.
            0x81 => {
                self.vdp_control_writes += 1;
                if !self.vdp_addr_latched {
                    self.vdp_addr_low = value;
                    self.vdp_addr_latched = true;
                } else {
                    self.vdp_addr_high = value & 0x3F;
                    self.vdp_code = (value >> 6) & 3;
                    if self.vdp_code == 2 {
                        // VDP register write: low nibble of high byte selects register.
                        let reg = value & 0x0F;
                        let val = self.vdp_addr_low;
                        self.vdp_regs[reg as usize] = val;
                        self.io_log.push(format!("vdp r{reg} = ${val:02X}"));
                    }
                    self.vdp_addr_latched = false;
                }
            }
            // Other I/O — ignore.
            _ => {}
        }
    }
}

fn parse_hex_u8(s: &str) -> Result<u8, String> {
    let trimmed = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u8::from_str_radix(trimmed, 16).map_err(|_| format!("invalid hex byte: {s}"))
}

fn buttons_to_sms_port_dc(spec: &str) -> u8 {
    let mut port = 0xFFu8;
    for raw in spec.split(',') {
        let button = raw.trim().to_ascii_lowercase();
        if button.is_empty() {
            continue;
        }
        let bit = match button.as_str() {
            "up" => 0,
            "down" => 1,
            "left" => 2,
            "right" => 3,
            "a" | "b1" | "button1" | "select" => 4,
            "b" | "b2" | "button2" | "start" => 5,
            other => panic!("unknown --buttons entry: {other}"),
        };
        port &= !(1 << bit);
    }
    port
}

fn parse_button_event(spec: &str) -> Result<(usize, u8), String> {
    let (frame, buttons) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:buttons for --buttons-at-frame, got {spec}"))?;
    let frame = frame
        .parse::<usize>()
        .map_err(|_| format!("invalid frame in --buttons-at-frame: {frame}"))?;
    Ok((frame, buttons_to_sms_port_dc(buttons)))
}

#[derive(Debug, Clone, Copy)]
struct RamExpectation {
    addr: u16,
    value: u8,
}

fn parse_ram_expectation(spec: &str) -> Result<RamExpectation, String> {
    let (addr, value) = spec
        .split_once('=')
        .or_else(|| spec.split_once(':'))
        .ok_or_else(|| format!("expected ADDR=HEX for --expect-ram, got {spec}"))?;
    let addr = parse_hex_addr(addr).ok_or_else(|| format!("invalid RAM address: {addr}"))?;
    let value = parse_hex_u8(value)?;
    Ok(RamExpectation { addr, value })
}

fn ram_index(addr: u16) -> Option<usize> {
    let idx = match addr {
        0x0000..=0x1FFF => addr,
        0xC000..=0xDFFF => addr - 0xC000,
        0xE000..=0xFFFF => addr - 0xE000,
        _ => return None,
    };
    Some(usize::from(idx))
}

fn load_button_script(path: &str) -> Result<Vec<(usize, u8)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read button script {path}: {err}"))?;
    let mut events = Vec::new();
    for (line_idx, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        events.push(
            parse_button_event(line)
                .map_err(|err| format!("invalid button script {path}:{}: {err}", line_idx + 1))?,
        );
    }
    Ok(events)
}

#[derive(Debug, Clone)]
struct RouteCheckpoint {
    frame: usize,
    name: String,
}

fn parse_checkpoint_spec(spec: &str) -> Result<RouteCheckpoint, String> {
    let (frame, name) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:name for --checkpoint, got {spec}"))?;
    let frame = frame
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid checkpoint frame: {frame}"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err("checkpoint name must not be empty".to_string());
    }
    Ok(RouteCheckpoint {
        frame,
        name: name.to_string(),
    })
}

fn load_checkpoint_script(path: &str) -> Result<Vec<RouteCheckpoint>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read checkpoint script {path}: {err}"))?;
    let mut checkpoints = Vec::new();
    for (line_idx, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        checkpoints.push(
            parse_checkpoint_spec(line).map_err(|err| {
                format!("invalid checkpoint script {path}:{}: {err}", line_idx + 1)
            })?,
        );
    }
    Ok(checkpoints)
}

fn checkpoint_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "checkpoint".to_string()
    } else {
        trimmed
    }
}

#[derive(Clone)]
struct SearchState {
    cpu: Cpu,
    bus: SmsBus,
    step: usize,
    next_irq_at: usize,
    irqs_fired: usize,
    next_button_event: usize,
}

#[derive(Debug)]
struct SearchResult {
    label: String,
    start: usize,
    duration: usize,
    max_aofs: u8,
    max_ppage: u8,
    max_px: u8,
    max_y_page: u8,
    final_state: u8,
    stop_flag: u8,
    first_stop_frame: Option<usize>,
}

#[derive(Debug)]
struct EndRouteResult {
    label: String,
    max_aofs: u8,
    max_ppage: u8,
    max_px: u8,
    min_y: u8,
    final_ppage: u8,
    final_px: u8,
    final_y: u8,
    final_state: u8,
    stop_flag: u8,
    flag_reads: usize,
    first_victory_frame: Option<usize>,
}

fn run_search_steps(
    state: &mut SearchState,
    max_steps: usize,
    button_events: &[(usize, u8)],
    target_frame: usize,
) -> Result<(), StepError> {
    for _ in 0..max_steps {
        if state.irqs_fired >= target_frame {
            break;
        }

        let pc = state.cpu.pc;
        state.bus.watch_step = state.step;
        state.bus.watch_pc = pc;
        state.bus.watch_bank1 = state.bus.slot_bank[1];
        state.bus.watch_sp = state.cpu.sp;

        if state.step >= state.next_irq_at && state.cpu.iff1 {
            while state.next_button_event < button_events.len()
                && state.irqs_fired >= button_events[state.next_button_event].0
            {
                state.bus.controller_port_dc = button_events[state.next_button_event].1;
                state.next_button_event += 1;
            }
            state.cpu.sp = state.cpu.sp.wrapping_sub(2);
            state.bus.write(state.cpu.sp, (state.cpu.pc & 0xFF) as u8);
            state
                .bus
                .write(state.cpu.sp.wrapping_add(1), (state.cpu.pc >> 8) as u8);
            state.bus.vdp_status_override = Some(0x80);
            state.cpu.pc = 0x0038;
            state.cpu.iff1 = false;
            state.cpu.iff2 = false;
            state.cpu.halted = false;
            state.irqs_fired += 1;
            state.next_irq_at = state.next_irq_at.saturating_add(IRQ_PERIOD);
        }

        if state.cpu.halted {
            break;
        }
        state.cpu.step(&mut state.bus)?;
        state.step += 1;
    }
    Ok(())
}

fn run_late_route_search(rom_path: &PathBuf, base_events: &[(usize, u8)]) {
    let rom = std::fs::read(rom_path).expect("read rom");
    let mut state = SearchState {
        cpu: Cpu::new(),
        bus: SmsBus::new(rom, 0xFF),
        step: 0,
        next_irq_at: IRQ_PERIOD,
        irqs_fired: 0,
        next_button_event: 0,
    };
    state.cpu.pc = 0x0000;
    state.cpu.sp = 0xDFF0;

    let snapshot_frame = 2050usize;
    run_search_steps(&mut state, 170_000_000, base_events, snapshot_frame)
        .expect("run to late-route snapshot");
    state.bus.io_log.clear();
    state.bus.bank_writes.clear();
    state.bus.watch_log.clear();
    state.bus.watch_read_log.clear();

    println!(
        "late-route snapshot frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} aofs={:02X} stop={:02X}",
        state.irqs_fired,
        state.bus.ram[0x006D],
        state.bus.ram[0x0086],
        state.bus.ram[0x00B5],
        state.bus.ram[0x00CE],
        state.bus.ram[0x071A],
        state.bus.ram[0x071C],
        state.bus.ram[0x072C],
        state.bus.ram[0x0723],
    );

    let mut results = Vec::new();
    for start in (2040usize..=2260).step_by(10) {
        for duration in [20usize, 35, 50, 70, 90] {
            let mut events = base_events.to_vec();
            events.push((start, buttons_to_sms_port_dc("right,a")));
            events.push((start + duration, buttons_to_sms_port_dc("right")));
            events.sort_by_key(|(frame, _)| *frame);

            let mut branch = state.clone();
            branch.next_button_event =
                events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
            let mut max_aofs = branch.bus.ram[0x072C];
            let mut max_ppage = branch.bus.ram[0x006D];
            let mut max_px = branch.bus.ram[0x0086];
            let mut max_y_page = branch.bus.ram[0x00B5];
            let mut first_stop_frame = None;

            while branch.irqs_fired < 3400 {
                let before = branch.irqs_fired;
                if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                    eprintln!("branch start={start} duration={duration} stopped: {err:?}");
                    break;
                }
                max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                let ppage = branch.bus.ram[0x006D];
                let px = branch.bus.ram[0x0086];
                if (ppage, px) > (max_ppage, max_px) {
                    max_ppage = ppage;
                    max_px = px;
                }
                max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
                if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                    first_stop_frame = Some(branch.irqs_fired);
                }
                if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                    break;
                }
                if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                    break;
                }
            }

            results.push(SearchResult {
                label: format!("one:{start}+{duration}"),
                start,
                duration,
                max_aofs,
                max_ppage,
                max_px,
                max_y_page,
                final_state: branch.bus.ram[0x000E],
                stop_flag: branch.bus.ram[0x0723],
                first_stop_frame,
            });
        }
    }

    let route_candidates = [
        (2040, 30, 2100, 30),
        (2040, 50, 2120, 40),
        (2040, 70, 2140, 50),
        (2060, 40, 2140, 40),
        (2060, 70, 2180, 40),
        (2060, 70, 2180, 60),
        (2060, 70, 2200, 50),
        (2080, 50, 2160, 50),
        (2080, 70, 2200, 50),
        (2100, 50, 2180, 60),
        (2100, 70, 2220, 50),
        (2120, 50, 2200, 60),
        (2140, 50, 2220, 60),
        (2060, 70, 2180, 60),
        (2060, 70, 2180, 60),
    ];
    for (first_start, first_duration, second_start, second_duration) in route_candidates {
        let mut events = base_events.to_vec();
        events.push((first_start, buttons_to_sms_port_dc("right,a")));
        events.push((
            first_start + first_duration,
            buttons_to_sms_port_dc("right"),
        ));
        events.push((second_start, buttons_to_sms_port_dc("right,a")));
        events.push((
            second_start + second_duration,
            buttons_to_sms_port_dc("right"),
        ));
        events.sort_by_key(|(frame, _)| *frame);

        let mut branch = state.clone();
        branch.next_button_event = events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
        let mut max_aofs = branch.bus.ram[0x072C];
        let mut max_ppage = branch.bus.ram[0x006D];
        let mut max_px = branch.bus.ram[0x0086];
        let mut max_y_page = branch.bus.ram[0x00B5];
        let mut first_stop_frame = None;
        while branch.irqs_fired < 3400 {
            let before = branch.irqs_fired;
            if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                eprintln!("branch route stopped: {err:?}");
                break;
            }
            max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
            let ppage = branch.bus.ram[0x006D];
            let px = branch.bus.ram[0x0086];
            if (ppage, px) > (max_ppage, max_px) {
                max_ppage = ppage;
                max_px = px;
            }
            max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
            if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                first_stop_frame = Some(branch.irqs_fired);
            }
            if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                break;
            }
            if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                break;
            }
        }

        results.push(SearchResult {
            label: format!("two:{first_start}+{first_duration},{second_start}+{second_duration}"),
            start: first_start,
            duration: first_duration,
            max_aofs,
            max_ppage,
            max_px,
            max_y_page,
            final_state: branch.bus.ram[0x000E],
            stop_flag: branch.bus.ram[0x0723],
            first_stop_frame,
        });
    }

    let triple_candidates = [
        (2060, 70, 2180, 40, 2260, 40),
        (2060, 70, 2180, 40, 2280, 50),
        (2060, 70, 2180, 60, 2260, 40),
        (2060, 70, 2180, 60, 2280, 60),
        (2080, 70, 2200, 50, 2280, 50),
        (2080, 70, 2200, 50, 2300, 60),
        (2100, 70, 2220, 50, 2280, 50),
        (2100, 70, 2220, 50, 2300, 60),
        (2100, 90, 2220, 60, 2300, 60),
        (2120, 70, 2220, 60, 2300, 70),
        (2040, 70, 2140, 50, 2220, 60),
        (2040, 90, 2160, 60, 2240, 60),
    ];
    for (s1, d1, s2, d2, s3, d3) in triple_candidates {
        let mut events = base_events.to_vec();
        for (start, duration) in [(s1, d1), (s2, d2), (s3, d3)] {
            events.push((start, buttons_to_sms_port_dc("right,a")));
            events.push((start + duration, buttons_to_sms_port_dc("right")));
        }
        events.sort_by_key(|(frame, _)| *frame);

        let mut branch = state.clone();
        branch.next_button_event = events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
        let mut max_aofs = branch.bus.ram[0x072C];
        let mut max_ppage = branch.bus.ram[0x006D];
        let mut max_px = branch.bus.ram[0x0086];
        let mut max_y_page = branch.bus.ram[0x00B5];
        let mut first_stop_frame = None;
        while branch.irqs_fired < 3400 {
            let before = branch.irqs_fired;
            if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                eprintln!("branch triple stopped: {err:?}");
                break;
            }
            max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
            let ppage = branch.bus.ram[0x006D];
            let px = branch.bus.ram[0x0086];
            if (ppage, px) > (max_ppage, max_px) {
                max_ppage = ppage;
                max_px = px;
            }
            max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
            if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                first_stop_frame = Some(branch.irqs_fired);
            }
            if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                break;
            }
            if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                break;
            }
        }
        results.push(SearchResult {
            label: format!("three:{s1}+{d1},{s2}+{d2},{s3}+{d3}"),
            start: s1,
            duration: d1,
            max_aofs,
            max_ppage,
            max_px,
            max_y_page,
            final_state: branch.bus.ram[0x000E],
            stop_flag: branch.bus.ram[0x0723],
            first_stop_frame,
        });
    }

    for fourth_start in (2360usize..=2520).step_by(10) {
        for fourth_duration in [20usize, 35, 50, 70, 90] {
            let mut events = base_events.to_vec();
            for (start, duration) in [
                (2100usize, 70usize),
                (2220usize, 50usize),
                (2280usize, 50usize),
                (fourth_start, fourth_duration),
            ] {
                events.push((start, buttons_to_sms_port_dc("right,a")));
                events.push((start + duration, buttons_to_sms_port_dc("right")));
            }
            events.sort_by_key(|(frame, _)| *frame);

            let mut branch = state.clone();
            branch.next_button_event =
                events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
            let mut max_aofs = branch.bus.ram[0x072C];
            let mut max_ppage = branch.bus.ram[0x006D];
            let mut max_px = branch.bus.ram[0x0086];
            let mut max_y_page = branch.bus.ram[0x00B5];
            let mut first_stop_frame = None;
            while branch.irqs_fired < 3400 {
                let before = branch.irqs_fired;
                if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                    eprintln!("branch fourth stopped: {err:?}");
                    break;
                }
                max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                let ppage = branch.bus.ram[0x006D];
                let px = branch.bus.ram[0x0086];
                if (ppage, px) > (max_ppage, max_px) {
                    max_ppage = ppage;
                    max_px = px;
                }
                max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
                if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                    first_stop_frame = Some(branch.irqs_fired);
                }
                if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                    break;
                }
                if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                    break;
                }
            }
            results.push(SearchResult {
                label: format!("four:2100+70,2220+50,2280+50,{fourth_start}+{fourth_duration}"),
                start: fourth_start,
                duration: fourth_duration,
                max_aofs,
                max_ppage,
                max_px,
                max_y_page,
                final_state: branch.bus.ram[0x000E],
                stop_flag: branch.bus.ram[0x0723],
                first_stop_frame,
            });
        }
    }

    results.sort_by_key(|r| {
        (
            std::cmp::Reverse(r.max_aofs),
            std::cmp::Reverse(r.max_ppage),
            std::cmp::Reverse(r.max_px),
            r.max_y_page,
            r.first_stop_frame.unwrap_or(usize::MAX),
        )
    });
    println!("top late-route probes:");
    for result in results.iter().take(25) {
        println!(
            "  {} start={} dur={} max_aofs={:02X} max_ppos={:02X}:{:02X} max_ypage={:02X} final_state={:02X} stop={} first_stop={:?}",
            result.label,
            result.start,
            result.duration,
            result.max_aofs,
            result.max_ppage,
            result.max_px,
            result.max_y_page,
            result.final_state,
            result.stop_flag,
            result.first_stop_frame,
        );
    }
}

fn run_end_route_search(rom_path: &PathBuf, base_events: &[(usize, u8)]) {
    let rom = std::fs::read(rom_path).expect("read rom");
    let mut state = SearchState {
        cpu: Cpu::new(),
        bus: SmsBus::new(rom, 0xFF),
        step: 0,
        next_irq_at: IRQ_PERIOD,
        irqs_fired: 0,
        next_button_event: 0,
    };
    state.cpu.pc = 0x0000;
    state.cpu.sp = 0xDFF0;

    let snapshot_frame = 2600usize;
    run_search_steps(&mut state, 190_000_000, base_events, snapshot_frame)
        .expect("run to end-route snapshot");
    state.bus.io_log.clear();
    state.bus.bank_writes.clear();
    state.bus.watch_log.clear();
    state.bus.watch_read_log.clear();

    println!(
        "end-route snapshot frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} aofs={:02X} state={:02X} stop={:02X}",
        state.irqs_fired,
        state.bus.ram[0x006D],
        state.bus.ram[0x0086],
        state.bus.ram[0x00B5],
        state.bus.ram[0x00CE],
        state.bus.ram[0x071A],
        state.bus.ram[0x071C],
        state.bus.ram[0x072C],
        state.bus.ram[0x000E],
        state.bus.ram[0x0723],
    );

    let mut results = Vec::new();
    for jumps in [4usize, 5, 6] {
        for first_start in (2610usize..=2670).step_by(10) {
            for period in [55usize, 65, 75, 85] {
                for duration in [25usize, 35, 45, 55, 65] {
                    let mut events = base_events.to_vec();
                    for n in 0..jumps {
                        let start = first_start + n * period;
                        events.push((start, buttons_to_sms_port_dc("right,a")));
                        events.push((start + duration, buttons_to_sms_port_dc("right")));
                    }
                    events.sort_by_key(|(frame, _)| *frame);

                    let mut branch = state.clone();
                    branch.next_button_event =
                        events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
                    branch.bus.watch_read_range = Some((0xC500, 0xC69F));
                    branch.bus.watch_read_log.clear();

                    let mut max_aofs = branch.bus.ram[0x072C];
                    let mut max_ppage = branch.bus.ram[0x006D];
                    let mut max_px = branch.bus.ram[0x0086];
                    let mut min_y = branch.bus.ram[0x00CE];
                    let mut first_victory_frame = None;

                    while branch.irqs_fired < 3400 {
                        let before = branch.irqs_fired;
                        if let Err(err) =
                            run_search_steps(&mut branch, 500_000, &events, before + 1)
                        {
                            eprintln!(
                                "end branch jumps={jumps} first={first_start} period={period} duration={duration} stopped: {err:?}"
                            );
                            break;
                        }
                        max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                        let ppage = branch.bus.ram[0x006D];
                        let px = branch.bus.ram[0x0086];
                        if (ppage, px) > (max_ppage, max_px) {
                            max_ppage = ppage;
                            max_px = px;
                        }
                        min_y = min_y.min(branch.bus.ram[0x00CE]);
                        if branch.bus.ram[0x000E] == 0x04 {
                            first_victory_frame = Some(branch.irqs_fired);
                            break;
                        }
                        if branch.bus.ram[0x0723] != 0 && branch.bus.ram[0x000E] != 0x04 {
                            break;
                        }
                    }

                    let flag_reads = branch
                        .bus
                        .watch_read_log
                        .iter()
                        .filter(|read| matches!(read.value, 0x24 | 0x25))
                        .count();
                    results.push(EndRouteResult {
                        label: format!(
                            "jumps={jumps} first={first_start} period={period} duration={duration}"
                        ),
                        max_aofs,
                        max_ppage,
                        max_px,
                        min_y,
                        final_ppage: branch.bus.ram[0x006D],
                        final_px: branch.bus.ram[0x0086],
                        final_y: branch.bus.ram[0x00CE],
                        final_state: branch.bus.ram[0x000E],
                        stop_flag: branch.bus.ram[0x0723],
                        flag_reads,
                        first_victory_frame,
                    });
                }
            }
        }
    }

    results.sort_by_key(|r| {
        (
            r.first_victory_frame.is_none(),
            std::cmp::Reverse(r.flag_reads),
            std::cmp::Reverse(r.max_aofs),
            std::cmp::Reverse(r.max_ppage),
            std::cmp::Reverse(r.max_px),
            r.final_y,
        )
    });
    println!("top end-route probes:");
    for result in results.iter().take(40) {
        println!(
            "  {} max_aofs={:02X} max_ppos={:02X}:{:02X} min_y={:02X} final={:02X}:{:02X} y={:02X} state={:02X} stop={} flag_reads={} victory={:?}",
            result.label,
            result.max_aofs,
            result.max_ppage,
            result.max_px,
            result.min_y,
            result.final_ppage,
            result.final_px,
            result.final_y,
            result.final_state,
            result.stop_flag,
            result.flag_reads,
            result.first_victory_frame,
        );
    }

    let mut follow_events = base_events.to_vec();
    let follow_first = 2620usize;
    let follow_period = 85usize;
    let follow_duration = 35usize;
    for n in 0..5 {
        let start = follow_first + n * follow_period;
        follow_events.push((start, buttons_to_sms_port_dc("right,a")));
        follow_events.push((start + follow_duration, buttons_to_sms_port_dc("right")));
    }
    follow_events.sort_by_key(|(frame, _)| *frame);
    let mut follow = state.clone();
    follow.next_button_event =
        follow_events.partition_point(|(frame, _)| *frame <= follow.irqs_fired);
    run_search_steps(&mut follow, 260_000_000, &follow_events, 6666)
        .expect("continue winning end-route candidate");
    println!(
        "continued winner frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} state={:02X} star_flag_task={:02X} collision_bits={:02X} enemy_flag={:02X} scroll_lock={:02X} flag_score={:02X} flag_y={:02X} area={:02X} level={:02X} fetch_timer={:02X} end_y={:02X} slide_timer={:02X}",
        follow.irqs_fired,
        follow.bus.ram[0x006D],
        follow.bus.ram[0x0086],
        follow.bus.ram[0x00B5],
        follow.bus.ram[0x00CE],
        follow.bus.ram[0x071A],
        follow.bus.ram[0x071C],
        follow.bus.ram[0x000E],
        follow.bus.ram[0x0746],
        follow.bus.ram[0x0490],
        follow.bus.ram[0x001B],
        follow.bus.ram[0x0723],
        follow.bus.ram[0x010F],
        follow.bus.ram[0x070F],
        follow.bus.ram[0x0760],
        follow.bus.ram[0x075C],
        follow.bus.ram[0x0757],
        follow.bus.ram[0x0713],
        follow.bus.ram[0x0785],
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: trace-sms <rom.sms> [--steps N] [--log-pcs]");
            eprintln!("                     [--buttons a,b,start,up,down,left,right]");
            eprintln!("                     [--buttons-at-frame FRAME:buttons]");
            eprintln!("                     [--buttons-script path]");
            eprintln!("                     [--checkpoint FRAME:name]");
            eprintln!("                     [--checkpoint-script path]");
            eprintln!("                     [--checkpoint-dir dir]");
            eprintln!("                     [--expect-no-trap]");
            eprintln!("                     [--expect-ram ADDR=HEX]");
            eprintln!("                     [--pad1-raw HEX]");
            eprintln!("                     [--search-end-routes]");
            std::process::exit(2);
        }
    };
    let mut steps: usize = 200_000;
    let mut log_pcs = false;
    let mut inject_irq = true;
    let mut controller_port_dc = 0xFF;
    let mut delayed_controller_port_dc: Option<u8> = None;
    let mut buttons_after_frame: Option<usize> = None;
    let mut button_events: Vec<(usize, u8)> = Vec::new();
    let mut expect_no_trap = false;
    let mut ram_expectations: Vec<RamExpectation> = Vec::new();
    let mut checkpoints: Vec<RouteCheckpoint> = Vec::new();
    let mut checkpoint_dir: PathBuf = PathBuf::from("out/smb/checkpoints");
    let mut search_late_routes = false;
    let mut search_end_routes = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--steps" => {
                i += 1;
                steps = args[i].parse().expect("steps int");
            }
            "--log-pcs" => log_pcs = true,
            "--search-late-routes" => search_late_routes = true,
            "--search-end-routes" => search_end_routes = true,
            "--no-irq" => inject_irq = false,
            "--buttons" => {
                i += 1;
                let value = args.get(i).expect("--buttons value");
                controller_port_dc = buttons_to_sms_port_dc(value);
            }
            "--buttons-after-frame" => {
                i += 1;
                let value = args.get(i).expect("--buttons-after-frame value");
                buttons_after_frame = Some(
                    value
                        .parse()
                        .expect("--buttons-after-frame expects an integer"),
                );
                delayed_controller_port_dc = Some(controller_port_dc);
                controller_port_dc = 0xFF;
            }
            "--buttons-at-frame" => {
                i += 1;
                let value = args.get(i).expect("--buttons-at-frame value");
                button_events.push(
                    parse_button_event(value)
                        .unwrap_or_else(|err| panic!("invalid --buttons-at-frame: {err}")),
                );
            }
            "--buttons-script" => {
                i += 1;
                let path = args.get(i).expect("--buttons-script path");
                button_events.extend(
                    load_button_script(path)
                        .unwrap_or_else(|err| panic!("invalid --buttons-script: {err}")),
                );
            }
            "--checkpoint" => {
                i += 1;
                let value = args.get(i).expect("--checkpoint FRAME:name");
                checkpoints.push(
                    parse_checkpoint_spec(value)
                        .unwrap_or_else(|err| panic!("invalid --checkpoint: {err}")),
                );
            }
            "--checkpoint-script" => {
                i += 1;
                let path = args.get(i).expect("--checkpoint-script path");
                checkpoints.extend(
                    load_checkpoint_script(path)
                        .unwrap_or_else(|err| panic!("invalid --checkpoint-script: {err}")),
                );
            }
            "--checkpoint-dir" => {
                i += 1;
                checkpoint_dir = PathBuf::from(args.get(i).expect("--checkpoint-dir path"));
            }
            "--expect-no-trap" => expect_no_trap = true,
            "--expect-ram" => {
                i += 1;
                let value = args.get(i).expect("--expect-ram ADDR=HEX");
                ram_expectations.push(
                    parse_ram_expectation(value)
                        .unwrap_or_else(|err| panic!("invalid --expect-ram: {err}")),
                );
            }
            "--pad1-raw" => {
                i += 1;
                let value = args.get(i).expect("--pad1-raw hex value");
                controller_port_dc = parse_hex_u8(value).expect("--pad1-raw expects hex byte");
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    button_events.sort_by_key(|(frame, _)| *frame);
    checkpoints.sort_by_key(|checkpoint| checkpoint.frame);

    if search_late_routes {
        run_late_route_search(&rom_path, &button_events);
        return;
    }
    if search_end_routes {
        run_end_route_search(&rom_path, &button_events);
        return;
    }

    let rom = std::fs::read(&rom_path).expect("read rom");
    let mut bus = SmsBus::new(rom, controller_port_dc);
    let mut cpu = Cpu::new();
    cpu.pc = 0x0000;
    cpu.sp = 0xDFF0;

    // PC histogram + last-100 ring buffer.
    let mut pc_counts: HashMap<u16, u32> = HashMap::new();
    let mut ring: Vec<(u16, u8)> = Vec::with_capacity(8192);
    // Separate ring of every non-sequential PC transition. Captures jumps
    // and rets at full step resolution without ballooning into NOP runs.
    let mut jump_ring: Vec<(u16, u16, u8, u8)> = Vec::with_capacity(2048);
    let mut last_pc: Option<u16> = None;
    let mut last_op: Option<u8> = None;
    let mut last_slot1: u8 = 1;
    let mut taken = 0usize;
    let mut last_err: Option<StepError> = None;
    let mut interrupt_at_step: Option<usize> = None;
    let mut first_translated_step: Option<usize> = None;
    let mut first_irq_handler_step: Option<usize> = None;
    let mut first_runtime_trap_step: Option<usize> = None;
    let mut first_ram_exec_step: Option<(usize, u16)> = None;
    // Count entries to specific addresses of interest.
    let mut call_targets: HashMap<u16, u32> = HashMap::new();
    let watch_exec_addr = std::env::var("SMS_WATCH_PC")
        .ok()
        .and_then(|s| parse_hex_addr(&s));
    let watch_exec_bank1 = std::env::var("SMS_WATCH_BANK1")
        .ok()
        .and_then(|s| u8::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
    let mut watch_exec_log: Vec<WatchExecHit> = Vec::new();
    // Ring of last 64 control transfers (CALL/RET/JP-indirect/conditional).
    // Each entry: (kind, from_pc, to_pc). Kind is "call", "ret", or "jp".
    let mut xfer_ring: Vec<(&'static str, u16, u16)> = Vec::with_capacity(256);

    // Inject an IRQ every 60k steps (roughly one "frame" of Z80 work).
    let mut next_irq_at = IRQ_PERIOD;
    let mut line_irq_at: Option<usize> = None;
    let mut irqs_fired = 0usize;
    let mut line_irqs_fired = 0usize;
    let mut next_button_event = 0usize;
    let mut next_checkpoint = 0usize;
    let mut checkpoint_dump_failed = false;
    let mut prev_frame_step = 0usize;
    let mut prev_frame_vram_writes = 0u32;
    let mut prev_frame_cram_writes = 0u32;
    let mut prev_frame_data_writes = 0u32;
    let mut prev_frame_control_writes = 0u32;
    let mut prev_frame_line_irqs = 0usize;
    let dump_each_frame_to = std::env::var("SMS_DUMP_EACH_FRAME").ok();
    let stop_on_fall = std::env::var("SMS_STOP_ON_FALL")
        .ok()
        .is_some_and(|v| v != "0");
    let mut first_fall_snapshot: Option<FallSnapshot> = None;

    for step in 0..steps {
        bus.watch_step = step;
        let pc = cpu.pc;
        bus.watch_pc = pc;
        bus.watch_bank1 = bus.slot_bank[1];
        bus.watch_sp = cpu.sp;
        let ret_lo = bus.read(cpu.sp) as u16;
        let ret_hi = bus.read(cpu.sp.wrapping_add(1)) as u16;
        bus.watch_ret = ret_lo | (ret_hi << 8);
        let op = bus.read(pc);
        if first_translated_step.is_none() && (0x4000..=0x7FFF).contains(&pc) {
            first_translated_step = Some(step);
        }
        if first_irq_handler_step.is_none() && pc == 0x0038 {
            first_irq_handler_step = Some(step);
        }
        if first_runtime_trap_step.is_none() && bus.ram[0x0B1D] == 0xE1 {
            first_runtime_trap_step = Some(step);
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!("*** first trap at step {step}: unresolved_id=${id:04X} pc=${pc:04X}");
        }
        if first_ram_exec_step.is_none() && pc >= 0xC000 {
            first_ram_exec_step = Some((step, pc));
        }
        if pc == RT_PPU_WRITE_ADDR {
            bus.record_trace_ppu_write_call(cpu.b, cpu.a);
        }
        if log_pcs && step < 200 {
            eprintln!("step {step:6}  PC=${pc:04X} op=${op:02X}");
        }
        *pc_counts.entry(pc).or_insert(0) += 1;
        if Some(pc) == watch_exec_addr
            && watch_exec_bank1.is_none_or(|bank| bank == bus.slot_bank[1])
        {
            watch_exec_log.push(WatchExecHit {
                step,
                pc,
                bank1: bus.slot_bank[1],
                sp: cpu.sp,
                ret: bus.watch_ret,
                op,
                a: cpu.a,
                f: cpu.f,
                p_shadow: bus.ram[0x0B03],
                x_shadow: bus.ram[0x0B00],
                y_shadow: bus.ram[0x0B01],
                zp00: bus.ram[0x0000],
                zp02: bus.ram[0x0002],
                zp03: bus.ram[0x0003],
                zp04: bus.ram[0x0004],
                zp05: bus.ram[0x0005],
                zp06: bus.ram[0x0006],
                zp07: bus.ram[0x0007],
                zp08: bus.ram[0x0008],
                ppage: bus.ram[0x006D],
                px: bus.ram[0x0086],
                ypage: bus.ram[0x00B5],
                py: bus.ram[0x00CE],
                yspeed: bus.ram[0x009F],
                eb: bus.ram[0x00EB],
                vertical_force: bus.ram[0x070E],
                area_obj_dispatch: bus.ram[0x0000].wrapping_add(bus.ram[0x0007]),
            });
        }
        if ring.len() == 8192 {
            ring.remove(0);
        }
        ring.push((pc, op));
        if let Some(prev) = last_pc {
            if pc.wrapping_sub(prev) > 3 {
                // Coalesce identical consecutive jumps (tight loops) into
                // a single entry to keep the ring useful over long runs.
                let same_as_last = jump_ring
                    .last()
                    .map(|&(p, t, _, _)| p == prev && t == pc)
                    .unwrap_or(false);
                if !same_as_last {
                    if jump_ring.len() == 16384 {
                        jump_ring.remove(0);
                    }
                    jump_ring.push((prev, pc, last_op.unwrap_or(0), last_slot1));
                }
            }
        }
        last_pc = Some(pc);
        last_op = Some(op);
        last_slot1 = bus.slot_bank[1];

        // Count calls: opcode $CD = unconditional CALL; track target.
        if op == 0xCD {
            let lo = bus.read(pc.wrapping_add(1)) as u16;
            let hi = bus.read(pc.wrapping_add(2)) as u16;
            let target = (hi << 8) | lo;
            *call_targets.entry(target).or_insert(0) += 1;
            if xfer_ring.len() == 256 {
                xfer_ring.remove(0);
            }
            xfer_ring.push(("call", pc, target));
        }
        // RET (unconditional). Conditional RETs (C0/C8/D0/D8/E0/E8/F0/F8) and
        // RETN/RETI (ED 45/4D etc) are recorded post-fact via the PC delta
        // below — we can't know if they took without executing first.
        if op == 0xC9 {
            // Peek the top of stack to predict the return target.
            let lo = bus.read(cpu.sp) as u16;
            let hi = bus.read(cpu.sp.wrapping_add(1)) as u16;
            let to = (hi << 8) | lo;
            if xfer_ring.len() == 256 {
                xfer_ring.remove(0);
            }
            xfer_ring.push(("ret", pc, to));
        }

        if inject_irq && step < next_irq_at && line_irq_at.is_some_and(|at| step >= at) && cpu.iff1
        {
            // Simulate a VDP line interrupt. It shares the IM1 vector with the
            // frame interrupt, but the status byte has bit 7 clear, so the
            // runtime can distinguish it after reading $BF.
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            bus.vdp_status_override = Some(0x00);
            cpu.pc = 0x0038;
            if first_irq_handler_step.is_none() {
                first_irq_handler_step = Some(step);
            }
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
            line_irqs_fired += 1;
            line_irq_at = None;
        }

        // Right before injecting the next IRQ, snapshot the framebuffer
        // so we can see how the screen evolves frame by frame.
        if inject_irq && step >= next_irq_at && cpu.iff1 {
            while next_button_event < button_events.len()
                && irqs_fired >= button_events[next_button_event].0
            {
                bus.controller_port_dc = button_events[next_button_event].1;
                next_button_event += 1;
            }
            if let (Some(frame), Some(port)) = (buttons_after_frame, delayed_controller_port_dc) {
                if irqs_fired >= frame {
                    bus.controller_port_dc = port;
                }
            }
            if let Some(ref dir) = dump_each_frame_to {
                let _ = std::fs::create_dir_all(dir);
                let path = format!("{dir}/frame_{:03}.ppm", irqs_fired);
                let _ = dump_framebuffer_ppm(&bus, &path);
            }
            while next_checkpoint < checkpoints.len()
                && irqs_fired >= checkpoints[next_checkpoint].frame
            {
                let checkpoint = &checkpoints[next_checkpoint];
                if let Err(err) =
                    dump_route_checkpoint(&bus, &cpu, step, irqs_fired, checkpoint, &checkpoint_dir)
                {
                    eprintln!(
                        "checkpoint dump failed for {} at frame {}: {err}",
                        checkpoint.name, checkpoint.frame
                    );
                    checkpoint_dump_failed = true;
                }
                next_checkpoint += 1;
            }
            // Per-frame peek of game state: mode/task plus key SMB gameplay
            // RAM. The gameplay fields are from the canonical SMB RAM map:
            // $86 player X, $6D player page, $57 horizontal speed,
            // $071A/$071C camera page/X, $06FC saved joypad bits.
            // $B5/$CE player Y page/position, $9F player Y speed, $1D
            // player action/state.
            let frame_steps = step.saturating_sub(prev_frame_step);
            let frame_vram_writes = bus.vram_writes.saturating_sub(prev_frame_vram_writes);
            let frame_cram_writes = bus.cram_writes.saturating_sub(prev_frame_cram_writes);
            let frame_data_writes = bus.vdp_data_writes.saturating_sub(prev_frame_data_writes);
            let frame_control_writes = bus
                .vdp_control_writes
                .saturating_sub(prev_frame_control_writes);
            let frame_line_irqs = line_irqs_fired.saturating_sub(prev_frame_line_irqs);
            eprintln!(
                "frame {:3}: $0770={:02X} $0772={:02X} $0773={:02X} $0774={:02X} ppos={:02X}:{:02X} spd={:02X} cam={:02X}:{:02X} y={:02X}:{:02X} yspd={:02X} act={:02X} joy={:02X} apage={:02X} bcol={:02X} aobj={:02X} aofs={:02X} alen={:02X}/{:02X}/{:02X} stop={:02X} steps={} vram+={} cram+={} data+={} ctrl+={} line_irq+={}",
                irqs_fired,
                bus.ram[0x0770],
                bus.ram[0x0772],
                bus.ram[0x0773],
                bus.ram[0x0774],
                bus.ram[0x006D],
                bus.ram[0x0086],
                bus.ram[0x0057],
                bus.ram[0x071A],
                bus.ram[0x071C],
                bus.ram[0x00B5],
                bus.ram[0x00CE],
                bus.ram[0x009F],
                bus.ram[0x001D],
                bus.ram[0x06FC],
                bus.ram[0x0725],
                bus.ram[0x06A0],
                bus.ram[0x072A],
                bus.ram[0x072C],
                bus.ram[0x0730],
                bus.ram[0x0731],
                bus.ram[0x0732],
                bus.ram[0x0723],
                frame_steps,
                frame_vram_writes,
                frame_cram_writes,
                frame_data_writes,
                frame_control_writes,
                frame_line_irqs,
            );
            prev_frame_step = step;
            prev_frame_vram_writes = bus.vram_writes;
            prev_frame_cram_writes = bus.cram_writes;
            prev_frame_data_writes = bus.vdp_data_writes;
            prev_frame_control_writes = bus.vdp_control_writes;
            prev_frame_line_irqs = line_irqs_fired;
            if first_fall_snapshot.is_none() && (bus.ram[0x0723] != 0 || bus.ram[0x00B5] >= 0x02) {
                let recent_reads = bus
                    .watch_read_log
                    .iter()
                    .rev()
                    .take(80)
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let recent_writes = bus
                    .watch_log
                    .iter()
                    .rev()
                    .take(80)
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                first_fall_snapshot = Some(FallSnapshot {
                    step,
                    frame: irqs_fired,
                    ram: bus.ram,
                    recent_reads,
                    recent_writes,
                });
                if stop_on_fall {
                    taken = step;
                    break;
                }
            }
        }
        if inject_irq && step >= next_irq_at && cpu.iff1 {
            // Simulate a maskable interrupt: push PC, jump to $0038 (IM1).
            if interrupt_at_step.is_none() {
                interrupt_at_step = Some(step);
            }
            line_irq_at = None;
            bus.render_scroll_split = None;
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            bus.vdp_status_override = Some(0x80);
            cpu.pc = 0x0038;
            if first_irq_handler_step.is_none() {
                first_irq_handler_step = Some(step);
            }
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
            irqs_fired += 1;
            next_irq_at = next_irq_at.saturating_add(IRQ_PERIOD);
        }

        if cpu.halted {
            // halted but no IRQ pending — endless halt. Stop.
            break;
        }
        match cpu.step(&mut bus) {
            Ok(()) => {
                taken += 1;
                if bus.vdp_regs[0] & 0x10 == 0 {
                    line_irq_at = None;
                } else if inject_irq && cpu.iff1 && line_irq_at.is_none() {
                    // R10 is loaded with one less than the target raster line.
                    // Convert that to a coarse instruction-step delay; this is
                    // not cycle-accurate, but it lets checkpoint rendering see
                    // the runtime's one-shot post-split scroll before the next
                    // frame IRQ snapshot.
                    let target_line = (bus.vdp_regs[10] as usize + 1).min(223);
                    bus.render_scroll_split = Some((target_line, bus.vdp_regs[8], bus.vdp_regs[9]));
                    let line_delay = (bus.vdp_regs[10] as usize + 1).clamp(8, 512);
                    line_irq_at = Some(step.saturating_add(line_delay));
                }
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    println!("=== trace-sms summary ===");
    println!("ROM: {}", rom_path.display());
    println!("steps run: {taken}");
    if let Some(e) = &last_err {
        println!("stopped on error: {e:?}");
    }
    if let Some(at) = interrupt_at_step {
        println!("injected IRQ at step {at}");
    }
    println!("\nMilestones:");
    print_milestone("entered translated slot-1 code", first_translated_step);
    print_milestone("entered IRQ/NMI bridge at $0038", first_irq_handler_step);
    print_milestone("hit runtime trap marker $CB1D=$E1", first_runtime_trap_step);
    match first_ram_exec_step {
        Some((step, pc)) => println!("  yes: executed RAM at ${pc:04X} at step {step}"),
        None => println!("   no: executed RAM at $C000-$FFFF"),
    }
    println!("VRAM writes: {}", bus.vram_writes);
    println!("CRAM writes: {}", bus.cram_writes);
    println!("VDP data-port writes: {}", bus.vdp_data_writes);
    println!("VDP control-port writes: {}", bus.vdp_control_writes);
    println!("VDP status reads: {}", bus.vdp_status_reads);
    println!("Controller reads: {}", bus.controller_reads);
    println!("Controller $DC raw: ${:02X}", bus.controller_port_dc);
    println!(
        "Gameplay state: player={:02X}:{:02X} speed=${:02X} camera={:02X}:{:02X} joy=${:02X} state=$0E:{:02X}",
        bus.ram[0x006D],
        bus.ram[0x0086],
        bus.ram[0x0057],
        bus.ram[0x071A],
        bus.ram[0x071C],
        bus.ram[0x06FC],
        bus.ram[0x000E],
    );
    println!(
        "Scroll state: scroll_x={:02X}:{:02X} vscroll_latch=${:02X}:${:02X} gates 06FF=${:02X} 03A1=${:02X} 0723=${:02X} 0755=${:02X} 0785=${:02X}",
        bus.ram[0x071A],
        bus.ram[0x071C],
        bus.ram[0x073F],
        bus.ram[0x0740],
        bus.ram[0x06FF],
        bus.ram[0x03A1],
        bus.ram[0x0723],
        bus.ram[0x0755],
        bus.ram[0x0785],
    );
    println!(
        "Player physics: y={:02X}:{:02X} yspd=${:02X} yfrac=${:02X} yfrac_spd=${:02X} action=${:02X} move_force=${:02X} jump_origin=${:02X} vertical_force=${:02X} friction_gate=${:02X}",
        bus.ram[0x00B5],
        bus.ram[0x00CE],
        bus.ram[0x009F],
        bus.ram[0x0416],
        bus.ram[0x0433],
        bus.ram[0x0704],
        bus.ram[0x0709],
        bus.ram[0x070A],
        bus.ram[0x070E],
        bus.ram[0x0747],
    );
    println!(
        "Parser state: page=${:02X} col=${:02X} back=${:02X} behind=${:02X} obj_page=${:02X} page_sel=${:02X} data_ofs=${:02X} slot_ofs=${:02X}/${:02X}/${:02X} len=${:02X}/${:02X}/${:02X} stair=${:02X} height=${:02X} block_col=${:02X}",
        bus.ram[0x0725],
        bus.ram[0x0726],
        bus.ram[0x0728],
        bus.ram[0x0729],
        bus.ram[0x072A],
        bus.ram[0x072B],
        bus.ram[0x072C],
        bus.ram[0x072D],
        bus.ram[0x072E],
        bus.ram[0x072F],
        bus.ram[0x0730],
        bus.ram[0x0731],
        bus.ram[0x0732],
        bus.ram[0x0734],
        bus.ram[0x0735],
        bus.ram[0x06A0],
    );
    println!(
        "End-level state: star_flag_task=${:02X} collision_bits=${:02X} enemy_flag=${:02X} scroll_lock=${:02X} flag_score=${:02X} flag_y=${:02X} area=${:02X} level=${:02X} fetch_timer=${:02X} end_y=${:02X} slide_timer=${:02X}",
        bus.ram[0x0746],
        bus.ram[0x0490],
        bus.ram[0x001B],
        bus.ram[0x0723],
        bus.ram[0x010F],
        bus.ram[0x070F],
        bus.ram[0x0760],
        bus.ram[0x075C],
        bus.ram[0x0757],
        bus.ram[0x0713],
        bus.ram[0x0785],
    );
    if let Some(snapshot) = &first_fall_snapshot {
        print_fall_snapshot(snapshot);
    }
    println!("VDP control I/O entries: {}", bus.io_log.len());
    println!("IRQs fired: {irqs_fired}");
    println!("Line IRQs fired: {line_irqs_fired}");
    println!(
        "Bank mapping: slot0={} slot1={} slot2={}",
        bus.slot_bank[0], bus.slot_bank[1], bus.slot_bank[2]
    );
    println!("Mapper writes total: {}", bus.bank_writes.len());
    for (port, value) in bus.bank_writes.iter().take(20) {
        println!("  W ${port:04X} = ${value:02X}");
    }
    if bus.watch_addr.is_some() || bus.watch_write_range.is_some() {
        if let Some(addr) = bus.watch_addr {
            println!("Write watch ${addr:04X}: {} writes", bus.watch_log.len());
        }
        if let Some((start, end)) = bus.watch_write_range {
            println!(
                "Write watch ${start:04X}-${end:04X}: {} writes",
                bus.watch_log.len()
            );
        }
        for write in bus.watch_log.iter().take(40) {
            println!(
                "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                write.step,
                write.addr,
                write.pc,
                write.bank1,
                write.sp,
                write.ret,
                write.value,
                write.ppage,
                write.px,
                write.ypage,
                write.py,
                write.player_state
            );
        }
        if bus.watch_log.len() > 40 {
            println!("  ... last 40 writes:");
            for write in bus
                .watch_log
                .iter()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!(
                    "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                    write.step,
                    write.addr,
                    write.pc,
                    write.bank1,
                    write.sp,
                    write.ret,
                    write.value,
                    write.ppage,
                    write.px,
                    write.ypage,
                    write.py,
                    write.player_state
                );
            }
        }
        let mut value_hist: HashMap<u8, usize> = HashMap::new();
        for write in &bus.watch_log {
            *value_hist.entry(write.value).or_insert(0) += 1;
        }
        let mut value_hist = value_hist.into_iter().collect::<Vec<_>>();
        value_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  write value histogram:");
        for (value, count) in value_hist.into_iter().take(16) {
            print!(" ${value:02X}:{count}");
        }
        println!();
        let flag_writes = bus
            .watch_log
            .iter()
            .filter(|write| matches!(write.value, 0x24 | 0x25))
            .collect::<Vec<_>>();
        println!("  flag metatile writes ($24/$25): {}", flag_writes.len());
        for write in flag_writes.iter().take(20) {
            println!(
                "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                write.step,
                write.addr,
                write.pc,
                write.bank1,
                write.value,
                write.ppage,
                write.px,
                write.ypage,
                write.py,
                write.player_state
            );
        }
        if flag_writes.len() > 20 {
            println!("    ... last 20 flag metatile writes:");
            for write in flag_writes.iter().rev().take(20).rev() {
                println!(
                    "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                    write.step,
                    write.addr,
                    write.pc,
                    write.bank1,
                    write.value,
                    write.ppage,
                    write.px,
                    write.ypage,
                    write.py,
                    write.player_state
                );
            }
        }
    }
    if bus.watch_read_addr.is_some() || bus.watch_read_range.is_some() {
        if let Some(addr) = bus.watch_read_addr {
            println!("Read watch ${addr:04X}: {} reads", bus.watch_read_log.len());
        }
        if let Some((start, end)) = bus.watch_read_range {
            println!(
                "Read watch ${start:04X}-${end:04X}: {} reads",
                bus.watch_read_log.len()
            );
        }
        for read in bus.watch_read_log.iter().take(40) {
            println!(
                "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
                read.step,
                read.addr,
                read.pc,
                read.bank1,
                read.sp,
                read.ret,
                read.value,
                read.ppage,
                read.px,
                read.ypage,
                read.py,
                read.player_state,
                read.x_shadow,
                read.y_shadow,
                read.zp02,
                read.zp03,
                read.zp04,
                read.zp05,
                read.zp06,
                read.zp07,
                read.zp08,
                read.yspeed,
                read.eb,
                read.vertical_force
            );
        }
        if bus.watch_read_log.len() > 40 {
            println!("  ... last 40 reads:");
            for read in bus.watch_read_log.iter().rev().take(40).rev() {
                println!(
                    "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
                    read.step,
                    read.addr,
                    read.pc,
                    read.bank1,
                    read.sp,
                    read.ret,
                    read.value,
                    read.ppage,
                    read.px,
                    read.ypage,
                    read.py,
                    read.player_state,
                    read.x_shadow,
                    read.y_shadow,
                    read.zp02,
                    read.zp03,
                    read.zp04,
                    read.zp05,
                    read.zp06,
                    read.zp07,
                    read.zp08,
                    read.yspeed,
                    read.eb,
                    read.vertical_force
                );
            }
        }
        let flag_reads = bus
            .watch_read_log
            .iter()
            .filter(|read| matches!(read.value, 0x24 | 0x25))
            .collect::<Vec<_>>();
        println!("  flag metatile reads ($24/$25): {}", flag_reads.len());
        for read in flag_reads.iter().take(20) {
            println!(
                "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} eb=${:02X}",
                read.step,
                read.addr,
                read.pc,
                read.bank1,
                read.ret,
                read.value,
                read.ppage,
                read.px,
                read.ypage,
                read.py,
                read.player_state,
                read.x_shadow,
                read.y_shadow,
                read.zp04,
                read.zp05,
                read.zp06,
                read.zp07,
                read.eb,
            );
        }
        if flag_reads.len() > 20 {
            println!("    ... last 20 flag metatile reads:");
            for read in flag_reads.iter().rev().take(20).rev() {
                println!(
                    "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} eb=${:02X}",
                    read.step,
                    read.addr,
                    read.pc,
                    read.bank1,
                    read.ret,
                    read.value,
                    read.ppage,
                    read.px,
                    read.ypage,
                    read.py,
                    read.player_state,
                    read.x_shadow,
                    read.y_shadow,
                    read.zp04,
                    read.zp05,
                    read.zp06,
                    read.zp07,
                    read.eb,
                );
            }
        }
    }
    if let Some(addr) = watch_exec_addr {
        println!("PC watch ${addr:04X}: {} hits", watch_exec_log.len());
        let mut by_a: HashMap<u8, usize> = HashMap::new();
        let mut by_zp04: HashMap<u8, usize> = HashMap::new();
        let mut by_a_zp04: HashMap<(u8, u8), usize> = HashMap::new();
        let mut by_dispatch: HashMap<u8, usize> = HashMap::new();
        for hit in &watch_exec_log {
            *by_a.entry(hit.a).or_insert(0) += 1;
            *by_zp04.entry(hit.zp04).or_insert(0) += 1;
            *by_a_zp04.entry((hit.a, hit.zp04)).or_insert(0) += 1;
            *by_dispatch.entry(hit.area_obj_dispatch).or_insert(0) += 1;
        }
        let mut a_hist = by_a.into_iter().collect::<Vec<_>>();
        a_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  A histogram:");
        for (a, count) in a_hist.into_iter().take(16) {
            print!(" ${a:02X}:{count}");
        }
        println!();
        let mut zp04_hist = by_zp04.into_iter().collect::<Vec<_>>();
        zp04_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  zp04 histogram:");
        for (zp04, count) in zp04_hist.into_iter().take(16) {
            print!(" ${zp04:02X}:{count}");
        }
        println!();
        let mut combo_hist = by_a_zp04.into_iter().collect::<Vec<_>>();
        combo_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  A/zp04 combos:");
        for ((a, zp04), count) in combo_hist.into_iter().take(16) {
            print!(" ${a:02X}/${zp04:02X}:{count}");
        }
        println!();
        let mut dispatch_hist = by_dispatch.into_iter().collect::<Vec<_>>();
        dispatch_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  area obj dispatch ($00+$07) histogram:");
        for (dispatch, count) in dispatch_hist.into_iter().take(32) {
            print!(" ${dispatch:02X}:{count}");
        }
        println!();
        for hit in watch_exec_log.iter().take(40) {
            println!(
                "  step {}: pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X}",
                hit.step,
                hit.pc,
                hit.bank1,
                hit.sp,
                hit.ret,
                hit.op,
                hit.a,
                hit.f,
                hit.p_shadow,
                hit.x_shadow,
                hit.y_shadow,
                hit.ppage,
                hit.px,
                hit.ypage,
                hit.py,
                hit.yspeed,
                hit.eb,
                hit.vertical_force,
                hit.zp00,
                hit.zp02,
                hit.zp03,
                hit.zp04,
                hit.zp05,
                hit.zp06,
                hit.zp07,
                hit.zp08,
                hit.area_obj_dispatch
            );
        }
        if watch_exec_log.len() > 40 {
            println!("  ... last 40 hits:");
            for hit in watch_exec_log
                .iter()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!(
                    "  step {}: pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X}",
                    hit.step,
                    hit.pc,
                    hit.bank1,
                    hit.sp,
                    hit.ret,
                    hit.op,
                    hit.a,
                    hit.f,
                    hit.p_shadow,
                    hit.x_shadow,
                    hit.y_shadow,
                    hit.ppage,
                    hit.px,
                    hit.ypage,
                    hit.py,
                    hit.yspeed,
                    hit.eb,
                    hit.vertical_force,
                    hit.zp00,
                    hit.zp02,
                    hit.zp03,
                    hit.zp04,
                    hit.zp05,
                    hit.zp06,
                    hit.zp07,
                    hit.zp08,
                    hit.area_obj_dispatch
                );
            }
        }
    }
    println!(
        "Cpu state: A=${:02X} B=${:02X} C=${:02X} D=${:02X} E=${:02X} H=${:02X} L=${:02X} SP=${:04X} PC=${:04X} IFF1={}",
        cpu.a, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l, cpu.sp, cpu.pc, cpu.iff1
    );
    let shadow_p = bus.read(0xCB03);
    println!(
        "Shadow 6502: A={:02X} X={:02X} Y={:02X} P={:02X} S={:02X}",
        cpu.a,
        bus.read(0xCB00),
        bus.read(0xCB01),
        shadow_p,
        bus.read(0xCB02)
    );
    let unresolved_id = (bus.read(0xCB1C) as u16) << 8 | bus.read(0xCB1B) as u16;
    println!(
        "Runtime diagnostics: unresolved_id=${unresolved_id:04X} trap_marker=${:02X} vbuf_used=${:02X} ppu_addr=${:02X}{:02X} ppu_mask=${:02X} split_flags=${:02X} split_pre=${:02X}:${:02X} split_post=${:02X}:${:02X}",
        bus.read(0xCB1D),
        bus.read(0xC800),
        bus.read(0xCB0F),
        bus.read(0xCB10),
        bus.read(0xCB09),
        bus.read(0xCB20),
        bus.read(0xCB21),
        bus.read(0xCB22),
        bus.read(0xCB23),
        bus.read(0xCB24)
    );
    println!("{}", format_nt_trace_ciram_summary(&bus, true));
    println!("{}", format_nt_trace_ciram_summary(&bus, false));
    println!("NES zero page $00-$0F:");
    for i in 0..16 {
        let b = bus.ram[i];
        print!(" ${b:02X}");
    }
    println!();
    let zp_ptr = ((bus.ram[1] as u16) << 8) | bus.ram[0] as u16;
    println!("NES ($00) pointer ${zp_ptr:04X} first 64 bytes:");
    for i in 0..64u16 {
        let off = zp_ptr.wrapping_add(i) as usize & 0x07ff;
        let b = bus.ram[off];
        print!(" ${b:02X}");
    }
    println!();

    // Top-10 most-visited PCs.
    let mut sorted: Vec<_> = pc_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\nTop 10 most-visited PCs:");
    for (pc, n) in sorted.iter().take(10) {
        println!("  ${pc:04X}: {n} times");
    }

    println!("\nLast 64 PC jumps (from op@bank1 -> to):");
    for (from, to, op, bank) in jump_ring.iter().rev().take(64).rev() {
        println!("  ${from:04X} op=${op:02X} bank1=${bank:02X} -> ${to:04X}");
    }

    println!("\nLast 32 control transfers (call/ret):");
    for (kind, from, to) in xfer_ring.iter().rev().take(32).rev() {
        println!("  {kind} from ${from:04X} -> ${to:04X}");
    }

    println!("\nFirst 32 I/O log entries:");
    for e in bus.io_log.iter().take(32) {
        println!("  {e}");
    }
    println!("\nVRAM peek $3F00 (SAT Y bytes): ");
    for i in 0..16 {
        let b = bus.vram[0x3F00 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $3F80 (SAT X+tile bytes): ");
    for i in 0..16 {
        let b = bus.vram[0x3F80 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Sprite data $C200 (NES $0200, first 32 bytes Y,tile,attr,X):");
    for i in 0..32 {
        let b = bus.ram[0x0200 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("OAM staging $C900 (first 32 bytes = 8 sprites NES Y,tile,attr,X):");
    for i in 0..32 {
        let b = bus.ram[0x0900 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("NES VRAM buffer $C300 (first 64 bytes):");
    for i in 0..64 {
        let b = bus.ram[0x0300 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("NES VRAM buffer $C340 (64 bytes):");
    for i in 0..64 {
        let b = bus.ram[0x0340 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Metatile buffer $C6A0-$C6AF:");
    for i in 0..16 {
        let b = bus.ram[0x06A0 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer 1 $C500-$C53F:");
    for i in 0..64 {
        let b = bus.ram[0x0500 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer 2 $C5D0-$C60F:");
    for i in 0..64 {
        let b = bus.ram[0x05D0 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer floor rows $C5C0-$C5CF / $C690-$C69F:");
    for i in 0..16 {
        let b = bus.ram[0x05C0 + i];
        print!(" ${b:02X}");
    }
    print!("  | ");
    for i in 0..16 {
        let b = bus.ram[0x0690 + i];
        print!(" ${b:02X}");
    }
    println!();
    let block_nonzero: Vec<(usize, u8)> = (0x0500..=0x06AF)
        .filter_map(|addr| {
            let b = bus.ram[addr];
            (b != 0).then_some((addr, b))
        })
        .collect();
    println!(
        "Block/metatile non-zero entries $C500-$C6AF: {}",
        block_nonzero.len()
    );
    for (addr, b) in block_nonzero.iter().take(80) {
        print!(" ${:04X}={:02X}", 0xC000 + addr, b);
    }
    println!();
    println!("CRAM (32 bytes):");
    for i in 0..32 {
        let b = bus.cram[i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $3800 (nametable start):");
    for i in 0..32 {
        let b = bus.vram[0x3700 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $0000 (tile 0):");
    for i in 0..32 {
        let b = bus.vram[i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $0020 (tile 1):");
    for i in 0..32 {
        let b = bus.vram[0x20 + i];
        print!(" ${b:02X}");
    }
    println!();
    // Quick nametable scan: find any non-zero entry.
    let mut nz_count = 0;
    for i in 0..1792 {
        if bus.vram[0x3700 + i] != 0 {
            nz_count += 1;
        }
    }
    println!("Nametable non-zero bytes: {nz_count}/1792");
    let mut chr_nz = 0;
    for i in 0..0x3700 {
        if bus.vram[i] != 0 {
            chr_nz += 1;
        }
    }
    println!("Tile pattern non-zero bytes: {chr_nz}/{}", 0x3700);

    // ASCII nametable dump: print each cell's low-byte tile index as 2-hex.
    // SMS nametable is 32 cols x 28 rows of 16-bit entries (low/high bytes).
    println!("\nNametable (low byte per cell, '.' = 0):");
    for row in 0..28 {
        let mut line = String::new();
        for col in 0..32 {
            let off = 0x3700 + (row * 32 + col) * 2;
            let lo = bus.vram[off];
            if lo == 0 {
                line.push_str("..");
            } else {
                line.push_str(&format!("{lo:02X}"));
            }
        }
        println!("{row:2}: {line}");
    }

    println!("\nTop 20 most-called targets:");
    let mut call_sorted: Vec<_> = call_targets.into_iter().collect();
    call_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (target, n) in call_sorted.iter().take(20) {
        println!("  call ${target:04X}: {n} times");
    }

    // Dump SMS framebuffer to PPM if requested by env var SMS_DUMP_PPM.
    if let Ok(path) = std::env::var("SMS_DUMP_PPM") {
        if let Err(e) = dump_framebuffer_ppm(&bus, &path) {
            eprintln!("PPM dump failed: {e}");
        } else {
            println!("Wrote framebuffer PPM to {path}");
        }
    }

    let mut acceptance_failed = false;
    if checkpoint_dump_failed {
        eprintln!("EXPECT FAIL: one or more checkpoint artifacts failed to write");
        acceptance_failed = true;
    }
    if next_checkpoint < checkpoints.len() {
        eprintln!(
            "EXPECT FAIL: {} checkpoint(s) not reached; next is frame {} ({})",
            checkpoints.len() - next_checkpoint,
            checkpoints[next_checkpoint].frame,
            checkpoints[next_checkpoint].name
        );
        acceptance_failed = true;
    }
    if expect_no_trap {
        if let Some(step) = first_runtime_trap_step {
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!(
                "EXPECT FAIL: runtime trap marker hit at step {step}, unresolved_id=${id:04X}"
            );
            acceptance_failed = true;
        } else if bus.ram[0x0B1D] == 0xE1 {
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!("EXPECT FAIL: runtime trap marker set, unresolved_id=${id:04X}");
            acceptance_failed = true;
        } else {
            println!("EXPECT ok: no runtime trap marker");
        }
    }
    for expected in &ram_expectations {
        let Some(idx) = ram_index(expected.addr) else {
            eprintln!(
                "EXPECT FAIL: RAM address ${:04X} is outside 8 KiB RAM/mirror",
                expected.addr
            );
            acceptance_failed = true;
            continue;
        };
        let actual = bus.ram[idx];
        if actual != expected.value {
            eprintln!(
                "EXPECT FAIL: RAM ${:04X} expected ${:02X}, got ${:02X}",
                expected.addr, expected.value, actual
            );
            acceptance_failed = true;
        } else {
            println!(
                "EXPECT ok: RAM ${:04X} == ${:02X}",
                expected.addr, expected.value
            );
        }
    }
    if acceptance_failed {
        std::process::exit(1);
    }
}

fn print_milestone(label: &str, step: Option<usize>) {
    match step {
        Some(step) => println!("  yes: {label} at step {step}"),
        None => println!("   no: {label}"),
    }
}

fn ram_at(ram: &[u8; RAM_SIZE], addr: u16) -> u8 {
    ram[(addr as usize) & (RAM_SIZE - 1)]
}

fn print_ram_range(label: &str, ram: &[u8; RAM_SIZE], start: u16, end: u16) {
    println!("{label} ${start:04X}-${end:04X}:");
    let mut addr = start;
    while addr <= end {
        print!("  ${addr:04X}:");
        for i in 0..16u16 {
            let a = addr.wrapping_add(i);
            if a > end {
                break;
            }
            print!(" {:02X}", ram_at(ram, a));
        }
        println!();
        if end.wrapping_sub(addr) < 16 {
            break;
        }
        addr = addr.wrapping_add(16);
    }
}

fn print_watch_tail(label: &str, entries: &[WatchWrite]) {
    println!("{label}: {} entries", entries.len());
    for entry in entries {
        println!(
            "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
            entry.step,
            entry.addr,
            entry.pc,
            entry.bank1,
            entry.sp,
            entry.ret,
            entry.value,
            entry.ppage,
            entry.px,
            entry.ypage,
            entry.py,
            entry.player_state,
            entry.x_shadow,
            entry.y_shadow,
            entry.zp02,
            entry.zp03,
            entry.zp04,
            entry.zp05,
            entry.zp06,
            entry.zp07,
            entry.zp08,
            entry.yspeed,
            entry.eb,
            entry.vertical_force,
        );
    }
}

fn print_fall_snapshot(snapshot: &FallSnapshot) {
    let ram = &snapshot.ram;
    println!(
        "\nFirst fall/death snapshot: frame={} step={} ppos={:02X}:{:02X} speed=${:02X} cam={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} act=${:02X} joy=${:02X} state=$0E:{:02X}",
        snapshot.frame,
        snapshot.step,
        ram_at(ram, 0x006D),
        ram_at(ram, 0x0086),
        ram_at(ram, 0x0057),
        ram_at(ram, 0x071A),
        ram_at(ram, 0x071C),
        ram_at(ram, 0x00B5),
        ram_at(ram, 0x00CE),
        ram_at(ram, 0x009F),
        ram_at(ram, 0x001D),
        ram_at(ram, 0x06FC),
        ram_at(ram, 0x000E),
    );
    println!(
        "  parser: apage={:02X} block_col={:02X} area_obj={:02X} aofs={:02X} len={:02X}/{:02X}/{:02X} stop={:02X} scroll_gates 06FF={:02X} 03A1={:02X}",
        ram_at(ram, 0x0725),
        ram_at(ram, 0x06A0),
        ram_at(ram, 0x072A),
        ram_at(ram, 0x072C),
        ram_at(ram, 0x0730),
        ram_at(ram, 0x0731),
        ram_at(ram, 0x0732),
        ram_at(ram, 0x0723),
        ram_at(ram, 0x06FF),
        ram_at(ram, 0x03A1),
    );
    println!(
        "  collision temps: zp00={:02X} zp01={:02X} zp04={:02X} zp06={:02X} zp07={:02X} eb={:02X} vertical_force={:02X}",
        ram_at(ram, 0x0000),
        ram_at(ram, 0x0001),
        ram_at(ram, 0x0004),
        ram_at(ram, 0x0006),
        ram_at(ram, 0x0007),
        ram_at(ram, 0x00EB),
        ram_at(ram, 0x070E),
    );
    print_ram_range("  Area parser row/buffer", ram, 0x06A0, 0x06AF);
    print_ram_range("  Block buffers", ram, 0xC500, 0xC6AF);
    print_watch_tail("  Recent watched reads before fall", &snapshot.recent_reads);
    print_watch_tail(
        "  Recent watched writes before fall",
        &snapshot.recent_writes,
    );
}

fn dump_route_checkpoint(
    bus: &SmsBus,
    cpu: &Cpu,
    step: usize,
    actual_frame: usize,
    checkpoint: &RouteCheckpoint,
    dir: &Path,
) -> std::io::Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(dir)?;
    let slug = checkpoint_slug(&checkpoint.name);
    let stem = format!("{:05}_{}", checkpoint.frame, slug);
    let ppm_path = dir.join(format!("{stem}.ppm"));
    let txt_path = dir.join(format!("{stem}.txt"));

    dump_framebuffer_ppm(bus, ppm_path.to_string_lossy().as_ref())?;

    let mut f = std::fs::File::create(&txt_path)?;
    writeln!(f, "checkpoint: {}", checkpoint.name)?;
    writeln!(f, "target_frame: {}", checkpoint.frame)?;
    writeln!(f, "actual_frame: {actual_frame}")?;
    writeln!(f, "step: {step}")?;
    writeln!(
        f,
        "cpu: pc=${:04X} sp=${:04X} a=${:02X} f=${:02X} iff1={}",
        cpu.pc, cpu.sp, cpu.a, cpu.f, cpu.iff1
    )?;
    writeln!(
        f,
        "mode: 0770=${:02X} 0772=${:02X} 0773=${:02X} 0774=${:02X} state_000E=${:02X}",
        bus.ram[0x0770], bus.ram[0x0772], bus.ram[0x0773], bus.ram[0x0774], bus.ram[0x000E]
    )?;
    writeln!(
        f,
        "player: page=${:02X} x=${:02X} ypage=${:02X} y=${:02X} xspd=${:02X} yspd=${:02X} action=${:02X}",
        bus.ram[0x006D],
        bus.ram[0x0086],
        bus.ram[0x00B5],
        bus.ram[0x00CE],
        bus.ram[0x0057],
        bus.ram[0x009F],
        bus.ram[0x001D]
    )?;
    writeln!(
        f,
        "scroll: cam_page=${:02X} cam_x=${:02X} vdp_reg8=${:02X} vdp_reg9=${:02X} nes_scroll_x=${:02X}",
        bus.ram[0x071A], bus.ram[0x071C], bus.vdp_regs[8], bus.vdp_regs[9], bus.ram[0x0B0C]
    )?;
    writeln!(
        f,
        "route: area=${:02X} level=${:02X} fetch_timer=${:02X} end_y=${:02X} slide_timer=${:02X}",
        bus.ram[0x0760], bus.ram[0x075C], bus.ram[0x0757], bus.ram[0x0713], bus.ram[0x0785]
    )?;
    writeln!(
        f,
        "runtime: trap_marker=${:02X} unresolved_id=${:04X} vbuf_used=${:02X} ppu_addr=${:02X}{:02X} ppu_mask=${:02X}",
        bus.ram[0x0B1D],
        ((bus.ram[0x0B1C] as u16) << 8) | bus.ram[0x0B1B] as u16,
        bus.ram[0x0800],
        bus.ram[0x0B0F],
        bus.ram[0x0B10],
        bus.ram[0x0B09]
    )?;
    writeln!(
        f,
        "split_scroll: flags=${:02X} pre=${:02X}:${:02X} post=${:02X}:${:02X} render_split={}",
        bus.ram[0x0B20],
        bus.ram[0x0B21],
        bus.ram[0x0B22],
        bus.ram[0x0B23],
        bus.ram[0x0B24],
        bus.render_scroll_split
            .map(|(line, top_x, top_y)| format!(
                "line={line} top_reg8=${top_x:02X} top_reg9=${top_y:02X}"
            ))
            .unwrap_or_else(|| "none".to_string())
    )?;
    writeln!(
        f,
        "vdp: r0=${:02X} r10=${:02X} vram_writes={} cram_writes={} data_writes={} control_writes={} status_reads={} controller_reads={}",
        bus.vdp_regs[0],
        bus.vdp_regs[10],
        bus.vram_writes,
        bus.cram_writes,
        bus.vdp_data_writes,
        bus.vdp_control_writes,
        bus.vdp_status_reads,
        bus.controller_reads
    )?;
    writeln!(
        f,
        "counts: nametable_nonzero={} chr_nonzero={} active_sprites={}",
        nametable_nonzero_bytes(bus),
        chr_nonzero_bytes(bus),
        active_sprite_count(bus)
    )?;
    writeln!(
        f,
        "nt_attr_shadow_nonzero={} first_nonzero={}",
        nt_attr_shadow_nonzero_bytes(bus),
        format_nt_attr_shadow_first_nonzero(bus)
    )?;
    writeln!(f, "{}", format_nt_trace_ciram_summary(bus, true))?;
    writeln!(f, "{}", format_nt_trace_ciram_summary(bus, false))?;
    writeln!(
        f,
        "nt_columns_nonzero_cells: {}",
        format_nametable_column_occupancy(bus)
    )?;
    writeln!(f, "{}", format_nt_fold_collisions(bus))?;
    writeln!(f, "{}", format_nt_explicit_s_mismatches(bus, false))?;
    writeln!(f, "{}", format_nt_explicit_s_mismatches(bus, true))?;
    writeln!(f, "framebuffer: {}", ppm_path.display())?;
    write_checkpoint_sat_diagnostics(&mut f, bus)?;

    println!(
        "CHECKPOINT {} frame={} actual_frame={} ppm={} state={}",
        checkpoint.name,
        checkpoint.frame,
        actual_frame,
        ppm_path.display(),
        txt_path.display()
    );

    Ok(())
}

fn nametable_nonzero_bytes(bus: &SmsBus) -> usize {
    (0..1792).filter(|i| bus.vram[0x3700 + i] != 0).count()
}

fn nt_attr_shadow_nonzero_bytes(bus: &SmsBus) -> usize {
    // Runtime $CB80-$CBFF maps to SMS RAM offset $0B80-$0BFF.
    (0..0x80).filter(|i| bus.ram[0x0B80 + i] != 0).count()
}

fn format_nt_attr_shadow_first_nonzero(bus: &SmsBus) -> String {
    let entries = (0..0x80)
        .filter_map(|i| {
            let value = bus.ram[0x0B80 + i];
            (value != 0).then(|| format!("{:02X}:{value:02X}", i))
        })
        .take(12)
        .collect::<Vec<_>>();
    if entries.is_empty() {
        "none".to_string()
    } else {
        entries.join(" ")
    }
}

fn format_nt_trace_ciram_summary(bus: &SmsBus, vertical_mirroring: bool) -> String {
    let (mode, ciram) = if vertical_mirroring {
        ("vertical", &bus.nt_trace_ciram_vertical)
    } else {
        ("horizontal", &bus.nt_trace_ciram_horizontal)
    };
    let tile_nonzero = (0..2)
        .flat_map(|page| (0..0x3C0).map(move |i| page * 0x400 + i))
        .filter(|i| ciram[*i] != 0)
        .count();
    let attr_nonzero = (0..2)
        .flat_map(|page| (0..0x40).map(move |i| page * 0x400 + 0x3C0 + i))
        .filter(|i| ciram[*i] != 0)
        .count();
    let first = ciram
        .iter()
        .enumerate()
        .filter_map(|(i, value)| (*value != 0).then(|| format!("{i:03X}:{value:02X}")))
        .take(12)
        .collect::<Vec<_>>();
    let first = if first.is_empty() {
        "none".to_string()
    } else {
        first.join(" ")
    };
    format!(
        "nt_trace_ciram_{mode}=writes:{} tile_writes:{} attr_writes:{} tile_nonzero:{} attr_nonzero:{} first={first}",
        bus.nt_trace_ciram_writes,
        bus.nt_trace_ciram_tile_writes,
        bus.nt_trace_ciram_attr_writes,
        tile_nonzero,
        attr_nonzero
    )
}

fn format_nametable_column_occupancy(bus: &SmsBus) -> String {
    let mut cols = [0usize; 32];
    for row in 0..28 {
        for (col, count) in cols.iter_mut().enumerate() {
            let off = 0x3700 + (row * 32 + col) * 2;
            if bus.vram[off] != 0 || bus.vram[off + 1] != 0 {
                *count += 1;
            }
        }
    }
    cols.iter()
        .enumerate()
        .map(|(col, count)| format!("{col:02}:{count:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_nt_fold_collisions(bus: &SmsBus) -> String {
    let mut examples = Vec::new();
    let mut total = 0usize;

    for (cell, pages) in bus.nt_fold_cell_pages.iter().copied().enumerate() {
        if pages.count_ones() <= 1 {
            continue;
        }
        total += 1;
        if examples.len() < 6 {
            let row = cell / 32;
            let col = cell % 32;
            let pages = (0..4)
                .filter(|page| pages & (1u8 << *page) != 0)
                .map(|page| page.to_string())
                .collect::<Vec<_>>()
                .join(",");
            examples.push(format!("cell={row:02},{col:02} pages={pages}"));
        }
    }

    if examples.is_empty() {
        format!("nt_fold_collisions={total} first=none")
    } else {
        format!("nt_fold_collisions={total} first={}", examples.join(" "))
    }
}

fn format_nt_explicit_s_mismatches(bus: &SmsBus, vertical_mirroring: bool) -> String {
    let (mode, total, examples) = if vertical_mirroring {
        (
            "vertical",
            bus.nt_explicit_s_mismatch_vertical,
            &bus.nt_explicit_s_mismatch_vertical_examples,
        )
    } else {
        (
            "horizontal",
            bus.nt_explicit_s_mismatch_horizontal,
            &bus.nt_explicit_s_mismatch_horizontal_examples,
        )
    };

    if examples.is_empty() {
        format!("nt_explicit_s_mismatch_{mode}={total} first=none")
    } else {
        let examples = examples
            .iter()
            .map(|example| {
                format!(
                    "ppu=${:04X} sms=${:04X} folded={} explicit={} attr={:02X}:{:02X}",
                    example.ppu_addr,
                    example.sms_addr,
                    example.folded_s,
                    example.explicit_s,
                    example.attr_index,
                    example.attr_byte
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("nt_explicit_s_mismatch_{mode}={total} first={examples}")
    }
}

fn chr_nonzero_bytes(bus: &SmsBus) -> usize {
    (0..0x3700).filter(|i| bus.vram[*i] != 0).count()
}

fn active_sprite_count(bus: &SmsBus) -> usize {
    let mut count = 0;
    for i in 0..64 {
        if bus.vram[0x3F00 + i] == 0xD0 {
            break;
        }
        count += 1;
    }
    count
}

fn sat_terminator_index(bus: &SmsBus) -> Option<usize> {
    (0..64).find(|i| bus.vram[0x3F00 + i] == 0xD0)
}

fn sprite_base_addr(bus: &SmsBus) -> usize {
    if bus.vdp_regs[6] & 0x04 != 0 {
        0x2000
    } else {
        0x0000
    }
}

fn vram_nonzero_range(bus: &SmsBus, start: usize, len: usize) -> usize {
    bus.vram[start..start + len]
        .iter()
        .filter(|byte| **byte != 0)
        .count()
}

fn write_checkpoint_sat_diagnostics<W: std::io::Write>(
    f: &mut W,
    bus: &SmsBus,
) -> std::io::Result<()> {
    let active = active_sprite_count(bus);
    let terminator = sat_terminator_index(bus);
    let sprite_base = sprite_base_addr(bus);
    let sprite_8x16 = bus.vdp_regs[1] & 0x02 != 0;
    let blank167_addr = sprite_base + 167 * 32;
    let blank167_nonzero = vram_nonzero_range(bus, blank167_addr, 32);
    let tail_start = terminator.unwrap_or(64);
    let tail_y_not_d0 = (tail_start..64)
        .filter(|i| bus.vram[0x3F00 + i] != 0xD0)
        .count();
    let tail_xtile_nonzero = (tail_start..64)
        .filter(|i| bus.vram[0x3F80 + i * 2] != 0 || bus.vram[0x3F80 + i * 2 + 1] != 0)
        .count();

    writeln!(
        f,
        "sat: r1=${:02X} r6=${:02X} ppu_ctrl=${:02X} sprite_base=${:04X} sprite_mode={} terminator={} active={} scratch_next={} blank167_addr=${:04X} blank167_nonzero_bytes={}",
        bus.vdp_regs[1],
        bus.vdp_regs[6],
        bus.ram[0x0B08],
        sprite_base,
        if sprite_8x16 { "8x16" } else { "8x8" },
        terminator
            .map(|i| i.to_string())
            .unwrap_or_else(|| "none".to_string()),
        active,
        bus.ram[0x1460],
        blank167_addr,
        blank167_nonzero,
    )?;
    writeln!(
        f,
        "sat_tail: y_not_d0_after_terminator={} xtile_nonzero_after_terminator={}",
        tail_y_not_d0, tail_xtile_nonzero
    )?;

    for i in 0..active.min(24) {
        let y = bus.vram[0x3F00 + i];
        let x = bus.vram[0x3F80 + i * 2];
        let tile = bus.vram[0x3F80 + i * 2 + 1];
        let attr = bus.ram[0x1480 + i];
        let tile_addr = sprite_base + tile as usize * 32;
        let tile_nonzero = if tile_addr + 32 <= bus.vram.len() {
            vram_nonzero_range(bus, tile_addr, 32)
        } else {
            0
        };
        writeln!(
            f,
            "sat_entry[{i:02}]: y=${y:02X} screen_y={} x=${x:02X} tile=${tile:02X} attr=${attr:02X} tile_addr=${tile_addr:04X} tile_nonzero_bytes={} behind_bg={} hflip={} vflip={} pal={}",
            y.wrapping_add(1),
            tile_nonzero,
            attr & 0x20 != 0,
            attr & 0x40 != 0,
            attr & 0x80 != 0,
            attr & 0x03,
        )?;
    }

    Ok(())
}

/// Render the current VRAM/CRAM state to a 256x224 RGB PPM image
/// so we can verify what the SMS *would* show without needing mednafen.
/// Handles background nametable, scroll, and a coarse line-scroll split; sprites
/// are overlaid after the background pass.
fn dump_framebuffer_ppm(bus: &SmsBus, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    const W: usize = 256;
    const H: usize = 224;
    let mut pixels = vec![0u8; W * H * 3];
    let mut bg_opaque = vec![false; W * H];

    // SMS CRAM byte → RGB. Each entry: --BBGGRR (2 bits per channel, 0-3).
    let cram_to_rgb = |b: u8| -> (u8, u8, u8) {
        let r = (b & 0x03) as u32;
        let g = ((b >> 2) & 0x03) as u32;
        let bl = ((b >> 4) & 0x03) as u32;
        let scale = |c: u32| (c * 255 / 3) as u8;
        (scale(r), scale(g), scale(bl))
    };

    for screen_y in 0..H {
        // Split timing and R0 top-row horizontal lock are output-scanline
        // decisions. Pick the scroll registers for this displayed line first,
        // then sample the nametable through the inverse scroll transform.
        let (base_reg8, reg9) =
            if let Some((split_line, top_reg8, top_reg9)) = bus.render_scroll_split {
                if screen_y < split_line {
                    (top_reg8 as usize, top_reg9 as usize)
                } else {
                    (bus.vdp_regs[8] as usize, bus.vdp_regs[9] as usize)
                }
            } else {
                (bus.vdp_regs[8] as usize, bus.vdp_regs[9] as usize)
            };
        let lock_top = bus.vdp_regs[0] & 0x40 != 0;
        let reg8 = if lock_top && screen_y < 16 {
            0
        } else {
            base_reg8
        };
        let source_y = (screen_y + reg9) % H;
        let row = source_y / 8;
        let py = source_y % 8;

        for screen_x in 0..W {
            let source_x = (screen_x + W - (reg8 % W)) & 0xFF;
            let col = source_x / 8;
            let px = source_x % 8;
            let off = 0x3700 + (row * 32 + col) * 2;
            let lo = bus.vram[off];
            let hi = bus.vram[off + 1];
            let tile_index = ((hi as u16 & 1) << 8) | lo as u16;
            let palette_offset = if hi & 0x08 != 0 { 16 } else { 0 };
            let tile_addr = (tile_index as usize) * 32;
            if tile_addr + 32 > 0x4000 {
                continue;
            }
            let p0 = bus.vram[tile_addr + py * 4];
            let p1 = bus.vram[tile_addr + py * 4 + 1];
            let p2 = bus.vram[tile_addr + py * 4 + 2];
            let p3 = bus.vram[tile_addr + py * 4 + 3];
            let bit = 7 - px;
            let c = ((p0 >> bit) & 1)
                | (((p1 >> bit) & 1) << 1)
                | (((p2 >> bit) & 1) << 2)
                | (((p3 >> bit) & 1) << 3);
            bg_opaque[screen_y * W + screen_x] = c != 0;
            let color = bus.cram[palette_offset + c as usize];
            let (r, g, b) = cram_to_rgb(color);
            let pi = (screen_y * W + screen_x) * 3;
            pixels[pi] = r;
            pixels[pi + 1] = g;
            pixels[pi + 2] = b;
        }
    }
    eprintln!(
        "framebuffer scroll: reg8={} reg9={} split={:?} | NES scrollX($CB0C)={} cam_lo($071C)={} cam_pg($071A)={} playerX($0086)={} playerPg($006D)={} | bg_variant_pool_next($CA00)={}",
        bus.vdp_regs[8],
        bus.vdp_regs[9],
        bus.render_scroll_split,
        bus.ram[0x0B0C],
        bus.ram[0x071C],
        bus.ram[0x071A],
        bus.ram[0x0086],
        bus.ram[0x006D],
        bus.ram[0x0A00]
    );

    // ── Sprite layer overlay ──────────────────────────────────────────────
    // SAT layout in VRAM at $3F00:
    //   $3F00..$3F3F  64 Y positions (1 byte each). Y==$D0 hides remaining.
    //   $3F80..$3FFF  64 (X, tile_number) pairs (2 bytes each).
    // Sprites use the sprite palette at CRAM[16..32]. Color 0 = transparent.
    let mut sat_entries = Vec::new();
    for i in 0..64 {
        let y = bus.vram[0x3F00 + i];
        if y == 0xD0 {
            break;
        } // terminator: remaining sprites hidden
        sat_entries.push(i);
    }
    let mut active_sprites = 0;
    // Lower SAT/OAM indices have higher sprite priority. Draw later entries
    // first so earlier entries are composited last and remain visible.
    for i in sat_entries.into_iter().rev() {
        let y = bus.vram[0x3F00 + i];
        let x = bus.vram[0x3F80 + i * 2];
        let tile = bus.vram[0x3F80 + i * 2 + 1] as usize;
        let attr = bus.ram[0x1480 + i]; // runtime SAT_ATTRS = $D480
        // SMS sprite Y is the byte value, displayed one line below
        // (y == 0 means line 1). Skip if off-screen.
        let sy_top = y as usize + 1;
        if sy_top >= H {
            continue;
        }
        let sprite_base = if bus.vdp_regs[6] & 0x04 != 0 {
            0x2000
        } else {
            0x0000
        };
        let tile_addr = sprite_base + tile * 32;
        if tile_addr + 32 > 0x4000 {
            continue;
        }
        active_sprites += 1;
        for py in 0..8 {
            let p0 = bus.vram[tile_addr + py * 4];
            let p1 = bus.vram[tile_addr + py * 4 + 1];
            let p2 = bus.vram[tile_addr + py * 4 + 2];
            let p3 = bus.vram[tile_addr + py * 4 + 3];
            for px in 0..8 {
                let bit = 7 - px;
                let c = ((p0 >> bit) & 1)
                    | (((p1 >> bit) & 1) << 1)
                    | (((p2 >> bit) & 1) << 2)
                    | (((p3 >> bit) & 1) << 3);
                if c == 0 {
                    continue;
                } // transparent
                let color = bus.cram[16 + c as usize];
                let (r, g, b) = cram_to_rgb(color);
                let sx = x as usize + px;
                let sy = sy_top + py;
                if sx >= W || sy >= H {
                    continue;
                }
                let pi = sy * W + sx;
                if attr & 0x20 != 0 && bg_opaque[pi] {
                    continue;
                }
                let pi = pi * 3;
                pixels[pi] = r;
                pixels[pi + 1] = g;
                pixels[pi + 2] = b;
            }
        }
    }
    let _ = active_sprites; // kept for potential future logging

    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{W} {H}\n255\n")?;
    f.write_all(&pixels)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_button_event_and_active_low_buttons() {
        let (frame, port) = parse_button_event("80:right,a").unwrap();
        assert_eq!(frame, 80);
        assert_eq!(port & (1 << 3), 0);
        assert_eq!(port & (1 << 4), 0);
        assert_ne!(port & (1 << 5), 0);
    }

    #[test]
    fn parses_checkpoint_colon_or_equals() {
        let checkpoint = parse_checkpoint_spec("123:title-initial").unwrap();
        assert_eq!(checkpoint.frame, 123);
        assert_eq!(checkpoint.name, "title-initial");

        let checkpoint = parse_checkpoint_spec("456=1-1 initial").unwrap();
        assert_eq!(checkpoint.frame, 456);
        assert_eq!(checkpoint.name, "1-1 initial");
    }

    #[test]
    fn checkpoint_slug_is_filesystem_safe() {
        assert_eq!(checkpoint_slug("Title Initial"), "title_initial");
        assert_eq!(
            checkpoint_slug("1-1: flagpole / transition"),
            "1-1_flagpole_transition"
        );
        assert_eq!(checkpoint_slug("!!!"), "checkpoint");
    }

    #[test]
    fn loads_checkpoint_script_with_comments_and_blanks() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trace_sms_checkpoint_test_{}_{}.txt",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &path,
            "# comment\n\n80:title\n220:game start # trailing comment\n",
        )
        .unwrap();

        let checkpoints = load_checkpoint_script(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints[0].frame, 80);
        assert_eq!(checkpoints[0].name, "title");
        assert_eq!(checkpoints[1].frame, 220);
        assert_eq!(checkpoints[1].name, "game start");
    }

    #[test]
    fn nt_fold_collision_diagnostic_tracks_tile_pages_only() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_nt_fold_write(0x3700);

        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x00;
        bus.record_nt_fold_write(0x3701);

        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_nt_fold_write(0x3702);

        assert_eq!(bus.nt_fold_cell_pages[0], 0b0011);
        assert_eq!(bus.nt_fold_cell_pages[1], 0);
        assert_eq!(
            format_nt_fold_collisions(&bus),
            "nt_fold_collisions=1 first=cell=00,00 pages=0,1"
        );
    }

    #[test]
    fn explicit_s_mismatch_diagnostic_compares_attr_shadow() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        // Folded rendering would read $CC01 for SMS cell $3700 and use S=0.
        bus.ram[0x0C01] = 0;
        // Under horizontal mirroring, NES $2400 aliases physical CIRAM page 0,
        // whose first attribute byte selects S=2 for the top-left quadrant.
        bus.ram[0x0B80] = 0b0000_0010;
        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x00;

        bus.record_nt_fold_write(0x3700);

        assert_eq!(bus.nt_explicit_s_mismatch_horizontal, 1);
        assert_eq!(bus.nt_explicit_s_mismatch_vertical, 0);
        assert_eq!(
            format_nt_explicit_s_mismatches(&bus, false),
            "nt_explicit_s_mismatch_horizontal=1 first=ppu=$2400 sms=$3700 folded=0 explicit=2 attr=00:02"
        );
        assert_eq!(
            format_nt_explicit_s_mismatches(&bus, true),
            "nt_explicit_s_mismatch_vertical=0 first=none"
        );
    }

    #[test]
    fn nt_ciram_index_obeys_horizontal_and_vertical_mirroring() {
        assert_eq!(nt_ciram_index(0x2000, true), 0x000);
        assert_eq!(nt_ciram_index(0x2400, true), 0x400);
        assert_eq!(nt_ciram_index(0x2800, true), 0x000);
        assert_eq!(nt_ciram_index(0x2C00, true), 0x400);

        assert_eq!(nt_ciram_index(0x2000, false), 0x000);
        assert_eq!(nt_ciram_index(0x2400, false), 0x000);
        assert_eq!(nt_ciram_index(0x2800, false), 0x400);
        assert_eq!(nt_ciram_index(0x2C00, false), 0x400);
    }

    #[test]
    fn trace_ppu_write_call_reconstructs_raw_ciram_without_runtime_writes() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x12;
        bus.record_trace_ppu_write_call(7, 0xAB);

        assert_eq!(bus.nt_trace_ciram_vertical[0x412], 0xAB);
        assert_eq!(bus.nt_trace_ciram_horizontal[0x012], 0xAB);
        assert_eq!(bus.nt_trace_ciram_writes, 1);
        assert_eq!(bus.nt_trace_ciram_tile_writes, 1);
        assert_eq!(bus.nt_trace_ciram_attr_writes, 0);

        bus.ram[0x0B0F] = 0x27;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call(7, 0x55);
        bus.record_trace_ppu_write_call(6, 0xFF);

        assert_eq!(bus.nt_trace_ciram_vertical[0x7C0], 0x55);
        assert_eq!(bus.nt_trace_ciram_horizontal[0x3C0], 0x55);
        assert_eq!(bus.nt_trace_ciram_writes, 2);
        assert_eq!(bus.nt_trace_ciram_tile_writes, 1);
        assert_eq!(bus.nt_trace_ciram_attr_writes, 1);
        assert!(format_nt_trace_ciram_summary(&bus, false).contains("attr_nonzero:1"));
    }
}
