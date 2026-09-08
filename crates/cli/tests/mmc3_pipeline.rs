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
    cart_ram: [u8; 0x2000],
}

impl oracle_6502::Bus for ReferenceBus<'_> {
    fn read(&mut self, addr: u16) -> u8 {
        if addr < 0x2000 {
            self.ram[usize::from(addr & 0x7ff)]
        } else if let Some(offset) = self.mapper.prg_ram_read_offset(addr) {
            self.cart_ram[offset]
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
        } else if let Some(offset) = self.mapper.prg_ram_write_offset(addr) {
            self.cart_ram[offset] = value;
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
        cart_ram: [0; 0x2000],
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

fn full_bus_fixture() -> (Vec<u8>, String) {
    let code = [
        0x78, 0xa9, 0x80, 0x8d, 1, 0xa0, // enable cartridge RAM
        0xa9, 0x5a, 0x8d, 0, 0x60, 0xa9, 0xa6, 0x8d, 0xff, 0x7f, 0xa9, 0xff, 0x85, 0xff, 0xa9,
        0x5f, 0x85, 0, 0xa0, 1, 0xb1, 0xff, 0x85,
        0x40, // ($ff),Y wraps pointer pair, crosses into $6000
        0xa9, 0, 0x85, 0xff, 0xa9, 0x60, 0x85, 0, 0xa2, 1, 0xa1, 0xfe, 0x85,
        0x41, // ($fe,X) wraps high pointer byte to zero
        0xad, 0xff, 0x7f, 0x85, 0x42, 0xa9, 0xc0, 0x8d, 1, 0xa0, 0xa9, 0xee, 0x8d, 0, 0x60, 0xad,
        0, 0x60, 0x85, 0x43, // protected write ignored
        0xa9, 0, 0x8d, 1, 0xa0, 0xa9, 0xee, 0x8d, 0, 0x60, 0xa9, 0x80, 0x8d, 1, 0xa0, 0xad, 0,
        0x60, 0x85, 0x44, 0xa9, 0x7f, 0x8d, 0xff, 1, 0xee, 0xff, 1, 0xad, 0xff, 1, 0x85, 0x45,
        0x4e, 0xff, 1, 0x2e, 0xff, 1, 0xad, 0xff, 1, 0x85, 0x46, 0xa9, 0x63, 0x8d, 0xff, 0x1f,
        0xad, 0xff, 7, 0x85, 0x47, 0xa9, 0, 0x85, 0xff, 0xa9, 0xe2, 0x85, 0, 0x6c, 0xff, 0,
    ];
    let (mut rom, profile) = two_bank_fixture(&code, &[0x60], &[0x60]);
    rom[10] = 7; // NES 2.0: 8 KiB volatile PRG RAM
    let finish = [
        0xa9, 0x7f, 0x85, 0xf0, 0xe6, 0xf0, // tagged zero-page store and RMW
        0x18, 0x65, 0xf0, 0x49, 0xaa, 0x85, 0x48, // ADC/EOR through RAM bus
        0x08, 0x68, 0x85, 0x49, // record original flags through guest stack
        0xa9, 0xa5, 0x85, 0x3f, 0x4c, 0x15, 0xe2,
    ];
    rom[16 + 0xe200..16 + 0xe200 + finish.len()].copy_from_slice(&finish);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[function]]\naddr=0xe200\nname='done'\n[input]\nmode='action'\npause_start=true\n";
    (rom, profile)
}

#[test]
fn mmc3_full_bus_reference_covers_cart_ram_and_indirect_boundaries() {
    let (rom, _) = full_bus_fixture();
    let ram = reference_ram(&rom);
    assert_eq!(
        &ram[0x40..0x48],
        &[0x5a, 0x5a, 0xa6, 0x5a, 0x5a, 0x80, 0x80, 0x63]
    );
    assert_eq!(ram[0x3f], 0xa5);
    assert_eq!(&ram[0x48..0x4a], &[0x55, 0x34]);
}

#[test]
fn mmc3_pipeline_rejects_dispatch_table_larger_than_one_mapped_slot() {
    check_dispatch_capacity(911, false);
}

#[test]
fn mmc3_pipeline_accepts_records_beyond_the_old_combined_directory_limit() {
    check_dispatch_capacity(900, true);
}

fn check_dispatch_capacity(roots_per_bank: u16, fits: bool) {
    let (mut rom, profile) = two_bank_fixture(&[0x4c, 0, 0xe1], &[0x60], &[0x60]);
    let mut profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    );
    // Many tiny legal routines fit the translated code budget but their
    // bank-qualified dispatch records do not fit the one mapped table slot.
    for bank in 0..3usize {
        rom[16 + bank * 0x2000..16 + bank * 0x2000 + usize::from(roots_per_bank) + 1].fill(0x60);
        for offset in 1..=roots_per_bank {
            profile.push_str(&format!(
                "[[bank_entry]]\nbank={bank}\naddr={}\n",
                0x8000 + offset
            ));
        }
    }
    let (work, result) = generated_fixture(
        &format!("dispatch-capacity-{roots_per_bank}"),
        &rom,
        &profile,
    );
    if fits {
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let asm = std::fs::read_to_string(work.join("sms/generated/translated.asm")).unwrap();
        assert!(asm.contains(".bank 0 slot 0\n.section \"rt_dispatch_directory_sec\" free"));
        assert!(asm.contains("rt_dispatch_table_end:"));
        return;
    }
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("MMC3 dispatch table exceeds its single 16 KiB slot")
    );
    assert!(
        !work.join("sms").exists(),
        "capacity error must precede project emission"
    );
}

