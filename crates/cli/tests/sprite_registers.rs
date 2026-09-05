//! Generic inline sprite-register policy, executed as emitted Z80 bytes.
use ir::{Op, Routine, ValueSrc};
use lower::LowerOptions;
use z80_emu::{Bus, Cpu};

struct Machine {
    ram: Box<[u8; 65536]>,
    ports: Vec<(u8, u8)>,
}

impl Bus for Machine {
    fn read(&mut self, address: u16) -> u8 {
        self.ram[address as usize]
    }
    fn write(&mut self, address: u16, value: u8) {
        self.ram[address as usize] = value;
    }
    fn in_port(&mut self, _: u8) -> u8 {
        0xff
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.ports.push((port, value));
    }
}

fn lowered_control(chr_ram: bool, deferred: bool) -> Vec<u8> {
    let profile = profile::load_from_str(&format!(
        "[rom]\nname='Synthetic'\nmapper=0\nprg_kib=32\nchr_kib={}\n[translation]\ndefer_sprite_registers={deferred}\n",
        if chr_ram { 0 } else { 8 }
    )).unwrap();
    let routine = Routine {
        entry: 0x8000,
        end: 0x8003,
        name: "control".into(),
        ops: vec![
            Op::PpuWrite {
                reg: 0,
                value: ValueSrc::A,
            },
            Op::Rts,
        ],
        branch_labels: vec![],
        external_calls: vec![],
        unresolved: vec![],
    };
    let mut program = z80_emit::Program::new();
    lower::lower_routine(
        &mut program,
        &routine,
        &LowerOptions {
            profile: Some(&profile),
            ..LowerOptions::default()
        },
    )
    .unwrap();
    program.label("rt_translated_rts");
    program.halt();
    program.finish().unwrap().bytes
}

fn run(bytes: &[u8], control: u8, mask: u8, iff: bool) -> (Machine, Cpu) {
    let mut m = Machine {
        ram: Box::new([0; 65536]),
        ports: vec![],
    };
    m.ram[..bytes.len()].copy_from_slice(bytes);
    m.ram[0xcb03] = 0xcd;
    m.ram[0xcb08] = 0xb0;
    m.ram[0xcb09] = mask;
    let mut cpu = Cpu::new();
    cpu.a = control;
    cpu.d = 0x52;
    cpu.e = 0xa9;
    cpu.sp = 0xdffc;
    cpu.iff1 = iff;
    cpu.iff2 = iff;
    for _ in 0..1000 {
        if cpu.halted {
            return (m, cpu);
        }
        if let Err(error) = cpu.step(&mut m) {
            assert!(cpu.halted, "unexpected instruction error: {error:?}");
        }
    }
    panic!("inline CTRL did not complete");
}

#[test]
fn deferred_inline_control_preserves_guest_effects_without_transient_ports() {
    for chr_ram in [false, true] {
        let legacy = lowered_control(chr_ram, false);
        let deferred = lowered_control(chr_ram, true);
        for iff in [false, true] {
            for mask in [0, 8, 0x10, 0x18] {
                for control in 0..=255 {
                    let (old, old_cpu) = run(&legacy, control, mask, iff);
                    let (new, cpu) = run(&deferred, control, mask, iff);
                    assert_eq!(&new.ram[0xc000..], &old.ram[0xc000..]);
                    assert_eq!(
                        (cpu.a, cpu.d, cpu.e, cpu.sp, cpu.iff1, cpu.iff2),
                        (
                            old_cpu.a,
                            old_cpu.d,
                            old_cpu.e,
                            old_cpu.sp,
                            old_cpu.iff1,
                            old_cpu.iff2
                        )
                    );
                    assert_eq!((cpu.a, cpu.d, cpu.e, cpu.sp), (control, 0x52, 0xa9, 0xdffc));
                    assert_eq!(new.ram[0xcb03], 0xcd, "STA preserves guest flags");
                    assert_eq!(new.ram[0xcb08], control);
                    let wanted_reg1 = 0xb0
                        | if mask & 0x18 != 0 { 0x40 } else { 0 }
                        | if control & 0x20 != 0 { 2 } else { 0 };
                    assert_eq!(new.ram[0xcb2d], wanted_reg1);
                    let base = if control & 0x20 == 0 && control & 8 != 0 {
                        0xfb
                    } else {
                        0xff
                    };
                    assert_eq!(old.ports, [(0xbf, base), (0xbf, 0x86)]);
                    assert!(new.ports.is_empty(), "physical base belongs to SAT commit");
                }
            }
        }
    }
}
