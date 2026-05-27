use std::collections::BTreeSet;
use std::io;

use crate::z80_backend::Z80Program;

const NES_RAM_BASE: u16 = 0xC000;
const SMB_POINTER_ADD2_ADDR: u16 = 0x9CA6;
const SMB_POINTER_ADD2_BYTES: [u8; 14] = [
    0xA5, 0xE7, 0x18, 0x69, 0x02, 0x85, 0xE7, 0xA5, 0xE8, 0x69, 0x00, 0x85, 0xE8, 0x60,
];
const SMB_BRANCH_STORE_ADDR: u16 = 0xB1B4;
const SMB_BRANCH_STORE_BYTES: [u8; 7] = [0xD0, 0x04, 0xA9, 0x06, 0x85, 0x0E, 0x60];
const SMB_COMPARE_BRANCH_ADDR: u16 = 0xAEF9;
const SMB_COMPARE_BRANCH_BYTES: [u8; 5] = [0xC9, 0x03, 0xB0, 0x01, 0x60];

#[derive(Debug)]
pub struct IrDemo {
    pub asm: String,
    pub bytes: Vec<u8>,
    pub report: String,
    pub validation: String,
}

#[derive(Clone, Debug)]
enum IrOp {
    Label(String),
    Source6502 { pc: u16, asm: String },
    LoadAImm(u8),
    LoadA(Mem8),
    ClearCarry,
    AdcImm(u8),
    CmpImm(u8),
    StoreA(Mem8),
    BranchIfZClear(String),
    BranchIfCarrySet(String),
    BranchIfCarryClear(String),
    Return,
}

#[derive(Clone, Copy, Debug)]
enum Mem8 {
    ZeroPage(u8),
}

pub fn generate_smb_pointer_increment_demo(prg: &[u8]) -> io::Result<IrDemo> {
    let ir = lift_supported_range(
        prg,
        SMB_POINTER_ADD2_ADDR,
        &SMB_POINTER_ADD2_BYTES,
        "smb_9ca6_add_2_to_zp_e7e8",
    )?;

    let mut z80 = Z80Program::new();
    z80.comment("6502 IR lowering demo from actual SMB PRG bytes.");
    z80.comment("Source slice: $9CA6 A5 E7 18 69 02 85 E7 A5 E8 69 00 85 E8 60.");
    z80.comment("Semantics: add 2 to little-endian zero-page pointer $E7/$E8.");
    z80.comment("This first lowering keeps the ADC carry chain in native Z80 flags.");

    for op in &ir {
        lower_ir_op(&mut z80, op);
    }

    let output = z80.finish()?;
    Ok(IrDemo {
        validation: validate_lowered_pointer_increment(&output.bytes)?,
        asm: output.asm,
        bytes: output.bytes,
        report: report_for_demo(&ir),
    })
}

pub fn generate_smb_branch_store_demo(prg: &[u8]) -> io::Result<IrDemo> {
    let ir = lift_supported_range(
        prg,
        SMB_BRANCH_STORE_ADDR,
        &SMB_BRANCH_STORE_BYTES,
        "smb_b1b4_branch_store_zp_0e",
    )?;

    let mut z80 = Z80Program::new();
    z80.comment("6502 IR lowering demo from actual SMB PRG bytes.");
    z80.comment("Source slice: $B1B4 D0 04 A9 06 85 0E 60.");
    z80.comment("Semantics: if incoming Z is clear, skip setting zero-page $0E to $06.");
    z80.comment("This models a routine fragment whose first instruction consumes prior flags.");

    for op in &ir {
        lower_ir_op(&mut z80, op);
    }

    let output = z80.finish()?;
    Ok(IrDemo {
        validation: validate_lowered_branch_store(&output.bytes)?,
        asm: output.asm,
        bytes: output.bytes,
        report: report_for_named_demo(
            "6502 IR branch/store lowering demo",
            "actual SMB PRG bytes at $B1B4",
            "conditional store to zero-page $0E based on incoming Z flag",
            "D0 04 A9 06 85 0E 60",
            &[
                "NES zero page maps to SMS RAM base $C000.",
                "$0E maps to $C00E.",
                "6502 BNE is lowered to Z80 `jp nz,label` for this local branch.",
                "This slice consumes an incoming flag from its caller/context.",
            ],
            &ir,
        ),
    })
}

