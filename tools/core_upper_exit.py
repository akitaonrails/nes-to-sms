#!/usr/bin/env python3
"""Input-only upper-exit acceptance from boot, extending the frozen indoor route.

Uses the existing libretro harness; no state loading, guest writes or ROM edits.
Screenshots are observations, not restorable emulator states. Python3.11+ only.
"""

import argparse
import csv
import json
from pathlib import Path
import time
import tomllib

import core_route
import core_traversal
from core_route import BUTTONS, Core, RouteError, counter_delta, pixel_rgb, sha256
from core_traversal import Traversal, integer, table, video_digest


def matches(state, constraints):
    return all(state[name] in value if isinstance(value, list) else
               (value.get("minimum", 0) <= state[name] <= value.get("maximum", 65535)
                if isinstance(value, dict) else state[name] == value)
               for name, value in constraints.items())


def load_profile(path):
    config = tomllib.loads(path.read_text())
    table(config, "upper exit", {"version", "prefix", "observations", "fields", "limits",
                                "alive", "fatal", "phases", "confirmation", "reset"})
    if type(config["version"]) is not int or config["version"] != 1:
        raise RouteError("unsupported upper-exit version")
    table(config["prefix"], "prefix", {"profile", "sha256", "runner_sha256", "mode"})
    prefix = config["prefix"]
    if not isinstance(prefix["profile"], str) or Path(prefix["profile"]).is_absolute():
        raise RouteError("prefix profile must be relative")
    prefix_path = path.parent / prefix["profile"]
    if sha256(prefix_path) != prefix["sha256"]:
        raise RouteError("frozen traversal profile hash mismatch")
    if sha256(Path(core_traversal.__file__)) != prefix["runner_sha256"]:
        raise RouteError("frozen traversal runner hash mismatch")
    traversal, base, base_path = core_traversal.load_profile(prefix_path)
    Traversal(traversal, base, prefix["mode"])
    observations = dict(base["ram"])
    words = base.get("ram_words", {})
    reserved = {"physical_frame", "extra_ticks", "phase", "buttons", "pixel_sha256", "events", "video_calls"}
    for name, offset in config["observations"].items():
        if not isinstance(name, str) or not name or name in observations or name in words or name in reserved:
            raise RouteError("duplicate/reserved observation")
        integer(offset, name, 0, traversal["ram_bytes"] - 1)
        observations[name] = offset
    widths = {**dict.fromkeys(observations, 1), **dict.fromkeys(words, 2)}

    def constraints(values, name):
        if not isinstance(values, dict) or not values:
            raise RouteError(f"{name} requires nonempty observation constraints")
        for field, rule in values.items():
            if field not in widths:
                raise RouteError(f"unknown observation {field}")
            maximum = (1 << (8 * widths[field])) - 1
            if isinstance(rule, dict):
                table(rule, name, set(), {"minimum", "maximum"})
                if not rule:
                    raise RouteError("empty bounds")
                for value in rule.values():
                    integer(value, field, 0, maximum)
                if rule.get("minimum", 0) > rule.get("maximum", maximum):
                    raise RouteError("inverted observation bounds")
            elif isinstance(rule, list):
                if not rule:
                    raise RouteError("empty allowed values")
                for value in rule:
                    integer(value, field, 0, maximum)
            else:
                integer(rule, field, 0, maximum)

    table(config["fields"], "fields", {"tick", "trap", "world", "camera", "screen"})
    for role, name in config["fields"].items():
        if widths.get(name) != (2 if role in ("world", "camera") else 1):
            raise RouteError(f"invalid field role {role}")
    constraints(config["alive"], "alive")
    constraints(config["fatal"], "fatal")
    table(config["limits"], "limits", {"physical_frames", "extra_ticks", "stall_frames", "counter_delta", "capture_every", "wall_seconds"})
    for name, value in config["limits"].items():
        integer(value, name, 1, 127 if name == "counter_delta" else None)
    table(config["confirmation"], "confirmation", {"samples", "position_tolerance"})
    integer(config["confirmation"]["samples"], "samples", 2)
    integer(config["confirmation"]["position_tolerance"], "position tolerance", 0, 127)
    names = set()
    if not isinstance(config["phases"], list) or len(config["phases"]) < 2:
        raise RouteError("ordered phases required")
    for phase in config["phases"]:
        table(phase, "phase", {"name", "held", "allowed", "finish", "minimum_ticks", "maximum_ticks"},
              {"pulse", "pulse_period", "pulse_duration", "seen", "minimum_advance"})
        if not isinstance(phase["name"], str) or not phase["name"] or phase["name"] in names | {"frozen-prefix", "complete"}:
            raise RouteError("duplicate/reserved phase name")
        names.add(phase["name"])
        constraints(phase["allowed"], "allowed")
        constraints(phase["finish"], "finish")
        if "seen" in phase:
            constraints(phase["seen"], "seen")
        integer(phase["maximum_ticks"], "maximum ticks", 1)
        integer(phase["minimum_ticks"], "minimum ticks", 0, phase["maximum_ticks"])
        integer(phase.get("minimum_advance", 0), "minimum advance", 0)
        for key in ("held", "pulse"):
            values = phase.get(key, [])
            if not isinstance(values, list) or any(value not in BUTTONS for value in values) or len(set(values)) != len(values):
                raise RouteError("invalid input buttons")
        if phase.get("pulse"):
            integer(phase.get("pulse_period"), "pulse period", 1)
            integer(phase.get("pulse_duration"), "pulse duration", 1, phase["pulse_period"])
    final = config["phases"][-1]
    if final["minimum_ticks"] < 1 or final.get("minimum_advance", 0) < 1:
        raise RouteError("final playable confirmation requires ticks and movement")
    table(config["reset"], "reset", {"phase", "previous", "current", "destination", "count"})
    reset = config["reset"]
    if reset["phase"] not in names or reset["phase"] == final["name"]:
        raise RouteError("reset must belong to a pre-confirmation phase")
    constraints(reset["previous"], "reset previous")
    constraints(reset["current"], "reset current")
    integer(reset["destination"], "reset destination", 0, 255)
    integer(reset["count"], "reset count", 1, 1)
    return config, traversal, base, observations, prefix_path, base_path


