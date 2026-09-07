//! Executable first MMC3 banking contract; this is not a rendering fixture.

use nes_rom::mmc3::{Mmc3, Mmc3Revision};
use std::path::PathBuf;

fn fixture() -> (Vec<u8>, String, Vec<(u16, u8)>) {
    let mut prg = vec![0xea; 0x10000];
    for bank in 0u8..8 {
        let page = &mut prg[usize::from(bank) * 0x2000..][..0x2000];
        // A cross-window call followed by a return exercises both dispatch
        // and preservation of the translated caller's bank/continuation.
        page[..8].copy_from_slice(&[0x20, 0x20, 0xa0, 0x85, 0x40 + bank, 0xa9, bank, 0x60]);
        page[0x10] = 0x80 + bank;
        page[0x20..0x23].copy_from_slice(&[0xa9, bank, 0x60]);
        page[0x1fff] = 0xc0 + bank;
    }
    let mut code = vec![0x78]; // SEI; no timing/IRQ claim in this fixture.
    let mut expected = Vec::new();
    fn write(code: &mut Vec<u8>, addr: u16, value: u8) {
        code.extend_from_slice(&[0xa9, value, 0x8d, addr as u8, (addr >> 8) as u8]);
    }
    fn call(code: &mut Vec<u8>, expected: &mut Vec<(u16, u8)>, target: u16, dest: u8, bank: u8) {
        code.extend_from_slice(&[0x20, target as u8, (target >> 8) as u8, 0x85, dest]);
        expected.push((0xc000 + u16::from(dest), bank));
    }
    write(&mut code, 0x8000, 6);
    write(&mut code, 0x8001, 0);
    write(&mut code, 0x8000, 7);
    write(&mut code, 0x8001, 1);
    for (i, bank) in [0, 1, 6, 7].into_iter().enumerate() {
        let addr = 0x8000 + (i as u16) * 0x2000;
        call(
            &mut code,
            &mut expected,
            if i == 3 { addr + 0x20 } else { addr },
            0x10 + i as u8,
            bank,
        );
    }
    // Every physical page can occupy R6, including the last two pages.
    for bank in 0u8..8 {
        write(&mut code, 0x8000, 6);
        write(&mut code, 0x8001, bank);
        call(&mut code, &mut expected, 0x8000, 0x20 + bank, bank);
    }
    write(&mut code, 0x8000, 0x46);
    write(&mut code, 0x8001, 2);
    write(&mut code, 0x8000, 0x47);
    write(&mut code, 0x8001, 3);
    for (i, bank) in [6, 3, 2, 7].into_iter().enumerate() {
        let addr = 0x8000 + (i as u16) * 0x2000;
        call(
            &mut code,
            &mut expected,
            if i == 3 { addr + 0x20 } else { addr },
            0x14 + i as u8,
            bank,
        );
        code.extend_from_slice(&[0xad, 0x10, (addr >> 8) as u8, 0x85, 0x34 + i as u8]);
        expected.push((0xc034 + i as u16, 0x80 + bank));
    }
    // Indexed read straddles $9FFF/$A000, whose pages differ in mode 1.
    code.extend_from_slice(&[
        0xa2, 0, 0xbd, 0xff, 0x9f, 0x85, 0x38, 0xe8, 0xbd, 0xff, 0x9f, 0x85, 0x39,
    ]);
    expected.extend([(0xc038, 0xc6), (0xc039, 0x20)]);
    // Full CPU-address wrap leaves PRG space and must use the NES RAM mirror.
    code.extend_from_slice(&[0xa9, 0x5a, 0x85, 0, 0xbd, 0xff, 0xff, 0x85, 0x3a]);
    expected.push((0xc03a, 0x5a));
    // Decode an aliased selector/data pair and mask an oversized R6 value.
    write(&mut code, 0x9ffe, 0x46);
    write(&mut code, 0x9fff, 0xff);
    call(&mut code, &mut expected, 0xc000, 0x28, 7);
    write(&mut code, 0x003f, 0xa5);
    expected.push((0xc03f, 0xa5));
    let done = 0xe100 + code.len() as u16;
    code.extend_from_slice(&[0x4c, done as u8, (done >> 8) as u8]);
    prg[0xe100..0xe100 + code.len()].copy_from_slice(&code);
    for offset in [0xfffa, 0xfffc, 0xfffe] {
        prg[offset..offset + 2].copy_from_slice(&0xe100u16.to_le_bytes());
    }
    let mut rom = vec![0; 16];
    rom[..4].copy_from_slice(b"NES\x1a");
    rom[4] = 4;
    rom[5] = 1;
    rom[6] = 0x40;
    rom[7] = 0x08;
    rom.extend(prg);
    rom.extend(vec![0; 0x2000]);
    let mut profile = "[rom]\nname='mmc3-banking-fixture'\nmapper=4\nprg_kib=64\nchr_kib=8\n[vectors]\nnmi=0xe100\nreset=0xe100\nirq=0xe100\n[translation]\nruntime_defines=['MMC3_BANKING_EXPERIMENT']\n".to_owned();
    for bank in 0..8 {
        for addr in [0x8000, 0xa000, 0xa020, 0xc000] {
            profile.push_str(&format!("[[bank_entry]]\nbank={bank}\naddr={addr}\n"));
        }
    }
    (rom, profile, expected)
}

