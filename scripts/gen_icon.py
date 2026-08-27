#!/usr/bin/env python3
"""生成应用图标源图（1024x1024 RGBA PNG，蓝色渐变圆）。纯标准库实现。"""
import struct
import zlib

SIZE = 1024


def chunk(tag: bytes, data: bytes) -> bytes:
    c = struct.pack(">I", len(data)) + tag + data
    return c + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def main() -> None:
    rows = []
    cx = cy = SIZE / 2
    max_d = SIZE / 2
    for y in range(SIZE):
        row = bytearray([0])  # filter: None
        for x in range(SIZE):
            dx, dy = x - cx, y - cy
            d = (dx * dx + dy * dy) ** 0.5 / max_d
            d = min(d, 1.0)
            # 蓝 → 紫 径向渐变
            r = int(32 + 78 * d)
            g = int(122 + 66 * d)
            b = int(238 - 70 * d)
            # 圆形软边缘
            if d < 0.9:
                a = 255
            else:
                a = int(255 * max(0.0, (1.0 - d) / 0.1))
            row += bytes((r, g, b, a))
        rows.append(bytes(row))
    raw = b"".join(rows)
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open("apps/desktop/src-tauri/icons/source.png", "wb") as f:
        f.write(png)
    print("written apps/desktop/src-tauri/icons/source.png")


if __name__ == "__main__":
    main()