#[test]
#[ignore = "requires Docker WLA-DX; checks the generated fixed-bank directory placement"]
fn mmc3_full_dispatch_directory_cannot_spill_out_of_fixed_bank() {
    let (rom, profile) = full_bus_fixture();
    let (work, result) = generated_fixture("dispatch-fixed-directory", &rom, &profile);
    assert!(result.status.success());
    let project = work.join("sms");
    let generated = std::fs::read_to_string(project.join("generated/translated.asm")).unwrap();
    let header = ".bank 0 slot 0\n.section \"rt_dispatch_directory_sec\" free\n";
    let directory = generated
        .split_once(header)
        .unwrap()
        .1
        .split_once(".ends")
        .unwrap()
        .0;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    let gid = std::process::Command::new("id").arg("-g").output().unwrap();
    assert!(uid.status.success() && gid.status.success());
    let user = format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    );
    for available in [384, 383] {
        // Isolate placement from unrelated runtime occupancy. These are the
        // exact generated directory bytes/section, with record addresses as
        // fixed forward references; a second empty bank must not absorb them.
        let mut asm = format!(
            ".memorymap\n defaultslot 0\n slotsize $4000\n slot 0 $0000\n slot 1 $4000\n.endme\n.rombankmap\n bankstotal 2\n banksize $4000\n banks 2\n.endro\n.bank 0 slot 0\n.org 0\n.section \"occupied\" force\n.dsb {}, $ff\n.ends\n",
            0x4000 - available
        );
        for page in 0x80..=0xff {
            asm.push_str(&format!(".define rt_dispatch_page_{page:02X} $4000\n"));
        }
        asm.push_str(header);
        asm.push_str(directory);
        asm.push_str(".ends\n");
        std::fs::write(project.join("sms.asm"), asm).unwrap();
        let output = std::process::Command::new("docker")
            .args(["run", "--rm", "--network", "none", "--user", &user, "-v"])
            .arg(format!("{}:/work", root.display()))
            .args(["nes-to-sms-poc", "make", "-B", "-C"])
            .arg(PathBuf::from("/work").join(project.strip_prefix(&root).unwrap()))
            .output()
            .unwrap();
        std::fs::write(
            work.join(format!("placement-{available}.log")),
            [&output.stdout[..], &output.stderr[..]].concat(),
        )
        .unwrap();
        assert_eq!(
            output.status.success(),
            available == 384,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if available == 384 {
            let symbols = std::fs::read_to_string(project.join("sms.sym")).unwrap();
            assert!(symbols.contains("00:3e80 rt_dispatch_page_table"));
            assert!(symbols.contains("00:3f80 rt_dispatch_page_counts"));
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("rt_dispatch_directory_sec"));
        }
    }
}

#[test]
#[ignore = "requires Docker WLA-DX; checks full-bank relocated index placement"]
fn mmc3_dense_index_cannot_spill_into_an_unreserved_bank() {
    let (rom, profile) = full_bus_fixture();
    let (work, result) = generated_fixture("dispatch-index-placement", &rom, &profile);
    assert!(result.status.success());
    let project = work.join("sms");
    let generated = std::fs::read_to_string(project.join("generated/translated.asm")).unwrap();
    let header = ".bank (PROJECT_ROM_DATA_BANK_BASE + 0) slot 1\n.section \"rt_dispatch_index_0_sec\" free\n";
    let index = generated
        .split_once(header)
        .unwrap()
        .1
        .split_once(".ends")
        .unwrap()
        .0;
    let labels: std::collections::BTreeSet<_> = index
        .lines()
        .filter_map(|line| line.trim().strip_prefix(".dw "))
        .collect();
    assert!(!labels.is_empty());
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let uid = std::process::Command::new("id").arg("-u").output().unwrap();
    let gid = std::process::Command::new("id").arg("-g").output().unwrap();
    let user = format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    );
    for occupied in [0, 1] {
        // Exact generated 16KiB section: a spare bank must not rescue even
        // one byte of accidental overlap in its explicitly reserved bank.
        let mut asm = String::from(
            ".memorymap\n defaultslot 0\n slotsize $4000\n slot 0 $0000\n slot 1 $4000\n.endme\n.rombankmap\n bankstotal 3\n banksize $4000\n banks 3\n.endro\n.define PROJECT_ROM_DATA_BANK_BASE 1\n",
        );
        if occupied != 0 {
            asm.push_str(".bank 1 slot 1\n.org 0\n.section \"occupied\" force\n.db $ff\n.ends\n");
        }
        for label in &labels {
            asm.push_str(&format!(".define {label} $4567\n"));
        }
        asm.push_str(header);
        asm.push_str(index);
        asm.push_str(".ends\n");
        std::fs::write(project.join("sms.asm"), asm).unwrap();
        let output = std::process::Command::new("docker")
            .args(["run", "--rm", "--network", "none", "--user", &user, "-v"])
            .arg(format!("{}:/work", root.display()))
            .args(["nes-to-sms-poc", "make", "-B", "-C"])
            .arg(PathBuf::from("/work").join(project.strip_prefix(&root).unwrap()))
            .output()
            .unwrap();
        std::fs::write(
            work.join(format!("placement-{occupied}.log")),
            [&output.stdout[..], &output.stderr[..]].concat(),
        )
        .unwrap();
        assert_eq!(
            output.status.success(),
            occupied == 0,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if occupied == 0 {
            let linked = std::fs::read(project.join("sms.sms")).unwrap();
            for word in linked[0x4000..0x8000].as_chunks::<2>().0 {
                assert_eq!(word, &[0x67, 0x45]);
            }
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("rt_dispatch_index_0_sec"));
        }
    }
}

