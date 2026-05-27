//! NMOS 6502 / Ricoh 2A03 interpreter — differential-test oracle.
//!
//! Instruction-accurate; no cycle accuracy. Decimal mode is a no-op (2A03).
//! Implements all official opcodes plus stable unofficials used by SMB.

#![allow(clippy::upper_case_acronyms)]

use cpu6502::{AddrMode, DecodeError, Mnemonic, OPCODE_TABLE, Operand, decode_at};

// ---------------------------------------------------------------------------
// Status flags
// ---------------------------------------------------------------------------

pub const FLAG_C: u8 = 0x01;
pub const FLAG_Z: u8 = 0x02;
pub const FLAG_I: u8 = 0x04;
pub const FLAG_D: u8 = 0x08;
pub const FLAG_B: u8 = 0x10;
pub const FLAG_U: u8 = 0x20;
pub const FLAG_V: u8 = 0x40;
pub const FLAG_N: u8 = 0x80;

// ---------------------------------------------------------------------------
// Bus trait
// ---------------------------------------------------------------------------

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    fn write(&mut self, addr: u16, value: u8);
}

// ---------------------------------------------------------------------------
// FlatBus — simple 64 KiB RAM, useful for tests
// ---------------------------------------------------------------------------

pub struct FlatBus {
    pub ram: Box<[u8; 0x10000]>,
}

impl FlatBus {
    pub fn new() -> Self {
        Self {
            ram: Box::new([0u8; 0x10000]),
        }
    }

    pub fn load(&mut self, addr: u16, bytes: &[u8]) {
        let start = addr as usize;
        let end = start + bytes.len();
        self.ram[start..end].copy_from_slice(bytes);
    }
}

impl Default for FlatBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for FlatBus {
    fn read(&mut self, addr: u16) -> u8 {
        self.ram[addr as usize]
    }
    fn write(&mut self, addr: u16, value: u8) {
        self.ram[addr as usize] = value;
    }
}

// ---------------------------------------------------------------------------
// CPU state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cpu {
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub sp: u8,
    pub pc: u16,
    pub p: u8,
}

impl Cpu {
    pub fn new() -> Self {
        Cpu {
            a: 0,
            x: 0,
            y: 0,
            sp: 0xFD,
            pc: 0,
            p: FLAG_I | FLAG_U,
        }
    }

    pub fn reset(&mut self, bus: &mut impl Bus) {
        self.pc = read16(bus, 0xFFFC);
    }

    pub fn nmi(&mut self, bus: &mut impl Bus) {
        let pc = self.pc;
        let p = self.p;
        self.push16(bus, pc);
        self.push(bus, (p & !FLAG_B) | FLAG_U);
        self.p |= FLAG_I;
        self.pc = read16(bus, 0xFFFA);
    }

    pub fn irq(&mut self, bus: &mut impl Bus) {
        if self.p & FLAG_I != 0 {
            return;
        }
        let pc = self.pc;
        let p = self.p;
        self.push16(bus, pc);
        self.push(bus, (p & !FLAG_B) | FLAG_U);
        self.p |= FLAG_I;
        self.pc = read16(bus, 0xFFFE);
    }

    pub fn step(&mut self, bus: &mut impl Bus) -> Result<StepInfo, StepError> {
        let pc_before = self.pc;
        let opcode = bus.read(self.pc);
        let _info = OPCODE_TABLE[opcode as usize];

        // Fetch raw bytes for the decoder (opcode + up to 2 operand bytes).
        let b0 = opcode;
        let b1 = bus.read(self.pc.wrapping_add(1));
        let b2 = bus.read(self.pc.wrapping_add(2));
        let raw = [b0, b1, b2];

        let insn = decode_at(&raw, self.pc, 0).map_err(StepError::DecodeError)?;

        // Reject JAM opcodes before advancing PC.
        if insn.mnemonic == Mnemonic::JAM {
            return Err(StepError::Jam {
                pc: pc_before,
                opcode,
            });
        }

        // Reject unstable opcodes.
        match opcode {
            // ANC
            0x0B | 0x2B |
            // ALR
            0x4B |
            // ARR
            0x6B |
            // ANE / XAA
            0x8B |
            // AHX (both modes)
            0x93 | 0x9F |
            // TAS
            0x9B |
            // SHY
            0x9C |
            // SHX
            0x9E |
            // LAS
            0xBB |
            // AXS / SBX
            0xCB => return Err(StepError::UnstableOpcode { pc: pc_before, opcode }),
            _ => {}
        }

        // Advance PC past the instruction.
        self.pc = self.pc.wrapping_add(insn.size as u16);

        // Resolve effective address / value.
        let ea: Option<u16> = self.resolve_ea(bus, insn.mode, insn.operand);

        // Execute the instruction.
        self.execute(bus, insn.mnemonic, insn.mode, insn.operand, ea, pc_before)?;

        Ok(StepInfo {
            pc_before,
            opcode,
            bytes: insn.size,
        })
    }

