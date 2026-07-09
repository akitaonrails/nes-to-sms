//! `trace-sms <rom.sms> [--steps N]` — run a built SMS ROM under
//! `z80_emu` and report execution path. Used to diagnose why a ROM
//! produces unexpected output (e.g. all-black screen).
//!
//! Implements minimal SMS hardware:
//! - Sega mapper: writes to $FFFC-$FFFF control banks for slot 0/1/2.
//! - I/O ports: $BE/$BF (VDP), $DC/$DD (controller) — VDP writes are
//!   logged, reads return sensible defaults so the CPU doesn't stall
//!   waiting forever (vblank flag toggles every "frame").
//! - Memory: 8 KB SMS RAM at $C000-$DFFF, mirrored at $E000-$FFFF.
//!   ROM banks live in `rom_banks[bank][offset]`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use z80_emu::{Bus, Cpu, StepError};

const BANK_SIZE: usize = 0x4000;
const CART_RAM_SIZE: usize = 0x8000;
const RAM_SIZE: usize = 0x2000;
const IRQ_PERIOD: usize = 60_000;

/// SMS_REAL_PACING=1: hostile, hardware-honest pacing — ~15K
/// instructions per frame (≈59,736 T-states at ~4 T/instruction),
/// boolean pending semantics (missed frame INTs don't queue), and
/// $FF-initialized RAM. Reproduces real-emulator stalls in-harness.
fn real_pacing() -> bool {
    std::env::var("SMS_REAL_PACING").is_ok()
}
fn irq_period() -> usize {
    if real_pacing() { 15_000 } else { IRQ_PERIOD }
}
const RT_PPU_WRITE_FALLBACK_ADDR: u16 = 0x0068;
const D3XX_TILE_DIRTY_BITMAP_BYTES: usize = 240;
const D3XX_ATTR_DIRTY_BITMAP_BYTES: usize = 16;
const MATERIALIZER_BUDGETS: [usize; 5] = [28, 56, 112, 224, 896];
// Trace-only acceptance hooks for future runtime nametable materializers. These
// symbols do not exist yet in normal builds; diagnostics report unavailable
// until one is present in the generated WLA symbol file.
const MATERIALIZER_HOOK_SYMBOLS: &[&str] = &[
    "rt_nt_materialize_render_off",
    "rt_nt_materializer_render_off",
    "rt_materialize_render_off",
    "rt_nt_materialize_bulk",
    "rt_nt_materializer_bulk",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExpectedMirroring {
    Vertical,
    Horizontal,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NtWriteKind {
    Tile,
    Attr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RamMigrationRange {
    CcFoldedS,
    D300CompactS,
    Da00BgvBase,
}

impl RamMigrationRange {
    fn index(self) -> usize {
        match self {
            Self::CcFoldedS => 0,
            Self::D300CompactS => 1,
            Self::Da00BgvBase => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CcFoldedS => "cc",
            Self::D300CompactS => "d300",
            Self::Da00BgvBase => "da00",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RamMigrationAccessKind {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum D3xxStorageRange {
    D300D3df,
    D3e0D3ff,
}

impl D3xxStorageRange {
    fn index(self) -> usize {
        match self {
            Self::D300D3df => 0,
            Self::D3e0D3ff => 1,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::D300D3df => "d300",
            Self::D3e0D3ff => "d3e0",
        }
    }
}

impl RamMigrationAccessKind {
    fn label(self) -> &'static str {
        match self {
            Self::Read => "r",
            Self::Write => "w",
        }
    }
}

fn ram_migration_range_for_physical_addr(addr: u16) -> Option<RamMigrationRange> {
    match addr {
        0xCC00..=0xD2FF => Some(RamMigrationRange::CcFoldedS),
        0xD300..=0xD3DF => Some(RamMigrationRange::D300CompactS),
        0xDA00..=0xDD7F => Some(RamMigrationRange::Da00BgvBase),
        _ => None,
    }
}

fn d3xx_storage_range_for_physical_addr(addr: u16) -> Option<D3xxStorageRange> {
    match addr {
        0xD300..=0xD3DF => Some(D3xxStorageRange::D300D3df),
        0xD3E0..=0xD3FF => Some(D3xxStorageRange::D3e0D3ff),
        _ => None,
    }
}

fn nt_ppu_write_kind(ppu_addr: u16) -> Option<NtWriteKind> {
    if !(0x2000..=0x2FFF).contains(&ppu_addr) {
        return None;
    }
    if (ppu_addr & 0x03FF) >= 0x03C0 {
        Some(NtWriteKind::Attr)
    } else {
        Some(NtWriteKind::Tile)
    }
}

impl ExpectedMirroring {
    fn label(self) -> &'static str {
        match self {
            Self::Vertical => "vertical",
            Self::Horizontal => "horizontal",
            Self::Unknown => "unknown",
        }
    }

    fn vertical_flag(self) -> Option<bool> {
        match self {
            Self::Vertical => Some(true),
            Self::Horizontal => Some(false),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Debug)]
struct MaterializerVisibleCell {
    key: u16,
    row: usize,
    col: usize,
    ppu_addr: u16,
    ciram: usize,
    reason: u8,
}

#[derive(Clone, Debug)]
struct MaterializerBacklogCell {
    key: u16,
    row: usize,
    col: usize,
    ppu_addr: u16,
    ciram: usize,
    reason: u8,
    frame_added: usize,
}

#[derive(Clone, Debug)]
struct MaterializerBudgetSim {
    budget: usize,
    backlog: VecDeque<MaterializerBacklogCell>,
    queued: [bool; 32 * 28],
    max_backlog: usize,
}

impl MaterializerBudgetSim {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            backlog: VecDeque::new(),
            queued: [false; 32 * 28],
            max_backlog: 0,
        }
    }

    fn new_all() -> Vec<Self> {
        MATERIALIZER_BUDGETS
            .iter()
            .copied()
            .map(Self::new)
            .collect()
    }

    fn step(
        &mut self,
        frame: usize,
        workset: &[MaterializerVisibleCell],
    ) -> MaterializerBudgetStep {
        let mut added = 0usize;
        for cell in workset {
            let key = usize::from(cell.key);
            if self.queued[key] {
                continue;
            }
            self.queued[key] = true;
            self.backlog.push_back(MaterializerBacklogCell {
                key: cell.key,
                row: cell.row,
                col: cell.col,
                ppu_addr: cell.ppu_addr,
                ciram: cell.ciram,
                reason: cell.reason,
                frame_added: frame,
            });
            added += 1;
        }
        self.max_backlog = self.max_backlog.max(self.backlog.len());

        let mut processed = 0usize;
        for _ in 0..self.budget {
            let Some(cell) = self.backlog.pop_front() else {
                break;
            };
            self.queued[usize::from(cell.key)] = false;
            processed += 1;
        }

        let oldest_age = self
            .backlog
            .front()
            .map(|cell| frame.saturating_sub(cell.frame_added));
        let deferred = self.backlog.iter().take(3).cloned().collect();
        MaterializerBudgetStep {
            budget: self.budget,
            added,
            processed,
            backlog: self.backlog.len(),
            max_backlog: self.max_backlog,
            oldest_age,
            deferred,
        }
    }

    fn snapshot(&self, frame: usize) -> MaterializerBudgetStep {
        MaterializerBudgetStep {
            budget: self.budget,
            added: 0,
            processed: 0,
            backlog: self.backlog.len(),
            max_backlog: self.max_backlog,
            oldest_age: self
                .backlog
                .front()
                .map(|cell| frame.saturating_sub(cell.frame_added)),
            deferred: self.backlog.iter().take(3).cloned().collect(),
        }
    }
}

#[derive(Clone, Debug)]
struct MaterializerBudgetStep {
    budget: usize,
    added: usize,
    processed: usize,
    backlog: usize,
    max_backlog: usize,
    oldest_age: Option<usize>,
    deferred: Vec<MaterializerBacklogCell>,
}

#[derive(Clone, Debug)]
struct MaterializerPendingCell {
    key: u16,
    first_seen: usize,
    last_reason: u8,
}

#[derive(Clone, Debug)]
struct MaterializerPolicySim {
    budget: usize,
    pending: VecDeque<MaterializerPendingCell>,
    queued: [bool; 32 * 28],
    max_after_backlog: usize,
    max_age: usize,
    stale_visible_frames: usize,
}

impl MaterializerPolicySim {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            pending: VecDeque::new(),
            queued: [false; 32 * 28],
            max_after_backlog: 0,
            max_age: 0,
            stale_visible_frames: 0,
        }
    }

    fn new_all() -> Vec<Self> {
        MATERIALIZER_BUDGETS
            .iter()
            .copied()
            .map(Self::new)
            .collect()
    }

    fn step(
        &mut self,
        frame: usize,
        workset: &[MaterializerVisibleCell],
        render_state: RenderState,
    ) -> MaterializerPolicyStep {
        let mut added = 0usize;
        for cell in workset {
            let key = usize::from(cell.key);
            if self.queued[key] {
                if let Some(pending) = self
                    .pending
                    .iter_mut()
                    .find(|pending| pending.key == cell.key)
                {
                    pending.last_reason |= cell.reason;
                }
                continue;
            }
            self.queued[key] = true;
            self.pending.push_back(MaterializerPendingCell {
                key: cell.key,
                first_seen: frame,
                last_reason: cell.reason,
            });
            added += 1;
        }

        let before = self.pending.len();
        let mut process_order = Vec::new();
        let mut selected = [false; 32 * 28];
        for cell in workset {
            let key = usize::from(cell.key);
            if self.queued[key] && !selected[key] {
                selected[key] = true;
                process_order.push(cell.key);
            }
        }
        for pending in &self.pending {
            let key = usize::from(pending.key);
            if !selected[key] {
                selected[key] = true;
                process_order.push(pending.key);
            }
        }

        let mut processed = 0usize;
        for key in process_order.into_iter().take(self.budget) {
            if let Some(pos) = self.pending.iter().position(|pending| pending.key == key) {
                self.pending.remove(pos);
                self.queued[usize::from(key)] = false;
                processed += 1;
            }
        }

        // Conservative visible-staleness counter: count frames where rendering
        // is enabled and pending visible cells remain after this budget's
        // processing slice. Render-off frames are not counted as visible stale.
        if render_state == RenderState::On && !self.pending.is_empty() {
            self.stale_visible_frames += 1;
        }
        let oldest_age = self
            .pending
            .iter()
            .map(|pending| frame.saturating_sub(pending.first_seen))
            .max();
        if let Some(age) = oldest_age {
            self.max_age = self.max_age.max(age);
        }
        self.max_after_backlog = self.max_after_backlog.max(self.pending.len());
        MaterializerPolicyStep {
            budget: self.budget,
            render_state,
            added,
            processed,
            before,
            after: self.pending.len(),
            max_after: self.max_after_backlog,
            max_age: self.max_age,
            stale_visible_frames: self.stale_visible_frames,
            deferred: self.pending.iter().take(3).cloned().collect(),
        }
    }

    fn snapshot(&self, _frame: usize, render_state: RenderState) -> MaterializerPolicyStep {
        MaterializerPolicyStep {
            budget: self.budget,
            render_state,
            added: 0,
            processed: 0,
            before: self.pending.len(),
            after: self.pending.len(),
            max_after: self.max_after_backlog,
            max_age: self.max_age,
            stale_visible_frames: self.stale_visible_frames,
            deferred: self.pending.iter().take(3).cloned().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderState {
    On,
    Off,
}

impl RenderState {
    fn label(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Debug)]
struct MaterializerPolicyStep {
    budget: usize,
    render_state: RenderState,
    added: usize,
    processed: usize,
    before: usize,
    after: usize,
    max_after: usize,
    max_age: usize,
    stale_visible_frames: usize,
    deferred: Vec<MaterializerPendingCell>,
}

#[derive(Clone, Debug)]
struct RuntimeMaterializerHook {
    name: String,
    addr: u16,
}

#[derive(Clone, Debug)]
struct RuntimeMaterializerOffense {
    step: usize,
    pc: u16,
    symbol: String,
    cb09: u8,
    vdp_reg1: u8,
}

#[derive(Clone, Debug)]
struct ActiveMaterializerHook {
    name: String,
    entry_sp: u16,
}

#[derive(Clone, Debug)]
struct RuntimeMaterializerMonitor {
    hooks: Vec<RuntimeMaterializerHook>,
    active: Option<ActiveMaterializerHook>,
    calls_on: u32,
    calls_off: u32,
    vdp_writes_on: u32,
    vdp_writes_off: u32,
    first_on_call: Option<RuntimeMaterializerOffense>,
    first_on_vdp: Option<RuntimeMaterializerOffense>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Z80StackWatermark {
    initial_sp: u16,
    low_sp: u16,
}

#[derive(Clone, Debug)]
struct D300PendingRead {
    addr: u16,
    pc: u16,
    render_state: RenderState,
}

impl Z80StackWatermark {
    fn new(initial_sp: u16) -> Self {
        Self {
            initial_sp,
            low_sp: initial_sp,
        }
    }

    fn observe(&mut self, sp: u16) {
        self.low_sp = self.low_sp.min(sp);
    }

    fn used(self) -> u16 {
        self.initial_sp.saturating_sub(self.low_sp)
    }
}

impl RuntimeMaterializerMonitor {
    fn new(symbols: &HashMap<String, (u8, u16)>) -> Self {
        let hooks = MATERIALIZER_HOOK_SYMBOLS
            .iter()
            .filter_map(|name| {
                symbols.get(*name).map(|(_, addr)| RuntimeMaterializerHook {
                    name: (*name).to_string(),
                    addr: *addr,
                })
            })
            .collect();
        Self {
            hooks,
            active: None,
            calls_on: 0,
            calls_off: 0,
            vdp_writes_on: 0,
            vdp_writes_off: 0,
            first_on_call: None,
            first_on_vdp: None,
        }
    }

    fn observe_pc(&mut self, step: usize, pc: u16, sp: u16, bus: &SmsBus) {
        let Some(hook) = self.hooks.iter().find(|hook| hook.addr == pc).cloned() else {
            return;
        };
        let render = current_render_state(bus);
        match render {
            RenderState::On => {
                self.calls_on += 1;
                if self.first_on_call.is_none() {
                    self.first_on_call =
                        Some(RuntimeMaterializerOffense::new(step, pc, &hook.name, bus));
                }
            }
            RenderState::Off => self.calls_off += 1,
        }
        self.active = Some(ActiveMaterializerHook {
            name: hook.name,
            entry_sp: sp,
        });
    }

    fn observe_vdp_writes(
        &mut self,
        step: usize,
        pc: u16,
        bus: &SmsBus,
        writes: u32,
        render: RenderState,
    ) {
        if writes == 0 || self.active.is_none() {
            return;
        }
        match render {
            RenderState::On => {
                self.vdp_writes_on = self.vdp_writes_on.saturating_add(writes);
                if self.first_on_vdp.is_none() {
                    let symbol = self
                        .active
                        .as_ref()
                        .map(|active| active.name.as_str())
                        .unwrap_or("unknown");
                    self.first_on_vdp =
                        Some(RuntimeMaterializerOffense::new(step, pc, symbol, bus));
                }
            }
            RenderState::Off => {
                self.vdp_writes_off = self.vdp_writes_off.saturating_add(writes);
            }
        }
    }

    fn observe_after_step(&mut self, op: u8, sp_after: u16) {
        if op != 0xC9 {
            return;
        }
        if self
            .active
            .as_ref()
            .is_some_and(|active| sp_after > active.entry_sp)
        {
            self.active = None;
        }
    }
}

impl RuntimeMaterializerOffense {
    fn new(step: usize, pc: u16, symbol: &str, bus: &SmsBus) -> Self {
        Self {
            step,
            pc,
            symbol: symbol.to_string(),
            cb09: bus.ram[0x0B09],
            vdp_reg1: bus.vdp_regs[1],
        }
    }
}

#[derive(Clone)]
struct SmsBus {
    rom: Vec<u8>,
    /// Current bank mapped into each slot. slot[0] = bank for $0000-$3FFF, etc.
    slot_bank: [u8; 3],
    /// Standard Sega mapper control register ($FFFC).
    mapper_control: u8,
    /// Standard Sega mapper SRAM: two 16 KiB banks, mapped into slot 2 when
    /// mapper_control bit 3 is set.
    cart_ram: [u8; CART_RAM_SIZE],
    cart_ram_reads: u32,
    cart_ram_writes: u32,
    ram: [u8; RAM_SIZE],
    /// Log of (frame, op, port, value).
    io_log: Vec<String>,
    /// VDP status reads. Real frame/line IRQ kind is supplied through
    /// `vdp_status_override` when the tracer injects an interrupt; fallback
    /// toggling keeps non-IRQ polling loops from stalling.
    vdp_status_reads: u32,
    /// Status byte returned by the next $BF read, used to distinguish injected
    /// frame IRQs (bit 7 set) from line IRQs (bit 7 clear).
    vdp_status_override: Option<u8>,
    display_enabled_edge: bool,
    psg_writes: u64,
    psg_log: Vec<u8>,
    /// VRAM 16 KiB and CRAM 32 B (for inspection if needed).
    vram: [u8; 0x4000],
    cram: [u8; 0x20],
    /// Last written VDP register values, for framebuffer interpretation.
    vdp_regs: [u8; 16],
    /// Approximate per-frame line-scroll split for checkpoint rendering:
    /// (screen line, top/pre reg8, top/pre reg9). After the injected line IRQ
    /// runs, the live VDP regs hold the bottom/post scroll values.
    render_scroll_split: Option<(usize, u8, u8)>,
    /// VDP address latch state (toggles between low/high byte).
    vdp_addr_high: u8,
    vdp_addr_low: u8,
    vdp_addr_latched: bool,
    /// VDP control / data port mode: 0 = next data write goes to VRAM, 1 = CRAM.
    vdp_code: u8,
    /// Total number of VRAM writes (diagnostic).
    vram_writes: u32,
    /// Total number of CRAM writes (diagnostic).
    cram_writes: u32,
    /// Total number of VDP data-port writes.
    vdp_data_writes: u32,
    /// Total number of VDP control-port writes.
    vdp_control_writes: u32,
    /// Total number of controller port reads.
    controller_reads: u32,
    /// Bitmask of NES nametable pages observed writing each folded SMS cell.
    nt_fold_cell_pages: [u8; 1024],
    /// Trace-only reconstruction of raw NES CIRAM writes observed at the
    /// `rt_ppu_write` call boundary, interpreted with vertical mirroring.
    nt_trace_ciram_vertical: [u8; 0x800],
    /// Same trace-only raw CIRAM reconstruction, interpreted with horizontal
    /// mirroring. Keeping both avoids baking SMB's mirroring mode into the
    /// diagnostic and lets future profiles compare the expected mode.
    nt_trace_ciram_horizontal: [u8; 0x800],
    /// Count of trace-observed PPUDATA writes into $2000-$2FFF.
    nt_trace_ciram_writes: u32,
    /// Count of trace-observed PPUDATA writes into tile bytes ($2000-$2FBF).
    nt_trace_ciram_tile_writes: u32,
    /// Count of trace-observed PPUDATA writes into attribute bytes.
    nt_trace_ciram_attr_writes: u32,
    nt_raw_tile_writes_on: u32,
    nt_raw_tile_writes_off: u32,
    nt_raw_attr_writes_on: u32,
    nt_raw_attr_writes_off: u32,
    nt_raw_frame_tile_writes: u32,
    nt_raw_frame_attr_writes: u32,
    nt_raw_max_frame_tile_writes: u32,
    nt_raw_max_frame_attr_writes: u32,
    nt_raw_max_frame_total_writes: u32,
    nt_raw_current_burst: u32,
    nt_raw_max_burst: u32,
    nt_raw_max_burst_frame: usize,
    nt_raw_max_burst_step: usize,
    bgv_runtime_tile_shadow_writes_on: u32,
    bgv_runtime_tile_shadow_writes_off: u32,
    bgv_runtime_attr_recompute_cells_on: u32,
    bgv_runtime_attr_recompute_cells_off: u32,
    bgv_runtime_frame_tile_shadow_writes: u32,
    bgv_runtime_frame_attr_recompute_cells: u32,
    bgv_runtime_max_frame_tile_shadow_writes: u32,
    bgv_runtime_max_frame_attr_recompute_cells: u32,
    bgv_runtime_max_frame_pressure: u32,
    bgv_runtime_current_burst_pressure: u32,
    bgv_runtime_max_burst_pressure: u32,
    bgv_runtime_max_burst_frame: usize,
    bgv_runtime_max_burst_step: usize,
    /// Trace-only source-tile view of the current folded SMS nametable. Unlike
    /// SMS VRAM nametable bytes, these are original NES tile IDs, so they are a
    /// safe comparison target for dry source-space projection diagnostics.
    nt_trace_folded_source_tiles: [u8; 0x400],
    nt_trace_folded_source_tile_seen: [bool; 0x400],
    /// Count of trace-observed tile writes into the folded source-tile shadow.
    nt_trace_folded_source_tile_writes: u32,
    /// Trace-only materializer dirty tile sets, indexed by mirrored CIRAM tile
    /// byte. Bit 0 = direct tile write, bit 1 = attribute write covering cell.
    nt_materializer_dirty_vertical: [u8; 0x800],
    nt_materializer_dirty_horizontal: [u8; 0x800],
    d3xx_tile_dirty_bitmap_vertical: [u8; D3XX_TILE_DIRTY_BITMAP_BYTES],
    d3xx_tile_dirty_bitmap_horizontal: [u8; D3XX_TILE_DIRTY_BITMAP_BYTES],
    d3xx_tile_dirty_frame_vertical: [u8; D3XX_TILE_DIRTY_BITMAP_BYTES],
    d3xx_tile_dirty_frame_horizontal: [u8; D3XX_TILE_DIRTY_BITMAP_BYTES],
    d3xx_tile_dirty_max_frame_vertical_bits: u32,
    d3xx_tile_dirty_max_frame_horizontal_bits: u32,
    d3xx_attr_dirty_bitmap_vertical: [u8; D3XX_ATTR_DIRTY_BITMAP_BYTES],
    d3xx_attr_dirty_bitmap_horizontal: [u8; D3XX_ATTR_DIRTY_BITMAP_BYTES],
    d3xx_attr_dirty_frame_vertical: [u8; D3XX_ATTR_DIRTY_BITMAP_BYTES],
    d3xx_attr_dirty_frame_horizontal: [u8; D3XX_ATTR_DIRTY_BITMAP_BYTES],
    d3xx_attr_dirty_max_frame_vertical_bits: u32,
    d3xx_attr_dirty_max_frame_horizontal_bits: u32,
    /// Tile writes where the folded $CC00 subpalette disagrees with the compact
    /// attribute shadow, interpreted as horizontal NES mirroring.
    nt_explicit_s_mismatch_horizontal: u32,
    nt_explicit_s_mismatch_horizontal_examples: Vec<NtExplicitSExample>,
    /// Same diagnostic, interpreted as vertical NES mirroring.
    nt_explicit_s_mismatch_vertical: u32,
    nt_explicit_s_mismatch_vertical_examples: Vec<NtExplicitSExample>,
    /// Raw SMS port $DC value for controller 1. Active-low; default $FF = released.
    controller_port_dc: u8,
    ram_migration_counts: [[u32; 4]; 3],
    ram_migration_frame_accesses: [u32; 3],
    ram_migration_max_frame_accesses: [u32; 3],
    ram_migration_pc_counts: [HashMap<u16, u32>; 6],
    nt_folded_s_compact_available: bool,
    d300_compact_store_range: Option<(u16, u16)>,
    d300_pending_read: Option<D300PendingRead>,
    d300_true_reads: u32,
    d300_rmw_reads: u32,
    d300_true_read_pc_counts: HashMap<u16, u32>,
    d300_rmw_read_pc_counts: HashMap<u16, u32>,
    d3xx_storage_counts: [[u32; 4]; 2],
    d3xx_storage_pc_counts: [HashMap<u16, u32>; 4],
    cc_subpal_range: Option<(u16, u16)>,
    cc_attr_write_range: Option<(u16, u16)>,
    cc_init_clear_range: Option<(u16, u16)>,
    cc_folded_s_counts: [[u32; 2]; 6],
    cc_folded_s_frame_reads: u32,
    cc_folded_s_frame_writes: u32,
    cc_folded_s_max_frame_reads: u32,
    cc_folded_s_max_frame_writes: u32,
    cc_folded_s_read_pc_counts: HashMap<u16, u32>,
    cc_folded_s_write_pc_counts: HashMap<u16, u32>,
    /// Log of every mapper write (port, value). Lets the trace report
    /// when a translated routine surprises us by re-banking a slot.
    bank_writes: Vec<(u16, u8)>,
    /// Per-address write tap. If `watch_addr` is set, every write to it
    /// pushes (step, value) into `watch_log`. Use to confirm whether a
    /// specific RAM byte ever gets touched.
    watch_addr: Option<u16>,
    watch_write_range: Option<(u16, u16)>,
    watch_log: Vec<WatchWrite>,
    /// Per-address read tap. If `watch_read_addr` is set, every RAM read from
    /// it pushes (step, value) into `watch_read_log`. This is useful for
    /// collision paths that indirect through block buffers.
    watch_read_addr: Option<u16>,
    watch_read_range: Option<(u16, u16)>,
    watch_read_log: Vec<WatchWrite>,
    watch_step: usize,
    watch_pc: u16,
    watch_bank1: u8,
    watch_sp: u16,
    watch_ret: u16,
}

#[derive(Clone, Copy, Debug)]
struct WatchWrite {
    step: usize,
    addr: u16,
    pc: u16,
    bank1: u8,
    sp: u16,
    ret: u16,
    value: u8,
    ppage: u8,
    px: u8,
    ypage: u8,
    py: u8,
    player_state: u8,
    x_shadow: u8,
    y_shadow: u8,
    zp02: u8,
    zp03: u8,
    zp04: u8,
    zp05: u8,
    zp06: u8,
    zp07: u8,
    zp08: u8,
    yspeed: u8,
    eb: u8,
    vertical_force: u8,
}

#[derive(Clone, Debug)]
struct NtExplicitSExample {
    ppu_addr: u16,
    sms_addr: u16,
    folded_s: u8,
    explicit_s: u8,
    attr_index: usize,
    attr_byte: u8,
}

#[derive(Clone, Copy, Debug)]
struct WatchExecHit {
    step: usize,
    pc: u16,
    bank1: u8,
    sp: u16,
    ret: u16,
    op: u8,
    a: u8,
    f: u8,
    p_shadow: u8,
    x_shadow: u8,
    y_shadow: u8,
    zp00: u8,
    zp02: u8,
    zp03: u8,
    zp04: u8,
    zp05: u8,
    zp06: u8,
    zp07: u8,
    zp08: u8,
    ppage: u8,
    px: u8,
    ypage: u8,
    py: u8,
    yspeed: u8,
    eb: u8,
    vertical_force: u8,
    area_obj_dispatch: u8,
}

#[derive(Clone)]
struct FallSnapshot {
    step: usize,
    frame: usize,
    ram: [u8; RAM_SIZE],
    recent_reads: Vec<WatchWrite>,
    recent_writes: Vec<WatchWrite>,
}

impl SmsBus {
    fn new(rom: Vec<u8>, controller_port_dc: u8) -> Self {
        Self {
            rom,
            slot_bank: [0, 1, 2],
            mapper_control: 0,
            cart_ram: [0; CART_RAM_SIZE],
            cart_ram_reads: 0,
            cart_ram_writes: 0,
            ram: [0; RAM_SIZE],
            io_log: Vec::new(),
            vdp_status_reads: 0,
            vdp_status_override: None,
            display_enabled_edge: false,
            psg_writes: 0,
            psg_log: Vec::new(),
            vram: if real_pacing() { [0xFF; 0x4000] } else { [0; 0x4000] },
            cram: [0; 0x20],
            vdp_regs: [0; 16],
            render_scroll_split: None,
            vdp_addr_high: 0,
            vdp_addr_low: 0,
            bank_writes: Vec::new(),
            watch_addr: std::env::var("SMS_WATCH_ADDR")
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            watch_write_range: std::env::var("SMS_WATCH_WRITE_RANGE")
                .ok()
                .and_then(|s| parse_addr_range(&s)),
            watch_log: Vec::new(),
            watch_read_addr: std::env::var("SMS_WATCH_READ_ADDR")
                .ok()
                .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            watch_read_range: std::env::var("SMS_WATCH_READ_RANGE")
                .ok()
                .and_then(|s| parse_addr_range(&s)),
            watch_read_log: Vec::new(),
            watch_step: 0,
            watch_pc: 0,
            watch_bank1: 1,
            watch_sp: 0,
            watch_ret: 0,
            vdp_addr_latched: false,
            vdp_code: 0,
            vram_writes: 0,
            cram_writes: 0,
            vdp_data_writes: 0,
            vdp_control_writes: 0,
            controller_reads: 0,
            nt_fold_cell_pages: [0; 1024],
            nt_trace_ciram_vertical: [0; 0x800],
            nt_trace_ciram_horizontal: [0; 0x800],
            nt_trace_ciram_writes: 0,
            nt_trace_ciram_tile_writes: 0,
            nt_trace_ciram_attr_writes: 0,
            nt_raw_tile_writes_on: 0,
            nt_raw_tile_writes_off: 0,
            nt_raw_attr_writes_on: 0,
            nt_raw_attr_writes_off: 0,
            nt_raw_frame_tile_writes: 0,
            nt_raw_frame_attr_writes: 0,
            nt_raw_max_frame_tile_writes: 0,
            nt_raw_max_frame_attr_writes: 0,
            nt_raw_max_frame_total_writes: 0,
            nt_raw_current_burst: 0,
            nt_raw_max_burst: 0,
            nt_raw_max_burst_frame: 0,
            nt_raw_max_burst_step: 0,
            bgv_runtime_tile_shadow_writes_on: 0,
            bgv_runtime_tile_shadow_writes_off: 0,
            bgv_runtime_attr_recompute_cells_on: 0,
            bgv_runtime_attr_recompute_cells_off: 0,
            bgv_runtime_frame_tile_shadow_writes: 0,
            bgv_runtime_frame_attr_recompute_cells: 0,
            bgv_runtime_max_frame_tile_shadow_writes: 0,
            bgv_runtime_max_frame_attr_recompute_cells: 0,
            bgv_runtime_max_frame_pressure: 0,
            bgv_runtime_current_burst_pressure: 0,
            bgv_runtime_max_burst_pressure: 0,
            bgv_runtime_max_burst_frame: 0,
            bgv_runtime_max_burst_step: 0,
            nt_trace_folded_source_tiles: [0; 0x400],
            nt_trace_folded_source_tile_seen: [false; 0x400],
            nt_trace_folded_source_tile_writes: 0,
            nt_materializer_dirty_vertical: [0; 0x800],
            nt_materializer_dirty_horizontal: [0; 0x800],
            d3xx_tile_dirty_bitmap_vertical: [0; D3XX_TILE_DIRTY_BITMAP_BYTES],
            d3xx_tile_dirty_bitmap_horizontal: [0; D3XX_TILE_DIRTY_BITMAP_BYTES],
            d3xx_tile_dirty_frame_vertical: [0; D3XX_TILE_DIRTY_BITMAP_BYTES],
            d3xx_tile_dirty_frame_horizontal: [0; D3XX_TILE_DIRTY_BITMAP_BYTES],
            d3xx_tile_dirty_max_frame_vertical_bits: 0,
            d3xx_tile_dirty_max_frame_horizontal_bits: 0,
            d3xx_attr_dirty_bitmap_vertical: [0; D3XX_ATTR_DIRTY_BITMAP_BYTES],
            d3xx_attr_dirty_bitmap_horizontal: [0; D3XX_ATTR_DIRTY_BITMAP_BYTES],
            d3xx_attr_dirty_frame_vertical: [0; D3XX_ATTR_DIRTY_BITMAP_BYTES],
            d3xx_attr_dirty_frame_horizontal: [0; D3XX_ATTR_DIRTY_BITMAP_BYTES],
            d3xx_attr_dirty_max_frame_vertical_bits: 0,
            d3xx_attr_dirty_max_frame_horizontal_bits: 0,
            nt_explicit_s_mismatch_horizontal: 0,
            nt_explicit_s_mismatch_horizontal_examples: Vec::new(),
            nt_explicit_s_mismatch_vertical: 0,
            nt_explicit_s_mismatch_vertical_examples: Vec::new(),
            controller_port_dc,
            ram_migration_counts: [[0; 4]; 3],
            ram_migration_frame_accesses: [0; 3],
            ram_migration_max_frame_accesses: [0; 3],
            ram_migration_pc_counts: std::array::from_fn(|_| HashMap::new()),
            nt_folded_s_compact_available: true,
            d300_compact_store_range: None,
            d300_pending_read: None,
            d300_true_reads: 0,
            d300_rmw_reads: 0,
            d300_true_read_pc_counts: HashMap::new(),
            d300_rmw_read_pc_counts: HashMap::new(),
            d3xx_storage_counts: [[0; 4]; 2],
            d3xx_storage_pc_counts: std::array::from_fn(|_| HashMap::new()),
            cc_subpal_range: None,
            cc_attr_write_range: None,
            cc_init_clear_range: None,
            cc_folded_s_counts: [[0; 2]; 6],
            cc_folded_s_frame_reads: 0,
            cc_folded_s_frame_writes: 0,
            cc_folded_s_max_frame_reads: 0,
            cc_folded_s_max_frame_writes: 0,
            cc_folded_s_read_pc_counts: HashMap::new(),
            cc_folded_s_write_pc_counts: HashMap::new(),
        }
    }
    fn rom_byte(&self, bank: u8, offset: u16) -> u8 {
        let i = bank as usize * BANK_SIZE + offset as usize;
        *self.rom.get(i).unwrap_or(&0xFF)
    }

    /// Apply a Sega mapper register write. `addr` is the canonical register
    /// address ($FFFC-$FFFF); callers translate mirror addresses first.
    fn apply_mapper_write(&mut self, addr: u16, value: u8) {
        match addr {
            0xFFFC => {
                self.mapper_control = value;
                self.io_log.push(format!("mapper ctrl=${value:02X}"));
                self.bank_writes.push((0xFFFC, value));
            }
            0xFFFD => {
                self.slot_bank[0] = value;
                self.bank_writes.push((0xFFFD, value));
            }
            0xFFFE => {
                self.slot_bank[1] = value;
                self.bank_writes.push((0xFFFE, value));
            }
            0xFFFF => {
                self.slot_bank[2] = value;
                self.bank_writes.push((0xFFFF, value));
            }
            _ => {}
        }
    }

    fn slot2_cart_ram_offset(&self, addr: u16) -> Option<usize> {
        if !(0x8000..=0xBFFF).contains(&addr) || self.mapper_control & 0x08 == 0 {
            return None;
        }
        let bank_base = if self.mapper_control & 0x04 != 0 {
            BANK_SIZE
        } else {
            0
        };
        Some(bank_base + usize::from(addr - 0x8000))
    }

    fn watches_read(&self, addr: u16) -> bool {
        Some(addr) == self.watch_read_addr
            || self
                .watch_read_range
                .is_some_and(|(start, end)| (start..=end).contains(&addr))
    }

    fn watches_write(&self, addr: u16) -> bool {
        Some(addr) == self.watch_addr
            || self
                .watch_write_range
                .is_some_and(|(start, end)| (start..=end).contains(&addr))
    }

    fn watch_entry(&self, addr: u16, value: u8) -> WatchWrite {
        WatchWrite {
            step: self.watch_step,
            addr,
            pc: self.watch_pc,
            bank1: self.watch_bank1,
            sp: self.watch_sp,
            ret: self.watch_ret,
            value,
            ppage: self.ram[0x006D],
            px: self.ram[0x0086],
            ypage: self.ram[0x00B5],
            py: self.ram[0x00CE],
            player_state: self.ram[0x000E],
            x_shadow: self.ram[0x0B00],
            y_shadow: self.ram[0x0B01],
            zp02: self.ram[0x0002],
            zp03: self.ram[0x0003],
            zp04: self.ram[0x0004],
            zp05: self.ram[0x0005],
            zp06: self.ram[0x0006],
            zp07: self.ram[0x0007],
            zp08: self.ram[0x0008],
            yspeed: self.ram[0x009F],
            eb: self.ram[0x00EB],
            vertical_force: self.ram[0x070E],
        }
    }

    fn record_ram_migration_access(&mut self, physical_addr: u16, kind: RamMigrationAccessKind) {
        self.record_d3xx_storage_access(physical_addr, kind);
        let Some(range) = ram_migration_range_for_physical_addr(physical_addr) else {
            if !self.is_compact_store_pc(self.watch_pc) {
                self.flush_d300_pending_true_read();
            }
            return;
        };
        let range_idx = range.index();
        let count_idx = match (kind, current_render_state(self)) {
            (RamMigrationAccessKind::Read, RenderState::On) => 0,
            (RamMigrationAccessKind::Read, RenderState::Off) => 1,
            (RamMigrationAccessKind::Write, RenderState::On) => 2,
            (RamMigrationAccessKind::Write, RenderState::Off) => 3,
        };
        self.ram_migration_counts[range_idx][count_idx] += 1;
        self.ram_migration_frame_accesses[range_idx] += 1;
        let pc_idx = range_idx * 2
            + match kind {
                RamMigrationAccessKind::Read => 0,
                RamMigrationAccessKind::Write => 1,
            };
        *self.ram_migration_pc_counts[pc_idx]
            .entry(self.watch_pc)
            .or_insert(0) += 1;

        if range == RamMigrationRange::CcFoldedS {
            self.record_cc_folded_s_dependency_access(kind);
        }

        if range == RamMigrationRange::D300CompactS {
            self.record_d300_dependency_access(physical_addr, kind);
        } else {
            if !self.is_compact_store_pc(self.watch_pc) {
                self.flush_d300_pending_true_read();
            }
        }
    }

    fn record_d3xx_storage_access(&mut self, physical_addr: u16, kind: RamMigrationAccessKind) {
        let Some(range) = d3xx_storage_range_for_physical_addr(physical_addr) else {
            return;
        };
        let count_idx = match (kind, current_render_state(self)) {
            (RamMigrationAccessKind::Read, RenderState::On) => 0,
            (RamMigrationAccessKind::Read, RenderState::Off) => 1,
            (RamMigrationAccessKind::Write, RenderState::On) => 2,
            (RamMigrationAccessKind::Write, RenderState::Off) => 3,
        };
        self.d3xx_storage_counts[range.index()][count_idx] += 1;
        let pc_idx = range.index() * 2
            + match kind {
                RamMigrationAccessKind::Read => 0,
                RamMigrationAccessKind::Write => 1,
            };
        *self.d3xx_storage_pc_counts[pc_idx]
            .entry(self.watch_pc)
            .or_insert(0) += 1;
    }

    fn record_cc_folded_s_dependency_access(&mut self, kind: RamMigrationAccessKind) {
        let render_idx = match current_render_state(self) {
            RenderState::On => 0,
            RenderState::Off => 1,
        };
        let pc = self.watch_pc;
        let count_idx = match kind {
            RamMigrationAccessKind::Read if self.is_cc_subpal_pc(pc) => 0,
            RamMigrationAccessKind::Read if self.is_cc_attr_write_pc(pc) => 1,
            RamMigrationAccessKind::Write if self.is_cc_attr_write_pc(pc) => 2,
            RamMigrationAccessKind::Write if self.is_cc_init_clear_pc(pc) => 3,
            RamMigrationAccessKind::Read => 4,
            RamMigrationAccessKind::Write => 5,
        };
        self.cc_folded_s_counts[count_idx][render_idx] += 1;
        match kind {
            RamMigrationAccessKind::Read => {
                self.cc_folded_s_frame_reads += 1;
                *self.cc_folded_s_read_pc_counts.entry(pc).or_insert(0) += 1;
            }
            RamMigrationAccessKind::Write => {
                self.cc_folded_s_frame_writes += 1;
                *self.cc_folded_s_write_pc_counts.entry(pc).or_insert(0) += 1;
            }
        }
    }

    fn is_cc_subpal_pc(&self, pc: u16) -> bool {
        self.cc_subpal_range
            .is_some_and(|(start, end)| (start..=end).contains(&pc))
    }

    fn is_cc_attr_write_pc(&self, pc: u16) -> bool {
        self.cc_attr_write_range
            .is_some_and(|(start, end)| (start..=end).contains(&pc))
    }

    fn is_cc_init_clear_pc(&self, pc: u16) -> bool {
        self.cc_init_clear_range
            .is_some_and(|(start, end)| (start..=end).contains(&pc))
    }

    fn is_compact_store_pc(&self, pc: u16) -> bool {
        self.d300_compact_store_range
            .is_some_and(|(start, end)| (start..=end).contains(&pc))
    }

    fn record_d300_dependency_access(&mut self, addr: u16, kind: RamMigrationAccessKind) {
        match kind {
            RamMigrationAccessKind::Read => {
                self.flush_d300_pending_true_read();
                if self.is_compact_store_pc(self.watch_pc) {
                    self.d300_pending_read = Some(D300PendingRead {
                        addr,
                        pc: self.watch_pc,
                        render_state: current_render_state(self),
                    });
                } else {
                    self.record_d300_true_read(self.watch_pc);
                }
            }
            RamMigrationAccessKind::Write => {
                if self.d300_pending_read.as_ref().is_some_and(|pending| {
                    pending.addr == addr && self.is_compact_store_pc(self.watch_pc)
                }) {
                    let pending = self.d300_pending_read.take().expect("pending read");
                    self.d300_rmw_reads += 1;
                    *self.d300_rmw_read_pc_counts.entry(pending.pc).or_insert(0) += 1;
                    let _ = pending.render_state;
                } else {
                    self.flush_d300_pending_true_read();
                }
            }
        }
    }

    fn record_d300_true_read(&mut self, pc: u16) {
        self.d300_true_reads += 1;
        *self.d300_true_read_pc_counts.entry(pc).or_insert(0) += 1;
    }

    fn flush_d300_pending_true_read(&mut self) {
        if let Some(pending) = self.d300_pending_read.take() {
            self.record_d300_true_read(pending.pc);
        }
    }

    fn finish_ram_migration_frame(&mut self) {
        self.flush_d300_pending_true_read();
        for i in 0..3 {
            self.ram_migration_max_frame_accesses[i] =
                self.ram_migration_max_frame_accesses[i].max(self.ram_migration_frame_accesses[i]);
            self.ram_migration_frame_accesses[i] = 0;
        }
        self.cc_folded_s_max_frame_reads = self
            .cc_folded_s_max_frame_reads
            .max(self.cc_folded_s_frame_reads);
        self.cc_folded_s_max_frame_writes = self
            .cc_folded_s_max_frame_writes
            .max(self.cc_folded_s_frame_writes);
        self.cc_folded_s_frame_reads = 0;
        self.cc_folded_s_frame_writes = 0;
    }

    fn record_nt_fold_write(&mut self, vram_addr: u16) {
        let masked = vram_addr & 0x3FFF;
        if !(0x3700..=0x3EFF).contains(&masked) {
            return;
        }

        let ppu_addr = ((self.ram[0x0B0F] as u16) << 8) | self.ram[0x0B10] as u16;
        if !(0x2000..=0x2FBF).contains(&ppu_addr) || (ppu_addr & 0x03FF) >= 0x03C0 {
            return;
        }

        let cell = ((masked - 0x3700) / 2) as usize;
        if cell < self.nt_fold_cell_pages.len() {
            let page = ((ppu_addr - 0x2000) >> 10) as u8;
            self.nt_fold_cell_pages[cell] |= 1 << page;
        }

        if masked & 1 == 0 {
            self.record_nt_explicit_s_mismatch(masked, ppu_addr, false);
            self.record_nt_explicit_s_mismatch(masked, ppu_addr, true);
        }
    }

    #[cfg(test)]
    fn record_trace_ppu_write_call(&mut self, reg: u8, value: u8) {
        self.record_trace_ppu_write_call_at(reg, value, 0, 0);
    }

    fn record_trace_ppu_write_call_at(&mut self, reg: u8, value: u8, step: usize, frame: usize) {
        if reg != 7 {
            self.nt_raw_current_burst = 0;
            self.bgv_runtime_current_burst_pressure = 0;
            return;
        }

        let ppu_addr = ((self.ram[0x0B0F] as u16) << 8) | self.ram[0x0B10] as u16;
        let Some(kind) = nt_ppu_write_kind(ppu_addr) else {
            self.nt_raw_current_burst = 0;
            self.bgv_runtime_current_burst_pressure = 0;
            return;
        };

        let vertical = nt_ciram_index(ppu_addr, true);
        let horizontal = nt_ciram_index(ppu_addr, false);
        self.nt_trace_ciram_vertical[vertical] = value;
        self.nt_trace_ciram_horizontal[horizontal] = value;
        self.nt_trace_ciram_writes += 1;
        self.record_nt_raw_write_stats(kind, step, frame);
        self.record_bgv_runtime_recompute_pressure(kind, step, frame);
        if kind == NtWriteKind::Attr {
            self.nt_trace_ciram_attr_writes += 1;
            self.record_d3xx_attr_dirty_candidate(vertical, horizontal);
            self.mark_materializer_attr_dirty(ppu_addr);
        } else {
            self.nt_trace_ciram_tile_writes += 1;
            self.mark_materializer_tile_dirty(ppu_addr, 0x01);
            self.record_d3xx_tile_dirty_candidate(vertical, horizontal);
            let folded_cell = (ppu_addr.wrapping_sub(0x2000) & 0x03FF) as usize;
            self.nt_trace_folded_source_tiles[folded_cell] = value;
            self.nt_trace_folded_source_tile_seen[folded_cell] = true;
            self.nt_trace_folded_source_tile_writes += 1;
        }
    }

    fn record_nt_raw_write_stats(&mut self, kind: NtWriteKind, step: usize, frame: usize) {
        match (kind, current_render_state(self)) {
            (NtWriteKind::Tile, RenderState::On) => self.nt_raw_tile_writes_on += 1,
            (NtWriteKind::Tile, RenderState::Off) => self.nt_raw_tile_writes_off += 1,
            (NtWriteKind::Attr, RenderState::On) => self.nt_raw_attr_writes_on += 1,
            (NtWriteKind::Attr, RenderState::Off) => self.nt_raw_attr_writes_off += 1,
        }
        match kind {
            NtWriteKind::Tile => self.nt_raw_frame_tile_writes += 1,
            NtWriteKind::Attr => self.nt_raw_frame_attr_writes += 1,
        }
        self.nt_raw_current_burst += 1;
        if self.nt_raw_current_burst > self.nt_raw_max_burst {
            self.nt_raw_max_burst = self.nt_raw_current_burst;
            self.nt_raw_max_burst_frame = frame;
            self.nt_raw_max_burst_step = step;
        }
    }

    fn finish_nt_raw_frame(&mut self) {
        let total = self.nt_raw_frame_tile_writes + self.nt_raw_frame_attr_writes;
        self.nt_raw_max_frame_tile_writes = self
            .nt_raw_max_frame_tile_writes
            .max(self.nt_raw_frame_tile_writes);
        self.nt_raw_max_frame_attr_writes = self
            .nt_raw_max_frame_attr_writes
            .max(self.nt_raw_frame_attr_writes);
        self.nt_raw_max_frame_total_writes = self.nt_raw_max_frame_total_writes.max(total);
        self.nt_raw_frame_tile_writes = 0;
        self.nt_raw_frame_attr_writes = 0;
        self.nt_raw_current_burst = 0;
    }

    fn record_bgv_runtime_recompute_pressure(
        &mut self,
        kind: NtWriteKind,
        step: usize,
        frame: usize,
    ) {
        let pressure = match kind {
            // One byte a hypothetical folded runtime tile source shadow would
            // maintain for each folded nametable tile write.
            NtWriteKind::Tile => 1,
            // One NES attr byte covers a 4x4 tile block; without BGV_BSHADOW,
            // each covered cell would need source-tile -> CHR-map base lookup.
            NtWriteKind::Attr => 16,
        };
        match (kind, current_render_state(self)) {
            (NtWriteKind::Tile, RenderState::On) => self.bgv_runtime_tile_shadow_writes_on += 1,
            (NtWriteKind::Tile, RenderState::Off) => self.bgv_runtime_tile_shadow_writes_off += 1,
            (NtWriteKind::Attr, RenderState::On) => {
                self.bgv_runtime_attr_recompute_cells_on += pressure
            }
            (NtWriteKind::Attr, RenderState::Off) => {
                self.bgv_runtime_attr_recompute_cells_off += pressure
            }
        }
        match kind {
            NtWriteKind::Tile => self.bgv_runtime_frame_tile_shadow_writes += 1,
            NtWriteKind::Attr => self.bgv_runtime_frame_attr_recompute_cells += pressure,
        }
        self.bgv_runtime_current_burst_pressure += pressure;
        if self.bgv_runtime_current_burst_pressure > self.bgv_runtime_max_burst_pressure {
            self.bgv_runtime_max_burst_pressure = self.bgv_runtime_current_burst_pressure;
            self.bgv_runtime_max_burst_frame = frame;
            self.bgv_runtime_max_burst_step = step;
        }
    }

    fn finish_bgv_runtime_recompute_frame(&mut self) {
        let total =
            self.bgv_runtime_frame_tile_shadow_writes + self.bgv_runtime_frame_attr_recompute_cells;
        self.bgv_runtime_max_frame_tile_shadow_writes = self
            .bgv_runtime_max_frame_tile_shadow_writes
            .max(self.bgv_runtime_frame_tile_shadow_writes);
        self.bgv_runtime_max_frame_attr_recompute_cells = self
            .bgv_runtime_max_frame_attr_recompute_cells
            .max(self.bgv_runtime_frame_attr_recompute_cells);
        self.bgv_runtime_max_frame_pressure = self.bgv_runtime_max_frame_pressure.max(total);
        self.bgv_runtime_frame_tile_shadow_writes = 0;
        self.bgv_runtime_frame_attr_recompute_cells = 0;
        self.bgv_runtime_current_burst_pressure = 0;
    }

    fn mark_materializer_tile_dirty(&mut self, ppu_addr: u16, reason: u8) {
        if !(0x2000..=0x2FBF).contains(&ppu_addr) || (ppu_addr & 0x03FF) >= 0x03C0 {
            return;
        }
        let vertical = nt_ciram_index(ppu_addr, true);
        let horizontal = nt_ciram_index(ppu_addr, false);
        self.nt_materializer_dirty_vertical[vertical] |= reason;
        self.nt_materializer_dirty_horizontal[horizontal] |= reason;
    }

    fn d3xx_tile_dirty_bit(ciram: usize) -> Option<usize> {
        let page = ciram / 0x400;
        let offset = ciram & 0x03FF;
        if page >= 2 || offset >= 0x03C0 {
            return None;
        }
        Some(page * 0x03C0 + offset)
    }

    fn set_d3xx_tile_dirty_bit(bitmap: &mut [u8; D3XX_TILE_DIRTY_BITMAP_BYTES], ciram: usize) {
        if let Some(bit) = Self::d3xx_tile_dirty_bit(ciram) {
            bitmap[bit / 8] |= 1 << (bit & 7);
        }
    }

    fn d3xx_attr_dirty_bit(ciram: usize) -> Option<usize> {
        let page = ciram / 0x400;
        let offset = ciram & 0x03FF;
        if page >= 2 || !(0x03C0..=0x03FF).contains(&offset) {
            return None;
        }
        Some(page * 0x40 + (offset - 0x03C0))
    }

    fn set_d3xx_attr_dirty_bit(bitmap: &mut [u8; D3XX_ATTR_DIRTY_BITMAP_BYTES], ciram: usize) {
        if let Some(bit) = Self::d3xx_attr_dirty_bit(ciram) {
            bitmap[bit / 8] |= 1 << (bit & 7);
        }
    }

    fn record_d3xx_tile_dirty_candidate(&mut self, vertical: usize, horizontal: usize) {
        Self::set_d3xx_tile_dirty_bit(&mut self.d3xx_tile_dirty_bitmap_vertical, vertical);
        Self::set_d3xx_tile_dirty_bit(&mut self.d3xx_tile_dirty_bitmap_horizontal, horizontal);
        Self::set_d3xx_tile_dirty_bit(&mut self.d3xx_tile_dirty_frame_vertical, vertical);
        Self::set_d3xx_tile_dirty_bit(&mut self.d3xx_tile_dirty_frame_horizontal, horizontal);
    }

    fn record_d3xx_attr_dirty_candidate(&mut self, vertical: usize, horizontal: usize) {
        Self::set_d3xx_attr_dirty_bit(&mut self.d3xx_attr_dirty_bitmap_vertical, vertical);
        Self::set_d3xx_attr_dirty_bit(&mut self.d3xx_attr_dirty_bitmap_horizontal, horizontal);
        Self::set_d3xx_attr_dirty_bit(&mut self.d3xx_attr_dirty_frame_vertical, vertical);
        Self::set_d3xx_attr_dirty_bit(&mut self.d3xx_attr_dirty_frame_horizontal, horizontal);
    }

    fn finish_d3xx_tile_dirty_frame(&mut self) {
        self.d3xx_tile_dirty_max_frame_vertical_bits = self
            .d3xx_tile_dirty_max_frame_vertical_bits
            .max(bitmap_count_bits(&self.d3xx_tile_dirty_frame_vertical));
        self.d3xx_tile_dirty_max_frame_horizontal_bits = self
            .d3xx_tile_dirty_max_frame_horizontal_bits
            .max(bitmap_count_bits(&self.d3xx_tile_dirty_frame_horizontal));
        self.d3xx_tile_dirty_frame_vertical = [0; D3XX_TILE_DIRTY_BITMAP_BYTES];
        self.d3xx_tile_dirty_frame_horizontal = [0; D3XX_TILE_DIRTY_BITMAP_BYTES];
        self.d3xx_attr_dirty_max_frame_vertical_bits = self
            .d3xx_attr_dirty_max_frame_vertical_bits
            .max(bitmap_count_bits(&self.d3xx_attr_dirty_frame_vertical));
        self.d3xx_attr_dirty_max_frame_horizontal_bits = self
            .d3xx_attr_dirty_max_frame_horizontal_bits
            .max(bitmap_count_bits(&self.d3xx_attr_dirty_frame_horizontal));
        self.d3xx_attr_dirty_frame_vertical = [0; D3XX_ATTR_DIRTY_BITMAP_BYTES];
        self.d3xx_attr_dirty_frame_horizontal = [0; D3XX_ATTR_DIRTY_BITMAP_BYTES];
    }

    fn mark_materializer_attr_dirty(&mut self, ppu_addr: u16) {
        if !(0x2000..=0x2FFF).contains(&ppu_addr) || (ppu_addr & 0x03FF) < 0x03C0 {
            return;
        }
        let page_base = ppu_addr & !0x03FF;
        let attr = usize::from((ppu_addr & 0x003F) as u8);
        let base_row = (attr / 8) * 4;
        let base_col = (attr & 7) * 4;
        for row in base_row..(base_row + 4) {
            for col in base_col..(base_col + 4) {
                let tile_addr = page_base + (row * 32 + col) as u16;
                self.mark_materializer_tile_dirty(tile_addr, 0x02);
            }
        }
    }

    fn clear_materializer_dirty(&mut self) {
        self.nt_materializer_dirty_vertical.fill(0);
        self.nt_materializer_dirty_horizontal.fill(0);
    }

    fn record_nt_explicit_s_mismatch(
        &mut self,
        sms_addr: u16,
        ppu_addr: u16,
        vertical_mirroring: bool,
    ) {
        let Some(folded_s) = self.nt_folded_shadow_s(sms_addr) else {
            return;
        };
        let (explicit_s, attr_index, attr_byte) =
            self.nt_attr_shadow_s(ppu_addr, vertical_mirroring);
        if folded_s == explicit_s {
            return;
        }

        let example = NtExplicitSExample {
            ppu_addr,
            sms_addr,
            folded_s,
            explicit_s,
            attr_index,
            attr_byte,
        };

        let (count, examples) = if vertical_mirroring {
            (
                &mut self.nt_explicit_s_mismatch_vertical,
                &mut self.nt_explicit_s_mismatch_vertical_examples,
            )
        } else {
            (
                &mut self.nt_explicit_s_mismatch_horizontal,
                &mut self.nt_explicit_s_mismatch_horizontal_examples,
            )
        };
        *count += 1;
        if examples.len() < 6 {
            examples.push(example);
        }
    }

    fn nt_folded_shadow_s(&self, sms_addr: u16) -> Option<u8> {
        let masked = sms_addr & 0x3FFF;
        if !(0x3700..=0x3EFF).contains(&masked) {
            return None;
        }

        // Runtime _bgv_sub_palette maps the SMS high-byte nametable address to
        // folded shadow storage by adding $9500: $3701 -> $CC01.
        let shadow_addr = (masked | 1).wrapping_add(0x9500);
        (0xC000..=0xDFFF)
            .contains(&shadow_addr)
            .then(|| self.ram[(shadow_addr - 0xC000) as usize] & 0x03)
    }

    fn nt_attr_shadow_s(&self, ppu_addr: u16, vertical_mirroring: bool) -> (u8, usize, u8) {
        let ciram = nt_ciram_index(ppu_addr, vertical_mirroring) as u16;
        let ciram_page = (ciram >> 10) as usize;
        let tile_offset = (ciram & 0x03FF) as usize;
        let coarse_y = tile_offset / 32;
        let coarse_x = tile_offset % 32;
        let attr_index = ciram_page * 64 + (coarse_y / 4) * 8 + coarse_x / 4;
        let attr_byte = self.ram[0x0B80 + attr_index];
        let shift = ((coarse_y & 0x02) << 1) | (coarse_x & 0x02);
        ((attr_byte >> shift) & 0x03, attr_index, attr_byte)
    }
}

fn nt_ciram_index(ppu_addr: u16, vertical_mirroring: bool) -> usize {
    let raw = ppu_addr.wrapping_sub(0x2000) & 0x0FFF;
    if vertical_mirroring {
        (raw & 0x07FF) as usize
    } else {
        ((raw & 0x03FF) | ((raw & 0x0800) >> 1)) as usize
    }
}

fn parse_hex_addr(s: &str) -> Option<u16> {
    u16::from_str_radix(
        s.trim().trim_start_matches("0x").trim_start_matches('$'),
        16,
    )
    .ok()
}

fn parse_addr_range(s: &str) -> Option<(u16, u16)> {
    let (start, end) = s.split_once('-')?;
    Some((parse_hex_addr(start)?, parse_hex_addr(end)?))
}

fn parse_wla_symbol_line(line: &str) -> Option<(u16, String)> {
    let mut parts = line.split_whitespace();
    let addr = parts.next()?;
    let label = parts.next()?;
    let (_bank, addr) = addr.split_once(':')?;
    let addr = parse_hex_addr(addr)?;
    Some((addr, label.to_string()))
}

fn parse_wla_symbol_definition(line: &str) -> Option<(String, u8, u16)> {
    let mut parts = line.split_whitespace();
    let bank_addr = parts.next()?;
    let label = parts.next()?;
    let (bank, addr) = bank_addr.split_once(':')?;
    let bank = u8::from_str_radix(bank, 16).ok()?;
    let addr = parse_hex_addr(addr)?;
    Some((label.to_string(), bank, addr))
}

fn load_wla_symbols(path: &Path) -> HashMap<u16, Vec<String>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut symbols: HashMap<u16, Vec<String>> = HashMap::new();
    for line in text.lines() {
        if let Some((addr, label)) = parse_wla_symbol_line(line) {
            symbols.entry(addr).or_default().push(label);
        }
    }
    symbols
}

fn load_wla_symbol_defs(path: &Path) -> HashMap<String, (u8, u16)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut symbols = HashMap::new();
    for line in text.lines() {
        if let Some((label, bank, addr)) = parse_wla_symbol_definition(line) {
            symbols.entry(label).or_insert((bank, addr));
        }
    }
    symbols
}

fn detect_expected_mirroring(asm_path: &Path) -> ExpectedMirroring {
    let Ok(text) = std::fs::read_to_string(asm_path) else {
        return ExpectedMirroring::Unknown;
    };
    let vertical = text.contains("NES_MIRRORING_VERTICAL");
    let horizontal = text.contains("NES_MIRRORING_HORIZONTAL");
    match (vertical, horizontal) {
        (true, false) => ExpectedMirroring::Vertical,
        (false, true) => ExpectedMirroring::Horizontal,
        _ => ExpectedMirroring::Unknown,
    }
}

fn d300_compact_store_range(symbols: &HashMap<String, (u8, u16)>) -> Option<(u16, u16)> {
    let (_, start) = *symbols.get("_bgv_compact_s_store_high_addr")?;
    let end = symbols
        .get("_bgv_compact_done")
        .map(|(_, addr)| *addr)
        .filter(|end| *end >= start)
        .unwrap_or(start);
    Some((start, end))
}

fn symbol_range_exclusive_end(
    symbols: &HashMap<String, (u8, u16)>,
    start_label: &str,
    end_label: &str,
) -> Option<(u16, u16)> {
    let (_, start) = *symbols.get(start_label)?;
    let (_, end_exclusive) = *symbols.get(end_label)?;
    if end_exclusive <= start {
        return None;
    }
    Some((start, end_exclusive.saturating_sub(1)))
}

fn cc_subpal_range(symbols: &HashMap<String, (u8, u16)>) -> Option<(u16, u16)> {
    symbol_range_exclusive_end(symbols, "_bgv_sub_palette", "_bgv_base_addr")
}

fn cc_attr_write_range(symbols: &HashMap<String, (u8, u16)>) -> Option<(u16, u16)> {
    let (_, start) = *symbols.get("_chrmap_attr_write_one")?;
    let (_, end) = *symbols.get("_caw_done")?;
    if end < start {
        return None;
    }
    Some((start, end))
}

fn cc_init_clear_range(symbols: &HashMap<String, (u8, u16)>) -> Option<(u16, u16)> {
    let (_, start) = *symbols.get("mem_fill")?;
    let (_, loop_pc) = *symbols.get("_mem_fill_loop")?;
    Some((start.min(loop_pc), start.max(loop_pc)))
}

fn rom_byte_at_symbol(
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
    label: &str,
    offset: usize,
) -> Option<u8> {
    let (bank, addr) = *symbols.get(label)?;
    let slot_offset = usize::from(addr & 0x3FFF);
    let physical = usize::from(bank) * BANK_SIZE + slot_offset + offset;
    rom.get(physical).copied()
}

fn format_symbol_suffix(symbols: &HashMap<u16, Vec<String>>, addr: u16) -> String {
    let Some(labels) = symbols.get(&addr) else {
        return String::new();
    };
    if labels.is_empty() {
        return String::new();
    }
    let mut shown = labels.iter().take(2).cloned().collect::<Vec<_>>().join("/");
    if labels.len() > 2 {
        shown.push_str("/...");
    }
    format!(" {shown}")
}

impl Bus for SmsBus {
    fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.rom_byte(0, addr), // first 1 KB always bank 0
            0x0400..=0x3FFF => self.rom_byte(self.slot_bank[0], addr),
            0x4000..=0x7FFF => self.rom_byte(self.slot_bank[1], addr - 0x4000),
            0x8000..=0xBFFF => {
                if let Some(offset) = self.slot2_cart_ram_offset(addr) {
                    self.cart_ram_reads += 1;
                    self.cart_ram[offset]
                } else {
                    self.rom_byte(self.slot_bank[2], addr - 0x8000)
                }
            }
            0xC000..=0xDFFF => {
                let value = self.ram[(addr - 0xC000) as usize];
                self.record_ram_migration_access(addr, RamMigrationAccessKind::Read);
                if self.watches_read(addr) {
                    self.watch_read_log.push(self.watch_entry(addr, value));
                }
                value
            }
            0xE000..=0xFFFB => {
                let value = self.ram[(addr - 0xE000) as usize];
                self.record_ram_migration_access(addr - 0x2000, RamMigrationAccessKind::Read);
                if self.watches_read(addr) {
                    self.watch_read_log.push(self.watch_entry(addr, value));
                }
                value
            } // mirror
            0xFFFC => self.mapper_control,
            0xFFFD => self.slot_bank[0],
            0xFFFE => self.slot_bank[1],
            0xFFFF => self.slot_bank[2],
        }
    }
    fn write(&mut self, addr: u16, value: u8) {
        match addr {
            0x0000..=0xBFFF => {
                if let Some(offset) = self.slot2_cart_ram_offset(addr) {
                    self.cart_ram[offset] = value;
                    self.cart_ram_writes += 1;
                }
                // Other writes to ROM area are ignored (real SMS hardware).
            }
            0xC000..=0xDFFF => {
                self.ram[(addr - 0xC000) as usize] = value;
                self.record_ram_migration_access(addr, RamMigrationAccessKind::Write);
                if self.watches_write(addr) {
                    self.watch_log.push(self.watch_entry(addr, value));
                }
                // Sega mapper registers are RAM-mirrored: Mednafen (and the
                // usual SMS Plus lineage) applies mapper writes on the
                // $DFFC-$DFFF mirror as well as $FFFC-$FFFF. Model that here
                // so stack/data traffic near the top of RAM can't silently
                // pass in the trace harness while reprogramming banks on a
                // real emulator (2026-07-03 Mednafen black-screen root cause).
                if addr >= 0xDFFC {
                    self.apply_mapper_write(addr | 0x2000, value);
                }
            }
            0xE000..=0xFFFB => {
                self.ram[(addr - 0xE000) as usize] = value;
                self.record_ram_migration_access(addr - 0x2000, RamMigrationAccessKind::Write);
            }
            0xFFFC..=0xFFFF => {
                // Mapper registers also write through to the RAM mirror.
                self.ram[(addr - 0xE000) as usize] = value;
                self.apply_mapper_write(addr, value);
            }
        }
    }
    fn in_port(&mut self, port: u8) -> u8 {
        match port & 0xC1 {
            // VDP data port $BE — VRAM read through the address latch
            // (post-increment; the real HW read-buffer prefetch collapses
            // to a direct read for sequential access, which is what the
            // CHR-RAM variant readback does).
            0x80 => {
                let addr = ((self.vdp_addr_high as u16) << 8) | self.vdp_addr_low as u16;
                let v = self.vram[(addr & 0x3FFF) as usize];
                let new = addr.wrapping_add(1);
                self.vdp_addr_high = (new >> 8) as u8;
                self.vdp_addr_low = (new & 0xFF) as u8;
                v
            }
            // VDP status / control port $BF — return VBlank flag bit toggling.
            0x81 => {
                self.vdp_status_reads += 1;
                if let Some(status) = self.vdp_status_override.take() {
                    self.vdp_addr_latched = false;
                    return status;
                }
                // Make bit 7 toggle every 1000 reads so polling loops advance.
                let vblank = if (self.vdp_status_reads / 100) & 1 == 0 {
                    0x80
                } else {
                    0x00
                };
                // Reading also resets the VDP address latch toggle.
                self.vdp_addr_latched = false;
                vblank
            }
            // V counter $7E / H counter $7F.
            0x40 => 0xFF,
            // I/O port $DC/$DD (controllers) — all buttons released.
            0xC0 => {
                self.controller_reads += 1;
                self.controller_port_dc
            }
            _ => 0xFF,
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        // PSG writes ($40-$7F): log for audio-path diagnostics (F.5).
        if (0x40..=0x7F).contains(&port) {
            self.psg_writes += 1;
            if self.psg_log.len() < 4096 {
                self.psg_log.push(value);
            }
            return;
        }
        match port & 0xC1 {
            // VDP data port $BE. SMS VDP codes after address-set:
            //   0=VRAM read, 1=VRAM write, 2=register write, 3=CRAM write.
            0x80 => {
                self.vdp_data_writes += 1;
                let addr = ((self.vdp_addr_high as u16) << 8) | self.vdp_addr_low as u16;
                match self.vdp_code {
                    0 | 1 => {
                        // VRAM write (code 0 is read-mode but real HW writes
                        // anyway in some cases; SMB doesn't rely on this).
                        let masked = (addr & 0x3FFF) as usize;
                        self.vram[masked] = value;
                        self.vram_writes += 1;
                        self.record_nt_fold_write(addr);
                    }
                    3 => {
                        let masked = (addr & 0x1F) as usize;
                        self.cram[masked] = value;
                        self.cram_writes += 1;
                    }
                    _ => {}
                }
                let new = addr.wrapping_add(1);
                self.vdp_addr_high = (new >> 8) as u8;
                self.vdp_addr_low = (new & 0xFF) as u8;
            }
            // VDP control port $BF — address/register write protocol.
            0x81 => {
                self.vdp_control_writes += 1;
                if !self.vdp_addr_latched {
                    self.vdp_addr_low = value;
                    self.vdp_addr_latched = true;
                } else {
                    self.vdp_addr_high = value & 0x3F;
                    self.vdp_code = (value >> 6) & 3;
                    if self.vdp_code == 2 {
                        // VDP register write: low nibble of high byte selects register.
                        let reg = value & 0x0F;
                        let val = self.vdp_addr_low;
                        if reg == 1 && val & 0x40 != 0 && self.vdp_regs[1] & 0x40 == 0 {
                            self.display_enabled_edge = true;
                        }
                        self.vdp_regs[reg as usize] = val;
                        self.io_log.push(format!("vdp r{reg} = ${val:02X}"));
                    }
                    self.vdp_addr_latched = false;
                }
            }
            // Other I/O — ignore.
            _ => {}
        }
    }
}

fn parse_hex_u8(s: &str) -> Result<u8, String> {
    let trimmed = s.trim().trim_start_matches("0x").trim_start_matches('$');
    u8::from_str_radix(trimmed, 16).map_err(|_| format!("invalid hex byte: {s}"))
}

fn buttons_to_sms_port_dc(spec: &str) -> u8 {
    let mut port = 0xFFu8;
    for raw in spec.split(',') {
        let button = raw.trim().to_ascii_lowercase();
        if button.is_empty() {
            continue;
        }
        let bit = match button.as_str() {
            "up" => 0,
            "down" => 1,
            "left" => 2,
            "right" => 3,
            "a" | "b1" | "button1" | "select" => 4,
            "b" | "b2" | "button2" | "start" => 5,
            other => panic!("unknown --buttons entry: {other}"),
        };
        port &= !(1 << bit);
    }
    port
}

fn parse_button_event(spec: &str) -> Result<(usize, u8), String> {
    let (frame, buttons) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:buttons for --buttons-at-frame, got {spec}"))?;
    let frame = frame
        .parse::<usize>()
        .map_err(|_| format!("invalid frame in --buttons-at-frame: {frame}"))?;
    Ok((frame, buttons_to_sms_port_dc(buttons)))
}

#[derive(Debug, Clone, Copy)]
struct RamExpectation {
    addr: u16,
    value: u8,
}

fn parse_ram_expectation(spec: &str) -> Result<RamExpectation, String> {
    let (addr, value) = spec
        .split_once('=')
        .or_else(|| spec.split_once(':'))
        .ok_or_else(|| format!("expected ADDR=HEX for --expect-ram, got {spec}"))?;
    let addr = parse_hex_addr(addr).ok_or_else(|| format!("invalid RAM address: {addr}"))?;
    let value = parse_hex_u8(value)?;
    Ok(RamExpectation { addr, value })
}

fn ram_index(addr: u16) -> Option<usize> {
    let idx = match addr {
        0x0000..=0x1FFF => addr,
        0xC000..=0xDFFF => addr - 0xC000,
        0xE000..=0xFFFF => addr - 0xE000,
        _ => return None,
    };
    Some(usize::from(idx))
}

fn load_button_script(path: &str) -> Result<Vec<(usize, u8)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read button script {path}: {err}"))?;
    let mut events = Vec::new();
    for (line_idx, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        events.push(
            parse_button_event(line)
                .map_err(|err| format!("invalid button script {path}:{}: {err}", line_idx + 1))?,
        );
    }
    Ok(events)
}

#[derive(Debug, Clone)]
struct RouteCheckpoint {
    frame: usize,
    name: String,
}

fn parse_checkpoint_spec(spec: &str) -> Result<RouteCheckpoint, String> {
    let (frame, name) = spec
        .split_once(':')
        .or_else(|| spec.split_once('='))
        .ok_or_else(|| format!("expected FRAME:name for --checkpoint, got {spec}"))?;
    let frame = frame
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("invalid checkpoint frame: {frame}"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err("checkpoint name must not be empty".to_string());
    }
    Ok(RouteCheckpoint {
        frame,
        name: name.to_string(),
    })
}

fn load_checkpoint_script(path: &str) -> Result<Vec<RouteCheckpoint>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read checkpoint script {path}: {err}"))?;
    let mut checkpoints = Vec::new();
    for (line_idx, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        checkpoints.push(
            parse_checkpoint_spec(line).map_err(|err| {
                format!("invalid checkpoint script {path}:{}: {err}", line_idx + 1)
            })?,
        );
    }
    Ok(checkpoints)
}

fn checkpoint_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' {
            out.push(ch);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "checkpoint".to_string()
    } else {
        trimmed
    }
}

