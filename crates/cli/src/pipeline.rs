//! End-to-end pipeline: NES ROM → SMS project.

use std::fmt;
use std::path::Path;

use analysis::nes_rom_like;
use lower::LowerOptions;
use sms_project::{NesMirroring, ProjectAssets, ProjectConfig, RawCiramBackend};

use crate::Args;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Rom(nes_rom::ParseError),
    Profile(profile::LoadError),
    Lift(ir::LiftError),
    Lower(lower::LowerError),
    Emit(z80_emit::EmitError),
    Project(sms_project::EmitError),
    Diagnostic(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o: {e}"),
            Error::Rom(e) => write!(f, "rom: {e}"),
            Error::Profile(e) => write!(f, "profile: {e}"),
            Error::Lift(e) => write!(f, "lift: {e:?}"),
            Error::Lower(e) => write!(f, "lower: {e}"),
            Error::Emit(e) => write!(f, "emit: {e:?}"),
            Error::Project(e) => write!(f, "project: {e}"),
            Error::Diagnostic(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
impl From<nes_rom::ParseError> for Error {
    fn from(e: nes_rom::ParseError) -> Self {
        Error::Rom(e)
    }
}
impl From<profile::LoadError> for Error {
    fn from(e: profile::LoadError) -> Self {
        Error::Profile(e)
    }
}
impl From<ir::LiftError> for Error {
    fn from(e: ir::LiftError) -> Self {
        Error::Lift(e)
    }
}
impl From<lower::LowerError> for Error {
    fn from(e: lower::LowerError) -> Self {
        Error::Lower(e)
    }
}
impl From<z80_emit::EmitError> for Error {
    fn from(e: z80_emit::EmitError) -> Self {
        Error::Emit(e)
    }
}
impl From<sms_project::EmitError> for Error {
    fn from(e: sms_project::EmitError) -> Self {
        Error::Project(e)
    }
}

pub fn run(args: &Args) -> Result<String, Error> {
    // 1. Read and parse the ROM.
    let rom_bytes = std::fs::read(&args.rom)?;
    let image = nes_rom::parse(&rom_bytes)?;
    let vectors = nes_rom::read_vectors(image.prg)
        .ok_or_else(|| Error::Diagnostic("could not read NMI/RESET/IRQ vectors from PRG".into()))?;

    // 2. Load the profile.
    let prof = profile::load_from_path(&args.profile)?;

    // Sanity-check the profile against the parsed ROM.
    if prof.rom.mapper != image.header.mapper {
        return Err(Error::Diagnostic(format!(
            "profile mapper={} but ROM mapper={}",
            prof.rom.mapper, image.header.mapper
        )));
    }
    if let Some(v) = prof.vectors {
        if v.reset != vectors.reset {
            return Err(Error::Diagnostic(format!(
                "profile RESET=${:04X} != ROM RESET=${:04X}",
                v.reset, vectors.reset
            )));
        }
        if v.nmi != vectors.nmi {
            return Err(Error::Diagnostic(format!(
                "profile NMI=${:04X} != ROM NMI=${:04X}",
                v.nmi, vectors.nmi
            )));
        }
    }

    // 3. Analyze. Discover functions; classify code/data.
    //
    // Mapper M0 (docs/mapper-plan.md): banked ROMs (PRG > 32 KiB) are
    // analyzed through per-bank 32 KiB VIEWS — each view = one switchable
    // bank at $8000-$BFFF + the fixed last bank at $C000-$FFFF, exactly
    // NROM-shaped, so analysis and lifting run unchanged per view. View 0
    // provides the shared fixed-bank code; banked-region discoveries are
    // future M1 work (bank-entry annotations); for now the fixed bank
    // alone boots UxROM titles to the trap-reporting stage.
    let banked = image.prg.len() > 32 * 1024;
    let analysis_view: Vec<u8> = if banked {
        let fixed = &image.prg[image.prg.len() - 0x4000..];
        let bank0 = &image.prg[..0x4000];
        let mut v = Vec::with_capacity(0x8000);
        v.extend_from_slice(bank0);
        v.extend_from_slice(fixed);
        v
    } else {
        image.prg.to_vec()
    };
    let analyzed = analysis::analyze(
        &analysis_view,
        nes_rom_like::Vectors {
            nmi: vectors.nmi,
            reset: vectors.reset,
            irq: vectors.irq,
        },
        &prof,
    );

    // 4. Lift each discovered function into IR.
    //
    // Analysis can produce overlapping ranges when a routine's linear walk
    // crosses into another known root's entry. Trim each routine's end to
    // be no later than the next routine's start, so two routines never
    // both contain the same byte. This avoids duplicate L_XXXX labels in
    // the lowered output; internal branches that target the trimmed-off
    // tail become external references and resolve via the alias label.
    let mut funcs: Vec<analysis::DiscoveredFunction> = analyzed.functions.functions.clone();
    // Banked ROMs (M1): functions discovered inside the switchable window
    // ($8000-$BFFF) came from walking the VIEW's bank-0 bytes — the real
    // target bank is only known at run time. Drop them: their callers'
    // targets become unresolved strict-trap stubs whose diagnostics
    // (together with the $CB62 bank shadow) name the (bank, addr) pairs
    // to annotate as [[bank_entry]] profile roots.
    if banked {
        funcs.retain(|f| f.addr >= 0xC000);
    }
    funcs.sort_by_key(|f| f.addr);
    for i in 0..funcs.len() {
        if i + 1 < funcs.len() && funcs[i].end > funcs[i + 1].addr {
            funcs[i].end = funcs[i + 1].addr;
        }
    }
    funcs.retain(|f| f.end > f.addr);

    // Convert profile jump-engine sites into the IR's representation once.
    let jump_engine_sites: Vec<ir::JumpEngineSite> = prof
        .jump_engines
        .iter()
        .map(|s| ir::JumpEngineSite {
            caller: s.caller,
            targets: s.targets.clone(),
        })
        .collect();

    // ---- Pass 1: lift everything once to collect all branch-target PCs ----
    // Each routine emits `L_XXXX` labels for its own internal targets,
    // but cross-routine branches (e.g., BEQ from InitScreen to a PC
    // inside SetVRAMAddr_A) only see them as external references. We
    // collect every `L_XXXX` name across all routines so the second
    // pass can emit a real label inside whichever routine owns that PC.
    let mut all_referenced_pcs: std::collections::HashSet<u16> = std::collections::HashSet::new();
    for f in &funcs {
        let opts = ir::LiftOptions {
            start: f.addr,
            end: f.end,
            entry_name: f.name.clone(),
            jump_engine_sites: jump_engine_sites.clone(),
            window_label_prefix: None,
            extra_label_pcs: Vec::new(),
        };
        if let Ok(r) = ir::lift_range(&analysis_view, &opts) {
            for lbl in r.branch_labels.iter().chain(r.external_calls.iter()) {
                if let Some(hex) = lbl.strip_prefix("L_")
                    && let Ok(addr) = u16::from_str_radix(hex, 16)
                {
                    all_referenced_pcs.insert(addr);
                }
            }
            let has_terminator = r.ops.last().is_some_and(|op| {
                matches!(
                    op,
                    ir::Op::Rts
                        | ir::Op::Rti
                        | ir::Op::Jmp { .. }
                        | ir::Op::JmpIndirect { .. }
                        | ir::Op::Brk
                        | ir::Op::Jam { .. }
                )
            });
            if !has_terminator {
                // If the final decoded instruction overlapped the next known
                // root (SMB uses BIT-operand alternate entries), the real
                // fallthrough is the decoded next PC, not the trimmed range
                // end. Pre-mark it so the owner routine emits an interior
                // label on pass 2.
                all_referenced_pcs.insert(r.end);
            }
        }
    }

    let mut lift_failures: Vec<String> = Vec::new();
    let mut banked_routines: Vec<ir::Routine> = Vec::new();
    // 4b. Banked-window translation units (mapper plan M1). For each
    // bank named by a [[bank_entry]], analyze an NROM-shaped view (that
    // bank + the fixed bank) rooted at its entries and lift the window
    // routines with bank-prefixed labels (L_bK_XXXX). Fixed-bank
    // re-discoveries are dropped (the shared fixed translation wins);
    // NEW fixed-bank functions reached only via banked code are lifted
    // unprefixed from the same view (identical bytes).
    if banked && !prof.bank_entries.is_empty() {
        use std::collections::BTreeMap;
        let mut by_bank: BTreeMap<u8, Vec<u16>> = BTreeMap::new();
        for be in &prof.bank_entries {
            by_bank.entry(be.bank).or_default().push(be.addr);
        }
        let mut known_fixed: std::collections::HashSet<u16> =
            funcs.iter().map(|f| f.addr).collect();
        for (bank, entries) in by_bank {
            let mut view = Vec::with_capacity(0x8000);
            view.extend_from_slice(
                &image.prg[bank as usize * 0x4000..(bank as usize + 1) * 0x4000],
            );
            view.extend_from_slice(&image.prg[image.prg.len() - 0x4000..]);
            let prefix = format!("b{bank}_");
            let mut bprof = prof.clone();
            bprof.functions = entries
                .iter()
                .map(|&a| profile::Function {
                    addr: a,
                    name: format!("L_{prefix}{a:04X}"),
                    note: None,
                })
                .collect();
            let banalyzed = analysis::analyze(
                &view,
                nes_rom_like::Vectors {
                    nmi: vectors.nmi,
                    reset: vectors.reset,
                    irq: vectors.irq,
                },
                &bprof,
            );
            let mut bfuncs: Vec<analysis::DiscoveredFunction> =
                banalyzed.functions.functions.clone();
            let main_ranges: Vec<(u16, u16)> = funcs.iter().map(|f| (f.addr, f.end)).collect();
            bfuncs.retain(|f| {
                if (0x8000..0xC000).contains(&f.addr) {
                    return true;
                }
                if f.addr < 0xC000 || known_fixed.contains(&f.addr) {
                    return false;
                }
                // A fixed-region discovery whose entry lies INSIDE a main
                // routine is a mid-routine alias target, not a new
                // function — the interior-label mechanism resolves it.
                !main_ranges.iter().any(|&(a, e)| f.addr > a && f.addr < e)
            });
            bfuncs.sort_by_key(|f| f.addr);
            bfuncs.dedup_by_key(|f| f.addr);
            // Fixed-region discoveries from this view must not overlap the
            // MAIN fixed routines: clamp each one's end to the next main
            // function start (otherwise their interior labels collide with
            // main entries/aliases — duplicate-label link failures).
            let mut main_starts: Vec<u16> = funcs.iter().map(|f| f.addr).collect();
            main_starts.sort_unstable();
            for f in bfuncs.iter_mut().filter(|f| f.addr >= 0xC000) {
                if let Some(&next) = main_starts.iter().find(|&&a| a > f.addr) {
                    if f.end > next {
                        f.end = next;
                    }
                }
            }
            if std::env::var("N2S_DEBUG_BANKFUNCS").is_ok() {
                for f in &bfuncs {
                    eprintln!("bank{bank} func ${:04X}-${:04X} {}", f.addr, f.end, f.name);
                }
            }
            for w in 0..bfuncs.len().saturating_sub(1) {
                let next = bfuncs[w + 1].addr;
                if bfuncs[w].end > next {
                    bfuncs[w].end = next;
                }
            }
            for f in &bfuncs {
                if f.addr >= 0xC000 {
                    // Later views must not re-lift this fixed routine.
                    known_fixed.insert(f.addr);
                }
            }
            // Interior-alias pass (mirrors the main funcs' two-pass):
            // collect every referenced window pc, then re-lift with
            // extra labels so cross-routine branch targets resolve.
            let mut bank_referenced: std::collections::HashSet<u16> = Default::default();
            for f in &bfuncs {
                let opts = ir::LiftOptions {
                    start: f.addr,
                    end: f.end,
                    entry_name: String::new(),
                    jump_engine_sites: jump_engine_sites.clone(),
                    window_label_prefix: (f.addr < 0xC000).then(|| prefix.clone()),
                    extra_label_pcs: Vec::new(),
                };
                if let Ok(r) = ir::lift_range(&view, &opts) {
                    for lbl in r.branch_labels.iter().chain(r.external_calls.iter()) {
                        if let Some(hex) = lbl
                            .strip_prefix(&format!("L_{prefix}"))
                            .or_else(|| lbl.strip_prefix("L_"))
                            && hex.len() == 4
                            && let Ok(a) = u16::from_str_radix(hex, 16)
                        {
                            bank_referenced.insert(a);
                        }
                    }
                }
            }
            for f in &bfuncs {
                let in_window = f.addr < 0xC000;
                let extras: Vec<u16> = bank_referenced
                    .iter()
                    .filter(|&&pc| pc > f.addr && pc < f.end)
                    .copied()
                    .collect();
                let opts = ir::LiftOptions {
                    start: f.addr,
                    end: f.end,
                    entry_name: if in_window {
                        format!("L_{prefix}{:04X}", f.addr)
                    } else {
                        format_label(f.addr)
                    },
                    jump_engine_sites: jump_engine_sites.clone(),
                    window_label_prefix: in_window.then(|| prefix.clone()),
                    extra_label_pcs: extras,
                };
                match ir::lift_range(&view, &opts) {
                    Ok(mut r) => {
                        ir::mark_rts_dispatch(&mut r.ops);
                        let has_terminator = r.ops.last().is_some_and(|op| {
                            matches!(
                                op,
                                ir::Op::Rts
                                    | ir::Op::Rti
                                    | ir::Op::Jmp { .. }
                                    | ir::Op::JmpIndirect { .. }
                                    | ir::Op::Brk
                                    | ir::Op::Jam { .. }
                            )
                        });
                        if !has_terminator {
                            // Trimmed fallthrough: continue into the next
                            // routine via an explicit jump (bank-prefixed
                            // when the target is in the window).
                            let tgt = if r.end < 0xC000 {
                                format!("L_{prefix}{:04X}", r.end)
                            } else {
                                format_label(r.end)
                            };
                            if !r.external_calls.contains(&tgt) {
                                r.external_calls.push(tgt.clone());
                            }
                            r.ops.push(ir::Op::Jmp { target: tgt });
                        }
                        for lbl in r.branch_labels.iter().chain(r.external_calls.iter()) {
                            if let Some(hex) = lbl.strip_prefix("L_")
                                && hex.len() == 4
                                && let Ok(a) = u16::from_str_radix(hex, 16)
                            {
                                all_referenced_pcs.insert(a);
                            }
                        }
                        banked_routines.push(r)
                    }
                    Err(e) => lift_failures.push(format!("bank{bank} ${:04X}: {:?}", f.addr, e)),
                }
            }
        }
    }

    let mut routines: Vec<ir::Routine> = Vec::with_capacity(funcs.len());
    for f in funcs.iter() {
        // Compute extra labels: PCs in our range that are referenced
        // from outside this routine but aren't its own entry point.
        let extras: Vec<u16> = all_referenced_pcs
            .iter()
            .filter(|&&pc| pc > f.addr && pc < f.end)
            .copied()
            .collect();
        let opts = ir::LiftOptions {
            start: f.addr,
            end: f.end,
            entry_name: f.name.clone(),
            jump_engine_sites: jump_engine_sites.clone(),
            window_label_prefix: None,
            extra_label_pcs: extras,
        };
        match ir::lift_range(&analysis_view, &opts) {
            Ok(mut r) => {
                // If the lifted routine doesn't end with a terminator,
                // it's a fall-through routine. Insert an explicit
                // `Op::Jmp` to the next function's L_XXXX so the
                // semantics are preserved at the harness AND in the
                // emitted SMS code. Without this, the lowered Z80 just
                // continues executing past the routine's body into
                // whatever bytes follow.
                let has_terminator = r.ops.last().is_some_and(|op| {
                    matches!(
                        op,
                        ir::Op::Rts
                            | ir::Op::Rti
                            | ir::Op::Jmp { .. }
                            | ir::Op::JmpIndirect { .. }
                            | ir::Op::Brk
                            | ir::Op::Jam { .. }
                    )
                });
                if !has_terminator {
                    let tail_addr = r.end;
                    let tail_has_owner = funcs
                        .iter()
                        .any(|owner| tail_addr >= owner.addr && tail_addr < owner.end);
                    if tail_has_owner {
                        let tail = format_label(tail_addr);
                        r.ops.push(ir::Op::Jmp {
                            target: tail.clone(),
                        });
                        if !r.external_calls.contains(&tail) {
                            r.external_calls.push(tail);
                        }
                    }
                }
                ir::mark_rts_dispatch(&mut r.ops);
                routines.push(r);
            }
            Err(e) => lift_failures.push(format!(
                "${:04X} {} (${:04X}..${:04X}): {:?}",
                f.addr, f.name, f.addr, f.end, e
            )),
        }
    }

    routines.extend(banked_routines);

    // Global interior-label dedup: overlapping fixed-region translations
    // (main vs bank-view discoveries) can each emit an interior label for
    // the same NES pc. The translations cover IDENTICAL bytes, so any
    // reference may resolve to whichever copy keeps the definition —
    // strip all but the first.
    {
        // Every routine's ENTRY label (its auto label — the lifter may
        // emit it as an Op::Label when the entry is a branch target).
        let auto_of = |r: &ir::Routine| -> String {
            if r.name.starts_with("L_b") {
                r.name.clone()
            } else {
                format_label(r.entry)
            }
        };
        let mut defined: std::collections::HashSet<String> =
            routines.iter().map(&auto_of).collect();
        for r in routines.iter_mut() {
            let own = auto_of(r);
            r.ops.retain(|op| {
                if let ir::Op::Label(l) = op {
                    if *l == own {
                        return true; // the routine's own entry definition
                    }
                    if defined.contains(l) {
                        return false; // defined elsewhere (overlap copy)
                    }
                    defined.insert(l.clone());
                }
                true
            });
        }
    }

    // 4c. [[bank_call]] rewrites: fixed-bank call sites whose window
    // target's bank is annotated get retargeted to the bank-prefixed
    // label; everything else stays an unresolved strict-trap stub.
    if !prof.bank_calls.is_empty() {
        use ir::Op;
        let map: std::collections::HashMap<String, String> = prof
            .bank_calls
            .iter()
            .map(|bc| {
                (
                    format!("L_{:04X}", bc.target),
                    format!("L_b{}_{:04X}", bc.bank, bc.target),
                )
            })
            .collect();
        for r in routines.iter_mut() {
            if r.name.starts_with("L_b") {
                continue; // banked units already carry their own prefix
            }
            for op in r.ops.iter_mut() {
                match op {
                    Op::Jsr { target } | Op::Jmp { target } => {
                        if let Some(new) = map.get(target) {
                            *target = new.clone();
                        }
                    }
                    _ => {}
                }
            }
            for lbl in r.external_calls.iter_mut() {
                if let Some(new) = map.get(lbl) {
                    *lbl = new.clone();
                }
            }
        }
    }

    // 5. Lower into Z80. Pre-declare all runtime symbols so the linker can
    //    bind them; we emit calls to them but the actual implementations
    //    live in runtime/*.s.
    // Each WLA-DX ROM bank is 16 KiB. We pin each generated_code_N
    // section to a unique bank in slot 1 ($4000-$7FFF) so the section's
    // labels resolve to logical slot-1 addresses (and `jp L_XXXX` from
    // boot/translated code lands on real translated bytes, not on the
    // in-bank offset which would alias slot 0). Banks start at 4 to
    // leave 0-3 for the runtime/boot + asset data the project emits.
    const SECTION_MAX_BYTES: u16 = 10 * 1024;
    const TRANSLATED_BANK_BASE: u8 = 4;
    const TRANSLATED_SLOT: u8 = 1;

    // ---- Fixed-point section assignment. Lowering shrinks whenever the
    // label→section map lets far_call/far_jmp downgrade to plain call/jp,
    // and shrinking moves the SECTION_MAX_BYTES rotation boundaries, which
    // changes which section each label lands in. A single dry pass is
    // therefore not sound: a downgrade decided against the dry layout can
    // straddle a boundary that moved in the real layout (observed as a
    // near `call` from bank $0A into bank 4 — wild execution — when the
    // promoted sound-engine routines shifted the layout). Iterate until
    // the map the emission consumed equals the map it produced; that
    // self-consistency makes every downgrade provably same-bank.
    // Iteration 0 with an empty map is the pessimistic all-far dry pass.
    let flag_reads: std::collections::HashMap<String, u8> = {
        let mut m = std::collections::HashMap::new();
        for r in &routines {
            let mask = lower::routine_incoming_flag_reads(&r.ops);
            m.insert(format_label(r.entry), mask);
            m.insert(r.name.clone(), mask);
        }
        m
    };

    let emit_translated = |section_map: &std::collections::HashMap<String, usize>|
     -> Result<(z80_emit::Program, Vec<String>, Vec<String>), Error> {
        let mut program = z80_emit::Program::new();
        let mut section_idx: u32 = 0;
        program.section(&format!("generated_code_{section_idx}"));
        program.set_section_placement(TRANSLATED_BANK_BASE + section_idx as u8, TRANSLATED_SLOT);
        program.org(0x4000);
        program.prepopulate_label_section(section_map);
        let mut section_base: u16 = program.current_addr();
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
            routine_flag_reads: Some(&flag_reads),
        };

        // Translated reset alias so boot.s can `jp translated_reset`.
        program.label("translated_reset");
        program.jp(&format_label(vectors.reset));
        // Translated NMI alias so runtime/boot.s stays game-agnostic.
        program.label("translated_nmi");
        program.jp(&format_label(vectors.nmi));
        // Translated IRQ/BRK alias: NES BRK vectors through the IRQ
        // handler and RTIs — a well-defined interrupt, not a crash.
        // (CV1's engine tolerates junk task dispatches this way.)
        program.label("translated_irq");
        program.jp(&format_label(vectors.irq));

        let mut prev_mapped_section: Option<usize> = None;
        let mut lower_failures: Vec<String> = Vec::new();
        let mut defined_labels: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        defined_labels.insert("translated_reset".to_string());
        defined_labels.insert("translated_nmi".to_string());

        if std::env::var("N2S_DEBUG_RNAMES").is_ok() {
            let mut names: std::collections::HashMap<&str, usize> = Default::default();
            for r in &routines {
                *names.entry(r.name.as_str()).or_insert(0) += 1;
            }
            for (n, c) in names.iter().filter(|(_, c)| **c > 1) {
                eprintln!("ROUTINE NAME x{c}: {n}");
            }
        }
        for r in &routines {
            // Rotate sections. With a frozen map (pass 2) the routine's
            // section comes from the map — near-call downgrades shrink code,
            // and re-rotating by size would move routines across the banks
            // their callers were downgraded against (observed as wild
            // near-calls into the wrong bank). Without a map (sizing pass)
            // rotate when the section fills.
            // Compare map TRANSITIONS, not absolute indices: the snapshot's
            // section numbering includes program-internal sections, so its
            // index space is offset from our counter.
            let snapshot_key = if r.name.starts_with("L_b") {
                r.name.clone() // banked units are registered under their prefixed label
            } else {
                format_label(r.entry)
            };
            let mapped_section = section_map.get(&snapshot_key).copied();
            let should_rotate = match (mapped_section, prev_mapped_section) {
                (Some(sec), Some(prev)) => sec != prev,
                (Some(_), None) => false,
                _ => {
                    let cur = program.current_addr();
                    // Wrapped past the 16 KiB slot = definitely rotate.
                    cur < section_base || cur.saturating_sub(section_base) >= SECTION_MAX_BYTES
                }
            };
            if mapped_section.is_some() {
                prev_mapped_section = mapped_section;
            }
            if std::env::var("N2S_DEBUG_ROT").is_ok() {
                eprintln!(
                    "ROT?p{} {} cur=${:04X} base=${:04X} mapped={:?} prev={:?} rotate={}",
                    if section_map.is_empty() { 1 } else { 2 },
                    r.name,
                    program.current_addr(),
                    section_base,
                    mapped_section,
                    prev_mapped_section,
                    should_rotate
                );
            }
            if should_rotate {
                section_idx += 1;
                if banked && section_idx > 11 {
                    return Err(Error::Diagnostic(format!(
                        "translated code overflows the banked 512K layout \
                         (section {section_idx} > 11; banks 16+ hold PRG data)"
                    )));
                }
                program.section(&format!("generated_code_{section_idx}"));
                program.set_section_placement(
                    TRANSLATED_BANK_BASE + section_idx as u8,
                    TRANSLATED_SLOT,
                );
                program.org(0x4000);
                section_base = program.current_addr();
            }

            // Internal call sites use the auto-generated `L_XXXX` label.
            // Profile-supplied names and the auto label both need to point
            // at the routine entry — UNLESS the lifter emits the auto label
            // itself (entry PC is also an internal branch target).
            let auto = if r.name.starts_with("L_b") {
                r.name.clone() // banked unit: entry label carries the bank prefix
            } else {
                format_label(r.entry)
            };
            // The lifter always emits Op::Label(entry_name) as the first
            // op — when the routine's NAME is the auto label (banked units,
            // L_bK_XXXX), that op IS the definition.
            let lifter_emits_auto = r.branch_labels.contains(&auto) || r.name == auto;
            if !lifter_emits_auto && !defined_labels.contains(&auto) {
                program.label(&auto);
                defined_labels.insert(auto.clone());
            }
            if lifter_emits_auto {
                defined_labels.insert(auto.clone());
            }
            defined_labels.insert(r.name.clone());
            for bl in &r.branch_labels {
                defined_labels.insert(bl.clone());
            }

            // Oversize guard: a mis-rooted walk through data decodes into
            // a colossal garbage 'routine' (10-90 KiB) that cannot fit a
            // 16 KiB bank. Emit a loud trap stub instead — if it is ever
            // really executed, the trap reports it like any other miss.
            if std::env::var("N2S_DEBUG_ROT").is_ok() && r.ops.len() > 500 {
                eprintln!("BIG ROUTINE {} ops={}", r.name, r.ops.len());
            }
            if r.ops.len() > 600 {
                if lifter_emits_auto {
                    // The ops (which carried the entry label) are not
                    // lowered — define the label on the stub instead.
                    program.label(&auto);
                }
                program.ld_a_imm(0xEE);
                program.ld_abs_a(0xCB1B);
                program.jp("rt_unresolved_jsr");
                lower_failures.push(format!(
                    "${:04X} {}: oversize ({} ops) — stubbed as data-walk",
                    r.entry,
                    r.name,
                    r.ops.len()
                ));
            } else {
                match lower::lower_routine(&mut program, r, &opts) {
                    Ok(()) => {}
                    Err(e) => {
                        lower_failures.push(format!("${:04X} {}: {}", r.entry, r.name, e))
                    }
                }
            }
            // Post-emit rotation: giants legitimately exceeding the
            // boundary start a fresh section for the next routine.
            {
                let cur = program.current_addr();
                if cur < section_base || cur.saturating_sub(section_base) >= SECTION_MAX_BYTES {
                    section_idx += 1;
                    program.section(&format!("generated_code_{section_idx}"));
                    program.set_section_placement(
                        TRANSLATED_BANK_BASE + section_idx as u8,
                        TRANSLATED_SLOT,
                    );
                    program.org(0x4000);
                    section_base = program.current_addr();
                    prev_mapped_section = None;
                }
            }
        }

        // Pre-declare runtime symbols (real bodies live in runtime/*.s;
        // placeholders keep z80_emit patch resolution happy).
        program.section("runtime_forward_decls");
        for sym in RUNTIME_SYMBOLS {
            program.label(sym);
            program.ret();
        }

        // External-call stubs: any label still referenced but not defined
        // traps loudly via rt_unresolved_jsr (strict default).
        let unresolved: Vec<String> = program.unresolved_labels();
        program.section("unresolved_stubs");
        for (idx, ext) in unresolved.iter().enumerate() {
            program.label(ext);
            if args.debug_unresolved_stubs {
                // Debug-only visual-progress mode: no-op that advances the
                // ScreenRoutines sub-task counter (NES $073C / SMS $C73C).
                program.ld_hl_imm(0xC73C);
                program.call("rt_inc_mem");
                program.ret();
            } else if let Some(addr) = ext
                .strip_prefix("L_")
                .map(|h| h.rsplit('_').next().unwrap_or(h))
                .and_then(|h| u16::from_str_radix(h, 16).ok())
                .filter(|a| (0x8000..0xC000).contains(a) && banked)
            {
                // Switchable-window target: the correct translation depends
                // on the bank mapped AT CALL TIME. Route through the
                // runtime (bank, addr) dispatch table — never hard-bind.
                program.ld_bc_imm(addr);
                program.jp("rt_banked_dispatch");
            } else {
                let id = idx as u16;
                program.ld_a_imm((id & 0x00FF) as u8);
                program.ld_abs_a(0xCB1B);
                program.ld_a_imm((id >> 8) as u8);
                program.ld_abs_a(0xCB1C);
                program.jp("rt_unresolved_jsr");
            }
        }
        // Banked-dispatch table (mapper plan M1): every translated routine
        // keyed by (NES bank, NES addr) for runtime indirect dispatch.
        // Fixed-bank routines use bank $FF (matches any window bank).
        program.section("rt_dispatch_table_sec");
        program.label("rt_dispatch_table");
        for r in &routines {
            let (bank, addr, label) = match r.name.strip_prefix("L_b") {
                Some(rest) => {
                    let mut it = rest.splitn(2, '_');
                    let b: u8 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0xFF);
                    let a = it
                        .next()
                        .and_then(|x| u16::from_str_radix(x, 16).ok())
                        .unwrap_or(r.entry);
                    (b, a, r.name.clone())
                }
                // Fixed-bank routines are DEFINED under their auto L_XXXX
                // label (profile display names are aliases only).
                None => (0xFF, r.entry, format_label(r.entry)),
            };
            program.dispatch_entry(addr, bank, &label);
        }
        program.data(None, &[0x00, 0x00]); // terminator: addr $0000

        Ok((program, lower_failures, unresolved))
    };

    // Two-pass freeze (H2): pass 1 sizes every cross-section call at the
    // full inline-far length and yields the section map; pass 2 emits with
    // near-call downgrades against that FROZEN map. Downgrades only shrink
    // sections, so every same-section pair from pass 1 stays same-section —
    // sound without iterating to a fixed point (the old equality iteration
    // oscillated once the far/near size delta grew to 24 bytes).
    let empty_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let (sizing_prog, _f, _u) = emit_translated(&empty_map)?;
    let section_map = sizing_prog.label_section_snapshot();
    if std::env::var("N2S_DEBUG_SECTIONS").is_ok() {
        for l in ["translated_reset", "L_8000", "L_800F", "L_8220", "L_9000"] {
            eprintln!("map[{l}] = {:?}", section_map.get(l));
        }
    }
    let (program, lower_failures, unresolved) = emit_translated(&section_map)?;

    let mut build = program.finish()?;
    // Post-process the asm listing for WLA-DX:
    // 1. Strip the runtime forward-decl section. The bytes still contain
    //    harmless `ret` placeholders for in-Rust patch resolution, but
    //    WLA-DX picks up the real bodies from runtime/*.s and would
    //    otherwise error on duplicate labels.
    // 2. Strip `.org` lines that appear inside `.section` blocks. WLA-DX
    //    rejects `.org` inside sectioned code; placement is driven by
    //    the bank map. z80_emit emits `.org` for internal label-patch
    //    addressing, which is correct for the byte buffer but not for
    //    WLA-DX text output.
    // 3. Strip `.bank N slot S` directives for translated sections that
    //    WLA-DX has trouble placing because of size; fall back to
    //    `superfree` placement. For now the directives stay enabled —
    //    re-enable this strip if linker overflows happen.
    build.asm = strip_section(&build.asm, "runtime_forward_decls");
    build.asm = strip_inline_org(&build.asm);

    // 8. Convert assets (CHR + a default palette + nametable placeholder).
    // CHR-RAM carts (chr_kib = 0) ship no pattern data: build the asset
    // set from an all-zero 8 KiB CHR (blank tiles, identity maps). The
    // runtime $2007 pattern-write conversion fills real tiles in play.
    let chr_ram_blank;
    let chr_source: &[u8] = if image.chr.is_empty() {
        chr_ram_blank = vec![0u8; 8192];
        &chr_ram_blank
    } else {
        image.chr
    };
    let (chr_4bpp, chr_maps, chr_report) = build_chr_assets(chr_source, &prof.chr_packs)?;
    let palette: [u8; 32] = default_palette();
    // Default name table: all zeros. Real rendering comes from translated
    // PPU $2006/$2007 writes during init/NMI. (Switch this to a tile-
    // index pattern for visual verification of the CHR-upload path.)
    let nametable = Some(vec![0u8; 32 * 28 * 2]);
    // Mirror the lower PRG window into a dedicated SMS slot-2 bank. The
    // translated SMB code reads data tables such as $805A/$806D/$8080 via raw
    // slot-2 addresses; without this bank those reads hit CHR/nametable assets.
    // Banked mappers (M1): every switchable 16 KiB NES bank becomes its
    // own SMS data bank; the fixed LAST bank is prg_high. NROM keeps the
    // flat low/high split.
    let (prg_low, prg_banks) = if banked {
        let banks: Vec<Vec<u8>> = image.prg.chunks(0x4000).map(|c| c.to_vec()).collect();
        (None, Some(banks))
    } else {
        (
            Some(image.prg[..image.prg.len().min(0x4000)].to_vec()),
            None,
        )
    };
    // Mirror the fixed upper PRG window as well. The translated code can run
    // from generated banks in slot 1, so original fixed-bank data tables such
    // as SMB's Bitmasks at $C68A are read via a slot-2 runtime helper.
    let prg_high = if banked {
        Some(image.prg[image.prg.len() - 0x4000..].to_vec())
    } else if image.prg.len() > 0x4000 {
        Some(image.prg[0x4000..image.prg.len().min(0x8000)].to_vec())
    } else {
        Some(image.prg[..image.prg.len().min(0x4000)].to_vec())
    };
    // Preserve raw NES CHR bytes for emulated PPUDATA reads. SMB's
    // DrawTitleScreen copies a command stream from PPU pattern-table space
    // ($1EC0+) through $2007; the converted SMS 4bpp tiles are not suitable
    // for that CPU-visible readback path.
    let chr_nes = Some(if image.chr.is_empty() {
        vec![0u8; 8192]
    } else {
        image.chr.to_vec()
    });

    let project_assets = ProjectAssets {
        chr_4bpp,
        palette,
        nametable,
        prg_low,
        prg_banks,
        prg_high,
        chr_nes,
        chr_maps: Some(chr_maps),
    };

    // 9. Emit the WLA-DX project.
    std::fs::create_dir_all(&args.out)?;
    let runtime_dir = args.runtime.as_deref().map(Path::new);
    // ROM size policy: at least PRG (32 KiB) + CHR (16 KiB SMS, doubled
    // from 8 KiB NES) + runtime + translated. The conservative lower
    // expands SMB's 32 KiB PRG into ~150 KiB of Z80; size the ROM so
    // WLA-DX's linker has room. 304 KiB is comfortable for SMB-sized
    // games plus the PRG/CHR data banks; larger NES titles would need more.
    let mirroring = match image.header.mirroring {
        nes_rom::Mirroring::Horizontal => NesMirroring::Horizontal,
        nes_rom::Mirroring::Vertical => NesMirroring::Vertical,
        nes_rom::Mirroring::FourScreen => {
            return Err(Error::Diagnostic(
                "unsupported NES four-screen nametable mirroring".into(),
            ));
        }
    };
    let cfg = ProjectConfig {
        // 512 KiB for everything: NROM translated uses banks 4-23;
        // banked carts use translated 4-15 + PRG data 16-23 + assets
        // 24-30 (1 MiB ROMs rendered black on real emulators).
        rom_kib: 512,
        region: 0x4C,
        title: truncate_title(&prof.rom.name),
        mirroring,
        raw_ciram_backend: RawCiramBackend::SramSlot2,
        mapper: prof.rom.mapper,
        chr_ram: image.chr.is_empty(),
    };
    sms_project::emit_project(&args.out, &build, &project_assets, &cfg, runtime_dir)?;

    // 10. Reports.
    let reports_dir = args.out.join("reports");
    std::fs::create_dir_all(&reports_dir)?;
    std::fs::write(reports_dir.join("discovery.txt"), &analyzed.report.text)?;
    // B.6 coverage report: contiguous unknown PRG ranges with a hex peek,
    // the attack list for closing classification coverage.
    {
        let mut txt = String::new();
        let (code, data, unknown) = analyzed.class_map.summary();
        let total = code + data + unknown;
        txt.push_str(&format!(
            "PRG classification: {code} code, {data} data, {unknown} unknown of {total}              ({:.1}% covered)\n\nUnknown ranges (NES addr, len, first bytes):\n",
            (code + data) as f64 * 100.0 / total as f64
        ));
        let mut i = 0usize;
        while i < total {
            if analyzed.class_map.class_at(i) == analysis::ByteClass::Unknown {
                let start = i;
                while i < total && analyzed.class_map.class_at(i) == analysis::ByteClass::Unknown {
                    i += 1;
                }
                let nes = 0x8000 + start;
                let peek: String = analysis_view[start..(start + 16).min(i)]
                    .iter()
                    .map(|b| format!("{b:02X} "))
                    .collect();
                txt.push_str(&format!("  ${nes:04X}  {:5}  {peek}\n", i - start));
            } else {
                i += 1;
            }
        }
        std::fs::write(reports_dir.join("coverage.txt"), txt)?;
    }
    std::fs::write(reports_dir.join("lifted.txt"), lifted_report(&routines))?;
    std::fs::write(reports_dir.join("chr_map.txt"), chr_report)?;
    write_optional_report(
        &reports_dir.join("lift_failures.txt"),
        (!lift_failures.is_empty()).then(|| lift_failures.join("\n")),
    )?;
    write_optional_report(
        &reports_dir.join("lower_failures.txt"),
        (!lower_failures.is_empty()).then(|| lower_failures.join("\n")),
    )?;
    let unresolved_report = if !unresolved.is_empty() {
        let mut report = String::new();
        for (idx, label) in unresolved.iter().enumerate() {
            report.push_str(&format!("{idx:04X}  {label}\n"));
        }
        Some(report)
    } else {
        None
    };
    write_optional_report(
        &reports_dir.join("unresolved_labels.txt"),
        unresolved_report,
    )?;

    // 11. Optional differential validation.
    let mut validation_summary = String::new();
    if args.validate {
        // Per-routine validation. (`validate_program` lowers everything
        // together but exceeds the 64 KiB Z80 address space for large
        // games; revisit when we add Z80-side banking to the harness.)
        let mut results: Vec<validation::ValidationResult> = Vec::with_capacity(routines.len());
        for r in &routines {
            results.push(validation::validate_routine(
                image.prg,
                r,
                args.validate_vectors,
            ));
        }
        let report = validation::format_report(&results);
        std::fs::write(reports_dir.join("validation.txt"), &report)?;
        let green = results.iter().filter(|r| r.is_green()).count();
        let skipped = results
            .iter()
            .filter(|r| r.skipped_reason.is_some())
            .count();
        let red = results.len() - green - skipped;
        validation_summary = format!(
            "\nvalidation: {green} green / {red} red / {skipped} skipped (of {})\n\
             validation report: {}",
            results.len(),
            reports_dir.join("validation.txt").display(),
        );
        // Don't fail the build on validation red — the project tree was
        // emitted successfully and the report captures the failures.
        // (Phase A.3+ triages these; for now we surface them.)
    }

    let unresolved_mode = if args.debug_unresolved_stubs {
        "debug stubs"
    } else {
        "strict traps"
    };

    Ok(format!(
        "wrote SMS project to {}\n\
         functions: {} ({} lifted, {} lift failures, {} lower failures)\n\
         unresolved labels: {} ({})\n\
         code bytes: {}, data bytes: {}, unknown bytes: {}\n\
         translated.asm size: {} bytes{}",
        args.out.display(),
        analyzed.functions.functions.len(),
        routines.len(),
        lift_failures.len(),
        lower_failures.len(),
        unresolved.len(),
        unresolved_mode,
        analyzed.class_map.summary().0,
        analyzed.class_map.summary().1,
        analyzed.class_map.summary().2,
        build.bytes.len(),
        validation_summary,
    ))
}

