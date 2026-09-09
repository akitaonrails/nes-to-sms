//! Independent canonical-atlas primitive evidence. These drive the assembled
//! cold/begin/intern/resolve/retire gates directly with literal candidate
//! bytes. They are not publisher integration, packet coverage, upload-budget
//! closure or Adventure Island rendering acceptance; pair interning has its
//! own explicit current-behavior check.

use super::*;

const AT_CAPACITY: u16 = 378;
const AT_PAYLOAD: usize = 0x8800;
const AT_FLAGS: usize = 0xb740;
const AT_HANDLES: usize = 0xb8ba;
const AT_HEADS: usize = 0xbaba;
const AT_LINKS: usize = 0xbcba;
const AT_FAULT: usize = 0xbfb0;
const AT_ALLOCATED_COUNT: usize = 0xbfb4;
const AT_BG_CANDIDATE: u16 = 0xabc0;

const AT_ALLOCATED: u8 = 0x01;
const AT_OLD: u8 = 0x02;
const AT_PENDING: u8 = 0x04;
const AT_DIRTY: u8 = 0x08;
const AT_PERMANENT_ZERO: u8 = 0x40;

/// Atlas storage lives in SRAM bank 1; candidates live in SRAM bank 0.
fn sram1(addr: usize) -> usize {
    BANK_SIZE + (addr - 0x8000)
}

fn sram0(addr: usize) -> usize {
    addr - 0x8000
}

fn atlas_fixture() -> (SmsBus, HashMap<String, (u8, u16)>) {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_ATLAS_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let bus = SmsBus::new(std::fs::read(path.join("sms.sms")).unwrap(), 0xff);
    (bus, defs)
}

/// The declared dense-ordinal to legal-physical-pattern mapping: 0..55 keep
/// their number, 56..375 shift past the $0700 name table to 120..439, and
/// 376..377 occupy 506..507 after the $3700 name table.
fn expected_physical(ordinal: u16) -> u16 {
    if ordinal >= 376 {
        ordinal + 130
    } else if ordinal >= 56 {
        ordinal + 64
    } else {
        ordinal
    }
}

/// Invoke one atlas gate. Asserts the caller's mapper registers, slot-1
/// shadow and native stack come back exactly, and that no trap fired.
fn atlas_call(
    bus: &mut SmsBus,
    defs: &HashMap<String, (u8, u16)>,
    label: &str,
    hl: u16,
) -> (u16, u16) {
    let mut cpu = Cpu::new();
    cpu.set_hl(hl);
    cpu.pc = defs[label].1;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    let mapping = (bus.mapper_control, bus.slot_bank, bus.ram[0x0b14]);
    for _ in 0..6_000_000 {
        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
            break;
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    assert_eq!(bus.read(0xcb1d), 0, "{label} trapped PC={:04X}", cpu.pc);
    assert_eq!(cpu.pc, 7, "{label} did not return");
    assert_eq!(
        (bus.mapper_control, bus.slot_bank, bus.ram[0x0b14]),
        mapping,
        "{label} must restore caller mapping"
    );
    (cpu.hl(), cpu.de())
}

/// Invoke one atlas gate expecting the strict packet trap; returns the
/// recorded fault selector. Bank restoration is intentionally not claimed on
/// this fatal path.
fn atlas_call_fault(
    bus: &mut SmsBus,
    defs: &HashMap<String, (u8, u16)>,
    label: &str,
    hl: u16,
) -> u8 {
    let mut cpu = Cpu::new();
    cpu.set_hl(hl);
    cpu.pc = defs[label].1;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    for _ in 0..6_000_000 {
        if cpu.pc == 7 || bus.read(0xcb1d) != 0 {
            break;
        }
        cpu.step(bus).unwrap();
    }
    assert_eq!(bus.read(0xcb1d), 0xea, "{label} must hit the strict trap");
    bus.cart_ram[sram1(AT_FAULT)]
}

/// Write one 32-byte candidate into the SRAM bank-0 staging window.
fn stage_candidate(bus: &mut SmsBus, bytes: &[u8; 32]) {
    let caller_mapping = bus.mapper_control;
    bus.write(0xfffc, 0x08);
    for (offset, value) in bytes.iter().enumerate() {
        bus.write(AT_BG_CANDIDATE + offset as u16, *value);
    }
    bus.write(0xfffc, caller_mapping);
}

/// Distinct literal candidate i: byte0/byte1 carry the index so the XOR hash
/// spreads across chains while identical indices stay byte-identical.
fn candidate(i: u16) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = (i & 0xff) as u8;
    bytes[1] = (i >> 8) as u8;
    bytes[2] = 0xa5;
    bytes
}

