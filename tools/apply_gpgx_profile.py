#!/usr/bin/env python3
"""Apply small observational hooks to an isolated, pinned GPGX build copy.

Do not use the read-only dependency checkout as destination. This is a mechanical
source patcher, not a downloader or build/dependency installation script.
"""
import argparse
import hashlib
from pathlib import Path
import subprocess

Z80_SHA256 = "482cb73d3f68337980d7d58a837ffb8b02bf646f565886687c742d7cd2df83b6"
SOURCE_PIN = "a7985a9c4278ac352f8ca7bb4d3cc6b36e9e3e7d"

def patch_source(source):
    edits = [
        ("#ifdef Z80_OVERCLOCK_SHIFT\n#define USE_CYCLES(A)",
         "/* Modified for nes-to-sms host-only profiling; original license retained. */\n"
         "static void np_charge(UINT32 nominal, UINT32 scaled);\n"
         "#ifdef Z80_OVERCLOCK_SHIFT\n#define USE_CYCLES(A)"),
        ("#define USE_CYCLES(A) Z80.cycles += ((A) * z80_cycle_ratio) >> Z80_OVERCLOCK_SHIFT",
         "#define USE_CYCLES(A) do { UINT32 np_n = (A); UINT32 np_s = (np_n * z80_cycle_ratio) >> Z80_OVERCLOCK_SHIFT; np_charge(np_n, np_s); Z80.cycles += np_s; } while (0)"),
        ("#define USE_CYCLES(A) Z80.cycles += (A)",
         "#define USE_CYCLES(A) do { UINT32 np_n = (A); np_charge(np_n, np_n); Z80.cycles += np_n; } while (0)"),
        ("static UINT32 EA;", 'static UINT32 EA;\n#include "nes_to_sms_profile.inc"'),
        ("    POP(pc);", "    POP(pc); np_return(0);"),
        ("  POP( pc ); \\\n  WZ = PC; \\\n  IFF1 = IFF2;", "  POP( pc ); np_return(2); \\\n  WZ = PC; \\\n  IFF1 = IFF2;"),
        ("  POP( pc ); \\\n  WZ = PC; \\\n/* according", "  POP( pc ); np_return(1); \\\n  WZ = PC; \\\n/* according"),
        ("OP(op,c9) { POP( pc );", "OP(op,c9) { POP( pc ); np_return(0);"),
        ("void z80_reset(void)\n{", "void z80_reset(void)\n{\n  np_reset();"),
        ("      take_interrupt();", "      np_enter(1); np_begin(2);\n      take_interrupt();\n      np_end();"),
        ("    EXEC_INLINE(op,ROP());", "    np_begin(HALT ? 1 : 0);\n    EXEC_INLINE(op,ROP());\n    np_end();"),
        ("    LOG((\"Z80 #%d take NMI\\n\", cpu_getactivecpu()));",
         "    np_enter(2); np_begin(3);\n    LOG((\"Z80 #%d take NMI\\n\", cpu_getactivecpu()));"),
        ("    USE_CYCLES(11*15);", "    USE_CYCLES(11*15);\n    np_end();"),
    ]
    for old, new in edits:
        if source.count(old) != 1:
            raise ValueError(f"expected one pinned patch site: {old!r}")
        source = source.replace(old, new)
    return source


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("build_copy", type=Path)
    args = parser.parse_args()
    root = args.build_copy.resolve()
    if "out" not in root.parts or ".slim" in root.parts:
        parser.error("destination must be an isolated out/ build copy")
    head = subprocess.check_output(["git", "-C", str(root), "rev-parse", "HEAD"], text=True).strip()
    if head != SOURCE_PIN:
        parser.error(f"source revision mismatch: {head}")
    if subprocess.check_output(["git", "-C", str(root), "status", "--porcelain", "--untracked-files=no"], text=True):
        parser.error("build copy has existing tracked source modifications")
    path = root / "core/z80/z80.c"
    raw = path.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    if digest != Z80_SHA256:
        parser.error(f"CPU source hash mismatch: {digest}, expected {Z80_SHA256}")
    print(f"input_z80_sha256={digest}")
    source = patch_source(raw.decode().replace("\r\n", "\n"))
    path.write_text(source)
    (path.parent / "nes_to_sms_profile.inc").write_bytes(Path(__file__).with_name("gpgx_profile.inc").read_bytes())


if __name__ == "__main__":
    main()