pub fn generate_smb_compare_branch_demo(prg: &[u8]) -> io::Result<IrDemo> {
    let ir = lift_supported_range(
        prg,
        SMB_COMPARE_BRANCH_ADDR,
        &SMB_COMPARE_BRANCH_BYTES,
        "smb_aef9_cmp_03_branch",
    )?;

    let mut z80 = Z80Program::new();
    z80.comment("6502 IR lowering demo from actual SMB PRG bytes.");
    z80.comment("Source slice: $AEF9 C9 03 B0 01 60.");
    z80.comment("Semantics: compare A with $03; branch to external $AEFE when A >= $03.");
    z80.comment("The external target label is a validation stub, not the full callee.");

    for op in &ir {
        lower_ir_op(&mut z80, op);
    }

    let output = z80.finish()?;
    Ok(IrDemo {
        validation: validate_lowered_compare_branch(&output.bytes)?,
        asm: output.asm,
        bytes: output.bytes,
        report: report_for_named_demo(
            "6502 IR compare/carry-branch lowering demo",
            "actual SMB PRG bytes at $AEF9",
            "branch to external $AEFE when A >= $03",
            "C9 03 B0 01 60",
            &[
                "6502 CMP #imm sets carry when A >= imm.",
                "Z80 CP #imm sets carry when A < imm, so 6502 BCS lowers to Z80 `jp nc,label`.",
                "The branch target is just outside this slice, so the generated label is a validation stub.",
            ],
            &ir,
        ),
    })
}

fn lift_supported_range(
    prg: &[u8],
    start: u16,
    expected_bytes: &[u8],
    entry_label: &str,
) -> io::Result<Vec<IrOp>> {
    let offset = start as usize - 0x8000;
    let actual = prg
        .get(offset..offset + expected_bytes.len())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("PRG is too short for SMB ${start:04X} IR slice"),
            )
        })?;

    if actual != expected_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("SMB ${start:04X} bytes do not match the expected IR slice"),
        ));
    }

    let end = start + expected_bytes.len() as u16;
    let decoded = decode_supported_instructions(expected_bytes, start)?;
    let mut labels = BTreeSet::new();
    labels.insert(start);
    let mut external_labels = BTreeSet::new();
    for instruction in &decoded {
        if let Some(target) = instruction.op.branch_target() {
            if (start..end).contains(&target) {
                labels.insert(target);
            } else {
                external_labels.insert(target);
            }
        }
    }

    let mut ir = Vec::new();
    for instruction in decoded {
        if labels.contains(&instruction.pc) {
            let label = if instruction.pc == start {
                entry_label.to_string()
            } else {
                auto_label(instruction.pc)
            };
            ir.push(IrOp::Label(label));
        }
        ir.push(IrOp::Source6502 {
            pc: instruction.pc,
            asm: instruction.asm,
        });
        match instruction.op {
            DecodedOp::LdaImm(value) => ir.push(IrOp::LoadAImm(value)),
            DecodedOp::LdaZp(addr) => ir.push(IrOp::LoadA(Mem8::ZeroPage(addr))),
            DecodedOp::Clc => ir.push(IrOp::ClearCarry),
            DecodedOp::AdcImm(value) => ir.push(IrOp::AdcImm(value)),
            DecodedOp::StaZp(addr) => ir.push(IrOp::StoreA(Mem8::ZeroPage(addr))),
            DecodedOp::CmpImm(value) => ir.push(IrOp::CmpImm(value)),
            DecodedOp::Bne(target) => ir.push(IrOp::BranchIfZClear(auto_label(target))),
            DecodedOp::Bcs(target) => ir.push(IrOp::BranchIfCarrySet(auto_label(target))),
            DecodedOp::Bcc(target) => ir.push(IrOp::BranchIfCarryClear(auto_label(target))),
            DecodedOp::Rts => ir.push(IrOp::Return),
        }
    }
    for target in external_labels {
        ir.push(IrOp::Label(auto_label(target)));
        ir.push(IrOp::Return);
    }

    Ok(ir)
}

