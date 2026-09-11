//! A real, accurate NES reference for differential validation.
//!
//! Wraps `tetanes-core` (a cycle-accurate NES emulator) behind a tiny surface:
//! load a ROM, clock whole frames, read the 2 KiB internal RAM. This is the
//! ground-truth oracle that lets `frame-diff` validate *new* games byte-exact —
//! unlike the simplified in-repo `oracle_6502`, which only matches SMB because
//! the two were co-tuned. See docs/conversion-strategy-2026-09.md (P0).

use std::io::Cursor;

use tetanes_core::control_deck::{Config, ControlDeck, HeadlessMode};
use tetanes_core::memory::RamState;

/// A running NES, driven a frame at a time.
pub struct NesRef {
    deck: ControlDeck,
}

impl NesRef {
    /// NES output frame dimensions (the PPU renders 256x240; display crops the
    /// top/bottom 8 lines to 224, but the buffer is the full 240).
    pub const FRAME_WIDTH: usize = 256;
    pub const FRAME_HEIGHT: usize = 240;

    /// Load an iNES ROM image and power on.
    ///
    /// RAM powers on all-zeros so the reference is deterministic and matches the
    /// SMS subject, whose boot zeroes RAM. Audio is disabled; video stays on so
    /// the rendered framebuffer can be read as the authoritative NES image.
    pub fn load(rom: &[u8]) -> Result<Self, String> {
        let config = Config::default()
            .with_headless_mode(HeadlessMode::NO_AUDIO)
            .with_ram_state(RamState::AllZeros);
        let mut deck = ControlDeck::with_config(config);
        let mut cursor = Cursor::new(rom.to_vec());
        deck.load_rom("rom", &mut cursor)
            .map_err(|e| format!("tetanes load_rom failed: {e}"))?;
        Ok(Self { deck })
    }

    /// Run exactly one NES frame (through the next vblank).
    pub fn clock_frame(&mut self) -> Result<(), String> {
        let _clocked = self
            .deck
            .clock_frame()
            .map_err(|e| format!("tetanes clock_frame failed: {e}"))?;
        Ok(())
    }

    /// The 2 KiB internal work RAM ($0000-$07FF) — the region frame-diff compares.
    pub fn wram(&self) -> &[u8] {
        self.deck.wram()
    }

    /// The current rendered frame as RGBA bytes (`FRAME_WIDTH * FRAME_HEIGHT * 4`).
    /// This is the authoritative NES image — the ground truth for rendering bugs.
    pub fn frame_rgba(&mut self) -> Vec<u8> {
        self.deck.frame_buffer().to_vec()
    }
}