    /// Run until the first RTS that returns to the caller's level.
    ///
    /// Tracks JSR/RTS nesting depth starting at 0. Stops when an RTS would
    /// bring depth below 0 (i.e., the first RTS that "returns" to the caller).
    /// The standard usage pattern: push a sentinel return address before
    /// setting PC to the routine, then call this method.
    pub fn run_until_rts(
        &mut self,
        bus: &mut impl Bus,
        max_steps: usize,
    ) -> Result<usize, StepError> {
        let mut depth: i32 = 0;
        let mut steps = 0usize;
        loop {
            if steps >= max_steps {
                return Err(StepError::StepLimitExceeded);
            }
            let info = OPCODE_TABLE[bus.read(self.pc) as usize];
            let is_jsr = info.mnemonic == Mnemonic::JSR;
            let is_rts = info.mnemonic == Mnemonic::RTS;
            self.step(bus)?;
            steps += 1;
            if is_jsr {
                depth += 1;
            } else if is_rts {
                if depth <= 0 {
                    return Ok(steps);
                }
                depth -= 1;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn flag(&self, f: u8) -> bool {
        self.p & f != 0
    }

    fn set_flag(&mut self, f: u8, v: bool) {
        if v {
            self.p |= f;
        } else {
            self.p &= !f;
        }
    }

    fn set_nz(&mut self, v: u8) {
        self.set_flag(FLAG_N, v & 0x80 != 0);
        self.set_flag(FLAG_Z, v == 0);
    }

    fn push(&mut self, bus: &mut impl Bus, val: u8) {
        bus.write(0x0100 | self.sp as u16, val);
        self.sp = self.sp.wrapping_sub(1);
    }

    fn pop(&mut self, bus: &mut impl Bus) -> u8 {
        self.sp = self.sp.wrapping_add(1);
        bus.read(0x0100 | self.sp as u16)
    }

    fn push16(&mut self, bus: &mut impl Bus, val: u16) {
        self.push(bus, (val >> 8) as u8);
        self.push(bus, val as u8);
    }

    fn pop16(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = self.pop(bus) as u16;
        let hi = self.pop(bus) as u16;
        (hi << 8) | lo
    }

    /// Resolve the effective address for the given mode/operand.
    /// Returns None only for Implied / Accumulator.
    fn resolve_ea(&self, bus: &mut impl Bus, mode: AddrMode, operand: Operand) -> Option<u16> {
        match mode {
            AddrMode::Implied | AddrMode::Accumulator => None,
            AddrMode::Immediate => {
                // Immediate: the operand byte is the value; no memory address needed.
                None
            }
            AddrMode::ZeroPage => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(a & 0x00FF)
            }
            AddrMode::ZeroPageX => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(((a as u8).wrapping_add(self.x)) as u16)
            }
            AddrMode::ZeroPageY => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(((a as u8).wrapping_add(self.y)) as u16)
            }
            AddrMode::Absolute => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(a)
            }
            AddrMode::AbsoluteX => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(a.wrapping_add(self.x as u16))
            }
            AddrMode::AbsoluteY => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                Some(a.wrapping_add(self.y as u16))
            }
            AddrMode::Indirect => {
                // NMOS page-wrap bug: high byte reads from same page if low=0xFF.
                let Operand::Addr(ptr) = operand else {
                    return None;
                };
                let lo = bus.read(ptr) as u16;
                let hi_addr = if ptr & 0xFF == 0xFF {
                    ptr & 0xFF00
                } else {
                    ptr + 1
                };
                let hi = bus.read(hi_addr) as u16;
                Some((hi << 8) | lo)
            }
            AddrMode::IndirectX => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                let ptr = (a as u8).wrapping_add(self.x) as u16;
                Some(read16_zp(bus, ptr))
            }
            AddrMode::IndirectY => {
                let Operand::Addr(a) = operand else {
                    return None;
                };
                let base = read16_zp(bus, a & 0xFF);
                Some(base.wrapping_add(self.y as u16))
            }
            AddrMode::Relative => None,
        }
    }

    fn read_operand(
        &self,
        bus: &mut impl Bus,
        mode: AddrMode,
        operand: Operand,
        ea: Option<u16>,
    ) -> u8 {
        match mode {
            AddrMode::Immediate => {
                if let Operand::Imm(v) = operand {
                    v
                } else {
                    0
                }
            }
            AddrMode::Accumulator => self.a,
            _ => bus.read(ea.unwrap_or(0)),
        }
    }

    fn do_adc(&mut self, m: u8) {
        let c = (self.p & FLAG_C) as u16;
        let a = self.a as u16;
        let m16 = m as u16;
        let t = a + m16 + c;
        let result = (t & 0xFF) as u8;
        self.set_flag(FLAG_C, t > 0xFF);
        self.set_flag(FLAG_V, (!(a ^ m16) & (a ^ t) & 0x80) != 0);
        self.a = result;
        self.set_nz(self.a);
    }

    fn do_sbc(&mut self, m: u8) {
        self.do_adc(m ^ 0xFF);
    }

    fn do_cmp(&mut self, reg: u8, m: u8) {
        let t = (reg as i16) - (m as i16);
        self.set_flag(FLAG_C, reg >= m);
        self.set_flag(FLAG_N, (t as u8) & 0x80 != 0);
        self.set_flag(FLAG_Z, t == 0);
    }

    fn do_asl_mem(&mut self, bus: &mut impl Bus, ea: u16) {
        let v = bus.read(ea);
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = v << 1;
        bus.write(ea, r);
        self.set_nz(r);
    }

    fn do_lsr_mem(&mut self, bus: &mut impl Bus, ea: u16) {
        let v = bus.read(ea);
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = v >> 1;
        bus.write(ea, r);
        self.set_nz(r);
    }

    fn do_rol_mem(&mut self, bus: &mut impl Bus, ea: u16) {
        let v = bus.read(ea);
        let old_c = (self.p & FLAG_C) != 0;
        self.set_flag(FLAG_C, v & 0x80 != 0);
        let r = (v << 1) | (old_c as u8);
        bus.write(ea, r);
        self.set_nz(r);
    }

    fn do_ror_mem(&mut self, bus: &mut impl Bus, ea: u16) {
        let v = bus.read(ea);
        let old_c = (self.p & FLAG_C) != 0;
        self.set_flag(FLAG_C, v & 0x01 != 0);
        let r = (v >> 1) | ((old_c as u8) << 7);
        bus.write(ea, r);
        self.set_nz(r);
    }

    fn execute(
        &mut self,
        bus: &mut impl Bus,
        mnemonic: Mnemonic,
        mode: AddrMode,
        operand: Operand,
        ea: Option<u16>,
        _pc_before: u16,
    ) -> Result<(), StepError> {
        use Mnemonic::*;
        match mnemonic {
            // --- Load / Store ---
            LDA => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.a = v;
                self.set_nz(self.a);
            }
            LDX => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.x = v;
                self.set_nz(self.x);
            }
            LDY => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.y = v;
                self.set_nz(self.y);
            }
            STA => {
                bus.write(ea.unwrap(), self.a);
            }
            STX => {
                bus.write(ea.unwrap(), self.x);
            }
            STY => {
                bus.write(ea.unwrap(), self.y);
            }

            // --- Transfer ---
            TAX => {
                self.x = self.a;
                self.set_nz(self.x);
            }
            TAY => {
                self.y = self.a;
                self.set_nz(self.y);
            }
            TXA => {
                self.a = self.x;
                self.set_nz(self.a);
            }
            TYA => {
                self.a = self.y;
                self.set_nz(self.a);
            }
            TSX => {
                self.x = self.sp;
                self.set_nz(self.x);
            }
            TXS => {
                self.sp = self.x;
            } // no flags

            // --- Stack ---
            PHA => {
                let a = self.a;
                self.push(bus, a);
            }
            PHP => {
                let p = self.p | FLAG_B | FLAG_U;
                self.push(bus, p);
            }
            PLA => {
                self.a = self.pop(bus);
                self.set_nz(self.a);
            }
            PLP => {
                let v = self.pop(bus);
                self.p = (v & !FLAG_B) | FLAG_U;
            }

            // --- Logical ---
            AND => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.a &= v;
                self.set_nz(self.a);
            }
            ORA => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.a |= v;
                self.set_nz(self.a);
            }
            EOR => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.a ^= v;
                self.set_nz(self.a);
            }
            BIT => {
                let m = bus.read(ea.unwrap());
                self.set_flag(FLAG_N, m & 0x80 != 0);
                self.set_flag(FLAG_V, m & 0x40 != 0);
                self.set_flag(FLAG_Z, self.a & m == 0);
            }

            // --- Arithmetic ---
            ADC => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.do_adc(v);
            }
            SBC => {
                let v = self.read_operand(bus, mode, operand, ea);
                self.do_sbc(v);
            }
            CMP => {
                let v = self.read_operand(bus, mode, operand, ea);
                let a = self.a;
                self.do_cmp(a, v);
            }
            CPX => {
                let v = self.read_operand(bus, mode, operand, ea);
                let x = self.x;
                self.do_cmp(x, v);
            }
            CPY => {
                let v = self.read_operand(bus, mode, operand, ea);
                let y = self.y;
                self.do_cmp(y, v);
            }

            // --- Shifts / Rotates ---
            ASL => {
                if mode == AddrMode::Accumulator {
                    self.set_flag(FLAG_C, self.a & 0x80 != 0);
                    self.a <<= 1;
                    let a = self.a;
                    self.set_nz(a);
                } else {
                    self.do_asl_mem(bus, ea.unwrap());
                }
            }
            LSR => {
                if mode == AddrMode::Accumulator {
                    self.set_flag(FLAG_C, self.a & 0x01 != 0);
                    self.a >>= 1;
                    let a = self.a;
                    self.set_nz(a);
                } else {
                    self.do_lsr_mem(bus, ea.unwrap());
                }
            }
            ROL => {
                if mode == AddrMode::Accumulator {
                    let old_c = (self.p & FLAG_C) != 0;
                    self.set_flag(FLAG_C, self.a & 0x80 != 0);
                    self.a = (self.a << 1) | (old_c as u8);
                    let a = self.a;
                    self.set_nz(a);
                } else {
                    self.do_rol_mem(bus, ea.unwrap());
                }
            }
            ROR => {
                if mode == AddrMode::Accumulator {
                    let old_c = (self.p & FLAG_C) != 0;
                    self.set_flag(FLAG_C, self.a & 0x01 != 0);
                    self.a = (self.a >> 1) | ((old_c as u8) << 7);
                    let a = self.a;
                    self.set_nz(a);
                } else {
                    self.do_ror_mem(bus, ea.unwrap());
                }
            }

            // --- Increment / Decrement ---
            INC => {
                let addr = ea.unwrap();
                let v = bus.read(addr).wrapping_add(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            DEC => {
                let addr = ea.unwrap();
                let v = bus.read(addr).wrapping_sub(1);
                bus.write(addr, v);
                self.set_nz(v);
            }
            INX => {
                self.x = self.x.wrapping_add(1);
                let v = self.x;
                self.set_nz(v);
            }
            INY => {
                self.y = self.y.wrapping_add(1);
                let v = self.y;
                self.set_nz(v);
            }
            DEX => {
                self.x = self.x.wrapping_sub(1);
                let v = self.x;
                self.set_nz(v);
            }
            DEY => {
                self.y = self.y.wrapping_sub(1);
                let v = self.y;
                self.set_nz(v);
            }

            // --- Branches ---
            BCC => self.branch(operand, !self.flag(FLAG_C)),
            BCS => self.branch(operand, self.flag(FLAG_C)),
            BEQ => self.branch(operand, self.flag(FLAG_Z)),
            BNE => self.branch(operand, !self.flag(FLAG_Z)),
            BMI => self.branch(operand, self.flag(FLAG_N)),
            BPL => self.branch(operand, !self.flag(FLAG_N)),
            BVC => self.branch(operand, !self.flag(FLAG_V)),
            BVS => self.branch(operand, self.flag(FLAG_V)),

            // --- Jumps / Calls ---
            JMP => {
                self.pc = ea.unwrap();
            }
            JSR => {
                // Push PC-1 (i.e., the byte before the next instruction = current PC - 1).
                // At this point self.pc has already been advanced past JSR (3 bytes),
                // so we push self.pc - 1.
                let ret = self.pc.wrapping_sub(1);
                self.push16(bus, ret);
                self.pc = ea.unwrap();
            }
            RTS => {
                let ret = self.pop16(bus);
                self.pc = ret.wrapping_add(1);
            }
            RTI => {
                let p = self.pop(bus);
                self.p = (p & !FLAG_B) | FLAG_U;
                self.pc = self.pop16(bus);
            }
            BRK => {
                // PC was already advanced by 2 (opcode + padding byte).
                let pc = self.pc;
                self.push16(bus, pc);
                let p = self.p | FLAG_B | FLAG_U;
                self.push(bus, p);
                self.p |= FLAG_I;
                self.pc = read16(bus, 0xFFFE);
            }

            // --- Flag ops ---
            CLC => self.p &= !FLAG_C,
            SEC => self.p |= FLAG_C,
            CLI => self.p &= !FLAG_I,
            SEI => self.p |= FLAG_I,
            CLV => self.p &= !FLAG_V,
            CLD => self.p &= !FLAG_D,
            SED => self.p |= FLAG_D,

            // --- NOP (official + unofficial) ---
            NOP => {
                // For non-implied NOPs, perform the read so memory-mapped I/O sees it,
                // but discard the result.
                if let Some(addr) = ea {
                    let _ = bus.read(addr);
                }
            }

            // --- Unofficial stable ---
            LAX => {
                // LAX #imm ($AB) is also here; mode == Immediate.
                let v = self.read_operand(bus, mode, operand, ea);
                self.a = v;
                self.x = v;
                self.set_nz(v);
            }
            SAX => {
                // Write A & X to effective address; no flags.
                bus.write(ea.unwrap(), self.a & self.x);
            }
            DCP => {
                // DEC memory, then CMP.
                let addr = ea.unwrap();
                let m = bus.read(addr).wrapping_sub(1);
                bus.write(addr, m);
                let a = self.a;
                self.do_cmp(a, m);
            }
            ISC => {
                // INC memory, then SBC.
                let addr = ea.unwrap();
                let m = bus.read(addr).wrapping_add(1);
                bus.write(addr, m);
                self.do_sbc(m);
            }
            SLO => {
                // ASL memory, then ORA A.
                let addr = ea.unwrap();
                let v = bus.read(addr);
                self.set_flag(FLAG_C, v & 0x80 != 0);
                let shifted = v << 1;
                bus.write(addr, shifted);
                self.a |= shifted;
                self.set_nz(self.a);
            }
            RLA => {
                // ROL memory, then AND A.
                let addr = ea.unwrap();
                let v = bus.read(addr);
                let old_c = (self.p & FLAG_C) != 0;
                self.set_flag(FLAG_C, v & 0x80 != 0);
                let rotated = (v << 1) | (old_c as u8);
                bus.write(addr, rotated);
                self.a &= rotated;
                self.set_nz(self.a);
            }
            SRE => {
                // LSR memory, then EOR A.
                let addr = ea.unwrap();
                let v = bus.read(addr);
                self.set_flag(FLAG_C, v & 0x01 != 0);
                let shifted = v >> 1;
                bus.write(addr, shifted);
                self.a ^= shifted;
                self.set_nz(self.a);
            }
            RRA => {
                // ROR memory, then ADC A.
                let addr = ea.unwrap();
                let v = bus.read(addr);
                let old_c = (self.p & FLAG_C) != 0;
                self.set_flag(FLAG_C, v & 0x01 != 0);
                let rotated = (v >> 1) | ((old_c as u8) << 7);
                bus.write(addr, rotated);
                self.do_adc(rotated);
            }

            // Unstable — caught earlier, but exhaust the match.
            XAA | AHX | TAS | LAS | SHX | SHY | ANE => {
                unreachable!("unstable opcode should have been caught before execute")
            }
            JAM => unreachable!("JAM should have been caught before execute"),
        }
        Ok(())
    }

    fn branch(&mut self, operand: Operand, taken: bool) {
        if taken {
            let Operand::Relative(rel) = operand else {
                return;
            };
            self.pc = self.pc.wrapping_add(rel as i16 as u16);
        }
    }
}

