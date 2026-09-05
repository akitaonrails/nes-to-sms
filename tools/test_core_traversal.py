"""Runner-contract tests only; actual game behavior is tested with libretro."""

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

from core_route import BUTTONS, RouteError
import core_traversal as traversal


PROFILE = Path(__file__).resolve().parents[1] / "profiles/cv1/acceptance/core-traversal.toml"


class TraversalTests(unittest.TestCase):
    def setUp(self):
        self.config, self.base, _ = traversal.load_profile(PROFILE)
        for completion in self.config["completions"].values():
            completion["minimum_ticks"] = 4
            completion["minimum_advance"] = 4
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        self.frame = -1

    def sample(self, tick=30, major=5, sub=6, area=0, world=48, count=0, **updates):
        state = dict.fromkeys([*self.base["ram"], *self.base["ram_words"]], 0)
        screen = min(world, 128)
        state.update(state=major, substate=sub, area=area, game_tick=tick, hearts=5,
                     player_world_x=world, player_x=screen, camera_x=world-screen,
                     escape_successes=count, **updates)
        return state

    def feed(self, **kwargs):
        self.frame += 1
        return self.route.update(self.frame, self.sample(**kwargs))

    def room(self):
        for values in [dict(tick=30), dict(tick=31, world=220),
                       dict(tick=32, major=10, sub=0, world=679),
                       dict(tick=33, major=10, sub=1, world=640),
                       dict(tick=34, major=10, sub=2, world=640),
                       dict(tick=35, major=10, sub=2, area=1, world=640),
                       dict(tick=36, sub=3, area=1), dict(tick=1, sub=2, area=1),
                       dict(tick=2, sub=0, area=1), dict(tick=3, area=1),
                       dict(tick=4, area=1), dict(tick=5, area=1)]:
            self.assertFalse(self.feed(**values))

    def test_ordered_phase_reset_and_normal_confirmed_progress(self):
        self.room()
        self.assertEqual([event["phase"] for event in self.route.events if event["kind"] == "phase"],
                         ["street", "door0", "door1", "door2", "room_setup", "room"])
        self.assertEqual(len(self.route.resets), 1)
        self.assertEqual((self.route.resets[0]["previous"], self.route.resets[0]["current"]), (36, 1))
        self.assertEqual(self.route.ticks, 10)  # Reset contributes no elapsed game ticks.
        for tick, world in [(6, 49), (7, 50), (8, 52), (9, 53)]:
            self.assertFalse(self.feed(tick=tick, area=1, world=world))
        self.assertTrue(self.feed(tick=10, area=1, world=54))
        metrics = self.route.metrics()
        self.assertFalse(metrics["escape_exercised"])
        self.assertEqual(metrics["diagnostic_coverage"], "not_observed")
        self.assertEqual(len(metrics["confirmed_endpoint"]), 3)
        self.assertEqual([row["state"]["player_world_x"] for row in metrics["confirmed_endpoint"]], [52, 53, 54])

    def test_missing_reversed_and_unknown_phases_fail(self):
        for bad in [dict(major=10, sub=1), dict(major=10, sub=2),
                    dict(area=1), dict(major=99, sub=0)]:
            with self.subTest(bad=bad):
                self.route = traversal.Traversal(self.config, self.base, "indoor")
                self.feed()
                with self.assertRaisesRegex(RouteError, "ordered transition"):
                    self.feed(tick=31, world=680, **bad)
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        self.room()
        with self.assertRaisesRegex(RouteError, "ordered transition"):
            self.feed(tick=6, major=10, sub=2, area=1)

    def test_door_requires_movement_and_narrow_area_transient(self):
        self.feed()
        with self.assertRaisesRegex(RouteError, "without required"):
            self.feed(tick=31, major=10, sub=0, world=100)
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        self.feed()
        with self.assertRaisesRegex(RouteError, "ordered transition"):
            self.feed(tick=31, major=10, sub=0, area=1, world=679)

    def test_setup_reset_may_arrive_with_phase_transition(self):
        for values in [dict(tick=30), dict(tick=31, major=10, sub=0, world=679),
                       dict(tick=32, major=10, sub=1, world=640),
                       dict(tick=33, major=10, sub=2, world=640),
                       dict(tick=1, sub=2, area=1), dict(tick=2, sub=0, area=1)]:
            self.feed(**values)
        self.assertEqual(self.route.phase, "room_setup")
        self.assertEqual(len(self.route.resets), 1)

    def test_only_one_exact_reset_and_normal_wrap(self):
        self.feed(tick=254)
        self.feed(tick=255)
        self.feed(tick=0)
        self.assertEqual(self.route.ticks, 2)
        with self.assertRaisesRegex(RouteError, "discontinuity"):
            self.feed(tick=80)
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        self.room()
        with self.assertRaisesRegex(RouteError, "discontinuity"):
            self.feed(tick=1, area=1)
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        self.room()
        # Recreate setup to exercise a second reset, independent of phase-order rejection.
        self.route.index = 4
        self.route.previous_tick = 80
        with self.assertRaisesRegex(RouteError, "discontinuity"):
            self.feed(tick=1, sub=2, area=1)

    def test_completion_requires_observed_reset(self):
        self.room()
        self.route.resets.clear()
        for tick, world in [(6, 49), (7, 50), (8, 52), (9, 53)]:
            self.feed(tick=tick, area=1, world=world)
        with self.assertRaisesRegex(RouteError, "required canonical"):
            self.feed(tick=10, area=1, world=54)

    def test_initial_gameplay_values_and_unknown_mode_fail(self):
        with self.assertRaisesRegex(RouteError, "initial gameplay"):
            self.feed(area=1)
        self.route = traversal.Traversal(self.config, self.base, "indoor")
        with self.assertRaisesRegex(RouteError, "initial hearts"):
            state = self.sample()
            state["hearts"] = 10
            self.route.update(0, state)
        with self.assertRaisesRegex(RouteError, "unknown traversal"):
            traversal.Traversal(self.config, self.base, "missing")

    def test_diagnostic_never_falls_back_to_normal_completion(self):
        self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
        self.room()
        for tick in range(6, 30):
            self.assertFalse(self.feed(tick=tick, area=1, world=48 + tick))
        self.assertIsNone(self.route.hit)
        self.assertIsNone(self.route.anchor)

    def test_diagnostic_requires_post_hit_soak_and_confirmed_movement(self):
        self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
        self.room()
        for tick in range(6, 13):
            self.assertFalse(self.feed(tick=tick, area=1, count=1))
        self.assertIsNotNone(self.route.hit)
        self.assertIsNotNone(self.route.anchor)
        self.assertEqual(self.route.diagnostic_total, 1)
        for tick, world in [(13, 49), (14, 50), (15, 52), (16, 53)]:
            self.assertFalse(self.feed(tick=tick, area=1, world=world, count=1))
        self.assertTrue(self.feed(tick=17, area=1, world=54, count=1))
        self.assertTrue(self.route.metrics()["escape_exercised"])
        self.assertTrue(all(row["physical_frame"] >= self.route.hit["physical_frame"] for row in self.route.anchor))

    def test_diagnostic_initial_outside_room_wrap_and_discontinuity(self):
        self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
        with self.assertRaisesRegex(RouteError, "outside"):
            self.feed(count=1)
        self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
        self.room()
        self.feed(tick=6, area=1, count=1)
        self.route.previous_diagnostic = 255
        self.feed(tick=7, area=1, count=0)
        self.assertEqual(self.route.diagnostic_total, 2)
        with self.assertRaisesRegex(RouteError, "discontinuity"):
            self.feed(tick=8, area=1, count=20)

    def test_hit_then_trap_or_stall_fails(self):
        for failure in ("trap", "stall"):
            self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
            self.room()
            self.feed(tick=6, area=1, count=1)
            with self.assertRaisesRegex(RouteError, failure):
                if failure == "trap":
                    self.feed(tick=7, area=1, count=1, trap=0xec)
                else:
                    for _ in range(self.config["limits"]["stall_frames"]):
                        self.feed(tick=6, area=1, count=1)

    def test_torn_anchor_and_endpoint_are_deferred_not_rewritten(self):
        self.route = traversal.Traversal(self.config, self.base, "indoor_escape")
        self.room()
        for tick, world in [(6, 256), (7, 0), (8, 256), (9, 257), (10, 258)]:
            self.assertFalse(self.feed(tick=tick, area=1, count=1, world=world))
        self.assertEqual([row["state"]["player_world_x"] for row in self.route.anchor], [256, 257, 258])
        for tick, world in [(11, 258), (12, 514), (13, 258), (14, 259), (15, 260), (16, 262), (17, 263)]:
            self.assertFalse(self.feed(tick=tick, area=1, count=1, world=world))
        self.assertTrue(self.feed(tick=18, area=1, count=1, world=264))
        self.assertGreater(self.route.rejected_windows, 0)
        self.assertEqual(min(row["state"]["player_world_x"] for row in self.route.endpoint), 262)

    def test_inconsistent_word_sum_cannot_confirm_anchor(self):
        self.room()
        self.route.anchor = None
        for tick in range(6, 12):
            state = self.sample(tick=tick, area=1)
            state["camera_x"] = 256
            self.assertFalse(self.route.update(self.frame + tick, state))
        self.assertIsNone(self.route.anchor)

    def test_input_schedule_uses_named_buttons_and_boot_route(self):
        self.assertEqual(self.route.buttons(), set())
        self.route.boot.phase = "press_start"
        self.assertEqual(self.route.buttons(), {BUTTONS["start"]})
        self.feed()
        for tick, names in [(0, []), (59, []), (60, ["right"]), (79, ["right"]),
                            (80, ["right", "a"]), (85, ["right", "a"]), (86, ["right"]),
                            (140, ["right", "a"]), (146, ["right"])]:
            self.route.ticks = tick
            self.assertEqual(self.route.buttons(), {BUTTONS[name] for name in names})

    def test_schema_rejects_bad_bounds_unknown_data_and_ambiguous_phases(self):
        mutations = [lambda c, b: c["inputs"].update(pulse_period=0),
                     lambda c, b: c["inputs"].update(pulse_duration=61),
                     lambda c, b: c["inputs"].update(held=["invalid"]),
                     lambda c, b: c["inputs"].update(pulse_start=True),
                     lambda c, b: c["limits"].update(physical_frames=True),
                     lambda c, b: c["confirmation"].update(neighbor_delta=256),
                     lambda c, b: c["reset"].update(count=2),
                     lambda c, b: c["phases"][1].update(allowed=c["phases"][0]["allowed"]),
                     lambda c, b: c["phases"][0].update(unknown=1),
                     lambda c, b: c["phases"][0]["allowed"].update(unknown=[0]),
                     lambda c, b: c["completions"]["indoor"].update(diagnostic=1),
                     lambda c, b: c["diagnostic"].update(field="trap"),
                     lambda c, b: c["fields"].update(world="player_x"),
                     lambda c, b: b["ram_words"].update(player_world_x=8191),
                     lambda c, b: b["ram_words"].update(trap=1),
                     lambda c, b: b["ram"].update(trap=False)]
        for mutate in mutations:
            config, base = copy.deepcopy(self.config), copy.deepcopy(self.base)
            mutate(config, base)
            with self.subTest(mutate=mutate), self.assertRaises(RouteError):
                traversal.validate(config, base)

    def test_profile_pins_both_frozen_inputs_and_rejects_unknown_top_level(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "profile.toml"
            base = Path(directory) / "core-routes.toml"
            base.write_bytes((PROFILE.parent / "core-routes.toml").read_bytes())
            original = PROFILE.read_text()
            for content, reason in [(original.replace("version = 1", "version = 1\nunknown = true"), "unknown"),
                                    (original.replace(self.config["base"]["sha256"], "0" * 64), "profile hash"),
                                    (original.replace(self.config["base"]["runner_sha256"], "0" * 64), "runner hash")]:
                path.write_text(content)
                with self.assertRaisesRegex(RouteError, reason):
                    traversal.load_profile(path)

    def test_video_hash_ignores_padding_and_rejects_invalid_data(self):
        self.assertEqual(traversal.video_digest((b"\x12\x34xx", 1, 1, 4, 2)),
                         traversal.video_digest((b"\x12\x34", 1, 1, 2, 2)))
        for pixels in [(b"", 1, 1, 2, 2), (b"xx", 1, 1, 1, 2), (b"xx", 1, 1, 2, 9)]:
            with self.assertRaises(RouteError):
                traversal.video_digest(pixels)


class RunTests(unittest.TestCase):
    # Use the same pure observations for a fake host interface, never as a game oracle.
    setUp = TraversalTests.setUp
    sample = TraversalTests.sample

    def fake_states(self):
        states = [self.sample(tick=30), self.sample(tick=31, world=220),
                  self.sample(tick=32, major=10, sub=0, world=679),
                  self.sample(tick=33, major=10, sub=1, world=640),
                  self.sample(tick=34, major=10, sub=2, world=640),
                  self.sample(tick=35, sub=3, area=1), self.sample(tick=1, sub=2, area=1)]
        states += [self.sample(tick=tick, area=1, world=48 + max(0, tick-4)) for tick in range(2, 20)]
        return states

    def exercise(self, failure=None):
        config, base = copy.deepcopy(self.config), copy.deepcopy(self.base)
        config["limits"]["physical_frames"] = 30
        states = self.fake_states()
        if failure == "limit":
            config["limits"]["physical_frames"] = 2
        if failure == "trap":
            states[1]["trap"] = 0xee
        instances = []

        class FakeCore:
            def __init__(self, *args):
                if failure == "construct":
                    raise RouteError("construct failed")
                instances.append(self)
                self.queried, self.advertised = {}, {}
                self.video_calls, self.error, self.pixel_format = 0, None, 2
                self.pixels, self.closed, self.index = None, False, 0
                self.ram = bytearray(8192)
                self.library = SimpleNamespace(retro_run=self.advance)
                self.info = SimpleNamespace(name=b"fake host", version=b"1")
                self.av = SimpleNamespace(timing=SimpleNamespace(fps=float("nan") if failure == "fps" else 59.9))

            def start(self, requested, size):
                if failure == "start":
                    raise RouteError("start options failed")

            def advance(self):
                state = states[min(self.index, len(states)-1)]
                self.index += 1
                for key, offset in base["ram"].items():
                    self.ram[offset] = state[key]
                for key, offset in base["ram_words"].items():
                    self.ram[offset:offset+2] = state[key].to_bytes(2, "little")
                if failure != "callback":
                    self.video_calls += 1
                    self.pixels = (b"\x1f\x00", 1, 1, 2, 2)
                if failure == "callback_error":
                    self.error = "callback failed"

            def check_options(self, requested):
                if failure == "final":
                    raise RouteError("final options failed")

            def close(self):
                self.closed = True
                if failure == "close":
                    raise RouteError("close failed")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "dummy"
            artifact.write_bytes(b"fixture")
            args = argparse.Namespace(profile=PROFILE, core=artifact, rom=artifact,
                                      mode="indoor", output=root / "result", capture_every=3)
            with patch.object(traversal, "load_profile", return_value=(config, base, PROFILE)), \
                    patch.object(traversal, "Core", FakeCore), contextlib.redirect_stdout(io.StringIO()):
                if failure:
                    with self.assertRaises(RouteError):
                        traversal.run(args)
                else:
                    self.assertEqual(traversal.run(args)["status"], "passed")
                summary = json.loads((args.output / "summary.json").read_text())
                self.assertEqual(summary["status"], "failed" if failure else "passed")
                self.assertEqual(summary["requested_options"], base["core_options"])
                if instances:
                    self.assertTrue(instances[0].closed)
                if failure in ("construct", "start", "fps"):
                    self.assertNotIn("core_fps", summary)
                if failure == "trap":
                    self.assertIn("238", (args.output / "frames.csv").read_text())
                if failure is None:
                    self.assertTrue(list(args.output.glob("*.ppm")))
                    self.assertTrue(summary["confirmed_endpoint"])
                    with self.assertRaises(FileExistsError):
                        traversal.run(args)

    def test_runner_success_records_provenance_captures_and_never_overwrites(self):
        self.exercise()

    def test_runner_errors_keep_failed_summary_and_close_the_core(self):
        for failure in ("construct", "start", "fps", "callback", "callback_error", "final", "limit", "trap", "close"):
            with self.subTest(failure=failure):
                self.exercise(failure)


if __name__ == "__main__":
    unittest.main()
