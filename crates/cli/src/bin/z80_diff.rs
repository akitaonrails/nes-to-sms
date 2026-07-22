//! Feature-gated Z80 differential diagnostic scaffold.
//!
//! Phase 1 intentionally uses a tiny flat bus and synthetic programs only. The
//! later CV1/Mednafen path must reuse or extract `trace-sms` `SmsBus` semantics
//! before its results are trusted, because SMS bus reads and device state have
//! side effects that this scaffold does not model.

use std::collections::HashMap;

use rustzx_z80::{Z80, Z80Bus};
use z80_emu::{Bus as LocalBus, Cpu as LocalCpu};

const BANK_SIZE: usize = 0x4000;
const CART_RAM_SIZE: usize = 0x8000;
const RAM_SIZE: usize = 0x2000;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FlatDiffBus {
    mem: Box<[u8; 0x10000]>,
    ports: [u8; 0x100],
}

impl FlatDiffBus {
    fn new() -> Self {
        Self {
            mem: Box::new([0; 0x10000]),
            ports: [0xFF; 0x100],
        }
    }

    fn load(&mut self, addr: u16, bytes: &[u8]) {
        let start = addr as usize;
        self.mem[start..start + bytes.len()].copy_from_slice(bytes);
    }

    fn peek(&self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }

    fn peek_opcodes(&self, pc: u16) -> [u8; 4] {
        [
            self.peek(pc),
            self.peek(pc.wrapping_add(1)),
            self.peek(pc.wrapping_add(2)),
            self.peek(pc.wrapping_add(3)),
        ]
    }
}

impl Default for FlatDiffBus {
    fn default() -> Self {
        Self::new()
    }
}

impl LocalBus for FlatDiffBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }

    fn write(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
    }

    fn in_port(&mut self, port: u8) -> u8 {
        self.ports[port as usize]
    }

    fn out_port(&mut self, port: u8, value: u8) {
        self.ports[port as usize] = value;
    }
}

impl Z80Bus for FlatDiffBus {
    fn read_internal(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }

    fn write_internal(&mut self, addr: u16, data: u8) {
        self.mem[addr as usize] = data;
    }

    fn wait_mreq(&mut self, _addr: u16, _clk: usize) {}
    fn wait_no_mreq(&mut self, _addr: u16, _clk: usize) {}
    fn wait_internal(&mut self, _clk: usize) {}

    fn read_io(&mut self, port: u16) -> u8 {
        self.ports[(port & 0x00FF) as usize]
    }

    fn write_io(&mut self, port: u16, data: u8) {
        self.ports[(port & 0x00FF) as usize] = data;
    }

    fn read_interrupt(&mut self) -> u8 {
        0xFF
    }

    fn reti(&mut self) {}
    fn halt(&mut self, _halted: bool) {}
    fn int_active(&self) -> bool {
        false
    }
    fn nmi_active(&self) -> bool {
        false
    }
    fn pc_callback(&mut self, _addr: u16) {}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct InitialState {
    af: u16,
    bc: u16,
    de: u16,
    hl: u16,
    sp: u16,
    pc: u16,
    af_shadow: u16,
    bc_shadow: u16,
    de_shadow: u16,
    hl_shadow: u16,
    iff1: bool,
    iff2: bool,
    ix: u16,
    iy: u16,
    i: u8,
    r: u8,
    im: u8,
    halted: bool,
    ei_delay: u8,
}

impl InitialState {
    fn unsupported_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        warnings.push("local z80_emu has no IX/IY/I/R/IM model; those fields are initialized in rustzx and reported as reference-only".to_string());
        warnings.push("rustzx-z80 0.16.0 does not expose setters for halted or EI skip-interrupt delay; scaffold cannot initialize those hidden fields".to_string());
        warnings.push("rustzx-z80 0.16.0 public alternate-H/L accessors do not expose an independently trustworthy HL' value; Phase 1 compares AF'/BC'/DE' but reports HL' as unavailable".to_string());
        if self.halted {
            warnings.push(
                "requested initial halted=true cannot be represented in both cores".to_string(),
            );
        }
        if self.ei_delay != 0 {
            warnings.push(
                "requested initial EI-delay cannot be represented in rustzx-z80 public API"
                    .to_string(),
            );
        }
        warnings
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ComparableState {
    pc: u16,
    af: u16,
    bc: u16,
    de: u16,
    hl: u16,
    sp: u16,
    af_shadow: u16,
    bc_shadow: u16,
    de_shadow: u16,
    hl_shadow: Option<u16>,
    iff1: bool,
    iff2: bool,
}

impl ComparableState {
    fn from_local(cpu: &LocalCpu) -> Self {
        Self {
            pc: cpu.pc,
            af: cpu.af(),
            bc: cpu.bc(),
            de: cpu.de(),
            hl: cpu.hl(),
            sp: cpu.sp,
            af_shadow: cpu.af_shadow,
            bc_shadow: cpu.bc_shadow,
            de_shadow: cpu.de_shadow,
            hl_shadow: Some(cpu.hl_shadow),
            iff1: cpu.iff1,
            iff2: cpu.iff2,
        }
    }

    fn from_rustzx(cpu: &Z80) -> Self {
        let regs = &cpu.regs;
        Self {
            pc: regs.get_pc(),
            af: regs.get_af(),
            bc: regs.get_bc(),
            de: regs.get_de(),
            hl: regs.get_hl(),
            sp: regs.get_sp(),
            af_shadow: ((regs.get_acc_alt() as u16) << 8) | regs.get_flags_alt() as u16,
            bc_shadow: ((regs.get_b_alt() as u16) << 8) | regs.get_c_alt() as u16,
            de_shadow: ((regs.get_d_alt() as u16) << 8) | regs.get_e_alt() as u16,
            hl_shadow: None,
            iff1: regs.get_iff1(),
            iff2: regs.get_iff2(),
        }
    }

