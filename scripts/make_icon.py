"""Draws clipr's app icon and writes assets/clipr.ico and assets/clipr.res.

The icon is the tray's lowercase "c" (a ring with an opening on the right that reads as a rewind arrow) in
the accent colour, on a dark rounded square. clipr.res holds the same images as a Windows resource; the
linker puts it into clipr.exe (see crates/clipr/build.rs), so Explorer, the Start menu and shortcuts show it.

Needs only the Python standard library. Run from the repository root: python scripts/make_icon.py
"""

import math
import struct
import zlib
from pathlib import Path

SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]
BG = (0x16, 0x16, 0x1D)
ACCENT = (0x8B, 0x5C, 0xF6)
SAMPLES = 4


def draw(size: int) -> bytes:
    s = float(size)
    c = s / 2.0
    corner = s * 0.22
    outer, inner = s * 0.34, s * 0.20
    rows = []
    for y in range(size):
        row = bytearray([0])  # PNG filter: none
        for x in range(size):
            bg = ring = 0
            for sy in range(SAMPLES):
                for sx in range(SAMPLES):
                    fx = x + (sx + 0.5) / SAMPLES
                    fy = y + (sy + 0.5) / SAMPLES
                    # Rounded square: inside unless in a corner beyond its radius.
                    dx = max(corner - fx, fx - (s - corner), 0.0)
                    dy = max(corner - fy, fy - (s - corner), 0.0)
                    if dx * dx + dy * dy <= corner * corner:
                        bg += 1
                        rx, ry = fx - c, fy - c
                        r = math.hypot(rx, ry)
                        angle = math.degrees(math.atan2(ry, rx))
                        if inner <= r <= outer and abs(angle) >= 35.0:
                            ring += 1
            n = SAMPLES * SAMPLES
            t = ring / bg if bg else 0.0
            colour = [round(BG[i] + (ACCENT[i] - BG[i]) * t) for i in range(3)]
            row += bytes(colour + [round(255 * bg / n)])
        rows.append(bytes(row))
    raw = b"".join(rows)

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images: list[tuple[int, bytes]]) -> bytes:
    out = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    for size, png in images:
        dim = 0 if size >= 256 else size
        out += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    return out + b"".join(png for _, png in images)


def res(images: list[tuple[int, bytes]]) -> bytes:
    def entry(kind: int, name: int, data: bytes, flags: int) -> bytes:
        header = struct.pack("<II", len(data), 32)
        header += struct.pack("<HHHH", 0xFFFF, kind, 0xFFFF, name)
        header += struct.pack("<IHHII", 0, flags, 0x0409, 0, 0)
        pad = b"\0" * (-len(data) % 4)
        return header + data + pad

    # A .res file starts with an empty entry.
    out = struct.pack("<II", 0, 32) + struct.pack("<HHHH", 0xFFFF, 0, 0xFFFF, 0) + struct.pack("<IHHII", 0, 0, 0, 0, 0)
    group = struct.pack("<HHH", 0, 1, len(images))
    for i, (size, png) in enumerate(images, start=1):
        out += entry(3, i, png, 0x1010)  # RT_ICON
        dim = 0 if size >= 256 else size
        group += struct.pack("<BBBBHHIH", dim, dim, 0, 0, 1, 32, len(png), i)
    out += entry(14, 1, group, 0x1030)  # RT_GROUP_ICON, id 1: the exe's icon
    return out


def main() -> None:
    images = [(size, draw(size)) for size in SIZES]
    assets = Path(__file__).resolve().parent.parent / "assets"
    assets.mkdir(exist_ok=True)
    (assets / "clipr.ico").write_bytes(ico(images))
    (assets / "clipr.res").write_bytes(res(images))
    (assets / "clipr-256.png").write_bytes(images[-1][1])
    print("wrote", ", ".join(str(assets / n) for n in ("clipr.ico", "clipr.res", "clipr-256.png")))


if __name__ == "__main__":
    main()