struct ReferenceBus<'a> {
    prg: &'a [u8],
    mapper: Mmc3,
    ram: [u8; 0x800],
}

impl oracle_6502::Bus for ReferenceBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.ram[usize::from(addr & 0x7ff)]
        } else {
            self.mapper
                .cpu_to_prg_offset(addr)
                .map(|offset| self.prg[offset])
                .unwrap_or(0)
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        if addr < 0x2000 {
            self.ram[usize::from(addr & 0x7ff)] = value;
        } else {
            self.mapper.write_register(addr, value);
        }
    }
}

#[test]
fn mmc3_fixture_original_6502_executes_both_modes_and_cross_window_calls() {
    let (rom, _, expected) = fixture();
    let ram = reference_ram(&rom);
    for (addr, value) in expected {
        assert_eq!(ram[usize::from(addr & 0x7ff)], value, "${addr:04X}");
    }
}

fn reference_ram(rom: &[u8]) -> [u8; 0x800] {
    let image = nes_rom::parse(rom).unwrap();
    let mut bus = ReferenceBus {
        prg: image.prg,
        mapper: Mmc3::new(
            &image.header,
            image.prg.len(),
            image.chr.len(),
            Mmc3Revision::Sharp,
        )
        .unwrap(),
        ram: [0; 0x800],
    };
    let mut cpu = oracle_6502::Cpu::new();
    cpu.reset(&mut bus);
    for _ in 0..5000 {
        cpu.step(&mut bus).unwrap();
    }
    bus.ram
}

fn generated_fixture(name: &str, rom: &[u8], profile: &str) -> (PathBuf, std::process::Output) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root
        .join("out/tests")
        .join(format!("mmc3-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("fixture.nes"), rom).unwrap();
    std::fs::write(work.join("profile.toml"), profile).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_nes-to-sms"))
        .arg(work.join("fixture.nes"))
        .arg(work.join("profile.toml"))
        .arg(work.join("sms"))
        .arg("--runtime")
        .arg(root.join("runtime"))
        .output()
        .unwrap();
    (work, output)
}

#[test]
fn mmc3_pipeline_generates_bounded_executable_contract() {
    let (rom, profile, expected) = fixture();
    let (work, output) = generated_fixture("executable", &rom, &profile);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = std::fs::read_to_string(work.join("sms/generated/translated.asm")).unwrap();
    assert!(
        !asm.contains("; WARN:"),
        "fixture contains a dropped operation"
    );
    for label in [
        "L_b0_8000:",
        "L_b3_A000:",
        "L_b2_C000:",
        "L_E020:",
        "L_E100:",
        "jp rt_banked_dispatch",
    ] {
        assert!(asm.contains(label), "missing {label}");
    }
    let image = nes_rom::parse(&rom).unwrap();
    for (pair, bytes) in image.prg.chunks_exact(0x4000).enumerate() {
        assert_eq!(
            std::fs::read(work.join(format!("sms/data/prg_bank_{pair}.bin"))).unwrap(),
            bytes
        );
    }
    let expectations = expected
        .iter()
        .map(|(addr, value)| format!("--expect-ram {addr:04X}={value:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    std::fs::write(
        work.join("trace-args.txt"),
        format!("--steps 2000000 --no-irq --expect-no-trap {expectations}\n"),
    )
    .unwrap();
    eprintln!(
        "MMC3 fixture: {} (generation only; assemble then run trace-args.txt)",
        work.display()
    );
}

#[test]
fn mmc3_pipeline_rejects_mapped_self_remapping_before_output() {
    let (mut rom, profile, _) = fixture();
    rom[16..24].copy_from_slice(&[0xa9, 6, 0x8d, 0x00, 0x80, 0xea, 0xea, 0x60]);
    let (work, output) = generated_fixture("self-remapping", &rom, &profile);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remapping continuations are not implemented")
    );
    assert!(!work.join("sms").exists());
}

#[test]
fn mmc3_pipeline_rejects_mapped_call_to_fixed_remapping_writer() {
    let (mut rom, profile, _) = fixture();
    // Mapped code would return into a window whose physical page changed.
    // Merely placing the writer itself in fixed ROM cannot make that safe.
    rom[16..24].copy_from_slice(&[0x20, 0x00, 0xe8, 0xea, 0xea, 0xea, 0xea, 0x60]);
    rom[16 + 0xe800..16 + 0xe80b]
        .copy_from_slice(&[0xa9, 6, 0x8d, 0x00, 0x80, 0xa9, 2, 0x8d, 0x01, 0x80, 0x60]);
    let (work, output) = generated_fixture("callee-remapping", &rom, &profile);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remapping continuations are not implemented")
    );
    assert!(!work.join("sms").exists());
}