fn flags(bus: &SmsBus, ordinal: u16) -> u8 {
    bus.cart_ram[sram1(AT_FLAGS) + usize::from(ordinal)]
}

fn head(bus: &SmsBus, hash: u8) -> u16 {
    let base = sram1(AT_HEADS) + usize::from(hash) * 2;
    u16::from_le_bytes([bus.cart_ram[base], bus.cart_ram[base + 1]])
}

fn link(bus: &SmsBus, ordinal: u16) -> u16 {
    let base = sram1(AT_LINKS) + usize::from(ordinal) * 2;
    u16::from_le_bytes([bus.cart_ram[base], bus.cart_ram[base + 1]])
}

fn payload<'a>(bus: &'a SmsBus, ordinal: u16) -> &'a [u8] {
    let base = sram1(AT_PAYLOAD) + usize::from(ordinal) * 32;
    &bus.cart_ram[base..base + 32]
}

fn cold_boot(bus: &mut SmsBus, defs: &HashMap<String, (u8, u16)>) {
    // Caller state the gate must restore: SRAM bank 0 mapped, distinct banks.
    bus.write(0xfffc, 0x08);
    bus.write(0xfffe, 0x05);
    bus.write(0xffff, 0x07);
    bus.write(0xcb14, 0x05);
    atlas_call(bus, defs, "rt_chr_atlas_cold_init", 0);
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_cold_init_establishes_permanent_zero_and_both_blank_tables() {
    let (mut bus, defs) = atlas_fixture();
    // Pre-dirty everything the cold path owns, plus the guest-SRAM window it
    // must NOT touch.
    for offset in 0..CART_RAM_SIZE {
        bus.cart_ram[offset] = 0x77;
    }
    for value in bus.vram.iter_mut() {
        *value = 0x77;
    }
    cold_boot(&mut bus, &defs);

    // Optional guest cartridge RAM $8000..$87FF stays untouched in BOTH banks.
    for offset in 0..0x800 {
        assert_eq!(
            bus.cart_ram[sram1(0x8000 + offset)],
            0x77,
            "guest SRAM1 {offset:04X}"
        );
        assert_eq!(
            bus.cart_ram[sram0(0x8000 + offset)],
            0x77,
            "guest SRAM0 {offset:04X}"
        );
    }
    // Payload and state zeroed; handle/hash arrays reset to empty $FF.
    assert!(payload(&bus, 0).iter().all(|&b| b == 0));
    assert_eq!(flags(&bus, 0), AT_ALLOCATED | AT_PERMANENT_ZERO);
    for ordinal in 1..AT_CAPACITY {
        assert_eq!(flags(&bus, ordinal), 0, "flags {ordinal}");
        assert_eq!(link(&bus, ordinal), 0xffff, "link {ordinal}");
    }
    for offset in 0..(AT_HEADS - AT_HANDLES) {
        assert_eq!(
            bus.cart_ram[sram1(AT_HANDLES) + offset],
            0xff,
            "handle {offset}"
        );
    }
    assert_eq!(head(&bus, 0), 0, "zero pattern owns hash chain 0");
    for hash in 1..=255u8 {
        assert_eq!(head(&bus, hash), 0xffff, "head {hash}");
    }
    let allocated = u16::from_le_bytes([
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(allocated, 1);

    // VRAM: physical pattern 0 blank, both $0700/$3700 name-table windows
    // (with their $0E00/$3E00 tails) blank, name-table base at $0700.
    assert!(bus.vram[0..32].iter().all(|&b| b == 0));
    assert!(bus.vram[0x0700..0x0f00].iter().all(|&b| b == 0));
    assert!(bus.vram[0x3700..0x3f00].iter().all(|&b| b == 0));
    assert_eq!(bus.vdp_regs[2], 0xf3);
    // SRAM bank-0 staging window cleared; candidate row is inside it.
    for offset in 0..0x800 {
        assert_eq!(
            bus.cart_ram[sram0(0xa000 + offset)],
            0,
            "staging {offset:04X}"
        );
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_interning_dedupes_chains_maps_physicals_and_faults_at_capacity() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);

    // First candidate XORs to hash 0, deliberately colliding with the
    // permanent zero pattern's chain.
    let colliding = {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x3c;
        bytes[1] = 0x3c;
        bytes
    };
    stage_candidate(&mut bus, &colliding);
    let (ordinal, physical) =
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE);
    assert_eq!((ordinal, physical), (1, 1));
    assert_eq!(flags(&bus, 1), AT_ALLOCATED | AT_PENDING | AT_DIRTY);
    assert_eq!(payload(&bus, 1), &colliding[..]);
    // Chain 0 now walks new -> permanent zero.
    assert_eq!(head(&bus, 0), 1);
    assert_eq!(link(&bus, 1), 0);

    // Byte-identical candidate dedupes to the same ordinal without allocating.
    stage_candidate(&mut bus, &colliding);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (1, 1)
    );
    // The all-zero candidate dedupes onto the protected permanent zero slot.
    stage_candidate(&mut bus, &[0u8; 32]);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (0, 0)
    );
    assert_eq!(
        flags(&bus, 0),
        AT_ALLOCATED | AT_PERMANENT_ZERO | AT_PENDING
    );

    // Fill every remaining slot with distinct candidates and check the
    // declared dense->physical mapping at each legality boundary.
    for i in 2..AT_CAPACITY {
        stage_candidate(&mut bus, &candidate(i));
        let (ordinal, physical) =
            atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE);
        assert_eq!(ordinal, i, "sequential allocation");
        assert_eq!(physical, expected_physical(i), "physical {i}");
    }
    let allocated = u16::from_le_bytes([
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(allocated, AT_CAPACITY);
    assert_eq!(expected_physical(55), 55);
    assert_eq!(expected_physical(56), 120);
    assert_eq!(expected_physical(375), 439);
    assert_eq!(expected_physical(376), 506);
    assert_eq!(expected_physical(377), 507);

    // Every slot is now pending in this packet: one more distinct candidate
    // must fail closed with the capacity fault, never evict live pixels.
    stage_candidate(&mut bus, &candidate(0x2000));
    assert_eq!(
        atlas_call_fault(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        1
    );
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_retirement_protects_two_generations_then_evicts_and_unlinks() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);

    // Packet A interns P1.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    let p1 = candidate(0x0105); // hash $A1
    stage_candidate(&mut bus, &p1);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (1, 1)
    );
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    assert_eq!(flags(&bus, 1), AT_ALLOCATED | AT_OLD | AT_DIRTY);
    assert_eq!(flags(&bus, 0), AT_ALLOCATED | AT_PERMANENT_ZERO);

    // Packet B interns only P2: P1 is the displayed picture and stays
    // protected, so P2 lands in the next slot.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    let p2 = candidate(0x0107); // hash $A3
    stage_candidate(&mut bus, &p2);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (2, 2)
    );
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    assert_eq!(
        flags(&bus, 1),
        AT_ALLOCATED | AT_DIRTY,
        "P1 generation expired"
    );
    assert_eq!(flags(&bus, 2), AT_ALLOCATED | AT_OLD | AT_DIRTY);

    // Packet C interns P3: slot 1 is reclaimable now, and its stale hash-4
    // chain entry must be unlinked exactly.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    let p3 = candidate(0x010b); // hash $AF
    stage_candidate(&mut bus, &p3);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (1, 1)
    );
    assert_eq!(payload(&bus, 1), &p3[..]);
    assert_eq!(head(&bus, 0xa1), 0xffff, "stale chain emptied");
    assert_eq!(head(&bus, 0xaf), 1, "new chain owns the recycled slot");
    assert_eq!(link(&bus, 1), 0xffff);
    // The displayed generation (P2) was untouched.
    assert_eq!(payload(&bus, 2), &p2[..]);
    assert_eq!(flags(&bus, 2), AT_ALLOCATED | AT_OLD | AT_DIRTY);
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_rejects_bad_arguments_and_pair_interning_stays_unadmitted() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);

    // Resolving an unallocated ordinal fails closed.
    let (mut bus2, _) = atlas_fixture();
    bus2.cart_ram.copy_from_slice(&bus.cart_ram);
    bus2.write(0xfffc, 0x08);
    assert_eq!(
        atlas_call_fault(&mut bus2, &defs, "rt_chr_atlas_resolve", 5),
        3
    );
    // Out-of-range ordinal fails closed.
    let (mut bus3, _) = atlas_fixture();
    bus3.cart_ram.copy_from_slice(&bus.cart_ram);
    bus3.write(0xfffc, 0x08);
    assert_eq!(
        atlas_call_fault(&mut bus3, &defs, "rt_chr_atlas_resolve", AT_CAPACITY),
        3
    );
    // A candidate outside the declared staging row is rejected before any copy.
    let (mut bus4, _) = atlas_fixture();
    bus4.cart_ram.copy_from_slice(&bus.cart_ram);
    bus4.write(0xfffc, 0x08);
    assert_eq!(
        atlas_call_fault(&mut bus4, &defs, "rt_chr_atlas_intern_bg", 0xac00),
        3
    );
    // Pair interning is documented as not yet admitted: it must fail closed
    // with its explicit selector, not silently allocate.
    assert_eq!(
        atlas_call_fault(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
        8
    );
}