#[test]
fn mmc3_cooperative_wait_requires_exact_physical_setup_and_poll() {
    let (mut rom, profile) = full_bus_fixture();
    let wait = [
        0xa9, 1, 0x85, 0x1c, 0xa9, 0, 0x85, 0x10, 0xa5, 0x10, 0x10, 0xfc, 0xa9, 0, 0x85, 0x1c,
        0x58, 0x60,
    ];
    rom[16..16 + wait.len()].copy_from_slice(&wait);
    let annotation = "\n[[cooperative_wait]]\nbank=0\nat=0x8008\ntick=0x10\ntick_enable=0x1c\n";
    let (work, output) = generated_fixture("wait-valid", &rom, &(profile.clone() + annotation));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = std::fs::read_to_string(work.join("sms/generated/translated.asm")).unwrap();
    assert_eq!(asm.matches("call rt_mmc3_wait_boundary").count(), 1);
    assert!(asm.contains("L_b0_8008:"));
    assert!(asm.contains("$8008: LDA"));
    assert!(asm.contains("$800A: BPL"));
    for (name, offset, byte) in [("wait-bad-setup", 3, 0x1d), ("wait-bad-branch", 10, 0x30)] {
        let mut invalid = rom.clone();
        invalid[16 + offset] = byte;
        let (_, output) = generated_fixture(name, &invalid, &(profile.clone() + annotation));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cooperative_wait"));
    }
    let (_, output) = generated_fixture(
        "wait-wrong-bank",
        &rom,
        &(profile + &annotation.replace("bank=0", "bank=1")),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cooperative_wait"));
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_bus_assembled_matches_original_6502() {
    let (rom, profile) = full_bus_fixture();
    let reference = reference_ram(&rom);
    let (work, project) = assembled_fixture("full-bus", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace
        .arg(project.join("sms.sms"))
        .args(["--steps", "2000000", "--no-irq", "--expect-no-trap"]);
    for (addr, value) in reference.iter().enumerate().take(0x4a).skip(0x3f) {
        trace
            .arg("--expect-ram")
            .arg(format!("{:04X}={value:02X}", 0xc000 + addr));
    }
    run_trace(&work, &mut trace);
}

#[test]
fn mmc3_pipeline_rejects_computed_callee_that_can_remap_a_pending_caller() {
    let (rom, profile) = computed_remapping_fixture();
    assert_eq!(reference_ram(&rom)[0x40], 0xee);
    let (work, output) = generated_fixture("computed-callee-remapping", &rom, &profile);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("remapping continuations are not implemented")
    );
    assert!(!work.join("sms").exists());
}

fn computed_remapping_fixture() -> (Vec<u8>, String) {
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
    (rom, profile)
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_continuation_resolves_live_mapping_after_computed_callee() {
    let (rom, profile) = computed_remapping_fixture();
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    );
    let reference = reference_ram(&rom);
    assert_eq!(reference[0x40], 0xee);
    let (work, project) = assembled_fixture("full-computed-remapping", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "2000000",
        "--no-irq",
        "--expect-no-trap",
        "--expect-ram",
        "C040=EE",
        "--expect-ram",
        "C03F=A5",
    ]);
    run_trace(&work, &mut trace);
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_store_barrier_resumes_in_new_physical_bank() {
    let reset = [
        0x78, 0xa9, 6, 0x8d, 0, 0x80, 0xa9, 0, 0x8d, 1, 0x80, 0x20, 0, 0x80, 0xa9, 0xa5, 0x85,
        0x3f, 0x4c, 0x12, 0xe1,
    ];
    let (rom, profile) = two_bank_fixture(
        &reset,
        &[0xa9, 1, 0x8d, 1, 0x80, 0xa9, 0xaa, 0x85, 0x40, 0x60],
        &[0xa9, 1, 0x8d, 1, 0x80, 0xa9, 0xee, 0x85, 0x40, 0x60],
    );
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    );
    assert_eq!(reference_ram(&rom)[0x40], 0xee);
    let (work, project) = assembled_fixture("full-store-remapping", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "2000000",
        "--no-irq",
        "--expect-no-trap",
        "--expect-ram",
        "C040=EE",
        "--expect-ram",
        "C03F=A5",
    ]);
    run_trace(&work, &mut trace);
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_nested_calls_materialize_guest_stack_at_high_sms_banks() {
    let reset = [
        0x78, 0xa2, 0xf0, 0x9a, 0x20, 0, 0xe2, 0xba, 0x86, 0x42, 0xa9, 0xa5, 0x85, 0x3f, 0x4c,
        0x0e, 0xe1,
    ];
    let (mut rom, profile) = two_bank_fixture(&reset, &[0x60], &[0x60]);
    rom[16 + 0xe200..16 + 0xe20a]
        .copy_from_slice(&[0xba, 0x86, 0x40, 0x20, 0, 0xe3, 0xba, 0x86, 0x43, 0x60]);
    rom[16 + 0xe300..16 + 0xe304].copy_from_slice(&[0xba, 0x86, 0x41, 0x60]);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    );
    let reference = reference_ram(&rom);
    assert_eq!(&reference[0x40..0x44], &[0xee, 0xec, 0xf0, 0xee]);
    for bank in [31, 32, 64, 95] {
        let (work, project) = assembled_fixture_in_bank(
            &format!("full-stack-bank{bank}"),
            &rom,
            &profile,
            Some(bank),
        );
        let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
        trace.arg(project.join("sms.sms")).args([
            "--steps",
            "1000000",
            "--no-irq",
            "--expect-no-trap",
            "--expect-ram",
            "C03F=A5",
        ]);
        for (addr, value) in reference.iter().enumerate().take(0x44).skip(0x40) {
            trace
                .arg("--expect-ram")
                .arg(format!("{:04X}={value:02X}", 0xc000 + addr));
        }
        run_trace(&work, &mut trace);
    }
}

