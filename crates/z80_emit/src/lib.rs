//! Z80 instruction encoder: emits machine code bytes and WLA-DX-compatible assembly text.

use std::collections::HashMap;

// ── Patch kinds ──────────────────────────────────────────────────────────────

/// A pending fixup to apply once labels are resolved.
#[derive(Debug)]
enum PatchKind {
    /// 16-bit absolute address, little-endian (jp / call).
    Abs16 { offset: usize },
    /// 8-bit signed relative offset for jr / djnz.
    Rel8 { offset: usize },
}

#[derive(Debug)]
struct Patch {
    /// Label being referenced.
    label: String,
    /// Which section (index) the patch lives in.
    section_idx: usize,
    /// Byte offset *within that section's byte vector* of the placeholder.
    kind: PatchKind,
}

// ── Section ──────────────────────────────────────────────────────────────────

struct Section {
    name: String,
    org: u16,
    bytes: Vec<u8>,
    asm: Vec<String>,
    /// Optional WLA-DX placement: explicit bank + slot. When None the
    /// section is emitted as `superfree` (linker picks any bank, but
    /// section symbols resolve to in-bank offsets — useful only for
    /// data the runtime accesses via bank-switching).
    placement: Option<SectionPlacement>,
}

#[derive(Clone, Copy)]
struct SectionPlacement {
    bank: u8,
    slot: u8,
}

impl Section {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            org: 0,
            bytes: Vec::new(),
            asm: Vec::new(),
            placement: None,
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn current_addr(&self) -> u16 {
        self.org.wrapping_add(self.bytes.len() as u16)
    }

    fn push_byte(&mut self, b: u8) {
        self.bytes.push(b);
    }

    fn push_u16_le(&mut self, v: u16) {
        self.bytes.push(v as u8);
        self.bytes.push((v >> 8) as u8);
    }

    fn push_asm(&mut self, line: impl Into<String>) {
        self.asm.push(line.into());
    }
}

// ── Program ──────────────────────────────────────────────────────────────────

pub struct Program {
    sections: Vec<Section>,
    current: usize,
    labels: HashMap<String, u16>,
    dup_labels: Vec<String>,
    patches: Vec<Patch>,
    label_counter: u64,
    /// Labels referenced only via asm-side text (e.g. `.dw target` /
    /// `.db :target` in far_call/far_jmp). Tracked so the pipeline can
    /// emit unresolved-stub fallbacks for them just like patched labels.
    referenced_labels: std::collections::BTreeSet<String>,
    /// Section index each label was defined in. Used by `far_call` to
    /// downgrade to a plain `call` when the target lives in the same
    /// section (= same bank), avoiding the trampoline overhead.
    label_section: HashMap<String, usize>,
}

impl Default for Program {
    fn default() -> Self {
        Self::new()
    }
}

impl Program {
    pub fn new() -> Self {
        let default_section = Section::new("__default__");
        Self {
            sections: vec![default_section],
            current: 0,
            labels: HashMap::new(),
            dup_labels: Vec::new(),
            patches: Vec::new(),
            label_counter: 0,
            referenced_labels: std::collections::BTreeSet::new(),
            label_section: HashMap::new(),
        }
    }

    /// Switch to (or create) a named section.
    pub fn section(&mut self, name: &str) {
        if let Some(idx) = self.sections.iter().position(|s| s.name == name) {
            self.current = idx;
        } else {
            self.sections.push(Section::new(name));
            self.current = self.sections.len() - 1;
        }
    }

    /// Set explicit WLA-DX bank+slot placement for the current section.
    /// When set, the section is emitted as `force` at the given bank/slot
    /// so its symbols resolve to slot-relative logical addresses (e.g.
    /// $4000+ for slot 1). Otherwise the section is `superfree` and the
    /// linker picks any bank — but symbols inside use in-bank offsets.
    pub fn set_section_placement(&mut self, bank: u8, slot: u8) {
        self.sections[self.current].placement = Some(SectionPlacement { bank, slot });
    }

    /// Set the base address for the current section.
    pub fn org(&mut self, addr: u16) {
        self.sections[self.current].org = addr;
        self.sections[self.current].push_asm(format!(".org ${:04X}", addr));
    }

    pub fn comment(&mut self, text: impl AsRef<str>) {
        let line = format!("; {}", text.as_ref());
        self.sections[self.current].push_asm(line);
    }

    pub fn label(&mut self, name: impl AsRef<str>) {
        let name = name.as_ref().to_string();
        let addr = self.sections[self.current].current_addr();
        if self.labels.contains_key(&name) {
            self.dup_labels.push(name.clone());
        }
        self.labels.insert(name.clone(), addr);
        self.label_section.insert(name.clone(), self.current);
        self.sections[self.current].push_asm(format!("{}:", name));
    }

    /// Snapshot the label→section map (for two-pass lowering: pass 1
    /// dry-lowers to discover section assignments, pass 2 seeds the
    /// real Program via `prepopulate_label_section`).
    pub fn label_section_snapshot(&self) -> HashMap<String, usize> {
        self.label_section.clone()
    }

    /// Index of the section we're currently emitting into. Exposed so
    /// the lower crate can compare a target's section against ours when
    /// deciding whether a branch can use a plain `jp_z`/`jp_nz` instead
    /// of the cross-bank trampoline.
    pub fn current_section_idx(&self) -> usize {
        self.current
    }

    /// Section index a label resolves into, if known. `None` for
    /// forward references whose label hasn't been emitted yet and
    /// wasn't seeded by `prepopulate_label_section`.
    pub fn label_section_idx(&self, name: &str) -> Option<usize> {
        self.label_section.get(name).copied()
    }

    /// Seed `label_section` from an external snapshot. Used by the
    /// pipeline's two-pass lowering so forward references inside the
    /// pass-2 routine know which section their targets land in and
    /// `far_call`/`far_jmp` can downgrade to plain call/jp when both
    /// ends sit in the same section.
    pub fn prepopulate_label_section(&mut self, map: &HashMap<String, usize>) {
        for (name, &sec) in map {
            self.label_section.entry(name.clone()).or_insert(sec);
        }
    }

    pub fn fresh_label(&mut self, prefix: &str) -> String {
        let lbl = format!("_{}_{}", prefix, self.label_counter);
        self.label_counter += 1;
        lbl
    }

    pub fn data(&mut self, label: Option<&str>, bytes: &[u8]) {
        if let Some(lbl) = label {
            self.label(lbl);
        }
        let hex: Vec<String> = bytes.iter().map(|b| format!("${:02X}", b)).collect();
        self.sections[self.current].push_asm(format!("  .db {}", hex.join(",")));
        for &b in bytes {
            self.sections[self.current].push_byte(b);
        }
    }

    pub fn align(&mut self, alignment: usize) {
        let cur_len = self.sections[self.current].len();
        let rem = cur_len % alignment;
        if rem != 0 {
            let pad = alignment - rem;
            self.sections[self.current].push_asm(format!("  .align {}", alignment));
            for _ in 0..pad {
                self.sections[self.current].push_byte(0x00);
            }
        }
    }

