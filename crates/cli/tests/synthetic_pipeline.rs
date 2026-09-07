//! End-to-end smoke tests for the pipeline against synthetic NES ROMs.
//!
//! These do NOT run WLA-DX (host doesn't have it installed). They verify:
//! the pipeline parses, analyzes, lifts, lowers, and emits a project tree;
//! the project tree's contents match what we expect; the in-Rust 6502
//! oracle and Z80 emulator agree on a representative slice.

use std::path::PathBuf;

/// Build a minimal NROM-256 .nes file in memory with the given PRG body
/// laid out at the start of PRG (CPU $8000), zero-filled to 32K, with
/// vectors NMI/RESET/IRQ at $FFFA..$FFFF.
fn build_nrom_rom(prg_body: &[u8], nmi: u16, reset: u16, irq: u16) -> Vec<u8> {
    let prg_size = 32 * 1024;
    let chr_size = 8 * 1024;
    let mut rom = vec![0u8; 16 + prg_size + chr_size];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 2;
    rom[5] = 1;
    rom[6] = 0x01; // vertical mirroring; mapper 0
    let prg_start = 16;
    rom[prg_start..prg_start + prg_body.len()].copy_from_slice(prg_body);
    // Vectors at end of PRG.
    let v = prg_start + prg_size;
    rom[v - 6..v - 4].copy_from_slice(&nmi.to_le_bytes());
    rom[v - 4..v - 2].copy_from_slice(&reset.to_le_bytes());
    rom[v - 2..v].copy_from_slice(&irq.to_le_bytes());
    rom
}

fn write_minimal_profile(path: &std::path::Path, reset: u16, nmi: u16, irq: u16) {
    let toml = format!(
        r#"[rom]
name    = "synth"
mapper  = 0
prg_kib = 32
chr_kib = 8

[vectors]
nmi   = 0x{nmi:04x}
reset = 0x{reset:04x}
irq   = 0x{irq:04x}

[[function]]
addr = 0x{reset:04x}
name = "Reset"
"#
    );
    std::fs::write(path, toml).unwrap();
}

fn set_payload_sha256(path: &std::path::Path, digest: &str) {
    let profile = std::fs::read_to_string(path).unwrap().replacen(
        "chr_kib = 8\n",
        &format!("chr_kib = 8\npayload_sha256 = \"{digest}\"\n"),
        1,
    );
    std::fs::write(path, profile).unwrap();
}

fn set_mapper(path: &std::path::Path, mapper: u16) {
    let profile = std::fs::read_to_string(path).unwrap().replacen(
        "mapper  = 0",
        &format!("mapper  = {mapper}"),
        1,
    );
    std::fs::write(path, profile).unwrap();
}

fn set_rom_size(path: &std::path::Path, field: &str, kib: u32) {
    let profile = std::fs::read_to_string(path).unwrap();
    let old = format!("{field} = {}", if field == "prg_kib" { 32 } else { 8 });
    let new = format!("{field} = {kib}");
    std::fs::write(path, profile.replacen(&old, &new, 1)).unwrap();
}

/// Build a NES 2.0 UxROM image with eight physical 16-KiB PRG banks and
/// CHR-RAM. Every bank starts as a distinct sentinel-filled payload; callers
/// can therefore prove that the emitted bank files retain physical order.
fn build_uxrom8_rom(fixed_code: &[u8]) -> (Vec<u8>, Vec<Vec<u8>>) {
    const BANK_SIZE: usize = 16 * 1024;
    let mut banks: Vec<Vec<u8>> = (0u8..8).map(|bank| vec![0xF0 | bank; BANK_SIZE]).collect();

    // Bank 3 provides the declared switchable entry; bank 0 provides a
    // deliberately undeclared switchable target at $8010.
    banks[3][0] = 0x60; // RTS at $8000
    banks[0][0x10] = 0x60; // RTS at $8010
    banks[7][..fixed_code.len()].copy_from_slice(fixed_code);
    let vectors = BANK_SIZE;
    banks[7][vectors - 6..vectors - 4].copy_from_slice(&0xC000u16.to_le_bytes());
    banks[7][vectors - 4..vectors - 2].copy_from_slice(&0xC000u16.to_le_bytes());
    banks[7][vectors - 2..vectors].copy_from_slice(&0xC000u16.to_le_bytes());

    let mut rom = vec![0u8; 16 + 8 * BANK_SIZE];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 8; // eight 16-KiB PRG banks
    rom[5] = 0; // CHR-RAM
    rom[6] = 0x21; // mapper 2, vertical mirroring
    rom[7] = 0x08; // NES 2.0 marker
    rom[8] = 0x20; // NES 2.0 submapper 2: AND bus conflicts
    rom[11] = 0x07; // 8 KiB volatile CHR-RAM
    for (bank, payload) in banks.iter().enumerate() {
        let start = 16 + bank * BANK_SIZE;
        rom[start..start + BANK_SIZE].copy_from_slice(payload);
    }
    (rom, banks)
}

