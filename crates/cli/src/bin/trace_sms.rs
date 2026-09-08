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
// Existing runtime allocation: chrmap.s BGV_REFCNT owns $DD80-$DE3F.
// Native pushes must stay above it; this is not newly allocated metadata.
const NATIVE_STACK_FLOOR: u16 = 0xDE40;

fn native_stack_guard_failed(enabled: bool, sp: u16) -> bool {
    enabled && sp < NATIVE_STACK_FLOOR
}

/// SMS_REAL_PACING=1: instruction-paced stress mode — 15K instructions
/// between IRQs, boolean pending semantics (missed INTs don't queue), and
/// $FF-initialized RAM. This is not a cycle-accurate video frame clock.
fn real_pacing() -> bool {
    std::env::var("SMS_REAL_PACING").is_ok()
}
fn irq_period() -> usize {
    if real_pacing() { 15_000 } else { IRQ_PERIOD }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FunctionalIrq {
    Frame,
    Line,
}

/// Explicit instruction-paced compatibility clock, NOT a physical beam model.
/// Step0 starts line224; each period has262 coarse lines. R10 supplies one
/// R10+1 active-line event per period, not the hardware's reload/underflow model.
#[derive(Clone, Debug)]
struct FunctionalVideo {
    period: usize,
    step: usize,
    epochs: usize,
    frame_pending: bool,
    line_pending: bool,
    line_at: Option<u128>,
}

impl FunctionalVideo {
    fn new(period: usize) -> Self {
        assert!(period >= 262);
        Self {
            period,
            step: 0,
            epochs: 0,
            frame_pending: false,
            line_pending: false,
            line_at: None,
        }
    }

    fn advance_to(&mut self, step: usize) {
        assert!(step >= self.step, "functional video clock moved backwards");
        let epoch = step / self.period;
        if epoch > self.epochs {
            self.frame_pending = true;
            self.epochs = epoch;
        }
        if let Some(at) = self.line_at
            && at <= step as u128
        {
            self.line_pending = true;
            self.line_at =
                Some(at + ((step as u128 - at) / self.period as u128 + 1) * self.period as u128);
        }
        self.step = step;
    }

    fn vcounter(&self) -> u8 {
        let phase = (self.step % self.period) as u128 * 262 / self.period as u128;
        let physical = (224 + phase) % 262;
        if physical <= 234 {
            physical as u8
        } else {
            (physical - 6) as u8
        }
    }

    fn write_r10(&mut self, value: u8) {
        let line = u16::from(value) + 1;
        if line >= 224 {
            self.line_at = None;
            return;
        } // Includes explicit FF park.
        let offset = (u128::from(line) + 38) * self.period as u128;
        let mut at = (self.step / self.period * self.period) as u128 + offset.div_ceil(262);
        if at <= self.step as u128 {
            at += self.period as u128;
        }
        self.line_at = Some(at);
        // Rewriting/parking does not acknowledge an already pending HINT.
    }

    fn irq(&self, regs: &[u8; 16]) -> Option<FunctionalIrq> {
        if self.frame_pending && regs[1] & 0x20 != 0 {
            Some(FunctionalIrq::Frame)
        } else if self.line_pending && regs[0] & 0x10 != 0 {
            Some(FunctionalIrq::Line)
        } else {
            None
        }
    }

    fn acknowledge(&mut self) -> u8 {
        let status = if self.frame_pending { 0x80 } else { 0 };
        self.frame_pending = false;
        self.line_pending = false;
        status
    }

    fn can_wake_halt(&self, cpu: &Cpu, regs: &[u8; 16], inject: bool) -> bool {
        inject
            && cpu.iff1
            && (regs[1] & 0x20 != 0 || (regs[0] & 0x10 != 0 && self.line_at.is_some()))
    }
}

fn parse_functional_video(value: Option<&str>) -> Result<(), String> {
    match value {
        Some("ntsc224") => Ok(()),
        Some(other) => Err(format!(
            "unsupported --functional-video mode: {other}; expected ntsc224"
        )),
        None => Err("--functional-video requires ntsc224".to_owned()),
    }
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
    functional_video: Option<FunctionalVideo>,
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
    io_entries: u64,
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
    /// SMS_LOG_PSG=<path>: stream every PSG (port $40-$7F) write to a file,
    /// one hex byte per line, for offline pitch-trajectory analysis (F.5).
    psg_log_out: Option<std::sync::Arc<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>>,
    /// VRAM 16 KiB and CRAM 32 B (for inspection if needed).
    vram: [u8; 0x4000],
    cram: [u8; 0x20],
    /// Last written VDP register values, for framebuffer interpretation.
    vdp_regs: [u8; 16],
    /// Approximate per-frame line-scroll split for checkpoint rendering:
    /// (screen line, top/pre reg8, top/pre reg9). After the injected line IRQ
    /// runs, the live VDP regs hold the bottom/post scroll values.
    render_scroll_split: Option<(usize, u8, u8)>,
    /// The split of the frame that just ended. Frame-IRQ injection moves the
    /// live split here before clearing it; checkpoint dumps land one loop
    /// iteration after injection, so without this latch they would render the
    /// whole frame with the playfield scroll (status bar visibly scrolled).
    render_scroll_split_latched: Option<(usize, u8, u8)>,
    /// True while the frame-IRQ period has elapsed but the IRQ has not been
    /// injected yet (level-held frame INT). Drives status-port bit 7.
    frame_int_pending: bool,
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
    watch_vram: Option<(usize, usize)>,
    watch_vram_logged: u32,
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
    bank_writes_total: u64,
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

#[derive(Clone, Copy, Debug)]
struct ReturnPtrWatchEvent {
    old_ptr: u16,
    new_ptr: u16,
    transition: ReturnPtrTransition,
    frame_base: Option<u16>,
    write: WatchWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReturnPtrTransition {
    Push4,
    Pop4,
    BridgePush,
    BridgePop,
    FullPop,
    D4xxAlarm,
    D3fcAlarm,
    UnalignedAlarm,
    OtherDeltaAlarm,
}

impl ReturnPtrTransition {
    const ALL: [Self; 9] = [
        Self::Push4,
        Self::Pop4,
        Self::BridgePush,
        Self::BridgePop,
        Self::FullPop,
        Self::D4xxAlarm,
        Self::D3fcAlarm,
        Self::UnalignedAlarm,
        Self::OtherDeltaAlarm,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Push4 => "push +4",
            Self::Pop4 => "pop -4",
            Self::BridgePush => "bridge push D3F8->D500",
            Self::BridgePop => "bridge pop D500->D3F8",
            Self::FullPop => "full pop D600->D5FC",
            Self::D4xxAlarm => "ALARM D4xx",
            Self::D3fcAlarm => "ALARM D3FC",
            Self::UnalignedAlarm => "ALARM unaligned",
            Self::OtherDeltaAlarm => "ALARM other delta",
        }
    }

    fn is_alarm(self) -> bool {
        matches!(
            self,
            Self::D4xxAlarm | Self::D3fcAlarm | Self::UnalignedAlarm | Self::OtherDeltaAlarm
        )
    }
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
    from_pc: u16,
    from_op: u8,
    bank1: u8,
    sp: u16,
    ret: u16,
    op: u8,
    a: u8,
    b: u8,
    c: u8,
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
    translated_return_ptr: u16,
    stack_6502: u8,
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
            io_entries: 0,
            vdp_status_reads: 0,
            vdp_status_override: None,
            functional_video: None,
            display_enabled_edge: false,
            psg_writes: 0,
            psg_log: Vec::new(),
            psg_log_out: std::env::var("SMS_LOG_PSG").ok().map(|path| {
                std::sync::Arc::new(std::sync::Mutex::new(std::io::BufWriter::new(
                    std::fs::File::create(&path)
                        .unwrap_or_else(|err| panic!("SMS_LOG_PSG create {path}: {err}")),
                )))
            }),
            vram: if real_pacing() {
                [0xFF; 0x4000]
            } else {
                [0; 0x4000]
            },
            cram: [0; 0x20],
            vdp_regs: [0; 16],
            render_scroll_split: None,
            render_scroll_split_latched: None,
            frame_int_pending: false,
            vdp_addr_high: 0,
            vdp_addr_low: 0,
            bank_writes: Vec::new(),
            bank_writes_total: 0,
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
            watch_vram: None,
            watch_vram_logged: 0,
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
        self.bank_writes_total += 1;
        match addr {
            0xFFFC => {
                self.mapper_control = value;
                self.io_entries += 1;
                if self.io_log.len() < 4096 {
                    self.io_log.push(format!("mapper ctrl=${value:02X}"));
                }
                if self.bank_writes.len() < 4096 {
                    self.bank_writes.push((0xFFFC, value));
                }
            }
            0xFFFD => {
                self.slot_bank[0] = value;
                if self.bank_writes.len() < 4096 {
                    self.bank_writes.push((0xFFFD, value));
                }
            }
            0xFFFE => {
                self.slot_bank[1] = value;
                if self.bank_writes.len() < 4096 {
                    self.bank_writes.push((0xFFFE, value));
                }
            }
            0xFFFF => {
                self.slot_bank[2] = value;
                if self.bank_writes.len() < 4096 {
                    self.bank_writes.push((0xFFFF, value));
                }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PcProfileCost {
    instructions: u64,
    approx_cycles: u64,
}

type PcProfile = HashMap<(Option<u8>, u16), PcProfileCost>;

fn pc_profile_address(bus: &SmsBus, pc: u16) -> (Option<u8>, u16) {
    let bank = match pc {
        0x0000..=0x03FF => Some(0),
        0x0400..=0x3FFF => Some(bus.slot_bank[0]),
        0x4000..=0x7FFF => Some(bus.slot_bank[1]),
        0x8000..=0xBFFF if bus.slot2_cart_ram_offset(pc).is_none() => Some(bus.slot_bank[2]),
        _ => None,
    };
    (bank, pc)
}

fn record_pc_profile_step(
    profile: &mut PcProfile,
    address: (Option<u8>, u16),
    cycles_before: u64,
    cycles_after: u64,
) {
    let cost = profile.entry(address).or_default();
    cost.instructions += 1;
    cost.approx_cycles += cycles_after.saturating_sub(cycles_before);
}

/// Attribute completed instructions to their nearest preceding symbol in the
/// same ROM bank. These are exclusive PC ranges, not inclusive call costs;
/// continuation labels can contain game logic as well as call machinery.
fn format_pc_profile(profile: &PcProfile, mut symbols: Vec<(u8, u16, String)>) -> String {
    use std::fmt::Write;

    symbols.sort();
    let total_cycles: u64 = profile.values().map(|cost| cost.approx_cycles).sum();
    let total_instructions: u64 = profile.values().map(|cost| cost.instructions).sum();
    let mut per_symbol: HashMap<String, PcProfileCost> = HashMap::new();
    for (&(bank, pc), cost) in profile {
        let name = match bank {
            Some(bank) => symbols[..symbols.partition_point(|(b, a, _)| (*b, *a) <= (bank, pc))]
                .last()
                .filter(|(b, _, _)| *b == bank)
                .map(|(b, a, name)| format!("{b:02X}:{a:04X} {name}"))
                .unwrap_or_else(|| format!("{bank:02X}:{pc:04X}?")),
            None => format!("non-ROM:{pc:04X}?"),
        };
        let row = per_symbol.entry(name).or_default();
        row.instructions += cost.instructions;
        row.approx_cycles += cost.approx_cycles;
    }
    let mut rows: Vec<_> = per_symbol.into_iter().collect();
    rows.sort_by(|(a_name, a), (b_name, b)| {
        b.approx_cycles
            .cmp(&a.approx_cycles)
            .then_with(|| a_name.cmp(b_name))
    });
    let mut report = format!(
        "pc_profile: {total_instructions} completed instructions; {total_cycles} approx_cycles; top PC ranges by approx_cycles:\n\
         pc_profile_scope: exclusive nearest-symbol attribution; not inclusive function/call overhead; CPU opcode costs are approximate; not scanline timing\n\
           cycle_share  approx_cycles  instructions  symbol\n"
    );
    for (name, cost) in rows.iter().take(40) {
        let share = if total_cycles == 0 {
            0.0
        } else {
            cost.approx_cycles as f64 * 100.0 / total_cycles as f64
        };
        writeln!(
            report,
            "  {:9.2}%  {:>13}  {:>12}  {}",
            share, cost.approx_cycles, cost.instructions, name
        )
        .unwrap();
    }
    report
}

fn format_irq_to_ei_cost(costs: &[u64]) -> Option<String> {
    if costs.is_empty() {
        return None;
    }
    // A full NTSC frame is only a reference, NOT the shorter VRAM upload
    // window. The IRQ-to-EI interval normally excludes translated game logic.
    const FRAME_BUDGET_CYCLES: u64 = 59_736;
    let mut sorted = costs.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let pct = |p: usize| sorted[(n - 1) * p / 100];
    let avg = sorted.iter().sum::<u64>() / n as u64;
    let over = sorted.iter().filter(|&&c| c > FRAME_BUDGET_CYCLES).count();
    Some(format!(
        "irq_to_ei_cost approx_cycles: intervals={n} min={} p50={} avg={avg} p90={} p99={} max={}\n\
         irq_to_ei_scope: injected frame IRQ to first interrupt-enabled instruction boundary; not total gameplay-frame cost; not a VBlank upload deadline\n\
         irq_to_ei_budget_comparison: full_ntsc_frame_cycles={FRAME_BUDGET_CYCLES} intervals_above_full_frame={over} ({:.1}%) worst_interval={:.2}x avg={:.2}x",
        sorted[0],
        pct(50),
        pct(90),
        pct(99),
        sorted[n - 1],
        over as f64 * 100.0 / n as f64,
        sorted[n - 1] as f64 / FRAME_BUDGET_CYCLES as f64,
        avg as f64 / FRAME_BUDGET_CYCLES as f64,
    ))
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
                let canonical_addr = addr - 0x2000;
                self.record_ram_migration_access(canonical_addr, RamMigrationAccessKind::Read);
                if self.watches_read(canonical_addr) {
                    self.watch_read_log
                        .push(self.watch_entry(canonical_addr, value));
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
                    if self.watches_write(addr) {
                        self.watch_log.push(self.watch_entry(addr, value));
                    }
                }
                // Other writes to ROM area are ignored (real SMS hardware).
            }
            0xC000..=0xDFFF => {
                self.ram[(addr - 0xC000) as usize] = value;
                self.record_ram_migration_access(addr, RamMigrationAccessKind::Write);
                if self.watches_write(addr) {
                    self.watch_log.push(self.watch_entry(addr, value));
                }
                // The Sega mapper decodes only the canonical $FFFC-$FFFF
                // addresses. $DFFC-$DFFF are ordinary work RAM, despite RAM
                // otherwise being mirrored through $FFFF.
            }
            0xE000..=0xFFFB => {
                self.ram[(addr - 0xE000) as usize] = value;
                let canonical_addr = addr - 0x2000;
                self.record_ram_migration_access(canonical_addr, RamMigrationAccessKind::Write);
                if self.watches_write(canonical_addr) {
                    self.watch_log.push(self.watch_entry(canonical_addr, value));
                }
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
                if let Some(clock) = &mut self.functional_video {
                    self.vdp_addr_latched = false;
                    return clock.acknowledge();
                }
                if let Some(status) = self.vdp_status_override.take() {
                    self.vdp_addr_latched = false;
                    return status;
                }
                // Bit 7 = frame interrupt pending (the period elapsed and the
                // IRQ has not been taken yet). The runtime's end-of-handler
                // pacing read uses this to detect real overruns; the previous
                // toggle-every-N-reads fake set the sticky overrun counter at
                // random and permanently suppressed the status-bar scroll
                // split inside the tracer.
                let vblank = if self.frame_int_pending { 0x80 } else { 0x00 };
                // Reading also resets the VDP address latch toggle.
                self.vdp_addr_latched = false;
                vblank
            }
            // Synthetic functional pacing has no beam model. Report the
            // START of 224-line VBlank, not FF (too late for bounded upload
            // admission). This permits both >=E0 and E0..EC polling loops;
            // it cannot establish a real VBlank deadline or video timing.
            // H-counter $7F remains the default FF response below.
            0x40 => self
                .functional_video
                .as_ref()
                .map_or(0xe0, FunctionalVideo::vcounter),
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
            if let Some(out) = &self.psg_log_out {
                use std::io::Write as _;
                if let Ok(mut out) = out.lock() {
                    let _ = writeln!(out, "{value:02X}");
                }
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
                        if let Some((wa, wl)) = self.watch_vram
                            && masked >= wa
                            && masked < wa + wl
                            && self.watch_vram_logged < 200
                        {
                            self.watch_vram_logged += 1;
                            eprintln!(
                                "VRAMW ${masked:04X} = {value:02X} pc=${:04X} bank1={}",
                                self.watch_pc, self.slot_bank[1]
                            );
                        }
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
                        if let Some(clock) = &mut self.functional_video {
                            if reg == 10 {
                                clock.write_r10(val);
                            }
                            if reg == 0 && val & 0x10 != 0 && self.vdp_regs[10] < 223 {
                                self.render_scroll_split = Some((
                                    usize::from(self.vdp_regs[10]) + 1,
                                    self.vdp_regs[8],
                                    self.vdp_regs[9],
                                ));
                            }
                        }
                        self.io_entries += 1;
                        if self.io_log.len() < 4096 {
                            self.io_log.push(format!("vdp r{reg} = ${val:02X}"));
                        }
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

/// `$E3` is recoverable telemetry from the translated-RTS emulated-stack
/// fallback. All other non-zero `$Ex` markers represent loud runtime traps.
fn is_hard_runtime_trap(marker: u8) -> bool {
    (marker & 0xF0) == 0xE0 && marker != 0xE3
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

        if state.step >= state.next_irq_at && state.cpu.iff1 && state.cpu.ei_pending == 0 {
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
            state.next_irq_at = if real_pacing() {
                state.step + irq_period()
            } else {
                state.next_irq_at.saturating_add(IRQ_PERIOD)
            };
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
    state.bus.io_entries = 0;
    state.bus.bank_writes.clear();
    state.bus.bank_writes_total = 0;
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
    state.bus.io_entries = 0;
    state.bus.bank_writes.clear();
    state.bus.bank_writes_total = 0;
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
            eprintln!("                     [--game-frames N]");
            eprintln!(
                "                     [--functional-video ntsc224] (instruction-paced, not beam timing)"
            );
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
    let mut target_game_frames: Option<usize> = None;
    let mut log_pcs = false;
    let mut inject_irq = true;
    let mut functional_video = false;
    let mut controller_port_dc = 0xFF;
    let mut delayed_controller_port_dc: Option<u8> = None;
    // SMS PAUSE button presses (Z80 NMI to $0066) at these script frames.
    let mut pause_at_frames: Vec<usize> = Vec::new();
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
            "--game-frames" => {
                i += 1;
                target_game_frames = Some(args[i].parse().expect("game frames int"));
            }
            "--log-pcs" => log_pcs = true,
            "--search-late-routes" => search_late_routes = true,
            "--search-end-routes" => search_end_routes = true,
            "--no-irq" => inject_irq = false,
            "--functional-video" => {
                i += 1;
                if let Err(error) = parse_functional_video(args.get(i).map(String::as_str)) {
                    eprintln!("{error}");
                    std::process::exit(2);
                }
                functional_video = true;
            }
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
            "--pause-at-frame" => {
                i += 1;
                let value = args.get(i).expect("--pause-at-frame frame number");
                pause_at_frames.push(value.parse::<usize>().expect("--pause-at-frame usize"));
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

    if functional_video && (search_late_routes || search_end_routes) {
        eprintln!("--functional-video is unsupported with route-search loops");
        std::process::exit(2);
    }

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
    let rt_ppu_write_cont_addr = symbol_defs.get("rt_ppu_write_cont").map(|(_, addr)| *addr);
    let log_ppu_values = std::env::var("SMS_LOG_PPU_VALUES")
        .ok()
        .map(|spec| {
            spec.split(',')
                .filter_map(|raw| {
                    u8::from_str_radix(
                        raw.trim().trim_start_matches("0x").trim_start_matches('$'),
                        16,
                    )
                    .ok()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let log_ppu_limit = std::env::var("SMS_LOG_PPU_LIMIT")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(200);
    let mut logged_ppu_writes = 0usize;
    let mut bus = SmsBus::new(rom, controller_port_dc);
    if functional_video {
        bus.functional_video = Some(FunctionalVideo::new(irq_period()));
        eprintln!(
            "FUNCTIONAL_VIDEO ntsc224: {} instruction/idle steps per synthetic epoch; not beam timing, gameplay ticks, or speed",
            irq_period()
        );
    }
    if let Ok(spec) = std::env::var("SMS_WATCH_VRAM")
        && let Some((a, l)) = spec.split_once(':')
        && let (Ok(a), Ok(l)) = (
            usize::from_str_radix(a.trim_start_matches("0x"), 16),
            usize::from_str_radix(l.trim_start_matches("0x"), 16),
        )
    {
        bus.watch_vram = Some((a, l));
    }
    bus.d300_compact_store_range = d300_compact_store_range(&symbol_defs);
    bus.nt_folded_s_compact_available = bus.d300_compact_store_range.is_some();
    bus.cc_subpal_range = cc_subpal_range(&symbol_defs);
    bus.cc_attr_write_range = cc_attr_write_range(&symbol_defs);
    bus.cc_init_clear_range = cc_init_clear_range(&symbol_defs);
    let mut cpu = Cpu::new();
    cpu.pc = 0x0000;
    cpu.sp = 0xDFF0;
    // SMS_LOAD_STATE=<mednafen .mcs>: transplant a real-emulator crash
    // state into z80_emu and run forward — divergence localizes CPU
    // emulation differences; stuck-state reproduces the real hang.
    if let Ok(path) = std::env::var("SMS_LOAD_STATE") {
        load_mednafen_state(&path, &mut cpu, &mut bus);
        eprintln!(
            "LOADED STATE: PC=${:04X} SP=${:04X} task=${:02X} slot=[{},{},{}] iff1={}",
            cpu.pc,
            cpu.sp,
            bus.ram[0x18],
            bus.slot_bank[0],
            bus.slot_bank[1],
            bus.slot_bank[2],
            cpu.iff1
        );
    }
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
    let watch_exec_after = std::env::var("SMS_WATCH_AFTER")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let watch_exec_limit = std::env::var("SMS_WATCH_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(100_000);
    let mut watch_exec_log: Vec<WatchExecHit> = Vec::new();
    // Ring of last 64 control transfers (CALL/RET/JP-indirect/conditional).
    // Each entry: (kind, from_pc, to_pc). Kind is "call", "ret", or "jp".
    let mut xfer_ring: Vec<(&'static str, u16, u16)> = Vec::with_capacity(256);

    // Synthetic instruction-count IRQ cadence, not a hardware video clock.
    let mut next_irq_at = irq_period();
    let mut line_irq_at: Option<usize> = None;
    let mut min_native_sp: u16 = 0xFFFF;
    // SMS_EXPECT_BGV_CONSISTENT=<max>: at every checkpoint, verify the folded
    // background bookkeeping against the live nametable and fail acceptance
    // if more than <max> cells disagree (stale variant-ring regression guard).
    let bgv_consistency_max: Option<usize> = std::env::var("SMS_EXPECT_BGV_CONSISTENT")
        .ok()
        .map(|v| v.trim().parse().unwrap_or(0));
    let mut bgv_worst: (usize, usize, String) = (0, 0, String::new());
    let mut irqs_fired = 0usize;
    let mut line_irqs_fired = 0usize;
    // SMS_PC_PROFILE=1: completed instructions and approximate CPU cycle
    // deltas per-(bank,pc), folded by the ROM's .sym symbols at exit.
    let mut pc_profile: Option<PcProfile> = std::env::var("SMS_PC_PROFILE")
        .is_ok()
        .then(Default::default);
    // Presentation/IRQ-to-EI interval only: boot.s normally enables IRQs
    // BEFORE calling translated_nmi. This does not measure game-frame cost.
    // Line IRQs do not start samples; IRQ scheduling remains instruction-based.
    let mut irq_to_ei_start: Option<u64> = None;
    let mut irq_to_ei_costs: Vec<u64> = Vec::new();
    let mut next_button_event = 0usize;
    let mut next_checkpoint = 0usize;
    // Frame at which SMB first enabled NMI; scripts count from here.
    let mut script_frame_base: Option<usize> = None;
    // Game frames actually delivered to the translated NMI. Injections that
    // land while a handler is still running (heavy frames overrun the 60K-step
    // injection period) are swallowed by the runtime's own gates — NES
    // edge-trigger semantics, the designed overrun pacing — so they must not
    // advance the script clock: routes are recorded on the NES frame timeline
    // (frame-diff drives one NMI per frame and never overruns).
    let mut game_frames: usize = 0;
    let mut stop_after_frame_handler = false;
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
    let abort_bad_sp = std::env::var("SMS_ABORT_BAD_SP")
        .ok()
        .is_some_and(|v| v != "0");
    let mut native_stack_aborted = false;
    let mut first_fall_snapshot: Option<FallSnapshot> = None;

    for step in 0..steps {
        if let Some(clock) = &mut bus.functional_video {
            clock.advance_to(step);
        }
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
            eprintln!(
                "  SP=${:04X} last_pc=${:04X} last_op=${:02X}",
                cpu.sp,
                last_pc.unwrap_or(0),
                last_op.unwrap_or(0)
            );
            eprintln!(
                "  stack: {}",
                (0..12)
                    .map(|i| format!(
                        "{:04X}",
                        bus.read(cpu.sp.wrapping_add(i * 2)) as u16
                            | (bus.read(cpu.sp.wrapping_add(i * 2 + 1)) as u16) << 8
                    ))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
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
        if first_runtime_trap_step.is_none() && is_hard_runtime_trap(bus.ram[0x0B1D]) {
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
            if std::env::var("SMS_TRAP_RING").is_ok() {
                eprintln!(
                    "  emulated S=${:02X} stack page $C1E0-$C1FF:",
                    bus.ram[0x0B02]
                );
                let hex: Vec<String> = (0x01E0..0x0200)
                    .map(|i| format!("{:02X}", bus.ram[i]))
                    .collect();
                eprintln!("    {}", hex.join(" "));
                eprintln!("  last 48 xfers:");
                for (k, f, t) in xfer_ring.iter().rev().take(48).rev() {
                    eprintln!("    {k} ${f:04X} -> ${t:04X}");
                }
            }
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
        if pc == rt_ppu_write_addr || Some(pc) == rt_ppu_write_cont_addr {
            let ppu_addr = ((bus.ram[0x0B0F] as u16) << 8) | bus.ram[0x0B10] as u16;
            if cpu.b == 7
                && (0x2000..0x3000).contains(&ppu_addr)
                && log_ppu_values.contains(&cpu.a)
                && logged_ppu_writes < log_ppu_limit
            {
                eprintln!(
                    "SMS_PPU_WRITE step={step} frame={irqs_fired} entry=${pc:04X} bank1={} from=${:04X} ret=${:04X} cont=${:04X} addr=${ppu_addr:04X} value=${:02X} ctrl=${:02X}",
                    bus.slot_bank[1],
                    last_pc.unwrap_or(pc),
                    bus.watch_ret,
                    (cpu.h as u16) << 8 | cpu.l as u16,
                    cpu.a,
                    bus.ram[0x0B08],
                );
                logged_ppu_writes += 1;
            }
            bus.record_trace_ppu_write_call_at(cpu.b, cpu.a, step, irqs_fired);
        }
        runtime_materializer_monitor.observe_pc(step, pc, cpu.sp, &bus);
        if log_pcs && step < 200 {
            eprintln!("step {step:6}  PC=${pc:04X} op=${op:02X}");
        }
        *pc_counts.entry(pc).or_insert(0) += 1;
        if Some(pc) == watch_exec_addr
            && watch_exec_bank1.is_none_or(|bank| bank == bus.slot_bank[1])
            && step >= watch_exec_after
            && watch_exec_log.len() < watch_exec_limit
        {
            watch_exec_log.push(WatchExecHit {
                step,
                pc,
                from_pc: last_pc.unwrap_or(pc),
                from_op: last_op.unwrap_or(op),
                bank1: bus.slot_bank[1],
                sp: cpu.sp,
                ret: bus.watch_ret,
                op,
                a: cpu.a,
                b: cpu.b,
                c: cpu.c,
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
                translated_return_ptr: u16::from_le_bytes([bus.ram[0x0B76], bus.ram[0x0B77]]),
                stack_6502: bus.ram[0x0B02],
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

        let functional_irq = bus
            .functional_video
            .as_ref()
            .and_then(|clock| clock.irq(&bus.vdp_regs));
        let line_due = if functional_video {
            functional_irq == Some(FunctionalIrq::Line)
        } else {
            step < next_irq_at && line_irq_at.is_some_and(|at| step >= at)
        };
        let frame_due = if functional_video {
            functional_irq == Some(FunctionalIrq::Frame)
        } else {
            step >= next_irq_at
        };
        if inject_irq && line_due && cpu.iff1 && cpu.ei_pending == 0 {
            // Simulate a VDP line interrupt. It shares the IM1 vector with the
            // frame interrupt, but the status byte has bit 7 clear, so the
            // runtime can distinguish it after reading $BF.
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            if !functional_video {
                bus.vdp_status_override = Some(0x00);
            }
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
        if inject_irq && frame_due && cpu.iff1 && cpu.ei_pending == 0 {
            // Script frames count from SMB's NMI enable ($CB08 bit 7) — the
            // same convention frame-diff uses — so one recorded script
            // drives both harnesses identically regardless of how many
            // boot-time IRQ frames precede translated init.
            // Mirror the runtime's translated-NMI gates (boot.s irq_handler):
            // bit7 of $CB08 (PPUCTRL NMI enable), $CB1A (NMI-started latch),
            // $CA11 (nesting depth, max 2). Only injections that will actually
            // run the translated NMI count as game frames.
            let nmi_enable = bus.ram[0x0B08] & 0x80 != 0;
            let nmi_started = bus.ram[0x0B1A] != 0;
            let nmi_depth = bus.ram[0x0A11];
            let runs_translated_nmi = if nmi_enable {
                nmi_depth < 2
            } else {
                nmi_started && nmi_depth == 0
            };
            if script_frame_base.is_none() && nmi_enable {
                script_frame_base = Some(game_frames);
            }
            let script_frame = script_frame_base.map(|base| game_frames - base);
            if runs_translated_nmi {
                game_frames += 1;
                stop_after_frame_handler = target_game_frames == Some(game_frames);
            }
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
            if let Some(f) = script_frame
                && pause_at_frames.contains(&f)
            {
                // SMS PAUSE = Z80 NMI: push PC, IFF1 -> IFF2, jump $0066.
                cpu.sp = cpu.sp.wrapping_sub(2);
                bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
                bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
                cpu.iff2 = cpu.iff1;
                cpu.iff1 = false;
                cpu.halted = false;
                cpu.pc = 0x0066;
                eprintln!("PAUSE NMI injected at script frame {f}");
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
                if bgv_consistency_max.is_some() {
                    let bad = bgv_inconsistent_cells(&bus);
                    println!(
                        "bgv consistency at checkpoint {} (frame {}): {} inconsistent cell(s)",
                        checkpoint.name, checkpoint.frame, bad
                    );
                    if bad > bgv_worst.0 {
                        bgv_worst = (bad, checkpoint.frame, checkpoint.name.clone());
                    }
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
        if inject_irq && frame_due && cpu.iff1 && cpu.ei_pending == 0 {
            // Simulate a maskable interrupt: push PC, jump to $0038 (IM1).
            if interrupt_at_step.is_none() {
                interrupt_at_step = Some(step);
            }
            line_irq_at = None;
            bus.render_scroll_split_latched = bus.render_scroll_split.take();
            cpu.sp = cpu.sp.wrapping_sub(2);
            bus.write(cpu.sp, (cpu.pc & 0xFF) as u8);
            bus.write(cpu.sp.wrapping_add(1), (cpu.pc >> 8) as u8);
            if !functional_video {
                bus.vdp_status_override = Some(0x80);
            }
            cpu.pc = 0x0038;
            if first_irq_handler_step.is_none() {
                first_irq_handler_step = Some(step);
            }
            cpu.iff1 = false;
            cpu.iff2 = false;
            cpu.halted = false;
            irqs_fired += 1;
            irq_to_ei_start = Some(cpu.cycles);
            if !functional_video {
                next_irq_at = if real_pacing() {
                    step + irq_period()
                } else {
                    next_irq_at.saturating_add(IRQ_PERIOD)
                };
            }
        }

        if cpu.halted {
            if bus
                .functional_video
                .as_ref()
                .is_some_and(|clock| clock.can_wake_halt(&cpu, &bus.vdp_regs, inject_irq))
            {
                continue; // One deterministic idle step; no guest instruction.
            }
            // halted but no IRQ pending — endless halt. Stop.
            break;
        }
        // Capture AFTER IRQ redirection, BEFORE the instruction can switch
        // banks. Do no address lookup when profiling is disabled.
        let profile_sample = pc_profile
            .as_ref()
            .map(|_| (pc_profile_address(&bus, cpu.pc), cpu.cycles));
        if !functional_video {
            bus.frame_int_pending = inject_irq && step >= next_irq_at;
        }
        let materializer_render_before = current_render_state(&bus);
        let materializer_vdp_writes_before = bus.vdp_data_writes;
        let mut result = cpu.step(&mut bus);
        if functional_video && matches!(result, Err(StepError::Halt)) && cpu.halted {
            // Cpu::step reports the executed HALT before its usual EI-delay
            // retirement. In this opt-in scheduler HALT is an instruction,
            // followed by idle clock steps until an eligible IRQ arrives.
            cpu.ei_pending = cpu.ei_pending.saturating_sub(1);
            result = Ok(());
        }
        match result {
            Ok(()) => {
                taken += 1;
                if let (Some(map), Some((address, before))) = (&mut pc_profile, profile_sample) {
                    record_pc_profile_step(map, address, before, cpu.cycles);
                }
                if let Some(start) = irq_to_ei_start
                    && cpu.iff1
                    && cpu.ei_pending == 0
                {
                    irq_to_ei_costs.push(cpu.cycles.saturating_sub(start));
                    irq_to_ei_start = None;
                    if stop_after_frame_handler {
                        break;
                    }
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
                if cpu.sp >= 0xC000 && cpu.sp < min_native_sp {
                    min_native_sp = cpu.sp;
                }
                if native_stack_guard_failed(abort_bad_sp, cpu.sp) {
                    native_stack_aborted = true;
                    eprintln!(
                        "SMS_ABORT_BAD_SP: step={step} pc=${pc:04X} op=${op:02X} sp_after=${:04X} floor=${NATIVE_STACK_FLOOR:04X} ret_after=${:04X} bank1=${:02X}",
                        cpu.sp,
                        bus.read(cpu.sp) as u16 | ((bus.read(cpu.sp.wrapping_add(1)) as u16) << 8),
                        bus.slot_bank[1]
                    );
                    break;
                }
                if !functional_video && bus.vdp_regs[0] & 0x10 == 0 {
                    line_irq_at = None;
                } else if !functional_video
                    && inject_irq
                    && cpu.iff1
                    && cpu.ei_pending == 0
                    && line_irq_at.is_none()
                {
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
    println!("min native SP observed: ${min_native_sp:04X}");
    println!("=== trace-sms summary ===");
    println!("ROM: {}", rom_path.display());
    println!("steps run: {taken}");
    println!("Z80 cycles: {}", cpu.cycles);
    println!("game frames delivered: {game_frames}");
    if let Some(base) = script_frame_base {
        println!(
            "script frames delivered: {}",
            game_frames.saturating_sub(base)
        );
    }
    if let Some(e) = &last_err {
        println!("stopped on error: {e:?}");
    }
    if let Some(at) = interrupt_at_step {
        println!("injected IRQ at step {at}");
    }
    if let Some(report) = format_irq_to_ei_cost(&irq_to_ei_costs) {
        println!("{report}");
    }
    if let Some(map) = pc_profile {
        let symbols = load_wla_symbol_defs(&rom_path.with_extension("sym"))
            .into_iter()
            .map(|(name, (bank, pc))| (bank, pc, name))
            .collect();
        eprint!("{}", format_pc_profile(&map, symbols));
    }
    println!("\nMilestones:");
    print_milestone("entered translated slot-1 code", first_translated_step);
    print_milestone("entered IRQ/NMI bridge at $0038", first_irq_handler_step);
    print_milestone(
        "hit hard runtime trap marker at $CB1D",
        first_runtime_trap_step,
    );
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
    println!("VDP control I/O entries: {}", bus.io_entries);
    println!("IRQs fired: {irqs_fired}");
    println!("Line IRQs fired: {line_irqs_fired}");
    if let Some(clock) = &bus.functional_video {
        println!(
            "Functional video: ntsc224 synthetic_epochs={} scheduler_steps={} period={} delivered_frame_irqs={irqs_fired} delivered_line_irqs={line_irqs_fired}; NOT physical video/game ticks/speed",
            clock.epochs, clock.step, clock.period
        );
    }
    println!(
        "Bank mapping: slot0={} slot1={} slot2={}",
        bus.slot_bank[0], bus.slot_bank[1], bus.slot_bank[2]
    );
    println!("Mapper writes total: {}", bus.bank_writes_total);
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
        let watch_tail = std::env::var("SMS_WATCH_TAIL")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(40);
        if bus.watch_log.len() > 40 && watch_tail > 0 {
            println!("  ... last {watch_tail} writes:");
            for write in bus
                .watch_log
                .iter()
                .rev()
                .take(watch_tail)
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
        if bus
            .watch_write_range
            .is_some_and(|(start, end)| start <= 0xCB76 && end >= 0xCB77)
        {
            print_translated_return_pointer_watch(&bus.watch_log);
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
                "  step {}: pc=${:04X} from=${:04X}/${:02X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} bc=${:02X}{:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X} trptr=${:04X} s6502=${:02X}",
                hit.step,
                hit.pc,
                hit.from_pc,
                hit.from_op,
                hit.bank1,
                hit.sp,
                hit.ret,
                hit.op,
                hit.a,
                hit.b,
                hit.c,
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
                hit.area_obj_dispatch,
                hit.translated_return_ptr,
                hit.stack_6502
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
                    "  step {}: pc=${:04X} from=${:04X}/${:02X} bank1=${:02X} sp=${:04X} ret=${:04X} op=${:02X} a=${:02X} bc=${:02X}{:02X} f=${:02X} p=${:02X} xsh=${:02X} ysh=${:02X} ppos={:02X}:{:02X} y={:02X}:{:02X} yspd=${:02X} eb=${:02X} vf=${:02X} zp00=${:02X} zp02=${:02X} zp03=${:02X} zp04=${:02X} zp05=${:02X} zp06=${:02X} zp07=${:02X} zp08=${:02X} disp=${:02X} trptr=${:04X} s6502=${:02X}",
                    hit.step,
                    hit.pc,
                    hit.from_pc,
                    hit.from_op,
                    hit.bank1,
                    hit.sp,
                    hit.ret,
                    hit.op,
                    hit.a,
                    hit.b,
                    hit.c,
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
                    hit.area_obj_dispatch,
                    hit.translated_return_ptr,
                    hit.stack_6502
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
        "Runtime diagnostics: unresolved_id=${unresolved_id:04X} trap_marker=${:02X} guard_depth=${:02X} vbuf_used=${:02X} ppu_addr=${:02X}{:02X} ppu_mask=${:02X} split_flags=${:02X} split_pre=${:02X}:${:02X} split_post=${:02X}:${:02X}",
        bus.read(0xCB1D),
        bus.read(0xD47F),
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
    if let Ok(spec) = std::env::var("SMS_DUMP_SRAM") {
        // Dump cart SRAM (offset:len hex) — e.g. the CHR-RAM mirror at $0800+.
        if let Some((a, l)) = spec.split_once(':') {
            if let (Ok(a), Ok(l)) = (
                usize::from_str_radix(a.trim_start_matches("0x"), 16),
                usize::from_str_radix(l.trim_start_matches("0x"), 16),
            ) {
                let hex: Vec<String> = bus.cart_ram[a..(a + l).min(bus.cart_ram.len())]
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                println!("SRAM ${a:04X}: {}", hex.join(" "));
            }
        }
    }
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

    let mut acceptance_failed = native_stack_aborted;
    if native_stack_aborted {
        eprintln!("EXPECT FAIL: native stack crossed runtime metadata floor");
    }
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
    if let Some(max_bad) = bgv_consistency_max {
        if bgv_worst.0 > max_bad {
            eprintln!(
                "EXPECT FAIL: bgv consistency worst {} cell(s) at frame {} ({}) exceeds max {}",
                bgv_worst.0, bgv_worst.1, bgv_worst.2, max_bad
            );
            acceptance_failed = true;
        } else {
            println!(
                "EXPECT ok: bgv consistency (worst {} cell(s), max {})",
                bgv_worst.0, max_bad
            );
        }
    }
    if expect_no_trap {
        if let Some(step) = first_runtime_trap_step {
            let id = (bus.ram[0x0B1C] as u16) << 8 | bus.ram[0x0B1B] as u16;
            eprintln!(
                "EXPECT FAIL: runtime trap marker hit at step {step}, unresolved_id=${id:04X}"
            );
            acceptance_failed = true;
        } else if is_hard_runtime_trap(bus.ram[0x0B1D]) {
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

fn reconstruct_return_ptr_watch_events(writes: &[WatchWrite]) -> Vec<ReturnPtrWatchEvent> {
    let writes = writes
        .iter()
        .copied()
        .filter(|write| matches!(write.addr, 0xCB76 | 0xCB77))
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut low = 0x00u8;
    let mut high = 0xD3u8;
    let mut ptr = u16::from_le_bytes([low, high]);
    let mut i = 0usize;
    while i < writes.len() {
        let write = writes[i];
        match write.addr {
            0xCB76 => {
                let old_ptr = ptr;
                low = write.value;
                if let Some(next) = writes.get(i + 1).copied() {
                    if next.step == write.step && next.addr == 0xCB77 {
                        high = next.value;
                        ptr = u16::from_le_bytes([low, high]);
                        events.push(ReturnPtrWatchEvent {
                            old_ptr,
                            new_ptr: ptr,
                            transition: classify_return_ptr_transition(old_ptr, ptr),
                            frame_base: return_ptr_frame_base(old_ptr, ptr),
                            write: next,
                        });
                        i += 2;
                        continue;
                    }
                }
                ptr = u16::from_le_bytes([low, high]);
                events.push(ReturnPtrWatchEvent {
                    old_ptr,
                    new_ptr: ptr,
                    transition: classify_return_ptr_transition(old_ptr, ptr),
                    frame_base: return_ptr_frame_base(old_ptr, ptr),
                    write,
                });
            }
            0xCB77 => {
                let old_ptr = ptr;
                high = write.value;
                ptr = u16::from_le_bytes([low, high]);
                events.push(ReturnPtrWatchEvent {
                    old_ptr,
                    new_ptr: ptr,
                    transition: classify_return_ptr_transition(old_ptr, ptr),
                    frame_base: return_ptr_frame_base(old_ptr, ptr),
                    write,
                });
            }
            _ => {}
        }
        i += 1;
    }
    events
}

fn classify_return_ptr_transition(old_ptr: u16, new_ptr: u16) -> ReturnPtrTransition {
    if (old_ptr & 0xFF00) == 0xD400 || (new_ptr & 0xFF00) == 0xD400 {
        return ReturnPtrTransition::D4xxAlarm;
    }
    if old_ptr == 0xD3FC || new_ptr == 0xD3FC {
        return ReturnPtrTransition::D3fcAlarm;
    }
    if old_ptr & 0x0003 != 0 || new_ptr & 0x0003 != 0 {
        return ReturnPtrTransition::UnalignedAlarm;
    }
    match (old_ptr, new_ptr) {
        (0xD3F8, 0xD500) => ReturnPtrTransition::BridgePush,
        (0xD500, 0xD3F8) => ReturnPtrTransition::BridgePop,
        (0xD600, 0xD5FC) => ReturnPtrTransition::FullPop,
        _ if new_ptr == old_ptr.wrapping_add(4) => ReturnPtrTransition::Push4,
        _ if new_ptr.wrapping_add(4) == old_ptr => ReturnPtrTransition::Pop4,
        _ => ReturnPtrTransition::OtherDeltaAlarm,
    }
}

fn return_ptr_frame_base(old_ptr: u16, new_ptr: u16) -> Option<u16> {
    match classify_return_ptr_transition(old_ptr, new_ptr) {
        ReturnPtrTransition::Push4 | ReturnPtrTransition::BridgePush => Some(old_ptr),
        ReturnPtrTransition::Pop4
        | ReturnPtrTransition::BridgePop
        | ReturnPtrTransition::FullPop => Some(new_ptr),
        _ => None,
    }
}

fn print_translated_return_pointer_watch(writes: &[WatchWrite]) {
    let events = reconstruct_return_ptr_watch_events(writes);
    let final_ptr = events.last().map(|event| event.new_ptr).unwrap_or(0xD300);
    let high_water_ptr = events.iter().map(|event| event.new_ptr).max();
    let high_water =
        high_water_ptr.and_then(|ptr| events.iter().find(|event| event.new_ptr == ptr));
    let first_invalid = events.iter().find(|event| event.transition.is_alarm());

    println!("Translated return pointer watch:");
    println!("  event count: {}", events.len());
    println!("  final pointer: ${final_ptr:04X}");
    if let Some(event) = high_water {
        println!(
            "  high-water pointer: ${:04X} first reached at step {} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X}",
            event.new_ptr,
            event.write.step,
            event.write.pc,
            event.write.bank1,
            event.write.sp,
            event.write.ret
        );
    } else {
        println!("  high-water pointer: none");
    }
    if let Some(event) = first_invalid {
        println!(
            "  first invalid transition: ${:04X}->${:04X} {} at step {} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X}",
            event.old_ptr,
            event.new_ptr,
            event.transition.label(),
            event.write.step,
            event.write.pc,
            event.write.bank1,
            event.write.sp,
            event.write.ret
        );
    } else {
        println!("  first invalid transition: none");
    }

    println!("  transition counts:");
    for transition in ReturnPtrTransition::ALL {
        let count = events
            .iter()
            .filter(|event| event.transition == transition)
            .count();
        println!("    {:<24} {count}", transition.label());
    }

    for threshold in [0xD3F8u16, 0xD500, 0xD580, 0xD5C0, 0xD5F0, 0xD600] {
        if let Some(event) = events.iter().find(|event| event.new_ptr >= threshold) {
            println!(
                "  first >= ${threshold:04X}: step {} ${:04X}->${:04X} {} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
                event.write.step,
                event.old_ptr,
                event.new_ptr,
                event.transition.label(),
                event.write.pc,
                event.write.bank1,
                event.write.sp,
                event.write.ret,
                event.write.ppage,
                event.write.px,
                event.write.ypage,
                event.write.py,
                event.write.player_state,
            );
        } else {
            println!("  first >= ${threshold:04X}: none");
        }
    }

    println!("  first 20 pointer events:");
    for event in events.iter().take(20) {
        print_return_ptr_watch_event(event);
    }
    if events.len() > 20 {
        println!("  last 40 pointer events:");
        for event in events
            .iter()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            print_return_ptr_watch_event(event);
        }
    }
}

fn print_return_ptr_watch_event(event: &ReturnPtrWatchEvent) {
    let frame_base = event
        .frame_base
        .map(|addr| format!(" frame_base=${addr:04X}"))
        .unwrap_or_default();
    println!(
        "    step {}: ${:04X}->${:04X} {}{} pc=${:04X} bank1=${:02X} sp=${:04X} ret=${:04X} ppos={:02X}:{:02X} y={:02X}:{:02X} st={:02X}",
        event.write.step,
        event.old_ptr,
        event.new_ptr,
        event.transition.label(),
        frame_base,
        event.write.pc,
        event.write.bank1,
        event.write.sp,
        event.write.ret,
        event.write.ppage,
        event.write.px,
        event.write.ypage,
        event.write.py,
        event.write.player_state,
    );
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
            .or(bus.render_scroll_split_latched)
            .map(|(line, top_x, top_y)| format!(
                "line={line} top_reg8=${top_x:02X} top_reg9=${top_y:02X}"
            ))
            .unwrap_or_else(|| "none".to_string())
    )?;
    writeln!(
        f,
        "vdp: r0=${:02X} r1=${:02X} r10=${:02X} vram_writes={} cram_writes={} data_writes={} control_writes={} status_reads={} controller_reads={}",
        bus.vdp_regs[0],
        bus.vdp_regs[1],
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
    "raw_ciram_storage=blocked reason=no_internal_ram_without_reclaim required_tile_bytes=1920 attr_bytes_existing=128 candidate=$CC00-$D3FF blocked_by=folded_s_reclaim_required stack_candidate=$DE40-$DFFB:no_go"
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
fn load_mednafen_state(path: &str, cpu: &mut Cpu, bus: &mut SmsBus) {
    // Expects an ALREADY-DECOMPRESSED Mednafen state (gunzip the .mcs
    // first: `gzip -dc state.mcs > state.raw`).
    let d = std::fs::read(path).expect("state file");
    // Top-level chunks: 32-byte name + u32 LE size.
    let mut chunks: std::collections::HashMap<String, (usize, usize)> = Default::default();
    let mut j = 8;
    while j + 36 < d.len() {
        let name = &d[j..j + 32];
        if name[0] != 0 && name.iter().all(|&b| b == 0 || (32..127).contains(&b)) {
            let size = u32::from_le_bytes(d[j + 32..j + 36].try_into().unwrap()) as usize;
            let nm: String = name
                .iter()
                .take_while(|&&b| b != 0)
                .map(|&b| b as char)
                .collect();
            if size > 0 && j + 36 + size <= d.len() && nm.len() >= 3 {
                chunks.insert(nm, (j + 36, size));
                j += 36 + size;
                continue;
            }
        }
        j += 1;
    }
    // Sub-chunks: 1-byte name-len + name + u32 LE size + data.
    let sub = |off: usize, size: usize| -> std::collections::HashMap<String, Vec<u8>> {
        let mut out = Default::default();
        let mut m: std::collections::HashMap<String, Vec<u8>> = out;
        let end = off + size;
        let mut j = off;
        while j + 5 < end {
            let nl = d[j] as usize;
            if nl == 0 || nl > 24 {
                break;
            }
            let name: String = d[j + 1..j + 1 + nl].iter().map(|&b| b as char).collect();
            let sz = u32::from_le_bytes(d[j + 1 + nl..j + 5 + nl].try_into().unwrap()) as usize;
            m.insert(name, d[j + 5 + nl..(j + 5 + nl + sz).min(d.len())].to_vec());
            j += 5 + nl + sz;
        }
        out = m;
        out
    };
    let w16 = |v: &[u8]| u16::from_le_bytes([v[0], v[1]]);
    if let Some(&(o, sz)) = chunks.get("Z80") {
        let z = sub(o, sz);
        let af = w16(&z["AF"]);
        cpu.a = (af >> 8) as u8;
        cpu.f = af as u8;
        let bc = w16(&z["BC"]);
        cpu.b = (bc >> 8) as u8;
        cpu.c = bc as u8;
        let de = w16(&z["DE"]);
        cpu.d = (de >> 8) as u8;
        cpu.e = de as u8;
        let hl = w16(&z["HL"]);
        cpu.h = (hl >> 8) as u8;
        cpu.l = hl as u8;
        cpu.af_shadow = w16(&z["AF_"]);
        cpu.bc_shadow = w16(&z["BC_"]);
        cpu.de_shadow = w16(&z["DE_"]);
        cpu.hl_shadow = w16(&z["HL_"]);
        cpu.sp = w16(&z["SP"]);
        cpu.pc = w16(&z["PC"]);
        cpu.iff1 = z["IFF1"][0] != 0;
        cpu.iff2 = z["IFF2"][0] != 0;
    }
    if let Some(&(o, sz)) = chunks.get("MAIN") {
        let m = sub(o, sz);
        if let Some(ram) = m.get("RAM") {
            for (i, &b) in ram.iter().take(0x2000).enumerate() {
                bus.ram[i] = b; // $C000-$DFFF
            }
        }
    }
    if let Some(&(o, sz)) = chunks.get("VDP") {
        let v = sub(o, sz);
        if let Some(vram) = v.get("vram") {
            for (i, &b) in vram.iter().take(0x4000).enumerate() {
                bus.vram[i] = b;
            }
        }
        if let Some(cram) = v.get("cram") {
            for (i, &b) in cram.iter().take(0x20).enumerate() {
                bus.cram[i] = b;
            }
        }
        if let Some(reg) = v.get("reg") {
            for (i, &b) in reg.iter().take(16).enumerate() {
                bus.vdp_regs[i] = b;
            }
        }
    }
    if let Some(&(o, sz)) = chunks.get("CART") {
        let c = sub(o, sz);
        if let Some(sram) = c.get("sram") {
            for (i, &b) in sram.iter().take(CART_RAM_SIZE).enumerate() {
                bus.cart_ram[i] = b;
            }
        }
        if let Some(fcr) = c.get("fcr") {
            // fcr[0]=$FFFC control, [1]=$FFFD slot0, [2]=$FFFE slot1, [3]=$FFFF slot2.
            bus.mapper_control = fcr[0];
            bus.slot_bank[0] = fcr[1];
            bus.slot_bank[1] = fcr[2];
            bus.slot_bank[2] = fcr[3];
        }
    }
}

/// Count visible NT cells whose written slot disagrees with the folded-BG
/// bookkeeping (base-shadow $DA00 + folded S $CC00 -> FC cache $D600). A
/// painted cell must reference FC[base*4+S]; a disagreement means a variant
/// ring slot was recycled under a live cell — the stale-tile class fixed by
/// the BGV_REFCNT allocator (runtime/chrmap.s). Only meaningful for the
/// folded-BG model (CHR-ROM games); identity-mode profiles should not enable
/// the SMS_EXPECT_BGV_CONSISTENT check.
fn bgv_inconsistent_cells(bus: &SmsBus) -> usize {
    let mut bad = 0;
    for cell in 0..896usize {
        let base = bus.ram[0x1A00 + cell] as usize;
        if base == 0 {
            continue;
        }
        let s = (bus.ram[0x0C00 + cell * 2 + 1] & 3) as usize;
        let fc = bus.ram[0x1600 + base * 4 + s];
        let nt = bus.vram[0x3700 + cell * 2];
        if fc == 0xFF || fc != nt {
            bad += 1;
        }
    }
    bad
}

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

    // VDP register 1 bit 6 = display enable. The runtime mirrors NES PPUMASK
    // blanking here (ppu.s _ppu_sync_vdp_reg1); a blanked frame shows the
    // backdrop color (register 7, sprite-palette index), like real hardware.
    if bus.vdp_regs[1] & 0x40 == 0 {
        let (r, g, b) = cram_to_rgb(bus.cram[16 + (bus.vdp_regs[7] & 0x0F) as usize]);
        for px in pixels.chunks_exact_mut(3) {
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
        let mut f = std::fs::File::create(path)?;
        write!(f, "P6\n{W} {H}\n255\n")?;
        f.write_all(&pixels)?;
        return Ok(());
    }

    for screen_y in 0..H {
        // Split timing and R0 top-row horizontal lock are output-scanline
        // decisions. Pick the scroll registers for this displayed line first,
        // then sample the nametable through the inverse scroll transform.
        let (base_reg8, reg9) = if let Some((split_line, top_reg8, top_reg9)) =
            bus.render_scroll_split.or(bus.render_scroll_split_latched)
        {
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
        let sprite_height = if bus.vdp_regs[1] & 0x02 != 0 { 16 } else { 8 };
        let tile = if sprite_height == 16 { tile & !1 } else { tile };
        let tile_addr = sprite_base + tile * 32;
        if tile_addr + sprite_height * 4 > 0x4000 {
            continue;
        }
        active_sprites += 1;
        for py in 0..sprite_height {
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
    fn functional_video_phase_is_stable_and_wraps_on_both_instruction_periods() {
        for period in [15_000, 60_000] {
            let mut clock = FunctionalVideo::new(period);
            for (offset, want) in [
                (0, 0xe0),
                (1, 0xe1),
                (10, 0xea),
                (11, 0xe5),
                (37, 0xff),
                (38, 0),
                (76, 38),
                (77, 39),
                (261, 223),
            ] {
                let step = (offset * period).div_ceil(262);
                clock.advance_to(step);
                assert_eq!(clock.vcounter(), want);
                assert_eq!(clock.vcounter(), want, "a read must not advance time");
            }
            clock.advance_to(period);
            assert_eq!(clock.vcounter(), 0xe0);
            assert_eq!(clock.epochs, 1);
            clock.advance_to(usize::MAX);
            assert_eq!(clock.epochs, usize::MAX / period);
        }
    }

    #[test]
    fn functional_events_coalesce_until_status_ack_even_while_delivery_is_disabled() {
        let mut clock = FunctionalVideo::new(60_000);
        let mut regs = [0; 16];
        clock.write_r10(38);
        let first = clock.line_at.unwrap() as usize;
        clock.advance_to(first - 1);
        assert!(!clock.line_pending);
        clock.advance_to(first);
        assert_eq!(clock.vcounter(), 39);
        assert!(clock.line_pending);
        assert_eq!(clock.irq(&regs), None);
        regs[0] = 0x10;
        assert_eq!(clock.irq(&regs), Some(FunctionalIrq::Line));
        assert_eq!(clock.acknowledge(), 0);
        assert_eq!(clock.irq(&regs), None);
        clock.advance_to(first);
        assert!(!clock.line_pending, "no phantom repeat after BF");
        clock.advance_to(3 * 60_000 + first);
        regs[1] = 0x20;
        assert_eq!(clock.epochs, 3);
        assert!(clock.frame_pending && clock.line_pending);
        assert_eq!(clock.irq(&regs), Some(FunctionalIrq::Frame));
        assert_eq!(clock.acknowledge(), 0x80);
        assert!(!clock.frame_pending && !clock.line_pending);
        clock.advance_to(3 * 60_000 + first + 1);
        assert_eq!(
            clock.irq(&regs),
            None,
            "unserviced epochs must not queue IRQs"
        );
    }

    #[test]
    fn functional_line_rearm_and_park_do_not_ack_pending_edges() {
        let mut clock = FunctionalVideo::new(15_000);
        clock.write_r10(38);
        let first = clock.line_at.unwrap() as usize;
        clock.advance_to(first);
        clock.write_r10(0xff);
        assert!(clock.line_pending);
        assert_eq!(clock.line_at, None);
        clock.acknowledge();
        clock.advance_to(30_000);
        assert!(!clock.line_pending);
        clock.write_r10(38);
        let next = clock.line_at.unwrap() as usize;
        assert!(next > clock.step);
        clock.advance_to(next);
        assert!(clock.line_pending);
        clock.write_r10(38);
        assert_eq!(clock.line_at, Some((next + 15_000) as u128));
    }

    #[test]
    fn functional_bus_status_ack_closes_latch_without_legacy_overrides() {
        let mut bus = SmsBus::new(vec![0; BANK_SIZE], 0xff);
        let mut clock = FunctionalVideo::new(60_000);
        clock.advance_to(60_000);
        bus.functional_video = Some(clock);
        bus.out_port(0xbf, 0x55);
        assert!(bus.vdp_addr_latched);
        assert_eq!(bus.in_port(0xbf), 0x80);
        assert!(!bus.vdp_addr_latched);
        bus.vdp_status_override = Some(0x80);
        assert_eq!(
            bus.in_port(0xbf),
            0,
            "new mode has no injection-time fake status"
        );
        assert_eq!(bus.in_port(0x7e), 0xe0);
        assert_eq!(bus.in_port(0x7f), 0xff, "H-counter remains unsupported");
        bus.out_port(0xbf, 38);
        bus.out_port(0xbf, 0x8a);
        assert!(bus.functional_video.as_ref().unwrap().line_at.is_some());
    }

    #[test]
    fn functional_halt_waits_only_for_an_enabled_future_source() {
        let mut clock = FunctionalVideo::new(60_000);
        let mut cpu = Cpu::new();
        cpu.halted = true;
        let mut regs = [0; 16];
        regs[1] = 0x20;
        assert!(
            !clock.can_wake_halt(&cpu, &regs, true),
            "DI HALT is a hard stop"
        );
        cpu.iff1 = true;
        assert!(clock.can_wake_halt(&cpu, &regs, true));
        assert!(!clock.can_wake_halt(&cpu, &regs, false));
        regs[1] = 0;
        regs[0] = 0x10;
        assert!(!clock.can_wake_halt(&cpu, &regs, true));
        clock.write_r10(38);
        assert!(clock.can_wake_halt(&cpu, &regs, true));
        clock.write_r10(0xff);
        assert!(!clock.can_wake_halt(&cpu, &regs, true));
    }

    #[test]
    fn functional_mode_parser_rejects_missing_or_unsupported_modes() {
        assert!(parse_functional_video(Some("ntsc224")).is_ok());
        for value in [None, Some(""), Some("pal"), Some("random"), Some("ntsc192")] {
            assert!(parse_functional_video(value).is_err());
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT Docker-assembled coherent project"]
    fn functional_clock_unblocks_actual_coherent_admission_without_changing_runtime() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let &(bank, target) = defs.get("rt_cv1_hud_try_chunk").unwrap();
        assert_eq!(bank, 0);
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        for progressing in [false, true] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            // Runtime presentation ABI only; no game's RAM or game logic.
            bus.ram[0x0813] = 11; // valid, split, served
            bus.ram[0x0802] = 0; // displayed, no explicit blank fence
            if progressing {
                bus.functional_video = Some(FunctionalVideo::new(15_000));
            }
            let mut admitted = false;
            let mut cpu = Cpu::new();
            for step in 0..15_000 {
                if let Some(clock) = &mut bus.functional_video {
                    clock.advance_to(step);
                }
                if cpu.pc == 0 || cpu.pc == 7 {
                    cpu.pc = target;
                    cpu.sp = 0xdff0;
                    cpu.b = 27;
                    bus.write(0xdff0, 7);
                    bus.write(0xdff1, 0);
                }
                cpu.step(&mut bus).unwrap();
                assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                assert_eq!(bus.ram[0x0b1d], 0);
                if cpu.pc == 7 && cpu.f & 1 == 0 {
                    admitted = true;
                    break;
                }
            }
            assert_eq!(admitted, progressing);
        }
    }

    #[test]
    fn synthetic_vcounter_admits_bounded_vblank_polling_without_beam_claim() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        for _ in 0..4 {
            let v = bus.in_port(0x7e);
            assert_eq!(v, 0xe0); // fixed synthetic start-of-blank, never a clock
            assert!(v >= 0xe0); // legacy presentation/split guard
            assert!(v.wrapping_sub(0xe0) < 13); // bounded SAT admission
        }
        assert_eq!(bus.in_port(0x7f), 0xff);
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT Docker-assembled MMC3 fixture"]
    fn mmc3_assembled_helpers_preserve_live_state_and_mapping() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        assert_eq!(rom.len(), 0x200000);
        for label in ["rt_mmc3_write", "rt_mmc3_read_prg"] {
            assert_eq!(defs[label].0, 0, "helper must remain in fixed slot 0");
        }
        // Seed only the runtime ABI, not game state. The separate full-pipeline
        // fixture starts at reset and compares against the original 6502 CPU.
        for iff in [false, true] {
            for sram_control in [0, 8, 12] {
                for address in [0x8010u16, 0xa010, 0xc010, 0xe010, 0] {
                    for writing in [false, true] {
                        let mut bus = SmsBus::new(rom.clone(), 0xff);
                        bus.write(0xfffe, 17);
                        bus.write(0xcb14, 17);
                        bus.write(0xffff, 104);
                        bus.write(0xfffc, sram_control);
                        bus.write(0xcb03, 0xa5);
                        bus.write(0xc000, 0x5a);
                        bus.write(0xc810, 6); // selected PRG register R6
                        for (offset, page) in [2, 3, 6, 7].into_iter().enumerate() {
                            bus.write(0xc819 + offset as u16, page);
                        }
                        let mapping = (bus.slot_bank, bus.read(0xfffc));
                        let label = if writing {
                            "rt_mmc3_write"
                        } else {
                            "rt_mmc3_read_prg"
                        };
                        let mut cpu = Cpu::new();
                        cpu.pc = defs[label].1;
                        cpu.sp = 0xdff0;
                        cpu.a = 2;
                        cpu.f = 0x95;
                        cpu.c = 0x87;
                        cpu.set_de(0x52a9);
                        cpu.set_hl(if writing { 0x9fff } else { address });
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        bus.write(cpu.sp, 7);
                        bus.write(cpu.sp + 1, 0);
                        for _ in 0..400 {
                            if cpu.pc == 7 {
                                break;
                            }
                            cpu.step(&mut bus).unwrap();
                            assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                            assert_eq!(bus.read(0xcb1d), 0);
                        }
                        assert_eq!(cpu.pc, 7, "{label} failed to return");
                        assert_eq!(cpu.sp, 0xdff2);
                        assert_eq!((cpu.c, cpu.de()), (0x87, 0x52a9));
                        assert_eq!(bus.read(0xcb03), 0xa5);
                        assert_eq!(bus.read(0xcb14), 17);
                        assert_eq!((bus.slot_bank, bus.read(0xfffc)), mapping);
                        assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                        if writing {
                            assert_eq!((cpu.a, cpu.f), (2, 0x95));
                            assert_eq!(bus.read(0xc817), 2); // R6 data latch
                            assert_eq!(bus.read(0xc819), 2); // normal-mode $8000 page
                        } else {
                            let expected = match address {
                                0x8010 => 0x82,
                                0xa010 => 0x83,
                                0xc010 => 0x86,
                                0xe010 => 0x87,
                                0 => 0x5a,
                                _ => unreachable!(),
                            };
                            assert_eq!(cpu.a, expected, "read ${address:04X}");
                        }
                    }
                }
            }
        }
        // Guest indirect addressing remains rejected by the experimental
        // pipeline. Exercise the reachable helper ABI directly as a separate
        // fail-closed defense, including effective-address and pointer wraps.
        assert_eq!(defs["rt_read_zp_ptr_y"].0, 0);
        for iff in [false, true] {
            for sram_control in [0, 8, 12] {
                for zp in [0x10u8, 0xff] {
                    for (pointer, y) in [
                        (0x1ffeu16, 1u8),
                        (0x1fff, 1),
                        (0x2000, 0),
                        (0x2007, 0),
                        (0x3fff, 0),
                        (0x4000, 0),
                        (0x4016, 0),
                        (0x4020, 0),
                        (0x6000, 0),
                        (0x7ffe, 1),
                        (0x7fff, 1),
                        (0x8000, 0x10),
                        (0x9fff, 1),
                        (0xffff, 1),
                        (0xff80, 0xff),
                    ] {
                        let effective = pointer.wrapping_add(u16::from(y));
                        let unsupported = (0x2000..0x8000).contains(&effective);
                        let mut bus = SmsBus::new(rom.clone(), 0xff);
                        bus.write(0xfffe, 17);
                        bus.write(0xcb14, 17);
                        bus.write(0xffff, 104);
                        bus.write(0xfffc, sram_control);
                        bus.write(0xcb03, 0xa5);
                        bus.write(0xc000, 0x5a);
                        bus.write(0xc07f, 0xb6);
                        bus.write(0xc7ff, 0x6d);
                        bus.write(0xc000 + u16::from(zp), pointer as u8);
                        bus.write(0xc000 + u16::from(zp.wrapping_add(1)), (pointer >> 8) as u8);
                        for (offset, page) in [2, 3, 6, 7].into_iter().enumerate() {
                            bus.write(0xc819 + offset as u16, page);
                        }
                        let expected = if effective < 0x2000 {
                            bus.read(0xc000 + (effective & 0x7ff))
                        } else if effective >= 0x8000 {
                            let page =
                                [2usize, 3, 6, 7][usize::from((effective - 0x8000) / 0x2000)];
                            rom[96 * BANK_SIZE + page * 0x2000 + usize::from(effective & 0x1fff)]
                        } else {
                            0
                        };
                        let mapping = (bus.slot_bank, bus.read(0xfffc));
                        let mut cpu = Cpu::new();
                        cpu.pc = defs["rt_read_zp_ptr_y"].1;
                        cpu.sp = 0xdff0;
                        cpu.b = zp;
                        cpu.c = 0x87;
                        cpu.d = 0x52;
                        cpu.e = y;
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        bus.write(cpu.sp, 7);
                        bus.write(cpu.sp + 1, 0);
                        for _ in 0..400 {
                            if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                                break;
                            }
                            cpu.step(&mut bus).unwrap();
                            assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                        }
                        assert_eq!(cpu.de(), 0x5200 + u16::from(y));
                        // Unlike rt_mmc3_read_prg, this address-construction
                        // helper documents BC as scratch; C now carries Y.
                        assert_eq!(cpu.c, y);
                        assert_eq!(bus.read(0xcb03), 0xa5);
                        assert_eq!(bus.read(0xcb14), 17);
                        assert_eq!((bus.slot_bank, bus.read(0xfffc)), mapping);
                        if unsupported {
                            assert_eq!(bus.read(0xcb1d), 0xe8, "untrapped ${effective:04X}");
                            assert_ne!(cpu.pc, 7);
                            assert!(!cpu.iff1 && !cpu.iff2);
                        } else {
                            assert_eq!(bus.read(0xcb1d), 0);
                            assert_eq!(cpu.pc, 7, "unreturned ${effective:04X}");
                            assert_eq!(cpu.sp, 0xdff2);
                            assert_eq!(cpu.a, expected, "read ${effective:04X} via ZP ${zp:02X}");
                            assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_cooperative_wait_signal_preserves_runtime_abi() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        for iff in [false, true] {
            for enable in [0, 1, 0xff] {
                for tick in [0, 1, 0x7f, 0x80, 0xff] {
                    let mut bus = SmsBus::new(rom.clone(), 0xff);
                    bus.write(0xc010, tick);
                    bus.write(0xc01c, enable);
                    bus.write(0xcb03, 0xa5);
                    bus.write(0xfffc, 0x0c);
                    bus.write(0xfffe, 95);
                    bus.write(0xffff, 96);
                    let mapping = (bus.mapper_control, bus.slot_bank);
                    let mut cpu = Cpu::new();
                    cpu.pc = defs["rt_mmc3_wait_boundary"].1;
                    cpu.sp = 0xdff0;
                    cpu.a = 0x69;
                    cpu.f = 0x95;
                    cpu.set_hl(0xc010);
                    cpu.set_bc(0xc01c);
                    cpu.set_de(0x52a9);
                    cpu.iff1 = iff;
                    cpu.iff2 = iff;
                    bus.write(cpu.sp, 7);
                    bus.write(cpu.sp + 1, 0);
                    for _ in 0..100 {
                        if cpu.pc == 7 {
                            break;
                        }
                        cpu.step(&mut bus).unwrap();
                    }
                    assert_eq!(cpu.pc, 7);
                    assert_eq!(bus.read(0xc825), u8::from(enable != 0 && tick < 0x80));
                    assert_eq!((bus.read(0xc010), bus.read(0xc01c)), (tick, enable));
                    assert_eq!(
                        (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
                        (0x69, 0x95, 0xc01c, 0x52a9, 0xc010)
                    );
                    assert_eq!((cpu.iff1, cpu.iff2, bus.read(0xcb03)), (iff, iff, 0xa5));
                    assert_eq!((bus.mapper_control, bus.slot_bank), mapping);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_start_injection_survives_host_frames_until_guest_polls() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        let call = |bus: &mut SmsBus, pc: u16, a: u8| {
            let mut cpu = Cpu::new();
            cpu.pc = pc;
            cpu.sp = 0xdff0;
            cpu.a = a;
            cpu.set_de(0x52a9);
            cpu.iff2 = true;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            for _ in 0..300 {
                if cpu.pc == 7 {
                    break;
                }
                cpu.step(bus).unwrap();
            }
            assert_eq!(cpu.pc, 7);
            assert_eq!(cpu.de(), 0x52a9);
            cpu.a
        };
        let latch = defs["rt_controller_latch"].1;
        let strobe = defs["rt_controller_strobe"].1;
        let read = defs["rt_controller_read"].1;
        let mut bus = SmsBus::new(rom, 0xff);
        // Invoke the actual SMS Pause vector, not a synthetic counter write.
        assert_eq!(call(&mut bus, 0x66, 0xa5), 0xa5);
        assert_eq!(bus.read(0xcb2e), 4);
        for _ in 0..150 {
            call(&mut bus, latch, 0);
        }
        assert_eq!(bus.read(0xcb2e), 4);
        assert_eq!(bus.read(0xcb06), 8);
        for poll in 0..4 {
            call(&mut bus, strobe, 0);
            let mut buttons = 0;
            for bit in 0..8 {
                assert_eq!(call(&mut bus, read, 0x17), 0);
                assert_eq!(bus.read(0xcb07), bit);
                assert_eq!(bus.read(0xcb2e), 4 - poll);
                buttons |= call(&mut bus, read, 0x16) << bit;
                if bit != 7 {
                    assert_eq!(bus.read(0xcb2e), 4 - poll);
                }
            }
            assert_eq!(buttons, 8);
            assert_eq!(bus.read(0xcb2e), 3 - poll);
            assert_eq!(call(&mut bus, read, 0x16), 1);
            assert_eq!(bus.read(0xcb2e), 3 - poll);
            call(&mut bus, latch, 0);
        }
        assert_eq!(bus.read(0xcb06), 0, "next host latch releases Start");
        // Repeated strobes / incomplete bytes cannot consume a fresh press.
        call(&mut bus, 0x66, 0);
        for _ in 0..10 {
            call(&mut bus, strobe, 0);
            for _ in 0..7 {
                call(&mut bus, read, 0x16);
            }
            call(&mut bus, latch, 0);
        }
        assert_eq!(bus.read(0xcb2e), 4);
        assert_eq!(bus.read(0xcb06), 8);
        // Action mapping never consults SMB1's mode byte; test each button.
        bus.write(0xcb2e, 0);
        for mode in [0, 1, 0xff] {
            bus.write(0xc770, mode);
            for (sms_bit, nes_bit) in [(0, 4), (1, 5), (2, 6), (3, 7), (4, 0), (5, 1)] {
                bus.controller_port_dc = !(1 << sms_bit);
                call(&mut bus, latch, 0);
                assert_eq!(bus.read(0xcb06), 1 << nes_bit, "mode={mode}");
            }
        }
        // Run+jump may span many physical frames. Only the Pause vector may
        // inject Start in full mode; holding both action buttons never does.
        bus.controller_port_dc = 0xcf;
        for frame in 0..180 {
            call(&mut bus, latch, 0);
            assert_eq!(bus.read(0xcb2e), 0, "A+B armed Start at frame {frame}");
            call(&mut bus, strobe, 0);
            let mut buttons = 0;
            for bit in 0..8 {
                buttons |= call(&mut bus, read, 0x16) << bit;
            }
            assert_eq!(buttons, 3, "A+B serial byte at frame {frame}");
        }
        call(&mut bus, 0x66, 0);
        for remaining in (0..4).rev() {
            call(&mut bus, latch, 0);
            call(&mut bus, strobe, 0);
            let mut buttons = 0;
            for bit in 0..8 {
                buttons |= call(&mut bus, read, 0x16) << bit;
            }
            assert_eq!(buttons, 11, "Pause must coexist with A+B");
            assert_eq!(bus.read(0xcb2e), remaining);
        }
        call(&mut bus, latch, 0);
        assert_eq!(bus.read(0xcb06), 3, "Start releases while A+B stays held");
        bus.controller_port_dc = 0xff;
        call(&mut bus, latch, 0);
        assert_eq!(bus.read(0xcb06), 0);
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_bus_and_wide_frames_preserve_runtime_abi() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        for iff in [false, true] {
            for bank in [31, 32, 64, 95] {
                for helper in [
                    "rt_translated_return_consume",
                    "rt_translated_return_escape",
                ] {
                    for (ptr, frame) in [(0xd304u16, 0xd300u16), (0xd500, 0xd3f8), (0xd600, 0xd5fc)]
                    {
                        let mut bus = SmsBus::new(rom.clone(), 0xff);
                        bus.write(0xcb76, ptr as u8);
                        bus.write(0xcb77, (ptr >> 8) as u8);
                        bus.write(frame, 0xff);
                        bus.write(frame + 2, 0xc0);
                        bus.write(frame + 3, bank);
                        bus.write(0xcb02, 0xff);
                        bus.write(0xc100, 0x23);
                        bus.write(0xc101, 0x81);
                        bus.write(0xcb03, 0xa5);
                        let mut cpu = Cpu::new();
                        cpu.pc = defs[helper].1;
                        cpu.sp = 0xdff0;
                        cpu.a = 0x69;
                        cpu.f = 0x95;
                        cpu.set_bc(0x8123);
                        cpu.set_de(0x52a9);
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        bus.write(cpu.sp, 7);
                        bus.write(cpu.sp + 1, 0);
                        for _ in 0..400 {
                            if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                                break;
                            }
                            cpu.step(&mut bus).unwrap();
                        }
                        assert_eq!(cpu.pc, 7, "{helper} bank={bank}");
                        assert_eq!(
                            (cpu.a, cpu.f, cpu.de(), bus.read(0xcb02), bus.read(0xcb03)),
                            (0x69, 0x95, 0x52a9, 0xff, 0xa5)
                        );
                        assert_eq!(
                            bus.read(0xcb76) as u16 | ((bus.read(0xcb77) as u16) << 8),
                            frame
                        );
                        assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                    }
                }
            }
            for protect in [0, 0x40, 0x80, 0xc0] {
                for control in [0, 8, 12] {
                    for writing in [false, true] {
                        let mut bus = SmsBus::new(rom.clone(), 0xff);
                        bus.cart_ram[0] = 0xab;
                        bus.cart_ram[0x4000] = 0x5a;
                        bus.write(0xfffc, control);
                        bus.write(0xfffe, 95);
                        bus.write(0xcb14, 17);
                        bus.write(0xc81e, protect);
                        bus.write(0xcb03, 0xa5);
                        let mapping = (bus.slot_bank, bus.read(0xfffc));
                        let mut cpu = Cpu::new();
                        cpu.pc = defs[if writing {
                            "rt_mmc3_write_bus"
                        } else {
                            "rt_mmc3_read_bus"
                        }]
                        .1;
                        cpu.sp = 0xdff0;
                        cpu.a = 0x6d;
                        cpu.set_hl(0x6000);
                        cpu.set_bc(0x4587);
                        cpu.set_de(0x52a9);
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        bus.write(cpu.sp, 7);
                        bus.write(cpu.sp + 1, 0);
                        for _ in 0..400 {
                            if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                                break;
                            }
                            cpu.step(&mut bus).unwrap();
                        }
                        if !writing && protect & 0x80 == 0 {
                            assert_eq!(bus.read(0xcb1d), 0xe8);
                        } else {
                            assert_eq!(cpu.pc, 7);
                            assert_eq!(cpu.a, if writing { 0x6d } else { 0x5a });
                            assert_eq!((cpu.bc(), cpu.de(), cpu.hl()), (0x4587, 0x52a9, 0x6000));
                            assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                        }
                        assert_eq!((bus.slot_bank, bus.read(0xfffc)), mapping);
                        assert_eq!(bus.cart_ram[0], 0xab);
                        assert_eq!(
                            bus.cart_ram[0x4000],
                            if writing && protect == 0x80 {
                                0x6d
                            } else {
                                0x5a
                            }
                        );
                        assert_eq!(bus.read(0xcb03), 0xa5);
                    }
                }
            }
        }
        for iff in [false, true] {
            for bank in [31u8, 32, 64, 95] {
                for owned in [false, true] {
                    for (ptr, frame) in [(0xd304u16, 0xd300u16), (0xd500, 0xd3f8), (0xd600, 0xd5fc)]
                    {
                        let mut bus = SmsBus::new(rom.clone(), 0xff);
                        bus.write(0xcb76, ptr as u8);
                        bus.write(0xcb77, (ptr >> 8) as u8);
                        bus.write(frame, 0xff); // full-frame call-time guest S
                        bus.write(frame + 1, 7);
                        bus.write(frame + 2, if owned { 0xc0 } else { 0x40 });
                        bus.write(frame + 3, bank);
                        bus.write(0xcb02, 0xff);
                        // Owned returns consume this live pair: E1FF + 1.
                        // Guest S=FF wraps both reads to $0100/$0101.
                        bus.write(0xc100, 0xff);
                        bus.write(0xc101, 0xe1);
                        bus.write(0xc81c, 7);
                        bus.write(0xcb7e, u8::from(!iff));
                        bus.write(0xcb03, 0xa5);
                        let mut cpu = Cpu::new();
                        cpu.pc = defs["rt_translated_rts"].1;
                        cpu.sp = 0xdff0;
                        cpu.a = 0x69;
                        cpu.set_de(0x52a9);
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        let destination = if owned {
                            defs["L_E200"]
                        } else {
                            (bank, 0x4007)
                        };
                        for _ in 0..2000 {
                            if cpu.pc == destination.1 || bus.read(0xcb1d) != 0 {
                                break;
                            }
                            cpu.step(&mut bus).unwrap();
                        }
                        assert_eq!(cpu.pc, destination.1, "bank={bank} owned={owned}");
                        assert_eq!(bus.read(0xcb14), destination.0);
                        assert_eq!(bus.slot_bank[1], destination.0);
                        assert_eq!(bus.read(0xcb02), if owned { 1 } else { 0xff });
                        assert_eq!(
                            bus.read(0xcb76) as u16 | ((bus.read(0xcb77) as u16) << 8),
                            frame
                        );
                        assert_eq!(
                            (cpu.a, cpu.de(), cpu.sp, bus.read(0xcb03)),
                            (0x69, 0x52a9, 0xdff0, 0xa5)
                        );
                        assert_eq!((cpu.iff1, cpu.iff2), (iff, iff));
                    }
                }
            }
            for control in [0, 8, 12] {
                for address in [0x0000u16, 0x1fff, 0x6000, 0x7fff, 0x8000, 0xffff] {
                    let mut bus = SmsBus::new(rom.clone(), 0xff);
                    bus.write(0xfffe, 95);
                    bus.write(0xcb14, 17); // nested data mapping differs from code shadow
                    bus.write(0xffff, 104);
                    bus.write(0xfffc, control);
                    bus.write(0xc81e, 0x80);
                    bus.write(0xcb03, 0xa5);
                    for (i, page) in [0, 1, 6, 7].into_iter().enumerate() {
                        bus.write(0xc819 + i as u16, page);
                    }
                    let mapping = (bus.slot_bank, bus.read(0xfffc));
                    let mut cpu = Cpu::new();
                    cpu.pc = defs["rt_mmc3_read_bus"].1;
                    cpu.sp = 0xdff0;
                    cpu.set_bc(0x4587);
                    cpu.set_de(0x52a9);
                    cpu.set_hl(address);
                    cpu.iff1 = iff;
                    cpu.iff2 = iff;
                    bus.write(cpu.sp, 7);
                    bus.write(cpu.sp + 1, 0);
                    for _ in 0..400 {
                        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                            break;
                        }
                        cpu.step(&mut bus).unwrap();
                    }
                    assert_eq!(cpu.pc, 7, "read ${address:04X}");
                    assert_eq!((cpu.bc(), cpu.de(), cpu.hl()), (0x4587, 0x52a9, address));
                    assert_eq!((bus.slot_bank, bus.read(0xfffc)), mapping);
                    assert_eq!(bus.read(0xcb03), 0xa5);
                    assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_ram_bus_fast_path_preserves_boundaries_and_interrupt_context() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        // These are the complete additions, including restoration before
        // the unchanged slow entries. Nominal Z80 costs (not Cpu's approximate
        // cycle counter): RAM read 74 T/write 95 T; misses add 23 T/65 T.
        let read_prefix = [
            0x7c, 0xfe, 0x20, 0x30, 9, 0xe5, 0xe6, 7, 0xf6, 0xc0, 0x67, 0x7e, 0xe1, 0xc9,
        ];
        let write_prefix = [
            0xe5, 0xf5, 0x7c, 0xfe, 0x20, 0x30, 9, 0xe6, 7, 0xf6, 0xc0, 0x67, 0xf1, 0x77, 0xe1,
            0xc9, 0xf1, 0xe1,
        ];
        let mut cases = 0;
        let mut injected = 0;
        for writing in [false, true] {
            let entry = defs[if writing {
                "rt_mmc3_write_bus"
            } else {
                "rt_mmc3_read_bus"
            }]
            .1;
            for address in [
                0x0000u16, 0x00ff, 0x0100, 0x07ff, 0x0800, 0x17ff, 0x1fff, 0x2000, 0x3fff, 0x4016,
                0x5fff, 0x6000, 0x7fff, 0x8000, 0xffff,
            ] {
                for iff in [false, true] {
                    for control in [0, 8, 12] {
                        let setup = || {
                            let mut bus = SmsBus::new(rom.clone(), 0xff);
                            for (addr, value) in [
                                (0xfffe, 95),
                                (0xcb14, 17),
                                (0xffff, 104),
                                (0xfffc, control),
                                (0xc81e, 0x80),
                                (0xcb03, 0xa5),
                                (0xcb28, 1),
                                (0xc831, 2),
                                (0xc83f, 0x35),
                            ] {
                                bus.write(addr, value);
                            }
                            for (i, page) in [0, 1, 6, 7].into_iter().enumerate() {
                                bus.write(0xc819 + i as u16, page);
                            }
                            bus.write(0xc000 + (address & 0x7ff), 0x96);
                            bus.cart_ram[0x4000 + (address as usize & 0x1fff)] = 0x96;
                            let mut cpu = Cpu::new();
                            cpu.pc = entry;
                            cpu.sp = 0xdff0;
                            cpu.a = 0x5a;
                            cpu.f = 0x95;
                            cpu.set_hl(address);
                            cpu.set_bc(0x4587);
                            cpu.set_de(0x52a9);
                            cpu.iff1 = iff;
                            cpu.iff2 = iff;
                            bus.write(cpu.sp, 7);
                            bus.write(cpu.sp + 1, 0);
                            (bus, cpu)
                        };
                        let (mut reference_bus, mut reference_cpu) = setup();
                        let mapping = (reference_bus.slot_bank, reference_bus.read(0xfffc));
                        let mut boundaries = Vec::new();
                        for _ in 0..5000 {
                            if reference_cpu.pc == 7 || reference_bus.read(0xcb1d) != 0 {
                                break;
                            }
                            boundaries.push((
                                reference_cpu.pc,
                                reference_cpu.iff1 && reference_cpu.ei_pending == 0,
                            ));
                            reference_cpu.step(&mut reference_bus).unwrap();
                        }
                        cases += 1;
                        if address == 0x5fff {
                            assert_eq!(reference_bus.read(0xcb1d), 0xe8);
                            continue;
                        }
                        assert_eq!(reference_bus.read(0xcb1d), 0, "{writing} {address:04x}");
                        assert_eq!(reference_cpu.pc, 7, "{writing} {address:04x}");
                        assert_eq!(
                            (
                                reference_cpu.bc(),
                                reference_cpu.de(),
                                reference_cpu.hl(),
                                reference_cpu.sp
                            ),
                            (0x4587, 0x52a9, address, 0xdff2)
                        );
                        assert_eq!((reference_cpu.iff1, reference_cpu.iff2), (iff, iff));
                        assert_eq!(
                            (reference_bus.slot_bank, reference_bus.read(0xfffc)),
                            mapping
                        );
                        assert_eq!(reference_bus.read(0xcb03), 0xa5);
                        if writing {
                            assert_eq!(reference_cpu.a, 0x5a);
                        } else {
                            let expected = match address {
                                0x2000 => 0x35,       // write-only PPU register returns its bus latch
                                0x3fff | 0x4016 => 0, // buffered PPUDATA / first serial button
                                0x6000 | 0x7fff => 0x96,
                                0x8000 | 0xffff => {
                                    let page =
                                        [0, 1, 6, 7][usize::from((address - 0x8000) / 0x2000)];
                                    rom[96 * 0x4000 + page * 0x2000 + usize::from(address & 0x1fff)]
                                }
                                _ => 0x96,
                            };
                            assert_eq!(reference_cpu.a, expected, "read {address:04x}");
                        }
                        if (0x6000..0x8000).contains(&address) {
                            assert_eq!(
                                reference_bus.cart_ram[0x4000 + usize::from(address & 0x1fff)],
                                if writing { 0x5a } else { 0x96 }
                            );
                        }
                        if address < 0x2000 {
                            assert_eq!(reference_cpu.a, if writing { 0x5a } else { 0x96 });
                            assert_eq!(
                                reference_bus.read(0xc000 + (address & 0x7ff)),
                                if writing { 0x5a } else { 0x96 }
                            );
                        }
                        // Inject the real IM1 handler at every interruptible
                        // instruction boundary, including before/after each
                        // new PUSH/POP. DI sections must remain uninterruptible.
                        for (step, (_, interruptible)) in boundaries.iter().enumerate() {
                            if !interruptible {
                                continue;
                            }
                            let (mut bus, mut cpu) = setup();
                            for _ in 0..step {
                                cpu.step(&mut bus).unwrap();
                            }
                            let interrupted = cpu;
                            cpu.sp -= 1;
                            bus.write(cpu.sp, (cpu.pc >> 8) as u8);
                            cpu.sp -= 1;
                            bus.write(cpu.sp, cpu.pc as u8);
                            cpu.pc = 0x38;
                            cpu.iff1 = false;
                            cpu.iff2 = false;
                            cpu.ei_pending = 0;
                            bus.frame_int_pending = true;
                            for _ in 0..20000 {
                                cpu.step(&mut bus).unwrap();
                                if cpu.pc == interrupted.pc && cpu.sp == interrupted.sp {
                                    break;
                                }
                            }
                            assert_eq!(
                                (cpu.pc, cpu.sp, cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
                                (
                                    interrupted.pc,
                                    interrupted.sp,
                                    interrupted.a,
                                    interrupted.f,
                                    interrupted.bc(),
                                    interrupted.de(),
                                    interrupted.hl()
                                )
                            );
                            assert_eq!((cpu.iff1, cpu.iff2), (true, true));
                            for _ in 0..5000 {
                                if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                                    break;
                                }
                                cpu.step(&mut bus).unwrap();
                            }
                            assert_eq!(
                                (cpu.pc, cpu.sp, cpu.a, cpu.bc(), cpu.de(), cpu.hl()),
                                (7, 0xdff2, reference_cpu.a, 0x4587, 0x52a9, address),
                                "write={writing} address={address:04x} step={step}"
                            );
                            assert_eq!((bus.slot_bank, bus.read(0xfffc)), mapping);
                            assert_eq!(bus.read(0xcb03), 0xa5);
                            assert_eq!((cpu.iff1, cpu.iff2), (true, true));
                            if address < 0x2000 {
                                assert_eq!(
                                    bus.read(0xc000 + (address & 0x7ff)),
                                    reference_bus.read(0xc000 + (address & 0x7ff))
                                );
                            }
                            injected += 1;
                        }
                    }
                }
            }
        }
        eprintln!("{cases} bus vectors; {injected} real host IRQ instruction-boundary injections");
        for (name, expected) in [
            ("rt_mmc3_read_bus", read_prefix.as_slice()),
            ("rt_mmc3_write_bus", write_prefix.as_slice()),
        ] {
            let (bank, entry) = defs[name];
            assert_eq!(bank, 0, "fast path must remain in fixed slot zero");
            assert_eq!(
                &rom[entry as usize..entry as usize + expected.len()],
                expected,
                "32-byte shared fast-path budget / nominal timing contract"
            );
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT from mmc3_full_bus_assembled_matches_original_6502"]
    fn mmc3_compiled_ram_rmw_preserves_old_and_final_bus_writes() {
        struct Writes {
            bus: SmsBus,
            enabled: bool,
            stack: Vec<u8>,
            zero_page: Vec<u8>,
        }
        impl Bus for Writes {
            fn read(&mut self, address: u16) -> u8 {
                self.bus.read(address)
            }
            fn write(&mut self, address: u16, value: u8) {
                if self.enabled {
                    match address {
                        0xc1ff => self.stack.push(value),
                        0xc0f0 => self.zero_page.push(value),
                        _ => {}
                    }
                }
                self.bus.write(address, value);
            }
            fn in_port(&mut self, port: u8) -> u8 {
                self.bus.in_port(port)
            }
            fn out_port(&mut self, port: u8, value: u8) {
                self.bus.out_port(port, value);
            }
        }
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let mut bus = Writes {
            bus: SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff),
            enabled: false,
            stack: Vec::new(),
            zero_page: Vec::new(),
        };
        let mut cpu = Cpu::new();
        for _ in 0..2_000_000 {
            if cpu.pc == defs["translated_reset"].1
                && bus.bus.slot_bank[1] == defs["translated_reset"].0
            {
                bus.enabled = true;
            }
            if bus.read(0xc03f) == 0xa5 || bus.read(0xcb1d) != 0 {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert!(bus.enabled);
        assert_eq!(bus.read(0xcb1d), 0);
        assert_eq!(bus.read(0xc03f), 0xa5);
        // Source fixture: STA/INC/LSR/ROL $01FF. Every RMW
        // writes its old value before its new value, even through fast RAM.
        assert_eq!(bus.stack, [0x7f, 0x7f, 0x80, 0x80, 0x40, 0x40, 0x80]);
        assert_eq!(bus.zero_page, [0x7f, 0x7f, 0x80]);
        assert_eq!((bus.read(0xc048), bus.read(0xc049)), (0x55, 0x34));
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME project"]
    fn mmc3_full_dispatch_matches_sorted_first_match() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        let physical = |name: &str| {
            let (bank, addr) = defs[name];
            usize::from(bank) * BANK_SIZE + usize::from(addr & 0x3fff)
        };
        let table = physical("rt_dispatch_table");
        let directory = physical("rt_dispatch_page_table");
        let counts = physical("rt_dispatch_page_counts");
        assert_eq!(counts, directory + 256);
        assert!(counts + 128 <= (table / BANK_SIZE + 1) * BANK_SIZE);
        assert_eq!(&rom[directory - 2..directory], &[0, 0]);
        assert_eq!((directory - 2 - table) % 6, 0);
        let records: Vec<_> = rom[table..directory - 2]
            .as_chunks::<6>()
            .0
            .iter()
            .map(|r| {
                (
                    u16::from_le_bytes([r[0], r[1]]),
                    r[2],
                    r[3],
                    u16::from_le_bytes([r[4], r[5]]),
                )
            })
            .collect();
        for page in 0..128usize {
            let address = ((page + 128) as u16) << 8;
            let index = records.partition_point(|r| r.0 < address);
            let count = records.iter().filter(|r| r.0 >> 8 == address >> 8).count();
            assert_eq!(rom[counts + page], u8::try_from(count).unwrap_or(0));
            let pointer =
                u16::from_le_bytes([rom[directory + page * 2], rom[directory + page * 2 + 1]]);
            assert_eq!(usize::from(pointer), (table & 0x3fff) + 0x4000 + index * 6);
        }
        assert!(records.windows(2).all(|r| {
            let key = |v: &(u16, u8, u8, u16)| {
                (
                    v.0,
                    if v.1 == 0xff {
                        0u16
                    } else {
                        u16::from(v.1) + 1
                    },
                )
            };
            key(&r[0]) <= key(&r[1])
        }));
        let bank_count: u8 = std::fs::read_to_string(path.join("sms.asm"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix(".define NES_MMC3_PRG_BANK_COUNT "))
            .unwrap()
            .parse()
            .unwrap();
        let mut queries = std::collections::BTreeSet::from([0x0000u16, 0x7fff, 0x8000, 0xffff]);
        for record in &records {
            queries.extend([
                record.0.saturating_sub(1),
                record.0,
                record.0.saturating_add(1),
            ]);
        }
        for page in 0x80u16..=0xff {
            queries.extend([page << 8, (page << 8) | 0xff]);
        }
        let mut bus = SmsBus::new(rom, 0xff);
        let mut calls = 0usize;
        let mut hits = 0usize;
        let mut min_sp = 0xdff0;
        for target in queries {
            for bank in 0..bank_count {
                // Independent old first-match rule, including wildcard-first
                // precedence. Never derive the expected record by binary search.
                let expected = records
                    .iter()
                    .find(|r| r.0 == target && (r.1 == 0xff || r.1 == bank));
                for iff in [false, true] {
                    let control = [0, 8, 12][calls % 3];
                    for (addr, value) in [
                        (0xfffe, 95),
                        (0xcb14, 95),
                        (0xffff, 104),
                        (0xfffc, control),
                        (0xcb03, 0xa5),
                        (0xcb02, 0xf7),
                        (0xcb7e, u8::from(!iff)),
                        (0xcb1d, 0),
                        (0xcb76, 0),
                        (0xcb77, 0xd3),
                        (0xd47d, 0xc0),
                        (0xd47e, 0xd4),
                    ] {
                        bus.write(addr, value);
                    }
                    for window in 0..4 {
                        bus.write(0xc819 + window, bank);
                    }
                    let mut cpu = Cpu::new();
                    cpu.pc = defs["rt_banked_tail_dispatch"].1;
                    cpu.sp = 0xdff0;
                    cpu.a = 0x69;
                    cpu.f = 0x95;
                    cpu.set_bc(target);
                    cpu.set_de(0x52a9);
                    cpu.iff1 = iff;
                    cpu.iff2 = iff;
                    let mut arrived = false;
                    for _ in 0..20000 {
                        if let Some(record) = expected
                            && cpu.pc == record.3
                            && (record.2 == 0 || bus.slot_bank[1] == record.2)
                        {
                            arrived = true;
                            break;
                        }
                        if bus.read(0xcb1d) != 0 {
                            break;
                        }
                        if (defs["_btd_lower_bound"].1..defs["_btd_lower_bound_end"].1)
                            .contains(&cpu.pc)
                        {
                            assert!(!cpu.iff1, "binary transaction must stay DI");
                        }
                        cpu.step(&mut bus).unwrap();
                        min_sp = min_sp.min(cpu.sp);
                    }
                    if let Some(record) = expected {
                        assert!(arrived, "target={target:04x} bank={bank} pc={:04x}", cpu.pc);
                        assert_eq!((cpu.a, cpu.de(), cpu.sp), (0x69, 0x52a9, 0xdff0));
                        assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                        assert_eq!(
                            (bus.read(0xcb14), bus.slot_bank[1], bus.read(0xcb7c)),
                            (record.2, record.2, record.1)
                        );
                        let first = records.iter().position(|r| r.0 == target).unwrap();
                        let selected = records
                            .iter()
                            .position(|r| std::ptr::eq(r, record))
                            .unwrap();
                        let page_count = records.iter().filter(|r| r.0 >> 8 == target >> 8).count();
                        let start = if page_count > 255 {
                            records
                                .iter()
                                .position(|r| r.0 >> 8 == target >> 8)
                                .unwrap()
                        } else {
                            first
                        };
                        assert_eq!(
                            bus.read(0xcb7d),
                            (selected - start + 1) as u8,
                            "visit diagnostic counts only residual equal-address scan"
                        );
                        hits += 1;
                    } else {
                        assert_eq!(bus.read(0xcb1d), 0xe2, "target={target:04x} bank={bank}");
                        assert_eq!(cpu.sp, 0xdff0, "miss must retire search scratch");
                    }
                    assert_eq!((bus.read(0xcb03), bus.read(0xcb02)), (0xa5, 0xf7));
                    assert_eq!(
                        (
                            bus.read(0xcb76),
                            bus.read(0xcb77),
                            bus.read(0xd47d),
                            bus.read(0xd47e)
                        ),
                        (0, 0xd3, 0xc0, 0xd4)
                    );
                    assert_eq!((bus.slot_bank[2], bus.read(0xfffc)), (104, control));
                    calls += 1;
                }
            }
        }
        assert!(
            min_sp >= 0xdfea,
            "helper exceeds six extra native stack bytes"
        );
        assert!(defs["_btd_lower_bound_end"].1 - defs["_btd_lower_bound"].1 + 3 <= 96);
        eprintln!(
            "{} sorted records; {calls} bank/address/IFF cases; {hits} hits; minSP={min_sp:04x}",
            records.len()
        );
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME project"]
    fn mmc3_full_dispatch_count_edges_and_pending_irq() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let original = std::fs::read(path.join("sms.sms")).unwrap();
        let table_bank = defs["rt_dispatch_table"].0;
        let physical = |addr: u16| usize::from(table_bank) * BANK_SIZE + usize::from(addr & 0x3fff);
        let base = 0x5000u16;
        let page = 0x28usize;
        let mut calls = 0;
        let mut irq_cases = 0;
        for count in [0usize, 1, 3, 127, 128, 167, 254, 255, 256, 257] {
            let mut rom = original.clone();
            // Synthetic records use the actual assembled search, including
            // conflicting wildcard/concrete records and six-byte strides.
            let mut records: Vec<_> = (0..count)
                .map(|i| {
                    (
                        0xa800 | ((i * 256 / count) as u16),
                        if i % 7 == 0 { 0xff } else { (i % 4) as u8 },
                        (4 + i % 16) as u8,
                        0x4007u16,
                    )
                })
                .collect();
            records.sort_by_key(|r| (r.0, if r.1 == 0xff { 0 } else { u16::from(r.1) + 1 }));
            for (i, &(addr, bank, sms_bank, label)) in records.iter().enumerate() {
                let offset = physical(base) + i * 6;
                rom[offset..offset + 6].copy_from_slice(&[
                    addr as u8,
                    (addr >> 8) as u8,
                    bank,
                    sms_bank,
                    label as u8,
                    (label >> 8) as u8,
                ]);
            }
            rom[physical(base) + count * 6..physical(base) + count * 6 + 2].fill(0);
            let directory = physical(defs["rt_dispatch_page_table"].1) + page * 2;
            rom[directory..directory + 2].copy_from_slice(&base.to_le_bytes());
            rom[physical(defs["rt_dispatch_page_counts"].1) + page] =
                u8::try_from(count).unwrap_or(0);
            let mut bus = SmsBus::new(rom, 0xff);
            for low in 0..=255u16 {
                for bank in 0..=4u8 {
                    let target = 0xa800 | low;
                    let expected = records
                        .iter()
                        .find(|r| r.0 == target && (r.1 == 0xff || r.1 == bank));
                    for iff in [false, true] {
                        for (addr, value) in [
                            (0xfffe, 95),
                            (0xcb14, 95),
                            (0xffff, 104),
                            (0xfffc, 12),
                            (0xcb03, 0xa5),
                            (0xcb7e, u8::from(!iff)),
                            (0xcb1d, 0),
                            (0xc81a, bank),
                            (0xcb28, 1),
                            (0xc831, 2),
                        ] {
                            bus.write(addr, value);
                        }
                        let mut cpu = Cpu::new();
                        cpu.pc = defs["rt_banked_tail_dispatch"].1;
                        cpu.sp = 0xdff0;
                        cpu.a = 0x69;
                        cpu.f = 0x95;
                        cpu.set_bc(target);
                        cpu.set_de(0x52a9);
                        cpu.iff1 = iff;
                        cpu.iff2 = iff;
                        let inject = |cpu: &mut Cpu, bus: &mut SmsBus| {
                            let interrupted = *cpu;
                            cpu.sp -= 1;
                            bus.write(cpu.sp, (cpu.pc >> 8) as u8);
                            cpu.sp -= 1;
                            bus.write(cpu.sp, cpu.pc as u8);
                            cpu.pc = 0x38;
                            cpu.iff1 = false;
                            cpu.iff2 = false;
                            cpu.ei_pending = 0;
                            bus.frame_int_pending = true;
                            for _ in 0..20000 {
                                cpu.step(bus).unwrap();
                                if cpu.pc == interrupted.pc && cpu.sp == interrupted.sp {
                                    break;
                                }
                            }
                            assert_eq!(
                                (cpu.pc, cpu.sp, cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl()),
                                (
                                    interrupted.pc,
                                    interrupted.sp,
                                    interrupted.a,
                                    interrupted.f,
                                    interrupted.bc(),
                                    interrupted.de(),
                                    interrupted.hl()
                                )
                            );
                            assert!(cpu.iff1 && cpu.iff2);
                        };
                        let with_irq = iff && bank == 0 && [0, 127, 128, 255].contains(&low);
                        if with_irq {
                            inject(&mut cpu, &mut bus);
                            irq_cases += 1;
                        }
                        // A request pending during search cannot enter the DI
                        // transaction; deliver only after the original EI/JP.
                        bus.frame_int_pending = with_irq;
                        for _ in 0..20000 {
                            if expected.is_some_and(|r| cpu.pc == r.3 && bus.slot_bank[1] == r.2)
                                || bus.read(0xcb1d) != 0
                            {
                                break;
                            }
                            if (defs["_btd_lower_bound"].1..defs["_btd_lower_bound_end"].1)
                                .contains(&cpu.pc)
                                || (defs["_btd_loop"].1..defs["_btd_hit"].1).contains(&cpu.pc)
                            {
                                assert!(!cpu.iff1, "search must not admit the pending IRQ");
                            }
                            cpu.step(&mut bus).unwrap();
                            assert!(cpu.sp >= 0xdfea);
                        }
                        if let Some(record) = expected {
                            assert_eq!(
                                (cpu.pc, bus.slot_bank[1], bus.read(0xcb7c)),
                                (record.3, record.2, record.1),
                                "count={count} low={low} bank={bank}"
                            );
                            assert_eq!((cpu.a, cpu.de(), cpu.sp), (0x69, 0x52a9, 0xdff0));
                            assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                            if with_irq {
                                inject(&mut cpu, &mut bus);
                                irq_cases += 1;
                            }
                        } else {
                            assert_eq!(
                                bus.read(0xcb1d),
                                0xe2,
                                "count={count} low={low} bank={bank}"
                            );
                            assert_eq!(cpu.sp, 0xdff0);
                        }
                        assert_eq!(
                            (bus.read(0xcb03), bus.read(0xfffc), bus.slot_bank[2]),
                            (0xa5, 12, 104)
                        );
                        calls += 1;
                    }
                }
            }
        }
        eprintln!(
            "{calls} synthetic count/duplicate/miss cases; {irq_cases} real IRQ entry/exit injections"
        );
    }

    #[test]
    fn native_stack_guard_rejects_existing_bg_refcounts_but_accepts_floor() {
        assert_eq!(NATIVE_STACK_FLOOR, 0xDD80 + 192);
        for sp in [0, 0xDD7F, 0xDD80, 0xDE3F] {
            assert!(native_stack_guard_failed(true, sp), "SP=${sp:04X}");
            assert!(!native_stack_guard_failed(false, sp));
        }
        for sp in [0xDE40, 0xDE41, 0xDFFB, 0xDFFC] {
            assert!(!native_stack_guard_failed(true, sp), "SP=${sp:04X}");
        }
    }

    #[test]
    fn pc_profile_ranks_cycle_cost_not_instruction_hits() {
        let mut rom = vec![0; BANK_SIZE];
        rom[1..4].copy_from_slice(&[0x2A, 0x34, 0x12]); // LD HL,($1234): 16 cycles.
        let mut bus = SmsBus::new(rom, 0xFF);
        let mut cpu = Cpu::new();
        let mut profile = PcProfile::new();
        for pc in [0, 0, 1] {
            cpu.pc = pc;
            let address = pc_profile_address(&bus, cpu.pc);
            let before = cpu.cycles;
            cpu.step(&mut bus).unwrap();
            record_pc_profile_step(&mut profile, address, before, cpu.cycles);
        }
        assert_eq!(
            profile[&(Some(0), 0)],
            PcProfileCost {
                instructions: 2,
                approx_cycles: 10, // The emulator's opcode approximation charges 5 per NOP.
            }
        );
        assert_eq!(
            profile.values().map(|cost| cost.approx_cycles).sum::<u64>(),
            cpu.cycles
        );
        let report = format_pc_profile(&profile, vec![(0, 0, "nop".into()), (0, 1, "load".into())]);
        assert!(report.contains("3 completed instructions; 26 approx_cycles"));
        let first_row: Vec<_> = report.lines().nth(3).unwrap().split_whitespace().collect();
        assert_eq!(first_row, ["61.54%", "16", "1", "00:0001", "load"]);
        assert!(report.contains("not inclusive function/call overhead"));
        assert!(report.contains("not scanline timing"));
    }

    #[test]
    fn pc_profile_uses_instruction_fetch_bank_before_mapper_write() {
        let mut rom = vec![0; BANK_SIZE * 3];
        rom[BANK_SIZE..BANK_SIZE + 3].copy_from_slice(&[0x32, 0xFE, 0xFF]);
        let mut bus = SmsBus::new(rom, 0xFF);
        let mut cpu = Cpu::new();
        let mut profile = PcProfile::new();
        cpu.pc = 0x4000;
        cpu.a = 2;
        let address = pc_profile_address(&bus, cpu.pc);
        let before = cpu.cycles;
        cpu.step(&mut bus).unwrap(); // LD ($FFFE),A changes the executing bank.
        record_pc_profile_step(&mut profile, address, before, cpu.cycles);
        assert_eq!(bus.slot_bank[1], 2);
        assert_eq!(profile[&(Some(1), 0x4000)].instructions, 1);
        assert!(!profile.contains_key(&(Some(2), 0x4000)));

        bus.slot_bank[0] = 2;
        assert_eq!(pc_profile_address(&bus, 0x0038), (Some(0), 0x0038));
        assert_eq!(pc_profile_address(&bus, 0x0400), (Some(2), 0x0400));
        assert_eq!(pc_profile_address(&bus, 0x8000), (Some(2), 0x8000));
        bus.write(0xFFFC, 0x08);
        assert_eq!(pc_profile_address(&bus, 0x8000), (None, 0x8000));
        assert_eq!(pc_profile_address(&bus, 0xC000), (None, 0xC000));
    }

    #[test]
    fn pc_profile_symbol_ranges_do_not_cross_banks_or_hide_unknown_code() {
        let cost = PcProfileCost {
            instructions: 1,
            approx_cycles: 4,
        };
        let profile = PcProfile::from([
            ((Some(0), 0x4000), cost),
            ((Some(1), 0x4000), cost),
            ((Some(2), 0x4000), cost),
            ((None, 0xC000), cost),
        ]);
        let report = format_pc_profile(
            &profile,
            vec![
                (1, 0x4000, "same_name".into()),
                (0, 0x4000, "same_name".into()),
            ],
        );
        assert!(report.contains("00:4000 same_name"));
        assert!(report.contains("01:4000 same_name"));
        assert!(report.contains("02:4000?"));
        assert!(report.contains("non-ROM:C000?"));
        let empty = format_pc_profile(&PcProfile::new(), Vec::new());
        assert!(empty.contains("0 completed instructions; 0 approx_cycles"));
        assert!(!empty.contains("NaN"));
    }

    #[test]
    fn irq_to_ei_report_does_not_claim_game_frame_or_vblank_cost() {
        assert_eq!(format_irq_to_ei_cost(&[]), None);
        let report = format_irq_to_ei_cost(&[119_472, 0, 59_736]).unwrap();
        assert!(
            report
                .starts_with("irq_to_ei_cost approx_cycles: intervals=3 min=0 p50=59736 avg=59736")
        );
        assert!(report.contains("not total gameplay-frame cost"));
        assert!(report.contains("not a VBlank upload deadline"));
        assert!(report.contains("intervals_above_full_frame=1 (33.3%)"));
        assert!(report.contains("worst_interval=2.00x avg=1.00x"));
    }

    #[test]
    fn recoverable_rts_fallback_is_not_a_hard_runtime_trap() {
        assert!(!is_hard_runtime_trap(0x00));
        assert!(!is_hard_runtime_trap(0xE3));
        for marker in [0xE1, 0xE2, 0xE4, 0xEE] {
            assert!(is_hard_runtime_trap(marker), "marker ${marker:02X}");
        }
    }

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
    fn dffc_ram_write_does_not_alias_mapper_control() {
        let mut bus = SmsBus::new(Vec::new(), 0xFF);
        bus.write(0xFFFC, 0x08);
        bus.write(0xDFFC, 0x55);

        assert_eq!(bus.read(0xDFFC), 0x55);
        assert_eq!(bus.read(0xFFFC), 0x08);
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
            "raw_ciram_storage=blocked reason=no_internal_ram_without_reclaim required_tile_bytes=1920 attr_bytes_existing=128 candidate=$CC00-$D3FF blocked_by=folded_s_reclaim_required stack_candidate=$DE40-$DFFB:no_go"
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

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_ppu_preserves_bus_state_and_physical_chr_semantics() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let asm = std::fs::read_to_string(path.join("sms.asm")).unwrap();
        let define = |name: &str| -> usize {
            asm.lines()
                .find_map(|line| line.strip_prefix(&format!(".define {name} ")))
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };
        let chr_base = define("MMC3_CHR_DATA_BASE") * BANK_SIZE;
        let pages = define("MMC3_CHR_BANK_COUNT");
        let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
        // Synthetic physical source markers, not injected game state or a
        // claimed playable-ROM run. Execute the actual assembled helpers.
        for page in 0..pages {
            for offset in 0..0x400 {
                rom[chr_base + page * 0x400 + offset] =
                    (page as u8).wrapping_mul(29).wrapping_add(offset as u8);
            }
        }
        for iff in [false, true] {
            for control in [0, 8, 12] {
                for inversion in [0, 0x80] {
                    let mut bus = SmsBus::new(rom.clone(), 0xff);
                    bus.write(0xfffe, 17);
                    bus.write(0xcb14, 17);
                    bus.write(0xffff, 99);
                    bus.write(0xfffc, control);
                    bus.write(0xcb03, 0xa5);
                    let invoke =
                        |bus: &mut SmsBus, label: &str, value: u8, reg: u8, address: u16| -> u8 {
                            assert_eq!(defs[label].0, 0);
                            let mapping = (bus.slot_bank, bus.read(0xfffc));
                            let mut cpu = Cpu::new();
                            cpu.pc = defs[label].1;
                            cpu.sp = 0xdff0;
                            cpu.a = value;
                            cpu.f = 0x95;
                            cpu.b = reg;
                            cpu.c = 0x87;
                            cpu.set_de(0x52a9);
                            cpu.set_hl(address);
                            cpu.iff1 = iff;
                            cpu.iff2 = iff;
                            bus.write(cpu.sp, 7);
                            bus.write(cpu.sp + 1, 0);
                            for _ in 0..3000 {
                                if cpu.pc == 7 {
                                    break;
                                }
                                cpu.step(bus).unwrap();
                                assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                                assert_eq!(bus.read(0xcb1d), 0, "{label} trapped");
                            }
                            assert_eq!(cpu.pc, 7, "{label} did not return");
                            assert_eq!(cpu.sp, 0xdff2);
                            assert_eq!(cpu.de(), 0x52a9, "{label} changed DE");
                            assert_eq!(bus.read(0xcb03), 0xa5);
                            assert_eq!(bus.read(0xcb14), 17);
                            assert_eq!(
                                (bus.slot_bank, bus.read(0xfffc)),
                                mapping,
                                "{label} leaked mapping"
                            );
                            assert_eq!((cpu.iff1, cpu.iff2, cpu.ei_pending), (iff, iff, 0));
                            if label == "rt_ppu_write" || label == "rt_mmc3_write" {
                                assert_eq!((cpu.a, cpu.f), (value, 0x95));
                            }
                            cpu.a
                        };
                    let registers = [0xff, 0xfc, 0x83, 0x9a, 5, 0x26];
                    for (register, value) in registers.into_iter().enumerate() {
                        invoke(
                            &mut bus,
                            "rt_mmc3_write",
                            register as u8 | inversion,
                            0,
                            0x8000,
                        );
                        invoke(&mut bus, "rt_mmc3_write", value, 0, 0x8001);
                    }
                    let mask = (pages - 1) as u8;
                    let mut map = [
                        registers[0] & !1,
                        registers[0] | 1,
                        registers[1] & !1,
                        registers[1] | 1,
                        registers[2],
                        registers[3],
                        registers[4],
                        registers[5],
                    ]
                    .map(|page| page & mask);
                    if inversion != 0 {
                        map.rotate_left(4);
                    }
                    for (index, page) in map.into_iter().enumerate() {
                        assert_eq!(bus.read(0xc850 + index as u16), page);
                        for offset in [0, 0x3ffu16] {
                            let address = (index as u16) * 0x400 + offset;
                            bus.write(0xcb0b, 0);
                            invoke(&mut bus, "rt_ppu_write", (address >> 8) as u8, 6, 0);
                            invoke(&mut bus, "rt_ppu_write", address as u8, 6, 0);
                            bus.write(0xcb11, 0xa7);
                            assert_eq!(invoke(&mut bus, "rt_ppu_read", 0, 7, 0), 0xa7);
                            let expected = (page as u8).wrapping_mul(29).wrapping_add(offset as u8);
                            assert_eq!(bus.read(0xcb11), expected, "CHR ${address:04X}");
                            bus.write(0xcb0f, (address >> 8) as u8);
                            bus.write(0xcb10, address as u8);
                            invoke(&mut bus, "rt_ppu_write", expected ^ 0xff, 7, 0);
                            assert_eq!(
                                bus.rom[chr_base + usize::from(page) * 0x400 + usize::from(offset)],
                                expected
                            );
                        }
                    }
                    for mirroring in [0, 1] {
                        invoke(&mut bus, "rt_mmc3_write", mirroring, 0, 0xa000);
                        for page in 0..4u16 {
                            bus.write(0xcb0f, 0x20 + (page * 4) as u8);
                            bus.write(0xcb10, 0x31);
                            invoke(&mut bus, "rt_ppu_write", 0x60 + page as u8, 7, 0);
                            let ciram_page = if mirroring == 0 { page & 1 } else { page >> 1 };
                            let old = bus.read(0xfffc);
                            bus.write(0xfffc, 8);
                            assert_eq!(bus.read(0x8031 + ciram_page * 0x400), 0x60 + page as u8);
                            bus.write(0xfffc, old);
                            bus.write(0xcb0f, 0x30 + (page * 4) as u8);
                            bus.write(0xcb10, 0x31);
                            invoke(&mut bus, "rt_ppu_read", 0, 7, 0);
                            assert_eq!(bus.read(0xcb11), 0x60 + page as u8);
                        }
                    }
                    // Palette aliases are immediate reads, and refill the
                    // delayed read buffer from the mirrored nametable below.
                    for index in [0x10u8, 0x14, 0x18, 0x1c] {
                        bus.write(0xcb0f, 0x3f);
                        bus.write(0xcb10, index);
                        invoke(&mut bus, "rt_ppu_write", 0x2b, 7, 0);
                        assert_eq!(bus.read(0xc860 + u16::from(index & 0x0f)), 0x2b);
                        bus.write(0xcb0f, 0x3f);
                        bus.write(0xcb10, index & 0x0f);
                        assert_eq!(invoke(&mut bus, "rt_ppu_read", 0, 7, 0), 0x2b);
                    }
                    invoke(&mut bus, "rt_ppu_write", 4, 0, 0);
                    bus.write(0xcb0f, 0x3f);
                    bus.write(0xcb10, 0xf0);
                    invoke(&mut bus, "rt_ppu_write", 0x13, 7, 0);
                    assert_eq!((bus.read(0xcb0f), bus.read(0xcb10)), (0x40, 0x10));
                    // Shared loopy w: a status read resets both aliases;
                    // scroll0/239 produces the native SMB3 TADD=$73A0.
                    invoke(&mut bus, "rt_ppu_read", 0, 2, 0);
                    invoke(&mut bus, "rt_ppu_write", 0xa8, 0, 0);
                    invoke(&mut bus, "rt_ppu_write", 0, 5, 0);
                    invoke(&mut bus, "rt_ppu_write", 239, 5, 0);
                    assert_eq!((bus.read(0xc834), bus.read(0xc835)), (0x73, 0xa0));
                    assert_eq!((bus.read(0xcb0b), bus.read(0xcb0e)), (0, 0));
                    // Actual IRQ intent distinguishes $2006 reload from
                    // temporary scroll latches. Match the selected native
                    // reference's visible $2007 read vertical increment.
                    for active in [0, 1] {
                        for mask in [0, 0x18] {
                            bus.write(0xc8f7, active);
                            invoke(&mut bus, "rt_ppu_write", mask, 1, 0);
                            invoke(&mut bus, "rt_ppu_read", 0, 2, 0);
                            invoke(&mut bus, "rt_ppu_write", 0x0b, 6, 0);
                            invoke(&mut bus, "rt_ppu_write", 0, 6, 0);
                            assert_eq!(bus.read(0xc8f7), if active == 1 { 3 } else { 0 });
                            for (before, after) in [
                                (0x0b00u16, 0x1b00),
                                (0x6ba0, 0x7ba0),
                                (0x7ba0, 0x0000),
                                (0x7be0, 0x0800),
                                (0x7b00, 0x0b20),
                            ] {
                                bus.write(0xcb0f, (before >> 8) as u8);
                                bus.write(0xcb10, before as u8);
                                invoke(&mut bus, "rt_ppu_read", 0, 7, 0);
                                let expected = if active == 1 && mask != 0 {
                                    after
                                } else {
                                    before + 1
                                };
                                assert_eq!(
                                    u16::from_be_bytes([bus.read(0xcb0f), bus.read(0xcb10)]),
                                    expected
                                );
                            }
                        }
                    }
                    bus.write(0xc8f7, 0);
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_renderer_uses_frozen_chr_and_exact_fine_y_split() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let asm = std::fs::read_to_string(path.join("sms.asm")).unwrap();
        let chr_base: usize = asm
            .lines()
            .find_map(|line| line.strip_prefix(".define MMC3_CHR_DATA_BASE "))
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * BANK_SIZE;
        let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
        // Two physically distinct, deliberately simple planar patterns.
        // The same logical NES tile is selected on both sides of the IRQ.
        for (page, planes) in [(0, [0xaa, 0x55]), (2, [0xf0, 0xcc])] {
            rom[chr_base + page * 0x400..chr_base + page * 0x400 + 8].fill(planes[0]);
            rom[chr_base + page * 0x400 + 8..chr_base + page * 0x400 + 16].fill(planes[1]);
        }
        for row in 0..16usize {
            let tile = row / 8;
            rom[chr_base + 4 * 0x400 + tile * 16 + row % 8] = 1 << (row % 8);
            rom[chr_base + 4 * 0x400 + tile * 16 + row % 8 + 8] = if row < 8 { 0x55 } else { 0xaa };
        }
        for fine_y in [0u8, 7] {
            for split in [192u8, 193] {
                let mut bus = SmsBus::new(rom.clone(), 0xff);
                bus.write(0xfffe, 17);
                bus.write(0xcb14, 17);
                bus.write(0xfffc, 8);
                bus.write(0xc831, 2); // renderer owns a frozen packet
                bus.write(0xcb08, 0xa8);
                bus.write(0xcb09, 0x18);
                bus.write(0xc824, 1); // horizontal CIRAM mirroring
                bus.write(0xc834, if fine_y == 7 { 0x73 } else { 0 });
                bus.write(0xc835, if fine_y == 7 { 0xa0 } else { 0 });
                for offset in 0..0x800 {
                    bus.write(0x8000 + offset, 0);
                }
                for offset in 0..0x100 {
                    bus.write(0xc900 + offset, 0xe0);
                }
                let sprite_attr =
                    3 | if fine_y == 7 { 0x40 } else { 0 } | if split == 193 { 0x80 } else { 0 };
                for (offset, value) in [24, 1, sprite_attr, 17].into_iter().enumerate() {
                    bus.write(0xc900 + offset as u16, value);
                }
                for index in 0..8 {
                    bus.write(0xc850 + index, index as u8);
                }
                for index in 0..32 {
                    bus.write(0xc860 + index, index as u8);
                }
                let invoke = |bus: &mut SmsBus, label: &str| {
                    let mut cpu = Cpu::new();
                    cpu.pc = defs[label].1;
                    cpu.sp = 0xdff0;
                    bus.write(cpu.sp, 7);
                    bus.write(cpu.sp + 1, 0);
                    let mut di_start = 0;
                    let mut max_di = 0;
                    let mut di_pc = cpu.pc;
                    let mut max_di_pc = (cpu.pc, cpu.pc);
                    for _ in 0..3_000_000 {
                        if cpu.pc == 7 {
                            break;
                        }
                        let enabled = cpu.iff1;
                        cpu.step(bus).unwrap();
                        if !enabled && cpu.iff1 && cpu.cycles - di_start > max_di {
                            max_di = cpu.cycles - di_start;
                            max_di_pc = (di_pc, cpu.pc);
                        }
                        if enabled && !cpu.iff1 {
                            di_start = cpu.cycles;
                            di_pc = cpu.pc;
                        }
                        assert_eq!(bus.read(0xcb1d), 0, "{label} trapped");
                        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                    }
                    assert_eq!(cpu.pc, 7, "{label} did not return");
                    assert!(
                        max_di < 59_736,
                        "{label} masked host interrupts for {max_di}T at {max_di_pc:04X?}"
                    );
                    (cpu.cycles, max_di)
                };
                invoke(&mut bus, "rt_mmc3_capture_playfield");
                bus.write(0xc850, 2);
                for index in 0..32 {
                    bus.write(0xc860 + index, 0x20 + index as u8);
                }
                invoke(&mut bus, "rt_mmc3_capture_hud");
                // Later live producer changes must not affect this frame.
                bus.write(0xc850, 4);
                bus.write(0x8000, 0x7f);
                bus.write(0xc837, split);
                let (cycles, max_di) = invoke(&mut bus, "rt_mmc3_frame_render");
                eprintln!(
                    "MMC3 renderer fixture fineY={fine_y} split={split}: interpreterT={cycles}, maxDI={max_di}T (no host IRQ delivery in this helper test)"
                );
                for y in 0..224usize {
                    for x in 0..256usize {
                        let source_y = y + usize::from(fine_y);
                        let entry = 0x3700 + (source_y / 8 * 32 + x / 8) * 2;
                        let tile = usize::from(bus.vram[entry])
                            | (usize::from(bus.vram[entry + 1] & 1) << 8);
                        let pattern = tile * 32 + (source_y & 7) * 4;
                        let pixel = (0..4).fold(0u8, |value, plane| {
                            value | (((bus.vram[pattern + plane] >> (7 - x % 8)) & 1) << plane)
                        });
                        let planes = if y < usize::from(split) {
                            [0xaa, 0x55]
                        } else {
                            [0xf0, 0xcc]
                        };
                        let expected = ((planes[0] >> (7 - x % 8)) & 1)
                            | (((planes[1] >> (7 - x % 8)) & 1) << 1);
                        assert_eq!(pixel, expected, "fineY={fine_y} split={split} at{x},{y}");
                    }
                }
                for index in 0..32usize {
                    let source = if index % 4 == 0 { 0 } else { index as u8 };
                    assert_eq!(bus.cram[index], assets::nes_palette_to_sms_color(source));
                }
                assert_eq!(bus.vram[0x3f00], 24);
                assert_eq!(&bus.vram[0x3f80..0x3f82], &[17, 0]);
                for row in 0..16usize {
                    let source_row = if sprite_attr & 0x80 != 0 {
                        15 - row
                    } else {
                        row
                    };
                    let mut p0 = 1u8 << (source_row % 8);
                    let mut p1: u8 = if source_row < 8 { 0x55 } else { 0xaa };
                    if sprite_attr & 0x40 != 0 {
                        p0 = p0.reverse_bits();
                        p1 = p1.reverse_bits();
                    }
                    assert_eq!(
                        &bus.vram[0x2000 + row * 4..0x2004 + row * 4],
                        &[p0, p1, p0 | p1, p0 | p1],
                        "8x16 attr={sprite_attr:02X} row{row}"
                    );
                }
                bus.write(0xc831, 0);
                invoke(&mut bus, "rt_mmc3_display_line");
                for index in 0..32usize {
                    let source = if index % 4 == 0 {
                        0x20
                    } else {
                        0x20 + index as u8
                    };
                    assert_eq!(bus.cram[index], assets::nes_palette_to_sms_color(source));
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_renderer_respects_irq_vertical_reload() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let asm = std::fs::read_to_string(path.join("sms.asm")).unwrap();
        let chr_base = asm
            .lines()
            .find_map(|line| line.strip_prefix(".define MMC3_CHR_DATA_BASE "))
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * BANK_SIZE;
        let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
        for tile in 0..64usize {
            for row in 0..8 {
                let value = (tile * 8 + row) as u8;
                rom[chr_base + tile * 16 + row] = value;
                rom[chr_base + tile * 16 + row + 8] = !value;
            }
        }
        for fine_y in [0u8, 7] {
            for split in [192u8, 193] {
                for irq_fine_y in [0u8, 1] {
                    let mut bus = SmsBus::new(rom.clone(), 0xff);
                    bus.write(0xfffc, 8);
                    bus.write(0xc831, 1);
                    bus.write(0xcb08, 0xa8);
                    bus.write(0xcb09, 0x18);
                    bus.write(0xc824, 1);
                    // Different nametable rows and planes expose both wrong
                    // coarse origin and a one-pixel fine-Y composition error.
                    for page in 0..2u16 {
                        for row in 0..30u16 {
                            for col in 0..32 {
                                bus.write(
                                    0x8000 + page * 0x400 + row * 32 + col,
                                    (page * 32 + row) as u8,
                                );
                            }
                        }
                    }
                    for offset in 0..256 {
                        bus.write(0xc900 + offset, 0xe0);
                    }
                    bus.write(0xc834, 3 | (fine_y << 4));
                    bus.write(0xc835, 0x40); // NMI origin coarseY26, NT2000
                    let invoke = |bus: &mut SmsBus, label: &str| {
                        let mut cpu = Cpu::new();
                        cpu.pc = defs[label].1;
                        cpu.sp = 0xdff0;
                        bus.write(cpu.sp, 7);
                        bus.write(cpu.sp + 1, 0);
                        for _ in 0..3_000_000 {
                            if cpu.pc == 7 {
                                break;
                            }
                            cpu.step(bus).unwrap();
                            assert_eq!(bus.read(0xcb1d), 0, "{label} trapped");
                        }
                        assert_eq!(cpu.pc, 7);
                    };
                    invoke(&mut bus, "rt_mmc3_capture_playfield");
                    bus.write(0xc8f7, 3); // actual IRQ + explicit $2006 reload
                    bus.write(0xcb0f, 0x0b | (irq_fine_y << 4));
                    bus.write(0xcb10, 0);
                    bus.write(0xc834, 0x73);
                    bus.write(0xc835, 0xa0); // final t differs from live v
                    invoke(&mut bus, "rt_mmc3_capture_hud");
                    assert_eq!(
                        (bus.read(0x9970), bus.read(0x9971), bus.read(0x9972)),
                        (0, 0x0b | (irq_fine_y << 4), 1)
                    );
                    bus.write(0xc8f7, 0);
                    bus.write(0xc837, split);
                    invoke(&mut bus, "rt_mmc3_frame_render");
                    for y in 0..224usize {
                        let (origin, delta) = if y < usize::from(split) {
                            (26 * 8 + usize::from(fine_y), y)
                        } else {
                            (
                                240 + 24 * 8 + usize::from(irq_fine_y),
                                y - usize::from(split),
                            )
                        };
                        let source_y = (origin + delta) % 480;
                        let tile = (source_y / 240) * 32 + (source_y % 240) / 8;
                        let p0 = (tile * 8 + source_y % 8) as u8;
                        let grid_y = y + usize::from(fine_y);
                        for x in 0..256usize {
                            let entry = 0x3700 + (grid_y / 8 * 32 + x / 8) * 2;
                            let slot = usize::from(bus.vram[entry])
                                | (usize::from(bus.vram[entry + 1] & 1) << 8);
                            let row = slot * 32 + grid_y % 8 * 4;
                            let pixel = ((bus.vram[row] >> (7 - x % 8)) & 1)
                                | (((bus.vram[row + 1] >> (7 - x % 8)) & 1) << 1);
                            let expected =
                                ((p0 >> (7 - x % 8)) & 1) | (((!p0 >> (7 - x % 8)) & 1) << 1);
                            assert_eq!(
                                pixel, expected,
                                "NMI fineY{fine_y} split{split} IRQ fineY{irq_fine_y} at{x},{y}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_bg_stable_matches_forced_rebuild() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let asm = std::fs::read_to_string(path.join("sms.asm")).unwrap();
        let chr_base = asm
            .lines()
            .find_map(|line| line.strip_prefix(".define MMC3_CHR_DATA_BASE "))
            .unwrap()
            .parse::<usize>()
            .unwrap()
            * BANK_SIZE;
        let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
        // Physical-page, tile and row identities must all affect pixels.
        for page in 0..8usize {
            for tile in 0..64usize {
                for row in 0..8usize {
                    rom[chr_base + page * 1024 + tile * 16 + row] = (page as u8 * 29)
                        .wrapping_add(tile as u8)
                        .rotate_left(row as u32);
                    rom[chr_base + page * 1024 + tile * 16 + row + 8] =
                        !(page as u8 * 19 + row as u8);
                }
            }
        }
        let invoke = |bus: &mut SmsBus, label: &str| {
            let mut cpu = Cpu::new();
            cpu.pc = defs[label].1;
            cpu.sp = 0xdff0;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            for _ in 0..3_000_000 {
                if cpu.pc == 7 {
                    break;
                }
                cpu.step(bus).unwrap();
                assert_eq!(bus.read(0xcb1d), 0, "{label} trapped");
            }
            assert_eq!(cpu.pc, 7);
            cpu.cycles
        };
        let write_ppu = |bus: &mut SmsBus, register: u8, value: u8| {
            let mut cpu = Cpu::new();
            cpu.pc = defs["rt_ppu_write"].1;
            cpu.sp = 0xdff0;
            cpu.a = value;
            cpu.b = register;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            for _ in 0..3000 {
                if cpu.pc == 7 {
                    break;
                }
                cpu.step(bus).unwrap();
            }
            assert_eq!(cpu.pc, 7);
            assert_eq!(bus.read(0xcb1d), 0);
        };
        let invoke_with_host_irq = |bus: &mut SmsBus| {
            let mut cpu = Cpu::new();
            cpu.pc = defs["rt_mmc3_frame_present"].1;
            cpu.sp = 0xdff0;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            bus.write(0xcb28, 1);
            let mut fired = false;
            let ticks = bus.read(0xcb04);
            for _ in 0..3_000_000 {
                if cpu.pc == 7 {
                    break;
                }
                if !fired && cpu.iff1 && cpu.ei_pending == 0 && bus.read(0xc831) == 2 {
                    cpu.sp -= 1;
                    bus.write(cpu.sp, (cpu.pc >> 8) as u8);
                    cpu.sp -= 1;
                    bus.write(cpu.sp, cpu.pc as u8);
                    cpu.pc = 0x38;
                    cpu.iff1 = false;
                    cpu.iff2 = false;
                    bus.frame_int_pending = true;
                    fired = true;
                }
                cpu.step(bus).unwrap();
                assert_eq!(bus.read(0xcb1d), 0);
                assert!(cpu.sp >= NATIVE_STACK_FLOOR);
            }
            assert!(fired);
            assert_eq!(cpu.pc, 7);
            assert_eq!(bus.read(0xcb04), ticks.wrapping_add(1));
            assert_eq!(
                (bus.read(0xc070), bus.read(0xc071)),
                (0, 0),
                "no nested guest handler"
            );
            cpu.cycles
        };
        // Distinct field groups and both source records. A sprite change
        // before a HUD-only BG change must not hide the latter comparison.
        for (fine_y, split, table, inverted) in [
            (0u8, 192u8, 0u8, false),
            (7, 193, 0, true),
            (7, 192, 16, false),
            (0, 193, 16, true),
        ] {
            let mut cases = vec![
                (0xc900u16, false, false),
                (0xc901, false, false),
                (0xc902, false, false),
                (0xc903, false, false),
                (0xc904, false, false), // hidden -> visible
                (0xc860, false, false),
                (0xc87f, true, false),
                (0xc836, false, false),
                (0xc836, true, false),
                (0x8000, false, true),
                (0x87ff, true, true),
                (0xcb08, false, true),
                (0xcb09, true, true),
                (0xcb0c, false, false),
                (0xcb0d, true, false),
                (0xc834, false, true),
                (0xc835, true, true),
                (0xc824, true, true),
                (0xcb10, true, true),
                (0xcb0f, true, true),
                (0xc8f7, true, true),
                (0xc837, true, false), // presentation independently checks split
            ];
            for page in 0..8u16 {
                cases.push((
                    0xc850 + page,
                    page & 1 != 0,
                    page / 4 == u16::from(table / 16),
                ));
            }
            // Record mask1=playfield only,2=HUD only,3=both. Keep each raw
            // scroll byte independent, then combine them with fine-X change.
            let mut cases: Vec<(Vec<u16>, u8, bool)> = cases
                .into_iter()
                .map(|(address, hud_only, bg)| (vec![address], if hud_only { 2 } else { 3 }, bg))
                .collect();
            cases.extend([(vec![0xcb0c], 1, false), (vec![0xcb0d], 1, false)]);
            cases.extend([(vec![0xcb0c], 2, false), (vec![0xcb0d], 3, false)]);
            for records in 1..=3 {
                cases.push((vec![0xcb0c, 0xcb0d], records, false));
                cases.push((vec![0xcb0c, 0xcb0d, 0xc836], records, false));
            }
            for (addresses, records, bg_changed) in cases {
                let address = addresses[0];
                let raw_scroll = matches!(address, 0xcb0c | 0xcb0d);
                let mut outcomes = Vec::new();
                let mut coarse_outcomes = Vec::new();
                for mode in 0..3 {
                    let force_full = mode == 1;
                    let mut bus = SmsBus::new(rom.clone(), 0xff);
                    bus.write(0xfffc, 8);
                    bus.write(0xc831, 1);
                    bus.write(0xcb08, 0xa8 | table);
                    bus.write(0xcb09, 0x18);
                    bus.write(0xc824, 1);
                    bus.write(0xc834, fine_y << 4);
                    bus.write(0xc837, split);
                    if raw_scroll {
                        // Real PPUADDR writes replace t while the raw $2005
                        // bytes retain an unrelated older scroll. Rendering
                        // must use t, including after the next raw-only change.
                        bus.write(0xcb0c, 0x57);
                        bus.write(0xcb0d, 0xa3);
                        bus.write(0xcb0b, 0);
                        for value in [0x20 | ((fine_y & 3) << 4), 0] {
                            write_ppu(&mut bus, 6, value);
                        }
                        assert_eq!(bus.read(0xc835), 0);
                        assert_eq!(bus.read(0xc834), 0x20 | ((fine_y & 3) << 4));
                        assert_eq!((bus.read(0xcb0c), bus.read(0xcb0d)), (0x57, 0xa3));
                    }
                    for page in 0..8u16 {
                        bus.write(0xc850 + page, page as u8 ^ if inverted { 4 } else { 0 });
                    }
                    for index in 0..32u16 {
                        bus.write(0xc860 + index, index as u8);
                    }
                    for index in 0..256u16 {
                        bus.write(0xc900 + index, 0xe0);
                    }
                    for (index, value) in [24, 1, 3, 17].into_iter().enumerate() {
                        bus.write(0xc900 + index as u16, value);
                    }
                    let capture = |bus: &mut SmsBus, mutate: bool| {
                        bus.write(0xc8f7, 0);
                        let original: Vec<_> =
                            addresses.iter().map(|&a| (a, bus.read(a))).collect();
                        if mutate && records & 1 != 0 {
                            for &(address, value) in &original {
                                bus.write(
                                    address,
                                    match address {
                                        0xc900 => 0xe0,                  // active -> hidden
                                        0xcb08 | 0xc834 => value ^ 0x10, // BG table / fine Y
                                        _ => value ^ 1,
                                    },
                                );
                            }
                        }
                        invoke(bus, "rt_mmc3_capture_playfield");
                        if mutate && records == 1 {
                            for &(address, value) in &original {
                                bus.write(address, value);
                            }
                        }
                        bus.write(0xc8f7, 3);
                        bus.write(0xcb0f, 0x1b);
                        bus.write(0xcb10, 0);
                        if mutate && records == 2 {
                            for &(address, _) in &original {
                                let value = bus.read(address);
                                bus.write(address, value ^ if address == 0xc8f7 { 2 } else { 1 });
                            }
                        }
                        invoke(bus, "rt_mmc3_capture_hud");
                    };
                    capture(&mut bus, false);
                    invoke(&mut bus, "rt_mmc3_frame_present");
                    let prior_bg = bus.vram[..0x2000].to_vec();
                    let prior_nt = bus.vram[0x3700..0x3f00].to_vec();
                    // Ensure an earlier non-BG difference never short-circuits
                    // the later BG compare; also exercises moving sprite SAT.
                    bus.write(0xc903, 23);
                    if address == 0xc904 {
                        bus.write(address, 24);
                    }
                    capture(&mut bus, true);
                    assert_eq!(
                        bus.read(0xc8ec) != 0,
                        bg_changed,
                        "group addr{addresses:04X?} records{records} table{table} inverted{inverted}"
                    );
                    assert_eq!(bus.read(0xc8f4), 1, "raw scroll must still change packet");
                    let producer = [0x8000, 0xc850, 0xc900, 0xc860].map(|a| (a, bus.read(a)));
                    // Frozen records, not subsequent producer writes, own this
                    // frame. Host-only service cannot reenter the producer.
                    bus.write(0x8000, 0xff);
                    bus.write(0xc850, 7);
                    bus.write(0xc900, 90);
                    bus.write(0xc860, 0x30);
                    if force_full {
                        bus.write(0xc8ec, 1);
                    }
                    let cycles = if mode == 2 {
                        invoke_with_host_irq(&mut bus)
                    } else {
                        invoke(&mut bus, "rt_mmc3_frame_present")
                    };
                    if !bg_changed && address != 0xc837 {
                        assert_eq!(&bus.vram[..0x2000], prior_bg);
                        assert_eq!(&bus.vram[0x3700..0x3f00], prior_nt);
                    }
                    let playfield_cram = bus.cram;
                    let playfield_regs = bus.vdp_regs;
                    bus.write(0xc831, 0);
                    invoke(&mut bus, "rt_mmc3_display_line");
                    outcomes.push((
                        bus.vram,
                        playfield_cram,
                        bus.cram,
                        bus.vdp_regs,
                        cycles,
                        playfield_regs,
                    ));
                    if raw_scroll {
                        // Follow raw-only reuse with a real coarse-t change.
                        // Restore only deliberate live mutations, not committed
                        // renderer state; capture must detect the next BG change.
                        for (address, value) in producer {
                            bus.write(address, value);
                        }
                        let next_coarse_x = (bus.read(0xc835) ^ 1) & 31;
                        let scroll = next_coarse_x * 8 + bus.read(0xc836);
                        bus.write(0xcb0b, 0);
                        write_ppu(&mut bus, 5, scroll);
                        assert_eq!(bus.read(0xc835) & 31, next_coarse_x);
                        capture(&mut bus, false);
                        assert_eq!(bus.read(0xc8ec), 1, "subsequent coarse t was ignored");
                        if mode == 2 {
                            invoke_with_host_irq(&mut bus);
                        } else {
                            invoke(&mut bus, "rt_mmc3_frame_present");
                        }
                        let playfield = (bus.cram, bus.vdp_regs);
                        bus.write(0xc831, 0);
                        invoke(&mut bus, "rt_mmc3_display_line");
                        coarse_outcomes.push((bus.vram, playfield, bus.cram, bus.vdp_regs));
                    }
                }
                assert_eq!(outcomes[0].0, outcomes[1].0, "VRAM addr{address:04X}");
                assert_eq!(outcomes[0].1, outcomes[1].1, "playfield palette");
                assert_eq!(outcomes[0].2, outcomes[1].2, "HUD palette");
                assert_eq!(outcomes[0].3, outcomes[1].3, "presentation registers");
                assert_eq!(outcomes[0].5, outcomes[1].5, "playfield registers");
                assert_eq!(outcomes[0].0, outcomes[2].0, "nested host VRAM");
                assert_eq!(
                    outcomes[0].1, outcomes[2].1,
                    "nested host playfield palette"
                );
                assert_eq!(outcomes[0].2, outcomes[2].2, "nested host HUD palette");
                assert_eq!(outcomes[0].3, outcomes[2].3, "nested host HUD registers");
                assert_eq!(
                    outcomes[0].5, outcomes[2].5,
                    "nested host playfield registers"
                );
                if raw_scroll {
                    assert_eq!(
                        coarse_outcomes[0], coarse_outcomes[1],
                        "coarse-t forced full"
                    );
                    assert_eq!(
                        coarse_outcomes[0], coarse_outcomes[2],
                        "coarse-t nested IRQ"
                    );
                }
                if !bg_changed && address != 0xc837 {
                    assert!(outcomes[0].4 < outcomes[1].4 / 2);
                }
                if address == 0xc903 {
                    eprintln!(
                        "MMC3 BG-stable synthetic fineY{fine_y} split{split} table{table} inversion{inverted}: fast={}T forcedFull={}T (no delivered host IRQ)",
                        outcomes[0].4, outcomes[1].4
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_packets_reuse_only_exact_committed_state() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        let mut bus = SmsBus::new(rom, 0xff);
        bus.write(0xfffc, 8);
        bus.write(0xc831, 1);
        bus.write(0xcb08, 0xa8);
        bus.write(0xcb09, 0x18);
        for offset in 0..256 {
            bus.write(0xc900 + offset, 0xe0);
        }
        let invoke = |bus: &mut SmsBus, label: &str| {
            let mut cpu = Cpu::new();
            cpu.pc = defs[label].1;
            cpu.sp = 0xdff0;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            for _ in 0..3_000_000 {
                if cpu.pc == 7 {
                    break;
                }
                cpu.step(bus).unwrap();
                assert_eq!(bus.read(0xcb1d), 0, "{label} trapped");
            }
            assert_eq!(cpu.pc, 7, "{label} did not return");
            cpu.cycles
        };
        let capture = |bus: &mut SmsBus| {
            invoke(bus, "rt_mmc3_capture_playfield") + invoke(bus, "rt_mmc3_capture_hud")
        };
        let first_capture = capture(&mut bus);
        assert_eq!(bus.read(0xc8f4), 1);
        let rebuild = invoke(&mut bus, "rt_mmc3_frame_present");
        assert_eq!(bus.read(0xc833), 1);
        let vram = bus.vram;
        let cram = bus.cram;
        let registers = bus.vdp_regs;
        bus.write(0xc831, 1);
        let same_capture = capture(&mut bus);
        assert_eq!(bus.read(0xc8f4), 0);
        let reuse = invoke(&mut bus, "rt_mmc3_frame_present");
        assert_eq!(bus.read(0xc831), 1, "reuse must not enter renderer");
        assert_eq!(bus.vram, vram);
        assert_eq!(bus.cram, cram);
        assert_eq!(bus.vdp_regs, registers);
        assert!(reuse < 500); // exact reuse plus mandatory layer validation
        eprintln!(
            "MMC3 exact-packet synthetic interpreterT: firstCapture={first_capture} rebuild={rebuild}, unchangedCapture={same_capture} reuse={reuse}"
        );

        // Every source identity participates, including bytes the present
        // viewport does not currently display. No hash is a correctness gate.
        for address in [
            0x8000, 0x87ff, 0xc900, 0xc9ff, 0xc850, 0xc857, 0xc860, 0xc87f, 0xcb08, 0xcb09, 0xcb0c,
            0xcb0d, 0xc834, 0xc835, 0xc836, 0xc824,
        ] {
            capture(&mut bus);
            assert_eq!(bus.read(0xc8f4), 0);
            let old = bus.read(address);
            bus.write(address, old ^ 1);
            capture(&mut bus);
            assert_eq!(bus.read(0xc8f4), 1, "identity {address:04X} ignored");
            bus.write(address, old);
            capture(&mut bus);
        }
        capture(&mut bus);
        // A HUD-only producer write must be observed after playfield freeze.
        bus.write(0x87fe, 1);
        invoke(&mut bus, "rt_mmc3_capture_hud");
        assert_eq!(bus.read(0xc8f4), 1);
        bus.write(0x87fe, 0);
        capture(&mut bus);
        capture(&mut bus);
        for (before, after) in [(0x1b00u16, 0x1b01u16), (0x1b00, 0x2b00)] {
            bus.write(0xc8f7, 3);
            bus.write(0xcb0f, (before >> 8) as u8);
            bus.write(0xcb10, before as u8);
            invoke(&mut bus, "rt_mmc3_capture_hud");
            bus.write(0xc8f7, 0);
            invoke(&mut bus, "rt_mmc3_capture_playfield");
            assert_eq!(bus.read(0xc8f4), 0);
            bus.write(0xc8f7, 3);
            bus.write(0xcb0f, (after >> 8) as u8);
            bus.write(0xcb10, after as u8);
            invoke(&mut bus, "rt_mmc3_capture_hud");
            assert_eq!(bus.read(0xc8f4), 1, "IRQ live-v identity must participate");
        }
        bus.write(0xc8f7, 0);
        invoke(&mut bus, "rt_mmc3_capture_playfield");
        invoke(&mut bus, "rt_mmc3_capture_hud");
        assert_eq!(
            bus.read(0xc8f4),
            1,
            "removing IRQ reload intent must participate"
        );
        capture(&mut bus);
        bus.write(0xc837, 192);
        assert!(invoke(&mut bus, "rt_mmc3_frame_present") > 100_000);
        assert_eq!(bus.read(0xc8f0), 192, "split-only changes must publish");
        bus.write(0xcb09, 0);
        capture(&mut bus);
        invoke(&mut bus, "rt_mmc3_frame_present");
        assert_eq!(bus.read(0xc833), 0);
        assert_eq!(
            bus.vdp_regs[1] & 0x40,
            0,
            "completed render-off packet must blank"
        );
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_partial_layers_trap_before_publication_or_reuse() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        let invoke = |bus: &mut SmsBus, entry: &str| {
            let mut cpu = Cpu::new();
            cpu.pc = defs[entry].1;
            cpu.sp = 0xdff0;
            bus.write(cpu.sp, 7);
            bus.write(cpu.sp + 1, 0);
            for _ in 0..3_000_000 {
                if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                    break;
                }
                cpu.step(bus).unwrap();
            }
            assert!(cpu.pc == 7 || bus.read(0xcb1d) != 0);
        };
        for entry in ["rt_mmc3_frame_present", "rt_mmc3_frame_render"] {
            for ready in [0, 1] {
                for changed in [0, 1] {
                    for record in [0x9900, 0x9940] {
                        for partial in [0x08, 0x10] {
                            let mut bus = SmsBus::new(rom.clone(), 0xff);
                            bus.write(0xfffc, 8);
                            bus.write(0x9909, 0x18);
                            bus.write(0x9949, 0x18);
                            bus.write(record + 9, partial);
                            bus.write(0xc833, ready);
                            bus.write(0xc8f4, changed);
                            bus.vram.fill(0x5a);
                            bus.cram.fill(0x21);
                            bus.vdp_regs[1] = if ready == 1 { 0xf2 } else { 0xb2 };
                            bus.vdp_regs[7] = 0x0d;
                            let video = (bus.vram, bus.cram, bus.vdp_regs);
                            bus.write(0xfffc, 0); // entry must select the frozen SRAM records
                            invoke(&mut bus, entry);
                            assert_eq!(
                                bus.read(0xcb1d),
                                0xea,
                                "{entry} accepted record{record:04X} mask{partial:02X}"
                            );
                            assert_eq!(
                                (bus.vram, bus.cram, bus.vdp_regs),
                                video,
                                "unsupported layers must not blank, repaint, recolor, or publish a frame"
                            );
                        }
                    }
                }
            }
        }
        for mask in [0x00, 0x18, 0x1e] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            bus.write(0xfffc, 8);
            for record in [0x9900, 0x9940] {
                bus.write(record + 8, 0xa8);
                bus.write(record + 9, mask);
            }
            for offset in 0..256 {
                bus.write(0x9800 + offset, 0xe0);
            }
            invoke(&mut bus, "rt_mmc3_frame_present");
            assert_eq!(bus.read(0xcb1d), 0);
            assert_eq!(bus.read(0xc833), u8::from(mask != 0));
            assert_eq!(bus.vdp_regs[1] & 0x40, if mask == 0 { 0 } else { 0x40 });
            if mask != 0 {
                assert_eq!(bus.vdp_regs[0] & 0x20, if mask == 0x18 { 0x20 } else { 0 });
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT assembled MMC3_FULL_RUNTIME fixture"]
    fn mmc3_full_renderer_accepts_256_keys_and_traps_the_257th() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        for overflow in [false, true] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            bus.write(0xfffc, 8);
            bus.write(0xc831, 2);
            bus.write(0xcb08, 0xa8);
            bus.write(0xcb09, 0x18);
            bus.write(0xc824, 1);
            for page in 0..2u16 {
                for offset in 0..0x3c0 {
                    bus.write(0x8000 + page * 0x400 + offset, offset as u8);
                }
            }
            for offset in 0..0x100 {
                bus.write(0xc900 + offset, 0xe0);
            }
            for index in 0..8 {
                bus.write(0xc850 + index, index as u8);
            }
            let invoke = |bus: &mut SmsBus, label: &str| {
                let mut cpu = Cpu::new();
                cpu.pc = defs[label].1;
                cpu.sp = 0xdff0;
                bus.write(cpu.sp, 7);
                bus.write(cpu.sp + 1, 0);
                for _ in 0..3_000_000 {
                    if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                        break;
                    }
                    cpu.step(bus).unwrap();
                    assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                }
                assert!(
                    cpu.pc == 7 || bus.read(0xcb1d) != 0,
                    "{label} did not finish"
                );
            };
            invoke(&mut bus, "rt_mmc3_capture_playfield");
            if overflow {
                bus.write(0xc850, 4);
            }
            invoke(&mut bus, "rt_mmc3_capture_hud");
            bus.write(0xc837, 192);
            invoke(&mut bus, "rt_mmc3_frame_render");
            assert_eq!((bus.read(0xc840), bus.read(0xc841)), (0, 1));
            assert_eq!(bus.read(0xcb1d), if overflow { 0xeb } else { 0 });
            if overflow {
                assert_eq!(
                    bus.vdp_regs[1] & 0x40,
                    0,
                    "overflow must remain explicitly blanked"
                );
            }
        }
    }

    #[test]
    #[ignore = "requires TRACE_FUNCTIONAL_PROJECT from mmc3_full_irq_bridge_fixture"]
    fn mmc3_full_irq_bridge_preserves_context_and_pending_order() {
        let path = PathBuf::from(std::env::var("TRACE_FUNCTIONAL_PROJECT").unwrap());
        let path = if path.is_absolute() {
            path
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path)
        };
        let defs = load_wla_symbol_defs(&path.join("sms.sym"));
        let rom = std::fs::read(path.join("sms.sms")).unwrap();
        for control in [0, 8, 12] {
            let mut bus = SmsBus::new(rom.clone(), 0xff);
            for (address, value) in [
                (0xfffe, 17),
                (0xcb14, 17),
                (0xffff, 99),
                (0xfffc, control),
                (0xcb28, 1),
                (0xcb02, 0xfd),
                (0xcb03, 0xa1),
                (0xcb08, 0xa8),
                (0xcb09, 0x18),
                (0xcb76, 0),
                (0xcb77, 0xd3),
                (0xd47d, 0xc0),
                (0xd47e, 0xd4),
                (0xcb15, 0x23),
                (0xcb18, 0x45),
                (0xcb27, 0x67),
            ] {
                bus.write(address, value);
            }
            for (index, page) in [0, 1, 6, 7].into_iter().enumerate() {
                bus.write(0xc819 + index as u16, page);
            }
            let invoke = |bus: &mut SmsBus, nested: bool, request: bool, trap: bool| {
                let original_mapping = (bus.slot_bank, bus.read(0xfffc));
                let original_p = bus.read(0xcb03);
                let mut cpu = Cpu::new();
                cpu.pc = defs["rt_mmc3_sms_interrupt"].1;
                cpu.sp = 0xdff0;
                cpu.a = 0x6d;
                cpu.f = 0x95;
                cpu.set_bc(0x8743);
                cpu.set_de(0x52a9);
                cpu.set_hl(0x67de);
                bus.write(cpu.sp, 7);
                bus.write(cpu.sp + 1, 0);
                bus.frame_int_pending = true;
                bus.write(0xc825, u8::from(request));
                let mut nested_fired = false;
                for _ in 0..3_000_000 {
                    if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
                        break;
                    }
                    if nested && !nested_fired && cpu.iff1 && bus.read(0xc831) == 1 {
                        // Actual nested IM1 context at a guest-handler safe
                        // boundary: host-service testing, not an A12 clock.
                        cpu.sp = cpu.sp.wrapping_sub(1);
                        bus.write(cpu.sp, (cpu.pc >> 8) as u8);
                        cpu.sp = cpu.sp.wrapping_sub(1);
                        bus.write(cpu.sp, cpu.pc as u8);
                        cpu.pc = 0x38;
                        cpu.iff1 = false;
                        cpu.iff2 = false;
                        cpu.ei_pending = 0;
                        bus.frame_int_pending = true;
                        nested_fired = true;
                    }
                    cpu.step(bus).unwrap();
                    assert!(cpu.sp >= NATIVE_STACK_FLOOR);
                }
                if trap {
                    assert_eq!(bus.read(0xcb1d), 0xea);
                    assert_eq!(
                        bus.read(0xc823),
                        1,
                        "stale IRQ must not be silently acknowledged"
                    );
                    return;
                }
                assert_eq!(bus.read(0xcb1d), 0);
                assert_eq!(cpu.pc, 7);
                assert_eq!(
                    (cpu.a, cpu.f, cpu.bc(), cpu.de(), cpu.hl(), cpu.sp),
                    (0x6d, 0x95, 0x8743, 0x52a9, 0x67de, 0xdff2)
                );
                assert_eq!((bus.slot_bank, bus.read(0xfffc)), original_mapping);
                assert_eq!(bus.read(0xcb14), 17);
                assert_eq!(bus.read(0xcb03), original_p);
                assert_eq!(bus.read(0xcb02), 0xfd);
                assert_eq!(
                    [bus.read(0xcb15), bus.read(0xcb18), bus.read(0xcb27)],
                    [0x23, 0x45, 0x67]
                );
                assert_eq!(bus.read(0xc831), 0);
                assert_eq!(bus.read(0xca11), 0);
                assert!(cpu.iff1 && cpu.iff2);
                assert_eq!(nested_fired, nested);
                assert_eq!(bus.read(0xc825), 0, "request must be consumed exactly once");
            };
            let write_mask = |bus: &mut SmsBus, mask: u8| {
                let mut cpu = Cpu::new();
                cpu.pc = defs["rt_ppu_write"].1;
                cpu.sp = 0xdff0;
                cpu.a = mask;
                cpu.b = 1;
                bus.write(cpu.sp, 7);
                bus.write(cpu.sp + 1, 0);
                for _ in 0..3000 {
                    if cpu.pc == 7 {
                        break;
                    }
                    cpu.step(bus).unwrap();
                }
                assert_eq!(cpu.pc, 7);
            };
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (1, 0));
            assert_eq!(bus.read(0xc832), 1);
            invoke(&mut bus, false, false, false); // physical host service only
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (1, 0));
            bus.write(0xc072, 1); // actual NMI handler acknowledges old pending
            bus.write(0xcb03, 0xa5);
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (2, 0));
            assert_eq!((bus.read(0xc823), bus.read(0xc8f8)), (0, 0));
            bus.write(0xc072, 0);
            bus.write(0xcb03, 0xa1);
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (3, 1));
            assert_eq!(bus.read(0xc823), 0); // translated IRQ acknowledged E000
            assert_eq!(bus.read(0xc832), 1); // next translated NMI armed split
            // Main briefly blanks then re-enables before its next wait. The
            // not-yet-asserted raster event is canceled, not delivered using
            // a bank context from half-completed main work.
            write_mask(&mut bus, 0);
            write_mask(&mut bus, 0x18);
            assert_eq!(bus.read(0xc8f6), 1);
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (4, 1));
            assert_eq!(bus.read(0xc8f6), 0);
            bus.write(0xc831, 1); // transient guest-handler writes do not cancel
            write_mask(&mut bus, 0);
            write_mask(&mut bus, 0x18);
            assert_eq!(bus.read(0xc8f6), 0);
            bus.write(0xc831, 0);
            bus.write(0xcb08, 0x28); // current NMI=0, no sticky legacy policy
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (4, 2));
            assert_eq!(bus.read(0xc832), 0);
            write_mask(&mut bus, 0);
            invoke(&mut bus, false, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (4, 2));
            assert_eq!(
                bus.read(0xc833),
                0,
                "completed main off epoch must blank with NMI off"
            );
            assert_eq!(bus.vdp_regs[1] & 0x40, 0);
            write_mask(&mut bus, 0x18);
            bus.write(0xcb08, 0xa8);
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (5, 2));
            bus.write(0xcb03, 0xa5);
            invoke(&mut bus, true, true, false);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (6, 2));
            assert_eq!((bus.read(0xc823), bus.read(0xc8f8)), (1, 1));
            write_mask(&mut bus, 0);
            write_mask(&mut bus, 0x18);
            assert_eq!(
                bus.read(0xc823),
                1,
                "main blanking cannot acknowledge pending IRQ"
            );
            bus.write(0xcb03, 0xa1);
            invoke(&mut bus, false, true, true);
            assert_eq!((bus.read(0xc070), bus.read(0xc071)), (6, 2));
        }
    }
}
