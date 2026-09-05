//! Game profile schema and loader.
//!
//! A profile is a TOML file that supplies static-analysis facts the
//! pipeline cannot reliably infer from raw bytes: extra function roots,
//! labels, data regions, indirect-dispatch jump tables, runtime
//! replacements, and RAM region tags.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Profile {
    pub rom: Rom,
    #[serde(default)]
    pub vectors: Option<Vectors>,
    #[serde(default, rename = "function")]
    pub functions: Vec<Function>,
    #[serde(default, rename = "bank_entry")]
    pub bank_entries: Vec<BankEntry>,
    #[serde(default, rename = "bank_call")]
    pub bank_calls: Vec<BankCall>,
    #[serde(default, rename = "label")]
    pub labels: Vec<Label>,
    #[serde(default, rename = "data_region")]
    pub data_regions: Vec<DataRegion>,
    #[serde(default, rename = "jump_table")]
    pub jump_tables: Vec<JumpTable>,
    #[serde(default, rename = "replacement")]
    pub replacements: Vec<Replacement>,
    #[serde(default, rename = "ram_tag")]
    pub ram_tags: Vec<RamTag>,
    #[serde(default, rename = "chr_pack")]
    pub chr_packs: Vec<ChrPackRange>,
    /// `JSR JumpEngine`-style dispatch sites. Each entry maps a call
    /// site to the inline `.dd2` target table that follows it in the
    /// original NES PRG. The lifter substitutes the JSR with a direct
    /// jump-table dispatch on A so the lowered Z80 doesn't need to
    /// manipulate the emulated 6502 stack.
    #[serde(default, rename = "jump_engine")]
    pub jump_engines: Vec<JumpEngineSite>,
    /// Tail `JMP` sites that deliberately discard one caller's 6502 JSR
    /// return address as stack data before returning through the caller below
    /// it. Translated calls use a software continuation stack, so these edges
    /// need an explicit bridge back to the emulated 6502 stack.
    #[serde(default, rename = "return_escape")]
    pub return_escapes: Vec<ReturnEscapeSite>,
    /// Controller mapping policy. SMS pads have two buttons; NES has
    /// four. `heuristic` (default) keeps the SMB behavior: a
    /// title-mode RAM discriminator flips buttons between
    /// Select/Start and A/B. `action` maps button 1 -> NES A and
    /// button 2 -> NES B unconditionally; combined with
    /// `pause_start`, the SMS PAUSE button injects a NES Start press
    /// (CV1: title start + in-game pause).
    #[serde(default)]
    pub input: Input,
    #[serde(default)]
    pub render: Render,
    #[serde(default)]
    pub translation: Translation,
}