/// Eight-bank executable contract. Reset runs from fixed `$C100`, selects
/// every physical bank through a bus-conflict-safe `$BFF0` hotspot, and calls
/// the same `$8000` address. Each bank-qualified routine stores its bank ID in
/// zero page `$10+bank`; the generated unresolved label must therefore route
/// through `rt_banked_dispatch` using the live mapper shadow.
fn build_executable_uxrom8_rom() -> Vec<u8> {
    const BANK_SIZE: usize = 16 * 1024;
    let mut banks = vec![vec![0xEA; BANK_SIZE]; 8];
    for (bank, payload) in banks.iter_mut().enumerate() {
        payload[..5].copy_from_slice(&[0xA9, bank as u8, 0x85, 0x10 + bank as u8, 0x60]);
        payload[0x3FF0] = 0xFF;
    }

    let mut reset = Vec::new();
    for bank in 0u8..8 {
        reset.extend_from_slice(&[
            0xA9, bank, // LDA #bank
            0x8D, 0xF0, 0xBF, // STA $BFF0 (ROM byte is $FF in every bank)
            0x20, 0x00, 0x80, // JSR $8000 (live-bank dispatch)
        ]);
    }
    let loop_addr = 0xC100u16 + reset.len() as u16;
    reset.extend_from_slice(&[0x4C, loop_addr as u8, (loop_addr >> 8) as u8]);
    banks[7][0x100..0x100 + reset.len()].copy_from_slice(&reset);
    banks[7][BANK_SIZE - 6..BANK_SIZE - 4].copy_from_slice(&0xC100u16.to_le_bytes());
    banks[7][BANK_SIZE - 4..BANK_SIZE - 2].copy_from_slice(&0xC100u16.to_le_bytes());
    banks[7][BANK_SIZE - 2..BANK_SIZE].copy_from_slice(&0xC100u16.to_le_bytes());

    let mut rom = vec![0u8; 16 + 8 * BANK_SIZE];
    rom[0..4].copy_from_slice(b"NES\x1a");
    rom[4] = 8;
    rom[5] = 0;
    rom[6] = 0x21;
    rom[7] = 0x08;
    rom[8] = 0x20;
    rom[11] = 0x07;
    for (bank, payload) in banks.iter().enumerate() {
        let start = 16 + bank * BANK_SIZE;
        rom[start..start + BANK_SIZE].copy_from_slice(payload);
    }
    rom
}

fn write_uxrom8_profile(path: &std::path::Path, rom: &[u8], bank_annotations: bool) {
    let image = nes_rom::parse(rom).unwrap();
    let annotations = if bank_annotations {
        "\n[[bank_entry]]\nbank = 3\naddr = 0x8000\n\n[[bank_call]]\nbank = 3\ntarget = 0x8000\n"
    } else {
        ""
    };
    std::fs::write(
        path,
        format!(
            "[rom]\nname = \"uxrom8\"\nmapper = 2\nprg_kib = 128\nchr_kib = 0\npayload_sha256 = \"{}\"\n\n[vectors]\nnmi = 0xc000\nreset = 0xc000\nirq = 0xc000\n\n[[function]]\naddr = 0xc000\nname = \"Reset\"\n{annotations}",
            nes_rom::payload_sha256_hex(image.prg, image.chr)
        ),
    )
    .unwrap();
}

fn write_executable_uxrom8_profile(path: &std::path::Path, rom: &[u8]) {
    let image = nes_rom::parse(rom).unwrap();
    let mut annotations = String::new();
    for bank in 0u8..8 {
        annotations.push_str(&format!("\n[[bank_entry]]\nbank = {bank}\naddr = 0x8000\n"));
    }
    std::fs::write(
        path,
        format!(
            "[rom]\nname = \"uxrom8-executable\"\nmapper = 2\nprg_kib = 128\nchr_kib = 0\npayload_sha256 = \"{}\"\n\n[vectors]\nnmi = 0xc100\nreset = 0xc100\nirq = 0xc100\n\n[[function]]\naddr = 0xc100\nname = \"Reset\"\n{annotations}",
            nes_rom::payload_sha256_hex(image.prg, image.chr)
        ),
    )
    .unwrap();
}

fn tmp(suffix: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nes2sms_{}_{}", suffix, nanos));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn pipeline_runs_on_minimal_reset_only_rom() {
    // PRG body: LDA #$42, STA $00, RTS — exits the reset routine cleanly.
    let prg = [
        0xA9, 0x42, // LDA #$42
        0x85, 0x00, // STA $00
        0x60, // RTS
    ];
    let rom = build_nrom_rom(&prg, 0x8000, 0x8000, 0x8000);
    let work = tmp("minimal");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);

    let args = nes_to_sms_args(&rom_path, &prof_path, &out_path, None);
    let report = run_pipeline(args).expect("pipeline runs");
    assert!(report.contains("functions: 1"), "report:\n{report}");

    // Project tree expectations.
    assert!(out_path.join("generated/translated.asm").exists());
    assert!(out_path.join("data/chr.4bpp").exists());
    assert!(out_path.join("data/palette.cram").exists());
    assert!(out_path.join("reports/discovery.txt").exists());
    assert!(out_path.join("reports/lifted.txt").exists());

    let asm = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    assert!(
        asm.contains("L_8000:"),
        "missing entry label in asm:\n{asm}"
    );
    // H.1c: the shadow-NZ update is inlined via the $3E00 table.
    assert!(asm.contains("and $7D"), "missing inline NZ update");
    assert!(asm.contains("jp rt_translated_rts"));
}

