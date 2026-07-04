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
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rom {
    pub name: String,
    pub mapper: u16,
    pub prg_kib: u32,
    pub chr_kib: u32,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Vectors {
    pub nmi: u16,
    pub reset: u16,
    pub irq: u16,
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
    /// Target labels indexed by the value of A at the JSR (A * 2 into
    /// the `.dd2` table).
    pub targets: Vec<String>,
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
        if let Some(prev_idx) = jump_engine_callers.insert(j.caller, idx) {
            return Err(LoadError::Validation(format!(
                "duplicate jump_engine caller ${:04X}: entries #{} and #{}",
                j.caller,
                prev_idx + 1,
                idx + 1
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
}