fn inline_dynjump_fixture() -> (Vec<u8>, String) {
    let reset = [
        0x78, 0xa2, 0xf0, 0x9a, 0x20, 0, 0x80, 0xba, 0x86, 0x44, 0xa9, 0xa5, 0x85, 0x3f, 0x4c,
        0x0e, 0xe1,
    ];
    let (mut rom, profile) =
        two_bank_fixture(&reset, &[0xa9, 0, 0x20, 0, 0xe2, 0x10, 0x80], &[0x60]);
    // Handler records the actual dispatcher A/Y/P, table pointer, target,
    // and nested-call S. No replacement is used for the dispatch engine.
    rom[16 + 0x10..16 + 0x1d].copy_from_slice(&[
        0x85, 0x40, 0x84, 0x41, 0x08, 0x68, 0x85, 0x42, 0xba, 0x86, 0x43, 0x60, 0xea,
    ]);
    rom[16 + 0xe200..16 + 0xe212].copy_from_slice(&[
        0x0a, 0xa8, 0x68, 0x85, 0, 0x68, 0x85, 1, 0xc8, 0xb1, 0, 0x85, 2, 0xc8, 0xb1, 0, 0x85, 3,
    ]);
    rom[16 + 0xe212..16 + 0xe215].copy_from_slice(&[0x6c, 2, 0]);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[bank_entry]]\nbank=0\naddr=0x8010\n\
           [[data_region]]\nbank=0\nstart=0x8005\nend=0x8006\n\
           [[return_consume]]\nat=0xe202\nsecond_pla=0xe205\ncalls=[{bank=0,caller=0x8002,target=0xe200}]\n";
    (rom, profile)
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_actual_inline_dispatch_preserves_guest_side_effects() {
    let (rom, profile) = inline_dynjump_fixture();
    let reference = reference_ram(&rom);
    assert_eq!(&reference[..4], &[4, 0x80, 0x10, 0x80]);
    assert_eq!(&reference[0x40..0x45], &[0x80, 2, 0xb4, 0xee, 0xf0]);
    let (work, project) = assembled_fixture("full-inline-dispatch", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "1000000",
        "--no-irq",
        "--expect-no-trap",
        "--expect-ram",
        "C03F=A5",
    ]);
    for addr in (0..4).chain(0x40..0x45) {
        trace
            .arg("--expect-ram")
            .arg(format!("{:04X}={:02X}", 0xc000 + addr, reference[addr]));
    }
    run_trace(&work, &mut trace);
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_unannotated_return_byte_pla_traps() {
    let (rom, profile) = inline_dynjump_fixture();
    let profile = profile.split("[[return_consume]]").next().unwrap();
    let (work, project) = assembled_fixture("full-unguarded-pla", &rom, profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "1000000",
        "--no-irq",
        "--expect-ram",
        "CB1D=E8",
        "--expect-ram",
        "CB1B=02",
        "--expect-ram",
        "CB1C=E2",
    ]);
    run_trace(&work, &mut trace);
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
fn mmc3_wrong_bank_data_annotation_cannot_hide_an_unsupported_opcode() {
    let (rom, profile) = two_bank_fixture(&[0x4c, 0, 0xe1], &[0x60], &[0x02]);
    let profile = profile
        .replace(
            "MMC3_BANKING_EXPERIMENT",
            "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
        )
        .replace("[[bank_entry]]\nbank=0\naddr=0x8000\n", "")
        + "[[data_region]]\nbank=0\nstart=0x8000\nend=0x8000\n";
    let (work, output) = generated_fixture("wrong-bank-data", &rom, &profile);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("L_b1_8000") && error.contains("JAM"),
        "{error}"
    );
    assert!(!work.join("sms").exists());
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_profile_root_preserves_branch_across_inline_data() {
    let reset = [0x78, 0x20, 0, 0x80, 0x85, 0x40, 0x4c, 6, 0xe1];
    let code = [0xa2, 0, 0xf0, 2, 0x02, 0x02, 0xa9, 0x69, 0x60];
    let (rom, profile) = two_bank_fixture(&reset, &code, &[0x60]);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[bank_entry]]\nbank=0\naddr=0x8006\n\
         [[data_region]]\nbank=0\nstart=0x8004\nend=0x8005\n";
    assert_eq!(reference_ram(&rom)[0x40], 0x69);
    let (work, project) = assembled_fixture("inline-data-branch", &rom, &profile);
    let unresolved =
        std::fs::read_to_string(project.join("reports/unresolved_labels.txt")).unwrap();
    assert!(!unresolved.contains("L_b0_8006"));
    assert!(
        unresolved.contains("L_b0_8004"),
        "data fallthrough stays strict"
    );
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace.arg(project.join("sms.sms")).args([
        "--steps",
        "500000",
        "--no-irq",
        "--expect-no-trap",
        "--expect-ram",
        "C040=69",
    ]);
    run_trace(&work, &mut trace);
}