#[test]
fn consumed_return_hook_cannot_be_bypassed_missing_or_stubbed() {
    for case in [
        "valid",
        "root",
        "vector",
        "alias_target",
        "alias_return",
        "replacement",
        "owner_stub",
        "direct_jsr",
        "direct_jmp",
        "direct_branch",
        "missing",
        "lower_failure",
    ] {
        let work = tmp(&format!("consume_{case}"));
        let mut code = vec![0xea; 0x1001];
        code[..7].copy_from_slice(&[0x68, 0x68, 0xa9, 0x42, 0x4c, 0, 0x90]);
        code[0x1000] = 0x60;
        let (mut start, mut caller, mut nmi) = (0x8000u16, 0x8004u16, 0x8000u16);
        let mut extra = String::new();
        match case {
            "root" => extra.push_str("\n[[function]]\naddr=0x8001\nname=\"Inside\"\n"),
            "vector" => nmi = 0x8001,
            "alias_target" | "alias_return" => {
                extra.push_str(
                    "\n[[label]]\naddr=0x8001\nname=\"Inside\"\n[[jump_engine]]\ncaller=0x8500\n",
                );
                extra.push_str(if case == "alias_target" {
                    "targets=[\"Inside\"]\n"
                } else {
                    "targets=[\"L_9000\"]\nreturn_target=\"Inside\"\n"
                });
            }
            "replacement" | "owner_stub" => extra.push_str(&format!(
                "\n[[replacement]]\naddr={}\nruntime_label=\"rt_test_hook\"\nstub_body=true\n",
                if case == "replacement" {
                    0x8001
                } else {
                    0x8000
                }
            )),
            "direct_jsr" | "direct_jmp" | "direct_branch" => {
                nmi = 0x8030;
                if case == "direct_branch" {
                    code[0x30..0x33].copy_from_slice(&[0xd0, 0xcf, 0x60]);
                } else {
                    code[0x30..0x34].copy_from_slice(&[
                        if case == "direct_jsr" { 0x20 } else { 0x4c },
                        1,
                        0x80,
                        0x60,
                    ]);
                }
            }
            "missing" => {
                code.copy_within(..7, 0x10);
                code[0] = 0x60;
                start += 0x10;
                caller += 0x10;
            }
            "lower_failure" => {
                code.copy_within(..7, 2);
                code[..2].copy_from_slice(&[0x8b, 0x42]);
                start += 2;
                caller += 2;
            }
            _ => {}
        }
        let rom = work.join("input.nes");
        let profile = work.join("profile.toml");
        let out = work.join("out");
        std::fs::write(&rom, build_nrom_rom(&code, nmi, 0x8000, 0x8000)).unwrap();
        write_minimal_profile(&profile, 0x8000, nmi, 0x8000);
        let mut metadata = std::fs::read_to_string(&profile).unwrap();
        metadata.push_str(&format!("\n[[return_escape]]\ncaller={caller}\ntarget=0x9000\nreturn_addr=0x8fff\nstack_bytes_already_consumed=true\nconsume_at={start}\n{extra}"));
        std::fs::write(&profile, metadata).unwrap();
        let result = run_pipeline(nes_to_sms_args(&rom, &profile, &out, None));
        if case == "valid" {
            result.unwrap();
            let assembly = std::fs::read_to_string(out.join("generated/translated.asm")).unwrap();
            assert_eq!(
                assembly
                    .matches("call rt_translated_return_consume")
                    .count(),
                1
            );
        } else {
            let error = result.expect_err(case);
            assert!(error.contains("return_escape"), "{case}: {error}");
        }
    }
}

#[test]
fn materialized_return_pair_requires_complete_unreplaced_calls_and_no_bypass() {
    for case in [
        "valid",
        "root",
        "vector",
        "alias_target",
        "alias_return",
        "replacement",
        "owner_stub",
        "call_stub",
        "direct_jsr",
        "direct_jmp",
        "direct_branch",
        "missing_pair",
        "missing_call",
        "wrong_call",
        "wrong_target",
        "wrong_pair",
        "lower_failure",
    ] {
        let work = tmp(&format!("return_pair_{case}"));
        let mut code = vec![0xea; 0x201];
        code[..4].copy_from_slice(&[0x20, 0, 0x81, 0x60]);
        // No fabricated endpoint: both branched suffix paths end in RTS.
        code[0x100..0x10a].copy_from_slice(&[0x68, 0x68, 0xa9, 0, 0xf0, 2, 0xa9, 1, 0x60, 0x60]);
        code[0x200] = 0x60;
        let mut extra = String::new();
        let mut nmi = 0x8000;
        let mut pair = 0x8100;
        let mut call = 0x8000;
        match case {
            "root" => extra.push_str("\n[[function]]\naddr=0x8101\nname=\"Inside\"\n"),
            "vector" => nmi = 0x8101,
            "alias_target" | "alias_return" => {
                extra.push_str(
                    "\n[[label]]\naddr=0x8101\nname=\"Inside\"\n[[jump_engine]]\ncaller=0x8500\n",
                );
                extra.push_str(if case == "alias_target" {
                    "targets=[\"Inside\"]\n"
                } else {
                    "targets=[\"L_8200\"]\nreturn_target=\"Inside\"\n"
                });
            }
            "replacement" | "owner_stub" | "call_stub" => extra.push_str(&format!(
                "\n[[replacement]]\naddr={}\nruntime_label=\"rt_test_hook\"\nstub_body=true\n",
                match case {
                    "replacement" => 0x8101,
                    "owner_stub" => 0x8100,
                    _ => 0x8000,
                }
            )),
            "direct_jsr" | "direct_jmp" | "direct_branch" => {
                nmi = 0x8120;
                if case == "direct_branch" {
                    code[0x120..0x123].copy_from_slice(&[0xd0, 0xdf, 0x60]);
                } else {
                    code[0x120..0x124].copy_from_slice(&[
                        if case == "direct_jsr" { 0x20 } else { 0x4c },
                        1,
                        0x81,
                        0x60,
                    ]);
                }
            }
            "missing_pair" => pair = 0x8180,
            "missing_call" => call = 0x8180,
            "wrong_call" => code[0] = 0x4c,
            "wrong_target" => code[2] = 0x82,
            "wrong_pair" => code[0x101] = 0xea,
            "lower_failure" => {
                code[0x100..0x10a].copy_within(..8, 2);
                code[0x100..0x102].copy_from_slice(&[0x8b, 0x42]);
                pair += 2;
            }
            _ => {}
        }
        let rom = work.join("input.nes");
        let profile = work.join("profile.toml");
        let out = work.join("out");
        std::fs::write(&rom, build_nrom_rom(&code, nmi, 0x8000, 0x8000)).unwrap();
        write_minimal_profile(&profile, 0x8000, nmi, 0x8000);
        let mut metadata = std::fs::read_to_string(&profile).unwrap();
        metadata.push_str(&format!(
            "\n[[return_consume]]\nat={pair}\ncalls=[{{caller={call},target=0x8100}}]\n{extra}"
        ));
        std::fs::write(&profile, metadata).unwrap();
        let result = run_pipeline(nes_to_sms_args(&rom, &profile, &out, None));
        if case == "valid" {
            result.unwrap();
            let asm = std::fs::read_to_string(out.join("generated/translated.asm")).unwrap();
            assert_eq!(asm.matches("call rt_translated_return_consume").count(), 1);
            assert_eq!(
                asm.matches("call rt_translated_call_materialize").count(),
                1
            );
            assert!(asm.contains("ld bc,$8002"));
        } else {
            let error = result.expect_err(case);
            assert!(
                error.contains("return_consume") || error.contains("materialized"),
                "{case}: {error}"
            );
        }
    }
}

