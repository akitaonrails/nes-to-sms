"""Opt-in real GPGX tests using original, tiny SMS opcode fixtures (no toy CPU).

Run inside Docker with GPGX_PROFILE_CORE and GPGX_STOCK_CORE pointing at the
instrumented and unmodified twins. Outputs and synthetic ROMs are temporary.
"""
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from core_profile import NativeProfiler, WindowConfig, validate_report
from core_route import Core


OPTIONS = {"genesis_plus_gx_region_detect": "ntsc-u",
           "genesis_plus_gx_frameskip": "disabled", "genesis_plus_gx_overclock": "500"}
PROFILE_CORE = os.environ.get("GPGX_PROFILE_CORE")
STOCK_CORE = os.environ.get("GPGX_STOCK_CORE")


def cartridge(body, *, mapper=False, interrupts=False, reti=False, initial_tick=0, arm_inside=False):
    rom = bytearray(65536)
    rom[0:3] = bytes.fromhex("c3 00 02")  # JP $0200
    # NMI sets a flag without disturbing the original flags/registers.
    nmi = bytes.fromhex("f5 3e 01 32 03 c0 f1 ed 45")
    rom[0x66:0x66+len(nmi)] = nmi
    if interrupts:
        rom[0x38:0x3b] = bytes.fromhex("c3 00 01")
        handler = bytes.fromhex(
            "f5 e5 db bf 21 02 c0 34 7e fe 01 20 10 "
            "fb 76 3a 03 c0 b7 28 f9 21 00 c0 34 f3 76 18 fd "
            "e1 f1 fb c9")
        if reti:
            handler = handler[:-1] + bytes.fromhex("ed 4d")
        if arm_inside:
            handler = handler.replace(bytes.fromhex("20 10"), bytes.fromhex("20 12"))
            handler = handler.replace(bytes.fromhex("21 00 c0 34 f3"), bytes.fromhex("21 00 c0 34 00 34 f3"))
        rom[0x100:0x100+len(handler)] = handler
    init = bytes.fromhex(
        "f3 ed 56 31 f0 df af 32 00 c0 32 02 c0 32 03 c0 "
        "3e 42 32 00 c1 3e 23 32 01 c1 3e 99 32 02 c1")
    if initial_tick:
        init = init.replace(bytes.fromhex("af 32 00 c0"), bytes([0x3e, initial_tick, 0x32, 0x00, 0xc0, 0xaf]))
    if interrupts:
        init += bytes.fromhex("3e 20 d3 bf 3e 81 d3 bf")  # VDP R1 IRQ enabled
    # Wait for actual controller Up input. No host RAM or CPU injection.
    init += bytes.fromhex("db dc e6 01 20 fa 21 00 c0 34")
    rom[0x200:0x200+len(init)] = init
    start = 0x200+len(init)
    rom[start:start+len(body)] = body
    if mapper:
        rom[0x8000:0x8005] = bytes.fromhex("3e 03 32 ff ff")
        rom[0xc005:0xc00e] = bytes.fromhex("21 00 c0 34 f3 76 18 fd 00")
    rom[0x7ff0:0x8000] = b"TMR SEGA" + bytes(7) + b"\x4e"
    return bytes(rom), start


def run_fixture(core_path, rom_bytes, profile, *, clock="500", pause=False, initial_tick=0, arm_inside=False):
    with tempfile.TemporaryDirectory() as folder:
        output = Path(folder)
        rom = output / "profile-fixture.sms"
        rom.write_bytes(rom_bytes)
        options = dict(OPTIONS, genesis_plus_gx_overclock=clock)
        core = Core(Path(core_path), rom, output, options)
        captures = []
        profiler = None
        try:
            core.start(options, 8192)
            # Allow options' boot overclock delay to expire while waiting on input.
            for _ in range(100):
                core.library.retro_run()
            if profile:
                profiler = NativeProfiler(core)
                if not arm_inside:
                    profiler.arm(WindowConfig(1, 0, initial_tick, 1, 2, 0))
            core.buttons = {4}  # RetroPad Up -> SMS controller bit 0
            complete = False
            for frame in range(12):
                core.library.retro_run()
                raw, width, height, pitch, fmt = core.pixels
                captures.append((hashlib.sha256(raw).hexdigest(), bytes(core.ram),
                                 width, height, pitch, fmt))
                if arm_inside and frame == 0 and profiler:
                    if core.ram[0] != 1 or core.ram[2] < 1:
                        raise AssertionError("host arming did not stop inside the outer IRQ")
                    profiler.arm(WindowConfig(1, 0, 1, 1, 2, 0))
                if pause and core.ram[2] >= 1:
                    core.buttons = {4, 3}
                if core.ram[0] == (initial_tick + (3 if arm_inside else 2)) & 255:
                    complete = True
                    break
            if not complete:
                raise AssertionError(f"fixture stalled; RAM prefix={bytes(core.ram[:8]).hex()}")
            report = None
            if profiler:
                path = output / "profile.json"
                profiler.dump(path)
                report = json.loads(path.read_text())
                validate_report(report, 1, 2)
            # CPU registers, flags, stack, mapper, VDP and RAM must also agree.
            size = core.fn("retro_serialize_size", C.c_size_t, [])()
            state = C.create_string_buffer(size)
            if not core.fn("retro_serialize", C.c_bool, [C.c_void_p, C.c_size_t])(state, size):
                raise AssertionError("core refused state serialization")
            # GPGX serializes Z80.irq_callback as a host function pointer, then
            # reconstructs it on load (core/state.c). Resolve that exact symbol
            # from this unstripped twin; exclude ONLY its one serialized pointer.
            symbols = {}
            for line in subprocess.check_output(["nm", "-a", str(core_path)], text=True).splitlines():
                parts = line.split()
                if len(parts) == 3 and parts[2] in ("retro_init", "z80_irq_callback"):
                    symbols[parts[2]] = int(parts[0], 16)
            address = C.cast(core.library.retro_init, C.c_void_p).value
            callback = address + symbols["z80_irq_callback"] - symbols["retro_init"]
            pointer = callback.to_bytes(C.sizeof(C.c_void_p), sys.byteorder)
            if state.raw.count(pointer) != 1:
                raise AssertionError("cannot uniquely identify serialized Z80.irq_callback")
            return report, captures, state.raw.replace(pointer, bytes(len(pointer)))
        finally:
            core.close()