/// Code-generation policy assertions the profile author certifies for the
/// game (verified empirically by the differential oracle and the acceptance
/// routes, not provable from the instruction stream).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Translation {
    /// Measured far-transfer edge profile for the bank placer: a file of
    /// `CALLER TARGET COUNT` lines (hex NES addresses, from frame-diff's
    /// FD_FAR_EDGES), resolved relative to the profile's directory. The
    /// pipeline clusters routines by edge weight under a section-size
    /// estimate so hot cross-bank transfers become near CALLs.
    #[serde(default)]
    pub edge_profile: Option<String>,
    /// Profile-guided bank grouping: routines whose NES entry address is
    /// listed here are emitted first, in list order, so they pack into the
    /// same early section(s) and their mutual calls become near CALLs.
    /// Populate from frame-diff's FD_PROFILE far-transfer histogram.
    #[serde(default)]
    pub hot_group: Vec<u16>,
    /// Assembly-time defines forwarded to the runtime (e.g. a guard for a
    /// game-specific hooks file such as `SMB_RUNTIME_HOOKS`).
    #[serde(default)]
    pub runtime_defines: Vec<String>,
    /// Leave physical sprite base/size changes to a prepared SAT commit.
    /// The runtime must advertise RUNTIME_HAS_SPRITE_REGISTER_COMMIT; project
    /// assembly fails if that capability is absent. Guest PPU shadows and
    /// deferred reg1 intent still update normally. Default preserves legacy
    /// immediate register-6 writes.
    #[serde(default)]
    pub defer_sprite_registers: bool,
    /// JSR/RTS discipline. `software` (default) routes every translated
    /// call through the runtime continuation stack — correct for any 6502
    /// stack usage. `native` asserts strict LIFO JSR/RTS pairing (no code
    /// consumes JSR return bytes outside declared `[[jump_engine]]` sites)
    /// and lowers calls to native Z80 CALL/RET with a slot-0 far shim for
    /// cross-bank targets. Requires mapper 0 and no `[[return_escape]]`.
    #[serde(default)]
    pub stack_discipline: StackDiscipline,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StackDiscipline {
    #[default]
    Software,
    Native,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Render {
    /// Arm the sprite-0 line-IRQ scroll split (SMB status bar). Games
    /// that don't need it should disable it: line-IRQ pending/counter
    /// semantics vary across emulators (a GPGX line-IRQ storm starved
    /// the frame handler on CV1).
    #[serde(default = "default_true")]
    pub scroll_split: bool,
    /// Optional display-only tile remap for a fixed number of top rows. This
    /// is useful when coarse PPU timing leaves transition-fill tiles in a
    /// status/title band. Raw CIRAM remains untouched.
    #[serde(default)]
    pub top_tile_remap_rows: u8,
    #[serde(default)]
    pub top_tile_remap_from: Vec<u8>,
    #[serde(default)]
    pub top_tile_remap_to: u8,
    /// Keep CHR-RAM background tiles in identity slots and ignore NES
    /// sub-palette variants. This trades palette fidelity for stable dense
    /// screens that exceed the SMS runtime's 256-slot variant pool.
    #[serde(default)]
    pub chr_ram_bg_identity: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Render {
    fn default() -> Self {
        Render {
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Input {
    #[serde(default)]
    pub mode: InputMode,
    #[serde(default)]
    pub pause_start: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InputMode {
    #[default]
    Heuristic,
    Action,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rom {
    pub name: String,
    pub mapper: u16,
    pub prg_kib: u32,
    pub chr_kib: u32,
    #[serde(default)]
    pub payload_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Vectors {
    pub nmi: u16,
    pub reset: u16,
    pub irq: u16,
}

/// A code entry point inside a switchable PRG window (mapper plan M1):
/// `addr` is only meaningful with `bank` mapped at $8000-$BFFF.
#[derive(Debug, Clone, Deserialize)]
pub struct BankEntry {
    pub bank: u8,
    pub addr: u16,
}

/// Binds a fixed-bank call TARGET in the switchable window to the bank
/// the game always has mapped when calling it. Trap diagnostics
/// ($CB62 shadow at trap time) supply these.
#[derive(Debug, Clone, Deserialize)]
pub struct BankCall {
    pub target: u16,
    pub bank: u8,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Function {
    pub addr: u16,
    pub name: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Label {
    pub addr: u16,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DataRegion {
    pub start: u16,
    pub end: u16,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JumpTable {
    pub addr: u16,
    pub entries: u16,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub targets: Vec<u16>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Replacement {
    pub addr: u16,
    pub runtime_label: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// When true, the translated body itself is emitted as a
    /// `call runtime_label / ret` stub, so EVERY entry mechanism —
    /// including conditional branches from neighboring routines — reaches
    /// the hook. Must stay false for replacements whose hook delegates
    /// back into this routine's own translated body.
    #[serde(default)]
    pub stub_body: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RamTag {
    pub start: u16,
    pub end: u16,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JumpEngineSite {
    /// PC of the `JSR JumpEngine` instruction.
    pub caller: u16,
    /// Physical switchable PRG bank containing this call site. Omit for
    /// NROM or for a mapper-2 call in the fixed `$C000-$FFFF` window.
    #[serde(default)]
    pub bank: Option<u8>,
    /// Target labels indexed by the value of A at the JSR (A * 2 into
    /// the `.dd2` table).
    pub targets: Vec<String>,
    /// Optional translated continuation used by stack-aware dispatchers that
    /// arrange a return address before invoking JumpEngine. Ordinary inline
    /// tables omit this and retain tail-dispatch semantics.
    #[serde(default)]
    pub return_target: Option<String>,
    /// Target indices that consume/cancel the arranged return themselves and
    /// therefore must remain tail transfers even when `return_target` is set.
    #[serde(default)]
    pub tail_indices: Vec<usize>,
    /// Emulated 6502 stack bytes consumed when a returning target reaches the
    /// explicit continuation. CV1's dispatcher arranges a two-byte RTS value.
    #[serde(default)]
    pub stack_return_bytes: u8,
    /// Optional accumulator value at each target entry. Real pointer-table
    /// engines commonly leave A holding the selected target's high byte.
    #[serde(default)]
    pub target_entry_a: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReturnEscapeSite {
    /// PC of the absolute `JMP` that escapes the current translated call.
    pub caller: u16,
    /// Expected destination encoded by the `JMP`. Checked by the lifter so a
    /// stale profile fails closed instead of changing control flow silently.
    pub target: u16,
    /// Return PC as stored by the original 6502 JSR (the address of the final
    /// JSR operand byte, before the RTS increment).
    pub return_addr: u16,
    /// The original code has already consumed a materialized return with
    /// PLA/PLA. Transfer ownership at `consume_at`, while both return bytes
    /// are still live; the final JMP must not inspect freed stack storage.
    #[serde(default)]
    pub stack_bytes_already_consumed: bool,
    /// First PLA of a consecutive PLA/PLA pair in the same straight-line
    /// block as `caller`. Required for already-consumed mode, forbidden
    /// otherwise. No known entry may bypass it into the remaining block.
    #[serde(default)]
    pub consume_at: Option<u16>,
    /// Physical switchable PRG bank containing this edge. Omit for NROM or a
    /// mapper-2 edge in the fixed `$C000-$FFFF` window.
    #[serde(default)]
    pub bank: Option<u8>,
}

impl JumpEngineSite {
    /// Start of the inline little-endian target table that immediately
    /// follows the three-byte `JSR` instruction.
    pub fn table_start(&self) -> usize {
        usize::from(self.caller) + 3
    }

    pub fn table_len(&self) -> Option<usize> {
        self.targets.len().checked_mul(2)
    }

    pub fn table_end(&self) -> Option<usize> {
        self.table_start().checked_add(self.table_len()?)
    }

    pub fn contains_table_byte(&self, addr: u16) -> bool {
        let addr = usize::from(addr);
        self.table_end()
            .is_some_and(|end| addr >= self.table_start() && addr < end)
    }

    /// Some 6502 programs deliberately make a dispatch target begin inside
    /// the pointer table, reusing pointer bytes as opcodes and operands. Once
    /// such a target begins, the remaining table suffix is executable as well
    /// as address data and must not stop discovery.
    pub fn contains_executable_table_suffix_byte(&self, addr: u16) -> bool {
        let table_start = self.table_start();
        let table_end = match self.table_end() {
            Some(end) => end,
            None => return false,
        };
        let first_code = self
            .targets
            .iter()
            .chain(self.return_target.iter())
            .filter_map(|label| {
                let hex = label.rsplit('_').next()?;
                (hex.len() == 4)
                    .then(|| u16::from_str_radix(hex, 16).ok())
                    .flatten()
                    .map(usize::from)
            })
            .filter(|&target| target >= table_start && target < table_end)
            .min();
        first_code.is_some_and(|start| usize::from(addr) >= start && usize::from(addr) < table_end)
    }

    pub fn applies_to_bank(&self, bank: Option<u8>) -> bool {
        self.bank == bank
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct ChrPackRange {
    /// NES pattern table: 0 for PPU $0000, 1 for PPU $1000.
    pub table: u8,
    /// First NES tile in the table, inclusive.
    pub start: u8,
    /// Last NES tile in the table, inclusive.
    pub end: u8,
    /// First physical SMS tile slot. Valid range is 0..447.
    pub dest: u16,
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Parse(toml::de::Error),
    Validation(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "i/o error: {e}"),
            LoadError::Parse(e) => write!(f, "parse error: {e}"),
            LoadError::Validation(m) => write!(f, "validation error: {m}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

impl From<toml::de::Error> for LoadError {
    fn from(e: toml::de::Error) -> Self {
        LoadError::Parse(e)
    }
}

pub fn load_from_str(s: &str) -> Result<Profile, LoadError> {
    let p: Profile = toml::from_str(s)?;
    validate(&p)?;
    Ok(p)
}

pub fn load_from_path(path: impl AsRef<Path>) -> Result<Profile, LoadError> {
    let s = std::fs::read_to_string(path)?;
    load_from_str(&s)
}

fn validate(p: &Profile) -> Result<(), LoadError> {
    if p.translation
        .runtime_defines
        .iter()
        .any(|name| name == "CONSUMED_RETURN_ESCAPE")
    {
        return Err(LoadError::Validation(
            "CONSUMED_RETURN_ESCAPE is reserved: use return_escape.stack_bytes_already_consumed"
                .into(),
        ));
    }
    if p.translation.runtime_defines.iter().any(|name| {
        matches!(
            name.as_str(),
            "DEFER_SPRITE_REGISTERS" | "RUNTIME_HAS_SPRITE_REGISTER_COMMIT"
        )
    }) {
        return Err(LoadError::Validation(
            "sprite commit defines are reserved: use translation.defer_sprite_registers; the runtime must provide its capability".into(),
        ));
    }
    if p.render.top_tile_remap_rows > 28 {
        return Err(LoadError::Validation(
            "render.top_tile_remap_rows must be in 0..=28".into(),
        ));
    }
    if p.render.top_tile_remap_from.len() > 4 {
        return Err(LoadError::Validation(
            "render.top_tile_remap_from supports at most four tiles".into(),
        ));
    }
    if p.render.top_tile_remap_rows > 0 && p.render.top_tile_remap_from.is_empty() {
        return Err(LoadError::Validation(
            "render.top_tile_remap_from is required when top_tile_remap_rows is non-zero".into(),
        ));
    }
    if let Some(digest) = &p.rom.payload_sha256
        && (digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        return Err(LoadError::Validation(
            "rom.payload_sha256 must be exactly 64 lowercase hexadecimal characters".into(),
        ));
    }
    validate_bank_annotations(p)?;
    let mut names = BTreeMap::new();
    for f in &p.functions {
        if let Some(prev) = names.insert(f.addr, f.name.clone()) {
            return Err(LoadError::Validation(format!(
                "duplicate function at ${:04X}: {} vs {}",
                f.addr, prev, f.name
            )));
        }
    }
    for r in &p.data_regions {
        if r.end < r.start {
            return Err(LoadError::Validation(format!(
                "data region end < start: ${:04X}..${:04X}",
                r.start, r.end
            )));
        }
    }
    for j in &p.jump_tables {
        if !j.targets.is_empty() && j.targets.len() as u16 != j.entries {
            return Err(LoadError::Validation(format!(
                "jump table at ${:04X}: entries={} but targets={}",
                j.addr,
                j.entries,
                j.targets.len()
            )));
        }
    }
    let mut jump_engine_callers = BTreeMap::new();
    for (idx, j) in p.jump_engines.iter().enumerate() {
        if let Some(prev_idx) = jump_engine_callers.insert((j.bank, j.caller), idx) {
            return Err(LoadError::Validation(format!(
                "duplicate jump_engine caller ${:04X} in bank {:?}: entries #{} and #{}",
                j.caller,
                j.bank,
                prev_idx + 1,
                idx + 1
            )));
        }
        if j.targets.is_empty() {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} must declare at least one target",
                j.caller
            )));
        }
        if !j.target_entry_a.is_empty() && j.target_entry_a.len() != j.targets.len() {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} target_entry_a length {} does not match {} targets",
                j.caller,
                j.target_entry_a.len(),
                j.targets.len()
            )));
        }
        if j.return_target.is_none() && (!j.tail_indices.is_empty() || j.stack_return_bytes != 0) {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} tail_indices/stack_return_bytes require return_target",
                j.caller
            )));
        }
        if !matches!(j.stack_return_bytes, 0 | 2) {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} stack_return_bytes must be 0 or 2",
                j.caller
            )));
        }
        let mut tail_indices = std::collections::BTreeSet::new();
        for &target_idx in &j.tail_indices {
            if target_idx >= j.targets.len() {
                return Err(LoadError::Validation(format!(
                    "jump_engine caller ${:04X} tail index {} is out of range for {} targets",
                    j.caller,
                    target_idx,
                    j.targets.len()
                )));
            }
            if !tail_indices.insert(target_idx) {
                return Err(LoadError::Validation(format!(
                    "jump_engine caller ${:04X} repeats tail index {}",
                    j.caller, target_idx
                )));
            }
        }
        let Some(table_end) = j.table_end() else {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} target table size overflows",
                j.caller
            )));
        };
        if table_end > 0x10000 {
            return Err(LoadError::Validation(format!(
                "jump_engine caller ${:04X} target table extends past $FFFF",
                j.caller
            )));
        }
    }
    let mut return_escape_callers = BTreeMap::new();
    for (idx, escape) in p.return_escapes.iter().enumerate() {
        if escape.stack_bytes_already_consumed != escape.consume_at.is_some() {
            return Err(LoadError::Validation(
                "return_escape consume_at is required exactly when stack_bytes_already_consumed=true"
                    .to_owned(),
            ));
        }
        if let Some(start) = escape.consume_at {
            if start < 0x8000
                || start.checked_add(2).is_none_or(|end| end > escape.caller)
                || (start < 0xc000) != (escape.caller < 0xc000)
            {
                return Err(LoadError::Validation(
                    "return_escape consume_at must precede caller in the same PRG window"
                        .to_owned(),
                ));
            }
            if p.return_escapes[..idx].iter().any(|previous| {
                previous.bank == escape.bank
                    && previous
                        .consume_at
                        .is_some_and(|other| start <= previous.caller && other <= escape.caller)
            }) {
                return Err(LoadError::Validation(
                    "overlapping return_escape consume ranges".to_owned(),
                ));
            }
        }
        if let Some(prev_idx) = return_escape_callers.insert((escape.bank, escape.caller), idx) {
            return Err(LoadError::Validation(format!(
                "duplicate return_escape caller ${:04X} in bank {:?}: entries #{} and #{}",
                escape.caller,
                escape.bank,
                prev_idx + 1,
                idx + 1
            )));
        }
        if escape.caller < 0x8000 {
            return Err(LoadError::Validation(format!(
                "return_escape caller must be in PRG ROM, got ${:04X}",
                escape.caller
            )));
        }
        if escape.target < 0x8000 {
            return Err(LoadError::Validation(format!(
                "return_escape target must be in PRG ROM, got ${:04X}",
                escape.target
            )));
        }
        if escape.return_addr < 0x8000 {
            return Err(LoadError::Validation(format!(
                "return_escape return_addr must be in PRG ROM, got ${:04X}",
                escape.return_addr
            )));
        }
    }
    let mut used_chr_slots = [false; 448];
    for r in &p.chr_packs {
        if r.table > 1 {
            return Err(LoadError::Validation(format!(
                "chr_pack table must be 0 or 1, got {}",
                r.table
            )));
        }
        if r.start > r.end {
            return Err(LoadError::Validation(format!(
                "chr_pack start > end: table {} ${:02X}..${:02X}",
                r.table, r.start, r.end
            )));
        }
        let len = u16::from(r.end) - u16::from(r.start) + 1;
        if r.dest + len > 448 {
            return Err(LoadError::Validation(format!(
                "chr_pack destination range exceeds SMS tile slots: dest={} len={}",
                r.dest, len
            )));
        }
        for slot in r.dest..(r.dest + len) {
            let used = &mut used_chr_slots[usize::from(slot)];
            if *used {
                return Err(LoadError::Validation(format!(
                    "chr_pack destination slot {} overlaps another range",
                    slot
                )));
            }
            *used = true;
        }
    }
    Ok(())
}