class UpperExit:
    def __init__(self, config, prefix):
        self.config, self.prefix = config, prefix
        self.index = -1
        self.ticks = self.phase_tick = self.lag = self.max_lag = 0
        self.previous = self.anchor = None
        self.previous_state = None
        self.resets = []
        self.seen = False
        self.confirmed = 0
        self.events, self.last_events = [], []

    @property
    def phase(self):
        return "frozen-prefix" if self.index < 0 else ("complete" if self.index == len(self.config["phases"]) else self.config["phases"][self.index]["name"])

    def buttons(self):
        if self.index < 0:
            return self.prefix.buttons()
        if self.phase == "complete":
            return set()
        spec = self.config["phases"][self.index]
        names = set(spec["held"])
        if spec.get("pulse") and (self.ticks - self.phase_tick) % spec["pulse_period"] < spec["pulse_duration"]:
            names.update(spec["pulse"])
        return {BUTTONS[name] for name in names}

    def advance(self, frame, state):
        reset = self.config["reset"]
        if self.phase == reset["phase"] and len(self.resets) != reset["count"]:
            raise RouteError("transition missing its required counter reset")
        self.index += 1
        self.phase_tick = self.ticks
        self.anchor = state[self.config["fields"]["world"]]
        self.seen = False
        self.confirmed = 0
        self.events.append({"frame": frame, "extra_ticks": self.ticks, "phase": self.phase, "state": dict(state)})
        self.last_events = [self.phase]

    def update(self, frame, state):
        self.last_events = []
        fields = self.config["fields"]
        if state[fields["trap"]]:
            raise RouteError(f"trap {state[fields['trap']]:#04x} at frame {frame}")
        if self.index < 0:
            if self.prefix.update(frame, state):
                self.previous = state[fields["tick"]]
                self.previous_state = dict(state)
                self.advance(frame, state)
            return False
        if any(matches(state, {key: rule}) for key, rule in self.config["fatal"].items()) or not matches(state, self.config["alive"]):
            raise RouteError("death/nonliving state after indoor prefix")
        reset = self.config["reset"]
        current = state[fields["tick"]]
        # A natural increment/wrap through the destination is not a reset.
        # Ambiguous small deltas get no exception; the required reset stays
        # fail-closed if this bounded route cannot establish it explicitly.
        reset_seen = (current == reset["destination"] and self.previous != current
                      and ((current - self.previous) & 255) > self.config["limits"]["counter_delta"]
                      and self.phase == reset["phase"]
                      and matches(self.previous_state, reset["previous"])
                      and matches(state, reset["current"]))
        if reset_seen:
            if len(self.resets) >= reset["count"]:
                raise RouteError("duplicate transition counter reset")
            self.resets.append({"frame": frame, "previous": self.previous, "current": current,
                                "extra_ticks": self.ticks, "state": dict(state)})
            self.last_events = ["counter-reset"]
            delta = 0  # A reset is not elapsed game time, including near wrap.
        else:
            delta = counter_delta(self.previous, current, self.config["limits"]["counter_delta"])
        self.previous = state[fields["tick"]]
        self.previous_state = dict(state)
        self.ticks += delta
        self.lag = 0 if delta or reset_seen else self.lag + 1
        self.max_lag = max(self.max_lag, self.lag)
        if self.lag > self.config["limits"]["stall_frames"]:
            raise RouteError("game-update stall")
        if self.ticks > self.config["limits"]["extra_ticks"]:
            raise RouteError("extra game-tick limit")
        spec = self.config["phases"][self.index]
        if not matches(state, spec["allowed"]):
            raise RouteError(f"unexpected state in ordered phase {self.phase}")
        elapsed = self.ticks - self.phase_tick
        if elapsed > spec["maximum_ticks"]:
            raise RouteError(f"phase {self.phase} exceeded game-tick limit")
        self.seen |= "seen" not in spec or matches(state, spec["seen"])
        coherent = abs(state[fields["world"]] - state[fields["camera"]] - state[fields["screen"]]) <= self.config["confirmation"]["position_tolerance"]
        qualifies = (self.seen and coherent and elapsed >= spec["minimum_ticks"]
                     and matches(state, spec["finish"])
                     and (not spec.get("minimum_advance") or state[fields["world"]] - self.anchor >= spec["minimum_advance"]))
        # Preserve reconnaissance input switching at its exact observed tick;
        # only final acceptance requires repeated coherent callback samples.
        if qualifies:
            # Repeated callbacks of one stopped game tick cannot confirm play.
            self.confirmed += int(delta > 0)
            if self.index < len(self.config["phases"]) - 1 or self.confirmed >= self.config["confirmation"]["samples"]:
                self.advance(frame, state)
        else:
            self.confirmed = 0
        return self.phase == "complete"