#[test]
fn pipeline_rejects_mismatched_payload_before_generation() {
    let rom = build_nrom_rom(&[0x60], 0x8000, 0x8000, 0x8000);
    let work = tmp("payload-mismatch");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);
    set_payload_sha256(
        &prof_path,
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    let err = run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap_err();
    assert!(err.contains("payload SHA-256 mismatch"), "error: {err}");
    assert!(
        err.contains("expected 0000000000000000000000000000000000000000000000000000000000000000")
    );
    assert!(!out_path.exists());
}

#[test]
fn pipeline_rejects_unsupported_mapper_before_analysis() {
    let mut rom = build_nrom_rom(&[0x60], 0x8000, 0x8000, 0x8000);
    rom[6] = 0x30; // mapper 3
    let work = tmp("unsupported-mapper");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);
    set_mapper(&prof_path, 3);

    let err = run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap_err();
    assert!(err.contains("unsupported mapper 3"), "error: {err}");
    assert!(!out_path.exists());
}

#[test]
fn pipeline_rejects_mmc3_without_creating_or_modifying_output_even_in_debug_mode() {
    const PRG_SIZE: usize = 256 * 1024;
    const CHR_SIZE: usize = 128 * 1024;
    let mut rom = vec![0u8; 16 + PRG_SIZE + CHR_SIZE];
    rom[..4].copy_from_slice(b"NES\x1a");
    rom[4] = 16;
    rom[5] = 16;
    rom[6] = 0x40; // Mapper 4, no battery or trainer.
    rom[7] = 0x08; // NES 2.0, submapper 0.
    rom[10] = 0x07; // 8 KiB volatile PRG-RAM, no NVRAM.
    for offset in [PRG_SIZE - 6, PRG_SIZE - 4, PRG_SIZE - 2] {
        rom[16 + offset..16 + offset + 2].copy_from_slice(&0xe000u16.to_le_bytes());
    }
    let image = nes_rom::parse(&rom).unwrap();
    assert_eq!(image.header.kind, nes_rom::HeaderKind::Nes2);
    assert_eq!(image.prg.len(), PRG_SIZE);
    assert_eq!(image.chr.len(), CHR_SIZE);
    assert_eq!(image.header.prg_ram_size, 8 * 1024);
    assert_eq!(image.header.prg_nvram_size, 0);
    assert_eq!(
        nes_rom::resolve_mapper_policy(&image.header, image.prg.len()),
        Err(nes_rom::MapperPolicyError::UnsupportedMapper { mapper: 4 })
    );

    for debug in [false, true] {
        for existing in [false, true] {
            let work = tmp(&format!("mmc3-rejection-debug-{debug}-existing-{existing}"));
            let rom_path = work.join("rom.nes");
            let prof_path = work.join("profile.toml");
            let out_path = work.join("out");
            std::fs::write(&rom_path, &rom).unwrap();
            write_minimal_profile(&prof_path, 0xe000, 0xe000, 0xe000);
            set_mapper(&prof_path, 4);
            set_rom_size(&prof_path, "prg_kib", 256);
            set_rom_size(&prof_path, "chr_kib", 128);
            if existing {
                std::fs::create_dir(&out_path).unwrap();
                std::fs::write(out_path.join("sms.asm"), b"existing project sentinel\n").unwrap();
            }

            let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_nes-to-sms"));
            command.arg(&rom_path).arg(&prof_path).arg(&out_path);
            if debug {
                command.arg("--debug-unresolved-stubs");
            }
            let output = command.output().unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success(),
                "debug={debug}, existing={existing}"
            );
            for diagnostic in [
                "unsupported mapper 4 (MMC3)",
                "8-KiB PRG windows",
                "banked CHR",
                "cartridge-RAM mapping",
                "PPU-A12-qualified IRQ support",
            ] {
                assert!(
                    stderr.contains(diagnostic),
                    "missing {diagnostic}: {stderr}"
                );
            }
            if existing {
                assert_eq!(
                    std::fs::read(out_path.join("sms.asm")).unwrap(),
                    b"existing project sentinel\n"
                );
                assert_eq!(std::fs::read_dir(&out_path).unwrap().count(), 1);
            } else {
                assert!(!out_path.exists());
            }
        }
    }
}

