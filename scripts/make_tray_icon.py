#!/usr/bin/env python3
"""生成 macOS 菜单栏托盘图标（模板图）。

从应用 logo（层叠卡片 + 播放键）极简化而来：
三张沿对角线错层的圆角卡片，前卡镂空播放三角，黑色剪影 + 透明底，
图形顶格铺满画布（无内边距），系统按菜单栏深浅自动着色（icon_as_template）。

用法: python3 make_tray_icon.py [输出到 src-tauri/icons 的路径]
需要 pillow。44x44 = 22pt @2x，macOS 状态栏标准尺寸。
"""
import sys

from PIL import Image, ImageDraw

PT = 44          # 逻辑尺寸（pt）
SS = 16          # 超采样倍数，抗锯齿
S = PT * SS      # 画布 704
CARD = 544       # 卡片边长
RAD = 124        # 卡片圆角
STEP = 80        # 卡片沿对角线的错层间距
GAP = 28         # 层与层之间的镂空缝隙


def card(center: tuple[int, int], expand: int = 0) -> Image.Image:
    x, y = center
    half = CARD // 2 + expand
    m = Image.new('L', (S, S), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [x - half, y - half, x + half - 1, y + half - 1],
        radius=RAD + expand, fill=255)
    return m


def main() -> None:
    # 前卡在左下、后卡往右上错层；三卡整体包围盒恰好顶满画布
    front = (272, 432)
    mid = (front[0] + STEP, front[1] - STEP)
    back = (front[0] + 2 * STEP, front[1] - 2 * STEP)

    import numpy as np
    alpha = np.zeros((S, S), dtype=np.uint16)
    # 后卡先画；每层先被「前一层外扩 GAP」的剪影挖出缝隙，保证层间分离。
    # 乘法必须用 uint16：uint8 直接乘会回绕溢出，alpha 全毁
    for c, front_c in ((back, mid), (mid, front)):
        a = np.asarray(card(c), dtype=np.uint16)
        hole = np.asarray(card(front_c, expand=GAP), dtype=np.uint16)
        a = a * (255 - hole) // 255
        np.maximum(alpha, a, out=alpha)
    np.maximum(alpha, np.asarray(card(front), dtype=np.uint16), out=alpha)

    # 播放三角整体镂空（透出菜单栏底色），中心略右移做视觉居中
    cx, cy = front
    tri = Image.new('L', (S, S), 0)
    ImageDraw.Draw(tri).polygon(
        [(cx - 56, cy - 110), (cx - 56, cy + 110), (cx + 129, cy)], fill=255)
    alpha = alpha * (255 - np.asarray(tri, dtype=np.uint16)) // 255

    icon = Image.new('RGBA', (S, S), (0, 0, 0, 0))
    icon.putalpha(Image.fromarray(alpha.astype(np.uint8)))
    out = icon.resize((PT, PT), Image.LANCZOS)

    dest = sys.argv[1] if len(sys.argv) > 1 else 'tray-44.png'
    out.save(dest)

    # 预览：本体(白底) + 模拟菜单栏深/浅底着色效果
    for scale, name in ((5, 'big'), (1, 'actual')):
        px = PT * scale
        bg = Image.new('RGB', (px, px), (255, 255, 255))
        bg.paste((28, 28, 30), (0, 0, px, px),
                 out.resize((px, px), Image.LANCZOS))
        bg.save(f'{dest}.{name}.preview.png')
    bar = Image.new('RGB', (PT * 10, PT + 16), (40, 40, 42))
    white = Image.new('RGBA', (PT, PT), (255, 255, 255, 255))
    white.putalpha(out.split()[3])
    bar.paste(white, (PT * 2, 8), white)
    bar.paste(out, (PT * 5, 8), out)
    bar.save(f'{dest}.menubar.preview.png')
    print(f'已输出 {dest}（{PT}x{PT} 模板图）及 *.big / *.actual / *.menubar 预览')


if __name__ == '__main__':
    main()