    fn first_diff(&self, other: &Self) -> Option<&'static str> {
        macro_rules! diff_field {
            ($field:ident) => {
                if self.$field != other.$field {
                    return Some(stringify!($field));
                }
            };
        }
        diff_field!(pc);
        diff_field!(af);
        diff_field!(bc);
        diff_field!(de);
        diff_field!(hl);
        diff_field!(sp);
        diff_field!(af_shadow);
        diff_field!(bc_shadow);
        diff_field!(de_shadow);
        if let (Some(lhs), Some(rhs)) = (self.hl_shadow, other.hl_shadow) {
            if lhs != rhs {
                return Some("hl_shadow");
            }
        }
        diff_field!(iff1);
        diff_field!(iff2);
        None
    }

    fn first_diff_masked(
        &self,
        other: &Self,
        ignore_undocumented_flags: bool,
    ) -> Option<&'static str> {
        if ignore_undocumented_flags
            && self.af != other.af
            && (self.af & !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y))
                == (other.af & !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y))
        {
            let mut lhs = *self;
            let mut rhs = *other;
            lhs.af &= !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y);
            rhs.af &= !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y);
            return lhs.first_diff(&rhs);
        }
        self.first_diff(other)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MismatchReport {
    step: usize,
    pre_local: ComparableState,
    pre_rustzx: ComparableState,
    local_opcodes: [u8; 4],
    rustzx_opcodes: [u8; 4],
    field: String,
    post_local: ComparableState,
    post_rustzx: ComparableState,
}

impl std::fmt::Display for MismatchReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "z80-diff mismatch at step {}\npre local: {:?}\npre rustzx: {:?}\nopcodes local: {:02X?}\nopcodes rustzx: {:02X?}\nfirst differing field: {}\npost local: {:?}\npost rustzx: {:?}",
            self.step,
            self.pre_local,
            self.pre_rustzx,
            self.local_opcodes,
            self.rustzx_opcodes,
            self.field,
            self.post_local,
            self.post_rustzx
        )
    }
}

fn init_local(state: InitialState) -> LocalCpu {
    let mut cpu = LocalCpu::new();
    cpu.set_af(state.af);
    cpu.set_bc(state.bc);
    cpu.set_de(state.de);
    cpu.set_hl(state.hl);
    cpu.sp = state.sp;
    cpu.pc = state.pc;
    cpu.af_shadow = state.af_shadow;
    cpu.bc_shadow = state.bc_shadow;
    cpu.de_shadow = state.de_shadow;
    cpu.hl_shadow = state.hl_shadow;
    cpu.iff1 = state.iff1;
    cpu.iff2 = state.iff2;
    cpu.halted = state.halted;
    cpu.ei_pending = state.ei_delay;
    cpu
}

fn init_rustzx(state: InitialState) -> Z80 {
    let mut cpu = Z80::default();
    cpu.regs.set_af(state.af);
    cpu.regs.set_bc(state.bc);
    cpu.regs.set_de(state.de);
    cpu.regs.set_hl(state.hl);
    cpu.regs.set_sp(state.sp);
    cpu.regs.set_pc(state.pc);
    cpu.regs.set_ix(state.ix);
    cpu.regs.set_iy(state.iy);
    cpu.regs.set_i(state.i);
    cpu.regs.set_r(state.r);
    cpu.regs.set_iff1(state.iff1);
    cpu.regs.set_iff2(state.iff2);
    cpu.set_im(state.im);

    cpu.regs.swap_af_alt();
    cpu.regs.set_af(state.af_shadow);
    cpu.regs.swap_af_alt();
    cpu.regs.exx();
    cpu.regs.set_bc(state.bc_shadow);
    cpu.regs.set_de(state.de_shadow);
    cpu.regs.set_hl(state.hl_shadow);
    cpu.regs.exx();
    cpu
}

fn first_memory_diff(local: &FlatDiffBus, rustzx: &FlatDiffBus) -> Option<u16> {
    local
        .mem
        .iter()
        .zip(rustzx.mem.iter())
        .position(|(a, b)| a != b)
        .map(|idx| idx as u16)
        .or_else(|| {
            local
                .ports
                .iter()
                .zip(rustzx.ports.iter())
                .position(|(a, b)| a != b)
                .map(|idx| 0xFF00 | idx as u16)
        })
}

fn run_lockstep_pair(
    mut local_cpu: LocalCpu,
    mut rustzx_cpu: Z80,
    mut local_bus: FlatDiffBus,
    mut rustzx_bus: FlatDiffBus,
    steps: usize,
) -> Result<(LocalCpu, Z80, FlatDiffBus, FlatDiffBus), MismatchReport> {
    for step in 0..steps {
        let pre_local = ComparableState::from_local(&local_cpu);
        let pre_rustzx = ComparableState::from_rustzx(&rustzx_cpu);
        let local_opcodes = local_bus.peek_opcodes(pre_local.pc);
        let rustzx_opcodes = rustzx_bus.peek_opcodes(pre_rustzx.pc);

        local_cpu
            .step(&mut local_bus)
            .unwrap_or_else(|err| panic!("local z80_emu step failed at {step}: {err:?}"));
        rustzx_cpu.emulate(&mut rustzx_bus);

        let post_local = ComparableState::from_local(&local_cpu);
        let post_rustzx = ComparableState::from_rustzx(&rustzx_cpu);
        let field = post_local
            .first_diff(&post_rustzx)
            .map(str::to_string)
            .or_else(|| {
                first_memory_diff(&local_bus, &rustzx_bus)
                    .map(|addr| format!("bus-visible memory/port at {addr:04X}"))
            });

        if let Some(field) = field {
            return Err(MismatchReport {
                step,
                pre_local,
                pre_rustzx,
                local_opcodes,
                rustzx_opcodes,
                field,
                post_local,
                post_rustzx,
            });
        }
    }

    Ok((local_cpu, rustzx_cpu, local_bus, rustzx_bus))
}

