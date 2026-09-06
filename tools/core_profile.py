#!/usr/bin/env python3
"""Host-only profiling wrapper around the unchanged, frozen core_route runner.

Requires an explicitly instrumented GPGX core. The profiler observes instructions;
it must never write guest memory, adjust cycle charges, or supply different input.
Its private ABI is deliberately separate from libretro and versioned below.
"""

import argparse
import ctypes as C
import json
from pathlib import Path
import sys

import core_route


ABI_VERSION = 1
COUNTERS = ("instructions", "nominal_master", "scaled_master")


class ProfileError(Exception):
    pass


class WindowConfig(C.Structure):
    # Match the private C ABI: uint32_t fields, no pointers or platform padding.
    _fields_ = [(name, C.c_uint32) for name in (
        "version", "tick_offset", "initial_tick", "start_tick", "end_tick",
        "predicate_count")]
    _fields_ += [("offsets", C.c_uint32 * 8), ("values", C.c_uint32 * 8)]


def window_config(profile, initial_tick, start, end):
    """Addresses and stage predicates come from the external route profile."""
    if (any(not isinstance(value, int) or isinstance(value, bool) for value in (initial_tick, start, end))
            or not 0 <= initial_tick <= 255 or not 0 < start < end <= 0xFFFFFFFF):
        raise ProfileError("window requires a byte tick and 0 < start < end")
    predicates = [(profile["ram"][name], value) for name, value in
                  zip(("state", "substate"), profile["boot"]["stage_state"], strict=True)]
    tick_offset = profile["ram"]["game_tick"]
    if any(not isinstance(offset, int) or isinstance(offset, bool) or not 0 <= offset < 8192
           for offset in [tick_offset, *(offset for offset, _ in predicates)]):
        raise ProfileError("profiler offsets must address SMS SYSTEM_RAM")
    if any(not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 255
           for _, value in predicates):
        raise ProfileError("stage predicates must be bytes")
    config = WindowConfig(ABI_VERSION, tick_offset, initial_tick, start, end, len(predicates))
    for index, (offset, value) in enumerate(predicates):
        config.offsets[index], config.values[index] = offset, value
    return config


def checked_counters(row):
    result = {}
    for key in COUNTERS:
        value = row.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise ProfileError(f"invalid {key} counter")
        result[key] = value
    if result["nominal_master"] % 15:
        raise ProfileError("nominal master costs must be whole Z80 T-states")
    return result


def validate_report(report, start, end):
    """Conservation is necessary, not proof that the core hooks are correct.

Core-side opcode/timing/interrupt tests and exact normal-core callback parity
remain separate mandatory gates. Unknown execution is retained and reported.
"""
    if report.get("schema") != ABI_VERSION:
        raise ProfileError("unsupported profiler report schema")
    window = report.get("window", {})
    if (window.get("start_tick"), window.get("end_tick"), window.get("observed_tick"),
            window.get("state")) != (start, end, end, "complete"):
        raise ProfileError("profile window is incomplete or does not match the request")
    if report.get("errors") != []:
        raise ProfileError(f"core accounting errors: {report.get('errors')!r}")
    totals = checked_counters(report["totals"])
    if not totals["instructions"] or not totals["nominal_master"]:
        raise ProfileError("empty profile window")
    sums = dict.fromkeys(COUNTERS, 0)
    unknown = dict.fromkeys(COUNTERS, 0)
    identities = set()
    for row in report["costs"]:
        identity = (row["source"], row["rom_offset"], row["pc"], row["irq_kind"], row["irq_depth"], row["kind"])
        if identity in identities:
            raise ProfileError("duplicate cost identity")
        identities.add(identity)
        if row["source"] not in ("rom", "ram", "unknown"):
            raise ProfileError("invalid instruction source")
        if row["irq_kind"] not in ("none", "irq", "nmi"):
            raise ProfileError("invalid interrupt kind")
        if row["kind"] not in ("instruction", "halt", "irq_entry", "nmi_entry", "external"):
            raise ProfileError("invalid cost kind")
        if row["source"] == "rom" and (not isinstance(row["rom_offset"], int)
                                       or isinstance(row["rom_offset"], bool) or row["rom_offset"] < 0):
            raise ProfileError("ROM execution requires a pre-fetch byte offset")
        if row["source"] != "rom" and row["rom_offset"] is not None:
            raise ProfileError("non-ROM execution cannot claim a ROM offset")
        if (not isinstance(row["pc"], int) or isinstance(row["pc"], bool) or not 0 <= row["pc"] <= 65535
                or not isinstance(row["irq_depth"], int) or isinstance(row["irq_depth"], bool) or row["irq_depth"] < 0
                or (row["irq_kind"] == "none") != (row["irq_depth"] == 0)):
            raise ProfileError("invalid PC or interrupt context")
        costs = checked_counters(row)
        for key, value in costs.items():
            sums[key] += value
            if row["source"] == "unknown" or row["kind"] == "external":
                unknown[key] += value
    if sums != totals:
        raise ProfileError("cost rows do not conserve totals")
    ticks = report["ticks"]
    if [row["tick"] for row in ticks] != list(range(start, end)):
        raise ProfileError("missing, duplicated or unordered complete tick intervals")
    tick_sums = dict.fromkeys(COUNTERS, 0)
    for row in ticks:
        for key, value in checked_counters(row).items():
            tick_sums[key] += value
    if tick_sums != totals:
        raise ProfileError("complete tick intervals do not conserve totals")
    return {"complete_intervals": end - start, "nominal_tstates": totals["nominal_master"] // 15,
            "scaled_master": totals["scaled_master"], "unknown_or_external": unknown}


