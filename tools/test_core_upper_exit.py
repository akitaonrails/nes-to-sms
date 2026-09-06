"""Host contract tests, not substitutes for original-ROM/actual-core evidence."""

import argparse
import contextlib
import copy
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import core_upper_exit as upper
from core_route import BUTTONS, RouteError


PROFILE = Path(__file__).resolve().parents[1] / "profiles/cv1/acceptance/core-upper-exit.toml"


class Prefix:
    """Only isolates the NEW phase cursor; frozen Traversal has its own tests."""
    def __init__(self, complete=True):
        self.complete = complete
        self.calls = 0

    def buttons(self):
        return {BUTTONS["start"]}

    def update(self, frame, state):
        self.calls += 1
        return self.complete

    def metrics(self):
        return {"fixture_calls": self.calls}


class UpperExitTests(unittest.TestCase):
    def setUp(self):
        self.loaded = upper.load_profile(PROFILE)
        self.config = copy.deepcopy(self.loaded[0])
        for phase in self.config["phases"]:
            phase["minimum_ticks"] = 1
            phase["maximum_ticks"] = 100
        self.config["phases"][1]["minimum_ticks"] = 2
        self.config["phases"][-1].update(minimum_ticks=2, minimum_advance=4)
        self.route = upper.UpperExit(self.config, Prefix())
        self.frame = -1

    def state(self, tick=0, world=746, **changes):
        base, observations = self.loaded[2:4]
        result = dict.fromkeys([*observations, *base["ram_words"]], 0)
        screen = min(world, 128)
        result.update(state=5, substate=6, area=1, actor_mode=0, health=32,
                      game_tick=tick, player_world_x=world, player_x=screen,
                      camera_x=world-screen, player_y=96)
        result.update(changes)
        return result

    def feed(self, **values):
        self.frame += 1
        return self.route.update(self.frame, self.state(**values))

    def to_next_room(self):
        self.assertFalse(self.feed())
        for values in [dict(tick=1, world=600),
                       dict(tick=2, world=610, actor_mode=4, player_y=180),
                       dict(tick=3, world=864), dict(tick=4, world=1286),
                       dict(tick=5, world=1296, actor_mode=4, player_y=176),
                       dict(tick=6, world=1391),
                       dict(tick=7, world=1516, state=8, substate=0),
                       dict(tick=8, world=1516, state=8, substate=1),
                       dict(tick=1, world=1516, state=8, substate=2),
                       dict(tick=2, world=1516, state=8, substate=3, area=2),
                       dict(tick=9, world=48, area=2)]:
            self.assertFalse(self.feed(**values))
        self.assertEqual(self.route.phase, "next-room-play")

    def test_frozen_prefix_is_delegated_and_never_skipped(self):
        self.route = upper.UpperExit(self.config, Prefix(False))
        self.assertEqual(self.route.buttons(), {BUTTONS["start"]})
        for tick in range(20):
            self.assertFalse(self.feed(tick=tick, state=1, substate=1, area=0))
        self.assertEqual(self.route.phase, "frozen-prefix")
        self.assertEqual(self.route.prefix.calls, 20)

    def test_exact_order_inputs_and_final_playable_soak(self):
        self.feed()
        self.assertEqual(self.route.buttons(), {BUTTONS["left"], BUTTONS["a"]})
        self.feed(tick=1, world=600)
        self.assertEqual(self.route.buttons(), {BUTTONS["up"], BUTTONS["right"]})
        self.setUp()
        self.to_next_room()
        self.assertEqual([event["phase"] for event in self.route.events],
                         [phase["name"] for phase in self.config["phases"]])
        self.assertFalse(self.feed(tick=10, world=49, area=2))
        self.assertFalse(self.feed(tick=11, world=52, area=2))
        self.assertFalse(self.feed(tick=12, world=53, area=2))
        self.assertTrue(self.feed(tick=13, world=54, area=2))
        self.assertEqual(self.route.phase, "complete")
        self.assertEqual(self.route.buttons(), set())

    def test_second_stair_requires_observed_ascent_not_static_upper_coordinates(self):
        self.feed()
        self.route.index = 3
        self.route.phase_tick = 0
        for tick in range(1, 5):
            self.assertFalse(self.feed(tick=tick, world=1391))
        self.assertEqual(self.route.phase, "second-climb")
        self.feed(tick=5, world=1300, actor_mode=4, player_y=150)
        self.feed(tick=6, world=1391)
        self.assertEqual(self.route.phase, "upper-exit")

    def test_first_stair_switches_to_defended_walk_at_observed_top(self):
        self.feed()
        self.feed(tick=1, world=600)
        self.assertEqual(self.route.phase, "first-climb")
        self.assertFalse(self.feed(tick=2, world=751))
        self.assertEqual(self.route.phase, "first-climb", "upper coordinates alone are not ascent")
        self.feed(tick=3, world=700, actor_mode=4, player_y=150)
        self.feed(tick=4, world=751)
        self.assertEqual(self.route.phase, "approach")
        self.assertEqual(self.route.buttons(), {BUTTONS["right"], BUTTONS["a"]})

    def test_transition_area_and_title_cannot_pass(self):
        self.to_next_room()
        for changes in [dict(state=1, substate=1, area=0), dict(state=8, area=2),
                        dict(state=5, substate=0, area=2), dict(area=1)]:
            with self.subTest(changes=changes), self.assertRaises(RouteError):
                self.feed(tick=10, world=90, **changes)

    def test_death_health_and_trap_fail_at_any_postprefix_phase(self):
        for changes in [dict(state=6), dict(actor_mode=8), dict(health=0),
                        dict(health=255), dict(trap=0xe2)]:
            self.setUp()
            self.feed()
            with self.subTest(changes=changes), self.assertRaises(RouteError):
                self.feed(tick=1, **changes)

    def test_tick_wrap_is_elapsed_progress_but_reset_and_stall_fail(self):
        self.feed(tick=255)
        self.feed(tick=0)
        self.assertEqual(self.route.ticks, 1)
        with self.assertRaisesRegex(RouteError, "discontinuity"):
            self.feed(tick=200)
        self.setUp()
        self.config["limits"]["stall_frames"] = 2
        self.feed()
        self.feed()
        self.feed()
        with self.assertRaisesRegex(RouteError, "stall"):
            self.feed()

    def test_duplicates_and_torn_positions_do_not_confirm_final_success(self):
        self.to_next_room()
        self.feed(tick=10, world=49, area=2)
        for _ in range(6):
            self.assertFalse(self.feed(tick=11, world=52, area=2))
        self.assertEqual(self.route.confirmed, 1)
        self.assertFalse(self.feed(tick=12, world=53, area=2, camera_x=999))
        self.assertEqual(self.route.confirmed, 0)
        self.assertFalse(self.feed(tick=13, world=54, area=2))

    def test_phase_and_total_tick_limits_fail_closed(self):
        for total in [False, True]:
            self.setUp()
            self.feed()
            if total:
                self.config["limits"]["extra_ticks"] = 1
            else:
                self.config["phases"][0]["maximum_ticks"] = 1
            with self.assertRaisesRegex(RouteError, "limit"):
                self.feed(tick=2)

    def test_reset_is_single_source_scoped_and_not_elapsed_time(self):
        self.feed(tick=30)
        self.route.index = 5
        self.route.previous_state = self.state(tick=30, state=8, substate=1)
        self.feed(tick=1, state=8, substate=2)
        self.assertEqual(self.route.ticks, 0)
        self.assertEqual(len(self.route.resets), 1)
        self.feed(tick=2, state=8, substate=1)
        with self.assertRaisesRegex(RouteError, "duplicate"):
            self.feed(tick=1, state=8, substate=2)
        for phase, current in [(0, dict()), (5, dict(state=8, substate=3)),
                               (5, dict(state=8, substate=2, area=2))]:
            self.setUp()
            self.feed(tick=30)
            self.route.index = phase
            self.route.previous_state = self.state(tick=30, state=8, substate=1)
            with self.assertRaisesRegex(RouteError, "discontinuity"):
                self.feed(tick=1, **current)

    def test_missing_reset_cannot_complete_transition(self):
        self.feed(tick=30)
        self.route.index = 5
        with self.assertRaisesRegex(RouteError, "missing.*reset"):
            self.feed(tick=31, world=48, area=2)

    def test_natural_increment_through_one_is_not_a_transition_reset(self):
        self.feed(tick=255)
        self.route.index = 5
        self.route.previous_state = self.state(tick=255, state=8, substate=1)
        self.feed(tick=0, state=8, substate=1)
        self.feed(tick=1, state=8, substate=1)
        self.assertEqual(self.route.ticks, 2)
        self.assertFalse(self.route.resets)
        self.route.previous = 31
        self.route.previous_state = self.state(tick=31, state=8, substate=1)
        self.feed(tick=1, state=8, substate=2)
        self.assertEqual(self.route.ticks, 2)
        self.assertEqual(len(self.route.resets), 1)

    def test_profile_pins_frozen_prefix_and_requires_final_movement(self):
        original = PROFILE.read_text()
        # Place beside the real profile so its relative frozen prefix resolves.
        with patch.object(Path, "read_text", autospec=True) as read:
            read.side_effect = lambda path: original.replace("minimum_advance = 40", "minimum_advance = 0") if path == PROFILE else self._read(path)
            with self.assertRaisesRegex(RouteError, "ticks and movement"):
                upper.load_profile(PROFILE)
        with patch.object(upper, "sha256", return_value="wrong"):
            with self.assertRaisesRegex(RouteError, "hash mismatch"):
                upper.load_profile(PROFILE)

    _read = staticmethod(Path.read_text)