fn run_synthetic(steps: usize) -> Result<(), MismatchReport> {
    let mut bus = FlatDiffBus::new();
    bus.load(
        0x0000,
        &[
            0x01, 0x34, 0x12, // ld bc,$1234
            0x11, 0x00, 0x20, // ld de,$2000
            0x21, 0x00, 0x40, // ld hl,$4000
            0x31, 0x00, 0xD0, // ld sp,$D000
            0x3E, 0x05, // ld a,$05
            0x3C, // inc a
            0x80, // add a,b
            0x77, // ld (hl),a
            0x34, // inc (hl)
            0xC3, 0x12, 0x00, // jp $0012
        ],
    );
    let state = InitialState::default();
    run_lockstep_pair(
        init_local(state),
        init_rustzx(state),
        bus.clone(),
        bus,
        steps,
    )
    .map(|_| ())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReferenceOnlyState {
    ix: u16,
    iy: u16,
    mem_ptr: u16,
    i: u8,
    r: u8,
    im: u8,
    halted: bool,
}

impl ReferenceOnlyState {
    fn from_rustzx(cpu: &Z80) -> Self {
        Self {
            ix: cpu.regs.get_ix(),
            iy: cpu.regs.get_iy(),
            mem_ptr: cpu.regs.get_mem_ptr(),
            i: cpu.regs.get_i(),
            r: cpu.regs.get_r(),
            im: cpu.get_im().into(),
            halted: cpu.is_halted(),
        }
    }
}

#[derive(Clone, Debug)]
struct ImportedState {
    cpu: InitialState,
    main_ram: [u8; RAM_SIZE],
    vram: [u8; 0x4000],
    cram: [u8; 0x20],
    vdp_regs: [u8; 16],
    cart_ram: [u8; CART_RAM_SIZE],
    fcr: [u8; 4],
    warnings: Vec<String>,
}

impl Default for ImportedState {
    fn default() -> Self {
        Self {
            cpu: InitialState::default(),
            main_ram: [0; RAM_SIZE],
            vram: [0; 0x4000],
            cram: [0; 0x20],
            vdp_regs: [0; 16],
            cart_ram: [0; CART_RAM_SIZE],
            fcr: [0, 1, 2, 3],
            warnings: Vec::new(),
        }
    }
}

fn le16(data: &[u8]) -> Option<u16> {
    (data.len() >= 2).then(|| u16::from_le_bytes([data[0], data[1]]))
}

fn parse_top_chunks(data: &[u8]) -> HashMap<String, (usize, usize)> {
    let mut chunks = HashMap::new();
    let mut j = 8;
    while j + 36 <= data.len() {
        let name = &data[j..j + 32];
        if name[0] != 0 && name.iter().all(|&b| b == 0 || (32..127).contains(&b)) {
            let size = u32::from_le_bytes(data[j + 32..j + 36].try_into().unwrap()) as usize;
            let nm: String = name
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();
            if size > 0 && j + 36 + size <= data.len() && nm.len() >= 3 {
                chunks.insert(nm, (j + 36, size));
                j += 36 + size;
                continue;
            }
        }
        j += 1;
    }
    chunks
}

fn parse_subchunks(data: &[u8], off: usize, size: usize) -> HashMap<String, Vec<u8>> {
    let mut out = HashMap::new();
    let end = off.saturating_add(size).min(data.len());
    let mut j = off;
    while j + 5 <= end {
        let nl = data[j] as usize;
        if nl == 0 || nl > 64 || j + 5 + nl > end {
            break;
        }
        let name = String::from_utf8_lossy(&data[j + 1..j + 1 + nl]).to_string();
        let sz = u32::from_le_bytes(data[j + 1 + nl..j + 5 + nl].try_into().unwrap()) as usize;
        let start = j + 5 + nl;
        if start + sz > end {
            break;
        }
        out.insert(name, data[start..start + sz].to_vec());
        j = start + sz;
    }
    out
}

fn import_mednafen_state_bytes(data: &[u8]) -> ImportedState {
    let mut imported = ImportedState::default();
    let chunks = parse_top_chunks(data);
    for required in ["Z80", "MAIN", "VDP", "CART"] {
        if !chunks.contains_key(required) {
            imported
                .warnings
                .push(format!("missing top-level Mednafen chunk {required}"));
        }
    }

    if let Some(&(off, size)) = chunks.get("Z80") {
        let z = parse_subchunks(data, off, size);
        macro_rules! take16 {
            ($name:literal, $field:ident) => {
                if let Some(v) = z.get($name).and_then(|v| le16(v)) {
                    imported.cpu.$field = v;
                } else {
                    imported.warnings.push(format!("missing Z80/{}", $name));
                }
            };
        }
        take16!("AF", af);
        take16!("BC", bc);
        take16!("DE", de);
        take16!("HL", hl);
        take16!("AF_", af_shadow);
        take16!("BC_", bc_shadow);
        take16!("DE_", de_shadow);
        take16!("HL_", hl_shadow);
        take16!("SP", sp);
        take16!("PC", pc);
        if let Some(v) = z.get("IFF1").and_then(|v| v.first()) {
            imported.cpu.iff1 = *v != 0;
        } else {
            imported.warnings.push("missing Z80/IFF1".to_string());
        }
        if let Some(v) = z.get("IFF2").and_then(|v| v.first()) {
            imported.cpu.iff2 = *v != 0;
        } else {
            imported.warnings.push("missing Z80/IFF2".to_string());
        }
        for (name, field) in [("IX", "ix"), ("IY", "iy")] {
            if let Some(v) = z.get(name).and_then(|v| le16(v)) {
                if field == "ix" {
                    imported.cpu.ix = v;
                } else {
                    imported.cpu.iy = v;
                }
            } else {
                imported.warnings.push(format!(
                    "missing Z80/{name}; defaulting reference-only {field}=0"
                ));
            }
        }
        for (name, set) in [
            ("I", 0usize),
            ("R", 1usize),
            ("IM", 2usize),
            ("IMODE", 2usize),
        ] {
            if let Some(v) = z.get(name).and_then(|v| v.first()) {
                match set {
                    0 => imported.cpu.i = *v,
                    1 => imported.cpu.r = *v,
                    _ => imported.cpu.im = (*v).min(2),
                }
            }
        }
        if !z.contains_key("I") {
            imported
                .warnings
                .push("missing Z80/I; defaulting reference-only I=0".to_string());
        }
        if !z.contains_key("R") {
            imported
                .warnings
                .push("missing Z80/R; defaulting reference-only R=0".to_string());
        }
        if !z.contains_key("IM") && !z.contains_key("IMODE") {
            imported
                .warnings
                .push("missing Z80/IM; defaulting reference-only IM=0".to_string());
        }
        imported.warnings.push("restored Z80 AF/BC/DE/HL, AF'/BC'/DE'/HL', SP, PC, IFF1/IFF2; IX/IY/I/R/IM are rustzx-only".to_string());
    }

    if let Some(&(off, size)) = chunks.get("MAIN") {
        let m = parse_subchunks(data, off, size);
        if let Some(ram) = m.get("RAM") {
            for (i, &b) in ram.iter().take(RAM_SIZE).enumerate() {
                imported.main_ram[i] = b;
            }
            imported.warnings.push("restored MAIN/RAM".to_string());
        } else {
            imported.warnings.push("missing MAIN/RAM".to_string());
        }
    }

    if let Some(&(off, size)) = chunks.get("VDP") {
        let v = parse_subchunks(data, off, size);
        if let Some(vram) = v.get("vram") {
            for (i, &b) in vram.iter().take(0x4000).enumerate() {
                imported.vram[i] = b;
            }
        } else {
            imported.warnings.push("missing VDP/vram".to_string());
        }
        if let Some(cram) = v.get("cram") {
            for (i, &b) in cram.iter().take(0x20).enumerate() {
                imported.cram[i] = b;
            }
        } else {
            imported.warnings.push("missing VDP/cram".to_string());
        }
        if let Some(reg) = v.get("reg") {
            for (i, &b) in reg.iter().take(16).enumerate() {
                imported.vdp_regs[i] = b;
            }
        } else {
            imported.warnings.push("missing VDP/reg".to_string());
        }
        imported.warnings.push(
            "restored VDP VRAM/CRAM/registers; VDP latch/code/status timing use unsafe defaults"
                .to_string(),
        );
    }

    if let Some(&(off, size)) = chunks.get("CART") {
        let c = parse_subchunks(data, off, size);
        if let Some(sram) = c.get("sram") {
            for (i, &b) in sram.iter().take(CART_RAM_SIZE).enumerate() {
                imported.cart_ram[i] = b;
            }
        } else {
            imported.warnings.push("missing CART/sram".to_string());
        }
        if let Some(fcr) = c.get("fcr") {
            for (i, &b) in fcr.iter().take(4).enumerate() {
                imported.fcr[i] = b;
            }
        } else {
            imported
                .warnings
                .push("missing CART/fcr; using banks 0/1/2".to_string());
        }
        imported
            .warnings
            .push("restored CART SRAM/FCR mapper registers".to_string());
    }
    imported
        .warnings
        .push("missing/unsafe hidden state: halted, EI delay/skip-interrupt, interrupt schedule, VDP latch/code/status timing unless present in future importer".to_string());
    imported
}

fn load_mednafen_state_file(path: &str) -> Result<ImportedState, String> {
    if path.ends_with(".gz") || path.ends_with(".mcs") {
        return Err(format!(
            "gzip Mednafen states are not decompressed by z80-diff yet; run: gzip -dc {path} > state.raw"
        ));
    }
    let data = std::fs::read(path).map_err(|e| format!("failed to read state {path}: {e}"))?;
    Ok(import_mednafen_state_bytes(&data))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BusEvent(String);

#[derive(Clone, Debug)]
struct SmsDiffBus {
    rom: Vec<u8>,
    slot_bank: [u8; 3],
    mapper_control: u8,
    cart_ram: Box<[u8; CART_RAM_SIZE]>,
    ram: Box<[u8; RAM_SIZE]>,
    vram: Box<[u8; 0x4000]>,
    cram: [u8; 0x20],
    vdp_regs: [u8; 16],
    vdp_addr_high: u8,
    vdp_addr_low: u8,
    vdp_addr_latched: bool,
    vdp_code: u8,
    vdp_status_reads: u32,
    controller_port_dc: u8,
    events: Vec<BusEvent>,
}

impl SmsDiffBus {
    fn new(rom: Vec<u8>, imported: &ImportedState) -> Self {
        Self {
            rom,
            slot_bank: [imported.fcr[1], imported.fcr[2], imported.fcr[3]],
            mapper_control: imported.fcr[0],
            cart_ram: Box::new(imported.cart_ram),
            ram: Box::new(imported.main_ram),
            vram: Box::new(imported.vram),
            cram: imported.cram,
            vdp_regs: imported.vdp_regs,
            vdp_addr_high: 0,
            vdp_addr_low: 0,
            vdp_addr_latched: false,
            vdp_code: 0,
            vdp_status_reads: 0,
            controller_port_dc: 0xFF,
            events: Vec::new(),
        }
    }

    fn rom_byte(&self, bank: u8, offset: u16) -> u8 {
        self.rom
            .get(bank as usize * BANK_SIZE + offset as usize)
            .copied()
            .unwrap_or(0xFF)
    }

    fn slot2_cart_ram_offset(&self, addr: u16) -> Option<usize> {
        if !(0x8000..=0xBFFF).contains(&addr) || self.mapper_control & 0x08 == 0 {
            return None;
        }
        let bank_base = if self.mapper_control & 0x04 != 0 {
            BANK_SIZE
        } else {
            0
        };
        Some(bank_base + usize::from(addr - 0x8000))
    }

    fn peek_u8(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.rom_byte(0, addr),
            0x0400..=0x3FFF => self.rom_byte(self.slot_bank[0], addr),
            0x4000..=0x7FFF => self.rom_byte(self.slot_bank[1], addr - 0x4000),
            0x8000..=0xBFFF => self
                .slot2_cart_ram_offset(addr)
                .map(|o| self.cart_ram[o])
                .unwrap_or_else(|| self.rom_byte(self.slot_bank[2], addr - 0x8000)),
            0xC000..=0xDFFF => self.ram[(addr - 0xC000) as usize],
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize],
            0xFFFC => self.mapper_control,
            0xFFFD => self.slot_bank[0],
            0xFFFE => self.slot_bank[1],
            0xFFFF => self.slot_bank[2],
        }
    }

    fn opcode_bytes(&self, pc: u16) -> [u8; 4] {
        [
            self.peek_u8(pc),
            self.peek_u8(pc.wrapping_add(1)),
            self.peek_u8(pc.wrapping_add(2)),
            self.peek_u8(pc.wrapping_add(3)),
        ]
    }

    fn apply_mapper_write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFFFC => self.mapper_control = value,
            0xFFFD => self.slot_bank[0] = value,
            0xFFFE => self.slot_bank[1] = value,
            0xFFFF => self.slot_bank[2] = value,
            _ => {}
        }
    }

    fn read_mem(&mut self, addr: u16) -> u8 {
        let v = self.peek_u8(addr);
        self.events
            .push(BusEvent(format!("read ${addr:04X} -> ${v:02X}")));
        v
    }

    fn write_mem(&mut self, addr: u16, value: u8) {
        self.events
            .push(BusEvent(format!("write ${addr:04X} = ${value:02X}")));
        match addr {
            0x0000..=0xBFFF => {
                if let Some(offset) = self.slot2_cart_ram_offset(addr) {
                    self.cart_ram[offset] = value;
                }
            }
            0xC000..=0xDFFF => {
                self.ram[(addr - 0xC000) as usize] = value;
            }
            0xE000..=0xFFFB => self.ram[(addr - 0xE000) as usize] = value,
            0xFFFC..=0xFFFF => {
                self.ram[(addr - 0xE000) as usize] = value;
                self.apply_mapper_write(addr, value);
            }
        }
    }

    fn read_port(&mut self, port: u8) -> u8 {
        let value = match port & 0xC1 {
            0x80 => {
                let addr = ((self.vdp_addr_high as u16) << 8) | self.vdp_addr_low as u16;
                let v = self.vram[(addr & 0x3FFF) as usize];
                let new = addr.wrapping_add(1);
                self.vdp_addr_high = (new >> 8) as u8;
                self.vdp_addr_low = new as u8;
                v
            }
            0x81 => {
                self.vdp_status_reads = self.vdp_status_reads.wrapping_add(1);
                self.vdp_addr_latched = false;
                if (self.vdp_status_reads / 100) & 1 == 0 {
                    0x80
                } else {
                    0x00
                }
            }
            0x40 => 0xFF,
            0xC0 => self.controller_port_dc,
            _ => 0xFF,
        };
        self.events
            .push(BusEvent(format!("in ${port:02X} -> ${value:02X}")));
        value
    }

    fn write_port(&mut self, port: u8, value: u8) {
        self.events
            .push(BusEvent(format!("out ${port:02X} = ${value:02X}")));
        if (0x40..=0x7F).contains(&port) {
            return;
        }
        match port & 0xC1 {
            0x80 => {
                let addr = ((self.vdp_addr_high as u16) << 8) | self.vdp_addr_low as u16;
                match self.vdp_code {
                    0 | 1 => self.vram[(addr & 0x3FFF) as usize] = value,
                    3 => self.cram[(addr & 0x1F) as usize] = value,
                    _ => {}
                }
                let new = addr.wrapping_add(1);
                self.vdp_addr_high = (new >> 8) as u8;
                self.vdp_addr_low = new as u8;
            }
            0x81 => {
                if !self.vdp_addr_latched {
                    self.vdp_addr_low = value;
                    self.vdp_addr_latched = true;
                } else {
                    self.vdp_addr_high = value & 0x3F;
                    self.vdp_code = (value >> 6) & 3;
                    if self.vdp_code == 2 {
                        self.vdp_regs[(value & 0x0F) as usize] = self.vdp_addr_low;
                    }
                    self.vdp_addr_latched = false;
                }
            }
            _ => {}
        }
    }

    fn clear_events(&mut self) {
        self.events.clear();
    }

    fn first_visible_diff(&self, other: &Self) -> Option<String> {
        if self.mapper_control != other.mapper_control {
            return Some(format!(
                "mapper_control ${:02X} != ${:02X}",
                self.mapper_control, other.mapper_control
            ));
        }
        if self.slot_bank != other.slot_bank {
            return Some(format!(
                "slot_bank {:?} != {:?}",
                self.slot_bank, other.slot_bank
            ));
        }
        if self.events != other.events {
            return Some(format!(
                "bus events {:?} != {:?}",
                self.events, other.events
            ));
        }
        for (name, lhs, rhs) in [
            ("ram", &self.ram[..], &other.ram[..]),
            ("cart_ram", &self.cart_ram[..], &other.cart_ram[..]),
            ("vram", &self.vram[..], &other.vram[..]),
            ("cram", &self.cram[..], &other.cram[..]),
            ("vdp_regs", &self.vdp_regs[..], &other.vdp_regs[..]),
        ] {
            if let Some(idx) = lhs.iter().zip(rhs.iter()).position(|(a, b)| a != b) {
                return Some(format!(
                    "{name}[${idx:04X}] ${:02X} != ${:02X}",
                    lhs[idx], rhs[idx]
                ));
            }
        }
        if (
            self.vdp_addr_high,
            self.vdp_addr_low,
            self.vdp_addr_latched,
            self.vdp_code,
        ) != (
            other.vdp_addr_high,
            other.vdp_addr_low,
            other.vdp_addr_latched,
            other.vdp_code,
        ) {
            return Some(format!(
                "vdp latch/code ({:02X},{:02X},{},{}) != ({:02X},{:02X},{},{})",
                self.vdp_addr_high,
                self.vdp_addr_low,
                self.vdp_addr_latched,
                self.vdp_code,
                other.vdp_addr_high,
                other.vdp_addr_low,
                other.vdp_addr_latched,
                other.vdp_code
            ));
        }
        if self.controller_port_dc != other.controller_port_dc {
            return Some(format!(
                "controller_port_dc ${:02X} != ${:02X}",
                self.controller_port_dc, other.controller_port_dc
            ));
        }
        None
    }
}

