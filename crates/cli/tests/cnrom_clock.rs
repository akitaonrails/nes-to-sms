//! Independent source-timing expectations, not a cycle-accurate NES oracle.
//!
//! Literal opcode rows follow the NMOS bus-cycle tables at
//! https://www.nesdev.org/6502_cpu.txt and hardware-tested branch totals in
//! https://raw.githubusercontent.com/christopherpow/nes-test-roms/master/instr_timing/readme.txt.
//! Interrupt expectations use the author's later cpu_interrupts_v2 tests;
//! the older bus reference's interrupt prose contains known inaccuracies.
//! Decoder/timing metadata is the subject here, never the expected-value source.

use cpu6502::timing::{BusSequence, CyclePenalty};
use std::path::PathBuf;
use std::process::Command;

fn assert_timing(opcode: u8, sequence: BusSequence, cycles: u8, penalty: CyclePenalty) {
    let instruction = cpu6502::decode_at(&[opcode, 0xff, 0x80], 0x8000, 0).unwrap();
    let actual = instruction.timing().expect("literal admitted opcode");
    assert_eq!(
        (actual.sequence, actual.base_cycles, actual.penalty),
        (sequence, cycles, penalty),
        "opcode ${opcode:02X}"
    );
}

#[test]
fn literal_read_and_store_encodings_keep_distinct_page_penalties() {
    use BusSequence::{Read, Write};
    use CyclePenalty::{IndexedReadPageCross as Page, None};
    // ORA, AND, EOR, ADC, LDA, CMP and SBC have these literal mode encodings.
    // No expected duration or mode is obtained from OPCODE_TABLE/timing().
    for base in [0x00, 0x20, 0x40, 0x60, 0xa0, 0xc0, 0xe0] {
        for (low, cycles, penalty) in [
            (0x01, 6, None), // (zp,X)
            (0x05, 3, None), // zp
            (0x09, 2, None), // immediate
            (0x0d, 4, None), // absolute
            (0x11, 5, Page), // (zp),Y
            (0x15, 4, None), // zp,X
            (0x19, 4, Page), // absolute,Y
            (0x1d, 4, Page), // absolute,X
        ] {
            assert_timing(base + low, Read, cycles, penalty);
        }
    }
    for (opcode, cycles, penalty) in [
        (0xa0, 2, None),
        (0xa4, 3, None),
        (0xac, 4, None),
        (0xb4, 4, None),
        (0xbc, 4, Page), // LDY
        (0xa2, 2, None),
        (0xa6, 3, None),
        (0xae, 4, None),
        (0xb6, 4, None),
        (0xbe, 4, Page), // LDX
        (0xc0, 2, None),
        (0xc4, 3, None),
        (0xcc, 4, None), // CPY
        (0xe0, 2, None),
        (0xe4, 3, None),
        (0xec, 4, None), // CPX
        (0x24, 3, None),
        (0x2c, 4, None), // BIT
        (0xa3, 6, None),
        (0xa7, 3, None),
        (0xaf, 4, None),
        (0xb3, 5, Page),
        (0xb7, 4, None),
        (0xbf, 4, Page), // stable LAX
        (0xeb, 2, None), // unofficial immediate SBC
    ] {
        assert_timing(opcode, Read, cycles, penalty);
    }
    for (opcode, cycles) in [
        (0x81, 6),
        (0x85, 3),
        (0x8d, 4),
        (0x91, 6),
        (0x95, 4),
        (0x99, 5),
        (0x9d, 5), // STA, never read-page penalties
        (0x84, 3),
        (0x8c, 4),
        (0x94, 4), // STY
        (0x86, 3),
        (0x8e, 4),
        (0x96, 4), // STX
        (0x83, 6),
        (0x87, 3),
        (0x8f, 4),
        (0x97, 4), // SAX
    ] {
        assert_timing(opcode, Write, cycles, None);
    }
}

