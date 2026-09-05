#!/usr/bin/env python3
"""Bounded ordered traversal through the existing actual-libretro harness.

Python3.11+, standard library only. Game facts belong in the traversal TOML;
normal progress does not imply a diagnostic helper hit or a full-stage clear.
Use the project Docker image for the emulator, without host installations.
"""

import argparse
from collections import deque
import csv
import hashlib
import json
import math
from pathlib import Path
import sys
import tomllib

import core_route
from core_route import BUTTONS, Core, Route, RouteError, counter_delta, pixel_rgb, sha256


CSV_FIELDS = {"physical_frame", "route_tick", "phase", "buttons", "pixel_sha256",
              "video_calls", "events", "reset_delta", "diagnostic_delta"}


def integer(value, name, minimum=0, maximum=None):
    if (type(value) is not int or value < minimum
            or (maximum is not None and value > maximum)):
        raise RouteError(f"{name} must be an integer in {minimum}..{maximum}")
    return value


def table(value, name, required, optional=()):
    if not isinstance(value, dict) or not set(required) <= value.keys():
        raise RouteError(f"{name} requires fields {sorted(required)}")
    unknown = value.keys() - set(required) - set(optional)
    if unknown:
        raise RouteError(f"unknown {name} fields: {sorted(unknown)}")


def load_profile(path):
    config = tomllib.loads(path.read_text())
    table(config, "traversal", {"version", "ram_bytes", "fields", "base", "limits",
                               "inputs", "phases", "reset", "confirmation", "diagnostic",
                               "completions", "initial_expected"})
    if type(config["version"]) is not int or config["version"] != 1:
        raise RouteError("unsupported traversal profile version")
    table(config["base"], "base", {"profile", "sha256", "runner_sha256", "route"})
    relative = config["base"]["profile"]
    if not isinstance(relative, str) or not relative or Path(relative).is_absolute():
        raise RouteError("base profile must be a nonempty relative path")
    base_path = path.parent / relative
    if sha256(base_path) != config["base"]["sha256"]:
        raise RouteError("base profile hash mismatch")
    if sha256(Path(core_route.__file__)) != config["base"]["runner_sha256"]:
        raise RouteError("frozen core_route runner hash mismatch")
    base = tomllib.loads(base_path.read_text())
    validate(config, base)
    Route(base, config["base"]["route"])  # Validate the unchanged boot route too.
    return config, base, base_path