def run(args):
    if args.output.exists() or args.output.is_symlink():
        raise RouteError("output already exists; refusing to overwrite provenance")
    args.output.mkdir(parents=True, exist_ok=False)
    summary = {"status": "failed", "scope": "from-boot upper-exit and next-room play; not a stage/boss clear",
               "state_capture": "screenshots only; no restorable states or state loading"}
    core = route = error = None
    frame, state = -1, {}
    started = time.monotonic()
    try:
        config, traversal, base, observations, prefix_path, base_path = load_profile(args.profile)
        route = UpperExit(config, Traversal(traversal, base, config["prefix"]["mode"]))
        requested = dict(base["core_options"])
        summary.update(core_sha256=sha256(args.core), rom_sha256=sha256(args.rom), profile_sha256=sha256(args.profile),
                       runner_sha256=sha256(Path(__file__)), prefix_profile_sha256=sha256(prefix_path),
                       prefix_runner_sha256=sha256(Path(core_traversal.__file__)), base_profile_sha256=sha256(base_path),
                       base_runner_sha256=sha256(Path(core_route.__file__)), requested_options=requested)
        core = Core(args.core, args.rom, args.output, requested)
        core.start(requested, traversal["ram_bytes"])
        words = base.get("ram_words", {})
        with (args.output / "frames.csv").open("w", newline="") as log:
            writer = csv.DictWriter(log, fieldnames=["physical_frame", "extra_ticks", "phase", "buttons", "pixel_sha256", "video_calls", "events", *observations, *words])
            writer.writeheader()
            complete = False
            for frame in range(config["limits"]["physical_frames"]):
                if time.monotonic() - started > config["limits"]["wall_seconds"]:
                    raise RouteError("wall-time limit")
                core.buttons = route.buttons()
                before = core.video_calls
                core.library.retro_run()
                state = {key: core.ram[offset] for key, offset in observations.items()}
                state.update({key: core.ram[offset] | core.ram[offset + 1] << 8 for key, offset in words.items()})
                if core.error or core.video_calls <= before or core.pixels is None:
                    raise RouteError(core.error or "missing video callback/image")
                update_error = None
                try:
                    complete = route.update(frame, state)
                except RouteError as caught:
                    update_error = caught
                writer.writerow({"physical_frame": frame, "extra_ticks": route.ticks, "phase": route.phase,
                                 "buttons": "+".join(name for name, value in BUTTONS.items() if value in core.buttons),
                                 "pixel_sha256": video_digest(core.pixels), "video_calls": core.video_calls - before,
                                 "events": "+".join(route.last_events), **state})
                if route.last_events or complete or update_error or frame % config["limits"]["capture_every"] == 0:
                    _, width, height, _, _ = core.pixels
                    (args.output / f"{frame:06d}-{route.phase}.ppm").write_bytes(f"P6\n{width} {height}\n255\n".encode() + pixel_rgb(*core.pixels))
                if route.last_events:
                    event = route.resets[-1] if route.last_events == ["counter-reset"] else route.events[-1]
                    print(json.dumps(event), flush=True)
                if update_error:
                    raise update_error
                if complete:
                    break
            if not complete:
                raise RouteError("physical-frame limit before playable next room")
        core.check_options(requested)
        summary["status"] = "passed"
    except Exception as caught:
        error = caught
    finally:
        if core is not None:
            summary.update(options_queried=core.queried, options_advertised=core.advertised, video_callbacks=core.video_calls)
            try:
                core.check_options(requested)
            except Exception as caught:
                error = error or caught
            try:
                core.close()
            except Exception as caught:
                error = error or caught
        if error:
            summary.update(status="failed", error=str(error))
        summary.update(last_frame=frame, final_state=state, elapsed_seconds=time.monotonic() - started)
        if route is not None:
            summary.update(phase=route.phase, extra_ticks=route.ticks, max_lag_frames=route.max_lag, events=route.events, tick_resets=route.resets, prefix=route.prefix.metrics())
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    if error:
        raise RouteError(str(error)) from error
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("core", "rom", "profile", "output"):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    try:
        run(args)
    except (RouteError, OSError, ValueError) as error:
        parser.exit(1, f"upper-exit acceptance failed: {error}\n")


if __name__ == "__main__":
    main()
