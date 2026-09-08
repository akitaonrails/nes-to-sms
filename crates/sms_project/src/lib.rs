//! Emit a buildable WLA-DX SMS project tree from a [`z80_emit::Build`] plus
//! optional asset bundles.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProjectAssets {
    /// Raw SMS 4bpp tile data (32 bytes/tile).
    pub chr_4bpp: Vec<u8>,
    /// 32-byte SMS CRAM palette image.
    pub palette: [u8; 32],
    /// Optional 1792-byte SMS name-table image (32 cols x 28 rows x 2 bytes).
    pub nametable: Option<Vec<u8>>,
    /// Optional lower NES PRG window ($8000-$BFFF) mirrored into SMS slot 2
    /// so translated code can read profiled PRG data tables directly.
    pub prg_low: Option<Vec<u8>>,
    /// Banked raw PRG in physical order, one 16 KiB SMS data bank per entry.
    /// UxROM entries are NES banks; MMC3 entries pack two adjacent 8 KiB pages.
    pub prg_banks: Option<Vec<Vec<u8>>>,
    /// Optional upper/fixed NES PRG window ($C000-$FFFF), or final MMC3 page
    /// pair. MMC3 reads use physical-page helpers, not the legacy fixed window.
    pub prg_high: Option<Vec<u8>>,
    /// Optional raw NES CHR bytes for emulated PPU $2007 pattern-table reads.
    pub chr_nes: Option<Vec<u8>>,
    /// Optional 0x600-byte CHR remap data for runtime tile lookup.
    pub chr_maps: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NesMirroring {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawCiramBackend {
    None,
    /// Standard Sega mapper SRAM bank 0 mapped into slot 2 ($8000-$BFFF)
    /// with mapper control $FFFC bit 3 set. The first 2 KiB ($8000-$87FF)
    /// are reserved for mirrored NES CIRAM.
    SramSlot2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UxromBusConflicts {
    None,
    And,
}

// Compact mapper layout: translated code 4-20, NES PRG data banks 21-28
// (8 x 16 KiB = 128 KiB carts like CV1), converted CHR in bank 29, and
// small runtime assets packed into bank 30. Bank 31 remains spare. Keeping
// the image at 512 KiB matters: 1 MiB Sega-mapper support is spotty in both
// GPGX and Mednafen.
pub const NES_PRG_BANK_BASE: u32 = 21;

/// Experimental raw PRG layout: two physical 8 KiB pages per SMS bank.
pub const MMC3_PRG_DATA_BASE: u32 = 96;

/// Exclusive translated-bank limit until continuation frames separate bank
/// identity from flags. Materialized returns currently retain only five bits.
pub const MMC3_CODE_BANK_LIMIT: u32 = 32;

const PACKED_PALETTE_OFFSET: u32 = 0x0000;
const PACKED_NAMETABLE_OFFSET: u32 = 0x0020;
const PACKED_CHR_NES_OFFSET: u32 = 0x0720;
const PACKED_CHR_MAPS_OFFSET: u32 = 0x2720;

#[derive(Debug, Clone)]
pub struct ProjectConfig<'a> {
    /// ROM size in KB. Must be a multiple of 16 and >= 16.
    pub rom_kib: u32,
    /// Cartridge region byte (e.g. $4C for Export 32K).
    pub region: u8,
    /// Title that will go in the ROM header (max 11 ASCII bytes).
    pub title: &'a str,
    /// NES header nametable mirroring mode.
    pub mirroring: NesMirroring,
    /// Storage backend for raw NES CIRAM source-of-truth.
    pub raw_ciram_backend: RawCiramBackend,
    /// NES mapper number (drives runtime .ifdef paths).
    pub mapper: u16,
    /// Mapper 2 bank count, validated against the emitted PRG bank assets.
    pub uxrom_bank_count: Option<u8>,
    /// Mapper 2 bus-conflict mode.
    pub uxrom_bus_conflicts: Option<UxromBusConflicts>,
    /// Mapper 4 physical 8 KiB PRG page count (assets contain 16 KiB pairs).
    pub mmc3_prg_bank_count: Option<u8>,
    /// CHR-RAM cart: patterns upload at runtime; variant regeneration
    /// reads back from VRAM instead of the (blank) data_chr asset.
    pub chr_ram: bool,
    /// Controller mapping: false = SMB title-mode heuristic (default),
    /// true = fixed button1->NES A, button2->NES B.
    pub input_action: bool,
    /// SMS PAUSE button injects a NES Start press (see input.s/boot.s).
    pub input_pause_start: bool,
    /// Arm the sprite-0 line-IRQ scroll split (see boot.s).
    pub scroll_split: bool,
    /// Display-only remap applied while materializing the declared top rows.
    pub top_tile_remap_rows: u8,
    pub top_tile_remap_from: Vec<u8>,
    pub top_tile_remap_to: u8,
    pub chr_ram_bg_identity: bool,
    /// Native CALL/RET stack discipline (profile `stack_discipline =
    /// "native"`): emits `.define NATIVE_CALLS 1` for the runtime.
    pub native_calls: bool,
    /// Profile-driven assembly defines (each emitted as `.define X 1`).
    pub runtime_defines: Vec<String>,
}

#[derive(Debug)]
pub enum EmitError {
    Io(io::Error),
    InvalidTitle(String),
    InvalidRomSize(u32),
    InvalidUxromConfig(String),
    InvalidMmc3Config(String),
    ReservedBankPlacement { bank: u32, reserved_bank: u32 },
    LayoutExceedsRomCapacity { required_bank: u32, bank_count: u32 },
}

impl fmt::Display for EmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmitError::Io(e) => write!(f, "I/O error: {e}"),
            EmitError::InvalidTitle(t) => {
                write!(f, "title must be ≤ 11 ASCII bytes, got: {t:?}")
            }
            EmitError::InvalidRomSize(n) => {
                write!(f, "rom_kib must be a multiple of 16 and >= 16, got: {n}")
            }
            EmitError::InvalidUxromConfig(reason) => {
                write!(f, "invalid UxROM configuration: {reason}")
            }
            EmitError::InvalidMmc3Config(reason) => {
                write!(f, "invalid MMC3 configuration: {reason}")
            }
            EmitError::ReservedBankPlacement {
                bank,
                reserved_bank,
            } => write!(
                f,
                "build.asm places code in bank {bank}, which collides with reserved data banks starting at {reserved_bank}"
            ),
            EmitError::LayoutExceedsRomCapacity {
                required_bank,
                bank_count,
            } => write!(
                f,
                "project layout requires ROM bank {required_bank}, but ROM capacity ends at bank {}",
                bank_count.saturating_sub(1)
            ),
        }
    }
}

impl std::error::Error for EmitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EmitError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for EmitError {
    fn from(e: io::Error) -> Self {
        EmitError::Io(e)
    }
}

// ── Validation ────────────────────────────────────────────────────────────────

