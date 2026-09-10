//! `frame-diff <smb.nes> <out.sms> [--frames N] [--script S]`
//!
//! Frame-level differential oracle. Runs the ORIGINAL SMB PRG on
//! `oracle_6502` (reference) and the generated SMS ROM on `z80_emu`
//! (subject), frame by frame, under an identical simplified
//! PPU/controller model, and reports the FIRST frame at which the
//! NES game-state RAM ($0000-$07FF) diverges.
//!
//! Both sides fire NMI/IRQ once per frame and rely on SMB's own NMI
//! handler to self-gate; the VBlank flag is set at frame start and
//! cleared on $2002 read. This is not cycle-accurate — it is a
//! deterministic game-logic model: if the translation is faithful,
//! the two RAM trajectories match frame for frame, and the first
//! divergence names the exact routine to fix next.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Shared input script
// ---------------------------------------------------------------------------

/// NES controller 1 button bitmask, in $4016 serial-read order:
/// bit0 A, bit1 B, bit2 Select, bit3 Start, bit4 Up, bit5 Down,
/// bit6 Left, bit7 Right.
#[derive(Clone, Copy, Default)]
struct Buttons(u8);

impl Buttons {
    const A: u8 = 1 << 0;
    const B: u8 = 1 << 1;
    const SELECT: u8 = 1 << 2;
    const START: u8 = 1 << 3;
    const UP: u8 = 1 << 4;
    const DOWN: u8 = 1 << 5;
    const LEFT: u8 = 1 << 6;
    const RIGHT: u8 = 1 << 7;
}

/// Maps a frame index to the held buttons. Deterministic.
fn script_buttons(frame: usize, script: &str) -> Buttons {
    match script {
        // Press Start on frames 40-44 (after the title has had time to
        // come up), release otherwise.
        "start" => {
            if (40..45).contains(&frame) {
                Buttons(Buttons::START)
            } else {
                Buttons(0)
            }
        }
        // Start, wait out the "WORLD 1-1" intermediate screen
        // (ScreenTimer is an interval timer, ~150 frames to expire), then
        // hold Right once GameCoreRoutine is actually running.
        "start_right" => {
            if (40..45).contains(&frame) {
                Buttons(Buttons::START)
            } else if frame >= 210 {
                Buttons(Buttons::RIGHT)
            } else {
                Buttons(0)
            }
        }
        // Hold Start every frame (debugging controller delivery).
        "start_hold" => Buttons(Buttons::START),
        // Press Start at frames 28-33 — early enough that the title is at
        // Task=03 (GameMenuRoutine) with DemoTimer>0, so BOTH the 6502
        // reference and the subject enter GameMode (before the demo
        // auto-plays). Used to diff gameplay sprite data apples-to-apples.
        "g" => {
            if (28..34).contains(&frame) {
                Buttons(Buttons::START)
            } else {
                Buttons(0)
            }
        }
        // Start, wait out the intermediate screen, then hold A (jump).
        "start_jump" => {
            if (40..45).contains(&frame) {
                Buttons(Buttons::START)
            } else if frame >= 210 {
                Buttons(Buttons::A)
            } else {
                Buttons(0)
            }
        }
        // Press Start within the matched window (frames 15-17), then
        // hold Right from frame 25, to enter GameMode before the
        // frame-22 demo divergence and test walking.
        "start_early_right" => {
            if (15..18).contains(&frame) {
                Buttons(Buttons::START)
            } else if frame >= 25 {
                Buttons(Buttons::RIGHT)
            } else {
                Buttons(0)
            }
        }
        _ => Buttons(0),
    }
}

#[derive(Clone)]
struct ButtonTimeline {
    builtin: String,
    events: Vec<(usize, u8)>, // frame -> raw SMS $DC active-low port value
    // frame -> NES controller byte (Buttons bits). When present the reference
    // uses these directly, bypassing the SMB-tuned SMS->NES title-mode
    // heuristic. Authored against the NES oracle for games (e.g. Adventure
    // Island) whose RAM map does not match the SMB oper_mode probe.
    nes_events: Option<Vec<(usize, u8)>>,
}

impl ButtonTimeline {
    fn builtin(name: String) -> Self {
        Self {
            builtin: name,
            events: Vec::new(),
            nes_events: None,
        }
    }

    fn from_events(events: Vec<(usize, u8)>) -> Self {
        Self {
            builtin: "buttons-script".to_string(),
            events,
            nes_events: None,
        }
    }

    fn from_nes_events(events: Vec<(usize, u8)>) -> Self {
        Self {
            builtin: "nes-buttons-script".to_string(),
            events: Vec::new(),
            nes_events: Some(events),
        }
    }

    /// The raw NES controller byte at `frame`, if this timeline was authored
    /// in NES-button terms. Events hold until the next one; neutral before.
    fn nes_at(&self, frame: usize) -> Option<u8> {
        let events = self.nes_events.as_ref()?;
        let idx = events.partition_point(|(event_frame, _)| *event_frame <= frame);
        Some(if idx == 0 { 0 } else { events[idx - 1].1 })
    }

    fn sms_dc_at(&self, frame: usize) -> u8 {
        if self.events.is_empty() {
            return nes_buttons_to_sms_dc(script_buttons(frame, &self.builtin));
        }
        let idx = self
            .events
            .partition_point(|(event_frame, _)| *event_frame <= frame);
        if idx == 0 {
            0xFF
        } else {
            self.events[idx - 1].1
        }
    }
}

fn buttons_to_sms_port_dc(spec: &str) -> Result<u8, String> {
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
            other => return Err(format!("unknown button entry: {other}")),
        };
        port &= !(1 << bit);
    }
    Ok(port)
}

fn parse_button_event(spec: &str) -> Result<(usize, u8), String> {
    let (frame, buttons) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:buttons, got {spec}"))?;
    let frame = frame
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid frame in button event: {frame}"))?;
    Ok((frame, buttons_to_sms_port_dc(buttons)?))
}

fn load_button_script(path: &str) -> Result<Vec<(usize, u8)>, String> {
    load_script_with(path, parse_button_event)
}

/// NES controller byte from names, held until the next event. Directly a
/// Buttons bitmask, with no SMS $DC round-trip or title-mode remapping.
fn buttons_to_nes(spec: &str) -> Result<u8, String> {
    let mut nes = 0u8;
    for raw in spec.split(',') {
        let name = raw.trim().to_ascii_lowercase();
        if name.is_empty() {
            continue;
        }
        nes |= match name.as_str() {
            "a" => Buttons::A,
            "b" => Buttons::B,
            "select" => Buttons::SELECT,
            "start" => Buttons::START,
            "up" => Buttons::UP,
            "down" => Buttons::DOWN,
            "left" => Buttons::LEFT,
            "right" => Buttons::RIGHT,
            other => return Err(format!("unknown NES button entry: {other}")),
        };
    }
    Ok(nes)
}

fn parse_nes_button_event(spec: &str) -> Result<(usize, u8), String> {
    let (frame, buttons) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:buttons, got {spec}"))?;
    let frame = frame
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid frame in NES button event: {frame}"))?;
    Ok((frame, buttons_to_nes(buttons)?))
}

fn load_nes_button_script(path: &str) -> Result<Vec<(usize, u8)>, String> {
    load_script_with(path, parse_nes_button_event)
}

fn load_script_with(
    path: &str,
    parse: impl Fn(&str) -> Result<(usize, u8), String>,
) -> Result<Vec<(usize, u8)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read button script {path}: {err}"))?;
    let mut events = Vec::new();
    for (line_idx, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        events.push(
            parse(line)
                .map_err(|err| format!("invalid button script {path}:{}: {err}", line_idx + 1))?,
        );
    }
    events.sort_by_key(|(frame, _)| *frame);
    Ok(events)
}

// ---------------------------------------------------------------------------
// Reference: NES system bus over oracle_6502
// ---------------------------------------------------------------------------

/// Stateful cartridge mappers routed ahead of the stateless MapperPolicy in
/// the reference bus. Add a variant per new mapper; the bus only needs PRG
/// mapping, the register-write port, and whether PRG-RAM at $6000 is enabled.
enum StatefulMapper {
    Mmc1(nes_rom::mmc1::Mmc1),
    Axrom(nes_rom::axrom::Axrom),
    Vrc2(nes_rom::vrc2::Vrc2),
    Fme7(nes_rom::fme7::Fme7),
    Mmc2(nes_rom::mmc2::Mmc2),
}

impl StatefulMapper {
    fn cpu_to_prg_offset(&self, addr: u16) -> Option<usize> {
        match self {
            Self::Mmc1(m) => m.cpu_to_prg_offset(addr),
            Self::Axrom(a) => a.cpu_to_prg_offset(addr),
            Self::Vrc2(v) => v.cpu_to_prg_offset(addr),
            Self::Fme7(f) => f.cpu_to_prg_offset(addr),
            Self::Mmc2(m) => m.cpu_to_prg_offset(addr),
        }
    }
    fn write_register(&mut self, addr: u16, value: u8) {
        match self {
            Self::Mmc1(m) => m.write_register(addr, value),
            Self::Axrom(a) => a.write_register(addr, value),
            Self::Vrc2(v) => v.write_register(addr, value),
            Self::Fme7(f) => f.write_register(addr, value),
            Self::Mmc2(m) => m.write_register(addr, value),
        }
    }
    fn prg_ram_enabled(&self) -> bool {
        match self {
            Self::Mmc1(m) => m.prg_ram_enabled(),
            Self::Axrom(_) => false,
            Self::Vrc2(_) => false,
            Self::Fme7(f) => f.prg6000_is_ram(),
            Self::Mmc2(m) => m.prg_ram_enabled(),
        }
    }
    /// Advance the mapper's scanline IRQ one step; true when it asserts IRQ.
    fn vrc_irq_scanline(&mut self) -> bool {
        match self {
            Self::Vrc2(v) => v.vrc_irq_scanline(),
            Self::Fme7(f) => f.irq_scanline(),
            _ => false,
        }
    }
    /// A mapper that answers its own $6000-$7FFF reads (e.g. VRC2's one-bit
    /// microwire latch) returns Some; None falls back to the WRAM array.
    fn wram_read(&self) -> Option<u8> {
        match self {
            Self::Vrc2(v) => Some(v.microwire_read()),
            _ => None,
        }
    }
    /// Returns true when the mapper consumed a $6000-$7FFF write itself.
    fn wram_write(&mut self, value: u8) -> bool {
        match self {
            Self::Vrc2(v) => {
                v.microwire_write(value);
                true
            }
            _ => false,
        }
    }
    /// Flat CHR-ROM offset for a PPU pattern address, when the mapper banks
    /// CHR in 1 KiB windows (VRC2/4). None means "use the default banking".
    fn chr_1k_offset(&self, ppu_addr: u16) -> Option<usize> {
        match self {
            Self::Vrc2(v) => {
                let window = ((ppu_addr >> 10) & 7) as u8;
                Some(v.chr_bank_1k(window) as usize * 1024 + (ppu_addr as usize & 0x3FF))
            }
            Self::Fme7(f) => {
                let window = ((ppu_addr >> 10) & 7) as u8;
                Some(f.chr_bank_1k(window) as usize * 1024 + (ppu_addr as usize & 0x3FF))
            }
            Self::Mmc2(m) => Some(m.chr_offset(ppu_addr)),
            _ => None,
        }
    }
}

struct NesBus {
    ram: [u8; 0x800],
    prg: Vec<u8>, // 32 KiB mapped at $8000-$FFFF
    // PPU model
    vblank: bool,
    addr_latch_toggle: bool,
    nmi_enabled: bool, // $2000 bit 7
    ppu_ctrl: u8,      // $2000 (bit 2 = VRAM address increment 1/32)
    ppu_mask: u8,      // $2001 (rendering-enable bits 3/4)
    // $2006/$2007 VRAM access: SMB's DrawTitleScreen reads its title
    // layout from CHR ROM through buffered PPUDATA reads, so the
    // reference must model the address latch, the 1-byte read buffer,
    // and the post-access increment.
    chr: Vec<u8>,
    ppu_addr: u16,
    ppu_addr_hi_latch: u8,
    ppu_read_buffer: u8,
    // Synthetic sprite-0 hit phase, mirroring the subject runtime's $CB12:
    // 0 = before hit (first poll while rendering returns bit6=0 and arms),
    // 1 = hit reached (subsequent polls return bit6=1). Reset each frame.
    sprite0_phase: u8,
    // Minimal APU model: register shadow + length counters, enough for
    // $4015 status reads (SMB's sound engine arbitrates SFX with them).
    // Mirrors the subject runtime's shim semantics (runtime/apu_stub.s).
    apu_regs: [u8; 0x18],
    apu_len: [u8; 4], // pulse1, pulse2, triangle, noise
    // Controller
    strobe: bool,
    ctrl_shift: u8,
    buttons: u8,
    joy_reads: u64,
    joy_dbg: u32,
    watch: Option<Vec<u16>>,
    watch_log: Vec<(u16, u8, u16)>,
    watch_bank_log: Vec<u8>,
    last_pc: u16,
    current_frame: Option<usize>,
    ppu_log_values: Vec<u8>,
    ppu_log_limit: usize,
    ppu_log_count: usize,
    /// UxROM: selected 16 KiB bank at $8000-$BFFF.
    prg_bank: u8,
    chr_bank: u8,
    mapper_policy: nes_rom::MapperPolicy,
    /// Stateful mapper (MMC1/AxROM/...), routed ahead of `mapper_policy`.
    stateful: Option<StatefulMapper>,
    /// 8 KiB PRG-RAM at $6000-$7FFF (battery-backed saves on MMC1 boards).
    prg_ram: Vec<u8>,
    /// CHR-RAM store for pattern-space $2007 writes (ground truth).
    chr_ram: Vec<u8>,
    /// PPU palette RAM ($3F00-$3F1F) captured from $2007 writes, so the
    /// reference can render ground-truth frames (FD_NES_DUMP).
    palette_ram: [u8; 32],
    /// Last $2005 first-write (X scroll) seen; SMB writes the playfield
    /// scroll after the sprite-0 hit, so at frame end this holds the
    /// playfield X of the frame.
    scroll_x_last: u8,
}

