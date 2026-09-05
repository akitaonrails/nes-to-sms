#!/usr/bin/env python3
"""Run a bounded, state/tick-anchored SMS route through an actual libretro core.

Python 3.11+, standard library only; run retro tooling in the project Docker image.
Each CSV row is a physical retro_run, not a translated NMI or a trace snapshot.
Pixel hashes/captures come exclusively from core video callbacks. Route addresses
and game input intent belong in the supplied profile, never in this runner.
"""

import argparse
import csv
import ctypes as C
import hashlib
import json
import math
from pathlib import Path
import sys
import tomllib


BUTTONS = {"b": 0, "y": 1, "select": 2, "start": 3, "up": 4,
           "down": 5, "left": 6, "right": 7, "a": 8, "x": 9}


class RouteError(Exception):
    pass


def counter_delta(previous, current, maximum=8):
    delta = (current - previous) & 255
    if delta > maximum:
        raise RouteError(f"tick discontinuity: {previous} -> {current} (delta {delta})")
    return delta


def pixel_rgb(raw, width, height, pitch, fmt):
    """libretro's formats are native-endian; ignore row padding."""
    if fmt not in (0, 1, 2):
        raise RouteError(f"unsupported pixel format {fmt}")
    size = 4 if fmt == 1 else 2
    if width < 1 or height < 1 or pitch < width * size or len(raw) < height * pitch:
        raise RouteError("invalid video dimensions/pitch/data length")
    rgb = bytearray(width * height * 3)
    pos = 0
    for y in range(height):
        for x in range(width):
            offset = y * pitch + x * size
            value = int.from_bytes(raw[offset:offset + size], sys.byteorder)
            if fmt == 1:
                color = ((value >> 16) & 255, (value >> 8) & 255, value & 255)
            else:
                green_bits = 6 if fmt == 2 else 5
                color = (((value >> (5 + green_bits)) & 31) * 255 // 31,
                         ((value >> 5) & ((1 << green_bits) - 1)) * 255 // ((1 << green_bits) - 1),
                         (value & 31) * 255 // 31)
            rgb[pos:pos + 3] = bytes(color)
            pos += 3
    return bytes(rgb)


class Route:
    def __init__(self, config, name):
        self.config = config
        self.spec = config["routes"][name]
        self.phase = "wait_title"
        self.title_frame = None
        self.stage_frame = None
        self.previous_tick = None
        self.ticks = 0
        self.lag = 0
        self.max_lag = 0
        self.observed = set()
        self.observation_ticks = {}
        self.initial_state = None
        self.latest_state = None
        self.landmarks = {}
        self.events = self.spec["events"]
        event_ticks = [event["tick"] for event in self.events]
        if (not event_ticks or event_ticks[0] != 0
                or any(a >= b for a, b in zip(event_ticks, event_ticks[1:]))
                or event_ticks[-1] >= self.spec["ticks"]):
            raise RouteError("route events must increase strictly from tick 0 before route end")
        for event in self.events:
            if any(button not in BUTTONS for button in event["buttons"]):
                raise RouteError("route contains an unknown libretro button")
        marks = self.spec.get("landmarks", [])
        if any(not isinstance(tick, int) or not 0 <= tick <= self.spec["ticks"] for tick in marks):
            raise RouteError("landmarks must be integer ticks within the route")

    def buttons(self):
        if self.phase == "press_start":
            return {BUTTONS["start"]}
        if self.phase != "stage":
            return set()
        current = self.events[0]
        for event in self.events[1:]:
            if event["tick"] > self.ticks:
                break
            current = event
        return {BUTTONS[button] for button in current["buttons"]}

    def update(self, frame, state):
        if state["trap"]:
            raise RouteError(f"trap marker {state['trap']:#04x} at physical frame {frame}")
        pair = [state["state"], state["substate"]]
        boot = self.config["boot"]
        if self.phase != "stage" and pair == boot["stage_state"]:
            self.phase = "stage"
            self.stage_frame = frame
            self.previous_tick = state["game_tick"]
            self.initial_state = dict(state)
            self.latest_state = dict(state)
            for key, expected in self.spec.get("initial_expected", {}).items():
                if state[key] != expected:
                    raise RouteError(f"initial {key}: {state[key]}, expected {expected}")
        elif self.phase == "wait_title" and pair == boot["title_state"]:
            self.phase = "settle_title"
            self.title_frame = frame
        elif self.phase == "settle_title":
            if pair != boot["title_state"]:
                raise RouteError("title state changed before Start")
            if frame - self.title_frame >= boot["title_settle_frames"]:
                self.phase = "press_start"
        elif self.phase == "press_start" and pair != boot["title_state"]:
            self.phase = "wait_stage"
        elif self.phase == "stage":
            if pair != boot["stage_state"]:
                raise RouteError(f"left expected gameplay state: {pair}")
            delta = counter_delta(self.previous_tick, state["game_tick"])
            self.previous_tick = state["game_tick"]
            self.ticks += delta
            self.lag = 0 if delta else self.lag + 1
            self.max_lag = max(self.max_lag, self.lag)
            if self.lag >= self.config["limits"]["stall_frames"]:
                raise RouteError(f"gameplay tick stalled for {self.lag} physical frames")
        if self.stage_frame is None and frame >= self.config["limits"]["boot_frames"]:
            raise RouteError("stage was not reached before the boot deadline")
        if self.phase == "stage":
            self.latest_state = dict(state)
            for tick in self.spec.get("landmarks", []):
                if str(tick) in self.landmarks:
                    continue
                if self.ticks > tick:
                    raise RouteError(f"physical sampling missed exact route landmark tick {tick}")
                if self.ticks == tick:
                    self.landmarks[str(tick)] = {
                        "physical_frame": frame,
                        "state": {key: state[key] for key in self.config.get("compare_fields", {})}}
            for key, minimum in self.spec.get("observe_min", {}).items():
                if state[key] >= minimum:
                    self.observed.add(key)
                    self.observation_ticks.setdefault(key, self.ticks)
        if self.ticks >= self.spec["ticks"]:
            for key, minimum in self.spec.get("minimum_advance", {}).items():
                advance = state[key] - self.initial_state[key]
                if advance < minimum:
                    raise RouteError(f"insufficient {key} movement: {advance}, need {minimum}")
            missing = set(self.spec.get("observe_min", {})) - self.observed
            if missing:
                raise RouteError(f"route did not observe required state: {sorted(missing)}")
            soak = self.spec.get("observation_soak_ticks", 0)
            if any(self.ticks - tick < soak for tick in self.observation_ticks.values()):
                raise RouteError(f"route did not run {soak} gameplay ticks after required observation")
            return True
        return False

    def metrics(self, frame, fps):
        intervals = frame - self.stage_frame if self.stage_frame is not None else 0
        return {"stage_frame": self.stage_frame, "game_ticks": self.ticks,
                "gameplay_frame_intervals": intervals,
                "game_ticks_per_second": self.ticks * fps / intervals if intervals else None,
                "max_lag_frames": self.max_lag,
                "advance": {key: self.latest_state[key] - self.initial_state[key]
                            for key in self.spec.get("minimum_advance", {})}
                           if self.initial_state is not None else {},
                "landmarks": self.landmarks,
                "observation_ticks": self.observation_ticks,
                "required_observations": sorted(self.observed)}


def compare_landmarks(baseline, candidate, fields):
    """Compare game facts at the same ticks, not IRQ/time bytes or physical frames.

Position tolerances belong in the profile and must account only for sampling a
running producer at a physical boundary. This is not a visual/RAM parity oracle.
"""
    if baseline.get("status") != "passed":
        raise RouteError("comparison baseline did not pass its route")
    for key in ("core_sha256", "profile_sha256", "requested_options", "route", "route_spec", "core_fps"):
        if baseline.get(key) != candidate.get(key):
            raise RouteError(f"incompatible comparison baseline: {key}")
    if not fields or not candidate["landmarks"]:
        raise RouteError("comparison needs profile fields and route landmarks")
    if set(baseline["landmarks"]) != set(candidate["landmarks"]):
        raise RouteError("comparison landmark tick sets differ")
    maximum = dict.fromkeys(fields, 0)
    for tick, mark in candidate["landmarks"].items():
        previous = baseline["landmarks"][tick]["state"]
        for key, tolerance in fields.items():
            if not isinstance(tolerance, int) or tolerance < 0:
                raise RouteError("comparison tolerances must be nonnegative integers")
            difference = abs(mark["state"][key] - previous[key])
            maximum[key] = max(maximum[key], difference)
            if difference > tolerance:
                raise RouteError(f"landmark {tick}: {key} changed {previous[key]} -> "
                                 f"{mark['state'][key]} (tolerance {tolerance})")
    return {"baseline_rom_sha256": baseline["rom_sha256"], "maximum_landmark_difference": maximum}


class Variable(C.Structure):
    _fields_ = [("key", C.c_char_p), ("value", C.c_char_p)]


class Game(C.Structure):
    _fields_ = [("path", C.c_char_p), ("data", C.c_void_p),
                ("size", C.c_size_t), ("meta", C.c_char_p)]


class System(C.Structure):
    _fields_ = [("name", C.c_char_p), ("version", C.c_char_p),
                ("extensions", C.c_char_p), ("fullpath", C.c_bool),
                ("block_extract", C.c_bool)]


class Geometry(C.Structure):
    _fields_ = [("base_width", C.c_uint), ("base_height", C.c_uint),
                ("max_width", C.c_uint), ("max_height", C.c_uint), ("aspect", C.c_float)]


class Timing(C.Structure):
    _fields_ = [("fps", C.c_double), ("sample_rate", C.c_double)]


class AVInfo(C.Structure):
    _fields_ = [("geometry", Geometry), ("timing", Timing)]


class Core:
    def __init__(self, path, rom, output, requested):
        self.library = C.CDLL(str(path.resolve()))
        self.options = {key.encode(): value.encode() for key, value in requested.items()}
        self.queried = {}
        self.advertised = {}
        self.directory = str(output.resolve()).encode()
        self.pixel_format = 0
        self.pixels = None
        self.buttons = set()
        self.video_calls = 0
        self.error = None
        self.initialized = False
        self.loaded = False
        self.callbacks = []  # Keep every C callback alive until after deinit.
        self.rom_data = C.create_string_buffer(rom.read_bytes())
        self.rom_path = str(rom.resolve()).encode()
        self.game = Game(self.rom_path, C.cast(self.rom_data, C.c_void_p), len(self.rom_data) - 1, None)
        self.fn("retro_api_version", C.c_uint, [])
        self.fn("retro_init", None, [])
        self.fn("retro_deinit", None, [])
        self.fn("retro_run", None, [])
        self.fn("retro_unload_game", None, [])
        self.fn("retro_load_game", C.c_bool, [C.POINTER(Game)])
        self.fn("retro_get_system_info", None, [C.POINTER(System)])
        self.fn("retro_get_system_av_info", None, [C.POINTER(AVInfo)])
        self.fn("retro_get_memory_data", C.c_void_p, [C.c_uint])
        self.fn("retro_get_memory_size", C.c_size_t, [C.c_uint])
        self.fn("retro_set_controller_port_device", None, [C.c_uint, C.c_uint])
        self.callback("environment", C.c_bool, [C.c_uint, C.c_void_p], self.environment)
        self.callback("video_refresh", None, [C.c_void_p, C.c_uint, C.c_uint, C.c_size_t], self.video)
        self.callback("audio_sample", None, [C.c_int16, C.c_int16], lambda left, right: None)
        self.callback("audio_sample_batch", C.c_size_t, [C.c_void_p, C.c_size_t], lambda data, size: size)
        self.callback("input_poll", None, [], lambda: None)
        self.callback("input_state", C.c_int16, [C.c_uint] * 4,
                      lambda port, device, index, ident: int(port == 0 and device == 1 and index == 0 and ident in self.buttons))

    def fn(self, name, result, args):
        fn = getattr(self.library, name)
        fn.restype, fn.argtypes = result, args
        return fn

    def callback(self, name, result, args, function):
        def guarded(*arguments):
            try:
                return function(*arguments)
            except Exception as error:
                # ctypes otherwise prints and discards callback exceptions.
                self.error = f"{name} callback: {error}"
                return False if result else None
        callback = C.CFUNCTYPE(result, *args)(guarded)
        self.callbacks.append(callback)
        self.fn("retro_set_" + name, None, [type(callback)])(callback)

    def environment(self, command, data):
        if command in (9, 30, 31):  # system/content/save directory
            C.cast(data, C.POINTER(C.c_char_p))[0] = self.directory
        elif command == 16:  # legacy core options; ask for version 0 below
            variables = C.cast(data, C.POINTER(Variable))
            index = 0
            while variables[index].key:
                key, value = variables[index].key, variables[index].value
                if value and b"; " in value:
                    choices = value.split(b"; ", 1)[1].split(b"|")
                    self.advertised[key.decode()] = [item.decode() for item in choices]
                    self.options.setdefault(key, choices[0])
                index += 1
        elif command == 15:
            variable = C.cast(data, C.POINTER(Variable)).contents
            variable.value = self.options.get(variable.key)
            self.queried[variable.key.decode()] = variable.value.decode() if variable.value else None
            return variable.value is not None
        elif command in (2, 3, 17):  # overscan false / can-dupe true / variable-update false
            C.cast(data, C.POINTER(C.c_bool))[0] = command == 3
        elif command in (39, 52):  # language English / core-options version 0
            C.cast(data, C.POINTER(C.c_uint))[0] = 0
        elif command == 10:
            self.pixel_format = C.cast(data, C.POINTER(C.c_int))[0]
            return self.pixel_format in (0, 1, 2)
        else:
            return False
        return True

    def video(self, data, width, height, pitch):
        self.video_calls += 1
        if data == C.c_void_p(-1).value:
            raise RouteError("hardware-rendered video is unsupported")
        if data:
            self.pixels = (C.string_at(data, height * pitch), width, height, pitch, self.pixel_format)

    def start(self, requested, minimum_ram):
        if self.library.retro_api_version() != 1:
            raise RouteError("unsupported libretro ABI")
        self.info = System()
        self.library.retro_get_system_info(C.byref(self.info))
        self.library.retro_init()
        self.initialized = True
        if not self.library.retro_load_game(C.byref(self.game)):
            raise RouteError("core refused ROM")
        self.loaded = True
        self.library.retro_set_controller_port_device(0, 1)
        self.av = AVInfo()
        self.library.retro_get_system_av_info(C.byref(self.av))
        if not math.isfinite(self.av.timing.fps) or self.av.timing.fps <= 0:
            raise RouteError(f"invalid core frame rate {self.av.timing.fps}")
        address = self.library.retro_get_memory_data(2)  # RETRO_MEMORY_SYSTEM_RAM
        size = self.library.retro_get_memory_size(2)
        if not address or size < minimum_ram:
            raise RouteError(f"core exposes {size} bytes of RAM; need {minimum_ram}")
        self.ram = (C.c_ubyte * size).from_address(address)
        self.check_options(requested)

    def check_options(self, requested):
        for key, value in requested.items():
            if value not in self.advertised.get(key, []):
                raise RouteError(f"core does not advertise requested option {key}={value}")
            if self.queried.get(key) != value:
                raise RouteError(f"core did not query requested option {key}={value}")
        if self.error:
            raise RouteError(self.error)

    def close(self):
        if self.loaded:
            self.library.retro_unload_game()
        if self.initialized:
            self.library.retro_deinit()


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(args):
    config = tomllib.loads(args.profile.read_text())
    words = config.get("ram_words", {})
    offsets = [*config["ram"].values(), *words.values()]
    if any(not isinstance(address, int) or isinstance(address, bool) or address < 0
           for address in offsets):
        raise RouteError("profile RAM offsets must be nonnegative integers")
    if set(config["ram"]) & set(words):
        raise RouteError("RAM byte and word observation names must be distinct")
    route = Route(config, args.route)
    requested = dict(config["core_options"])
    if args.overclock:
        requested["genesis_plus_gx_overclock"] = args.overclock
    if args.capture_every < 1 or args.frames < 1:
        raise RouteError("frames and capture-every must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    core = Core(args.core, args.rom, args.output, requested)
    summary = {"status": "failed", "route": args.route,
               "core_sha256": sha256(args.core), "rom_sha256": sha256(args.rom),
               "profile_sha256": sha256(args.profile), "runner_sha256": sha256(Path(__file__)),
               "requested_options": requested, "max_physical_frames": args.frames,
               "capture_every": args.capture_every, "route_spec": route.spec}
    frame = -1
    state = {}
    irq_total = 0
    previous_irq = None
    last_state = None
    try:
        core.start(requested, max([address + 1 for address in config["ram"].values()]
                                  + [address + 2 for address in words.values()]))
        summary.update(core=core.info.name.decode(), core_version=core.info.version.decode(),
                       core_fps=core.av.timing.fps)
        with (args.output / "frames.csv").open("w", newline="") as log:
            fields = ["physical_frame", "route_tick", "phase", "buttons", "irq_total",
                      "video_calls", "pixel_sha256", *config["ram"], *words]
            writer = csv.DictWriter(log, fieldnames=fields)
            writer.writeheader()
            for frame in range(args.frames):
                core.buttons = route.buttons()
                calls_before = core.video_calls
                core.library.retro_run()
                if core.error:
                    raise RouteError(core.error)
                if core.video_calls == calls_before or core.pixels is None:
                    raise RouteError("physical frame had no core video callback/image")
                state = {key: core.ram[address] for key, address in config["ram"].items()}
                state.update({key: core.ram[address] | (core.ram[address + 1] << 8)
                              for key, address in words.items()})
                if previous_irq is not None:
                    irq_total += (state["irq"] - previous_irq) & 255
                previous_irq = state["irq"]
                complete = route.update(frame, state)
                raw, width, height, pitch, fmt = core.pixels
                # Exclude padding from hashes. Preserve native format in provenance.
                size = 4 if fmt == 1 else 2
                digest = hashlib.sha256(b"".join(raw[y * pitch:y * pitch + width * size]
                                               for y in range(height))).hexdigest()
                writer.writerow({"physical_frame": frame, "route_tick": route.ticks,
                                 "phase": route.phase, "buttons": "+".join(str(b) for b in sorted(core.buttons)),
                                 "irq_total": irq_total, "video_calls": core.video_calls - calls_before,
                                 "pixel_sha256": digest, **state})
                pair = (state["state"], state["substate"])
                if pair != last_state or complete or (route.phase == "stage" and frame % args.capture_every == 0):
                    rgb = pixel_rgb(*core.pixels)
                    name = f"frame-{frame:06d}-tick-{route.ticks:05d}.ppm"
                    (args.output / name).write_bytes(f"P6\n{width} {height}\n255\n".encode() + rgb)
                if pair != last_state:
                    print(f"frame={frame} state={pair} ticks={route.ticks}", flush=True)
                    last_state = pair
                if complete:
                    summary["status"] = "passed"
                    break
            if summary["status"] != "passed":
                raise RouteError("physical-frame limit reached before route completion")
        core.check_options(requested)
        if args.compare:
            summary.update(route.metrics(frame, core.av.timing.fps))
            summary["comparison"] = compare_landmarks(
                json.loads(args.compare.read_text()), summary, config.get("compare_fields", {}))
    except Exception as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
        raise
    finally:
        summary.update(last_frame=frame, final_state=state, irq_total=irq_total,
                       options_queried=core.queried, options_advertised=core.advertised,
                       video_callbacks=core.video_calls, pixel_format=core.pixel_format,
                       **route.metrics(frame, core.av.timing.fps if hasattr(core, "av") else 60))
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({key: value for key, value in summary.items()
                          if key not in ("options_advertised", "options_queried")}), flush=True)
        core.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("profile", type=Path)
    parser.add_argument("route")
    parser.add_argument("output", type=Path, help="new artifact directory (never overwritten)")
    parser.add_argument("--overclock", help="override the profile's numeric GPGX overclock")
    parser.add_argument("--compare", type=Path, help="baseline summary.json; require matching tick landmarks before claiming improvement")
    parser.add_argument("--frames", type=int, default=12000, help="hard physical-frame bound")
    parser.add_argument("--capture-every", type=int, default=10, help="gameplay physical-frame capture stride; 1 records continuous output")
    args = parser.parse_args()
    try:
        run(args)
    except (RouteError, OSError, KeyError, ValueError) as error:
        parser.exit(1, f"core route failed: {error}\n")


if __name__ == "__main__":
    main()