fn validate_config(
    cfg: &ProjectConfig<'_>,
    assets: &ProjectAssets,
    build: &z80_emit::Build,
) -> Result<(), EmitError> {
    let mmc3_full = cfg
        .runtime_defines
        .iter()
        .any(|name| name == "MMC3_FULL_RUNTIME");
    if mmc3_full && cfg.mapper != 4 {
        return Err(EmitError::InvalidMmc3Config(
            "full MMC3 runtime requires mapper 4".into(),
        ));
    }
    if !cfg.title.is_ascii() || cfg.title.len() > 11 {
        return Err(EmitError::InvalidTitle(cfg.title.to_string()));
    }
    if cfg.rom_kib < 16 || cfg.rom_kib % 16 != 0 {
        return Err(EmitError::InvalidRomSize(cfg.rom_kib));
    }
    if assets.prg_banks.is_some() && !matches!(cfg.mapper, 2 | 4) {
        return Err(EmitError::InvalidUxromConfig(
            "PRG bank assets require mapper 2".into(),
        ));
    }
    if cfg.mapper == 2 {
        let count = cfg
            .uxrom_bank_count
            .ok_or_else(|| EmitError::InvalidUxromConfig("missing UxROM bank count".into()))?;
        if !matches!(count, 2 | 4 | 8 | 16) {
            return Err(EmitError::InvalidUxromConfig(format!(
                "bank count must be 2, 4, 8, or 16, got {count}"
            )));
        }
        if cfg.uxrom_bus_conflicts.is_none() {
            return Err(EmitError::InvalidUxromConfig(
                "missing bus-conflict mode".into(),
            ));
        }
        if assets.prg_banks.as_ref().map(Vec::len) != Some(count as usize) {
            return Err(EmitError::InvalidUxromConfig(
                "PRG bank assets do not match configured bank count".into(),
            ));
        }
    } else if cfg.uxrom_bank_count.is_some() || cfg.uxrom_bus_conflicts.is_some() {
        return Err(EmitError::InvalidUxromConfig(
            "UxROM settings supplied for a non-UxROM mapper".into(),
        ));
    }
    if cfg.mapper == 4 {
        let count = cfg.mmc3_prg_bank_count.ok_or_else(|| {
            EmitError::InvalidMmc3Config("missing physical 8 KiB PRG page count".into())
        })?;
        if count < 4 || !count.is_power_of_two() {
            return Err(EmitError::InvalidMmc3Config(format!(
                "physical PRG page count must be a power of two and at least 4, got {count}"
            )));
        }
        if assets.prg_banks.as_ref().map(Vec::len) != Some(usize::from(count) / 2) {
            return Err(EmitError::InvalidMmc3Config(
                "packed 16 KiB PRG assets do not match physical 8 KiB page count".into(),
            ));
        }
        if assets
            .prg_banks
            .as_ref()
            .unwrap()
            .iter()
            .any(|bank| bank.len() != 0x4000)
            || assets.prg_low.is_some()
        {
            return Err(EmitError::InvalidMmc3Config(
                "PRG assets require exact 16 KiB pairs and no separate low-window asset".into(),
            ));
        }
        if cfg.rom_kib != 2048 {
            return Err(EmitError::InvalidMmc3Config(
                "experimental MMC3 layout requires a 2048 KiB SMS image".into(),
            ));
        }
        if !mmc3_full
            && !cfg
                .runtime_defines
                .iter()
                .any(|name| name == "MMC3_BANKING_EXPERIMENT")
        {
            return Err(EmitError::InvalidMmc3Config(
                "MMC3 requires the explicit MMC3_BANKING_EXPERIMENT capability gate".into(),
            ));
        }
        if mmc3_full {
            let chr_len = assets.chr_nes.as_ref().map_or(0, Vec::len);
            if !(0x2000..=0x40000).contains(&chr_len)
                || !chr_len.is_power_of_two()
                || cfg.chr_ram
                || cfg.chr_ram_bg_identity
                || cfg.native_calls
                || assets.chr_4bpp.len() != 0x4000
                || cfg.raw_ciram_backend != RawCiramBackend::SramSlot2
                || cfg
                    .runtime_defines
                    .iter()
                    .any(|name| name.starts_with("CV1_") || name == "MMC3_BANKING_EXPERIMENT")
            {
                return Err(EmitError::InvalidMmc3Config(
                    "full MMC3 requires power-of-two 8..256 KiB CHR-ROM, software stack, SRAM CIRAM, and no CV1/experiment capabilities".into(),
                ));
            }
        } else if cfg.chr_ram
            || cfg.chr_ram_bg_identity
            || assets.chr_nes.as_ref().map(Vec::len) != Some(0x2000)
        {
            return Err(EmitError::InvalidMmc3Config(
                "banking experiment supports only a fixed 8 KiB CHR-ROM asset".into(),
            ));
        }
    } else if cfg.mmc3_prg_bank_count.is_some() {
        return Err(EmitError::InvalidMmc3Config(
            "MMC3 settings supplied for a non-MMC3 mapper".into(),
        ));
    }
    let prg_data_base = if cfg.mapper == 4 {
        MMC3_PRG_DATA_BASE
    } else {
        NES_PRG_BANK_BASE
    };
    let banked_error = |reason: &str| {
        if cfg.mapper == 4 {
            EmitError::InvalidMmc3Config(reason.into())
        } else {
            EmitError::InvalidUxromConfig(reason.into())
        }
    };
    let explicit_banks: Vec<u32> = build
        .asm
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix(".bank "))
        .filter_map(|tail| tail.split_whitespace().next()?.parse::<u32>().ok())
        .collect();
    let reserved_bank = if assets.prg_banks.is_some() {
        prg_data_base
    } else {
        24
    };
    if let Some(&bank) = explicit_banks.iter().find(|&&bank| bank >= reserved_bank) {
        return Err(EmitError::ReservedBankPlacement {
            bank,
            reserved_bank,
        });
    }
    if cfg.mapper == 4
        && !mmc3_full
        && let Some(bank) = explicit_banks
            .iter()
            .find(|&&bank| bank >= MMC3_CODE_BANK_LIMIT)
    {
        return Err(EmitError::InvalidMmc3Config(format!(
            "translated bank {bank} exceeds the phase-1 continuation ABI limit {}; raw PRG placement does not enlarge return-frame bank identity",
            MMC3_CODE_BANK_LIMIT - 1
        )));
    }
    let mut required_bank = explicit_banks.into_iter().max().unwrap_or(0);
    let asset_base = if let Some(banks) = &assets.prg_banks {
        required_bank = required_bank.max(prg_data_base + banks.len() as u32 - 1);
        prg_data_base + banks.len() as u32
    } else {
        24
    };
    if let Some(banks) = &assets.prg_banks {
        if assets.chr_4bpp.len() > 0x4000 {
            return Err(banked_error(
                "converted CHR does not fit its packed asset bank",
            ));
        }
        if assets.nametable.as_ref().is_some_and(|v| v.len() > 0x700)
            || (!mmc3_full && assets.chr_nes.as_ref().is_some_and(|v| v.len() > 0x2000))
            || assets
                .chr_maps
                .as_ref()
                .is_some_and(|v| v.len() > (0x4000 - PACKED_CHR_MAPS_OFFSET as usize))
        {
            return Err(banked_error(
                "small runtime assets exceed the packed bank layout",
            ));
        }
        if assets.prg_high.as_deref() != banks.last().map(Vec::as_slice) {
            return Err(banked_error(if cfg.mapper == 4 {
                "high PRG asset must match the final packed MMC3 pair"
            } else {
                "fixed PRG asset must match the final UxROM bank"
            }));
        }
        required_bank = required_bank.max(asset_base + 1);
        if mmc3_full {
            required_bank = required_bank.max(
                asset_base + 1 + (assets.chr_nes.as_ref().unwrap().len() as u32).div_ceil(0x4000),
            );
        }
    } else {
        for (enabled, offset) in [
            (true, 0),
            (true, 1),
            (assets.nametable.is_some(), 2),
            (assets.prg_low.is_some(), 3),
            (assets.chr_nes.is_some(), 4),
            (assets.chr_maps.is_some(), 5),
            (assets.prg_high.is_some(), 6),
        ] {
            if enabled {
                required_bank = required_bank.max(asset_base + offset);
            }
        }
    }
    let bank_count = cfg.rom_kib / 16;
    if build.project_data_bank_count != 0 {
        if !mmc3_full {
            return Err(EmitError::InvalidMmc3Config(
                "project data-bank reservation currently requires full MMC3".into(),
            ));
        }
        required_bank += u32::from(build.project_data_bank_count);
    }
    if required_bank >= bank_count {
        return Err(EmitError::LayoutExceedsRomCapacity {
            required_bank,
            bank_count,
        });
    }
    Ok(())
}

