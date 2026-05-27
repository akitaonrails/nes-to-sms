//! Differential test harness: 6502 oracle vs Z80 emulator.
//!
//! Given a lifted IR routine and the original PRG bytes, the harness:
//! 1. Lowers the routine into a self-contained Z80 program (the routine
//!    body plus Rust-emitted implementations of the runtime helpers it
//!    calls into).
//! 2. For each randomized initial state, runs the original 6502 code in
//!    `oracle_6502` and the lowered Z80 in `z80_emu` against the same
//!    NES-RAM / Z80-RAM contents.
//! 3. Compares final register and RAM state.
//!
//! The runtime helpers emitted here intentionally duplicate the behavior
//! of `runtime/*.s`. Both implementations should match; a divergence is
//! a bug in one of them, surfaced by the harness.

use std::collections::BTreeMap;

use ir::{Op, Routine};
use lower::{LowerOptions, sms_layout};
use oracle_6502::Bus as OracleBus;
use z80_emit::Program;
use z80_emu::Bus as Z80Bus;

mod runtime_stubs;
pub use runtime_stubs::{REQUIRED_HELPERS, emit_runtime_helpers};

/// Result of running one routine through N random initial states.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub routine_name: String,
    pub routine_entry: u16,
    pub vectors_run: usize,
    pub vectors_passed: usize,
    pub failures: Vec<VectorFailure>,
    pub skipped_reason: Option<String>,
}

impl ValidationResult {
    pub fn is_green(&self) -> bool {
        self.skipped_reason.is_none()
            && self.vectors_run > 0
            && self.vectors_passed == self.vectors_run
    }
}