    pub fn current_addr(&self) -> u16 {
        self.sections[self.current].current_addr()
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn sec(&mut self) -> &mut Section {
        &mut self.sections[self.current]
    }

    fn emit1(&mut self, b: u8, asm: impl Into<String>) {
        self.sec().push_byte(b);
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
    }

    fn emit_imm8(&mut self, op: u8, n: u8, asm: impl Into<String>) {
        self.sec().push_byte(op);
        self.sec().push_byte(n);
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
    }

    fn emit_imm16(&mut self, op: u8, nn: u16, asm: impl Into<String>) {
        self.sec().push_byte(op);
        self.sec().push_u16_le(nn);
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
    }

    fn emit_cb(&mut self, op: u8, asm: impl Into<String>) {
        self.sec().push_byte(0xCB);
        self.sec().push_byte(op);
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
    }

    /// Emit a 3-byte instruction (op + 16-bit placeholder) with a label patch.
    fn emit_jp_like(&mut self, op: u8, label: &str, asm: impl Into<String>) {
        let section_idx = self.current;
        let offset = self.sections[self.current].len() + 1; // byte index of lo byte
        self.sec().push_byte(op);
        self.sec().push_byte(0x00); // placeholder lo
        self.sec().push_byte(0x00); // placeholder hi
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
        self.patches.push(Patch {
            label: label.to_string(),
            section_idx,
            kind: PatchKind::Abs16 { offset },
        });
    }

    /// Emit a 2-byte relative-jump instruction with a label patch.
    fn emit_jr_like(&mut self, op: u8, label: &str, asm: impl Into<String>) {
        let section_idx = self.current;
        let offset = self.sections[self.current].len() + 1; // byte index of displacement
        self.sec().push_byte(op);
        self.sec().push_byte(0x00); // placeholder
        let line = format!("  {}", asm.into());
        self.sec().push_asm(line);
        self.patches.push(Patch {
            label: label.to_string(),
            section_idx,
            kind: PatchKind::Rel8 { offset },
        });
    }

    // ── 8-bit loads ──────────────────────────────────────────────────────────

    pub fn ld_a_imm(&mut self, value: u8) {
        self.emit_imm8(0x3E, value, format!("ld a,${:02X}", value));
    }
    pub fn ld_b_imm(&mut self, value: u8) {
        self.emit_imm8(0x06, value, format!("ld b,${:02X}", value));
    }
    pub fn ld_c_imm(&mut self, value: u8) {
        self.emit_imm8(0x0E, value, format!("ld c,${:02X}", value));
    }
    pub fn ld_d_imm(&mut self, value: u8) {
        self.emit_imm8(0x16, value, format!("ld d,${:02X}", value));
    }
    pub fn ld_e_imm(&mut self, value: u8) {
        self.emit_imm8(0x1E, value, format!("ld e,${:02X}", value));
    }
    pub fn ld_h_imm(&mut self, value: u8) {
        self.emit_imm8(0x26, value, format!("ld h,${:02X}", value));
    }
    pub fn ld_l_imm(&mut self, value: u8) {
        self.emit_imm8(0x2E, value, format!("ld l,${:02X}", value));
    }

    pub fn ld_a_abs(&mut self, addr: u16) {
        self.emit_imm16(0x3A, addr, format!("ld a,(${:04X})", addr));
    }
    pub fn ld_abs_a(&mut self, addr: u16) {
        self.emit_imm16(0x32, addr, format!("ld (${:04X}),a", addr));
    }

    pub fn ld_a_hl_ptr(&mut self) {
        self.emit1(0x7E, "ld a,(hl)");
    }
    pub fn ld_hl_ptr_a(&mut self) {
        self.emit1(0x77, "ld (hl),a");
    }
    pub fn ld_a_de_ptr(&mut self) {
        self.emit1(0x1A, "ld a,(de)");
    }
    pub fn ld_de_ptr_a(&mut self) {
        self.emit1(0x12, "ld (de),a");
    }
    pub fn ld_a_bc_ptr(&mut self) {
        self.emit1(0x0A, "ld a,(bc)");
    }
    pub fn ld_bc_ptr_a(&mut self) {
        self.emit1(0x02, "ld (bc),a");
    }

    pub fn ld_a_b(&mut self) {
        self.emit1(0x78, "ld a,b");
    }
    pub fn ld_a_c(&mut self) {
        self.emit1(0x79, "ld a,c");
    }
    pub fn ld_a_d(&mut self) {
        self.emit1(0x7A, "ld a,d");
    }
    pub fn ld_a_e(&mut self) {
        self.emit1(0x7B, "ld a,e");
    }
    pub fn ld_a_h(&mut self) {
        self.emit1(0x7C, "ld a,h");
    }
    pub fn ld_a_l(&mut self) {
        self.emit1(0x7D, "ld a,l");
    }

    pub fn ld_b_a(&mut self) {
        self.emit1(0x47, "ld b,a");
    }
    pub fn ld_c_a(&mut self) {
        self.emit1(0x4F, "ld c,a");
    }
    pub fn ld_d_a(&mut self) {
        self.emit1(0x57, "ld d,a");
    }
    pub fn ld_e_a(&mut self) {
        self.emit1(0x5F, "ld e,a");
    }
    pub fn ld_h_a(&mut self) {
        self.emit1(0x67, "ld h,a");
    }
    pub fn ld_l_a(&mut self) {
        self.emit1(0x6F, "ld l,a");
    }

    // ── 16-bit loads ─────────────────────────────────────────────────────────

    pub fn ld_hl_imm(&mut self, value: u16) {
        self.emit_imm16(0x21, value, format!("ld hl,${:04X}", value));
    }
    pub fn ld_bc_imm(&mut self, value: u16) {
        self.emit_imm16(0x01, value, format!("ld bc,${:04X}", value));
    }
    pub fn ld_de_imm(&mut self, value: u16) {
        self.emit_imm16(0x11, value, format!("ld de,${:04X}", value));
    }
    pub fn ld_sp_imm(&mut self, value: u16) {
        self.emit_imm16(0x31, value, format!("ld sp,${:04X}", value));
    }
    /// Block copy (HL)->(DE), BC bytes, ascending. ED B0.
    pub fn ldir(&mut self) {
        self.sec().push_byte(0xED);
        self.sec().push_byte(0xB0);
        self.sec().push_asm("  ldir".to_string());
    }

    pub fn ld_hl_label(&mut self, label: &str) {
        self.emit_jp_like(0x21, label, format!("ld hl,{}", label));
    }
    pub fn ld_bc_label(&mut self, label: &str) {
        self.emit_jp_like(0x01, label, format!("ld bc,{}", label));
    }
    pub fn ld_de_label(&mut self, label: &str) {
        self.emit_jp_like(0x11, label, format!("ld de,{}", label));
    }

    pub fn ld_hl_abs(&mut self, addr: u16) {
        self.emit_imm16(0x2A, addr, format!("ld hl,(${:04X})", addr));
    }
    pub fn ld_abs_hl(&mut self, addr: u16) {
        self.emit_imm16(0x22, addr, format!("ld (${:04X}),hl", addr));
    }

    // ── Arithmetic ───────────────────────────────────────────────────────────

    pub fn add_a_imm(&mut self, value: u8) {
        self.emit_imm8(0xC6, value, format!("add a,${:02X}", value));
    }
    pub fn adc_a_imm(&mut self, value: u8) {
        self.emit_imm8(0xCE, value, format!("adc a,${:02X}", value));
    }
    pub fn sub_imm(&mut self, value: u8) {
        self.emit_imm8(0xD6, value, format!("sub ${:02X}", value));
    }
    pub fn sbc_a_imm(&mut self, value: u8) {
        self.emit_imm8(0xDE, value, format!("sbc a,${:02X}", value));
    }
    pub fn and_imm(&mut self, value: u8) {
        self.emit_imm8(0xE6, value, format!("and ${:02X}", value));
    }
    pub fn or_imm(&mut self, value: u8) {
        self.emit_imm8(0xF6, value, format!("or ${:02X}", value));
    }
    pub fn xor_imm(&mut self, value: u8) {
        self.emit_imm8(0xEE, value, format!("xor ${:02X}", value));
    }
    pub fn cp_imm(&mut self, value: u8) {
        self.emit_imm8(0xFE, value, format!("cp ${:02X}", value));
    }

    pub fn add_a_a(&mut self) {
        self.emit1(0x87, "add a,a");
    }
    pub fn add_a_b(&mut self) {
        self.emit1(0x80, "add a,b");
    }
    pub fn add_a_c(&mut self) {
        self.emit1(0x81, "add a,c");
    }
    pub fn add_a_d(&mut self) {
        self.emit1(0x82, "add a,d");
    }
    pub fn add_a_e(&mut self) {
        self.emit1(0x83, "add a,e");
    }
    pub fn add_a_h(&mut self) {
        self.emit1(0x84, "add a,h");
    }
    pub fn add_a_l(&mut self) {
        self.emit1(0x85, "add a,l");
    }
    pub fn add_a_hl_ptr(&mut self) {
        self.emit1(0x86, "add a,(hl)");
    }

    pub fn adc_a_a(&mut self) {
        self.emit1(0x8F, "adc a,a");
    }
    pub fn adc_a_b(&mut self) {
        self.emit1(0x88, "adc a,b");
    }
    pub fn adc_a_c(&mut self) {
        self.emit1(0x89, "adc a,c");
    }
    pub fn adc_a_d(&mut self) {
        self.emit1(0x8A, "adc a,d");
    }
    pub fn adc_a_e(&mut self) {
        self.emit1(0x8B, "adc a,e");
    }
    pub fn adc_a_h(&mut self) {
        self.emit1(0x8C, "adc a,h");
    }
    pub fn adc_a_l(&mut self) {
        self.emit1(0x8D, "adc a,l");
    }
    pub fn adc_a_hl_ptr(&mut self) {
        self.emit1(0x8E, "adc a,(hl)");
    }

    pub fn sub_a(&mut self) {
        self.emit1(0x97, "sub a");
    }
    pub fn sub_b(&mut self) {
        self.emit1(0x90, "sub b");
    }
    pub fn sub_c(&mut self) {
        self.emit1(0x91, "sub c");
    }
    pub fn sub_d(&mut self) {
        self.emit1(0x92, "sub d");
    }
    pub fn sub_e(&mut self) {
        self.emit1(0x93, "sub e");
    }
    pub fn sub_h(&mut self) {
        self.emit1(0x94, "sub h");
    }
    pub fn sub_l(&mut self) {
        self.emit1(0x95, "sub l");
    }
    pub fn sub_hl_ptr(&mut self) {
        self.emit1(0x96, "sub (hl)");
    }

    pub fn sbc_a_a(&mut self) {
        self.emit1(0x9F, "sbc a,a");
    }
    pub fn sbc_a_b(&mut self) {
        self.emit1(0x98, "sbc a,b");
    }
    pub fn sbc_a_c(&mut self) {
        self.emit1(0x99, "sbc a,c");
    }
    pub fn sbc_a_d(&mut self) {
        self.emit1(0x9A, "sbc a,d");
    }
    pub fn sbc_a_e(&mut self) {
        self.emit1(0x9B, "sbc a,e");
    }
    pub fn sbc_a_h(&mut self) {
        self.emit1(0x9C, "sbc a,h");
    }
    pub fn sbc_a_l(&mut self) {
        self.emit1(0x9D, "sbc a,l");
    }
    pub fn sbc_a_hl_ptr(&mut self) {
        self.emit1(0x9E, "sbc a,(hl)");
    }

    pub fn and_a(&mut self) {
        self.emit1(0xA7, "and a");
    }
    pub fn and_b(&mut self) {
        self.emit1(0xA0, "and b");
    }
    pub fn and_c(&mut self) {
        self.emit1(0xA1, "and c");
    }
    pub fn and_d(&mut self) {
        self.emit1(0xA2, "and d");
    }
    pub fn and_e(&mut self) {
        self.emit1(0xA3, "and e");
    }
    pub fn and_h(&mut self) {
        self.emit1(0xA4, "and h");
    }
    pub fn and_l(&mut self) {
        self.emit1(0xA5, "and l");
    }
    pub fn and_hl_ptr(&mut self) {
        self.emit1(0xA6, "and (hl)");
    }

    pub fn or_a(&mut self) {
        self.emit1(0xB7, "or a");
    }
    pub fn or_b(&mut self) {
        self.emit1(0xB0, "or b");
    }
    pub fn or_c(&mut self) {
        self.emit1(0xB1, "or c");
    }
    pub fn or_d(&mut self) {
        self.emit1(0xB2, "or d");
    }
    pub fn or_e(&mut self) {
        self.emit1(0xB3, "or e");
    }
    pub fn or_h(&mut self) {
        self.emit1(0xB4, "or h");
    }
    pub fn or_l(&mut self) {
        self.emit1(0xB5, "or l");
    }
    pub fn or_hl_ptr(&mut self) {
        self.emit1(0xB6, "or (hl)");
    }

    pub fn xor_a(&mut self) {
        self.emit1(0xAF, "xor a");
    }
    pub fn xor_b(&mut self) {
        self.emit1(0xA8, "xor b");
    }
    pub fn xor_c(&mut self) {
        self.emit1(0xA9, "xor c");
    }
    pub fn xor_d(&mut self) {
        self.emit1(0xAA, "xor d");
    }
    pub fn xor_e(&mut self) {
        self.emit1(0xAB, "xor e");
    }
    pub fn xor_h(&mut self) {
        self.emit1(0xAC, "xor h");
    }
    pub fn xor_l(&mut self) {
        self.emit1(0xAD, "xor l");
    }
    pub fn xor_hl_ptr(&mut self) {
        self.emit1(0xAE, "xor (hl)");
    }

    pub fn cp_a(&mut self) {
        self.emit1(0xBF, "cp a");
    }
    pub fn cp_b(&mut self) {
        self.emit1(0xB8, "cp b");
    }
    pub fn cp_c(&mut self) {
        self.emit1(0xB9, "cp c");
    }
    pub fn cp_d(&mut self) {
        self.emit1(0xBA, "cp d");
    }
    pub fn cp_e(&mut self) {
        self.emit1(0xBB, "cp e");
    }
    pub fn cp_h(&mut self) {
        self.emit1(0xBC, "cp h");
    }
    pub fn cp_l(&mut self) {
        self.emit1(0xBD, "cp l");
    }
    pub fn cp_hl_ptr(&mut self) {
        self.emit1(0xBE, "cp (hl)");
    }

    // ── 16-bit add ────────────────────────────────────────────────────────────

    pub fn add_hl_bc(&mut self) {
        self.emit1(0x09, "add hl,bc");
    }
    pub fn add_hl_de(&mut self) {
        self.emit1(0x19, "add hl,de");
    }
    pub fn add_hl_hl(&mut self) {
        self.emit1(0x29, "add hl,hl");
    }
    pub fn add_hl_sp(&mut self) {
        self.emit1(0x39, "add hl,sp");
    }

    // ── Reg-to-reg loads (non-A targets and sources) ─────────────────────────

    pub fn ld_b_b(&mut self) {
        self.emit1(0x40, "ld b,b");
    }
    pub fn ld_b_c(&mut self) {
        self.emit1(0x41, "ld b,c");
    }
    pub fn ld_b_d(&mut self) {
        self.emit1(0x42, "ld b,d");
    }
    pub fn ld_b_e(&mut self) {
        self.emit1(0x43, "ld b,e");
    }
    pub fn ld_b_h(&mut self) {
        self.emit1(0x44, "ld b,h");
    }
    pub fn ld_b_l(&mut self) {
        self.emit1(0x45, "ld b,l");
    }
    pub fn ld_b_hl_ptr(&mut self) {
        self.emit1(0x46, "ld b,(hl)");
    }

    pub fn ld_c_b(&mut self) {
        self.emit1(0x48, "ld c,b");
    }
    pub fn ld_c_c(&mut self) {
        self.emit1(0x49, "ld c,c");
    }
    pub fn ld_c_d(&mut self) {
        self.emit1(0x4A, "ld c,d");
    }
    pub fn ld_c_e(&mut self) {
        self.emit1(0x4B, "ld c,e");
    }
    pub fn ld_c_h(&mut self) {
        self.emit1(0x4C, "ld c,h");
    }
    pub fn ld_c_l(&mut self) {
        self.emit1(0x4D, "ld c,l");
    }
    pub fn ld_c_hl_ptr(&mut self) {
        self.emit1(0x4E, "ld c,(hl)");
    }

    pub fn ld_d_b(&mut self) {
        self.emit1(0x50, "ld d,b");
    }
    pub fn ld_d_c(&mut self) {
        self.emit1(0x51, "ld d,c");
    }
    pub fn ld_d_d(&mut self) {
        self.emit1(0x52, "ld d,d");
    }
    pub fn ld_d_e(&mut self) {
        self.emit1(0x53, "ld d,e");
    }
    pub fn ld_d_h(&mut self) {
        self.emit1(0x54, "ld d,h");
    }
    pub fn ld_d_l(&mut self) {
        self.emit1(0x55, "ld d,l");
    }
    pub fn ld_d_hl_ptr(&mut self) {
        self.emit1(0x56, "ld d,(hl)");
    }

    pub fn ld_e_b(&mut self) {
        self.emit1(0x58, "ld e,b");
    }
    pub fn ld_e_c(&mut self) {
        self.emit1(0x59, "ld e,c");
    }
    pub fn ld_e_d(&mut self) {
        self.emit1(0x5A, "ld e,d");
    }
    pub fn ld_e_e(&mut self) {
        self.emit1(0x5B, "ld e,e");
    }
    pub fn ld_e_h(&mut self) {
        self.emit1(0x5C, "ld e,h");
    }
    pub fn ld_e_l(&mut self) {
        self.emit1(0x5D, "ld e,l");
    }
    pub fn ld_e_hl_ptr(&mut self) {
        self.emit1(0x5E, "ld e,(hl)");
    }

    pub fn ld_h_b(&mut self) {
        self.emit1(0x60, "ld h,b");
    }
    pub fn ld_h_c(&mut self) {
        self.emit1(0x61, "ld h,c");
    }
    pub fn ld_h_d(&mut self) {
        self.emit1(0x62, "ld h,d");
    }
    pub fn ld_h_e(&mut self) {
        self.emit1(0x63, "ld h,e");
    }
    pub fn ld_h_h(&mut self) {
        self.emit1(0x64, "ld h,h");
    }
    pub fn ld_h_l(&mut self) {
        self.emit1(0x65, "ld h,l");
    }
    pub fn ld_h_hl_ptr(&mut self) {
        self.emit1(0x66, "ld h,(hl)");
    }

    pub fn ld_l_b(&mut self) {
        self.emit1(0x68, "ld l,b");
    }
    pub fn ld_l_c(&mut self) {
        self.emit1(0x69, "ld l,c");
    }
    pub fn ld_l_d(&mut self) {
        self.emit1(0x6A, "ld l,d");
    }
    pub fn ld_l_e(&mut self) {
        self.emit1(0x6B, "ld l,e");
    }
    pub fn ld_l_h(&mut self) {
        self.emit1(0x6C, "ld l,h");
    }
    pub fn ld_l_l(&mut self) {
        self.emit1(0x6D, "ld l,l");
    }
    pub fn ld_l_hl_ptr(&mut self) {
        self.emit1(0x6E, "ld l,(hl)");
    }

    pub fn ld_hl_ptr_b(&mut self) {
        self.emit1(0x70, "ld (hl),b");
    }
    pub fn ld_hl_ptr_c(&mut self) {
        self.emit1(0x71, "ld (hl),c");
    }
    pub fn ld_hl_ptr_d(&mut self) {
        self.emit1(0x72, "ld (hl),d");
    }
    pub fn ld_hl_ptr_e(&mut self) {
        self.emit1(0x73, "ld (hl),e");
    }
    pub fn ld_hl_ptr_h(&mut self) {
        self.emit1(0x74, "ld (hl),h");
    }
    pub fn ld_hl_ptr_l(&mut self) {
        self.emit1(0x75, "ld (hl),l");
    }

    pub fn ld_de_abs(&mut self, addr: u16) {
        // ED 5B nn nn — load DE from (nn).
        self.sec().push_byte(0xED);
        self.sec().push_byte(0x5B);
        self.sec().push_u16_le(addr);
        let line = format!("  ld de,(${:04X})", addr);
        self.sec().push_asm(line);
    }
    pub fn ld_abs_de(&mut self, addr: u16) {
        // ED 53 nn nn — store DE to (nn).
        self.sec().push_byte(0xED);
        self.sec().push_byte(0x53);
        self.sec().push_u16_le(addr);
        let line = format!("  ld (${:04X}),de", addr);
        self.sec().push_asm(line);
    }

    pub fn inc_a(&mut self) {
        self.emit1(0x3C, "inc a");
    }
    pub fn dec_a(&mut self) {
        self.emit1(0x3D, "dec a");
    }
    pub fn inc_b(&mut self) {
        self.emit1(0x04, "inc b");
    }
    pub fn dec_b(&mut self) {
        self.emit1(0x05, "dec b");
    }
    pub fn inc_c(&mut self) {
        self.emit1(0x0C, "inc c");
    }
    pub fn dec_c(&mut self) {
        self.emit1(0x0D, "dec c");
    }
    pub fn inc_d(&mut self) {
        self.emit1(0x14, "inc d");
    }
    pub fn dec_d(&mut self) {
        self.emit1(0x15, "dec d");
    }
    pub fn inc_e(&mut self) {
        self.emit1(0x1C, "inc e");
    }
    pub fn dec_e(&mut self) {
        self.emit1(0x1D, "dec e");
    }
    pub fn inc_h(&mut self) {
        self.emit1(0x24, "inc h");
    }
    pub fn dec_h(&mut self) {
        self.emit1(0x25, "dec h");
    }
    pub fn inc_l(&mut self) {
        self.emit1(0x2C, "inc l");
    }
    pub fn dec_l(&mut self) {
        self.emit1(0x2D, "dec l");
    }
    pub fn inc_hl_ptr(&mut self) {
        self.emit1(0x34, "inc (hl)");
    }
    pub fn dec_hl_ptr(&mut self) {
        self.emit1(0x35, "dec (hl)");
    }

    pub fn inc_hl(&mut self) {
        self.emit1(0x23, "inc hl");
    }
    pub fn dec_hl(&mut self) {
        self.emit1(0x2B, "dec hl");
    }
    pub fn inc_bc(&mut self) {
        self.emit1(0x03, "inc bc");
    }
    pub fn dec_bc(&mut self) {
        self.emit1(0x0B, "dec bc");
    }
    pub fn inc_de(&mut self) {
        self.emit1(0x13, "inc de");
    }
    pub fn dec_de(&mut self) {
        self.emit1(0x1B, "dec de");
    }

    // ── Shifts and rotates ────────────────────────────────────────────────────

    pub fn rlca(&mut self) {
        self.emit1(0x07, "rlca");
    }
    pub fn rrca(&mut self) {
        self.emit1(0x0F, "rrca");
    }
    pub fn rla(&mut self) {
        self.emit1(0x17, "rla");
    }
    pub fn rra(&mut self) {
        self.emit1(0x1F, "rra");
    }

    pub fn sla_a(&mut self) {
        self.emit_cb(0x27, "sla a");
    }
    pub fn srl_a(&mut self) {
        self.emit_cb(0x3F, "srl a");
    }
    pub fn rl_a(&mut self) {
        self.emit_cb(0x17, "rl a");
    }
    pub fn rr_a(&mut self) {
        self.emit_cb(0x1F, "rr a");
    }

    /// `bit n,(hl)` — test bit n of memory at (HL). Sets Z if bit clear.
    /// Does NOT touch A. n must be 0..=7.
    pub fn bit_n_hl_ptr(&mut self, bit: u8) {
        assert!(bit < 8, "bit must be 0..=7");
        let op = 0x46 | (bit << 3);
        self.emit_cb(op, format!("bit {bit},(hl)"));
    }
    /// `set n,(hl)`.
    pub fn set_n_hl_ptr(&mut self, bit: u8) {
        assert!(bit < 8);
        let op = 0xC6 | (bit << 3);
        self.emit_cb(op, format!("set {bit},(hl)"));
    }
    /// `res n,(hl)`.
    pub fn res_n_hl_ptr(&mut self, bit: u8) {
        assert!(bit < 8);
        let op = 0x86 | (bit << 3);
        self.emit_cb(op, format!("res {bit},(hl)"));
    }

    // ── Flags ─────────────────────────────────────────────────────────────────

    pub fn scf(&mut self) {
        self.emit1(0x37, "scf");
    }
    pub fn ccf(&mut self) {
        self.emit1(0x3F, "ccf");
    }
    pub fn cpl(&mut self) {
        self.emit1(0x2F, "cpl");
    }

    // ── Control ───────────────────────────────────────────────────────────────

    pub fn nop(&mut self) {
        self.emit1(0x00, "nop");
    }
    pub fn halt(&mut self) {
        self.emit1(0x76, "halt");
    }
    pub fn di(&mut self) {
        self.emit1(0xF3, "di");
    }
    pub fn ei(&mut self) {
        self.emit1(0xFB, "ei");
    }

    // ── Jumps / calls ─────────────────────────────────────────────────────────

    pub fn jp(&mut self, label: &str) {
        self.emit_jp_like(0xC3, label, format!("jp {}", label));
    }
    pub fn jp_z(&mut self, label: &str) {
        self.emit_jp_like(0xCA, label, format!("jp z,{}", label));
    }
    pub fn jp_nz(&mut self, label: &str) {
        self.emit_jp_like(0xC2, label, format!("jp nz,{}", label));
    }
    pub fn jp_c(&mut self, label: &str) {
        self.emit_jp_like(0xDA, label, format!("jp c,{}", label));
    }
    pub fn jp_nc(&mut self, label: &str) {
        self.emit_jp_like(0xD2, label, format!("jp nc,{}", label));
    }
    pub fn jp_m(&mut self, label: &str) {
        self.emit_jp_like(0xFA, label, format!("jp m,{}", label));
    }
    pub fn jp_p(&mut self, label: &str) {
        self.emit_jp_like(0xF2, label, format!("jp p,{}", label));
    }
    pub fn jp_pe(&mut self, label: &str) {
        // Parity Even / Overflow set
        self.emit_jp_like(0xEA, label, format!("jp pe,{}", label));
    }
    pub fn jp_po(&mut self, label: &str) {
        // Parity Odd / Overflow clear
        self.emit_jp_like(0xE2, label, format!("jp po,{}", label));
    }

    pub fn jr(&mut self, label: &str) {
        self.emit_jr_like(0x18, label, format!("jr {}", label));
    }
    pub fn jr_z(&mut self, label: &str) {
        self.emit_jr_like(0x28, label, format!("jr z,{}", label));
    }
    pub fn jr_nz(&mut self, label: &str) {
        self.emit_jr_like(0x20, label, format!("jr nz,{}", label));
    }
    pub fn jr_c(&mut self, label: &str) {
        self.emit_jr_like(0x38, label, format!("jr c,{}", label));
    }
    pub fn jr_nc(&mut self, label: &str) {
        self.emit_jr_like(0x30, label, format!("jr nc,{}", label));
    }
    pub fn djnz(&mut self, label: &str) {
        self.emit_jr_like(0x10, label, format!("djnz {}", label));
    }

    pub fn call(&mut self, label: &str) {
        self.emit_jp_like(0xCD, label, format!("call {}", label));
    }

    /// Bank-aware cross-section call. Emits:
    ///   call rt_far_call
    ///   .dw <label>      ; logical slot-1 address
    ///   .db :<label>     ; bank number (resolved by WLA-DX at link)
    ///
    /// Used for translated `JSR L_XXXX` so the call works regardless of
    /// which bank the target lives in. The 3 placeholder bytes after the
    /// `call` are filled by WLA-DX during assembly. Our Rust-side `bytes`
    /// tracking pushes $00 placeholders to keep address arithmetic
    /// consistent — the final ROM comes from WLA-DX, which sees the
    /// correct `.dw`/`.db` and emits the real values.
    ///
    /// Optimisation: if the target was already defined in the current
    /// section, we emit a plain `call` — same-section means same bank
    /// at runtime, so no bank switch is needed.
    pub fn far_call(&mut self, label: &str) {
        if self.label_section.get(label) == Some(&self.current) {
            self.call(label);
            return;
        }
        self.emit_far_gate(label, false);
    }

    /// Compact far dispatch (H2): target address and bank are immediates,
    /// transferred through the slot-0 rt_far_gate shim (switching $FFFE
    /// from slot-1 code would swap the executing bank under the PC — the
    /// gate must run from slot 0). Saves the trampoline's inline data
    /// block decode.
    fn emit_far_gate(&mut self, label: &str, jump: bool) {
        self.emit_bytes_asm(&[0x32, 0x15, 0xCB], "  ld ($cb15),a");
        // ld de, TARGET — 16-bit label immediate. Text-only (binary bytes
        // stay zero): cross-section transfers are never executed in the
        // validation emulator, and a binary patch would hard-error on
        // targets only WLA can resolve (profile stubs, other sections).
        self.sec().push_byte(0x11);
        self.sec().push_byte(0x00);
        self.sec().push_byte(0x00);
        self.sec().push_asm(format!("  ld de,{label}"));
        // ld a, :TARGET — bank immediate (binary placeholder; validation
        // never executes cross-section transfers).
        self.emit_bytes_asm(&[0x3E, 0x00], &format!("  ld a,:{label}"));
        if jump {
            self.emit_bytes_asm(&[0xC3, 0x00, 0x00], "  jp rt_far_gate");
        } else {
            self.emit_bytes_asm(&[0xCD, 0x00, 0x00], "  call rt_far_gate");
        }
        self.referenced_labels.insert(label.to_string());
    }

    /// Raw byte+asm emission helper for composite sequences.
    fn emit_bytes_asm(&mut self, bytes: &[u8], asm: &str) {
        for &b in bytes {
            self.sec().push_byte(b);
        }
        self.sec().push_asm(asm.to_string());
    }

    /// Bank-aware cross-section JMP. Same encoding as far_call but
    /// dispatches via `rt_far_jmp` which switches slot 1, jumps, and
    /// restores the previous bank when the target returns. Downgrades to plain `jp` for
    /// same-section targets.
    pub fn far_jmp(&mut self, label: &str) {
        if self.label_section.get(label) == Some(&self.current) {
            self.jp(label);
            return;
        }
        self.emit_far_gate(label, true);
    }

    fn emit_far(&mut self, target: &str, dispatcher: &str) {
        let section_idx = self.current;
        let offset = self.sections[self.current].len() + 1;
        self.sec().push_byte(0xCD);
        self.sec().push_byte(0x00);
        self.sec().push_byte(0x00);
        self.sec()
            .push_asm(format!("  call {} ; → {}", dispatcher, target));
        self.patches.push(Patch {
            label: dispatcher.to_string(),
            section_idx,
            kind: PatchKind::Abs16 { offset },
        });
        self.sec().push_byte(0x00);
        self.sec().push_byte(0x00);
        self.sec().push_byte(0x00);
        self.sec().push_asm(format!("  .dw {}", target));
        self.sec().push_asm(format!("  .db :{}", target));
        // Track the target so unresolved_labels() picks it up if no
        // routine defines it (e.g., profile-only names like
        // PrimaryGameSetup that aren't backed by a lift).
        self.referenced_labels.insert(target.to_string());
    }
    pub fn call_z(&mut self, label: &str) {
        self.emit_jp_like(0xCC, label, format!("call z,{}", label));
    }
    pub fn call_nz(&mut self, label: &str) {
        self.emit_jp_like(0xC4, label, format!("call nz,{}", label));
    }

    pub fn ret(&mut self) {
        self.emit1(0xC9, "ret");
    }
    pub fn ret_z(&mut self) {
        self.emit1(0xC8, "ret z");
    }
    pub fn ret_nz(&mut self) {
        self.emit1(0xC0, "ret nz");
    }
    pub fn ret_c(&mut self) {
        self.emit1(0xD8, "ret c");
    }
    pub fn ret_nc(&mut self) {
        self.emit1(0xD0, "ret nc");
    }

    // ── Stack ─────────────────────────────────────────────────────────────────

    pub fn push_af(&mut self) {
        self.emit1(0xF5, "push af");
    }
    pub fn pop_af(&mut self) {
        self.emit1(0xF1, "pop af");
    }
    pub fn push_bc(&mut self) {
        self.emit1(0xC5, "push bc");
    }
    pub fn pop_bc(&mut self) {
        self.emit1(0xC1, "pop bc");
    }
    pub fn push_de(&mut self) {
        self.emit1(0xD5, "push de");
    }
    pub fn pop_de(&mut self) {
        self.emit1(0xD1, "pop de");
    }
    pub fn push_hl(&mut self) {
        self.emit1(0xE5, "push hl");
    }
    pub fn pop_hl(&mut self) {
        self.emit1(0xE1, "pop hl");
    }

    // ── I/O ───────────────────────────────────────────────────────────────────

    pub fn in_a(&mut self, port: u8) {
        self.emit_imm8(0xDB, port, format!("in a,(${:02X})", port));
    }
    pub fn out_a(&mut self, port: u8) {
        self.emit_imm8(0xD3, port, format!("out (${:02X}),a", port));
    }

    // ── Bit ops (CB prefix) on A ──────────────────────────────────────────────

    pub fn bit_a(&mut self, bit: u8) {
        assert!(bit < 8, "bit index must be 0..7");
        let op = 0x47 | (bit << 3);
        self.emit_cb(op, format!("bit {},a", bit));
    }
    pub fn set_a(&mut self, bit: u8) {
        assert!(bit < 8, "bit index must be 0..7");
        let op = 0xC7 | (bit << 3);
        self.emit_cb(op, format!("set {},a", bit));
    }
    pub fn res_a(&mut self, bit: u8) {
        assert!(bit < 8, "bit index must be 0..7");
        let op = 0x87 | (bit << 3);
        self.emit_cb(op, format!("res {},a", bit));
    }

    // ── finish ────────────────────────────────────────────────────────────────

    /// Return the set of label names referenced by patches but not (yet)
    /// defined by any `label()` call. Useful for the cli to discover
    /// what stubs it still needs to emit.
    pub fn unresolved_labels(&self) -> Vec<String> {
        let mut missing: std::collections::BTreeSet<String> = Default::default();
        for patch in &self.patches {
            if !self.labels.contains_key(&patch.label) {
                missing.insert(patch.label.clone());
            }
        }
        for label in &self.referenced_labels {
            if !self.labels.contains_key(label) {
                missing.insert(label.clone());
            }
        }
        missing.into_iter().collect()
    }

    pub fn finish(mut self) -> Result<Build, EmitError> {
        if let Some(name) = self.dup_labels.into_iter().next() {
            return Err(EmitError::DuplicateLabel(name));
        }

        // Apply patches.
        for patch in &self.patches {
            let target_addr = self
                .labels
                .get(&patch.label)
                .copied()
                .ok_or_else(|| EmitError::UnresolvedLabel(patch.label.clone()))?;

            match &patch.kind {
                PatchKind::Abs16 { offset } => {
                    let sec = &mut self.sections[patch.section_idx];
                    sec.bytes[*offset] = target_addr as u8;
                    sec.bytes[*offset + 1] = (target_addr >> 8) as u8;
                }
                PatchKind::Rel8 { offset } => {
                    let sec = &mut self.sections[patch.section_idx];
                    // The relative displacement is calculated from the byte *after* the
                    // displacement byte (i.e., PC = org + offset + 1).
                    let instr_end_addr = sec.org as i32 + *offset as i32 + 1;
                    let delta = target_addr as i32 - instr_end_addr;
                    if delta < -128 || delta > 127 {
                        return Err(EmitError::JrOutOfRange {
                            label: patch.label.clone(),
                            delta,
                        });
                    }
                    sec.bytes[*offset] = delta as i8 as u8;
                }
            }
        }

        // Build assembly listing.
        let mut asm = String::new();
        for sec in &self.sections {
            if sec.name == "__default__" && sec.bytes.is_empty() && sec.asm.is_empty() {
                continue;
            }
            // Section header: explicit `.bank N slot S` + `force` if the
            // caller pinned the placement, else `superfree`. Pinned
            // placement is needed when the section's labels must resolve
            // to specific logical addresses (e.g. translated code in
            // slot 1 → addresses $4000-$7FFF). `superfree` leaves
            // placement to the linker but means labels inside resolve
            // to in-bank offsets, not logical addresses.
            if let Some(p) = sec.placement {
                // `.bank N slot S` sets the slot context; `free` lets
                // WLA-DX place the section anywhere in the bank that
                // fits (without forcing a specific origin). Symbols
                // inside resolve to slot-relative logical addresses.
                asm.push_str(&format!(".bank {} slot {}\n", p.bank, p.slot));
                asm.push_str(&format!(".section \"{}\" free\n", sec.name));
            } else {
                asm.push_str(&format!(".section \"{}\" superfree\n", sec.name));
            }
            for line in &sec.asm {
                asm.push_str(line);
                asm.push('\n');
            }
            asm.push_str(".ends\n");
        }

        // Concatenate bytes and build SectionOut.
        let mut all_bytes = Vec::new();
        let mut sections_out = Vec::new();
        for sec in self.sections {
            if sec.name == "__default__" && sec.bytes.is_empty() {
                continue;
            }
            all_bytes.extend_from_slice(&sec.bytes);
            sections_out.push(SectionOut {
                name: sec.name,
                org: sec.org,
                bytes: sec.bytes,
            });
        }

        Ok(Build {
            bytes: all_bytes,
            sections: sections_out,
            asm,
        })
    }
}

// ── Public output types ───────────────────────────────────────────────────────

pub struct Build {
    pub bytes: Vec<u8>,
    pub sections: Vec<SectionOut>,
    pub asm: String,
}

pub struct SectionOut {
    pub name: String,
    pub org: u16,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub enum EmitError {
    UnresolvedLabel(String),
    JrOutOfRange { label: String, delta: i32 },
    DuplicateLabel(String),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── opcode bytes ─────────────────────────────────────────────────────────

    #[test]
    fn test_nop_halt_di_ei() {
        let mut p = Program::new();
        p.nop();
        p.halt();
        p.di();
        p.ei();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x00, 0x76, 0xF3, 0xFB]);
    }