#[derive(Clone)]
struct SearchState {
    cpu: Cpu,
    bus: SmsBus,
    step: usize,
    next_irq_at: usize,
    irqs_fired: usize,
    next_button_event: usize,
}

#[derive(Debug)]
struct SearchResult {
    label: String,
    start: usize,
    duration: usize,
    max_aofs: u8,
    max_ppage: u8,
    max_px: u8,
    max_y_page: u8,
    final_state: u8,
    stop_flag: u8,
    first_stop_frame: Option<usize>,
}

#[derive(Debug)]
struct EndRouteResult {
    label: String,
    max_aofs: u8,
    max_ppage: u8,
    max_px: u8,
    min_y: u8,
    final_ppage: u8,
    final_px: u8,
    final_y: u8,
    final_state: u8,
    stop_flag: u8,
    flag_reads: usize,
    first_victory_frame: Option<usize>,
}

fn run_search_steps(
    state: &mut SearchState,
    max_steps: usize,
    button_events: &[(usize, u8)],
    target_frame: usize,
) -> Result<(), StepError> {
    for _ in 0..max_steps {
        if state.irqs_fired >= target_frame {
            break;
        }

        let pc = state.cpu.pc;
        state.bus.watch_step = state.step;
        state.bus.watch_pc = pc;
        state.bus.watch_bank1 = state.bus.slot_bank[1];
        state.bus.watch_sp = state.cpu.sp;

        if state.step >= state.next_irq_at && state.cpu.iff1 {
            while state.next_button_event < button_events.len()
                && state.irqs_fired >= button_events[state.next_button_event].0
            {
                state.bus.controller_port_dc = button_events[state.next_button_event].1;
                state.next_button_event += 1;
            }
            state.cpu.sp = state.cpu.sp.wrapping_sub(2);
            state.bus.write(state.cpu.sp, (state.cpu.pc & 0xFF) as u8);
            state
                .bus
                .write(state.cpu.sp.wrapping_add(1), (state.cpu.pc >> 8) as u8);
            state.bus.vdp_status_override = Some(0x80);
            state.cpu.pc = 0x0038;
            state.cpu.iff1 = false;
            state.cpu.iff2 = false;
            state.cpu.halted = false;
            state.irqs_fired += 1;
            state.next_irq_at = if real_pacing() { state.step + irq_period() } else { state.next_irq_at.saturating_add(IRQ_PERIOD) };
        }

        if state.cpu.halted {
            break;
        }
        state.cpu.step(&mut state.bus)?;
        state.step += 1;
    }
    Ok(())
}