#[derive(Debug, Clone)]
pub struct VectorFailure {
    pub seed: u64,
    pub initial: InitialState,
    pub oracle: FinalState,
    pub z80: FinalState,
    pub diffs: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct InitialState {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub p: u8,
    pub sp: u8,
    /// Seed used to generate zero-page and RAM contents.
    pub mem_seed: u64,
}

#[derive(Debug, Clone)]
pub struct FinalState {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    /// 6502 status bits in canonical NV-BDIZC layout.
    pub p: u8,
    pub sp: u8,
    /// Zero-page snapshot (256 bytes).
    pub zp: Vec<u8>,
    /// NES RAM beyond zero page ($0200-$07FF) snapshot.
    pub ram: Vec<u8>,
}

/// Whether a routine is eligible for differential validation. Routines
/// that touch hardware (PPU/APU/OAM-DMA/controller/mapper) or do
/// indirect dispatch can't be validated cleanly without a runtime, and
/// we report them as `skipped` rather than red.
pub fn classify_routine(routine: &Routine) -> Option<String> {
    // The harness needs the routine to end with RTS so `run_until_rts`
    // and `run_until_ret` both terminate cleanly. Routines whose tails
    // were trimmed by the cli's overlap heuristic fall through past
    // their last lifted op into garbage and produce meaningless diffs.
    // Require an Rts somewhere in the ops list as a sanity gate.
    // The routine must contain a terminator (RTS, RTI, or JMP). A
    // routine that tail-calls another via JMP is fine — the called
    // routine's RTS unwinds the validation runner's depth correctly.
    if !routine.ops.iter().any(|op| {
        matches!(
            op,
            Op::Rts | Op::Rti | Op::Jmp { .. } | Op::JmpIndirect { .. } | Op::JumpEngineCall { .. }
        )
    }) {
        return Some("no terminal RTS/JMP (trimmed range or fall-through)".into());
    }
    for op in &routine.ops {
        match op {
            Op::PpuWrite { .. }
            | Op::PpuRead { .. }
            | Op::OamDmaWrite { .. }
            | Op::ApuWrite { .. }
            | Op::ApuRead { .. }
            | Op::ControllerRead { .. }
            | Op::MapperWrite { .. } => {
                return Some("hardware access".into());
            }
            Op::JmpIndirect { .. } => return Some("indirect jump".into()),
            Op::JsrUnknown { .. } => return Some("unresolved jsr".into()),
            Op::Unsupported { reason, .. } => {
                return Some(format!("unsupported op: {reason}"));
            }
            Op::Jam { .. } => return Some("jam opcode".into()),
            Op::Brk => return Some("brk".into()),
            Op::Jsr { target } | Op::Jmp { target } | Op::BranchIf { target, .. } => {
                // External Jsr/Jmp/BranchIf would land on stub `ret`s
                // in the harness; the oracle follows the real callee.
                // The resulting diff is a false-positive failure, so
                // skip these routines.
                if !routine.branch_labels.contains(target) && target != &routine.name {
                    return Some(format!("external transfer to {target}"));
                }
            }
            Op::JumpEngineCall { .. } => {
                // Dispatches to non-local labels by definition (each
                // game-mode handler). Skip; correctness of the dispatch
                // is validated end-to-end in trace_sms, not per-routine.
                return Some("jump engine dispatch".into());
            }
            // PrgRom indexed reads are now diff'd — both oracle and z80
            // bus have the full PRG embedded at NES addresses $8000+.
            // (Lower emits raw base for PrgRom; the z80 bus exposes
            // PRG at those addresses inside the harness.)
            Op::LdaMem { .. }
            | Op::LdxMem { .. }
            | Op::LdyMem { .. }
            | Op::AdcMem { .. }
            | Op::SbcMem { .. }
            | Op::AndMem { .. }
            | Op::OraMem { .. }
            | Op::EorMem { .. }
            | Op::CmpMem { .. }
            | Op::CpxMem { .. }
            | Op::CpyMem { .. }
            | Op::BitMem { .. } => {}
            _ => {}
        }
    }
    None
}

/// Validate every routine in `routines` against the 6502 oracle. All
/// routines are lowered into a single Z80 program so cross-routine
/// `JSR`/`JMP` calls resolve, enabling validation of tail-calling
/// routines that previously failed the "no terminal RTS" gate. Returns
/// one ValidationResult per input routine in the same order.
///
/// `prg` is the full NES PRG (NROM mapped at $8000..=$FFFF).
pub fn validate_program(
    prg: &[u8],
    routines: &[Routine],
    vector_count: usize,
) -> Vec<ValidationResult> {
    // Lower every routine together so JSR/JMP between them resolves.
    let entry_map = match lower_all_for_validation(routines) {
        Ok(m) => m,
        Err(reason) => {
            // Total failure — every routine becomes a skip with the
            // same reason.
            return routines
                .iter()
                .map(|r| ValidationResult {
                    routine_name: r.name.clone(),
                    routine_entry: r.entry,
                    vectors_run: 0,
                    vectors_passed: 0,
                    failures: vec![],
                    skipped_reason: Some(format!("program lowering failed: {reason}")),
                })
                .collect();
        }
    };
    let z80_bytes = entry_map.bytes;

    routines
        .iter()
        .map(|r| {
            if let Some(reason) = classify_routine(r) {
                return ValidationResult {
                    routine_name: r.name.clone(),
                    routine_entry: r.entry,
                    vectors_run: 0,
                    vectors_passed: 0,
                    failures: vec![],
                    skipped_reason: Some(reason),
                };
            }
            let Some(&entry_addr) = entry_map.entries.get(&r.entry) else {
                return ValidationResult {
                    routine_name: r.name.clone(),
                    routine_entry: r.entry,
                    vectors_run: 0,
                    vectors_passed: 0,
                    failures: vec![],
                    skipped_reason: Some("entry label not found after program lowering".into()),
                };
            };
            run_diff_vectors(prg, r, &z80_bytes, entry_addr, vector_count)
        })
        .collect()
}

#[derive(Debug)]
struct LoweredProgram {
    bytes: Vec<u8>,
    /// 6502 PRG address → Z80 address of routine entry.
    entries: std::collections::BTreeMap<u16, u16>,
}

fn lower_all_for_validation(routines: &[Routine]) -> Result<LoweredProgram, String> {
    let mut program = Program::new();
    program.section("validation");
    program.org(0x4000);

    emit_runtime_helpers(&mut program);

    // Helper routines bracket: track each routine's entry address.
    let mut entries = std::collections::BTreeMap::new();
    let mut emitted_names: std::collections::BTreeSet<String> = Default::default();

    let opts = LowerOptions {
        profile: None,
        emit_source_comments: false,
    };

    for r in routines {
        let addr = program.current_addr();
        // Emit an L_XXXX alias unless the routine itself will (when the
        // entry is a branch target inside its body).
        let auto = format!("L_{:04X}", r.entry);
        let routine_emits_auto = r.branch_labels.contains(&auto);
        if !routine_emits_auto && !emitted_names.contains(&auto) {
            program.label(&auto);
            emitted_names.insert(auto);
        }
        emitted_names.insert(r.name.clone());
        for bl in &r.branch_labels {
            emitted_names.insert(bl.clone());
        }
        if let Err(e) = lower::lower_routine(&mut program, r, &opts) {
            return Err(format!("{}: {e}", r.name));
        }
        entries.insert(r.entry, addr);
    }

    // Stub external references so finish() resolves cleanly.
    let unresolved = program.unresolved_labels();
    if !unresolved.is_empty() {
        program.section("validation_unresolved");
        for ext in &unresolved {
            program.label(ext);
            program.ret(); // unstubbed external callee = no-op return
        }
    }

    let build = program.finish().map_err(|e| format!("{e:?}"))?;
    Ok(LoweredProgram {
        bytes: build.bytes,
        entries,
    })
}

fn run_diff_vectors(
    prg: &[u8],
    routine: &Routine,
    z80_bytes: &[u8],
    entry_addr: u16,
    vector_count: usize,
) -> ValidationResult {
    let prg_offset = routine.entry as usize - 0x8000;
    let routine_end = approx_routine_end(routine);
    let prg_end = (routine_end as usize - 0x8000).min(prg.len());
    let routine_bytes = &prg[prg_offset..prg_end.max(prg_offset + 1)];

    let mut failures = Vec::new();
    let mut passed = 0;
    for seed in 0..vector_count as u64 {
        let init = gen_initial_state(seed, routine);
        let oracle_final = run_oracle_full_prg(prg, routine.entry, routine_bytes, init);
        let z80_final = run_z80(z80_bytes, entry_addr, init);
        let diffs = diff_state(&oracle_final, &z80_final);
        if diffs.is_empty() {
            passed += 1;
        } else {
            failures.push(VectorFailure {
                seed,
                initial: init,
                oracle: oracle_final,
                z80: z80_final,
                diffs,
            });
        }
    }

    ValidationResult {
        routine_name: routine.name.clone(),
        routine_entry: routine.entry,
        vectors_run: vector_count,
        vectors_passed: passed,
        failures,
        skipped_reason: None,
    }
}

#[allow(dead_code)]
fn _validate_program_keep_for_future() {}

/// Run the 6502 oracle from `entry` with the FULL PRG loaded. Used by
/// `validate_routine` so JSR/JMP into routines outside the validated
/// one's range follow real bytes instead of returning to garbage.
fn run_oracle_full_prg(
    prg: &[u8],
    entry: u16,
    _routine_bytes_unused: &[u8],
    init: InitialState,
) -> FinalState {
    let mut bus = oracle_6502::FlatBus::new();
    // Load the full PRG at NROM base.
    bus.load(0x8000, prg);
    // Seed zero page and RAM mirror with the same random bytes.
    let mut zp = [0u8; 0x100];
    let mut ram = [0u8; 0x600];
    fill_memory_with_seed(&mut zp, init.mem_seed);
    fill_memory_with_seed(&mut ram, init.mem_seed.wrapping_add(1));
    for (i, b) in zp.iter().enumerate() {
        bus.write(i as u16, *b);
    }
    for (i, b) in ram.iter().enumerate() {
        bus.write(0x0200 + i as u16, *b);
    }
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = entry;
    cpu.a = init.a;
    cpu.x = init.x;
    cpu.y = init.y;
    cpu.p = init.p;
    cpu.sp = init.sp.wrapping_sub(2);
    let hi_slot = 0x0100u16 + init.sp as u16;
    let lo_slot = 0x0100u16 + init.sp.wrapping_sub(1) as u16;
    bus.write(hi_slot, 0xFF);
    bus.write(lo_slot, 0xFE);
    let _ = cpu.run_until_rts(&mut bus, 200_000);

    let mut zp_out = vec![0u8; 0x100];
    let mut ram_out = vec![0u8; 0x600];
    for i in 0..0x100usize {
        zp_out[i] = bus.read(i as u16);
    }
    for i in 0..0x600usize {
        ram_out[i] = bus.read(0x0200 + i as u16);
    }
    FinalState {
        a: cpu.a,
        x: cpu.x,
        y: cpu.y,
        p: cpu.p,
        sp: cpu.sp,
        zp: zp_out,
        ram: ram_out,
    }
}

/// Validate a single routine over `vector_count` random initial states.
///
/// `prg` is the full NES PRG (NROM mapped at $8000..=$FFFF).
pub fn validate_routine(prg: &[u8], routine: &Routine, vector_count: usize) -> ValidationResult {
    if let Some(reason) = classify_routine(routine) {
        return ValidationResult {
            routine_name: routine.name.clone(),
            routine_entry: routine.entry,
            vectors_run: 0,
            vectors_passed: 0,
            failures: vec![],
            skipped_reason: Some(reason),
        };
    }

    // Lower the routine into a self-contained Z80 program.
    let (z80_bytes, entry_addr) = match lower_for_validation(routine) {
        Ok(out) => out,
        Err(reason) => {
            return ValidationResult {
                routine_name: routine.name.clone(),
                routine_entry: routine.entry,
                vectors_run: 0,
                vectors_passed: 0,
                failures: vec![],
                skipped_reason: Some(format!("lowering failed: {reason}")),
            };
        }
    };

    let prg_offset = routine.entry as usize - 0x8000;
    let routine_end = approx_routine_end(routine);
    let prg_end = (routine_end as usize - 0x8000).min(prg.len());
    let routine_bytes = &prg[prg_offset..prg_end.max(prg_offset + 1)];

    // Load the FULL PRG into the oracle so PrgRom reads find real
    // bytes (matching the z80 side which now embeds PRG at $8000).
    let _ = routine_bytes;

    let mut failures = Vec::new();
    let mut passed = 0;
    for seed in 0..vector_count as u64 {
        let init = gen_initial_state(seed, routine);
        let oracle_final = run_oracle_full_prg(prg, routine.entry, prg, init);
        let z80_final = run_z80_with_prg(&z80_bytes, entry_addr, init, Some(prg));
        let diffs = diff_state(&oracle_final, &z80_final);
        if diffs.is_empty() {
            passed += 1;
        } else {
            failures.push(VectorFailure {
                seed,
                initial: init,
                oracle: oracle_final,
                z80: z80_final,
                diffs,
            });
        }
    }

    ValidationResult {
        routine_name: routine.name.clone(),
        routine_entry: routine.entry,
        vectors_run: vector_count,
        vectors_passed: passed,
        failures,
        skipped_reason: None,
    }
}

fn approx_routine_end(routine: &Routine) -> u16 {
    // Find the largest PC among Source markers and add 3 for worst-case
    // instruction size. Upper bound — over-allocates a couple of bytes
    // but we only slice PRG for the oracle, which won't read past the
    // routine's RTS.
    let max_pc = routine
        .ops
        .iter()
        .filter_map(|op| match op {
            Op::Source { pc, .. } => Some(*pc),
            _ => None,
        })
        .max()
        .unwrap_or(routine.entry);
    max_pc.saturating_add(3)
}

fn lower_for_validation(routine: &Routine) -> Result<(Vec<u8>, u16), String> {
    let mut program = Program::new();
    program.section("validation");
    program.org(0x4000);

    emit_runtime_helpers(&mut program);

    let entry_addr = program.current_addr();
    program.label("validation_entry");

    let opts = LowerOptions {
        profile: None,
        emit_source_comments: false,
    };
    lower::lower_routine(&mut program, routine, &opts).map_err(|e| format!("{e}"))?;

    // Stub any label the routine references but doesn't define (external
    // JSR/JMP/branch targets). For the harness these become `ret`-stubs
    // so finish() resolves cleanly; the routine still validates against
    // the oracle for the portion it executes before any tail-call exit.
    let unresolved = program.unresolved_labels();
    if !unresolved.is_empty() {
        program.section("validation_unresolved");
        for ext in &unresolved {
            program.label(ext);
            program.ret();
        }
    }

    let build = program.finish().map_err(|e| format!("{e:?}"))?;
    Ok((build.bytes, entry_addr))
}

// ─── Initial state generation ────────────────────────────────────────────────

fn gen_initial_state(seed: u64, _routine: &Routine) -> InitialState {
    let mut rng = SimpleRng::new(seed);
    InitialState {
        a: rng.next() as u8,
        x: rng.next() as u8,
        y: rng.next() as u8,
        // P: keep I and U set (matches reset). Don't randomize D — 2A03
        // ignores it. Randomize C, Z, V, N freely.
        p: (rng.next() as u8 & 0b1100_0011) | 0b0010_0100,
        // SP: leave plenty of room; start at $FD (canonical post-reset).
        sp: 0xFD,
        mem_seed: rng.next(),
    }
}

fn fill_memory_with_seed(memory: &mut [u8], seed: u64) {
    let mut rng = SimpleRng::new(seed);
    for slot in memory.iter_mut() {
        *slot = rng.next() as u8;
    }
}

struct SimpleRng(u64);
impl SimpleRng {
    fn new(seed: u64) -> Self {
        // xorshift64*; avoid zero state.
        Self(seed.wrapping_add(0x9E3779B97F4A7C15))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
}

// ─── Oracle and Z80 runs ─────────────────────────────────────────────────────

#[allow(dead_code)]
fn run_oracle(prg_bytes: &[u8], entry: u16, init: InitialState) -> FinalState {
    let mut bus = oracle_6502::FlatBus::new();
    bus.load(entry, prg_bytes);
    // Zero page + RAM randomized.
    let mut zp = [0u8; 0x100];
    let mut ram = [0u8; 0x600];
    fill_memory_with_seed(&mut zp, init.mem_seed);
    fill_memory_with_seed(&mut ram, init.mem_seed.wrapping_add(1));
    for (i, b) in zp.iter().enumerate() {
        bus.write(i as u16, *b);
    }
    for (i, b) in ram.iter().enumerate() {
        bus.write(0x0200 + i as u16, *b);
    }
    // Push a sentinel return so run_until_rts can return cleanly.
    // Standard pattern: SP = init.sp, push $FF $FE (RTS will return to
    // $FFFF). The oracle's run_until_rts tracks JSR/RTS depth and exits
    // on the first RTS that brings depth below 0.
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = entry;
    cpu.a = init.a;
    cpu.x = init.x;
    cpu.y = init.y;
    cpu.p = init.p;
    // Push a sentinel return ($FFFE-1) onto the 6502 stack so the
    // routine's final RTS pops it cleanly, leaving SP back at `init.sp`.
    // Without this, RTS pops two arbitrary bytes and SP ends at init.sp + 2.
    cpu.sp = init.sp.wrapping_sub(2);
    let hi_slot = 0x0100u16 + init.sp as u16;
    let lo_slot = 0x0100u16 + init.sp.wrapping_sub(1) as u16;
    bus.write(hi_slot, 0xFF); // high byte of sentinel return addr
    bus.write(lo_slot, 0xFE); // low byte: target = $FFFE+1 = $FFFF
    // 6502 step budget per routine. SMB's RAM-clear loop ($90CC) does
    // ~14K instructions; bump well above that so we don't truncate the
    // run mid-loop. The corresponding Z80 budget below is much higher
    // since each 6502 op expands to many Z80 ops.
    let _ = cpu.run_until_rts(&mut bus, 200_000);

    let mut zp_out = vec![0u8; 0x100];
    let mut ram_out = vec![0u8; 0x600];
    for i in 0..0x100usize {
        zp_out[i] = bus.read(i as u16);
    }
    for i in 0..0x600usize {
        ram_out[i] = bus.read(0x0200 + i as u16);
    }
    FinalState {
        a: cpu.a,
        x: cpu.x,
        y: cpu.y,
        p: cpu.p,
        sp: cpu.sp,
        zp: zp_out,
        ram: ram_out,
    }
}

fn run_z80(z80_bytes: &[u8], entry: u16, init: InitialState) -> FinalState {
    run_z80_with_prg(z80_bytes, entry, init, None)
}

fn run_z80_with_prg(
    z80_bytes: &[u8],
    entry: u16,
    init: InitialState,
    prg: Option<&[u8]>,
) -> FinalState {
    let mut bus = z80_emu::FlatBus::new();
    // Load the Z80 program starting at $4000.
    for (i, b) in z80_bytes.iter().enumerate() {
        bus.mem[0x4000 + i] = *b;
    }
    // Embed the NES PRG bytes at their NES addresses ($8000-$BFFF) so
    // the lowered code's PrgRom indexed reads find real bytes. We can
    // ONLY embed the lower 16 KiB: SMS $C000-$FFFF is where the shadow
    // 6502 state, zero page, ram mirror, and emulated 6502 stack live.
    // Embedding NES PRG $C000-$FFFF would overwrite those. PrgRom reads
    // into NES $C000+ stay unmapped in the harness; routines that depend
    // on them will diff and surface as Phase D follow-ups.
    if let Some(prg) = prg {
        let copy_len = prg.len().min(0xC000 - 0x8000);
        for (i, b) in prg[..copy_len].iter().enumerate() {
            bus.mem[0x8000 + i] = *b;
        }
    }
    // Seed SMS RAM zp/ram (same bytes as the oracle saw at NES zp/ram).
    let mut zp = [0u8; 0x100];
    let mut ram = [0u8; 0x600];
    fill_memory_with_seed(&mut zp, init.mem_seed);
    fill_memory_with_seed(&mut ram, init.mem_seed.wrapping_add(1));
    for (i, b) in zp.iter().enumerate() {
        bus.write(sms_layout::NES_ZP_BASE + i as u16, *b);
    }
    for (i, b) in ram.iter().enumerate() {
        // SMS RAM at $C200..$C7FF mirrors NES $0200..$07FF.
        bus.write(0xC200 + i as u16, *b);
    }
    // Shadow X/Y/S/P.
    bus.write(sms_layout::SHADOW_X, init.x);
    bus.write(sms_layout::SHADOW_Y, init.y);
    bus.write(sms_layout::SHADOW_S, init.sp);
    bus.write(sms_layout::SHADOW_P, init.p);

    let mut cpu = z80_emu::Cpu::new();
    cpu.pc = entry;
    cpu.a = init.a;
    // F: irrelevant; lowered code reads/writes shadow P, not native F.
    cpu.sp = 0xDFFE;
    // Push a sentinel return: when the routine RETs, PC = $FFFE which is
    // outside the routine. run_until_ret stops on first RET that pops
    // below initial depth.
    // Conservative Z80 expands one 6502 op into 10–40 Z80 instructions
    // (helpers, push/pop, shadow updates). Give it 10× the 6502 budget.
    let _ = cpu.run_until_ret(&mut bus, 2_000_000);

    let mut zp_out = vec![0u8; 0x100];
    let mut ram_out = vec![0u8; 0x600];
    for i in 0..0x100usize {
        zp_out[i] = bus.read(sms_layout::NES_ZP_BASE + i as u16);
    }
    for i in 0..0x600usize {
        ram_out[i] = bus.read(0xC200 + i as u16);
    }
    FinalState {
        a: cpu.a,
        x: bus.read(sms_layout::SHADOW_X),
        y: bus.read(sms_layout::SHADOW_Y),
        p: bus.read(sms_layout::SHADOW_P),
        sp: bus.read(sms_layout::SHADOW_S),
        zp: zp_out,
        ram: ram_out,
    }
}

fn diff_state(oracle: &FinalState, z80: &FinalState) -> Vec<String> {
    let mut diffs = Vec::new();
    if oracle.a != z80.a {
        diffs.push(format!("A: oracle=${:02X} z80=${:02X}", oracle.a, z80.a));
    }
    if oracle.x != z80.x {
        diffs.push(format!("X: oracle=${:02X} z80=${:02X}", oracle.x, z80.x));
    }
    if oracle.y != z80.y {
        diffs.push(format!("Y: oracle=${:02X} z80=${:02X}", oracle.y, z80.y));
    }
    // Compare P with masks. Ignore B (bit 4) and U (bit 5) since they
    // exist mostly as stack-push artifacts.
    let p_mask: u8 = 0b1100_1111;
    if (oracle.p & p_mask) != (z80.p & p_mask) {
        diffs.push(format!(
            "P: oracle=${:02X}({}) z80=${:02X}({})",
            oracle.p & p_mask,
            flag_names(oracle.p),
            z80.p & p_mask,
            flag_names(z80.p),
        ));
    }
    if oracle.sp != z80.sp {
        diffs.push(format!("SP: oracle=${:02X} z80=${:02X}", oracle.sp, z80.sp));
    }
    // RAM: diff zp and ram.
    let mut zp_diffs: BTreeMap<usize, (u8, u8)> = BTreeMap::new();
    for i in 0..0x100usize {
        if oracle.zp[i] != z80.zp[i] {
            zp_diffs.insert(i, (oracle.zp[i], z80.zp[i]));
        }
    }
    if !zp_diffs.is_empty() {
        for (addr, (o, z)) in zp_diffs.iter().take(4) {
            diffs.push(format!("zp ${addr:02X}: oracle=${o:02X} z80=${z:02X}"));
        }
        if zp_diffs.len() > 4 {
            diffs.push(format!("(+ {} more zp diffs)", zp_diffs.len() - 4));
        }
    }
    let mut ram_diffs: Vec<(usize, u8, u8)> = Vec::new();
    for i in 0..0x600usize {
        if oracle.ram[i] != z80.ram[i] {
            ram_diffs.push((i, oracle.ram[i], z80.ram[i]));
        }
    }
    if !ram_diffs.is_empty() {
        for (addr_off, o, z) in ram_diffs.iter().take(4) {
            let nes_addr = 0x0200 + *addr_off;
            diffs.push(format!(
                "ram nes:${nes_addr:04X}: oracle=${o:02X} z80=${z:02X}"
            ));
        }
        if ram_diffs.len() > 4 {
            diffs.push(format!("(+ {} more ram diffs)", ram_diffs.len() - 4));
        }
    }
    diffs
}

fn flag_names(p: u8) -> String {
    let mut s = String::new();
    if p & 0x80 != 0 {
        s.push('N');
    } else {
        s.push('-');
    }
    if p & 0x40 != 0 {
        s.push('V');
    } else {
        s.push('-');
    }
    s.push('-');
    s.push('-');
    if p & 0x08 != 0 {
        s.push('D');
    } else {
        s.push('-');
    }
    if p & 0x04 != 0 {
        s.push('I');
    } else {
        s.push('-');
    }
    if p & 0x02 != 0 {
        s.push('Z');
    } else {
        s.push('-');
    }
    if p & 0x01 != 0 {
        s.push('C');
    } else {
        s.push('-');
    }
    s
}

/// Generate a plain-text validation report from a batch of results.
pub fn format_report(results: &[ValidationResult]) -> String {
    let mut s = String::new();
    let total = results.len();
    let green = results.iter().filter(|r| r.is_green()).count();
    let skipped = results
        .iter()
        .filter(|r| r.skipped_reason.is_some())
        .count();
    let red = total - green - skipped;
    s.push_str(&format!(
        "Validation summary: {green} green / {red} red / {skipped} skipped (of {total})\n\n"
    ));
    for r in results {
        if let Some(reason) = &r.skipped_reason {
            s.push_str(&format!(
                "  skip  ${:04X} {} — {reason}\n",
                r.routine_entry, r.routine_name
            ));
            continue;
        }
        let mark = if r.is_green() { "ok  " } else { "FAIL" };
        s.push_str(&format!(
            "  {mark}  ${:04X} {} — {}/{} passed\n",
            r.routine_entry, r.routine_name, r.vectors_passed, r.vectors_run,
        ));
        for f in r.failures.iter().take(3) {
            s.push_str(&format!(
                "        seed={} init A=${:02X} X=${:02X} Y=${:02X} P=${:02X}\n",
                f.seed, f.initial.a, f.initial.x, f.initial.y, f.initial.p,
            ));
            for d in &f.diffs {
                s.push_str(&format!("          {d}\n"));
            }
        }
        if r.failures.len() > 3 {
            s.push_str(&format!(
                "        (+ {} more failures suppressed)\n",
                r.failures.len() - 3
            ));
        }
    }
    s
}