impl Default for Cpu {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Step result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct StepInfo {
    pub pc_before: u16,
    pub opcode: u8,
    pub bytes: u8,
}

#[derive(Debug)]
pub enum StepError {
    UnstableOpcode { pc: u16, opcode: u8 },
    Jam { pc: u16, opcode: u8 },
    DecodeError(DecodeError),
    Halt,
    StepLimitExceeded,
}

impl std::fmt::Display for StepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepError::UnstableOpcode { pc, opcode } => {
                write!(f, "unstable opcode ${opcode:02X} at ${pc:04X}")
            }
            StepError::Jam { pc, opcode } => {
                write!(f, "JAM opcode ${opcode:02X} at ${pc:04X}")
            }
            StepError::DecodeError(e) => write!(f, "decode error: {e}"),
            StepError::Halt => write!(f, "halt"),
            StepError::StepLimitExceeded => write!(f, "step limit exceeded"),
        }
    }
}

impl std::error::Error for StepError {}

// ---------------------------------------------------------------------------
// Bus helpers
// ---------------------------------------------------------------------------

fn read16(bus: &mut impl Bus, addr: u16) -> u16 {
    let lo = bus.read(addr) as u16;
    let hi = bus.read(addr.wrapping_add(1)) as u16;
    (hi << 8) | lo
}