fn validate_bank_annotations(p: &Profile) -> Result<(), LoadError> {
    if p.rom.mapper != 2 {
        if !p.bank_entries.is_empty()
            || !p.bank_calls.is_empty()
            || p.jump_engines.iter().any(|site| site.bank.is_some())
            || p.return_escapes.iter().any(|site| site.bank.is_some())
        {
            return Err(LoadError::Validation(format!(
                "bank-qualified annotations require mapper 2, got mapper {}",
                p.rom.mapper
            )));
        }
        return Ok(());
    }

    // Keep this layout policy aligned with nes_rom::resolve_mapper_policy:
    // supported UxROM payloads contain 2, 4, 8, or 16 physical 16-KiB banks.
    if p.rom.prg_kib == 0 || p.rom.prg_kib % 16 != 0 {
        return Err(LoadError::Validation(format!(
            "mapper 2 rom.prg_kib must be a positive multiple of 16 KiB, got {}",
            p.rom.prg_kib
        )));
    }
    let bank_count = p.rom.prg_kib / 16;
    if !matches!(bank_count, 2 | 4 | 8 | 16) {
        return Err(LoadError::Validation(format!(
            "mapper 2 rom.prg_kib must describe 2, 4, 8, or 16 16-KiB banks, got {} KiB ({bank_count} banks)",
            p.rom.prg_kib
        )));
    }

    let mut entries = BTreeMap::new();
    for entry in &p.bank_entries {
        validate_switchable_address("bank_entry.addr", entry.addr)?;
        if u32::from(entry.bank) >= bank_count {
            return Err(LoadError::Validation(format!(
                "bank_entry bank {} is out of range for {bank_count} mapper 2 banks",
                entry.bank
            )));
        }
        if entries.insert((entry.bank, entry.addr), ()).is_some() {
            return Err(LoadError::Validation(format!(
                "duplicate bank_entry for bank {} at ${:04X}",
                entry.bank, entry.addr
            )));
        }
    }

    let mut calls = BTreeMap::new();
    for call in &p.bank_calls {
        validate_switchable_address("bank_call.target", call.target)?;
        if u32::from(call.bank) >= bank_count {
            return Err(LoadError::Validation(format!(
                "bank_call bank {} is out of range for {bank_count} mapper 2 banks",
                call.bank
            )));
        }
        if let Some(previous_bank) = calls.insert(call.target, call.bank) {
            return Err(LoadError::Validation(format!(
                "duplicate bank_call target ${:04X}: bank {} conflicts with bank {}",
                call.target, previous_bank, call.bank
            )));
        }
    }
    for site in &p.jump_engines {
        if let Some(bank) = site.bank {
            validate_switchable_address("jump_engine.caller", site.caller)?;
            if u32::from(bank) >= bank_count {
                return Err(LoadError::Validation(format!(
                    "jump_engine bank {bank} is out of range for {bank_count} mapper 2 banks"
                )));
            }
        } else if site.caller < 0xC000 {
            return Err(LoadError::Validation(format!(
                "mapper 2 jump_engine caller ${:04X} in the switchable window requires bank",
                site.caller
            )));
        }
    }
    for site in &p.return_escapes {
        if let Some(bank) = site.bank {
            validate_switchable_address("return_escape.caller", site.caller)?;
            if u32::from(bank) >= bank_count {
                return Err(LoadError::Validation(format!(
                    "return_escape bank {bank} is out of range for {bank_count} mapper 2 banks"
                )));
            }
        } else if site.caller < 0xC000 {
            return Err(LoadError::Validation(format!(
                "mapper 2 return_escape caller ${:04X} in the switchable window requires bank",
                site.caller
            )));
        }
    }
    Ok(())
}