class RunnerTests(unittest.TestCase):
    def test_existing_directory_file_and_symlink_are_never_touched(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            directory = root / "prior"
            directory.mkdir()
            sentinel = directory / "summary.json"
            sentinel.write_text("prior provenance")
            file = root / "file"
            file.write_text("old")
            link = root / "link"
            link.symlink_to(root / "absent")
            for output in [directory, file, link]:
                args = argparse.Namespace(output=output, profile=PROFILE, core=file, rom=file)
                with patch.object(upper, "Core") as core, self.assertRaisesRegex(RouteError, "already exists"):
                    upper.run(args)
                core.assert_not_called()
            self.assertEqual(sentinel.read_text(), "prior provenance")
            self.assertEqual(file.read_text(), "old")
            self.assertTrue(link.is_symlink())

    def exercise_failure(self, failure):
        config, traversal, base, observations, prefix_path, base_path = upper.load_profile(PROFILE)
        config["limits"]["physical_frames"] = 1
        closed = []

        class FakeCore:
            def __init__(self, *args):
                if failure == "constructor":
                    raise RouteError("constructor failure")
                self.ram = bytearray(traversal["ram_bytes"])
                self.pixels = None
                self.video_calls = 0
                self.error = None
                self.queried, self.advertised = {}, {}
                self.library = SimpleNamespace(retro_run=self.advance)

            def start(self, *args):
                if failure == "start":
                    raise RouteError("start failure")

            def advance(self):
                if failure != "callback":
                    self.video_calls += 1
                    self.pixels = (b"\x1f\0", 1, 1, 2, 2)
                if failure == "trap":
                    self.ram[base["ram"]["trap"]] = 0xe2

            def check_options(self, *args):
                if failure == "options":
                    raise RouteError("options failure")

            def close(self):
                closed.append(True)

        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            artifact = root / "input"
            artifact.write_bytes(b"fixture")
            args = argparse.Namespace(core=artifact, rom=artifact, profile=PROFILE, output=root / "new")
            with patch.object(upper, "load_profile", return_value=(config, traversal, base, observations, prefix_path, base_path)), patch.object(upper, "Core", FakeCore), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaises(RouteError):
                    upper.run(args)
            summary = json.loads((args.output / "summary.json").read_text())
            self.assertEqual(summary["status"], "failed")
            self.assertTrue(summary["error"])
            self.assertIn("rom_sha256", summary)
            if failure != "constructor":
                self.assertEqual(closed, [True])
            if failure == "trap":
                self.assertEqual(summary["final_state"]["trap"], 0xe2)
                self.assertIn("226", (args.output / "frames.csv").read_text())

    def test_failures_retain_provenance_and_close_core(self):
        for failure in ["constructor", "start", "callback", "trap", "options", "limit"]:
            with self.subTest(failure=failure):
                self.exercise_failure(failure)


if __name__ == "__main__":
    unittest.main()