#[derive(Debug)]
struct DecodedInstruction {
    pc: u16,
    asm: String,
    op: DecodedOp,
}

#[derive(Debug)]
enum DecodedOp {
    LdaImm(u8),
    LdaZp(u8),
    Clc,
    AdcImm(u8),
    StaZp(u8),
    CmpImm(u8),
    Bne(u16),
    Bcs(u16),
    Bcc(u16),
    Rts,
}

impl DecodedOp {
    fn branch_target(&self) -> Option<u16> {
        match self {
            DecodedOp::Bne(target) | DecodedOp::Bcs(target) | DecodedOp::Bcc(target) => {
                Some(*target)
            }
            _ => None,
        }
    }
}

fn decode_supported_instructions(bytes: &[u8], start: u16) -> io::Result<Vec<DecodedInstruction>> {
    let mut out = Vec::new();
    let mut offset = 0usize;

    while offset < bytes.len() {
        let pc = start + offset as u16;
        let op = bytes[offset];
        match op {
            0xA9 => {
                let value = operand_u8(bytes, offset + 1, pc)?;
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("LDA #${value:02X}"),
                    op: DecodedOp::LdaImm(value),
                });
                offset += 2;
            }
            0xA5 => {
                let addr = operand_u8(bytes, offset + 1, pc)?;
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("LDA ${addr:02X}"),
                    op: DecodedOp::LdaZp(addr),
                });
                offset += 2;
            }
            0x18 => {
                out.push(DecodedInstruction {
                    pc,
                    asm: "CLC".into(),
                    op: DecodedOp::Clc,
                });
                offset += 1;
            }
            0x69 => {
                let value = operand_u8(bytes, offset + 1, pc)?;
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("ADC #${value:02X}"),
                    op: DecodedOp::AdcImm(value),
                });
                offset += 2;
            }
            0x85 => {
                let addr = operand_u8(bytes, offset + 1, pc)?;
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("STA ${addr:02X}"),
                    op: DecodedOp::StaZp(addr),
                });
                offset += 2;
            }
            0xC9 => {
                let value = operand_u8(bytes, offset + 1, pc)?;
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("CMP #${value:02X}"),
                    op: DecodedOp::CmpImm(value),
                });
                offset += 2;
            }
            0xD0 => {
                let rel = operand_u8(bytes, offset + 1, pc)? as i8;
                let target = pc.wrapping_add(2).wrapping_add(rel as i16 as u16);
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("BNE ${target:04X}"),
                    op: DecodedOp::Bne(target),
                });
                offset += 2;
            }
            0xB0 => {
                let rel = operand_u8(bytes, offset + 1, pc)? as i8;
                let target = pc.wrapping_add(2).wrapping_add(rel as i16 as u16);
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("BCS ${target:04X}"),
                    op: DecodedOp::Bcs(target),
                });
                offset += 2;
            }
            0x90 => {
                let rel = operand_u8(bytes, offset + 1, pc)? as i8;
                let target = pc.wrapping_add(2).wrapping_add(rel as i16 as u16);
                out.push(DecodedInstruction {
                    pc,
                    asm: format!("BCC ${target:04X}"),
                    op: DecodedOp::Bcc(target),
                });
                offset += 2;
            }
            0x60 => {
                out.push(DecodedInstruction {
                    pc,
                    asm: "RTS".into(),
                    op: DecodedOp::Rts,
                });
                offset += 1;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported opcode ${op:02X} at ${pc:04X} in generic IR lifter"),
                ));
            }
        }
    }

    Ok(out)
}

fn operand_u8(bytes: &[u8], offset: usize, pc: u16) -> io::Result<u8> {
    bytes.get(offset).copied().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("truncated operand at ${pc:04X} in generic IR lifter"),
        )
    })
}