fn validate_switchable_address(field: &str, addr: u16) -> Result<(), LoadError> {
    if !(0x8000..0xC000).contains(&addr) {
        return Err(LoadError::Validation(format!(
            "{field} must be in switchable $8000-$BFFF, got ${addr:04X}"
        )));
    }
    Ok(())
}

impl Profile {
    /// All known function roots discovered via the profile, including
    /// jump-table targets. Vector-derived roots are added by the analyzer.
    pub fn function_roots(&self) -> Vec<u16> {
        let mut roots: Vec<u16> = self.functions.iter().map(|f| f.addr).collect();
        for j in &self.jump_tables {
            roots.extend_from_slice(&j.targets);
        }
        roots.sort_unstable();
        roots.dedup();
        roots
    }

    pub fn label_for(&self, addr: u16) -> Option<&str> {
        if let Some(f) = self.functions.iter().find(|f| f.addr == addr) {
            return Some(&f.name);
        }
        self.labels
            .iter()
            .find(|l| l.addr == addr)
            .map(|l| l.name.as_str())
    }

    pub fn is_data_byte(&self, addr: u16) -> bool {
        self.data_regions
            .iter()
            .any(|r| addr >= r.start && addr <= r.end)
            || self.jump_engines.iter().any(|site| {
                site.contains_table_byte(addr) && !site.contains_executable_table_suffix_byte(addr)
            })
    }

