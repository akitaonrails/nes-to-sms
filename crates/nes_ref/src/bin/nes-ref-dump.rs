//! `nes-ref-dump <rom.nes> [frames] [addr:len]`
//!
//! Runs a ROM on the real NES reference (tetanes) for N frames and prints the
//! 2 KiB internal RAM — either a fnv-1a hash of the whole page, or a hex window
//! `addr:len`. Ground truth for the P0 differential-validation work: use it to
//! decide whether a new game's SMS divergence is a real translation bug (the
//! real NES disagrees with the SMS subject) or an artifact of the simplified
//! in-repo oracle (the real NES agrees with the subject).

use nes_ref::NesRef;

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn main() {
    let mut args = std::env::args().skip(1);
    let rom_path = args
        .next()
        .expect("usage: nes-ref-dump <rom.nes> [frames] [addr:len]");
    let frames: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(120);
    let window: Option<(usize, usize)> = args.next().and_then(|s| {
        let (a, l) = s.split_once(':')?;
        Some((
            usize::from_str_radix(a.trim_start_matches("0x").trim_start_matches('$'), 16).ok()?,
            l.parse().ok()?,
        ))
    });

    let rom = std::fs::read(&rom_path).expect("read rom");
    let mut nes = NesRef::load(&rom).expect("load rom into tetanes");
    for _ in 0..frames {
        nes.clock_frame().expect("clock frame");
    }
    let wram = nes.wram();

    println!(
        "tetanes ref: {} frames, wram fnv1a=0x{:016x}",
        frames,
        fnv1a(wram)
    );
    if let Some((addr, len)) = window {
        let end = (addr + len).min(wram.len());
        let hex: Vec<String> = wram[addr..end].iter().map(|b| format!("{b:02X}")).collect();
        println!("  ${:04X}..${:04X}: {}", addr, end, hex.join(" "));
    }
}
