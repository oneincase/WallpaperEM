#!/usr/bin/env python3
"""抓图体检：尺寸 / 是否空白 / 平均亮度，可选输出缩略图便于目检。

为什么需要：`screencapture -l <id>` 对**最小化或位于其它 Space** 的窗口不报错，
而是安静地给一张空窗图（整片深色 + 圆角）。所以每抓一张就量一下统计量，
std 接近 0 就说明这页没抓到，得把窗口唤到当前 Space 再重来。

用法：
    verify.py <png> [缩略图输出路径]
退出码：0 正常；1 疑似空白（std < 3）。
"""
import sys

from PIL import Image, ImageStat


def check(path: str, thumb: str | None = None) -> bool:
    im = Image.open(path)
    has_alpha = im.mode in ("RGBA", "LA")
    rgb = im.convert("RGB")
    st = ImageStat.Stat(rgb)
    mean = [round(v) for v in st.mean]
    std = [round(v) for v in st.stddev]
    opaque = 0.0
    if has_alpha:
        hist = im.split()[-1].histogram()
        opaque = round(sum(hist[255:]) / (im.width * im.height) * 100, 2)
    ok = max(std) >= 3
    print(f"{path} {im.width}x{im.height} mode={im.mode} mean={mean} std={std} "
          f"full-opaque={opaque}% -> {'ok' if ok else 'SUSPECT EMPTY'}")
    if thumb:
        rgb.resize((im.width // 3, im.height // 3), Image.LANCZOS).save(thumb, "PNG")
        print("thumb:", thumb)
    return ok


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__)
        raise SystemExit(2)
    sys.exit(0 if check(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None) else 1)