fn run_late_route_search(rom_path: &PathBuf, base_events: &[(usize, u8)]) {
    let rom = std::fs::read(rom_path).expect("read rom");
    let mut state = SearchState {
        cpu: Cpu::new(),
        bus: SmsBus::new(rom, 0xFF),
        step: 0,
        next_irq_at: irq_period(),
        irqs_fired: 0,
        next_button_event: 0,
    };
    state.cpu.pc = 0x0000;
    state.cpu.sp = 0xDFF0;

    let snapshot_frame = 2050usize;
    run_search_steps(&mut state, 170_000_000, base_events, snapshot_frame)
        .expect("run to late-route snapshot");
    state.bus.io_log.clear();
    state.bus.bank_writes.clear();
    state.bus.watch_log.clear();
    state.bus.watch_read_log.clear();

    println!(
        "late-route snapshot frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} aofs={:02X} stop={:02X}",
        state.irqs_fired,
        state.bus.ram[0x006D],
        state.bus.ram[0x0086],
        state.bus.ram[0x00B5],
        state.bus.ram[0x00CE],
        state.bus.ram[0x071A],
        state.bus.ram[0x071C],
        state.bus.ram[0x072C],
        state.bus.ram[0x0723],
    );

    let mut results = Vec::new();
    for start in (2040usize..=2260).step_by(10) {
        for duration in [20usize, 35, 50, 70, 90] {
            let mut events = base_events.to_vec();
            events.push((start, buttons_to_sms_port_dc("right,a")));
            events.push((start + duration, buttons_to_sms_port_dc("right")));
            events.sort_by_key(|(frame, _)| *frame);

            let mut branch = state.clone();
            branch.next_button_event =
                events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
            let mut max_aofs = branch.bus.ram[0x072C];
            let mut max_ppage = branch.bus.ram[0x006D];
            let mut max_px = branch.bus.ram[0x0086];
            let mut max_y_page = branch.bus.ram[0x00B5];
            let mut first_stop_frame = None;

            while branch.irqs_fired < 3400 {
                let before = branch.irqs_fired;
                if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                    eprintln!("branch start={start} duration={duration} stopped: {err:?}");
                    break;
                }
                max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                let ppage = branch.bus.ram[0x006D];
                let px = branch.bus.ram[0x0086];
                if (ppage, px) > (max_ppage, max_px) {
                    max_ppage = ppage;
                    max_px = px;
                }
                max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
                if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                    first_stop_frame = Some(branch.irqs_fired);
                }
                if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                    break;
                }
                if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                    break;
                }
            }

            results.push(SearchResult {
                label: format!("one:{start}+{duration}"),
                start,
                duration,
                max_aofs,
                max_ppage,
                max_px,
                max_y_page,
                final_state: branch.bus.ram[0x000E],
                stop_flag: branch.bus.ram[0x0723],
                first_stop_frame,
            });
        }
    }

    let route_candidates = [
        (2040, 30, 2100, 30),
        (2040, 50, 2120, 40),
        (2040, 70, 2140, 50),
        (2060, 40, 2140, 40),
        (2060, 70, 2180, 40),
        (2060, 70, 2180, 60),
        (2060, 70, 2200, 50),
        (2080, 50, 2160, 50),
        (2080, 70, 2200, 50),
        (2100, 50, 2180, 60),
        (2100, 70, 2220, 50),
        (2120, 50, 2200, 60),
        (2140, 50, 2220, 60),
        (2060, 70, 2180, 60),
        (2060, 70, 2180, 60),
    ];
    for (first_start, first_duration, second_start, second_duration) in route_candidates {
        let mut events = base_events.to_vec();
        events.push((first_start, buttons_to_sms_port_dc("right,a")));
        events.push((
            first_start + first_duration,
            buttons_to_sms_port_dc("right"),
        ));
        events.push((second_start, buttons_to_sms_port_dc("right,a")));
        events.push((
            second_start + second_duration,
            buttons_to_sms_port_dc("right"),
        ));
        events.sort_by_key(|(frame, _)| *frame);

        let mut branch = state.clone();
        branch.next_button_event = events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
        let mut max_aofs = branch.bus.ram[0x072C];
        let mut max_ppage = branch.bus.ram[0x006D];
        let mut max_px = branch.bus.ram[0x0086];
        let mut max_y_page = branch.bus.ram[0x00B5];
        let mut first_stop_frame = None;
        while branch.irqs_fired < 3400 {
            let before = branch.irqs_fired;
            if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                eprintln!("branch route stopped: {err:?}");
                break;
            }
            max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
            let ppage = branch.bus.ram[0x006D];
            let px = branch.bus.ram[0x0086];
            if (ppage, px) > (max_ppage, max_px) {
                max_ppage = ppage;
                max_px = px;
            }
            max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
            if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                first_stop_frame = Some(branch.irqs_fired);
            }
            if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                break;
            }
            if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                break;
            }
        }

        results.push(SearchResult {
            label: format!("two:{first_start}+{first_duration},{second_start}+{second_duration}"),
            start: first_start,
            duration: first_duration,
            max_aofs,
            max_ppage,
            max_px,
            max_y_page,
            final_state: branch.bus.ram[0x000E],
            stop_flag: branch.bus.ram[0x0723],
            first_stop_frame,
        });
    }

    let triple_candidates = [
        (2060, 70, 2180, 40, 2260, 40),
        (2060, 70, 2180, 40, 2280, 50),
        (2060, 70, 2180, 60, 2260, 40),
        (2060, 70, 2180, 60, 2280, 60),
        (2080, 70, 2200, 50, 2280, 50),
        (2080, 70, 2200, 50, 2300, 60),
        (2100, 70, 2220, 50, 2280, 50),
        (2100, 70, 2220, 50, 2300, 60),
        (2100, 90, 2220, 60, 2300, 60),
        (2120, 70, 2220, 60, 2300, 70),
        (2040, 70, 2140, 50, 2220, 60),
        (2040, 90, 2160, 60, 2240, 60),
    ];
    for (s1, d1, s2, d2, s3, d3) in triple_candidates {
        let mut events = base_events.to_vec();
        for (start, duration) in [(s1, d1), (s2, d2), (s3, d3)] {
            events.push((start, buttons_to_sms_port_dc("right,a")));
            events.push((start + duration, buttons_to_sms_port_dc("right")));
        }
        events.sort_by_key(|(frame, _)| *frame);

        let mut branch = state.clone();
        branch.next_button_event = events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
        let mut max_aofs = branch.bus.ram[0x072C];
        let mut max_ppage = branch.bus.ram[0x006D];
        let mut max_px = branch.bus.ram[0x0086];
        let mut max_y_page = branch.bus.ram[0x00B5];
        let mut first_stop_frame = None;
        while branch.irqs_fired < 3400 {
            let before = branch.irqs_fired;
            if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                eprintln!("branch triple stopped: {err:?}");
                break;
            }
            max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
            let ppage = branch.bus.ram[0x006D];
            let px = branch.bus.ram[0x0086];
            if (ppage, px) > (max_ppage, max_px) {
                max_ppage = ppage;
                max_px = px;
            }
            max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
            if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                first_stop_frame = Some(branch.irqs_fired);
            }
            if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                break;
            }
            if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                break;
            }
        }
        results.push(SearchResult {
            label: format!("three:{s1}+{d1},{s2}+{d2},{s3}+{d3}"),
            start: s1,
            duration: d1,
            max_aofs,
            max_ppage,
            max_px,
            max_y_page,
            final_state: branch.bus.ram[0x000E],
            stop_flag: branch.bus.ram[0x0723],
            first_stop_frame,
        });
    }

    for fourth_start in (2360usize..=2520).step_by(10) {
        for fourth_duration in [20usize, 35, 50, 70, 90] {
            let mut events = base_events.to_vec();
            for (start, duration) in [
                (2100usize, 70usize),
                (2220usize, 50usize),
                (2280usize, 50usize),
                (fourth_start, fourth_duration),
            ] {
                events.push((start, buttons_to_sms_port_dc("right,a")));
                events.push((start + duration, buttons_to_sms_port_dc("right")));
            }
            events.sort_by_key(|(frame, _)| *frame);

            let mut branch = state.clone();
            branch.next_button_event =
                events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
            let mut max_aofs = branch.bus.ram[0x072C];
            let mut max_ppage = branch.bus.ram[0x006D];
            let mut max_px = branch.bus.ram[0x0086];
            let mut max_y_page = branch.bus.ram[0x00B5];
            let mut first_stop_frame = None;
            while branch.irqs_fired < 3400 {
                let before = branch.irqs_fired;
                if let Err(err) = run_search_steps(&mut branch, 500_000, &events, before + 1) {
                    eprintln!("branch fourth stopped: {err:?}");
                    break;
                }
                max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                let ppage = branch.bus.ram[0x006D];
                let px = branch.bus.ram[0x0086];
                if (ppage, px) > (max_ppage, max_px) {
                    max_ppage = ppage;
                    max_px = px;
                }
                max_y_page = max_y_page.max(branch.bus.ram[0x00B5]);
                if first_stop_frame.is_none() && branch.bus.ram[0x0723] != 0 {
                    first_stop_frame = Some(branch.irqs_fired);
                }
                if first_stop_frame.is_some() && branch.bus.ram[0x00B5] >= 0x02 {
                    break;
                }
                if branch.bus.ram[0x072C] >= 0x60 || branch.bus.ram[0x000E] == 0x04 {
                    break;
                }
            }
            results.push(SearchResult {
                label: format!("four:2100+70,2220+50,2280+50,{fourth_start}+{fourth_duration}"),
                start: fourth_start,
                duration: fourth_duration,
                max_aofs,
                max_ppage,
                max_px,
                max_y_page,
                final_state: branch.bus.ram[0x000E],
                stop_flag: branch.bus.ram[0x0723],
                first_stop_frame,
            });
        }
    }

    results.sort_by_key(|r| {
        (
            std::cmp::Reverse(r.max_aofs),
            std::cmp::Reverse(r.max_ppage),
            std::cmp::Reverse(r.max_px),
            r.max_y_page,
            r.first_stop_frame.unwrap_or(usize::MAX),
        )
    });
    println!("top late-route probes:");
    for result in results.iter().take(25) {
        println!(
            "  {} start={} dur={} max_aofs={:02X} max_ppos={:02X}:{:02X} max_ypage={:02X} final_state={:02X} stop={} first_stop={:?}",
            result.label,
            result.start,
            result.duration,
            result.max_aofs,
            result.max_ppage,
            result.max_px,
            result.max_y_page,
            result.final_state,
            result.stop_flag,
            result.first_stop_frame,
        );
    }
}

