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
        // Start early, then hold Right.
        "start_right" => {
            if (40..45).contains(&frame) {
                Buttons(Buttons::START)
            } else if frame >= 80 {
                Buttons(Buttons::RIGHT)
            } else {
                Buttons(0)
            }
        }
        _ => Buttons(0),
    }
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
    // Controller
    strobe: bool,
    ctrl_shift: u8,
    buttons: u8,
}

impl NesBus {
    fn new(prg: Vec<u8>) -> Self {
        Self {
            ram: [0; 0x800],
            prg,
            vblank: false,
            addr_latch_toggle: false,
            nmi_enabled: false,
            strobe: false,
            ctrl_shift: 0,
            buttons: 0,
        }
    }

    fn prg_read(&self, addr: u16) -> u8 {
        // 32 KiB PRG at $8000-$FFFF; if 16 KiB, mirror — SMB is 32 KiB.
        let off = (addr as usize - 0x8000) % self.prg.len();
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
                        // Reading $2002 clears VBlank + resets the $2005/$2006 toggle.
                        self.vblank = false;
                        self.addr_latch_toggle = false;
                        v
                    }
                    // $2004 OAM data read, $2007 VRAM read: not needed by SMB
                    // game logic for state evolution; return 0.
                    _ => 0,
                }
            }
            0x4016 => {
                // Controller 1 serial read: bit0 = next button bit.
                let bit = self.ctrl_shift & 1;
                if !self.strobe {
                    self.ctrl_shift >>= 1;
                    self.ctrl_shift |= 0x80; // after 8 reads, returns 1s
                }
                0x40 | bit
            }
            0x4017 => 0x40, // controller 2: nothing pressed
            0x8000..=0xFFFF => self.prg_read(addr),
            _ => 0,
        }
    }

    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[(addr & 0x07FF) as usize] = value,
            0x2000..=0x3FFF => {
                // PPU register writes: model only what affects the
                // $2005/$2006 write toggle and the NMI-enable bit; the
                // rest are side-effect-free for game-state RAM evolution.
                let reg = 0x2000 + (addr & 7);
                if reg == 0x2000 {
                    self.nmi_enabled = value & 0x80 != 0;
                }
                if reg == 0x2005 || reg == 0x2006 {
                    self.addr_latch_toggle = !self.addr_latch_toggle;
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
fn run_reference(prg: Vec<u8>, frames: usize, script: &str) -> ([u8; 0x800], Vec<[u8; 0x800]>) {
    use oracle_6502::Cpu;
    let mut cpu = Cpu::new();
    let mut bus = NesBus::new(prg);
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
    let init_snap = bus.ram;
    eprintln!("  ref pre-roll: {pre} insn, nmi_enabled={}", bus.nmi_enabled);

    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for frame in 0..frames {
        bus.buttons = script_buttons(frame, script).0;
        bus.vblank = true;
        if bus.nmi_enabled {
            cpu.nmi(&mut bus);
        }
        for _ in 0..REF_INSN_PER_FRAME {
            if cpu.step(&mut bus).is_err() {
                break;
            }
        }
        snaps.push(bus.ram);
    }
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
    // Debug: when Some, log writes to these NES addresses (as $Cxxx).
    watch: Option<Vec<u16>>,
    watch_log: Vec<(u16, u8)>,
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
                        self.watch_log.push((nes, value));
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
            0x80 => 0x00,       // VDP data port $BE
            0x81 => 0xFF,       // VDP status/control $BF — ack reads
            0xC0 => self.port_dc, // controller port 1 ($DC)
            0xC1 => 0xFF,       // controller port 2 ($DD)
            0x40 => 0xFF,       // H/V counter
            _ => 0xFF,
        }
    }
    fn out_port(&mut self, _port: u8, _value: u8) {
        // VDP / PSG writes don't affect NES game RAM; ignore for the diff.
    }
}

/// Map NES controller buttons to the SMS $DC port (active-low) the way
/// `runtime/input.s` expects: bit0 Up, bit1 Down, bit2 Left, bit3
/// Right, bit4 Button1 (NES A / Select), bit5 Button2 (NES B / Start).
fn nes_buttons_to_sms_dc(b: Buttons) -> u8 {
    let mut pressed = 0u8; // 1 = pressed (we invert at the end)
    if b.0 & Buttons::UP != 0 { pressed |= 1 << 0; }
    if b.0 & Buttons::DOWN != 0 { pressed |= 1 << 1; }
    if b.0 & Buttons::LEFT != 0 { pressed |= 1 << 2; }
    if b.0 & Buttons::RIGHT != 0 { pressed |= 1 << 3; }
    if b.0 & (Buttons::A | Buttons::SELECT) != 0 { pressed |= 1 << 4; }
    if b.0 & (Buttons::B | Buttons::START) != 0 { pressed |= 1 << 5; }
    !pressed // active-low
}