def validate(config, base):
    size = integer(config["ram_bytes"], "ram_bytes", 1, 65536)
    observations = {}
    for group, width in [("ram", 1), ("ram_words", 2)]:
        values = base.get(group, {})
        if not isinstance(values, dict):
            raise RouteError(f"base {group} must be a table")
        for name, offset in values.items():
            if not isinstance(name, str) or not name or name in observations or name in CSV_FIELDS:
                raise RouteError(f"duplicate/reserved observation {name}")
            integer(offset, f"RAM offset {name}", 0, size - width)
            observations[name] = width
    table(config["fields"], "fields", {"tick", "trap", "world", "camera", "screen"})
    for role, name in config["fields"].items():
        width = 2 if role in ("world", "camera") else 1
        if not isinstance(name, str) or observations.get(name) != width:
            raise RouteError(f"{role} must name a {width}-byte observation")
    if len(set(config["fields"].values())) != len(config["fields"]):
        raise RouteError("traversal field roles must be distinct")

    def expected(values, context, sets=False):
        if not isinstance(values, dict) or not values:
            raise RouteError(f"{context} must contain observation constraints")
        for name, value in values.items():
            if name not in observations:
                raise RouteError(f"unknown observation {name} in {context}")
            items = value if sets else [value]
            if not isinstance(items, list) or not items:
                raise RouteError(f"{context}.{name} requires nonempty values")
            for item in items:
                integer(item, f"{context}.{name}", 0, (1 << (observations[name] * 8)) - 1)
            if len(set(items)) != len(items):
                raise RouteError(f"duplicate allowed values in {context}.{name}")

    expected(config["initial_expected"], "initial_expected")
    phases = config["phases"]
    if not isinstance(phases, list) or len(phases) < 2:
        raise RouteError("at least two ordered phases are required")
    names = []
    for phase in phases:
        table(phase, "phase", {"name", "allowed"}, {"minimum_advance"})
        name = phase["name"]
        if not isinstance(name, str) or not name or name == "boot" or name in names:
            raise RouteError("phase names must be unique nonempty names other than boot")
        names.append(name)
        expected(phase["allowed"], name, sets=True)
        for field, minimum in phase.get("minimum_advance", {}).items():
            if field not in observations:
                raise RouteError(f"unknown advance observation {field}")
            integer(minimum, "phase minimum_advance", 1)
    for index, phase in enumerate(phases):
        for other in phases[index + 1:]:
            shared = phase["allowed"].keys() & other["allowed"].keys()
            if all(set(phase["allowed"][key]) & set(other["allowed"][key]) for key in shared):
                raise RouteError(f"ambiguous phase membership: {phase['name']}/{other['name']}")
    table(config["reset"], "reset", {"phase", "expected", "destination", "count"})
    reset = config["reset"]
    if reset["phase"] not in names[:-1]:
        raise RouteError("reset phase must precede the final phase")
    expected(reset["expected"], "reset.expected")
    reset_phase = phases[names.index(reset["phase"])]["allowed"]
    if any(key not in reset["expected"] or reset["expected"][key] not in values
           for key, values in reset_phase.items()):
        raise RouteError("reset expected state must identify its allowed phase")
    integer(reset["destination"], "reset destination", 0, 255)
    integer(reset["count"], "reset count", 1, 1)
    table(config["limits"], "limits", {"physical_frames", "stall_frames", "counter_delta", "capture_every"})
    for key in ("physical_frames", "stall_frames", "capture_every"):
        integer(config["limits"][key], key, 1)
    integer(config["limits"]["counter_delta"], "counter_delta", 1, 127)
    table(config["inputs"], "inputs", {"idle_ticks", "held", "pulse", "pulse_start", "pulse_period", "pulse_duration"})
    inputs = config["inputs"]
    for key in ("idle_ticks", "pulse_start"):
        integer(inputs[key], key)
    integer(inputs["pulse_period"], "pulse_period", 1)
    integer(inputs["pulse_duration"], "pulse_duration", 1, inputs["pulse_period"])
    if inputs["pulse_start"] < inputs["idle_ticks"]:
        raise RouteError("pulse cannot start during the initial idle")
    for key in ("held", "pulse"):
        values = inputs[key]
        if (not isinstance(values, list) or not values
                or any(not isinstance(value, str) or value not in BUTTONS for value in values)
                or len(set(values)) != len(values)):
            raise RouteError(f"{key} requires distinct known libretro buttons")
    table(config["confirmation"], "confirmation", {"neighbor_delta", "position_tolerance"})
    for name, value in config["confirmation"].items():
        integer(value, name, 0, 127)
    table(config["diagnostic"], "diagnostic", {"field", "offset", "initial", "counter_delta"})
    diagnostic = config["diagnostic"]
    if (not isinstance(diagnostic["field"], str) or not diagnostic["field"]
            or diagnostic["field"] in observations or diagnostic["field"] in CSV_FIELDS):
        raise RouteError("diagnostic field must be a distinct observation")
    integer(diagnostic["offset"], "diagnostic offset", 0, size - 1)
    integer(diagnostic["initial"], "diagnostic initial", 0, 0)
    integer(diagnostic["counter_delta"], "diagnostic counter_delta", 1, 127)
    if not isinstance(config["completions"], dict) or not config["completions"]:
        raise RouteError("completion modes must be a nonempty table")
    for name, completion in config["completions"].items():
        table(completion, f"completion {name}", {"diagnostic", "minimum_ticks", "minimum_advance"})
        if type(completion["diagnostic"]) is not bool:
            raise RouteError("completion diagnostic must be boolean")
        integer(completion["minimum_ticks"], "completion minimum_ticks", 1)
        integer(completion["minimum_advance"], "completion minimum_advance", 1)
    options = base.get("core_options")
    if (not isinstance(options, dict) or not options
            or any(not isinstance(key, str) or not isinstance(value, str) or not key or not value
                   for key, value in options.items())):
        raise RouteError("core options must be nonempty string pairs")


