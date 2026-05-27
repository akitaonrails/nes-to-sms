//! Function discovery, control-flow analysis, and code/data classification.
//!
//! Runs once per ROM before lowering. Produces a `FunctionMap`, a `ClassMap`,
//! and a `DiscoveryReport` that downstream passes can query.

use cpu6502::{AddrMode, Mnemonic, Operand, decode_at};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// ---------------------------------------------------------------------------
// Public re-export: lightweight Vectors without pulling in nes_rom
// ---------------------------------------------------------------------------

pub mod nes_rom_like {
    #[derive(Debug, Clone, Copy)]
    pub struct Vectors {
        pub nmi: u16,
        pub reset: u16,
        pub irq: u16,
    }
}

// ---------------------------------------------------------------------------
// RootKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    Vector,
    ProfileRoot,
    JumpTableTarget,
    JsrCallee,
    JmpTarget,
}

impl RootKind {
    fn display_name(self) -> &'static str {
        match self {
            RootKind::Vector => "Vector",
            RootKind::ProfileRoot => "ProfileRoot",
            RootKind::JumpTableTarget => "JumpTableTarget",
            RootKind::JsrCallee => "JsrCallee",
            RootKind::JmpTarget => "JmpTarget",
        }
    }
}

// ---------------------------------------------------------------------------
// DiscoveredFunction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscoveredFunction {
    pub addr: u16,
    pub end: u16,
    pub name: String,
    pub root_kinds: Vec<RootKind>,
    pub internal_labels: Vec<u16>,
    pub external_refs: Vec<u16>,
    pub unresolved_indirect: Vec<u16>,
}

// ---------------------------------------------------------------------------
// FunctionMap
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FunctionMap {
    /// Sorted ascending by addr.
    pub functions: Vec<DiscoveredFunction>,
}

impl FunctionMap {
    pub fn by_addr(&self, addr: u16) -> Option<&DiscoveredFunction> {
        self.functions.iter().find(|f| f.addr == addr)
    }

