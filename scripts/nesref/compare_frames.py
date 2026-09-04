#!/usr/bin/env python3
"""Cell-level visual diff: NES reference PPM (frame-diff FD_NES_DUMP) vs SMS
trace-sms checkpoint PPM.

Both renders are exact palette output, so after quantizing the NES colors to
the SMS 2-bit-per-channel space, matching cells should differ only by
quantization. Reports 8x8 cells whose mean quantized distance is large.

Usage: compare_frames.py <ref.ppm> <sms.ppm> [--label NAME]
"""
import sys


def load_ppm(path):
    d = open(path, "rb").read()
    assert d[:2] == b"P6"
    # header: P6\n<w> <h>\n255\n  (frame-diff writes "P6\nW H\n255\n")
    parts = d.split(b"\n", 3)
    w, h = map(int, parts[1].split())
    px = parts[3][: w * h * 3]
    return w, h, px


def q2(px):
    return bytes(round(v * 3 / 255) * 85 for v in px)


def main():
    ref_path, sms_path = sys.argv[1], sys.argv[2]
    label = sys.argv[4] if len(sys.argv) > 4 else sms_path
    rw, rh, ref = load_ppm(ref_path)
    sw, sh, sms = load_ppm(sms_path)
    assert rw == sw == 256
    ref = q2(ref)

    def row(buf, w, y):
        return buf[y * w * 3 : (y + 1) * w * 3]

    # Vertical alignment: NES renders 240 rows, the SMS build shows 224.
    # Find the ref row offset minimizing total distance.
    best_off, best_score = 0, None
    for off in range(0, rh - sh + 1):
        s = 0
        for y in range(0, sh, 8):  # sparse probe
            r = row(ref, rw, y + off)
            m = row(sms, sw, y)
            s += sum(abs(a - b) for a, b in zip(r, m))
        if best_score is None or s < best_score:
            best_off, best_score = off, s

    bad = []
    for cy in range(sh // 8):
        for cx in range(32):
            dist = 0
            for py in range(8):
                ry = cy * 8 + py + best_off
                sy = cy * 8 + py
                ro = (ry * rw + cx * 8) * 3
                so = (sy * sw + cx * 8) * 3
                dist += sum(
                    abs(ref[ro + i] - sms[so + i]) for i in range(24)
                )
            mean = dist / (64 * 3)
            if mean > 24:  # ~more than a quarter-step average per channel
                bad.append((cy, cx, round(mean)))
    print(f"{label}: voffset={best_off} mismatched_cells={len(bad)}")
    for cy, cx, mean in bad[:40]:
        print(f"  cell row={cy} col={cx} screen=({cx*8},{cy*8}) meandist={mean}")
    if len(bad) > 40:
        print(f"  ... and {len(bad)-40} more")


if __name__ == "__main__":
    main()