    pub fn is_data_byte_in_bank(&self, addr: u16, bank: Option<u8>) -> bool {
        self.data_regions
            .iter()
            .any(|r| addr >= r.start && addr <= r.end)
            || self.jump_engines.iter().any(|site| {
                site.applies_to_bank(bank)
                    && site.contains_table_byte(addr)
                    && !site.contains_executable_table_suffix_byte(addr)
            })
    }

    pub fn jump_engine_at(&self, caller: u16, bank: Option<u8>) -> Option<&JumpEngineSite> {
        self.jump_engines
            .iter()
            .find(|site| site.caller == caller && site.applies_to_bank(bank))
    }

    pub fn return_escape_at(&self, caller: u16, bank: Option<u8>) -> Option<&ReturnEscapeSite> {
        self.return_escapes
            .iter()
            .find(|site| site.caller == caller && site.bank == bank)
    }

    /// True when the profile certifies native Z80 CALL/RET lowering
    /// (see `Translation::stack_discipline`).
    pub fn native_calls(&self) -> bool {
        self.translation.stack_discipline == StackDiscipline::Native
    }

    /// Keep runtime assembly policy synchronized with inline hardware lowering.
    pub fn effective_runtime_defines(&self) -> Vec<String> {
        let mut defines = self.translation.runtime_defines.clone();
        if self.translation.defer_sprite_registers {
            defines.push("DEFER_SPRITE_REGISTERS".into());
        }
        if self
            .return_escapes
            .iter()
            .any(|site| site.stack_bytes_already_consumed)
        {
            defines.push("CONSUMED_RETURN_ESCAPE".into());
        }
        defines
    }