fn two_bank_fixture(reset: &[u8], bank_zero: &[u8], bank_one: &[u8]) -> (Vec<u8>, String) {
    let (mut rom, profile, _) = fixture();
    rom[16 + 0xe100..16 + 0xfffa].fill(0xea);
    rom[16 + 0xe100..16 + 0xe100 + reset.len()].copy_from_slice(reset);
    rom[16..16 + bank_zero.len()].copy_from_slice(bank_zero);
    rom[16 + 0x2000..16 + 0x2000 + bank_one.len()].copy_from_slice(bank_one);
    let profile = format!(
        "{}[[bank_entry]]\nbank=0\naddr=0x8000\n[[bank_entry]]\nbank=1\naddr=0x8000\n",
        profile.split("[[bank_entry]]").next().unwrap()
    );
    (rom, profile)
}

#[test]
fn mmc3_pipeline_rejects_computed_callee_that_can_remap_a_pending_caller() {
    let reset = [
        0x78, 0xa9, 6, 0x8d, 0, 0x80, 0xa9, 0, 0x8d, 1, 0x80, 0x20, 0, 0x80, 0xa9, 0xa5, 0x85,
        0x3f, 0x4c, 0x12, 0xe1,
    ];
    let (mut rom, mut profile) = two_bank_fixture(
        &reset,
        &[0x20, 0, 0xe3, 0xa9, 0xaa, 0x85, 0x40, 0x60],
        &[0x20, 0, 0xe3, 0xa9, 0xee, 0x85, 0x40, 0x60],
    );
    // The fixed callee synthesizes an RTS target, which enters another
    // fixed routine that remaps the pending $8003 return continuation.
    rom[16 + 0xe300..16 + 0xe307].copy_from_slice(&[0xa9, 0xe1, 0x48, 0xa9, 0xff, 0x48, 0x60]);
    rom[16 + 0xe200..16 + 0xe206].copy_from_slice(&[0xa9, 1, 0x8d, 1, 0x80, 0x60]);
    profile.push_str("[[function]]\naddr=0xe200\nname='writer'\n");
    assert_eq!(reference_ram(&rom)[0x40], 0xee);
    let (work, output) = generated_fixture("computed-callee-remapping", &rom, &profile);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remapping continuations are not implemented")
    );
    assert!(!work.join("sms").exists());
}

