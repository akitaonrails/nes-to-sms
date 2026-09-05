"""No emulator needed: python3 -m unittest discover -s tools -p test_core_route.py."""
import ctypes as C
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from core_route import AVInfo, Core, Route, RouteError, compare_landmarks, counter_delta, pixel_rgb, run


def config():
    return {"boot": {"title_state": [1, 1], "stage_state": [5, 6], "title_settle_frames": 2},
            "limits": {"boot_frames": 20, "stall_frames": 3},
            "routes": {"test": {"ticks": 4, "events": [
                {"tick": 0, "buttons": []}, {"tick": 2, "buttons": ["right", "a"]}]}}}


def state(tick=0, pair=(5, 6), **values):
    return {"state": pair[0], "substate": pair[1], "game_tick": tick, "trap": 0, **values}


class RouteTests(unittest.TestCase):
    def test_state_and_tick_anchors(self):
        route = Route(config(), "test")
        route.update(0, state(pair=(0, 0)))
        route.update(1, state(pair=(1, 1)))
        self.assertEqual(route.buttons(), set())
        route.update(3, state(pair=(1, 1)))
        self.assertEqual(route.buttons(), {3})
        route.update(4, state(pair=(5, 0)))
        self.assertEqual(route.buttons(), set())
        route.update(5, state(254))
        route.update(6, state(255))
        route.update(7, state(255))
        self.assertEqual(route.buttons(), set())
        route.update(8, state(0))
        self.assertEqual(route.buttons(), {7, 8})
        self.assertTrue(route.update(9, state(2)))
        self.assertEqual(route.metrics(9, 60)["game_ticks_per_second"], 60)
        self.assertEqual(route.max_lag, 1)

    def test_failures(self):
        for updates, message in [
            ([(0, state(trap=0xe1))], "trap"),
            ([(20, state(pair=(0, 0)))], "stage was not reached"),
            ([(0, state()), (1, state(pair=(7, 0)))], "left expected"),
            ([(0, state()), (1, state()), (2, state()), (3, state())], "stalled"),
            ([(0, state()), (1, state(100))], "discontinuity"),
        ]:
            with self.subTest(message=message):
                route = Route(config(), "test")
                with self.assertRaisesRegex(RouteError, message):
                    for frame, sample in updates:
                        route.update(frame, sample)

    def test_required_heart_observation(self):
        data = config()
        data["routes"]["test"]["observe_min"] = {"hearts": 10}
        route = Route(data, "test")
        route.update(0, state(hearts=5))
        with self.assertRaisesRegex(RouteError, "required state"):
            route.update(1, state(4, hearts=5))
        route = Route(data, "test")
        route.update(0, state(hearts=5))
        route.update(1, state(2, hearts=10))
        self.assertTrue(route.update(2, state(4, hearts=5)))

    def test_heart_requires_initial_count_and_post_pickup_soak(self):
        data = config()
        data["routes"]["test"].update(initial_expected={"hearts": 5},
                                     observe_min={"hearts": 10}, observation_soak_ticks=2)
        route = Route(data, "test")
        with self.assertRaisesRegex(RouteError, "initial hearts"):
            route.update(0, state(hearts=10))
        self.assertEqual(route.metrics(0, 60)["game_ticks"], 0)
        route = Route(data, "test")
        route.update(0, state(hearts=5))
        with self.assertRaisesRegex(RouteError, "gameplay ticks after"):
            route.update(1, state(4, hearts=10))
        route = Route(data, "test")
        route.update(0, state(hearts=5))
        route.update(1, state(2, hearts=10))
        self.assertTrue(route.update(2, state(4, hearts=10)))
        self.assertEqual(route.observation_ticks, {"hearts": 2})

    def test_wrap_and_initial_metrics(self):
        self.assertEqual(counter_delta(255, 0), 1)
        self.assertIsNone(Route(config(), "test").metrics(0, 60)["game_ticks_per_second"])

    def test_reject_ambiguous_events(self):
        data = config()
        data["routes"]["test"]["events"][1]["tick"] = 0
        with self.assertRaisesRegex(RouteError, "increase strictly"):
            Route(data, "test")

    def test_advancing_ticks_without_movement_is_not_a_walk(self):
        data = config()
        data["routes"]["test"]["minimum_advance"] = {"camera_x": 3}
        route = Route(data, "test")
        route.update(0, state(camera_x=250))
        with self.assertRaisesRegex(RouteError, "insufficient camera_x movement"):
            route.update(1, state(4, camera_x=250))
        route = Route(data, "test")
        route.update(0, state(camera_x=250))
        self.assertTrue(route.update(1, state(4, camera_x=258)))
        self.assertEqual(route.metrics(1, 60)["advance"], {"camera_x": 8})

    def test_landmarks_are_tick_anchored(self):
        data = config()
        data["compare_fields"] = {"camera_x": 2}
        data["routes"]["test"]["landmarks"] = [0, 2, 4]
        route = Route(data, "test")
        route.update(10, state(camera_x=0))
        route.update(11, state(1, camera_x=1))
        route.update(12, state(1, camera_x=1))
        route.update(13, state(2, camera_x=2))
        route.update(14, state(4, camera_x=4))
        self.assertEqual(route.landmarks["2"], {"physical_frame": 13, "state": {"camera_x": 2}})
        route = Route(data, "test")
        route.update(0, state(camera_x=0))
        with self.assertRaisesRegex(RouteError, "missed exact"):
            route.update(1, state(3, camera_x=3))

    def test_comparison_ignores_physical_timing_but_rejects_changed_route(self):
        baseline = {"status": "passed", "rom_sha256": "baseline",
                    "landmarks": {"2": {"physical_frame": 30, "state": {"camera_x": 260}}}}
        candidate = {"landmarks": {"2": {"physical_frame": 20, "state": {"camera_x": 261}}}}
        result = compare_landmarks(baseline, candidate, {"camera_x": 2})
        self.assertEqual(result["maximum_landmark_difference"], {"camera_x": 1})
        candidate["landmarks"]["2"]["state"]["camera_x"] = 250
        with self.assertRaisesRegex(RouteError, "landmark 2"):
            compare_landmarks(baseline, candidate, {"camera_x": 2})
        candidate["requested_options"] = {"clock": "100"}
        with self.assertRaisesRegex(RouteError, "incompatible"):
            compare_landmarks(baseline, candidate, {"camera_x": 2})


