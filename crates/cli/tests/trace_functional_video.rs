//! CLI-only synthetic SMS programs; no commercial ROM or external emulator.
use std::process::{Command, Output};

fn trace(args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trace-sms"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("SMS_") {
            command.env_remove(key);
        }
    }
    command.args(args).output().expect("run trace-sms")
}

#[test]
fn functional_video_rejects_invalid_mode_and_unimplemented_search_loops() {
    for flags in [
        vec!["--functional-video"],
        vec!["--functional-video", "pal"],
        vec!["--functional-video", "ntsc224", "--search-end-routes"],
        vec!["--functional-video", "ntsc224", "--search-late-routes"],
    ] {
        let mut args = vec!["deliberately-missing-rom.sms"];
        args.extend(flags);
        let result = trace(&args);
        assert_eq!(result.status.code(), Some(2));
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("functional-video"));
        assert!(
            !error.contains("read rom"),
            "validate option combinations before file access"
        );
    }
}

#[test]
fn actual_cli_halt_resumes_only_in_explicit_functional_mode() {
    let directory = std::env::temp_dir().join(format!(
        "nes-to-sms-functional-video-{}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("halt.sms");
    let mut rom = vec![0; 0x4000];
    // SP=DFFC; SMS frame IRQ enabled; EI; HALT; JP HALT.
    rom[..16].copy_from_slice(&[
        0x31, 0xfc, 0xdf, 0x3e, 0x20, 0xd3, 0xbf, 0x3e, 0x81, 0xd3, 0xbf, 0xfb, 0x76, 0xc3, 0x0c, 0,
    ]);
    // Real IM1 handler: preserve AF, acknowledge BF, increment RAM, EI/RET.
    let irq = [
        0xf5, 0xdb, 0xbf, 0x3a, 0, 0xc0, 0x3c, 0x32, 0, 0xc0, 0xf1, 0xfb, 0xc9,
    ];
    rom[0x38..0x38 + irq.len()].copy_from_slice(&irq);
    std::fs::write(&path, &rom).unwrap();
    let name = path.to_str().unwrap();
    let old = trace(&[
        name,
        "--steps",
        "120100",
        "--expect-no-trap",
        "--expect-ram",
        "C000=00",
    ]);
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    assert!(!String::from_utf8_lossy(&old.stdout).contains("Functional video:"));
    let new = trace(&[
        name,
        "--functional-video",
        "ntsc224",
        "--steps",
        "120100",
        "--expect-no-trap",
        "--expect-ram",
        "C000=02",
    ]);
    assert!(
        new.status.success(),
        "{}",
        String::from_utf8_lossy(&new.stderr)
    );
    let output = String::from_utf8_lossy(&new.stdout);
    assert!(output.contains("IRQs fired: 2"));
    assert!(output.contains("synthetic_epochs=2"));
    let no_irq = trace(&[
        name,
        "--functional-video",
        "ntsc224",
        "--no-irq",
        "--steps",
        "120100",
        "--expect-no-trap",
        "--expect-ram",
        "C000=00",
    ]);
    assert!(no_irq.status.success());
    assert!(String::from_utf8_lossy(&no_irq.stdout).contains("synthetic_epochs=0"));

    rom[11] = 0xf3; // DI; HALT must not idle until the step budget expires.
    std::fs::write(&path, &rom).unwrap();
    let disabled = trace(&[
        name,
        "--functional-video",
        "ntsc224",
        "--steps",
        "120100",
        "--expect-no-trap",
        "--expect-ram",
        "C000=00",
    ]);
    assert!(disabled.status.success());
    assert!(String::from_utf8_lossy(&disabled.stdout).contains("synthetic_epochs=0"));

    // Runtime trap plus HALT remains a failure, even in the resumable mode.
    rom[..9].copy_from_slice(&[0x31, 0xfc, 0xdf, 0x3e, 0xe5, 0x32, 0x1d, 0xcb, 0x76]);
    std::fs::write(&path, &rom).unwrap();
    let trapped = trace(&[name, "--functional-video", "ntsc224", "--expect-no-trap"]);
    assert_eq!(trapped.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&trapped.stderr).contains("runtime trap marker"));
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