impl LocalBus for SmsDiffBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.read_mem(addr)
    }
    fn write(&mut self, addr: u16, value: u8) {
        self.write_mem(addr, value);
    }
    fn in_port(&mut self, port: u8) -> u8 {
        self.read_port(port)
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.write_port(port, value);
    }
}

impl Z80Bus for SmsDiffBus {
    fn read_internal(&mut self, addr: u16) -> u8 {
        self.read_mem(addr)
    }
    fn write_internal(&mut self, addr: u16, data: u8) {
        self.write_mem(addr, data);
    }
    fn wait_mreq(&mut self, _addr: u16, _clk: usize) {}
    fn wait_no_mreq(&mut self, _addr: u16, _clk: usize) {}
    fn wait_internal(&mut self, _clk: usize) {}
    fn read_io(&mut self, port: u16) -> u8 {
        self.read_port((port & 0x00FF) as u8)
    }
    fn write_io(&mut self, port: u16, data: u8) {
        self.write_port((port & 0x00FF) as u8, data);
    }
    fn read_interrupt(&mut self) -> u8 {
        0xFF
    }
    fn reti(&mut self) {}
    fn halt(&mut self, _halted: bool) {}
    fn int_active(&self) -> bool {
        false
    }
    fn nmi_active(&self) -> bool {
        false
    }
    fn pc_callback(&mut self, _addr: u16) {}
}