fn cross_window_consume_fixture(normal_return: bool) -> (Vec<u8>, String) {
    let reset = [
        0x78, 0x20, 0x80, 0xe1, 0xa9, 0xa5, 0x85, 0x3f, 0x4c, 8, 0xe1,
    ];
    let wrapper = [0x20, 0, 0xe2, 0xe6, 0x40, 0x68, 0x68, 0x4c, 0, 0xe3];
    // A store in another physical bank makes $8006 a potential mapped
    // continuation, but it cannot authorize bypassing bank0's first PLA.
    let collision = [0xa9, 0, 0xea, 0x8d, 1, 0x80, 0x60];
    let (mut rom, profile) = two_bank_fixture(&reset, &wrapper, &collision);
    rom[16 + 0xe180..16 + 0xe186].copy_from_slice(&[0x20, 0, 0x80, 0xe6, 0x41, 0x60]);
    let callee = if normal_return {
        [0x60, 0xea, 0xea]
    } else {
        [0x4c, 5, 0x80]
    };
    rom[16 + 0xe200..16 + 0xe203].copy_from_slice(&callee);
    rom[16 + 0xe300..16 + 0xe303].copy_from_slice(&[0xa9, 0x69, 0x60]);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[bank_entry]]\nbank=0\naddr=0x8005\n\
         [[function]]\naddr=0xe300\nname='suffix'\n\
         [[return_consume]]\nbank=0\nat=0x8005\n\
         calls=[{bank=0,caller=0x8000,target=0xe200},{caller=0xe180,target=0x8000}]\n";
    (rom, profile)
}

#[test]
fn mmc3_consume_interior_rejects_real_entries_despite_continuation_collision() {
    for decoded in [false, true] {
        let (mut rom, mut profile) = cross_window_consume_fixture(false);
        if decoded {
            rom[16 + 0x10..16 + 0x13].copy_from_slice(&[0x4c, 6, 0x80]);
        }
        profile.push_str(&format!(
            "[[bank_entry]]\nbank=0\naddr={}\n",
            if decoded { 0x8010 } else { 0x8006 }
        ));
        let (_, output) = generated_fixture(&format!("consume-bypass-{decoded}"), &rom, &profile);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("bypass"));
    }
}

#[test]
#[ignore = "requires existing nes-to-sms-poc Docker toolchain"]
fn mmc3_full_cross_window_consume_preserves_both_caller_paths() {
    for normal_return in [false, true] {
        let (rom, profile) = cross_window_consume_fixture(normal_return);
        let reference = reference_ram(&rom);
        assert_eq!(
            &reference[0x3f..0x42],
            &[0xa5, u8::from(normal_return), u8::from(!normal_return)]
        );
        let (work, project) = assembled_fixture(
            &format!("cross-window-consume-{normal_return}"),
            &rom,
            &profile,
        );
        let symbols = std::fs::read_to_string(project.join("sms.sym")).unwrap();
        assert!(!symbols.lines().any(|line| line.ends_with(" L_b0_8006")));
        assert!(symbols.lines().any(|line| line.ends_with(" L_b1_8006")));
        let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
        trace.arg(project.join("sms.sms")).args([
            "--steps",
            "500000",
            "--no-irq",
            "--expect-no-trap",
        ]);
        for addr in 0x3f..0x42 {
            trace
                .arg("--expect-ram")
                .arg(format!("{:04X}={:02X}", 0xc000 + addr, reference[addr]));
        }
        trace.args([
            "--expect-ram",
            "CB76=00",
            "--expect-ram",
            "CB77=D3",
            "--expect-ram",
            "CB02=FD",
        ]);
        run_trace(&work, &mut trace);
    }
}

