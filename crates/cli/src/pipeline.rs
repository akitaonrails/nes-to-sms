//! End-to-end pipeline: NES ROM → SMS project.

use std::fmt;
use std::path::Path;

use analysis::nes_rom_like;
use lower::LowerOptions;
use sms_project::{ProjectAssets, ProjectConfig};
use z80_emit::Program;

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
    let analyzed = analysis::analyze(
        image.prg,
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
            extra_label_pcs: Vec::new(),
        };
        if let Ok(r) = ir::lift_range(image.prg, &opts) {
            for lbl in r.branch_labels.iter().chain(r.external_calls.iter()) {
                if let Some(hex) = lbl.strip_prefix("L_") {
                    if let Ok(addr) = u16::from_str_radix(hex, 16) {
                        all_referenced_pcs.insert(addr);
                    }
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

    let mut routines: Vec<ir::Routine> = Vec::with_capacity(funcs.len());
    let mut lift_failures: Vec<String> = Vec::new();
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
            extra_label_pcs: extras,
        };
        match ir::lift_range(image.prg, &opts) {
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
                routines.push(r);
            }
            Err(e) => lift_failures.push(format!(
                "${:04X} {} (${:04X}..${:04X}): {:?}",
                f.addr, f.name, f.addr, f.end, e
            )),
        }
    }

    // 5. Lower into Z80. Pre-declare all runtime symbols so the linker can
    //    bind them; we emit calls to them but the actual implementations
    //    live in runtime/*.s.
    let mut program = Program::new();
    // Each WLA-DX ROM bank is 16 KiB. We pin each generated_code_N
    // section to a unique bank in slot 1 ($4000-$7FFF) so the section's
    // labels resolve to logical slot-1 addresses (and `jp L_XXXX` from
    // boot/translated code lands on real translated bytes, not on the
    // in-bank offset which would alias slot 0). Banks start at 4 to
    // leave 0-3 for the runtime/boot + asset data the project emits.
    const SECTION_MAX_BYTES: u16 = 10 * 1024;
    const TRANSLATED_BANK_BASE: u8 = 4;
    const TRANSLATED_SLOT: u8 = 1;

    // ---- Pass 1: dry-lower into a throwaway Program to discover which
    // section each label ends up in. The result is fed into Pass 2 via
    // `prepopulate_label_section`, so far_call/far_jmp can downgrade to
    // plain call/jp whenever both ends sit in the same section (= same
    // bank). Without this, forward-reference branches inside a routine
    // see an empty label_section map and pessimistically emit the
    // trampoline pattern.
    // Interprocedural flag-liveness map: routine label → incoming-flag-read
    // mask. Lets the lowerer see past calls to flag-agnostic callees when
    // deciding whether a fused/lifted op's flags are dead. Keyed by both
    // the L_XXXX entry label and the routine name (JSR targets use either).
    let flag_reads: std::collections::HashMap<String, u8> = {
        let mut m = std::collections::HashMap::new();
        for r in &routines {
            let mask = lower::routine_incoming_flag_reads(&r.ops);
            m.insert(format_label(r.entry), mask);
            m.insert(r.name.clone(), mask);
        }
        m
    };

    let label_section_map: std::collections::HashMap<String, usize> = {
        let mut p = z80_emit::Program::new();
        let mut sidx: u32 = 0;
        p.section(&format!("generated_code_{sidx}"));
        p.set_section_placement(TRANSLATED_BANK_BASE + sidx as u8, TRANSLATED_SLOT);
        p.org(0x4000);
        let mut sbase: u16 = p.current_addr();
        p.label("translated_reset");
        p.jp(&format_label(vectors.reset));
        p.label("translated_nmi");
        p.jp(&format_label(vectors.nmi));
        let dry_opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: false,
            routine_flag_reads: Some(&flag_reads),
        };
        for r in &routines {
            if p.current_addr().saturating_sub(sbase) >= SECTION_MAX_BYTES {
                sidx += 1;
                p.section(&format!("generated_code_{sidx}"));
                p.set_section_placement(TRANSLATED_BANK_BASE + sidx as u8, TRANSLATED_SLOT);
                p.org(0x4000);
                sbase = p.current_addr();
            }
            let auto = format_label(r.entry);
            if !r.branch_labels.contains(&auto) {
                p.label(&auto);
            }
            let _ = lower::lower_routine(&mut p, r, &dry_opts);
        }
        p.label_section_snapshot()
    };

    let mut section_idx: u32 = 0;
    program.section(&format!("generated_code_{section_idx}"));
    program.set_section_placement(TRANSLATED_BANK_BASE + section_idx as u8, TRANSLATED_SLOT);
    // .org goes via z80_emit but emits no asm directive when placement
    // is pinned; symbols still use slot-1 addresses ($4000+) because
    // the section is forced into slot 1 by `.bank N slot 1`.
    program.org(0x4000);
    program.prepopulate_label_section(&label_section_map);
    let mut section_base: u16 = program.current_addr();
    let opts = LowerOptions {
        profile: Some(&prof),
        emit_source_comments: true,
        routine_flag_reads: Some(&flag_reads),
    };

    // Translated reset alias so boot.s can `jp translated_reset`.
    program.label("translated_reset");
    // If RESET resolves to a discovered function, jump to its label.
    program.jp(&format_label(vectors.reset));
    // Translated NMI alias so runtime/boot.s stays game-agnostic.
    program.label("translated_nmi");
    program.jp(&format_label(vectors.nmi));

    let mut lower_failures: Vec<String> = Vec::new();
    let mut defined_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    defined_labels.insert("translated_reset".to_string());
    defined_labels.insert("translated_nmi".to_string());

    for r in &routines {
        // Rotate sections when the current one fills up.
        if program.current_addr().saturating_sub(section_base) >= SECTION_MAX_BYTES {
            section_idx += 1;
            program.section(&format!("generated_code_{section_idx}"));
            program
                .set_section_placement(TRANSLATED_BANK_BASE + section_idx as u8, TRANSLATED_SLOT);
            program.org(0x4000);
            section_base = program.current_addr();
        }

        // Internal call sites use the auto-generated `L_XXXX` label. Profile-
        // supplied names (e.g. "Reset") and the auto label both need to point
        // at the routine entry. z80_emit allows two distinct labels at the
        // same address, so we emit the auto label here before the named one
        // is emitted inside lower_routine — UNLESS the lifter is going to
        // emit it anyway (when the entry PC is also an internal branch
        // target). That's tracked by branch_labels containing the auto name.
        let auto = format_label(r.entry);
        let lifter_emits_auto = r.branch_labels.contains(&auto);
        if !lifter_emits_auto && !defined_labels.contains(&auto) {
            program.label(&auto);
            defined_labels.insert(auto.clone());
        }
        if lifter_emits_auto {
            // The lifter will emit the auto label; record it so the
            // unresolved-stub pass doesn't try to redefine.
            defined_labels.insert(auto);
        }
        defined_labels.insert(r.name.clone());
        for bl in &r.branch_labels {
            defined_labels.insert(bl.clone());
        }

        match lower::lower_routine(&mut program, r, &opts) {
            Ok(()) => {}
            Err(e) => lower_failures.push(format!("${:04X} {}: {}", r.entry, r.name, e)),
        }
    }

    // 6. Pre-declare runtime symbols (the runtime/*.s files supply the
    //    real bodies; these placeholders only exist so z80_emit's patch
    //    resolution succeeds for the in-Rust byte buffer). Emit BEFORE
    //    the unresolved-stubs pass so we don't redundantly stub them.
    program.section("runtime_forward_decls");
    for sym in RUNTIME_SYMBOLS {
        program.label(sym);
        defined_labels.insert((*sym).to_string());
        program.ret();
    }

    // 7. External-call stubs: ask z80_emit what labels are still referenced
    //    but not defined. By default each unresolved label jumps to
    //    `rt_unresolved_jsr` so the final ROM traps loudly if it is reached.
    //    A temporary debug flag can restore the old permissive stubs for
    //    visual experiments, but strict trapping is the trustworthy default.
    //
    // We trust z80_emit's labels-map (not our `defined_labels` tracker)
    // because some labels we pre-recorded never actually got emitted —
    // a lower failure can leave a routine partially-emitted with branch
    // labels missing.
    let unresolved: Vec<String> = program.unresolved_labels();
    program.section("unresolved_stubs");
    for (idx, ext) in unresolved.iter().enumerate() {
        program.label(ext);
        if args.debug_unresolved_stubs {
            // Debug-only visual-progress mode: treat unresolved profile-named
            // symbols as a "no-op that advances the sub-task counter". The
            // ScreenRoutines dispatcher reads NES $073C (SMS $C73C) and runs
            // sub-tasks in sequence; incrementing lets the state machine move
            // past missing work. This is intentionally opt-in because it can
            // hide the real blocker on normal builds.
            program.ld_hl_imm(0xC73C);
            program.call("rt_inc_mem");
            program.ret();
        } else {
            let id = idx as u16;
            program.ld_a_imm((id & 0x00FF) as u8);
            program.ld_abs_a(0xCB1B);
            program.ld_a_imm((id >> 8) as u8);
            program.ld_abs_a(0xCB1C);
            program.jp("rt_unresolved_jsr");
        }
    }

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
    let (chr_4bpp, chr_maps, chr_report) = build_chr_assets(image.chr, &prof.chr_packs);
    let palette: [u8; 32] = default_palette();
    // Default name table: all zeros. Real rendering comes from translated
    // PPU $2006/$2007 writes during init/NMI. (Switch this to a tile-
    // index pattern for visual verification of the CHR-upload path.)
    let nametable = Some(vec![0u8; 32 * 28 * 2]);
    // Mirror the lower PRG window into a dedicated SMS slot-2 bank. The
    // translated SMB code reads data tables such as $805A/$806D/$8080 via raw
    // slot-2 addresses; without this bank those reads hit CHR/nametable assets.
    let prg_low = Some(image.prg[..image.prg.len().min(0x4000)].to_vec());
    // Mirror the fixed upper PRG window as well. The translated code can run
    // from generated banks in slot 1, so original fixed-bank data tables such
    // as SMB's Bitmasks at $C68A are read via a slot-2 runtime helper.
    let prg_high = if image.prg.len() > 0x4000 {
        Some(image.prg[0x4000..image.prg.len().min(0x8000)].to_vec())
    } else {
        Some(image.prg[..image.prg.len().min(0x4000)].to_vec())
    };
    // Preserve raw NES CHR bytes for emulated PPUDATA reads. SMB's
    // DrawTitleScreen copies a command stream from PPU pattern-table space
    // ($1EC0+) through $2007; the converted SMS 4bpp tiles are not suitable
    // for that CPU-visible readback path.
    let chr_nes = Some(image.chr.to_vec());

    let project_assets = ProjectAssets {
        chr_4bpp,
        palette,
        nametable,
        prg_low,
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
    let cfg = ProjectConfig {
        rom_kib: 304,
        region: 0x4C,
        title: truncate_title(&prof.rom.name),
    };
    sms_project::emit_project(&args.out, &build, &project_assets, &cfg, runtime_dir)?;

    // 10. Reports.
    let reports_dir = args.out.join("reports");
    std::fs::create_dir_all(&reports_dir)?;
    std::fs::write(reports_dir.join("discovery.txt"), &analyzed.report.text)?;
    std::fs::write(reports_dir.join("lifted.txt"), lifted_report(&routines))?;
    std::fs::write(reports_dir.join("chr_map.txt"), chr_report)?;
    if !lift_failures.is_empty() {
        std::fs::write(
            reports_dir.join("lift_failures.txt"),
            lift_failures.join("\n"),
        )?;
    }
    if !lower_failures.is_empty() {
        std::fs::write(
            reports_dir.join("lower_failures.txt"),
            lower_failures.join("\n"),
        )?;
    }
    if !unresolved.is_empty() {
        let mut report = String::new();
        for (idx, label) in unresolved.iter().enumerate() {
            report.push_str(&format!("{idx:04X}  {label}\n"));
        }
        std::fs::write(reports_dir.join("unresolved_labels.txt"), report)?;
    }

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

fn build_chr_assets(chr: &[u8], packs: &[profile::ChrPackRange]) -> (Vec<u8>, Vec<u8>, String) {
    if packs.is_empty() {
        let chr_4bpp = assets::nes_chr_to_sms_4bpp(chr);
        let mut physical = [[None; 256]; 2];
        for tile in 0..=255usize {
            physical[0][tile] = Some(tile as u16);
        }
        for tile in 0..192usize {
            physical[1][tile] = Some(256 + tile as u16);
        }
        let (chr_maps, unmapped) = build_chr_maps(&physical);
        let report = chr_pack_report(chr, packs, &physical, chr_4bpp.len(), unmapped);
        return (chr_4bpp, chr_maps, report);
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
    let (chr_maps, unmapped) = build_chr_maps(&physical);
    let report = chr_pack_report(chr, packs, &physical, chr_4bpp.len(), unmapped);
    (chr_4bpp, chr_maps, report)
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

fn build_chr_maps(physical: &[[Option<u16>; 256]; 2]) -> (Vec<u8>, usize) {
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

    // Unmapped sprite tiles resolve to a reserved blank/transparent slot
    // instead of slot 0 (a real, non-blank tile). In 224-line mode the name
    // table sits at $3700 (slot 440); within the valid sprite region 256-439
    // the profile packs patterns into 256-422, reserves slot 423 as the blank
    // (unpacked = all-zero = transparent), and keeps 424-439 as runtime flip
    // scratch. This is how SMB's blank sprite tile $FC -- and any sprite tile
    // that didn't fit -- end up see-through rather than garbage. Value is
    // relative to the VDP sprite pattern base (reg6 = $2000): slot 423 -> 167.
    const BLANK_SPRITE_VALUE: u8 = 167;
    for (table_idx, table) in physical.iter().take(2).enumerate() {
        for slot in table.iter().take(256) {
            let value = match slot {
                Some(slot @ 0..=255) if table_idx == 1 => *slot as u8,
                Some(slot @ 256..=511) if table_idx == 0 => (*slot - 256) as u8,
                Some(_) => {
                    unmapped += 1;
                    BLANK_SPRITE_VALUE
                }
                None => {
                    unmapped += 1;
                    BLANK_SPRITE_VALUE
                }
            };
            out.push(value);
        }
    }

    debug_assert_eq!(out.len(), 0x600);
    (out, unmapped)
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
    "rt_ppu_read",
    "rt_oam_dma",
    "rt_apu_write",
    "rt_apu_read",
    "rt_sound_stub",
    "rt_controller_strobe",
    "rt_controller_read",
    "rt_controller_read_indexed_x",
    "rt_mapper_write",
    "rt_indirect_jmp",
    "rt_unresolved_jsr",
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