    #[test]
    fn test_ld_r_imm() {
        let mut p = Program::new();
        p.ld_a_imm(0x42);
        p.ld_b_imm(0x01);
        p.ld_c_imm(0x02);
        p.ld_d_imm(0x03);
        p.ld_e_imm(0x04);
        p.ld_h_imm(0x05);
        p.ld_l_imm(0x06);
        let b = p.finish().unwrap();
        assert_eq!(
            b.bytes,
            &[
                0x3E, 0x42, 0x06, 0x01, 0x0E, 0x02, 0x16, 0x03, 0x1E, 0x04, 0x26, 0x05, 0x2E, 0x06
            ]
        );
    }

    #[test]
    fn test_ld_a_abs_and_store() {
        let mut p = Program::new();
        p.ld_a_abs(0xC000);
        p.ld_abs_a(0xC001);
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x3A, 0x00, 0xC0, 0x32, 0x01, 0xC0]);
    }

    #[test]
    fn test_ld_ptr_ops() {
        let mut p = Program::new();
        p.ld_a_hl_ptr();
        p.ld_hl_ptr_a();
        p.ld_a_de_ptr();
        p.ld_de_ptr_a();
        p.ld_a_bc_ptr();
        p.ld_bc_ptr_a();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x7E, 0x77, 0x1A, 0x12, 0x0A, 0x02]);
    }

    #[test]
    fn test_ld_reg_to_a() {
        let mut p = Program::new();
        p.ld_a_b();
        p.ld_a_c();
        p.ld_a_d();
        p.ld_a_e();
        p.ld_a_h();
        p.ld_a_l();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D]);
    }

    #[test]
    fn test_ld_a_to_reg() {
        let mut p = Program::new();
        p.ld_b_a();
        p.ld_c_a();
        p.ld_d_a();
        p.ld_e_a();
        p.ld_h_a();
        p.ld_l_a();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x47, 0x4F, 0x57, 0x5F, 0x67, 0x6F]);
    }

    #[test]
    fn test_ld_16bit_imm() {
        let mut p = Program::new();
        p.ld_hl_imm(0x1234);
        p.ld_bc_imm(0xABCD);
        p.ld_de_imm(0x0000);
        p.ld_sp_imm(0xDFFE);
        let b = p.finish().unwrap();
        assert_eq!(
            b.bytes,
            &[
                0x21, 0x34, 0x12, 0x01, 0xCD, 0xAB, 0x11, 0x00, 0x00, 0x31, 0xFE, 0xDF
            ]
        );
    }

    #[test]
    fn test_ld_hl_abs_and_store() {
        let mut p = Program::new();
        p.ld_hl_abs(0x8000);
        p.ld_abs_hl(0x8002);
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x2A, 0x00, 0x80, 0x22, 0x02, 0x80]);
    }

    #[test]
    fn test_arithmetic_imm() {
        let mut p = Program::new();
        p.add_a_imm(0x01);
        p.adc_a_imm(0x02);
        p.sub_imm(0x03);
        p.sbc_a_imm(0x04);
        p.and_imm(0x0F);
        p.or_imm(0x10);
        p.xor_imm(0xFF);
        p.cp_imm(0x00);
        let b = p.finish().unwrap();
        assert_eq!(
            b.bytes,
            &[
                0xC6, 0x01, 0xCE, 0x02, 0xD6, 0x03, 0xDE, 0x04, 0xE6, 0x0F, 0xF6, 0x10, 0xEE, 0xFF,
                0xFE, 0x00
            ]
        );
    }

    #[test]
    fn test_add_reg() {
        let mut p = Program::new();
        p.add_a_a();
        p.add_a_b();
        p.add_a_c();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x87, 0x80, 0x81]);
    }

    #[test]
    fn test_logical_a() {
        let mut p = Program::new();
        p.and_a();
        p.or_a();
        p.xor_a();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0xA7, 0xB7, 0xAF]);
    }

    #[test]
    fn test_inc_dec() {
        let mut p = Program::new();
        p.inc_a();
        p.dec_a();
        p.inc_b();
        p.dec_b();
        p.inc_c();
        p.dec_c();
        p.inc_hl();
        p.dec_hl();
        p.inc_bc();
        p.dec_bc();
        p.inc_de();
        p.dec_de();
        let b = p.finish().unwrap();
        assert_eq!(
            b.bytes,
            &[
                0x3C, 0x3D, 0x04, 0x05, 0x0C, 0x0D, 0x23, 0x2B, 0x03, 0x0B, 0x13, 0x1B
            ]
        );
    }

    #[test]
    fn test_flags_misc() {
        let mut p = Program::new();
        p.scf();
        p.ccf();
        p.cpl();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x37, 0x3F, 0x2F]);
    }

    #[test]
    fn test_rotates() {
        let mut p = Program::new();
        p.rlca();
        p.rrca();
        p.rla();
        p.rra();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x07, 0x0F, 0x17, 0x1F]);
    }

    #[test]
    fn test_cb_shifts() {
        let mut p = Program::new();
        p.sla_a();
        p.srl_a();
        p.rl_a();
        p.rr_a();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0xCB, 0x27, 0xCB, 0x3F, 0xCB, 0x17, 0xCB, 0x1F]);
    }

    #[test]
    fn test_bit_ops() {
        let mut p = Program::new();
        p.bit_a(0);
        p.bit_a(7);
        p.set_a(3);
        p.res_a(5);
        let b = p.finish().unwrap();
        // bit 0,a = CB 47; bit 7,a = CB 7F; set 3,a = CB DF; res 5,a = CB AF
        assert_eq!(b.bytes, &[0xCB, 0x47, 0xCB, 0x7F, 0xCB, 0xDF, 0xCB, 0xAF]);
    }

    #[test]
    fn test_stack() {
        let mut p = Program::new();
        p.push_af();
        p.pop_af();
        p.push_bc();
        p.pop_bc();
        p.push_de();
        p.pop_de();
        p.push_hl();
        p.pop_hl();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0xF5, 0xF1, 0xC5, 0xC1, 0xD5, 0xD1, 0xE5, 0xE1]);
    }

    #[test]
    fn test_io() {
        let mut p = Program::new();
        p.in_a(0xBE);
        p.out_a(0xBF);
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0xDB, 0xBE, 0xD3, 0xBF]);
    }

    #[test]
    fn test_ret_variants() {
        let mut p = Program::new();
        p.ret();
        p.ret_z();
        p.ret_nz();
        p.ret_c();
        p.ret_nc();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0xC9, 0xC8, 0xC0, 0xD8, 0xD0]);
    }

    // ── label patching ────────────────────────────────────────────────────────

    #[test]
    fn test_jp_forward_label() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.jp("target");
        p.nop();
        p.label("target");
        p.nop();
        let b = p.finish().unwrap();
        // jp = C3 04 00 (target is at offset 4 = org 0 + 4 bytes)
        assert_eq!(&b.bytes[0..3], &[0xC3, 0x04, 0x00]);
    }

    #[test]
    fn test_jp_backward_label() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x1000);
        p.label("loop");
        p.nop();
        p.jp("loop");
        let b = p.finish().unwrap();
        // loop is at 0x1000; jp placeholder at offset 1; patched with 0x1000
        assert_eq!(&b.bytes[1..4], &[0xC3, 0x00, 0x10]);
    }

    #[test]
    fn test_call_label() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.call("sub");
        p.ret();
        p.label("sub");
        p.ret();
        let b = p.finish().unwrap();
        // call = CD 04 00 (sub at offset 4)
        assert_eq!(&b.bytes[0..3], &[0xCD, 0x04, 0x00]);
    }

    #[test]
    fn test_jr_forward() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        // jr to skip_nop; jr is 2 bytes, then nop (1), then skip_nop label
        p.jr("skip");
        p.nop(); // 1 byte
        p.label("skip");
        p.nop();
        let b = p.finish().unwrap();
        // jr: offset byte is at byte[1]. PC after jr = 0+2 = 2. target = 3. delta = 1.
        assert_eq!(b.bytes[0], 0x18);
        assert_eq!(b.bytes[1], 0x01_u8);
    }

    #[test]
    fn test_jr_backward() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0010);
        p.label("back");
        p.nop(); // offset 0
        p.nop(); // offset 1
        p.jr("back"); // offset 2; jr at bytes 2,3; PC after = 0x0014; target = 0x0010; delta = -4
        let b = p.finish().unwrap();
        assert_eq!(b.bytes[2], 0x18);
        assert_eq!(b.bytes[3], (-4_i8) as u8);
    }

    #[test]
    fn test_jr_out_of_range() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.jr("far");
        // emit 200 nops to push target out of range
        for _ in 0..200 {
            p.nop();
        }
        p.label("far");
        let result = p.finish();
        assert!(matches!(result, Err(EmitError::JrOutOfRange { .. })));
    }

    #[test]
    fn test_djnz() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.label("loop_start");
        p.nop();
        p.djnz("loop_start");
        let b = p.finish().unwrap();
        // djnz at offset 1; displacement byte at offset 2; PC after = 3; target = 0; delta = -3
        assert_eq!(b.bytes[1], 0x10);
        assert_eq!(b.bytes[2], (-3_i8) as u8);
    }

    #[test]
    fn test_unresolved_label() {
        let mut p = Program::new();
        p.jp("nowhere");
        let result = p.finish();
        assert!(matches!(result, Err(EmitError::UnresolvedLabel(s)) if s == "nowhere"));
    }

    #[test]
    fn test_duplicate_label() {
        let mut p = Program::new();
        p.section("test");
        p.label("foo");
        p.nop();
        p.label("foo");
        p.nop();
        let result = p.finish();
        assert!(matches!(result, Err(EmitError::DuplicateLabel(s)) if s == "foo"));
    }

    // ── multi-section ─────────────────────────────────────────────────────────

    #[test]
    fn test_multi_section_bytes_concat() {
        let mut p = Program::new();
        p.section("a");
        p.org(0x0000);
        p.nop();
        p.section("b");
        p.org(0x8000);
        p.halt();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes, &[0x00, 0x76]);
        assert_eq!(b.sections.len(), 2);
        assert_eq!(b.sections[0].name, "a");
        assert_eq!(b.sections[1].org, 0x8000);
    }

    #[test]
    fn test_cross_section_label() {
        let mut p = Program::new();
        p.section("boot");
        p.org(0x0000);
        p.jp("main");
        p.section("main_sec");
        p.org(0x0100);
        p.label("main");
        p.nop();
        let b = p.finish().unwrap();
        // jp should be patched with 0x0100
        assert_eq!(&b.sections[0].bytes[0..3], &[0xC3, 0x00, 0x01]);
    }

    // ── assembly listing ──────────────────────────────────────────────────────

    #[test]
    fn test_asm_listing_basic() {
        let mut p = Program::new();
        p.section("boot");
        p.org(0x0000);
        p.label("boot_entry");
        p.di();
        p.ld_sp_imm(0xDFFE);
        p.jp("main");
        p.section("code");
        p.org(0x0100);
        p.label("main");
        p.nop();
        let b = p.finish().unwrap();

        assert!(b.asm.contains(".section \"boot\" superfree"));
        assert!(b.asm.contains(".org $0000"));
        assert!(b.asm.contains("boot_entry:"));
        assert!(b.asm.contains("  di"));
        assert!(b.asm.contains("  ld sp,$DFFE"));
        assert!(b.asm.contains("  jp main"));
        assert!(b.asm.contains(".ends"));
        assert!(b.asm.contains(".section \"code\" superfree"));
        assert!(b.asm.contains("main:"));
    }

    #[test]
    fn test_asm_listing_section_order() {
        let mut p = Program::new();
        p.section("first");
        p.nop();
        p.section("second");
        p.halt();
        let b = p.finish().unwrap();
        let first_pos = b.asm.find("\"first\"").unwrap();
        let second_pos = b.asm.find("\"second\"").unwrap();
        assert!(first_pos < second_pos);
    }

    #[test]
    fn test_comment_in_asm() {
        let mut p = Program::new();
        p.section("test");
        p.comment("initialize stack");
        p.nop();
        let b = p.finish().unwrap();
        assert!(b.asm.contains("; initialize stack"));
    }

    #[test]
    fn test_data_bytes() {
        let mut p = Program::new();
        p.section("data");
        p.data(Some("my_table"), &[0x01, 0x02, 0x03]);
        let b = p.finish().unwrap();
        assert_eq!(&b.bytes, &[0x01, 0x02, 0x03]);
        assert!(b.asm.contains("my_table:"));
        assert!(b.asm.contains(".db $01,$02,$03"));
    }

    #[test]
    fn test_fresh_label_unique() {
        let mut p = Program::new();
        let l1 = p.fresh_label("loop");
        let l2 = p.fresh_label("loop");
        assert_ne!(l1, l2);
    }

    #[test]
    fn test_current_addr_tracks() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0100);
        assert_eq!(p.current_addr(), 0x0100);
        p.nop();
        assert_eq!(p.current_addr(), 0x0101);
        p.ld_hl_imm(0x0000);
        assert_eq!(p.current_addr(), 0x0104);
    }

    #[test]
    fn test_jp_variants() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        // Emit all jp variants pointing to same target
        p.jp_z("t");
        p.jp_nz("t");
        p.jp_c("t");
        p.jp_nc("t");
        p.jp_m("t");
        p.jp_p("t");
        p.label("t");
        p.nop();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes[0], 0xCA); // jp z
        assert_eq!(b.bytes[3], 0xC2); // jp nz
        assert_eq!(b.bytes[6], 0xDA); // jp c
        assert_eq!(b.bytes[9], 0xD2); // jp nc
        assert_eq!(b.bytes[12], 0xFA); // jp m
        assert_eq!(b.bytes[15], 0xF2); // jp p
        // target = 18 (0x12)
        assert_eq!(b.bytes[1], 0x12);
        assert_eq!(b.bytes[2], 0x00);
    }

    #[test]
    fn test_jr_variants() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.jr_z("t");
        p.jr_nz("t");
        p.jr_c("t");
        p.jr_nc("t");
        p.label("t");
        p.nop();
        let b = p.finish().unwrap();
        assert_eq!(b.bytes[0], 0x28); // jr z
        assert_eq!(b.bytes[2], 0x20); // jr nz
        assert_eq!(b.bytes[4], 0x38); // jr c
        assert_eq!(b.bytes[6], 0x30); // jr nc
        // Each jr is 2 bytes; target at offset 8.
        // jr_z offset byte at [1]: PC after = 2, target = 8, delta = 6
        assert_eq!(b.bytes[1], 6);
        // jr_nz offset byte at [3]: PC after = 4, target = 8, delta = 4
        assert_eq!(b.bytes[3], 4);
        // jr_c offset byte at [5]: PC after = 6, target = 8, delta = 2
        assert_eq!(b.bytes[5], 2);
        // jr_nc offset byte at [7]: PC after = 8, target = 8, delta = 0
        assert_eq!(b.bytes[7], 0);
    }

    #[test]
    fn test_ld_label_16bit() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.ld_hl_label("data");
        p.ld_bc_label("data");
        p.ld_de_label("data");
        p.label("data");
        p.nop();
        let b = p.finish().unwrap();
        let target = 0x0009_u16; // 3*3 bytes = 9
        assert_eq!(b.bytes[0], 0x21);
        assert_eq!(b.bytes[1], target as u8);
        assert_eq!(b.bytes[2], (target >> 8) as u8);
        assert_eq!(b.bytes[3], 0x01);
        assert_eq!(b.bytes[6], 0x11);
    }

    #[test]
    fn test_align() {
        let mut p = Program::new();
        p.section("test");
        p.org(0x0000);
        p.nop(); // 1 byte
        p.align(4);
        p.nop();
        let b = p.finish().unwrap();
        // after align(4): 3 padding bytes, then nop
        assert_eq!(b.bytes.len(), 5);
        assert_eq!(&b.bytes[1..4], &[0x00, 0x00, 0x00]);
        assert_eq!(b.bytes[4], 0x00);
    }
}