#[derive(Clone, Debug)]
struct Symbols(Vec<(u16, String)>);

impl Symbols {
    fn load(path: Option<&str>) -> Self {
        let Some(path) = path else {
            return Self(Vec::new());
        };
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self(Vec::new());
        };
        let mut entries = Vec::new();
        for line in text.lines() {
            let mut label = None;
            let mut addr = None;
            for token in line.split_whitespace() {
                let t = token.trim_matches(|c: char| c == ':' || c == ';');
                if let Some(hex) = t.strip_prefix('$') {
                    addr = u16::from_str_radix(hex, 16).ok();
                } else if t.len() == 4 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                    addr = u16::from_str_radix(t, 16).ok();
                } else if !t.contains('=') && t.chars().any(|c| c == '_' || c.is_ascii_alphabetic())
                {
                    label = Some(t.to_string());
                }
            }
            if let (Some(addr), Some(label)) = (addr, label) {
                entries.push((addr, label));
            }
        }
        entries.sort_by_key(|(addr, _)| *addr);
        Self(entries)
    }

    fn nearest(&self, pc: u16) -> String {
        self.0
            .iter()
            .take_while(|(addr, _)| *addr <= pc)
            .last()
            .map(|(addr, label)| format!("{label}+${:04X}", pc.wrapping_sub(*addr)))
            .unwrap_or_else(|| "<no symbol>".to_string())
    }
}