    pub fn replacement_for(&self, addr: u16) -> Option<&Replacement> {
        self.replacements.iter().find(|r| r.addr == addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[rom]
name   = "Test"
mapper = 0
prg_kib = 32
chr_kib = 8

[vectors]
nmi = 0x8082
reset = 0x8000
irq = 0xfff0

[[function]]
addr = 0x8000
name = "Start"

[[function]]
addr = 0x9ca6
name = "GetPipeHeight"

[[label]]
addr = 0xb1b4
name = "BranchStore"

[[data_region]]
start = 0xb000
end   = 0xb0ff
name  = "AreaData"

[[jump_table]]
addr = 0xb1b6
entries = 2
name = "GameMode"
targets = [0xb1d4, 0xb1dc]

[[replacement]]
addr = 0x8082
runtime_label = "rt_vblank"
reason = "SMS VDP frame"

[[ram_tag]]
start = 0x06a1
end   = 0x06a1
name  = "VRAM_Buffer1_Offset"

[[chr_pack]]
table = 1
start = 0x00
end = 0x0f
dest = 0x100
"#;

    #[test]
    fn parses_full_profile() {
        let p = load_from_str(SAMPLE).expect("load");
        assert_eq!(p.rom.mapper, 0);
        assert_eq!(p.vectors.unwrap().reset, 0x8000);
        assert_eq!(p.functions.len(), 2);
        assert_eq!(p.label_for(0x9ca6), Some("GetPipeHeight"));
        assert_eq!(p.label_for(0xb1b4), Some("BranchStore"));
        assert!(p.is_data_byte(0xb050));
        assert!(!p.is_data_byte(0xb100));
        assert_eq!(
            p.replacement_for(0x8082).unwrap().runtime_label,
            "rt_vblank"
        );
        assert_eq!(p.chr_packs.len(), 1);
        assert_eq!(p.chr_packs[0].table, 1);
        assert_eq!(p.chr_packs[0].dest, 0x100);
        let roots = p.function_roots();
        assert!(roots.contains(&0x8000));
        assert!(roots.contains(&0xb1d4));
    }

    #[test]
    fn deferred_sprite_policy_defaults_off_and_emits_runtime_contract() {
        let normal = load_from_str(SAMPLE).unwrap();
        assert!(!normal.translation.defer_sprite_registers);
        assert!(normal.effective_runtime_defines().is_empty());
        let deferred = load_from_str(&format!(
            "{SAMPLE}\n[translation]\ndefer_sprite_registers = true\nruntime_defines = [\"TEST_BACKEND\"]\n"
        )).unwrap();
        assert_eq!(
            deferred.effective_runtime_defines(),
            ["TEST_BACKEND", "DEFER_SPRITE_REGISTERS"]
        );
        for reserved in [
            "DEFER_SPRITE_REGISTERS",
            "RUNTIME_HAS_SPRITE_REGISTER_COMMIT",
        ] {
            assert!(
                load_from_str(&format!(
                    "{SAMPLE}\n[translation]\nruntime_defines = [\"{reserved}\"]\n"
                ))
                .is_err(),
                "must not forge capability or desynchronize lowering"
            );
        }
    }

    #[test]
    fn validates_top_tile_remap_policy() {
        let profile = load_from_str(
            r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[render]
top_tile_remap_rows = 6
top_tile_remap_from = [0x37, 0x38]
top_tile_remap_to = 0
"#,
        )
        .unwrap();
        assert_eq!(profile.render.top_tile_remap_rows, 6);
        assert_eq!(profile.render.top_tile_remap_from, [0x37, 0x38]);

        let invalid = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[render]
top_tile_remap_rows = 1
"#;
        assert!(matches!(
            load_from_str(invalid),
            Err(LoadError::Validation(_))
        ));
    }

    #[test]
    fn rejects_duplicate_function() {
        let s = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[function]]
addr = 0x8000
name = "A"
[[function]]
addr = 0x8000
name = "B"
"#;
        assert!(matches!(load_from_str(s), Err(LoadError::Validation(_))));
    }

    #[test]
    fn accepts_optional_payload_sha256() {
        let profile = load_from_str(
            r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8
payload_sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
"#,
        )
        .unwrap();
        assert_eq!(
            profile.rom.payload_sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert!(
            load_from_str(
                r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8
"#
            )
            .unwrap()
            .rom
            .payload_sha256
            .is_none()
        );
    }

    #[test]
    fn rejects_invalid_payload_sha256() {
        for digest in [
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015a",
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD",
            "zz7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ] {
            let source = format!(
                "[rom]\nname = \"x\"\nmapper = 0\nprg_kib = 32\nchr_kib = 8\npayload_sha256 = \"{digest}\""
            );
            assert!(matches!(
                load_from_str(&source),
                Err(LoadError::Validation(_))
            ));
        }
    }

    #[test]
    fn rejects_inverted_data_region() {
        let s = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[data_region]]
start = 0x9000
end   = 0x8000
"#;
        assert!(matches!(load_from_str(s), Err(LoadError::Validation(_))));
    }

    #[test]
    fn rejects_duplicate_jump_engine_caller() {
        let s = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[jump_engine]]
caller = 0x8000
targets = ["A"]

[[jump_engine]]
caller = 0x8000
targets = ["B"]
"#;
        let err = load_from_str(s).expect_err("duplicate caller should fail");
        assert!(matches!(err, LoadError::Validation(_)));
        assert!(
            err.to_string()
                .contains("duplicate jump_engine caller $8000")
        );
    }