#[test]
fn mmc3_full_discovers_backward_branch_before_any_existing_routine() {
    let (mut rom, profile) = two_bank_fixture(&[0xa9, 1, 0xd0, 0xec, 0x60], &[0x60], &[0x60]);
    rom[16 + 0xe0f0..16 + 0xe0f7].copy_from_slice(&[0xa9, 0x7c, 0x85, 0x40, 0x4c, 0, 0xe1]);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    );
    let (work, output) = generated_fixture("backward-branch-root", &rom, &profile);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = std::fs::read_to_string(work.join("sms/generated/translated.asm")).unwrap();
    let translated = asm.split(".section \"unresolved_stubs\"").next().unwrap();
    assert!(translated.contains("L_E0F0:"));
    assert!(translated.contains("6502 $E0F0: LDA #$7C"));
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
    assembled_fixture_in_bank(name, rom, profile, None)
}

fn assembled_fixture_in_bank(
    name: &str,
    rom: &[u8],
    profile: &str,
    code_bank: Option<u8>,
) -> (PathBuf, PathBuf) {
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
    if let Some(bank) = code_bank {
        // Relocate this tiny single-section fixture through the real linker;
        // bank-of-label frame bytes must not be mocked by a flat CPU bus.
        let path = project.join("generated/translated.asm");
        let asm = std::fs::read_to_string(&path).unwrap();
        assert_eq!(asm.matches(".bank 4 slot 1").count(), 1);
        assert!(!asm.contains(".bank 5 slot 1"));
        std::fs::write(
            path,
            asm.replace(".bank 4 slot 1", &format!(".bank {bank} slot 1")),
        )
        .unwrap();
    }
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

#[test]
#[ignore = "requires existing Docker assembler; produces fixture for trace-sms IRQ bridge tests"]
fn mmc3_full_irq_bridge_fixture() {
    let (mut rom, profile) = two_bank_fixture(&[0x4c, 0x00, 0xe1], &[0x60], &[0x60]);
    rom[10] = 7;
    // An actual translated poll authorizes the cooperative scheduler; the
    // helper tests enter it independently of the fixture's reset loop.
    let wait = [
        0xa9, 1, 0x85, 0x1c, 0xa9, 0, 0x85, 0x10, 0xa5, 0x10, 0x10, 0xfc, 0xa9, 0, 0x85, 0x1c,
        0x58, 0x60,
    ];
    rom[16..16 + wait.len()].copy_from_slice(&wait);
    // Actual translated 6502 handlers, not Z80-side stand-ins. NMI arms the
    // declared raster split; IRQ acknowledges it. Both restore interrupted A.
    let nmi = [
        0x48, 0xa5, 0x72, 0xf0, 5, 0xa9, 0, 0x8d, 0, 0xe0, 0xa9, 0xc0, 0x8d, 0, 0xc0, 0xa9, 0,
        0x8d, 1, 0xc0, 0x8d, 1, 0xe0, 0xe6, 0x70, 0xc6, 0x10, 0x68, 0x40,
    ];
    let irq = [0x48, 0xa9, 0, 0x8d, 0, 0xe0, 0xe6, 0x71, 0x68, 0x40];
    rom[16 + 0xe700..16 + 0xe700 + nmi.len()].copy_from_slice(&nmi);
    rom[16 + 0xe720..16 + 0xe720 + irq.len()].copy_from_slice(&irq);
    rom[16 + 0xfffa..16 + 0xfffc].copy_from_slice(&0xe700u16.to_le_bytes());
    rom[16 + 0xfffe..16 + 0x10000].copy_from_slice(&0xe720u16.to_le_bytes());
    let profile = profile
        .replace("nmi=0xe100", "nmi=0xe700")
        .replace("irq=0xe100", "irq=0xe720")
        .replace(
            "MMC3_BANKING_EXPERIMENT",
            "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
        )
        + "[[cooperative_wait]]\nbank=0\nat=0x8008\ntick=0x10\ntick_enable=0x1c\n";
    let image = nes_rom::parse(&rom).unwrap();
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
        cart_ram: [0; 0x2000],
    };
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = 7;
    cpu.a = 0x6d;
    cpu.x = 0x52;
    cpu.y = 0xa9;
    cpu.p = 0xa1;
    for nmi_event in [true, false, true] {
        if nmi_event {
            cpu.nmi(&mut bus);
        } else {
            cpu.irq(&mut bus);
        }
        for _ in 0..100 {
            if cpu.pc == 7 {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(
            (cpu.pc, cpu.a, cpu.x, cpu.y, cpu.p, cpu.sp),
            (7, 0x6d, 0x52, 0xa9, 0xa1, 0xfd)
        );
    }
    assert_eq!((bus.ram[0x70], bus.ram[0x71]), (2, 1));
    let (_, project) = assembled_fixture("full-irq-bridge", &rom, &profile);
    eprintln!("IRQ bridge fixture: {}", project.display());
}

#[test]
#[ignore = "requires existing Docker assembler; executes original cooperative polling body"]
fn mmc3_cooperative_wait_assembled_returns_after_actual_nmi() {
    let wait = [
        0xa9, 1, 0x85, 0x1c, 0xa9, 0, 0x85, 0x10, 0xa5, 0x10, 0x10, 0xfc, 0xa9, 0, 0x85, 0x1c,
        0x58, 0x60,
    ];
    let reset = [
        0xa9, 0xa8, 0x8d, 0, 0x20, 0x20, 0, 0x80, 0xa9, 0x5a, 0x85, 0x40, 0x4c, 0x0c, 0xe1,
    ];
    let (mut rom, profile) = two_bank_fixture(&reset, &wait, &[0x60]);
    rom[10] = 7;
    rom[16 + 0xe700..16 + 0xe707].copy_from_slice(&[0x48, 0xc6, 0x10, 0xe6, 0x70, 0x68, 0x40]);
    rom[16 + 0xfffa..16 + 0xfffc].copy_from_slice(&0xe700u16.to_le_bytes());
    let profile = profile.replace("nmi=0xe100", "nmi=0xe700").replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[cooperative_wait]]\nbank=0\nat=0x8008\ntick=0x10\ntick_enable=0x1c\n";
    let image = nes_rom::parse(&rom).unwrap();
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
        cart_ram: [0; 0x2000],
    };
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = 0xe100;
    let mut nmi_delivered = false;
    for _ in 0..100 {
        if cpu.pc == 0x8008 && !nmi_delivered {
            assert_eq!((bus.ram[0x10], bus.ram[0x1c]), (0, 1));
            cpu.nmi(&mut bus);
            nmi_delivered = true;
        }
        cpu.step(&mut bus).unwrap();
        if bus.ram[0x40] == 0x5a {
            break;
        }
    }
    assert!(nmi_delivered);
    assert_eq!(
        (bus.ram[0x10], bus.ram[0x1c], bus.ram[0x40], bus.ram[0x70]),
        (0xff, 0, 0x5a, 1)
    );
    assert_eq!(cpu.p & 4, 0);
    let (work, project) = assembled_fixture("cooperative-wait", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace
        .arg(project.join("sms.sms"))
        .args(["--steps", "3000000", "--expect-no-trap"]);
    for (addr, value) in [
        (0xc010, 0xff),
        (0xc01c, 0),
        (0xc040, 0x5a),
        (0xc070, 1),
        (0xc825, 0),
    ] {
        trace
            .arg("--expect-ram")
            .arg(format!("{addr:04X}={value:02X}"));
    }
    run_trace(&work, &mut trace);
}

#[test]
#[ignore = "requires existing Docker assembler; stresses full software calls and tails"]
fn mmc3_full_dynamic_dispatch_does_not_accumulate_native_frames() {
    // 1024 ordinary mapped calls, then 1024 mapped tail cycles under one
    // software owner. Repeated targets exercise the old MRU hit/miss paths.
    let mut reset = vec![
        0xa9, 7, 0x8d, 0, 0x80, 0xa9, 1, 0x8d, 1, 0x80, 0xa0, 4, 0xa2, 0, 0x20, 0, 0x80, 0xe8,
        0xd0, 0xfa, 0x88, 0xd0, 0xf5, 0xa9, 0x44, 0x85, 0x40, 0xa9, 0, 0x85, 0x50, 0xa9, 4, 0x85,
        0x51, 0x20, 0x20, 0x80, 0x08, 0x68, 0x85, 0x42, 0xa9, 0x5a, 0x85, 0x41,
    ];
    let stop = 0xe100 + reset.len() as u16;
    reset.extend_from_slice(&[0x4c, stop as u8, (stop >> 8) as u8]);
    let (mut rom, profile) = two_bank_fixture(&reset, &[0x60], &[0x60]);
    rom[10] = 7;
    rom[16 + 0x20..16 + 0x23].copy_from_slice(&[0x4c, 0x20, 0xa0]);
    // DEC low; BNE tail; DEC high; BEQ done; tail:JMP $8020; done:SEC/RTS.
    let tail = [
        0xc6, 0x50, 0xd0, 4, 0xc6, 0x51, 0xf0, 3, 0x4c, 0x20, 0x80, 0x38, 0x60,
    ];
    rom[16 + 0x2020..16 + 0x2020 + tail.len()].copy_from_slice(&tail);
    let profile = profile.replace(
        "MMC3_BANKING_EXPERIMENT",
        "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
    ) + "[[bank_entry]]\nbank=0\naddr=0x8020\n[[bank_entry]]\nbank=1\naddr=0xa020\n";
    let image = nes_rom::parse(&rom).unwrap();
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
        cart_ram: [0; 0x2000],
    };
    let mut cpu = oracle_6502::Cpu::new();
    cpu.pc = 0xe100;
    for _ in 0..20000 {
        if cpu.pc == stop {
            break;
        }
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.pc, stop);
    assert_eq!((cpu.x, cpu.y, cpu.sp), (0, 0, 0xfd));
    assert_eq!(&bus.ram[0x40..0x43], &[0x44, 0x5a, 0x37]);
    let (work, project) = assembled_fixture("full-dispatch-stack", &rom, &profile);
    let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    trace
        .arg(project.join("sms.sms"))
        .args(["--steps", "5000000", "--no-irq", "--expect-no-trap"]);
    for (addr, value) in [
        (0xc040, 0x44),
        (0xc041, 0x5a),
        (0xc042, 0x37),
        (0xc050, 0),
        (0xc051, 0),
        (0xcb02, 0xfd),
        (0xcb76, 0),
        (0xcb77, 0xd3),
        (0xd47d, 0xc0),
        (0xd47e, 0xd4),
    ] {
        trace
            .arg("--expect-ram")
            .arg(format!("{addr:04X}={value:02X}"));
    }
    run_trace(&work, &mut trace);
    let log = std::fs::read_to_string(work.join("trace.log")).unwrap();
    let state = log
        .lines()
        .find(|line| line.starts_with("Cpu state:"))
        .unwrap();
    assert!(
        state.contains("A=$5A") && state.contains("D=$00 E=$00"),
        "{state}"
    );
    assert!(
        log.contains("SP=$DFFC"),
        "native caller depth must not accumulate: {log}"
    );
}