#[test]
fn mmc3_pipeline_rejects_unimplemented_indirect_memory_and_jump_forms() {
    for (name, code) in [
        ("indirect-y-load", vec![0xa0, 0, 0xb1, 0, 0x4c, 4, 0xe1]),
        ("indirect-jump", vec![0x6c, 0, 0]),
    ] {
        let (rom, profile) = two_bank_fixture(&code, &[0x60], &[0x60]);
        let (work, output) = generated_fixture(name, &rom, &profile);
        assert!(!output.status.success(), "accepted {name}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("MMC3"));
        assert!(!work.join("sms").exists());
    }
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_assembled_dynamic_call_preserves_flags_across_distinct_physical_targets() {
    let reset = [
        0x78, 0xa9, 6, 0x8d, 0, 0x80, 0xa9, 0, 0x8d, 1, 0x80, 0xa2, 0, 0x08, 0xa9, 1, 0x20, 0,
        0x80, 0xa9, 0, 0xa9, 0xa5, 0x85, 0x3f, 0x4c, 0x19, 0xe1,
    ];
    // Bank 0 consumes incoming Z; bank 1 overwrites Z before its return.
    // A bare-address summary from bank 1 must not suppress bank 0's input.
    let (rom, profile) = two_bank_fixture(
        &reset,
        &[
            0xd0, 5, 0xa9, 0xee, 0x85, 0x40, 0x60, 0xa9, 0xaa, 0x85, 0x40, 0x60,
        ],
        &[0xa9, 7, 0x60],
    );
    let reference = reference_ram(&rom);
    assert_eq!(reference[0x40], 0xaa);
    let (work, project) = assembled_fixture("physical-target-flags", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "500000",
        "--no-irq",
        "--expect-no-trap",
        "--expect-ram",
        "C040=AA",
        "--expect-ram",
        "C03F=A5",
    ]);
    run_trace(&work, &mut trace);
}

/// Run explicitly whenever MMC3 emitter, lowering, or runtime banking changes:
/// cargo test -p nes_to_sms --test mmc3_pipeline mmc3_assembled -- --ignored --nocapture
#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_assembled_banking_matches_original_6502() {
    let (rom, profile, expected) = fixture();
    let reference = reference_ram(&rom);
    for &(addr, value) in &expected {
        assert_eq!(
            reference[usize::from(addr & 0x7ff)],
            value,
            "fixture ${addr:04X}"
        );
    }
    let (work, project) = assembled_fixture("assembled", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace
        .arg(project.join("sms.sms"))
        .args(["--steps", "2000000", "--no-irq", "--expect-no-trap"]);
    // Compare observations from the executed original CPU against the actual
    // assembled SMS runtime, not a second model of translated mapper helpers.
    for (addr, _) in expected {
        trace.arg("--expect-ram").arg(format!(
            "{addr:04X}={:02X}",
            reference[usize::from(addr & 0x7ff)]
        ));
    }
    for addr in 0xc040..=0xc047u16 {
        trace.arg("--expect-ram").arg(format!(
            "{addr:04X}={:02X}",
            reference[usize::from(addr & 0x7ff)]
        ));
    }
    run_trace(&work, &mut trace);
    eprintln!("Assembled MMC3/6502 parity passed: {}", work.display());
}

fn assembled_fixture(name: &str, rom: &[u8], profile: &str) -> (PathBuf, PathBuf) {
    let (work, output) = generated_fixture(name, rom, profile);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let project = work.join("sms").canonicalize().unwrap();
    let container_project = PathBuf::from("/work").join(project.strip_prefix(&root).unwrap());
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    let gid = std::process::Command::new("id").arg("-g").output().unwrap();
    assert!(uid.status.success() && gid.status.success());
    let user = format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    );
    let assembled = std::process::Command::new("docker")
        .args(["run", "--rm", "--network", "none", "--user", &user, "-v"])
        .arg(format!("{}:/work", root.display()))
        .args(["nes-to-sms-poc", "make", "-B", "-C"])
        .arg(container_project)
        .output()
        .expect("start existing Docker assembler");
    std::fs::write(
        work.join("assemble.log"),
        [&assembled.stdout[..], &assembled.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        assembled.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&assembled.stdout),
        String::from_utf8_lossy(&assembled.stderr)
    );
    (work, project)
}

fn run_trace(work: &std::path::Path, trace: &mut std::process::Command) {
    let traced = trace.output().unwrap();
    std::fs::write(
        work.join("trace.log"),
        [&traced.stdout[..], &traced.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        traced.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&traced.stdout),
        String::from_utf8_lossy(&traced.stderr)
    );
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_assembled_unsupported_chr_and_irq_effects_trap() {
    for (name, bytes) in [
        (
            "chr-trap",
            vec![0xa9, 0, 0x8d, 0, 0x80, 0xa9, 1, 0x8d, 1, 0x80],
        ),
        ("irq-trap", vec![0xa9, 0, 0x8d, 1, 0xe0]),
    ] {
        let (mut rom, profile, _) = fixture();
        rom[16 + 0xe100..16 + 0xe100 + bytes.len()].copy_from_slice(&bytes);
        let stop = 0xe100u16 + bytes.len() as u16;
        rom[16 + usize::from(stop)..16 + usize::from(stop) + 3].copy_from_slice(&[
            0x4c,
            stop as u8,
            (stop >> 8) as u8,
        ]);
        let (work, project) = assembled_fixture(name, &rom, &profile);
        let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
        trace.arg(project.join("sms.sms")).args([
            "--steps",
            "500000",
            "--no-irq",
            "--expect-ram",
            "CB1D=E8",
        ]);
        run_trace(&work, &mut trace);
    }
}