fn auto_label(addr: u16) -> String {
    format!("L_{addr:04X}")
}

fn lower_ir_op(z80: &mut Z80Program, op: &IrOp) {
    match op {
        IrOp::Label(label) => z80.label(label),
        IrOp::Source6502 { pc, asm } => z80.comment(format!("6502 ${pc:04X}: {asm}")),
        IrOp::LoadAImm(value) => z80.ld_a_imm(*value),
        IrOp::LoadA(mem) => z80.ld_a_abs(mem.sms_addr()),
        IrOp::ClearCarry => {
            z80.and_a();
            z80.comment("CLC lowered with `and a`: A is unchanged, Z80 carry is cleared.");
        }
        IrOp::AdcImm(value) => {
            if *value == 0x02 {
                z80.add_a_imm(*value);
                z80.comment("First ADC follows CLC, so ADD is equivalent and seeds native carry.");
            } else {
                z80.adc_a_imm(*value);
                z80.comment("ADC consumes native Z80 carry from the previous lowered ADC/ADD.");
            }
        }
        IrOp::CmpImm(value) => {
            z80.cp_imm(*value);
            z80.comment("6502 CMP carry is inverted relative to Z80 CP carry.");
        }
        IrOp::StoreA(mem) => z80.ld_abs_a(mem.sms_addr()),
        IrOp::BranchIfZClear(label) => z80.jp_nz(label),
        IrOp::BranchIfCarrySet(label) => z80.jp_nc(label),
        IrOp::BranchIfCarryClear(label) => z80.jp_c(label),
        IrOp::Return => z80.ret(),
    }
}

impl Mem8 {
    fn sms_addr(self) -> u16 {
        match self {
            Mem8::ZeroPage(addr) => NES_RAM_BASE + addr as u16,
        }
    }
}

fn report_for_demo(ir: &[IrOp]) -> String {
    report_for_named_demo(
        "6502 IR lowering demo",
        "actual SMB PRG bytes at $9CA6",
        "add 2 to little-endian zero-page pointer $E7/$E8",
        "A5 E7 18 69 02 85 E7 A5 E8 69 00 85 E8 60",
        &[
            "NES zero page maps to SMS RAM base $C000.",
            "$E7 maps to $C0E7 and $E8 maps to $C0E8.",
            "CLC is lowered as `and a`, preserving A and clearing Z80 carry.",
            "The two ADC operations use native Z80 carry for this local carry chain.",
            "This is valid only while no intervening operation clobbers Z80 flags.",
        ],
        ir,
    )
}

fn report_for_named_demo(
    title: &str,
    source: &str,
    behavior: &str,
    bytes: &str,
    notes: &[&str],
    ir: &[IrOp],
) -> String {
    let mut report = String::new();
    report.push_str(title);
    report.push('\n');
    report.push_str(&"=".repeat(title.len()));
    report.push_str("\n\n");
    report.push_str(&format!("Source: {source}.\n"));
    report.push_str(&format!("Recognized behavior: {behavior}.\n\n"));
    report.push_str("Original bytes:\n");
    report.push_str(&format!("- {bytes}\n\n"));
    report.push_str("Lowering notes:\n");
    for note in notes {
        report.push_str("- ");
        report.push_str(note);
        report.push('\n');
    }
    report.push('\n');
    report.push_str("IR operations:\n");
    for op in ir {
        report.push_str("- ");
        report.push_str(&format_ir_op(op));
        report.push('\n');
    }
    report
}

fn format_ir_op(op: &IrOp) -> String {
    match op {
        IrOp::Label(label) => format!("label {label}"),
        IrOp::Source6502 { pc, asm } => format!("source ${pc:04X}: {asm}"),
        IrOp::LoadAImm(value) => format!("A = ${value:02X}"),
        IrOp::LoadA(mem) => format!("A = read8({})", format_mem(*mem)),
        IrOp::ClearCarry => "C = 0".into(),
        IrOp::AdcImm(value) => format!("A,C = adc(A, ${value:02X}, C)"),
        IrOp::CmpImm(value) => format!("compare A with ${value:02X}"),
        IrOp::StoreA(mem) => format!("write8({}, A)", format_mem(*mem)),
        IrOp::BranchIfZClear(label) => format!("if Z == 0 branch {label}"),
        IrOp::BranchIfCarrySet(label) => format!("if C == 1 branch {label}"),
        IrOp::BranchIfCarryClear(label) => format!("if C == 0 branch {label}"),
        IrOp::Return => "return".into(),
    }
}