@unittest.skipUnless(PROFILE_CORE and STOCK_CORE, "set both GPGX core paths; run inside Docker")
class RealCoreTests(unittest.TestCase):
    def twins(self, rom, **kwargs):
        measured = run_fixture(PROFILE_CORE, rom, True, **kwargs)
        stock = run_fixture(STOCK_CORE, rom, False, **kwargs)
        self.assertEqual(measured[1], stock[1], "every callback and full SYSTEM_RAM must match")
        self.assertEqual(measured[2], stock[2], "serialized CPU/mapper/video state must match")
        return measured[0]

    def test_taken_not_taken_branches_and_repeating_block_costs(self):
        # LDIR executes 21+21+16T. JR NZ is untaken (7T), JR Z taken (12T).
        body = bytes.fromhex("01 03 00 21 00 c1 11 10 c1 ed b0 3e 00 b7 20 00 28 00 21 00 c0 34 f3 76 18 fd")
        rom, start = cartridge(body)
        # 139T total; at500 each of15 USE_CYCLES charges rounds down once:
        # 139*3 - 15 =402 scaled master cycles (not139*3).
        for clock, scaled in [("100", 139*15), ("500", 402)]:
            with self.subTest(clock=clock):
                data = self.twins(rom, clock=clock)
                self.assertEqual(data["totals"]["instructions"], 12)
                self.assertEqual(data["totals"]["nominal_master"], 139*15)
                self.assertEqual(data["totals"]["scaled_master"], scaled)
                rows = [row for row in data["costs"] if row["pc"] == start+9]
                self.assertEqual(len(rows), 1)
                self.assertEqual(rows[0]["instructions"], 3)
                self.assertEqual(rows[0]["nominal_master"], 58*15)

    def test_mapper_write_belongs_to_outgoing_bank(self):
        rom, _ = cartridge(bytes.fromhex("c3 00 80"), mapper=True)
        data = self.twins(rom)
        self.assertEqual(data["totals"]["nominal_master"], 51*15)
        writer = next(row for row in data["costs"] if row["pc"] == 0x8002)
        successor = next(row for row in data["costs"] if row["pc"] == 0x8005)
        self.assertEqual(writer["rom_offset"], 0x8002)
        self.assertEqual(successor["rom_offset"], 0xc005)

    def test_halt_nested_interrupt_pause_nmi_and_window_close_inside_irq(self):
        for reti in (False, True):
            with self.subTest(reti=reti):
                rom, _ = cartridge(bytes.fromhex("fb 76 18 fd"), interrupts=True, reti=reti)
                data = self.twins(rom, pause=True)
                irq = data["interrupts"]
                self.assertGreaterEqual(irq["max_depth"], 2)
                self.assertGreaterEqual(irq["irq_entries"], 2)
                self.assertEqual(irq["nmi_entries"], 1)
                self.assertGreater(irq["halt_repeats"], 0)
                self.assertEqual(irq["open"], [])
                self.assertEqual(len(irq["close"]), 1)
                self.assertEqual(irq["close"][0]["kind"], "irq")
                self.assertEqual(data["errors"], [])
                for kind, count, tstates in [("irq_entry", irq["irq_entries"], 13),
                                               ("nmi_entry", irq["nmi_entries"], 11),
                                               ("halt", irq["halt_repeats"], 4)]:
                    nominal = sum(row["nominal_master"] for row in data["costs"] if row["kind"] == kind)
                    self.assertEqual(nominal, count*tstates*15)

    def test_arming_and_both_window_boundaries_inside_irq_preserve_context(self):
        rom, _ = cartridge(bytes.fromhex("fb 76 18 fd"), interrupts=True, arm_inside=True)
        data = self.twins(rom, pause=True, arm_inside=True)
        self.assertEqual(data["totals"]["instructions"], 2)
        self.assertEqual(data["totals"]["nominal_master"], 15*15)
        self.assertEqual(len(data["interrupts"]["open"]), 1)
        self.assertEqual(data["interrupts"]["open"], data["interrupts"]["close"])
        self.assertTrue(all(row["irq_kind"] == "irq" and row["irq_depth"] == 1 for row in data["costs"]))

    def test_counter_wrap_keeps_complete_instruction_window(self):
        rom, _ = cartridge(bytes.fromhex("00 21 00 c0 34 f3 76 18 fd"), initial_tick=254)
        data = self.twins(rom, initial_tick=254)
        self.assertEqual(data["totals"]["instructions"], 3)
        self.assertEqual(data["totals"]["nominal_master"], 25*15)


if __name__ == "__main__":
    unittest.main()