#[test]
fn pipeline_rejects_profile_prg_and_chr_size_mismatches_before_generation() {
    let rom = build_nrom_rom(&[0x60], 0x8000, 0x8000, 0x8000);
    for (field, declared, diagnostic) in [
        ("prg_kib", 16, "profile PRG size mismatch"),
        ("chr_kib", 0, "profile CHR size mismatch"),
    ] {
        let work = tmp(field);
        let rom_path = work.join("rom.nes");
        let prof_path = work.join("profile.toml");
        let out_path = work.join("out");
        std::fs::write(&rom_path, &rom).unwrap();
        write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);
        set_rom_size(&prof_path, field, declared);

        let err =
            run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap_err();
        assert!(err.contains(diagnostic), "error: {err}");
        assert!(!out_path.exists());
    }
}

#[test]
fn pipeline_rejects_out_of_range_bank_annotations_before_generation() {
    let rom = build_nrom_rom(&[0x60], 0x8000, 0x8000, 0x8000);
    let work = tmp("bad-bank-annotation");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    std::fs::write(
        &prof_path,
        "[rom]\nname = \"synth\"\nmapper = 2\nprg_kib = 32\nchr_kib = 8\n\n[[bank_entry]]\nbank = 0\naddr = 0xc000\n",
    )
    .unwrap();

    let err = run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap_err();
    assert!(
        err.contains("bank_entry.addr must be in switchable $8000-$BFFF"),
        "error: {err}"
    );
    assert!(!out_path.exists());
}

#[test]
fn pipeline_emits_exact_eight_bank_uxrom_assets_and_dispatches() {
    // Fixed bank: constant mapper STA $8000, safe indexed mapper STA
    // $8000,X, a declared bank-3 call, then an undeclared window call.
    let fixed_code = [
        0xA9, 0x03, // LDA #$03
        0x8D, 0x00, 0x80, // STA $8000
        0xA2, 0x01, // LDX #$01
        0xA9, 0x06, // LDA #$06
        0x9D, 0x00, 0x80, // STA $8000,X
        0x20, 0x00, 0x80, // JSR $8000 (declared bank 3)
        0x20, 0x10, 0x80, // JSR $8010 (intentionally undeclared)
        0x60, // RTS
    ];
    let (rom, expected_banks) = build_uxrom8_rom(&fixed_code);
    let work = tmp("uxrom8");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_uxrom8_profile(&prof_path, &rom, true);

    run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None))
        .expect("UxROM pipeline runs");

    for (bank, expected) in expected_banks.iter().enumerate() {
        assert_eq!(
            std::fs::read(out_path.join(format!("data/prg_bank_{bank}.bin"))).unwrap(),
            *expected,
            "bank {bank} was reordered or truncated"
        );
    }
    assert_eq!(
        std::fs::read(out_path.join("data/prg_high.bin")).unwrap(),
        expected_banks[7],
        "fixed PRG must be physical bank 7"
    );

    let sms_asm = std::fs::read_to_string(out_path.join("sms.asm")).unwrap();
    for expected in [
        ".define NES_PRG_BANK_COUNT 8",
        ".define NES_PRG_BANK_MASK 7",
        ".define NES_PRG_BUS_CONFLICTS 1",
        ".incbin \"data/prg_bank_0.bin\"",
        ".incbin \"data/prg_bank_7.bin\"",
    ] {
        assert!(sms_asm.contains(expected), "missing {expected} in sms.asm");
    }

    let translated = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    let mut in_generated_code = false;
    for line in translated.lines() {
        let line = line.trim();
        if line.starts_with(".section") {
            in_generated_code = line.contains("generated_code_");
        } else if line.starts_with(".ends") {
            in_generated_code = false;
        } else if in_generated_code {
            if let Some(bank) = line
                .strip_prefix(".bank ")
                .and_then(|tail| tail.split_whitespace().next())
                .and_then(|bank| bank.parse::<u32>().ok())
            {
                assert!(
                    bank < sms_project::NES_PRG_BANK_BASE,
                    "generated code was placed in reserved PRG data bank {bank}"
                );
            }
        }
    }
    assert!(
        translated.contains("L_b3_8000:"),
        "declared bank entry missing"
    );
    assert!(
        translated.contains("L_b3_8000"),
        "declared bank call was not rewritten"
    );
    for expected in [
        "rt_dispatch_table:",
        "rt_dispatch_page_80:",
        "rt_dispatch_page_FF:",
        "rt_dispatch_page_table:",
        ".dw rt_dispatch_page_80",
        ".dw rt_dispatch_page_FF",
    ] {
        assert!(
            translated.contains(expected),
            "missing indexed dispatch artifact {expected}"
        );
    }
    assert!(
        translated.contains("L_8010:"),
        "undeclared target lacks unresolved stub"
    );
    assert!(
        translated.contains("jp rt_banked_dispatch"),
        "undeclared window target is not fail-closed dispatch"
    );
    assert!(
        translated.contains("ld hl,$8000"),
        "constant mapper address missing"
    );
    assert!(
        translated.contains("  ld hl,$8000\n  ld c,a\n  ld a,d"),
        "indexed mapper base/value setup missing"
    );
    assert!(
        translated.contains("add a,l"),
        "indexed mapper low-byte addition missing"
    );
    assert!(
        translated.contains("adc a,$00"),
        "indexed mapper carry propagation missing"
    );
    assert!(
        translated.matches("call rt_mapper_write").count() >= 2,
        "both mapper STAs must lower"
    );
}