// ── Runtime directory helpers ─────────────────────────────────────────────────

/// Recursively collect all `*.s` file paths under `dir`, relative to `dir`.
fn collect_s_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    collect_s_files_inner(dir, dir, &mut result)?;
    result.sort();
    Ok(result)
}

fn collect_s_files_inner(root: &Path, current: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            collect_s_files_inner(root, &path, out)?;
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("s") {
                let rel = path.strip_prefix(root).expect("path under root");
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Recursively copy `src` directory into `dst` directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if ft.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

// ── File content generators ───────────────────────────────────────────────────

fn makefile_content() -> &'static str {
    // Monolithic build: sms.asm `.include`s every runtime file plus the
    // generated/translated.asm, so we only assemble one source.
    "# Auto-generated WLA-DX SMS project.\n\
     WLA      ?= wla-z80\n\
     WLALINK  ?= wlalink\n\
     OBJDIR   ?= obj\n\
     OUT      ?= sms.sms\n\
     \n\
     .PHONY: all clean\n\
     all: $(OUT)\n\
     \n\
     $(OBJDIR):\n\
     \tmkdir -p $(OBJDIR)\n\
     \n\
     $(OBJDIR)/sms.o: sms.asm | $(OBJDIR)\n\
     \t$(WLA) -o $@ $<\n\
     \n\
     $(OUT): $(OBJDIR)/sms.o link.cfg\n\
     \t$(WLALINK) -S link.cfg $(OUT)\n\
     \n\
     clean:\n\
     \trm -rf $(OBJDIR) $(OUT)\n"
}

fn link_cfg_content(_runtime_s_files: &[PathBuf]) -> String {
    // Single object file under the monolithic-include build model.
    "[objects]\nobj/sms.o\n".to_string()
}

fn build_runtime_includes(runtime_s_files: &[PathBuf]) -> String {
    // boot.s must be first because it owns the reset vector at $0000.
    let mut sorted: Vec<&PathBuf> = runtime_s_files.iter().collect();
    sorted.sort_by_key(|p| {
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if name == "boot.s" {
            (0u8, name)
        } else if name == "nz_table.s" {
            // Must be LAST: its `.orga $3e00 force` moves the WLA placement
            // cursor, and any section included after it would spill past the
            // 16 KiB bank-0 boundary.
            (2u8, name)
        } else {
            (1u8, name)
        }
    });
    let mut out = String::new();
    for rel in sorted {
        let name = rel.file_name().and_then(|s| s.to_str()).unwrap_or("");
        out.push_str(&format!(".include \"runtime/{name}\"\n         "));
    }
    out
}

fn sms_asm_content(
    cfg: &ProjectConfig<'_>,
    assets: &ProjectAssets,
    has_nametable: bool,
    runtime_s_files: &[PathBuf],
    project_data_bank_count: u16,
) -> String {
    let rom_banks = cfg.rom_kib / 16;
    let mmc3_full = cfg
        .runtime_defines
        .iter()
        .any(|name| name == "MMC3_FULL_RUNTIME");
    let runtime_includes = build_runtime_includes(runtime_s_files);
    let prg_data_base = if cfg.mapper == 4 {
        MMC3_PRG_DATA_BASE
    } else {
        NES_PRG_BANK_BASE
    };
    // Asset banks: NROM keeps the legacy 24-30; banked carts place them
    // ABOVE the NES PRG data banks so generated code can grow into 4-35.
    let asset_base: u32 = if assets.prg_banks.is_some() {
        prg_data_base
            + assets
                .prg_banks
                .as_ref()
                .map(|b| b.len() as u32)
                .unwrap_or(0)
    } else {
        24
    };
    let mut mapper_define = format!(".define NES_MAPPER {}", cfg.mapper);
    if project_data_bank_count != 0 {
        // validate_config admits this only for full MMC3 and proves the
        // reservation fits after all packed PRG, boot assets and raw CHR.
        let data_base =
            asset_base + 2 + (assets.chr_nes.as_ref().unwrap().len() as u32).div_ceil(0x4000);
        mapper_define.push_str(&format!("\n.define PROJECT_ROM_DATA_BANK_BASE {data_base}"));
    }
    if cfg.input_action {
        mapper_define.push_str("\n.define INPUT_MODE_ACTION 1");
    }
    if cfg.input_pause_start {
        mapper_define.push_str("\n.define INPUT_PAUSE_START 1");
    }
    if !cfg.scroll_split {
        mapper_define.push_str("\n.define NO_SCROLL_SPLIT 1");
    }
    if cfg.top_tile_remap_rows > 0 {
        mapper_define.push_str(&format!(
            "\n.define PROFILE_TOP_TILE_REMAP_ROWS {}\n.define PROFILE_TOP_TILE_REMAP_TO ${:02X}",
            cfg.top_tile_remap_rows, cfg.top_tile_remap_to
        ));
        for (index, tile) in cfg.top_tile_remap_from.iter().enumerate() {
            mapper_define.push_str(&format!(
                "\n.define PROFILE_TOP_TILE_REMAP_FROM_{index} ${tile:02X}"
            ));
        }
    }
    if cfg.native_calls {
        mapper_define.push_str("\n.define NATIVE_CALLS 1");
    }
    for def in &cfg.runtime_defines {
        mapper_define.push_str(&format!("\n.define {def} 1"));
    }
    // A code-generation policy may suppress immediate hardware effects only
    // if the selected runtime supplies their eventual commit. Check AFTER
    // its includes so an absent backend cannot produce a silently broken ROM.
    let capability_check = if cfg
        .runtime_defines
        .iter()
        .any(|name| name == "DEFER_SPRITE_REGISTERS")
    {
        ".ifndef RUNTIME_HAS_SPRITE_REGISTER_COMMIT\n.fail \"deferred sprite registers require a runtime SAT commit backend\"\n.endif\n"
    } else {
        ""
    };
    if cfg.chr_ram_bg_identity {
        mapper_define.push_str("\n.define PROFILE_CHR_RAM_BG_IDENTITY 1");
    }
    if cfg.chr_ram {
        mapper_define.push_str("\n.define NES_CHR_RAM 1\n.define CHR_RAM_SRAM_BASE $8800");
    }
    if cfg.mapper == 4 {
        mapper_define.push_str(&format!(
            "\n.define NES_MMC3 1\n.define NES_MMC3_PRG_BANK_COUNT {}\n.define MMC3_PRG_DATA_BASE {MMC3_PRG_DATA_BASE}",
            cfg.mmc3_prg_bank_count.expect("validated MMC3 config")
        ));
        if mmc3_full {
            let pages = assets.chr_nes.as_ref().expect("validated CHR-ROM").len() / 0x400;
            mapper_define.push_str(&format!(
                "\n.define MMC3_CHR_DATA_BASE {}\n.define MMC3_CHR_BANK_COUNT {pages}\n.define MMC3_CHR_BANK_MASK {}",
                asset_base + 2, pages - 1,
            ));
        }
    } else if assets.prg_banks.is_some() {
        mapper_define.push_str(&format!(
            "\n.define NES_PRG_BANK_BASE {NES_PRG_BANK_BASE}\n.define NES_PRG_BANK_COUNT {}\n.define NES_PRG_BANK_MASK {}",
            cfg.uxrom_bank_count.expect("validated UxROM config"),
            cfg.uxrom_bank_count.expect("validated UxROM config") - 1
        ));
        let conflicts = if cfg.uxrom_bus_conflicts == Some(UxromBusConflicts::And) {
            1
        } else {
            0
        };
        mapper_define.push_str(&format!("\n.define NES_PRG_BUS_CONFLICTS {conflicts}"));
    }
    let mirroring_define = match cfg.mirroring {
        NesMirroring::Vertical => ".define NES_MIRRORING_VERTICAL 1",
        NesMirroring::Horizontal => ".define NES_MIRRORING_HORIZONTAL 1",
    };
    let raw_ciram_define = match cfg.raw_ciram_backend {
        RawCiramBackend::None => "; raw CIRAM backend disabled".to_string(),
        RawCiramBackend::SramSlot2 => "\
         .define RAW_CIRAM_BACKEND_SRAM 1\n\
         .define RAW_CIRAM_SRAM_BASE $8000\n\
         .define RAW_CIRAM_SRAM_CTRL $08"
            .to_string(),
    };
    let mut out = format!(
        "; Top-level SMS project file. Generated by sms_project.\n\
         .memorymap\n\
         \tdefaultslot 0\n\
         \tslotsize $4000\n\
         \tslot 0 $0000\n\
         \tslotsize $4000\n\
         \tslot 1 $4000\n\
         \tslotsize $4000\n\
         \tslot 2 $8000\n\
         \tslotsize $2000\n\
         \tslot 3 $C000\n\
         .endme\n\
         \n\
         .rombankmap\n\
         \tbankstotal {rom_banks}\n\
         \tbanksize $4000\n\
         \tbanks {rom_banks}\n\
         .endro\n\
         \n\
         .sdsctag 1.0,\"{title}\",\"NES-to-SMS translation\",\"auto\"\n\
         .bank 0 slot 0\n\
         \n\
         {mapper_define}\n\
         {mirroring_define}\n\
         {raw_ciram_define}\n\
         \n\
         ; Pull in runtime + generated translation.\n\
         {runtime_includes}{capability_check}.include \"generated/translated.asm\"\n\
         \n\
         ; ── Data blobs ───────────────────────────────────────────────────\n\
         ; Assets are pinned to slot 2. Mapper builds pack the small blobs\n\
         ; together; symbols retain their exact slot-2 logical addresses.\n\
         .bank {asset_chr} slot 2\n\
         .org ${palette_org:04X}\n\
         .section \"data_chr\" force\n\
         data_chr:\n\
         .incbin \"data/chr.4bpp\"\n\
         data_chr_end:\n\
         .ends\n\
         \n\
         .define data_chr_size {chr_size}\n\
         \n\
         .bank {asset_palette} slot 2\n\
         .org $0000\n\
         .section \"data_palette\" force\n\
         data_palette:\n\
         .incbin \"data/palette.cram\"\n\
         .ends\n",
        rom_banks = rom_banks,
        asset_chr = asset_base,
        asset_palette = asset_base + 1,
        palette_org = PACKED_PALETTE_OFFSET,
        title = cfg.title,
        mapper_define = mapper_define,
        mirroring_define = mirroring_define,
        raw_ciram_define = raw_ciram_define,
        chr_size = assets.chr_4bpp.len(),
    );

    if has_nametable {
        let (bank, org) = if assets.prg_banks.is_some() {
            (asset_base + 1, PACKED_NAMETABLE_OFFSET)
        } else {
            (asset_base + 2, 0)
        };
        out.push_str(&format!(
            "\n\
             .bank {bank} slot 2\n\
             .org ${org:04X}\n\
             .section \"data_nametable\" force\n\
             data_nametable:\n\
             .incbin \"data/nametable.bin\"\n\
             .ends\n"
        ));
    }

    if assets.prg_low.is_some() {
        out.push_str(&format!(
            "\n\
             .bank {} slot 2\n\
             .org $0000\n\
             .section \"data_prg_low\" force\n\
             data_prg_low:\n\
             .incbin \"data/prg_low.bin\"\n\
             .ends\n",
            asset_base + 3
        ));
    }

    if let Some(banks) = &assets.prg_banks {
        // Banked mappers: NES PRG bank k lives at SMS bank
        // NES_PRG_BANK_BASE + k, pinned to slot 2. rt_mapper_write maps
        // the selected bank into the window; data_prg_low aliases bank 0
        // so game-agnostic runtime restore paths keep working.
        for (k, _) in banks.iter().enumerate() {
            let bank = prg_data_base + k as u32;
            out.push_str(&format!(
                "\n.bank {bank} slot 2\n\
                 .org $0000\n\
                 .section \"data_prg_bank_{k}\" force\n\
                 data_prg_bank_{k}:\n\
                 .incbin \"data/prg_bank_{k}.bin\"\n\
                 .ends\n"
            ));
        }
        out.push_str("\n.define data_prg_low data_prg_bank_0\n");
    }

    if assets.prg_high.is_some() {
        if let Some(banks) = &assets.prg_banks {
            out.push_str(&format!(
                "\n.define data_prg_high data_prg_bank_{}\n",
                banks.len() - 1
            ));
        } else {
            out.push_str(&format!(
                "\n\
                 .bank {} slot 2\n\
                 .org $0000\n\
                 .section \"data_prg_high\" force\n\
                 data_prg_high:\n\
                 .incbin \"data/prg_high.bin\"\n\
                 .ends\n",
                asset_base + 6
            ));
        }
    }

    if mmc3_full {
        for (page, bytes) in assets.chr_nes.as_ref().unwrap().chunks(0x4000).enumerate() {
            let bank = asset_base + 2 + page as u32;
            let skip = page * 0x4000;
            out.push_str(&format!(
                "\n.bank {bank} slot 2\n.org $0000\n.section \"data_mmc3_chr_{page}\" force\ndata_mmc3_chr_{page}:\n.incbin \"data/chr.nes\" SKIP {skip} READ {}\n.ends\n", bytes.len()
            ));
        }
        // Legacy code remains assembled but its PPU read path is bypassed.
        out.push_str("\n.define data_chr_nes data_mmc3_chr_0\n");
    } else if assets.chr_nes.is_some() {
        let (bank, org) = if assets.prg_banks.is_some() {
            (asset_base + 1, PACKED_CHR_NES_OFFSET)
        } else {
            (asset_base + 4, 0)
        };
        out.push_str(&format!(
            "\n\
             .bank {bank} slot 2\n\
             .org ${org:04X}\n\
             .section \"data_chr_nes\" force\n\
             data_chr_nes:\n\
             .incbin \"data/chr.nes\"\n\
             .ends\n"
        ));
    }

    if assets.chr_maps.is_some() {
        let (bank, org) = if assets.prg_banks.is_some() {
            (asset_base + 1, PACKED_CHR_MAPS_OFFSET)
        } else {
            (asset_base + 5, 0)
        };
        out.push_str(&format!(
            "\n\
             .bank {bank} slot 2\n\
             .org ${org:04X}\n\
             .section \"data_chr_maps\" force\n\
             data_chr_maps:\n\
             data_chr_bg_map0:\n\
             .incbin \"data/chr_maps.bin\" READ $200\n\
             data_chr_bg_map1:\n\
             .incbin \"data/chr_maps.bin\" SKIP $200 READ $200\n\
             data_chr_sprite_map0:\n\
             .incbin \"data/chr_maps.bin\" SKIP $400 READ $100\n\
             data_chr_sprite_map1:\n\
             .incbin \"data/chr_maps.bin\" SKIP $500 READ $100\n\
             data_chr_maps_end:\n\
             .ends\n"
        ));
    }

    out
}

// ── Core data emission (shared between the two public functions) ───────────────

fn emit_data_files(
    out_dir: &Path,
    build: &z80_emit::Build,
    assets: &ProjectAssets,
) -> Result<(), EmitError> {
    let generated_dir = out_dir.join("generated");
    fs::create_dir_all(&generated_dir)?;
    fs::write(generated_dir.join("translated.asm"), &build.asm)?;

    let data_dir = out_dir.join("data");
    fs::create_dir_all(&data_dir)?;
    fs::write(data_dir.join("chr.4bpp"), &assets.chr_4bpp)?;
    fs::write(data_dir.join("palette.cram"), assets.palette)?;
    if let Some(nt) = &assets.nametable {
        fs::write(data_dir.join("nametable.bin"), nt)?;
    }
    if let Some(prg_low) = &assets.prg_low {
        fs::write(data_dir.join("prg_low.bin"), prg_low)?;
    }
    if let Some(prg_high) = &assets.prg_high {
        fs::write(data_dir.join("prg_high.bin"), prg_high)?;
    }
    if let Some(banks) = &assets.prg_banks {
        for (k, b) in banks.iter().enumerate() {
            fs::write(data_dir.join(format!("prg_bank_{k}.bin")), b)?;
        }
    }
    if let Some(chr_nes) = &assets.chr_nes {
        fs::write(data_dir.join("chr.nes"), chr_nes)?;
    }
    if let Some(chr_maps) = &assets.chr_maps {
        fs::write(data_dir.join("chr_maps.bin"), chr_maps)?;
    }

    Ok(())
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Emit the full project tree under `out_dir`.
///
/// Creates directories as needed and overwrites existing files. If
/// `runtime_src_dir` is provided its contents are recursively copied into
/// `out_dir/runtime/`.
pub fn emit_project(
    out_dir: &Path,
    build: &z80_emit::Build,
    assets: &ProjectAssets,
    cfg: &ProjectConfig<'_>,
    runtime_src_dir: Option<&Path>,
) -> Result<(), EmitError> {
    validate_config(cfg, assets, build)?;

    fs::create_dir_all(out_dir)?;

    // Collect runtime .s files (needed for link.cfg) before copying.
    let s_files = if let Some(src) = runtime_src_dir {
        collect_s_files(src)?
    } else {
        Vec::new()
    };

    // Makefile
    fs::write(out_dir.join("Makefile"), makefile_content())?;

    // link.cfg
    fs::write(out_dir.join("link.cfg"), link_cfg_content(&s_files))?;

    // sms.asm
    let sms_asm = sms_asm_content(
        cfg,
        assets,
        assets.nametable.is_some(),
        &s_files,
        build.project_data_bank_count,
    );
    fs::write(out_dir.join("sms.asm"), sms_asm)?;

    // runtime/
    if let Some(src) = runtime_src_dir {
        let runtime_dst = out_dir.join("runtime");
        copy_dir_recursive(src, &runtime_dst)?;
    }

    // generated/ + data/
    emit_data_files(out_dir, build, assets)?;

    Ok(())
}

/// Emit only the generated asm and data files.
///
/// Skips Makefile, link.cfg, sms.asm, and runtime. Useful for tests and the
/// "assets only" mode.
pub fn emit_assets_only(
    out_dir: &Path,
    build: &z80_emit::Build,
    assets: &ProjectAssets,
) -> Result<(), EmitError> {
    fs::create_dir_all(out_dir)?;
    emit_data_files(out_dir, build, assets)?;
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(prefix: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::env::temp_dir().join(format!("{prefix}_{ts}"))
    }

    fn minimal_build() -> z80_emit::Build {
        let mut p = z80_emit::Program::new();
        p.nop();
        p.finish().unwrap()
    }

    fn minimal_assets() -> ProjectAssets {
        ProjectAssets {
            prg_banks: None,
            chr_4bpp: vec![0u8; 32],
            palette: [0u8; 32],
            nametable: None,
            prg_low: None,
            prg_high: None,
            chr_nes: None,
            chr_maps: None,
        }
    }

    fn minimal_cfg() -> ProjectConfig<'static> {
        ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 512,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        }
    }

    fn uxrom_bank_payloads() -> Vec<Vec<u8>> {
        (0u8..8)
            .map(|bank| {
                (0..0x4000)
                    .map(|offset| {
                        bank.wrapping_mul(0x1d)
                            .wrapping_add((offset as u8).wrapping_mul(0x49))
                    })
                    .collect()
            })
            .collect()
    }

    fn uxrom_assets() -> (ProjectAssets, Vec<Vec<u8>>) {
        let banks = uxrom_bank_payloads();
        let mut assets = minimal_assets();
        assets.prg_high = Some(banks.last().unwrap().clone());
        assets.prg_banks = Some(banks.clone());
        (assets, banks)
    }

    fn uxrom_cfg(bus_conflicts: UxromBusConflicts) -> ProjectConfig<'static> {
        let mut cfg = minimal_cfg();
        cfg.mapper = 2;
        cfg.uxrom_bank_count = Some(8);
        cfg.uxrom_bus_conflicts = Some(bus_conflicts);
        cfg
    }

    fn mmc3_assets_and_cfg(count: u8) -> (ProjectAssets, ProjectConfig<'static>) {
        let mut assets = minimal_assets();
        let pairs: Vec<Vec<u8>> = (0..count / 2)
            .map(|pair| {
                let mut bytes = vec![pair * 2; 0x2000];
                bytes.extend(vec![pair * 2 + 1; 0x2000]);
                bytes
            })
            .collect();
        assets.prg_high = pairs.last().cloned();
        assets.prg_banks = Some(pairs);
        assets.chr_nes = Some(vec![0; 0x2000]);
        assets.chr_4bpp = vec![0; 0x4000];
        let mut cfg = minimal_cfg();
        cfg.mapper = 4;
        cfg.rom_kib = 2048;
        cfg.mmc3_prg_bank_count = Some(count);
        cfg.runtime_defines.push("MMC3_BANKING_EXPERIMENT".into());
        (assets, cfg)
    }

    // ── 1. emit_project produces all expected files and directories ────────────

    #[test]
    fn test_emit_project_all_files_present() {
        let out = unique_dir("sms_proj_full");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = minimal_cfg();

        // Create a tiny runtime_src_dir with one stub file.
        let rt_src = unique_dir("sms_proj_rt_src");
        fs::create_dir_all(&rt_src).unwrap();
        fs::write(rt_src.join("boot.s"), "; stub boot\n").unwrap();

        emit_project(&out, &build, &assets, &cfg, Some(&rt_src)).unwrap();

        assert!(out.join("Makefile").exists(), "Makefile missing");
        assert!(out.join("link.cfg").exists(), "link.cfg missing");
        assert!(out.join("sms.asm").exists(), "sms.asm missing");
        assert!(out.join("runtime").is_dir(), "runtime/ dir missing");
        assert!(
            out.join("runtime/boot.s").exists(),
            "runtime/boot.s missing"
        );
        assert!(
            out.join("generated/translated.asm").exists(),
            "translated.asm missing"
        );
        assert!(out.join("data/chr.4bpp").exists(), "chr.4bpp missing");
        assert!(
            out.join("data/palette.cram").exists(),
            "palette.cram missing"
        );

        fs::remove_dir_all(&out).unwrap();
        fs::remove_dir_all(&rt_src).unwrap();
    }

    #[test]
    #[ignore = "requires WLA-DX inside Docker; run cargo test -p sms_project deferred_sprite_capability -- --ignored"]
    fn deferred_sprite_capability_is_checked_by_the_assembler() {
        for deferred in [false, true] {
            for capability in [false, true] {
                let root = unique_dir("sms_sprite_capability");
                let runtime = root.join("source");
                let project = root.join("project");
                fs::create_dir_all(&runtime).unwrap();
                fs::write(
                    runtime.join("backend.s"),
                    if capability {
                        ".define RUNTIME_HAS_SPRITE_REGISTER_COMMIT 1\n"
                    } else {
                        "; backend has no prepared sprite commit\n"
                    },
                )
                .unwrap();
                let mut cfg = minimal_cfg();
                if deferred {
                    cfg.runtime_defines.push("DEFER_SPRITE_REGISTERS".into());
                }
                emit_project(
                    &project,
                    &minimal_build(),
                    &minimal_assets(),
                    &cfg,
                    Some(&runtime),
                )
                .unwrap();
                let result = std::process::Command::new("make")
                    .current_dir(&project)
                    .output()
                    .expect("run this explicit test inside the Docker WLA-DX toolchain");
                assert_eq!(
                    result.status.success(),
                    !deferred || capability,
                    "deferred={deferred}, capability={capability}: {}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                );
                if deferred && !capability {
                    assert!(String::from_utf8_lossy(&result.stderr).contains(
                        "deferred sprite registers require a runtime SAT commit backend"
                    ));
                }
                fs::remove_dir_all(root).unwrap();
            }
        }
    }

    // ── 2. emit_assets_only produces only generated/ and data/ ───────────────

    #[test]
    fn test_emit_assets_only_no_scaffold() {
        let out = unique_dir("sms_proj_assets");
        let build = minimal_build();
        let assets = ProjectAssets {
            prg_banks: None,
            chr_4bpp: vec![0xAB; 64],
            palette: [0x55; 32],
            nametable: Some(vec![0u8; 1792]),
            prg_low: None,
            prg_high: None,
            chr_nes: None,
            chr_maps: None,
        };

        emit_assets_only(&out, &build, &assets).unwrap();

        assert!(
            out.join("generated/translated.asm").exists(),
            "translated.asm missing"
        );
        assert!(out.join("data/chr.4bpp").exists(), "chr.4bpp missing");
        assert!(
            out.join("data/palette.cram").exists(),
            "palette.cram missing"
        );
        assert!(
            out.join("data/nametable.bin").exists(),
            "nametable.bin missing"
        );

        // Scaffold files must NOT be present.
        assert!(!out.join("Makefile").exists(), "unexpected Makefile");
        assert!(!out.join("link.cfg").exists(), "unexpected link.cfg");
        assert!(!out.join("sms.asm").exists(), "unexpected sms.asm");
        assert!(!out.join("runtime").exists(), "unexpected runtime/");

        fs::remove_dir_all(&out).unwrap();
    }

    // ── 3. Title > 11 chars returns InvalidTitle ──────────────────────────────

    #[test]
    fn test_invalid_title_too_long() {
        let out = unique_dir("sms_proj_bad_title");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 32,
            region: 0x4C,
            title: "TOOLONGTITLE", // 12 chars
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        let err = emit_project(&out, &build, &assets, &cfg, None).unwrap_err();
        assert!(matches!(err, EmitError::InvalidTitle(_)));
    }

    // ── 4. rom_kib = 17 returns InvalidRomSize ────────────────────────────────

    #[test]
    fn test_invalid_rom_size_not_multiple_of_16() {
        let out = unique_dir("sms_proj_bad_rom");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 17,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        let err = emit_project(&out, &build, &assets, &cfg, None).unwrap_err();
        assert!(matches!(err, EmitError::InvalidRomSize(17)));
    }

    // ── 5. sms.asm contains .rombankmap with correct bankstotal ──────────────

    #[test]
    fn test_sms_asm_rombankmap_bankstotal() {
        let out = unique_dir("sms_proj_bankmap");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 512,
            region: 0x4C,
            title: "BANKS4",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        // 512 KiB / 16 = 32 banks
        assert!(
            sms_asm.contains("bankstotal 32"),
            "expected 'bankstotal 32' in sms.asm"
        );
        assert!(
            sms_asm.contains("banks 32"),
            "expected 'banks 32' in sms.asm"
        );

        fs::remove_dir_all(&out).unwrap();
    }

    // ── 6. link.cfg lists runtime .s files + sms.o + translated.o ────────────

    #[test]
    fn test_link_cfg_lists_all_runtime_s_files() {
        let out = unique_dir("sms_proj_linkcfg");
        let rt_src = unique_dir("sms_proj_rt_src2");
        fs::create_dir_all(&rt_src).unwrap();
        fs::write(rt_src.join("boot.s"), "; boot stub\n").unwrap();
        fs::write(rt_src.join("vdp.s"), "; vdp stub\n").unwrap();

        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = minimal_cfg();

        emit_project(&out, &build, &assets, &cfg, Some(&rt_src)).unwrap();

        // Monolithic build: sms.o is the only object. Runtime files are
        // `.include`d into sms.asm, not assembled separately. Verify the
        // sms.asm reflects that.
        let link_cfg = fs::read_to_string(out.join("link.cfg")).unwrap();
        assert!(
            link_cfg.contains("obj/sms.o"),
            "sms.o missing from link.cfg"
        );
        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".include \"runtime/boot.s\""));
        assert!(sms_asm.contains(".include \"runtime/vdp.s\""));
        assert!(sms_asm.contains(".include \"generated/translated.asm\""));

        fs::remove_dir_all(&out).unwrap();
        fs::remove_dir_all(&rt_src).unwrap();
    }

    // ── Bonus: rom_kib < 16 returns InvalidRomSize ───────────────────────────

    #[test]
    fn test_invalid_rom_size_too_small() {
        let out = unique_dir("sms_proj_rom_small");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 8,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        let err = emit_project(&out, &build, &assets, &cfg, None).unwrap_err();
        assert!(matches!(err, EmitError::InvalidRomSize(8)));
    }

    // ── Bonus: nametable.bin only written when nametable is Some ─────────────

    #[test]
    fn test_nametable_absent_when_none() {
        let out = unique_dir("sms_proj_no_nt");
        let build = minimal_build();
        let assets = minimal_assets(); // nametable: None
        let cfg = minimal_cfg();

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        assert!(
            !out.join("data/nametable.bin").exists(),
            "nametable.bin should be absent"
        );

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(
            !sms_asm.contains("data_nametable"),
            "nametable section should be absent"
        );

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn test_sms_asm_contains_vertical_mirroring_define() {
        let out = unique_dir("sms_proj_vertical_mirroring");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 512,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define NES_MIRRORING_VERTICAL 1"));
        assert!(!sms_asm.contains(".define NES_MIRRORING_HORIZONTAL 1"));

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn test_sms_asm_contains_top_tile_remap_defines() {
        let out = unique_dir("sms_proj_top_tile_remap");
        let build = minimal_build();
        let assets = minimal_assets();
        let mut cfg = minimal_cfg();
        cfg.top_tile_remap_rows = 6;
        cfg.top_tile_remap_from = vec![0x37, 0x38];
        cfg.top_tile_remap_to = 0;

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define PROFILE_TOP_TILE_REMAP_ROWS 6"));
        assert!(sms_asm.contains(".define PROFILE_TOP_TILE_REMAP_FROM_0 $37"));
        assert!(sms_asm.contains(".define PROFILE_TOP_TILE_REMAP_FROM_1 $38"));
        assert!(sms_asm.contains(".define PROFILE_TOP_TILE_REMAP_TO $00"));

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn test_sms_asm_contains_horizontal_mirroring_define() {
        let out = unique_dir("sms_proj_horizontal_mirroring");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 512,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Horizontal,
            raw_ciram_backend: RawCiramBackend::None,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define NES_MIRRORING_HORIZONTAL 1"));
        assert!(!sms_asm.contains(".define NES_MIRRORING_VERTICAL 1"));

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn test_sms_asm_contains_raw_ciram_sram_defines() {
        let out = unique_dir("sms_proj_raw_ciram_sram");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            mapper: 0,
            uxrom_bank_count: None,
            mmc3_prg_bank_count: None,
            uxrom_bus_conflicts: None,
            chr_ram: false,
            input_action: false,
            input_pause_start: false,
            scroll_split: true,
            top_tile_remap_rows: 0,
            top_tile_remap_from: Vec::new(),
            top_tile_remap_to: 0,
            chr_ram_bg_identity: false,
            rom_kib: 512,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::SramSlot2,
            native_calls: false,
            runtime_defines: Vec::new(),
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define RAW_CIRAM_BACKEND_SRAM 1"));
        assert!(sms_asm.contains(".define RAW_CIRAM_SRAM_BASE $8000"));
        assert!(sms_asm.contains(".define RAW_CIRAM_SRAM_CTRL $08"));

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn uxrom_eight_banks_preserve_assets_fixed_bank_and_mapper_defines() {
        let out = unique_dir("sms_proj_uxrom");
        let build = minimal_build();
        let (assets, banks) = uxrom_assets();
        let cfg = uxrom_cfg(UxromBusConflicts::And);

        emit_project(&out, &build, &assets, &cfg, None).unwrap();
        let asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert_eq!(banks.len(), 8);
        assert!(asm.contains(".define NES_PRG_BANK_COUNT 8"));
        assert!(asm.contains(".define NES_PRG_BANK_MASK 7"));
        assert!(asm.contains(".define NES_PRG_BUS_CONFLICTS 1"));
        for (bank, expected) in banks.iter().enumerate() {
            assert_eq!(
                fs::read(out.join(format!("data/prg_bank_{bank}.bin"))).unwrap(),
                *expected,
                "PRG bank {bank} was reordered, aliased, or truncated"
            );
            assert!(asm.contains(&format!(".bank {} slot 2", NES_PRG_BANK_BASE + bank as u32)));
            assert!(asm.contains(&format!(".incbin \"data/prg_bank_{bank}.bin\"")));
        }
        assert!(!out.join("data/prg_bank_8.bin").exists());
        assert_eq!(
            fs::read(out.join("data/prg_high.bin")).unwrap(),
            *banks.last().unwrap(),
            "fixed PRG asset must be the physical final PRG bank"
        );
        assert!(asm.contains(".define data_prg_high data_prg_bank_7"));
        assert!(asm.contains(&format!(".bank {} slot 2", NES_PRG_BANK_BASE + 9)));
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn uxrom_without_conflicts_emits_zero_define() {
        let out = unique_dir("sms_proj_uxrom_no_conflict");
        let build = minimal_build();
        let (assets, _) = uxrom_assets();
        let cfg = uxrom_cfg(UxromBusConflicts::None);
        emit_project(&out, &build, &assets, &cfg, None).unwrap();
        let asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(asm.contains(".define NES_PRG_BUS_CONFLICTS 0"));
        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn rejects_prg_bank_assets_for_non_uxrom_mapper() {
        let out = unique_dir("sms_proj_non_uxrom_prg_banks");
        let build = minimal_build();
        let (assets, _) = uxrom_assets();
        let cfg = minimal_cfg();

        let err = emit_project(&out, &build, &assets, &cfg, None).unwrap_err();
        assert!(matches!(err, EmitError::InvalidUxromConfig(_)));
        assert!(!out.exists());
    }

    #[test]
    fn rejects_explicit_build_bank_in_reserved_uxrom_data_range() {
        let out = unique_dir("sms_proj_reserved_bank");
        let mut build = minimal_build();
        build
            .asm
            .push_str(&format!("\n.bank {NES_PRG_BANK_BASE} slot 1\n"));
        let (assets, _) = uxrom_assets();
        let cfg = uxrom_cfg(UxromBusConflicts::None);

        assert!(matches!(
            emit_project(&out, &build, &assets, &cfg, None),
            Err(EmitError::ReservedBankPlacement {
                bank: NES_PRG_BANK_BASE,
                reserved_bank: NES_PRG_BANK_BASE,
            })
        ));
    }

    #[test]
    fn rejects_uxrom_layout_that_exceeds_rom_capacity() {
        let out = unique_dir("sms_proj_uxrom_overflow");
        let build = minimal_build();
        let (assets, _) = uxrom_assets();
        let mut cfg = uxrom_cfg(UxromBusConflicts::None);
        cfg.rom_kib = 480;

        assert!(matches!(
            emit_project(&out, &build, &assets, &cfg, None),
            Err(EmitError::LayoutExceedsRomCapacity { .. })
        ));
    }

    #[test]
    fn mmc3_preserves_physical_pages_and_uses_separate_runtime_defines() {
        let out = unique_dir("sms_proj_mmc3");
        let (assets, cfg) = mmc3_assets_and_cfg(32);
        let mut build = minimal_build();
        build.asm.push_str("\n.bank 31 slot 1\n");
        emit_project(&out, &build, &assets, &cfg, None).unwrap();
        let asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(asm.contains("bankstotal 128"));
        assert!(asm.contains(".define NES_MMC3 1"));
        assert!(asm.contains(".define NES_MMC3_PRG_BANK_COUNT 32"));
        assert!(asm.contains(".define MMC3_PRG_DATA_BASE 96"));
        assert!(!asm.contains(".define NES_PRG_BANK_"));
        assert!(!asm.contains(".define NES_PRG_BUS_CONFLICTS"));
        assert!(asm.contains(".define data_prg_low data_prg_bank_0"));
        assert!(asm.contains(".define data_prg_high data_prg_bank_15"));
        assert!(asm.contains(".bank 112 slot 2"));
        assert!(asm.contains(".bank 113 slot 2"));
        for pair in 0..16u8 {
            let bytes = fs::read(out.join(format!("data/prg_bank_{pair}.bin"))).unwrap();
            assert_eq!(&bytes[..0x2000], vec![pair * 2; 0x2000]);
            assert_eq!(&bytes[0x2000..], vec![pair * 2 + 1; 0x2000]);
            assert!(asm.contains(&format!(
                ".bank {} slot 2",
                MMC3_PRG_DATA_BASE + u32::from(pair)
            )));
        }
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn mmc3_rejects_invalid_configuration_before_creating_output() {
        for case in 0..12 {
            let out = unique_dir("sms_proj_mmc3_invalid");
            let (mut assets, mut cfg) = mmc3_assets_and_cfg(8);
            match case {
                0 => cfg.mmc3_prg_bank_count = None,
                1 => cfg.mmc3_prg_bank_count = Some(3),
                2 => cfg.mmc3_prg_bank_count = Some(16),
                3 => {
                    assets.prg_banks.as_mut().unwrap()[0].pop();
                }
                4 => cfg.rom_kib = 1024,
                5 => cfg.runtime_defines.clear(),
                6 => cfg.chr_ram = true,
                7 => assets.chr_nes.as_mut().unwrap().push(0),
                8 => assets.prg_low = Some(vec![0; 0x4000]),
                9 => assets.prg_high.as_mut().unwrap()[0] ^= 1,
                10 => cfg.mapper = 0,
                11 => assets.chr_4bpp.push(0),
                _ => unreachable!(),
            }
            // Non-MMC3 PRG assets are caught by the older banked-asset gate.
            assert!(
                matches!(
                    emit_project(&out, &minimal_build(), &assets, &cfg, None),
                    Err(EmitError::InvalidMmc3Config(_) | EmitError::InvalidUxromConfig(_))
                ),
                "case {case}"
            );
            assert!(!out.exists(), "case {case} created partial output");
        }
        let mut cfg = minimal_cfg();
        cfg.mmc3_prg_bank_count = Some(8);
        assert!(matches!(
            validate_config(&cfg, &minimal_assets(), &minimal_build()),
            Err(EmitError::InvalidMmc3Config(_))
        ));
    }

    #[test]
    fn mmc3_rejects_code_data_overlap_and_asset_capacity_overflow() {
        let (assets, cfg) = mmc3_assets_and_cfg(8);
        let mut build = minimal_build();
        build.asm.push_str("\n.bank 96 slot 1\n");
        assert!(matches!(
            validate_config(&cfg, &assets, &build),
            Err(EmitError::ReservedBankPlacement {
                bank: 96,
                reserved_bank: 96
            })
        ));
        let (assets, cfg) = mmc3_assets_and_cfg(64);
        assert!(matches!(
            validate_config(&cfg, &assets, &minimal_build()),
            Err(EmitError::LayoutExceedsRomCapacity {
                required_bank: 129,
                bank_count: 128
            })
        ));
    }

    #[test]
    fn mmc3_code_banks_respect_materialized_continuation_bank_bits() {
        let (assets, cfg) = mmc3_assets_and_cfg(8);
        let mut last_supported = minimal_build();
        last_supported.asm.push_str("\n.bank 31 slot 1\n");
        assert!(validate_config(&cfg, &assets, &last_supported).is_ok());
        for bank in [32, 63, 64, 95] {
            let mut build = minimal_build();
            build.asm.push_str(&format!("\n.bank {bank} slot 1\n"));
            assert!(matches!(
                validate_config(&cfg, &assets, &build),
                Err(EmitError::InvalidMmc3Config(reason)) if reason.contains("continuation ABI")
            ));
        }
    }

    #[test]
    fn mmc3_full_chr_layout_preserves_every_physical_page() {
        let (mut assets, mut cfg) = mmc3_assets_and_cfg(32);
        cfg.runtime_defines = vec!["MMC3_FULL_RUNTIME".into(), "SMB3_MMC3_SINGLE_SPLIT".into()];
        cfg.raw_ciram_backend = RawCiramBackend::SramSlot2;
        assets.chr_nes = Some((0..128u8).flat_map(|page| vec![page; 0x400]).collect());
        let mut build = minimal_build();
        build.asm.push_str("\n.bank 95 slot 1\n");
        let out = unique_dir("sms_proj_mmc3_full_chr");
        emit_project(&out, &build, &assets, &cfg, None).unwrap();
        let asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(asm.contains(".define MMC3_CHR_DATA_BASE 114"));
        assert!(asm.contains(".define MMC3_CHR_BANK_COUNT 128"));
        assert!(asm.contains(".define MMC3_CHR_BANK_MASK 127"));
        assert!(!asm.contains(".define NES_CHR_RAM"));
        assert!(!asm.contains(".define NES_PRG_BANK_BASE"));
        for bank in 0..8 {
            assert!(asm.contains(&format!(".bank {} slot 2", 114 + bank)));
            assert!(asm.contains(&format!("SKIP {} READ 16384", bank * 16384)));
        }
        assert_eq!(
            fs::read(out.join("data/chr.nes")).unwrap(),
            assets.chr_nes.unwrap()
        );
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn mmc3_full_rejects_unsafe_graphics_contracts() {
        for case in 0..8 {
            let (mut assets, mut cfg) = mmc3_assets_and_cfg(32);
            cfg.runtime_defines = vec!["MMC3_FULL_RUNTIME".into()];
            cfg.raw_ciram_backend = RawCiramBackend::SramSlot2;
            assets.chr_nes = Some(vec![0; 0x20000]);
            match case {
                0 => cfg.native_calls = true,
                1 => cfg.raw_ciram_backend = RawCiramBackend::None,
                2 => cfg.runtime_defines.push("CV1_RUNTIME_HOOKS".into()),
                3 => cfg.runtime_defines.push("MMC3_BANKING_EXPERIMENT".into()),
                4 => assets.chr_nes = Some(vec![0; 0x18000]),
                5 => assets.chr_nes = Some(vec![0; 0x1000]),
                6 => cfg.chr_ram = true,
                7 => assets.chr_nes = Some(vec![0; 0x40000]), // bank129 exceeds image
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    validate_config(&cfg, &assets, &minimal_build()),
                    Err(EmitError::InvalidMmc3Config(_)
                        | EmitError::LayoutExceedsRomCapacity { .. })
                ),
                "case {case}"
            );
        }
    }

    #[test]
    fn project_data_reservation_follows_assets_and_fails_at_image_boundary() {
        let (mut assets, mut cfg) = mmc3_assets_and_cfg(32);
        cfg.runtime_defines = vec!["MMC3_FULL_RUNTIME".into()];
        cfg.raw_ciram_backend = RawCiramBackend::SramSlot2;
        assets.chr_nes = Some(vec![0; 0x20000]);
        let mut build = minimal_build();
        build.project_data_bank_count = 4;
        let out = unique_dir("sms_proj_dense_index");
        emit_project(&out, &build, &assets, &cfg, None).unwrap();
        let asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(asm.contains(".define PROJECT_ROM_DATA_BANK_BASE 122"));
        fs::remove_dir_all(out).unwrap();
        build.project_data_bank_count = 6;
        assert!(
            validate_config(&cfg, &assets, &build).is_ok(),
            "banks122..127 exactly fit"
        );
        build.project_data_bank_count = 7;
        assert!(matches!(
            validate_config(&cfg, &assets, &build),
            Err(EmitError::LayoutExceedsRomCapacity {
                required_bank: 128,
                bank_count: 128
            })
        ));
        build.project_data_bank_count = 4;
        cfg.runtime_defines = vec!["MMC3_BANKING_EXPERIMENT".into()];
        assets.chr_nes = Some(vec![0; 8192]);
        assert!(
            validate_config(&cfg, &assets, &build).is_err(),
            "no experimental/legacy reservation"
        );
    }
}
