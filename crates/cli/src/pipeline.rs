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
    MapperPolicy(nes_rom::MapperPolicyError),
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
            Error::MapperPolicy(e) => write!(f, "mapper policy: {e}"),
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
impl From<nes_rom::MapperPolicyError> for Error {
    fn from(e: nes_rom::MapperPolicyError) -> Self {
        Error::MapperPolicy(e)
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

/// Mapper-store violations cannot safely degrade to generated stubs: doing so
/// could turn an unsupported store into a wrong bank switch. Other lowering
/// errors remain diagnostics while the converter's broader coverage grows.
fn lower_error_is_fatal(error: &lower::LowerError) -> bool {
    matches!(error, lower::LowerError::UnsupportedMapperStore { .. })
}

/// Full-mode six-byte records and their terminator share one mapped ROM slot.
/// The two directories are placed separately in always-mapped bank zero.
/// Zero counts preserve the original scan for empty or oversized pages.
fn mmc3_dispatch_page_counts(records: &[(u8, u16, String)]) -> Result<[u8; 128], Error> {
    let bytes = records.len().saturating_mul(6).saturating_add(2);
    if bytes > 0x4000 {
        return Err(Error::Diagnostic(format!(
            "MMC3 dispatch table exceeds its single 16 KiB slot: {bytes} bytes for {} records and terminator",
            records.len()
        )));
    }
    let mut counts = [0usize; 128];
    for &(_, addr, _) in records {
        let Some(page) = (addr >> 8).checked_sub(0x80) else {
            return Err(Error::Diagnostic(format!(
                "MMC3 dispatch target ${addr:04X} is outside PRG ROM"
            )));
        };
        counts[usize::from(page)] += 1;
    }
    Ok(counts.map(|count| u8::try_from(count).unwrap_or(0)))
}

/// Exact outputs of the current page-local lower_bound helper, expressed as
/// record indices so WLA-DX can relocate each pointer after placing the table.
/// Count zero deliberately returns the page base, including oversized pages.
fn mmc3_dispatch_pointer_indices(records: &[(u8, u16, String)], counts: &[u8; 128]) -> Vec<usize> {
    let mut indices = Vec::with_capacity(0x8000);
    let mut start = 0;
    for (page, &count) in counts.iter().enumerate() {
        let high = page as u16 + 0x80;
        let mut end = start;
        while end < records.len() && records[end].1 >> 8 == high {
            end += 1;
        }
        let mut lower = start;
        for low in 0..=255 {
            if count != 0 {
                while lower < end && (records[lower].1 & 0xff) < low {
                    lower += 1;
                }
            }
            indices.push(lower);
        }
        start = end;
    }
    indices
}

/// A mapped unit cannot keep executing its old translation after changing
/// that window. Until remapping continuations are modeled, reject all mapped
/// writers, including an indirect store that could address mapper registers
/// and a call chain that reaches a fixed-window mapper writer.
fn check_mmc3_mapping_continuations(routines: &[ir::Routine]) -> Result<(), Error> {
    let mut writers: std::collections::HashSet<String> = routines
        .iter()
        .filter(|r| {
            r.ops.iter().any(|op| {
                matches!(
                    op,
                    ir::Op::MapperWrite { .. }
                        // Computed targets may enter a mapper writer even
                        // when no static call edge names it.
                        | ir::Op::RtsDispatch
                        | ir::Op::JmpIndirect { .. }
                        | ir::Op::JsrUnknown { .. }
                        | ir::Op::StaMem {
                            region: ir::MemRegion::PrgRom | ir::MemRegion::Mapper,
                            ..
                        }
                        | ir::Op::StaMem {
                            addr: ir::AddrExpr::IndirectX(_) | ir::AddrExpr::IndirectY(_),
                            ..
                        }
                )
            })
        })
        .map(routine_auto_label)
        .collect();
    loop {
        let old_len = writers.len();
        for routine in routines {
            if routine
                .external_calls
                .iter()
                .any(|target| writers.contains(target))
            {
                writers.insert(routine_auto_label(routine));
            }
        }
        if writers.len() == old_len {
            break;
        }
    }
    if let Some(routine) = routines
        .iter()
        .find(|r| r.name.starts_with("L_b") && writers.contains(&r.name))
    {
        return Err(Error::Diagnostic(format!(
            "MMC3 mapped routine {} can change PRG mapping; remapping continuations are not implemented",
            routine.name
        )));
    }
    Ok(())
}

/// Only decoded instruction boundaries become legal live-mapping resumptions.
/// A different physical bank must already have an analyzed owner for the PC;
/// a missing destination stays a strict dispatch miss.
fn add_mmc3_continuation_labels(
    routines: &mut [ir::Routine],
    prof: &profile::Profile,
) -> std::collections::BTreeSet<String> {
    let mut pcs: std::collections::BTreeSet<u16> = routines
        .iter()
        .flat_map(|routine| {
            routine
                .ops
                .iter()
                .enumerate()
                .filter(|(_, op)| {
                    matches!(op, ir::Op::Jsr { .. } | ir::Op::MaterializedJsr { .. })
                        || (routine.name.starts_with("L_b") && op.may_remap_mmc3_prg())
                })
                .map(|(index, _)| routine.next_source_pc(index))
        })
        .collect();
    if prof.source_clock_experiment() {
        pcs.extend(routines.iter().flat_map(|r| r.ops.iter()).filter_map(|op| {
            if let ir::Op::Source { pc, .. } = op {
                Some(*pc)
            } else {
                None
            }
        }));
    }
    let mut continuations = std::collections::BTreeSet::new();
    for routine in routines {
        let bank = profile_target_identity(&routine.name).and_then(|(bank, _)| bank);
        let prefix = routine
            .name
            .strip_prefix("L_b")
            .and_then(|rest| rest.split_once('_'))
            .map(|(bank, _)| format!("L_b{bank}_"))
            .unwrap_or_else(|| "L_".into());
        let mut ops = Vec::with_capacity(routine.ops.len());
        for op in std::mem::take(&mut routine.ops) {
            if let ir::Op::Source { pc, .. } = &op
                && pcs.contains(pc)
                && *pc != routine.entry
                // Cross-bank resumptions cannot enter after a verified
                // ownership transfer. Real decoded/profile entries are still
                // rejected by check_consume_entries; unknown entries trap.
                && !prof.return_consumes.iter().any(|site| {
                    site.bank == bank
                        && *pc > site.at
                        && *pc <= site.second_pla.unwrap_or(site.at + 1)
                })
            {
                let label = format!("{prefix}{pc:04X}");
                continuations.insert(label.clone());
                if !routine.branch_labels.contains(&label) {
                    routine.branch_labels.push(label.clone());
                    ops.push(ir::Op::Label(label));
                }
            }
            ops.push(op);
        }
        routine.ops = ops;
    }
    continuations
}

/// One WLA-DX slot is a physical 16 KiB ROM bank.
const TRANSLATED_SECTION_CAPACITY: usize = 0x4000;
const TRANSLATED_BANK_BASE: u32 = 4;
const TRANSLATED_SLOT: u8 = 1;

/// Static translation geometry is separate from live mapper state. In
/// particular an MMC3 analysis unit establishes exactly one physical 8 KiB
/// page at one CPU window, not a guessed mapping of its neighbours.
#[derive(Clone, Copy)]
enum TranslationMapping {
    Legacy(nes_rom::MapperPolicy),
    Mmc3 { bank_count: u8 },
}

impl TranslationMapping {
    fn resolve(image: &nes_rom::Image<'_>, prof: &profile::Profile) -> Result<Self, Error> {
        if prof.cnrom_bus_experiment() {
            nes_rom::cnrom::Cnrom::new(&image.header, image.prg.len(), image.chr.len(), 0)
                .map_err(|e| Error::Diagnostic(e.to_string()))?;
            // CHR banking never changes CPU code identity.
            return Ok(Self::Legacy(nes_rom::MapperPolicy::Nrom {
                prg_len: image.prg.len(),
            }));
        }
        if image.header.mapper == 4
            && prof
                .translation
                .runtime_defines
                .iter()
                .any(|d| d == "MMC3_BANKING_EXPERIMENT" || d == "MMC3_FULL_RUNTIME")
        {
            let board = nes_rom::mmc3::Mmc3::new(
                &image.header,
                image.prg.len(),
                image.chr.len(),
                nes_rom::mmc3::Mmc3Revision::Sharp,
            )
            .map_err(|e| Error::Diagnostic(e.to_string()))?;
            if !prof.mmc3_full_runtime()
                && (image.header.prg_ram_size != 0 || image.header.prg_nvram_size != 0)
            {
                return Err(Error::Diagnostic(
                    "MMC3 banking experiment does not yet support cartridge RAM".into(),
                ));
            }
            if !prof.mmc3_full_runtime() && image.chr.len() != 0x2000 {
                return Err(Error::Diagnostic("MMC3 banking experiment requires 8 KiB CHR; banked CHR presentation is not implemented".into()));
            }
            Ok(Self::Mmc3 {
                bank_count: board.prg_bank_count(),
            })
        } else {
            Ok(Self::Legacy(nes_rom::resolve_mapper_policy(
                &image.header,
                image.prg.len(),
            )?))
        }
    }

    fn is_mmc3(self) -> bool {
        matches!(self, Self::Mmc3 { .. })
    }
    fn is_banked(self) -> bool {
        match self {
            Self::Legacy(p) => p.is_banked(),
            Self::Mmc3 { .. } => true,
        }
    }
    fn bank_count(self) -> u8 {
        match self {
            Self::Legacy(p) => p.bank_count(),
            Self::Mmc3 { bank_count } => bank_count,
        }
    }
    fn fixed_start(self) -> u16 {
        if self.is_mmc3() { 0xe000 } else { 0xc000 }
    }
    fn window(self, addr: u16) -> analysis::AnalysisWindow {
        if self.is_mmc3() {
            analysis::AnalysisWindow {
                start: addr & 0xe000,
                end_inclusive: addr | 0x1fff,
            }
        } else {
            analysis::AnalysisWindow::SWITCHABLE_16K
        }
    }
    fn analysis_view(self, prg: &[u8], bank: u8, window: u16) -> Result<Vec<u8>, Error> {
        match self {
            Self::Legacy(p) => Ok(p.analysis_view(prg, bank)?),
            Self::Mmc3 { bank_count } => {
                if bank >= bank_count || !matches!(window, 0x8000 | 0xa000 | 0xc000) {
                    return Err(Error::Diagnostic(format!(
                        "invalid MMC3 analysis unit: bank {bank} window ${window:04X}"
                    )));
                }
                let mut view = vec![0; 0x8000];
                view[0x6000..].copy_from_slice(&prg[prg.len() - 0x2000..]);
                let offset = usize::from(window - 0x8000);
                let physical = usize::from(bank) * 0x2000;
                view[offset..offset + 0x2000].copy_from_slice(&prg[physical..physical + 0x2000]);
                Ok(view)
            }
        }
    }
    fn vectors(self, prg: &[u8]) -> Result<Option<nes_rom::Vectors>, Error> {
        match self {
            Self::Legacy(p) => Ok(nes_rom::read_vectors_with_policy(p, prg)?),
            Self::Mmc3 { .. } => {
                let tail = &prg[prg.len() - 6..];
                Ok(Some(nes_rom::Vectors {
                    nmi: u16::from_le_bytes([tail[0], tail[1]]),
                    reset: u16::from_le_bytes([tail[2], tail[3]]),
                    irq: u16::from_le_bytes([tail[4], tail[5]]),
                }))
            }
        }
    }
    fn legacy(self) -> Option<nes_rom::MapperPolicy> {
        match self {
            Self::Legacy(p) => Some(p),
            Self::Mmc3 { .. } => None,
        }
    }
}

fn validate_translation_routine(
    mapping: TranslationMapping,
    prg: &[u8],
    routine: &ir::Routine,
    vector_count: usize,
    cnrom_bus: bool,
) -> validation::ValidationResult {
    if mapping.is_mmc3() || cnrom_bus {
        // The isolated harness uses flat NES PRG and legacy Z80 runtime
        // stubs. Even a routine with no explicit mapper write may depend on
        // an 8 KiB mapping; running it there cannot establish MMC3 parity.
        validation::ValidationResult {
            routine_name: routine.name.clone(),
            routine_entry: routine.entry,
            vectors_run: 0,
            vectors_passed: 0,
            failures: Vec::new(),
            skipped_reason: Some(if cnrom_bus {
                "CNROM requires its raw CPU bus, guest-stack dispatch and assembled SMS runtime; isolated legacy stubs cannot establish parity".into()
            } else {
                "MMC3 requires a mapper-aware NES bus and assembled SMS runtime; isolated validation uses legacy mappings".into()
            }),
        }
    } else {
        validation::validate_routine(prg, routine, vector_count)
    }
}

fn translated_section_bank(section_idx: u32, bank_limit: u32) -> Result<u8, Error> {
    let bank = TRANSLATED_BANK_BASE
        .checked_add(section_idx)
        .ok_or_else(|| {
            Error::Diagnostic("translated section index overflows WLA bank numbering".to_string())
        })?;
    if bank >= bank_limit {
        let max_section = bank_limit - TRANSLATED_BANK_BASE - 1;
        if bank_limit == sms_project::MMC3_CODE_BANK_LIMIT {
            return Err(Error::Diagnostic(format!(
                "translated code exceeds the MMC3 phase-1 continuation ABI: section {section_idx} > {max_section}; code banks must remain below {bank_limit}"
            )));
        }
        return Err(Error::Diagnostic(format!(
            "translated code overflows the banked layout \
             (section {section_idx} > {max_section}; banks \
             {}+ hold PRG data)",
            bank_limit
        )));
    }
    u8::try_from(bank).map_err(|_| {
        Error::Diagnostic(format!(
            "translated code bank {bank} exceeds WLA's bank range"
        ))
    })
}

fn begin_translated_section(
    program: &mut z80_emit::Program,
    section_idx: u32,
    bank_limit: u32,
) -> Result<u16, Error> {
    let bank = translated_section_bank(section_idx, bank_limit)?;
    program.section(&format!("generated_code_{section_idx}"));
    let expected_program_idx = usize::try_from(section_idx)
        .ok()
        .and_then(|idx| idx.checked_add(1))
        .ok_or_else(|| {
            Error::Diagnostic(format!(
                "translated logical section {section_idx} exceeds Program section indexing"
            ))
        })?;
    let actual_program_idx = program.current_section_idx();
    if actual_program_idx != expected_program_idx {
        return Err(Error::Diagnostic(format!(
            "translated logical section {section_idx} mapped to Program section {actual_program_idx}, expected {expected_program_idx}; helper sections must not precede generated code"
        )));
    }
    program.set_section_placement(bank, TRANSLATED_SLOT);
    program.org(0x4000);
    Ok(program.current_addr())
}

fn advance_translated_section(
    program: &mut z80_emit::Program,
    section_idx: &mut u32,
    bank_limit: u32,
) -> Result<u16, Error> {
    *section_idx = section_idx.checked_add(1).ok_or_else(|| {
        Error::Diagnostic("translated section index overflows WLA bank numbering".to_string())
    })?;
    begin_translated_section(program, *section_idx, bank_limit)
}

fn translated_section_usage(program: &z80_emit::Program) -> usize {
    program.current_section_len()
}

/// Transactionally pack one complete routine during the sizing pass. The
/// closure owns all routine-local label/diagnostic mutation through `state`.
fn pack_sizing_candidate<S: Clone>(
    program: &mut z80_emit::Program,
    state: &mut S,
    section_idx: &mut u32,
    bank_limit: u32,
    routine_name: &str,
    emit: impl Fn(&mut z80_emit::Program, &mut S) -> Result<(), Error>,
) -> Result<u32, Error> {
    let mut candidate = program.clone();
    let mut candidate_state = state.clone();
    emit(&mut candidate, &mut candidate_state)?;
    if translated_section_usage(&candidate) > TRANSLATED_SECTION_CAPACITY {
        advance_translated_section(program, section_idx, bank_limit)?;
        candidate = program.clone();
        candidate_state = state.clone();
        emit(&mut candidate, &mut candidate_state)?;
        let used = translated_section_usage(&candidate);
        if used > TRANSLATED_SECTION_CAPACITY {
            return Err(Error::Diagnostic(format!(
                "translated routine {routine_name} exceeds physical 16 KiB slot: {used} bytes"
            )));
        }
    }
    *program = candidate;
    *state = candidate_state;
    Ok(*section_idx)
}

fn routine_auto_label(r: &ir::Routine) -> String {
    if r.name.starts_with("L_b") {
        r.name.clone()
    } else {
        format_label(r.entry)
    }
}

/// Parse profile jump-engine target labels into their physical identity.
/// `L_F000` is a fixed-window address; `L_b6_A123` is switchable bank 6.
/// Unqualified switchable labels intentionally return `(None, addr)` because
/// they mean "the mapper bank selected at runtime" and must not root every
/// physical bank during static analysis.
fn profile_target_identity(label: &str) -> Option<(Option<u8>, u16)> {
    if let Some(rest) = label.strip_prefix("L_b") {
        let (bank, addr) = rest.split_once('_')?;
        return Some((
            Some(bank.parse().ok()?),
            u16::from_str_radix(addr, 16).ok()?,
        ));
    }
    label
        .strip_prefix("L_")
        .filter(|rest| !rest.contains('_'))
        .and_then(|addr| u16::from_str_radix(addr, 16).ok())
        .map(|addr| (None, addr))
}

/// Resolve every profile spelling accepted by lowering, without treating an
/// unqualified switchable address as a newly invented physical-bank fact.
fn consume_target_identity(prof: &profile::Profile, label: &str) -> Option<(Option<u8>, u16)> {
    profile_target_identity(label)
        .or_else(|| {
            prof.functions
                .iter()
                .find(|f| f.name == label)
                .map(|f| (None, f.addr))
        })
        .or_else(|| {
            prof.labels
                .iter()
                .find(|f| f.name == label)
                .map(|f| (None, f.addr))
        })
}

fn check_consume_entries(
    prof: &profile::Profile,
    entries: &[(Option<u8>, u16)],
) -> Result<(), Error> {
    for site in prof
        .return_escapes
        .iter()
        .filter(|s| s.stack_bytes_already_consumed)
    {
        let start = site.consume_at.expect("validated profile");
        if let Some(&(bank, addr)) = entries.iter().find(|(bank, addr)| {
            *addr > start
                && *addr <= site.caller
                && (*addr >= if prof.rom.mapper == 4 { 0xe000 } else { 0xc000 }
                    || prof.rom.mapper == 0
                    || bank.is_none()
                    || *bank == site.bank)
        }) {
            return Err(Error::Diagnostic(format!(
                "return_escape consume_at ${start:04X} has bypass entry ${addr:04X} in bank {bank:?}"
            )));
        }
    }
    for site in &prof.return_consumes {
        if let Some(&(bank, addr)) = entries.iter().find(|(bank, addr)| {
            *addr > site.at
                && *addr <= site.second_pla.unwrap_or(site.at + 1)
                && (*addr >= if prof.rom.mapper == 4 { 0xe000 } else { 0xc000 }
                    || prof.rom.mapper == 0
                    || bank.is_none()
                    || *bank == site.bank)
        }) {
            return Err(Error::Diagnostic(format!(
                "return_consume ${:04X} has second-PLA bypass entry ${addr:04X} in bank {bank:?}",
                site.at
            )));
        }
    }
    Ok(())
}

fn return_consume_sites(prof: &profile::Profile, bank: Option<u8>) -> Vec<ir::ReturnConsumeSite> {
    prof.return_consumes
        .iter()
        .filter(|s| s.bank == bank)
        .map(|s| ir::ReturnConsumeSite {
            at: s.at,
            second_pla: s.second_pla,
            return_addrs: s.calls.iter().map(|call| call.caller + 2).collect(),
        })
        .collect()
}

fn lift_data_regions(
    prof: &profile::Profile,
    bank: Option<u8>,
) -> Vec<std::ops::RangeInclusive<u16>> {
    if !prof.dynamic_cpu_bus() {
        return Vec::new();
    }
    prof.data_regions
        .iter()
        .filter(|region| region.bank.is_none() || region.bank == bank)
        .map(|region| region.start..=region.end)
        .collect()
}

fn materialized_call_sites(
    prof: &profile::Profile,
    bank: Option<u8>,
) -> Vec<ir::MaterializedCallSite> {
    prof.return_consumes
        .iter()
        .flat_map(|s| &s.calls)
        .filter(|s| s.bank == bank)
        .map(|s| ir::MaterializedCallSite {
            caller: s.caller,
            target: s.target,
        })
        .collect()
}

/// Re-run discovery until every decoded internal branch label is owned by a
/// non-overlapping routine range. This matters when a separately discovered
/// entry is embedded inside a larger routine: range normalization trims the
/// outer routine at that entry, and a branch around the embedded routine can
/// otherwise leave its continuation with no translated owner.
fn analyze_with_continuation_roots(
    prg: &[u8],
    vectors: nes_rom_like::Vectors,
    prof: &mut profile::Profile,
    window: analysis::AnalysisWindow,
    bank: Option<u8>,
) -> analysis::Analyzed {
    loop {
        let analyzed = analysis::analyze_in_window(prg, vectors, prof, window, bank);
        let mut normalized = analyzed.functions.functions.clone();
        normalized.sort_by_key(|function| function.addr);
        normalized.dedup_by_key(|function| function.addr);
        for index in 0..normalized.len().saturating_sub(1) {
            let next = normalized[index + 1].addr;
            if normalized[index].end > next {
                normalized[index].end = next;
            }
        }
        normalized.retain(|function| function.end > function.addr);

        let mut continuations = std::collections::BTreeSet::new();
        for function in &analyzed.functions.functions {
            let external = if prof.dynamic_cpu_bus() {
                function.external_refs.as_slice()
            } else {
                &[]
            };
            for &label in function.internal_labels.iter().chain(external) {
                let is_owned = normalized
                    .iter()
                    .any(|owner| label >= owner.addr && label < owner.end);
                if window.contains(label) && !is_owned && !prof.is_data_byte_in_bank(label, bank) {
                    continuations.insert(label);
                }
            }
        }
        continuations.retain(|address| {
            prof.functions
                .iter()
                .all(|function| function.addr != *address)
        });
        if continuations.is_empty() {
            return analyzed;
        }

        for address in continuations {
            prof.functions.push(profile::Function {
                addr: address,
                name: prof
                    .label_for(address)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("func_{address:04X}")),
                note: Some("branch continuation after embedded routine".to_string()),
            });
        }
    }
}

/// Labels whose definitions belong at this routine's entry address if it is
/// stubbed. A BTreeSet makes aliases stable and eliminates overlap between
/// the routine name, lifted labels, and branch targets.
fn routine_owned_labels(r: &ir::Routine) -> Vec<String> {
    let mut labels = std::collections::BTreeSet::new();
    labels.insert(routine_auto_label(r));
    labels.insert(r.name.clone());
    for op in &r.ops {
        if let ir::Op::Label(label) = op {
            labels.insert(label.clone());
        }
    }
    labels.extend(r.branch_labels.iter().cloned());
    labels.into_iter().collect()
}

/// Define every alias a stubbed routine owns, then emit its single strict
/// unresolved-call trap body. Callers must begin from a routine-local
/// snapshot so this remains an atomic fallback.
fn emit_routine_trap_stub(
    program: &mut z80_emit::Program,
    defined_labels: &mut std::collections::BTreeSet<String>,
    r: &ir::Routine,
) {
    for label in routine_owned_labels(r) {
        if !defined_labels.contains(&label) {
            program.label(&label);
            defined_labels.insert(label);
        }
    }
    program.ld_a_imm(0xEE);
    program.ld_abs_a(0xCB1B);
    program.jp("rt_unresolved_jsr");
}

fn emit_translated_routine(
    program: &mut z80_emit::Program,
    defined_labels: &mut std::collections::BTreeSet<String>,
    lower_failures: &mut Vec<String>,
    r: &ir::Routine,
    opts: &LowerOptions<'_>,
) -> Result<(), Error> {
    let pre_routine_program = program.clone();
    let pre_routine_labels = defined_labels.clone();
    let auto = routine_auto_label(r);
    // A stub-body replacement supersedes the whole translated body: any
    // entry (JSR, JMP, computed dispatch, or a conditional branch from a
    // neighboring routine) lands on `call hook / ret`. Interior labels are
    // not emitted; a surviving external reference to one fails closed
    // through the unresolved-label machinery.
    if let Some(profile) = opts.profile
        && let Some(rep) = profile.replacement_for(r.entry)
        && rep.stub_body
    {
        if !defined_labels.contains(&auto) {
            program.label(&auto);
        }
        defined_labels.insert(auto.clone());
        if r.name != auto && !defined_labels.contains(&r.name) {
            program.label(&r.name);
        }
        defined_labels.insert(r.name.clone());
        program.call(&rep.runtime_label.clone());
        program.ret();
        return Ok(());
    }
    let lifter_emits_auto = r.branch_labels.contains(&auto) || r.name == auto;
    if r.ops.len() > 600 {
        if opts.profile.is_some_and(|p| p.dynamic_cpu_bus()) {
            return Err(Error::Diagnostic(format!(
                "MMC3 full runtime routine {} exceeds the analyzed routine size bound ({} ops)",
                r.name,
                r.ops.len()
            )));
        }
        *program = pre_routine_program;
        *defined_labels = pre_routine_labels;
        emit_routine_trap_stub(program, defined_labels, r);
        lower_failures.push(format!(
            "${:04X} {}: oversize ({} ops) — stubbed as data-walk",
            r.entry,
            r.name,
            r.ops.len()
        ));
        return Ok(());
    }
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
    if let Err(e) = lower::lower_routine(program, r, opts) {
        if lower_error_is_fatal(&e)
            || opts
                .profile
                .is_some_and(|p| p.rom.mapper == 4 || p.cnrom_bus_experiment())
        {
            *program = pre_routine_program;
            *defined_labels = pre_routine_labels;
            if opts.profile.is_some_and(|p| p.mmc3_full_runtime()) {
                return Err(Error::Diagnostic(format!("MMC3 routine {}: {e}", r.name)));
            }
            return Err(Error::Lower(e));
        }
        *program = pre_routine_program;
        *defined_labels = pre_routine_labels;
        emit_routine_trap_stub(program, defined_labels, r);
        lower_failures.push(format!("${:04X} {}: {}", r.entry, r.name, e));
    }
    Ok(())
}

fn emit_translated_vector_aliases(program: &mut z80_emit::Program, reset: u16, nmi: u16, irq: u16) {
    program.label("translated_reset");
    program.translated_tail_jmp(&format_label(reset));
    program.label("translated_nmi");
    program.translated_tail_jmp(&format_label(nmi));
    program.label("translated_irq");
    program.translated_tail_jmp(&format_label(irq));
}

pub fn run(args: &Args) -> Result<String, Error> {
    // 1. Read and parse the ROM.
    let rom_bytes = std::fs::read(&args.rom)?;
    let image = nes_rom::parse(&rom_bytes)?;
    // 2. Load the profile.
    let prof = profile::load_from_path(&args.profile)?;
    if prof.cnrom_bus_experiment() && args.debug_unresolved_stubs {
        return Err(Error::Diagnostic(
            "CNROM bus experiment forbids permissive unresolved stubs".into(),
        ));
    }
    if prof.cnrom_bus_experiment()
        && (rom_bytes[7] & 3 != 0
            || rom_bytes[12..16] != [0, 0, 0, 0]
            || rom_bytes.len() != 16 + image.prg.len() + image.chr.len())
    {
        return Err(Error::Diagnostic("CNROM bus experiment requires a plain NTSC NES2 synthetic image with no trainer, miscellaneous ROM, expansion device or trailing data".into()));
    }

    if prof.native_calls() && prof.rom.mapper != 0 {
        return Err(Error::Diagnostic(format!(
            "stack_discipline = \"native\" requires mapper 0 (NROM); profile mapper is {}",
            prof.rom.mapper
        )));
    }
    if prof.native_calls() && !prof.return_escapes.is_empty() {
        return Err(Error::Diagnostic(
            "stack_discipline = \"native\" is incompatible with [[return_escape]] sites"
                .to_string(),
        ));
    }

    // Sanity-check the profile against the parsed ROM.
    let actual_prg_kib = image.prg.len() / 1024;
    if prof.rom.prg_kib as usize != actual_prg_kib {
        return Err(Error::Diagnostic(format!(
            "profile PRG size mismatch: profile prg_kib={} KiB, ROM PRG payload={} KiB ({} bytes)",
            prof.rom.prg_kib,
            actual_prg_kib,
            image.prg.len()
        )));
    }
    let actual_chr_kib = image.chr.len() / 1024;
    if prof.rom.chr_kib as usize != actual_chr_kib {
        return Err(Error::Diagnostic(format!(
            "profile CHR size mismatch: profile chr_kib={} KiB, ROM CHR-ROM payload={} KiB ({} bytes)",
            prof.rom.chr_kib,
            actual_chr_kib,
            image.chr.len()
        )));
    }
    if let Some(expected) = &prof.rom.payload_sha256 {
        let actual = nes_rom::payload_sha256_hex(image.prg, image.chr);
        if expected != &actual {
            return Err(Error::Diagnostic(format!(
                "profile payload SHA-256 mismatch: expected {expected}, actual {actual}"
            )));
        }
    }
    if prof.rom.mapper != image.header.mapper {
        return Err(Error::Diagnostic(format!(
            "profile mapper={} but ROM mapper={}",
            prof.rom.mapper, image.header.mapper
        )));
    }
    let policy = TranslationMapping::resolve(&image, &prof)?;
    // Profile validation checks declared mapper-2 bank bounds. Recheck
    // against the parsed ROM policy before any banked analysis slicing.
    if policy.is_banked() {
        let actual_bank_count = policy.bank_count();
        for entry in &prof.bank_entries {
            if entry.bank >= actual_bank_count {
                return Err(Error::Diagnostic(format!(
                    "bank_entry bank {} is out of range for parsed ROM's {actual_bank_count} mapper 2 banks",
                    entry.bank
                )));
            }
        }
        for call in &prof.bank_calls {
            if call.bank >= actual_bank_count {
                return Err(Error::Diagnostic(format!(
                    "bank_call bank {} is out of range for parsed ROM's {actual_bank_count} mapper 2 banks",
                    call.bank
                )));
            }
        }
    }
    let vectors = policy
        .vectors(image.prg)?
        .ok_or_else(|| Error::Diagnostic("could not read NMI/RESET/IRQ vectors from PRG".into()))?;
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

    // 3. Analyze. UxROM fixed code is discovered exactly once. Each physical
    // switchable bank is analyzed separately below and is rooted only by its
    // verified [[bank_entry]] facts; vectors are never replayed in those views.
    let banked = policy.is_banked();
    let fixed_start = policy.fixed_start();
    let code_bank_limit = if prof.mmc3_full_runtime() {
        sms_project::MMC3_PRG_DATA_BASE
    } else if policy.is_mmc3() {
        sms_project::MMC3_CODE_BANK_LIMIT
    } else if banked {
        sms_project::NES_PRG_BANK_BASE
    } else {
        256
    };
    let mut consume_entries: Vec<(Option<u8>, u16)> = [vectors.nmi, vectors.reset, vectors.irq]
        .into_iter()
        .map(|pc| (None, pc))
        .collect();
    consume_entries.extend(prof.functions.iter().map(|f| (None, f.addr)));
    consume_entries.extend(prof.bank_entries.iter().map(|f| (Some(f.bank), f.addr)));
    consume_entries.extend(prof.bank_calls.iter().map(|f| (Some(f.bank), f.target)));
    consume_entries.extend(
        prof.jump_tables
            .iter()
            .flat_map(|t| t.targets.iter())
            .map(|&pc| (None, pc)),
    );
    consume_entries.extend(prof.replacements.iter().map(|r| (None, r.addr)));
    consume_entries.extend(
        prof.jump_engines
            .iter()
            .flat_map(|s| s.targets.iter().chain(s.return_target.iter()))
            .filter_map(|target| consume_target_identity(&prof, target)),
    );
    check_consume_entries(&prof, &consume_entries)?;
    let mut bank_entries_by_bank: std::collections::BTreeMap<(u8, u16), Vec<u16>> =
        std::collections::BTreeMap::new();
    for entry in &prof.bank_entries {
        bank_entries_by_bank
            .entry((entry.bank, policy.window(entry.addr).start))
            .or_default()
            .push(entry.addr);
    }
    // A profiled inline table is a real reachability edge. Root its fixed
    // targets and its explicitly bank-qualified window targets; leave
    // unqualified window targets dynamic so analysis never invents physical
    // bank facts that the reference/profile did not establish.
    for site in &prof.jump_engines {
        for target in site.targets.iter().chain(site.return_target.iter()) {
            if let Some((Some(bank), addr)) = profile_target_identity(target)
                && addr < fixed_start
            {
                bank_entries_by_bank
                    .entry((bank, policy.window(addr).start))
                    .or_default()
                    .push(addr);
            }
        }
    }
    for entries in bank_entries_by_bank.values_mut() {
        entries.sort_unstable();
        entries.dedup();
    }

    // A window routine can call shared fixed code that is not otherwise a
    // vector/profile root. Pre-discover those cross-window references and feed
    // them into the one fixed-bank pass. The window pass itself cannot walk or
    // classify fixed bytes.
    let mut fixed_prof = prof.clone();
    for site in &prof.jump_engines {
        for target in site.targets.iter().chain(site.return_target.iter()) {
            if let Some((_, addr)) = profile_target_identity(target)
                && ((!banked && addr >= 0x8000) || (banked && addr >= fixed_start))
                && fixed_prof
                    .functions
                    .iter()
                    .all(|function| function.addr != addr)
            {
                fixed_prof.functions.push(profile::Function {
                    addr,
                    name: prof
                        .label_for(addr)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("func_{addr:04X}")),
                    note: Some("profiled JumpEngine target".to_string()),
                });
            }
        }
    }
    if banked {
        for (&(bank, window_start), entries) in &bank_entries_by_bank {
            let view = policy.analysis_view(image.prg, bank, window_start)?;
            let mut window_prof = prof.clone();
            window_prof.functions = entries
                .iter()
                .map(|&addr| profile::Function {
                    addr,
                    name: format!("L_b{bank}_{addr:04X}"),
                    note: None,
                })
                .collect();
            window_prof.jump_tables.clear();
            window_prof
                .jump_engines
                .retain(|site| site.bank == Some(bank));
            let window_analysis = analyze_with_continuation_roots(
                &view,
                nes_rom_like::Vectors {
                    nmi: 0,
                    reset: 0,
                    irq: 0,
                },
                &mut window_prof,
                policy.window(window_start),
                Some(bank),
            );
            for target in window_analysis
                .functions
                .functions
                .iter()
                .flat_map(|function| function.external_refs.iter().copied())
                .filter(|&target| target >= fixed_start)
            {
                if fixed_prof
                    .functions
                    .iter()
                    .all(|function| function.addr != target)
                {
                    fixed_prof.functions.push(profile::Function {
                        addr: target,
                        name: prof
                            .label_for(target)
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("func_{target:04X}")),
                        note: Some(format!("called from mapper bank {bank}")),
                    });
                }
            }
        }
        fixed_prof.jump_engines.retain(|site| site.bank.is_none());
    }
    let mut analysis_view = policy.analysis_view(image.prg, 0, 0x8000)?;
    if prof.cnrom_bus_experiment() && analysis_view.len() == 0x4000 {
        // Discovery/lifting index a full CPU PRG window. Keep both CPU
        // identities of the mirrored16KiB chip without changing raw assets.
        analysis_view.extend_from_within(..);
    }
    let analysis_vectors = nes_rom_like::Vectors {
        nmi: vectors.nmi,
        reset: vectors.reset,
        irq: vectors.irq,
    };
    let analyzed = if banked {
        analyze_with_continuation_roots(
            &analysis_view,
            analysis_vectors,
            &mut fixed_prof,
            analysis::AnalysisWindow {
                start: fixed_start,
                end_inclusive: 0xffff,
            },
            None,
        )
    } else {
        analyze_with_continuation_roots(
            &analysis_view,
            analysis_vectors,
            &mut fixed_prof,
            analysis::AnalysisWindow::FULL_PRG,
            None,
        )
    };

    // 4. Lift each discovered function into IR.
    //
    // Analysis can produce overlapping ranges when a routine's linear walk
    // crosses into another known root's entry. Trim each routine's end to
    // be no later than the next routine's start, so two routines never
    // both contain the same byte. This avoids duplicate L_XXXX labels in
    // the lowered output; internal branches that target the trimmed-off
    // tail become external references and resolve via the alias label.
    let mut funcs: Vec<analysis::DiscoveredFunction> = analyzed.functions.functions.clone();
    for f in &funcs {
        consume_entries.push((None, f.addr));
        consume_entries.extend(
            f.external_refs
                .iter()
                .chain(f.internal_labels.iter())
                .map(|&pc| (None, pc)),
        );
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
        .filter(|site| !banked || site.bank.is_none())
        .map(|s| ir::JumpEngineSite {
            caller: s.caller,
            targets: s.targets.clone(),
            return_target: s.return_target.clone(),
            tail_indices: s.tail_indices.clone(),
            stack_return_bytes: s.stack_return_bytes,
            target_entry_a: s.target_entry_a.clone(),
        })
        .collect();
    let return_escape_sites: Vec<ir::ReturnEscapeSite> = prof
        .return_escapes
        .iter()
        .filter(|site| !banked || site.bank.is_none())
        .map(|site| ir::ReturnEscapeSite {
            caller: site.caller,
            target: site.target,
            return_addr: site.return_addr,
            stack_bytes_already_consumed: site.stack_bytes_already_consumed,
            consume_at: site.consume_at,
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
            return_escape_sites: return_escape_sites.clone(),
            return_consume_sites: return_consume_sites(&prof, None),
            materialized_call_sites: materialized_call_sites(&prof, None),
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: prof.dynamic_cpu_bus(),
            data_regions: lift_data_regions(&prof, None),
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
            let has_terminator = r.ops.last().is_some_and(ir::Op::is_hard_terminator);
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
    // 4b. Banked-window translation units. Each physical bank is rooted only
    // at its verified entries and constrained to $8000-$BFFF. Calls into the
    // fixed window were folded into the one fixed pass above.
    if banked
        && if policy.is_mmc3() {
            !bank_entries_by_bank.is_empty()
        } else {
            !prof.bank_entries.is_empty()
        }
    {
        for ((bank, window_start), entries) in bank_entries_by_bank {
            let view = policy.analysis_view(image.prg, bank, window_start)?;
            let window = policy.window(window_start);
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
            bprof.jump_tables.clear();
            bprof.jump_engines.retain(|site| site.bank == Some(bank));
            let banalyzed = analyze_with_continuation_roots(
                &view,
                nes_rom_like::Vectors {
                    nmi: 0,
                    reset: 0,
                    irq: 0,
                },
                &mut bprof,
                window,
                Some(bank),
            );
            let mut bfuncs: Vec<analysis::DiscoveredFunction> =
                banalyzed.functions.functions.clone();
            for f in &bfuncs {
                consume_entries.push((Some(bank), f.addr));
                consume_entries.extend(
                    f.external_refs
                        .iter()
                        .chain(f.internal_labels.iter())
                        .map(|&pc| (Some(bank), pc)),
                );
            }
            bfuncs.sort_by_key(|f| f.addr);
            bfuncs.dedup_by_key(|f| f.addr);
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
            let bank_jump_engine_sites: Vec<ir::JumpEngineSite> = prof
                .jump_engines
                .iter()
                .filter(|site| site.bank == Some(bank))
                .map(|site| ir::JumpEngineSite {
                    caller: site.caller,
                    targets: site.targets.clone(),
                    return_target: site.return_target.clone(),
                    tail_indices: site.tail_indices.clone(),
                    stack_return_bytes: site.stack_return_bytes,
                    target_entry_a: site.target_entry_a.clone(),
                })
                .collect();
            let bank_return_escape_sites: Vec<ir::ReturnEscapeSite> = prof
                .return_escapes
                .iter()
                .filter(|site| site.bank == Some(bank))
                .map(|site| ir::ReturnEscapeSite {
                    caller: site.caller,
                    target: site.target,
                    return_addr: site.return_addr,
                    stack_bytes_already_consumed: site.stack_bytes_already_consumed,
                    consume_at: site.consume_at,
                })
                .collect();
            // Interior-alias pass (mirrors the main funcs' two-pass):
            // collect every referenced window pc, then re-lift with
            // extra labels so cross-routine branch targets resolve.
            let mut bank_referenced: std::collections::HashSet<u16> = Default::default();
            for f in &bfuncs {
                let opts = ir::LiftOptions {
                    start: f.addr,
                    end: f.end,
                    entry_name: String::new(),
                    jump_engine_sites: bank_jump_engine_sites.clone(),
                    return_escape_sites: bank_return_escape_sites.clone(),
                    return_consume_sites: return_consume_sites(&prof, Some(bank)),
                    materialized_call_sites: materialized_call_sites(&prof, Some(bank)),
                    window_label_prefix: Some(prefix.clone()),
                    window_label_range: window.start..window.end_inclusive + 1,
                    dynamic_cpu_bus: prof.dynamic_cpu_bus(),
                    data_regions: lift_data_regions(&prof, Some(bank)),
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
                let in_window = window.contains(f.addr);
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
                    jump_engine_sites: bank_jump_engine_sites.clone(),
                    return_escape_sites: bank_return_escape_sites.clone(),
                    return_consume_sites: return_consume_sites(&prof, Some(bank)),
                    materialized_call_sites: materialized_call_sites(&prof, Some(bank)),
                    window_label_prefix: in_window.then(|| prefix.clone()),
                    window_label_range: window.start..window.end_inclusive + 1,
                    dynamic_cpu_bus: prof.dynamic_cpu_bus(),
                    data_regions: lift_data_regions(&prof, Some(bank)),
                    extra_label_pcs: extras,
                };
                match ir::lift_range(&view, &opts) {
                    Ok(mut r) => {
                        ir::mark_rts_dispatch(&mut r.ops);
                        let has_terminator = r.ops.last().is_some_and(ir::Op::is_hard_terminator);
                        if !has_terminator {
                            // Trimmed fallthrough: continue into the next
                            // routine via an explicit jump (bank-prefixed
                            // when the target is in the window).
                            let tgt = if window.contains(r.end) {
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
            return_escape_sites: return_escape_sites.clone(),
            return_consume_sites: return_consume_sites(&prof, None),
            materialized_call_sites: materialized_call_sites(&prof, None),
            window_label_prefix: None,
            window_label_range: 0x8000..0xC000,
            dynamic_cpu_bus: prof.dynamic_cpu_bus(),
            data_regions: lift_data_regions(&prof, None),
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
                let has_terminator = r.ops.last().is_some_and(ir::Op::is_hard_terminator);
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
    for wait in &prof.source_poll_loops {
        let offset = usize::from(wait.at - 0x8000) % image.prg.len();
        let expected = [0xa5, wait.zp, 0xd0, 0xfc];
        let sources: Vec<_> = routines
            .iter()
            .flat_map(|r| r.ops.iter())
            .filter_map(|op| {
                if let ir::Op::Source {
                    pc, instruction, ..
                } = op
                {
                    Some((*pc, instruction))
                } else {
                    None
                }
            })
            .collect();
        let head = sources
            .iter()
            .filter(|(pc, instruction)| {
                *pc == wait.at
                    && instruction.is_some_and(|i| {
                        i.opcode == 0xa5 && i.operand == cpu6502::Operand::Addr(u16::from(wait.zp))
                    })
            })
            .count();
        let branch = sources
            .iter()
            .filter(|(pc, instruction)| {
                *pc == wait.at + 2
                    && instruction.is_some_and(|i| {
                        i.opcode == 0xd0 && i.operand == cpu6502::Operand::Relative(-4)
                    })
            })
            .count();
        if image.prg.get(offset..offset + 4) != Some(expected.as_slice())
            || head != 1
            || branch != 1
        {
            return Err(Error::Diagnostic(format!(
                "source_poll_loop ${:04X} requires unique decoded LDA zp / BNE same-page self and exact original bytes",
                wait.at
            )));
        }
    }
    if prof.mmc3_full_runtime() {
        for wait in &prof.cooperative_waits {
            let offset = usize::from(wait.bank) * 0x2000 + usize::from(wait.at & 0x1fff);
            let expected = [
                0xa9,
                1,
                0x85,
                wait.tick_enable,
                0xa9,
                0,
                0x85,
                wait.tick,
                0xa5,
                wait.tick,
                0x10,
                0xfc,
            ];
            if image.prg.get(offset - 8..offset + 4) != Some(expected.as_slice()) {
                return Err(Error::Diagnostic(format!(
                    "cooperative_wait {}:${:04X}: expected enable/clear setup and LDA tick / BPL self",
                    wait.bank, wait.at
                )));
            }
            let mut owners = 0;
            for routine in &mut routines {
                if profile_target_identity(&routine.name).and_then(|(bank, _)| bank)
                    != Some(wait.bank)
                {
                    continue;
                }
                if let Some(index) = routine
                    .ops
                    .iter()
                    .position(|op| matches!(op, ir::Op::Source { pc, .. } if *pc == wait.at))
                {
                    if prof.replacement_for(routine.entry).is_some() {
                        return Err(Error::Diagnostic(
                            "cooperative_wait owner cannot be replaced".into(),
                        ));
                    }
                    routine.ops.insert(
                        index + 1,
                        ir::Op::CooperativeWait {
                            tick: wait.tick,
                            tick_enable: wait.tick_enable,
                        },
                    );
                    owners += 1;
                }
            }
            if owners != 1 {
                return Err(Error::Diagnostic(format!(
                    "cooperative_wait {}:${:04X} requires exactly one translated owner, found {owners}",
                    wait.bank, wait.at
                )));
            }
        }
    }
    let mut mmc3_continuations = std::collections::BTreeSet::new();
    if prof.dynamic_cpu_bus() {
        if !lift_failures.is_empty() {
            return Err(Error::Diagnostic(format!(
                "MMC3 full runtime lift failed: {}",
                lift_failures.join("; ")
            )));
        }
        mmc3_continuations = add_mmc3_continuation_labels(&mut routines, &prof);
    } else if policy.is_mmc3() {
        check_mmc3_mapping_continuations(&routines)?;
    }

    for routine in &routines {
        let bank = profile_target_identity(&routine.name).and_then(|(bank, _)| bank);
        for target in routine
            .branch_labels
            .iter()
            .chain(routine.external_calls.iter())
        {
            if let Some((target_bank, addr)) = consume_target_identity(&prof, target) {
                consume_entries.push((target_bank.or(bank), addr));
            }
        }
    }
    check_consume_entries(&prof, &consume_entries)?;

    // Unlike ordinary unsupported routines, a profiled early ownership
    // transfer cannot degrade to a missing stub while its later JMP remains.
    let mut consume_owners = std::collections::HashSet::new();
    for site in prof
        .return_escapes
        .iter()
        .filter(|site| site.stack_bytes_already_consumed)
    {
        let start = site.consume_at.expect("profile validates consume_at");
        let owners: Vec<_> =
            routines
                .iter()
                .filter(|routine| {
                    let matching_bank = match site.bank {
                        Some(bank) => routine.name.starts_with(&format!("L_b{bank}_")),
                        None => !routine.name.starts_with("L_b"),
                    };
                    matching_bank && routine.ops.windows(2).any(|ops| {
                        matches!(
                            (&ops[0], &ops[1]),
                            (ir::Op::Source { pc, .. }, ir::Op::ReturnEscapeConsume { return_addr })
                                if *pc == start && *return_addr == site.return_addr
                        )
                    }) && routine.ops.iter().any(|op| {
                        matches!(op,
                ir::Op::Source { pc, .. } if *pc == site.caller)
                    })
                })
                .collect();
        if owners.len() != 1 {
            return Err(Error::Diagnostic(format!(
                "return_escape consume_at ${start:04X} in bank {:?} must have exactly one fully lifted owner; found {}. {}",
                site.bank,
                owners.len(),
                lift_failures.join("; ")
            )));
        }
        consume_owners.insert(owners[0].name.clone());
        if prof.replacement_for(owners[0].entry).is_some()
            || prof
                .replacements
                .iter()
                .any(|replacement| replacement.addr >= start && replacement.addr <= site.caller)
        {
            return Err(Error::Diagnostic(format!(
                "return_escape consuming owner {} cannot be replaced",
                owners[0].name
            )));
        }
    }

    // Standalone pairs have no artificial terminal edge: only the second PLA
    // cannot be entered directly. Calls and pairs must survive the complete
    // pipeline, never silently disappear into unsupported/replacement stubs.
    for site in &prof.return_consumes {
        let facts = std::iter::once((site.bank, site.at, None)).chain(
            site.calls
                .iter()
                .map(|call| (call.bank, call.caller, Some(call.target))),
        );
        for (bank, pc, call_target) in facts {
            let owners: Vec<_> = routines
                .iter()
                .filter(|routine| {
                    let identity = profile_target_identity(&routine.name).and_then(|(b, _)| b);
                    identity == bank
                        && routine.ops.windows(2).any(|ops| {
                            matches!(&ops[0], ir::Op::Source { pc: source, .. } if *source == pc)
                                && match (&ops[1], call_target) {
                                    (ir::Op::ReturnConsume { return_addrs }, None) => {
                                        *return_addrs
                                            == site
                                                .calls
                                                .iter()
                                                .map(|c| c.caller + 2)
                                                .collect::<Vec<_>>()
                                    }
                                    (
                                        ir::Op::MaterializedJsr {
                                            target,
                                            return_addr,
                                        },
                                        Some(expected),
                                    ) => {
                                        *return_addr == pc + 2
                                            && profile_target_identity(target)
                                                .is_some_and(|(_, addr)| addr == expected)
                                    }
                                    _ => false,
                                }
                        })
                })
                .collect();
            if owners.len() != 1 {
                return Err(Error::Diagnostic(format!(
                    "return_consume site ${pc:04X} in bank {bank:?} must have one fully lifted owner; found {}. {}",
                    owners.len(),
                    lift_failures.join("; ")
                )));
            }
            let owner = owners[0];
            if prof.replacement_for(owner.entry).is_some()
                || prof.replacements.iter().any(|r| {
                    r.addr >= pc
                        && r.addr
                            <= if call_target.is_some() {
                                pc + 2
                            } else {
                                site.second_pla.unwrap_or(pc + 1)
                            }
                })
            {
                return Err(Error::Diagnostic(format!(
                    "return_consume owner {} cannot be replaced",
                    owner.name
                )));
            }
            consume_owners.insert(owner.name.clone());
        }
    }

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
                    Op::Jsr { target }
                    | Op::MaterializedJsr { target, .. }
                    | Op::Jmp { target }
                    | Op::ReturnEscape { target, .. } => {
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

    // Phase S: profile-guided hot grouping. Routines named in the profile's
    // `[translation] hot_group` (from the measured far-transfer histogram)
    // are emitted first, in list order, so the per-frame call cluster packs
    // into the same early section(s); everything else keeps address order.
    if !prof.translation.hot_group.is_empty() {
        let rank = |r: &ir::Routine| -> usize {
            prof.translation
                .hot_group
                .iter()
                .position(|&a| a == r.entry)
                .unwrap_or(usize::MAX)
        };
        let mut hot: Vec<ir::Routine> = Vec::new();
        let mut rest: Vec<ir::Routine> = Vec::new();
        for r in routines.drain(..) {
            if rank(&r) != usize::MAX {
                hot.push(r);
            } else {
                rest.push(r);
            }
        }
        hot.sort_by_key(|r| rank(r));
        hot.extend(rest);
        routines = hot;
    }

    // Phase S: edge-weighted bank placement from a measured far-transfer
    // profile (FD_FAR_EDGES). Clusters routines connected by hot dynamic
    // edges under a conservative size estimate; ordering anchors each
    // cluster at its earliest member's original position, so routines
    // outside clusters keep full address-order locality (the two earlier
    // static/manual grouping attempts lost exactly that).
    if let Some(rel) = prof.translation.edge_profile.clone() {
        let path = args
            .profile
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join(&rel);
        match std::fs::read_to_string(&path) {
            Err(e) => eprintln!(
                "warning: edge_profile {} unreadable ({e}); keeping address order",
                path.display()
            ),
            Ok(text) => {
                let mut ranges: Vec<(u16, u16, usize)> = routines
                    .iter()
                    .enumerate()
                    .map(|(i, r)| (r.entry, r.end, i))
                    .collect();
                ranges.sort_unstable();
                let starts: Vec<u16> = ranges.iter().map(|&(s, _, _)| s).collect();
                let resolve = |addr: u16| -> Option<usize> {
                    let p = starts.partition_point(|&s| s <= addr);
                    let &(s, e, idx) = ranges.get(p.checked_sub(1)?)?;
                    (addr >= s && addr < e).then_some(idx)
                };
                let mut edges: std::collections::HashMap<(usize, usize), u64> = Default::default();
                for line in text.lines() {
                    let mut it = line.split_whitespace();
                    let (Some(c), Some(t), Some(n)) = (it.next(), it.next(), it.next()) else {
                        continue;
                    };
                    let (Ok(c), Ok(t), Ok(n)) = (
                        u16::from_str_radix(c, 16),
                        u16::from_str_radix(t, 16),
                        n.parse::<u64>(),
                    ) else {
                        continue;
                    };
                    if let (Some(ci), Some(ti)) = (resolve(c), resolve(t))
                        && ci != ti
                    {
                        *edges.entry((ci.min(ti), ci.max(ti))).or_default() += n;
                    }
                }
                let n = routines.len();
                let est: Vec<usize> = routines.iter().map(|r| 32 + r.ops.len() * 12).collect();
                fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
                    while parent[x] != x {
                        parent[x] = parent[parent[x]];
                        x = parent[x];
                    }
                    x
                }
                let mut parent: Vec<usize> = (0..n).collect();
                let mut csize: Vec<usize> = est;
                let mut sorted: Vec<((usize, usize), u64)> = edges.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let mut merged = 0usize;
                for ((i, j), _w) in sorted {
                    let (ri, rj) = (uf_find(&mut parent, i), uf_find(&mut parent, j));
                    if ri != rj && csize[ri] + csize[rj] <= TRANSLATED_SECTION_CAPACITY {
                        parent[rj] = ri;
                        csize[ri] += csize[rj];
                        merged += 1;
                    }
                }
                if merged > 0 {
                    let cluster_of: Vec<usize> = (0..n).map(|i| uf_find(&mut parent, i)).collect();
                    let mut anchor: std::collections::HashMap<usize, usize> = Default::default();
                    for i in 0..n {
                        anchor.entry(cluster_of[i]).or_insert(i);
                    }
                    let mut order: Vec<usize> = (0..n).collect();
                    order.sort_by_key(|&i| (anchor[&cluster_of[i]], i));
                    let mut slots: Vec<Option<ir::Routine>> =
                        routines.drain(..).map(Some).collect();
                    for i in order {
                        routines.push(slots[i].take().expect("placement permutation"));
                    }
                    eprintln!("edge placer: {merged} merges from the measured profile");
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
    // Transactional next-fit sizing records an explicit logical section for
    // every routine. Final emission follows that frozen plan; near-call
    // downgrades only shrink bodies and never repack or relocate routines.
    let flag_reads: std::collections::HashMap<String, u8> = {
        let mut m = std::collections::HashMap::new();
        for r in &routines {
            let mask = lower::routine_incoming_flag_reads(&r.ops);
            // A dynamic MMC3 address has no proven physical target. Keep
            // only qualified summaries so the lowerer preserves flags
            // conservatively for an unqualified dispatch.
            if !policy.is_mmc3() || r.entry >= fixed_start {
                m.insert(format_label(r.entry), mask);
            }
            m.insert(r.name.clone(), mask);
        }
        m
    };

    let emit_translated = |section_map: &std::collections::HashMap<String, usize>,
                           routine_sections: Option<&[u32]>|
     -> Result<
        (z80_emit::Program, Vec<String>, Vec<String>, Vec<u32>),
        Error,
    > {
        let mut program = z80_emit::Program::new();
        program.set_wide_continuation_banks(prof.mmc3_full_runtime());
        let mut section_idx: u32 = 0;
        begin_translated_section(&mut program, section_idx, code_bank_limit)?;
        program.prepopulate_label_section(section_map);
        let opts = LowerOptions {
            profile: Some(&prof),
            emit_source_comments: true,
            routine_flag_reads: Some(&flag_reads),
        };

        emit_translated_vector_aliases(&mut program, vectors.reset, vectors.nmi, vectors.irq);

        let mut lower_failures: Vec<String> = Vec::new();
        let mut defined_labels: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        defined_labels.insert("translated_reset".to_string());
        defined_labels.insert("translated_nmi".to_string());
        defined_labels.insert("translated_irq".to_string());

        if std::env::var("N2S_DEBUG_RNAMES").is_ok() {
            let mut names: std::collections::HashMap<&str, usize> = Default::default();
            for r in &routines {
                *names.entry(r.name.as_str()).or_insert(0) += 1;
            }
            for (n, c) in names.iter().filter(|(_, c)| **c > 1) {
                eprintln!("ROUTINE NAME x{c}: {n}");
            }
        }
        let emit_routine = |program: &mut z80_emit::Program,
                            defined_labels: &mut std::collections::BTreeSet<String>,
                            lower_failures: &mut Vec<String>,
                            r: &ir::Routine|
         -> Result<(), Error> {
            let before = lower_failures.len();
            emit_translated_routine(program, defined_labels, lower_failures, r, &opts)?;
            if consume_owners.contains(&r.name) && lower_failures.len() != before {
                return Err(Error::Diagnostic(format!(
                    "return_escape/return_consume consuming owner {} failed lowering: {}",
                    r.name,
                    lower_failures[before..].join("; ")
                )));
            }
            Ok(())
        };

        let mut assigned_sections = Vec::with_capacity(routines.len());
        for (routine_index, r) in routines.iter().enumerate() {
            if let Some(plan) = routine_sections {
                let planned = *plan.get(routine_index).ok_or_else(|| {
                    Error::Diagnostic("frozen layout is missing a routine assignment".to_string())
                })?;
                if planned < section_idx || planned > section_idx + 1 {
                    return Err(Error::Diagnostic(format!(
                        "frozen layout has impossible section {planned} for {} (current {section_idx})",
                        r.name
                    )));
                }
                if planned > section_idx {
                    begin_translated_section(&mut program, planned, code_bank_limit)?;
                    section_idx = planned;
                }
                let mut candidate = program.clone();
                let mut candidate_labels = defined_labels.clone();
                let mut candidate_failures = lower_failures.clone();
                emit_routine(
                    &mut candidate,
                    &mut candidate_labels,
                    &mut candidate_failures,
                    r,
                )?;
                let used = translated_section_usage(&candidate);
                if used > TRANSLATED_SECTION_CAPACITY {
                    return Err(Error::Diagnostic(format!(
                        "frozen layout overflow for {} in section {planned}: {used} bytes",
                        r.name
                    )));
                }
                program = candidate;
                defined_labels = candidate_labels;
                lower_failures = candidate_failures;
                assigned_sections.push(planned);
            } else {
                let mut state = (defined_labels, lower_failures);
                let assigned = pack_sizing_candidate(
                    &mut program,
                    &mut state,
                    &mut section_idx,
                    code_bank_limit,
                    &r.name,
                    |candidate, state| emit_routine(candidate, &mut state.0, &mut state.1, r),
                )?;
                defined_labels = state.0;
                lower_failures = state.1;
                assigned_sections.push(assigned);
            }
        }

        // Pre-declare runtime symbols (real bodies live in runtime/*.s;
        // placeholders keep z80_emit patch resolution happy). Profile
        // replacement targets are runtime labels too.
        program.section("runtime_forward_decls");
        for sym in RUNTIME_SYMBOLS {
            program.label(sym);
            program.ret();
        }
        if prof.mmc3_full_runtime() {
            for sym in [
                "rt_mmc3_read_bus",
                "rt_mmc3_write_bus",
                "rt_mmc3_indirect_jump",
                "rt_mmc3_guard_pla",
            ] {
                program.label(sym);
                program.ret();
            }
        }
        if prof.cnrom_bus_experiment() {
            for sym in [
                "rt_cpu_read_bus",
                "rt_cpu_write_bus",
                "rt_cpu_indirect_jump",
            ] {
                program.label(sym);
                program.ret();
            }
        }
        if prof.source_clock_experiment() {
            for sym in [
                "rt_source_begin",
                "rt_source_poll_loop",
                "rt_source_read_bus",
                "rt_source_write_bus",
                "rt_source_push",
                "rt_source_pop",
                "rt_source_jsr",
                "rt_source_rts",
                "rt_source_rti",
                "rt_source_brk",
                "rt_source_indirect_jump",
            ] {
                program.label(sym);
                program.ret();
            }
        }
        for rep in &prof.replacements {
            if RUNTIME_SYMBOLS.contains(&rep.runtime_label.as_str()) {
                continue; // already forward-declared above
            }
            program.label(&rep.runtime_label);
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
                // A missing explicit physical-bank fact must trap, not be
                // reinterpreted as whichever bank happens to be mapped now.
                .filter(|h| !policy.is_mmc3() || h.len() == 4)
                .map(|h| h.rsplit('_').next().unwrap_or(h))
                .and_then(|h| u16::from_str_radix(h, 16).ok())
                .filter(|a| (0x8000..fixed_start).contains(a) && banked)
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
        // Fixed-bank routines use bank $FF (matches any window bank). Keep the
        // records address-sorted and emit a high-byte directory so the runtime
        // starts at the requested 256-byte NES page instead of linearly
        // walking every routine discovered before it.
        program.section("rt_dispatch_table_sec");
        program.label("rt_dispatch_table");
        let mut dispatch_records = routines
            .iter()
            .map(|r| match r.name.strip_prefix("L_b") {
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
            })
            .collect::<Vec<_>>();
        if prof.dynamic_cpu_bus() {
            for routine in &routines {
                for label in &routine.branch_labels {
                    if mmc3_continuations.contains(label)
                        && let Some((bank, addr)) = profile_target_identity(label)
                    {
                        dispatch_records.push((bank.unwrap_or(0xff), addr, label.clone()));
                    }
                }
            }
            dispatch_records.sort();
            dispatch_records.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        }
        // Computed dispatch honors profile replacements too: a dispatched
        // NES address whose routine is replaced lands on the runtime hook
        // (slot 0) instead of the translated body.
        for rec in dispatch_records.iter_mut() {
            if rec.0 == 0xFF
                && let Some(rep) = prof.replacement_for(rec.1)
            {
                rec.2 = rep.runtime_label.clone();
            }
        }
        // Preserve the dispatch table's precedence at duplicate addresses:
        // fixed-bank entries historically appeared before mapper-window
        // entries and therefore win the runtime's first-match search.
        dispatch_records.sort_by_key(|(bank, addr, _)| {
            (
                *addr,
                if *bank == 0xFF {
                    0u16
                } else {
                    *bank as u16 + 1
                },
            )
        });
        if prof.source_clock_experiment() {
            // Complete high-byte groups never straddle a mapped record bank.
            // Each group has its own real terminator, including empty pages.
            let mut bank = 0u8;
            let mut used = 0usize;
            program.set_section_project_data_placement(bank, 1);
            for page in 0x80u16..=0xff {
                let group: Vec<_> = dispatch_records
                    .iter()
                    .filter(|(_, addr, _)| addr >> 8 == page)
                    .collect();
                let size = group.len() * 6 + 2;
                if size > 0x4000 {
                    return Err(Error::Diagnostic(
                        "source clock dispatch page exceeds a ROM bank".into(),
                    ));
                }
                if used + size > 0x4000 {
                    bank = bank.checked_add(1).ok_or_else(|| {
                        Error::Diagnostic(
                            "source clock dispatch banks exceed mapper addressability".into(),
                        )
                    })?;
                    program.section(&format!("rt_dispatch_records_{bank}_sec"));
                    program.set_section_project_data_placement(bank, 1);
                    used = 0;
                }
                program.label(format!("rt_dispatch_page_{page:02X}"));
                for (nes_bank, addr, label) in group {
                    program.dispatch_entry(*addr, *nes_bank, label);
                }
                program.data(None, &[0, 0]);
                used += size;
            }
            program.section("rt_dispatch_directory_sec");
            program.set_section_placement(0, 0);
            program.label("rt_dispatch_page_table");
            for page in 0x80u16..=0xff {
                let label = format!("rt_dispatch_page_{page:02X}");
                program.bank_label(&label);
                program.word_label(&label);
            }
            program.label("rt_dispatch_directory_end");
            return Ok((program, lower_failures, unresolved, assigned_sections));
        }
        let page_counts = prof
            .mmc3_full_runtime()
            .then(|| mmc3_dispatch_page_counts(&dispatch_records))
            .transpose()?;
        let mut next_record = 0usize;
        for page in 0x80u16..=0xFF {
            program.label(format!("rt_dispatch_page_{page:02X}"));
            while next_record < dispatch_records.len()
                && (dispatch_records[next_record].1 >> 8) == page
            {
                let (bank, addr, label) = &dispatch_records[next_record];
                if page_counts.is_some() {
                    program.label(format!("rt_dispatch_record_{next_record}"));
                }
                program.dispatch_entry(*addr, *bank, label);
                next_record += 1;
            }
        }
        if page_counts.is_some() {
            program.label(format!("rt_dispatch_record_{next_record}"));
        }
        program.data(None, &[0x00, 0x00]); // terminator: addr $0000
        if page_counts.is_some() {
            program.label("rt_dispatch_table_end");
            // Absolute directory references need no mapper change. Pin this
            // small section so an exhausted bank zero fails at link time.
            program.section("rt_dispatch_directory_sec");
            program.set_section_placement(0, 0);
        }
        program.label("rt_dispatch_page_table");
        for page in 0x80u16..=0xFF {
            program.word_label(&format!("rt_dispatch_page_{page:02X}"));
        }
        if let Some(counts) = page_counts {
            program.data(Some("rt_dispatch_page_counts"), &counts);
            program.label("rt_dispatch_directory_end");
            assert_eq!(program.current_section_len(), 256 + 128);
            let indices = mmc3_dispatch_pointer_indices(&dispatch_records, &counts);
            for (bank_offset, chunk) in indices.as_chunks::<0x2000>().0.iter().enumerate() {
                program.section(&format!("rt_dispatch_index_{bank_offset}_sec"));
                program.set_section_project_data_placement(bank_offset as u8, 1);
                program.label(format!("rt_dispatch_index_{bank_offset}"));
                for index in chunk {
                    program.word_label(&format!("rt_dispatch_record_{index}"));
                }
                assert_eq!(program.current_section_len(), 0x4000);
            }
        }

        Ok((program, lower_failures, unresolved, assigned_sections))
    };

    // Sizing uses pessimistic far forms and records logical section IDs;
    // final emission consumes those IDs exactly while near forms only shrink.
    let empty_map: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let (sizing_prog, _f, _u, routine_sections) = emit_translated(&empty_map, None)?;
    let section_map = sizing_prog.label_section_snapshot();
    if std::env::var("N2S_DEBUG_SECTIONS").is_ok() {
        for l in ["translated_reset", "L_8000", "L_800F", "L_8220", "L_9000"] {
            eprintln!("map[{l}] = {:?}", section_map.get(l));
        }
    }
    let (program, lower_failures, unresolved, _) =
        emit_translated(&section_map, Some(&routine_sections))?;

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
    if prof.source_clock_experiment() {
        let code_after_data =
            31 + image.chr.len().div_ceil(0x4000) as u32 + u32::from(build.project_data_bank_count);
        build.asm = remap_source_code_banks(&build.asm, code_after_data)?;
    }

    // 8. Convert assets (CHR + a default palette + nametable placeholder).
    // CHR-RAM carts (chr_kib = 0) ship no pattern data: build the asset
    // set from an all-zero 8 KiB CHR (blank tiles, identity maps). The
    // runtime $2007 pattern-write conversion fills real tiles in play.
    let chr_ram_blank;
    let chr_source: &[u8] = if image.chr.is_empty() || prof.dynamic_cpu_bus() {
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
    let (prg_low, prg_banks) = if policy.is_mmc3() {
        (
            None,
            Some(
                image
                    .prg
                    .as_chunks::<0x4000>()
                    .0
                    .iter()
                    .map(|pair| pair.to_vec())
                    .collect(),
            ),
        )
    } else if banked {
        let legacy = policy.legacy().expect("UxROM mapping");
        let banks: Vec<Vec<u8>> = (0..policy.bank_count())
            .map(|bank| legacy.prg_bank(image.prg, bank).map(|bytes| bytes.to_vec()))
            .collect::<Result<_, _>>()?;
        (None, Some(banks))
    } else {
        (
            Some(
                policy
                    .legacy()
                    .expect("NROM mapping")
                    .lower_prg(image.prg)
                    .to_vec(),
            ),
            None,
        )
    };
    // Mirror the fixed upper PRG window as well. The translated code can run
    // from generated banks in slot 1, so original fixed-bank data tables such
    // as SMB's Bitmasks at $C68A are read via a slot-2 runtime helper.
    let prg_high = Some(if policy.is_mmc3() {
        image.prg[image.prg.len() - 0x4000..].to_vec()
    } else {
        policy
            .legacy()
            .expect("legacy mapping")
            .fixed_prg(image.prg)
            .to_vec()
    });
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
        // Preserve accepted legacy layouts. Synthetic CNROM reserves raw
        // CHR pages31..38 in a1MiB image; this is bus evidence, not a claim
        // that its unfinished presentation works on real SMS hardware.
        rom_kib: if prof.source_clock_experiment() {
            4096 // all 256 Sega bank-register identities, without asset overlap
        } else if policy.is_mmc3() {
            2048
        } else if prof.cnrom_bus_experiment() {
            1024
        } else {
            512
        },
        region: 0x4C,
        title: truncate_title(&prof.rom.name),
        mirroring,
        raw_ciram_backend: RawCiramBackend::SramSlot2,
        mapper: prof.rom.mapper,
        uxrom_bank_count: (banked && !policy.is_mmc3()).then_some(policy.bank_count()),
        mmc3_prg_bank_count: policy.is_mmc3().then_some(policy.bank_count()),
        cnrom: prof
            .cnrom_bus_experiment()
            .then_some(sms_project::CnromConfig {
                bus_conflicts: image.header.submapper == 2,
                prg_ram: image.header.prg_ram_size == 2048,
            }),
        uxrom_bus_conflicts: policy
            .legacy()
            .and_then(|p| p.uxrom_bus_conflicts())
            .map(|mode| match mode {
                nes_rom::UxromBusConflicts::None => sms_project::UxromBusConflicts::None,
                nes_rom::UxromBusConflicts::And => sms_project::UxromBusConflicts::And,
            }),
        chr_ram: image.chr.is_empty(),
        input_action: prof.input.mode == profile::InputMode::Action,
        input_pause_start: prof.input.pause_start,
        scroll_split: prof.render.scroll_split,
        top_tile_remap_rows: prof.render.top_tile_remap_rows,
        top_tile_remap_from: prof.render.top_tile_remap_from.clone(),
        top_tile_remap_to: prof.render.top_tile_remap_to,
        chr_ram_bg_identity: prof.render.chr_ram_bg_identity,
        native_calls: prof.native_calls(),
        runtime_defines: prof.effective_runtime_defines(),
    };
    sms_project::emit_project(&args.out, &build, &project_assets, &cfg, runtime_dir)?;

    // Phase S3.1 — Tier-3 relayout closure analysis (report only, no
    // codegen). For every RAM address reached by an indexed access, record
    // how the program touches it; a candidate array is transposable only
    // if every access inside its span is index-register-relative to its
    // own base and no dynamic pointer ((zp),Y / (zp,X)) can alias RAM at
    // all without further value analysis. See docs/speed-recovery-plan.md.
    {
        use ir::{AddrExpr, MemRegion, Op};
        use std::collections::BTreeMap;
        let mut idx_bases: BTreeMap<u16, (u32, u32)> = BTreeMap::new(); // base -> (x_count, y_count)
        let mut const_hits: BTreeMap<u16, u32> = BTreeMap::new();
        let mut zp_indexed: BTreeMap<u8, u32> = BTreeMap::new();
        let mut ind_ptrs: BTreeMap<u8, u32> = BTreeMap::new();
        let ram_region = |r: MemRegion| {
            matches!(
                r,
                MemRegion::Ram | MemRegion::RamMirror | MemRegion::ZeroPage
            )
        };
        for r in &routines {
            for op in &r.ops {
                let acc: Option<(&AddrExpr, MemRegion)> = match op {
                    Op::LdaMem { addr, region }
                    | Op::LdxMem { addr, region }
                    | Op::LdyMem { addr, region }
                    | Op::StaMem { addr, region }
                    | Op::StxMem { addr, region }
                    | Op::StyMem { addr, region }
                    | Op::AdcMem { addr, region }
                    | Op::SbcMem { addr, region }
                    | Op::CmpMem { addr, region }
                    | Op::CpxMem { addr, region }
                    | Op::CpyMem { addr, region }
                    | Op::AndMem { addr, region }
                    | Op::OraMem { addr, region }
                    | Op::EorMem { addr, region }
                    | Op::BitMem { addr, region }
                    | Op::IncMem { addr, region }
                    | Op::DecMem { addr, region }
                    | Op::AslMem { addr, region }
                    | Op::LsrMem { addr, region }
                    | Op::RolMem { addr, region }
                    | Op::RorMem { addr, region }
                    | Op::SaxMem { addr, region } => Some((addr, *region)),
                    _ => None,
                };
                let Some((addr, region)) = acc else { continue };
                match addr {
                    AddrExpr::AbsIndexedX(b) if ram_region(region) => {
                        idx_bases.entry(*b).or_default().0 += 1;
                    }
                    AddrExpr::AbsIndexedY(b) if ram_region(region) => {
                        idx_bases.entry(*b).or_default().1 += 1;
                    }
                    AddrExpr::Const(a) if ram_region(region) => {
                        *const_hits.entry(*a).or_default() += 1;
                    }
                    AddrExpr::ZpConst(z) => {
                        *const_hits.entry(*z as u16).or_default() += 1;
                    }
                    AddrExpr::ZpIndexedX(z) | AddrExpr::ZpIndexedY(z) => {
                        *zp_indexed.entry(*z).or_default() += 1;
                    }
                    AddrExpr::IndirectX(z) | AddrExpr::IndirectY(z) => {
                        *ind_ptrs.entry(*z).or_default() += 1;
                    }
                    _ => {}
                }
            }
        }
        let mut txt = String::new();
        txt.push_str(
            "Tier-3 relayout closure analysis (S3.1)\n\
             ========================================\n\
             A candidate parallel array [base .. next_base) is transposable only\n\
             when every access in its span is `base,X`/`base,Y` with the SAME\n\
             base and index meaning, no bare Const access lands inside the span\n\
             (or each such access is individually relocatable), and no dynamic\n\
             pointer can alias it. Dynamic pointers below alias ALL of RAM\n\
             absent value analysis, so any nonzero pointer-access count keeps\n\
             whole-program relayout in research territory.\n\n",
        );
        txt.push_str(&format!(
            "dynamic-pointer accesses ((zp),Y / (zp,X)): {} sites across {} zero-page pointers\n",
            ind_ptrs.values().sum::<u32>(),
            ind_ptrs.len()
        ));
        for (zp, n) in &ind_ptrs {
            txt.push_str(&format!("  ptr zp ${zp:02X}: {n} sites\n"));
        }
        txt.push_str(&format!(
            "\nzp,X / zp,Y indexed sites: {} (zero-page relayout candidates share these)\n\n",
            zp_indexed.values().sum::<u32>()
        ));
        txt.push_str("indexed bases (span = to next observed base):\n");
        let bases: Vec<u16> = idx_bases.keys().copied().collect();
        for (bi, base) in bases.iter().enumerate() {
            let (xs, ys) = idx_bases[base];
            let span_end = bases
                .get(bi + 1)
                .copied()
                .unwrap_or_else(|| base.saturating_add(0x100).min(0x0800))
                .max(base.saturating_add(1));
            let aliased: Vec<String> = const_hits
                .range(base + 1..span_end)
                .map(|(a, n)| format!("${a:04X}x{n}"))
                .collect();
            txt.push_str(&format!(
                "  ${base:04X} span ${:04X}: {xs:>3} ,X  {ys:>3} ,Y  {}\n",
                span_end,
                if aliased.is_empty() {
                    "CLEAN".to_string()
                } else {
                    format!("ALIASED by const: {}", aliased.join(" "))
                }
            ));
        }
        let clean = bases
            .iter()
            .enumerate()
            .filter(|(bi, base)| {
                let span_end = bases
                    .get(bi + 1)
                    .copied()
                    .unwrap_or_else(|| base.saturating_add(0x100).min(0x0800))
                    .max(base.saturating_add(1));
                const_hits.range(**base + 1..span_end).next().is_none()
            })
            .count();
        txt.push_str(&format!(
            "\nsummary: {} indexed bases, {clean} with const-clean spans, {} dynamic-pointer sites\n",
            bases.len(),
            ind_ptrs.values().sum::<u32>()
        ));
        let reports_dir = args.out.join("reports");
        std::fs::create_dir_all(&reports_dir)?;
        std::fs::write(reports_dir.join("relayout.txt"), &txt)?;
    }

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
            results.push(validate_translation_routine(
                policy,
                image.prg,
                r,
                args.validate_vectors,
                prof.cnrom_bus_experiment(),
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

/// Source-clock-only physical placement. Logical section IDs remain unchanged
/// for near/far analysis; WLA resolves every bank-of-label after this remap.
fn remap_source_code_banks(asm: &str, code_after_data: u32) -> Result<String, Error> {
    let lines: Vec<_> = asm.lines().collect();
    let mut out = String::with_capacity(asm.len());
    for (index, line) in lines.iter().enumerate() {
        if lines
            .get(index + 1)
            .is_some_and(|next| next.starts_with(".section \"generated_code_"))
        {
            let logical = line
                .strip_prefix(".bank ")
                .and_then(|tail| tail.strip_suffix(" slot 1"))
                .and_then(|number| number.parse::<u32>().ok())
                .ok_or_else(|| {
                    Error::Diagnostic("source code section lacks its explicit slot-one bank".into())
                })?;
            let physical = if logical < 24 {
                logical
            } else {
                code_after_data.checked_add(logical - 24).ok_or_else(|| {
                    Error::Diagnostic("source code bank allocation overflows".into())
                })?
            };
            if physical >= 256 {
                return Err(Error::Diagnostic(format!(
                    "source code physical bank {physical} exceeds the 256-bank Sega addressability limit; no output truncated"
                )));
            }
            out.push_str(&format!(".bank {physical} slot 1\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Remove `.org` directives inside sections; placement comes from the bank map.
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
    "rt_banked_tail_dispatch",
    "rt_rts_dispatch",
    "rt_translated_rts",
    "rt_translated_return_escape",
    "rt_translated_return_consume",
    "rt_mmc3_wait_boundary",
    "rt_translated_call_materialize",
    "rt_translated_call_gate",
    "rt_translated_tail_gate",
    "rt_far_tail",
    "rt_far_ncall",
    "rt_indirect_jmp",
    "rt_unresolved_jsr",
    "rt_unresolved_jsr_flash",
    "rt_brk",
    "rt_rti",
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
    "rt_read_prg_high",
    "rt_read_prg_high_indexed",
    "rt_write_indexed",
    "rt_read_zp_ptr_y",
    "rt_write_zp_ptr_y",
    "rt_far_jmp",
];

#[cfg(test)]
mod tests {
    #[test]
    fn source_code_remap_preserves_sections_and_rejects_physical_overflow() {
        let asm = ".bank 23 slot 1\n.section \"generated_code_22\" free\n.ends\n.bank 24 slot 1\n.section \"generated_code_23\" free\n  .db :L_9000\n.ends\n.bank (PROJECT_ROM_DATA_BANK_BASE + 0) slot 1\n.section \"rt_dispatch_table_sec\" free\n.ends\n.bank 0 slot 0\n.section \"rt_dispatch_directory_sec\" free\n.ends\n";
        let remapped = super::remap_source_code_banks(asm, 52).unwrap();
        assert_eq!(remapped, asm.replace(".bank 24 slot 1", ".bank 52 slot 1"));
        assert!(
            super::remap_source_code_banks(asm, 255)
                .unwrap()
                .contains(".bank 255 slot 1")
        );
        assert!(super::remap_source_code_banks(asm, 256).is_err());
        let far = asm.replace(".bank 24 slot 1", ".bank 228 slot 1");
        assert!(super::remap_source_code_banks(&far, 52).is_err());
    }
    #[test]
    fn mmc3_dispatch_counts_preserve_page_edges_and_oversized_fallback() {
        let mut records = Vec::new();
        for (page, count) in [
            (0x80u16, 1),
            (0x81, 3),
            (0xa8, 167),
            (0xfe, 255),
            (0xff, 256),
        ] {
            for index in 0..count {
                records.push((index as u8 % 32, (page << 8) | index as u16, String::new()));
            }
        }
        let original = records.clone();
        let counts = super::mmc3_dispatch_page_counts(&records).unwrap();
        assert_eq!(
            records, original,
            "counting must not reorder duplicate precedence"
        );
        for page in 0x80u16..=0xff {
            let expected = match page {
                0x80 => 1,
                0x81 => 3,
                0xa8 => 167,
                0xfe => 255,
                _ => 0,
            };
            assert_eq!(counts[usize::from(page - 0x80)], expected);
        }
        assert!(super::mmc3_dispatch_page_counts(&[(0, 0x7fff, String::new())]).is_err());
    }

    #[test]
    fn mmc3_dispatch_capacity_excludes_fixed_directories_but_includes_terminator() {
        for count in [2666, 2667, 2726, 2730] {
            let records = vec![(0, 0x8000, String::new()); count];
            assert!(super::mmc3_dispatch_page_counts(&records).is_ok());
        }
        let mut records = vec![(0, 0x8000, String::new()); 2730];
        assert!(super::mmc3_dispatch_page_counts(&records).is_ok());
        records.push((0, 0x8000, String::new()));
        assert!(
            super::mmc3_dispatch_page_counts(&records)
                .unwrap_err()
                .to_string()
                .contains("single 16 KiB slot: 16388 bytes")
        );
    }

    use super::*;

    #[test]
    fn parses_profile_jump_target_physical_identity() {
        assert_eq!(profile_target_identity("L_E3D7"), Some((None, 0xE3D7)));
        assert_eq!(
            profile_target_identity("L_b6_A75E"),
            Some((Some(6), 0xA75E))
        );
        assert_eq!(profile_target_identity("runtime_helper"), None);
    }

    #[test]
    fn consuming_entries_resolve_aliases_and_preserve_physical_bank_identity() {
        let mut profile = profile::load_from_str("[rom]\nname=\"x\"\nmapper=2\nprg_kib=128\nchr_kib=0\n[[return_escape]]\ncaller=0x8010\ntarget=0xC100\nreturn_addr=0xC200\nbank=3\nstack_bytes_already_consumed=true\nconsume_at=0x8000\n").unwrap();
        profile.functions.push(profile::Function {
            addr: 0xc123,
            name: "Friendly".into(),
            note: None,
        });
        profile.labels.push(profile::Label {
            addr: 0x8001,
            name: "Interior".into(),
        });
        assert_eq!(
            consume_target_identity(&profile, "Friendly"),
            Some((None, 0xc123))
        );
        assert_eq!(
            consume_target_identity(&profile, "Interior"),
            Some((None, 0x8001))
        );
        assert_eq!(
            consume_target_identity(&profile, "L_b4_8001"),
            Some((Some(4), 0x8001))
        );
        assert!(
            check_consume_entries(
                &profile,
                &[(Some(4), 0x8001), (Some(3), 0x8000), (Some(3), 0x8011)]
            )
            .is_ok()
        );
        for entry in [
            (Some(3), 0x8001),
            (Some(3), 0x8003),
            (Some(3), 0x8010),
            (None, 0x8001),
        ] {
            assert!(
                check_consume_entries(&profile, &[entry]).is_err(),
                "{entry:?}"
            );
        }
        profile.return_escapes[0].consume_at = Some(0xc000);
        profile.return_escapes[0].caller = 0xc010;
        profile.return_escapes[0].bank = None;
        assert!(
            check_consume_entries(&profile, &[(Some(4), 0xc001)]).is_err(),
            "fixed window is shared"
        );
    }

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

    #[test]
    fn only_structured_mapper_store_lower_errors_are_fatal() {
        assert!(lower_error_is_fatal(
            &lower::LowerError::UnsupportedMapperStore {
                pc: Some(0x8000),
                reason: "invalid mapper store".to_string(),
            }
        ));
        assert!(!lower_error_is_fatal(&lower::LowerError::UnsupportedOp {
            pc: Some(0x8000),
            reason: "unrelated lowering gap".to_string(),
        }));
    }

    #[test]
    fn banked_translated_sections_stop_before_prg_data_bank() {
        let last_section = sms_project::NES_PRG_BANK_BASE - TRANSLATED_BANK_BASE - 1;
        assert_eq!(
            u32::from(
                translated_section_bank(last_section, sms_project::NES_PRG_BANK_BASE).unwrap()
            ),
            sms_project::NES_PRG_BANK_BASE - 1
        );
        let err =
            translated_section_bank(last_section + 1, sms_project::NES_PRG_BANK_BASE).unwrap_err();
        assert!(
            err.to_string()
                .contains(&format!("section {} > {last_section}", last_section + 1))
        );
    }

    #[test]
    fn mmc3_analysis_establishes_only_one_eight_kib_window_and_fixed_vectors() {
        let mapping = TranslationMapping::Mmc3 { bank_count: 8 };
        let mut prg: Vec<u8> = (0..8).flat_map(|bank| vec![bank; 0x2000]).collect();
        prg[0xfffa..].copy_from_slice(&[0x10, 0xe0, 0x20, 0xe0, 0x30, 0xe0]);
        for window in [0x8000, 0xa000, 0xc000] {
            let view = mapping.analysis_view(&prg, 3, window).unwrap();
            for cpu_window in [0x8000, 0xa000, 0xc000] {
                assert_eq!(
                    view[usize::from(cpu_window - 0x8000)],
                    if window == cpu_window { 3 } else { 0 }
                );
            }
            assert_eq!(view[0x6000], 7);
            assert_eq!(mapping.window(window).start, window);
            assert_eq!(mapping.window(window).end_inclusive, window + 0x1fff);
        }
        let vectors = mapping.vectors(&prg).unwrap().unwrap();
        assert_eq!(
            (vectors.nmi, vectors.reset, vectors.irq),
            (0xe010, 0xe020, 0xe030)
        );
        assert!(mapping.analysis_view(&prg, 8, 0x8000).is_err());
        assert!(mapping.analysis_view(&prg, 0, 0xe000).is_err());
    }

    #[test]
    fn mmc3_code_packing_stops_at_continuation_abi_limit() {
        let last = sms_project::MMC3_CODE_BANK_LIMIT - TRANSLATED_BANK_BASE - 1;
        assert_eq!(
            u32::from(translated_section_bank(last, sms_project::MMC3_CODE_BANK_LIMIT).unwrap()),
            sms_project::MMC3_CODE_BANK_LIMIT - 1
        );
        assert!(translated_section_bank(last + 1, sms_project::MMC3_CODE_BANK_LIMIT).is_err());
    }

    #[test]
    fn isolated_mmc3_validation_reports_skip_not_legacy_bus_parity() {
        let mut prg = vec![0; 0x8000];
        prg[..3].copy_from_slice(&[0xa9, 0x42, 0x60]);
        let routine = ir::lift_range(
            &prg,
            &ir::LiftOptions {
                start: 0x8000,
                end: 0x8003,
                entry_name: "L_b0_8000".into(),
                ..ir::LiftOptions::default()
            },
        )
        .unwrap();
        let result = validate_translation_routine(
            TranslationMapping::Mmc3 { bank_count: 8 },
            &prg,
            &routine,
            8,
            false,
        );
        assert_eq!(result.routine_name, "L_b0_8000");
        assert_eq!(result.routine_entry, 0x8000);
        assert_eq!((result.vectors_run, result.vectors_passed), (0, 0));
        assert!(!result.is_green());
        assert!(result.failures.is_empty());
        assert!(
            result
                .skipped_reason
                .as_deref()
                .unwrap()
                .contains("assembled SMS runtime")
        );
        assert!(validation::format_report(&[result]).contains("skipped"));

        let legacy = validate_translation_routine(
            TranslationMapping::Legacy(nes_rom::MapperPolicy::Nrom { prg_len: prg.len() }),
            &prg,
            &routine,
            8,
            false,
        );
        assert!(legacy.is_green());
        assert_eq!(legacy.vectors_run, 8);
    }

    #[test]
    fn translated_section_identity_rejects_interposed_program_section() {
        let mut program = z80_emit::Program::new();
        program.section("unexpected_helper");

        let err = begin_translated_section(&mut program, 0, 256).unwrap_err();
        assert!(err.to_string().contains("logical section 0"));
        assert!(err.to_string().contains("expected 1"));
    }

    #[test]
    fn vector_aliases_use_tail_gates_for_cross_section_targets() {
        let (mut program, mut section) = packing_program();
        let target_program_section = program.current_section_idx() + 1;
        program.prepopulate_label_section(&std::collections::HashMap::from([
            ("L_C000".to_string(), target_program_section),
            ("L_C100".to_string(), target_program_section),
            ("L_C200".to_string(), target_program_section),
        ]));

        emit_translated_vector_aliases(&mut program, 0xC000, 0xC100, 0xC200);
        advance_translated_section(&mut program, &mut section, 256).unwrap();
        for label in ["L_C000", "L_C100", "L_C200"] {
            program.label(label);
            program.ret();
        }
        program.section("test_runtime_stubs");
        program.label("rt_translated_tail_gate");
        program.ret();

        let build = program.finish().unwrap();
        assert_eq!(build.asm.matches("jp rt_translated_tail_gate").count(), 3);
        for label in ["L_C000", "L_C100", "L_C200"] {
            assert!(!build.asm.contains(&format!("jp {label}")));
        }
    }

    fn packing_program() -> (z80_emit::Program, u32) {
        let mut program = z80_emit::Program::new();
        let section = 0;
        begin_translated_section(&mut program, section, 256).unwrap();
        (program, section)
    }

    #[test]
    fn packing_replays_crossing_candidate_in_next_section() {
        let (mut program, mut section) = packing_program();
        program.data(None, &vec![0xAA; 0x3FFF]);
        let mut state = Vec::<String>::new();
        let assigned = pack_sizing_candidate(
            &mut program,
            &mut state,
            &mut section,
            256,
            "crossing_marker",
            |candidate, state| {
                candidate.label("crossing_marker");
                candidate.data(None, &[0xC1, 0xC2]);
                state.push("committed".to_string());
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(assigned, 1);
        let build = program.finish().unwrap();
        let first = build
            .sections
            .iter()
            .find(|s| s.name == "generated_code_0")
            .unwrap();
        let second = build
            .sections
            .iter()
            .find(|s| s.name == "generated_code_1")
            .unwrap();
        assert_eq!(first.bytes.len(), 0x3FFF);
        assert_eq!(second.bytes, [0xC1, 0xC2]);
        assert_eq!(state, ["committed"]);
    }

    #[test]
    fn retry_relowers_section_sensitive_jsr_as_far() {
        let (mut program, mut section) = packing_program();
        let section_zero = program.current_section_idx();
        program.label("L_target");
        program.data(None, &vec![0xAA; 0x3FFF]);
        let attempts = std::cell::Cell::new(0);

        let routine = ir::Routine {
            entry: 0x8000,
            end: 0x8003,
            name: "L_8000".to_string(),
            ops: vec![
                ir::Op::Label("L_8000".to_string()),
                ir::Op::Jsr {
                    target: "L_target".to_string(),
                },
                ir::Op::Rts,
            ],
            branch_labels: vec!["L_8000".to_string()],
            external_calls: vec!["L_target".to_string()],
            unresolved: Vec::new(),
        };
        let mut state = ();
        let assigned = pack_sizing_candidate(
            &mut program,
            &mut state,
            &mut section,
            256,
            "L_8000",
            |candidate, _| {
                attempts.set(attempts.get() + 1);
                lower::lower_routine(candidate, &routine, &LowerOptions::default())
                    .map_err(Error::from)
            },
        )
        .unwrap();

        assert_eq!(assigned, 1);
        assert_eq!(section, 1);
        assert_eq!(attempts.get(), 2);
        assert_ne!(program.current_section_idx(), section_zero);
        assert_eq!(
            program.label_section_idx("L_8000"),
            Some(program.current_section_idx())
        );

        let unresolved = program.unresolved_labels();
        program.section("test_runtime_stubs");
        for label in unresolved {
            program.label(&label);
            program.ret();
        }
        let build = program.finish().unwrap();
        let first = build
            .sections
            .iter()
            .find(|s| s.name == "generated_code_0")
            .unwrap();
        let second = build
            .sections
            .iter()
            .find(|s| s.name == "generated_code_1")
            .unwrap();

        assert_eq!(first.bytes.len(), 0x3FFF);
        assert!(!second.bytes.is_empty());
        assert_eq!(build.asm.matches("L_8000:").count(), 1);
        assert!(build.asm.contains("jp rt_translated_call_gate"));
        assert!(!build.asm.contains("ld a,(hl)"));
    }

    #[test]
    fn packing_exact_full_final_section_does_not_advance() {
        let (mut program, mut section) = packing_program();
        let mut state = ();
        let assigned = pack_sizing_candidate(
            &mut program,
            &mut state,
            &mut section,
            256,
            "exact_full",
            |candidate, _| {
                candidate.data(None, &vec![0; 0x4000]);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(assigned, 0);
        assert_eq!(section, 0);
        assert_eq!(program.current_section_len(), 0x4000);
    }

    #[test]
    fn packing_empty_section_oversize_reports_exact_size() {
        let (mut program, mut section) = packing_program();
        let mut state = ();
        let err = pack_sizing_candidate(
            &mut program,
            &mut state,
            &mut section,
            256,
            "too_large",
            |candidate, _| {
                candidate.data(None, &vec![0; 0x4001]);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("too_large"));
        assert!(err.to_string().contains("16385 bytes"));
    }

    #[test]
    fn packing_uses_all_banks_before_reserved_uxrom_data() {
        let (mut program, mut section) = packing_program();
        let mut state = ();
        let section_count = sms_project::NES_PRG_BANK_BASE - TRANSLATED_BANK_BASE;
        for index in 0..section_count {
            let assigned = pack_sizing_candidate(
                &mut program,
                &mut state,
                &mut section,
                sms_project::NES_PRG_BANK_BASE,
                "full_section",
                |candidate, _| {
                    candidate.data(None, &vec![0; 0x4000]);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(assigned, index);
        }
        let err = pack_sizing_candidate(
            &mut program,
            &mut state,
            &mut section,
            sms_project::NES_PRG_BANK_BASE,
            "first_reserved_bank",
            |candidate, _| {
                candidate.data(None, &[0]);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains(&format!(
            "banks {}+ hold PRG data",
            sms_project::NES_PRG_BANK_BASE
        )));
    }

    #[test]
    fn nonfatal_lowering_rolls_back_to_only_trap_stub() {
        let (mut program, _) = packing_program();
        let routine = ir::Routine {
            entry: 0x8000,
            end: 0x8003,
            name: "profile_bad".to_string(),
            ops: vec![
                ir::Op::Label("profile_bad".to_string()),
                ir::Op::Label("L_inner".to_string()),
                ir::Op::Nop,
                ir::Op::Unsupported {
                    pc: 0x8002,
                    opcode: 0x8B,
                    mnemonic: "XAA".to_string(),
                    reason: "unstable opcode".to_string(),
                },
            ],
            branch_labels: vec!["profile_bad".to_string(), "L_inner".to_string()],
            external_calls: Vec::new(),
            unresolved: Vec::new(),
        };
        let mut labels = std::collections::BTreeSet::new();
        let mut failures = Vec::new();
        emit_translated_routine(
            &mut program,
            &mut labels,
            &mut failures,
            &routine,
            &LowerOptions::default(),
        )
        .unwrap();
        assert!(labels.contains("L_8000"));
        assert!(labels.contains("profile_bad"));
        assert!(labels.contains("L_inner"));
        program.label("rt_unresolved_jsr");
        program.ret();
        let build = program.finish().unwrap();
        assert!(failures.iter().any(|failure| failure.contains("XAA")));
        assert_eq!(build.asm.matches("L_8000:").count(), 1);
        assert_eq!(build.asm.matches("profile_bad:").count(), 1);
        assert_eq!(build.asm.matches("L_inner:").count(), 1);
        assert_eq!(build.asm.matches("ld a,$EE").count(), 1);
        assert!(build.asm.contains("ld ($CB1B),a"));
        assert!(!build.asm.contains("  nop"));
        assert_eq!(
            build
                .bytes
                .windows(5)
                .filter(|bytes| *bytes == [0x3E, 0xEE, 0x32, 0x1B, 0xCB])
                .count(),
            1
        );
        assert_eq!(&build.bytes[..5], [0x3E, 0xEE, 0x32, 0x1B, 0xCB]);
    }

    #[test]
    fn dense_dispatch_indices_preserve_original_page_fallbacks() {
        let mut records = Vec::new();
        for (page, count) in [
            (0x80u16, 1usize),
            (0x9f, 255),
            (0xa0, 256),
            (0xc0, 257),
            (0xfe, 3),
            (0xff, 1),
        ] {
            for i in 0..count {
                // Repeated lows preserve wildcard/concrete first-match order.
                records.push((
                    i as u8,
                    (page << 8) | (i * 251 / count) as u16,
                    String::new(),
                ));
            }
        }
        let original = records.clone();
        let counts = mmc3_dispatch_page_counts(&records).unwrap();
        let indices = mmc3_dispatch_pointer_indices(&records, &counts);
        assert_eq!(indices.len(), 32768);
        for target in 0x8000u16..=0xffff {
            let page = target >> 8;
            let start = records
                .iter()
                .position(|r| r.1 >> 8 >= page)
                .unwrap_or(records.len());
            let expected = if counts[usize::from(page - 0x80)] == 0 {
                start
            } else {
                records
                    .iter()
                    .enumerate()
                    .skip(start)
                    .find(|(_, r)| r.1 >= target)
                    .map_or(records.len(), |(i, _)| i)
            };
            assert_eq!(
                indices[usize::from(target - 0x8000)],
                expected,
                "PC{target:04X}"
            );
        }
        assert_eq!(records, original);
        assert_eq!(*indices.last().unwrap(), records.len(), "final terminator");
        assert_eq!(
            mmc3_dispatch_pointer_indices(&[], &[0; 128]),
            vec![0; 32768]
        );
    }
}