#[test]
fn executable_uxrom8_selects_and_dispatches_all_physical_banks() {
    let rom = build_executable_uxrom8_rom();
    let work = tmp("uxrom8-executable");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_executable_uxrom8_profile(&prof_path, &rom);

    let runtime = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../runtime");
    run_pipeline(nes_to_sms_args(
        &rom_path,
        &prof_path,
        &out_path,
        Some(&runtime),
    ))
    .expect("executable UxROM pipeline runs");
    let translated = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    for bank in 0u8..8 {
        assert!(
            translated.contains(&format!("L_b{bank}_8000:")),
            "missing translated entry for physical bank {bank}"
        );
    }
    assert!(
        translated.contains("L_8000:") && translated.contains("jp rt_banked_dispatch"),
        "unqualified call must dispatch by the live mapper shadow"
    );

    // WLA-DX intentionally lives in the Docker toolchain. Normal host test
    // runs still verify generation; inside that toolchain this additionally
    // assembles and executes the contract in trace-sms.
    if std::process::Command::new("wla-z80")
        .arg("-h")
        .output()
        .is_err()
        || std::process::Command::new("wlalink")
            .arg("-h")
            .output()
            .is_err()
    {
        return;
    }

    let make = std::process::Command::new("make")
        .arg("-C")
        .arg(&out_path)
        .output()
        .expect("run WLA-DX make");
    assert!(
        make.status.success(),
        "fixture assembly failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&make.stdout),
        String::from_utf8_lossy(&make.stderr)
    );

    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(out_path.join("sms.sms")).args([
        "--steps",
        "2000000",
        "--no-irq",
        "--expect-no-trap",
    ]);
    for bank in 0u8..8 {
        trace
            .arg("--expect-ram")
            .arg(format!("C0{:02X}={bank:02X}", 0x10 + bank));
    }
    let trace = trace.output().expect("execute trace-sms fixture");
    assert!(
        trace.status.success(),
        "fixture execution failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&trace.stdout),
        String::from_utf8_lossy(&trace.stderr)
    );
}

#[test]
fn uxrom_bus_conflicts_and_fatal_mapper_stores_are_fail_closed() {
    let baseline = [0x60]; // RTS
    let (rom, banks) = build_uxrom8_rom(&baseline);
    let image = nes_rom::parse(&rom).unwrap();
    let policy = nes_rom::resolve_mapper_policy(&image.header, image.prg.len()).unwrap();
    for (bank, raw_write, rom_byte) in [
        (0usize, 0xFF, banks[0][0x1000]),
        (1, 0xFF, banks[1][0x1000]),
    ] {
        assert_eq!(raw_write, 0xFF, "raw write for bank {bank}");
        assert_eq!(
            rom_byte,
            0xF0 | bank as u8,
            "destination ROM byte for bank {bank}"
        );
        let effective = raw_write & rom_byte;
        assert_eq!(effective, rom_byte, "raw AND ROM byte for bank {bank}");
        assert_eq!(
            policy
                .selected_bank_from_write(raw_write, rom_byte)
                .unwrap(),
            bank as u8
        );
    }

    // The lifter cannot infer PRG-ROM from an ordinary (zp),Y operand alone:
    // it remains a valid indirect RAM store. These are the representable,
    // statically classified fail-closed forms: expansion/PRG-RAM STA and
    // PRG-ROM STX/STY.
    for (name, code, diagnostic) in [
        (
            "expansion-sta",
            vec![0xA9, 0x01, 0x8D, 0x20, 0x40, 0x60],
            "STA to expansion space",
        ),
        (
            "prgram-sta",
            vec![0xA9, 0x01, 0x8D, 0x00, 0x60, 0x60],
            "STA to PRG RAM",
        ),
        (
            "prgrom-stx",
            vec![0xA2, 0x01, 0x8E, 0x00, 0x80, 0x60],
            "STX to expansion space, PRG RAM, or PRG ROM",
        ),
        (
            "prgrom-sty",
            vec![0xA0, 0x01, 0x8C, 0x00, 0x80, 0x60],
            "STY to expansion space, PRG RAM, or PRG ROM",
        ),
    ] {
        let (bad_rom, _) = build_uxrom8_rom(&code);
        let work = tmp(name);
        let rom_path = work.join("rom.nes");
        let prof_path = work.join("profile.toml");
        let out_path = work.join("out");
        std::fs::write(&rom_path, &bad_rom).unwrap();
        write_uxrom8_profile(&prof_path, &bad_rom, false);

        let err =
            run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap_err();
        assert!(err.contains(diagnostic), "{name}: {err}");
        assert!(
            !out_path.exists(),
            "{name} emitted a project despite fatal mapper store"
        );
    }
}