#[derive(Debug)]
struct CvMismatchReport {
    step: usize,
    pre_local: ComparableState,
    pre_rustzx: ComparableState,
    post_local: ComparableState,
    post_rustzx: ComparableState,
    reference_only: ReferenceOnlyState,
    local_opcodes: [u8; 4],
    rustzx_opcodes: [u8; 4],
    field: String,
    mapper_control: u8,
    slot_bank: [u8; 3],
    local_symbol: String,
    rustzx_symbol: String,
    warnings: Vec<String>,
}

impl std::fmt::Display for CvMismatchReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "z80-diff CV mismatch at step {}", self.step)?;
        writeln!(f, "pre local: {:?}", self.pre_local)?;
        writeln!(f, "pre rustzx: {:?}", self.pre_rustzx)?;
        writeln!(f, "post local: {:?}", self.post_local)?;
        writeln!(f, "post rustzx: {:?}", self.post_rustzx)?;
        writeln!(f, "reference-only rustzx: {:?}", self.reference_only)?;
        writeln!(
            f,
            "opcodes local: {:02X?} ({})",
            self.local_opcodes, self.local_symbol
        )?;
        writeln!(
            f,
            "opcodes rustzx: {:02X?} ({})",
            self.rustzx_opcodes, self.rustzx_symbol
        )?;
        writeln!(
            f,
            "mapper_control=${:02X} slot_bank={:?}",
            self.mapper_control, self.slot_bank
        )?;
        writeln!(f, "first differing field/component: {}", self.field)?;
        writeln!(f, "warnings:")?;
        for warning in &self.warnings {
            writeln!(f, "- {warning}")?;
        }
        Ok(())
    }
}

