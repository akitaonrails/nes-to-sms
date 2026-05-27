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
                // $2005/$2006 write toggle; the rest are side-effect-free
                // for game-state RAM evolution.
                if 0x2000 + (addr & 7) == 0x2005 || 0x2000 + (addr & 7) == 0x2006 {
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

fn run_reference(prg: Vec<u8>, frames: usize, script: &str) -> Vec<[u8; 0x800]> {
    use oracle_6502::Cpu;
    let mut cpu = Cpu::new();
    let mut bus = NesBus::new(prg);
    cpu.reset(&mut bus);

    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for frame in 0..frames {
        bus.buttons = script_buttons(frame, script).0;
        // Frame start: raise VBlank and fire NMI (SMB's NMI self-gates
        // on its own RAM flags, so firing every frame is safe).
        bus.vblank = true;
        cpu.nmi(&mut bus);
        // Run the frame's worth of instructions. The CPU will execute
        // the NMI handler then drop back to the main thread, which
        // mostly spins waiting for the next frame.
        for _ in 0..REF_INSN_PER_FRAME {
            if cpu.step(&mut bus).is_err() {
                break;
            }
        }
        snaps.push(bus.ram);
    }
    snaps
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
}

impl SmsBus {
    fn new(rom: Vec<u8>) -> Self {
        Self {
            rom,
            slot_bank: [0, 1, 2],
            ram: [0; 0x2000],
            port_dc: 0xFF,
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
            0xC000..=0xDFFF => self.ram[(addr - 0xC000) as usize] = value,
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

const SUBJ_INSN_PER_FRAME: usize = 1_500_000;

fn run_subject(rom: Vec<u8>, frames: usize, script: &str) -> Vec<[u8; 0x800]> {
    use z80_emu::Cpu;
    let mut cpu = Cpu::new();
    let mut bus = SmsBus::new(rom);
    cpu.pc = 0x0000;
    cpu.sp = 0xDFF0;

    let mut snaps: Vec<[u8; 0x800]> = Vec::with_capacity(frames);
    for frame in 0..frames {
        bus.port_dc = nes_buttons_to_sms_dc(script_buttons(frame, script));
        // Fire IRQ (IM1 → $0038) if interrupts enabled; the boot
        // irq_handler acks the VDP INT, sets the VBlank flag, and calls
        // the translated NMI — the subject's per-frame driver.
        if cpu.iff1 {
            cpu.sp = cpu.sp.wrapping_sub(2);
            let pc = cpu.pc;
            <SmsBus as z80_emu::Bus>::write(&mut bus, cpu.sp, (pc & 0xFF) as u8);
            <SmsBus as z80_emu::Bus>::write(&mut bus, cpu.sp.wrapping_add(1), (pc >> 8) as u8);
            cpu.pc = 0x0038;
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
        }
        for _ in 0..SUBJ_INSN_PER_FRAME {
            if cpu.halted {
                break;
            }
            if cpu.step(&mut bus).is_err() {
                break;
            }
        }
        // NES RAM equivalent: SMS $C000-$C7FF.
        let mut snap = [0u8; 0x800];
        snap.copy_from_slice(&bus.ram[0..0x800]);
        snaps.push(snap);
    }
    snaps
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
    let ref_snaps = run_reference(prg, frames, &script);

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
    let subj_snaps = run_subject(rom, frames, &script);

    // Compare frame by frame. Report the first divergence and the
    // addresses that differ, focusing on the game-state page $0700-$07FF
    // first (operation mode, task, timers) then the whole $0000-$07FF.
    println!("\n=== divergence report ===");
    let mut first_div: Option<usize> = None;
    for f in 0..frames.min(subj_snaps.len()).min(ref_snaps.len()) {
        let r = &ref_snaps[f];
        let s = &subj_snaps[f];
        if r == s {
            continue;
        }
        first_div = Some(f);
        // Collect differing addresses.
        let mut diffs: Vec<(usize, u8, u8)> = Vec::new();
        for a in 0..0x800 {
            if r[a] != s[a] {
                diffs.push((a, r[a], s[a]));
            }
        }
        println!(
            "FIRST DIVERGENCE at frame {f}: {} of 2048 bytes differ",
            diffs.len()
        );
        // Show the most meaningful game vars first if they differ.
        let key_vars: &[(usize, &str)] = &[
            (0x0770, "OperMode"),
            (0x0772, "OperMode_Task"),
            (0x073C, "ScreenRoutineTask"),
            (0x0778, "DisableScreenFlag/PPUctrl"),
            (0x06D6, "?"),
        ];
        for &(addr, name) in key_vars {
            if r[addr] != s[addr] {
                println!("  ${addr:04X} {name}: ref=${:02X} subj=${:02X}", r[addr], s[addr]);
            }
        }
        // Then the first 24 differing addresses overall.
        println!("  first differing addresses:");
        for (a, rv, sv) in diffs.iter().take(24) {
            println!("    ${a:04X}: ref=${rv:02X} subj=${sv:02X}");
        }
        break;
    }
    match first_div {
        None => println!("NO DIVERGENCE across {frames} frames — subject matches reference."),
        Some(f) => println!("\nFix target: frame {f}. The reference progression above shows what should happen."),
    }
}