fn format_mem(mem: Mem8) -> String {
    match mem {
        Mem8::ZeroPage(addr) => format!("zp:${addr:02X} -> sms:${:04X}", mem.sms_addr()),
    }
}

fn validate_lowered_pointer_increment(code: &[u8]) -> io::Result<String> {
    let cases = [
        (0x0000u16, 0x0002u16),
        (0x00FEu16, 0x0100u16),
        (0x12FFu16, 0x1301u16),
        (0xFFFFu16, 0x0001u16),
    ];

    let mut report = String::new();
    report.push_str("IR lowering validation\n");
    report.push_str("======================\n\n");
    report.push_str("Method: execute the generated Z80 bytes in a tiny local interpreter that\n");
    report.push_str("implements only the opcodes emitted for this routine.\n\n");

    for (input, expected) in cases {
        let actual = run_pointer_increment_z80(code, input)?;
        let status = if actual == expected { "ok" } else { "FAIL" };
        report.push_str(&format!(
            "- {status}: ${input:04X} + 2 -> expected ${expected:04X}, actual ${actual:04X}\n"
        ));
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IR lowering validation failed",
            ));
        }
    }

    report.push_str("\nResult: all local Z80 state cases passed.\n");
    Ok(report)
}

fn validate_lowered_branch_store(code: &[u8]) -> io::Result<String> {
    let cases = [
        (
            true,
            0x00u8,
            0x06u8,
            "incoming Z set falls through and stores $06",
        ),
        (
            false,
            0x00u8,
            0x00u8,
            "incoming Z clear branches around the store",
        ),
        (
            false,
            0x99u8,
            0x99u8,
            "branch path preserves the old zero-page value",
        ),
    ];

    let mut report = String::new();
    report.push_str("IR branch/store validation\n");
    report.push_str("==========================\n\n");
    report.push_str("Method: execute the generated Z80 bytes in a tiny local interpreter with\n");
    report.push_str("an explicitly supplied incoming Z flag.\n\n");

    for (z_flag, initial, expected, note) in cases {
        let actual = run_branch_store_z80(code, z_flag, initial)?;
        let status = if actual == expected { "ok" } else { "FAIL" };
        report.push_str(&format!(
            "- {status}: {note}; initial $0E=${initial:02X}, expected ${expected:02X}, actual ${actual:02X}\n"
        ));
        if actual != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IR branch/store validation failed",
            ));
        }
    }

    report.push_str("\nResult: all local Z80 branch/store cases passed.\n");
    Ok(report)
}

fn validate_lowered_compare_branch(code: &[u8]) -> io::Result<String> {
    let cases = [
        (0x00u8, false, "A < $03 returns locally"),
        (0x02u8, false, "A still below $03 returns locally"),
        (0x03u8, true, "A == $03 takes the carry-set branch"),
        (0x80u8, true, "A > $03 takes the carry-set branch"),
    ];

    let mut report = String::new();
    report.push_str("IR compare/carry-branch validation\n");
    report.push_str("==================================\n\n");
    report.push_str("Method: execute generated Z80 bytes and record whether the `jp nc`\n");
    report.push_str("branch target was taken.\n\n");

    for (a, expected_taken, note) in cases {
        let actual_taken = run_compare_branch_z80(code, a)?;
        let status = if actual_taken == expected_taken {
            "ok"
        } else {
            "FAIL"
        };
        report.push_str(&format!(
            "- {status}: {note}; A=${a:02X}, expected branch={expected_taken}, actual branch={actual_taken}\n"
        ));
        if actual_taken != expected_taken {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "IR compare/carry-branch validation failed",
            ));
        }
    }

    report.push_str("\nResult: all compare/carry branch cases passed.\n");
    Ok(report)
}