fn format_label(addr: u16) -> String {
    format!("L_{addr:04X}")
}

fn build_chr_assets(
    chr: &[u8],
    packs: &[profile::ChrPackRange],
) -> Result<(Vec<u8>, Vec<u8>, String), Error> {
    if packs.is_empty() {
        let chr_4bpp = assets::nes_chr_to_sms_4bpp(chr);
        let mut physical = [[None; 256]; 2];
        for (tile, slot) in physical[0].iter_mut().enumerate() {
            *slot = Some(tile as u16);
        }
        for (tile, slot) in physical[1].iter_mut().take(192).enumerate() {
            *slot = Some(256 + tile as u16);
        }
        let (chr_maps, unmapped, fallbacks) = build_chr_maps(&physical, &chr_4bpp)
            .map_err(|err| Error::Diagnostic(format!("CHR map generation failed: {err}")))?;
        let report = chr_pack_report(chr, packs, &physical, chr_4bpp.len(), unmapped);
        let report = append_sprite_fallback_report(report, fallbacks);
        return Ok((chr_4bpp, chr_maps, report));
    }

    let mut chr_4bpp = vec![0u8; 448 * 32];
    let mut physical = [[None; 256]; 2];
    for range in packs {
        for (offset, tile) in (range.start..=range.end).enumerate() {
            let source_tile = usize::from(range.table) * 256 + usize::from(tile);
            let dest_slot = usize::from(range.dest) + offset;
            copy_converted_chr_tile(
                chr,
                source_tile,
                &mut chr_4bpp[dest_slot * 32..dest_slot * 32 + 32],
            );
            physical[usize::from(range.table)][usize::from(tile)] = Some(dest_slot as u16);
        }
    }
    let (chr_maps, unmapped, fallbacks) = build_chr_maps(&physical, &chr_4bpp)
        .map_err(|err| Error::Diagnostic(format!("CHR map generation failed: {err}")))?;
    let report = chr_pack_report(chr, packs, &physical, chr_4bpp.len(), unmapped);
    let report = append_sprite_fallback_report(report, fallbacks);
    Ok((chr_4bpp, chr_maps, report))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SpriteFallbacks {
    base_2000_rel: Option<u8>,
    base_0000_rel: Option<u8>,
}

fn append_sprite_fallback_report(mut report: String, fallbacks: SpriteFallbacks) -> String {
    report.push_str(&format!(
        "sprite fallback tile bytes: base $2000 -> {}, base $0000 -> {}\n",
        format_optional_tile(fallbacks.base_2000_rel),
        format_optional_tile(fallbacks.base_0000_rel)
    ));
    report
}

fn format_optional_tile(tile: Option<u8>) -> String {
    tile.map(|tile| format!("${tile:02X}"))
        .unwrap_or_else(|| "not needed".to_string())
}

fn chr_pack_report(
    chr: &[u8],
    packs: &[profile::ChrPackRange],
    physical: &[[Option<u16>; 256]; 2],
    chr_4bpp_len: usize,
    map_fallback_entries: usize,
) -> String {
    let mut s = String::new();
    s.push_str("CHR packing report\n\n");
    s.push_str(&format!("source NES CHR bytes: {}\n", chr.len()));
    s.push_str(&format!("emitted SMS 4bpp bytes: {}\n", chr_4bpp_len));
    s.push_str(&format!("emitted SMS tile slots: {}\n", chr_4bpp_len / 32));
    s.push_str(&format!(
        "map fallback entries (BG + sprite maps): {}\n\n",
        map_fallback_entries
    ));

    if packs.is_empty() {
        s.push_str("mode: implicit identity/default map\n\n");
    } else {
        s.push_str("mode: profile [[chr_pack]]\n");
        for p in packs {
            s.push_str(&format!(
                "  table {} ${:02X}-${:02X} -> SMS slot ${:03X}\n",
                p.table, p.start, p.end, p.dest
            ));
        }
        s.push('\n');
    }

    for table in 0..2usize {
        let bg_unmapped = tile_runs(physical, table, |slot| slot.is_none());
        let sprite_unusable_base_0000 =
            tile_runs(physical, table, |slot| !matches!(slot, Some(0..=255)));
        let sprite_unusable_base_2000 =
            tile_runs(physical, table, |slot| !matches!(slot, Some(256..=511)));
        s.push_str(&format!(
            "table {} BG-unmapped tiles: {}\n",
            table,
            format_runs(&bg_unmapped)
        ));
        s.push_str(&format!(
            "table {} sprite-unusable tiles with VDP sprite base $0000: {}\n",
            table,
            format_runs(&sprite_unusable_base_0000)
        ));
        s.push_str(&format!(
            "table {} sprite-unusable tiles with VDP sprite base $2000: {}\n",
            table,
            format_runs(&sprite_unusable_base_2000)
        ));
    }
    s
}

fn tile_runs<F>(physical: &[[Option<u16>; 256]; 2], table: usize, pred: F) -> Vec<(u8, u8)>
where
    F: Fn(Option<u16>) -> bool,
{
    let mut runs = Vec::new();
    let mut start: Option<u8> = None;
    for tile in 0..=255u16 {
        let hit = pred(physical[table][tile as usize]);
        match (start, hit) {
            (None, true) => start = Some(tile as u8),
            (Some(s), false) => {
                runs.push((s, (tile - 1) as u8));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        runs.push((s, 0xff));
    }
    runs
}

fn format_runs(runs: &[(u8, u8)]) -> String {
    if runs.is_empty() {
        return "none".to_string();
    }
    runs.iter()
        .map(|(start, end)| {
            if start == end {
                format!("${start:02X}")
            } else {
                format!("${start:02X}-${end:02X}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn build_chr_maps(
    physical: &[[Option<u16>; 256]; 2],
    chr_4bpp: &[u8],
) -> Result<(Vec<u8>, usize, SpriteFallbacks), String> {
    let mut out = Vec::with_capacity(0x600);
    let mut unmapped = 0usize;

    for table in physical.iter().take(2) {
        for slot in table.iter().take(256) {
            let slot = match slot {
                Some(slot) => *slot,
                None => {
                    unmapped += 1;
                    0
                }
            };
            out.push((slot & 0x00ff) as u8);
            out.push(((slot >> 8) & 0x0001) as u8);
        }
    }

    // Unmapped/unusable sprite tiles must resolve to a transparent tile under
    // the *active* SMS sprite base. Table 0 is used with VDP sprite base $2000,
    // so a physical slot in 256..=423 becomes a relative SAT tile byte. Table 1
    // is used with VDP sprite base $0000, so a physical slot in 0..=255 is
    // already the SAT tile byte. Keep SMB's established $2000 blank (slot 423 ->
    // relative 167) when it is transparent, but do not reuse that value for
    // base $0000 where it would point at physical slot 167.
    let needs_fallback_2000 = physical[0]
        .iter()
        .any(|slot| !matches!(slot, Some(256..=511)));
    let needs_fallback_0000 = physical[1]
        .iter()
        .any(|slot| !matches!(slot, Some(0..=255)));
    let fallback_2000 =
        if needs_fallback_2000 {
            Some(transparent_sprite_fallback_2000(chr_4bpp).ok_or_else(|| {
                "no transparent sprite fallback tile for SMS base $2000".to_string()
            })?)
        } else {
            None
        };
    let fallback_0000 =
        if needs_fallback_0000 {
            Some(transparent_sprite_fallback_0000(chr_4bpp).ok_or_else(|| {
                "no transparent sprite fallback tile for SMS base $0000".to_string()
            })?)
        } else {
            None
        };
    let fallbacks = SpriteFallbacks {
        base_2000_rel: fallback_2000,
        base_0000_rel: fallback_0000,
    };
    for (table_idx, table) in physical.iter().take(2).enumerate() {
        for slot in table.iter().take(256) {
            let value = match slot {
                Some(slot @ 0..=255) if table_idx == 1 => *slot as u8,
                Some(slot @ 256..=511) if table_idx == 0 => (*slot - 256) as u8,
                Some(_) => {
                    unmapped += 1;
                    if table_idx == 0 {
                        fallback_2000.expect("fallback required for table 0")
                    } else {
                        fallback_0000.expect("fallback required for table 1")
                    }
                }
                None => {
                    unmapped += 1;
                    if table_idx == 0 {
                        fallback_2000.expect("fallback required for table 0")
                    } else {
                        fallback_0000.expect("fallback required for table 1")
                    }
                }
            };
            out.push(value);
        }
    }

    debug_assert_eq!(out.len(), 0x600);
    Ok((out, unmapped, fallbacks))
}

fn transparent_sprite_fallback_2000(chr_4bpp: &[u8]) -> Option<u8> {
    if sms_tile_is_transparent(chr_4bpp, 423) {
        return Some(167);
    }
    (256..=423)
        .find(|slot| sms_tile_is_transparent(chr_4bpp, *slot))
        .map(|slot| (slot - 256) as u8)
}

fn transparent_sprite_fallback_0000(chr_4bpp: &[u8]) -> Option<u8> {
    (0..=255)
        .find(|slot| sms_tile_is_transparent(chr_4bpp, *slot))
        .map(|slot| slot as u8)
}

fn sms_tile_is_transparent(chr_4bpp: &[u8], slot: usize) -> bool {
    let start = slot * 32;
    let end = start + 32;
    end <= chr_4bpp.len() && chr_4bpp[start..end].iter().all(|byte| *byte == 0)
}

fn copy_converted_chr_tile(chr: &[u8], source_tile: usize, dest: &mut [u8]) {
    let nes_base = source_tile * 16;
    if nes_base + 16 > chr.len() {
        return;
    }
    for y in 0..8 {
        let row_base = y * 4;
        dest[row_base] = chr[nes_base + y];
        dest[row_base + 1] = chr[nes_base + 8 + y];
    }
}

/// Remove `.org` directives that appear inside `.section ... .ends` blocks.
/// WLA-DX rejects `.org` in sectioned code; placement comes from the
/// memory/ROM bank map instead.
fn strip_inline_org(asm: &str) -> String {
    let mut out = String::with_capacity(asm.len());
    let mut in_section = false;
    for line in asm.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(".section ") {
            in_section = true;
        } else if trimmed.starts_with(".ends") {
            in_section = false;
        }
        if in_section && trimmed.starts_with(".org ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Remove a `.section "name" ... .ends` block from a WLA-DX assembly
/// listing. Returns the original string if the section isn't found.
fn strip_section(asm: &str, section_name: &str) -> String {
    let needle = format!(".section \"{section_name}\"");
    let Some(start) = asm.find(&needle) else {
        return asm.to_string();
    };
    let Some(end_rel) = asm[start..].find(".ends") else {
        return asm.to_string();
    };
    let end = start + end_rel + ".ends".len();
    let mut out = String::with_capacity(asm.len());
    out.push_str(&asm[..start]);
    // Eat one trailing newline after .ends if present so we don't leave
    // a blank stub line.
    let tail = &asm[end..];
    let tail = tail.strip_prefix('\n').unwrap_or(tail);
    out.push_str(tail);
    out
}

fn truncate_title(s: &str) -> &str {
    let bytes = s.as_bytes();
    let n = bytes.iter().take_while(|b| b.is_ascii()).count().min(11);
    std::str::from_utf8(&bytes[..n]).unwrap_or("nes2sms")
}

fn lifted_report(routines: &[ir::Routine]) -> String {
    let mut s = String::new();
    s.push_str(&format!("Lifted {} routines\n\n", routines.len()));
    for r in routines {
        s.push_str(&format!(
            "${:04X}  {}\n  ops: {}\n  branch labels: {:?}\n  external calls: {:?}\n  unresolved: {:?}\n",
            r.entry,
            r.name,
            r.ops.len(),
            r.branch_labels,
            r.external_calls,
            r.unresolved,
        ));
    }
    s
}

fn write_optional_report(path: &std::path::Path, content: Option<String>) -> Result<(), Error> {
    if let Some(content) = content {
        std::fs::write(path, content)?;
    } else {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// 32-byte SMS CRAM with a placeholder grayscale ramp.
fn default_palette() -> [u8; 32] {
    let mut p = [0u8; 32];
    // Background palette ramp: black, dark gray, light gray, white.
    p[0] = 0x00;
    p[1] = 0x15;
    p[2] = 0x2A;
    p[3] = 0x3F;
    // Sprite palette ramp.
    p[16] = 0x00;
    p[17] = 0x10;
    p[18] = 0x20;
    p[19] = 0x3F;
    p
}

/// Symbols defined in runtime/*.s. We forward-declare them in the
/// generated Program so internal `call` patches resolve at z80_emit
/// `finish()` time. At assemble time WLA-DX picks up the real bodies
/// from runtime/*.s and the linker reconciles.
const RUNTIME_SYMBOLS: &[&str] = &[
    "rt_set_nz_a",
    "rt_adc_a",
    "rt_sbc_a",
    "rt_cmp_a",
    "rt_cpx_a",
    "rt_cpy_a",
    "rt_push6502",
    "rt_pop6502",
    "rt_ppu_write",
    "rt_ppu_write_cont",
    "rt_ppu_read",
    "rt_oam_dma",
    "rt_apu_write",
    "rt_apu_read",
    "rt_sound_stub",
    "rt_controller_strobe",
    "rt_controller_read",
    "rt_controller_read_indexed_x",
    "rt_mapper_write",
    "rt_restore_prg_window",
    "rt_banked_dispatch",
    "rt_rts_dispatch",
    "rt_translated_rts",
    "rt_translated_call_gate",
    "rt_translated_tail_gate",
    "rt_indirect_jmp",
    "rt_unresolved_jsr",
    "rt_unresolved_jsr_flash",
    "rt_brk",
    "rt_asl_a",
    "rt_asl_mem",
    "rt_lsr_a",
    "rt_lsr_mem",
    "rt_rol_a",
    "rt_rol_mem",
    "rt_ror_a",
    "rt_ror_mem",
    "rt_bit_mem",
    "rt_inc_mem",
    "rt_dec_mem",
    "rt_read_indexed",
    "rt_read_prg_high_indexed",
    "rt_write_indexed",
    "rt_read_zp_ptr_y",
    "rt_write_zp_ptr_y",
    "rt_far_call",
    "rt_far_jmp",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn physical_maps() -> [[Option<u16>; 256]; 2] {
        [[None; 256]; 2]
    }

    fn map_table_to_base_2000(physical: &mut [[Option<u16>; 256]; 2], table: usize) {
        for (tile, slot) in physical[table].iter_mut().enumerate() {
            *slot = Some(256 + tile as u16);
        }
    }

    fn map_table_to_base_0000(physical: &mut [[Option<u16>; 256]; 2], table: usize) {
        for (tile, slot) in physical[table].iter_mut().enumerate() {
            *slot = Some(tile as u16);
        }
    }

    #[test]
    fn sprite_base_2000_fallback_prefers_reserved_blank_167() {
        let mut physical = physical_maps();
        map_table_to_base_0000(&mut physical, 1);
        let chr_4bpp = vec![0u8; 448 * 32];

        let (maps, unmapped, fallbacks) = build_chr_maps(&physical, &chr_4bpp).unwrap();

        assert_eq!(maps[0x400], 167);
        assert_eq!(fallbacks.base_2000_rel, Some(167));
        assert_eq!(fallbacks.base_0000_rel, None);
        assert_eq!(unmapped, 512);
    }

    #[test]
    fn sprite_base_0000_fallback_uses_transparent_base0_tile() {
        let mut physical = physical_maps();
        map_table_to_base_2000(&mut physical, 0);
        let mut chr_4bpp = vec![0xffu8; 512 * 32];
        chr_4bpp[5 * 32..6 * 32].fill(0);

        let (maps, unmapped, fallbacks) = build_chr_maps(&physical, &chr_4bpp).unwrap();

        assert_eq!(maps[0x500], 5);
        assert_eq!(fallbacks.base_2000_rel, None);
        assert_eq!(fallbacks.base_0000_rel, Some(5));
        assert_eq!(unmapped, 512);
    }

    #[test]
    fn sprite_base_0000_fallback_fails_closed_without_transparent_tile() {
        let mut physical = physical_maps();
        map_table_to_base_2000(&mut physical, 0);
        let chr_4bpp = vec![0xffu8; 512 * 32];

        let err = build_chr_maps(&physical, &chr_4bpp).unwrap_err();

        assert!(err.contains("SMS base $0000"));
    }
}