fn run_cv_mode(
    rom_path: &str,
    state_path: &str,
    sym_path: Option<&str>,
    steps: usize,
    ignore_undocumented_flags: bool,
) -> Result<(), String> {
    let rom = std::fs::read(rom_path).map_err(|e| format!("failed to read ROM {rom_path}: {e}"))?;
    let imported = load_mednafen_state_file(state_path)?;
    let mut warnings = imported.cpu.unsupported_warnings();
    warnings.extend(imported.warnings.clone());
    let symbols = Symbols::load(sym_path);
    let mut local_cpu = init_local(imported.cpu);
    let mut rustzx_cpu = init_rustzx(imported.cpu);
    let mut local_bus = SmsDiffBus::new(rom.clone(), &imported);
    let mut rustzx_bus = SmsDiffBus::new(rom, &imported);
    let mut masked_undocumented_flags = 0usize;
    let mut first_masked_undocumented_flags: Option<(usize, u16, u16, u16)> = None;

    for step in 0..steps {
        local_bus.clear_events();
        rustzx_bus.clear_events();
        let pre_local = ComparableState::from_local(&local_cpu);
        let pre_rustzx = ComparableState::from_rustzx(&rustzx_cpu);
        let local_opcodes = local_bus.opcode_bytes(pre_local.pc);
        let rustzx_opcodes = rustzx_bus.opcode_bytes(pre_rustzx.pc);
        let local_symbol = symbols.nearest(pre_local.pc);
        let rustzx_symbol = symbols.nearest(pre_rustzx.pc);

        let local_err = local_cpu.step(&mut local_bus).err();
        rustzx_cpu.emulate(&mut rustzx_bus);

        let post_local = ComparableState::from_local(&local_cpu);
        let post_rustzx = ComparableState::from_rustzx(&rustzx_cpu);
        if ignore_undocumented_flags
            && post_local.af != post_rustzx.af
            && (post_local.af & !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y))
                == (post_rustzx.af & !u16::from(z80_emu::FLAG_X | z80_emu::FLAG_Y))
        {
            masked_undocumented_flags += 1;
            first_masked_undocumented_flags.get_or_insert((
                step,
                pre_local.pc,
                post_local.af,
                post_rustzx.af,
            ));
        }
        let mut field = post_local
            .first_diff_masked(&post_rustzx, ignore_undocumented_flags)
            .map(str::to_string);
        if field.is_none() {
            field = local_bus.first_visible_diff(&rustzx_bus);
        }
        if let Some(err) = local_err {
            field.get_or_insert_with(|| format!("local z80_emu step error: {err:?}"));
        }
        if let Some(field) = field {
            return Err(CvMismatchReport {
                step,
                pre_local,
                pre_rustzx,
                post_local,
                post_rustzx,
                reference_only: ReferenceOnlyState::from_rustzx(&rustzx_cpu),
                local_opcodes,
                rustzx_opcodes,
                field,
                mapper_control: local_bus.mapper_control,
                slot_bank: local_bus.slot_bank,
                local_symbol,
                rustzx_symbol,
                warnings,
            }
            .to_string());
        }
    }
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    if masked_undocumented_flags > 0 {
        if let Some((step, pc, local_af, rustzx_af)) = first_masked_undocumented_flags {
            eprintln!(
                "warning: masked {masked_undocumented_flags} undocumented X/Y-only AF mismatches; first at step {step} pc=${pc:04X} local_af=${local_af:04X} rustzx_af=${rustzx_af:04X}"
            );
        }
    }
    println!("z80-diff CV lockstep passed for {steps} steps");
    Ok(())
}