class Traversal:
    """One ordered phase cursor; physical observations are never rewritten."""

    def __init__(self, config, base, mode):
        if mode not in config["completions"]:
            raise RouteError(f"unknown traversal mode {mode}")
        self.config, self.base = config, base
        self.spec = config["completions"][mode]
        self.boot = Route(base, config["base"]["route"])
        self.index = -1
        self.ticks = self.lag = self.max_lag = 0
        self.previous_tick = None
        self.initial = None
        self.stage_frame = self.room_frame = self.room_tick = None
        self.events, self.resets = [], []
        self.recent = deque(maxlen=3)
        self.hit = self.anchor = self.endpoint = None
        self.previous_diagnostic = config["diagnostic"]["initial"]
        self.diagnostic_total = self.rejected_windows = 0
        self.last_events = []
        self.reset_delta = self.diagnostic_delta = 0

    @property
    def phase(self):
        return "boot" if self.index < 0 else self.config["phases"][self.index]["name"]

    def buttons(self):
        if self.index < 0:
            return self.boot.buttons()
        spec = self.config["inputs"]
        if self.ticks < spec["idle_ticks"]:
            return set()
        names = set(spec["held"])
        if (self.ticks >= spec["pulse_start"]
                and (self.ticks - spec["pulse_start"]) % spec["pulse_period"] < spec["pulse_duration"]):
            names.update(spec["pulse"])
        return {BUTTONS[name] for name in names}

    def event(self, kind, frame, state, **extra):
        value = {"kind": kind, "physical_frame": frame, "route_tick": self.ticks,
                 "phase": self.phase, "state": dict(state), **extra}
        self.events.append(value)
        self.last_events.append(kind)
        return value

    def update(self, frame, state):
        self.last_events = []
        self.reset_delta = self.diagnostic_delta = 0
        fields = self.config["fields"]
        if state[fields["trap"]]:
            raise RouteError(f"trap {state[fields['trap']]:#04x} at physical frame {frame}")
        matches = [index for index, phase in enumerate(self.config["phases"])
                   if all(state[key] in values for key, values in phase["allowed"].items())]
        if self.index < 0:
            self.boot.update(frame, state)
            if self.boot.phase != "stage":
                self.check_diagnostic(frame, state)
                return False
            if matches != [0]:
                raise RouteError("initial gameplay did not match the first traversal phase")
            for key, value in self.config["initial_expected"].items():
                if state[key] != value:
                    raise RouteError(f"initial {key}: {state[key]}, expected {value}")
            self.index = 0
            self.stage_frame, self.initial = frame, dict(state)
            self.previous_tick = state[fields["tick"]]
            self.event("phase", frame, state)
        else:
            if len(matches) != 1 or matches[0] not in (self.index, self.index + 1):
                raise RouteError(f"invalid ordered transition from {self.phase}: {matches}, state={state}")
            changed = matches[0] != self.index
            self.index = matches[0]
            current = state[fields["tick"]]
            delta = (current - self.previous_tick) & 255
            reset = self.config["reset"]
            if (delta > self.config["limits"]["counter_delta"]
                    and self.phase == reset["phase"]
                    and current == reset["destination"]
                    and all(state[key] == value for key, value in reset["expected"].items())
                    and len(self.resets) < reset["count"]):
                self.reset_delta = delta
                self.resets.append(self.event("tick_reset", frame, state,
                                              previous=self.previous_tick, current=current))
                delta = 0
            else:
                delta = counter_delta(self.previous_tick, current, self.config["limits"]["counter_delta"])
            self.previous_tick = current
            self.ticks += delta
            self.lag = 0 if delta else self.lag + 1
            self.max_lag = max(self.max_lag, self.lag)
            if self.lag >= self.config["limits"]["stall_frames"]:
                raise RouteError(f"gameplay tick stalled for {self.lag} physical frames")
            if changed:
                for key, minimum in self.config["phases"][self.index].get("minimum_advance", {}).items():
                    if state[key] - self.initial[key] < minimum:
                        raise RouteError(f"entered {self.phase} without required {key} movement")
                self.event("phase", frame, state)
                if self.index == len(self.config["phases"]) - 1:
                    self.room_frame, self.room_tick = frame, self.ticks
        self.check_diagnostic(frame, state)
        self.recent.append({"physical_frame": frame, "route_tick": self.ticks,
                            "phase": self.phase, "state": dict(state)})
        return self.check_completion(frame, state)

    def check_diagnostic(self, frame, state):
        if not self.spec["diagnostic"]:
            return
        spec = self.config["diagnostic"]
        value = state[spec["field"]]
        delta = counter_delta(self.previous_diagnostic, value, spec["counter_delta"])
        if self.index != len(self.config["phases"]) - 1 and value != spec["initial"]:
            raise RouteError("diagnostic success observed outside the final traversal phase")
        self.previous_diagnostic = value
        self.diagnostic_delta = delta
        self.diagnostic_total += delta
        if delta and self.hit is None:
            self.hit = self.event("diagnostic_hit", frame, state, count=value)

    def confirmed_window(self, earliest):
        if len(self.recent) != 3:
            return None
        rows = list(self.recent)
        if any(row["phase"] != self.config["phases"][-1]["name"]
               or row["physical_frame"] < earliest for row in rows):
            return None
        spec, fields = self.config["confirmation"], self.config["fields"]
        for field in (fields["world"], fields["camera"]):
            if any(abs(a["state"][field] - b["state"][field]) > spec["neighbor_delta"]
                   for a, b in zip(rows, rows[1:])):
                self.rejected_windows += 1
                return None
        if any(abs(row["state"][fields["world"]] - row["state"][fields["camera"]]
                   - row["state"][fields["screen"]]) > spec["position_tolerance"] for row in rows):
            self.rejected_windows += 1
            return None
        return rows

    def check_completion(self, frame, state):
        if self.room_frame is None or (self.spec["diagnostic"] and self.hit is None):
            return False
        earliest = self.hit["physical_frame"] if self.spec["diagnostic"] else self.room_frame
        rows = self.confirmed_window(earliest)
        if rows is None:
            return False
        if self.anchor is None:
            self.anchor = rows
            self.event("confirmed_anchor", frame, state, samples=rows)
            return False
        start_tick = self.anchor[1]["route_tick"] if self.spec["diagnostic"] else self.room_tick
        world = self.config["fields"]["world"]
        advance = min(row["state"][world] for row in rows) - max(row["state"][world] for row in self.anchor)
        if (rows[0]["route_tick"] - start_tick < self.spec["minimum_ticks"]
                or advance < self.spec["minimum_advance"]):
            return False
        if len(self.resets) != self.config["reset"]["count"]:
            raise RouteError("completion did not observe the required canonical counter reset")
        self.endpoint = rows
        self.event("confirmed_endpoint", frame, state, samples=rows, minimum_advance=advance)
        return True

    def metrics(self):
        return {"phase": self.phase, "game_ticks": self.ticks, "stage_frame": self.stage_frame,
                "room_frame": self.room_frame, "room_tick": self.room_tick,
                "max_lag_frames": self.max_lag, "tick_resets": self.resets, "events": self.events,
                "escape_exercised": bool(self.spec["diagnostic"] and self.hit),
                "diagnostic_observation": self.hit, "diagnostic_total": self.diagnostic_total,
                "diagnostic_coverage": "observed" if self.hit else "not_observed",
                "confirmed_anchor": self.anchor, "confirmed_endpoint": self.endpoint,
                "rejected_coordinate_windows": self.rejected_windows}


