#!/usr/bin/env python3
"""Convert FCEUX gui.gdscreenshot() truecolor gd dumps to PNG.

gd truecolor string layout: u16 magic (0xFFFE), u16 width, u16 height,
u8 truecolor flag, s32 transparent color, then per pixel a big-endian
u32 with bits 24-30 alpha (0 = opaque, 127 = transparent) and RGB below.

Usage: gd2png.py <file.gd> [more.gd ...]  — writes sibling .png at 2x nearest.
"""
import struct
import sys

from PIL import Image


def convert(path):
    data = open(path, "rb").read()
    magic, width, height = struct.unpack(">HHH", data[:6])
    assert magic in (0xFFFE, 0xFFFF), f"{path}: not a gd dump (magic {magic:#x})"
    px_off = 11  # 6 header + 1 truecolor flag + 4 transparent
    img = Image.new("RGB", (width, height))
    put = img.putdata
    pixels = []
    for i in range(width * height):
        (argb,) = struct.unpack_from(">I", data, px_off + 4 * i)
        pixels.append(((argb >> 16) & 0xFF, (argb >> 8) & 0xFF, argb & 0xFF))
    put(pixels)
    out = path.rsplit(".", 1)[0] + ".png"
    img.resize((width * 2, height * 2), Image.NEAREST).save(out)
    print(f"{out}: {width}x{height}")


if __name__ == "__main__":
    for p in sys.argv[1:]:
        convert(p)