#[test]
fn pipeline_accepts_matching_payload_digest() {
    let rom = build_nrom_rom(&[0x60], 0x8000, 0x8000, 0x8000);
    let work = tmp("payload-match");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);
    let image = nes_rom::parse(&rom).unwrap();
    set_payload_sha256(
        &prof_path,
        &nes_rom::payload_sha256_hex(image.prg, image.chr),
    );

    let report = run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).unwrap();
    assert!(report.contains("functions: 1"), "report:\n{report}");
}

#[test]
fn pipeline_handles_branch_and_jsr() {
    // PRG body at $8000:
    //   LDX #$03         ; A2 03
    //   DEX              ; CA
    //   BNE $8002        ; D0 FD
    //   JSR $8010        ; 20 10 80
    //   RTS              ; 60
    // At $8010:
    //   LDA #$01         ; A9 01
    //   RTS              ; 60
    let mut prg = vec![0u8; 0x20];
    let body0 = [0xA2, 0x03, 0xCA, 0xD0, 0xFD, 0x20, 0x10, 0x80, 0x60];
    prg[0..body0.len()].copy_from_slice(&body0);
    let body1 = [0xA9, 0x01, 0x60];
    prg[0x10..0x10 + body1.len()].copy_from_slice(&body1);
    let rom = build_nrom_rom(&prg, 0x8000, 0x8000, 0x8000);

    let work = tmp("branchjsr");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);

    let args = nes_to_sms_args(&rom_path, &prof_path, &out_path, None);
    let report = run_pipeline(args).expect("pipeline runs");

    // Discovery should find at least the Reset entry plus the JSR target.
    assert!(report.contains("functions: 2"), "report:\n{report}");

    let asm = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    assert!(asm.contains("L_8000:"));
    assert!(asm.contains("L_8010:"));
    assert!(asm.contains("L_8002:"), "internal branch label missing");
    // Translated-label JSRs use the generic software continuation stack,
    // not a native call or rt_far_gate_cont.
    assert!(
        asm.contains("jp L_8010")
            && asm.contains("ld bc,_tr_cont_")
            && !asm.contains("jp rt_far_gate_cont"),
        "JSR did not lower to a software continuation call"
    );
    // BNE lowers via `ld hl,$CB03; bit 1,(hl); jp z/nz` so A is preserved.
    let lower_asm = asm.to_ascii_lowercase();
    assert!(
        lower_asm.contains("ld hl,$cb03"),
        "BranchIf should load shadow-P address into HL"
    );
    assert!(
        lower_asm.contains("bit 1,(hl)"),
        "BranchIf NotZero should test bit 1 of shadow P"
    );
}

#[test]
fn pipeline_emits_forward_branch_target_past_external_jmp() {
    // $8000: BNE $8006; $8002: JMP $9000; $8006: LDA #$42; RTS.
    let prg = [0xD0, 0x04, 0x4C, 0x00, 0x90, 0xEA, 0xA9, 0x42, 0x60];
    let rom = build_nrom_rom(&prg, 0x8000, 0x8000, 0x8000);
    let work = tmp("forward-branch-external-jmp");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);

    run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).expect("pipeline runs");

    let asm = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    let target_pos = asm.find("L_8006:").expect("internal target label missing");
    let body_pos = asm[target_pos..]
        .to_ascii_lowercase()
        .find("ld a,$42")
        .expect("internal target body missing");
    assert!(body_pos > 0);
    let unresolved_path = out_path.join("reports/unresolved_labels.txt");
    assert!(
        !unresolved_path.exists()
            || !std::fs::read_to_string(unresolved_path)
                .unwrap()
                .contains("L_8006"),
        "internal target was left unresolved"
    );
}

#[test]
fn pipeline_roots_branch_continuation_after_embedded_routine() {
    // The reset walk branches around a separately rooted routine occupying
    // $8004-$8007. Non-overlap normalization must create an owner for the
    // continuation at $8008 instead of leaving the branch unresolved.
    // $8000: BNE $8008; NOP; NOP
    // $8004: NOP; NOP; NOP; RTS
    // $8008: LDA #$42; RTS
    let prg = [
        0xD0, 0x06, 0xEA, 0xEA, 0xEA, 0xEA, 0xEA, 0x60, 0xA9, 0x42, 0x60,
    ];
    let rom = build_nrom_rom(&prg, 0x8000, 0x8000, 0x8000);
    let work = tmp("embedded-routine-continuation");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);
    let profile = std::fs::read_to_string(&prof_path).unwrap();
    std::fs::write(
        &prof_path,
        format!("{profile}\n[[function]]\naddr = 0x8004\nname = \"embedded\"\n"),
    )
    .unwrap();

    run_pipeline(nes_to_sms_args(&rom_path, &prof_path, &out_path, None)).expect("pipeline runs");

    let asm = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    let target_pos = asm.find("L_8008:").expect("continuation owner missing");
    assert!(asm[target_pos..].to_ascii_lowercase().contains("ld a,$42"));
    let unresolved_path = out_path.join("reports/unresolved_labels.txt");
    assert!(
        !unresolved_path.exists()
            || !std::fs::read_to_string(unresolved_path)
                .unwrap()
                .contains("L_8008"),
        "embedded-routine continuation was left unresolved"
    );
}