#[derive(Debug, Default)]
struct Args {
    rom: Option<String>,
    state: Option<String>,
    sym: Option<String>,
    steps: Option<usize>,
    ignore_undocumented_flags: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--rom" => parsed.rom = args.next(),
            "--state" => parsed.state = args.next(),
            "--sym" => parsed.sym = args.next(),
            "--steps" => parsed.steps = args.next().and_then(|s| s.parse().ok()),
            "--ignore-undocumented-flags" => parsed.ignore_undocumented_flags = true,
            s if !s.starts_with('-') && parsed.steps.is_none() => parsed.steps = s.parse().ok(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(parsed)
}

fn main() {
    let args = parse_args().unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(2);
    });
    if args.rom.is_some() || args.state.is_some() {
        let rom = args.rom.as_deref().unwrap_or_else(|| {
            eprintln!("error: --rom is required for CV mode");
            std::process::exit(2);
        });
        let state = args.state.as_deref().unwrap_or_else(|| {
            eprintln!("error: --state is required for CV mode");
            std::process::exit(2);
        });
        if let Err(report) = run_cv_mode(
            rom,
            state,
            args.sym.as_deref(),
            args.steps.unwrap_or(1000),
            args.ignore_undocumented_flags,
        ) {
            eprintln!("{report}");
            std::process::exit(1);
        }
        return;
    }

    let steps = args.steps.unwrap_or(12);
    let warnings = InitialState::default().unsupported_warnings();
    for warning in warnings {
        eprintln!("warning: {warning}");
    }
    match run_synthetic(steps) {
        Ok(()) => println!("z80-diff synthetic lockstep passed for {steps} steps"),
        Err(report) => {
            eprintln!("{report}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subchunk(name: &str, data: &[u8], out: &mut Vec<u8>) {
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }

    fn topchunk(name: &str, payload: &[u8], out: &mut Vec<u8>) {
        let mut fixed = [0u8; 32];
        fixed[..name.len()].copy_from_slice(name.as_bytes());
        out.extend_from_slice(&fixed);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }

    #[test]
    fn synthetic_program_locksteps() {
        run_synthetic(12).expect("synthetic program should lockstep");
    }

    #[test]
    fn mismatch_report_is_deterministic() {
        let mut local_bus = FlatDiffBus::new();
        let mut rustzx_bus = FlatDiffBus::new();
        local_bus.load(0, &[0x00]); // nop
        rustzx_bus.load(0, &[0x3C]); // inc a
        let state = InitialState::default();

        let report = match run_lockstep_pair(
            init_local(state),
            init_rustzx(state),
            local_bus,
            rustzx_bus,
            1,
        ) {
            Ok(_) => panic!("different opcodes must produce a mismatch"),
            Err(report) => report,
        };

        assert_eq!(report.step, 0);
        assert_eq!(report.local_opcodes[0], 0x00);
        assert_eq!(report.rustzx_opcodes[0], 0x3C);
        assert_eq!(report.field, "af");
    }

    #[test]
    fn undocumented_flag_mask_only_ignores_xy_bits() {
        let mut lhs = ComparableState {
            pc: 0x1000,
            af: 0xD012,
            bc: 0,
            de: 0,
            hl: 0,
            sp: 0,
            af_shadow: 0,
            bc_shadow: 0,
            de_shadow: 0,
            hl_shadow: None,
            iff1: false,
            iff2: false,
        };
        let mut rhs = lhs;
        rhs.af = 0xD01A;
        assert_eq!(lhs.first_diff_masked(&rhs, true), None);

        rhs.af = 0xD01B;
        assert_eq!(lhs.first_diff_masked(&rhs, true), Some("af"));

        lhs.pc = 0x1001;
        assert_eq!(lhs.first_diff_masked(&rhs, true), Some("pc"));
    }

    #[test]
    fn mednafen_state_parser_restores_core_chunks() {
        let mut z80 = Vec::new();
        for (name, value) in [
            ("AF", 0x1234u16),
            ("BC", 0x2345),
            ("DE", 0x3456),
            ("HL", 0x4567),
            ("AF_", 0x5678),
            ("BC_", 0x6789),
            ("DE_", 0x789A),
            ("HL_", 0x89AB),
            ("SP", 0xDFF0),
            ("PC", 0x8123),
            ("IX", 0x1111),
            ("IY", 0x2222),
        ] {
            subchunk(name, &value.to_le_bytes(), &mut z80);
        }
        subchunk("IFF1", &[1], &mut z80);
        subchunk("IFF2", &[0], &mut z80);
        subchunk("I", &[0xAA], &mut z80);
        subchunk("R", &[0x55], &mut z80);
        subchunk("IM", &[2], &mut z80);

        let mut main = Vec::new();
        let mut ram = vec![0u8; RAM_SIZE];
        ram[3] = 0xCC;
        subchunk("RAM", &ram, &mut main);

        let mut vdp = Vec::new();
        let mut vram = vec![0u8; 0x4000];
        vram[7] = 0xDD;
        subchunk("vram", &vram, &mut vdp);
        subchunk("cram", &[0x12; 0x20], &mut vdp);
        subchunk("reg", &[0x34; 16], &mut vdp);

        let mut cart = Vec::new();
        let mut sram = vec![0u8; CART_RAM_SIZE];
        sram[0x10] = 0xEE;
        subchunk("sram", &sram, &mut cart);
        subchunk("fcr", &[0x08, 4, 5, 6], &mut cart);

        let mut raw = vec![0; 8];
        topchunk("Z80", &z80, &mut raw);
        topchunk("MAIN", &main, &mut raw);
        topchunk("VDP", &vdp, &mut raw);
        topchunk("CART", &cart, &mut raw);

        let imported = import_mednafen_state_bytes(&raw);
        assert_eq!(imported.cpu.af, 0x1234);
        assert_eq!(imported.cpu.pc, 0x8123);
        assert!(imported.cpu.iff1);
        assert_eq!(imported.cpu.ix, 0x1111);
        assert_eq!(imported.cpu.im, 2);
        assert_eq!(imported.main_ram[3], 0xCC);
        assert_eq!(imported.vram[7], 0xDD);
        assert_eq!(imported.cram[0], 0x12);
        assert_eq!(imported.vdp_regs[0], 0x34);
        assert_eq!(imported.cart_ram[0x10], 0xEE);
        assert_eq!(imported.fcr, [0x08, 4, 5, 6]);
    }

    #[test]
    fn sms_diff_bus_banking_sram_and_peek_are_non_mutating() {
        let mut imported = ImportedState::default();
        imported.fcr = [0x08, 1, 2, 3];
        imported.cart_ram[0] = 0xA5;
        let mut rom = vec![0xFF; 4 * BANK_SIZE];
        rom[BANK_SIZE + 0x0400] = 0x11;
        rom[2 * BANK_SIZE] = 0x22;
        rom[3 * BANK_SIZE] = 0x33;
        let mut bus = SmsDiffBus::new(rom, &imported);

        assert_eq!(bus.peek_u8(0x0400), 0x11);
        assert_eq!(bus.peek_u8(0x4000), 0x22);
        assert_eq!(bus.peek_u8(0x8000), 0xA5);
        assert!(bus.events.is_empty());

        bus.write_mem(0xFFFF, 1);
        assert_eq!(bus.slot_bank[2], 1);
        bus.mapper_control = 0;
        assert_eq!(bus.peek_u8(0x8000), 0xFF);
    }

    #[test]
    fn dffc_ram_write_does_not_alias_mapper_control() {
        let imported = ImportedState::default();
        let mut bus = SmsDiffBus::new(Vec::new(), &imported);
        bus.write_mem(0xFFFC, 0x08);
        bus.write_mem(0xDFFC, 0x55);

        assert_eq!(bus.peek_u8(0xDFFC), 0x55);
        assert_eq!(bus.mapper_control, 0x08);
    }
}
