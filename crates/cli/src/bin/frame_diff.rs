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
}

impl ButtonTimeline {
    fn builtin(name: String) -> Self {
        Self {
            builtin: name,
            events: Vec::new(),
        }
    }

    fn from_events(events: Vec<(usize, u8)>) -> Self {
        Self {
            builtin: "buttons-script".to_string(),
            events,
        }
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
    events.sort_by_key(|(frame, _)| *frame);
    Ok(events)
}

// ---------------------------------------------------------------------------
// Reference: NES system bus over oracle_6502
// ---------------------------------------------------------------------------

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
    /// UxROM: selected 16 KiB bank at $8000-$BFFF.
    prg_bank: u8,
    /// CHR-RAM store for pattern-space $2007 writes (ground truth).
    chr_ram: Vec<u8>,
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

    fn new(prg: Vec<u8>, chr: Vec<u8>) -> Self {
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
            prg_bank: 0,
            chr_ram: vec![0u8; 0x3000],
        }
    }

    fn prg_read(&self, addr: u16) -> u8 {
        if self.prg.len() > 32 * 1024 {
            // Banked (UxROM model): $8000-$BFFF = selected 16 KiB bank,
            // $C000-$FFFF = fixed last bank.
            let off = if addr >= 0xC000 {
                self.prg.len() - 0x4000 + (addr as usize - 0xC000)
            } else {
                (self.prg_bank as usize * 0x4000 + (addr as usize - 0x8000)) % self.prg.len()
            };
            self.prg[off]
        } else {
            // 32 KiB PRG at $8000-$FFFF; if 16 KiB, mirror.
            let off = (addr as usize - 0x8000) % self.prg.len();
            self.prg[off]
        }
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
                        if self.ppu_mask & 0x18 != 0 {
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
                        self.ppu_read_buffer = if a < 0x2000 {
                            *self.chr.get(a).unwrap_or(&0)
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
                    if (a as usize) < self.chr_ram.len() {
                        self.chr_ram[a as usize] = value;
                    }
                    // Writes advance the VRAM address like reads do.
                    let inc = if self.ppu_ctrl & 0x04 != 0 { 32 } else { 1 };
                    self.ppu_addr = self.ppu_addr.wrapping_add(inc);
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
            0x8000..=0xFFFF => {
                // UxROM mapper register: any write selects the window bank.
                if self.prg.len() > 32 * 1024 {
                    let nbanks = (self.prg.len() / 0x4000) as u8;
                    self.prg_bank = value % nbanks;
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Reference frame stepper
// ---------------------------------------------------------------------------

const REF_INSN_PER_FRAME: usize = 200_000;
const REF_PREROLL_CAP: usize = 2_000_000;

/// Returns (init_snapshot, per_frame_snapshots). The init snapshot is
/// RAM at the moment SMB first enables NMI ($2000 bit 7) — i.e. when
/// reset-init is essentially done and the game wants frames. From
/// there each frame fires one NMI.
fn run_reference(
    prg: Vec<u8>,
    chr: Vec<u8>,
    frames: usize,
    timeline: &ButtonTimeline,
) -> ([u8; 0x800], Vec<[u8; 0x800]>) {
    use oracle_6502::Cpu;
    let mut cpu = Cpu::new();
    let mut bus = NesBus::new(prg, chr);
    cpu.reset(&mut bus);

    // Pre-roll: run reset-init until NMI is enabled. SMB polls $2002 for
    // VBlank during this phase, so keep VBlank available.
    bus.vblank = true;
    let mut pre = 0usize;
    while !bus.nmi_enabled && pre < REF_PREROLL_CAP {
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
    let mut call_log: Vec<(usize, u8, u16, u16)> = Vec::new();
    let mut bank_entry_set: std::collections::BTreeSet<(u8, u16)> = Default::default();
    let mut nmi_latched = bus.nmi_enabled;
    let mut nmi_fires = 0usize;
    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for frame in 0..frames {
        bus.buttons = effective_nes_buttons(frame, timeline, bus.ram[0x0770]);
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
        for _ in 0..REF_INSN_PER_FRAME {
            if debug_writes {
                bus.last_pc = cpu.pc;
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
        snaps.push(bus.ram);
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
        for row in 6..14 {
            let base = 0x2000 + row * 32;
            let hex: Vec<String> = bus.chr_ram[base..base + 32]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            eprintln!("REF NT row {row:02}: {}", hex.join(" "));
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
    // Controller: SMS port $DC, active-low (1 = released).
    port_dc: u8,
    // Debug: when Some, log writes to these NES addresses (as $Cxxx),
    // capturing the CPU PC at the time of the write.
    watch: Option<Vec<u16>>,
    watch_log: Vec<(u16, u8, u16)>,
    watch_bank_log: Vec<u8>,
    last_pc: u16,
}

impl SmsBus {
    fn new(rom: Vec<u8>) -> Self {
        Self {
            rom,
            slot_bank: [0, 1, 2],
            ram: [0; 0x2000],
            port_dc: 0xFF,
            watch: None,
            watch_log: Vec::new(),
            watch_bank_log: Vec::new(),
            last_pc: 0,
        }
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
            0x8000..=0xBFFF => self.rom_byte(self.slot_bank[2], addr - 0x8000),
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
            0x0000..=0xBFFF => {} // ROM
            0xC000..=0xDFFF => {
                self.ram[(addr - 0xC000) as usize] = value;
                if let Some(w) = &self.watch {
                    let nes = addr - 0xC000;
                    if w.contains(&nes) {
                        self.watch_log.push((nes, value, self.last_pc));
                    }
                }
            }
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize] = value,
            0xFFFC => {}
            0xFFFD => self.slot_bank[0] = value,
            0xFFFE => self.slot_bank[1] = value,
            0xFFFF => self.slot_bank[2] = value,
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port & 0xC1 {
            0x80 => 0x00,         // VDP data port $BE
            0x81 => 0xFF,         // VDP status/control $BF — ack reads
            0xC0 => self.port_dc, // controller port 1 ($DC)
            0xC1 => 0xFF,         // controller port 2 ($DD)
            0x40 => 0xFF,         // H/V counter
            _ => 0xFF,
        }
    }
    fn out_port(&mut self, _port: u8, _value: u8) {
        // VDP / PSG writes don't affect NES game RAM; ignore for the diff.
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
fn run_subject(
    rom: Vec<u8>,
    frames: usize,
    timeline: &ButtonTimeline,
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
        for _ in 0..subj_insn_per_frame() {
            if cpu.halted {
                break;
            }
            bus.last_pc = cpu.pc;
            if cpu.step(&mut bus).is_err() {
                break;
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
    let prg = image.prg.to_vec();

    let (timeline, script_desc) = if let Some(path) = buttons_script {
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
    let (ref_init, ref_snaps) = run_reference(prg, image.chr.to_vec(), frames, &timeline);

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
    let (subj_init, subj_snaps) = run_subject(rom, frames, &timeline);

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

    // Sprite-data differential: dump $0200-$023F (16 OAM entries: Y,tile,
    // attr,X) for ref and subj at FD_DUMP_SPRITES=<frame>, to compare the
    // sprite tiles SMB builds vs what our translation builds.
    if let Ok(fs) = std::env::var("FD_DUMP_SPRITES") {
        if let Ok(f) = fs.parse::<usize>() {
            let dump = |label: &str, snap: &[u8; 0x800]| {
                eprint!("  [sprites @{f}] {label}:");
                for i in 0..64 {
                    eprint!(" {:02X}", snap[0x200 + i]);
                }
                eprintln!();
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
            let mut hits: Vec<(usize, usize)> = addr_hits.into_iter().collect();
            hits.sort_by(|a, b| b.1.cmp(&a.1));
            for (a, n) in hits.iter().take(30) {
                let first = addr_first.get(a).copied().unwrap_or(0);
                println!("    ${a:04X}: {n} frames, first={first}", a = a, n = n);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