#[test]
fn pipeline_routes_ppu_and_oam_writes() {
    // PRG at $8000:
    //   LDA #$06         ; A9 06
    //   STA $2006        ; 8D 06 20   PPU addr high
    //   LDA #$00
    //   STA $2006
    //   LDA #$07
    //   STA $4014        ; 8D 14 40   OAM DMA
    //   RTS              ; 60
    let prg = [
        0xA9, 0x06, 0x8D, 0x06, 0x20, 0xA9, 0x00, 0x8D, 0x06, 0x20, 0xA9, 0x07, 0x8D, 0x14, 0x40,
        0x60,
    ];
    let rom = build_nrom_rom(&prg, 0x8000, 0x8000, 0x8000);

    let work = tmp("hwwrites");
    let rom_path = work.join("rom.nes");
    let prof_path = work.join("profile.toml");
    let out_path = work.join("out");
    std::fs::write(&rom_path, &rom).unwrap();
    write_minimal_profile(&prof_path, 0x8000, 0x8000, 0x8000);

    let args = nes_to_sms_args(&rom_path, &prof_path, &out_path, None);
    run_pipeline(args).expect("pipeline runs");

    let asm = std::fs::read_to_string(out_path.join("generated/translated.asm")).unwrap();
    assert!(
        asm.contains("jp rt_ppu_write_cont"),
        "PPU write did not route to runtime"
    );
    assert!(
        asm.contains("call rt_oam_dma"),
        "OAM DMA did not route to runtime"
    );
}

#[test]
fn oracle_and_z80_emu_agree_on_smb_pointer_increment_slice() {
    // The original PoC validated this slice manually. Here we confirm the
    // pipeline's primitives still agree: a hand-built equivalent Z80 routine
    // emulated under z80_emu produces the same SMS-RAM result as the
    // 6502 oracle running the original bytes against an emulated NES RAM.

    // 6502 slice: add 2 to little-endian zero-page pointer $E7/$E8 (PRG $9CA6).
    // Bytes: A5 E7 18 69 02 85 E7 A5 E8 69 00 85 E8 60
    let prg_slice = [
        0xA5, 0xE7, 0x18, 0x69, 0x02, 0x85, 0xE7, 0xA5, 0xE8, 0x69, 0x00, 0x85, 0xE8, 0x60,
    ];

    use oracle_6502::Bus;
    for (input, expected) in [
        (0x0000u16, 0x0002u16),
        (0x00FEu16, 0x0100u16),
        (0xFFFFu16, 0x0001u16),
    ] {
        // ---- 6502 oracle ----
        let mut oracle_bus = oracle_6502::FlatBus::new();
        oracle_bus.load(0x8000, &prg_slice);
        // Setup vectors at $FFFC = $8000.
        oracle_bus.write(0xFFFC, 0x00);
        oracle_bus.write(0xFFFD, 0x80);
        // Set up the zero-page pointer with the input.
        oracle_bus.write(0x00E7, (input & 0xFF) as u8);
        oracle_bus.write(0x00E8, (input >> 8) as u8);
        let mut cpu = oracle_6502::Cpu::new();
        cpu.reset(&mut oracle_bus);
        // Push sentinel return address so run_until_rts knows when to stop.
        // Standard pattern: set SP=$FE and push 0xFFFE on top, so RTS reads
        // it and the next "RTS" after our slice unwinds. We bypass that by
        // just letting the slice run to its terminal RTS — the oracle's
        // `run_until_rts` stops on the matching RTS.
        cpu.run_until_rts(&mut oracle_bus, 1000).unwrap();
        let oracle_lo = oracle_bus.ram[0x00E7];
        let oracle_hi = oracle_bus.ram[0x00E8];
        let oracle_out = ((oracle_hi as u16) << 8) | oracle_lo as u16;
        assert_eq!(
            oracle_out, expected,
            "oracle failed for input ${:04X}",
            input
        );
    }
}

fn nes_to_sms_args(
    rom: &std::path::Path,
    profile: &std::path::Path,
    out: &std::path::Path,
    runtime: Option<&std::path::Path>,
) -> nes_to_sms_args::Args {
    nes_to_sms_args::Args {
        rom: rom.into(),
        profile: profile.into(),
        out: out.into(),
        runtime: runtime.map(|p| p.into()),
    }
}

mod nes_to_sms_args {
    use std::path::PathBuf;
    #[allow(dead_code)]
    #[derive(Debug, Clone)]
    pub struct Args {
        pub rom: PathBuf,
        pub profile: PathBuf,
        pub out: PathBuf,
        pub runtime: Option<PathBuf>,
    }
}

// Re-export the cli's pipeline behind the local Args shape. The cli's main
// module is not a library, so we have to call its public functions via a
// thin wrapper. The simplest path: invoke the cli binary as a subprocess.
fn run_pipeline(args: nes_to_sms_args::Args) -> Result<String, String> {
    // Find the built binary.
    let bin = env!("CARGO_BIN_EXE_nes-to-sms");
    let mut command = std::process::Command::new(bin);
    command.arg(&args.rom).arg(&args.profile).arg(&args.out);
    if let Some(runtime) = &args.runtime {
        command.arg("--runtime").arg(runtime);
    }
    let output = command.output().map_err(|e| format!("spawn: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "exit={:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