class PixelTests(unittest.TestCase):
    def test_formats_and_row_padding(self):
        for fmt, size, value in [(0, 2, 0x7c00), (1, 4, 0x00ff0000), (2, 2, 0xf800)]:
            raw = value.to_bytes(size, sys.byteorder) + b"xx"
            self.assertEqual(pixel_rgb(raw, 1, 1, size + 2, fmt), b"\xff\0\0")
        self.assertEqual(pixel_rgb((0x07e0).to_bytes(2, sys.byteorder), 1, 1, 2, 2), b"\0\xff\0")

    def test_invalid_video(self):
        for values in [(b"", 1, 1, 2, 2), (b"00", 2, 1, 2, 2), (b"00", 1, 1, 2, 3)]:
            with self.assertRaises(RouteError):
                pixel_rgb(*values)


class CoreContractTests(unittest.TestCase):
    def test_invalid_core_frame_rate(self):
        for fps in (0, -1, float("inf"), float("nan")):
            core = Core.__new__(Core)
            core.game = C.c_int()
            def av_info(pointer):
                C.cast(pointer, C.POINTER(AVInfo)).contents.timing.fps = fps
            core.library = SimpleNamespace(
                retro_api_version=lambda: 1, retro_get_system_info=lambda info: None,
                retro_init=lambda: None, retro_load_game=lambda game: True,
                retro_set_controller_port_device=lambda port, device: None,
                retro_get_system_av_info=av_info)
            with self.assertRaisesRegex(RouteError, "invalid core frame rate"):
                core.start({}, 1)

    def test_options_must_be_advertised_and_queried(self):
        core = Core.__new__(Core)
        core.error = None
        core.advertised = {"clock": ["100", "500"]}
        core.queried = {}
        with self.assertRaisesRegex(RouteError, "did not query"):
            core.check_options({"clock": "500"})
        core.queried = {"clock": "500"}
        core.check_options({"clock": "500"})
        with self.assertRaisesRegex(RouteError, "does not advertise"):
            core.check_options({"clock": "500%"})

    def test_duplicate_video_retains_previous_image(self):
        core = Core.__new__(Core)
        core.pixel_format, core.video_calls, core.pixels = 2, 0, None
        raw = C.create_string_buffer(b"\0\0")
        core.video(C.addressof(raw), 1, 1, 2)
        original = core.pixels
        core.video(None, 1, 1, 2)
        self.assertEqual(core.pixels, original)
        self.assertEqual(core.video_calls, 2)
        with self.assertRaisesRegex(RouteError, "hardware-rendered"):
            core.video(C.c_void_p(-1).value, 1, 1, 2)


class RunContractTests(unittest.TestCase):
    def test_final_option_failure_cannot_publish_success(self):
        class FakeCore:
            def __init__(self, *args):
                self.library = self
                self.error = None
                self.queried = self.advertised = {}
                self.video_calls = self.pixel_format = 0
                self.pixels = (b"\0\0", 1, 1, 2, 0)
                self.ram = [5, 6, 0, 0, 0]
                self.info = SimpleNamespace(name=b"fake", version=b"test")
                self.av = SimpleNamespace(timing=SimpleNamespace(fps=60))

            def start(self, *args):
                pass

            def retro_run(self):
                self.video_calls += 1
                self.ram[2] += 1

            def check_options(self, requested):
                raise RouteError("late option failure")

            def close(self):
                pass

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            rom = root / "rom"
            rom.write_bytes(b"test fixture")
            profile = root / "route.toml"
            profile.write_text('''
[ram]
state = 0
substate = 1
game_tick = 2
trap = 3
irq = 4
[boot]
title_state = [1, 1]
stage_state = [5, 6]
title_settle_frames = 1
[limits]
boot_frames = 10
stall_frames = 3
[core_options]
[routes.test]
ticks = 1
events = [{tick = 0, buttons = []}]
''')
            args = SimpleNamespace(core=rom, rom=rom, profile=profile, route="test",
                                   output=root / "result", overclock=None,
                                   frames=10, capture_every=1, compare=None)
            with patch("core_route.Core", FakeCore), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RouteError, "late option failure"):
                    run(args)
            summary = json.loads((args.output / "summary.json").read_text())
            self.assertEqual(summary["status"], "failed")
            self.assertEqual(summary["error"], "late option failure")
            self.assertEqual(summary["game_ticks_per_second"], 60)
            with self.assertRaises(FileExistsError):
                run(args)
            args.output = root / "bounded"
            args.frames = 1
            with patch("core_route.Core", FakeCore), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(RouteError, "physical-frame limit"):
                    run(args)
            summary = json.loads((args.output / "summary.json").read_text())
            self.assertEqual(summary["status"], "failed")

    def test_negative_ram_offset_rejected_before_core_load(self):
        with tempfile.TemporaryDirectory() as folder:
            profile = Path(folder) / "bad.toml"
            profile.write_text("[ram]\ntrap = -1\n")
            with self.assertRaisesRegex(RouteError, "nonnegative"):
                run(SimpleNamespace(profile=profile))


if __name__ == "__main__":
    unittest.main()
