#!/usr/bin/env python3
"""窗口抓图 → README 用 JPEG：压平圆角外的透明像素 + 质量压缩。

为什么不用 sips 直接转：窗口是透明 + CSS 圆角的，圆角外的像素 alpha=0，
sips 会把它们压成**白色** —— 深色玻璃 UI 上一眼就是四个白角。这里改成
压到「玻璃边缘色」（取左缘中点像素），圆角与窗口自身边缘自然衔接。

用法：
    pipeline.py <src.png> <dst.jpg> [质量=88]
默认短边超过 1602 时按比例缩到 1602（2x 抓拍的 1240x801 窗口正好不缩）。
"""
import os
import sys

from PIL import Image


def convert(src: str, dst: str, quality: int = 88, max_short: int = 1602) -> tuple[tuple[int, int], tuple[int, int, int]]:
    im = Image.open(src).convert("RGBA")
    w, h = im.size
    if max_short and min(w, h) > max_short:
        scale = max_short / min(w, h)
        im = im.resize((round(w * scale), round(h * scale)), Image.LANCZOS)
    edge = im.getpixel((2, im.height // 2))[:3]
    bg = Image.new("RGB", im.size, edge)
    bg.paste(im, mask=im.split()[3])
    bg.save(dst, "JPEG", quality=quality, optimize=True, progressive=True)
    return bg.size, edge


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(__doc__)
        raise SystemExit(2)
    q = int(sys.argv[3]) if len(sys.argv) > 3 else 88
    size, edge = convert(sys.argv[1], sys.argv[2], q)
    print(f"{sys.argv[2]}  {size[0]}x{size[1]}  edge=#{edge[0]:02x}{edge[1]:02x}{edge[2]:02x}  "
          f"{os.path.getsize(sys.argv[2]) // 1024}KB")