impl NesBus {
    /// Two half-frame length-counter ticks per video frame, mirroring the
    /// subject runtime's apu_frame_tick approximation.
    fn apu_frame_tick(&mut self) {
        let halts = [
            self.apu_regs[0x00] & 0x20 != 0,
            self.apu_regs[0x04] & 0x20 != 0,
            self.apu_regs[0x08] & 0x80 != 0,
            self.apu_regs[0x0C] & 0x20 != 0,
        ];
        for _ in 0..2 {
            for ch in 0..4 {
                if !halts[ch] && self.apu_len[ch] > 0 {
                    self.apu_len[ch] -= 1;
                }
            }
        }
    }

    fn new(
        prg: Vec<u8>,
        chr: Vec<u8>,
        mapper_policy: nes_rom::MapperPolicy,
        stateful: Option<StatefulMapper>,
    ) -> Self {
        Self {
            ram: [0; 0x800],
            prg,
            vblank: false,
            addr_latch_toggle: false,
            nmi_enabled: false,
            ppu_ctrl: 0,
            ppu_mask: 0,
            chr,
            ppu_addr: 0,
            ppu_addr_hi_latch: 0,
            ppu_read_buffer: 0,
            sprite0_phase: 0,
            apu_regs: [0; 0x18],
            apu_len: [0; 4],
            strobe: false,
            ctrl_shift: 0,
            buttons: 0,
            joy_reads: 0,
            joy_dbg: 0,
            watch: None,
            watch_log: Vec::new(),
            watch_bank_log: Vec::new(),
            last_pc: 0,
            current_frame: None,
            ppu_log_values: std::env::var("FD_LOG_PPU_VALUES")
                .ok()
                .map(|spec| {
                    spec.split(',')
                        .filter_map(|raw| {
                            u8::from_str_radix(
                                raw.trim().trim_start_matches("0x").trim_start_matches('$'),
                                16,
                            )
                            .ok()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            ppu_log_limit: std::env::var("FD_LOG_PPU_LIMIT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(200),
            ppu_log_count: 0,
            prg_bank: 0,
            stateful,
            prg_ram: vec![0u8; nes_rom::mmc1::PRG_RAM_SIZE],
            chr_bank: 0,
            mapper_policy,
            chr_ram: vec![0u8; 0x3000],
            palette_ram: [0u8; 32],
            scroll_x_last: 0,
        }
    }

    fn prg_read(&self, addr: u16) -> u8 {
        if let Some(m) = &self.stateful {
            return self.prg[m.cpu_to_prg_offset(addr).expect("PRG read address")];
        }
        let off = self
            .mapper_policy
            .cpu_to_prg_offset(addr, self.prg_bank)
            .expect("valid reference mapper bank")
            .expect("PRG read address");
        self.prg[off]
    }
}

impl oracle_6502::Bus for NesBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize],
            0x2000..=0x3FFF => {
                match 0x2000 + (addr & 7) {
                    0x2002 => {
                        let mut v = 0u8;
                        if self.vblank {
                            v |= 0x80;
                        }
                        // Synthetic sprite-0 hit (bit 6), mirroring the subject
                        // runtime's $CB12 handshake (runtime/ppu.s): only while
                        // rendering is enabled (PPUMASK bits 3/4); the first poll
                        // arms the phase and returns 0, later polls return 1. SMB's
                        // NMI waits for bit6 to clear then set — without this the
                        // reference NMI spins forever and never runs the engine.
                        // Sprite-0 hit synthesis, generalized past SMB: a
                        // read while VBlank is set (frame start) CLEARS the
                        // hit (phase 0), because the real hit clears at the
                        // pre-render line. Only reads after VBlank has been
                        // cleared, with rendering on, arm then report the hit.
                        // This satisfies both SMB's clear-then-set NMI wait
                        // and Zelda's wait-for-clear init loop ($E4D4).
                        if self.vblank {
                            self.sprite0_phase = 0;
                        } else if self.ppu_mask & 0x18 != 0 {
                            if self.sprite0_phase == 0 {
                                self.sprite0_phase = 1;
                            } else {
                                v |= 0x40;
                            }
                        }
                        // Reading $2002 clears VBlank + resets the $2005/$2006 toggle.
                        self.vblank = false;
                        self.addr_latch_toggle = false;
                        v
                    }
                    0x2007 => {
                        // Buffered PPUDATA read: returns the buffer, then
                        // refills it from the current VRAM address. CHR
                        // ROM ($0000-$1FFF) is the only backing store the
                        // reference models; nametable reads return 0.
                        let ret = self.ppu_read_buffer;
                        let a = (self.ppu_addr & 0x3FFF) as usize;
                        self.ppu_read_buffer = if a < 0x2000 && !self.chr.is_empty() {
                            // Apply the mapper's CHR banking so $2007 reads (used
                            // by games like Batman to copy CHR-ROM into WRAM)
                            // return the currently-banked byte, not flat CHR.
                            let off = self
                                .stateful
                                .as_ref()
                                .and_then(|m| m.chr_1k_offset(a as u16))
                                .unwrap_or((self.chr_bank as usize) * 0x2000 + a);
                            self.chr[off % self.chr.len()]
                        } else if a < 0x2000 {
                            self.chr_ram[a]
                        } else {
                            0
                        };
                        let inc = if self.ppu_ctrl & 0x04 != 0 { 32 } else { 1 };
                        self.ppu_addr = self.ppu_addr.wrapping_add(inc);
                        ret
                    }
                    // $2004 OAM data read: not needed by SMB game logic.
                    _ => 0,
                }
            }
            0x4016 => {
                // Controller 1 serial read: bit0 = next button bit.
                self.joy_reads += 1;
                if self.buttons != 0 && self.joy_dbg < 24 {
                    self.joy_dbg += 1;
                    eprintln!(
                        "    [ref $4016 read] buttons=${:02X} strobe={} shift=${:02X} -> bit {}",
                        self.buttons,
                        self.strobe as u8,
                        self.ctrl_shift,
                        self.ctrl_shift & 1
                    );
                }
                let bit = self.ctrl_shift & 1;
                if !self.strobe {
                    self.ctrl_shift >>= 1;
                    self.ctrl_shift |= 0x80; // after 8 reads, returns 1s
                }
                0x40 | bit
            }
            0x4015 => {
                let mut v = 0u8;
                for ch in 0..4 {
                    if self.apu_len[ch] > 0 {
                        v |= 1 << ch;
                    }
                }
                v
            }
            0x4017 => 0x40, // controller 2: nothing pressed
            0x6000..=0x7FFF => {
                if let Some(off) = self
                    .stateful
                    .as_ref()
                    .and_then(|m| m.cpu_to_prg_offset(addr))
                {
                    self.prg[off]
                } else if let Some(v) = self.stateful.as_ref().and_then(|m| m.wram_read()) {
                    v
                } else if self.stateful.as_ref().is_some_and(|m| m.prg_ram_enabled()) {
                    self.prg_ram[(addr - 0x6000) as usize]
                } else {
                    0
                }
            }
            0x8000..=0xFFFF => self.prg_read(addr),
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => {
                let nes = addr & 0x07FF;
                self.ram[nes as usize] = value;
                if let Some(w) = &self.watch {
                    if w.contains(&nes) {
                        // Encode the current PRG bank in the high byte of a
                        // third slot? Keep tuple shape: fold bank into pc's
                        // unused range only for logging via eprintln at dump
                        // time — instead store bank in value's spare... no:
                        // simplest is a parallel log.
                        self.watch_log.push((nes, value, self.last_pc));
                        self.watch_bank_log.push(self.prg_bank);
                    }
                }
            }
            0x2000..=0x3FFF => {
                // PPU register writes: model only what affects the
                // $2005/$2006 write toggle and the NMI-enable bit; the
                // rest are side-effect-free for game-state RAM evolution.
                let reg = 0x2000 + (addr & 7);
                if reg == 0x2000 {
                    self.nmi_enabled = value & 0x80 != 0;
                    self.ppu_ctrl = value;
                }
                if reg == 0x2001 {
                    self.ppu_mask = value; // rendering-enable bits for sprite-0 synth
                }
                if reg == 0x2006 {
                    if !self.addr_latch_toggle {
                        self.ppu_addr_hi_latch = value;
                    } else {
                        self.ppu_addr = ((self.ppu_addr_hi_latch as u16) << 8) | value as u16;
                    }
                }
                if reg == 0x2007 {
                    // CHR-RAM model: store pattern-space writes so the
                    // subject's uploaded tiles can be compared against
                    // ground truth (FD_DUMP_CHRRAM).
                    let a = self.ppu_addr & 0x3FFF;
                    if (0x2000..0x3000).contains(&a)
                        && self.ppu_log_values.contains(&value)
                        && self.ppu_log_count < self.ppu_log_limit
                    {
                        eprintln!(
                            "REF_PPU_WRITE frame={} bank={} pc=${:04X} addr=${a:04X} value=${value:02X} ctrl=${:02X}",
                            self.current_frame
                                .map_or_else(|| "pre".to_string(), |frame| frame.to_string()),
                            self.prg_bank,
                            self.last_pc,
                            self.ppu_ctrl,
                        );
                        self.ppu_log_count += 1;
                    }
                    if (a as usize) < self.chr_ram.len() {
                        self.chr_ram[a as usize] = value;
                    }
                    if (0x3F00..0x4000).contains(&a) {
                        let mut p = (a & 0x1F) as usize;
                        // $3F10/$14/$18/$1C are hardware mirrors of
                        // $3F00/04/08/0C (SMB parks the sky color at $3F10).
                        if p & 0x13 == 0x10 {
                            p &= !0x10;
                        }
                        self.palette_ram[p] = value;
                    }
                    // Writes advance the VRAM address like reads do.
                    let inc = if self.ppu_ctrl & 0x04 != 0 { 32 } else { 1 };
                    self.ppu_addr = self.ppu_addr.wrapping_add(inc);
                }
                if reg == 0x2005 && !self.addr_latch_toggle {
                    self.scroll_x_last = value;
                }
                if reg == 0x2005 || reg == 0x2006 {
                    self.addr_latch_toggle = !self.addr_latch_toggle;
                }
            }
            0x4000..=0x4013 | 0x4015 | 0x4017 => {
                const LEN_TABLE: [u8; 32] = [
                    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18,
                    48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30,
                ];
                let idx = (addr - 0x4000) as usize;
                self.apu_regs[idx] = value;
                let enabled = self.apu_regs[0x15];
                match idx {
                    0x03 if enabled & 1 != 0 => self.apu_len[0] = LEN_TABLE[(value >> 3) as usize],
                    0x07 if enabled & 2 != 0 => self.apu_len[1] = LEN_TABLE[(value >> 3) as usize],
                    0x0B if enabled & 4 != 0 => self.apu_len[2] = LEN_TABLE[(value >> 3) as usize],
                    0x0F if enabled & 8 != 0 => self.apu_len[3] = LEN_TABLE[(value >> 3) as usize],
                    0x15 => {
                        for ch in 0..4 {
                            if value & (1 << ch) == 0 {
                                self.apu_len[ch] = 0;
                            }
                        }
                    }
                    _ => {}
                }
            }
            0x4014 => {
                // OAM DMA: copies page (value<<8) to OAM. No effect on
                // $0000-$07FF game RAM, so skip for the comparison.
            }
            0x4016 => {
                let new_strobe = value & 1 != 0;
                // On strobe high→low transition, latch buttons.
                if self.strobe && !new_strobe {
                    self.ctrl_shift = self.buttons;
                }
                if new_strobe {
                    self.ctrl_shift = self.buttons;
                }
                self.strobe = new_strobe;
            }
            0x6000..=0x7FFF => {
                let consumed = self
                    .stateful
                    .as_mut()
                    .map(|m| m.wram_write(value))
                    .unwrap_or(false);
                if !consumed && self.stateful.as_ref().is_some_and(|m| m.prg_ram_enabled()) {
                    self.prg_ram[(addr - 0x6000) as usize] = value;
                }
            }
            0x8000..=0xFFFF => {
                if let Some(m) = &mut self.stateful {
                    m.write_register(addr, value);
                }
                if self.mapper_policy.is_banked() {
                    let bus_byte = self.prg_read(addr);
                    self.prg_bank = self
                        .mapper_policy
                        .selected_bank_from_write(value, bus_byte)
                        .expect("valid mapper write bank");
                }
                // CNROM: the write selects the 8 KiB CHR bank; PRG is fixed.
                let bus_byte = self.prg_read(addr);
                if let Some(bank) = self
                    .mapper_policy
                    .cnrom_chr_bank_from_write(value, bus_byte)
                {
                    self.chr_bank = bank;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod mapper_tests {
    use super::*;

    fn uxrom(conflicts: nes_rom::UxromBusConflicts) -> nes_rom::MapperPolicy {
        nes_rom::MapperPolicy::Uxrom {
            bank_count: 2,
            bus_conflicts: conflicts,
        }
    }

    #[test]
    fn mapper_conflict_uses_current_lower_or_fixed_upper_rom_byte() {
        let mut prg = vec![0u8; 2 * nes_rom::PRG_BANK_SIZE];
        prg[0] = 1;
        prg[nes_rom::PRG_BANK_SIZE] = 1;
        let mut bus = NesBus::new(prg, vec![], uxrom(nes_rom::UxromBusConflicts::And), None);

        oracle_6502::Bus::write(&mut bus, 0x8000, 3);
        assert_eq!(bus.prg_bank, 1);
        bus.prg_bank = 0;
        oracle_6502::Bus::write(&mut bus, 0xc000, 3);
        assert_eq!(bus.prg_bank, 1);
    }

    #[test]
    fn mapper_without_conflicts_uses_raw_write() {
        let mut bus = NesBus::new(
            vec![0; 2 * nes_rom::PRG_BANK_SIZE],
            vec![],
            uxrom(nes_rom::UxromBusConflicts::None),
            None,
        );
        oracle_6502::Bus::write(&mut bus, 0x8000, 1);
        assert_eq!(bus.prg_bank, 1);
    }
}

// ---------------------------------------------------------------------------
// Reference frame stepper
// ---------------------------------------------------------------------------

const REF_INSN_PER_FRAME: usize = 200_000;
const REF_PREROLL_CAP: usize = 2_000_000;

/// Returns (init_snapshot, per_frame_snapshots). The init snapshot is
/// RAM at the moment SMB first enables NMI ($2000 bit 7) — i.e. when
/// Canonical NTSC NES master palette (Nestopia/Blargg), $00-$3F.
const NES_PALETTE: [u32; 64] = [
    0x545454, 0x001E74, 0x081090, 0x300088, 0x440064, 0x5C0030, 0x540400, 0x3C1800, 0x202A00,
    0x083A00, 0x004000, 0x003C00, 0x00323C, 0x000000, 0x000000, 0x000000, 0x989698, 0x084CC4,
    0x3032EC, 0x5C1EE4, 0x8814B0, 0xA01464, 0x982220, 0x783C00, 0x545A00, 0x287200, 0x087C00,
    0x007628, 0x006678, 0x000000, 0x000000, 0x000000, 0xECEEEC, 0x4C9AEC, 0x787CEC, 0xB062EC,
    0xE454EC, 0xEC58B4, 0xEC6A64, 0xD48820, 0xA0AA00, 0x74C400, 0x4CD020, 0x38CC6C, 0x38B4CC,
    0x3C3C3C, 0x000000, 0x000000, 0xECEEEC, 0xA8CCEC, 0xBCBCEC, 0xD4B2EC, 0xECAEEC, 0xECAED4,
    0xECB4B0, 0xE4C490, 0xCCD278, 0xB4DE78, 0xA8E290, 0x98E2B4, 0xA0D6E4, 0xA0A2A0, 0x000000,
    0x000000,
];

/// Render the NES reference's current display state to a PPM: background from
/// the captured nametable/attribute shadow (vertical mirroring), sprites from
/// the OAM page ($0200), colors from captured palette RAM. Approximates SMB's
/// sprite-0 split: rows above y=32 render unscrolled (status bar), the rest
/// with the frame's last $2005 X write plus the $2000 nametable-select bit —
/// the same presentation model the SMS runtime implements.
fn render_nes_ppm(bus: &NesBus, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    const W: usize = 256;
    const H: usize = 240;
    let bg_pattern = if bus.ppu_ctrl & 0x10 != 0 { 0x1000 } else { 0 };
    let spr_pattern = if bus.ppu_ctrl & 0x08 != 0 { 0x1000 } else { 0 };
    let chr = |addr: usize| -> u8 {
        // Pattern reads come from CHR ROM when present, else the CHR-RAM shadow.
        if !bus.chr.is_empty() {
            if let Some(off) = bus
                .stateful
                .as_ref()
                .and_then(|m| m.chr_1k_offset((addr & 0x1FFF) as u16))
            {
                return bus.chr[off % bus.chr.len()];
            }
            let base = (bus.chr_bank as usize) * 0x2000;
            bus.chr[(base + (addr & 0x1FFF)) % bus.chr.len()]
        } else {
            bus.chr_ram[addr & 0x1FFF]
        }
    };
    let nt = |addr: usize| -> u8 { bus.chr_ram[0x2000 + (addr & 0x7FF)] };
    let color = |idx: u8| -> (u8, u8, u8) {
        let rgb = NES_PALETTE[(idx & 0x3F) as usize];
        ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    };
    let scroll9 = bus.scroll_x_last as usize + (((bus.ppu_ctrl & 1) as usize) << 8);

    let bg_show = bus.ppu_mask & 0x08 != 0;
    let spr_show = bus.ppu_mask & 0x10 != 0;
    let mut pix = vec![0u8; W * H * 3];
    let mut bg_opaque = vec![false; W * H];
    if !bg_show {
        // Rendering disabled: the PPU shows the universal background color.
        let (r, g, b) = color(bus.palette_ram[0]);
        for o in (0..pix.len()).step_by(3) {
            pix[o] = r;
            pix[o + 1] = g;
            pix[o + 2] = b;
        }
    }
    for y in 0..if bg_show { H } else { 0 } {
        let scroll = if y < 32 { 0 } else { scroll9 };
        for x in 0..W {
            let sx = (x + scroll) & 0x1FF;
            let page = sx >> 8;
            let fx = sx & 0xFF;
            let (col, row) = (fx / 8, y / 8);
            let nt_off = page * 0x400 + row * 32 + col;
            let tile = nt(nt_off) as usize;
            let attr = nt(page * 0x400 + 0x3C0 + (row / 4) * 8 + col / 4);
            let quad = ((row & 2) | ((col & 2) >> 1)) as u8;
            let pal = (attr >> (quad * 2)) & 3;
            let (py, px) = (y % 8, fx % 8);
            let lo = chr(bg_pattern + tile * 16 + py);
            let hi = chr(bg_pattern + tile * 16 + 8 + py);
            let bit = 7 - px;
            let v = ((lo >> bit) & 1) | (((hi >> bit) & 1) << 1);
            let c = if v == 0 {
                bus.palette_ram[0]
            } else {
                bus.palette_ram[(pal * 4 + v) as usize]
            };
            let (r, g, b) = color(c);
            let o = (y * W + x) * 3;
            pix[o] = r;
            pix[o + 1] = g;
            pix[o + 2] = b;
            bg_opaque[y * W + x] = v != 0;
        }
    }
    // Sprites, back to front so lower OAM indices win overlaps.
    for i in (0..if spr_show { 64 } else { 0 }).rev() {
        let o = 0x200 + i * 4;
        let sy = bus.ram[o] as usize;
        if sy >= 0xEF {
            continue;
        }
        let tile = bus.ram[o + 1] as usize;
        let attr = bus.ram[o + 2];
        let sx = bus.ram[o + 3] as usize;
        let behind = attr & 0x20 != 0;
        let pal = 0x10 + ((attr & 3) as usize) * 4;
        for py in 0..8usize {
            let ty = if attr & 0x80 != 0 { 7 - py } else { py };
            let lo = chr(spr_pattern + tile * 16 + ty);
            let hi = chr(spr_pattern + tile * 16 + 8 + ty);
            let y = sy + 1 + py;
            if y >= H {
                continue;
            }
            for px in 0..8usize {
                let tx = if attr & 0x40 != 0 { px } else { 7 - px };
                let v = ((lo >> tx) & 1) | (((hi >> tx) & 1) << 1);
                if v == 0 {
                    continue;
                }
                let x = sx + px;
                if x >= W {
                    continue;
                }
                if behind && bg_opaque[y * W + x] {
                    continue;
                }
                let (r, g, b) = color(bus.palette_ram[pal + v as usize]);
                let d = (y * W + x) * 3;
                pix[d] = r;
                pix[d + 1] = g;
                pix[d + 2] = b;
            }
        }
    }
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "P6\n{W} {H}\n255")?;
    f.write_all(&pix)
}

/// reset-init is essentially done and the game wants frames. From
/// there each frame fires one NMI.
fn run_reference(
    prg: Vec<u8>,
    chr: Vec<u8>,
    mapper_policy: nes_rom::MapperPolicy,
    stateful: Option<StatefulMapper>,
    frames: usize,
    timeline: &ButtonTimeline,
) -> ([u8; 0x800], Vec<[u8; 0x800]>) {
    use oracle_6502::Cpu;
    let mut cpu = Cpu::new();
    let mut bus = NesBus::new(prg, chr, mapper_policy, stateful);
    cpu.reset(&mut bus);

    // Pre-roll: run reset-init until NMI is enabled. SMB polls $2002 for
    // VBlank during this phase, so keep VBlank available.
    bus.vblank = true;
    let mut pre = 0usize;
    while !bus.nmi_enabled && pre < REF_PREROLL_CAP {
        bus.last_pc = cpu.pc;
        if cpu.step(&mut bus).is_err() {
            break;
        }
        bus.vblank = true; // keep VBlank pollable during init
        pre += 1;
    }
    // FD_ANCHOR_RENDER=1 (mapper plan M1): games that enable NMI early
    // and keep initializing (CV1) can't be frame-aligned at NMI-enable.
    // Anchor instead on rendering-enabled (PPUMASK bg+sprites, bits 3+4):
    // run whole frames (NMI + frame budget) until the mask bit sets on
    // both sides, then compare from that common visual milestone.
    // FD_ANCHOR=addr:val — generic semantic anchor: run whole frames on
    // both sides until NES RAM[addr] == val, then compare from there.
    let sem_anchor: Option<(usize, u8)> = std::env::var("FD_ANCHOR").ok().and_then(|s| {
        let (a, v) = s.split_once(':')?;
        Some((
            usize::from_str_radix(a, 16).ok()?,
            u8::from_str_radix(v, 16).ok()?,
        ))
    });
    if let Some((aa, av)) = sem_anchor {
        let mut aframes = 0usize;
        while bus.ram[aa] != av && aframes < 1800 {
            bus.vblank = true;
            bus.sprite0_phase = 0;
            if bus.nmi_enabled {
                cpu.nmi(&mut bus);
            }
            for _ in 0..REF_INSN_PER_FRAME {
                bus.last_pc = cpu.pc;
                if cpu.step(&mut bus).is_err() {
                    break;
                }
            }
            bus.apu_frame_tick();
            aframes += 1;
        }
        eprintln!(
            "  ref sem-anchor: {aframes} frames, ram[${aa:04X}]=${:02X}",
            bus.ram[aa]
        );
    }
    if std::env::var("FD_ANCHOR_RENDER").is_ok() {
        let mut aframes = 0usize;
        while bus.ppu_mask & 0x18 != 0x18 && aframes < 900 {
            bus.vblank = true;
            bus.sprite0_phase = 0;
            if bus.nmi_enabled {
                cpu.nmi(&mut bus);
            }
            for _ in 0..REF_INSN_PER_FRAME {
                bus.last_pc = cpu.pc;
                if cpu.step(&mut bus).is_err() {
                    break;
                }
                if bus.ppu_mask & 0x18 == 0x18 {
                    break;
                }
            }
            bus.apu_frame_tick();
            aframes += 1;
        }
        eprintln!(
            "  ref render-anchor: {aframes} frames, ppu_mask=${:02X}",
            bus.ppu_mask
        );
    }
    let init_snap = bus.ram;
    if std::env::var("FD_DUMP_RAMCODE").is_ok() {
        let hex: Vec<String> = bus.ram[0x05C0..0x0620]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        eprintln!("  ref ram[05C0..0620]: {}", hex.join(" "));
    }
    eprintln!(
        "  ref pre-roll: {pre} insn, nmi_enabled={}",
        bus.nmi_enabled
    );

    // Mirror the subject runtime's "once NMI has been enabled, keep
    // firing the frame NMI even if SMB later clears $2000 bit 7" latch
    // ($CB1A in runtime/boot.s). Without this the reference stops
    // running NMIs (and thus ReadJoypads) whenever SMB briefly disables
    // NMI during the title, so it never sees a controller press.
    // FD_TRACE_FRAME=N: during frame N, log visits to GameMenuRoutine
    // decision PCs (which path it takes when Start is pressed).
    let trace_frame: Option<usize> = std::env::var("FD_TRACE_FRAME")
        .ok()
        .and_then(|s| s.parse().ok());
    let debug_frame: Option<usize> = std::env::var("FD_DEBUG_FRAME")
        .ok()
        .and_then(|s| s.parse().ok());
    let watch_list = parse_watch_list();
    let trace_pcs: &[(u16, &str)] = &[
        (0x8231, "TitleScreenMode"),
        (0x8E04, "JumpEngine"),
        (0x8245, "GameMenuRoutine"),
        (0x8255, "StartGame"),
        (0x8258, "ChkSelect(not-start)"),
        (0x82D8, "ChkContinue"),
        (0x82E6, "StartWorld1"),
        (0x82F2, "inc OperMode"),
        (0x82C9, "ResetTitle"),
        (0x82C0, "RunDemo"),
        (0x82BB, "NullJoypad"),
    ];

    let log_bank_entries = std::env::var("FD_LOG_BANK_ENTRIES").is_ok();
    let call_log_frame: Option<usize> = std::env::var("FD_LOG_CALLS")
        .ok()
        .and_then(|v| v.parse().ok());
    // FD_TRACE_PC=E959,CA6D: log register and mapper state whenever the
    // reference executes one of the listed CPU addresses. This is useful for
    // proving indirect-dispatch selectors and bank identities from the NES,
    // without deriving profile facts from an already-diverged SMS subject.
    let trace_pc_list: Vec<u16> = std::env::var("FD_TRACE_PC")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|raw| {
                    u16::from_str_radix(
                        raw.trim().trim_start_matches("0x").trim_start_matches('$'),
                        16,
                    )
                    .ok()
                })
                .collect()
        })
        .unwrap_or_default();
    let trace_pc_limit = std::env::var("FD_TRACE_PC_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(100);
    let mut trace_pc_hits = 0usize;
    let mut call_log: Vec<(usize, u8, u16, u16)> = Vec::new();
    let mut bank_entry_set: std::collections::BTreeSet<(u8, u16)> = Default::default();
    let mut nmi_latched = bus.nmi_enabled;
    let mut nmi_fires = 0usize;
    // Action games that put NES Start on the SMS Pause NMI cannot use SMB's
    // RAM-driven title/gameplay face-button remap. This reference-only knob
    // keeps scripted `start` events as NES Start for those profiles.
    let pause_is_start = std::env::var("FD_PAUSE_START").is_ok();
    // FD_NES_DUMP=dir:f1,f2,... — render the NES reference's ground-truth
    // framebuffer (from captured nametable/palette/OAM/scroll state) to
    // dir/ref_NNNNN.ppm at the listed frames.
    let nes_dump: Option<(String, Vec<usize>)> =
        std::env::var("FD_NES_DUMP").ok().and_then(|spec| {
            let (dir, list) = spec.split_once(':')?;
            let frames_wanted = list
                .split(',')
                .filter_map(|f| f.trim().parse::<usize>().ok())
                .collect();
            Some((dir.to_string(), frames_wanted))
        });
    if let Some((dir, _)) = &nes_dump {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for frame in 0..frames {
        bus.current_frame = Some(frame);
        bus.buttons = match timeline.nes_at(frame) {
            Some(nes) => nes,
            None => effective_nes_buttons(
                frame,
                timeline,
                if pause_is_start { 0 } else { bus.ram[0x0770] },
            ),
        };
        bus.vblank = true;
        bus.sprite0_phase = 0; // new frame: re-arm the sprite-0 hit handshake
        if bus.nmi_enabled {
            nmi_latched = true;
        }
        if nmi_latched {
            cpu.nmi(&mut bus);
            nmi_fires += 1;
        }
        let tracing = Some(frame) == trace_frame;
        let debug_writes = Some(frame) == debug_frame;
        if debug_writes {
            bus.watch = Some(watch_list.clone());
            bus.watch_log.clear();
            bus.watch_bank_log.clear();
        }
        let hist_frame = std::env::var("FD_PC_HIST")
            .ok()
            .and_then(|v| v.parse::<usize>().ok());
        let mut pc_hist: std::collections::HashMap<u16, u32> = std::collections::HashMap::new();
        let distinct_on = std::env::var("FD_DISTINCT").is_ok();
        let mut distinct: std::collections::HashSet<u16> = std::collections::HashSet::new();
        let mut pcmin = 0xFFFFu16;
        let mut pcmax = 0u16;
        let mut insn_executed = 0u32;
        // Drive the mapper's scanline IRQ (VRC4) 262 times per frame, spread
        // across the fixed instruction budget. The frame-granular oracle has no
        // cycle clock, so this stands in for scanline timing: enough for
        // IRQ-paced main loops (Gradius II, Goemon) to advance.
        const SCANLINES_PER_FRAME: u32 = 262;
        let scanline_interval = (REF_INSN_PER_FRAME as u32 / SCANLINES_PER_FRAME).max(1);
        let mut insn_since_scanline = 0u32;
        for _ in 0..REF_INSN_PER_FRAME {
            bus.last_pc = cpu.pc;
            insn_executed += 1;
            insn_since_scanline += 1;
            if insn_since_scanline >= scanline_interval {
                insn_since_scanline = 0;
                let fire = bus.stateful.as_mut().is_some_and(|m| m.vrc_irq_scanline());
                if fire {
                    cpu.irq(&mut bus);
                }
            }
            if distinct_on {
                distinct.insert(cpu.pc);
                pcmin = pcmin.min(cpu.pc);
                pcmax = pcmax.max(cpu.pc);
            }
            if Some(frame) == hist_frame {
                *pc_hist.entry(cpu.pc).or_insert(0) += 1;
            }
            if trace_pc_hits < trace_pc_limit && trace_pc_list.contains(&cpu.pc) {
                let stack_p = bus.ram[0x0100 | cpu.sp.wrapping_add(1) as usize];
                let stack_lo = bus.ram[0x0100 | cpu.sp.wrapping_add(2) as usize];
                let stack_hi = bus.ram[0x0100 | cpu.sp.wrapping_add(3) as usize];
                eprintln!(
                    "TRACE_PC frame={frame} bank={} pc=${:04X} A=${:02X} X=${:02X} Y=${:02X} P=${:02X} SP=${:02X} stack_p=${stack_p:02X} stack_pc=${stack_hi:02X}{stack_lo:02X}",
                    bus.prg_bank, cpu.pc, cpu.a, cpu.x, cpu.y, cpu.p, cpu.sp
                );
                trace_pc_hits += 1;
            }
            if tracing {
                let pc = cpu.pc;
                if let Some((_, name)) = trace_pcs.iter().find(|(p, _)| *p == pc) {
                    eprintln!(
                        "  [trace f{frame}] {name} (pc=${pc:04X}) A=${:02X} $06FC=${:02X} $07A2(demoT)=${:02X}",
                        cpu.a, bus.ram[0x06FC], bus.ram[0x07A2]
                    );
                }
            }
            if let Some(cf) = call_log_frame {
                if frame <= cf && call_log.len() < 400 && cpu.pc >= 0x8000 {
                    let op = bus.prg_read(cpu.pc);
                    if op == 0x20 {
                        let t = bus.prg_read(cpu.pc.wrapping_add(1)) as u16
                            | (bus.prg_read(cpu.pc.wrapping_add(2)) as u16) << 8;
                        call_log.push((frame, bus.prg_bank, cpu.pc, t));
                    }
                }
            }
            if log_bank_entries && cpu.pc < 0x2000 {
                eprintln!("RAM_EXEC pc=${:04X} bank={}", cpu.pc, bus.prg_bank);
            }
            if log_bank_entries {
                // Ground truth for [[bank_entry]]: JSR/JMP whose operand
                // lands in the switchable window, keyed by the mapped bank.
                let pc = cpu.pc;
                if pc >= 0x8000 {
                    let op = bus.prg_read(pc);
                    if op == 0x6C {
                        // jmp (ind): log the LANDING (bank, target).
                        let p = bus.prg_read(pc.wrapping_add(1)) as u16
                            | (bus.prg_read(pc.wrapping_add(2)) as u16) << 8;
                        let t = if p < 0x2000 {
                            bus.ram[(p & 0x7FF) as usize] as u16
                                | (bus.ram[((p.wrapping_add(1)) & 0x7FF) as usize] as u16) << 8
                        } else if p >= 0x8000 {
                            bus.prg_read(p) as u16 | (bus.prg_read(p.wrapping_add(1)) as u16) << 8
                        } else {
                            0
                        };
                        if (0x8000..0xC000).contains(&t) {
                            bank_entry_set.insert((bus.prg_bank, t));
                        }
                    }
                    if op == 0x20 || op == 0x4C {
                        let t = bus.prg_read(pc.wrapping_add(1)) as u16
                            | (bus.prg_read(pc.wrapping_add(2)) as u16) << 8;
                        if (0x8000..0xC000).contains(&t) {
                            bank_entry_set.insert((bus.prg_bank, t));
                        }
                    }
                }
            }
            if cpu.step(&mut bus).is_err() {
                break;
            }
        }
        if debug_writes {
            eprintln!("  [ref debug] frame {frame} watched writes (addr <- val @ pc):");
            for (i, (a, v, pc)) in bus.watch_log.iter().take(100_000).enumerate() {
                let bank = bus.watch_bank_log.get(i).copied().unwrap_or(0);
                eprintln!("    ${a:04X} <- ${v:02X} @ pc=${pc:04X} bank={bank}");
            }
            bus.watch = None;
        }
        // Length counters tick after the frame's NMI ran — matching the
        // subject, whose apu_frame_tick runs after the translated NMI.
        bus.apu_frame_tick();
        if std::env::var("FD_WATCH_TASK").is_ok() && frame % 30 == 0 {
            eprintln!(
                "REF frame {frame}: task$18={:02X} $19={:02X} $0D={:02X}",
                bus.ram[0x18], bus.ram[0x19], bus.ram[0x0D]
            );
        }
        // FD_REF_TRACK=off,off,... prints those RAM byte offsets (hex, e.g.
        // 0x37 or 594) for the reference every frame. General per-frame guest
        // state probe for route/parity work and mapper debugging.
        if let Ok(spec) = std::env::var("FD_REF_TRACK") {
            let vals: Vec<String> = spec
                .split(',')
                .filter_map(|raw| {
                    let t = raw.trim().trim_start_matches("0x").trim_start_matches('$');
                    usize::from_str_radix(t, 16).ok().filter(|a| *a < 0x800)
                })
                .map(|a| format!("${a:03X}=${:02X}", bus.ram[a]))
                .collect();
            println!("REF_TRACK f{frame} {}", vals.join(" "));
        }
        if let Some((dir, frames_wanted)) = &nes_dump {
            if frames_wanted.contains(&frame) {
                let path = format!("{dir}/ref_{frame:05}.ppm");
                match render_nes_ppm(&bus, &path) {
                    Ok(()) => {
                        let pal: Vec<String> =
                            bus.palette_ram.iter().map(|b| format!("{b:02X}")).collect();
                        eprintln!(
                            "FD_NES_DUMP wrote {path} ctrl=${:02X} mask=${:02X} scrollx=${:02X} pal={}",
                            bus.ppu_ctrl,
                            bus.ppu_mask,
                            bus.scroll_x_last,
                            pal.join(" ")
                        );
                    }
                    Err(e) => eprintln!("FD_NES_DUMP failed for {path}: {e}"),
                }
            }
        }
        snaps.push(bus.ram);
        if std::env::var("FD_DBG_PC").is_ok() && frame % 100 == 0 {
            eprintln!("DBG frame {frame} pc=${:04X}", bus.last_pc);
        }
        if distinct_on && frame % 10 == 0 {
            eprintln!(
                "DISTINCT f{frame} n={} insn={insn_executed} pcmin=${pcmin:04X} pcmax=${pcmax:04X} mask=${:02X} nmi={nmi_fires}",
                distinct.len(),
                bus.ppu_mask
            );
        }
        if Some(frame) == hist_frame {
            let mut v: Vec<_> = pc_hist.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            for (pc, c) in v.into_iter().take(12) {
                eprintln!("HIST f{frame} pc=${pc:04X} count={c}");
            }
        }
    }
    if call_log_frame.is_some() {
        for (f, b, pc, t) in &call_log {
            eprintln!("CALL f{f} b{b} ${pc:04X} -> ${t:04X}");
        }
    }
    if log_bank_entries {
        for (b, t) in &bank_entry_set {
            eprintln!("BANK_ENTRY bank={b} addr=0x{t:04x}");
        }
    }
    if std::env::var("FD_DUMP_NT").is_ok() {
        let page = std::env::var("FD_DUMP_NT_PAGE")
            .ok()
            .and_then(|value| {
                usize::from_str_radix(
                    value
                        .trim()
                        .trim_start_matches("0x")
                        .trim_start_matches('$'),
                    16,
                )
                .ok()
            })
            .unwrap_or(0)
            .min(3);
        eprintln!(
            "REF PPU ctrl=${:02X} mask=${:02X} addr=${:04X} nt_page={page}",
            bus.ppu_ctrl, bus.ppu_mask, bus.ppu_addr
        );
        // Include the two 32-byte attribute-table rows after the 30 tile
        // rows.  Keeping them in the same dump makes it possible to
        // reconstruct the exact visible (tile, sub-palette) working set.
        for row in 0..32 {
            let base = 0x2000 + page * 0x400 + row * 32;
            let hex: Vec<String> = bus.chr_ram[base..base + 32]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            eprintln!("REF NT p{page} row {row:02}: {}", hex.join(" "));
        }
    }
    if let Ok(t) = std::env::var("FD_DUMP_CHRRAM") {
        if let Ok(tile) = usize::from_str_radix(t.trim_start_matches("0x"), 16) {
            let base = tile * 16;
            let hex: Vec<String> = bus.chr_ram[base..base + 16]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            eprintln!("CHRRAM tile {tile:03X}: {}", hex.join(" "));
        }
    }
    eprintln!(
        "  ref total $4016 reads: {}, nmi fires: {nmi_fires}",
        bus.joy_reads
    );
    (init_snap, snaps)
}

// ---------------------------------------------------------------------------
// Subject: SMS bus over z80_emu
// ---------------------------------------------------------------------------

const SMS_BANK: usize = 0x4000;

struct SmsBus {
    rom: Vec<u8>,
    slot_bank: [u8; 3],
    ram: [u8; 0x2000], // $C000-$DFFF, mirrored $E000-$FFFF
    // Cartridge SRAM (raw-CIRAM / CHR-RAM backend): mapped into slot 2
    // when $FFFC bit 3 is set.
    sram: Vec<u8>,
    sram_enabled: bool,
    // VDP model (Phase S VDP-parity oracle): control-port latch, address
    // register with the 2-bit code, 16 KiB VRAM, 32-byte CRAM.
    vdp_latch: Option<u8>,
    vdp_addr: u16,
    vdp_code: u8,
    vram: Vec<u8>,
    cram: [u8; 32],
    // Controller: SMS port $DC, active-low (1 = released).
    port_dc: u8,
    // Debug: when Some, log writes to these NES addresses (as $Cxxx),
    // capturing the CPU PC at the time of the write.
    watch: Option<Vec<u16>>,
    watch_log: Vec<(u16, u8, u16)>,
    last_pc: u16,
    // Phase S folded-BG analysis counters.
    ciram_writes: u64,
    ciram_same: u64,
    attr_writes: u64,
    attr_same: u64,
}

impl SmsBus {
    fn new(rom: Vec<u8>) -> Self {
        Self {
            rom,
            slot_bank: [0, 1, 2],
            ram: [0; 0x2000],
            sram: vec![0; 0x4000],
            sram_enabled: false,
            vdp_latch: None,
            vdp_addr: 0,
            vdp_code: 0,
            vram: vec![0; 0x4000],
            cram: [0; 32],
            port_dc: 0xFF,
            watch: None,
            watch_log: Vec::new(),
            last_pc: 0,
            ciram_writes: 0,
            ciram_same: 0,
            attr_writes: 0,
            attr_same: 0,
        }
    }
    fn vdp_hash(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in self.vram.iter().chain(self.cram.iter()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
    fn rom_byte(&self, bank: u8, off: u16) -> u8 {
        let i = bank as usize * SMS_BANK + off as usize;
        *self.rom.get(i).unwrap_or(&0xFF)
    }
}

impl z80_emu::Bus for SmsBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.rom_byte(0, addr), // fixed first 1 KiB
            0x0400..=0x3FFF => self.rom_byte(self.slot_bank[0], addr),
            0x4000..=0x7FFF => self.rom_byte(self.slot_bank[1], addr - 0x4000),
            0x8000..=0xBFFF => {
                if self.sram_enabled {
                    self.sram[(addr - 0x8000) as usize]
                } else {
                    self.rom_byte(self.slot_bank[2], addr - 0x8000)
                }
            }
            0xC000..=0xDFFF => self.ram[(addr - 0xC000) as usize],
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize],
            0xFFFC => 0,
            0xFFFD => self.slot_bank[0],
            0xFFFE => self.slot_bank[1],
            0xFFFF => self.slot_bank[2],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x8000..=0xBFFF if self.sram_enabled => {
                let i = (addr - 0x8000) as usize;
                if i < 0x800 {
                    self.ciram_writes += 1;
                    if self.sram[i] == value {
                        self.ciram_same += 1;
                    }
                }
                self.sram[i] = value;
            }
            0x0000..=0xBFFF => {} // ROM
            0xC000..=0xDFFF => {
                if (0xCB80..=0xCBFF).contains(&addr) {
                    self.attr_writes += 1;
                    if self.ram[(addr - 0xC000) as usize] == value {
                        self.attr_same += 1;
                    }
                }
                self.ram[(addr - 0xC000) as usize] = value;
                if let Some(w) = &self.watch {
                    let nes = addr - 0xC000;
                    if w.contains(&nes) {
                        self.watch_log.push((nes, value, self.last_pc));
                    }
                }
            }
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize] = value,
            0xFFFC => self.sram_enabled = value & 0x08 != 0,
            0xFFFD => self.slot_bank[0] = value,
            0xFFFE => self.slot_bank[1] = value,
            0xFFFF => self.slot_bank[2] = value,
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port & 0xC1 {
            0x80 => 0x00, // VDP data port $BE
            0x81 => {
                self.vdp_latch = None; // control reads reset the write latch
                0xFF // VDP status — ack reads
            }
            0xC0 => self.port_dc, // controller port 1 ($DC)
            0xC1 => 0xFF,         // controller port 2 ($DD)
            0x40 => 0xFF,         // H/V counter
            _ => 0xFF,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        // VDP model for the VDP-parity oracle. PSG and other ports are
        // still ignored.
        match port & 0xC1 {
            0x81 => {
                // Control port $BF: two-byte latch.
                match self.vdp_latch.take() {
                    None => self.vdp_latch = Some(value),
                    Some(lo) => {
                        self.vdp_code = value >> 6;
                        self.vdp_addr = ((value as u16 & 0x3F) << 8) | lo as u16;
                        // Code 2 is a register write; codes 0/1/3 set the
                        // address for reads/writes (reads unmodeled).
                    }
                }
            }
            0x80 => {
                // Data port $BE: write to VRAM or CRAM per the code.
                self.vdp_latch = None;
                if self.vdp_code == 3 {
                    self.cram[(self.vdp_addr & 0x1F) as usize] = value;
                } else {
                    self.vram[(self.vdp_addr & 0x3FFF) as usize] = value;
                }
                self.vdp_addr = self.vdp_addr.wrapping_add(1) & 0x3FFF;
            }
            _ => {}
        }
    }
}

/// Inverse of the SMS $DC mapping, replicating `rt_controller_latch`
/// exactly — including its **mode-dependent** face-button mapping keyed on
/// OperMode ($0770):
///   - title/menu mode (oper == 0): Button1 -> NES Select, Button2 -> NES Start
///   - gameplay modes  (oper != 0): Button1 -> NES A,      Button2 -> NES B
/// This matters because SMB's GameMenuRoutine starts the game on Start
/// *alone* (`cmp #$10`); a fixed B+Start alias would never start the game.
fn sms_dc_to_nes(dc: u8, title_mode: bool) -> u8 {
    let pressed = !dc;
    let mut nes = 0u8;
    if title_mode {
        if pressed & (1 << 4) != 0 {
            nes |= Buttons::SELECT;
        }
        if pressed & (1 << 5) != 0 {
            nes |= Buttons::START;
        }
    } else {
        if pressed & (1 << 4) != 0 {
            nes |= Buttons::A;
        }
        if pressed & (1 << 5) != 0 {
            nes |= Buttons::B;
        }
    }
    if pressed & (1 << 0) != 0 {
        nes |= Buttons::UP;
    }
    if pressed & (1 << 1) != 0 {
        nes |= Buttons::DOWN;
    }
    if pressed & (1 << 2) != 0 {
        nes |= Buttons::LEFT;
    }
    if pressed & (1 << 3) != 0 {
        nes |= Buttons::RIGHT;
    }
    nes
}

/// NES buttons the reference should see for a script frame: the script's
/// intent round-tripped through the SMS controller mapping, using the
/// reference's current OperMode so the title/gameplay split matches the
/// runtime.
fn effective_nes_buttons(frame: usize, timeline: &ButtonTimeline, oper_mode: u8) -> u8 {
    sms_dc_to_nes(timeline.sms_dc_at(frame), oper_mode == 0)
}

fn parse_watch_list() -> Vec<u16> {
    // FD_WATCH=all watches every NES RAM address ($0000-$07FF): combined
    // with FD_DEBUG_FRAME=N this logs the full ordered write sequence of
    // one frame on both sides, so the first mismatching write pinpoints
    // the diverging instruction.
    if std::env::var("FD_WATCH").as_deref() == Ok("all") {
        return (0u16..0x800).collect();
    }
    std::env::var("FD_WATCH")
        .ok()
        .map(|s| {
            s.split(',')
                .filter_map(|t| u16::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        })
        .unwrap_or_else(|| vec![0x0001])
}

/// Map NES controller buttons to the SMS $DC port (active-low) the way
/// `runtime/input.s` expects: bit0 Up, bit1 Down, bit2 Left, bit3
/// Right, bit4 Button1 (NES A / Select), bit5 Button2 (NES B / Start).
fn nes_buttons_to_sms_dc(b: Buttons) -> u8 {
    let mut pressed = 0u8; // 1 = pressed (we invert at the end)
    if b.0 & Buttons::UP != 0 {
        pressed |= 1 << 0;
    }
    if b.0 & Buttons::DOWN != 0 {
        pressed |= 1 << 1;
    }
    if b.0 & Buttons::LEFT != 0 {
        pressed |= 1 << 2;
    }
    if b.0 & Buttons::RIGHT != 0 {
        pressed |= 1 << 3;
    }
    if b.0 & (Buttons::A | Buttons::SELECT) != 0 {
        pressed |= 1 << 4;
    }
    if b.0 & (Buttons::B | Buttons::START) != 0 {
        pressed |= 1 << 5;
    }
    !pressed // active-low
}

// Generous per-frame budget: with the translated sound engine active a
// heavy frame can exceed 2M instructions; truncating a frame mid-handler
// leaves IFF disabled so every later fire_irq is silently skipped and the
// subject appears dead.
fn subj_insn_per_frame() -> usize {
    std::env::var("FD_SUBJ_IPF")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8_000_000)
}
// Pre-roll keeps the original 2M chunk so the boot/init IRQ cadence — and
// therefore the subject's first-NMI phase alignment against the reference —
// stays identical to the calibrated behavior.
const SUBJ_PREROLL_CHUNK: usize = 2_000_000;
// Pre-roll budget for SMS boot + SMB's translated reset-init (until NMI
// enable). The VDP critical-section lock adds per-PPU-access overhead to
// init's thousands of $2006/$2007 writes, so keep generous headroom.
const SUBJ_PREROLL_CAP: usize = 24_000_000;
const PPUCTRL_SHADOW: usize = 0x0B08; // SMS $CB08 = NES $2000 shadow

fn snap_nes_ram(bus: &SmsBus) -> [u8; 0x800] {
    let mut s = [0u8; 0x800];
    s.copy_from_slice(&bus.ram[0..0x800]);
    s
}

/// Addresses excluded from the differential comparison. The 6502 stack
/// page ($0100-$01FF) is call-frame scratch: the subject uses the Z80
/// stack for JSR/RTS and only mirrors PHA/PHP/RTI into the emulated
/// 6502 stack, so its contents legitimately differ and are not game
/// state.
fn is_excluded(addr: usize) -> bool {
    // 6502 stack page: call-frame scratch.
    if (0x0100..0x0200).contains(&addr) {
        return true;
    }
    // JumpEngine dispatch scratch ($04/$05 = pulled return address,
    // $06/$07 = selected target pointer). Our JumpEngineCall replaces
    // SMB's JumpEngine wholesale with a cp/jp chain and does not write
    // these — and could only write Z80 (slot-1) addresses, not the NES
    // addresses the original leaves, so faithful replication is
    // impossible. They are dispatch internals, not game state.
    if (0x0004..0x0008).contains(&addr) {
        return true;
    }
    // Optional: exclude the VRAM update buffers to surface game-logic
    // divergences hidden behind render-buffer phasing. Keep the window
    // tight: SMB's VRAM_Buffer1/2 live at $0300-$03C3, but $03C4-$03FF
    // (sprite-shuffle offsets, block-object state such as $03D1/$03E4+)
    // is real game state that logic branches on — excluding it hid the
    // true first divergence behind downstream OAM symptoms.
    if std::env::var("FD_EXCLUDE_VRAMBUF").is_ok() && (0x0300..0x03C4).contains(&addr) {
        return true;
    }
    // $07B5/$07B7 (sound-engine SFX length trackers) latch the exact
    // interleaving of $4015 status reads against APU length-counter ticks.
    // Both the reference model and the subject shim approximate the real
    // 240 Hz frame sequencer at whole-video-frame granularity, and their
    // tick phases legitimately differ by up to one frame, so these two
    // bytes can hold transiently different SFX durations. Everything the
    // engine derives from them stays byte-identical (verified: no other
    // divergence across the route), so exclude just these two.
    if addr == 0x07B5 || addr == 0x07B7 {
        return true;
    }
    // Optional: exclude SMB audio-engine RAM while SoundEngine is intentionally
    // stubbed/deferred. The corresponding writer PCs are in the $F3xx-$F7xx
    // sound engine. Keep this opt-in so audio work can remove the exclusion.
    if std::env::var("FD_EXCLUDE_AUDIO").is_ok() && is_deferred_audio_addr(addr) {
        return true;
    }
    false
}

fn is_deferred_audio_addr(addr: usize) -> bool {
    // $07B0-$07CF: SMB sound-engine working RAM (music/sfx buffers and
    // length counters). $07C8-$07CF observed written only from the
    // $F3xx-$F7xx SoundEngine (e.g. $07CA @ $F72D/$F7D7), which is
    // intentionally stubbed while audio is deferred.
    matches!(addr, 0x00F0..=0x00FF | 0x07B0..=0x07CF)
}

/// Returns (init_snapshot, per_frame_snapshots). Mirrors run_reference:
/// pre-roll through SMS boot + SMB's translated reset-init until SMB
/// enables NMI (PPUCTRL shadow $CB08 bit 7), capture init RAM, then
/// fire one IRQ per frame (gated on the same NMI-enable bit).
/// FD_PROFILE=1: attribute subject cycles inside the measured NMI window of
/// steady frames (last third) to WLA symbols from the `.sym` file next to
/// the ROM. Key = (slot region, mapped bank, pc).
fn profile_report(
    prof: &std::collections::HashMap<(u8, u8, u16), u64>,
    sym_path: &std::path::Path,
) {
    // Parse "[labels]" lines of form "bb:aaaa name".
    let mut tables: std::collections::HashMap<(u8, u8), Vec<(u16, String)>> =
        std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string(sym_path) {
        let mut in_labels = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_labels = line == "[labels]";
                continue;
            }
            if !in_labels || line.is_empty() || line.starts_with(';') {
                continue;
            }
            let Some((bank_s, rest)) = line.split_once(':') else {
                continue;
            };
            let Some((addr_s, name)) = rest.split_once(' ') else {
                continue;
            };
            let (Ok(bank), Ok(addr)) = (
                u8::from_str_radix(bank_s, 16),
                u16::from_str_radix(addr_s, 16),
            ) else {
                continue;
            };
            let region = match addr {
                0x0000..=0x3FFF => 0u8,
                0x4000..=0x7FFF => 1,
                _ => 2,
            };
            tables
                .entry((region, bank))
                .or_default()
                .push((addr, name.to_string()));
        }
    } else {
        eprintln!("  [profile] no sym file at {}", sym_path.display());
    }
    for v in tables.values_mut() {
        v.sort();
    }
    let resolve = |region: u8, bank: u8, pc: u16| -> String {
        let bank = if region == 0 { 0 } else { bank };
        if let Some(tab) = tables.get(&(region, bank)) {
            let i = tab.partition_point(|(a, _)| *a <= pc);
            if i > 0 {
                let (addr, name) = &tab[i - 1];
                return format!("{name} (+{:X})", pc - addr);
            }
        }
        format!("?r{region}b{bank:02X}:{pc:04X}")
    };
    // Aggregate per symbol (drop the +offset for grouping).
    let mut by_sym: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let total: u64 = prof.values().sum();
    for (&(region, bank, pc), &cyc) in prof {
        let sym = resolve(region, bank, pc);
        let base = sym.split(" (+").next().unwrap_or(&sym).to_string();
        *by_sym.entry(base).or_default() += cyc;
    }
    let mut rows: Vec<(String, u64)> = by_sym.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    eprintln!("  [profile] steady-frame NMI cycles by symbol (total {total}):");
    let mut cum = 0u64;
    for (name, cyc) in rows.iter().take(45) {
        cum += cyc;
        eprintln!(
            "    {:>10} cyc  {:5.1}%  (cum {:5.1}%)  {}",
            cyc,
            *cyc as f64 * 100.0 / total.max(1) as f64,
            cum as f64 * 100.0 / total.max(1) as f64,
            name
        );
    }
}

fn run_subject(
    rom: Vec<u8>,
    frames: usize,
    timeline: &ButtonTimeline,
    sym_path: Option<std::path::PathBuf>,
) -> ([u8; 0x800], Vec<[u8; 0x800]>) {
    use z80_emu::{Bus, Cpu};
    let mut cpu = Cpu::new();
    let mut bus = SmsBus::new(rom);
    cpu.pc = 0x0000;
    cpu.sp = 0xDFF0;

    let nmi_enabled = |bus: &SmsBus| bus.ram[PPUCTRL_SHADOW] & 0x80 != 0;

    // Fire the frame IRQ (IM1 -> $0038) if interrupts are enabled. The
    // runtime irq_handler always sets the VBlank flag + acks, and only
    // runs the game NMI once SMB has enabled it ($CB08 bit 7) — so
    // firing every frame is correct in both the pre-roll (init polls
    // $2002 for VBlank, no game NMI yet) and steady-state phases.
    let fire_irq = |cpu: &mut Cpu, bus: &mut SmsBus| {
        // The VDP frame interrupt is level-held: if the CPU currently has
        // interrupts disabled (e.g. inside the runtime's VDP DI bracket in
        // the idle loop's $2002 poll), step until IFF1 re-enables instead
        // of silently dropping the frame — a dropped IRQ freezes the
        // subject for a frame and desyncs it from the reference.
        let mut settle = 0usize;
        while !cpu.iff1 && settle < 100_000 {
            if cpu.halted || cpu.step(bus).is_err() {
                break;
            }
            settle += 1;
        }
        if cpu.iff1 {
            cpu.sp = cpu.sp.wrapping_sub(2);
            let pc = cpu.pc;
            Bus::write(bus, cpu.sp, (pc & 0xFF) as u8);
            Bus::write(bus, cpu.sp.wrapping_add(1), (pc >> 8) as u8);
            cpu.pc = 0x0038;
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
        }
    };

    // Pre-roll: run frame-by-frame (firing IRQ each frame for VBlank)
    // until SMB enables NMI. Cap by total instructions.
    let mut pre = 0usize;
    let mut pre_frames = 0usize;
    while !nmi_enabled(&bus) && pre < SUBJ_PREROLL_CAP {
        fire_irq(&mut cpu, &mut bus);
        for _ in 0..SUBJ_PREROLL_CHUNK {
            if cpu.halted || cpu.step(&mut bus).is_err() {
                break;
            }
            pre += 1;
            if nmi_enabled(&bus) {
                break;
            }
        }
        pre_frames += 1;
    }
    // NMI-enable is detected inside rt_ppu_write's VDP critical section,
    // which runs with interrupts disabled. Step until the bracket exits
    // (IFF1 restored) so frame 0's fired IRQ is not silently swallowed —
    // otherwise the subject misses one NMI and every snapshot is phase-
    // shifted against the reference.
    let mut settle = 0usize;
    while !cpu.iff1 && settle < 10_000 {
        if cpu.halted || cpu.step(&mut bus).is_err() {
            break;
        }
        settle += 1;
    }
    let sem_anchor: Option<(usize, u8)> = std::env::var("FD_ANCHOR").ok().and_then(|s| {
        let (a, v) = s.split_once(':')?;
        Some((
            usize::from_str_radix(a, 16).ok()?,
            u8::from_str_radix(v, 16).ok()?,
        ))
    });
    if let Some((aa, av)) = sem_anchor {
        let mut aframes = 0usize;
        while bus.ram[aa] != av && aframes < 1800 {
            fire_irq(&mut cpu, &mut bus);
            for _ in 0..SUBJ_PREROLL_CHUNK {
                if cpu.halted || cpu.step(&mut bus).is_err() {
                    break;
                }
                if bus.ram[aa] == av {
                    break;
                }
            }
            aframes += 1;
        }
        eprintln!(
            "  subj sem-anchor: {aframes} frames, ram[${aa:04X}]=${:02X}",
            bus.ram[aa]
        );
    }
    if std::env::var("FD_ANCHOR_RENDER").is_ok() {
        let mut aframes = 0usize;
        // SMS $CB09 = NES PPUMASK shadow.
        while bus.ram[0x0B09] & 0x18 != 0x18 && aframes < 900 {
            fire_irq(&mut cpu, &mut bus);
            for _ in 0..SUBJ_PREROLL_CHUNK {
                if cpu.halted || cpu.step(&mut bus).is_err() {
                    break;
                }
                if bus.ram[0x0B09] & 0x18 == 0x18 {
                    break;
                }
            }
            aframes += 1;
        }
        eprintln!(
            "  subj render-anchor: {aframes} frames, mask-shadow=${:02X}",
            bus.ram[0x0B09]
        );
    }
    let init_snap = snap_nes_ram(&bus);
    eprintln!(
        "  subj pre-roll: {pre} insn over {pre_frames} frames, PC=${:04X} nmi_enabled={} $C772={:02X}",
        cpu.pc,
        nmi_enabled(&bus),
        bus.ram[0x772]
    );
    // Helper: report if the unresolved-jsr trap has fired ($CB1D=$E1)
    // and which routine id ($CB1B/$CB1C).
    let report_trap = |bus: &SmsBus, cpu: &z80_emu::Cpu, when: &str| {
        if bus.ram[0x0B1D] & 0xF0 == 0xE0 && bus.ram[0x0B1D] != 0 {
            let id = bus.ram[0x0B1B] as u16 | ((bus.ram[0x0B1C] as u16) << 8);
            eprintln!(
                "  *** TRAP ({when}): marker=${:02X} id=${id:04X} nes_bank={} disp_ret=${:04X} z80_pc=${:04X} sp=${:04X} last_hit=${:04X}@b{:02X}",
                bus.ram[0x0B1D],
                bus.ram[0x0B1A],
                bus.ram[0x0B73] as u16 | (bus.ram[0x0B74] as u16) << 8,
                cpu.pc,
                cpu.sp,
                bus.ram[0x0B7A] as u16 | (bus.ram[0x0B7B] as u16) << 8,
                bus.ram[0x0B7C]
            );
        }
    };
    report_trap(&bus, &cpu, "pre-roll");

    let debug_frame: Option<usize> = std::env::var("FD_DEBUG_FRAME")
        .ok()
        .and_then(|s| s.parse().ok());
    // FD_WATCH=0x0001,0x000E,... — NES addresses to log writes to (with PC)
    // during the debug frame. Defaults to $0001 (the first divergence).
    let watch_list = parse_watch_list();

    // FD_MEASURE_NMI=1 — measure the per-frame NMI cost (instructions from
    // IRQ-inject until the stack unwinds back, i.e. the NMI chain returns to
    // the main wait-loop). This is the real per-frame work that must fit in
    // the SMS budget (~59,736 Z80 cycles ≈ ~6,000 instructions/frame).
    let measure_nmi = std::env::var("FD_MEASURE_NMI").is_ok();
    let mut nmi_costs: Vec<usize> = Vec::new();

    // FD_PROFILE=1 — per-symbol cycle attribution over the same NMI window,
    // steady frames only (last third, matching steady_avg).
    let profile = std::env::var("FD_PROFILE").is_ok();
    let steady_start = frames.saturating_sub(frames / 3);
    let mut prof: std::collections::HashMap<(u8, u8, u16), u64> = std::collections::HashMap::new();
    let mut far_hist: std::collections::HashMap<(u8, u16), u32> = std::collections::HashMap::new();
    let mut far_edges: std::collections::HashMap<((u8, u16), (u8, u16)), u32> =
        std::collections::HashMap::new();
    // FD_VDP_DUMP=<file>: record a per-frame FNV hash of VRAM+CRAM.
    // FD_VDP_CHECK=<file>: compare against a recorded golden run.
    let vdp_dump: Option<String> = std::env::var("FD_VDP_DUMP").ok();
    let vdp_check: Option<Vec<u64>> = std::env::var("FD_VDP_CHECK").ok().and_then(|p| {
        std::fs::read_to_string(p).ok().map(|t| {
            t.lines()
                .filter_map(|l| u64::from_str_radix(l.trim(), 16).ok())
                .collect()
        })
    });
    let mut vdp_hashes: Vec<u64> = Vec::new();
    let mut gbv_allocs_steady: u64 = 0;
    let gbv_alloc_pc: Option<u16> = sym_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| {
            text.lines().find_map(|l| {
                let l = l.trim();
                let (bank_addr, name) = l.split_once(' ')?;
                if name == "_gbv_alloc" {
                    let (_b, a) = bank_addr.split_once(':')?;
                    u16::from_str_radix(a, 16).ok()
                } else {
                    None
                }
            })
        });
    // Addresses of the native far-shim entries, from the sym file.
    let far_entries: Vec<u16> = sym_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|text| {
            text.lines()
                .filter_map(|l| {
                    let l = l.trim();
                    let (bank_addr, name) = l.split_once(' ')?;
                    if name == "rt_far_ncall" || name == "rt_far_tail" {
                        let (_b, a) = bank_addr.split_once(':')?;
                        u16::from_str_radix(a, 16).ok()
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for _frame in 0..frames {
        bus.port_dc = timeline.sms_dc_at(_frame);
        let dbg = Some(_frame) == debug_frame;
        if dbg {
            bus.watch = Some(watch_list.clone());
            bus.watch_log.clear();
        }
        let sp_before = cpu.sp;
        let cyc_before = cpu.cycles;
        fire_irq(&mut cpu, &mut bus);
        let fired = cpu.pc == 0x0038;
        let mut nmi_done = !fired;
        let prof_this_frame = profile && _frame >= steady_start;
        for _ in 0..subj_insn_per_frame() {
            if cpu.halted {
                break;
            }
            bus.last_pc = cpu.pc;
            let cyc0 = cpu.cycles;
            if cpu.step(&mut bus).is_err() {
                break;
            }
            if prof_this_frame && !nmi_done {
                let pc = bus.last_pc;
                let key = match pc {
                    0x0000..=0x3FFF => (0u8, 0u8, pc),
                    0x4000..=0x7FFF => (1, bus.slot_bank[1], pc),
                    _ => (2, bus.slot_bank[2], pc),
                };
                *prof.entry(key).or_default() += cpu.cycles - cyc0;
                // Far-transfer histogram: at the shim entries BC = target
                // label address and H = target bank.
                if far_entries.contains(&cpu.pc) {
                    let tgt = ((cpu.b as u16) << 8) | cpu.c as u16;
                    *far_hist.entry((cpu.h, tgt)).or_default() += 1u32;
                    // Edge collection for the bank placer: the transfer
                    // site is last_pc (the call/jp into the shim).
                    if (0x4000..0x8000).contains(&pc) {
                        *far_edges
                            .entry(((bus.slot_bank[1], pc), (cpu.h, tgt)))
                            .or_default() += 1u32;
                    }
                }
                if Some(cpu.pc) == gbv_alloc_pc {
                    gbv_allocs_steady += 1;
                }
            }
            if !nmi_done && cpu.sp >= sp_before {
                nmi_done = true;
                if measure_nmi {
                    nmi_costs.push((cpu.cycles - cyc_before) as usize);
                }
            }
        }
        if dbg {
            eprintln!("  [debug] frame {_frame} watched writes (addr <- val @ pc):");
            for (a, v, pc) in bus.watch_log.iter().take(100_000) {
                eprintln!("    ${a:04X} <- ${v:02X} @ pc=${pc:04X}");
            }
            bus.watch = None;
        }
        snaps.push(snap_nes_ram(&bus));
        if vdp_dump.is_some() || vdp_check.is_some() {
            vdp_hashes.push(bus.vdp_hash());
        }
    }
    if let Some(path) = &vdp_dump {
        let text: String = vdp_hashes.iter().map(|h| format!("{h:016x}\n")).collect();
        let _ = std::fs::write(path, text);
        eprintln!(
            "  [vdp] dumped {} per-frame VRAM+CRAM hashes",
            vdp_hashes.len()
        );
    }
    if let Some(golden) = &vdp_check {
        let mut mismatches = 0usize;
        let mut first: Option<usize> = None;
        for (i, h) in vdp_hashes.iter().enumerate() {
            if golden.get(i) != Some(h) {
                mismatches += 1;
                if first.is_none() {
                    first = Some(i);
                }
            }
        }
        match first {
            None => eprintln!(
                "  [vdp] VDP PARITY OK across {} frames (VRAM+CRAM byte-exact)",
                vdp_hashes.len()
            ),
            Some(f) => eprintln!(
                "  [vdp] VDP DIVERGENCE: {mismatches} of {} frames differ, first at frame {f}",
                vdp_hashes.len()
            ),
        }
    }
    report_trap(&bus, &cpu, "after frames");
    if measure_nmi && !nmi_costs.is_empty() {
        let n = nmi_costs.len();
        let total: usize = nmi_costs.iter().sum();
        let max = *nmi_costs.iter().max().unwrap();
        let min = *nmi_costs.iter().min().unwrap();
        // steady-state = last third of frames (past area-parse/intermediate)
        let tail = &nmi_costs[n.saturating_sub(n / 3).min(n - 1)..];
        let tail_avg = tail.iter().sum::<usize>() / tail.len().max(1);
        eprintln!(
            "  [NMI cost] frames={n} avg={} min={min} max={max} steady_avg={tail_avg} cycles/frame  (SMS budget ~59736)",
            total / n
        );
    }
    let sym_path_copy = sym_path.clone();
    if profile
        && !prof.is_empty()
        && let Some(sym) = sym_path
    {
        profile_report(&prof, &sym);
        eprintln!(
            "  [folded-BG] raw-CIRAM tile writes: {} total, {} same-value ({:.0}%); attr-shadow writes: {} total, {} same-value ({:.0}%); variant allocations in steady frames: {}",
            bus.ciram_writes,
            bus.ciram_same,
            bus.ciram_same as f64 * 100.0 / bus.ciram_writes.max(1) as f64,
            bus.attr_writes,
            bus.attr_same,
            bus.attr_same as f64 * 100.0 / bus.attr_writes.max(1) as f64,
            gbv_allocs_steady,
        );
        if !far_hist.is_empty() {
            let mut rows: Vec<((u8, u16), u32)> = far_hist.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            eprintln!("  [profile] far transfers by (bank, target), steady frames:");
            for ((bank, tgt), n) in rows.iter().take(20) {
                eprintln!("    {n:>7} x  bank {bank:02} target ${tgt:04X}");
            }
        }
        // FD_FAR_EDGES=<file>: dump caller->target far edges resolved to
        // NES addresses (via L_/func_ symbol names) for the profile's
        // edge-weighted bank placer.
        if let (Ok(path), Some(sym)) = (std::env::var("FD_FAR_EDGES"), sym_path_copy.as_ref()) {
            let mut tables: std::collections::HashMap<u8, Vec<(u16, u16)>> = Default::default();
            if let Ok(text) = std::fs::read_to_string(sym) {
                for line in text.lines() {
                    let line = line.trim();
                    let Some((bank_addr, name)) = line.split_once(' ') else {
                        continue;
                    };
                    let Some((b, a)) = bank_addr.split_once(':') else {
                        continue;
                    };
                    let (Ok(bank), Ok(addr)) =
                        (u8::from_str_radix(b, 16), u16::from_str_radix(a, 16))
                    else {
                        continue;
                    };
                    let nes = name
                        .strip_prefix("L_")
                        .or_else(|| name.strip_prefix("func_"))
                        .and_then(|h| u16::from_str_radix(h, 16).ok());
                    if let Some(nes) = nes
                        && (0x4000..0x8000).contains(&addr)
                    {
                        tables.entry(bank).or_default().push((addr, nes));
                    }
                }
            }
            for t in tables.values_mut() {
                t.sort();
            }
            let resolve = |bank: u8, pc: u16| -> Option<u16> {
                let t = tables.get(&bank)?;
                let i = t.partition_point(|&(a, _)| a <= pc);
                t.get(i.checked_sub(1)?).map(|&(_, nes)| nes)
            };
            let mut agg: std::collections::HashMap<(u16, u16), u32> = Default::default();
            for (((cb, cpc), (tb, tpc)), n) in &far_edges {
                if let (Some(c), Some(t)) = (resolve(*cb, *cpc), resolve(*tb, *tpc)) {
                    *agg.entry((c, t)).or_default() += n;
                }
            }
            let mut rows: Vec<((u16, u16), u32)> = agg.into_iter().collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            let text: String = rows
                .iter()
                .map(|((c, t), n)| format!("{c:04X} {t:04X} {n}\n"))
                .collect();
            let _ = std::fs::write(&path, text);
            eprintln!("  [profile] dumped {} far edges to {path}", rows.len());
        }
    }
    (init_snap, snaps)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let nes_path = PathBuf::from(args.get(1).expect(
        "usage: frame-diff <smb.nes> <out.sms> [--frames N] [--script S] [--buttons-script path]",
    ));
    let _sms_path = args.get(2).cloned();

    let mut frames = 120usize;
    let mut script = "none".to_string();
    let mut buttons_script: Option<String> = None;
    let mut nes_buttons_script: Option<String> = None;
    let mut ref_only = false;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--frames" => {
                i += 1;
                frames = args[i].parse().expect("frames int");
            }
            "--script" => {
                i += 1;
                script = args[i].clone();
            }
            "--buttons-script" => {
                i += 1;
                buttons_script = Some(args[i].clone());
            }
            "--nes-buttons-script" => {
                i += 1;
                nes_buttons_script = Some(args[i].clone());
            }
            "--ref-only" => ref_only = true,
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let nes = std::fs::read(&nes_path).expect("read nes");
    let image = nes_rom::parse(&nes).expect("parse nes");
    let stateful = match image.header.mapper {
        1 => Some(StatefulMapper::Mmc1(
            nes_rom::mmc1::Mmc1::new(&image.header, image.prg.len()).expect("supported MMC1 board"),
        )),
        7 => Some(StatefulMapper::Axrom(
            nes_rom::axrom::Axrom::new(&image.header, image.prg.len())
                .expect("supported AxROM board"),
        )),
        22 | 23 | 25 => Some(StatefulMapper::Vrc2(
            nes_rom::vrc2::Vrc2::new(&image.header, image.prg.len(), image.chr.len())
                .expect("supported VRC2/4 board"),
        )),
        69 => Some(StatefulMapper::Fme7(
            nes_rom::fme7::Fme7::new(&image.header, image.prg.len(), image.chr.len())
                .expect("supported FME-7 board"),
        )),
        9 | 10 => Some(StatefulMapper::Mmc2(
            nes_rom::mmc2::Mmc2::new(&image.header, image.prg.len(), image.chr.len())
                .expect("supported MMC2/MMC4 board"),
        )),
        _ => None,
    };
    let mapper_policy = if stateful.is_some() {
        // The stateful mapper is routed via `stateful`; the policy is unused.
        nes_rom::MapperPolicy::Nrom {
            prg_len: image.prg.len(),
        }
    } else {
        nes_rom::resolve_mapper_policy_oracle(&image.header, image.prg.len())
            .expect("supported mapper policy")
    };
    let prg = image.prg.to_vec();

    let (timeline, script_desc) = if let Some(path) = nes_buttons_script {
        let events = load_nes_button_script(&path)
            .unwrap_or_else(|err| panic!("invalid --nes-buttons-script: {err}"));
        (
            ButtonTimeline::from_nes_events(events),
            format!("nes-buttons-script:{path}"),
        )
    } else if let Some(path) = buttons_script {
        let events = load_button_script(&path)
            .unwrap_or_else(|err| panic!("invalid --buttons-script: {err}"));
        (
            ButtonTimeline::from_events(events),
            format!("buttons-script:{path}"),
        )
    } else {
        (ButtonTimeline::builtin(script.clone()), script.clone())
    };

    eprintln!(
        "Reference: running SMB PRG ({} bytes) for {frames} frames, script={script_desc}",
        prg.len()
    );
    let (ref_init, ref_snaps) = run_reference(
        prg,
        image.chr.to_vec(),
        mapper_policy,
        stateful,
        frames,
        &timeline,
    );

    // FD_REF_ONLY=1: print a compact per-frame reference trajectory for
    // authoring/recalibrating input scripts against real-NES dynamics
    // (player page:x, y, state, lives, world/level/area, OperMode/task),
    // then exit without running the subject. Emits a line every 16 frames
    // and on every OperMode/task/state/lives change.
    if std::env::var("FD_REF_ONLY").is_ok() {
        let mut last = (0xFFu8, 0xFFu8, 0xFFu8, 0xFFu8);
        for (f, s) in ref_snaps.iter().enumerate() {
            let ud = s[0x000D];
            let ps = s[0x06FC];
            let key = (s[0x0770], s[0x0772], s[0x000E], s[0x075A]);
            if f % 16 == 0 || key != last {
                println!(
                    "f={f:4} mode={:02X} task={:02X} x={:02X}:{:02X} y={:02X} yspd={:02X} state={:02X} lives={:02X} wla={:02X}{:02X}{:02X} ud={ud:02X} pstate={ps:02X}",
                    s[0x0770],
                    s[0x0772],
                    s[0x006D],
                    s[0x0086],
                    s[0x00CE],
                    s[0x009F],
                    s[0x000E],
                    s[0x075A],
                    s[0x075F],
                    s[0x075C],
                    s[0x0760],
                );
                last = key;
            }
        }
        return;
    }

    // Report reference progression of key game-state vars.
    println!("frame | $0770 $0772 $0773 $0772.. (operation/task)");
    let mut last_770 = 0xFFu8;
    let mut last_772 = 0xFFu8;
    for (f, snap) in ref_snaps.iter().enumerate() {
        let a770 = snap[0x0770];
        let a772 = snap[0x0772];
        let a73c = snap[0x073C];
        // Print only frames where $0770 or $0772 changed, plus the first few.
        if f < 6 || a770 != last_770 || a772 != last_772 {
            println!("  {f:3} | OperMode=${a770:02X} Task=${a772:02X} ScreenRtn=${a73c:02X}");
            last_770 = a770;
            last_772 = a772;
        }
    }

    if ref_only {
        return;
    }

    let sms_path = _sms_path.expect("need <out.sms> for subject side");
    let rom = std::fs::read(&sms_path).expect("read sms rom");
    eprintln!(
        "Subject: running SMS ROM ({} bytes) for {frames} frames",
        rom.len()
    );
    let sym_path = std::path::Path::new(&sms_path).with_extension("sym");
    let (subj_init, subj_snaps) = run_subject(rom, frames, &timeline, Some(sym_path));

    // First, compare the init snapshot (RAM at the NMI-enable point).
    // If reset-init translation is faithful, these match and we move on
    // to per-frame NMI comparison. If not, fix reset-init first.
    {
        let mut diffs: Vec<(usize, u8, u8)> = Vec::new();
        for a in 0..0x800 {
            if !is_excluded(a) && ref_init[a] != subj_init[a] {
                diffs.push((a, ref_init[a], subj_init[a]));
            }
        }
        if diffs.is_empty() {
            println!("\nINIT SNAPSHOT: match ({} bytes identical)", 0x800);
        } else {
            println!(
                "\nINIT SNAPSHOT DIVERGES: {} of 2048 bytes differ at the NMI-enable point.",
                diffs.len()
            );
            println!("  (reset-init translation is not yet faithful — fix this before frames)");
            for (a, rv, sv) in diffs.iter().take(32) {
                println!("    ${a:04X}: ref=${rv:02X} subj=${sv:02X}");
            }
        }
    }

    // Compare frame by frame. Report the first divergence and the
    // addresses that differ, focusing on the game-state page $0700-$07FF
    // first (operation mode, task, timers) then the whole $0000-$07FF.
    // Debug: trajectory of a chosen address both sides (FD_TRAJ=0xADDR).
    if let Ok(spec) = std::env::var("FD_TRAJ") {
        let addr = usize::from_str_radix(spec.trim_start_matches("0x"), 16).unwrap_or(0x7A7);
        let lo = frames.min(ref_snaps.len()).min(subj_snaps.len());
        eprint!("  [traj ${addr:04X}] ref :");
        for f in 0..lo {
            eprint!(" {:02X}", ref_snaps[f][addr]);
        }
        eprintln!();
        eprint!("  [traj ${addr:04X}] subj:");
        for f in 0..lo {
            eprint!(" {:02X}", subj_snaps[f][addr]);
        }
        eprintln!();
    }

    // Gameplay-focused differential: the broad RAM diff can be dominated by
    // score/audio/bookkeeping bytes. This summarizes the player and progression
    // variables that decide whether a route is still semantically aligned.
    if std::env::var("FD_KEY_SUMMARY").is_ok() {
        const KEY_ADDRS: &[(usize, &str)] = &[
            (0x000D, "UserData"),
            (0x000E, "Player_State"),
            (0x0057, "Player_X_Speed"),
            (0x006D, "Player_Page"),
            (0x0086, "Player_X"),
            (0x009F, "Player_Y_Speed"),
            (0x00CE, "Player_Y"),
            (0x06FC, "Player_State2"),
            (0x075A, "NumberOfLives"),
            (0x075C, "WorldNumber"),
            (0x0760, "AreaNumber"),
            (0x0770, "OperMode"),
            (0x0772, "OperMode_Task"),
        ];

        let lo = frames.min(ref_snaps.len()).min(subj_snaps.len());
        let mut first_key_div: Option<(usize, usize, &'static str, u8, u8)> = None;
        'frames: for f in 0..lo {
            for &(addr, name) in KEY_ADDRS {
                let rv = ref_snaps[f][addr];
                let sv = subj_snaps[f][addr];
                if rv != sv {
                    first_key_div = Some((f, addr, name, rv, sv));
                    break 'frames;
                }
            }
        }
        match first_key_div {
            Some((f, addr, name, rv, sv)) => eprintln!(
                "  [key summary] first gameplay-key divergence: frame={f} ${addr:04X} {name} ref=${rv:02X} subj=${sv:02X}"
            ),
            None => eprintln!("  [key summary] gameplay keys match across {lo} frames"),
        }

        let sample_frames = std::env::var("FD_KEY_FRAMES")
            .ok()
            .map(|s| {
                s.split(',')
                    .filter_map(|p| p.trim().parse::<usize>().ok())
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| vec![416, 720, 1120, 1328, 1510, 1531, 1600, 1736, 1906]);
        for f in sample_frames.into_iter().filter(|&f| f < lo) {
            let r = &ref_snaps[f];
            let s = &subj_snaps[f];
            eprintln!(
                "  [key @{f}] ref x={:02X}:{:02X} spd={:02X} y={:02X} yspd={:02X} state={:02X} pstate={:02X} mode={:02X}:{:02X} wla={:02X}{:02X}{:02X} lives={:02X} ud={:02X}",
                r[0x006D],
                r[0x0086],
                r[0x0057],
                r[0x00CE],
                r[0x009F],
                r[0x000E],
                r[0x06FC],
                r[0x0770],
                r[0x0772],
                r[0x075F],
                r[0x075C],
                r[0x0760],
                r[0x075A],
                r[0x000D]
            );
            eprintln!(
                "  [key @{f}] subj x={:02X}:{:02X} spd={:02X} y={:02X} yspd={:02X} state={:02X} pstate={:02X} mode={:02X}:{:02X} wla={:02X}{:02X}{:02X} lives={:02X} ud={:02X}",
                s[0x006D],
                s[0x0086],
                s[0x0057],
                s[0x00CE],
                s[0x009F],
                s[0x000E],
                s[0x06FC],
                s[0x0770],
                s[0x0772],
                s[0x075F],
                s[0x075C],
                s[0x0760],
                s[0x075A],
                s[0x000D]
            );
        }
    }

    // Sprite-data differential: dump $0200-$023F (16 OAM entries: Y,tile,
    // attr,X) for ref and subj at FD_DUMP_SPRITES=<frame>, to compare the
    // sprite tiles SMB builds vs what our translation builds.
    if let Ok(fs) = std::env::var("FD_DUMP_SPRITES") {
        if let Ok(f) = fs.parse::<usize>() {
            let dump = |label: &str, snap: &[u8; 0x800]| {
                eprint!("  [sprites @{f}] {label}:");
                for i in 0..256 {
                    eprint!(" {:02X}", snap[0x200 + i]);
                }
                eprintln!();
                // Count distinct (tile, attr & 0xC3) among visible sprites
                // (NES Y < 0xEF) — the sprite palette/flip variants a CHR-RAM
                // build must generate this frame.
                let mut variants = std::collections::BTreeSet::new();
                for s in 0..64 {
                    let y = snap[0x200 + s * 4];
                    if y >= 0xEF {
                        continue;
                    }
                    let tile = snap[0x200 + s * 4 + 1];
                    let key = snap[0x200 + s * 4 + 2] & 0xC3;
                    if key != 0 {
                        variants.insert((tile, key));
                    }
                }
                eprintln!(
                    "  [sprites @{f}] {label}: {} distinct palette/flip variants needed",
                    variants.len()
                );
            };
            if f < ref_snaps.len() {
                dump("ref ", &ref_snaps[f]);
            }
            if f < subj_snaps.len() {
                dump("subj", &subj_snaps[f]);
            }
        }
    }

    println!("\n=== divergence report ===");
    let mut first_div: Option<usize> = None;
    // Tally which addresses diverge across ALL frames (to see whether
    // it's one persistent var like the RNG, or spreading corruption).
    let mut addr_hits: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut addr_first: std::collections::BTreeMap<usize, usize> =
        std::collections::BTreeMap::new();
    let mut diverged_frames = 0usize;
    for f in 0..frames.min(subj_snaps.len()).min(ref_snaps.len()) {
        let r = &ref_snaps[f];
        let s = &subj_snaps[f];
        let mut diffs: Vec<(usize, u8, u8)> = Vec::new();
        for a in 0..0x800 {
            if !is_excluded(a) && r[a] != s[a] {
                diffs.push((a, r[a], s[a]));
                *addr_hits.entry(a).or_insert(0) += 1;
                addr_first.entry(a).or_insert(f);
            }
        }
        if diffs.is_empty() {
            continue;
        }
        diverged_frames += 1;
        if first_div.is_none() {
            first_div = Some(f);
            println!(
                "FIRST DIVERGENCE at frame {f}: {} bytes differ",
                diffs.len()
            );
            println!("  first differing addresses:");
            for (a, rv, sv) in diffs.iter().take(16) {
                println!("    ${a:04X}: ref=${rv:02X} subj=${sv:02X}");
            }
        }
    }
    match first_div {
        None => println!("NO DIVERGENCE across {frames} frames — subject matches reference."),
        Some(f) => {
            println!(
                "\n{diverged_frames}/{frames} frames diverged (first at frame {f}). \
                 Persistently-diverging addresses (addr: #frames, first frame):"
            );
            let mut hits: Vec<(usize, usize)> = addr_hits.iter().map(|(a, n)| (*a, *n)).collect();
            hits.sort_by(|a, b| b.1.cmp(&a.1));
            for (a, n) in hits.iter().take(30) {
                let first = addr_first.get(a).copied().unwrap_or(0);
                println!("    ${a:04X}: {n} frames, first={first}", a = a, n = n);
            }
            // Late-onset view: init artifacts diverge from frame 0 and crowd
            // out the signal on long routes. The addresses whose FIRST
            // divergence happens latest are the ones that snowball into
            // end-of-route failures (traps, wrong-state dispatch).
            let mut late: Vec<(usize, usize)> = addr_first
                .iter()
                .map(|(a, f)| (*a, *f))
                .filter(|(_, f)| *f > 0)
                .collect();
            late.sort_by(|a, b| b.1.cmp(&a.1));
            println!("\nLatest-onset divergences (addr, first diverging frame, values then):");
            for (a, f) in late.iter().take(30) {
                let rv = ref_snaps[*f][*a];
                let sv = subj_snaps[*f][*a];
                println!("    ${a:04X}: first={f} ref=${rv:02X} subj=${sv:02X}");
            }
            // Full ascending first-divergence timeline (opt-in): every
            // address that ever diverged, in onset order, with hit counts.
            // Transient one-frame diffs (phase noise) are distinguishable
            // from persistent absences via the hit count.
            if std::env::var("FD_FULL_TIMELINE").is_ok() {
                let mut all: Vec<(usize, usize)> =
                    addr_first.iter().map(|(a, f)| (*a, *f)).collect();
                all.sort_by_key(|(_, f)| *f);
                println!("\nFull first-divergence timeline (addr, first, hits, values at onset):");
                for (a, f) in all {
                    let rv = ref_snaps[f][a];
                    let sv = subj_snaps[f][a];
                    let n = addr_hits.get(&a).copied().unwrap_or(0);
                    println!("    ${a:04X}: first={f} hits={n} ref=${rv:02X} subj=${sv:02X}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use oracle_6502::Bus as _;

    fn test_nes_bus() -> NesBus {
        NesBus::new(
            vec![0u8; 0x8000],
            vec![0u8; 0x2000],
            nes_rom::MapperPolicy::Nrom { prg_len: 0x8000 },
            None,
        )
    }

    fn ppu_write(bus: &mut NesBus, addr: u16, value: u8) {
        bus.read(0x2002); // reset the address latch
        bus.write(0x2006, (addr >> 8) as u8);
        bus.write(0x2006, (addr & 0xFF) as u8);
        bus.write(0x2007, value);
    }

    /// $3F10/$14/$18/$1C are hardware mirrors of $3F00/04/08/0C. SMB parks
    /// the sky color at $3F10; rendering it from an unmirrored slot painted
    /// the reference frames with a black sky (regression: FD_NES_DUMP).
    #[test]
    fn palette_write_to_3f10_mirrors_universal_background() {
        let mut bus = test_nes_bus();
        ppu_write(&mut bus, 0x3F00, 0x0F);
        assert_eq!(bus.palette_ram[0], 0x0F);
        ppu_write(&mut bus, 0x3F10, 0x22);
        assert_eq!(bus.palette_ram[0], 0x22, "$3F10 write must land at $3F00");
        // Non-mirror sprite entries stay where they are written.
        ppu_write(&mut bus, 0x3F11, 0x16);
        assert_eq!(bus.palette_ram[0x11], 0x16);
        assert_eq!(bus.palette_ram[0x01], 0x00);
    }

    /// The last $2005 first-write is the playfield X scroll of the frame
    /// (SMB writes it after the sprite-0 split). $2002 reads reset the
    /// latch so a stray second write must not be captured as X.
    #[test]
    fn scroll_capture_tracks_first_write_only() {
        let mut bus = test_nes_bus();
        bus.read(0x2002);
        bus.write(0x2005, 0x77); // X
        bus.write(0x2005, 0x00); // Y — must not overwrite the captured X
        assert_eq!(bus.scroll_x_last, 0x77);
    }

    #[test]
    fn parses_acceptance_button_event_as_sms_port() {
        let (frame, port) = parse_button_event("80:right,a").unwrap();
        assert_eq!(frame, 80);
        assert_eq!(port & (1 << 3), 0);
        assert_eq!(port & (1 << 4), 0);
        assert_ne!(port & (1 << 5), 0);
    }

    #[test]
    fn timeline_holds_last_script_event() {
        let timeline = ButtonTimeline::from_events(vec![
            (80, buttons_to_sms_port_dc("start").unwrap()),
            (220, buttons_to_sms_port_dc("right").unwrap()),
        ]);
        assert_eq!(timeline.sms_dc_at(79), 0xFF);
        assert_eq!(timeline.sms_dc_at(80) & (1 << 5), 0);
        assert_eq!(timeline.sms_dc_at(219) & (1 << 5), 0);
        assert_eq!(timeline.sms_dc_at(220) & (1 << 3), 0);
        assert_ne!(timeline.sms_dc_at(220) & (1 << 5), 0);
    }

    #[test]
    fn effective_buttons_use_mode_dependent_sms_face_mapping() {
        let timeline =
            ButtonTimeline::from_events(vec![(0, buttons_to_sms_port_dc("start").unwrap())]);
        assert_eq!(effective_nes_buttons(0, &timeline, 0), Buttons::START);
        assert_eq!(effective_nes_buttons(0, &timeline, 1), Buttons::B);
    }
}