#[test]
#[ignore = "requires existing Docker assembler; compares rewritten live RTS bytes with 6502"]
fn mmc3_full_rts_honors_rewritten_live_return_and_current_mapping() {
    for stack in [0u8, 1, 0xfd, 0xff] {
        for mapped in [false, true] {
            let reset = [
                0xa2, stack, 0x9a, 0x20, 0, 0xe2, 0xa9, 0xaa, 0x85, 0x40, 0x4c, 0x0a, 0xe1,
            ];
            let (mut rom, profile) = two_bank_fixture(&reset, &[0x60], &[0x60]);
            rom[10] = 7;
            let target = if mapped { 0xa020u16 } else { 0xe300u16 };
            let return_addr = target - 1;
            // INX wraps within the guest stack page, unlike absolute $0101,X.
            let mut callee = vec![
                0xba,
                0xe8,
                0xa9,
                return_addr as u8,
                0x9d,
                0,
                1,
                0xe8,
                0xa9,
                (return_addr >> 8) as u8,
                0x9d,
                0,
                1,
            ];
            if mapped {
                callee.extend_from_slice(&[0xa9, 7, 0x8d, 0, 0x80, 0xa9, 2, 0x8d, 1, 0x80]);
            }
            callee.extend_from_slice(&[0xa2, 0x52, 0xa0, 0xa9, 0xa9, 0x69, 0x38, 0xf8, 0x60]);
            rom[16 + 0xe200..16 + 0xe200 + callee.len()].copy_from_slice(&callee);
            let mut destination = vec![
                0x85, 0x40, 0x08, 0x68, 0x85, 0x41, 0x8a, 0x85, 0x42, 0x98, 0x85, 0x43, 0xba, 0x8a,
                0x85, 0x44,
            ];
            let stop = target + destination.len() as u16;
            destination.extend_from_slice(&[0x4c, stop as u8, (stop >> 8) as u8]);
            let offset = if mapped { 0x4020 } else { 0xe300 };
            rom[16 + offset..16 + offset + destination.len()].copy_from_slice(&destination);
            let profile = profile.replace(
                "MMC3_BANKING_EXPERIMENT",
                "MMC3_FULL_RUNTIME','SMB3_MMC3_SINGLE_SPLIT",
            ) + if mapped {
                "[[bank_entry]]\nbank=2\naddr=0xa020\n"
            } else {
                "[[function]]\naddr=0xe300\nname='rewritten_destination'\n"
            };
            let image = nes_rom::parse(&rom).unwrap();
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
                cart_ram: [0; 0x2000],
            };
            let mut cpu = oracle_6502::Cpu::new();
            cpu.pc = 0xe100;
            for _ in 0..200 {
                if cpu.pc == stop {
                    break;
                }
                cpu.step(&mut bus).unwrap();
            }
            assert_eq!(cpu.pc, stop);
            assert_eq!(&bus.ram[0x40..0x45], &[0x69, 0x3d, 0x52, 0xa9, stack]);
            assert_eq!(cpu.sp, stack);
            let (work, project) = assembled_fixture(
                &format!("rewritten-rts-{stack:02x}-{mapped}"),
                &rom,
                &profile,
            );
            let mut trace = std::process::Command::new(env!("CARGO_BIN_EXE_trace-sms"));
            trace.arg(project.join("sms.sms")).args([
                "--steps",
                "500000",
                "--no-irq",
                "--expect-no-trap",
            ]);
            for (offset, value) in bus.ram[0x40..0x45].iter().enumerate() {
                trace
                    .arg("--expect-ram")
                    .arg(format!("{:04X}={value:02X}", 0xc040 + offset));
            }
            trace.args(["--expect-ram", "CB76=00", "--expect-ram", "CB77=D3"]);
            run_trace(&work, &mut trace);
        }
    }
}