const SUBJ_INSN_PER_FRAME: usize = 2_000_000;
const SUBJ_PREROLL_CAP: usize = 8_000_000;
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
    (0x0100..0x0200).contains(&addr)
}

/// Returns (init_snapshot, per_frame_snapshots). Mirrors run_reference:
/// pre-roll through SMS boot + SMB's translated reset-init until SMB
/// enables NMI (PPUCTRL shadow $CB08 bit 7), capture init RAM, then
/// fire one IRQ per frame (gated on the same NMI-enable bit).
fn run_subject(rom: Vec<u8>, frames: usize, script: &str) -> ([u8; 0x800], Vec<[u8; 0x800]>) {
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
        for _ in 0..SUBJ_INSN_PER_FRAME {
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
    let init_snap = snap_nes_ram(&bus);
    eprintln!(
        "  subj pre-roll: {pre} insn over {pre_frames} frames, PC=${:04X} nmi_enabled={} $C772={:02X}",
        cpu.pc,
        nmi_enabled(&bus),
        bus.ram[0x772]
    );

    let debug_frame: Option<usize> = std::env::var("FD_DEBUG_FRAME")
        .ok()
        .and_then(|s| s.parse().ok());

    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for _frame in 0..frames {
        bus.port_dc = nes_buttons_to_sms_dc(script_buttons(_frame, script));
        if Some(_frame) == debug_frame {
            bus.watch = Some(vec![0x0000, 0x07A7, 0x07A8]);
            bus.watch_log.clear();
        }
        fire_irq(&mut cpu, &mut bus);
        for _ in 0..SUBJ_INSN_PER_FRAME {
            if cpu.halted || cpu.step(&mut bus).is_err() {
                break;
            }
        }
        if Some(_frame) == debug_frame {
            eprintln!("  [debug] frame {_frame} writes to $00/$07A7/$07A8:");
            for (a, v) in bus.watch_log.iter().take(40) {
                eprintln!("    ${a:04X} <- ${v:02X}");
            }
            bus.watch = None;
        }
        snaps.push(snap_nes_ram(&bus));
    }
    (init_snap, snaps)
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let nes_path = PathBuf::from(args.get(1).expect("usage: frame-diff <smb.nes> <out.sms> [--frames N] [--script S]"));
    let _sms_path = args.get(2).cloned();

    let mut frames = 120usize;
    let mut script = "none".to_string();
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

    eprintln!("Reference: running SMB PRG ({} bytes) for {frames} frames, script={script}", prg.len());
    let (ref_init, ref_snaps) = run_reference(prg, frames, &script);

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
            println!(
                "  {f:3} | OperMode=${a770:02X} Task=${a772:02X} ScreenRtn=${a73c:02X}"
            );
            last_770 = a770;
            last_772 = a772;
        }
    }

    if ref_only {
        return;
    }

    let sms_path = _sms_path.expect("need <out.sms> for subject side");
    let rom = std::fs::read(&sms_path).expect("read sms rom");
    eprintln!("Subject: running SMS ROM ({} bytes) for {frames} frames", rom.len());
    let (subj_init, subj_snaps) = run_subject(rom, frames, &script);

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
    // Debug: $07A7 trajectory both sides.
    eprint!("  [traj] ref $07A7:");
    for f in 0..frames.min(ref_snaps.len()).min(8) { eprint!(" {:02X}", ref_snaps[f][0x7A7]); }
    eprintln!();
    eprint!("  [traj] subj $07A7:");
    for f in 0..frames.min(subj_snaps.len()).min(8) { eprint!(" {:02X}", subj_snaps[f][0x7A7]); }
    eprintln!();

    println!("\n=== divergence report ===");
    let mut first_div: Option<usize> = None;
    // Tally which addresses diverge across ALL frames (to see whether
    // it's one persistent var like the RNG, or spreading corruption).
    let mut addr_hits: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut diverged_frames = 0usize;
    for f in 0..frames.min(subj_snaps.len()).min(ref_snaps.len()) {
        let r = &ref_snaps[f];
        let s = &subj_snaps[f];
        let mut diffs: Vec<(usize, u8, u8)> = Vec::new();
        for a in 0..0x800 {
            if !is_excluded(a) && r[a] != s[a] {
                diffs.push((a, r[a], s[a]));
                *addr_hits.entry(a).or_insert(0) += 1;
            }
        }
        if diffs.is_empty() {
            continue;
        }
        diverged_frames += 1;
        if first_div.is_none() {
            first_div = Some(f);
            println!("FIRST DIVERGENCE at frame {f}: {} bytes differ", diffs.len());
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
                 Persistently-diverging addresses (addr: #frames):"
            );
            let mut hits: Vec<(usize, usize)> = addr_hits.into_iter().collect();
            hits.sort_by(|a, b| b.1.cmp(&a.1));
            for (a, n) in hits.iter().take(30) {
                println!("    ${a:04X}: {n} frames", a = a, n = n);
            }
        }
    }
}