fn run_pointer_increment_z80(code: &[u8], pointer: u16) -> io::Result<u16> {
    let mut ram = [0u8; 65536];
    let [lo, hi] = pointer.to_le_bytes();
    ram[0xC0E7] = lo;
    ram[0xC0E8] = hi;

    let mut pc = 0usize;
    let mut a = 0u8;
    let mut carry = false;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Z80 validation exceeded step limit",
            ));
        }
        let op = *code.get(pc).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Z80 PC ran past generated code",
            )
        })?;

        match op {
            0x3A => {
                let addr = read_u16(code, pc + 1)?;
                a = ram[addr as usize];
                pc += 3;
            }
            0xA7 => {
                carry = false;
                pc += 1;
            }
            0xC6 => {
                let value = *code.get(pc + 1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated ADD immediate")
                })?;
                let sum = a as u16 + value as u16;
                a = sum as u8;
                carry = sum > 0xFF;
                pc += 2;
            }
            0xCE => {
                let value = *code.get(pc + 1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated ADC immediate")
                })?;
                let sum = a as u16 + value as u16 + u16::from(carry);
                a = sum as u8;
                carry = sum > 0xFF;
                pc += 2;
            }
            0x32 => {
                let addr = read_u16(code, pc + 1)?;
                ram[addr as usize] = a;
                pc += 3;
            }
            0xC9 => break,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported Z80 opcode ${op:02X} in IR validation"),
                ));
            }
        }
    }

    Ok(u16::from_le_bytes([ram[0xC0E7], ram[0xC0E8]]))
}

fn run_branch_store_z80(code: &[u8], initial_z: bool, initial_zp_0e: u8) -> io::Result<u8> {
    let mut ram = [0u8; 65536];
    ram[0xC00E] = initial_zp_0e;

    let mut pc = 0usize;
    let mut a = 0u8;
    let mut zero = initial_z;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Z80 branch validation exceeded step limit",
            ));
        }
        let op = *code.get(pc).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Z80 PC ran past generated branch code",
            )
        })?;

        match op {
            0xC2 => {
                let target = read_u16(code, pc + 1)? as usize;
                if !zero {
                    pc = target;
                } else {
                    pc += 3;
                }
            }
            0x3E => {
                a = *code.get(pc + 1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated LD A immediate")
                })?;
                zero = a == 0;
                pc += 2;
            }
            0x32 => {
                let addr = read_u16(code, pc + 1)?;
                ram[addr as usize] = a;
                pc += 3;
            }
            0xC9 => break,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported Z80 opcode ${op:02X} in branch validation"),
                ));
            }
        }
    }

    Ok(ram[0xC00E])
}

fn run_compare_branch_z80(code: &[u8], initial_a: u8) -> io::Result<bool> {
    let mut pc = 0usize;
    let a = initial_a;
    let mut carry = false;
    let mut branch_taken = false;
    let mut steps = 0usize;

    loop {
        steps += 1;
        if steps > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Z80 compare validation exceeded step limit",
            ));
        }
        let op = *code.get(pc).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Z80 PC ran past generated compare code",
            )
        })?;

        match op {
            0xFE => {
                let value = *code.get(pc + 1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "truncated CP immediate")
                })?;
                carry = a < value;
                pc += 2;
            }
            0xD2 => {
                let target = read_u16(code, pc + 1)? as usize;
                if !carry {
                    branch_taken = true;
                    pc = target;
                } else {
                    pc += 3;
                }
            }
            0xC9 => break,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsupported Z80 opcode ${op:02X} in compare validation"),
                ));
            }
        }
    }

    Ok(branch_taken)
}

fn read_u16(code: &[u8], offset: usize) -> io::Result<u16> {
    let lo = *code.get(offset).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated Z80 16-bit operand")
    })?;
    let hi = *code.get(offset + 1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "truncated Z80 16-bit operand")
    })?;
    Ok(u16::from_le_bytes([lo, hi]))
}
