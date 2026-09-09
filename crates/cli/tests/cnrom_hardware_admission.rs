//! Capability admission/capacity only; these fixtures do not prove gameplay.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(name: &str, nops: usize, device: u8, hardware: bool) -> (PathBuf, Output) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = root.join(format!(
        "out/tests/cnrom-hardware-{name}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work).unwrap();
    let mut rom = vec![0; 16 + 32768 + 8192];
    rom[..4].copy_from_slice(b"NES\x1a");
    rom[4..9].copy_from_slice(&[2, 1, 0x30, 8, 0x20]);
    rom[15] = device;
    rom[16..16 + nops].fill(0xea);
    let stop = 0x8000 + nops as u16;
    rom[16 + nops..19 + nops].copy_from_slice(&[0x4c, stop as u8, (stop >> 8) as u8]);
    for offset in [0x7ffa, 0x7ffc, 0x7ffe] {
        rom[16 + offset..18 + offset].copy_from_slice(&0x8000u16.to_le_bytes());
    }
    std::fs::write(work.join("fixture.nes"), rom).unwrap();
    let capability = if hardware {
        "CNROM_SOURCE_HARDWARE_EXPERIMENT"
    } else {
        "CNROM_SOURCE_CLOCK_EXPERIMENT"
    };
    std::fs::write(work.join("profile.toml"), format!("[rom]\nname='admission'\nmapper=3\nprg_kib=32\nchr_kib=8\n[translation]\nstack_discipline='software'\nruntime_defines=['{capability}']\n")).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_nes-to-sms"))
        .arg(work.join("fixture.nes"))
        .arg(work.join("profile.toml"))
        .arg(work.join("sms"))
        .arg("--runtime")
        .arg(root.join("runtime"))
        .output()
        .unwrap();
    (work, result)
}

#[test]
fn standard_pad_header_is_hardware_only_and_other_devices_fail_before_output() {
    for (device, hardware, accepted) in [
        (0, false, true),
        (1, false, false),
        (0, true, true),
        (1, true, true),
        (2, true, false),
        (8, true, false),
        (0x81, true, false),
    ] {
        let (work, result) = fixture(&format!("pad{device}-{hardware}"), 1, device, hardware);
        assert_eq!(
            result.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(work.join("sms").exists(), accepted);
        if !accepted {
            std::fs::create_dir(work.join("sms")).unwrap();
            std::fs::write(work.join("sms/keep.txt"), "existing output").unwrap();
            let (_, repeated) = fixture(&format!("pad{device}-{hardware}"), 1, device, hardware);
            assert!(!repeated.status.success());
            assert_eq!(
                std::fs::read_to_string(work.join("sms/keep.txt")).unwrap(),
                "existing output"
            );
            assert_eq!(std::fs::read_dir(work.join("sms")).unwrap().count(), 1);
        }
    }
}

#[test]
fn verified_large_hardware_routine_uses_actual_byte_capacity_not_ir_count() {
    let (work, accepted) = fixture("large-accepted", 600, 1, true);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let asm = std::fs::read_to_string(work.join("sms/generated/translated.asm")).unwrap();
    assert!(asm.contains("L_8000:"));
    assert!(asm.contains("L_8257:")); // Every original NOP, including the last.
    assert!(asm.contains("L_8258:")); // Real terminal branch, not a trap stub.
    let (work, legacy) = fixture("large-old-mode", 600, 0, false);
    assert!(!legacy.status.success());
    assert!(String::from_utf8_lossy(&legacy.stderr).contains("analyzed routine size bound"));
    assert!(!work.join("sms").exists());
    let (work, overflow) = fixture("large-byte-overflow", 2000, 1, true);
    assert!(!overflow.status.success());
    let diagnostic = String::from_utf8_lossy(&overflow.stderr);
    assert!(
        diagnostic.contains("exceeds physical 16 KiB slot:") && diagnostic.contains("bytes"),
        "{diagnostic}"
    );
    assert!(!work.join("sms").exists());
}