fn run_end_route_search(rom_path: &PathBuf, base_events: &[(usize, u8)]) {
    let rom = std::fs::read(rom_path).expect("read rom");
    let mut state = SearchState {
        cpu: Cpu::new(),
        bus: SmsBus::new(rom, 0xFF),
        step: 0,
        next_irq_at: irq_period(),
        irqs_fired: 0,
        next_button_event: 0,
    };
    state.cpu.pc = 0x0000;
    state.cpu.sp = 0xDFF0;

    let snapshot_frame = 2600usize;
    run_search_steps(&mut state, 190_000_000, base_events, snapshot_frame)
        .expect("run to end-route snapshot");
    state.bus.io_log.clear();
    state.bus.bank_writes.clear();
    state.bus.watch_log.clear();
    state.bus.watch_read_log.clear();

    println!(
        "end-route snapshot frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} aofs={:02X} state={:02X} stop={:02X}",
        state.irqs_fired,
        state.bus.ram[0x006D],
        state.bus.ram[0x0086],
        state.bus.ram[0x00B5],
        state.bus.ram[0x00CE],
        state.bus.ram[0x071A],
        state.bus.ram[0x071C],
        state.bus.ram[0x072C],
        state.bus.ram[0x000E],
        state.bus.ram[0x0723],
    );

    let mut results = Vec::new();
    for jumps in [4usize, 5, 6] {
        for first_start in (2610usize..=2670).step_by(10) {
            for period in [55usize, 65, 75, 85] {
                for duration in [25usize, 35, 45, 55, 65] {
                    let mut events = base_events.to_vec();
                    for n in 0..jumps {
                        let start = first_start + n * period;
                        events.push((start, buttons_to_sms_port_dc("right,a")));
                        events.push((start + duration, buttons_to_sms_port_dc("right")));
                    }
                    events.sort_by_key(|(frame, _)| *frame);

                    let mut branch = state.clone();
                    branch.next_button_event =
                        events.partition_point(|(frame, _)| *frame <= branch.irqs_fired);
                    branch.bus.watch_read_range = Some((0xC500, 0xC69F));
                    branch.bus.watch_read_log.clear();

                    let mut max_aofs = branch.bus.ram[0x072C];
                    let mut max_ppage = branch.bus.ram[0x006D];
                    let mut max_px = branch.bus.ram[0x0086];
                    let mut min_y = branch.bus.ram[0x00CE];
                    let mut first_victory_frame = None;

                    while branch.irqs_fired < 3400 {
                        let before = branch.irqs_fired;
                        if let Err(err) =
                            run_search_steps(&mut branch, 500_000, &events, before + 1)
                        {
                            eprintln!(
                                "end branch jumps={jumps} first={first_start} period={period} duration={duration} stopped: {err:?}"
                            );
                            break;
                        }
                        max_aofs = max_aofs.max(branch.bus.ram[0x072C]);
                        let ppage = branch.bus.ram[0x006D];
                        let px = branch.bus.ram[0x0086];
                        if (ppage, px) > (max_ppage, max_px) {
                            max_ppage = ppage;
                            max_px = px;
                        }
                        min_y = min_y.min(branch.bus.ram[0x00CE]);
                        if branch.bus.ram[0x000E] == 0x04 {
                            first_victory_frame = Some(branch.irqs_fired);
                            break;
                        }
                        if branch.bus.ram[0x0723] != 0 && branch.bus.ram[0x000E] != 0x04 {
                            break;
                        }
                    }

                    let flag_reads = branch
                        .bus
                        .watch_read_log
                        .iter()
                        .filter(|read| matches!(read.value, 0x24 | 0x25))
                        .count();
                    results.push(EndRouteResult {
                        label: format!(
                            "jumps={jumps} first={first_start} period={period} duration={duration}"
                        ),
                        max_aofs,
                        max_ppage,
                        max_px,
                        min_y,
                        final_ppage: branch.bus.ram[0x006D],
                        final_px: branch.bus.ram[0x0086],
                        final_y: branch.bus.ram[0x00CE],
                        final_state: branch.bus.ram[0x000E],
                        stop_flag: branch.bus.ram[0x0723],
                        flag_reads,
                        first_victory_frame,
                    });
                }
            }
        }
    }

    results.sort_by_key(|r| {
        (
            r.first_victory_frame.is_none(),
            std::cmp::Reverse(r.flag_reads),
            std::cmp::Reverse(r.max_aofs),
            std::cmp::Reverse(r.max_ppage),
            std::cmp::Reverse(r.max_px),
            r.final_y,
        )
    });
    println!("top end-route probes:");
    for result in results.iter().take(40) {
        println!(
            "  {} max_aofs={:02X} max_ppos={:02X}:{:02X} min_y={:02X} final={:02X}:{:02X} y={:02X} state={:02X} stop={} flag_reads={} victory={:?}",
            result.label,
            result.max_aofs,
            result.max_ppage,
            result.max_px,
            result.min_y,
            result.final_ppage,
            result.final_px,
            result.final_y,
            result.final_state,
            result.stop_flag,
            result.flag_reads,
            result.first_victory_frame,
        );
    }

    let mut follow_events = base_events.to_vec();
    let follow_first = 2620usize;
    let follow_period = 85usize;
    let follow_duration = 35usize;
    for n in 0..5 {
        let start = follow_first + n * follow_period;
        follow_events.push((start, buttons_to_sms_port_dc("right,a")));
        follow_events.push((start + follow_duration, buttons_to_sms_port_dc("right")));
    }
    follow_events.sort_by_key(|(frame, _)| *frame);
    let mut follow = state.clone();
    follow.next_button_event =
        follow_events.partition_point(|(frame, _)| *frame <= follow.irqs_fired);
    run_search_steps(&mut follow, 260_000_000, &follow_events, 6666)
        .expect("continue winning end-route candidate");
    println!(
        "continued winner frame={} ppos={:02X}:{:02X} y={:02X}:{:02X} cam={:02X}:{:02X} state={:02X} star_flag_task={:02X} collision_bits={:02X} enemy_flag={:02X} scroll_lock={:02X} flag_score={:02X} flag_y={:02X} area={:02X} level={:02X} fetch_timer={:02X} end_y={:02X} slide_timer={:02X}",
        follow.irqs_fired,
        follow.bus.ram[0x006D],
        follow.bus.ram[0x0086],
        follow.bus.ram[0x00B5],
        follow.bus.ram[0x00CE],
        follow.bus.ram[0x071A],
        follow.bus.ram[0x071C],
        follow.bus.ram[0x000E],
        follow.bus.ram[0x0746],
        follow.bus.ram[0x0490],
        follow.bus.ram[0x001B],
        follow.bus.ram[0x0723],
        follow.bus.ram[0x010F],
        follow.bus.ram[0x070F],
        follow.bus.ram[0x0760],
        follow.bus.ram[0x075C],
        follow.bus.ram[0x0757],
        follow.bus.ram[0x0713],
        follow.bus.ram[0x0785],
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rom_path = match args.get(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: trace-sms <rom.sms> [--steps N] [--log-pcs]");
            eprintln!("                     [--buttons a,b,start,up,down,left,right]");
            eprintln!("                     [--buttons-at-frame FRAME:buttons]");
            eprintln!("                     [--buttons-script path]");
            eprintln!("                     [--checkpoint FRAME:name]");
            eprintln!("                     [--checkpoint-script path]");
            eprintln!("                     [--checkpoint-dir dir]");
            eprintln!("                     [--expect-no-trap]");
            eprintln!("                     [--expect-ram ADDR=HEX]");
            eprintln!("                     [--pad1-raw HEX]");
            eprintln!("                     [--search-end-routes]");
            std::process::exit(2);
        }
    };
    let mut steps: usize = 200_000;
    let mut log_pcs = false;
    let mut inject_irq = true;
    let mut controller_port_dc = 0xFF;
    let mut delayed_controller_port_dc: Option<u8> = None;
    let mut buttons_after_frame: Option<usize> = None;
    let mut button_events: Vec<(usize, u8)> = Vec::new();
    let mut expect_no_trap = false;
    let mut ram_expectations: Vec<RamExpectation> = Vec::new();
    let mut checkpoints: Vec<RouteCheckpoint> = Vec::new();
    let mut checkpoint_dir: PathBuf = PathBuf::from("out/smb/checkpoints");
    let mut search_late_routes = false;
    let mut search_end_routes = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--steps" => {
                i += 1;
                steps = args[i].parse().expect("steps int");
            }
            "--log-pcs" => log_pcs = true,
            "--search-late-routes" => search_late_routes = true,
            "--search-end-routes" => search_end_routes = true,
            "--no-irq" => inject_irq = false,
            "--buttons" => {
                i += 1;
                let value = args.get(i).expect("--buttons value");
                controller_port_dc = buttons_to_sms_port_dc(value);
            }
            "--buttons-after-frame" => {
                i += 1;
                let value = args.get(i).expect("--buttons-after-frame value");
                buttons_after_frame = Some(
                    value
                        .parse()
                        .expect("--buttons-after-frame expects an integer"),
                );
                delayed_controller_port_dc = Some(controller_port_dc);
                controller_port_dc = 0xFF;
            }
            "--buttons-at-frame" => {
                i += 1;
                let value = args.get(i).expect("--buttons-at-frame value");
                button_events.push(
                    parse_button_event(value)
                        .unwrap_or_else(|err| panic!("invalid --buttons-at-frame: {err}")),
                );
            }
            "--buttons-script" => {
                i += 1;
                let path = args.get(i).expect("--buttons-script path");
                button_events.extend(
                    load_button_script(path)
                        .unwrap_or_else(|err| panic!("invalid --buttons-script: {err}")),
                );
            }
            "--checkpoint" => {
                i += 1;
                let value = args.get(i).expect("--checkpoint FRAME:name");
                checkpoints.push(
                    parse_checkpoint_spec(value)
                        .unwrap_or_else(|err| panic!("invalid --checkpoint: {err}")),
                );
            }
            "--checkpoint-script" => {
                i += 1;
                let path = args.get(i).expect("--checkpoint-script path");
                checkpoints.extend(
                    load_checkpoint_script(path)
                        .unwrap_or_else(|err| panic!("invalid --checkpoint-script: {err}")),
                );
            }
            "--checkpoint-dir" => {
                i += 1;
                checkpoint_dir = PathBuf::from(args.get(i).expect("--checkpoint-dir path"));
            }
            "--expect-no-trap" => expect_no_trap = true,
            "--expect-ram" => {
                i += 1;
                let value = args.get(i).expect("--expect-ram ADDR=HEX");
                ram_expectations.push(
                    parse_ram_expectation(value)
                        .unwrap_or_else(|err| panic!("invalid --expect-ram: {err}")),
                );
            }
            "--pad1-raw" => {
                i += 1;
                let value = args.get(i).expect("--pad1-raw hex value");
                controller_port_dc = parse_hex_u8(value).expect("--pad1-raw expects hex byte");
            }
            other => {
                eprintln!("unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    button_events.sort_by_key(|(frame, _)| *frame);
    checkpoints.sort_by_key(|checkpoint| checkpoint.frame);

    if search_late_routes {
        run_late_route_search(&rom_path, &button_events);
        return;
    }
    if search_end_routes {
        run_end_route_search(&rom_path, &button_events);
        return;
    }

    let rom = std::fs::read(&rom_path).expect("read rom");
    let sym_path = rom_path.with_extension("sym");
    let symbols = load_wla_symbols(&sym_path);
    let symbol_defs = load_wla_symbol_defs(&sym_path);
    let asm_path = rom_path.with_extension("asm");
    let expected_mirroring = detect_expected_mirroring(&asm_path);
    let mut runtime_materializer_monitor = RuntimeMaterializerMonitor::new(&symbol_defs);
    let rt_ppu_write_addr = if let Some((_, addr)) = symbol_defs.get("rt_ppu_write") {
        *addr
    } else {
        eprintln!(
            "WARN: rt_ppu_write symbol missing in {}; using fallback ${RT_PPU_WRITE_FALLBACK_ADDR:04X}",
            sym_path.display()
        );
        RT_PPU_WRITE_FALLBACK_ADDR
    };
    let mut bus = SmsBus::new(rom, controller_port_dc);
    bus.d300_compact_store_range = d300_compact_store_range(&symbol_defs);
    bus.nt_folded_s_compact_available = bus.d300_compact_store_range.is_some();
    bus.cc_subpal_range = cc_subpal_range(&symbol_defs);
    bus.cc_attr_write_range = cc_attr_write_range(&symbol_defs);
    bus.cc_init_clear_range = cc_init_clear_range(&symbol_defs);
    let mut cpu = Cpu::new();
    cpu.pc = 0x0000;
    cpu.sp = 0xDFF0;
    let mut stack_watermark = Z80StackWatermark::new(cpu.sp);

    // PC histogram + last-100 ring buffer.
    let mut pc_counts: HashMap<u16, u32> = HashMap::new();
    let mut ring: Vec<(u16, u8)> = Vec::with_capacity(8192);
    // Separate ring of every non-sequential PC transition. Captures jumps
    // and rets at full step resolution without ballooning into NOP runs.
    let mut jump_ring: Vec<(u16, u16, u8, u8)> = Vec::with_capacity(2048);
    let mut last_pc: Option<u16> = None;
    let mut last_op: Option<u8> = None;
    let mut last_slot1: u8 = 1;
    let mut taken = 0usize;
    let mut last_err: Option<StepError> = None;
    let mut interrupt_at_step: Option<usize> = None;
    let mut first_translated_step: Option<usize> = None;
    let mut first_irq_handler_step: Option<usize> = None;
    let mut first_runtime_trap_step: Option<usize> = None;
    let mut first_ram_exec_step: Option<(usize, u16)> = None;
    // Count entries to specific addresses of interest.
    let mut call_targets: HashMap<u16, u32> = HashMap::new();
    let watch_exec_addr = std::env::var("SMS_WATCH_PC")
        .ok()
        .and_then(|s| parse_hex_addr(&s));
    let watch_exec_bank1 = std::env::var("SMS_WATCH_BANK1")
        .ok()
        .and_then(|s| u8::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok());
    let mut watch_exec_log: Vec<WatchExecHit> = Vec::new();
    // Ring of last 64 control transfers (CALL/RET/JP-indirect/conditional).
    // Each entry: (kind, from_pc, to_pc). Kind is "call", "ret", or "jp".
    let mut xfer_ring: Vec<(&'static str, u16, u16)> = Vec::with_capacity(256);

    // Inject an IRQ every 60k steps (roughly one "frame" of Z80 work).
    let mut next_irq_at = irq_period();
    let mut line_irq_at: Option<usize> = None;
    let mut irqs_fired = 0usize;
    let mut line_irqs_fired = 0usize;
    // SMS_PC_PROFILE=1: per-(bank,pc) execution histogram, folded by
    // the ROM's .sym symbols at exit — the optimizer's hot-spot oracle.
    let mut pc_profile: Option<std::collections::HashMap<(u8, u16), u64>> =
        std::env::var("SMS_PC_PROFILE")
            .is_ok()
            .then(Default::default);
    // Per-frame handler cost: approx cycles from frame-IRQ injection until
    // IFF1 re-enables (the translated NMI's `ei` path). Line-IRQ handler
    // cost is not attributed here. Used to budget against real SMS timing.
    let mut frame_cost_start: Option<u64> = None;
    let mut frame_costs: Vec<u64> = Vec::new();
    let mut next_button_event = 0usize;
    let mut next_checkpoint = 0usize;
    // Frame at which SMB first enabled NMI; scripts count from here.
    let mut script_frame_base: Option<usize> = None;
    let mut checkpoint_dump_failed = false;
    let zpy_log_pc: Option<u16> = std::env::var("SMS_LOG_ZPY")
        .ok()
        .and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    let mut zpy_logged = 0usize;
    let mapped_log_pc: Option<u16> = std::env::var("SMS_LOG_MAPPED")
        .ok()
        .and_then(|v| u16::from_str_radix(v.trim_start_matches("0x"), 16).ok());
    let mut mapped_logged = 0usize;
    let mut prev_frame_step = 0usize;
    let mut prev_frame_vram_writes = 0u32;
    let mut prev_frame_cram_writes = 0u32;
    let mut prev_frame_data_writes = 0u32;
    let mut prev_frame_control_writes = 0u32;
    let mut prev_frame_line_irqs = 0usize;
    let mut prev_coarse_scroll = current_coarse_scroll(&bus);
    let mut materializer_budget_sims = MaterializerBudgetSim::new_all();
    let mut materializer_policy_sims = MaterializerPolicySim::new_all();
    let dump_each_frame_to = std::env::var("SMS_DUMP_EACH_FRAME").ok();
    let stop_on_fall = std::env::var("SMS_STOP_ON_FALL")
        .ok()
        .is_some_and(|v| v != "0");
    let mut first_fall_snapshot: Option<FallSnapshot> = None;

    for step in 0..steps {
        bus.watch_step = step;
        stack_watermark.observe(cpu.sp);
        let pc = cpu.pc;
        bus.watch_pc = pc;
        bus.watch_bank1 = bus.slot_bank[1];
        bus.watch_sp = cpu.sp;
        let ret_lo = bus.read(cpu.sp) as u16;
        let ret_hi = bus.read(cpu.sp.wrapping_add(1)) as u16;
        bus.watch_ret = ret_lo | (ret_hi << 8);
        let op = bus.read(pc);
        // Wild-jump detector: legitimate execution is only ROM/SRAM
        // slots 0-2 ($0000-$BFFF). PC in RAM ($C000+) means the Z80
        // jumped into garbage — the real-emulator crash. Report the
        // control-transfer ring and halt.
        if std::env::var("SMS_TRAP_RAM_EXEC").is_ok() && pc >= 0xC000 && step > 300_000 {
            eprintln!("*** WILD JUMP: PC=${pc:04X} (RAM) at step {step}, irqs={irqs_fired}");
            eprintln!("  SP=${:04X} last_pc=${:04X} last_op=${:02X}", cpu.sp, last_pc.unwrap_or(0), last_op.unwrap_or(0));
            eprintln!("  stack: {}", (0..12).map(|i| format!("{:04X}", bus.read(cpu.sp.wrapping_add(i*2)) as u16 | (bus.read(cpu.sp.wrapping_add(i*2+1)) as u16) << 8)).collect::<Vec<_>>().join(" "));
            eprintln!("  last 24 xfers:");
            for (k, f, t) in xfer_ring.iter().rev().take(24).rev() {
                eprintln!("    {k} ${f:04X} -> ${t:04X}");
            }
            std::process::exit(7);
        }
        if first_translated_step.is_none() && (0x4000..=0x7FFF).contains(&pc) {
            first_translated_step = Some(step);
        }
        if first_irq_handler_step.is_none() && pc == 0x0038 {
            first_irq_handler_step = Some(step);
        }
        if first_runtime_trap_step.is_none()
            && (bus.ram[0x0B1D] & 0xF0) == 0xE0
            && bus.ram[0x0B1D] != 0
        {
            first_runtime_trap_step = Some(step);
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            let sp = cpu.sp;
            let ret = bus.read(sp) as u16 | ((bus.read(sp.wrapping_add(1)) as u16) << 8);
            eprintln!(
                "*** first trap at step {step}: unresolved_id=${id:04X} pc=${pc:04X} \
                 ret=${ret:04X} (call at ${:04X}) slot1_bank={} slot2_bank={} nes_bank={} disp_ret=${:04X} ind_ptr=${:04X} scans={}",
                ret.wrapping_sub(3),
                bus.slot_bank[1],
                bus.slot_bank[2],
                bus.ram[0x0B1A],
                bus.ram[0x0B73] as u16 | (bus.ram[0x0B74] as u16) << 8,
                bus.ram[0x0B75] as u16 | (bus.ram[0x0B76] as u16) << 8,
                bus.ram[0x0B7D],
            );
        }
        if let Some(mw_pc) = mapped_log_pc {
            if pc == mw_pc && mapped_logged < 60 {
                let de = (cpu.d as u16) << 8 | cpu.e as u16;
                if (0x3780..0x37C0).contains(&de) && cpu.a != 0 && cpu.a != 0x40 {
                    eprintln!(
                        "MAPPED #{mapped_logged}: A=${:02X} DE=${de:04X} (cell {})",
                        cpu.a,
                        (de - 0x3700) / 2
                    );
                    mapped_logged += 1;
                }
            }
        }
        if let Some(zpy_pc) = zpy_log_pc {
            if pc == zpy_pc && zpy_logged < 40 {
                let zp = cpu.b;
                let lo = bus.ram[zp as usize];
                let hi = bus.ram[zp.wrapping_add(1) as usize];
                eprintln!(
                    "ZPY call #{zpy_logged}: zp=${zp:02X} ptr=${hi:02X}{lo:02X} Y=${:02X} bank62={:02X}",
                    cpu.e, bus.ram[0x0B62]
                );
                zpy_logged += 1;
            }
        }
        if first_ram_exec_step.is_none() && pc >= 0xC000 {
            first_ram_exec_step = Some((step, pc));
        }
        if pc == rt_ppu_write_addr {
            bus.record_trace_ppu_write_call_at(cpu.b, cpu.a, step, irqs_fired);
        }
        runtime_materializer_monitor.observe_pc(step, pc, cpu.sp, &bus);
        if log_pcs && step < 200 {
            eprintln!("step {step:6}  PC=${pc:04X} op=${op:02X}");
        }
        *pc_counts.entry(pc).or_insert(0) += 1;
        if Some(pc) == watch_exec_addr
            && watch_exec_bank1.is_none_or(|bank| bank == bus.slot_bank[1])
        {
            watch_exec_log.push(WatchExecHit {
                step,
                pc,
                bank1: bus.slot_bank[1],
                sp: cpu.sp,
                ret: bus.watch_ret,
                op,
                a: cpu.a,
                f: cpu.f,
                p_shadow: bus.ram[0x0B03],
                x_shadow: bus.ram[0x0B00],
                y_shadow: bus.ram[0x0B01],
                zp00: bus.ram[0x0000],
                zp02: bus.ram[0x0002],
                zp03: bus.ram[0x0003],
                zp04: bus.ram[0x0004],
                zp05: bus.ram[0x0005],
                zp06: bus.ram[0x0006],
                zp07: bus.ram[0x0007],
                zp08: bus.ram[0x0008],
                ppage: bus.ram[0x006D],
                px: bus.ram[0x0086],
                ypage: bus.ram[0x00B5],
                py: bus.ram[0x00CE],
                yspeed: bus.ram[0x009F],
                eb: bus.ram[0x00EB],
                vertical_force: bus.ram[0x070E],
                area_obj_dispatch: bus.ram[0x0000].wrapping_add(bus.ram[0x0007]),
            });
        }
        if ring.len() == 8192 {
            ring.remove(0);
        }
        ring.push((pc, op));
        if let Some(prev) = last_pc {
            if pc.wrapping_sub(prev) > 3 {
                // Coalesce identical consecutive jumps (tight loops) into
                // a single entry to keep the ring useful over long runs.
                let same_as_last = jump_ring
                    .last()
                    .map(|&(p, t, _, _)| p == prev && t == pc)
                    .unwrap_or(false);
                if !same_as_last {
                    if jump_ring.len() == 16384 {
                        jump_ring.remove(0);
                    }
                    jump_ring.push((prev, pc, last_op.unwrap_or(0), last_slot1));
                }
            }
        }
        last_pc = Some(pc);
        last_op = Some(op);
        last_slot1 = bus.slot_bank[1];

        // Count calls: opcode $CD = unconditional CALL; track target.
        if op == 0xCD {
            let lo = bus.read(pc.wrapping_add(1)) as u16;
            let hi = bus.read(pc.wrapping_add(2)) as u16;
            let target = (hi << 8) | lo;
            *call_targets.entry(target).or_insert(0) += 1;
            if xfer_ring.len() == 256 {
                xfer_ring.remove(0);
            }
            xfer_ring.push(("call", pc, target));
        }
        // RET (unconditional). Conditional RETs (C0/C8/D0/D8/E0/E8/F0/F8) and
        // RETN/RETI (ED 45/4D etc) are recorded post-fact via the PC delta
        // below — we can't know if they took without executing first.
        if op == 0xC9 {
            // Peek the top of stack to predict the return target.
            let lo = bus.read(cpu.sp) as u16;
            let hi = bus.read(cpu.sp.wrapping_add(1)) as u16;
            let to = (hi << 8) | lo;
            if xfer_ring.len() == 256 {
                xfer_ring.remove(0);
            }
            xfer_ring.push(("ret", pc, to));
        }

        if inject_irq && step < next_irq_at && line_irq_at.is_some_and(|at| step >= at) && cpu.iff1
        {
            // Simulate a VDP line interrupt. It shares the IM1 vector with the
            // frame interrupt, but the status byte has bit 7 clear, so the
            // runtime can distinguish it after reading $BF.
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            bus.vdp_status_override = Some(0x00);
            cpu.pc = 0x0038;
            if first_irq_handler_step.is_none() {
                first_irq_handler_step = Some(step);
            }
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
            line_irqs_fired += 1;
            line_irq_at = None;
        }

        // Right before injecting the next IRQ, snapshot the framebuffer
        // so we can see how the screen evolves frame by frame.
        if inject_irq && step >= next_irq_at && cpu.iff1 {
            // Script frames count from SMB's NMI enable ($CB08 bit 7) — the
            // same convention frame-diff uses — so one recorded script
            // drives both harnesses identically regardless of how many
            // boot-time IRQ frames precede translated init.
            if script_frame_base.is_none() && bus.ram[0x0B08] & 0x80 != 0 {
                script_frame_base = Some(irqs_fired);
            }
            let script_frame = script_frame_base.map(|base| irqs_fired - base);
            while next_button_event < button_events.len()
                && script_frame.is_some_and(|f| f >= button_events[next_button_event].0)
            {
                bus.controller_port_dc = button_events[next_button_event].1;
                next_button_event += 1;
            }
            if let (Some(frame), Some(port)) = (buttons_after_frame, delayed_controller_port_dc) {
                if script_frame.is_some_and(|f| f >= frame) {
                    bus.controller_port_dc = port;
                }
            }
            if let Some(ref dir) = dump_each_frame_to {
                let _ = std::fs::create_dir_all(dir);
                let path = format!("{dir}/frame_{:03}.ppm", irqs_fired);
                let _ = dump_framebuffer_ppm(&bus, &path);
            }
            while next_checkpoint < checkpoints.len()
                && script_frame.is_some_and(|f| f >= checkpoints[next_checkpoint].frame)
            {
                let checkpoint = &checkpoints[next_checkpoint];
                if let Err(err) = dump_route_checkpoint(
                    &bus,
                    &cpu,
                    step,
                    irqs_fired,
                    checkpoint,
                    CheckpointDumpContext {
                        dir: &checkpoint_dir,
                        symbols: &symbol_defs,
                        expected_mirroring,
                        prev_coarse_scroll,
                        curr_coarse_scroll: current_coarse_scroll(&bus),
                        materializer_budget_sims: &materializer_budget_sims,
                        materializer_policy_sims: &materializer_policy_sims,
                        runtime_materializer_monitor: &runtime_materializer_monitor,
                    },
                ) {
                    eprintln!(
                        "checkpoint dump failed for {} at frame {}: {err}",
                        checkpoint.name, checkpoint.frame
                    );
                    checkpoint_dump_failed = true;
                }
                next_checkpoint += 1;
            }
            // Per-frame peek of game state: mode/task plus key SMB gameplay
            // RAM. The gameplay fields are from the canonical SMB RAM map:
            // $86 player X, $6D player page, $57 horizontal speed,
            // $071A/$071C camera page/X, $06FC saved joypad bits.
            // $B5/$CE player Y page/position, $9F player Y speed, $1D
            // player action/state.
            let frame_steps = step.saturating_sub(prev_frame_step);
            let frame_vram_writes = bus.vram_writes.saturating_sub(prev_frame_vram_writes);
            let frame_cram_writes = bus.cram_writes.saturating_sub(prev_frame_cram_writes);
            let frame_data_writes = bus.vdp_data_writes.saturating_sub(prev_frame_data_writes);
            let frame_control_writes = bus
                .vdp_control_writes
                .saturating_sub(prev_frame_control_writes);
            let frame_line_irqs = line_irqs_fired.saturating_sub(prev_frame_line_irqs);
            let coarse_scroll = current_coarse_scroll(&bus);
            let materializer_work =
                format_materializer_work_estimate(prev_coarse_scroll, coarse_scroll);
            let materializer_dirty = format_nt_materializer_dirty_visible(
                &bus,
                expected_mirroring,
                prev_coarse_scroll,
                coarse_scroll,
            );
            let materializer_budget = step_materializer_budget_sims(
                &mut materializer_budget_sims,
                &bus,
                expected_mirroring,
                prev_coarse_scroll,
                coarse_scroll,
                irqs_fired,
            );
            let materializer_policy = step_materializer_policy_sims(
                &mut materializer_policy_sims,
                &bus,
                expected_mirroring,
                prev_coarse_scroll,
                coarse_scroll,
                irqs_fired,
            );
            eprintln!(
                "frame {:3}: $0770={:02X} $0772={:02X} $0773={:02X} $0774={:02X} ppos={:02X}:{:02X} spd={:02X} cam={:02X}:{:02X} y={:02X}:{:02X} yspd={:02X} act={:02X} joy={:02X} apage={:02X} bcol={:02X} aobj={:02X} aofs={:02X} alen={:02X}/{:02X}/{:02X} stop={:02X} steps={} vram+={} cram+={} data+={} ctrl+={} line_irq+={} {} {} {} {}",
                irqs_fired,
                bus.ram[0x0770],
                bus.ram[0x0772],
                bus.ram[0x0773],
                bus.ram[0x0774],
                bus.ram[0x006D],
                bus.ram[0x0086],
                bus.ram[0x0057],
                bus.ram[0x071A],
                bus.ram[0x071C],
                bus.ram[0x00B5],
                bus.ram[0x00CE],
                bus.ram[0x009F],
                bus.ram[0x001D],
                bus.ram[0x06FC],
                bus.ram[0x0725],
                bus.ram[0x06A0],
                bus.ram[0x072A],
                bus.ram[0x072C],
                bus.ram[0x0730],
                bus.ram[0x0731],
                bus.ram[0x0732],
                bus.ram[0x0723],
                frame_steps,
                frame_vram_writes,
                frame_cram_writes,
                frame_data_writes,
                frame_control_writes,
                frame_line_irqs,
                materializer_work,
                materializer_dirty,
                materializer_budget,
                materializer_policy,
            );
            if std::env::var("SMS_DUMP_SAT").is_ok() && (90..=110).contains(&irqs_fired) {
                let ys: Vec<String> = (0..24)
                    .map(|i| format!("{:02X}", bus.vram[0x3F00 + i]))
                    .collect();
                let xt: Vec<String> = (0..12)
                    .map(|i| format!("{:02X}", bus.vram[0x3F80 + i]))
                    .collect();
                eprintln!("SAT f{irqs_fired}: Y {}  XT {}", ys.join(" "), xt.join(" "));
            }
            if bus.display_enabled_edge {
                bus.display_enabled_edge = false;
                if let Ok(dir) = std::env::var("SMS_DUMP_ON_ENABLE") {
                    let _ = std::fs::create_dir_all(&dir);
                    let path = format!("{dir}/enable_{irqs_fired:05}.ppm");
                    let _ = dump_framebuffer_ppm(&bus, &path);
                    eprintln!("DISPLAY-ON dump: {path}");
                }
            }
            prev_frame_step = step;
            prev_frame_vram_writes = bus.vram_writes;
            prev_frame_cram_writes = bus.cram_writes;
            prev_frame_data_writes = bus.vdp_data_writes;
            prev_frame_control_writes = bus.vdp_control_writes;
            prev_frame_line_irqs = line_irqs_fired;
            prev_coarse_scroll = coarse_scroll;
            bus.finish_nt_raw_frame();
            bus.finish_bgv_runtime_recompute_frame();
            bus.finish_d3xx_tile_dirty_frame();
            bus.finish_ram_migration_frame();
            bus.clear_materializer_dirty();
            if first_fall_snapshot.is_none() && (bus.ram[0x0723] != 0 || bus.ram[0x00B5] >= 0x02) {
                let recent_reads = bus
                    .watch_read_log
                    .iter()
                    .rev()
                    .take(80)
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                let recent_writes = bus
                    .watch_log
                    .iter()
                    .rev()
                    .take(80)
                    .copied()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                first_fall_snapshot = Some(FallSnapshot {
                    step,
                    frame: irqs_fired,
                    ram: bus.ram,
                    recent_reads,
                    recent_writes,
                });
                if stop_on_fall {
                    taken = step;
                    break;
                }
            }
        }
        if inject_irq && step >= next_irq_at && cpu.iff1 {
            // Simulate a maskable interrupt: push PC, jump to $0038 (IM1).
            if interrupt_at_step.is_none() {
                interrupt_at_step = Some(step);
            }
            line_irq_at = None;
            bus.render_scroll_split = None;
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            bus.vdp_status_override = Some(0x80);
            cpu.pc = 0x0038;
            if first_irq_handler_step.is_none() {
                first_irq_handler_step = Some(step);
            }
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
            irqs_fired += 1;
            frame_cost_start = Some(cpu.cycles);
            next_irq_at = if real_pacing() { step + irq_period() } else { next_irq_at.saturating_add(IRQ_PERIOD) };
        }

        if cpu.halted {
            // halted but no IRQ pending — endless halt. Stop.
            break;
        }
        if let Some(map) = pc_profile.as_mut() {
            let bank = match pc {
                0x0000..=0x3FFF => 0u8,
                0x4000..=0x7FFF => bus.slot_bank[1],
                _ => bus.slot_bank[2],
            };
            *map.entry((bank, pc)).or_insert(0u64) += 1;
        }
        let materializer_render_before = current_render_state(&bus);
        let materializer_vdp_writes_before = bus.vdp_data_writes;
        match cpu.step(&mut bus) {
            Ok(()) => {
                taken += 1;
                if let Some(start) = frame_cost_start
                    && cpu.iff1
                {
                    frame_costs.push(cpu.cycles.saturating_sub(start));
                    frame_cost_start = None;
                }
                let materializer_vdp_writes = bus
                    .vdp_data_writes
                    .saturating_sub(materializer_vdp_writes_before);
                runtime_materializer_monitor.observe_vdp_writes(
                    step,
                    pc,
                    &bus,
                    materializer_vdp_writes,
                    materializer_render_before,
                );
                runtime_materializer_monitor.observe_after_step(op, cpu.sp);
                if bus.vdp_regs[0] & 0x10 == 0 {
                    line_irq_at = None;
                } else if inject_irq && cpu.iff1 && line_irq_at.is_none() {
                    // R10 is loaded with one less than the target raster line.
                    // Convert that to a coarse instruction-step delay; this is
                    // not cycle-accurate, but it lets checkpoint rendering see
                    // the runtime's one-shot post-split scroll before the next
                    // frame IRQ snapshot.
                    let target_line = (bus.vdp_regs[10] as usize + 1).min(223);
                    bus.render_scroll_split = Some((target_line, bus.vdp_regs[8], bus.vdp_regs[9]));
                    let line_delay = (bus.vdp_regs[10] as usize + 1).clamp(8, 512);
                    line_irq_at = Some(step.saturating_add(line_delay));
                }
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    bus.finish_nt_raw_frame();
    bus.finish_bgv_runtime_recompute_frame();
    bus.finish_d3xx_tile_dirty_frame();
    bus.finish_ram_migration_frame();
    if let Ok(spec) = std::env::var("SMS_DUMP_RAM") {
        if let Some((a, l)) = spec.split_once(':') {
            if let (Ok(a), Ok(l)) = (
                usize::from_str_radix(a.trim_start_matches("0x"), 16),
                usize::from_str_radix(l.trim_start_matches("0x"), 16),
            ) {
                let base = a - 0xC000;
                let hex: Vec<String> = bus.ram[base..base + l]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                println!("RAM ${a:04X}: {}", hex.join(" "));
            }
        }
    }
    if let Ok(spec) = std::env::var("SMS_DUMP_VRAM") {
        if let Some((a, l)) = spec.split_once(':') {
            if let (Ok(a), Ok(l)) = (
                usize::from_str_radix(a.trim_start_matches("0x"), 16),
                usize::from_str_radix(l.trim_start_matches("0x"), 16),
            ) {
                let hex: Vec<String> = bus.vram[a..a + l]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                println!("VRAM ${a:04X}: {}", hex.join(" "));
            }
        }
    }
    if let Ok(t) = std::env::var("SMS_DUMP_TILE") {
        if let Ok(tile) = usize::from_str_radix(t.trim_start_matches("0x"), 16) {
            let base = tile * 32;
            let hex: Vec<String> = bus.vram[base..base + 32]
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect();
            println!("VRAM tile {tile:03X}: {}", hex.join(" "));
        }
    }
    println!("=== trace-sms summary ===");
    println!("ROM: {}", rom_path.display());
    println!("steps run: {taken}");
    if let Some(e) = &last_err {
        println!("stopped on error: {e:?}");
    }
    if let Some(at) = interrupt_at_step {
        println!("injected IRQ at step {at}");
    }
    if !frame_costs.is_empty() {
        // NTSC SMS: 59,736 T-states per frame. A handler that exceeds the
        // full frame budget cannot keep 60 fps on real hardware/Mednafen.
        const FRAME_BUDGET_CYCLES: u64 = 59_736;
        let mut sorted = frame_costs.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let pct = |p: usize| sorted[(n - 1) * p / 100];
        let avg = sorted.iter().sum::<u64>() / n as u64;
        let over: usize = sorted.iter().filter(|&&c| c > FRAME_BUDGET_CYCLES).count();
        let worst_ratio = *sorted.last().unwrap() as f64 / FRAME_BUDGET_CYCLES as f64;
        println!(
            "frame_handler_cost approx_cycles: frames={} min={} p50={} avg={} p90={} p99={} max={}",
            n,
            sorted[0],
            pct(50),
            avg,
            pct(90),
            pct(99),
            sorted[n - 1]
        );
        if let Some(map) = pc_profile {
            let sym_path = rom_path.with_extension("sym");
            let mut syms: Vec<(u8, u16, String)> = Vec::new();
            if let Ok(text) = std::fs::read_to_string(&sym_path) {
                let mut in_labels = false;
                for line in text.lines() {
                    let t = line.trim();
                    if t.starts_with('[') {
                        in_labels = t == "[labels]";
                        continue;
                    }
                    if !in_labels || t.is_empty() || t.starts_with(';') {
                        continue;
                    }
                    if let Some((ba, name)) = t.split_once(' ')
                        && let Some((b, a)) = ba.split_once(':')
                        && let (Ok(b), Ok(a)) =
                            (u8::from_str_radix(b, 16), u16::from_str_radix(a, 16))
                    {
                        syms.push((b, a, name.to_string()));
                    }
                }
                syms.sort();
            }
            let total: u64 = map.values().sum();
            let mut per_sym: std::collections::HashMap<String, u64> = Default::default();
            for (&(bank, pc), &n) in &map {
                let name = syms
                    .iter()
                    .rev()
                    .find(|(b, a, _)| *b == bank && *a <= pc)
                    .map(|(_, _, s)| s.clone())
                    .unwrap_or_else(|| format!("{bank:02X}:{pc:04X}?"));
                *per_sym.entry(name).or_insert(0) += n;
            }
            let mut rows: Vec<(String, u64)> = per_sym.into_iter().collect();
            rows.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            eprintln!("pc_profile: {total} sampled instructions; top symbols:");
            for (name, n) in rows.iter().take(40) {
                eprintln!(
                    "  {:6.2}%  {:>12}  {}",
                    *n as f64 * 100.0 / total as f64,
                    n,
                    name
                );
            }
        }

        println!(
            "frame_budget: budget={} over_budget_frames={} ({:.1}%) worst_frame={:.2}x avg={:.2}x",
            FRAME_BUDGET_CYCLES,
            over,
            over as f64 * 100.0 / n as f64,
            worst_ratio,
            avg as f64 / FRAME_BUDGET_CYCLES as f64
        );
    }
    println!("\nMilestones:");
    print_milestone("entered translated slot-1 code", first_translated_step);
    print_milestone("entered IRQ/NMI bridge at $0038", first_irq_handler_step);
    print_milestone("hit runtime trap marker $CB1D=$E1", first_runtime_trap_step);
    match first_ram_exec_step {
        Some((step, pc)) => println!("  yes: executed RAM at ${pc:04X} at step {step}"),
        None => println!("   no: executed RAM at $C000-$FFFF"),
    }
    println!("VRAM writes: {}", bus.vram_writes);
    println!(
        "PSG writes: {} (first 96: {})",
        bus.psg_writes,
        bus.psg_log
            .iter()
            .take(96)
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!("CRAM writes: {}", bus.cram_writes);
    println!("VDP data-port writes: {}", bus.vdp_data_writes);
    println!("VDP control-port writes: {}", bus.vdp_control_writes);
    println!("VDP status reads: {}", bus.vdp_status_reads);
    println!("Controller reads: {}", bus.controller_reads);
    println!("Controller $DC raw: ${:02X}", bus.controller_port_dc);
    println!(
        "Gameplay state: player={:02X}:{:02X} speed=${:02X} camera={:02X}:{:02X} joy=${:02X} state=$0E:{:02X}",
        bus.ram[0x006D],
        bus.ram[0x0086],
        bus.ram[0x0057],
        bus.ram[0x071A],
        bus.ram[0x071C],
        bus.ram[0x06FC],
        bus.ram[0x000E],
    );
    println!(
        "Scroll state: scroll_x={:02X}:{:02X} vscroll_latch=${:02X}:${:02X} gates 06FF=${:02X} 03A1=${:02X} 0723=${:02X} 0755=${:02X} 0785=${:02X}",
        bus.ram[0x071A],
        bus.ram[0x071C],
        bus.ram[0x073F],
        bus.ram[0x0740],
        bus.ram[0x06FF],
        bus.ram[0x03A1],
        bus.ram[0x0723],
        bus.ram[0x0755],
        bus.ram[0x0785],
    );
    println!(
        "Player physics: y={:02X}:{:02X} yspd=${:02X} yfrac=${:02X} yfrac_spd=${:02X} action=${:02X} move_force=${:02X} jump_origin=${:02X} vertical_force=${:02X} friction_gate=${:02X}",
        bus.ram[0x00B5],
        bus.ram[0x00CE],
        bus.ram[0x009F],
        bus.ram[0x0416],
        bus.ram[0x0433],
        bus.ram[0x0704],
        bus.ram[0x0709],
        bus.ram[0x070A],
        bus.ram[0x070E],
        bus.ram[0x0747],
    );
    println!(
        "Parser state: page=${:02X} col=${:02X} back=${:02X} behind=${:02X} obj_page=${:02X} page_sel=${:02X} data_ofs=${:02X} slot_ofs=${:02X}/${:02X}/${:02X} len=${:02X}/${:02X}/${:02X} stair=${:02X} height=${:02X} block_col=${:02X}",
        bus.ram[0x0725],
        bus.ram[0x0726],
        bus.ram[0x0728],
        bus.ram[0x0729],
        bus.ram[0x072A],
        bus.ram[0x072B],
        bus.ram[0x072C],
        bus.ram[0x072D],
        bus.ram[0x072E],
        bus.ram[0x072F],
        bus.ram[0x0730],
        bus.ram[0x0731],
        bus.ram[0x0732],
        bus.ram[0x0734],
        bus.ram[0x0735],
        bus.ram[0x06A0],
    );
    println!(
        "End-level state: star_flag_task=${:02X} collision_bits=${:02X} enemy_flag=${:02X} scroll_lock=${:02X} flag_score=${:02X} flag_y=${:02X} area=${:02X} level=${:02X} fetch_timer=${:02X} end_y=${:02X} slide_timer=${:02X}",
        bus.ram[0x0746],
        bus.ram[0x0490],
        bus.ram[0x001B],
        bus.ram[0x0723],
        bus.ram[0x010F],
        bus.ram[0x070F],
        bus.ram[0x0760],
        bus.ram[0x075C],
        bus.ram[0x0757],
        bus.ram[0x0713],
        bus.ram[0x0785],
    );
    if let Some(snapshot) = &first_fall_snapshot {
        print_fall_snapshot(snapshot);
    }
    println!("VDP control I/O entries: {}", bus.io_log.len());
    println!("IRQs fired: {irqs_fired}");
    println!("Line IRQs fired: {line_irqs_fired}");
    println!(
        "Bank mapping: slot0={} slot1={} slot2={}",
        bus.slot_bank[0], bus.slot_bank[1], bus.slot_bank[2]
    );
    println!("Mapper writes total: {}", bus.bank_writes.len());
    for (port, value) in bus.bank_writes.iter().take(20) {
        println!("  W ${port:04X} = ${value:02X}");
    }
    if bus.watch_addr.is_some() || bus.watch_write_range.is_some() {
        if let Some(addr) = bus.watch_addr {
            println!("Write watch ${addr:04X}: {} writes", bus.watch_log.len());
        }
        if let Some((start, end)) = bus.watch_write_range {
            println!(
                "Write watch ${start:04X}-${end:04X}: {} writes",
                bus.watch_log.len()
            );
        }
        for write in bus.watch_log.iter().take(40) {
            println!(
                "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                write.step,
                write.addr,
                write.pc,
                write.bank1,
                write.sp,
                write.ret,
                write.value,
                write.ppage,
                write.px,
                write.ypage,
                write.py,
                write.player_state
            );
        }
        if bus.watch_log.len() > 40 {
            println!("  ... last 40 writes:");
            for write in bus
                .watch_log
                .iter()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!(
                    "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                    write.step,
                    write.addr,
                    write.pc,
                    write.bank1,
                    write.sp,
                    write.ret,
                    write.value,
                    write.ppage,
                    write.px,
                    write.ypage,
                    write.py,
                    write.player_state
                );
            }
        }
        let mut value_hist: HashMap<u8, usize> = HashMap::new();
        for write in &bus.watch_log {
            *value_hist.entry(write.value).or_insert(0) += 1;
        }
        let mut value_hist = value_hist.into_iter().collect::<Vec<_>>();
        value_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  write value histogram:");
        for (value, count) in value_hist.into_iter().take(16) {
            print!(" ${value:02X}:{count}");
        }
        println!();
        let flag_writes = bus
            .watch_log
            .iter()
            .filter(|write| matches!(write.value, 0x24 | 0x25))
            .collect::<Vec<_>>();
        println!("  flag metatile writes ($24/$25): {}", flag_writes.len());
        for write in flag_writes.iter().take(20) {
            println!(
                "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                write.step,
                write.addr,
                write.pc,
                write.bank1,
                write.value,
                write.ppage,
                write.px,
                write.ypage,
                write.py,
                write.player_state
            );
        }
        if flag_writes.len() > 20 {
            println!("    ... last 20 flag metatile writes:");
            for write in flag_writes.iter().rev().take(20).rev() {
                println!(
                    "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                    write.step,
                    write.addr,
                    write.pc,
                    write.bank1,
                    write.value,
                    write.ppage,
                    write.px,
                    write.ypage,
                    write.py,
                    write.player_state
                );
            }
        }
    }
    if bus.watch_read_addr.is_some() || bus.watch_read_range.is_some() {
        if let Some(addr) = bus.watch_read_addr {
            println!("Read watch ${addr:04X}: {} reads", bus.watch_read_log.len());
        }
        if let Some((start, end)) = bus.watch_read_range {
            println!(
                "Read watch ${start:04X}-${end:04X}: {} reads",
                bus.watch_read_log.len()
            );
        }
        for read in bus.watch_read_log.iter().take(40) {
            println!(
                "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
                read.step,
                read.addr,
                read.pc,
                read.bank1,
                read.sp,
                read.ret,
                read.value,
                read.ppage,
                read.px,
                read.ypage,
                read.py,
                read.player_state,
                read.x_shadow,
                read.y_shadow,
                read.zp02,
                read.zp03,
                read.zp04,
                read.zp05,
                read.zp06,
                read.zp07,
                read.zp08,
                read.yspeed,
                read.eb,
                read.vertical_force
            );
        }
        if bus.watch_read_log.len() > 40 {
            println!("  ... last 40 reads:");
            for read in bus.watch_read_log.iter().rev().take(40).rev() {
                println!(
                    "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
                    read.step,
                    read.addr,
                    read.pc,
                    read.bank1,
                    read.sp,
                    read.ret,
                    read.value,
                    read.ppage,
                    read.px,
                    read.ypage,
                    read.py,
                    read.player_state,
                    read.x_shadow,
                    read.y_shadow,
                    read.zp02,
                    read.zp03,
                    read.zp04,
                    read.zp05,
                    read.zp06,
                    read.zp07,
                    read.zp08,
                    read.yspeed,
                    read.eb,
                    read.vertical_force
                );
            }
        }
        let flag_reads = bus
            .watch_read_log
            .iter()
            .filter(|read| matches!(read.value, 0x24 | 0x25))
            .collect::<Vec<_>>();
        println!("  flag metatile reads ($24/$25): {}", flag_reads.len());
        for read in flag_reads.iter().take(20) {
            println!(
                "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} eb=${:02X}",
                read.step,
                read.addr,
                read.pc,
                read.bank1,
                read.ret,
                read.value,
                read.ppage,
                read.px,
                read.ypage,
                read.py,
                read.player_state,
                read.x_shadow,
                read.y_shadow,
                read.zp04,
                read.zp05,
                read.zp06,
                read.zp07,
                read.eb,
            );
        }
        if flag_reads.len() > 20 {
            println!("    ... last 20 flag metatile reads:");
            for read in flag_reads.iter().rev().take(20).rev() {
                println!(
                    "    step {}: addr=${:04X} pc=${:04X} bank1=${:02X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} eb=${:02X}",
                    read.step,
                    read.addr,
                    read.pc,
                    read.bank1,
                    read.ret,
                    read.value,
                    read.ppage,
                    read.px,
                    read.ypage,
                    read.py,
                    read.player_state,
                    read.x_shadow,
                    read.y_shadow,
                    read.zp04,
                    read.zp05,
                    read.zp06,
                    read.zp07,
                    read.eb,
                );
            }
        }
    }
    if let Some(addr) = watch_exec_addr {
        println!("PC watch ${addr:04X}: {} hits", watch_exec_log.len());
        let mut by_a: HashMap<u8, usize> = HashMap::new();
        let mut by_zp04: HashMap<u8, usize> = HashMap::new();
        let mut by_a_zp04: HashMap<(u8, u8), usize> = HashMap::new();
        let mut by_dispatch: HashMap<u8, usize> = HashMap::new();
        for hit in &watch_exec_log {
            *by_a.entry(hit.a).or_insert(0) += 1;
            *by_zp04.entry(hit.zp04).or_insert(0) += 1;
            *by_a_zp04.entry((hit.a, hit.zp04)).or_insert(0) += 1;
            *by_dispatch.entry(hit.area_obj_dispatch).or_insert(0) += 1;
        }
        let mut a_hist = by_a.into_iter().collect::<Vec<_>>();
        a_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  A histogram:");
        for (a, count) in a_hist.into_iter().take(16) {
            print!(" ${a:02X}:{count}");
        }
        println!();
        let mut zp04_hist = by_zp04.into_iter().collect::<Vec<_>>();
        zp04_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  zp04 histogram:");
        for (zp04, count) in zp04_hist.into_iter().take(16) {
            print!(" ${zp04:02X}:{count}");
        }
        println!();
        let mut combo_hist = by_a_zp04.into_iter().collect::<Vec<_>>();
        combo_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  A/zp04 combos:");
        for ((a, zp04), count) in combo_hist.into_iter().take(16) {
            print!(" ${a:02X}/${zp04:02X}:{count}");
        }
        println!();
        let mut dispatch_hist = by_dispatch.into_iter().collect::<Vec<_>>();
        dispatch_hist.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        print!("  area obj dispatch ($00+$07) histogram:");
        for (dispatch, count) in dispatch_hist.into_iter().take(32) {
            print!(" ${dispatch:02X}:{count}");
        }
        println!();
        for hit in watch_exec_log.iter().take(40) {
            println!(
                "  step {}: pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X}",
                hit.step,
                hit.pc,
                hit.bank1,
                hit.sp,
                hit.ret,
                hit.op,
                hit.a,
                hit.f,
                hit.p_shadow,
                hit.x_shadow,
                hit.y_shadow,
                hit.ppage,
                hit.px,
                hit.ypage,
                hit.py,
                hit.yspeed,
                hit.eb,
                hit.vertical_force,
                hit.zp00,
                hit.zp02,
                hit.zp03,
                hit.zp04,
                hit.zp05,
                hit.zp06,
                hit.zp07,
                hit.zp08,
                hit.area_obj_dispatch
            );
        }
        if watch_exec_log.len() > 40 {
            println!("  ... last 40 hits:");
            for hit in watch_exec_log
                .iter()
                .rev()
                .take(40)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                println!(
                    "  step {}: pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X}",
                    hit.step,
                    hit.pc,
                    hit.bank1,
                    hit.sp,
                    hit.ret,
                    hit.op,
                    hit.a,
                    hit.f,
                    hit.p_shadow,
                    hit.x_shadow,
                    hit.y_shadow,
                    hit.ppage,
                    hit.px,
                    hit.ypage,
                    hit.py,
                    hit.yspeed,
                    hit.eb,
                    hit.vertical_force,
                    hit.zp00,
                    hit.zp02,
                    hit.zp03,
                    hit.zp04,
                    hit.zp05,
                    hit.zp06,
                    hit.zp07,
                    hit.zp08,
                    hit.area_obj_dispatch
                );
            }
        }
    }
    println!(
        "Cpu state: A=${:02X} B=${:02X} C=${:02X} D=${:02X} E=${:02X} H=${:02X} L=${:02X} SP=${:04X} PC=${:04X} IFF1={}",
        cpu.a, cpu.b, cpu.c, cpu.d, cpu.e, cpu.h, cpu.l, cpu.sp, cpu.pc, cpu.iff1
    );
    let shadow_p = bus.read(0xCB03);
    println!(
        "Shadow 6502: A={:02X} X={:02X} Y={:02X} P={:02X} S={:02X}",
        cpu.a,
        bus.read(0xCB00),
        bus.read(0xCB01),
        shadow_p,
        bus.read(0xCB02)
    );
    let unresolved_id = (bus.read(0xCB1C) as u16) << 8 | bus.read(0xCB1B) as u16;
    println!(
        "Runtime diagnostics: unresolved_id=${unresolved_id:04X} trap_marker=${:02X} vbuf_used=${:02X} ppu_addr=${:02X}{:02X} ppu_mask=${:02X} split_flags=${:02X} split_pre=${:02X}:${:02X} split_post=${:02X}:${:02X}",
        bus.read(0xCB1D),
        bus.read(0xC800),
        bus.read(0xCB0F),
        bus.read(0xCB10),
        bus.read(0xCB09),
        bus.read(0xCB20),
        bus.read(0xCB21),
        bus.read(0xCB22),
        bus.read(0xCB23),
        bus.read(0xCB24)
    );
    if bus.read(0xCB1D) == 0xE1 {
        // rt_unresolved_jsr trap: the CALL's return address is on the Z80
        // stack — name the call site (and its bank via the mapper regs).
        let sp = cpu.sp;
        let ret = bus.read(sp) as u16 | ((bus.read(sp.wrapping_add(1)) as u16) << 8);
        eprintln!(
            "  unresolved-jsr call site: ret=${ret:04X} (call at ${:04X}) slot1_bank={} slot2_bank={}",
            ret.wrapping_sub(3),
            bus.slot_bank[1],
            bus.slot_bank[2],
        );
    }

    println!("{}", format_nt_trace_ciram_summary(&bus, true));
    println!("{}", format_nt_trace_ciram_summary(&bus, false));
    println!("{}", format_nt_dry_project_summary(&bus, true));
    println!("{}", format_nt_dry_project_summary(&bus, false));
    println!("{}", format_nt_folded_s_compact_mismatches(&bus));
    println!(
        "{}",
        format_bgv_base_shadow_mismatches(&bus, &bus.rom, &symbol_defs)
    );
    println!(
        "{}",
        format_bgv_recompute_folded(&bus, &bus.rom, &symbol_defs)
    );
    println!(
        "{}",
        format_bgv_base_from_dry_ciram_mismatches(&bus, &bus.rom, &symbol_defs, true)
    );
    println!(
        "{}",
        format_bgv_base_from_dry_ciram_mismatches(&bus, &bus.rom, &symbol_defs, false)
    );
    println!(
        "{}",
        format_bgv_recompute_ciram(&bus, &bus.rom, &symbol_defs, expected_mirroring)
    );
    println!(
        "{}",
        format_nt_materializer_expected_delta(&bus, expected_mirroring)
    );
    println!(
        "{}",
        format_nt_materializer_dirty_visible(
            &bus,
            expected_mirroring,
            prev_coarse_scroll,
            current_coarse_scroll(&bus),
        )
    );
    println!(
        "{}",
        format_materializer_budget_snapshots(
            &materializer_budget_sims,
            expected_mirroring,
            irqs_fired,
        )
    );
    println!(
        "{}",
        format_materializer_policy_snapshots(
            &materializer_policy_sims,
            &bus,
            expected_mirroring,
            prev_coarse_scroll,
            current_coarse_scroll(&bus),
            current_render_state(&bus),
        )
    );
    println!(
        "{}",
        format_runtime_materializer_hooks(&runtime_materializer_monitor)
    );
    println!("{}", format_nt_raw_write_stats(&bus));
    if std::env::var("SMS_DUMP_CIRAM").is_ok() {
        for row in 0..12usize {
            let mut raw = String::new();
            let mut vr = String::new();
            for col in 0..40usize {
                let (page, c31) = (col / 32, col % 32);
                let off = page * 0x400 + row * 32 + c31;
                raw.push_str(&format!("{:02X} ", bus.cart_ram[off]));
                if col < 32 {
                    let cell = 0x3700 + (row * 32 + col) * 2;
                    vr.push_str(&format!("{:02X} ", bus.vram[cell]));
                }
            }
            println!("ciram r{row:02}: {raw}");
            println!("vram  r{row:02}: {vr}");
        }
    }

    println!("{}", format_nt_raw_frame_stats(&bus));
    println!("{}", format_nt_raw_shadow_parity(&bus));
    println!("{}", format_raw_ciram_storage_decision());
    println!("{}", format_raw_ciram_backend(&bus));
    println!("{}", format_bgv_recompute_runtime_cost(&bus));
    println!("{}", format_ram_migration_access(&bus));
    println!("{}", format_ram_migration_dependency(&bus));
    println!("{}", format_cc_folded_s_dependency(&bus));
    println!("{}", format_d3xx_storage_candidate(&bus));
    println!("{}", format_d3xx_dirty_bitmap_candidate(&bus));
    println!("{}", format_d3xx_full_dirty_bitmap_candidate(&bus));
    println!("{}", format_d3xx_dirty_runtime_cost(&bus));
    println!("{}", format_z80_stack_low_water(stack_watermark));
    println!("NES zero page $00-$0F:");
    for i in 0..16 {
        let b = bus.ram[i];
        print!(" ${b:02X}");
    }
    println!();
    let zp_ptr = ((bus.ram[1] as u16) << 8) | bus.ram[0] as u16;
    println!("NES ($00) pointer ${zp_ptr:04X} first 64 bytes:");
    for i in 0..64u16 {
        let off = zp_ptr.wrapping_add(i) as usize & 0x07ff;
        let b = bus.ram[off];
        print!(" ${b:02X}");
    }
    println!();

    // Top-10 most-visited PCs.
    let mut sorted: Vec<_> = pc_counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    println!("\nTop 10 most-visited PCs:");
    for (pc, n) in sorted.iter().take(10) {
        println!("  ${pc:04X}: {n} times");
    }

    println!("\nLast 64 PC jumps (from op@bank1 -> to):");
    for (from, to, op, bank) in jump_ring.iter().rev().take(64).rev() {
        println!("  ${from:04X} op=${op:02X} bank1=${bank:02X} -> ${to:04X}");
    }

    println!("\nLast 32 control transfers (call/ret):");
    for (kind, from, to) in xfer_ring.iter().rev().take(32).rev() {
        println!("  {kind} from ${from:04X} -> ${to:04X}");
    }

    println!("\nFirst 32 I/O log entries:");
    for e in bus.io_log.iter().take(32) {
        println!("  {e}");
    }
    println!("\nVRAM peek $3F00 (SAT Y bytes): ");
    for i in 0..16 {
        let b = bus.vram[0x3F00 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $3F80 (SAT X+tile bytes): ");
    for i in 0..16 {
        let b = bus.vram[0x3F80 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Sprite data $C200 (NES $0200, first 32 bytes Y,tile,attr,X):");
    for i in 0..32 {
        let b = bus.ram[0x0200 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("OAM staging $C900 (first 32 bytes = 8 sprites NES Y,tile,attr,X):");
    for i in 0..32 {
        let b = bus.ram[0x0900 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("NES VRAM buffer $C300 (first 64 bytes):");
    for i in 0..64 {
        let b = bus.ram[0x0300 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("NES VRAM buffer $C340 (64 bytes):");
    for i in 0..64 {
        let b = bus.ram[0x0340 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Metatile buffer $C6A0-$C6AF:");
    for i in 0..16 {
        let b = bus.ram[0x06A0 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer 1 $C500-$C53F:");
    for i in 0..64 {
        let b = bus.ram[0x0500 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer 2 $C5D0-$C60F:");
    for i in 0..64 {
        let b = bus.ram[0x05D0 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("Block buffer floor rows $C5C0-$C5CF / $C690-$C69F:");
    for i in 0..16 {
        let b = bus.ram[0x05C0 + i];
        print!(" ${b:02X}");
    }
    print!("  | ");
    for i in 0..16 {
        let b = bus.ram[0x0690 + i];
        print!(" ${b:02X}");
    }
    println!();
    let block_nonzero: Vec<(usize, u8)> = (0x0500..=0x06AF)
        .filter_map(|addr| {
            let b = bus.ram[addr];
            (b != 0).then_some((addr, b))
        })
        .collect();
    println!(
        "Block/metatile non-zero entries $C500-$C6AF: {}",
        block_nonzero.len()
    );
    for (addr, b) in block_nonzero.iter().take(80) {
        print!(" ${:04X}={:02X}", 0xC000 + addr, b);
    }
    println!();
    println!("CRAM (32 bytes):");
    for i in 0..32 {
        let b = bus.cram[i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $3800 (nametable start):");
    for i in 0..32 {
        let b = bus.vram[0x3700 + i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $0000 (tile 0):");
    for i in 0..32 {
        let b = bus.vram[i];
        print!(" ${b:02X}");
    }
    println!();
    println!("VRAM peek $0020 (tile 1):");
    for i in 0..32 {
        let b = bus.vram[0x20 + i];
        print!(" ${b:02X}");
    }
    println!();
    // Quick nametable scan: find any non-zero entry.
    let mut nz_count = 0;
    for i in 0..1792 {
        if bus.vram[0x3700 + i] != 0 {
            nz_count += 1;
        }
    }
    println!("Nametable non-zero bytes: {nz_count}/1792");
    let mut chr_nz = 0;
    for i in 0..0x3700 {
        if bus.vram[i] != 0 {
            chr_nz += 1;
        }
    }
    println!("Tile pattern non-zero bytes: {chr_nz}/{}", 0x3700);

    // ASCII nametable dump: print each cell's low-byte tile index as 2-hex.
    // SMS nametable is 32 cols x 28 rows of 16-bit entries (low/high bytes).
    println!("\nNametable (low byte per cell, '.' = 0):");
    for row in 0..28 {
        let mut line = String::new();
        for col in 0..32 {
            let off = 0x3700 + (row * 32 + col) * 2;
            let lo = bus.vram[off];
            if lo == 0 {
                line.push_str("..");
            } else {
                line.push_str(&format!("{lo:02X}"));
            }
        }
        println!("{row:2}: {line}");
    }

    println!("\nTop 20 most-called targets:");
    let mut call_sorted: Vec<_> = call_targets.into_iter().collect();
    call_sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (target, n) in call_sorted.iter().take(20) {
        let suffix = format_symbol_suffix(&symbols, *target);
        println!("  call ${target:04X}{suffix}: {n} times");
    }

    // Dump SMS framebuffer to PPM if requested by env var SMS_DUMP_PPM.
    if let Ok(path) = std::env::var("SMS_DUMP_PPM") {
        if let Err(e) = dump_framebuffer_ppm(&bus, &path) {
            eprintln!("PPM dump failed: {e}");
        } else {
            println!("Wrote framebuffer PPM to {path}");
        }
    }

    let mut acceptance_failed = false;
    if checkpoint_dump_failed {
        eprintln!("EXPECT FAIL: one or more checkpoint artifacts failed to write");
        acceptance_failed = true;
    }
    if next_checkpoint < checkpoints.len() {
        eprintln!(
            "EXPECT FAIL: {} checkpoint(s) not reached; next is frame {} ({})",
            checkpoints.len() - next_checkpoint,
            checkpoints[next_checkpoint].frame,
            checkpoints[next_checkpoint].name
        );
        acceptance_failed = true;
    }
    if expect_no_trap {
        if let Some(step) = first_runtime_trap_step {
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!(
                "EXPECT FAIL: runtime trap marker hit at step {step}, unresolved_id=${id:04X}"
            );
            acceptance_failed = true;
        } else if bus.ram[0x0B1D] == 0xE1 {
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!("EXPECT FAIL: runtime trap marker set, unresolved_id=${id:04X}");
            acceptance_failed = true;
        } else {
            println!("EXPECT ok: no runtime trap marker");
        }
    }
    for expected in &ram_expectations {
        let Some(idx) = ram_index(expected.addr) else {
            eprintln!(
                "EXPECT FAIL: RAM address ${:04X} is outside 8 KiB RAM/mirror",
                expected.addr
            );
            acceptance_failed = true;
            continue;
        };
        let actual = bus.ram[idx];
        if actual != expected.value {
            eprintln!(
                "EXPECT FAIL: RAM ${:04X} expected ${:02X}, got ${:02X}",
                expected.addr, expected.value, actual
            );
            acceptance_failed = true;
        } else {
            println!(
                "EXPECT ok: RAM ${:04X} == ${:02X}",
                expected.addr, expected.value
            );
        }
    }
    if acceptance_failed {
        std::process::exit(1);
    }
}

fn print_milestone(label: &str, step: Option<usize>) {
    match step {
        Some(step) => println!("  yes: {label} at step {step}"),
        None => println!("   no: {label}"),
    }
}

fn ram_at(ram: &[u8; RAM_SIZE], addr: u16) -> u8 {
    ram[(addr as usize) & (RAM_SIZE - 1)]
}

fn print_ram_range(label: &str, ram: &[u8; RAM_SIZE], start: u16, end: u16) {
    println!("{label} ${start:04X}-${end:04X}:");
    let mut addr = start;
    while addr <= end {
        print!("  ${addr:04X}:");
        for i in 0..16u16 {
            let a = addr.wrapping_add(i);
            if a > end {
                break;
            }
            print!(" {:02X}", ram_at(ram, a));
        }
        println!();
        if end.wrapping_sub(addr) < 16 {
            break;
        }
        addr = addr.wrapping_add(16);
    }
}

fn print_watch_tail(label: &str, entries: &[WatchWrite]) {
    println!("{label}: {} entries", entries.len());
    for entry in entries {
        println!(
            "  step {}: addr=${:04X} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} value=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X} xsh=${:02X} ysh=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} yspd=${:02X} eb=${:02X} vf=${:02X}",
            entry.step,
            entry.addr,
            entry.pc,
            entry.bank1,
            entry.sp,
            entry.ret,
            entry.value,
            entry.ppage,
            entry.px,
            entry.ypage,
            entry.py,
            entry.player_state,
            entry.x_shadow,
            entry.y_shadow,
            entry.zp02,
            entry.zp03,
            entry.zp04,
            entry.zp05,
            entry.zp06,
            entry.zp07,
            entry.zp08,
            entry.yspeed,
            entry.eb,
            entry.vertical_force,
        );
    }
}

fn print_fall_snapshot(snapshot: &FallSnapshot) {
    let ram = &snapshot.ram;
    println!(
        "\nFirst fall/death snapshot: frame={} step={} ppos={:02X}:{:02X} speed=${:02X} cam={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} act=${:02X} joy=${:02X} state=$0E:{:02X}",
        snapshot.frame,
        snapshot.step,
        ram_at(ram, 0x006D),
        ram_at(ram, 0x0086),
        ram_at(ram, 0x0057),
        ram_at(ram, 0x071A),
        ram_at(ram, 0x071C),
        ram_at(ram, 0x00B5),
        ram_at(ram, 0x00CE),
        ram_at(ram, 0x009F),
        ram_at(ram, 0x001D),
        ram_at(ram, 0x06FC),
        ram_at(ram, 0x000E),
    );
    println!(
        "  parser: apage={:02X} block_col={:02X} area_obj={:02X} aofs={:02X} len={:02X}/{:02X}/{:02X} stop={:02X} scroll_gates 06FF={:02X} 03A1={:02X}",
        ram_at(ram, 0x0725),
        ram_at(ram, 0x06A0),
        ram_at(ram, 0x072A),
        ram_at(ram, 0x072C),
        ram_at(ram, 0x0730),
        ram_at(ram, 0x0731),
        ram_at(ram, 0x0732),
        ram_at(ram, 0x0723),
        ram_at(ram, 0x06FF),
        ram_at(ram, 0x03A1),
    );
    println!(
        "  collision temps: zp00={:02X} zp01={:02X} zp04={:02X} zp06={:02X} zp07={:02X} eb={:02X} vertical_force={:02X}",
        ram_at(ram, 0x0000),
        ram_at(ram, 0x0001),
        ram_at(ram, 0x0004),
        ram_at(ram, 0x0006),
        ram_at(ram, 0x0007),
        ram_at(ram, 0x00EB),
        ram_at(ram, 0x070E),
    );
    print_ram_range("  Area parser row/buffer", ram, 0x06A0, 0x06AF);
    print_ram_range("  Block buffers", ram, 0xC500, 0xC6AF);
    print_watch_tail("  Recent watched reads before fall", &snapshot.recent_reads);
    print_watch_tail(
        "  Recent watched writes before fall",
        &snapshot.recent_writes,
    );
}

struct CheckpointDumpContext<'a> {
    dir: &'a Path,
    symbols: &'a HashMap<String, (u8, u16)>,
    expected_mirroring: ExpectedMirroring,
    prev_coarse_scroll: (u8, u8),
    curr_coarse_scroll: (u8, u8),
    materializer_budget_sims: &'a [MaterializerBudgetSim],
    materializer_policy_sims: &'a [MaterializerPolicySim],
    runtime_materializer_monitor: &'a RuntimeMaterializerMonitor,
}

fn dump_route_checkpoint(
    bus: &SmsBus,
    cpu: &Cpu,
    step: usize,
    actual_frame: usize,
    checkpoint: &RouteCheckpoint,
    context: CheckpointDumpContext<'_>,
) -> std::io::Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(context.dir)?;
    let slug = checkpoint_slug(&checkpoint.name);
    let stem = format!("{:05}_{}", checkpoint.frame, slug);
    let ppm_path = context.dir.join(format!("{stem}.ppm"));
    let txt_path = context.dir.join(format!("{stem}.txt"));

    dump_framebuffer_ppm(bus, ppm_path.to_string_lossy().as_ref())?;

    let mut f = std::fs::File::create(&txt_path)?;
    writeln!(f, "checkpoint: {}", checkpoint.name)?;
    writeln!(f, "target_frame: {}", checkpoint.frame)?;
    writeln!(f, "actual_frame: {actual_frame}")?;
    writeln!(f, "step: {step}")?;
    writeln!(
        f,
        "cpu: pc=${:04X} sp=${:04X} a=${:02X} f=${:02X} iff1={}",
        cpu.pc, cpu.sp, cpu.a, cpu.f, cpu.iff1
    )?;
    writeln!(
        f,
        "mode: 0770=${:02X} 0772=${:02X} 0773=${:02X} 0774=${:02X} state_000E=${:02X}",
        bus.ram[0x0770], bus.ram[0x0772], bus.ram[0x0773], bus.ram[0x0774], bus.ram[0x000E]
    )?;
    writeln!(
        f,
        "player: page=${:02X} x=${:02X} ypage=${:02X} y=${:02X} xspd=${:02X} yspd=${:02X} action=${:02X}",
        bus.ram[0x006D],
        bus.ram[0x0086],
        bus.ram[0x00B5],
        bus.ram[0x00CE],
        bus.ram[0x0057],
        bus.ram[0x009F],
        bus.ram[0x001D]
    )?;
    writeln!(
        f,
        "scroll: cam_page=${:02X} cam_x=${:02X} vdp_reg8=${:02X} vdp_reg9=${:02X} nes_scroll_x=${:02X}",
        bus.ram[0x071A], bus.ram[0x071C], bus.vdp_regs[8], bus.vdp_regs[9], bus.ram[0x0B0C]
    )?;
    writeln!(
        f,
        "route: area=${:02X} level=${:02X} fetch_timer=${:02X} end_y=${:02X} slide_timer=${:02X}",
        bus.ram[0x0760], bus.ram[0x075C], bus.ram[0x0757], bus.ram[0x0713], bus.ram[0x0785]
    )?;
    writeln!(
        f,
        "runtime: trap_marker=${:02X} unresolved_id=${:04X} vbuf_used=${:02X} ppu_addr=${:02X}{:02X} ppu_mask=${:02X}",
        bus.ram[0x0B1D],
        ((bus.ram[0x0B1C] as u16) << 8) | bus.ram[0x0B1B] as u16,
        bus.ram[0x0800],
        bus.ram[0x0B0F],
        bus.ram[0x0B10],
        bus.ram[0x0B09]
    )?;
    writeln!(
        f,
        "split_scroll: flags=${:02X} pre=${:02X}:${:02X} post=${:02X}:${:02X} render_split={}",
        bus.ram[0x0B20],
        bus.ram[0x0B21],
        bus.ram[0x0B22],
        bus.ram[0x0B23],
        bus.ram[0x0B24],
        bus.render_scroll_split
            .map(|(line, top_x, top_y)| format!(
                "line={line} top_reg8=${top_x:02X} top_reg9=${top_y:02X}"
            ))
            .unwrap_or_else(|| "none".to_string())
    )?;
    writeln!(
        f,
        "vdp: r0=${:02X} r10=${:02X} vram_writes={} cram_writes={} data_writes={} control_writes={} status_reads={} controller_reads={}",
        bus.vdp_regs[0],
        bus.vdp_regs[10],
        bus.vram_writes,
        bus.cram_writes,
        bus.vdp_data_writes,
        bus.vdp_control_writes,
        bus.vdp_status_reads,
        bus.controller_reads
    )?;
    writeln!(
        f,
        "cram: {}",
        bus.cram
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(" ")
    )?;
    writeln!(
        f,
        "counts: nametable_nonzero={} chr_nonzero={} active_sprites={}",
        nametable_nonzero_bytes(bus),
        chr_nonzero_bytes(bus),
        active_sprite_count(bus)
    )?;
    writeln!(
        f,
        "nt_attr_shadow_nonzero={} first_nonzero={}",
        nt_attr_shadow_nonzero_bytes(bus),
        format_nt_attr_shadow_first_nonzero(bus)
    )?;
    writeln!(f, "{}", format_nt_trace_ciram_summary(bus, true))?;
    writeln!(f, "{}", format_nt_trace_ciram_summary(bus, false))?;
    writeln!(f, "{}", format_nt_dry_project_summary(bus, true))?;
    writeln!(f, "{}", format_nt_dry_project_summary(bus, false))?;
    writeln!(f, "{}", format_nt_folded_s_compact_mismatches(bus))?;
    writeln!(
        f,
        "{}",
        format_bgv_base_shadow_mismatches(bus, &bus.rom, context.symbols)
    )?;
    writeln!(
        f,
        "{}",
        format_bgv_recompute_folded(bus, &bus.rom, context.symbols)
    )?;
    writeln!(
        f,
        "{}",
        format_bgv_base_from_dry_ciram_mismatches(bus, &bus.rom, context.symbols, true)
    )?;
    writeln!(
        f,
        "{}",
        format_bgv_base_from_dry_ciram_mismatches(bus, &bus.rom, context.symbols, false)
    )?;
    writeln!(
        f,
        "{}",
        format_bgv_recompute_ciram(bus, &bus.rom, context.symbols, context.expected_mirroring)
    )?;
    writeln!(
        f,
        "{}",
        format_nt_materializer_expected_delta(bus, context.expected_mirroring)
    )?;
    writeln!(
        f,
        "{}",
        format_nt_materializer_dirty_visible(
            bus,
            context.expected_mirroring,
            context.prev_coarse_scroll,
            context.curr_coarse_scroll,
        )
    )?;
    writeln!(
        f,
        "{}",
        format_materializer_budget_snapshots(
            context.materializer_budget_sims,
            context.expected_mirroring,
            actual_frame,
        )
    )?;
    writeln!(
        f,
        "{}",
        format_materializer_policy_snapshots(
            context.materializer_policy_sims,
            bus,
            context.expected_mirroring,
            context.prev_coarse_scroll,
            context.curr_coarse_scroll,
            current_render_state(bus),
        )
    )?;
    writeln!(
        f,
        "{}",
        format_runtime_materializer_hooks(context.runtime_materializer_monitor)
    )?;
    writeln!(f, "{}", format_nt_raw_write_stats(bus))?;
    writeln!(f, "{}", format_nt_raw_frame_stats(bus))?;
    writeln!(f, "{}", format_nt_raw_shadow_parity(bus))?;
    writeln!(f, "{}", format_raw_ciram_storage_decision())?;
    writeln!(f, "{}", format_raw_ciram_backend(bus))?;
    writeln!(f, "{}", format_bgv_recompute_runtime_cost(bus))?;
    writeln!(f, "{}", format_ram_migration_access(bus))?;
    writeln!(f, "{}", format_ram_migration_dependency(bus))?;
    writeln!(f, "{}", format_cc_folded_s_dependency(bus))?;
    writeln!(f, "{}", format_d3xx_storage_candidate(bus))?;
    writeln!(f, "{}", format_d3xx_dirty_bitmap_candidate(bus))?;
    writeln!(f, "{}", format_d3xx_full_dirty_bitmap_candidate(bus))?;
    writeln!(f, "{}", format_d3xx_dirty_runtime_cost(bus))?;
    writeln!(
        f,
        "nt_columns_nonzero_cells: {}",
        format_nametable_column_occupancy(bus)
    )?;
    writeln!(f, "{}", format_nt_fold_collisions(bus))?;
    writeln!(f, "{}", format_nt_explicit_s_mismatches(bus, false))?;
    writeln!(f, "{}", format_nt_explicit_s_mismatches(bus, true))?;
    writeln!(f, "framebuffer: {}", ppm_path.display())?;
    write_checkpoint_sat_diagnostics(&mut f, bus)?;

    println!(
        "CHECKPOINT {} frame={} actual_frame={} ppm={} state={}",
        checkpoint.name,
        checkpoint.frame,
        actual_frame,
        ppm_path.display(),
        txt_path.display()
    );

    Ok(())
}

fn nametable_nonzero_bytes(bus: &SmsBus) -> usize {
    (0..1792).filter(|i| bus.vram[0x3700 + i] != 0).count()
}

fn nt_attr_shadow_nonzero_bytes(bus: &SmsBus) -> usize {
    // Runtime $CB80-$CBFF maps to SMS RAM offset $0B80-$0BFF.
    (0..0x80).filter(|i| bus.ram[0x0B80 + i] != 0).count()
}

fn format_nt_attr_shadow_first_nonzero(bus: &SmsBus) -> String {
    let entries = (0..0x80)
        .filter_map(|i| {
            let value = bus.ram[0x0B80 + i];
            (value != 0).then(|| format!("{:02X}:{value:02X}", i))
        })
        .take(12)
        .collect::<Vec<_>>();
    if entries.is_empty() {
        "none".to_string()
    } else {
        entries.join(" ")
    }
}

fn format_nt_trace_ciram_summary(bus: &SmsBus, vertical_mirroring: bool) -> String {
    let (mode, ciram) = if vertical_mirroring {
        ("vertical", &bus.nt_trace_ciram_vertical)
    } else {
        ("horizontal", &bus.nt_trace_ciram_horizontal)
    };
    let tile_nonzero = (0..2)
        .flat_map(|page| (0..0x3C0).map(move |i| page * 0x400 + i))
        .filter(|i| ciram[*i] != 0)
        .count();
    let attr_nonzero = (0..2)
        .flat_map(|page| (0..0x40).map(move |i| page * 0x400 + 0x3C0 + i))
        .filter(|i| ciram[*i] != 0)
        .count();
    let first = ciram
        .iter()
        .enumerate()
        .filter_map(|(i, value)| (*value != 0).then(|| format!("{i:03X}:{value:02X}")))
        .take(12)
        .collect::<Vec<_>>();
    let first = if first.is_empty() {
        "none".to_string()
    } else {
        first.join(" ")
    };
    format!(
        "nt_trace_ciram_{mode}=writes:{} tile_writes:{} attr_writes:{} tile_nonzero:{} attr_nonzero:{} first={first}",
        bus.nt_trace_ciram_writes,
        bus.nt_trace_ciram_tile_writes,
        bus.nt_trace_ciram_attr_writes,
        tile_nonzero,
        attr_nonzero
    )
}

fn nt_trace_ciram(bus: &SmsBus, vertical_mirroring: bool) -> &[u8; 0x800] {
    if vertical_mirroring {
        &bus.nt_trace_ciram_vertical
    } else {
        &bus.nt_trace_ciram_horizontal
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NtDryProjection {
    dry_tile: u8,
    folded_tile: u8,
    ppu_addr: u16,
    source_row: usize,
    source_col: usize,
}

fn nt_dry_project_tile_for_cell(
    bus: &SmsBus,
    row: usize,
    col: usize,
    vertical_mirroring: bool,
) -> NtDryProjection {
    let scroll_x = bus.ram[0x0B0C] as usize;
    let scroll_y = bus.ram[0x0B0D] as usize;
    let ppu_ctrl = bus.ram[0x0B08];
    let x = col + scroll_x / 8;
    let y = row + scroll_y / 8;
    let nt_x = (ppu_ctrl & 0x01) as usize;
    let nt_y = ((ppu_ctrl >> 1) & 0x01) as usize;
    let page_x = nt_x + x / 32;
    let page_y = nt_y + y / 30;
    let nt_page = (page_y & 1) * 2 + (page_x & 1);
    let source_row = y % 30;
    let source_col = x % 32;
    let ppu_offset = nt_page * 0x400 + source_row * 32 + source_col;
    let ppu_addr = 0x2000 + ppu_offset as u16;
    let ciram = nt_trace_ciram(bus, vertical_mirroring);
    let dry_tile = ciram[nt_ciram_index(ppu_addr, vertical_mirroring)];
    let folded_cell = source_row * 32 + source_col;
    let folded_tile = bus.nt_trace_folded_source_tiles[folded_cell];
    NtDryProjection {
        dry_tile,
        folded_tile,
        ppu_addr,
        source_row,
        source_col,
    }
}

fn format_nt_dry_project_summary(bus: &SmsBus, vertical_mirroring: bool) -> String {
    let mode = if vertical_mirroring {
        "vertical"
    } else {
        "horizontal"
    };
    let mut diffs = 0usize;
    let mut rows_28_29 = 0usize;
    let mut examples = Vec::new();

    // Dry/approximate source-space projection: compare what a page-aware
    // materializer would read from reconstructed CIRAM against the trace-only
    // folded source-tile shadow that approximates today's SMS nametable source.
    // Do not compare against SMS VRAM tile bytes; those are generated variant
    // slots, not NES tile identities.
    for row in 0..28 {
        for col in 0..32 {
            let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
            if projection.source_row >= 28 {
                rows_28_29 += 1;
            }
            if projection.dry_tile != projection.folded_tile {
                diffs += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "r={row:02},c={col:02} dry={:02X} folded={:02X} ppu={:04X}",
                        projection.dry_tile, projection.folded_tile, projection.ppu_addr
                    ));
                }
            }
        }
    }

    let first = if examples.is_empty() {
        "none".to_string()
    } else {
        examples.join(" ")
    };
    format!(
        "nt_dry_project_{mode}=diffs:{diffs} rows_28_29:{rows_28_29} folded_writes:{} first={first}",
        bus.nt_trace_folded_source_tile_writes
    )
}

fn format_nt_materializer_expected_delta(
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
) -> String {
    let Some(vertical_mirroring) = expected_mirroring.vertical_flag() else {
        return "nt_materializer_expected=unknown diffs=unavailable reason=missing_or_conflicting_mirroring".to_string();
    };

    let mut diffs = 0usize;
    let mut cols = [0usize; 32];
    let mut rows = [0usize; 28];
    let mut examples = Vec::new();

    for (row, row_count) in rows.iter_mut().enumerate() {
        for (col, col_count) in cols.iter_mut().enumerate() {
            let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
            if projection.dry_tile != projection.folded_tile {
                diffs += 1;
                *row_count += 1;
                *col_count += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "r={row:02},c={col:02} dry={:02X} folded={:02X} ppu={:04X}",
                        projection.dry_tile, projection.folded_tile, projection.ppu_addr
                    ));
                }
            }
        }
    }

    let cols = cols
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(col, count)| format!("{col:02}:{count}"))
        .collect::<Vec<_>>();
    let rows = rows
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(row, count)| format!("{row:02}:{count}"))
        .collect::<Vec<_>>();
    let first = if examples.is_empty() {
        "none".to_string()
    } else {
        examples.join(" ")
    };
    format!(
        "nt_materializer_expected={} diffs={diffs} cols={} rows={} first={first}",
        expected_mirroring.label(),
        if cols.is_empty() {
            "none".to_string()
        } else {
            cols.join(" ")
        },
        if rows.is_empty() {
            "none".to_string()
        } else {
            rows.join(" ")
        }
    )
}

fn current_coarse_scroll(bus: &SmsBus) -> (u8, u8) {
    (bus.ram[0x0B0C] / 8, bus.ram[0x0B0D] / 8)
}

fn coarse_delta(prev: u8, curr: u8, modulus: i16) -> i16 {
    let mut delta = i16::from(curr) - i16::from(prev);
    let half = modulus / 2;
    if delta > half {
        delta -= modulus;
    } else if delta < -half {
        delta += modulus;
    }
    delta
}

fn format_materializer_work_estimate(prev: (u8, u8), curr: (u8, u8)) -> String {
    let dx = coarse_delta(prev.0, curr.0, 32);
    let dy = coarse_delta(prev.1, curr.1, 32);
    let work = materializer_entering_work(prev, curr);
    format!(
        "mat_c={},{} d={},{} work={work}/896",
        curr.0, curr.1, dx, dy
    )
}

fn materializer_entering_work(prev: (u8, u8), curr: (u8, u8)) -> usize {
    let dx = coarse_delta(prev.0, curr.0, 32);
    let dy = coarse_delta(prev.1, curr.1, 32);
    let entering_cols = usize::from(dx.unsigned_abs()).min(32);
    let entering_rows = usize::from(dy.unsigned_abs()).min(28);
    entering_cols * 28 + entering_rows * (32usize.saturating_sub(entering_cols))
}

fn materializer_is_entering_cell(row: usize, col: usize, prev: (u8, u8), curr: (u8, u8)) -> bool {
    let dx = coarse_delta(prev.0, curr.0, 32);
    let dy = coarse_delta(prev.1, curr.1, 32);
    let entering_cols = usize::from(dx.unsigned_abs()).min(32);
    let entering_rows = usize::from(dy.unsigned_abs()).min(28);
    let entering_col = if dx > 0 {
        col >= 32usize.saturating_sub(entering_cols)
    } else if dx < 0 {
        col < entering_cols
    } else {
        false
    };
    let entering_row = if dy > 0 {
        row >= 28usize.saturating_sub(entering_rows)
    } else if dy < 0 {
        row < entering_rows
    } else {
        false
    };
    entering_col || entering_row
}

fn materializer_dirty_set(bus: &SmsBus, vertical_mirroring: bool) -> &[u8; 0x800] {
    if vertical_mirroring {
        &bus.nt_materializer_dirty_vertical
    } else {
        &bus.nt_materializer_dirty_horizontal
    }
}

fn materializer_dirty_reason(bits: u8) -> &'static str {
    match bits {
        0x00 => "clean",
        0x01 => "tile",
        0x02 => "attr",
        0x03 => "tile+attr",
        0x04 => "enter",
        0x05 => "enter+tile",
        0x06 => "enter+attr",
        0x07 => "enter+tile+attr",
        _ => "unknown",
    }
}

fn materializer_visible_workset(
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> Option<Vec<MaterializerVisibleCell>> {
    let vertical_mirroring = expected_mirroring.vertical_flag()?;
    let dirty = materializer_dirty_set(bus, vertical_mirroring);
    let mut cells: Vec<Option<MaterializerVisibleCell>> = vec![None; 32 * 28];

    for row in 0..28 {
        for col in 0..32 {
            let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
            let ciram = nt_ciram_index(projection.ppu_addr, vertical_mirroring);
            let mut reason = dirty[ciram] & 0x03;
            if materializer_is_entering_cell(row, col, prev_coarse, curr_coarse) {
                reason |= 0x04;
            }
            if reason == 0 {
                continue;
            }
            let key = (row * 32 + col) as u16;
            cells[usize::from(key)] = Some(MaterializerVisibleCell {
                key,
                row,
                col,
                ppu_addr: projection.ppu_addr,
                ciram,
                reason,
            });
        }
    }

    Some(cells.into_iter().flatten().collect())
}

fn materializer_prioritized_workset(
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> Option<Vec<MaterializerVisibleCell>> {
    let mut entering = Vec::new();
    let mut dirty = Vec::new();
    for cell in materializer_visible_workset(bus, expected_mirroring, prev_coarse, curr_coarse)? {
        if cell.reason & 0x04 != 0 {
            entering.push(cell);
        } else {
            dirty.push(cell);
        }
    }
    entering.extend(dirty);
    Some(entering)
}

fn current_render_state(bus: &SmsBus) -> RenderState {
    if bus.ram[0x0B09] & 0x18 != 0 {
        RenderState::On
    } else {
        RenderState::Off
    }
}

fn format_nt_materializer_dirty_visible(
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> String {
    if expected_mirroring.vertical_flag().is_none() {
        return "nt_materializer_dirty_visible=unavailable expected=unknown reason=missing_or_conflicting_mirroring".to_string();
    };
    let mut total = 0usize;
    let mut dirty_unique = 0usize;
    let mut cols = [0usize; 32];
    let mut rows = [0usize; 28];
    let mut examples = Vec::new();

    if let Some(workset) =
        materializer_visible_workset(bus, expected_mirroring, prev_coarse, curr_coarse)
    {
        for cell in workset.iter().filter(|cell| cell.reason & 0x03 != 0) {
            total += 1;
            rows[cell.row] += 1;
            cols[cell.col] += 1;
            if cell.reason & 0x04 == 0 {
                dirty_unique += 1;
            }
            if examples.len() < 8 {
                examples.push(format!(
                    "r={:02},c={:02} ppu={:04X} ciram={:03X} reason={}",
                    cell.row,
                    cell.col,
                    cell.ppu_addr,
                    cell.ciram,
                    materializer_dirty_reason(cell.reason & 0x03)
                ));
            }
        }
    }

    let cols = cols
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(col, count)| format!("{col:02}:{count}"))
        .collect::<Vec<_>>();
    let rows = rows
        .iter()
        .enumerate()
        .filter(|(_, count)| **count != 0)
        .map(|(row, count)| format!("{row:02}:{count}"))
        .collect::<Vec<_>>();
    let entering = materializer_entering_work(prev_coarse, curr_coarse);
    let combined = entering + dirty_unique;
    let first = if examples.is_empty() {
        "none".to_string()
    } else {
        examples.join(" ")
    };
    format!(
        "nt_materializer_dirty_visible={total} expected={} cols={} rows={} entering={entering} dirty_unique={dirty_unique} combined={combined}/896 first={first}",
        expected_mirroring.label(),
        if cols.is_empty() {
            "none".to_string()
        } else {
            cols.join(" ")
        },
        if rows.is_empty() {
            "none".to_string()
        } else {
            rows.join(" ")
        }
    )
}

fn format_materializer_budget_step(step: &MaterializerBudgetStep) -> String {
    let age = step
        .oldest_age
        .map(|age| age.to_string())
        .unwrap_or_else(|| "none".to_string());
    let deferred = if step.deferred.is_empty() {
        "none".to_string()
    } else {
        step.deferred
            .iter()
            .map(|cell| {
                format!(
                    "{:02},{:02}/{}:{:04X}:{:03X}",
                    cell.row,
                    cell.col,
                    materializer_dirty_reason(cell.reason),
                    cell.ppu_addr,
                    cell.ciram
                )
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    format!(
        "mat_budget={} add={} processed={} backlog={} max={} age={} def={}",
        step.budget, step.added, step.processed, step.backlog, step.max_backlog, age, deferred
    )
}

fn format_materializer_budget_steps(steps: &[MaterializerBudgetStep]) -> String {
    if steps.is_empty() {
        "nt_materializer_budget=unavailable expected=unknown reason=missing_or_conflicting_mirroring"
            .to_string()
    } else {
        steps
            .iter()
            .map(format_materializer_budget_step)
            .collect::<Vec<_>>()
            .join(" ; ")
    }
}

fn step_materializer_budget_sims(
    sims: &mut [MaterializerBudgetSim],
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
    frame: usize,
) -> String {
    let Some(workset) =
        materializer_visible_workset(bus, expected_mirroring, prev_coarse, curr_coarse)
    else {
        return format_materializer_budget_steps(&[]);
    };
    let steps = sims
        .iter_mut()
        .map(|sim| sim.step(frame, &workset))
        .collect::<Vec<_>>();
    format_materializer_budget_steps(&steps)
}

fn format_materializer_budget_snapshots(
    sims: &[MaterializerBudgetSim],
    expected_mirroring: ExpectedMirroring,
    frame: usize,
) -> String {
    if expected_mirroring.vertical_flag().is_none() {
        return format_materializer_budget_steps(&[]);
    }
    let steps = sims
        .iter()
        .map(|sim| sim.snapshot(frame))
        .collect::<Vec<_>>();
    format_materializer_budget_steps(&steps)
}

fn policy_cell_description(
    pending: &MaterializerPendingCell,
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> String {
    let row = usize::from(pending.key) / 32;
    let col = usize::from(pending.key) % 32;
    let Some(vertical_mirroring) = expected_mirroring.vertical_flag() else {
        return format!(
            "{row:02},{col:02}/{}:unknown",
            materializer_dirty_reason(pending.last_reason)
        );
    };
    let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
    let ciram = nt_ciram_index(projection.ppu_addr, vertical_mirroring);
    let mut reason = materializer_dirty_set(bus, vertical_mirroring)[ciram] & 0x03;
    if materializer_is_entering_cell(row, col, prev_coarse, curr_coarse) {
        reason |= 0x04;
    }
    if reason == 0 {
        reason = pending.last_reason;
    }
    format!(
        "{row:02},{col:02}/{}:{:04X}:{ciram:03X}",
        materializer_dirty_reason(reason),
        projection.ppu_addr
    )
}

fn format_materializer_policy_step(
    step: &MaterializerPolicyStep,
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> String {
    let deferred = if step.deferred.is_empty() {
        "none".to_string()
    } else {
        step.deferred
            .iter()
            .map(|pending| {
                policy_cell_description(pending, bus, expected_mirroring, prev_coarse, curr_coarse)
            })
            .collect::<Vec<_>>()
            .join("|")
    };
    format!(
        "mat_sched_budget={} render={} add={} processed={} before={} after={} max_after={} max_age={} stale_frames={} def={}",
        step.budget,
        step.render_state.label(),
        step.added,
        step.processed,
        step.before,
        step.after,
        step.max_after,
        step.max_age,
        step.stale_visible_frames,
        deferred
    )
}

fn format_materializer_policy_steps(
    steps: &[MaterializerPolicyStep],
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
) -> String {
    if steps.is_empty() {
        "nt_materializer_sched=unavailable expected=unknown reason=missing_or_conflicting_mirroring"
            .to_string()
    } else {
        steps
            .iter()
            .map(|step| {
                format_materializer_policy_step(
                    step,
                    bus,
                    expected_mirroring,
                    prev_coarse,
                    curr_coarse,
                )
            })
            .collect::<Vec<_>>()
            .join(" ; ")
    }
}

fn step_materializer_policy_sims(
    sims: &mut [MaterializerPolicySim],
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
    frame: usize,
) -> String {
    let Some(workset) =
        materializer_prioritized_workset(bus, expected_mirroring, prev_coarse, curr_coarse)
    else {
        return format_materializer_policy_steps(
            &[],
            bus,
            expected_mirroring,
            prev_coarse,
            curr_coarse,
        );
    };
    let render_state = current_render_state(bus);
    let steps = sims
        .iter_mut()
        .map(|sim| sim.step(frame, &workset, render_state))
        .collect::<Vec<_>>();
    format_materializer_policy_steps(&steps, bus, expected_mirroring, prev_coarse, curr_coarse)
}

fn format_materializer_policy_snapshots(
    sims: &[MaterializerPolicySim],
    bus: &SmsBus,
    expected_mirroring: ExpectedMirroring,
    prev_coarse: (u8, u8),
    curr_coarse: (u8, u8),
    render_state: RenderState,
) -> String {
    if expected_mirroring.vertical_flag().is_none() {
        return format_materializer_policy_steps(
            &[],
            bus,
            expected_mirroring,
            prev_coarse,
            curr_coarse,
        );
    }
    let steps = sims
        .iter()
        .map(|sim| sim.snapshot(0, render_state))
        .collect::<Vec<_>>();
    format_materializer_policy_steps(&steps, bus, expected_mirroring, prev_coarse, curr_coarse)
}

fn format_runtime_materializer_offense(offense: &Option<RuntimeMaterializerOffense>) -> String {
    offense
        .as_ref()
        .map(|offense| {
            format!(
                "step={} pc=${:04X} symbol={} cb09=${:02X} vdp_r1=${:02X}",
                offense.step, offense.pc, offense.symbol, offense.cb09, offense.vdp_reg1
            )
        })
        .unwrap_or_else(|| "none".to_string())
}

fn format_runtime_materializer_hooks(monitor: &RuntimeMaterializerMonitor) -> String {
    if monitor.hooks.is_empty() {
        return "mat_runtime_hooks=unavailable symbols=none".to_string();
    }
    let symbols = monitor
        .hooks
        .iter()
        .map(|hook| format!("{}:${:04X}", hook.name, hook.addr))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "mat_runtime_hooks=calls_on={} calls_off={} vdp_writes_on={} vdp_writes_off={} first_on_call={} first_on_vdp={} symbols={}",
        monitor.calls_on,
        monitor.calls_off,
        monitor.vdp_writes_on,
        monitor.vdp_writes_off,
        format_runtime_materializer_offense(&monitor.first_on_call),
        format_runtime_materializer_offense(&monitor.first_on_vdp),
        symbols
    )
}

fn format_nt_raw_write_stats(bus: &SmsBus) -> String {
    let total = bus.nt_raw_tile_writes_on
        + bus.nt_raw_tile_writes_off
        + bus.nt_raw_attr_writes_on
        + bus.nt_raw_attr_writes_off;
    format!(
        "nt_raw_write_stats=tile_on={} tile_off={} attr_on={} attr_off={} total={}",
        bus.nt_raw_tile_writes_on,
        bus.nt_raw_tile_writes_off,
        bus.nt_raw_attr_writes_on,
        bus.nt_raw_attr_writes_off,
        total
    )
}

fn format_nt_raw_frame_stats(bus: &SmsBus) -> String {
    format!(
        "nt_raw_frame_stats=max_frame_tile={} max_frame_attr={} max_frame_total={} max_burst={} first_burst_frame={} first_burst_step={}",
        bus.nt_raw_max_frame_tile_writes,
        bus.nt_raw_max_frame_attr_writes,
        bus.nt_raw_max_frame_total_writes,
        bus.nt_raw_max_burst,
        bus.nt_raw_max_burst_frame,
        bus.nt_raw_max_burst_step
    )
}

fn format_nt_raw_shadow_parity(bus: &SmsBus) -> String {
    format!(
        "nt_raw_shadow_parity=unavailable reason=runtime_raw_ciram_missing trace_writes={}",
        bus.nt_trace_ciram_writes
    )
}

fn format_raw_ciram_storage_decision() -> &'static str {
    "raw_ciram_storage=blocked reason=no_internal_ram_without_reclaim required_tile_bytes=1920 attr_bytes_existing=128 candidate=$CC00-$D3FF blocked_by=folded_s_reclaim_required stack_candidate=$DD80-$DFFD:no_go"
}

fn format_raw_ciram_backend(bus: &SmsBus) -> String {
    let ciram_nonzero = bus.cart_ram[..0x0800]
        .iter()
        .filter(|value| **value != 0)
        .count();
    format!(
        "raw_ciram_backend=sram_slot2 base=$8000 size=2048 mapper_ctrl=${:02X} reads={} writes={} ciram_nonzero={} caveat=standard_sega_mapper_sram_scaffold",
        bus.mapper_control, bus.cart_ram_reads, bus.cart_ram_writes, ciram_nonzero
    )
}

fn format_z80_stack_low_water(watermark: Z80StackWatermark) -> String {
    format!(
        "z80_stack_low_water=sp=${:04X} used={}",
        watermark.low_sp,
        watermark.used()
    )
}

fn nt_folded_cc_s(bus: &SmsBus, cell: usize) -> u8 {
    let shadow_addr = 0x0C01 + cell * 2;
    bus.ram[shadow_addr] & 0x03
}

fn nt_folded_compact_s(bus: &SmsBus, cell: usize) -> u8 {
    let byte = bus.ram[0x1300 + cell / 4];
    (byte >> ((cell & 0x03) * 2)) & 0x03
}

fn format_nt_folded_s_compact_mismatches(bus: &SmsBus) -> String {
    if !bus.nt_folded_s_compact_available {
        return "nt_folded_s_compact_mismatch=unavailable reason=compact_shadow_retired"
            .to_string();
    }

    let mut total = 0usize;
    let mut examples = Vec::new();
    for cell in 0..(32 * 28) {
        let folded = nt_folded_cc_s(bus, cell);
        let compact = nt_folded_compact_s(bus, cell);
        if folded != compact {
            total += 1;
            if examples.len() < 8 {
                let row = cell / 32;
                let col = cell % 32;
                examples.push(format!(
                    "cell={row:02},{col:02} cc={folded} compact={compact}"
                ));
            }
        }
    }
    if examples.is_empty() {
        format!("nt_folded_s_compact_mismatch={total} first=none")
    } else {
        format!(
            "nt_folded_s_compact_mismatch={total} first={}",
            examples.join(" ")
        )
    }
}

fn expected_base_slot_for_tile(
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
    table1: bool,
    tile: u8,
) -> Option<u8> {
    let label = if table1 {
        "data_chr_bg_map1"
    } else {
        "data_chr_bg_map0"
    };
    // The map entries are two-byte records; byte 0 is the base SMS tile slot.
    rom_byte_at_symbol(rom, symbols, label, usize::from(tile) * 2)
}

#[derive(Debug, Clone)]
struct BgvRecomputeStats {
    compared: usize,
    mismatches: usize,
    missing_map: bool,
    first_mismatch: Option<String>,
}

fn bgv_recompute_status(stats: &BgvRecomputeStats, blocked_status: &'static str) -> &'static str {
    if stats.missing_map || stats.compared == 0 {
        "unavailable"
    } else if stats.mismatches == 0 {
        "ready"
    } else {
        blocked_status
    }
}

fn bgv_recompute_from_folded_stats(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
) -> BgvRecomputeStats {
    let table1 = bus.ram[0x0B08] & 0x10 != 0;
    let mut stats = BgvRecomputeStats {
        compared: 0,
        mismatches: 0,
        missing_map: false,
        first_mismatch: None,
    };
    for cell in 0..(32 * 28) {
        if !bus.nt_trace_folded_source_tile_seen[cell] {
            continue;
        }
        let tile = bus.nt_trace_folded_source_tiles[cell];
        let Some(expected) = expected_base_slot_for_tile(rom, symbols, table1, tile) else {
            stats.missing_map = true;
            continue;
        };
        stats.compared += 1;
        let actual = bus.ram[0x1A00 + cell];
        if actual != expected {
            stats.mismatches += 1;
            if stats.first_mismatch.is_none() {
                let row = cell / 32;
                let col = cell % 32;
                stats.first_mismatch = Some(format!(
                    "cell={row:02},{col:02} tile={tile:02X} shadow={actual:02X} expected={expected:02X}"
                ));
            }
        }
    }
    stats
}

fn format_bgv_recompute_folded(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
) -> String {
    let stats = bgv_recompute_from_folded_stats(bus, rom, symbols);
    let status = bgv_recompute_status(&stats, "blocked");
    let reason = if stats.missing_map {
        " reason=missing_data_chr_bg_map"
    } else if stats.compared == 0 {
        " reason=no_folded_source_tiles_seen"
    } else {
        ""
    };
    format!(
        "bgv_recompute_folded=status={status} mismatches={} compared={} first={} runtime_reclaim=blocked_by_raw_source_missing{reason}",
        stats.mismatches,
        stats.compared,
        stats.first_mismatch.unwrap_or_else(|| "none".to_string())
    )
}

fn bgv_recompute_from_ciram_stats(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
    vertical_mirroring: bool,
) -> BgvRecomputeStats {
    let table1 = bus.ram[0x0B08] & 0x10 != 0;
    let mut stats = BgvRecomputeStats {
        compared: 0,
        mismatches: 0,
        missing_map: false,
        first_mismatch: None,
    };
    for row in 0..28 {
        for col in 0..32 {
            let cell = row * 32 + col;
            let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
            let Some(expected) =
                expected_base_slot_for_tile(rom, symbols, table1, projection.dry_tile)
            else {
                stats.missing_map = true;
                continue;
            };
            stats.compared += 1;
            let actual = bus.ram[0x1A00 + cell];
            if actual != expected {
                stats.mismatches += 1;
                if stats.first_mismatch.is_none() {
                    stats.first_mismatch = Some(format!(
                        "cell={row:02},{col:02} ppu={:04X} dry_tile={:02X} shadow={actual:02X} expected={expected:02X}",
                        projection.ppu_addr, projection.dry_tile
                    ));
                }
            }
        }
    }
    stats
}

fn format_bgv_recompute_ciram(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
    expected_mirroring: ExpectedMirroring,
) -> String {
    let Some(vertical_mirroring) = expected_mirroring.vertical_flag() else {
        return "bgv_recompute_ciram=status=unavailable expected=unknown mismatches=0 compared=0 first=none reason=missing_or_conflicting_mirroring runtime_reclaim=blocked_by_raw_source_missing".to_string();
    };
    let stats = bgv_recompute_from_ciram_stats(bus, rom, symbols, vertical_mirroring);
    let status = bgv_recompute_status(&stats, "blocked_by_folded_projection_bug");
    let reason = if stats.missing_map {
        " reason=missing_data_chr_bg_map"
    } else if stats.compared == 0 {
        " reason=no_ciram_projection_compared"
    } else {
        ""
    };
    format!(
        "bgv_recompute_ciram=status={status} expected={} mismatches={} compared={} first={} runtime_reclaim=blocked_by_raw_source_missing{reason}",
        expected_mirroring.label(),
        stats.mismatches,
        stats.compared,
        stats.first_mismatch.unwrap_or_else(|| "none".to_string())
    )
}

fn format_bgv_recompute_runtime_cost(bus: &SmsBus) -> String {
    let tile_total = bus.bgv_runtime_tile_shadow_writes_on + bus.bgv_runtime_tile_shadow_writes_off;
    let attr_cells_total =
        bus.bgv_runtime_attr_recompute_cells_on + bus.bgv_runtime_attr_recompute_cells_off;
    let observed_render = if bus.bgv_runtime_tile_shadow_writes_on == 0
        && bus.bgv_runtime_attr_recompute_cells_on == 0
        && (tile_total != 0 || attr_cells_total != 0)
    {
        "all_off"
    } else if tile_total == 0 && attr_cells_total == 0 {
        "none"
    } else {
        "mixed_or_on"
    };
    format!(
        "bgv_recompute_runtime_cost=da00_reclaim=blocked_by_missing_runtime_source tile_shadow_on={} tile_shadow_off={} attr_recompute_cells_on={} attr_recompute_cells_off={} max_frame_tile_shadow={} max_frame_attr_cells={} max_frame_pressure={} max_burst_pressure={} first_burst_frame={} first_burst_step={} observed_render={} caveat=trace_only_no_runtime_source",
        bus.bgv_runtime_tile_shadow_writes_on,
        bus.bgv_runtime_tile_shadow_writes_off,
        bus.bgv_runtime_attr_recompute_cells_on,
        bus.bgv_runtime_attr_recompute_cells_off,
        bus.bgv_runtime_max_frame_tile_shadow_writes,
        bus.bgv_runtime_max_frame_attr_recompute_cells,
        bus.bgv_runtime_max_frame_pressure,
        bus.bgv_runtime_max_burst_pressure,
        bus.bgv_runtime_max_burst_frame,
        bus.bgv_runtime_max_burst_step,
        observed_render
    )
}

fn format_ram_migration_top(bus: &SmsBus) -> String {
    let mut parts = Vec::new();
    for range in [
        RamMigrationRange::CcFoldedS,
        RamMigrationRange::D300CompactS,
        RamMigrationRange::Da00BgvBase,
    ] {
        for kind in [RamMigrationAccessKind::Read, RamMigrationAccessKind::Write] {
            let idx = range.index() * 2
                + match kind {
                    RamMigrationAccessKind::Read => 0,
                    RamMigrationAccessKind::Write => 1,
                };
            let mut entries = bus.ram_migration_pc_counts[idx]
                .iter()
                .map(|(pc, count)| (*pc, *count))
                .collect::<Vec<_>>();
            entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let tops = entries
                .into_iter()
                .take(3)
                .map(|(pc, count)| format!("${pc:04X}:{count}"))
                .collect::<Vec<_>>();
            parts.push(format!(
                "{}_{}:{}",
                range.label(),
                kind.label(),
                if tops.is_empty() {
                    "none".to_string()
                } else {
                    tops.join("|")
                }
            ));
        }
    }
    parts.join(",")
}

fn format_pc_count_top(map: &HashMap<u16, u32>) -> String {
    let mut entries = map
        .iter()
        .map(|(pc, count)| (*pc, *count))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let tops = entries
        .into_iter()
        .take(3)
        .map(|(pc, count)| format!("${pc:04X}:{count}"))
        .collect::<Vec<_>>();
    if tops.is_empty() {
        "none".to_string()
    } else {
        tops.join("|")
    }
}

fn bitmap_count_bits(bitmap: &[u8]) -> u32 {
    bitmap.iter().map(|byte| byte.count_ones()).sum()
}

fn bitmap_count_nonzero_bytes(bitmap: &[u8]) -> usize {
    bitmap.iter().filter(|byte| **byte != 0).count()
}

fn format_d3xx_storage_top(bus: &SmsBus) -> String {
    let mut parts = Vec::new();
    for range in [D3xxStorageRange::D300D3df, D3xxStorageRange::D3e0D3ff] {
        for kind in [RamMigrationAccessKind::Read, RamMigrationAccessKind::Write] {
            let idx = range.index() * 2
                + match kind {
                    RamMigrationAccessKind::Read => 0,
                    RamMigrationAccessKind::Write => 1,
                };
            parts.push(format!(
                "{}_{}:{}",
                range.label(),
                kind.label(),
                format_pc_count_top(&bus.d3xx_storage_pc_counts[idx])
            ));
        }
    }
    parts.join(",")
}

fn format_d3xx_storage_candidate(bus: &SmsBus) -> String {
    let d300 = bus.d3xx_storage_counts[D3xxStorageRange::D300D3df.index()];
    let d3e0 = bus.d3xx_storage_counts[D3xxStorageRange::D3e0D3ff.index()];
    let d300_reads = d300[0] + d300[1];
    let d300_writes = d300[2] + d300[3];
    let d3e0_reads = d3e0[0] + d3e0[1];
    let d3e0_writes = d3e0[2] + d3e0[3];
    let d300_accesses = d300_reads + d300_writes;
    let d3e0_accesses = d3e0_reads + d3e0_writes;
    let available = if d300_accesses == 0 && d3e0_accesses == 0 {
        256
    } else {
        224
    };
    let dirty_bitmap_fit = if available >= 240 { "yes" } else { "no" };
    let status = if d300_accesses != 0 {
        "blocked_by_d300_d3df_access"
    } else if d3e0_accesses != 0 {
        "blocked_by_d3e0_d3ff_access"
    } else {
        "reserved_for_metadata_only"
    };
    format!(
        "d3xx_storage_candidate=d300_d3df_reads={} d300_d3df_writes={} d3e0_d3ff_reads={} d3e0_d3ff_writes={} top={} raw_tile_shadow=fits:no required=1920 available={} folded_s_compact=fits:yes required=224 caveat=hot_read_no_go raw_tile_dirty_bitmap=fits:{} required=240 available={} status={} caveat=trace_only_storage_candidate",
        d300_reads,
        d300_writes,
        d3e0_reads,
        d3e0_writes,
        format_d3xx_storage_top(bus),
        available,
        dirty_bitmap_fit,
        available,
        status
    )
}

fn format_d3xx_dirty_bitmap_candidate(bus: &SmsBus) -> String {
    let vertical_bits = bitmap_count_bits(&bus.d3xx_tile_dirty_bitmap_vertical);
    let horizontal_bits = bitmap_count_bits(&bus.d3xx_tile_dirty_bitmap_horizontal);
    let vertical_bytes = bitmap_count_nonzero_bytes(&bus.d3xx_tile_dirty_bitmap_vertical);
    let horizontal_bytes = bitmap_count_nonzero_bytes(&bus.d3xx_tile_dirty_bitmap_horizontal);
    format!(
        "d3xx_dirty_bitmap_candidate=layout=$D300-$D3EF bytes_required={} bytes_available=256 spare=$D3F0-$D3FF vertical_bits={} horizontal_bits={} vertical_bytes={} horizontal_bytes={} max_frame_vertical_bits={} max_frame_horizontal_bits={} attr_dirty=not_represented raw_tile_shadow=fits:no status=fits_metadata_only caveat=trace_only_no_runtime_writes",
        D3XX_TILE_DIRTY_BITMAP_BYTES,
        vertical_bits,
        horizontal_bits,
        vertical_bytes,
        horizontal_bytes,
        bus.d3xx_tile_dirty_max_frame_vertical_bits,
        bus.d3xx_tile_dirty_max_frame_horizontal_bits,
    )
}

fn format_d3xx_full_dirty_bitmap_candidate(bus: &SmsBus) -> String {
    let vertical_tile_bits = bitmap_count_bits(&bus.d3xx_tile_dirty_bitmap_vertical);
    let horizontal_tile_bits = bitmap_count_bits(&bus.d3xx_tile_dirty_bitmap_horizontal);
    let vertical_attr_bits = bitmap_count_bits(&bus.d3xx_attr_dirty_bitmap_vertical);
    let horizontal_attr_bits = bitmap_count_bits(&bus.d3xx_attr_dirty_bitmap_horizontal);
    let vertical_bytes = bitmap_count_nonzero_bytes(&bus.d3xx_tile_dirty_bitmap_vertical)
        + bitmap_count_nonzero_bytes(&bus.d3xx_attr_dirty_bitmap_vertical);
    let horizontal_bytes = bitmap_count_nonzero_bytes(&bus.d3xx_tile_dirty_bitmap_horizontal)
        + bitmap_count_nonzero_bytes(&bus.d3xx_attr_dirty_bitmap_horizontal);
    format!(
        "d3xx_full_dirty_bitmap_candidate=layout=tile:$D300-$D3EF,attr:$D3F0-$D3FF bytes_required={} bytes_available=256 vertical_tile_bits={} horizontal_tile_bits={} vertical_attr_bits={} horizontal_attr_bits={} vertical_bytes={} horizontal_bytes={} max_frame_vertical_tile_bits={} max_frame_horizontal_tile_bits={} max_frame_vertical_attr_bits={} max_frame_horizontal_attr_bits={} raw_tile_shadow=fits:no status=fits_all_dirty_metadata caveat=trace_only_no_runtime_writes",
        D3XX_TILE_DIRTY_BITMAP_BYTES + D3XX_ATTR_DIRTY_BITMAP_BYTES,
        vertical_tile_bits,
        horizontal_tile_bits,
        vertical_attr_bits,
        horizontal_attr_bits,
        vertical_bytes,
        horizontal_bytes,
        bus.d3xx_tile_dirty_max_frame_vertical_bits,
        bus.d3xx_tile_dirty_max_frame_horizontal_bits,
        bus.d3xx_attr_dirty_max_frame_vertical_bits,
        bus.d3xx_attr_dirty_max_frame_horizontal_bits,
    )
}

fn format_d3xx_dirty_runtime_cost(bus: &SmsBus) -> String {
    let tile_on = bus.nt_raw_tile_writes_on;
    let tile_off = bus.nt_raw_tile_writes_off;
    let attr_on = bus.nt_raw_attr_writes_on;
    let attr_off = bus.nt_raw_attr_writes_off;
    let on = tile_on + attr_on;
    let off = tile_off + attr_off;
    let render_observed = match (on != 0, off != 0) {
        (false, false) => "none",
        (true, false) => "all_on",
        (false, true) => "all_off",
        (true, true) => "mixed",
    };
    format!(
        "d3xx_dirty_runtime_cost=runtime_marking=blocked_until_raw_source tile_ops_on={} tile_ops_off={} attr_ops_on={} attr_ops_off={} max_frame_tile_ops={} max_frame_attr_ops={} max_frame_total_ops={} max_burst_ops={} first_burst_frame={} first_burst_step={} render_observed={} caveat=trace_only_no_runtime_writes",
        tile_on,
        tile_off,
        attr_on,
        attr_off,
        bus.nt_raw_max_frame_tile_writes,
        bus.nt_raw_max_frame_attr_writes,
        bus.nt_raw_max_frame_total_writes,
        bus.nt_raw_max_burst,
        bus.nt_raw_max_burst_frame,
        bus.nt_raw_max_burst_step,
        render_observed,
    )
}

fn format_cc_folded_s_dependency(bus: &SmsBus) -> String {
    let subpal = bus.cc_folded_s_counts[0];
    let attr_compare = bus.cc_folded_s_counts[1];
    let attr_writes = bus.cc_folded_s_counts[2];
    let init_clear_writes = bus.cc_folded_s_counts[3];
    let other_reads = bus.cc_folded_s_counts[4];
    let other_writes = bus.cc_folded_s_counts[5];
    let other_total = other_reads[0] + other_reads[1] + other_writes[0] + other_writes[1];
    let subpal_total = subpal[0] + subpal[1];
    let reclaim = if other_total != 0 {
        "blocked_by_other_accesses"
    } else if subpal_total != 0 {
        "blocked_by_true_consumers"
    } else {
        "candidate_after_replacement_source"
    };
    format!(
        "cc_folded_s_dependency=subpal_reads_on={} subpal_reads_off={} attr_compare_reads_on={} attr_compare_reads_off={} attr_writes_on={} attr_writes_off={} init_clear_writes_on={} init_clear_writes_off={} other_reads_on={} other_reads_off={} other_writes_on={} other_writes_off={} max_frame_reads={} max_frame_writes={} read_top={} write_top={} cc_reclaim={} caveat=trace_only_dependency_classifier",
        subpal[0],
        subpal[1],
        attr_compare[0],
        attr_compare[1],
        attr_writes[0],
        attr_writes[1],
        init_clear_writes[0],
        init_clear_writes[1],
        other_reads[0],
        other_reads[1],
        other_writes[0],
        other_writes[1],
        bus.cc_folded_s_max_frame_reads
            .max(bus.cc_folded_s_frame_reads),
        bus.cc_folded_s_max_frame_writes
            .max(bus.cc_folded_s_frame_writes),
        format_pc_count_top(&bus.cc_folded_s_read_pc_counts),
        format_pc_count_top(&bus.cc_folded_s_write_pc_counts),
        reclaim
    )
}

fn format_ram_migration_access(bus: &SmsBus) -> String {
    let cc = bus.ram_migration_counts[RamMigrationRange::CcFoldedS.index()];
    let d300 = bus.ram_migration_counts[RamMigrationRange::D300CompactS.index()];
    let da00 = bus.ram_migration_counts[RamMigrationRange::Da00BgvBase.index()];
    format!(
        "ram_migration_access=cc_reads_on={} cc_reads_off={} cc_writes_on={} cc_writes_off={} d300_reads_on={} d300_reads_off={} d300_writes_on={} d300_writes_off={} da00_reads_on={} da00_reads_off={} da00_writes_on={} da00_writes_off={} max_frame_cc={} max_frame_d300={} max_frame_da00={} top={} caveat=trace_only_internal_ram_accesses",
        cc[0],
        cc[1],
        cc[2],
        cc[3],
        d300[0],
        d300[1],
        d300[2],
        d300[3],
        da00[0],
        da00[1],
        da00[2],
        da00[3],
        bus.ram_migration_max_frame_accesses[RamMigrationRange::CcFoldedS.index()],
        bus.ram_migration_max_frame_accesses[RamMigrationRange::D300CompactS.index()],
        bus.ram_migration_max_frame_accesses[RamMigrationRange::Da00BgvBase.index()],
        format_ram_migration_top(bus)
    )
}

fn format_ram_migration_dependency(bus: &SmsBus) -> String {
    let d300 = bus.ram_migration_counts[RamMigrationRange::D300CompactS.index()];
    let reclaim = if bus.d300_true_reads == 0 {
        "ready_if_no_true_consumers"
    } else {
        "blocked_by_true_consumers"
    };
    let write_top_idx = RamMigrationRange::D300CompactS.index() * 2 + 1;
    format!(
        "ram_migration_dependency=d300_true_reads={} d300_rmw_reads={} d300_writes={} d300_reads_on={} d300_reads_off={} d300_writes_on={} d300_writes_off={} d300_read_top={} d300_write_top={} d300_rmw_top={} d300_true_top={} d300_reclaim={} caveat=trace_only_dependency_classifier",
        bus.d300_true_reads,
        bus.d300_rmw_reads,
        d300[2] + d300[3],
        d300[0],
        d300[1],
        d300[2],
        d300[3],
        format_pc_count_top(
            &bus.ram_migration_pc_counts[RamMigrationRange::D300CompactS.index() * 2]
        ),
        format_pc_count_top(&bus.ram_migration_pc_counts[write_top_idx]),
        format_pc_count_top(&bus.d300_rmw_read_pc_counts),
        format_pc_count_top(&bus.d300_true_read_pc_counts),
        reclaim
    )
}

fn format_bgv_base_shadow_mismatches(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
) -> String {
    let table1 = bus.ram[0x0B08] & 0x10 != 0;
    let mut compared = 0usize;
    let mut missing_map = false;
    let mut total = 0usize;
    let mut examples = Vec::new();

    for cell in 0..(32 * 28) {
        if !bus.nt_trace_folded_source_tile_seen[cell] {
            continue;
        }
        let tile = bus.nt_trace_folded_source_tiles[cell];
        let Some(expected) = expected_base_slot_for_tile(rom, symbols, table1, tile) else {
            missing_map = true;
            continue;
        };
        compared += 1;
        let actual = bus.ram[0x1A00 + cell]; // $DA00 + cell
        if actual != expected {
            total += 1;
            if examples.len() < 8 {
                let row = cell / 32;
                let col = cell % 32;
                examples.push(format!(
                    "cell={row:02},{col:02} tile={tile:02X} shadow={actual:02X} expected={expected:02X}"
                ));
            }
        }
    }

    if missing_map {
        return format!(
            "bgv_base_shadow_mismatch=unavailable compared:{compared} reason=missing_data_chr_bg_map"
        );
    }
    if examples.is_empty() {
        format!("bgv_base_shadow_mismatch={total} compared:{compared} first=none")
    } else {
        format!(
            "bgv_base_shadow_mismatch={total} compared:{compared} first={}",
            examples.join(" ")
        )
    }
}

fn format_bgv_base_from_dry_ciram_mismatches(
    bus: &SmsBus,
    rom: &[u8],
    symbols: &HashMap<String, (u8, u16)>,
    vertical_mirroring: bool,
) -> String {
    let mode = if vertical_mirroring {
        "vertical"
    } else {
        "horizontal"
    };
    let table1 = bus.ram[0x0B08] & 0x10 != 0;
    let mut compared = 0usize;
    let mut rows_28_29 = 0usize;
    let mut missing_map = false;
    let mut total = 0usize;
    let mut examples = Vec::new();

    for row in 0..28 {
        for col in 0..32 {
            let cell = row * 32 + col;
            let projection = nt_dry_project_tile_for_cell(bus, row, col, vertical_mirroring);
            if projection.source_row >= 28 {
                rows_28_29 += 1;
            }

            let Some(expected) =
                expected_base_slot_for_tile(rom, symbols, table1, projection.dry_tile)
            else {
                missing_map = true;
                continue;
            };
            compared += 1;
            let actual = bus.ram[0x1A00 + cell]; // $DA00 + visible cell
            if actual != expected {
                total += 1;
                if examples.len() < 8 {
                    examples.push(format!(
                        "cell={row:02},{col:02} ppu={:04X} dry_tile={:02X} shadow={actual:02X} expected={expected:02X}",
                        projection.ppu_addr, projection.dry_tile
                    ));
                }
            }
        }
    }

    if missing_map {
        return format!(
            "bgv_base_dry_ciram_{mode}_mismatch=unavailable compared:{compared} rows_28_29:{rows_28_29} reason=missing_data_chr_bg_map"
        );
    }
    if examples.is_empty() {
        format!(
            "bgv_base_dry_ciram_{mode}_mismatch={total} compared:{compared} rows_28_29:{rows_28_29} first=none"
        )
    } else {
        format!(
            "bgv_base_dry_ciram_{mode}_mismatch={total} compared:{compared} rows_28_29:{rows_28_29} first={}",
            examples.join(" ")
        )
    }
}

fn format_nametable_column_occupancy(bus: &SmsBus) -> String {
    let mut cols = [0usize; 32];
    for row in 0..28 {
        for (col, count) in cols.iter_mut().enumerate() {
            let off = 0x3700 + (row * 32 + col) * 2;
            if bus.vram[off] != 0 || bus.vram[off + 1] != 0 {
                *count += 1;
            }
        }
    }
    cols.iter()
        .enumerate()
        .map(|(col, count)| format!("{col:02}:{count:02}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_nt_fold_collisions(bus: &SmsBus) -> String {
    let mut examples = Vec::new();
    let mut total = 0usize;

    for (cell, pages) in bus.nt_fold_cell_pages.iter().copied().enumerate() {
        if pages.count_ones() <= 1 {
            continue;
        }
        total += 1;
        if examples.len() < 6 {
            let row = cell / 32;
            let col = cell % 32;
            let pages = (0..4)
                .filter(|page| pages & (1u8 << *page) != 0)
                .map(|page| page.to_string())
                .collect::<Vec<_>>()
                .join(",");
            examples.push(format!("cell={row:02},{col:02} pages={pages}"));
        }
    }

    if examples.is_empty() {
        format!("nt_fold_collisions={total} first=none")
    } else {
        format!("nt_fold_collisions={total} first={}", examples.join(" "))
    }
}

fn format_nt_explicit_s_mismatches(bus: &SmsBus, vertical_mirroring: bool) -> String {
    let (mode, total, examples) = if vertical_mirroring {
        (
            "vertical",
            bus.nt_explicit_s_mismatch_vertical,
            &bus.nt_explicit_s_mismatch_vertical_examples,
        )
    } else {
        (
            "horizontal",
            bus.nt_explicit_s_mismatch_horizontal,
            &bus.nt_explicit_s_mismatch_horizontal_examples,
        )
    };

    if examples.is_empty() {
        format!("nt_explicit_s_mismatch_{mode}={total} first=none")
    } else {
        let examples = examples
            .iter()
            .map(|example| {
                format!(
                    "ppu=${:04X} sms=${:04X} folded={} explicit={} attr={:02X}:{:02X}",
                    example.ppu_addr,
                    example.sms_addr,
                    example.folded_s,
                    example.explicit_s,
                    example.attr_index,
                    example.attr_byte
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("nt_explicit_s_mismatch_{mode}={total} first={examples}")
    }
}

fn chr_nonzero_bytes(bus: &SmsBus) -> usize {
    (0..0x3700).filter(|i| bus.vram[*i] != 0).count()
}

fn active_sprite_count(bus: &SmsBus) -> usize {
    let mut count = 0;
    for i in 0..64 {
        if bus.vram[0x3F00 + i] == 0xD0 {
            break;
        }
        count += 1;
    }
    count
}

fn sat_terminator_index(bus: &SmsBus) -> Option<usize> {
    (0..64).find(|i| bus.vram[0x3F00 + i] == 0xD0)
}

fn sprite_base_addr(bus: &SmsBus) -> usize {
    if bus.vdp_regs[6] & 0x04 != 0 {
        0x2000
    } else {
        0x0000
    }
}

fn vram_nonzero_range(bus: &SmsBus, start: usize, len: usize) -> usize {
    bus.vram[start..start + len]
        .iter()
        .filter(|byte| **byte != 0)
        .count()
}

#[derive(Clone, Copy, Debug)]
struct VisibleOamSprite {
    oam_index: usize,
    raw_y: u8,
    source_tile: u8,
    attr: u8,
    x: u8,
}

fn visible_oam_sprites(bus: &SmsBus) -> Vec<VisibleOamSprite> {
    let mut sprites = Vec::new();
    for oam_index in 0..64 {
        let base = 0x0900 + oam_index * 4; // $C900 OAM staging, RAM-indexed
        let raw_y = bus.ram[base];
        if raw_y >= 0xCF {
            continue;
        }
        sprites.push(VisibleOamSprite {
            oam_index,
            raw_y,
            source_tile: bus.ram[base + 1],
            attr: bus.ram[base + 2],
            x: bus.ram[base + 3],
        });
    }
    sprites
}

fn format_sprite_fallback_demand(bus: &SmsBus, active: usize, sprite_base: usize) -> String {
    const SAT_BLANK_REL: u8 = 0xA7;
    let visible = visible_oam_sprites(bus);
    let table = if bus.ram[0x0B08] & 0x08 != 0 { 1 } else { 0 };
    let mut fallback_entries = Vec::new();
    let mut unique = [false; 256];
    let mut unique_count = 0usize;
    let mut known_blank_count = 0usize;

    for sat_index in 0..active.min(64) {
        let resolved = bus.vram[0x3F80 + sat_index * 2 + 1];
        if resolved != SAT_BLANK_REL {
            continue;
        }
        let Some(source) = visible.get(sat_index) else {
            continue;
        };
        if !unique[usize::from(source.source_tile)] {
            unique[usize::from(source.source_tile)] = true;
            unique_count += 1;
        }
        if source.source_tile == 0xFC {
            known_blank_count += 1;
        }
        if fallback_entries.len() < 10 {
            fallback_entries.push(format!(
                "sat={sat_index:02} oam={:02} src=${:02X} attr=${:02X} x=${:02X} y=${:02X}",
                source.oam_index, source.source_tile, source.attr, source.x, source.raw_y
            ));
        }
    }

    let total = fallback_entries.len();
    let fallback_total = (0..active.min(64))
        .filter(|sat_index| bus.vram[0x3F80 + sat_index * 2 + 1] == SAT_BLANK_REL)
        .count();
    let first = if fallback_entries.is_empty() {
        "none".to_string()
    } else {
        fallback_entries.join(" ")
    };
    let unique_src = unique
        .iter()
        .enumerate()
        .filter_map(|(tile, seen)| seen.then_some(format!("${tile:02X}")))
        .collect::<Vec<_>>()
        .join(",");
    let unique_src = if unique_src.is_empty() {
        "none".to_string()
    } else {
        unique_src
    };
    format!(
        "sprite_fallback_demand=active_visible={} unique_src_count={} unique_src={} known_blank_fc={} table={} sprite_base=${:04X} fallback_tile=${:02X} reported={} first={}",
        fallback_total,
        unique_count,
        unique_src,
        known_blank_count,
        table,
        sprite_base,
        SAT_BLANK_REL,
        total,
        first
    )
}

fn write_checkpoint_sat_diagnostics<W: std::io::Write>(
    f: &mut W,
    bus: &SmsBus,
) -> std::io::Result<()> {
    let active = active_sprite_count(bus);
    let terminator = sat_terminator_index(bus);
    let sprite_base = sprite_base_addr(bus);
    let sprite_8x16 = bus.vdp_regs[1] & 0x02 != 0;
    let blank167_addr = sprite_base + 167 * 32;
    let blank167_nonzero = vram_nonzero_range(bus, blank167_addr, 32);
    let tail_start = terminator.unwrap_or(64);
    let tail_y_not_d0 = (tail_start..64)
        .filter(|i| bus.vram[0x3F00 + i] != 0xD0)
        .count();
    let tail_xtile_nonzero = (tail_start..64)
        .filter(|i| bus.vram[0x3F80 + i * 2] != 0 || bus.vram[0x3F80 + i * 2 + 1] != 0)
        .count();

    writeln!(
        f,
        "sat: r1=${:02X} r6=${:02X} ppu_ctrl=${:02X} sprite_base=${:04X} sprite_mode={} terminator={} active={} scratch_next={} blank167_addr=${:04X} blank167_nonzero_bytes={}",
        bus.vdp_regs[1],
        bus.vdp_regs[6],
        bus.ram[0x0B08],
        sprite_base,
        if sprite_8x16 { "8x16" } else { "8x8" },
        terminator
            .map(|i| i.to_string())
            .unwrap_or_else(|| "none".to_string()),
        active,
        bus.ram[0x1460],
        blank167_addr,
        blank167_nonzero,
    )?;
    writeln!(
        f,
        "sat_tail: y_not_d0_after_terminator={} xtile_nonzero_after_terminator={}",
        tail_y_not_d0, tail_xtile_nonzero
    )?;
    writeln!(
        f,
        "{}",
        format_sprite_fallback_demand(bus, active, sprite_base)
    )?;

    for i in 0..active.min(24) {
        let y = bus.vram[0x3F00 + i];
        let x = bus.vram[0x3F80 + i * 2];
        let tile = bus.vram[0x3F80 + i * 2 + 1];
        let attr = bus.ram[0x1480 + i];
        let tile_addr = sprite_base + tile as usize * 32;
        let tile_nonzero = if tile_addr + 32 <= bus.vram.len() {
            vram_nonzero_range(bus, tile_addr, 32)
        } else {
            0
        };
        writeln!(
            f,
            "sat_entry[{i:02}]: y=${y:02X} screen_y={} x=${x:02X} tile=${tile:02X} attr=${attr:02X} tile_addr=${tile_addr:04X} tile_nonzero_bytes={} behind_bg={} hflip={} vflip={} pal={}",
            y.wrapping_add(1),
            tile_nonzero,
            attr & 0x20 != 0,
            attr & 0x40 != 0,
            attr & 0x80 != 0,
            attr & 0x03,
        )?;
    }

    Ok(())
}

/// Render the current VRAM/CRAM state to a 256x224 RGB PPM image
/// so we can verify what the SMS *would* show without needing mednafen.
/// Handles background nametable, scroll, and a coarse line-scroll split; sprites
/// are overlaid after the background pass.
fn dump_framebuffer_ppm(bus: &SmsBus, path: &str) -> std::io::Result<()> {
    use std::io::Write;
    const W: usize = 256;
    const H: usize = 224;
    let mut pixels = vec![0u8; W * H * 3];
    let mut bg_priority = vec![false; W * H];

    // SMS CRAM byte → RGB. Each entry: --BBGGRR (2 bits per channel, 0-3).
    let cram_to_rgb = |b: u8| -> (u8, u8, u8) {
        let r = (b & 0x03) as u32;
        let g = ((b >> 2) & 0x03) as u32;
        let bl = ((b >> 4) & 0x03) as u32;
        let scale = |c: u32| (c * 255 / 3) as u8;
        (scale(r), scale(g), scale(bl))
    };

    for screen_y in 0..H {
        // Split timing and R0 top-row horizontal lock are output-scanline
        // decisions. Pick the scroll registers for this displayed line first,
        // then sample the nametable through the inverse scroll transform.
        let (base_reg8, reg9) =
            if let Some((split_line, top_reg8, top_reg9)) = bus.render_scroll_split {
                if screen_y < split_line {
                    (top_reg8 as usize, top_reg9 as usize)
                } else {
                    (bus.vdp_regs[8] as usize, bus.vdp_regs[9] as usize)
                }
            } else {
                (bus.vdp_regs[8] as usize, bus.vdp_regs[9] as usize)
            };
        let lock_top = bus.vdp_regs[0] & 0x40 != 0;
        let reg8 = if lock_top && screen_y < 16 {
            0
        } else {
            base_reg8
        };
        let source_y = (screen_y + reg9) % H;
        let row = source_y / 8;
        let py = source_y % 8;

        for screen_x in 0..W {
            let source_x = (screen_x + W - (reg8 % W)) & 0xFF;
            let col = source_x / 8;
            let px = source_x % 8;
            let off = 0x3700 + (row * 32 + col) * 2;
            let lo = bus.vram[off];
            let hi = bus.vram[off + 1];
            let tile_index = ((hi as u16 & 1) << 8) | lo as u16;
            let palette_offset = if hi & 0x08 != 0 { 16 } else { 0 };
            let tile_addr = (tile_index as usize) * 32;
            if tile_addr + 32 > 0x4000 {
                continue;
            }
            let p0 = bus.vram[tile_addr + py * 4];
            let p1 = bus.vram[tile_addr + py * 4 + 1];
            let p2 = bus.vram[tile_addr + py * 4 + 2];
            let p3 = bus.vram[tile_addr + py * 4 + 3];
            let bit = 7 - px;
            let c = ((p0 >> bit) & 1)
                | (((p1 >> bit) & 1) << 1)
                | (((p2 >> bit) & 1) << 2)
                | (((p3 >> bit) & 1) << 3);
            // SMS sprite/background priority is controlled by the nametable
            // priority bit, not by the staged NES OAM behind-background bit.
            // Color 0 remains transparent and must not mask sprites.
            bg_priority[screen_y * W + screen_x] = hi & 0x10 != 0 && c != 0;
            let color = bus.cram[palette_offset + c as usize];
            let (r, g, b) = cram_to_rgb(color);
            let pi = (screen_y * W + screen_x) * 3;
            pixels[pi] = r;
            pixels[pi + 1] = g;
            pixels[pi + 2] = b;
        }
    }
    eprintln!(
        "framebuffer scroll: reg8={} reg9={} split={:?} | NES scrollX($CB0C)={} cam_lo($071C)={} cam_pg($071A)={} playerX($0086)={} playerPg($006D)={} | bg_variant_pool_next($CA00)={}",
        bus.vdp_regs[8],
        bus.vdp_regs[9],
        bus.render_scroll_split,
        bus.ram[0x0B0C],
        bus.ram[0x071C],
        bus.ram[0x071A],
        bus.ram[0x0086],
        bus.ram[0x006D],
        bus.ram[0x0A00]
    );

    // ── Sprite layer overlay ──────────────────────────────────────────────
    // SAT layout in VRAM at $3F00:
    //   $3F00..$3F3F  64 Y positions (1 byte each). Y==$D0 hides remaining.
    //   $3F80..$3FFF  64 (X, tile_number) pairs (2 bytes each).
    // Sprites use the sprite palette at CRAM[16..32]. Color 0 = transparent.
    let mut sat_entries = Vec::new();
    for i in 0..64 {
        let y = bus.vram[0x3F00 + i];
        if y == 0xD0 {
            break;
        } // terminator: remaining sprites hidden
        sat_entries.push(i);
    }
    let mut active_sprites = 0;
    // Lower SAT/OAM indices have higher sprite priority. Draw later entries
    // first so earlier entries are composited last and remain visible.
    for i in sat_entries.into_iter().rev() {
        let y = bus.vram[0x3F00 + i];
        let x = bus.vram[0x3F80 + i * 2];
        let tile = bus.vram[0x3F80 + i * 2 + 1] as usize;
        // SMS sprite Y is the byte value, displayed one line below
        // (y == 0 means line 1). Skip if off-screen.
        let sy_top = y as usize + 1;
        if sy_top >= H {
            continue;
        }
        let sprite_base = if bus.vdp_regs[6] & 0x04 != 0 {
            0x2000
        } else {
            0x0000
        };
        let tile_addr = sprite_base + tile * 32;
        if tile_addr + 32 > 0x4000 {
            continue;
        }
        active_sprites += 1;
        for py in 0..8 {
            let p0 = bus.vram[tile_addr + py * 4];
            let p1 = bus.vram[tile_addr + py * 4 + 1];
            let p2 = bus.vram[tile_addr + py * 4 + 2];
            let p3 = bus.vram[tile_addr + py * 4 + 3];
            for px in 0..8 {
                let bit = 7 - px;
                let c = ((p0 >> bit) & 1)
                    | (((p1 >> bit) & 1) << 1)
                    | (((p2 >> bit) & 1) << 2)
                    | (((p3 >> bit) & 1) << 3);
                if c == 0 {
                    continue;
                } // transparent
                let color = bus.cram[16 + c as usize];
                let (r, g, b) = cram_to_rgb(color);
                let sx = x as usize + px;
                let sy = sy_top + py;
                if sx >= W || sy >= H {
                    continue;
                }
                let pi = sy * W + sx;
                if bg_priority[pi] {
                    continue;
                }
                let pi = pi * 3;
                pixels[pi] = r;
                pixels[pi + 1] = g;
                pixels[pi + 2] = b;
            }
        }
    }
    let _ = active_sprites; // kept for potential future logging

    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{W} {H}\n255\n")?;
    f.write_all(&pixels)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_button_event_and_active_low_buttons() {
        let (frame, port) = parse_button_event("80:right,a").unwrap();
        assert_eq!(frame, 80);
        assert_eq!(port & (1 << 3), 0);
        assert_eq!(port & (1 << 4), 0);
        assert_ne!(port & (1 << 5), 0);
    }

    #[test]
    fn parses_checkpoint_colon_or_equals() {
        let checkpoint = parse_checkpoint_spec("123:title-initial").unwrap();
        assert_eq!(checkpoint.frame, 123);
        assert_eq!(checkpoint.name, "title-initial");

        let checkpoint = parse_checkpoint_spec("456=1-1 initial").unwrap();
        assert_eq!(checkpoint.frame, 456);
        assert_eq!(checkpoint.name, "1-1 initial");
    }

    #[test]
    fn checkpoint_slug_is_filesystem_safe() {
        assert_eq!(checkpoint_slug("Title Initial"), "title_initial");
        assert_eq!(
            checkpoint_slug("1-1: flagpole / transition"),
            "1-1_flagpole_transition"
        );
        assert_eq!(checkpoint_slug("!!!"), "checkpoint");
    }

    #[test]
    fn parses_wla_symbol_lines_for_call_labels() {
        assert_eq!(
            parse_wla_symbol_line("00:0492 _bgv_sub_palette"),
            Some((0x0492, "_bgv_sub_palette".to_string()))
        );
        assert_eq!(
            parse_wla_symbol_definition("11:8000 data_chr_bg_map0"),
            Some(("data_chr_bg_map0".to_string(), 0x11, 0x8000))
        );
        assert_eq!(parse_wla_symbol_line("[labels]"), None);

        let mut symbols = HashMap::new();
        symbols.insert(
            0x0492,
            vec!["_bgv_sub_palette".to_string(), "alias".to_string()],
        );
        assert_eq!(
            format_symbol_suffix(&symbols, 0x0492),
            " _bgv_sub_palette/alias"
        );
        assert_eq!(format_symbol_suffix(&symbols, 0x1234), "");
    }

    #[test]
    fn base_shadow_diagnostic_compares_seen_source_tiles() {
        let mut bus = SmsBus::new(vec![0; 0x4000], 0xFF);
        let mut symbols = HashMap::new();
        symbols.insert("data_chr_bg_map0".to_string(), (0, 0x8000));
        symbols.insert("data_chr_bg_map1".to_string(), (0, 0x8200));

        let mut rom = vec![0; 0x500];
        rom[0x10] = 0x42; // map0[tile 8].base
        bus.nt_trace_folded_source_tile_seen[3] = true;
        bus.nt_trace_folded_source_tiles[3] = 8;
        bus.ram[0x1A00 + 3] = 0x42;

        assert_eq!(
            format_bgv_base_shadow_mismatches(&bus, &rom, &symbols),
            "bgv_base_shadow_mismatch=0 compared:1 first=none"
        );

        bus.ram[0x1A00 + 3] = 0x24;
        assert_eq!(
            format_bgv_base_shadow_mismatches(&bus, &rom, &symbols),
            "bgv_base_shadow_mismatch=1 compared:1 first=cell=00,03 tile=08 shadow=24 expected=42"
        );
    }

    #[test]
    fn dry_ciram_base_shadow_diagnostic_compares_projected_tiles() {
        let mut bus = SmsBus::new(vec![0; 0x4000], 0xFF);
        let mut symbols = HashMap::new();
        symbols.insert("data_chr_bg_map0".to_string(), (0, 0x8000));
        symbols.insert("data_chr_bg_map1".to_string(), (0, 0x8200));

        let mut rom = vec![0; 0x500];
        rom[0x10] = 0x42; // map0[tile 8].base
        bus.nt_trace_ciram_vertical[3] = 8;
        bus.ram[0x1A00 + 3] = 0x24;

        assert_eq!(
            format_bgv_base_from_dry_ciram_mismatches(&bus, &rom, &symbols, true),
            "bgv_base_dry_ciram_vertical_mismatch=1 compared:896 rows_28_29:0 first=cell=00,03 ppu=2003 dry_tile=08 shadow=24 expected=42"
        );

        assert_eq!(
            format_bgv_base_from_dry_ciram_mismatches(&bus, &rom, &HashMap::new(), false),
            "bgv_base_dry_ciram_horizontal_mismatch=unavailable compared:0 rows_28_29:0 reason=missing_data_chr_bg_map"
        );
    }

    #[test]
    fn bgv_recompute_folded_reports_ready_when_shadow_matches() {
        let mut bus = SmsBus::new(vec![0; 0x4000], 0xFF);
        let mut symbols = HashMap::new();
        symbols.insert("data_chr_bg_map0".to_string(), (0, 0x8000));
        let mut rom = vec![0; 0x100];
        rom[0x10] = 0x42; // map0[tile 8].base
        bus.nt_trace_folded_source_tile_seen[3] = true;
        bus.nt_trace_folded_source_tiles[3] = 8;
        bus.ram[0x1A00 + 3] = 0x42;

        assert_eq!(
            format_bgv_recompute_folded(&bus, &rom, &symbols),
            "bgv_recompute_folded=status=ready mismatches=0 compared=1 first=none runtime_reclaim=blocked_by_raw_source_missing"
        );
    }

    #[test]
    fn bgv_recompute_folded_reports_blocked_on_mismatch() {
        let mut bus = SmsBus::new(vec![0; 0x4000], 0xFF);
        let mut symbols = HashMap::new();
        symbols.insert("data_chr_bg_map0".to_string(), (0, 0x8000));
        let mut rom = vec![0; 0x100];
        rom[0x10] = 0x42;
        bus.nt_trace_folded_source_tile_seen[3] = true;
        bus.nt_trace_folded_source_tiles[3] = 8;
        bus.ram[0x1A00 + 3] = 0x24;

        assert_eq!(
            format_bgv_recompute_folded(&bus, &rom, &symbols),
            "bgv_recompute_folded=status=blocked mismatches=1 compared=1 first=cell=00,03 tile=08 shadow=24 expected=42 runtime_reclaim=blocked_by_raw_source_missing"
        );
    }

    #[test]
    fn bgv_recompute_ciram_reports_blocked_status_and_unknown_unavailable() {
        let mut bus = SmsBus::new(vec![0; 0x4000], 0xFF);
        let mut symbols = HashMap::new();
        symbols.insert("data_chr_bg_map0".to_string(), (0, 0x8000));
        let mut rom = vec![0; 0x100];
        rom[0x10] = 0x42;
        bus.nt_trace_ciram_vertical[3] = 8;
        bus.ram[0x1A00 + 3] = 0x24;

        assert!(format_bgv_recompute_ciram(&bus, &rom, &symbols, ExpectedMirroring::Vertical)
            .contains("bgv_recompute_ciram=status=blocked_by_folded_projection_bug expected=vertical mismatches=1 compared=896 first=cell=00,03 ppu=2003 dry_tile=08 shadow=24 expected=42 runtime_reclaim=blocked_by_raw_source_missing"));
        assert_eq!(
            format_bgv_recompute_ciram(&bus, &rom, &symbols, ExpectedMirroring::Unknown),
            "bgv_recompute_ciram=status=unavailable expected=unknown mismatches=0 compared=0 first=none reason=missing_or_conflicting_mirroring runtime_reclaim=blocked_by_raw_source_missing"
        );
    }

    #[test]
    fn loads_checkpoint_script_with_comments_and_blanks() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "trace_sms_checkpoint_test_{}_{}.txt",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(
            &path,
            "# comment\n\n80:title\n220:game start # trailing comment\n",
        )
        .unwrap();

        let checkpoints = load_checkpoint_script(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(checkpoints.len(), 2);
        assert_eq!(checkpoints[0].frame, 80);
        assert_eq!(checkpoints[0].name, "title");
        assert_eq!(checkpoints[1].frame, 220);
        assert_eq!(checkpoints[1].name, "game start");
    }

    #[test]
    fn nt_fold_collision_diagnostic_tracks_tile_pages_only() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_nt_fold_write(0x3700);

        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x00;
        bus.record_nt_fold_write(0x3701);

        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_nt_fold_write(0x3702);

        assert_eq!(bus.nt_fold_cell_pages[0], 0b0011);
        assert_eq!(bus.nt_fold_cell_pages[1], 0);
        assert_eq!(
            format_nt_fold_collisions(&bus),
            "nt_fold_collisions=1 first=cell=00,00 pages=0,1"
        );
    }

    #[test]
    fn explicit_s_mismatch_diagnostic_compares_attr_shadow() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        // Folded rendering would read $CC01 for SMS cell $3700 and use S=0.
        bus.ram[0x0C01] = 0;
        // Under horizontal mirroring, NES $2400 aliases physical CIRAM page 0,
        // whose first attribute byte selects S=2 for the top-left quadrant.
        bus.ram[0x0B80] = 0b0000_0010;
        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x00;

        bus.record_nt_fold_write(0x3700);

        assert_eq!(bus.nt_explicit_s_mismatch_horizontal, 1);
        assert_eq!(bus.nt_explicit_s_mismatch_vertical, 0);
        assert_eq!(
            format_nt_explicit_s_mismatches(&bus, false),
            "nt_explicit_s_mismatch_horizontal=1 first=ppu=$2400 sms=$3700 folded=0 explicit=2 attr=00:02"
        );
        assert_eq!(
            format_nt_explicit_s_mismatches(&bus, true),
            "nt_explicit_s_mismatch_vertical=0 first=none"
        );
    }

    #[test]
    fn nt_ciram_index_obeys_horizontal_and_vertical_mirroring() {
        assert_eq!(nt_ciram_index(0x2000, true), 0x000);
        assert_eq!(nt_ciram_index(0x2400, true), 0x400);
        assert_eq!(nt_ciram_index(0x2800, true), 0x000);
        assert_eq!(nt_ciram_index(0x2C00, true), 0x400);

        assert_eq!(nt_ciram_index(0x2000, false), 0x000);
        assert_eq!(nt_ciram_index(0x2400, false), 0x000);
        assert_eq!(nt_ciram_index(0x2800, false), 0x400);
        assert_eq!(nt_ciram_index(0x2C00, false), 0x400);
    }

    #[test]
    fn trace_ppu_write_call_reconstructs_raw_ciram_without_runtime_writes() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B0F] = 0x24;
        bus.ram[0x0B10] = 0x12;
        bus.record_trace_ppu_write_call(7, 0xAB);

        assert_eq!(bus.nt_trace_ciram_vertical[0x412], 0xAB);
        assert_eq!(bus.nt_trace_ciram_horizontal[0x012], 0xAB);
        assert_eq!(bus.nt_trace_ciram_writes, 1);
        assert_eq!(bus.nt_trace_ciram_tile_writes, 1);
        assert_eq!(bus.nt_trace_ciram_attr_writes, 0);
        assert_eq!(bus.nt_trace_folded_source_tiles[0x012], 0xAB);
        assert_eq!(bus.nt_trace_folded_source_tile_writes, 1);

        bus.ram[0x0B0F] = 0x27;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call(7, 0x55);
        bus.record_trace_ppu_write_call(6, 0xFF);

        assert_eq!(bus.nt_trace_ciram_vertical[0x7C0], 0x55);
        assert_eq!(bus.nt_trace_ciram_horizontal[0x3C0], 0x55);
        assert_eq!(bus.nt_trace_ciram_writes, 2);
        assert_eq!(bus.nt_trace_ciram_tile_writes, 1);
        assert_eq!(bus.nt_trace_ciram_attr_writes, 1);
        assert_eq!(bus.nt_trace_folded_source_tile_writes, 1);
        assert!(format_nt_trace_ciram_summary(&bus, false).contains("attr_nonzero:1"));
    }

    #[test]
    fn dry_project_compares_ciram_projection_to_folded_source_tiles() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B08] = 0x01; // base nametable page 1 ($2400)
        bus.nt_trace_ciram_vertical[0x400] = 0x44;
        bus.nt_trace_ciram_horizontal[0x000] = 0x33;
        bus.nt_trace_folded_source_tiles[0x000] = 0x22;
        bus.nt_trace_folded_source_tile_writes = 1;

        let vertical = nt_dry_project_tile_for_cell(&bus, 0, 0, true);
        assert_eq!(vertical.ppu_addr, 0x2400);
        assert_eq!(vertical.source_row, 0);
        assert_eq!(vertical.source_col, 0);
        assert_eq!(vertical.dry_tile, 0x44);
        assert_eq!(vertical.folded_tile, 0x22);

        let horizontal = nt_dry_project_tile_for_cell(&bus, 0, 0, false);
        assert_eq!(horizontal.ppu_addr, 0x2400);
        assert_eq!(horizontal.dry_tile, 0x33);
        assert_eq!(horizontal.folded_tile, 0x22);

        assert!(format_nt_dry_project_summary(&bus, true).contains(
            "nt_dry_project_vertical=diffs:1 rows_28_29:0 folded_writes:1 first=r=00,c=00 dry=44 folded=22 ppu=2400"
        ));
    }

    #[test]
    fn dry_project_reports_when_scroll_y_uses_hidden_nes_rows() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0D] = 224; // 28 coarse rows

        let projection = nt_dry_project_tile_for_cell(&bus, 0, 0, true);
        assert_eq!(projection.source_row, 28);
        assert!(format_nt_dry_project_summary(&bus, true).contains("rows_28_29:64"));
    }

    #[test]
    fn materializer_expected_delta_groups_by_rows_and_columns() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.nt_trace_ciram_vertical[0] = 0x11;
        bus.nt_trace_ciram_vertical[1] = 0x22;
        bus.nt_trace_folded_source_tiles[0] = 0x10;
        bus.nt_trace_folded_source_tiles[1] = 0x22;

        assert_eq!(
            format_nt_materializer_expected_delta(&bus, ExpectedMirroring::Vertical),
            "nt_materializer_expected=vertical diffs=1 cols=00:1 rows=00:1 first=r=00,c=00 dry=11 folded=10 ppu=2000"
        );
        assert_eq!(
            format_nt_materializer_expected_delta(&bus, ExpectedMirroring::Unknown),
            "nt_materializer_expected=unknown diffs=unavailable reason=missing_or_conflicting_mirroring"
        );
    }

    #[test]
    fn materializer_work_estimate_formats_coarse_scroll_delta() {
        assert_eq!(
            format_materializer_work_estimate((11, 0), (12, 0)),
            "mat_c=12,0 d=1,0 work=28/896"
        );
        assert_eq!(
            format_materializer_work_estimate((31, 0), (0, 1)),
            "mat_c=0,1 d=1,1 work=59/896"
        );
    }

    #[test]
    fn materializer_dirty_visible_tracks_tile_writes() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x03;
        bus.record_trace_ppu_write_call(7, 0x44);

        assert_eq!(
            format_nt_materializer_dirty_visible(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0),),
            "nt_materializer_dirty_visible=1 expected=vertical cols=03:1 rows=00:1 entering=0 dirty_unique=1 combined=1/896 first=r=00,c=03 ppu=2003 ciram=003 reason=tile"
        );
        bus.clear_materializer_dirty();
        assert!(format_nt_materializer_dirty_visible(
            &bus,
            ExpectedMirroring::Vertical,
            (0, 0),
            (0, 0),
        )
        .contains("nt_materializer_dirty_visible=0"));
    }

    #[test]
    fn materializer_dirty_visible_tracks_attr_4x4_blocks() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call(7, 0xFF);

        let summary =
            format_nt_materializer_dirty_visible(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0));
        assert!(summary.contains("nt_materializer_dirty_visible=16"));
        assert!(summary.contains("cols=00:4 01:4 02:4 03:4"));
        assert!(summary.contains("rows=00:4 01:4 02:4 03:4"));
        assert!(summary.contains("reason=attr"));
    }

    #[test]
    fn materializer_dirty_visible_handles_unknown_and_entering_overlap() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x1F;
        bus.record_trace_ppu_write_call(7, 0x44);

        assert_eq!(
            format_nt_materializer_dirty_visible(&bus, ExpectedMirroring::Unknown, (0, 0), (0, 0),),
            "nt_materializer_dirty_visible=unavailable expected=unknown reason=missing_or_conflicting_mirroring"
        );
        assert!(format_nt_materializer_dirty_visible(
            &bus,
            ExpectedMirroring::Vertical,
            (0, 0),
            (1, 0),
        )
        .contains("entering=28 dirty_unique=0 combined=28/896"));
    }

    #[test]
    fn materializer_budget_dedupes_entering_and_dirty_cells() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x1F; // visible row 0, entering col 31 for dx=1
        bus.record_trace_ppu_write_call(7, 0x44);

        let workset =
            materializer_visible_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (1, 0))
                .unwrap();
        assert_eq!(workset.len(), 28); // one entering column, dirty cell overlaps it
        assert_eq!(workset.iter().filter(|cell| cell.key == 31).count(), 1);

        let mut sim = MaterializerBudgetSim::new(28);
        let step = sim.step(1, &workset);
        assert_eq!(step.added, 28);
        assert_eq!(step.processed, 28);
        assert_eq!(step.backlog, 0);
    }

    #[test]
    fn materializer_budget_backlog_persists_and_drains() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call(7, 0x11);
        bus.ram[0x0B10] = 0x01;
        bus.record_trace_ppu_write_call(7, 0x22);

        let workset =
            materializer_visible_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0))
                .unwrap();
        let mut sim = MaterializerBudgetSim::new(1);
        let first = sim.step(10, &workset);
        assert_eq!(
            (
                first.added,
                first.processed,
                first.backlog,
                first.max_backlog
            ),
            (2, 1, 1, 2)
        );
        assert_eq!(first.oldest_age, Some(0));

        bus.clear_materializer_dirty();
        let empty = materializer_visible_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0))
            .unwrap();
        let second = sim.step(11, &empty);
        assert_eq!(
            (
                second.added,
                second.processed,
                second.backlog,
                second.max_backlog
            ),
            (0, 1, 0, 2)
        );
    }

    #[test]
    fn materializer_budget_reports_unknown_mirroring() {
        let mut sims = MaterializerBudgetSim::new_all();
        let bus = SmsBus::new(Vec::new(), 0xFF);
        assert_eq!(
            step_materializer_budget_sims(
                &mut sims,
                &bus,
                ExpectedMirroring::Unknown,
                (0, 0),
                (0, 0),
                0,
            ),
            "nt_materializer_budget=unavailable expected=unknown reason=missing_or_conflicting_mirroring"
        );
    }

    #[test]
    fn materializer_policy_prioritizes_entering_before_dirty() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call(7, 0x11);

        let workset =
            materializer_prioritized_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (1, 0))
                .unwrap();
        assert_eq!(workset[0].key, 31); // entering right edge comes before dirty cell 0
        assert!(
            workset
                .iter()
                .any(|cell| cell.key == 0 && cell.reason & 0x01 != 0)
        );
    }

    #[test]
    fn materializer_policy_recomputes_pending_payload_from_latest_state() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        let mut sim = MaterializerPolicySim::new(0);

        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call(7, 0x11);
        let first =
            materializer_prioritized_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0))
                .unwrap();
        sim.step(0, &first, RenderState::On);

        bus.clear_materializer_dirty();
        bus.ram[0x0B0C] = 8; // visible cell 0 now projects to NES $2001
        let step = sim.snapshot(1, RenderState::On);
        let rendered = format_materializer_policy_step(
            &step,
            &bus,
            ExpectedMirroring::Vertical,
            (1, 0),
            (1, 0),
        );
        assert!(rendered.contains("00,00/tile:2001:001"));
        assert!(!rendered.contains("00,00/tile:2000:000"));
    }

    #[test]
    fn materializer_policy_tracks_max_after_backlog_and_age() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        let mut sim = MaterializerPolicySim::new(0);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call(7, 0x11);
        let workset =
            materializer_prioritized_workset(&bus, ExpectedMirroring::Vertical, (0, 0), (0, 0))
                .unwrap();
        let first = sim.step(5, &workset, RenderState::On);
        assert_eq!(
            (first.before, first.after, first.max_after, first.max_age),
            (1, 1, 1, 0)
        );
        assert_eq!(first.stale_visible_frames, 1);

        let second = sim.step(8, &workset, RenderState::Off);
        assert_eq!(second.after, 1);
        assert_eq!(second.max_after, 1);
        assert_eq!(second.max_age, 3);
        assert_eq!(second.stale_visible_frames, 1);

        let third = sim.step(9, &workset, RenderState::On);
        assert_eq!(third.max_age, 4);
        assert_eq!(third.stale_visible_frames, 2);
    }

    #[test]
    fn materializer_policy_reports_unknown_mirroring() {
        let mut sims = MaterializerPolicySim::new_all();
        let bus = SmsBus::new(Vec::new(), 0xFF);
        assert_eq!(
            step_materializer_policy_sims(
                &mut sims,
                &bus,
                ExpectedMirroring::Unknown,
                (0, 0),
                (0, 0),
                0,
            ),
            "nt_materializer_sched=unavailable expected=unknown reason=missing_or_conflicting_mirroring"
        );
    }

    #[test]
    fn runtime_materializer_hooks_report_unavailable_without_symbols() {
        let monitor = RuntimeMaterializerMonitor::new(&HashMap::new());
        assert_eq!(
            format_runtime_materializer_hooks(&monitor),
            "mat_runtime_hooks=unavailable symbols=none"
        );
    }

    #[test]
    fn runtime_materializer_hooks_count_render_on_and_off_calls() {
        let mut symbols = HashMap::new();
        symbols.insert("rt_nt_materialize_render_off".to_string(), (0, 0x1234));
        let mut monitor = RuntimeMaterializerMonitor::new(&symbols);
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0B09] = 0x00;
        monitor.observe_pc(10, 0x1234, 0xDFF0, &bus);
        bus.ram[0x0B09] = 0x18;
        bus.vdp_regs[1] = 0x40;
        monitor.observe_pc(11, 0x1234, 0xDFEE, &bus);

        let line = format_runtime_materializer_hooks(&monitor);
        assert!(line.contains("calls_on=1 calls_off=1"));
        assert!(line.contains(
            "first_on_call=step=11 pc=$1234 symbol=rt_nt_materialize_render_off cb09=$18 vdp_r1=$40"
        ));
    }

    #[test]
    fn runtime_materializer_hooks_report_render_on_vdp_writes() {
        let mut symbols = HashMap::new();
        symbols.insert("rt_nt_materializer_bulk".to_string(), (0, 0x2345));
        let mut monitor = RuntimeMaterializerMonitor::new(&symbols);
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B09] = 0x18;
        bus.vdp_regs[1] = 0x40;

        monitor.observe_pc(20, 0x2345, 0xDFF0, &bus);
        monitor.observe_vdp_writes(21, 0x2348, &bus, 3, RenderState::On);

        let line = format_runtime_materializer_hooks(&monitor);
        assert!(line.contains("vdp_writes_on=3 vdp_writes_off=0"));
        assert!(line.contains(
            "first_on_vdp=step=21 pc=$2348 symbol=rt_nt_materializer_bulk cb09=$18 vdp_r1=$40"
        ));
    }

    #[test]
    fn nt_ppu_write_kind_classifies_tiles_and_attrs_by_page_offset() {
        assert_eq!(nt_ppu_write_kind(0x2000), Some(NtWriteKind::Tile));
        assert_eq!(nt_ppu_write_kind(0x23BF), Some(NtWriteKind::Tile));
        assert_eq!(nt_ppu_write_kind(0x23C0), Some(NtWriteKind::Attr));
        assert_eq!(nt_ppu_write_kind(0x27FF), Some(NtWriteKind::Attr));
        assert_eq!(nt_ppu_write_kind(0x2BC0), Some(NtWriteKind::Attr));
        assert_eq!(nt_ppu_write_kind(0x3000), None);
    }

    #[test]
    fn nt_raw_write_stats_split_render_state_and_frames() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B09] = 0x18;
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call_at(7, 0x11, 7, 1);
        bus.ram[0x0B09] = 0x00;
        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call_at(7, 0x22, 8, 1);
        bus.finish_nt_raw_frame();

        assert_eq!(
            format_nt_raw_write_stats(&bus),
            "nt_raw_write_stats=tile_on=1 tile_off=0 attr_on=0 attr_off=1 total=2"
        );
        assert_eq!(
            format_nt_raw_frame_stats(&bus),
            "nt_raw_frame_stats=max_frame_tile=1 max_frame_attr=1 max_frame_total=2 max_burst=2 first_burst_frame=1 first_burst_step=8"
        );
    }

    #[test]
    fn bgv_recompute_runtime_cost_reports_blocked_render_split_pressure() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B09] = 0x18;
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call_at(7, 0x11, 7, 1);
        bus.ram[0x0B09] = 0x00;
        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call_at(7, 0x22, 8, 1);
        bus.finish_bgv_runtime_recompute_frame();

        assert_eq!(
            format_bgv_recompute_runtime_cost(&bus),
            "bgv_recompute_runtime_cost=da00_reclaim=blocked_by_missing_runtime_source tile_shadow_on=1 tile_shadow_off=0 attr_recompute_cells_on=0 attr_recompute_cells_off=16 max_frame_tile_shadow=1 max_frame_attr_cells=16 max_frame_pressure=17 max_burst_pressure=17 first_burst_frame=1 first_burst_step=8 observed_render=mixed_or_on caveat=trace_only_no_runtime_source"
        );
    }

    #[test]
    fn bgv_recompute_runtime_cost_observes_all_off_only_as_observation() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B09] = 0x00;
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call_at(7, 0x11, 1, 0);
        bus.finish_bgv_runtime_recompute_frame();

        assert!(format_bgv_recompute_runtime_cost(&bus).contains(
            "da00_reclaim=blocked_by_missing_runtime_source tile_shadow_on=0 tile_shadow_off=1"
        ));
        assert!(format_bgv_recompute_runtime_cost(&bus).contains("observed_render=all_off"));
    }

    #[test]
    fn ram_migration_range_classifies_shadow_addresses() {
        assert_eq!(
            ram_migration_range_for_physical_addr(0xCC00),
            Some(RamMigrationRange::CcFoldedS)
        );
        assert_eq!(
            ram_migration_range_for_physical_addr(0xD2FF),
            Some(RamMigrationRange::CcFoldedS)
        );
        assert_eq!(
            ram_migration_range_for_physical_addr(0xD300),
            Some(RamMigrationRange::D300CompactS)
        );
        assert_eq!(
            ram_migration_range_for_physical_addr(0xDA00),
            Some(RamMigrationRange::Da00BgvBase)
        );
        assert_eq!(ram_migration_range_for_physical_addr(0xDD80), None);
    }

    #[test]
    fn ram_migration_access_splits_render_state_and_top_pcs() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.watch_pc = 0x1111;
        bus.ram[0x0B09] = 0x18;
        bus.write(0xCC01, 0x02);
        let _ = bus.read(0xCC01);
        bus.watch_pc = 0x2222;
        bus.ram[0x0B09] = 0x00;
        let _ = bus.read(0xF300); // mirror of $D300
        bus.write(0xFA00, 0x42); // mirror of $DA00
        bus.finish_ram_migration_frame();

        let line = format_ram_migration_access(&bus);
        assert!(line.contains("cc_reads_on=1 cc_reads_off=0 cc_writes_on=1 cc_writes_off=0"));
        assert!(line.contains("d300_reads_on=0 d300_reads_off=1"));
        assert!(
            line.contains("da00_reads_on=0 da00_reads_off=0 da00_writes_on=0 da00_writes_off=1")
        );
        assert!(line.contains("max_frame_cc=2 max_frame_d300=1 max_frame_da00=1"));
        assert!(line.contains("cc_r:$1111:1"));
        assert!(line.contains("cc_w:$1111:1"));
        assert!(line.contains("d300_r:$2222:1"));
        assert!(line.contains("da00_w:$2222:1"));
    }

    #[test]
    fn ram_migration_dependency_classifies_true_and_rmw_d300_reads() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.d300_compact_store_range = Some((0x4000, 0x4010));

        bus.watch_pc = 0x4004;
        bus.ram[0x0B09] = 0x18;
        let _ = bus.read(0xD300);
        bus.write(0xD300, 0x01);

        bus.watch_pc = 0x5000;
        bus.ram[0x0B09] = 0x00;
        let _ = bus.read(0xD301);
        bus.finish_ram_migration_frame();

        let line = format_ram_migration_dependency(&bus);
        assert!(line.contains("d300_true_reads=1 d300_rmw_reads=1 d300_writes=1"));
        assert!(line.contains("d300_reads_on=1 d300_reads_off=1"));
        assert!(line.contains("d300_writes_on=1 d300_writes_off=0"));
        assert!(line.contains("d300_rmw_top=$4004:1"));
        assert!(line.contains("d300_true_top=$5000:1"));
        assert!(line.contains("d300_reclaim=blocked_by_true_consumers"));
    }

    #[test]
    fn ram_migration_dependency_reports_ready_when_only_rmw_reads() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.d300_compact_store_range = Some((0x4000, 0x4010));
        bus.watch_pc = 0x4004;
        let _ = bus.read(0xD300);
        bus.write(0xD300, 0x01);

        assert!(
            format_ram_migration_dependency(&bus)
                .contains("d300_reclaim=ready_if_no_true_consumers")
        );
    }

    #[test]
    fn ram_migration_dependency_keeps_pending_read_across_trace_stack_peek() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.d300_compact_store_range = Some((0x4000, 0x4010));
        bus.watch_pc = 0x4004;
        let _ = bus.read(0xD300);

        // The main trace loop peeks at the stack between instructions while
        // leaving watch_pc set to the next compact-store helper PC. That
        // non-shadow RAM read must not turn the compact read into a false
        // true-consumer classification.
        bus.watch_pc = 0x4008;
        let _ = bus.read(0xDFF0);
        bus.write(0xD300, 0x01);

        let line = format_ram_migration_dependency(&bus);
        assert!(line.contains("d300_true_reads=0 d300_rmw_reads=1"));
        assert!(line.contains("d300_reclaim=ready_if_no_true_consumers"));
    }

    #[test]
    fn d3xx_storage_candidate_reports_clean_full_range() {
        let bus = SmsBus::new(Vec::new(), 0xFF);

        let line = format_d3xx_storage_candidate(&bus);
        assert!(line.contains("d300_d3df_reads=0 d300_d3df_writes=0"));
        assert!(line.contains("d3e0_d3ff_reads=0 d3e0_d3ff_writes=0"));
        assert!(line.contains("top=d300_r:none,d300_w:none,d3e0_r:none,d3e0_w:none"));
        assert!(line.contains("raw_tile_shadow=fits:no required=1920 available=256"));
        assert!(line.contains("raw_tile_dirty_bitmap=fits:yes required=240 available=256"));
        assert!(line.contains("status=reserved_for_metadata_only"));
        assert!(line.contains("caveat=trace_only_storage_candidate"));
    }

    #[test]
    fn d3xx_storage_candidate_blocks_on_d3e0_gap_access() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.watch_pc = 0x3456;
        let _ = bus.read(0xD3E0);

        let line = format_d3xx_storage_candidate(&bus);
        assert!(line.contains("d300_d3df_reads=0 d300_d3df_writes=0"));
        assert!(line.contains("d3e0_d3ff_reads=1 d3e0_d3ff_writes=0"));
        assert!(line.contains("top=d300_r:none,d300_w:none,d3e0_r:$3456:1,d3e0_w:none"));
        assert!(line.contains("raw_tile_shadow=fits:no required=1920 available=224"));
        assert!(line.contains("raw_tile_dirty_bitmap=fits:no required=240 available=224"));
        assert!(line.contains("status=blocked_by_d3e0_d3ff_access"));
    }

    #[test]
    fn d3xx_storage_candidate_blocks_on_d300_subrange_access() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.watch_pc = 0x4567;
        bus.write(0xF300, 0x12);

        let line = format_d3xx_storage_candidate(&bus);
        assert!(line.contains("d300_d3df_reads=0 d300_d3df_writes=1"));
        assert!(line.contains("d3e0_d3ff_reads=0 d3e0_d3ff_writes=0"));
        assert!(line.contains("top=d300_r:none,d300_w:$4567:1,d3e0_r:none,d3e0_w:none"));
        assert!(line.contains("status=blocked_by_d300_d3df_access"));
    }

    #[test]
    fn cc_folded_s_dependency_classifies_known_ranges() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.cc_subpal_range = Some((0x2000, 0x200F));
        bus.cc_attr_write_range = Some((0x3000, 0x301F));

        bus.ram[0x0B09] = 0x18;
        bus.watch_pc = 0x2004;
        let _ = bus.read(0xCC00);
        bus.watch_pc = 0x3008;
        let _ = bus.read(0xCC01);
        bus.write(0xCC01, 0x01);

        bus.ram[0x0B09] = 0x00;
        bus.watch_pc = 0x4000;
        let _ = bus.read(0xEC02);
        bus.write(0xEC02, 0x02);
        bus.finish_ram_migration_frame();

        let line = format_cc_folded_s_dependency(&bus);
        assert!(line.contains("subpal_reads_on=1 subpal_reads_off=0"));
        assert!(line.contains("attr_compare_reads_on=1 attr_compare_reads_off=0"));
        assert!(line.contains("attr_writes_on=1 attr_writes_off=0"));
        assert!(line.contains("other_reads_on=0 other_reads_off=1"));
        assert!(line.contains("other_writes_on=0 other_writes_off=1"));
        assert!(line.contains("max_frame_reads=3 max_frame_writes=2"));
        assert!(line.contains("read_top=$2004:1|$3008:1|$4000:1"));
        assert!(line.contains("write_top=$3008:1|$4000:1"));
        assert!(line.contains("cc_reclaim=blocked_by_other_accesses"));
    }

    #[test]
    fn cc_folded_s_dependency_reports_true_consumer_blocker() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.cc_subpal_range = Some((0x2000, 0x200F));
        bus.watch_pc = 0x2004;
        let _ = bus.read(0xCC00);

        let line = format_cc_folded_s_dependency(&bus);
        assert!(line.contains("subpal_reads_off=1"));
        assert!(line.contains("cc_reclaim=blocked_by_true_consumers"));
    }

    #[test]
    fn cc_folded_s_dependency_reports_candidate_for_maintenance_only() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.cc_attr_write_range = Some((0x3000, 0x301F));
        bus.watch_pc = 0x3008;
        let _ = bus.read(0xCC01);
        bus.write(0xCC01, 0x01);

        let line = format_cc_folded_s_dependency(&bus);
        assert!(line.contains("attr_compare_reads_off=1"));
        assert!(line.contains("attr_writes_off=1"));
        assert!(line.contains("cc_reclaim=candidate_after_replacement_source"));
    }

    #[test]
    fn cc_folded_s_dependency_classifies_init_clear_as_non_blocking() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.cc_init_clear_range = Some((0x5000, 0x500F));

        bus.watch_pc = 0x5004;
        bus.write(0xCC00, 0x00);

        let line = format_cc_folded_s_dependency(&bus);
        assert!(line.contains("init_clear_writes_off=1"));
        assert!(line.contains("other_writes_off=0"));
        assert!(line.contains("cc_reclaim=candidate_after_replacement_source"));
    }

    #[test]
    fn d3xx_dirty_bitmap_candidate_reports_clean_bitmap() {
        let bus = SmsBus::new(Vec::new(), 0xFF);

        assert_eq!(
            format_d3xx_dirty_bitmap_candidate(&bus),
            "d3xx_dirty_bitmap_candidate=layout=$D300-$D3EF bytes_required=240 bytes_available=256 spare=$D3F0-$D3FF vertical_bits=0 horizontal_bits=0 vertical_bytes=0 horizontal_bytes=0 max_frame_vertical_bits=0 max_frame_horizontal_bits=0 attr_dirty=not_represented raw_tile_shadow=fits:no status=fits_metadata_only caveat=trace_only_no_runtime_writes"
        );
    }

    #[test]
    fn d3xx_dirty_bitmap_candidate_tracks_unique_tile_dirty_bits() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x20;
        bus.ram[0x0B10] = 0x00;
        bus.record_trace_ppu_write_call(7, 0x12);
        bus.record_trace_ppu_write_call(7, 0x34);
        bus.finish_d3xx_tile_dirty_frame();

        let line = format_d3xx_dirty_bitmap_candidate(&bus);
        assert!(line.contains("vertical_bits=1 horizontal_bits=1"));
        assert!(line.contains("vertical_bytes=1 horizontal_bytes=1"));
        assert!(line.contains("max_frame_vertical_bits=1 max_frame_horizontal_bits=1"));
    }

    #[test]
    fn d3xx_dirty_bitmap_candidate_ignores_attr_writes() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call(7, 0xFF);
        bus.finish_d3xx_tile_dirty_frame();

        let line = format_d3xx_dirty_bitmap_candidate(&bus);
        assert!(line.contains("vertical_bits=0 horizontal_bits=0"));
        assert!(line.contains("attr_dirty=not_represented"));
    }

    #[test]
    fn d3xx_full_dirty_bitmap_candidate_reports_clean_layout() {
        let bus = SmsBus::new(Vec::new(), 0xFF);

        assert_eq!(
            format_d3xx_full_dirty_bitmap_candidate(&bus),
            "d3xx_full_dirty_bitmap_candidate=layout=tile:$D300-$D3EF,attr:$D3F0-$D3FF bytes_required=256 bytes_available=256 vertical_tile_bits=0 horizontal_tile_bits=0 vertical_attr_bits=0 horizontal_attr_bits=0 vertical_bytes=0 horizontal_bytes=0 max_frame_vertical_tile_bits=0 max_frame_horizontal_tile_bits=0 max_frame_vertical_attr_bits=0 max_frame_horizontal_attr_bits=0 raw_tile_shadow=fits:no status=fits_all_dirty_metadata caveat=trace_only_no_runtime_writes"
        );
    }

    #[test]
    fn d3xx_full_dirty_bitmap_candidate_tracks_attr_bits() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B0F] = 0x23;
        bus.ram[0x0B10] = 0xC0;
        bus.record_trace_ppu_write_call(7, 0xFF);
        bus.finish_d3xx_tile_dirty_frame();

        let line = format_d3xx_full_dirty_bitmap_candidate(&bus);
        assert!(line.contains("vertical_tile_bits=0 horizontal_tile_bits=0"));
        assert!(line.contains("vertical_attr_bits=1 horizontal_attr_bits=1"));
        assert!(line.contains("vertical_bytes=1 horizontal_bytes=1"));
        assert!(line.contains("max_frame_vertical_attr_bits=1 max_frame_horizontal_attr_bits=1"));
    }

    #[test]
    fn d3xx_dirty_runtime_cost_reports_empty_state() {
        let bus = SmsBus::new(Vec::new(), 0xFF);

        assert_eq!(
            format_d3xx_dirty_runtime_cost(&bus),
            "d3xx_dirty_runtime_cost=runtime_marking=blocked_until_raw_source tile_ops_on=0 tile_ops_off=0 attr_ops_on=0 attr_ops_off=0 max_frame_tile_ops=0 max_frame_attr_ops=0 max_frame_total_ops=0 max_burst_ops=0 first_burst_frame=0 first_burst_step=0 render_observed=none caveat=trace_only_no_runtime_writes"
        );
    }

    #[test]
    fn d3xx_dirty_runtime_cost_splits_render_state() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.ram[0x0B09] = 0x18;
        bus.record_nt_raw_write_stats(NtWriteKind::Tile, 7, 3);
        bus.finish_nt_raw_frame();

        bus.ram[0x0B09] = 0x00;
        bus.record_nt_raw_write_stats(NtWriteKind::Attr, 11, 4);
        bus.finish_nt_raw_frame();

        let line = format_d3xx_dirty_runtime_cost(&bus);
        assert!(line.contains("tile_ops_on=1 tile_ops_off=0"));
        assert!(line.contains("attr_ops_on=0 attr_ops_off=1"));
        assert!(line.contains("max_frame_tile_ops=1 max_frame_attr_ops=1 max_frame_total_ops=1"));
        assert!(line.contains("render_observed=mixed"));
    }

    #[test]
    fn raw_shadow_parity_reports_unavailable_trace_source() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.nt_trace_ciram_writes = 3;
        assert_eq!(
            format_nt_raw_shadow_parity(&bus),
            "nt_raw_shadow_parity=unavailable reason=runtime_raw_ciram_missing trace_writes=3"
        );
    }

    #[test]
    fn sega_mapper_slot2_sram_maps_two_banks() {
        let mut rom = vec![0xFF; BANK_SIZE * 4];
        rom[BANK_SIZE * 2] = 0x22;
        let mut bus = SmsBus::new(rom, 0xFF);
        assert_eq!(bus.read(0x8000), 0x22);

        bus.write(0xFFFC, 0x08);
        bus.write(0x8000, 0x34);
        assert_eq!(bus.read(0x8000), 0x34);

        bus.write(0xFFFC, 0x0C);
        assert_eq!(bus.read(0x8000), 0x00);
        bus.write(0x8000, 0x56);
        assert_eq!(bus.read(0x8000), 0x56);

        bus.write(0xFFFC, 0x08);
        assert_eq!(bus.read(0x8000), 0x34);
        bus.write(0xFFFC, 0x00);
        assert_eq!(bus.read(0x8000), 0x22);
    }

    #[test]
    fn raw_ciram_backend_reports_slot2_sram_activity() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.write(0xFFFC, 0x08);
        bus.write(0x8001, 0x77);
        let _ = bus.read(0x8001);

        assert_eq!(
            format_raw_ciram_backend(&bus),
            "raw_ciram_backend=sram_slot2 base=$8000 size=2048 mapper_ctrl=$08 reads=1 writes=1 ciram_nonzero=1 caveat=standard_sega_mapper_sram_scaffold"
        );
    }

    #[test]
    fn raw_ciram_storage_decision_reports_blocked_reclaim_requirement() {
        assert_eq!(
            format_raw_ciram_storage_decision(),
            "raw_ciram_storage=blocked reason=no_internal_ram_without_reclaim required_tile_bytes=1920 attr_bytes_existing=128 candidate=$CC00-$D3FF blocked_by=folded_s_reclaim_required stack_candidate=$DD80-$DFFD:no_go"
        );
    }

    #[test]
    fn z80_stack_low_water_tracks_minimum_sp() {
        let mut watermark = Z80StackWatermark::new(0xDFF0);
        watermark.observe(0xDFE0);
        watermark.observe(0xDFF8);
        watermark.observe(0xDFD0);
        assert_eq!(watermark.low_sp, 0xDFD0);
        assert_eq!(watermark.used(), 0x20);
        assert_eq!(
            format_z80_stack_low_water(watermark),
            "z80_stack_low_water=sp=$DFD0 used=32"
        );
    }

    #[test]
    fn folded_s_compact_diagnostic_compares_cc_shadow_to_bitpack() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);

        bus.ram[0x0C01] = 2; // cell 0 in active folded $CCxx state.
        bus.ram[0x1300] = 2; // cell 0 in compact $D300 bitpack.
        assert_eq!(
            format_nt_folded_s_compact_mismatches(&bus),
            "nt_folded_s_compact_mismatch=0 first=none"
        );

        bus.ram[0x0C03] = 3; // cell 1, but compact still has 0 for bits 2..3.
        assert_eq!(nt_folded_cc_s(&bus, 1), 3);
        assert_eq!(nt_folded_compact_s(&bus, 1), 0);
        assert_eq!(
            format_nt_folded_s_compact_mismatches(&bus),
            "nt_folded_s_compact_mismatch=1 first=cell=00,01 cc=3 compact=0"
        );
    }

    #[test]
    fn folded_s_compact_diagnostic_reports_retired_shadow() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.nt_folded_s_compact_available = false;

        assert_eq!(
            format_nt_folded_s_compact_mismatches(&bus),
            "nt_folded_s_compact_mismatch=unavailable reason=compact_shadow_retired"
        );
    }
}
