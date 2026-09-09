//! Independent canonical-atlas primitive evidence. These drive the assembled
//! cold/begin/intern/resolve/retire gates directly with literal candidate
//! bytes, including aligned 8x16 pair interning. They are not publisher
//! integration, packet coverage, upload-budget closure or Adventure Island
//! rendering acceptance.

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
const AT_PAIR_FIRST: u8 = 0x10;
const AT_PAIR_SECOND: u8 = 0x20;
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
fn assembled_atlas_rejects_bad_arguments_on_every_gate() {
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
    // A pair candidate outside the declared 64-byte row is rejected too.
    assert_eq!(
        atlas_call_fault(&mut bus, &defs, "rt_chr_atlas_intern_pair", 0xabe0),
        3
    );
}

/// One 64-byte pair candidate: top half varies by index, bottom half all
/// zero so the 64-byte hash deliberately equals the top half's 32-byte hash
/// (kind discrimination, not hashing, must separate singles from pairs).
fn pair_candidate(i: u16) -> [u8; 64] {
    let mut bytes = [0u8; 64];
    bytes[0] = (i & 0xff) as u8;
    bytes[1] = (i >> 8) as u8;
    bytes[2] = 0x5a;
    bytes
}

fn stage_pair(bus: &mut SmsBus, bytes: &[u8; 64]) {
    let caller_mapping = bus.mapper_control;
    bus.write(0xfffc, 0x08);
    for (offset, value) in bytes.iter().enumerate() {
        bus.write(AT_BG_CANDIDATE + offset as u16, *value);
    }
    bus.write(0xfffc, caller_mapping);
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_pair_interning_aligns_dedupes_and_keeps_kind_chains_apart() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);

    // First pair opens the sprite window: even ordinal 192, physical 256 is
    // the first even sprite-base pattern, second half physically contiguous.
    let q1 = pair_candidate(0x0004); // hash $5E, shared with singles below
    stage_pair(&mut bus, &q1);
    let (first, physical) =
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE);
    assert_eq!((first, physical), (192, 256));
    assert_eq!(
        flags(&bus, 192),
        AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_FIRST
    );
    assert_eq!(
        flags(&bus, 193),
        AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_SECOND
    );
    assert_eq!(payload(&bus, 192), &q1[..32]);
    assert_eq!(payload(&bus, 193), &q1[32..]);
    assert_eq!(head(&bus, 0x5e), 192);
    assert_eq!(link(&bus, 192), 0xffff);
    let allocated = u16::from_le_bytes([
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(allocated, 3);

    // Byte-identical pair dedupes without allocating.
    stage_pair(&mut bus, &q1);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
        (192, 256)
    );

    // A single candidate equal to the pair's TOP half hashes into the same
    // chain but must not dedupe onto the pair node.
    let mut top = [0u8; 32];
    top.copy_from_slice(&q1[..32]);
    stage_candidate(&mut bus, &top);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
        (1, 1)
    );
    assert_eq!(head(&bus, 0x5e), 1, "single chained ahead of the pair");
    assert_eq!(link(&bus, 1), 192);

    // A second identical pair lookup still resolves the pair, walking past
    // the newly chained single with the same first 32 bytes.
    stage_pair(&mut bus, &q1);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
        (192, 256)
    );
    let allocated = u16::from_le_bytes([
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(allocated, 4);
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_pair_window_fills_boundary_maps_reclaims_and_fails_closed() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);

    // Fill the whole sprite window: 93 pairs cover ordinals 192..377, with
    // the physical mapping staying even-aligned across the 375/376 seam.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    for index in 0..93u16 {
        let ordinal = 192 + index * 2;
        stage_pair(&mut bus, &pair_candidate(0x0100 + index));
        assert_eq!(
            atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
            (ordinal, expected_physical(ordinal)),
            "pair {index}"
        );
        assert_eq!(expected_physical(ordinal) & 1, 0, "even alignment {index}");
        assert_eq!(
            expected_physical(ordinal) + 1,
            expected_physical(ordinal + 1),
            "contiguous halves {index}"
        );
    }
    assert_eq!(expected_physical(376), 506);
    // Every pair slot is pending: one more distinct pair fails closed.
    stage_pair(&mut bus, &pair_candidate(0x2000));
    let mut probe = atlas_fixture().0;
    probe.cart_ram.copy_from_slice(&bus.cart_ram);
    probe.write(0xfffc, 0x08);
    stage_pair(&mut probe, &pair_candidate(0x2000));
    assert_eq!(
        atlas_call_fault(
            &mut probe,
            &defs,
            "rt_chr_atlas_intern_pair",
            AT_BG_CANDIDATE
        ),
        1
    );

    // Two retirements expire the displayed generation; a fresh pair then
    // reclaims the first expired pair with an exact chain unlink.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    let fresh = pair_candidate(0x3000);
    stage_pair(&mut bus, &fresh);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
        (192, 256)
    );
    assert_eq!(payload(&bus, 192), &fresh[..32]);
    assert_eq!(payload(&bus, 193), &fresh[32..]);
    // pair_candidate(0x0100) hashed to 1^0^$5A = $5B and was alone there.
    assert_eq!(head(&bus, 0x5b), 0xffff, "stale pair chain emptied");
    let allocated = u16::from_le_bytes([
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(
        allocated, 187,
        "93 pairs + zero, one pair replaced in place"
    );
}

/// Unscripted PAL counter transport for full-packet publisher runs: one
/// physical PAL line per $7E read, repeating 313. Records every out for
/// register-history assertions.
struct AtlasPalBus {
    inner: SmsBus,
    line: usize,
}

impl Bus for AtlasPalBus {
    fn read(&mut self, address: u16) -> u8 {
        self.inner.read(address)
    }
    fn write(&mut self, address: u16, value: u8) {
        self.inner.write(address, value);
    }
    fn in_port(&mut self, port: u8) -> u8 {
        if port == 0x7e {
            let value = match self.line {
                0..=255 => self.line as u8,
                256..=266 => (self.line - 256) as u8,
                267..=312 => (self.line - 267 + 0xd2) as u8,
                _ => unreachable!(),
            };
            self.line = (self.line + 1) % 313;
            value
        } else {
            self.inner.in_port(port)
        }
    }
    fn out_port(&mut self, port: u8, value: u8) {
        self.inner.out_port(port, value);
    }
}

/// Run one full packet presentation; assert no trap and a clean return.
/// With `allow_blank` false, additionally assert the publisher NEVER takes
/// the blank fallback (M3P_STATE 2). Boot and the first packet start from
/// the legitimate display-off blank, so they pass `true`.
fn atlas_packet_invoke(cpu: &mut Cpu, bus: &mut AtlasPalBus, entry: u16, allow_blank: bool) {
    cpu.pc = entry;
    cpu.sp = 0xdff0;
    bus.write(0xdff0, 7);
    bus.write(0xdff1, 0);
    for _ in 0..6_000_000 {
        if cpu.pc == 7 || bus.inner.read(0xcb1d) != 0 {
            break;
        }
        if !allow_blank {
            assert_ne!(bus.inner.ram[0x19f6], 2, "blank fallback PC={:04X}", cpu.pc);
        }
        cpu.step(bus).unwrap();
        assert!(cpu.sp >= NATIVE_STACK_FLOOR);
    }
    assert_eq!(bus.inner.read(0xcb1d), 0, "packet trap PC={:04X}", cpu.pc);
    assert_eq!(cpu.pc, 7, "packet helper did not return");
}

/// Mode4 background palette index honoring the LIVE reg2 table selection in
/// 240-line mode, with zero scroll. Deliberately independent of the packet
/// code's own addressing.
fn atlas_background_index(bus: &SmsBus, x: usize, y: usize) -> u8 {
    assert!(x < 256 && y < 240);
    let base = ((usize::from(bus.vdp_regs[2]) & 0x0c) << 10) | 0x700;
    let entry = base + 2 * ((y / 8) * 32 + x / 8);
    let attributes = bus.vram[entry + 1];
    let tile = usize::from(bus.vram[entry]) + usize::from(attributes & 1) * 256;
    let px = if attributes & 2 == 0 {
        x & 7
    } else {
        7 - (x & 7)
    };
    let py = if attributes & 4 == 0 {
        y & 7
    } else {
        7 - (y & 7)
    };
    let offset = tile * 32 + py * 4;
    let color = (0..4).fold(0, |color, plane| {
        color | (((bus.vram[offset + plane] >> (7 - px)) & 1) << plane)
    });
    color + if attributes & 8 == 0 { 0 } else { 16 }
}

const ATLAS_LITERAL_PLANES: [(u8, u8); 8] = [
    (0x5a, 0x3c),
    (0xa5, 0xc3),
    (0xff, 0x00),
    (0x00, 0xff),
    (0xff, 0xff),
    (0x00, 0x00),
    (0x55, 0x55),
    (0xaa, 0xaa),
];
const ATLAS_LITERAL_PIXELS: [[u8; 8]; 8] = [
    [0, 1, 2, 3, 3, 2, 1, 0],
    [3, 2, 1, 0, 0, 1, 2, 3],
    [1; 8],
    [2; 8],
    [3; 8],
    [0; 8],
    [0, 3, 0, 3, 0, 3, 0, 3],
    [3, 0, 3, 0, 3, 0, 3, 0],
];

/// Freeze one literal packet into the capture windows and seal it validated.
fn atlas_seal_packet(bus: &mut AtlasPalBus, frame: u8) {
    bus.write(0xfffc, 0x08);
    for base in [0x8800u16, 0x9000] {
        for offset in 0..0x800 {
            bus.write(
                base + offset,
                if offset & 0x3ff >= 0x3c0 { 0xe4 } else { 0 },
            );
        }
    }
    for offset in 0..256 {
        bus.write(0x9800 + offset, 0xe0);
    }
    for (offset, value) in [31, 1, 2, 24].into_iter().enumerate() {
        bus.write(0x9800 + offset as u16, value);
    }
    for base in [0x9900u16, 0x9940] {
        for offset in 0..64 {
            bus.write(base + offset, 0);
        }
        for page in 0..8 {
            bus.write(base + page, page as u8);
        }
        bus.write(base + 9, 0x1e);
        for offset in 0..32 {
            bus.write(
                base + 16 + offset,
                [0x0f, 0x30, 0x01, 0x16][offset as usize & 3],
            );
        }
    }
    bus.write(0xfffc, 0x0c);
    bus.write(0xd400, 2); // Explicitly sealed literal fixture, not PPU proof.
    bus.write(0xd402, frame);
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_packets_publish_double_buffers_and_never_blank() {
    let path = PathBuf::from(std::env::var("TRACE_CNROM_ATLAS_PROJECT").unwrap());
    let defs = load_wla_symbol_defs(&path.join("sms.sym"));
    let mut rom = std::fs::read(path.join("sms.sms")).unwrap();
    // Literal test art injected into a test-owned ROM copy at the raw CHR
    // base: tile0 the literal planes, tile1 solid sprite/background color 1.
    let raw = 31 * BANK_SIZE;
    for (row, &(low, high)) in ATLAS_LITERAL_PLANES.iter().enumerate() {
        rom[raw + row] = low;
        rom[raw + row + 8] = high;
    }
    rom[raw + 16..raw + 24].fill(0xff);
    rom[raw + 24..raw + 32].fill(0);
    let mut bus = AtlasPalBus {
        inner: SmsBus::new(rom, 0xff),
        line: 0,
    };
    let mut cpu = Cpu::new();
    atlas_packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_init"].1, true);
    atlas_packet_invoke(&mut cpu, &mut bus, defs["rt_chr_atlas_cold_init"].1, true);
    assert_eq!(
        bus.inner.vdp_regs[2], 0xf3,
        "cold display owns the $0700 table"
    );

    // Packet 1 prepares the $3700 table and flips to it.
    atlas_seal_packet(&mut bus, 0x11);
    atlas_packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1, true);
    assert_eq!(bus.inner.read(0xd400), 0, "packet retired");
    assert_eq!(bus.inner.vdp_regs[2], 0xff, "flip to the $3700 table");
    assert_ne!(bus.inner.vdp_regs[1] & 0x40, 0, "display committed");
    for y in 0..240 {
        for x in 0..256 {
            let color = ATLAS_LITERAL_PIXELS[y & 7][x & 7];
            let quadrant = ((x >> 4) & 1) + 2 * ((y >> 4) & 1);
            let expected = if color == 0 {
                0
            } else {
                color + quadrant as u8 * 4
            };
            assert_eq!(
                atlas_background_index(&bus.inner, x, y),
                expected,
                "packet1 pixel{x},{y}"
            );
        }
    }
    // The visible sprite pair: SAT holds the physical low byte; base $2000
    // shows the solid tile, its 8x8-mode bottom half the canonical blank.
    assert_eq!(bus.inner.vram[0x3f00], 31);
    assert_eq!(bus.inner.vram[0x3f80], 24);
    let sprite = 0x2000 + usize::from(bus.inner.vram[0x3f81]) * 32;
    assert_eq!(bus.inner.vram[0x3f81] & 1, 0, "pair-aligned sprite tile");
    for row in 0..8 {
        assert_eq!(
            &bus.inner.vram[sprite + row * 4..sprite + row * 4 + 4],
            &[0xff, 0, 0, 0xff],
            "sprite row {row}"
        );
    }
    assert!(
        bus.inner.vram[sprite + 32..sprite + 64]
            .iter()
            .all(|&b| b == 0)
    );
    let count_after_first = u16::from_le_bytes([
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);

    // Packet 2 changes exactly one background cell; it lands in the $0700
    // table and the flip returns there.
    atlas_seal_packet(&mut bus, 0x12);
    bus.write(0xfffc, 0x08);
    bus.write(0x8800, 1); // cell (0,0) now the solid color-1 tile
    bus.write(0xfffc, 0x0c);
    atlas_packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1, false);
    assert_eq!(bus.inner.read(0xd400), 0);
    assert_eq!(bus.inner.vdp_regs[2], 0xf3, "flip back to the $0700 table");
    for y in 0..240 {
        for x in 0..256 {
            let expected = if x < 8 && y < 8 {
                1
            } else {
                let color = ATLAS_LITERAL_PIXELS[y & 7][x & 7];
                let quadrant = ((x >> 4) & 1) + 2 * ((y >> 4) & 1);
                if color == 0 {
                    0
                } else {
                    color + quadrant as u8 * 4
                }
            };
            assert_eq!(
                atlas_background_index(&bus.inner, x, y),
                expected,
                "packet2 pixel{x},{y}"
            );
        }
    }
    let count_after_second = u16::from_le_bytes([
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);

    // Packet 3 repeats packet 2 byte-for-byte: pure canonical reuse, no new
    // allocations, flip forward again with identical pixels.
    atlas_seal_packet(&mut bus, 0x13);
    bus.write(0xfffc, 0x08);
    bus.write(0x8800, 1);
    bus.write(0xfffc, 0x0c);
    atlas_packet_invoke(&mut cpu, &mut bus, defs["rt_cnrom_packet_present"].1, false);
    assert_eq!(bus.inner.read(0xd400), 0);
    assert_eq!(bus.inner.vdp_regs[2], 0xff, "flip forward again");
    let count_after_third = u16::from_le_bytes([
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT)],
        bus.inner.cart_ram[sram1(AT_ALLOCATED_COUNT) + 1],
    ]);
    assert_eq!(
        count_after_second, count_after_third,
        "identical packet allocates nothing new"
    );
    assert!(count_after_second >= count_after_first);
    for y in 0..240 {
        for x in 0..256 {
            let expected = if x < 8 && y < 8 {
                1
            } else {
                let color = ATLAS_LITERAL_PIXELS[y & 7][x & 7];
                let quadrant = ((x >> 4) & 1) + 2 * ((y >> 4) & 1);
                if color == 0 {
                    0
                } else {
                    color + quadrant as u8 * 4
                }
            };
            assert_eq!(
                atlas_background_index(&bus.inner, x, y),
                expected,
                "packet3 pixel{x},{y}"
            );
        }
    }
    // No canonical slot stays dirty after publication.
    for ordinal in 0..AT_CAPACITY {
        let flags = bus.inner.cart_ram[sram1(AT_FLAGS) + usize::from(ordinal)];
        assert_eq!(flags & AT_DIRTY, 0, "dirty ordinal {ordinal}");
    }
}

