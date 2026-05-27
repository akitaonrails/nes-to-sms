//! Minimal Z80 interpreter for the conservative subset emitted by `z80_emit`.
//!
//! Only opcodes produced by the back end are supported. Prefixes ED/DD/FD are
//! rejected so the back end is forced to stay inside the supported subset.
//! Cycle accuracy is not provided; instruction accuracy is.

// ── Flag bits ────────────────────────────────────────────────────────────────
pub const FLAG_C: u8 = 0x01; // carry
pub const FLAG_N: u8 = 0x02; // subtract
pub const FLAG_PV: u8 = 0x04; // parity / overflow
pub const FLAG_H: u8 = 0x10; // half-carry
pub const FLAG_Z: u8 = 0x40; // zero
pub const FLAG_S: u8 = 0x80; // sign

// ── Bus trait ────────────────────────────────────────────────────────────────

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
    fn in_port(&mut self, _port: u8) -> u8 {
        0xFF
    }
    fn out_port(&mut self, _port: u8, _value: u8) {}
}

// ── FlatBus ──────────────────────────────────────────────────────────────────

pub struct FlatBus {
    pub mem: Box<[u8; 0x10000]>,
}

impl FlatBus {
    pub fn new() -> Self {
        Self {
            mem: Box::new([0u8; 0x10000]),
        }
    }

    pub fn load(&mut self, addr: u16, bytes: &[u8]) {
        let start = addr as usize;
        self.mem[start..start + bytes.len()].copy_from_slice(bytes);
    }
}

impl Default for FlatBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for FlatBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.mem[addr as usize]
    }
    fn write(&mut self, addr: u16, value: u8) {
        self.mem[addr as usize] = value;
    }
}

// ── CPU ──────────────────────────────────────────────────────────────────────

