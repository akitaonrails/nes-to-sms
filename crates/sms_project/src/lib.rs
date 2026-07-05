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
    /// Optional upper/fixed NES PRG window ($C000-$FFFF) mirrored into SMS slot 2
    /// for runtime-assisted reads of fixed-bank data tables.
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
}

#[derive(Debug)]
pub enum EmitError {
    Io(io::Error),
    InvalidTitle(String),
    InvalidRomSize(u32),
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

fn validate_config(cfg: &ProjectConfig<'_>) -> Result<(), EmitError> {
    if !cfg.title.is_ascii() || cfg.title.len() > 11 {
        return Err(EmitError::InvalidTitle(cfg.title.to_string()));
    }
    if cfg.rom_kib < 16 || cfg.rom_kib % 16 != 0 {
        return Err(EmitError::InvalidRomSize(cfg.rom_kib));
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
) -> String {
    let rom_banks = cfg.rom_kib / 16;
    let runtime_includes = build_runtime_includes(runtime_s_files);
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
         {mirroring_define}\n\
         {raw_ciram_define}\n\
         \n\
         ; Pull in runtime + generated translation.\n\
         {runtime_includes}.include \"generated/translated.asm\"\n\
         \n\
         ; ── Data blobs ───────────────────────────────────────────────────\n\
         ; Each asset lives in its own dedicated bank, pinned to slot 2 so\n\
         ; the symbol value is a clean slot-2 logical address ($8000-$BFFF).\n\
         ; boot.s switches the right bank into slot 2 before reading.\n\
         .bank 12 slot 2\n\
         .org $0000\n\
         .section \"data_chr\" force\n\
         data_chr:\n\
         .incbin \"data/chr.4bpp\"\n\
         data_chr_end:\n\
         .ends\n\
         \n\
         .define data_chr_size {chr_size}\n\
         \n\
         .bank 13 slot 2\n\
         .org $0000\n\
         .section \"data_palette\" force\n\
         data_palette:\n\
         .incbin \"data/palette.cram\"\n\
         .ends\n",
        rom_banks = rom_banks,
        title = cfg.title,
        mirroring_define = mirroring_define,
        raw_ciram_define = raw_ciram_define,
        chr_size = assets.chr_4bpp.len(),
    );

    if has_nametable {
        out.push_str(
            "\n\
             .bank 14 slot 2\n\
             .org $0000\n\
             .section \"data_nametable\" force\n\
             data_nametable:\n\
             .incbin \"data/nametable.bin\"\n\
             .ends\n",
        );
    }

    if assets.prg_low.is_some() {
        out.push_str(
            "\n\
             .bank 15 slot 2\n\
             .org $0000\n\
             .section \"data_prg_low\" force\n\
             data_prg_low:\n\
             .incbin \"data/prg_low.bin\"\n\
             .ends\n",
        );
    }

    if assets.prg_high.is_some() {
        out.push_str(
            "\n\
             .bank 18 slot 2\n\
             .org $0000\n\
             .section \"data_prg_high\" force\n\
             data_prg_high:\n\
             .incbin \"data/prg_high.bin\"\n\
             .ends\n",
        );
    }

    if assets.chr_nes.is_some() {
        out.push_str(
            "\n\
             .bank 16 slot 2\n\
             .org $0000\n\
             .section \"data_chr_nes\" force\n\
             data_chr_nes:\n\
             .incbin \"data/chr.nes\"\n\
             .ends\n",
        );
    }

    if assets.chr_maps.is_some() {
        out.push_str(
            "\n\
             .bank 17 slot 2\n\
             .org $0000\n\
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
             .ends\n",
        );
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
    validate_config(cfg)?;

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
    let sms_asm = sms_asm_content(cfg, assets, assets.nametable.is_some(), &s_files);
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
            rom_kib: 32,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
        }
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

    // ── 2. emit_assets_only produces only generated/ and data/ ───────────────

    #[test]
    fn test_emit_assets_only_no_scaffold() {
        let out = unique_dir("sms_proj_assets");
        let build = minimal_build();
        let assets = ProjectAssets {
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
            rom_kib: 32,
            region: 0x4C,
            title: "TOOLONGTITLE", // 12 chars
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
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
            rom_kib: 17,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
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
            rom_kib: 64,
            region: 0x4C,
            title: "BANKS4",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        // 64 KiB / 16 = 4 banks
        assert!(
            sms_asm.contains("bankstotal 4"),
            "expected 'bankstotal 4' in sms.asm"
        );
        assert!(sms_asm.contains("banks 4"), "expected 'banks 4' in sms.asm");

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
            rom_kib: 8,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
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
            rom_kib: 32,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::None,
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define NES_MIRRORING_VERTICAL 1"));
        assert!(!sms_asm.contains(".define NES_MIRRORING_HORIZONTAL 1"));

        fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn test_sms_asm_contains_horizontal_mirroring_define() {
        let out = unique_dir("sms_proj_horizontal_mirroring");
        let build = minimal_build();
        let assets = minimal_assets();
        let cfg = ProjectConfig {
            rom_kib: 32,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Horizontal,
            raw_ciram_backend: RawCiramBackend::None,
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
            rom_kib: 64,
            region: 0x4C,
            title: "TEST",
            mirroring: NesMirroring::Vertical,
            raw_ciram_backend: RawCiramBackend::SramSlot2,
        };

        emit_project(&out, &build, &assets, &cfg, None).unwrap();

        let sms_asm = fs::read_to_string(out.join("sms.asm")).unwrap();
        assert!(sms_asm.contains(".define RAW_CIRAM_BACKEND_SRAM 1"));
        assert!(sms_asm.contains(".define RAW_CIRAM_SRAM_BASE $8000"));
        assert!(sms_asm.contains(".define RAW_CIRAM_SRAM_CTRL $08"));

        fs::remove_dir_all(&out).unwrap();
    }
}