#[test]
#[ignore = "requires TRACE_CNROM_ATLAS_PROJECT assembled atlas-capability fixture"]
fn assembled_atlas_pair_reclaims_expired_singles_inside_the_sprite_window() {
    let (mut bus, defs) = atlas_fixture();
    cold_boot(&mut bus, &defs);

    // Fill singles through ordinal 193 so the sprite window's first pair
    // slots hold plain background patterns.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    for i in 1..=193u16 {
        stage_candidate(&mut bus, &candidate(i));
        assert_eq!(
            atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_bg", AT_BG_CANDIDATE),
            (i, expected_physical(i))
        );
    }
    // Expire them across two retirements.
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_retire", 0);
    atlas_call(&mut bus, &defs, "rt_chr_atlas_begin", 0);

    let h192 = 192u8 ^ 0xa5; // candidate(192) XOR hash
    let h193 = 193u8 ^ 0xa5;
    assert_eq!(head(&bus, h192), 192);
    assert_eq!(head(&bus, h193), 193);
    let q = pair_candidate(0x0777);
    stage_pair(&mut bus, &q);
    assert_eq!(
        atlas_call(&mut bus, &defs, "rt_chr_atlas_intern_pair", AT_BG_CANDIDATE),
        (192, 256)
    );
    assert_eq!(head(&bus, h192), 0xffff, "expired single unlinked");
    assert_eq!(head(&bus, h193), 0xffff, "expired single unlinked");
    assert_eq!(
        flags(&bus, 192),
        AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_FIRST
    );
    assert_eq!(
        flags(&bus, 193),
        AT_ALLOCATED | AT_PENDING | AT_DIRTY | AT_PAIR_SECOND
    );
    // Expired singles below the window are reclaimed only by need, not swept.
    assert_eq!(payload(&bus, 191), &candidate(191)[..]);
}