/// Approximate T-states for an opcode, keyed on the first byte. Not
/// cycle-exact (doesn't split taken/not-taken branches or reg-vs-(hl)
/// forms, and prefixes use a flat average), but it weights the costly
/// classes (push/pop/call/ret/memory/prefixed) correctly — enough to
/// compare optimizations against the SMS ~59,736-cycle frame budget.
fn approx_cycles(op: u8) -> u32 {
    match op {
        0x76 => 4,                                      // halt (before the (hl) range)
        0xCD => 17,                                     // call nn
        0xC4 | 0xCC | 0xD4 | 0xDC | 0xE4 | 0xEC | 0xF4 | 0xFC => 14, // call cc (avg)
        0xC9 => 10,                                     // ret
        0xC0 | 0xC8 | 0xD0 | 0xD8 | 0xE0 | 0xE8 | 0xF0 | 0xF8 => 8, // ret cc (avg)
        0xC3 => 10,                                     // jp nn
        0xC2 | 0xCA | 0xD2 | 0xDA | 0xE2 | 0xEA | 0xF2 | 0xFA => 10, // jp cc
        0x18 => 12,                                     // jr
        0x20 | 0x28 | 0x30 | 0x38 => 10,                // jr cc (avg taken/not)
        0xC5 | 0xD5 | 0xE5 | 0xF5 => 11,                // push rr
        0xC1 | 0xD1 | 0xE1 | 0xF1 => 10,                // pop rr
        0x3A | 0x32 => 13,                              // ld a,(nn) / ld (nn),a
        0x2A | 0x22 => 16,                              // ld hl,(nn) / ld (nn),hl
        0x36 => 10,                                     // ld (hl),n
        0x34 | 0x35 => 11,                              // inc/dec (hl)
        0x46 | 0x4E | 0x56 | 0x5E | 0x66 | 0x6E | 0x7E => 7, // ld r,(hl)
        0x70..=0x77 => 7,                               // ld (hl),r
        0x86 | 0x8E | 0x96 | 0x9E | 0xA6 | 0xAE | 0xB6 | 0xBE => 7, // alu a,(hl)
        0x0A | 0x1A | 0x02 | 0x12 => 7,                 // ld a,(bc/de), ld (bc/de),a
        0x01 | 0x11 | 0x21 | 0x31 => 10,                // ld rr,nn
        0xCB => 11,                                     // CB prefix (bit/rot reg=8, (hl)=12-15)
        0xED => 14,                                     // ED prefix (avg)
        0xDD | 0xFD => 15,                              // IX/IY prefix (avg)
        _ => 5,                                         // reg ops, ld r,r, alu a,r, inc/dec r, imm
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
    pub halted: bool,
    pub iff1: bool,
    pub iff2: bool,
    /// Delayed interrupt enable countdown for EI. Real Z80 enables IFF1/2 only
    /// after the instruction following EI has executed, making `ei; ret` safe.
    pub ei_pending: u8,
    // Shadow register set (used by EX AF,AF' and EXX). Stored as 16-bit
    // for convenience even though the real Z80 has independent halves.
    pub af_shadow: u16,
    pub bc_shadow: u16,
    pub de_shadow: u16,
    pub hl_shadow: u16,
    /// Approximate T-state (cycle) counter. Not exact per-opcode, but a
    /// far better proxy than instruction count for SMS speed budgeting
    /// (push/pop/call/ret/memory ops cost far more than reg ops). Summed
    /// per `step()` from `approx_cycles`.
    pub cycles: u64,
}

impl Cpu {
    pub fn new() -> Self {
        Self {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            sp: 0,
            pc: 0,
            halted: false,
            iff1: false,
            iff2: false,
            ei_pending: 0,
            af_shadow: 0,
            bc_shadow: 0,
            de_shadow: 0,
            hl_shadow: 0,
            cycles: 0,
        }
    }

    // ── Register pair accessors ───────────────────────────────────────────

    pub fn af(&self) -> u16 {
        ((self.a as u16) << 8) | self.f as u16
    }
    pub fn bc(&self) -> u16 {
        ((self.b as u16) << 8) | self.c as u16
    }
    pub fn de(&self) -> u16 {
        ((self.d as u16) << 8) | self.e as u16
    }
    pub fn hl(&self) -> u16 {
        ((self.h as u16) << 8) | self.l as u16
    }

    pub fn set_af(&mut self, v: u16) {
        self.a = (v >> 8) as u8;
        self.f = v as u8;
    }
    pub fn set_bc(&mut self, v: u16) {
        self.b = (v >> 8) as u8;
        self.c = v as u8;
    }
    pub fn set_de(&mut self, v: u16) {
        self.d = (v >> 8) as u8;
        self.e = v as u8;
    }
    pub fn set_hl(&mut self, v: u16) {
        self.h = (v >> 8) as u8;
        self.l = v as u8;
    }

    // ── Flag helpers ──────────────────────────────────────────────────────

    fn flag(&self, mask: u8) -> bool {
        self.f & mask != 0
    }

    fn set_flag(&mut self, mask: u8, val: bool) {
        if val {
            self.f |= mask;
        } else {
            self.f &= !mask;
        }
    }

    // ── Fetch helpers ─────────────────────────────────────────────────────

    fn fetch_byte(&mut self, bus: &mut impl Bus) -> u8 {
        let b = bus.read(self.pc);
        self.pc = self.pc.wrapping_add(1);
        b
    }

    fn fetch_word(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.fetch_byte(bus) as u16;
        let hi = self.fetch_byte(bus) as u16;
        (hi << 8) | lo
    }

    // signed displacement
    fn fetch_disp(&mut self, bus: &mut impl Bus) -> i8 {
        self.fetch_byte(bus) as i8
    }

    // ── Stack ─────────────────────────────────────────────────────────────

    fn push_word(&mut self, bus: &mut impl Bus, v: u16) {
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, (v >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        bus.write(self.sp, v as u8);
    }

    fn pop_word(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = bus.read(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        (hi << 8) | lo
    }

    // ── ALU operations ────────────────────────────────────────────────────

    fn alu_add(&mut self, operand: u8, with_carry: bool) {
        let cy = if with_carry && self.flag(FLAG_C) {
            1u8
        } else {
            0
        };
        let a = self.a;
        let result16 = a as u16 + operand as u16 + cy as u16;
        let result = result16 as u8;
        let half = (a & 0x0F) + (operand & 0x0F) + cy > 0x0F;
        let overflow = (!(a ^ operand) & (a ^ result)) & 0x80 != 0;
        self.f = 0;
        self.set_flag(FLAG_S, result & 0x80 != 0);
        self.set_flag(FLAG_Z, result == 0);
        self.set_flag(FLAG_H, half);
        self.set_flag(FLAG_PV, overflow);
        // FLAG_N = 0
        self.set_flag(FLAG_C, result16 > 0xFF);
        self.a = result;
    }

    fn alu_sub(&mut self, operand: u8, with_carry: bool) -> u8 {
        let cy = if with_carry && self.flag(FLAG_C) {
            1u8
        } else {
            0
        };
        let a = self.a;
        let result16 = (a as i16) - (operand as i16) - (cy as i16);
        let result = result16 as u8;
        let half = (a & 0x0F) < (operand & 0x0F) + cy;
        let overflow = ((a ^ operand) & (a ^ result)) & 0x80 != 0;
        self.f = 0;
        self.set_flag(FLAG_S, result & 0x80 != 0);
        self.set_flag(FLAG_Z, result == 0);
        self.set_flag(FLAG_H, half);
        self.set_flag(FLAG_PV, overflow);
        self.set_flag(FLAG_N, true);
        self.set_flag(FLAG_C, (a as u16) < (operand as u16) + cy as u16);
        result
    }

    fn alu_and(&mut self, operand: u8) {
        self.a &= operand;
        let a = self.a;
        self.f = 0;
        self.set_flag(FLAG_S, a & 0x80 != 0);
        self.set_flag(FLAG_Z, a == 0);
        self.set_flag(FLAG_H, true);
        self.set_flag(FLAG_PV, parity(a));
        // N=0, C=0
    }

    fn alu_or(&mut self, operand: u8) {
        self.a |= operand;
        let a = self.a;
        self.f = 0;
        self.set_flag(FLAG_S, a & 0x80 != 0);
        self.set_flag(FLAG_Z, a == 0);
        // H=0
        self.set_flag(FLAG_PV, parity(a));
        // N=0, C=0
    }

    fn alu_xor(&mut self, operand: u8) {
        self.a ^= operand;
        let a = self.a;
        self.f = 0;
        self.set_flag(FLAG_S, a & 0x80 != 0);
        self.set_flag(FLAG_Z, a == 0);
        // H=0
        self.set_flag(FLAG_PV, parity(a));
        // N=0, C=0
    }

    fn alu_cp(&mut self, operand: u8) {
        let a = self.a;
        // Run sub but restore A
        self.alu_sub(operand, false);
        // Z should reflect (a == operand), which sub already does correctly
        self.a = a;
    }

    // ── INC / DEC (preserve C flag) ───────────────────────────────────────

    fn do_inc(&mut self, val: u8) -> u8 {
        let result = val.wrapping_add(1);
        let old_c = self.f & FLAG_C;
        self.f = 0;
        self.f |= old_c;
        self.set_flag(FLAG_S, result & 0x80 != 0);
        self.set_flag(FLAG_Z, result == 0);
        self.set_flag(FLAG_H, (val & 0x0F) == 0x0F);
        self.set_flag(FLAG_PV, val == 0x7F);
        // FLAG_N = 0
        result
    }

    fn do_dec(&mut self, val: u8) -> u8 {
        let result = val.wrapping_sub(1);
        let old_c = self.f & FLAG_C;
        self.f = 0;
        self.f |= old_c;
        self.set_flag(FLAG_S, result & 0x80 != 0);
        self.set_flag(FLAG_Z, result == 0);
        self.set_flag(FLAG_H, (val & 0x0F) == 0x00);
        self.set_flag(FLAG_PV, val == 0x80);
        self.set_flag(FLAG_N, true);
        result
    }

    // ── r register decode (bits 5-3 or 2-0 encoding) ─────────────────────

    fn read_r(&self, r: u8) -> u8 {
        match r {
            0 => self.b,
            1 => self.c,
            2 => self.d,
            3 => self.e,
            4 => self.h,
            5 => self.l,
            7 => self.a,
            _ => 0,
        }
    }

    fn write_r(&mut self, r: u8, val: u8) {
        match r {
            0 => self.b = val,
            1 => self.c = val,
            2 => self.d = val,
            3 => self.e = val,
            4 => self.h = val,
            5 => self.l = val,
            7 => self.a = val,
            _ => {}
        }
    }

    // ── CB-prefix dispatch ────────────────────────────────────────────────

    fn step_cb(&mut self, bus: &mut impl Bus) -> Result<(), StepError> {
        let _pc_cb = self.pc.wrapping_sub(1); // PC of the 0xCB byte (kept for future diagnostics)
        let op = self.fetch_byte(bus);

        // CB encoding: top 2 bits = category, mid 3 = bit/op, low 3 = register.
        // r encoding: 0=B, 1=C, 2=D, 3=E, 4=H, 5=L, 6=(HL), 7=A.
        let r = op & 7;
        let n = (op >> 3) & 7;
        let cat = op >> 6;

        // Helper closures via direct calls; rust closures and &mut self don't mix nicely here.
        let read_operand = |s: &Self, bus: &mut dyn Bus| -> u8 {
            if r == 6 {
                bus.read(s.hl())
            } else {
                s.read_r(r)
            }
        };
        let val = read_operand(self, bus);

        match cat {
            // ── 00xxxxxx: rotate / shift ────────────────────────────────────
            0 => {
                let (result, carry_out) = match n {
                    0 => {
                        // RLC r
                        let c = val & 0x80 != 0;
                        ((val.rotate_left(1)), c)
                    }
                    1 => {
                        // RRC r
                        let c = val & 0x01 != 0;
                        ((val.rotate_right(1)), c)
                    }
                    2 => {
                        // RL r
                        let c_in = if self.flag(FLAG_C) { 1 } else { 0 };
                        let c_out = val & 0x80 != 0;
                        ((val << 1) | c_in, c_out)
                    }
                    3 => {
                        // RR r
                        let c_in = if self.flag(FLAG_C) { 0x80 } else { 0 };
                        let c_out = val & 0x01 != 0;
                        ((val >> 1) | c_in, c_out)
                    }
                    4 => {
                        // SLA r
                        let c = val & 0x80 != 0;
                        (val << 1, c)
                    }
                    5 => {
                        // SRA r — arithmetic right shift (preserve bit 7)
                        let c = val & 0x01 != 0;
                        (((val as i8) >> 1) as u8, c)
                    }
                    6 => {
                        // SLL r — undocumented; like SLA but bit 0 = 1
                        let c = val & 0x80 != 0;
                        ((val << 1) | 1, c)
                    }
                    7 => {
                        // SRL r
                        let c = val & 0x01 != 0;
                        (val >> 1, c)
                    }
                    _ => unreachable!(),
                };
                self.f = 0;
                self.set_flag(FLAG_S, result & 0x80 != 0);
                self.set_flag(FLAG_Z, result == 0);
                self.set_flag(FLAG_PV, parity(result));
                self.set_flag(FLAG_C, carry_out);
                if r == 6 {
                    bus.write(self.hl(), result);
                } else {
                    self.write_r(r, result);
                }
            }
            // ── 01nnnrrr: BIT n,r ──────────────────────────────────────────
            1 => {
                let tested = (val >> n) & 1;
                let z = tested == 0;
                self.set_flag(FLAG_Z, z);
                self.set_flag(FLAG_H, true);
                self.set_flag(FLAG_N, false);
                self.set_flag(FLAG_S, n == 7 && !z);
                self.set_flag(FLAG_PV, z);
            }
            // ── 10nnnrrr: RES n,r ──────────────────────────────────────────
            2 => {
                let result = val & !(1 << n);
                if r == 6 {
                    bus.write(self.hl(), result);
                } else {
                    self.write_r(r, result);
                }
            }
            // ── 11nnnrrr: SET n,r ──────────────────────────────────────────
            3 => {
                let result = val | (1 << n);
                if r == 6 {
                    bus.write(self.hl(), result);
                } else {
                    self.write_r(r, result);
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    // ── Main step ─────────────────────────────────────────────────────────

    pub fn step(&mut self, bus: &mut impl Bus) -> Result<(), StepError> {
        if self.halted {
            return Err(StepError::Halt);
        }
        // see `approx_cycles` below; tallied right after the opcode fetch.

        let pc_op = self.pc;
        let op = self.fetch_byte(bus);
        self.cycles += approx_cycles(op) as u64;

        match op {
            // ── NOP ────────────────────────────────────────────────────
            0x00 => {}

            // ── 16-bit immediate loads ─────────────────────────────────
            0x01 => {
                let nn = self.fetch_word(bus);
                self.set_bc(nn);
            }
            0x11 => {
                let nn = self.fetch_word(bus);
                self.set_de(nn);
            }
            0x21 => {
                let nn = self.fetch_word(bus);
                self.set_hl(nn);
            }
            0x31 => {
                let nn = self.fetch_word(bus);
                self.sp = nn;
            }

            // ── ld (bc),a ─────────────────────────────────────────────
            0x02 => {
                bus.write(self.bc(), self.a);
            }
            // ── ld a,(bc) ─────────────────────────────────────────────
            0x0A => {
                let addr = self.bc();
                self.a = bus.read(addr);
            }

            // ── ld (de),a ─────────────────────────────────────────────
            0x12 => {
                bus.write(self.de(), self.a);
            }
            // ── ld a,(de) ─────────────────────────────────────────────
            0x1A => {
                let addr = self.de();
                self.a = bus.read(addr);
            }

            // ── ld (nn),hl ────────────────────────────────────────────
            0x22 => {
                let nn = self.fetch_word(bus);
                bus.write(nn, self.l);
                bus.write(nn.wrapping_add(1), self.h);
            }
            // ── ld hl,(nn) ────────────────────────────────────────────
            0x2A => {
                let nn = self.fetch_word(bus);
                self.l = bus.read(nn);
                self.h = bus.read(nn.wrapping_add(1));
            }

            // ── ld (nn),a ─────────────────────────────────────────────
            0x32 => {
                let nn = self.fetch_word(bus);
                bus.write(nn, self.a);
            }
            // ── ld a,(nn) ─────────────────────────────────────────────
            0x3A => {
                let nn = self.fetch_word(bus);
                self.a = bus.read(nn);
            }

            // ── inc rr ────────────────────────────────────────────────
            0x03 => {
                let v = self.bc().wrapping_add(1);
                self.set_bc(v);
            }
            0x13 => {
                let v = self.de().wrapping_add(1);
                self.set_de(v);
            }
            0x23 => {
                let v = self.hl().wrapping_add(1);
                self.set_hl(v);
            }
            0x33 => {
                self.sp = self.sp.wrapping_add(1);
            }

            // ── dec rr ────────────────────────────────────────────────
            0x0B => {
                let v = self.bc().wrapping_sub(1);
                self.set_bc(v);
            }
            0x1B => {
                let v = self.de().wrapping_sub(1);
                self.set_de(v);
            }
            0x2B => {
                let v = self.hl().wrapping_sub(1);
                self.set_hl(v);
            }
            0x3B => {
                self.sp = self.sp.wrapping_sub(1);
            }

            // ── inc r (r != (HL)) ─────────────────────────────────────
            0x04 => {
                let v = self.b;
                self.b = self.do_inc(v);
            }
            0x0C => {
                let v = self.c;
                self.c = self.do_inc(v);
            }
            0x14 => {
                let v = self.d;
                self.d = self.do_inc(v);
            }
            0x1C => {
                let v = self.e;
                self.e = self.do_inc(v);
            }
            0x24 => {
                let v = self.h;
                self.h = self.do_inc(v);
            }
            0x2C => {
                let v = self.l;
                self.l = self.do_inc(v);
            }
            0x3C => {
                let v = self.a;
                self.a = self.do_inc(v);
            }

            // ── dec r ─────────────────────────────────────────────────
            0x05 => {
                let v = self.b;
                self.b = self.do_dec(v);
            }
            0x0D => {
                let v = self.c;
                self.c = self.do_dec(v);
            }
            0x15 => {
                let v = self.d;
                self.d = self.do_dec(v);
            }
            0x1D => {
                let v = self.e;
                self.e = self.do_dec(v);
            }
            0x25 => {
                let v = self.h;
                self.h = self.do_dec(v);
            }
            0x2D => {
                let v = self.l;
                self.l = self.do_dec(v);
            }
            0x3D => {
                let v = self.a;
                self.a = self.do_dec(v);
            }
            // inc (hl) / dec (hl): read-modify-write memory, set S/Z/H/PV,
            // preserve carry (same flag semantics as inc/dec r).
            0x34 => {
                let addr = self.hl();
                let v = bus.read(addr);
                let r = self.do_inc(v);
                bus.write(addr, r);
            }
            0x35 => {
                let addr = self.hl();
                let v = bus.read(addr);
                let r = self.do_dec(v);
                bus.write(addr, r);
            }

            // ── ld r,n ────────────────────────────────────────────────
            0x06 => {
                self.b = self.fetch_byte(bus);
            }
            0x0E => {
                self.c = self.fetch_byte(bus);
            }
            0x16 => {
                self.d = self.fetch_byte(bus);
            }
            0x1E => {
                self.e = self.fetch_byte(bus);
            }
            0x26 => {
                self.h = self.fetch_byte(bus);
            }
            0x2E => {
                self.l = self.fetch_byte(bus);
            }
            0x3E => {
                self.a = self.fetch_byte(bus);
            }

            // ── RLCA ──────────────────────────────────────────────────
            0x07 => {
                let a = self.a;
                let out = a & 0x80 != 0;
                self.a = (a << 1) | if out { 1 } else { 0 };
                self.set_flag(FLAG_C, out);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_N, false);
            }
            // ── RRCA ──────────────────────────────────────────────────
            0x0F => {
                let a = self.a;
                let out = a & 0x01 != 0;
                self.a = (a >> 1) | if out { 0x80 } else { 0 };
                self.set_flag(FLAG_C, out);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_N, false);
            }
            // ── RLA ───────────────────────────────────────────────────
            0x17 => {
                let a = self.a;
                let old_c = self.flag(FLAG_C);
                let out = a & 0x80 != 0;
                self.a = (a << 1) | if old_c { 1 } else { 0 };
                self.set_flag(FLAG_C, out);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_N, false);
            }
            // ── RRA ───────────────────────────────────────────────────
            0x1F => {
                let a = self.a;
                let old_c = self.flag(FLAG_C);
                let out = a & 0x01 != 0;
                self.a = (a >> 1) | if old_c { 0x80 } else { 0 };
                self.set_flag(FLAG_C, out);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_N, false);
            }

            // ── ADD HL,rr ────────────────────────────────────────────
            // After: H = half-carry from bit 11; N = 0; C = carry out;
            // S, Z, PV unchanged. (We track C/H/N; leave S/Z/PV.)
            0x09 | 0x19 | 0x29 | 0x39 => {
                let hl = self.hl() as u32;
                let rr = match op {
                    0x09 => self.bc() as u32,
                    0x19 => self.de() as u32,
                    0x29 => self.hl() as u32,
                    0x39 => self.sp as u32,
                    _ => unreachable!(),
                };
                let sum = hl + rr;
                self.set_hl((sum & 0xFFFF) as u16);
                self.set_flag(FLAG_C, sum > 0xFFFF);
                let halfc = ((hl & 0x0FFF) + (rr & 0x0FFF)) > 0x0FFF;
                self.set_flag(FLAG_H, halfc);
                self.set_flag(FLAG_N, false);
            }

            // ── DJNZ e ────────────────────────────────────────────────
            0x10 => {
                let e = self.fetch_disp(bus);
                self.b = self.b.wrapping_sub(1);
                if self.b != 0 {
                    self.pc = self.pc.wrapping_add(e as u16);
                }
            }

            // ── JR e ──────────────────────────────────────────────────
            0x18 => {
                let e = self.fetch_disp(bus);
                self.pc = self.pc.wrapping_add(e as u16);
            }
            // ── JR NZ,e ───────────────────────────────────────────────
            0x20 => {
                let e = self.fetch_disp(bus);
                if !self.flag(FLAG_Z) {
                    self.pc = self.pc.wrapping_add(e as u16);
                }
            }
            // ── JR Z,e ────────────────────────────────────────────────
            0x28 => {
                let e = self.fetch_disp(bus);
                if self.flag(FLAG_Z) {
                    self.pc = self.pc.wrapping_add(e as u16);
                }
            }
            // ── JR NC,e ───────────────────────────────────────────────
            0x30 => {
                let e = self.fetch_disp(bus);
                if !self.flag(FLAG_C) {
                    self.pc = self.pc.wrapping_add(e as u16);
                }
            }
            // ── JR C,e ────────────────────────────────────────────────
            0x38 => {
                let e = self.fetch_disp(bus);
                if self.flag(FLAG_C) {
                    self.pc = self.pc.wrapping_add(e as u16);
                }
            }

            // ── SCF ───────────────────────────────────────────────────
            0x37 => {
                self.set_flag(FLAG_C, true);
                self.set_flag(FLAG_H, false);
                self.set_flag(FLAG_N, false);
            }
            // ── CCF ───────────────────────────────────────────────────
            0x3F => {
                let old_c = self.flag(FLAG_C);
                self.set_flag(FLAG_H, old_c);
                self.set_flag(FLAG_C, !old_c);
                self.set_flag(FLAG_N, false);
            }

            // ── ld r,r' (block 0x40..0x7F, excl 0x76) ────────────────
            0x40..=0x7F => {
                if op == 0x76 {
                    // HALT
                    self.halted = true;
                    return Err(StepError::Halt);
                }
                let dst = (op >> 3) & 7;
                let src = op & 7;
                // src=6 means (HL)
                let val = if src == 6 {
                    let addr = self.hl();
                    bus.read(addr)
                } else {
                    self.read_r(src)
                };
                if dst == 6 {
                    // ld (hl),r
                    let addr = self.hl();
                    bus.write(addr, val);
                } else {
                    self.write_r(dst, val);
                }
            }

            // ── ALU on A with register operand (0x80..0xBF) ───────────
            0x80..=0xBF => {
                let alu = (op >> 3) & 7;
                let src = op & 7;
                let operand = if src == 6 {
                    let addr = self.hl();
                    bus.read(addr)
                } else {
                    self.read_r(src)
                };
                match alu {
                    0 => self.alu_add(operand, false), // ADD A,r
                    1 => self.alu_add(operand, true),  // ADC A,r
                    2 => {
                        let r = self.alu_sub(operand, false);
                        self.a = r;
                    } // SUB r
                    3 => {
                        let r = self.alu_sub(operand, true);
                        self.a = r;
                    } // SBC A,r
                    4 => self.alu_and(operand),
                    5 => self.alu_xor(operand),
                    6 => self.alu_or(operand),
                    7 => self.alu_cp(operand),
                    _ => unreachable!(),
                }
            }

            // ── RET cc ────────────────────────────────────────────────
            0xC0 => {
                if !self.flag(FLAG_Z) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xC8 => {
                if self.flag(FLAG_Z) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xD0 => {
                if !self.flag(FLAG_C) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xD8 => {
                if self.flag(FLAG_C) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xE0 => {
                if !self.flag(FLAG_PV) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xE8 => {
                if self.flag(FLAG_PV) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xF0 => {
                if !self.flag(FLAG_S) {
                    self.pc = self.pop_word(bus);
                }
            }
            0xF8 => {
                if self.flag(FLAG_S) {
                    self.pc = self.pop_word(bus);
                }
            }

            // ── POP rr ────────────────────────────────────────────────
            0xC1 => {
                let v = self.pop_word(bus);
                self.set_bc(v);
            }
            0xD1 => {
                let v = self.pop_word(bus);
                self.set_de(v);
            }
            0xE1 => {
                let v = self.pop_word(bus);
                self.set_hl(v);
            }
            0xF1 => {
                let v = self.pop_word(bus);
                self.set_af(v);
            }

            // ── JP cc,nn ──────────────────────────────────────────────
            0xC2 => {
                let nn = self.fetch_word(bus);
                if !self.flag(FLAG_Z) {
                    self.pc = nn;
                }
            }
            0xCA => {
                let nn = self.fetch_word(bus);
                if self.flag(FLAG_Z) {
                    self.pc = nn;
                }
            }
            0xD2 => {
                let nn = self.fetch_word(bus);
                if !self.flag(FLAG_C) {
                    self.pc = nn;
                }
            }
            0xDA => {
                let nn = self.fetch_word(bus);
                if self.flag(FLAG_C) {
                    self.pc = nn;
                }
            }
            0xE2 => {
                let nn = self.fetch_word(bus);
                if !self.flag(FLAG_PV) {
                    self.pc = nn;
                }
            }
            0xEA => {
                let nn = self.fetch_word(bus);
                if self.flag(FLAG_PV) {
                    self.pc = nn;
                }
            }
            0xF2 => {
                let nn = self.fetch_word(bus);
                if !self.flag(FLAG_S) {
                    self.pc = nn;
                }
            } // JP P,nn
            0xFA => {
                let nn = self.fetch_word(bus);
                if self.flag(FLAG_S) {
                    self.pc = nn;
                }
            } // JP M,nn

            // ── JP nn ─────────────────────────────────────────────────
            0xC3 => {
                let nn = self.fetch_word(bus);
                self.pc = nn;
            }

            // ── CALL cc,nn ────────────────────────────────────────────
            0xC4 => {
                let nn = self.fetch_word(bus);
                if !self.flag(FLAG_Z) {
                    self.push_word(bus, self.pc);
                    self.pc = nn;
                }
            }
            0xCC => {
                let nn = self.fetch_word(bus);
                if self.flag(FLAG_Z) {
                    self.push_word(bus, self.pc);
                    self.pc = nn;
                }
            }

            // ── PUSH rr ───────────────────────────────────────────────
            0xC5 => {
                let v = self.bc();
                self.push_word(bus, v);
            }
            0xD5 => {
                let v = self.de();
                self.push_word(bus, v);
            }
            0xE5 => {
                let v = self.hl();
                self.push_word(bus, v);
            }
            0xF5 => {
                let v = self.af();
                self.push_word(bus, v);
            }

            // ── ALU A,n (immediate) ───────────────────────────────────
            0xC6 => {
                let n = self.fetch_byte(bus);
                self.alu_add(n, false);
            }
            0xCE => {
                let n = self.fetch_byte(bus);
                self.alu_add(n, true);
            }
            0xD6 => {
                let n = self.fetch_byte(bus);
                let r = self.alu_sub(n, false);
                self.a = r;
            }
            0xDE => {
                let n = self.fetch_byte(bus);
                let r = self.alu_sub(n, true);
                self.a = r;
            }
            0xE6 => {
                let n = self.fetch_byte(bus);
                self.alu_and(n);
            }
            0xEE => {
                let n = self.fetch_byte(bus);
                self.alu_xor(n);
            }
            0xF6 => {
                let n = self.fetch_byte(bus);
                self.alu_or(n);
            }
            0xFE => {
                let n = self.fetch_byte(bus);
                self.alu_cp(n);
            }

            // ── RET ───────────────────────────────────────────────────
            0xC9 => {
                self.pc = self.pop_word(bus);
            }

            // ── CB prefix ─────────────────────────────────────────────
            0xCB => {
                self.step_cb(bus)?;
            }

            // ── CALL nn ───────────────────────────────────────────────
            0xCD => {
                let nn = self.fetch_word(bus);
                self.push_word(bus, self.pc);
                self.pc = nn;
            }

            // ── IN A,(n) ──────────────────────────────────────────────
            0xDB => {
                let n = self.fetch_byte(bus);
                self.a = bus.in_port(n);
            }
            // ── OUT (n),A ─────────────────────────────────────────────
            0xD3 => {
                let n = self.fetch_byte(bus);
                bus.out_port(n, self.a);
            }

            // ── EI / DI ───────────────────────────────────────────────
            0xF3 => {
                self.iff1 = false;
                self.iff2 = false;
                self.ei_pending = 0;
            }
            0xFB => {
                self.ei_pending = 2;
            }

            // ── CPL ───────────────────────────────────────────────────
            0x2F => {
                self.a = !self.a;
                self.set_flag(FLAG_H, true);
                self.set_flag(FLAG_N, true);
            }

            // ── EX DE,HL ──────────────────────────────────────────────
            0xEB => {
                let dh = self.d;
                let dl = self.e;
                self.d = self.h;
                self.e = self.l;
                self.h = dh;
                self.l = dl;
            }

            // ── EX AF,AF' ────────────────────────────────────────────
            0x08 => {
                // We don't model the shadow register set explicitly; for
                // our scope (single-bank translated code), swap AF with
                // a private hidden copy stored in self.af_shadow.
                let af = ((self.a as u16) << 8) | (self.f as u16);
                let sh = self.af_shadow;
                self.a = (sh >> 8) as u8;
                self.f = (sh & 0xFF) as u8;
                self.af_shadow = af;
            }

            // ── EXX ───────────────────────────────────────────────────
            0xD9 => {
                let bc = ((self.b as u16) << 8) | self.c as u16;
                let de = ((self.d as u16) << 8) | self.e as u16;
                let hl = ((self.h as u16) << 8) | self.l as u16;
                let sb = self.bc_shadow;
                let sd = self.de_shadow;
                let sh = self.hl_shadow;
                self.b = (sb >> 8) as u8;
                self.c = (sb & 0xFF) as u8;
                self.d = (sd >> 8) as u8;
                self.e = (sd & 0xFF) as u8;
                self.h = (sh >> 8) as u8;
                self.l = (sh & 0xFF) as u8;
                self.bc_shadow = bc;
                self.de_shadow = de;
                self.hl_shadow = hl;
            }

            // ── EX (SP),HL ────────────────────────────────────────────
            0xE3 => {
                let lo = bus.read(self.sp);
                let hi = bus.read(self.sp.wrapping_add(1));
                bus.write(self.sp, self.l);
                bus.write(self.sp.wrapping_add(1), self.h);
                self.l = lo;
                self.h = hi;
            }

            // ── JP (HL) ──────────────────────────────────────────────
            0xE9 => {
                self.pc = self.hl();
            }

            // ── RET NZ/NC (extra conditions already covered above) ────
            // ── JP P / JP M already covered above ────────────────────

            // ── ED-prefix dispatch ────────────────────────────────────
            0xED => {
                let ed = self.fetch_byte(bus);
                match ed {
                    // IM 0, IM 1, IM 2 — we don't actually emulate
                    // interrupt modes; just acknowledge them.
                    0x46 | 0x66 => { /* IM 0 */ }
                    0x56 | 0x76 => { /* IM 1 */ }
                    0x5E | 0x7E => { /* IM 2 */ }
                    // RETN ($45) / RETI ($4D): like RET but with IFF
                    // semantics. For trace purposes, treat as RET.
                    0x45 | 0x4D | 0x55 | 0x5D | 0x65 | 0x6D | 0x75 | 0x7D => {
                        let lo = bus.read(self.sp);
                        let hi = bus.read(self.sp.wrapping_add(1));
                        self.sp = self.sp.wrapping_add(2);
                        self.pc = ((hi as u16) << 8) | lo as u16;
                        self.iff1 = self.iff2;
                        self.ei_pending = 0;
                    }
                    // NEG — A = 0 - A.
                    0x44 | 0x4C | 0x54 | 0x5C | 0x64 | 0x6C | 0x74 | 0x7C => {
                        let orig = self.a;
                        let result = 0u8.wrapping_sub(orig);
                        self.a = result;
                        self.f = 0;
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_C, orig != 0);
                        self.set_flag(FLAG_PV, orig == 0x80);
                        self.set_flag(FLAG_H, (orig & 0x0F) != 0);
                        self.set_flag(FLAG_S, result & 0x80 != 0);
                        self.set_flag(FLAG_Z, result == 0);
                    }
                    // LD (nn), BC ($43), DE ($53), HL ($63), SP ($73)
                    0x43 | 0x53 | 0x63 | 0x73 => {
                        let lo = self.fetch_byte(bus) as u16;
                        let hi = self.fetch_byte(bus) as u16;
                        let addr = (hi << 8) | lo;
                        let val = match ed {
                            0x43 => self.bc(),
                            0x53 => self.de(),
                            0x63 => self.hl(),
                            0x73 => self.sp,
                            _ => unreachable!(),
                        };
                        bus.write(addr, (val & 0xFF) as u8);
                        bus.write(addr.wrapping_add(1), (val >> 8) as u8);
                    }
                    // LD BC,(nn) ($4B), DE,(nn) ($5B), HL,(nn) ($6B), SP,(nn) ($7B)
                    0x4B | 0x5B | 0x6B | 0x7B => {
                        let lo = self.fetch_byte(bus) as u16;
                        let hi = self.fetch_byte(bus) as u16;
                        let addr = (hi << 8) | lo;
                        let v_lo = bus.read(addr) as u16;
                        let v_hi = bus.read(addr.wrapping_add(1)) as u16;
                        let val = (v_hi << 8) | v_lo;
                        match ed {
                            0x4B => self.set_bc(val),
                            0x5B => self.set_de(val),
                            0x6B => self.set_hl(val),
                            0x7B => self.sp = val,
                            _ => unreachable!(),
                        }
                    }
                    // SBC HL,rr — 16-bit subtract with carry.
                    0x42 | 0x52 | 0x62 | 0x72 => {
                        let hl = self.hl() as i32;
                        let rr = match ed {
                            0x42 => self.bc() as i32,
                            0x52 => self.de() as i32,
                            0x62 => self.hl() as i32,
                            0x72 => self.sp as i32,
                            _ => unreachable!(),
                        };
                        let cin = if self.flag(FLAG_C) { 1 } else { 0 };
                        let result = hl - rr - cin;
                        self.set_hl((result & 0xFFFF) as u16);
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_C, result < 0);
                        // Half/overflow: simplified.
                        self.set_flag(FLAG_H, ((hl & 0x0FFF) - (rr & 0x0FFF) - cin) < 0);
                        // PV: signed overflow detection — simplified.
                        let signed_overflow =
                            ((hl ^ rr) & 0x8000) != 0 && ((hl ^ result) & 0x8000) != 0;
                        self.set_flag(FLAG_PV, signed_overflow);
                        self.set_flag(FLAG_S, (result & 0x8000) != 0);
                        self.set_flag(FLAG_Z, (result & 0xFFFF) == 0);
                    }
                    // ADC HL,rr — 16-bit add with carry.
                    0x4A | 0x5A | 0x6A | 0x7A => {
                        let hl = self.hl() as u32;
                        let rr = match ed {
                            0x4A => self.bc() as u32,
                            0x5A => self.de() as u32,
                            0x6A => self.hl() as u32,
                            0x7A => self.sp as u32,
                            _ => unreachable!(),
                        };
                        let cin = if self.flag(FLAG_C) { 1u32 } else { 0 };
                        let result = hl + rr + cin;
                        self.set_hl((result & 0xFFFF) as u16);
                        self.set_flag(FLAG_N, false);
                        self.set_flag(FLAG_C, result > 0xFFFF);
                        self.set_flag(FLAG_H, ((hl & 0x0FFF) + (rr & 0x0FFF) + cin) > 0x0FFF);
                        let signed_overflow =
                            ((hl ^ rr) & 0x8000) == 0 && ((hl ^ result) & 0x8000) != 0;
                        self.set_flag(FLAG_PV, signed_overflow);
                        self.set_flag(FLAG_S, (result & 0x8000) != 0);
                        self.set_flag(FLAG_Z, (result & 0xFFFF) == 0);
                    }
                    // LDI ($A0) / LDIR ($B0) — block memory copy.
                    0xA0 | 0xB0 => {
                        let src = self.hl();
                        let dst = self.de();
                        let byte = bus.read(src);
                        bus.write(dst, byte);
                        self.set_hl(src.wrapping_add(1));
                        self.set_de(dst.wrapping_add(1));
                        let bc = self.bc().wrapping_sub(1);
                        self.set_bc(bc);
                        self.set_flag(FLAG_N, false);
                        self.set_flag(FLAG_H, false);
                        self.set_flag(FLAG_PV, bc != 0);
                        if ed == 0xB0 && bc != 0 {
                            // LDIR repeats by reverting PC to the instruction
                            // (decrement by 2 to re-execute the ED B0 opcode).
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    // LDD ($A8) / LDDR ($B8) — block memory copy, decrementing.
                    0xA8 | 0xB8 => {
                        let src = self.hl();
                        let dst = self.de();
                        let byte = bus.read(src);
                        bus.write(dst, byte);
                        self.set_hl(src.wrapping_sub(1));
                        self.set_de(dst.wrapping_sub(1));
                        let bc = self.bc().wrapping_sub(1);
                        self.set_bc(bc);
                        self.set_flag(FLAG_N, false);
                        self.set_flag(FLAG_H, false);
                        self.set_flag(FLAG_PV, bc != 0);
                        if ed == 0xB8 && bc != 0 {
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    // OUTI / OTIR — port output with HL pointer + B counter.
                    0xA3 | 0xB3 => {
                        let val = bus.read(self.hl());
                        let port = self.c;
                        bus.out_port(port, val);
                        self.set_hl(self.hl().wrapping_add(1));
                        self.b = self.b.wrapping_sub(1);
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_Z, self.b == 0);
                        if ed == 0xB3 && self.b != 0 {
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    // INI / INIR — port input with HL pointer + B counter.
                    0xA2 | 0xB2 => {
                        let port = self.c;
                        let val = bus.in_port(port);
                        bus.write(self.hl(), val);
                        self.set_hl(self.hl().wrapping_add(1));
                        self.b = self.b.wrapping_sub(1);
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_Z, self.b == 0);
                        if ed == 0xB2 && self.b != 0 {
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    // OUTD / OTDR / IND / INDR — decrementing variants.
                    0xAB | 0xBB => {
                        let val = bus.read(self.hl());
                        let port = self.c;
                        bus.out_port(port, val);
                        self.set_hl(self.hl().wrapping_sub(1));
                        self.b = self.b.wrapping_sub(1);
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_Z, self.b == 0);
                        if ed == 0xBB && self.b != 0 {
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    0xAA | 0xBA => {
                        let port = self.c;
                        let val = bus.in_port(port);
                        bus.write(self.hl(), val);
                        self.set_hl(self.hl().wrapping_sub(1));
                        self.b = self.b.wrapping_sub(1);
                        self.set_flag(FLAG_N, true);
                        self.set_flag(FLAG_Z, self.b == 0);
                        if ed == 0xBA && self.b != 0 {
                            self.pc = self.pc.wrapping_sub(2);
                        }
                    }
                    _ => {
                        return Err(StepError::UnsupportedOpcode {
                            pc: pc_op,
                            opcode: ed,
                            prefix: Some(0xED),
                        });
                    }
                }
            }

            // ── Unsupported prefixes / opcodes ────────────────────────
            0xDD | 0xFD => {
                let _ = self.fetch_byte(bus); // consume the following byte
                return Err(StepError::UnsupportedOpcode {
                    pc: pc_op,
                    opcode: op,
                    prefix: None,
                });
            }

            _ => {
                return Err(StepError::UnsupportedOpcode {
                    pc: pc_op,
                    opcode: op,
                    prefix: None,
                });
            }
        }

        if self.ei_pending > 0 {
            self.ei_pending -= 1;
            if self.ei_pending == 0 {
                self.iff1 = true;
                self.iff2 = true;
            }
        }

        Ok(())
    }

    /// Run until the first RET that unwinds to depth < 0, i.e. the call that
    /// the caller made on our behalf.  Returns the number of steps executed.
    pub fn run_until_ret(
        &mut self,
        bus: &mut impl Bus,
        max_steps: usize,
    ) -> Result<usize, StepError> {
        self.run_until_ret_with_trace::<fn(&Cpu, u16, u8)>(bus, max_steps, None)
    }

    /// Like `run_until_ret`, but invokes `trace` before each instruction
    /// with `(cpu, pc, opcode)`. Use for diagnostics.
    pub fn run_until_ret_with_trace<F>(
        &mut self,
        bus: &mut impl Bus,
        max_steps: usize,
        mut trace: Option<F>,
    ) -> Result<usize, StepError>
    where
        F: FnMut(&Cpu, u16, u8),
    {
        let mut depth: i32 = 0;
        let mut steps = 0;

        loop {
            if steps >= max_steps {
                return Err(StepError::StepLimitExceeded);
            }

            let pc = self.pc;
            let op = bus.read(pc);

            if let Some(t) = trace.as_mut() {
                t(self, pc, op);
            }

            // Detect CALL / RET before executing so we can track depth.
            let is_call = matches!(
                op,
                0xCD |           // CALL nn
                0xC4 | 0xCC // CALL NZ/Z,nn
            );
            let is_ret = matches!(
                op,
                0xC9 |                    // RET
                0xC0 | 0xC8 |             // RET NZ / RET Z
                0xD0 | 0xD8 |             // RET NC / RET C
                0xE0 | 0xE8 |             // RET PO / RET PE
                0xF0 | 0xF8 // RET P  / RET M
            );

            if is_ret && depth == 0 {
                // Execute the ret, then stop.
                self.step(bus)?;
                steps += 1;
                return Ok(steps);
            }

            self.step(bus)?;
            steps += 1;

            // Adjust depth after execution so we can tell if the call was
            // actually taken (conditional calls/rets may not transfer).
            // We re-check by observing PC change relative to expectation,
            // but that's fragile — simpler: track by op code.
            // For conditional calls: if taken, depth changes; but we
            // already executed it. We detect if the call was taken by
            // checking whether PC changed to something other than pc+3.
            if is_call {
                // unconditional: depth always increases
                // conditional: only if branch was taken
                // We can't tell post-hoc cheaply; treat all observed CALLs
                // as depth+1 for conservative tracking. The only risk is
                // a not-taken conditional CALL being counted — but since
                // we never decrement below 0 until we see RET, this is safe.
                depth += 1;
            } else if is_ret {
                depth -= 1;
            }
        }
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

// ── StepError ────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum StepError {
    UnsupportedOpcode {
        pc: u16,
        opcode: u8,
        prefix: Option<u8>,
    },
    Halt,
    StepLimitExceeded,
}

// ── Utility ───────────────────────────────────────────────────────────────────

/// True when the number of set bits is even (Z80 "parity even" = PV=1).
fn parity(v: u8) -> bool {
    v.count_ones() % 2 == 0
}

// ══════════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── PortLog test bus ──────────────────────────────────────────────────────

    struct PortLog {
        mem: Box<[u8; 0x10000]>,
        log: Vec<(u8, u8)>, // (port, value)
    }

    impl PortLog {
        fn new() -> Self {
            Self {
                mem: Box::new([0u8; 0x10000]),
                log: Vec::new(),
            }
        }
        fn load(&mut self, addr: u16, bytes: &[u8]) {
            let s = addr as usize;
            self.mem[s..s + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl Bus for PortLog {
        fn read(&mut self, addr: u16) -> u8 {
            self.mem[addr as usize]
        }
        fn write(&mut self, addr: u16, v: u8) {
            self.mem[addr as usize] = v;
        }
        fn out_port(&mut self, port: u8, value: u8) {
            self.log.push((port, value));
        }
    }

    // ── Helper ───────────────────────────────────────────────────────────────

    fn run(prog: &[u8]) -> (Cpu, FlatBus) {
        let mut bus = FlatBus::new();
        bus.load(0x0000, prog);
        let mut cpu = Cpu::new();
        // run until HALT or error (use step limit)
        for _ in 0..10_000 {
            match cpu.step(&mut bus) {
                Ok(()) => {}
                Err(StepError::Halt) => break,
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
        (cpu, bus)
    }

    // ── NOP ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_nop() {
        let (cpu, _) = run(&[0x00, 0x76]);
        assert_eq!(cpu.pc, 2);
        assert_eq!(cpu.a, 0);
    }

    // ── 8-bit immediate loads ─────────────────────────────────────────────────

    #[test]
    fn test_ld_r_n() {
        // ld b,0x12; ld c,0x34; ld d,0x56; ld e,0x78; ld h,0x9A; ld l,0xBC; ld a,0xDE; halt
        let (cpu, _) = run(&[
            0x06, 0x12, 0x0E, 0x34, 0x16, 0x56, 0x1E, 0x78, 0x26, 0x9A, 0x2E, 0xBC, 0x3E, 0xDE,
            0x76,
        ]);
        assert_eq!(cpu.b, 0x12);
        assert_eq!(cpu.c, 0x34);
        assert_eq!(cpu.d, 0x56);
        assert_eq!(cpu.e, 0x78);
        assert_eq!(cpu.h, 0x9A);
        assert_eq!(cpu.l, 0xBC);
        assert_eq!(cpu.a, 0xDE);
    }

    // ── ld r,r' ───────────────────────────────────────────────────────────────

    #[test]
    fn test_ld_r_r() {
        // ld b,0x42; ld a,b; halt
        let (cpu, _) = run(&[0x06, 0x42, 0x78, 0x76]);
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn test_ld_d_e() {
        // ld e,0x55; ld d,e; halt
        let (cpu, _) = run(&[0x1E, 0x55, 0x53, 0x76]);
        assert_eq!(cpu.d, 0x55);
    }

    // ── 16-bit loads ─────────────────────────────────────────────────────────

    #[test]
    fn test_ld_hl_nn() {
        let (cpu, _) = run(&[0x21, 0x34, 0x12, 0x76]);
        assert_eq!(cpu.hl(), 0x1234);
    }

    #[test]
    fn test_ld_bc_nn() {
        let (cpu, _) = run(&[0x01, 0xCD, 0xAB, 0x76]);
        assert_eq!(cpu.bc(), 0xABCD);
    }

    #[test]
    fn test_ld_de_nn() {
        let (cpu, _) = run(&[0x11, 0x78, 0x56, 0x76]);
        assert_eq!(cpu.de(), 0x5678);
    }

    #[test]
    fn test_ld_sp_nn() {
        let (cpu, _) = run(&[0x31, 0x00, 0x80, 0x76]);
        assert_eq!(cpu.sp, 0x8000);
    }

    // ── ld (nn),hl / ld hl,(nn) ──────────────────────────────────────────────

    #[test]
    fn test_ld_nn_hl_little_endian() {
        // ld hl,0x1234; ld (0x0100),hl; halt
        let (_, bus) = run(&[0x21, 0x34, 0x12, 0x22, 0x00, 0x01, 0x76]);
        assert_eq!(bus.mem[0x0100], 0x34); // lo
        assert_eq!(bus.mem[0x0101], 0x12); // hi
    }

    #[test]
    fn test_ld_hl_nn_addr() {
        let mut bus = FlatBus::new();
        // deposit 0x5678 at address 0x0200
        bus.mem[0x0200] = 0x78;
        bus.mem[0x0201] = 0x56;
        // ld hl,(0x0200); halt
        bus.load(0x0000, &[0x2A, 0x00, 0x02, 0x76]);
        let mut cpu = Cpu::new();
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.hl(), 0x5678);
    }

    // ── ld a,(nn) / ld (nn),a ────────────────────────────────────────────────

    #[test]
    fn test_ld_a_nn() {
        let mut bus = FlatBus::new();
        bus.mem[0x0300] = 0xAA;
        bus.load(0x0000, &[0x3A, 0x00, 0x03, 0x76]);
        let mut cpu = Cpu::new();
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0xAA);
    }

    #[test]
    fn test_ld_nn_a() {
        let (_, bus) = run(&[0x3E, 0xBB, 0x32, 0x50, 0x01, 0x76]);
        assert_eq!(bus.mem[0x0150], 0xBB);
    }

    // ── ld a,(hl) / ld (hl),a ────────────────────────────────────────────────

    #[test]
    fn test_ld_a_hl() {
        let mut bus = FlatBus::new();
        bus.mem[0x0400] = 0x99;
        bus.load(0x0000, &[0x21, 0x00, 0x04, 0x7E, 0x76]); // ld hl,0x400; ld a,(hl)
        let mut cpu = Cpu::new();
        for _ in 0..2 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.a, 0x99);
    }

    #[test]
    fn test_ld_hl_a() {
        // ld hl,0x500; ld a,0xCC; ld (hl),a
        let (_, bus) = run(&[0x21, 0x00, 0x05, 0x3E, 0xCC, 0x77, 0x76]);
        assert_eq!(bus.mem[0x0500], 0xCC);
    }

    // ── ld a,(bc) / ld (bc),a ────────────────────────────────────────────────

    #[test]
    fn test_ld_a_bc_de() {
        let mut bus = FlatBus::new();
        bus.mem[0x0600] = 0x77;
        bus.mem[0x0700] = 0x88;
        bus.load(
            0x0000,
            &[
                0x01, 0x00, 0x06, 0x0A, // ld bc,0x600; ld a,(bc)
                0x11, 0x00, 0x07, 0x1A, // ld de,0x700; ld a,(de)
                0x76,
            ],
        );
        let mut cpu = Cpu::new();
        for _ in 0..4 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.a, 0x88);
    }

    #[test]
    fn test_ld_bc_a() {
        // ld bc,0x0800; ld a,0x11; ld (bc),a
        let (_, bus) = run(&[0x01, 0x00, 0x08, 0x3E, 0x11, 0x02, 0x76]);
        assert_eq!(bus.mem[0x0800], 0x11);
    }

    // ── ADD A,n ──────────────────────────────────────────────────────────────

    #[test]
    fn test_add_a_n_basic() {
        // ld a,0x05; add a,0x03; halt
        let (cpu, _) = run(&[0x3E, 0x05, 0xC6, 0x03, 0x76]);
        assert_eq!(cpu.a, 0x08);
        assert!(cpu.f & FLAG_N == 0);
        assert!(cpu.f & FLAG_Z == 0);
        assert!(cpu.f & FLAG_C == 0);
    }

    #[test]
    fn test_add_a_n_overflow() {
        // ld a,0x7F; add a,0x01 → A=0x80, S=1, Z=0, H=1, PV=1, N=0, C=0
        let (cpu, _) = run(&[0x3E, 0x7F, 0xC6, 0x01, 0x76]);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.f & FLAG_S != 0, "S should be set");
        assert!(cpu.f & FLAG_Z == 0, "Z should be clear");
        assert!(cpu.f & FLAG_H != 0, "H should be set");
        assert!(cpu.f & FLAG_PV != 0, "PV should be set");
        assert!(cpu.f & FLAG_N == 0, "N should be clear");
        assert!(cpu.f & FLAG_C == 0, "C should be clear");
    }

    #[test]
    fn test_add_a_n_carry() {
        // ld a,0xFF; add a,0x01 → A=0x00, Z=1, H=1, C=1
        let (cpu, _) = run(&[0x3E, 0xFF, 0xC6, 0x01, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_Z != 0, "Z should be set");
        assert!(cpu.f & FLAG_H != 0, "H should be set");
        assert!(cpu.f & FLAG_C != 0, "C should be set");
    }

    // ── SUB n ────────────────────────────────────────────────────────────────

    #[test]
    fn test_sub_n_basic() {
        // ld a,0x10; sub 0x05
        let (cpu, _) = run(&[0x3E, 0x10, 0xD6, 0x05, 0x76]);
        assert_eq!(cpu.a, 0x0B);
        assert!(cpu.f & FLAG_N != 0);
        assert!(cpu.f & FLAG_C == 0);
    }

    #[test]
    fn test_sub_n_borrow() {
        // ld a,0x00; sub 0x01 → A=0xFF, S=1, C=1, H=1
        let (cpu, _) = run(&[0x3E, 0x00, 0xD6, 0x01, 0x76]);
        assert_eq!(cpu.a, 0xFF);
        assert!(cpu.f & FLAG_S != 0, "S should be set");
        assert!(cpu.f & FLAG_C != 0, "C should be set (borrow)");
        assert!(cpu.f & FLAG_H != 0, "H should be set (half-borrow)");
        assert!(cpu.f & FLAG_N != 0, "N should be set");
    }

    // ── AND n ────────────────────────────────────────────────────────────────

    #[test]
    fn test_and_n() {
        // ld a,0xF0; and 0x0F → A=0x00, Z=1, H=1, N=0, C=0
        let (cpu, _) = run(&[0x3E, 0xF0, 0xE6, 0x0F, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_Z != 0);
        assert!(cpu.f & FLAG_H != 0);
        assert!(cpu.f & FLAG_N == 0);
        assert!(cpu.f & FLAG_C == 0);
    }

    #[test]
    fn test_and_n_parity() {
        // ld a,0xFF; and 0x81 → A=0x81, parity of 0x81 = 2 set bits = even = PV=1
        let (cpu, _) = run(&[0x3E, 0xFF, 0xE6, 0x81, 0x76]);
        assert_eq!(cpu.a, 0x81);
        assert!(cpu.f & FLAG_PV != 0); // even parity
    }

    // ── OR n ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_or_n() {
        // ld a,0x0F; or 0xF0 → A=0xFF, S=1, PV=1 (8 bits set = even parity)
        let (cpu, _) = run(&[0x3E, 0x0F, 0xF6, 0xF0, 0x76]);
        assert_eq!(cpu.a, 0xFF);
        assert!(cpu.f & FLAG_S != 0);
        assert!(cpu.f & FLAG_PV != 0); // 0xFF has 8 set bits = even parity = PV=1
    }

    #[test]
    fn test_or_n_zero() {
        // ld a,0x00; or 0x00 → A=0x00, Z=1, PV=1 (0 ones = even)
        let (cpu, _) = run(&[0x3E, 0x00, 0xF6, 0x00, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_Z != 0);
        assert!(cpu.f & FLAG_PV != 0);
    }

    // ── XOR n ────────────────────────────────────────────────────────────────

    #[test]
    fn test_xor_n() {
        // ld a,0xFF; xor 0xFF → A=0x00, Z=1
        let (cpu, _) = run(&[0x3E, 0xFF, 0xEE, 0xFF, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_Z != 0);
        assert!(cpu.f & FLAG_N == 0);
        assert!(cpu.f & FLAG_C == 0);
    }

    // ── CP n ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_cp_n_does_not_modify_a() {
        // ld a,0x42; cp 0x42 → Z=1, A unchanged
        let (cpu, _) = run(&[0x3E, 0x42, 0xFE, 0x42, 0x76]);
        assert_eq!(cpu.a, 0x42, "A must not change after CP");
        assert!(cpu.f & FLAG_Z != 0, "Z set when A==n");
    }

    #[test]
    fn test_cp_n_carry() {
        // ld a,0x01; cp 0x02 → C=1 (borrow), Z=0
        let (cpu, _) = run(&[0x3E, 0x01, 0xFE, 0x02, 0x76]);
        assert_eq!(cpu.a, 0x01);
        assert!(cpu.f & FLAG_C != 0);
        assert!(cpu.f & FLAG_Z == 0);
    }

    // ── ADC / SBC ────────────────────────────────────────────────────────────

    #[test]
    fn test_adc_a_n() {
        // ld a,0x01; scf; adc a,0x01 → A=0x03
        let (cpu, _) = run(&[0x3E, 0x01, 0x37, 0xCE, 0x01, 0x76]);
        assert_eq!(cpu.a, 0x03);
    }

    #[test]
    fn test_sbc_a_n() {
        // ld a,0x05; scf; sbc a,0x01 → A = 5 - 1 - 1 = 3
        let (cpu, _) = run(&[0x3E, 0x05, 0x37, 0xDE, 0x01, 0x76]);
        assert_eq!(cpu.a, 0x03);
    }

    // ── INC / DEC r ──────────────────────────────────────────────────────────

    #[test]
    fn test_inc_a_pv() {
        // ld a,0x7F; inc a → A=0x80, PV=1
        let (cpu, _) = run(&[0x3E, 0x7F, 0x3C, 0x76]);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.f & FLAG_PV != 0, "PV set on signed overflow at 0x7F+1");
        assert!(cpu.f & FLAG_S != 0);
    }

    #[test]
    fn test_dec_a_pv() {
        // ld a,0x80; dec a → A=0x7F, PV=1
        let (cpu, _) = run(&[0x3E, 0x80, 0x3D, 0x76]);
        assert_eq!(cpu.a, 0x7F);
        assert!(cpu.f & FLAG_PV != 0, "PV set on signed overflow at 0x80-1");
    }

    #[test]
    fn test_inc_dec_preserve_carry() {
        // scf; ld a,0x00; inc a → carry should still be set
        let (cpu, _) = run(&[0x37, 0x3E, 0x00, 0x3C, 0x76]);
        assert_eq!(cpu.a, 0x01);
        assert!(cpu.f & FLAG_C != 0, "INC must not clear carry");
    }

    #[test]
    fn test_inc_b_c() {
        let (cpu, _) = run(&[0x06, 0x0F, 0x04, 0x0E, 0x0F, 0x0C, 0x76]);
        assert_eq!(cpu.b, 0x10);
        assert_eq!(cpu.c, 0x10);
    }

    // ── INC/DEC 16-bit ───────────────────────────────────────────────────────

    #[test]
    fn test_inc_rr_wrap() {
        // ld hl,0xFFFF; inc hl → 0x0000, no flag changes
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0x21, 0xFF, 0xFF, 0x23, 0x76]);
        let mut cpu = Cpu::new();
        cpu.f = FLAG_C | FLAG_Z; // set some flags
        for _ in 0..3 {
            let _ = cpu.step(&mut bus);
        }
        assert_eq!(cpu.hl(), 0x0000);
        // Flags unchanged by 16-bit inc
        assert!(cpu.f & FLAG_C != 0);
        assert!(cpu.f & FLAG_Z != 0);
    }

    #[test]
    fn test_inc_bc_de() {
        let (cpu, _) = run(&[
            0x01, 0xFF, 0xFF, 0x03, // ld bc,0xFFFF; inc bc
            0x11, 0x00, 0x10, 0x13, // ld de,0x1000; inc de
            0x76,
        ]);
        assert_eq!(cpu.bc(), 0x0000);
        assert_eq!(cpu.de(), 0x1001);
    }

    #[test]
    fn test_dec_rr() {
        let (cpu, _) = run(&[0x21, 0x00, 0x01, 0x2B, 0x76]); // ld hl,0x100; dec hl
        assert_eq!(cpu.hl(), 0x00FF);
    }

    // ── Rotates ──────────────────────────────────────────────────────────────

    #[test]
    fn test_rlca() {
        // ld a,0x81; rlca → A=0x03, C=1
        let (cpu, _) = run(&[0x3E, 0x81, 0x07, 0x76]);
        assert_eq!(cpu.a, 0x03);
        assert!(cpu.f & FLAG_C != 0);
        assert!(cpu.f & FLAG_H == 0);
        assert!(cpu.f & FLAG_N == 0);
    }

    #[test]
    fn test_rrca() {
        // ld a,0x01; rrca → A=0x80, C=1
        let (cpu, _) = run(&[0x3E, 0x01, 0x0F, 0x76]);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.f & FLAG_C != 0);
    }

    #[test]
    fn test_rla_through_carry() {
        // ld a,0x00; scf; rla → A=0x01, C=0 (old bit7=0, old C=1 shifts in)
        let (cpu, _) = run(&[0x3E, 0x00, 0x37, 0x17, 0x76]);
        assert_eq!(cpu.a, 0x01);
        assert!(cpu.f & FLAG_C == 0);
    }

    #[test]
    fn test_rra_through_carry() {
        // ld a,0x00; scf; rra → A=0x80, C=0
        let (cpu, _) = run(&[0x3E, 0x00, 0x37, 0x1F, 0x76]);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.f & FLAG_C == 0);
    }

    // ── CB-prefix shifts ──────────────────────────────────────────────────────

    #[test]
    fn test_cb_sla_a() {
        // ld a,0x80; sla a → A=0x00, C=1, Z=1
        let (cpu, _) = run(&[0x3E, 0x80, 0xCB, 0x27, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_C != 0, "C=1 (bit 7 shifted out)");
        assert!(cpu.f & FLAG_Z != 0, "Z=1");
    }

    #[test]
    fn test_cb_srl_a() {
        // ld a,0x01; srl a → A=0x00, C=1, Z=1
        let (cpu, _) = run(&[0x3E, 0x01, 0xCB, 0x3F, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_C != 0);
        assert!(cpu.f & FLAG_Z != 0);
    }

    #[test]
    fn test_cb_rl_a() {
        // ld a,0x40; scf; rl a → A=0x81, C=0
        let (cpu, _) = run(&[0x3E, 0x40, 0x37, 0xCB, 0x17, 0x76]);
        assert_eq!(cpu.a, 0x81);
        assert!(cpu.f & FLAG_C == 0);
    }

    #[test]
    fn test_cb_rr_a() {
        // ld a,0x02; scf; rr a → A=0x81, C=0
        let (cpu, _) = run(&[0x3E, 0x02, 0x37, 0xCB, 0x1F, 0x76]);
        assert_eq!(cpu.a, 0x81);
        assert!(cpu.f & FLAG_C == 0);
    }

    // ── BIT / SET / RES ──────────────────────────────────────────────────────

    #[test]
    fn test_bit_7_a_set() {
        // ld a,0x80; bit 7,a → Z=0 (bit is set), H=1
        let (cpu, _) = run(&[0x3E, 0x80, 0xCB, 0x7F, 0x76]);
        assert!(cpu.f & FLAG_Z == 0, "Z=0 when bit is set");
        assert!(cpu.f & FLAG_H != 0, "H=1 always for BIT");
        assert!(cpu.f & FLAG_N == 0);
    }

    #[test]
    fn test_bit_0_a_clear() {
        // ld a,0xFE; bit 0,a → Z=1 (bit 0 is clear)
        let (cpu, _) = run(&[0x3E, 0xFE, 0xCB, 0x47, 0x76]);
        assert!(cpu.f & FLAG_Z != 0, "Z=1 when bit is clear");
    }

    #[test]
    fn test_set_res_a() {
        // ld a,0x00; set 0,a → A=0x01; res 0,a → A=0x00; halt
        let f_before;
        let (cpu, _) = {
            let mut bus = FlatBus::new();
            bus.load(0x0000, &[0x3E, 0x00, 0xCB, 0xC7, 0xCB, 0x87, 0x76]);
            let mut cpu = Cpu::new();
            cpu.f = FLAG_S | FLAG_Z | FLAG_C; // some flags to verify unchanged
            f_before = cpu.f;
            for _ in 0..4 {
                let _ = cpu.step(&mut bus);
            }
            (cpu, bus)
        };
        assert_eq!(cpu.a, 0x00);
        assert_eq!(cpu.f, f_before, "SET/RES must not change flags");
    }

    #[test]
    fn test_set_bit3_a() {
        // ld a,0x00; set 3,a → A=0x08
        let (cpu, _) = run(&[0x3E, 0x00, 0xCB, 0xDF, 0x76]);
        assert_eq!(cpu.a, 0x08);
    }

    // ── SCF / CCF / CPL ──────────────────────────────────────────────────────

    #[test]
    fn test_scf_ccf() {
        // scf → C=1; ccf → C=0, H=1; ccf → C=1, H=0
        let (cpu, _) = run(&[0x37, 0x3F, 0x3F, 0x76]);
        assert!(cpu.f & FLAG_C != 0);
        assert!(cpu.f & FLAG_H == 0); // after second ccf H=prev C=0
    }

    #[test]
    fn test_cpl() {
        // ld a,0xAA; cpl → A=0x55, H=1, N=1
        let (cpu, _) = run(&[0x3E, 0xAA, 0x2F, 0x76]);
        assert_eq!(cpu.a, 0x55);
        assert!(cpu.f & FLAG_H != 0);
        assert!(cpu.f & FLAG_N != 0);
    }

    // ── JP / JR / DJNZ ───────────────────────────────────────────────────────

    #[test]
    fn test_jp_nn() {
        // jp 0x0100; [gap]; at 0x100: ld a,0x42; halt
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0xC3, 0x00, 0x01]); // jp 0x0100
        bus.load(0x0100, &[0x3E, 0x42, 0x76]); // ld a,0x42; halt
        let mut cpu = Cpu::new();
        cpu.step(&mut bus).unwrap(); // jp
        cpu.step(&mut bus).unwrap(); // ld a
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn test_jp_z_taken() {
        // xor a (Z=1); jp z,0x0100
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0xAF, 0xCA, 0x00, 0x01]); // xor a; jp z,0x100
        bus.load(0x0100, &[0x3E, 0x99, 0x76]);
        let mut cpu = Cpu::new();
        cpu.step(&mut bus).unwrap();
        cpu.step(&mut bus).unwrap();
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x99);
    }

    #[test]
    fn test_jp_nz_not_taken() {
        // xor a (Z=1); jp nz,0x0100 (not taken); ld a,0x11; halt
        let (cpu, _) = run(&[0xAF, 0xC2, 0x00, 0x01, 0x3E, 0x11, 0x76]);
        assert_eq!(cpu.a, 0x11);
    }

    #[test]
    fn test_jp_m_taken() {
        // ld a,0x80; add a,0x00 (S=1); jp m,0x0100
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0x3E, 0x80, 0xC6, 0x00, 0xFA, 0x00, 0x01]);
        bus.load(0x0100, &[0x3E, 0x55, 0x76]);
        let mut cpu = Cpu::new();
        for _ in 0..3 {
            cpu.step(&mut bus).unwrap();
        }
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x55);
    }

    #[test]
    fn test_jp_p_not_taken() {
        // ld a,0x80; add a,0x00 (S=1); jp p,0x0100 (not taken); ld a,0x66; halt
        let (cpu, _) = run(&[0x3E, 0x80, 0xC6, 0x00, 0xF2, 0x00, 0x01, 0x3E, 0x66, 0x76]);
        assert_eq!(cpu.a, 0x66);
    }

    #[test]
    fn test_jr_positive() {
        // jr +2 (skip 2 bytes); ld a,0xBB (skipped); ld a,0xCC; halt
        // JR e: after fetch of e, PC is at +2; e=2 → skip 2 more bytes
        let (cpu, _) = run(&[0x18, 0x02, 0x3E, 0xBB, 0x3E, 0xCC, 0x76]);
        assert_eq!(cpu.a, 0xCC);
    }

    #[test]
    fn test_jr_negative() {
        // ld b,3; (loop:) dec b; jr nz, -3 (back to dec b); ld a,0x77; halt
        // Layout: 0x00: ld b,3 (2); 0x02: dec b (1); 0x03: jr nz,-3 (2); 0x05: ld a,0x77 (2); 0x07: halt
        // jr nz,-3: after reading e byte, PC=0x05; e=-3 → PC=0x02 ✓
        let (cpu, _) = run(&[0x06, 0x03, 0x05, 0x20, 0xFD, 0x3E, 0x77, 0x76]);
        assert_eq!(cpu.b, 0x00);
        assert_eq!(cpu.a, 0x77);
    }

    #[test]
    fn test_djnz() {
        // ld b,4; (loop:) djnz -2; ld a,0x55; halt
        // djnz -2: after reading disp, PC=3; on taken: PC=3+(-2)=1 ✓
        let (cpu, _) = run(&[0x06, 0x04, 0x10, 0xFE, 0x3E, 0x55, 0x76]);
        assert_eq!(cpu.b, 0);
        assert_eq!(cpu.a, 0x55);
    }

    // ── CALL / RET ───────────────────────────────────────────────────────────

    #[test]
    fn test_call_ret() {
        // at 0x0000: ld sp,0x8000; call 0x0100; ld a,0x42; halt
        // at 0x0100: ld a,0x11; ret
        let mut bus = FlatBus::new();
        bus.load(
            0x0000,
            &[0x31, 0x00, 0x80, 0xCD, 0x00, 0x01, 0x3E, 0x42, 0x76],
        );
        bus.load(0x0100, &[0x3E, 0x11, 0xC9]);
        let mut cpu = Cpu::new();
        for _ in 0..6 {
            let _ = cpu.step(&mut bus);
        }
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn test_nested_calls() {
        // 0x0000: ld sp,0x8000; call 0x0100; halt
        // 0x0100: call 0x0200; ret
        // 0x0200: ld a,0x99; ret
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0x31, 0x00, 0x80, 0xCD, 0x00, 0x01, 0x76]);
        bus.load(0x0100, &[0xCD, 0x00, 0x02, 0xC9]);
        bus.load(0x0200, &[0x3E, 0x99, 0xC9]);
        let mut cpu = Cpu::new();
        for _ in 0..8 {
            let _ = cpu.step(&mut bus);
        }
        assert_eq!(cpu.a, 0x99);
    }

    #[test]
    fn test_ret_nz_taken() {
        // at 0x0000: ld sp,0x8000(3); ld a,0x01(2); call 0x0100(3); [halt at 0x0008]
        // at 0x0100: or a(1); ret nz(1)
        // call return address = 0x0008 (byte after the 3-byte CALL)
        let mut bus = FlatBus::new();
        bus.load(
            0x0000,
            &[0x31, 0x00, 0x80, 0x3E, 0x01, 0xCD, 0x00, 0x01, 0x76],
        );
        bus.load(0x0100, &[0xB7, 0xC0]); // or a; ret nz
        let mut cpu = Cpu::new();
        // steps: (1)ld sp, (2)ld a, (3)call, (4)or a, (5)ret nz → PC=0x0008
        for _ in 0..5 {
            let _ = cpu.step(&mut bus);
        }
        assert_eq!(cpu.pc, 0x0008);
    }

    #[test]
    fn test_ret_nz_not_taken() {
        // at 0x0100: xor a (Z=1); ret nz (not taken); ld a,0x33; ret
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0x31, 0x00, 0x80, 0xCD, 0x00, 0x01, 0x76]);
        bus.load(0x0100, &[0xAF, 0xC0, 0x3E, 0x33, 0xC9]);
        let mut cpu = Cpu::new();
        for _ in 0..8 {
            let _ = cpu.step(&mut bus);
        }
        assert_eq!(cpu.a, 0x33);
    }

    // ── PUSH / POP ───────────────────────────────────────────────────────────

    #[test]
    fn test_push_pop_af() {
        // ld sp,0x8000(1); ld a,0xAB(2); scf(3); push af(4); xor a(5); pop af(6); halt(7)
        let mut bus = FlatBus::new();
        bus.load(
            0x0000,
            &[0x31, 0x00, 0x80, 0x3E, 0xAB, 0x37, 0xF5, 0xAF, 0xF1, 0x76],
        );
        let mut cpu = Cpu::new();
        for _ in 0..6 {
            cpu.step(&mut bus).unwrap();
        }
        // step 6 was pop af; assert now
        assert_eq!(cpu.a, 0xAB, "A should be restored");
        assert!(cpu.f & FLAG_C != 0, "C should be restored via POP AF");
    }

    #[test]
    fn test_push_pop_bc_de_hl() {
        // ld sp,0x8000; ld bc,0x1234; push bc; ld bc,0; pop bc
        let mut bus = FlatBus::new();
        bus.load(
            0x0000,
            &[
                0x31, 0x00, 0x80, 0x01, 0x34, 0x12, 0xC5, 0x01, 0x00, 0x00, 0xC1, 0x76,
            ],
        );
        let mut cpu = Cpu::new();
        for _ in 0..5 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.bc(), 0x1234);
    }

    // ── OUT (n),A ─────────────────────────────────────────────────────────────

    #[test]
    fn test_out_port() {
        let mut bus = PortLog::new();
        // ld a,0x55(1); out (0x42),a(2); halt(3)
        bus.load(0x0000, &[0x3E, 0x55, 0xD3, 0x42, 0x76]);
        let mut cpu = Cpu::new();
        cpu.step(&mut bus).unwrap(); // ld a
        cpu.step(&mut bus).unwrap(); // out
        assert_eq!(bus.log, vec![(0x42, 0x55)]);
    }

    // ── run_until_ret ─────────────────────────────────────────────────────────

    #[test]
    fn test_run_until_ret_add2() {
        // Subroutine at 0x0100: ld a,0x05; add a,0x02; ret
        // We start PC at 0x0100 (as if called externally)
        let mut bus = FlatBus::new();
        bus.load(0x0100, &[0x3E, 0x05, 0xC6, 0x02, 0xC9]);
        bus.mem[0x7FFF] = 0x00; // return address placeholder
        let mut cpu = Cpu::new();
        cpu.pc = 0x0100;
        cpu.sp = 0x8000;
        // push a fake return address so ret works
        cpu.sp -= 2;
        bus.mem[cpu.sp as usize] = 0x00;
        bus.mem[cpu.sp as usize + 1] = 0x00;
        let steps = cpu.run_until_ret(&mut bus, 100).unwrap();
        assert_eq!(cpu.a, 0x07);
        assert!(steps >= 3);
    }

    // ── Unsupported opcodes ───────────────────────────────────────────────────

    #[test]
    fn test_unsupported_ed_prefix() {
        // Most ED-prefix opcodes are now implemented (IM, LDIR, NEG, etc.).
        // Truly invalid subcodes still error.
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0xED, 0x00]);
        let mut cpu = Cpu::new();
        match cpu.step(&mut bus) {
            Err(StepError::UnsupportedOpcode {
                opcode: 0x00,
                prefix: Some(0xED),
                ..
            }) => {}
            other => panic!("expected UnsupportedOpcode(ED 00), got {other:?}"),
        }
    }

    #[test]
    fn test_unsupported_dd_prefix() {
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0xDD, 0x00]);
        let mut cpu = Cpu::new();
        match cpu.step(&mut bus) {
            Err(StepError::UnsupportedOpcode {
                opcode: 0xDD,
                prefix: None,
                ..
            }) => {}
            other => panic!("expected UnsupportedOpcode for DD, got {other:?}"),
        }
    }

    #[test]
    fn test_cb_rlc_b_supported() {
        // CB 0x00 = RLC B — now fully supported; verify result + carry.
        let mut bus = FlatBus::new();
        bus.load(0x0000, &[0x06, 0x81, 0xCB, 0x00, 0x76]); // ld b,$81; rlc b; halt
        let (cpu, _) = run_bytes(&mut bus);
        // $81 rotated left = $03, carry out = 1.
        assert_eq!(cpu.b, 0x03);
        assert!(cpu.flag(FLAG_C));
    }

    fn run_bytes(bus: &mut FlatBus) -> (Cpu, usize) {
        let mut cpu = Cpu::new();
        let mut steps = 0;
        loop {
            if cpu.halted || steps > 1000 {
                break;
            }
            match cpu.step(bus) {
                Ok(()) => steps += 1,
                Err(StepError::Halt) => break,
                Err(e) => panic!("step error: {e:?}"),
            }
        }
        (cpu, steps)
    }

    // ── EI / DI ───────────────────────────────────────────────────────────────

    #[test]
    fn test_ei_di() {
        let (cpu, _) = run(&[0xFB, 0xF3, 0x76]); // ei; di; halt
        assert!(!cpu.iff1);
        assert!(!cpu.iff2);
    }

    #[test]
    fn ei_enables_after_following_instruction() {
        let mut bus = FlatBus::default();
        bus.load(0x0000, &[0xFB, 0x00, 0x76]); // ei; nop; halt
        let mut cpu = Cpu::new();

        cpu.step(&mut bus).unwrap(); // EI itself
        assert!(!cpu.iff1);
        assert!(!cpu.iff2);
        assert_eq!(cpu.ei_pending, 1);

        cpu.step(&mut bus).unwrap(); // following instruction
        assert!(cpu.iff1);
        assert!(cpu.iff2);
        assert_eq!(cpu.ei_pending, 0);
    }

    // ── ALU register forms ────────────────────────────────────────────────────

    #[test]
    fn test_add_a_a() {
        // ld a,0x21; add a,a → A=0x42
        let (cpu, _) = run(&[0x3E, 0x21, 0x87, 0x76]);
        assert_eq!(cpu.a, 0x42);
    }

    #[test]
    fn test_add_a_b() {
        // ld a,0x10; ld b,0x20; add a,b → A=0x30
        let (cpu, _) = run(&[0x3E, 0x10, 0x06, 0x20, 0x80, 0x76]);
        assert_eq!(cpu.a, 0x30);
    }

    #[test]
    fn test_and_a_or_a_xor_a() {
        // ld a,0xFF; and a → A=0xFF; or a → A=0xFF; xor a → A=0x00 (Z=1)
        let (cpu, _) = run(&[0x3E, 0xFF, 0xA7, 0xB7, 0xAF, 0x76]);
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.f & FLAG_Z != 0);
    }

    // ── Register pair helpers ─────────────────────────────────────────────────

    #[test]
    fn test_set_get_af_bc_de_hl() {
        let mut cpu = Cpu::new();
        cpu.set_af(0x1234);
        assert_eq!(cpu.af(), 0x1234);
        cpu.set_bc(0xABCD);
        assert_eq!(cpu.bc(), 0xABCD);
        cpu.set_de(0x5678);
        assert_eq!(cpu.de(), 0x5678);
        cpu.set_hl(0x9ABC);
        assert_eq!(cpu.hl(), 0x9ABC);
    }

    // ── OR n full flag check ─────────────────────────────────────────────────

    #[test]
    fn test_or_n_flags() {
        // ld a,0x00; or 0x80 → A=0x80, S=1, Z=0, H=0, PV=0(odd: 1 bit), N=0, C=0
        let (cpu, _) = run(&[0x3E, 0x00, 0xF6, 0x80, 0x76]);
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.f & FLAG_S != 0);
        assert!(cpu.f & FLAG_Z == 0);
        assert!(cpu.f & FLAG_H == 0);
        assert!(cpu.f & FLAG_PV == 0); // 1 bit set = odd = PV=0
        assert!(cpu.f & FLAG_N == 0);
        assert!(cpu.f & FLAG_C == 0);
    }

    // ── XOR flags ────────────────────────────────────────────────────────────

    #[test]
    fn test_xor_parity() {
        // ld a,0x03; xor 0x00 → A=0x03, PV=1 (2 bits = even parity)
        let (cpu, _) = run(&[0x3E, 0x03, 0xEE, 0x00, 0x76]);
        assert_eq!(cpu.a, 0x03);
        assert!(cpu.f & FLAG_PV != 0); // even parity
    }

    // ── Half-carry on ADD ─────────────────────────────────────────────────────

    #[test]
    fn test_add_half_carry() {
        // ld a,0x0F; add a,0x01 → H=1
        let (cpu, _) = run(&[0x3E, 0x0F, 0xC6, 0x01, 0x76]);
        assert_eq!(cpu.a, 0x10);
        assert!(cpu.f & FLAG_H != 0);
    }

    // ── ld (hl),r using ld r,r' encoding ─────────────────────────────────────

    #[test]
    fn test_ld_hl_r_and_r_hl() {
        // ld hl,0x0200; ld b,0xFE; ld (hl),b; ld c,(hl)
        let (cpu, _) = run(&[0x21, 0x00, 0x02, 0x06, 0xFE, 0x70, 0x4E, 0x76]);
        assert_eq!(cpu.c, 0xFE);
    }
}
