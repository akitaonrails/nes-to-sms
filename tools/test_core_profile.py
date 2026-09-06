"""Contract tests only: these do not replace real-core timing and parity gates."""

from copy import deepcopy
import ctypes as C
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import core_route
from core_profile import ProfileError, WindowConfig, run_profile, validate_report, window_config
from apply_gpgx_profile import patch_source


def report():
    # Two closed intervals, ending inside an interrupt. Costs deliberately use
    # different nominal/scaled ratios to expose accidental multiplication.
    return {
        "schema": 1,
        "window": {"start_tick": 60, "end_tick": 62, "observed_tick": 62, "state": "complete"},
        "errors": [],
        "totals": {"instructions": 3, "nominal_master": 465, "scaled_master": 91},
        "costs": [
            {"source": "rom", "rom_offset": 0x10000, "pc": 0x8000, "irq_kind": "none",
             "irq_depth": 0, "kind": "instruction", "instructions": 1, "nominal_master": 60, "scaled_master": 11},
            {"source": "rom", "rom_offset": 0x38, "pc": 0x38, "irq_kind": "irq",
             "irq_depth": 1, "kind": "instruction", "instructions": 2, "nominal_master": 405, "scaled_master": 80}],
        "ticks": [
            {"tick": 60, "instructions": 1, "nominal_master": 60, "scaled_master": 11},
            {"tick": 61, "instructions": 2, "nominal_master": 405, "scaled_master": 80}],
    }


class ReportTests(unittest.TestCase):
    def test_complete_window_with_distinct_scaled_costs(self):
        result = validate_report(report(), 60, 62)
        self.assertEqual(result["complete_intervals"], 2)
        self.assertEqual(result["nominal_tstates"], 31)
        self.assertEqual(result["scaled_master"], 91)

    def test_incomplete_or_wrong_window_is_not_measurement(self):
        for key, value in [("state", "collecting"), ("observed_tick", 61), ("start_tick", 59)]:
            data = report()
            data["window"][key] = value
            with self.subTest(key=key), self.assertRaisesRegex(ProfileError, "window"):
                validate_report(data, 60, 62)

    def test_missing_or_duplicated_interval_is_rejected(self):
        for ticks in [report()["ticks"][:1], [report()["ticks"][0]] * 2]:
            data = report()
            data["ticks"] = ticks
            with self.assertRaisesRegex(ProfileError, "intervals"):
                validate_report(data, 60, 62)

    def test_each_accounting_view_must_conserve(self):
        for view in ("costs", "ticks"):
            data = report()
            data[view][0]["scaled_master"] += 1
            with self.subTest(view=view), self.assertRaisesRegex(ProfileError, "conserve"):
                validate_report(data, 60, 62)

    def test_invalid_counter_and_partial_tstate_rejected(self):
        for value in (-1, True, 2.5, 61):
            data = report()
            data["costs"][0]["nominal_master"] = value
            with self.subTest(value=value), self.assertRaises(ProfileError):
                validate_report(data, 60, 62)

    def test_bank_identity_is_not_just_pc(self):
        data = report()
        extra = deepcopy(data["costs"][0])
        extra["rom_offset"] += 0x4000
        data["costs"].append(extra)
        for key in ("instructions", "nominal_master", "scaled_master"):
            data["totals"][key] += extra[key]
            data["ticks"][0][key] += extra[key]
        validate_report(data, 60, 62)
        data["costs"][-1]["rom_offset"] = data["costs"][0]["rom_offset"]
        with self.assertRaisesRegex(ProfileError, "duplicate"):
            validate_report(data, 60, 62)

    def test_unknown_execution_and_external_costs_stay_visible(self):
        data = report()
        data["costs"][0].update(source="unknown", rom_offset=None, kind="external")
        self.assertEqual(validate_report(data, 60, 62)["unknown_or_external"],
                         {"instructions": 1, "nominal_master": 60, "scaled_master": 11})

    def test_bad_interrupt_identity_invalidates_report(self):
        data = report()
        data["errors"] = ["RETI stack identity mismatch"]
        with self.assertRaisesRegex(ProfileError, "accounting errors"):
            validate_report(data, 60, 62)
        data = report()
        data["costs"][1]["irq_depth"] = 0
        with self.assertRaisesRegex(ProfileError, "interrupt context"):
            validate_report(data, 60, 62)

    def test_ram_cannot_claim_rom_provenance(self):
        data = report()
        data["costs"][0]["source"] = "ram"
        with self.assertRaisesRegex(ProfileError, "non-ROM"):
            validate_report(data, 60, 62)