#[test]
fn all_66_memory_rmw_encodings_have_fixed_not_read_penalty_timing() {
    let families = [
        [0, 0xe6, 0xee, 0, 0xf6, 0, 0xfe],          // INC
        [0, 0xc6, 0xce, 0, 0xd6, 0, 0xde],          // DEC
        [0, 0x06, 0x0e, 0, 0x16, 0, 0x1e],          // ASL
        [0, 0x46, 0x4e, 0, 0x56, 0, 0x5e],          // LSR
        [0, 0x26, 0x2e, 0, 0x36, 0, 0x3e],          // ROL
        [0, 0x66, 0x6e, 0, 0x76, 0, 0x7e],          // ROR
        [0xe3, 0xe7, 0xef, 0xf3, 0xf7, 0xfb, 0xff], // ISC
        [0xc3, 0xc7, 0xcf, 0xd3, 0xd7, 0xdb, 0xdf], // DCP
        [0x03, 0x07, 0x0f, 0x13, 0x17, 0x1b, 0x1f], // SLO
        [0x43, 0x47, 0x4f, 0x53, 0x57, 0x5b, 0x5f], // SRE
        [0x23, 0x27, 0x2f, 0x33, 0x37, 0x3b, 0x3f], // RLA
        [0x63, 0x67, 0x6f, 0x73, 0x77, 0x7b, 0x7f], // RRA
    ];
    let mut checked = 0;
    for opcodes in families {
        // (zp,X), zp, abs, (zp),Y, zp,X, abs,Y, abs,X.
        for (opcode, cycles) in opcodes.into_iter().zip([8, 5, 6, 8, 6, 7, 7]) {
            if opcode != 0 {
                assert_timing(
                    opcode,
                    BusSequence::ReadModifyWrite,
                    cycles,
                    CyclePenalty::None,
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 66);
}

#[test]
fn stack_control_branches_and_read_nops_have_literal_timing_classes() {
    use BusSequence::*;
    use CyclePenalty::{BranchTakenAndPageCross as BranchPage, IndexedReadPageCross as Page, None};
    for opcode in [0x10, 0x30, 0x50, 0x70, 0x90, 0xb0, 0xd0, 0xf0] {
        assert_timing(opcode, Branch, 2, BranchPage);
    }
    for (opcode, sequence, cycles) in [
        (0x00, Brk, 7),
        (0x20, Jsr, 6),
        (0x40, Rti, 6),
        (0x60, Rts, 6),
        (0x4c, JumpAbsolute, 3),
        (0x6c, JumpIndirect, 5),
        (0x08, Push, 3),
        (0x48, Push, 3),
        (0x28, Pull, 4),
        (0x68, Pull, 4),
    ] {
        assert_timing(opcode, sequence, cycles, None);
    }
    for opcode in [
        0x0a, 0x2a, 0x4a, 0x6a, // accumulator shifts, no memory RMW
        0x18, 0x38, 0x58, 0x78, 0xb8, 0xd8, 0xf8, 0x88, 0xc8, 0xca, 0xe8, 0x8a, 0x98, 0x9a, 0xa8,
        0xaa, 0xba, 0xea, 0x1a, 0x3a, 0x5a, 0x7a, 0xda, 0xfa,
    ] {
        assert_timing(opcode, Implied, 2, None);
    }
    for (opcodes, cycles, penalty) in [
        (&[0x80, 0x82, 0x89, 0xc2, 0xe2][..], 2, None),
        (&[0x04, 0x44, 0x64][..], 3, None),
        (&[0x14, 0x34, 0x54, 0x74, 0xd4, 0xf4][..], 4, None),
        (&[0x0c][..], 4, None),
        (&[0x1c, 0x3c, 0x5c, 0x7c, 0xdc, 0xfc][..], 4, Page),
    ] {
        for &opcode in opcodes {
            assert_timing(opcode, Read, cycles, penalty);
        }
    }
}

#[test]
fn unstable_encodings_and_jam_do_not_gain_a_clock_template() {
    for opcode in [
        0x02, 0x12, 0x22, 0x32, 0x42, 0x52, 0x62, 0x72, 0x92, 0xb2, 0xd2, 0xf2, 0x8b, 0xab, 0x93,
        0x9f, 0x9b, 0x9c, 0x9e, 0xbb,
    ] {
        let instruction = cpu6502::decode_at(&[opcode, 0xff, 0x80], 0x8000, 0).unwrap();
        assert_eq!(instruction.timing(), None, "opcode ${opcode:02X}");
    }
}

#[test]
fn compound_ir_preserves_one_typed_source_with_original_operand_identity() {
    for opcode in [0x0f, 0x2f, 0x4f, 0x6f, 0xcf, 0xef] {
        let mut prg = vec![0; 32768];
        prg[..3].copy_from_slice(&[opcode, 0x01, 0xb8]);
        let routine = ir::lift_range(
            &prg,
            &ir::LiftOptions {
                start: 0x8000,
                end: 0x8003,
                dynamic_cpu_bus: true,
                ..ir::LiftOptions::default()
            },
        )
        .unwrap();
        let sources: Vec<_> = routine
            .ops
            .iter()
            .filter_map(|op| match op {
                ir::Op::Source {
                    pc,
                    size,
                    instruction,
                    ..
                } => Some((*pc, *size, *instruction)),
                _ => None,
            })
            .collect();
        assert_eq!(sources.len(), 1, "compound opcode ${opcode:02X}");
        let (pc, size, instruction) = sources[0];
        let instruction = instruction.expect("decoded source cannot lose typed metadata");
        assert_eq!(
            (pc, size, instruction.pc, instruction.opcode),
            (0x8000, 3, 0x8000, opcode)
        );
        assert_eq!(instruction.operand, cpu6502::Operand::Addr(0xb801));
        assert_eq!(instruction.mode, cpu6502::AddrMode::Absolute);
        assert!(
            routine.ops.len() > 2,
            "compound still has its semantic operations"
        );
    }
}

struct ClockFixture {
    name: &'static str,
    prg: Vec<u8>,
    stop: u16,
    cycles: u32,
    expected: Vec<(u16, u8)>,
    nmi: u16,
    irq: u16,
    extra_roots: Vec<u16>,
    trace_steps: u32,
    shadow_p: Option<u8>,
    fast_forward: bool,
    poll_loops: Vec<(u16, u8)>,
    source_hardware: bool,
    source_deferred: bool,
    vertical: bool,
    chr: Vec<u8>,
}

impl ClockFixture {
    fn new(name: &'static str, main: &[u8], cycles: u32, expected: &[(u16, u8)]) -> Self {
        let mut prg = vec![0xea; 32768];
        prg[..main.len()].copy_from_slice(main);
        let mut fixture = Self {
            name,
            prg,
            stop: 0,
            cycles,
            expected: expected.to_vec(),
            nmi: 0x8000,
            irq: 0x8000,
            extra_roots: Vec::new(),
            trace_steps: 1_500_000,
            shadow_p: None,
            fast_forward: false,
            poll_loops: Vec::new(),
            source_hardware: false,
            source_deferred: false,
            vertical: false,
            chr: vec![0; 32768],
        };
        fixture.finish_at(0x8000 + main.len() as u16);
        fixture
    }

    fn place(&mut self, pc: u16, bytes: &[u8]) {
        let offset = usize::from(pc - 0x8000);
        self.prg[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    fn finish_at(&mut self, pc: u16) {
        // Freeze source time after a visible completion marker by deliberately
        // reading rejected timed APU status. That final read's 4 cycles count.
        self.place(pc, &[0xa9, 0xa5, 0x8d, 0xff, 7, 0xad, 0x15, 0x40]);
        let fallback = pc + 8;
        self.place(fallback, &[0x4c, fallback as u8, (fallback >> 8) as u8]);
        self.stop = pc + 5; // The source-state oracle stops BEFORE unsupported I/O.
    }

    fn rom(&self) -> Vec<u8> {
        let mut prg = self.prg.clone();
        for (offset, vector) in [(0x7ffa, self.nmi), (0x7ffc, 0x8000), (0x7ffe, self.irq)] {
            prg[offset..offset + 2].copy_from_slice(&vector.to_le_bytes());
        }
        let mut rom = b"NES\x1a".to_vec();
        rom.extend([2, 4, 0x30, 8, 0x10, 0, 0, 0, 0, 0, 0, 0]);
        if self.vertical {
            rom[6] |= 1;
        }
        if self.source_hardware {
            rom[15] = 1; // NES2 standard controllers, not unspecified expansion.
        }
        rom.extend(prg);
        rom.extend(&self.chr);
        rom
    }
}

fn clock_fixtures() -> Vec<ClockFixture> {
    let basic = ClockFixture::new(
        "basic",
        &[
            0x78, 0xa2, 0x3f, 0x9a, 0xa9, 0x55, 0x85, 0x10, 0xa5, 0x10, 0x48, 0x68, 0xea,
        ],
        33,
        &[(0x10, 0x55)],
    ); // 2+2+2+2+3+3+3+4+2 + terminal10.
    let same_page = ClockFixture::new("branch-same-page", &[0xa2, 2, 0xca, 0xd0, 0xfd], 21, &[]); // LDX2; DEX2/BNE3; DEX2/BNE2; terminal10.

    let mut forward = ClockFixture::new("branch-forward-cross", &[0x38, 0x4c, 0xfc, 0x80], 19, &[]); // SEC2 + JMP3 + taken crossing4 + terminal10.
    forward.place(0x80fc, &[0xb0, 2]);
    forward.finish_at(0x8100);

    let mut pc_after_operand = ClockFixture::new(
        "branch-opcode-page-edge",
        &[0x38, 0x4c, 0xfe, 0x80],
        18,
        &[],
    ); // Target8100 equals PC+2, so this is NOT a crossing branch.
    pc_after_operand.place(0x80fe, &[0xb0, 0]);
    pc_after_operand.finish_at(0x8100);

    let mut backward = ClockFixture::new(
        "branch-backward-cross",
        &[0xa2, 2, 0x4c, 0xfe, 0x80],
        29,
        &[],
    ); // Setup5; DEX2/NOP2/BNE4; DEX2/NOP2/BNE2; terminal10.
    backward.place(0x80fe, &[0xca, 0xea, 0xd0, 0xfc]);
    backward.finish_at(0x8102);

    let mut stack = ClockFixture::new(
        "jsr-real-stack",
        &[0xa2, 0x3f, 0x9a, 0xa9, 0x55, 0x20, 0, 0x81, 0x8d, 0, 5],
        41,
        &[(0x500, 0x55)],
    ); // Main16 + callee15 + terminal10.
    stack.place(0x8100, &[0x48, 0xa9, 0xaa, 0x68, 0x60]);

    let indexed = ClockFixture::new(
        "dynamic-indexed",
        &[
            0xa2, 1, 0xa0, 1, 0xa9, 0x80, 0x8d, 0xff, 2, 0xa9, 0x42, 0x8d, 0, 3, 0xbd, 0xfe, 2,
            0x8d, 0, 5, // Read4: no crossing.
            0xbd, 0xff, 2, 0x8d, 1, 5, // Read5: crossing.
            0xa9, 0xff, 0x85, 0xff, 0xa9, 2, 0x85, 0, 0xb1, 0xff, 0x8d, 2,
            5, // Indirect6: pointer wraps FF->00, EA crosses.
            0x9d, 0xff, 2, // Store5 regardless of crossing.
        ],
        68,
        &[(0x500, 0x80), (0x501, 0x42), (0x502, 0x42)],
    );

    let mut rmw = ClockFixture::new(
        "rmw-ordered",
        &[
            0xa2, 1, 0xa9, 0x53, 0x38, 0xee, 1, 0xb8, 0x0f, 1, 0xb8, 0x8d, 0, 5,
        ],
        32,
        &[(0x500, 0x57)],
    ); // Setup6 + INC6 + SLO6 + store4 + terminal10.
    rmw.place(0xb801, &[2]); // Immutable PRG operand for BOTH source instructions.
    let mut plp = ClockFixture::new("plp-normalized", &[], 23, &[]);
    plp.place(
        0x8000,
        &[
            0xa2, 0x3f, 0x9a, 0xa9, 0xa5, 0x8d, 0xff, 7, 0xa9, 0, 0x48, 0x28, 0xad, 0x15, 0x40,
            0x4c, 0x0f, 0x80,
        ],
    );
    plp.stop = 0x800c;
    plp.shadow_p = Some(0x20);
    vec![
        basic,
        same_page,
        forward,
        pc_after_operand,
        backward,
        stack,
        indexed,
        rmw,
        plp,
    ]
}

fn interrupt_and_dma_fixtures() -> Vec<ClockFixture> {
    // Deliberate +2 NOP phase avoids the rejected set-1 status-read window.
    // Status reads at C=6+7*k; k=3929 sees VBlank at C27509, PPU241:5.
    let mut status = ClockFixture::new(
        "status-poll",
        &[
            0xea, 0xad, 2, 0x20, 0x10, 0xfb, 0x8d, 0, 5, 0xad, 2, 0x20, 0x8d, 1, 5,
        ],
        27533,
        &[(0x500, 0x80), (0x501, 0)],
    );
    status.trace_steps = 30_000_000;

    // Delay: LDY2 + 22*(LDX2 + DEX/BNE1274 + DEY2 + BNE3) - 1 = 28183.
    // Setup SEI/LDX/TXS adds6. The final PPUCTRL store reaches C28195.
    // INC20 must run BEFORE NMI; interrupted BNE at8015 must use restored Z=0,
    // although the handler deliberately leaves A=0. NMI saves PC8015/P24.
    let mut nmi = ClockFixture::new(
        "nmi-restored-flags",
        &[
            0x78, 0xa2, 0x3f, 0x9a, 0xa0, 22, 0xa2, 0xff, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8, 0xa9,
            0x80, 0x8d, 0, 0x20, 0xe6, 0x20, 0xd0, 2, 0xa9, 0xba, 0x8d, 0, 5, 0xa5, 0x21, 0x8d, 1,
            5,
        ],
        28244,
        &[
            (0x20, 1),
            (0x21, 1),
            (0x500, 0),
            (0x501, 1),
            (0x13f, 0x80),
            (0x13e, 0x15),
            (0x13d, 0x24),
        ],
    );
    nmi.nmi = 0xc000;
    nmi.place(0xc000, &[0xe6, 0x21, 0xa9, 0, 0x40]); // INC5/LDA2/RTI6.
    nmi.trace_steps = 30_000_000;

    let mut brk = ClockFixture::new(
        "brk-rti-stack-wrap",
        &[
            0xa2, 0, 0x9a, 0x38, 0xf8, 0xa9, 0x80, 0,
            0xea, // BRK at8007, ignored padding8008; return8009.
            0x30, 2, 0xa9, 0xba, 0x8d, 0, 5,
        ],
        42,
        &[(0x500, 1), (0x100, 0x80), (0x1ff, 9), (0x1fe, 0xbd)],
    );
    brk.irq = 0xc100;
    brk.place(0xc100, &[0xa9, 1, 0x40]); // A1 but RTI restores N1 from pre-BRK A80.
    brk.extra_roots.push(0x8009);

    // Default source alignment declares odd-numbered transfers get cycles.
    // Write4014 at C24 is put: 514 stalls. Extra BITzp3 moves it to get: 513.
    let dma_program = [
        0xa9, 0x12, 0x8d, 0, 3, 0xa9, 0x34, 0x8d, 0xff, 3, 0xa9, 0xf9, 0x8d, 3, 0x20, 0xa9, 3,
        0x8d, 0x14, 0x40,
    ];
    let dma = ClockFixture::new(
        "dma-put-write",
        &dma_program,
        548,
        &[(0x300, 0x12), (0x3ff, 0x34), (0x9f9, 0x12), (0x9f8, 0x34)],
    );
    let mut shifted = vec![0x24, 0]; // BIT $00 changes source alignment by3.
    shifted.extend(dma_program);
    let dma_shifted = ClockFixture::new(
        "dma-get-write",
        &shifted,
        550,
        &[(0x300, 0x12), (0x3ff, 0x34), (0x9f9, 0x12), (0x9f8, 0x34)],
    );
    vec![status, nmi, brk, dma, dma_shifted]
}

fn nmi_dead_flags_fixture(family: &str) -> ClockFixture {
    // Literal results from A=$80, P=$A4 (C=0), not lowering/timing metadata.
    // ADC: $80+$80=$100; SBC: $80-$01-1=$7E. Both overflow.
    let (name, instruction, overwrite, a, p, resume, cycles) = match family {
        "cmp" => (
            "nmi-dead-cmp",
            &[0xc9, 0x80][..],
            &[0xc9, 0][..],
            0x80,
            0x27,
            0x8015u16,
            28251,
        ),
        "adc" => (
            "nmi-dead-adc",
            &[0x69, 0x80][..],
            &[0xb8, 0xc9, 0][..],
            0,
            0x67,
            0x8015,
            28253,
        ),
        "sbc" => (
            "nmi-dead-sbc",
            &[0xe9, 1][..],
            &[0xb8, 0xc9, 0][..],
            0x7e,
            0x65,
            0x8015,
            28253,
        ),
        "asl" => (
            "nmi-dead-asl",
            &[0x0a][..],
            &[0xc9, 0][..],
            0,
            0x27,
            0x8014,
            28251,
        ),
        "lsr" => (
            "nmi-dead-lsr",
            &[0x4a][..],
            &[0xc9, 0][..],
            0x40,
            0x24,
            0x8014,
            28251,
        ),
        _ => panic!("unknown literal flag family"),
    };
    // Same real-VBlank delay as the original Gate 3 failing CMP probe:
    // NMI-enable store completes at C28195; the instruction at $8013 accepts
    // the edge, retires in 2 cycles, and NMI enters before its flag overwriter.
    // CLV+CMP intentionally kills V/N/Z/C for ADC/SBC liveness as well.
    let mut main = vec![
        0x78, 0xa2, 0x3f, 0x9a, 0xa0, 22, 0xa2, 0xff, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8, 0xa9,
        0x80, 0x8d, 0, 0x20,
    ];
    main.extend(instruction);
    main.extend(overwrite);
    let mut fixture = ClockFixture::new(
        name,
        &main,
        cycles, // 28195 + instruction2 + entry7 + handler35 + overwrite2/4 + terminal10.
        &[
            (0x21, 1),
            (0x500, a), // NMI-observed guest A/X/Y, before handler mutations.
            (0x501, 0),
            (0x502, 0),
            (0x503, 0x3c), // Guest S after the real three-byte interrupt frame.
            (0x13f, 0x80),
            (0x13e, resume as u8),
            (0x13d, p),    // P visible at source boundary, even if later overwritten.
            (0xb02, 0x3f), // RTI consumed that frame, no extra guest return owner.
        ],
    );
    // Store A/X/Y before TSX. Restore X after observing S; leave A=0 on purpose,
    // and let RTI restore the pre-handler P. Later CMP0 and terminal LDAA5 give PA5.
    fixture.nmi = 0xc000;
    fixture.place(
        0xc000,
        &[
            0x8d, 0, 5, 0x8e, 1, 5, 0x8c, 2, 5, 0xba, 0x8e, 3, 5, 0xae, 1, 5, 0xe6, 0x21, 0xa9, 0,
            0x40,
        ],
    );
    fixture.shadow_p = Some(0xa5);
    fixture.expected.extend([
        (0xa8c, fixture.stop as u8), // Final source PC is the rejected LDA4015.
        (0xa8d, (fixture.stop >> 8) as u8),
    ]);
    fixture.trace_steps = 30_000_000;
    fixture
}

fn quiet_wait_fixtures() -> Vec<ClockFixture> {
    [
        ("quiet-off-positive", false, 1, false),
        ("quiet-on-positive", true, 1, false),
        ("quiet-off-negative", false, 0x80, false),
        ("quiet-on-negative", true, 0x80, false),
        ("quiet-off-alternate", false, 0x80, true),
        ("quiet-on-alternate", true, 0x80, true),
        ("quiet-off-zero", false, 0, false),
        ("quiet-on-zero", true, 0, false),
    ]
    .into_iter()
    .map(|(name, fast, value, alternate)| {
        let mut main = vec![
            0x78, 0xa2, 0x3f, 0x9a, 0xa9, value, 0x85, 0x20, 0xa9, 0x80, 0x8d, 0, 0x20, 0xa9, value,
        ];
        if alternate {
            // A80/RAM80 but CMP7F gives N0/Z0/C1. Enter BNE directly: the
            // next LDA must establish N1, even though A already equals RAM.
            main.extend([0xc9, 0x7f, 0x4c, 2, 0x81]);
        } else {
            main.extend([0x4c, 0, 0x81]);
        }
        let cycles = if value == 0 {
            37
        } else if alternate {
            27555
        } else {
            27556
        };
        let saved_p = if alternate {
            0xa5
        } else if value == 0x80 {
            0xa4
        } else {
            0x24
        };
        let mut fixture = ClockFixture::new(name, &main, cycles, &[(0x20, 0), (0xb02, 0x3f)]);
        fixture.place(0x8100, &[0xa5, 0x20, 0xd0, 0xfc]);
        fixture.finish_at(0x8104);
        fixture.nmi = 0xc000;
        fixture.place(0xc000, &[0x8d, 0, 5, 0xa9, 0, 0x85, 0x20, 0xe6, 0x21, 0x40]);
        fixture.poll_loops.push((0x8100, 0x20));
        fixture.fast_forward = fast;
        fixture.shadow_p = Some(if alternate { 0xa5 } else { 0xa4 });
        fixture.expected.extend([(0xa8c, 9), (0xa8d, 0x81)]);
        if value != 0 {
            fixture.expected.extend([
                (0x21, 1),
                (0x500, value),
                (0x13f, 0x81),
                (0x13e, 2),
                (0x13d, saved_p),
            ]);
        } else {
            fixture.expected.push((0x21, 0));
        }
        fixture.trace_steps = if fast { 2_000_000 } else { 8_000_000 };
        // Normal loop starts C22. Edge27508 is BNE cycle6; next LDA polls27510,
        // retires27511. NMI7+handler20 ->27538; resumed BNE3 then LDA3/BNE2
        // ->27546, terminal10 ->27556. Alternate starts BNE at24 then head27;
        // edge is LDA cycle1, retiring27510, so everything finishes one earlier.
        fixture
    })
    .collect()
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; precise/accelerated real-NMI wait equivalence"]
fn assembled_quiet_waits_keep_literal_nmi_state_and_zero_or_alternate_entry() {
    for fixture in quiet_wait_fixtures() {
        assert_clock_fixture(&fixture);
    }
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; real guest NMI flag-boundary regression"]
fn assembled_nmi_observes_dead_cmp_flags() {
    assert_clock_fixture(&nmi_dead_flags_fixture("cmp"));
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; real guest NMI flag-boundary regression"]
fn assembled_nmi_observes_dead_adc_flags() {
    assert_clock_fixture(&nmi_dead_flags_fixture("adc"));
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; real guest NMI flag-boundary regression"]
fn assembled_nmi_observes_dead_sbc_flags() {
    assert_clock_fixture(&nmi_dead_flags_fixture("sbc"));
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; real guest NMI flag-boundary regression"]
fn assembled_nmi_observes_dead_asl_flags() {
    assert_clock_fixture(&nmi_dead_flags_fixture("asl"));
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; real guest NMI flag-boundary regression"]
fn assembled_nmi_observes_dead_lsr_flags() {
    assert_clock_fixture(&nmi_dead_flags_fixture("lsr"));
}

struct FixedPrgBus<'a> {
    prg: &'a [u8],
    ram: [u8; 2048],
}

impl oracle_6502::Bus for FixedPrgBus<'_> {
    fn read(&mut self, address: u16) -> u8 {
        match address {
            0..=0x1fff => self.ram[usize::from(address) & 0x7ff],
            0x8000..=0xffff => self.prg[usize::from(address - 0x8000)],
            _ => panic!("untimed fixture oracle cannot read I/O ${address:04X}"),
        }
    }

    fn write(&mut self, address: u16, value: u8) {
        match address {
            0..=0x1fff => self.ram[usize::from(address) & 0x7ff] = value,
            0x8000..=0xffff => {} // Fixed PRG is not writable memory.
            _ => panic!("untimed fixture oracle cannot write I/O ${address:04X}"),
        }
    }
}

#[test]
fn literal_clock_programs_reach_declared_source_results_before_trap_endpoint() {
    // No cycle claim: existing instruction oracle checks source flow/state.
    // The independently literal cycle totals are tested only by assembled runs.
    for fixture in clock_fixtures() {
        let mut bus = FixedPrgBus {
            prg: &fixture.prg,
            ram: [0; 2048],
        };
        let mut cpu = oracle_6502::Cpu {
            pc: 0x8000,
            ..oracle_6502::Cpu::new()
        };
        for step in 0..1000 {
            if cpu.pc == fixture.stop {
                break;
            }
            cpu.step(&mut bus).unwrap();
            assert!(step < 999, "{} failed to reach endpoint", fixture.name);
        }
        assert_eq!(bus.ram[0x7ff], 0xa5, "{} completion marker", fixture.name);
        for &(address, value) in &fixture.expected {
            assert_eq!(
                bus.ram[usize::from(address)],
                value,
                "{} ${address:04X}",
                fixture.name
            );
        }
        if let Some(expected) = fixture.shadow_p {
            assert_eq!(cpu.p, expected, "{} reference P", fixture.name);
        }
    }
}

fn generate_clock(fixture: &ClockFixture) -> (PathBuf, std::process::Output) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let work = root.join(format!(
        "out/tests/cnrom-clock-{}{}-{}",
        fixture.name,
        if fixture.source_deferred {
            "-deferred"
        } else {
            ""
        },
        std::process::id()
    ));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("fixture.nes"), fixture.rom()).unwrap();
    let mut profile = "[rom]\nname='source-clock-fixture'\nmapper=3\nprg_kib=32\nchr_kib=32\n[translation]\nstack_discipline='software'\nruntime_defines=['CNROM_SOURCE_CLOCK_EXPERIMENT']\n".to_owned();
    if fixture.source_hardware {
        profile = profile.replace(
            "CNROM_SOURCE_CLOCK_EXPERIMENT",
            "CNROM_SOURCE_HARDWARE_EXPERIMENT",
        );
    }
    if fixture.source_deferred {
        assert!(fixture.source_hardware);
        profile = profile.replace(
            "'CNROM_SOURCE_HARDWARE_EXPERIMENT'",
            "'CNROM_SOURCE_HARDWARE_EXPERIMENT','CNROM_SOURCE_HARDWARE_DEFERRED_EXPERIMENT'",
        );
    }
    if fixture.fast_forward {
        profile.push_str("source_clock_fast_forward=true\n");
    }
    for &(at, zp) in &fixture.poll_loops {
        profile.push_str(&format!("[[source_poll_loop]]\nat={at}\nzp={zp}\n"));
    }
    for &entry in &fixture.extra_roots {
        profile.push_str(&format!(
            "[[function]]\naddr={entry}\nname='clock_entry_{entry:04x}'\n"
        ));
    }
    std::fs::write(work.join("profile.toml"), profile).unwrap();
    let generator = std::env::var_os("CNROM_GENERATOR_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_nes-to-sms").into());
    let generated = Command::new(generator)
        .arg(work.join("fixture.nes"))
        .arg(work.join("profile.toml"))
        .arg(work.join("sms"))
        .arg("--runtime")
        .arg(
            std::env::var_os("CNROM_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join("runtime")),
        )
        .output()
        .unwrap();
    std::fs::write(
        work.join("generate.log"),
        [&generated.stdout[..], &generated.stderr[..]].concat(),
    )
    .unwrap();
    (work, generated)
}

fn assemble_clock(fixture: &ClockFixture) -> PathBuf {
    let (work, generated) = generate_clock(fixture);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(
        generated.status.success(),
        "{} generation: {}",
        fixture.name,
        String::from_utf8_lossy(&generated.stderr)
    );
    let uid = Command::new("id").arg("-u").output().unwrap();
    let gid = Command::new("id").arg("-g").output().unwrap();
    assert!(uid.status.success() && gid.status.success());
    let user = format!(
        "{}:{}",
        String::from_utf8_lossy(&uid.stdout).trim(),
        String::from_utf8_lossy(&gid.stdout).trim()
    );
    let assembled = Command::new("docker")
        .args(["run", "--rm", "--network", "none", "--user", &user, "-v"])
        .arg(format!("{}:/work", root.display()))
        .args(["nes-to-sms-poc", "make", "-B", "-C"])
        .arg(
            PathBuf::from("/work")
                .join(work.strip_prefix(&root).unwrap())
                .join("sms"),
        )
        .output()
        .unwrap();
    std::fs::write(
        work.join("assemble.log"),
        [&assembled.stdout[..], &assembled.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        assembled.status.success(),
        "{} assembly: {}",
        fixture.name,
        String::from_utf8_lossy(&assembled.stderr)
    );
    work
}

fn large_directory_fixture() -> ClockFixture {
    // Thirty-two real nested source routines plus an NMI handler provide 3278
    // decoded PCs. Each short routine stays below the separate size ceiling.
    // All nested calls execute; the deepest wait takes a real NMI, then all31
    // guest RTS pairs return through the banked directory to the original main.
    let mut main = vec![0xa2, 0x7f, 0x9a, 0xa9, 0x80, 0x8d, 0, 0x20];
    main.extend([0xea; 100]);
    main.extend([0x20, 0, 0x81]);
    let mut fixture = ClockFixture::new(
        "directory-large",
        &main,
        27735,
        &[
            (0x20, 0),
            (0x21, 1),
            (0x141, 0x9f),
            (0x140, 0x68),
            (0x13f, 0x24),
            (0xb02, 0x7f),
            (0xa8c, 0x74),
            (0xa8d, 0x80),
        ],
    );
    for page in 0x81u16..0x9f {
        let mut body = vec![0xea; 100];
        body.extend([0x20, 0, (page + 1) as u8, 0x60]);
        fixture.place(page << 8, &body);
    }
    let mut deepest = vec![0xea; 100];
    deepest.extend([0xa9, 1, 0x85, 0x20, 0xa5, 0x20, 0xd0, 0xfc, 0x60]);
    fixture.place(0x9f00, &deepest);
    fixture.nmi = 0xc000;
    fixture.place(0xc000, &[0xa9, 0, 0x85, 0x20, 0xe6, 0x21, 0x40]);
    fixture.trace_steps = 30_000_000;
    fixture.shadow_p = Some(0xa4);
    // Before waiting: setup10 + NOP6400 + JSR186 + LDA/STA5 = C6601.
    // VBlank edge C27508 is LDA cycle3, so BNE polls at27509 and retires27511.
    // NMI7 + handler16 ->27534; exit LDA3/BNE2 ->27539;
    // 31*RTS6 ->27725; terminal10 ->27735. No host-frame count enters this total.
    fixture
}

#[test]
fn clock_dispatch_capacity_accepts_full_bankable_boundary_directory() {
    let fixture = large_directory_fixture();
    let (work, generated) = generate_clock(&fixture);
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    assert!(work.join("sms").exists());
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; >2687 real decoded PCs and guest continuations"]
fn assembled_large_directory_preserves_deep_rts_and_nmi_continuations() {
    assert_clock_fixture(&large_directory_fixture());
}

#[test]
#[ignore = "requires existing Docker WLA toolchain; translated code beyond logical bank23"]
fn assembled_dense_directory_executes_remapped_high_code_banks() {
    // 6400 ADC-immediate instructions produce enough actual translated bytes
    // to cross logical code bank23. A55+C0 plus ADC0 always remains55/P24.
    let mut main = vec![0xa2, 0xff, 0x9a, 0x18, 0xa9, 0x55];
    for _ in 0..100 {
        main.extend([0x69, 0]);
    }
    main.extend([0x20, 0, 0x81]);
    main.extend([0x20, 0, 0xe0, 0x20, 0x80, 0xe0]); // Execute all256 PCs in two bounded routines.
    let mut fixture = ClockFixture::new(
        "directory-dense",
        &main,
        14110,
        &[(0x500, 0x55), (0xb02, 0xff), (0xa8c, 0xdc), (0xa8d, 0x80)],
    );
    for page in 0x81u16..=0xbf {
        let mut body = Vec::new();
        for _ in 0..100 {
            body.extend([0x69, 0]);
        }
        if page == 0xbf {
            body.extend([0x8d, 0, 5, 0x60]);
        } else {
            body.extend([0x20, 0, (page + 1) as u8, 0x60]);
        }
        fixture.place(page << 8, &body);
    }
    fixture.shadow_p = Some(0xa4);
    fixture.trace_steps = 8_000_000;
    fixture.place(0xe000, &[0xea; 256]);
    fixture.place(0xe07f, &[0x60]);
    fixture.place(0xe0ff, &[0x60]);
    // Setup8 + 6400*ADC2 + 63*(JSR6+RTS6) + leaf store4 + full-page532 + terminal10.
    let work = assert_clock_fixture(&fixture);
    let symbols = std::fs::read_to_string(work.join("sms/sms.sym")).unwrap();
    let destination = symbols
        .lines()
        .find(|line| line.ends_with(" L_BF00"))
        .unwrap();
    let bank = u8::from_str_radix(destination.split(':').next().unwrap(), 16).unwrap();
    assert!(
        bank >= 36,
        "dense target must execute beyond the24..35 source/data reservation"
    );
}

#[test]
#[ignore = "requires completed CNROM source-clock runtime and existing Docker WLA toolchain"]
fn assembled_source_templates_keep_literal_cycles_and_guest_results() {
    for fixture in clock_fixtures() {
        assert_clock_fixture(&fixture);
    }
}

#[test]
#[ignore = "requires completed CNROM source-clock runtime and existing Docker WLA toolchain"]
fn assembled_source_status_nmi_brk_and_dma_keep_literal_timing_and_state() {
    for fixture in interrupt_and_dma_fixtures() {
        assert_clock_fixture(&fixture);
    }
}

fn assert_clock_fixture(fixture: &ClockFixture) -> PathBuf {
    let work = assemble_clock(fixture);
    let trace_binary = std::env::var_os("CNROM_TRACE_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_trace-sms").into());
    let mut trace = Command::new(trace_binary);
    if fixture.source_deferred {
        let symbols = std::fs::read_to_string(work.join("sms/sms.sym")).unwrap();
        trace.env("SMS_SOURCE_DOMAIN_FORMAT", domain_format(&symbols));
    }
    trace
        .arg(work.join("sms/sms.sms"))
        .arg("--steps")
        .arg(fixture.trace_steps.to_string());
    for (address, value) in fixture
        .expected
        .iter()
        .copied()
        .chain([(0x7ff, 0xa5), (0xb1d, 0xe8)])
        .chain(fixture.shadow_p.map(|p| (0xb03, p)))
        .chain(
            fixture
                .cycles
                .to_le_bytes()
                .into_iter()
                .enumerate()
                .map(|(index, value)| (0xa80 + index as u16, value)),
        )
    {
        trace
            .arg("--expect-ram")
            .arg(format!("{:04X}={value:02X}", 0xc000 + address));
    }
    let output = trace.output().unwrap();
    std::fs::write(
        work.join("trace.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{} runtime: {}\n{}",
        fixture.name,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    work
}

#[test]
#[ignore = "requires Docker WLA; genuine two-PHA computed RTS through pipeline rewrite"]
fn assembled_computed_rts_dispatch_keeps_six_source_cycles() {
    for (name, hardware) in [
        ("computed-rts-clock", false),
        ("computed-rts-hardware", true),
    ] {
        // Both unmatched pushes belong to one discovered routine, so the actual
        // pipeline's mark_rts_dispatch rewrite (not a hand-built IR) must fire.
        let mut fixture = ClockFixture::new(
            name,
            &[0xa2, 0x3f, 0x9a, 0xa9, 0x81, 0x48, 0xa9, 0x1f, 0x48, 0x60],
            if hardware { 40 } else { 33 },
            &[(0x21, 0x1f), (0x13e, 0x1f), (0x13f, 0x81), (0xb02, 0x3f)],
        );
        fixture.place(0x8120, &[0x85, 0x21]);
        fixture.finish_at(0x8122);
        fixture.extra_roots.push(0x8120);
        fixture.source_hardware = hardware;
        fixture.source_deferred = hardware;
        fixture.shadow_p = Some(0xa4);
        if hardware {
            fixture.place(fixture.stop, &[0xad, 0x00, 0x50]);
        }
        // The established instruction oracle independently checks the computed
        // target, guest stack, and surviving A, without claiming bus timing.
        let mut bus = FixedPrgBus {
            prg: &fixture.prg,
            ram: [0; 2048],
        };
        let mut cpu = oracle_6502::Cpu {
            pc: 0x8000,
            ..oracle_6502::Cpu::new()
        };
        for _ in 0..20 {
            if cpu.pc == fixture.stop {
                break;
            }
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!((cpu.pc, cpu.a, cpu.sp, cpu.p), (0x8127, 0xa5, 0x3f, 0xa4));
        assert_eq!(
            (bus.ram[0x21], bus.ram[0x13e], bus.ram[0x13f]),
            (0x1f, 0x1f, 0x81)
        );
        let work = assert_clock_fixture(&fixture);
        eprintln!("computed RTS {name}: {}", work.display());
    }
}

fn source_irq_fixture(kind: &str) -> ClockFixture {
    let (name, mut main, setup_cycles, boundary, opcode, pushed_p, delayed) = match kind {
        "cli" => (
            "hardware-irq-cli",
            vec![0xa2, 0x3f, 0x9a],
            4,
            29828,
            0x58,
            0x22,
            true,
        ),
        "sei" => (
            "hardware-irq-sei",
            vec![0xa2, 0x3f, 0x9a, 0x58],
            6,
            29828,
            0x78,
            0x26,
            false,
        ),
        "plp-mask" => (
            "hardware-irq-plp-mask",
            vec![0xa2, 0x3e, 0x9a, 0xa9, 0x24, 0x8d, 0x3f, 1, 0x58],
            12,
            29826,
            0x28,
            0x24,
            false,
        ),
        "plp-unmask" => (
            "hardware-irq-plp-unmask",
            vec![0xa2, 0x3e, 0x9a, 0xa9, 0x20, 0x8d, 0x3f, 1],
            10,
            29826,
            0x28,
            0x20,
            true,
        ),
        "rti" => (
            "hardware-irq-rti",
            vec![
                0xa2, 0x3c, 0x9a, 0xa9, 0x20, 0x8d, 0x3d, 1, 0xa9, 0, 0x8d, 0x3e, 1, 0xa9, 0x90,
                0x8d, 0x3f, 1,
            ],
            22,
            29824,
            0x40,
            0x20,
            false,
        ),
        _ => unreachable!(),
    };
    // Enable pulse1 with length10. Its first half-frame event reduces it to9;
    // source $4015 therefore reports41 (active pulse + newly asserted IRQ).
    main.extend([0xa9, 1, 0x8d, 0x15, 0x40, 0xa9, 0, 0x8d, 3, 0x40]);
    // Literal delay: LDY23=2; each outer body LDX0=2, 256 DEX/BNE pairs=1279,
    // DEY/BNE=5 except last4. Total 2 +23*1286 -1 =29579 source cycles.
    main.extend([0xa0, 23, 0xa2, 0, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8]);
    let reached = 7 + setup_cycles + 12 + 29579; // Includes seven reset reads.
    assert_eq!((boundary - reached) % 2, 0);
    main.extend(vec![0xea; (boundary - reached) / 2]);
    let critical = 0x8000 + main.len() as u16;
    main.extend([opcode, 0xea]);
    let return_pc = if opcode == 0x40 {
        0x9000
    } else {
        critical + 1 + u16::from(delayed)
    };
    let cycles = if opcode == 0x40 { 29887 } else { 29889 };
    let mut fixture = ClockFixture::new(
        name,
        &main,
        cycles,
        &[
            (0x500, pushed_p),
            (0x501, return_pc as u8),
            (0x502, (return_pc >> 8) as u8),
            (0x503, 0x41),
            (0xabd, 0x3c), // Source-boundary X/Y snapshots, not inactive CB00.
            (0xabc, 0),
            (0xb02, 0x3f),
            (0x1500, 0),
            (0x1501, 0),
            (0x1400, 1), // IRQ cleared; frame0 retired, frame1 capture in flight.
            (0xa88, 1),
        ],
    );
    fixture.source_hardware = true;
    fixture.trace_steps = 12_000_000;
    fixture.irq = 0xc000;
    // Observe the REAL guest stack, then read internal APU status outside the
    // three-cycle IRQ-assertion race, and RTI without fake frame ownership.
    fixture.place(
        0xc000,
        &[
            0xba, 0xbd, 1, 1, 0x8d, 0, 5, 0xbd, 2, 1, 0x8d, 1, 5, 0xbd, 3, 1, 0x8d, 2, 5, 0xad,
            0x15, 0x40, 0x8d, 3, 5, 0x40,
        ],
    ); // 2 +3*(4+4) +4+4 +6 =40.
    if opcode == 0x40 {
        fixture.finish_at(0x9000);
        fixture.extra_roots.push(0x9000);
    }
    fixture.place(fixture.stop, &[0xad, 0, 0x50]); // Explicit guarded expansion.
    fixture.shadow_p = Some(if pushed_p & 4 != 0 { 0xa4 } else { 0xa0 });
    fixture
}

#[test]
#[ignore = "requires Docker WLA; genuine source APU IRQ at cold cycle29829"]
fn assembled_source_hardware_irq_polls_cli_sei_plp_and_rti_at_literal_phases() {
    for kind in ["cli", "sei", "plp-mask", "plp-unmask", "rti"] {
        let work = assert_clock_fixture(&source_irq_fixture(kind));
        eprintln!("hardware IRQ {kind}: {}", work.display());
    }
}

fn source_render_fixture() -> ClockFixture {
    let delay = |count| [0xa0, count, 0xa2, 0, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8];
    let mut main = delay(23).to_vec(); // C7+29579=29586, before startup release.
    main.extend([0xea; 42]); // C29670, after release at29668.
    main.extend([0xa9, 0x40, 0x8d, 0x17, 0x40]); // Inhibit APU IRQ, C29676.
    main.extend([0xa9, 0, 0x8d, 3, 0x20]); // OAMADDR0, C29682.
    main.extend([0xa2, 0, 0xa9, 0xff, 0x8d, 4, 0x20, 0xe8, 0xd0, 0xfa]);
    // LDX/LDA4 +256*(STA4+INX2+BNE3)-1 =2307; OAMADDR wraps0.
    main.extend(delay(20)); // C31989+25721=57710, second source VBlank.
    main.extend([0xa9, 0x3f, 0x8d, 6, 0x20, 0xa9, 0, 0x8d, 6, 0x20]);
    for index in 0..32 {
        main.extend([0xa9, [0x0f, 0x30, 0x01, 0x16][index & 3], 0x8d, 7, 0x20]);
    }
    // CIRAM is the declared cold zero-filled memory: every cell uses tile0 /
    // palette0. Clear ctrl/scroll/address through actual translated writes.
    main.extend([
        0xa9, 0, 0x8d, 0, 0x20, 0x8d, 5, 0x20, 0x8d, 5, 0x20, 0x8d, 6, 0x20, 0x8d, 6, 0x20,
    ]);
    // Palette changes and scroll/address setup finish C57936, still VBlank.
    main.extend([0xa9, 0x1e, 0x8d, 1, 0x20]); // Enable both layers C57942.
    main.extend(delay(23)); // C87521, beyond whole rendered interval2.
    let mut fixture = ClockFixture::new(
        "hardware-rendered-frame",
        &main,
        87531,
        &[
            (0x1400, 0),
            (0x1406, 2),
            (0x1407, 0),
            (0x1408, 0),
            (0x1409, 0),
            (0xa88, 2),
            (0xb09, 0x1e),
            (0xb0a, 0),
            (0x1317, 0),
            (0x1500, 0),
        ],
    );
    fixture.source_hardware = true;
    fixture.trace_steps = 45_000_000;
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    fixture.chr[..16].copy_from_slice(&[
        0x5a, 0xa5, 0xff, 0, 0xff, 0, 0x55, 0xaa, 0x3c, 0xc3, 0, 0xff, 0xff, 0, 0x55, 0xaa,
    ]);
    fixture
}

#[test]
#[ignore = "requires Docker WLA; complete frame from genuine translated NES register writes"]
fn assembled_source_hardware_builds_and_retires_its_first_literal_rendered_frame() {
    let work = assert_clock_fixture(&source_render_fixture());
    eprintln!("hardware rendered frame: {}", work.display());
}

fn source_status_fixture() -> ClockFixture {
    let mut main = vec![0xa0, 21, 0xa2, 0, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8];
    // Seven reset +27007 delay +LDAzp3 +186NOP372 =27389.
    main.extend([0xa5, 0]);
    main.extend([0xea; 186]);
    main.extend([0xad, 2, 0x20, 0x8d, 0, 5]); // Read27393; store27397.
    let mut fixture = ClockFixture::new(
        "hardware-status-27393",
        &main,
        27407,
        &[(0x500, 0), (0xaa3, 1), (0xaa5, 0), (0x1400, 0), (0xa88, 0)],
    );
    fixture.source_hardware = true;
    fixture.trace_steps = 12_000_000;
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    fixture.shadow_p = Some(0xa4);
    fixture
}

#[test]
#[ignore = "requires Docker WLA; translated startup poll three dots before first VBlank"]
fn assembled_source_hardware_status_at_27393_is_normal_not_a_collision() {
    let work = assert_clock_fixture(&source_status_fixture());
    eprintln!("hardware status27393: {}", work.display());
}

#[test]
#[ignore = "requires Docker WLA; exact off/on deferred-domain source fixtures"]
fn assembled_source_hardware_deferred_pairs_keep_literal_endpoints() {
    for mut fixture in [
        source_irq_fixture("cli"),
        source_irq_fixture("rti"),
        source_status_fixture(),
        source_render_fixture(),
    ] {
        let precise = assert_clock_fixture(&fixture);
        fixture.source_deferred = true;
        let deferred = assert_clock_fixture(&fixture);
        eprintln!(
            "domain pair {}: precise={} deferred={}",
            fixture.name,
            precise.display(),
            deferred.display()
        );
    }
}

#[test]
#[ignore = "requires CNROM_DEFERRED_PROJECT assembled deferred subject"]
fn deferred_cli_requires_explicit_domain_observer_acknowledgment() {
    let project = PathBuf::from(std::env::var("CNROM_DEFERRED_PROJECT").unwrap());
    let symbols = std::fs::read_to_string(project.join("sms.sym")).unwrap();
    let format = domain_format(&symbols);
    let trace = std::env::var_os("CNROM_TRACE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_trace-sms")));
    for acknowledgment in [
        None,
        Some("incorrect-v0"),
        Some("deferred-v1"),
        Some("deferred-v2"),
    ]
    .into_iter()
    .filter(|value| *value != Some(format))
    {
        let mut command = Command::new(&trace);
        command.arg(project.join("sms.sms")).args(["--steps", "1"]);
        command.env_remove("SMS_SOURCE_DOMAIN_FORMAT");
        if let Some(value) = acknowledgment {
            command.env("SMS_SOURCE_DOMAIN_FORMAT", value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains(&format!("require SMS_SOURCE_DOMAIN_FORMAT={format}"))
        );
    }
    let output = Command::new(trace)
        .arg(project.join("sms.sms"))
        .args(["--search-late-routes", "--steps", "1"])
        .env("SMS_SOURCE_DOMAIN_FORMAT", format)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("legacy route observers do not support")
    );
}

fn domain_format(symbols: &str) -> &'static str {
    if symbols.contains("rt_source_domain_format_v3") {
        "deferred-v3"
    } else if symbols.contains("rt_source_domain_format_v2") {
        "deferred-v2"
    } else {
        "deferred-v1"
    }
}

#[test]
#[ignore = "requires Docker WLA and frozen v3 runtime; same-source precise/ordinary/CPU-wait triple"]
fn assembled_source_rendered_poll_triple_keeps_literal_nmi_exit_and_frame() {
    let mut fixture = source_render_fixture();
    let tail = fixture.stop - 5 - 10;
    assert_eq!(
        &fixture.prg[usize::from(tail - 0x8000)..usize::from(tail - 0x8000) + 10],
        &[0xa0, 23, 0xa2, 0, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8]
    );
    // C57942: preserve the existing genuine cold setup and whole rendered
    // picture; replace only its final busy delay with an annotated guest wait.
    fixture.place(
        tail,
        &[
            0xad, 2, 0x20, 0xa9, 0x80, 0x8d, 0, 0x20, 0xa9, 1, 0x85, 0x20, 0x4c, 0, 0x82,
        ],
    );
    fixture.place(0x8200, &[0xa5, 0x20, 0xd0, 0xfc]);
    fixture.finish_at(0x8204);
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    fixture.nmi = 0xc000;
    fixture.place(0xc000, &[0xa9, 0, 0x85, 0x20, 0xe6, 0x21, 0x40]);
    fixture.cycles = 86996;
    fixture.shadow_p = Some(0xa4);
    fixture.expected.extend([
        (0x20, 0),
        (0x21, 1),
        (0x1fd, 0x82),
        (0x1fc, 0),
        (0x1fb, 0x24),
        (0xb02, 0xfd),
        (0xb08, 0x80),
        (0xa84, 124),
        (0xa85, 0),
        (0xa86, 241),
        (0xa87, 0),
    ]);
    fixture.poll_loops.push((0x8200, 0x20));
    for (name, deferred, fast) in [
        ("hardware-rendered-poll-precise", false, false),
        ("hardware-rendered-poll-ordinary", true, false),
        ("hardware-rendered-poll-fast", true, true),
    ] {
        fixture.name = name;
        fixture.source_deferred = deferred;
        fixture.fast_forward = fast;
        let work = assert_clock_fixture(&fixture);
        eprintln!("rendered poll {name}: {}", work.display());
    }
}

#[test]
#[ignore = "requires Docker WLA and CNROM_RUNTIME_DIR frozen precise source-hardware runtime"]
fn assembled_source_hardware_audio_two_literal_periods_and_silence() {
    assert!(
        std::env::var_os("CNROM_RUNTIME_DIR").is_some(),
        "freeze runtime input explicitly"
    );
    let delay = |count| [0xa0, count, 0xa2, 0, 0xca, 0xd0, 0xfd, 0x88, 0xd0, 0xf8];
    let mut main = vec![0xa9, 0x40, 0x8d, 0x17, 0x40]; // C13 IRQ inhibit.
    main.extend(delay(23)); //29579 original cycles, C29592.
    for (address, value) in [
        (0x4000u16, 0x3f),
        (0x4001, 0),
        (0x4002, 0xfd),
        (0x4015, 1),
        (0x4003, 0),
    ] {
        main.extend([0xa9, value, 0x8d, address as u8, (address >> 8) as u8]);
    }
    // Timer253 +1 -> SMSperiod254 atC29622, constantvolume15/lengthhalt.
    main.extend([0xa9, 0xa1, 0x8d, 0xfe, 7]); // C29628.
    main.extend(delay(10)); //12861 cycles ->42489.
    main.extend([0xa9, 0x7e, 0x8d, 2, 0x40]); // C42495 period127.
    main.extend([0xa9, 0xa2, 0x8d, 0xfe, 7]); // C42501.
    main.extend(delay(10)); // C55362.
    main.extend([0xa9, 0, 0x8d, 0x15, 0x40]); // C55368 silence.
    main.extend([0xa9, 0xa3, 0x8d, 0xfe, 7]); // C55374.
    let mut fixture = ClockFixture::new(
        "hardware-audio-periods",
        &main,
        55384,
        &[
            (0x7fe, 0xa3),
            (0xb02, 0xfd),
            (0xb09, 0),
            (0xb30, 0x3f),
            (0xb32, 0x7e),
            (0xb45, 0),
            (0x1500, 0),
            (0x1501, 0),
            (0xa88, 1),
            (0xa84, 85),
            (0xa85, 0),
            (0xa86, 225),
            (0xa87, 0),
        ],
    );
    fixture.source_hardware = true;
    fixture.shadow_p = Some(0xa4);
    fixture.trace_steps = 21_000_000;
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    let work = assert_clock_fixture(&fixture);
    eprintln!("genuine source audio: {}", work.display());
}

#[test]
#[ignore = "requires Docker WLA and CNROM_RUNTIME_DIR isolated materializer runtime"]
fn assembled_source_hardware_vertical_ppu_literal_subject() {
    assert!(std::env::var_os("CNROM_RUNTIME_DIR").is_some());
    let mut fixture = ClockFixture::new("hardware-ppu-vertical", &[0xea], 19, &[]);
    fixture.source_hardware = true;
    fixture.vertical = true;
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    let work = assert_clock_fixture(&fixture);
    let assembly = std::fs::read_to_string(work.join("sms/sms.asm")).unwrap();
    assert!(assembly.contains(".define NES_MIRRORING_VERTICAL"));
    eprintln!("vertical source PPU subject: {}", work.display());
}

#[test]
#[ignore = "requires Docker WLA and CNROM_RUNTIME_DIR isolated materializer runtime"]
fn assembled_source_hardware_horizontal_ppu_literal_subject() {
    assert!(std::env::var_os("CNROM_RUNTIME_DIR").is_some());
    let mut fixture = ClockFixture::new("hardware-ppu-horizontal", &[0xea], 19, &[]);
    fixture.source_hardware = true;
    fixture.place(fixture.stop, &[0xad, 0, 0x50]);
    let work = assert_clock_fixture(&fixture);
    let assembly = std::fs::read_to_string(work.join("sms/sms.asm")).unwrap();
    assert!(!assembly.contains(".define NES_MIRRORING_VERTICAL"));
    eprintln!("horizontal source PPU subject: {}", work.display());
}