class NativeProfiler:
    def __init__(self, core):
        self.core = core
        try:
            version = core.fn("retro_profile_version", C.c_uint32, [])()
            self.arm_fn = core.fn("retro_profile_arm", C.c_int,
                                 [C.POINTER(WindowConfig), C.c_size_t])
            self.dump_fn = core.fn("retro_profile_dump", C.c_int, [C.c_char_p])
        except AttributeError as error:
            raise ProfileError("core does not provide the observational profiler ABI") from error
        if version != ABI_VERSION:
            raise ProfileError(f"unsupported profiler ABI {version}")

    def arm(self, config):
        if self.arm_fn(C.byref(config), C.sizeof(config)) != 0:
            raise ProfileError("core refused the profiling window")

    def dump(self, path):
        if self.dump_fn(str(path.resolve()).encode()) != 0:
            raise ProfileError("core could not write its profiler report")


def run_profile(args, backend_factory=NativeProfiler):
    """Reuse frozen route behavior; replacements are restored even on failure."""
    if not 0 < args.start_tick < args.end_tick:
        raise ProfileError("window requires 0 < start < end")
    # Reject prior evidence before installing wrappers or entering the cleanup
    # path, which records provenance for newly created failed runs too.
    if args.output.exists() or args.output.is_symlink():
        raise ProfileError(f"output already exists: {args.output}")
    original_core, original_route = core_route.Core, core_route.Route
    context = {"core": None, "backend": None, "armed": False, "dump_error": None}
    report_path = args.output / "profile.json"

    class ProfiledCore(original_core):
        def start(self, requested, minimum_ram):
            super().start(requested, minimum_ram)
            context["core"] = self
            context["backend"] = backend_factory(self)

        def close(self):
            try:
                if context["backend"] is not None:
                    context["backend"].dump(report_path)
            except Exception as error:
                context["dump_error"] = str(error)
            finally:
                super().close()

    class ProfiledRoute(original_route):
        def __init__(self, config, name):
            super().__init__(config, name)
            if args.end_tick > self.spec["ticks"]:
                raise ProfileError("profile window extends beyond the frozen route")

        def update(self, frame, state):
            complete = super().update(frame, state)
            if self.phase == "stage" and not context["armed"]:
                if self.ticks != 0:
                    raise ProfileError("missed initial route tick anchor")
                context["backend"].arm(window_config(
                    self.config, self.previous_tick, args.start_tick, args.end_tick))
                context["armed"] = True
            return complete

    core_route.Core, core_route.Route = ProfiledCore, ProfiledRoute
    try:
        core_route.run(args)
    finally:
        core_route.Core, core_route.Route = original_core, original_route
        if args.output.exists():
            provenance = {"wrapper_sha256": core_route.sha256(Path(__file__)), "abi": ABI_VERSION,
                          "start_tick": args.start_tick, "end_tick": args.end_tick,
                          "armed": context["armed"], "dump_error": context["dump_error"]}
            (args.output / "profile-wrapper.json").write_text(json.dumps(provenance, indent=2) + "\n")
    if context["dump_error"]:
        raise ProfileError(context["dump_error"])
    result = validate_report(json.loads(report_path.read_text()), args.start_tick, args.end_tick)
    print(json.dumps(result, indent=2))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("core", type=Path)
    parser.add_argument("rom", type=Path)
    parser.add_argument("profile", type=Path)
    parser.add_argument("route")
    parser.add_argument("output", type=Path)
    parser.add_argument("--overclock")
    parser.add_argument("--compare", type=Path)
    parser.add_argument("--frames", type=int, default=12000)
    parser.add_argument("--capture-every", type=int, default=10)
    parser.add_argument("--start-tick", type=int, default=60)
    parser.add_argument("--end-tick", type=int, default=420)
    args = parser.parse_args()
    try:
        run_profile(args)
    except (ProfileError, core_route.RouteError) as error:
        print(f"profiling failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