class WrapperTests(unittest.TestCase):
    def test_existing_output_is_rejected_without_touching_prior_evidence(self):
        original_core, original_route = core_route.Core, core_route.Route
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "accepted"
            output.mkdir()
            provenance = output / "profile-wrapper.json"
            provenance.write_bytes(b"prior provenance\n")
            (output / "summary.json").write_bytes(b"prior route summary\n")
            before = {path.name: path.read_bytes() for path in output.iterdir()}
            inode = output.stat().st_ino
            args = SimpleNamespace(output=output, start_tick=60, end_tick=420)
            with patch("core_route.run", side_effect=core_route.RouteError("existing output")) as runner:
                with self.assertRaisesRegex(ProfileError, "output already exists"):
                    run_profile(args)
                runner.assert_not_called()
            self.assertEqual(output.stat().st_ino, inode)
            self.assertEqual({path.name: path.read_bytes() for path in output.iterdir()}, before)
        self.assertIs(core_route.Core, original_core)
        self.assertIs(core_route.Route, original_route)

    def test_new_failed_output_keeps_failure_provenance(self):
        original_core, original_route = core_route.Core, core_route.Route
        with tempfile.TemporaryDirectory() as folder:
            args = SimpleNamespace(output=Path(folder) / "new", start_tick=60, end_tick=420)
            def fail_after_creation(run_args):
                run_args.output.mkdir()
                raise core_route.RouteError("fixture failure")
            with patch("core_route.run", side_effect=fail_after_creation):
                with self.assertRaisesRegex(core_route.RouteError, "fixture failure"):
                    run_profile(args)
            evidence = json.loads((args.output / "profile-wrapper.json").read_text())
            self.assertFalse(evidence["armed"])
            self.assertEqual((evidence["start_tick"], evidence["end_tick"]), (60, 420))
        self.assertIs(core_route.Core, original_core)
        self.assertIs(core_route.Route, original_route)

    def test_patcher_rejects_missing_or_ambiguous_source_sites(self):
        for source in ("unrelated source", "#ifdef Z80_OVERCLOCK_SHIFT\n#define USE_CYCLES(A)" * 2):
            with self.assertRaisesRegex(ValueError, "expected one pinned patch site"):
                patch_source(source)

    def test_external_profile_defines_addresses_and_initial_wrap_anchor(self):
        profile = {"ram": {"state": 12, "substate": 13, "game_tick": 20},
                   "boot": {"stage_state": [9, 10]}}
        config = window_config(profile, 250, 60, 420)
        self.assertEqual(C.sizeof(WindowConfig), 88)
        self.assertEqual((config.tick_offset, config.initial_tick), (20, 250))
        self.assertEqual(list(config.offsets)[:2], [12, 13])
        self.assertEqual(list(config.values)[:2], [9, 10])
        profile["ram"]["game_tick"] = 8192
        with self.assertRaisesRegex(ProfileError, "SYSTEM_RAM"):
            window_config(profile, 250, 60, 420)

    def test_wrapper_restores_frozen_classes_after_failure(self):
        original_core, original_route = core_route.Core, core_route.Route
        with tempfile.TemporaryDirectory() as folder:
            args = SimpleNamespace(output=Path(folder) / "new", start_tick=60, end_tick=420)
            with patch("core_route.run", side_effect=core_route.RouteError("fixture failure")):
                with self.assertRaisesRegex(core_route.RouteError, "fixture failure"):
                    run_profile(args)
        self.assertIs(core_route.Core, original_core)
        self.assertIs(core_route.Route, original_route)


if __name__ == "__main__":
    unittest.main()