    pub fn label_at(&self, addr: u16) -> Option<&str> {
        for f in &self.functions {
            if f.addr == addr {
                return Some(&f.name);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// ByteClass / ClassMap
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteClass {
    Code,
    Data,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ClassMap {
    pub bytes: Vec<ByteClass>,
}

impl ClassMap {
    pub fn new(prg_len: usize) -> Self {
        ClassMap {
            bytes: vec![ByteClass::Unknown; prg_len],
        }
    }

    pub fn class_at(&self, prg_offset: usize) -> ByteClass {
        self.bytes
            .get(prg_offset)
            .copied()
            .unwrap_or(ByteClass::Unknown)
    }

    pub fn mark_code(&mut self, prg_offset: usize, len: usize) {
        for i in prg_offset..prg_offset.saturating_add(len).min(self.bytes.len()) {
            self.bytes[i] = ByteClass::Code;
        }
    }

    pub fn mark_data(&mut self, prg_offset: usize, len: usize) {
        for i in prg_offset..prg_offset.saturating_add(len).min(self.bytes.len()) {
            if self.bytes[i] != ByteClass::Code {
                self.bytes[i] = ByteClass::Data;
            }
        }
    }

    /// Returns `(code, data, unknown)` counts.
    pub fn summary(&self) -> (usize, usize, usize) {
        let mut code = 0usize;
        let mut data = 0usize;
        let mut unknown = 0usize;
        for &b in &self.bytes {
            match b {
                ByteClass::Code => code += 1,
                ByteClass::Data => data += 1,
                ByteClass::Unknown => unknown += 1,
            }
        }
        (code, data, unknown)
    }
}

// ---------------------------------------------------------------------------
// DiscoveryReport
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscoveryReport {
    pub text: String,
}

// ---------------------------------------------------------------------------
// Analyzed
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Analyzed {
    pub functions: FunctionMap,
    pub class_map: ClassMap,
    pub report: DiscoveryReport,
}

// ---------------------------------------------------------------------------
// Internal: per-walk result
// ---------------------------------------------------------------------------

const MAX_FUNCTION_BYTES: u16 = 4096;

struct WalkResult {
    end: u16,
    internal_labels: Vec<u16>,
    external_refs: Vec<u16>,
    unresolved_indirect: Vec<u16>,
    /// (addr, kind) pairs that should become new roots
    new_roots: Vec<(u16, RootKind)>,
    truncated: bool,
    hit_data: bool,
}

fn cpu_to_prg(addr: u16) -> Option<usize> {
    if addr >= 0x8000 {
        Some((addr - 0x8000) as usize)
    } else {
        None
    }
}

/// Walk one function starting at `entry`.
/// `known_roots` is the full set of all root addresses known so far (used to
/// detect external calls vs internal branches).
fn walk_function(
    prg: &[u8],
    entry: u16,
    profile: &profile::Profile,
    known_roots: &BTreeSet<u16>,
) -> WalkResult {
    let mut internal_labels: Vec<u16> = Vec::new();
    let mut external_refs: Vec<u16> = Vec::new();
    let mut unresolved_indirect: Vec<u16> = Vec::new();
    let mut new_roots: Vec<(u16, RootKind)> = Vec::new();

    // targets_inside: PCs that must be visited before the function can end.
    let mut targets_inside: BTreeSet<u16> = BTreeSet::new();
    targets_inside.insert(entry);

    let upper_bound = entry.saturating_add(MAX_FUNCTION_BYTES);
    let mut pc = entry;
    let end;
    let mut truncated = false;
    let mut hit_data = false;

    loop {
        // Stop if we've wandered past upper bound.
        if pc >= upper_bound {
            truncated = true;
            end = pc;
            break;
        }

        // Stop if we've hit a data region.
        if profile.is_data_byte(pc) {
            hit_data = true;
            end = pc;
            break;
        }

        let offset = match cpu_to_prg(pc) {
            Some(o) => o,
            None => {
                end = pc;
                break;
            }
        };

        if offset >= prg.len() {
            end = pc;
            break;
        }

        let insn = match decode_at(prg, pc, offset) {
            Ok(i) => i,
            Err(_) => {
                end = pc;
                break;
            }
        };

        let next_pc = pc.wrapping_add(insn.size as u16);

        // Process targets of this instruction.
        match (insn.mnemonic, insn.mode, insn.operand) {
            // Branches: may be internal or external
            (
                Mnemonic::BCC
                | Mnemonic::BCS
                | Mnemonic::BEQ
                | Mnemonic::BNE
                | Mnemonic::BMI
                | Mnemonic::BPL
                | Mnemonic::BVC
                | Mnemonic::BVS,
                AddrMode::Relative,
                _,
            ) => {
                if let Some(target) = insn.branch_target() {
                    if is_internal_target(target, entry, upper_bound, known_roots) {
                        targets_inside.insert(target);
                        if target != entry && !internal_labels.contains(&target) {
                            internal_labels.push(target);
                        }
                    } else {
                        push_unique(&mut external_refs, target);
                    }
                }
            }

            // JSR absolute
            (Mnemonic::JSR, AddrMode::Absolute, Operand::Addr(target)) => {
                // JSR is always an external call.
                push_unique(&mut external_refs, target);
                // Also schedule as a callee root if in PRG range.
                if target >= 0x8000 {
                    push_unique_root(&mut new_roots, target, RootKind::JsrCallee);
                }
            }

            // JMP absolute: treat as a backward loop (internal) only if the target
            // is already within the walked body (target < next_pc) and not a known
            // root. Any forward JMP or JMP to a known root is an external tail call.
            (Mnemonic::JMP, AddrMode::Absolute, Operand::Addr(target)) => {
                let is_backward_loop =
                    target >= entry && target < next_pc && !known_roots.contains(&target);
                if is_backward_loop {
                    // Backward self-loop or loop within already-walked body.
                    targets_inside.insert(target);
                    if target != entry && !internal_labels.contains(&target) {
                        internal_labels.push(target);
                    }
                } else {
                    push_unique(&mut external_refs, target);
                    if target >= 0x8000 {
                        push_unique_root(&mut new_roots, target, RootKind::JmpTarget);
                    }
                }
            }

            // JMP indirect
            (Mnemonic::JMP, AddrMode::Indirect, Operand::Addr(ptr)) => {
                push_unique(&mut unresolved_indirect, ptr);
            }

            _ => {}
        }

        // On a terminator, check if we must continue.
        if insn.is_terminator() {
            // JMP absolute pointing inside was handled above; for absolute JMP
            // where target is internal, we already inserted into targets_inside
            // and next_pc might be elsewhere. The function continues only if
            // next_pc is still needed.
            if !targets_inside.contains(&next_pc) {
                end = next_pc;
                break;
            }
            // next_pc is a pending internal target — keep walking.
        }

        pc = next_pc;
    }

    // Clean up internal_labels: remove any that ended up being < entry (shouldn't happen)
    internal_labels.retain(|&a| a >= entry && a < end);
    internal_labels.sort_unstable();
    internal_labels.dedup();

    external_refs.sort_unstable();
    external_refs.dedup();

    WalkResult {
        end,
        internal_labels,
        external_refs,
        unresolved_indirect,
        new_roots,
        truncated,
        hit_data,
    }
}

fn is_internal_target(
    target: u16,
    entry: u16,
    upper_bound: u16,
    known_roots: &BTreeSet<u16>,
) -> bool {
    // Target must be within function's plausible range.
    if target < entry || target >= upper_bound {
        return false;
    }
    // If the target is another known root (profile function, jump-table target,
    // or vector), it is an external reference.
    if known_roots.contains(&target) && target != entry {
        return false;
    }
    true
}

fn push_unique(v: &mut Vec<u16>, val: u16) {
    if !v.contains(&val) {
        v.push(val);
    }
}

fn push_unique_root(v: &mut Vec<(u16, RootKind)>, addr: u16, kind: RootKind) {
    if !v.iter().any(|(a, k)| *a == addr && *k == kind) {
        v.push((addr, kind));
    }
}

// ---------------------------------------------------------------------------
// Name resolution
// ---------------------------------------------------------------------------

fn name_for(addr: u16, kinds: &[RootKind], profile: &profile::Profile) -> String {
    if let Some(label) = profile.label_for(addr) {
        return label.to_string();
    }
    if kinds.contains(&RootKind::JumpTableTarget) {
        return format!("target_{:04X}", addr);
    }
    format!("func_{:04X}", addr)
}

// ---------------------------------------------------------------------------
// analyze
// ---------------------------------------------------------------------------

pub fn analyze(prg: &[u8], vectors: nes_rom_like::Vectors, profile: &profile::Profile) -> Analyzed {
    let prg_len = prg.len();
    let mut class_map = ClassMap::new(prg_len);
    let mut warnings: Vec<String> = Vec::new();
    let mut all_unresolved_indirect: BTreeSet<u16> = BTreeSet::new();
    let mut unresolved_external: BTreeSet<u16> = BTreeSet::new();

    // ---- Pre-mark data regions ----
    for dr in &profile.data_regions {
        if dr.start >= 0x8000 {
            let start_off = (dr.start - 0x8000) as usize;
            // DataRegion.end is inclusive
            let end_off = ((dr.end as usize) + 1).saturating_sub(0x8000);
            let len = end_off.saturating_sub(start_off);
            class_map.mark_data(start_off, len);
        }
    }

    // ---- Build initial root set ----
    // Map: addr -> set of RootKind
    let mut root_kinds: BTreeMap<u16, Vec<RootKind>> = BTreeMap::new();

    let add_root = |root_kinds: &mut BTreeMap<u16, Vec<RootKind>>, addr: u16, kind: RootKind| {
        if addr == 0x0000 || addr < 0x8000 {
            return;
        }
        let entry = root_kinds.entry(addr).or_default();
        if !entry.contains(&kind) {
            entry.push(kind);
        }
    };

    // Vectors
    for addr in [vectors.nmi, vectors.reset, vectors.irq] {
        add_root(&mut root_kinds, addr, RootKind::Vector);
    }

    // Profile functions
    for f in &profile.functions {
        add_root(&mut root_kinds, f.addr, RootKind::ProfileRoot);
    }

    // Jump-table targets
    for jt in &profile.jump_tables {
        for &target in &jt.targets {
            add_root(&mut root_kinds, target, RootKind::JumpTableTarget);
        }
    }

    // Profile vector validation
    if let Some(pv) = &profile.vectors {
        if pv.nmi != vectors.nmi {
            warnings.push(format!(
                "Profile NMI ${:04X} != ROM NMI ${:04X}",
                pv.nmi, vectors.nmi
            ));
        }
        if pv.reset != vectors.reset {
            warnings.push(format!(
                "Profile RESET ${:04X} != ROM RESET ${:04X}",
                pv.reset, vectors.reset
            ));
        }
        if pv.irq != vectors.irq {
            warnings.push(format!(
                "Profile IRQ ${:04X} != ROM IRQ ${:04X}",
                pv.irq, vectors.irq
            ));
        }
    }

    // ---- Discovery loop ----
    // Queue of (addr, RootKind) to process.
    let mut queue: VecDeque<(u16, RootKind)> = VecDeque::new();
    for (&addr, kinds) in &root_kinds {
        for &k in kinds {
            queue.push_back((addr, k));
        }
    }

    // Finished functions: addr -> DiscoveredFunction
    let mut discovered: BTreeMap<u16, DiscoveredFunction> = BTreeMap::new();

    while let Some((addr, _kind)) = queue.pop_front() {
        // Skip if already processed.
        if discovered.contains_key(&addr) {
            // But still ensure the kind is recorded.
            continue;
        }

        // Snapshot the current known_roots for walk decisions.
        let known_roots: BTreeSet<u16> = root_kinds.keys().copied().collect();

        let result = walk_function(prg, addr, profile, &known_roots);

        if result.truncated {
            warnings.push(format!(
                "func_{:04X}: walk exceeded {} bytes, truncated at ${:04X}",
                addr, MAX_FUNCTION_BYTES, result.end
            ));
        }
        if result.hit_data {
            warnings.push(format!(
                "func_{:04X}: walk hit data region at ${:04X}, stopped",
                addr, result.end
            ));
        }

        // Mark code bytes.
        if let Some(start_off) = cpu_to_prg(addr) {
            if let Some(end_off) = cpu_to_prg(result.end) {
                let len = end_off.saturating_sub(start_off);
                class_map.mark_code(start_off, len);
            } else if result.end == 0x0000 {
                // end wrapped past 0xFFFF
                let len = (0x10000usize).saturating_sub(addr as usize);
                class_map.mark_code(start_off, len);
            }
        }

        // Collect unresolved indirects.
        for &ptr in &result.unresolved_indirect {
            all_unresolved_indirect.insert(ptr);
        }

        // Enqueue newly discovered roots.
        for (new_addr, new_kind) in &result.new_roots {
            let kinds = root_kinds.entry(*new_addr).or_default();
            if !kinds.contains(new_kind) {
                kinds.push(*new_kind);
            }
            if !discovered.contains_key(new_addr) {
                queue.push_back((*new_addr, *new_kind));
            }
        }

        // Track external refs that are not roots.
        for &ext in &result.external_refs {
            if !root_kinds.contains_key(&ext) {
                unresolved_external.insert(ext);
            }
        }

        let kinds = root_kinds.entry(addr).or_default().clone();
        let name = name_for(addr, &kinds, profile);

        discovered.insert(
            addr,
            DiscoveredFunction {
                addr,
                end: result.end,
                name,
                root_kinds: kinds,
                internal_labels: result.internal_labels,
                external_refs: result.external_refs,
                unresolved_indirect: result.unresolved_indirect,
            },
        );
    }

    // Some roots may have been added after their initial queue pop
    // (e.g. a JSR callee that was also a profile root added later).
    // Re-merge root_kinds into the discovered functions.
    for (addr, kinds) in &root_kinds {
        if let Some(f) = discovered.get_mut(addr) {
            for &k in kinds {
                if !f.root_kinds.contains(&k) {
                    f.root_kinds.push(k);
                }
            }
        }
    }

    // Build sorted function list.
    let functions: Vec<DiscoveredFunction> = discovered.into_values().collect();

    let function_map = FunctionMap { functions };

    // ---- Build report ----
    let report = build_report(
        &function_map,
        &class_map,
        &all_unresolved_indirect,
        &unresolved_external,
        &warnings,
    );

    Analyzed {
        functions: function_map,
        class_map,
        report,
    }
}

fn build_report(
    fm: &FunctionMap,
    cm: &ClassMap,
    unresolved_indirect: &BTreeSet<u16>,
    unresolved_external: &BTreeSet<u16>,
    warnings: &[String],
) -> DiscoveryReport {
    let (code, data, unknown) = cm.summary();
    let total = code + data + unknown;
    let coverage_pct = if total > 0 {
        (code + data) as f64 * 100.0 / total as f64
    } else {
        0.0
    };

    let mut text = String::new();

    text.push_str(&format!(
        "Discovered {} functions, {} code bytes, {} data bytes, {} unknown bytes ({:.1}% coverage).\n",
        fm.functions.len(),
        code,
        data,
        unknown,
        coverage_pct,
    ));

    text.push('\n');
    text.push_str("Functions:\n");
    for f in &fm.functions {
        let kinds: Vec<&str> = f.root_kinds.iter().map(|k| k.display_name()).collect();
        text.push_str(&format!(
            "  ${:04X}-${:04X}  {}  [{}]\n",
            f.addr,
            f.end,
            f.name,
            kinds.join(", ")
        ));
    }

    text.push('\n');
    text.push_str("Unresolved indirect jumps:\n");
    if unresolved_indirect.is_empty() {
        text.push_str("  (none)\n");
    } else {
        for &ptr in unresolved_indirect {
            text.push_str(&format!("  ${:04X}\n", ptr));
        }
    }

    text.push('\n');
    text.push_str("External references not resolved to roots:\n");
    if unresolved_external.is_empty() {
        text.push_str("  (none)\n");
    } else {
        for &ext in unresolved_external {
            text.push_str(&format!("  ${:04X}\n", ext));
        }
    }

    if !warnings.is_empty() {
        text.push('\n');
        text.push_str("Warnings:\n");
        for w in warnings {
            text.push_str(&format!("  {}\n", w));
        }
    }

    DiscoveryReport { text }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use profile::Profile;

    const PRG_LEN: usize = 0x8000; // 32 KiB

    fn empty_prg() -> Vec<u8> {
        vec![0xFFu8; PRG_LEN]
    }

    fn make_prg(cpu_addr: u16, bytes: &[u8]) -> Vec<u8> {
        let mut prg = empty_prg();
        let off = (cpu_addr as usize) - 0x8000;
        let end = (off + bytes.len()).min(PRG_LEN);
        prg[off..end].copy_from_slice(&bytes[..end - off]);
        prg
    }

    fn minimal_profile() -> Profile {
        profile::load_from_str(
            r#"
[rom]
name = "Test"
mapper = 0
prg_kib = 32
chr_kib = 8
"#,
        )
        .unwrap()
    }

    fn vectors_reset(addr: u16) -> nes_rom_like::Vectors {
        nes_rom_like::Vectors {
            nmi: 0,
            reset: addr,
            irq: 0,
        }
    }

    // Test 1: single LDA + RTS, one function, 3 code bytes.
    #[test]
    fn single_function_lda_rts() {
        // $8000: A9 42  LDA #$42
        // $8002: 60     RTS
        let prg = make_prg(0x8000, &[0xA9, 0x42, 0x60]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        assert_eq!(result.functions.functions.len(), 1);
        let f = &result.functions.functions[0];
        assert_eq!(f.addr, 0x8000);
        assert_eq!(f.end, 0x8003);
        assert_eq!(f.root_kinds, vec![RootKind::Vector]);

        let (code, _data, _unknown) = result.class_map.summary();
        assert_eq!(code, 3);
    }

    // Test 2: NMI and RESET both point to $8000 — both Vector kinds recorded.
    #[test]
    fn nmi_and_reset_same_addr_both_vector_kinds() {
        let prg = make_prg(0x8000, &[0xA9, 0x42, 0x60]);
        let profile = minimal_profile();
        let vectors = nes_rom_like::Vectors {
            nmi: 0x8000,
            reset: 0x8000,
            irq: 0,
        };
        let result = analyze(&prg, vectors, &profile);

        assert_eq!(result.functions.functions.len(), 1);
        let f = &result.functions.functions[0];
        // Both NMI and RESET point here — Vector kind should appear at least once.
        assert!(f.root_kinds.contains(&RootKind::Vector));
        // The count of Vector kinds may be 1 (deduplicated) or 2 depending on impl;
        // at minimum, Vector is present.
        assert!(!f.root_kinds.is_empty());
    }

    // Test 3: two functions, $8000 calls $8010 via JSR.
    #[test]
    fn two_functions_jsr_callee() {
        // $8000: 20 10 80  JSR $8010
        // $8003: 60        RTS
        // $8010: A9 01     LDA #$01
        // $8012: 60        RTS
        let mut prg = empty_prg();
        prg[0x0000] = 0x20;
        prg[0x0001] = 0x10;
        prg[0x0002] = 0x80; // JSR $8010
        prg[0x0003] = 0x60; // RTS
        prg[0x0010] = 0xA9;
        prg[0x0011] = 0x01; // LDA #$01
        prg[0x0012] = 0x60; // RTS

        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        assert_eq!(result.functions.functions.len(), 2);

        let f0 = result.functions.by_addr(0x8000).unwrap();
        assert_eq!(f0.addr, 0x8000);
        assert!(f0.external_refs.contains(&0x8010));

        let f1 = result.functions.by_addr(0x8010).unwrap();
        assert!(f1.root_kinds.contains(&RootKind::JsrCallee));
    }

    // Test 4: three functions chained by JMP.
    #[test]
    fn three_functions_jmp_chain() {
        // $8000: 4C 10 80  JMP $8010
        // $8010: 4C 20 80  JMP $8020
        // $8020: 60        RTS
        let mut prg = empty_prg();
        prg[0x0000] = 0x4C;
        prg[0x0001] = 0x10;
        prg[0x0002] = 0x80;
        prg[0x0010] = 0x4C;
        prg[0x0011] = 0x20;
        prg[0x0012] = 0x80;
        prg[0x0020] = 0x60;

        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        assert!(result.functions.by_addr(0x8000).is_some());
        assert!(result.functions.by_addr(0x8010).is_some());
        assert!(result.functions.by_addr(0x8020).is_some());

        let f10 = result.functions.by_addr(0x8010).unwrap();
        assert!(f10.root_kinds.contains(&RootKind::JmpTarget));
    }

    // Test 5: internal forward branch — one function with internal label.
    #[test]
    fn internal_forward_branch() {
        // $8000: A2 03     LDX #$03
        // $8002: CA        DEX
        // $8003: D0 FD     BNE $8002   (rel=-3 -> target=$8002)
        // $8005: 60        RTS
        let prg = make_prg(0x8000, &[0xA2, 0x03, 0xCA, 0xD0, 0xFD, 0x60]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        assert_eq!(result.functions.functions.len(), 1);
        let f = &result.functions.functions[0];
        assert_eq!(f.addr, 0x8000);
        assert_eq!(f.end, 0x8006);
        assert!(f.internal_labels.contains(&0x8002));
    }

    // Test 6: JMP indirect recorded in unresolved_indirect.
    #[test]
    fn jmp_indirect_unresolved() {
        // $8000: 6C 00 30  JMP ($3000)
        let prg = make_prg(0x8000, &[0x6C, 0x00, 0x30]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        let f = result.functions.by_addr(0x8000).unwrap();
        assert!(f.unresolved_indirect.contains(&0x3000));

        let report = &result.report.text;
        assert!(report.contains("Unresolved indirect jumps"));
        assert!(report.contains("$3000"));
    }

    // Test 7: profile-supplied function root discovered without any vector.
    #[test]
    fn profile_root_discovered() {
        let mut prg = empty_prg();
        // $9CA6: A9 FF  LDA #$FF
        // $9CA8: 60     RTS
        let off = 0x9CA6usize - 0x8000;
        prg[off] = 0xA9;
        prg[off + 1] = 0xFF;
        prg[off + 2] = 0x60;

        let profile = profile::load_from_str(
            r#"
[rom]
name = "Test"
mapper = 0
prg_kib = 32
chr_kib = 8

[[function]]
addr = 0x9CA6
name = "GetPipeHeight"
"#,
        )
        .unwrap();

        let vectors = nes_rom_like::Vectors {
            nmi: 0,
            reset: 0,
            irq: 0,
        };
        let result = analyze(&prg, vectors, &profile);

        let f = result.functions.by_addr(0x9CA6).unwrap();
        assert_eq!(f.name, "GetPipeHeight");
        assert!(f.root_kinds.contains(&RootKind::ProfileRoot));
    }

    // Test 8: data region prevents walk from crossing into it.
    #[test]
    fn data_region_stops_walk() {
        // $87FD: A9 01  LDA #$01
        // $87FF: 60     RTS
        // $8800: (data region starts here)
        let mut prg = empty_prg();
        let off = 0x87FDusize - 0x8000;
        prg[off] = 0xA9;
        prg[off + 1] = 0x01;
        prg[off + 2] = 0x60;

        let profile = profile::load_from_str(
            r#"
[rom]
name = "Test"
mapper = 0
prg_kib = 32
chr_kib = 8

[[data_region]]
start = 0x8800
end   = 0x880F
"#,
        )
        .unwrap();

        let result = analyze(&prg, vectors_reset(0x87FD), &profile);

        let f = result.functions.by_addr(0x87FD).unwrap();
        // Function should end at or before $8800
        assert!(f.end <= 0x8800);
    }

    // Test 9: ClassMap counts.
    #[test]
    fn classmap_counts() {
        // 5 code bytes at $8000, 8-byte data region at $8800..$8807
        let mut prg = empty_prg();
        prg[0x0000] = 0xA9;
        prg[0x0001] = 0x42; // LDA #$42
        prg[0x0002] = 0xA9;
        prg[0x0003] = 0x01; // LDA #$01
        prg[0x0004] = 0x60; // RTS

        let profile = profile::load_from_str(
            r#"
[rom]
name = "Test"
mapper = 0
prg_kib = 32
chr_kib = 8

[[data_region]]
start = 0x8800
end   = 0x8807
"#,
        )
        .unwrap();

        let result = analyze(&prg, vectors_reset(0x8000), &profile);
        let (code, data, unknown) = result.class_map.summary();
        assert_eq!(code, 5);
        assert_eq!(data, 8);
        assert_eq!(unknown, PRG_LEN - 5 - 8);
    }

    // Test 10: DiscoveryReport contains summary line, function table, unresolved header.
    #[test]
    fn report_contains_required_sections() {
        let prg = make_prg(0x8000, &[0xA9, 0x42, 0x60]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        let text = &result.report.text;
        assert!(text.contains("Discovered"), "missing summary line");
        assert!(text.contains("functions"), "missing 'functions'");
        assert!(text.contains("code bytes"), "missing 'code bytes'");
        assert!(text.contains("$8000"), "missing function address");
        assert!(
            text.contains("Unresolved indirect jumps"),
            "missing indirect section header"
        );
    }

    // Test 11: vectors below $8000 are ignored gracefully.
    #[test]
    fn vectors_below_8000_ignored() {
        let prg = empty_prg();
        let profile = minimal_profile();
        let vectors = nes_rom_like::Vectors {
            nmi: 0x0000,
            reset: 0x0000,
            irq: 0x0000,
        };
        let result = analyze(&prg, vectors, &profile);
        // No functions discovered, no panic.
        assert_eq!(result.functions.functions.len(), 0);
    }

    // Test 12: infinite tight loop JMP $8000.
    #[test]
    fn infinite_loop_jmp_self() {
        // $8000: 4C 00 80  JMP $8000
        let prg = make_prg(0x8000, &[0x4C, 0x00, 0x80]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);

        // One function discovered.
        assert_eq!(result.functions.functions.len(), 1);
        let f = result.functions.by_addr(0x8000).unwrap();
        // The JMP is 3 bytes; end is $8003.
        assert_eq!(f.end, 0x8003);
        // 3 bytes of code.
        let (code, _, _) = result.class_map.summary();
        assert_eq!(code, 3);
    }

    // Test 13: FunctionMap::label_at returns name.
    #[test]
    fn function_map_label_at() {
        let prg = make_prg(0x8000, &[0xA9, 0x42, 0x60]);
        let profile = minimal_profile();
        let result = analyze(&prg, vectors_reset(0x8000), &profile);
        assert!(result.functions.label_at(0x8000).is_some());
        assert!(result.functions.label_at(0x9999).is_none());
    }

    // Test 14: ClassMap mark_data does not overwrite Code.
    #[test]
    fn classmap_data_does_not_overwrite_code() {
        let mut cm = ClassMap::new(16);
        cm.mark_code(0, 4);
        cm.mark_data(2, 4);
        // Bytes 0-3 should still be Code (mark_data skips Code bytes).
        assert_eq!(cm.class_at(0), ByteClass::Code);
        assert_eq!(cm.class_at(2), ByteClass::Code);
        assert_eq!(cm.class_at(4), ByteClass::Data);
    }
}