    #[test]
    fn rejects_empty_or_overflowing_jump_engine_table() {
        let empty = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[jump_engine]]
caller = 0x8000
targets = []
"#;
        let err = load_from_str(empty).expect_err("empty table should fail");
        assert!(err.to_string().contains("at least one target"));

        let overflow = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[jump_engine]]
caller = 0xFFFC
targets = ["A"]
"#;
        let err = load_from_str(overflow).expect_err("overflowing table should fail");
        assert!(err.to_string().contains("extends past $FFFF"));
    }

    #[test]
    fn validates_stack_aware_jump_engine_metadata() {
        let valid = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[jump_engine]]
caller = 0x9000
targets = ["L_A000", "L_A100"]
return_target = "L_9007"
tail_indices = [1]
stack_return_bytes = 2
target_entry_a = [0xA0, 0xA1]
"#;
        let profile = load_from_str(valid).expect("valid stack-aware metadata");
        assert_eq!(profile.jump_engines[0].tail_indices, vec![1]);
        assert_eq!(profile.jump_engines[0].stack_return_bytes, 2);

        for (fragment, diagnostic) in [
            ("tail_indices = [2]", "tail index 2 is out of range"),
            ("stack_return_bytes = 1", "must be 0 or 2"),
            ("target_entry_a = [0xA0]", "length 1 does not match 2"),
        ] {
            let invalid = valid
                .replace("tail_indices = [1]", "")
                .replace("stack_return_bytes = 2", "")
                .replace("target_entry_a = [0xA0, 0xA1]", fragment);
            let err = load_from_str(&invalid).expect_err("invalid metadata must fail");
            assert!(err.to_string().contains(diagnostic), "{err}");
        }
    }

    #[test]
    fn mapper_2_requires_bank_for_window_jump_engine() {
        let missing_bank = r#"
[rom]
name = "x"
mapper = 2
prg_kib = 128
chr_kib = 0

[[jump_engine]]
caller = 0x9000
targets = ["A"]
"#;
        let err = load_from_str(missing_bank).expect_err("window site needs bank");
        assert!(err.to_string().contains("requires bank"));

        let qualified = r#"
[rom]
name = "x"
mapper = 2
prg_kib = 128
chr_kib = 0

[[jump_engine]]
caller = 0x9000
bank = 3
targets = ["A"]

[[jump_engine]]
caller = 0x9000
bank = 4
targets = ["B"]
"#;
        let profile = load_from_str(qualified).expect("same PC in distinct banks is valid");
        assert_eq!(profile.jump_engines.len(), 2);
    }

    #[test]
    fn rejects_invalid_mapper_2_bank_annotations() {
        let base = "[rom]\nname = \"x\"\nmapper = 2\nprg_kib = 32\nchr_kib = 0\n";
        for (annotation, diagnostic) in [
            (
                "[[bank_entry]]\nbank = 2\naddr = 0x8000",
                "bank_entry bank 2 is out of range",
            ),
            (
                "[[bank_entry]]\nbank = 0\naddr = 0xc000",
                "bank_entry.addr must be in switchable",
            ),
            (
                "[[bank_call]]\nbank = 2\ntarget = 0x8000",
                "bank_call bank 2 is out of range",
            ),
            (
                "[[bank_call]]\nbank = 0\ntarget = 0xc000",
                "bank_call.target must be in switchable",
            ),
        ] {
            let err =
                load_from_str(&format!("{base}\n{annotation}")).expect_err("invalid annotation");
            assert!(err.to_string().contains(diagnostic), "{err}");
        }
    }

    #[test]
    fn rejects_unsupported_mapper_2_prg_layout() {
        let err = load_from_str("[rom]\nname = \"x\"\nmapper = 2\nprg_kib = 48\nchr_kib = 0")
            .expect_err("unsupported mapper 2 layout");
        assert!(
            err.to_string()
                .contains("must describe 2, 4, 8, or 16 16-KiB banks")
        );
    }

    #[test]
    fn rejects_duplicate_bank_entries_and_calls() {
        let base = "[rom]\nname = \"x\"\nmapper = 2\nprg_kib = 32\nchr_kib = 0\n";
        let duplicate_entry = format!(
            "{base}\n[[bank_entry]]\nbank = 0\naddr = 0x8000\n[[bank_entry]]\nbank = 0\naddr = 0x8000"
        );
        assert!(
            load_from_str(&duplicate_entry)
                .expect_err("duplicate entry")
                .to_string()
                .contains("duplicate bank_entry for bank 0 at $8000")
        );
        for banks in [(0, 0), (0, 1)] {
            let duplicate_call = format!(
                "{base}\n[[bank_call]]\nbank = {}\ntarget = 0x8000\n[[bank_call]]\nbank = {}\ntarget = 0x8000",
                banks.0, banks.1
            );
            assert!(
                load_from_str(&duplicate_call)
                    .expect_err("duplicate call")
                    .to_string()
                    .contains("duplicate bank_call target $8000")
            );
        }
    }

    #[test]
    fn rejects_bank_annotations_for_non_mapper_2_profiles() {
        let err = load_from_str(
            "[rom]\nname = \"x\"\nmapper = 0\nprg_kib = 32\nchr_kib = 8\n\n[[bank_entry]]\nbank = 0\naddr = 0x8000",
        )
        .expect_err("non-mapper bank annotation");
        assert!(err.to_string().contains("require mapper 2, got mapper 0"));
    }

    #[test]
    fn validates_return_escape_metadata_and_bank_identity() {
        let fixed = r#"
[rom]
name = "x"
mapper = 2
prg_kib = 128
chr_kib = 0

[[return_escape]]
caller = 0xe7d0
target = 0xec60
return_addr = 0xea79
"#;
        let profile = load_from_str(fixed).expect("fixed-window escape");
        let escape = profile
            .return_escape_at(0xE7D0, None)
            .expect("qualified lookup");
        assert_eq!(escape.target, 0xEC60);
        assert_eq!(escape.return_addr, 0xEA79);
        assert!(!escape.stack_bytes_already_consumed);
        assert!(
            !profile
                .effective_runtime_defines()
                .contains(&"CONSUMED_RETURN_ESCAPE".into())
        );
        let consumed = load_from_str(&format!(
            "{fixed}\nstack_bytes_already_consumed = true\nconsume_at = 0xE7C0\n"
        ))
        .expect("already-consumed escape");
        assert!(consumed.return_escapes[0].stack_bytes_already_consumed);
        assert_eq!(
            consumed.effective_runtime_defines(),
            ["CONSUMED_RETURN_ESCAPE"]
        );

        let window = fixed.replace("caller = 0xe7d0", "caller = 0x87d0");
        let err = load_from_str(&window).expect_err("window escape requires bank");
        assert!(err.to_string().contains("return_escape caller $87D0"));
        assert!(err.to_string().contains("requires bank"));

        let qualified = window.replace("caller = 0x87d0", "caller = 0x87d0\nbank = 3");
        load_from_str(&qualified).expect("bank-qualified window escape");
    }

    #[test]
    fn rejects_duplicate_or_non_prg_return_escape_metadata() {
        let base = r#"
[rom]
name = "x"
mapper = 0
prg_kib = 32
chr_kib = 8

[[return_escape]]
caller = 0x9000
target = 0xa000
return_addr = 0x8fff
"#;
        let duplicate = format!(
            "{base}\n[[return_escape]]\ncaller = 0x9000\ntarget = 0xa100\nreturn_addr = 0x8fff"
        );
        assert!(
            load_from_str(&duplicate)
                .expect_err("duplicate escape")
                .to_string()
                .contains("duplicate return_escape caller $9000")
        );

        let bad = base.replace("return_addr = 0x8fff", "return_addr = 0x7fff");
        assert!(
            load_from_str(&bad)
                .expect_err("RAM return address")
                .to_string()
                .contains("return_addr must be in PRG ROM")
        );
    }

    #[test]
    fn consumed_escape_metadata_is_required_banked_and_nonoverlapping() {
        let base = "[rom]\nname=\"x\"\nmapper=2\nprg_kib=128\nchr_kib=0\n\n[[return_escape]]\ncaller=0xC010\ntarget=0xC020\nreturn_addr=0xC100\n";
        for suffix in [
            "stack_bytes_already_consumed=true",
            "consume_at=0xC000",
            "stack_bytes_already_consumed=true\nconsume_at=0xBFFE",
            "stack_bytes_already_consumed=true\nconsume_at=0xC00F",
            "stack_bytes_already_consumed=true\nconsume_at=0x7FFF",
        ] {
            assert!(
                load_from_str(&format!("{base}{suffix}")).is_err(),
                "{suffix}"
            );
        }
        let valid = format!("{base}stack_bytes_already_consumed=true\nconsume_at=0xC000\n");
        load_from_str(&valid).unwrap();
        let overlap = format!(
            "{valid}\n[[return_escape]]\ncaller=0xC030\ntarget=0xC100\nreturn_addr=0xC100\nstack_bytes_already_consumed=true\nconsume_at=0xC010\n"
        );
        assert!(
            load_from_str(&overlap)
                .unwrap_err()
                .to_string()
                .contains("overlapping")
        );
        let banked = valid
            .replace("0xC", "0x8")
            .replace("caller=", "bank=3\ncaller=");
        assert_eq!(
            load_from_str(&banked).unwrap().return_escapes[0].bank,
            Some(3)
        );
    }
}