def video_digest(pixels):
    raw, width, height, pitch, fmt = pixels
    if fmt not in (0, 1, 2):
        raise RouteError(f"unsupported pixel format {fmt}")
    size = 4 if fmt == 1 else 2
    if width < 1 or height < 1 or pitch < width * size or len(raw) < height * pitch:
        raise RouteError("invalid video dimensions/pitch/data length")
    return hashlib.sha256(b"".join(raw[y * pitch:y * pitch + width * size]
                                   for y in range(height))).hexdigest()


def run(args):
    config, base, base_path = load_profile(args.profile)
    route = Traversal(config, base, args.mode)
    stride = config["limits"]["capture_every"] if args.capture_every is None else args.capture_every
    integer(stride, "capture_every", 1)
    requested = dict(base["core_options"])
    observations = dict(base["ram"])
    if route.spec["diagnostic"]:
        observations[config["diagnostic"]["field"]] = config["diagnostic"]["offset"]
    words = base.get("ram_words", {})
    args.output.mkdir(parents=True, exist_ok=False)
    summary = {"status": "failed", "mode": args.mode, "scope": "ordered traversal; not a full-stage clear",
               "requested_options": requested, "max_physical_frames": config["limits"]["physical_frames"],
               "capture_every": stride, "completion_spec": route.spec}
    core, error, frame, state = None, None, -1, {}
    try:
        summary.update(core_sha256=sha256(args.core), rom_sha256=sha256(args.rom),
                       profile_sha256=sha256(args.profile), base_profile_sha256=sha256(base_path),
                       runner_sha256=sha256(Path(__file__)), base_runner_sha256=sha256(Path(core_route.__file__)))
        core = Core(args.core, args.rom, args.output, requested)
        core.start(requested, config["ram_bytes"])
        fps = core.av.timing.fps
        if not math.isfinite(fps) or fps <= 0:
            raise RouteError(f"invalid core frame rate {fps}")
        summary.update(core=core.info.name.decode(), core_version=core.info.version.decode(), core_fps=fps)
        with (args.output / "frames.csv").open("w", newline="") as log:
            writer = csv.DictWriter(log, fieldnames=["physical_frame", "route_tick", "phase", "buttons",
                                                    "pixel_sha256", "video_calls", "events", "reset_delta",
                                                    "diagnostic_delta", *observations, *words])
            writer.writeheader()
            complete = False
            for frame in range(config["limits"]["physical_frames"]):
                core.buttons = route.buttons()
                before = core.video_calls
                core.library.retro_run()
                if core.error or core.video_calls == before or core.pixels is None:
                    raise RouteError(core.error or "physical frame had no core video callback/image")
                state = {key: core.ram[offset] for key, offset in observations.items()}
                state.update({key: core.ram[offset] | (core.ram[offset + 1] << 8) for key, offset in words.items()})
                digest = video_digest(core.pixels)
                update_error = None
                try:
                    complete = route.update(frame, state)
                except RouteError as caught:
                    update_error = caught
                writer.writerow({"physical_frame": frame, "route_tick": route.ticks, "phase": route.phase,
                                 "buttons": "+".join(name for name, value in BUTTONS.items() if value in core.buttons),
                                 "pixel_sha256": digest, "video_calls": core.video_calls - before,
                                 "events": "+".join(route.last_events), "reset_delta": route.reset_delta,
                                 "diagnostic_delta": route.diagnostic_delta, **state})
                if route.last_events or complete or update_error or frame % stride == 0:
                    _, width, height, _, _ = core.pixels
                    rgb = pixel_rgb(*core.pixels)
                    name = f"frame-{frame:06d}-tick-{route.ticks:05d}.ppm"
                    (args.output / name).write_bytes(f"P6\n{width} {height}\n255\n".encode() + rgb)
                if route.last_events:
                    print(json.dumps({"frame": frame, "tick": route.ticks, "phase": route.phase,
                                      "events": route.last_events, "state": state}), flush=True)
                if update_error:
                    raise update_error
                if complete:
                    break
            if not complete:
                raise RouteError("physical-frame limit reached before traversal completion")
        core.check_options(requested)
        summary["status"] = "passed"
    except Exception as caught:
        error = caught
    finally:
        if core is not None:
            summary.update(options_queried=core.queried, options_advertised=core.advertised,
                           video_callbacks=core.video_calls, pixel_format=core.pixel_format)
            try:
                core.close()
            except Exception as caught:
                error = error or caught
        if error:
            summary.update(status="failed", error=str(error))
        summary.update(last_frame=frame, final_state=state, **route.metrics())
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(json.dumps({key: value for key, value in summary.items()
                          if key not in ("options_advertised", "options_queried", "events")}), flush=True)
    if error:
        raise RouteError(str(error)) from error
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("profile", type=Path)
    parser.add_argument("mode")
    parser.add_argument("output", type=Path, help="new artifact directory; never overwritten")
    parser.add_argument("--capture-every", type=int, help="physical callback capture stride")
    args = parser.parse_args()
    try:
        run(args)
    except (RouteError, OSError, KeyError, ValueError) as error:
        parser.exit(1, f"core traversal failed: {error}\n")


if __name__ == "__main__":
    main()