/// Read a 16-bit pointer from zero page with zero-page wrap on the high byte.
fn read16_zp(bus: &mut impl Bus, ptr: u16) -> u16 {
    let lo = bus.read(ptr & 0xFF) as u16;
    let hi = bus.read(ptr.wrapping_add(1) & 0xFF) as u16;
    (hi << 8) | lo
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_with_bus(program: &[u8], load_at: u16) -> (Cpu, FlatBus) {
        let mut bus = FlatBus::new();
        bus.load(load_at, program);
        // Reset vector → load_at
        bus.ram[0xFFFC] = load_at as u8;
        bus.ram[0xFFFD] = (load_at >> 8) as u8;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        (cpu, bus)
    }

    // ------------------------------------------------------------------
    // LDA / STA / LDX / LDY
    // ------------------------------------------------------------------

    #[test]
    fn lda_immediate() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xA9, 0x42, 0xEA], 0x8000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x42);
        assert!(!cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn lda_zero_page() {
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0xAB;
        bus.load(0x8000, &[0xA5, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0xAB);
        assert!(cpu.flag(FLAG_N));
    }

    #[test]
    fn lda_absolute() {
        let mut bus = FlatBus::new();
        bus.ram[0x1234] = 0x55;
        bus.load(0x8000, &[0xAD, 0x34, 0x12]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x55);
    }

    #[test]
    fn lda_absolute_x_page_cross() {
        let mut bus = FlatBus::new();
        bus.ram[0x0100] = 0x77; // $00FF + 1 = $0100
        bus.load(0x8000, &[0xBD, 0xFF, 0x00]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.x = 0x01;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x77);
    }

    #[test]
    fn sta_zero_page() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x85, 0x20], 0x8000);
        cpu.a = 0xBE;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x20], 0xBE);
    }

    #[test]
    fn ldx_ldy_transfer_flags() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xA2, 0x00, 0xA0, 0xFF], 0x8000);
        cpu.step(&mut bus).unwrap(); // LDX #$00
        assert!(cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
        cpu.step(&mut bus).unwrap(); // LDY #$FF
        assert!(!cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_N));
    }

    #[test]
    fn transfer_ops_flags() {
        // TAX sets N and Z
        let (mut cpu, mut bus) = cpu_with_bus(&[0xAA, 0xA8, 0x8A, 0x98], 0x8000);
        cpu.a = 0x80;
        cpu.step(&mut bus).unwrap(); // TAX
        assert_eq!(cpu.x, 0x80);
        assert!(cpu.flag(FLAG_N));
        cpu.a = 0x00;
        cpu.step(&mut bus).unwrap(); // TAY
        assert_eq!(cpu.y, 0x00);
        assert!(cpu.flag(FLAG_Z));
        cpu.x = 0x42;
        cpu.step(&mut bus).unwrap(); // TXA
        assert_eq!(cpu.a, 0x42);
        cpu.step(&mut bus).unwrap(); // TYA
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
    }

    // ------------------------------------------------------------------
    // ADC / SBC overflow tests
    // ------------------------------------------------------------------

    #[test]
    fn adc_overflow_positive() {
        // 0x7F + 0x01 → 0x80, V=1 (signed overflow)
        let (mut cpu, mut bus) = cpu_with_bus(&[0x69, 0x01], 0x8000);
        cpu.a = 0x7F;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.flag(FLAG_V));
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_C));
    }

    #[test]
    fn adc_overflow_negative() {
        // 0x80 + 0xFF → 0x7F, V=1, C=1
        let (mut cpu, mut bus) = cpu_with_bus(&[0x69, 0xFF], 0x8000);
        cpu.a = 0x80;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x7F);
        assert!(cpu.flag(FLAG_V));
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn adc_overflow_50_plus_50() {
        // 0x50 + 0x50 → 0xA0, V=1
        let (mut cpu, mut bus) = cpu_with_bus(&[0x69, 0x50], 0x8000);
        cpu.a = 0x50;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0xA0);
        assert!(cpu.flag(FLAG_V));
    }

    #[test]
    fn adc_carry_zero() {
        // 0xFF + 0x01 → 0x00, Z=1, C=1
        let (mut cpu, mut bus) = cpu_with_bus(&[0x69, 0x01], 0x8000);
        cpu.a = 0xFF;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_V));
    }

    #[test]
    fn sbc_basic() {
        // 0x50 - 0x10 = 0x40, C set (borrow clear)
        let (mut cpu, mut bus) = cpu_with_bus(&[0xE9, 0x10], 0x8000);
        cpu.a = 0x50;
        cpu.p |= FLAG_C; // SEC
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x40);
        assert!(cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_V));
    }

    // ------------------------------------------------------------------
    // CMP
    // ------------------------------------------------------------------

    #[test]
    fn cmp_equal() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xC9, 0x42], 0x8000);
        cpu.a = 0x42;
        cpu.step(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_N));
    }

    #[test]
    fn cmp_less() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xC9, 0x80], 0x8000);
        cpu.a = 0x10;
        cpu.step(&mut bus).unwrap();
        assert!(!cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_Z));
    }

    #[test]
    fn cmp_greater() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xC9, 0x10], 0x8000);
        cpu.a = 0x80;
        cpu.step(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_C));
        assert!(!cpu.flag(FLAG_Z));
    }

    // ------------------------------------------------------------------
    // Shifts / Rotates
    // ------------------------------------------------------------------

    #[test]
    fn asl_accumulator() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x0A], 0x8000);
        cpu.a = 0x81;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x02);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn lsr_zero_page() {
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x03;
        bus.load(0x8000, &[0x46, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x01);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn rol_ror_carry_chain() {
        // ROL A with carry=1: shifts bit 7 out to carry, old carry in at bit 0
        let (mut cpu, mut bus) = cpu_with_bus(&[0x2A, 0x6A], 0x8000);
        cpu.a = 0x80;
        cpu.p |= FLAG_C; // carry=1
        cpu.step(&mut bus).unwrap(); // ROL A
        assert_eq!(cpu.a, 0x01); // 0x80 << 1 | 1 = 0x01, carry = 1
        assert!(cpu.flag(FLAG_C));
        cpu.step(&mut bus).unwrap(); // ROR A
        // 0x01 >> 1 | (carry=1 << 7) = 0x80, new carry = 1
        assert_eq!(cpu.a, 0x80);
        assert!(cpu.flag(FLAG_C));
    }

    // ------------------------------------------------------------------
    // BIT
    // ------------------------------------------------------------------

    #[test]
    fn bit_flags() {
        let mut bus = FlatBus::new();
        bus.ram[0x20] = 0b1100_0000; // bit7=1, bit6=1
        bus.load(0x8000, &[0x24, 0x20]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x00; // A & M == 0 → Z=1
        cpu.step(&mut bus).unwrap();
        assert!(cpu.flag(FLAG_N));
        assert!(cpu.flag(FLAG_V));
        assert!(cpu.flag(FLAG_Z));
    }

    #[test]
    fn bit_no_z_when_match() {
        let mut bus = FlatBus::new();
        bus.ram[0x20] = 0x0F;
        bus.load(0x8000, &[0x24, 0x20]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x0F;
        cpu.step(&mut bus).unwrap();
        assert!(!cpu.flag(FLAG_Z));
        assert!(!cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_V));
    }

    // ------------------------------------------------------------------
    // Branches
    // ------------------------------------------------------------------

    #[test]
    fn bne_taken() {
        // BNE $02 from $8000 → should jump to $8004
        let (mut cpu, mut bus) = cpu_with_bus(&[0xD0, 0x02, 0xEA, 0xEA, 0xEA], 0x8000);
        cpu.p &= !FLAG_Z;
        let info = cpu.step(&mut bus).unwrap();
        assert_eq!(info.pc_before, 0x8000);
        assert_eq!(cpu.pc, 0x8004);
    }

    #[test]
    fn bne_not_taken() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xD0, 0x10], 0x8000);
        cpu.p |= FLAG_Z;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.pc, 0x8002);
    }

    #[test]
    fn bpl_negative_offset() {
        // BPL $FE from $8000: offset -2, PC after fetch = $8002, target = $8000
        let (mut cpu, mut bus) = cpu_with_bus(&[0x10, 0xFE], 0x8000);
        cpu.p &= !FLAG_N;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.pc, 0x8000);
    }

    // ------------------------------------------------------------------
    // JMP indirect page-wrap bug
    // ------------------------------------------------------------------

    #[test]
    fn jmp_indirect_page_wrap_bug() {
        let mut bus = FlatBus::new();
        // JMP ($30FF): should read $30FF for lo, $3000 for hi (not $3100)
        bus.ram[0x30FF] = 0x03;
        bus.ram[0x3000] = 0xAA; // hi byte
        bus.ram[0x3100] = 0xFF; // this should NOT be read
        bus.load(0x8000, &[0x6C, 0xFF, 0x30]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.pc, 0xAA03);
    }

    // ------------------------------------------------------------------
    // JSR / RTS / RTI
    // ------------------------------------------------------------------

    #[test]
    fn jsr_rts_roundtrip() {
        // JSR $8005; NOP; NOP; NOP; RTS
        // After JSR, PC = $8005; after RTS, PC = $8003 (past JSR)
        let mut bus = FlatBus::new();
        bus.load(
            0x8000,
            &[
                0x20, 0x05, 0x80, // JSR $8005
                0xEA, // NOP  ($8003)
                0xEA, // NOP  ($8004)
                0x60, // RTS  ($8005)
            ],
        );
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap(); // JSR
        assert_eq!(cpu.pc, 0x8005);
        cpu.step(&mut bus).unwrap(); // RTS
        assert_eq!(cpu.pc, 0x8003);
    }

    #[test]
    fn rti_restores_pc_and_p() {
        let mut bus = FlatBus::new();
        bus.load(0x8000, &[0x40]); // RTI at $8000
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        // Manually push return state: P then PClo then PChi (stack is lifo)
        cpu.push16(&mut bus, 0x1234); // push PC = $1234
        cpu.push(&mut bus, 0b1010_0001); // push P with FLAG_C|FLAG_N (B cleared, U set)
        cpu.step(&mut bus).unwrap(); // RTI
        assert_eq!(cpu.pc, 0x1234);
        // B cleared, U set; C and N should be set
        assert!(cpu.flag(FLAG_C));
        assert!(cpu.flag(FLAG_N));
        assert!(!cpu.flag(FLAG_B));
        assert!(cpu.flag(FLAG_U));
    }

    // ------------------------------------------------------------------
    // Stack: PHA / PLA / PHP / PLP
    // ------------------------------------------------------------------

    #[test]
    fn pha_pla() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x48, 0x68], 0x8000);
        cpu.a = 0xDE;
        cpu.step(&mut bus).unwrap(); // PHA
        cpu.a = 0x00;
        cpu.step(&mut bus).unwrap(); // PLA
        assert_eq!(cpu.a, 0xDE);
        assert!(cpu.flag(FLAG_N));
    }

    #[test]
    fn php_plp() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x08, 0x28], 0x8000);
        cpu.p = 0b0000_0001; // only C set
        cpu.step(&mut bus).unwrap(); // PHP — pushes P | B | U
        let pushed = bus.ram[0x01FD]; // sp was 0xFD, decremented to 0xFC after push
        assert_eq!(pushed & FLAG_B, FLAG_B);
        assert_eq!(pushed & FLAG_U, FLAG_U);
        cpu.p = 0xFF;
        cpu.step(&mut bus).unwrap(); // PLP — should clear B, set U
        assert!(!cpu.flag(FLAG_B));
        assert!(cpu.flag(FLAG_U));
    }

    // ------------------------------------------------------------------
    // Indexed Indirect (zp,X) and Indirect Indexed (zp),Y
    // ------------------------------------------------------------------

    #[test]
    fn indexed_indirect_zpx_wrap() {
        // LDA ($FE,X) with X=2 → ptr at ($FE+2)&$FF = $00
        // $0000/$0001 hold address $1234, $1234 holds $99
        let mut bus = FlatBus::new();
        bus.ram[0x00] = 0x34;
        bus.ram[0x01] = 0x12;
        bus.ram[0x1234] = 0x99;
        bus.load(0x8000, &[0xA1, 0xFE]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.x = 0x02;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x99);
    }

    #[test]
    fn indirect_indexed_zpy_carry() {
        // LDA ($10),Y with $10/$11 = $00FF, Y=2 → effective addr $0101
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0xFF;
        bus.ram[0x11] = 0x00;
        bus.ram[0x0101] = 0x77;
        bus.load(0x8000, &[0xB1, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.y = 0x02;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x77);
    }

    // ------------------------------------------------------------------
    // Unofficial opcodes
    // ------------------------------------------------------------------

    #[test]
    fn unofficial_lax_zp() {
        let mut bus = FlatBus::new();
        bus.ram[0x30] = 0xBE;
        bus.load(0x8000, &[0xA7, 0x30]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0xBE);
        assert_eq!(cpu.x, 0xBE);
        assert!(cpu.flag(FLAG_N));
    }

    #[test]
    fn unofficial_sax_zp() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x87, 0x50], 0x8000);
        cpu.a = 0b1111_0000;
        cpu.x = 0b1010_1010;
        cpu.step(&mut bus).unwrap();
        // A & X = 0b1010_0000
        assert_eq!(bus.ram[0x50], 0b1010_0000);
    }

    #[test]
    fn unofficial_dcp_counter() {
        // Store 1 at $10, DCP $10 (dec to 0), then BNE should not branch (A=0, M=0)
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x01;
        bus.load(0x8000, &[0xC7, 0x10, 0xD0, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x00;
        cpu.step(&mut bus).unwrap(); // DCP: mem $10 → 0x00, CMP A(0)==M(0)
        assert_eq!(bus.ram[0x10], 0x00);
        assert!(cpu.flag(FLAG_Z));
        assert!(cpu.flag(FLAG_C));
        cpu.step(&mut bus).unwrap(); // BNE: Z=1, not taken
        assert_eq!(cpu.pc, 0x8004);
    }

    #[test]
    fn unofficial_isc_subtract() {
        // ISC $10: inc $10 from 1 → 2, then SBC A-2 (A=5, C=1) → A=3
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x01;
        bus.load(0x8000, &[0xE7, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x05;
        cpu.p |= FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x02);
        assert_eq!(cpu.a, 0x03);
    }

    #[test]
    fn unofficial_slo_ora() {
        // SLO $10: ASL $10 (0x40 → 0x80, C=0), ORA A (A=0x01 | 0x80 = 0x81)
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x40;
        bus.load(0x8000, &[0x07, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x01;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x80);
        assert_eq!(cpu.a, 0x81);
        assert!(!cpu.flag(FLAG_C));
    }

    #[test]
    fn unofficial_rla_and() {
        // RLA $10: ROL $10 (0x80 with C=1 → 0x01, new C=1), AND A (A=0xFF & 0x01 = 0x01)
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x80;
        bus.load(0x8000, &[0x27, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0xFF;
        cpu.p |= FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x01);
        assert_eq!(cpu.a, 0x01);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn unofficial_sre_eor() {
        // SRE $10: LSR $10 (0x03 → 0x01, C=1), EOR A (A=0xFF ^ 0x01 = 0xFE)
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x03;
        bus.load(0x8000, &[0x47, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0xFF;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x01);
        assert_eq!(cpu.a, 0xFE);
        assert!(cpu.flag(FLAG_C));
    }

    #[test]
    fn unofficial_rra_adc() {
        // RRA $10: ROR $10 (0x02 with C=0 → 0x01, new C=0), ADC A (A=0x10 + 0x01 = 0x11)
        let mut bus = FlatBus::new();
        bus.ram[0x10] = 0x02;
        bus.load(0x8000, &[0x67, 0x10]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        cpu.a = 0x10;
        cpu.p &= !FLAG_C;
        cpu.step(&mut bus).unwrap();
        assert_eq!(bus.ram[0x10], 0x01);
        assert_eq!(cpu.a, 0x11);
    }

    #[test]
    fn unofficial_nop_04_advances_two() {
        // $04 is NOP zp (2 bytes), should advance PC by 2
        let (mut cpu, mut bus) = cpu_with_bus(&[0x04, 0x42], 0x8000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.pc, 0x8002);
    }

    // ------------------------------------------------------------------
    // JAM / Unstable errors
    // ------------------------------------------------------------------

    #[test]
    fn jam_returns_error() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x02], 0x8000);
        let err = cpu.step(&mut bus).unwrap_err();
        assert!(matches!(err, StepError::Jam { opcode: 0x02, .. }));
    }

    #[test]
    fn jam_12_returns_error() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x12], 0x8000);
        let err = cpu.step(&mut bus).unwrap_err();
        assert!(matches!(err, StepError::Jam { .. }));
    }

    #[test]
    fn unstable_8b_returns_error() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0x8B, 0x00], 0x8000);
        let err = cpu.step(&mut bus).unwrap_err();
        assert!(matches!(
            err,
            StepError::UnstableOpcode { opcode: 0x8B, .. }
        ));
    }

    // ------------------------------------------------------------------
    // LAX #imm ($AB) — execute as A=X=imm
    // ------------------------------------------------------------------

    #[test]
    fn lax_imm_ab() {
        let (mut cpu, mut bus) = cpu_with_bus(&[0xAB, 0x55], 0x8000);
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.a, 0x55);
        assert_eq!(cpu.x, 0x55);
    }

    // ------------------------------------------------------------------
    // run_until_rts
    // ------------------------------------------------------------------

    #[test]
    fn run_until_rts_simple_routine() {
        // Routine at $0200: LDA #$42; STA $10; RTS
        // Caller sets up sentinel so run_until_rts stops at RTS.
        let mut bus = FlatBus::new();
        bus.load(0x0200, &[0xA9, 0x42, 0x85, 0x10, 0x60]);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x02;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        // Push sentinel return address ($FFFF → pops to $0000 after +1)
        cpu.push16(&mut bus, 0xFFFF);
        let steps = cpu.run_until_rts(&mut bus, 1000).unwrap();
        assert_eq!(steps, 3); // LDA, STA, RTS
        assert_eq!(bus.ram[0x10], 0x42);
    }

    #[test]
    fn run_until_rts_step_limit() {
        // Infinite loop: JMP $8000
        let (mut cpu, mut bus) = cpu_with_bus(&[0x4C, 0x00, 0x80], 0x8000);
        cpu.push16(&mut bus, 0xFFFF);
        let err = cpu.run_until_rts(&mut bus, 10).unwrap_err();
        assert!(matches!(err, StepError::StepLimitExceeded));
    }

    // ------------------------------------------------------------------
    // Classic adder loop: sum 0..=9 in zero page
    // ------------------------------------------------------------------

    #[test]
    fn adder_loop_0_to_9() {
        // Sums 0+1+2+...+9 = 45 using a decrement loop.
        //
        // $8000: A9 00     LDA #0      ; A = accumulator
        // $8002: A2 09     LDX #9      ; X = 9
        // $8004: 86 01     STX $01     ; $01 = X   (loop top)
        // $8006: 18        CLC
        // $8007: 65 01     ADC $01     ; A += X
        // $8009: CA        DEX
        // $800A: 10 F8     BPL $8004   ; branch if N=0 (X >= 0)
        //                              ; PC after fetch = $800C; $800C + (-8) = $8004 ✓
        // $800C: 85 00     STA $00     ; store result
        // $800E: 00        BRK
        let mut bus = FlatBus::new();
        let prog: &[u8] = &[
            0xA9, 0x00, // $8000: LDA #0
            0xA2, 0x09, // $8002: LDX #9
            0x86, 0x01, // $8004: STX $01   ← loop top
            0x18, // $8006: CLC
            0x65, 0x01, // $8007: ADC $01
            0xCA, // $8009: DEX
            0x10, 0xF8, // $800A: BPL -8 → $8004
            0x85, 0x00, // $800C: STA $00
            0x00, // $800E: BRK
        ];
        bus.load(0x8000, prog);
        bus.ram[0xFFFC] = 0x00;
        bus.ram[0xFFFD] = 0x80;
        // BRK vector — point to $FFFF so the BRK simply halts cleanly
        bus.ram[0xFFFE] = 0xFF;
        bus.ram[0xFFFF] = 0xFF;
        let mut cpu = Cpu::new();
        cpu.reset(&mut bus);
        // Run until BRK opcode (do not step through it, just stop)
        for _ in 0..200 {
            if bus.read(cpu.pc) == 0x00 {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(bus.ram[0x00], 45); // 0+1+2+...+9 = 45
    }
}
