#!/usr/bin/env bash
# P9 的 Plasma 壁纸插件包已迁到**独立仓库** WallpaperEM-Plasma（与主仓同级目录），
# 包本体与它的验证脚本都在那边：
#
#   ../WallpaperEM-Plasma/io.github.oneincase.wallpaperem  ← 实际是仓库根
#   ../WallpaperEM-Plasma/scripts/verify.sh                ← 16 项结构/显示/降级断言（Xvfb+qmlscene）
#   ../WallpaperEM-Plasma/scripts/plasma-real-test.sh       ← 18 项真桌面断言（真实 plasmashell 里选为壁纸）
#   ../WallpaperEM-Plasma/docs/real-plasma-verification.md  ← 真桌面验证记录（对照实验 + 原始数字）
#
# 本脚本保留为**转发入口**，避免主仓里的历史引用失效；不要再往这里加断言，
# 新断言请加在独立仓库的 verify.sh / plasma-real-test.sh 里。
#
# 用法：bash scripts/kde-plugin-check.sh            # = 独立仓的 verify.sh
#       bash scripts/kde-plugin-check.sh --real     # = 独立仓的 plasma-real-test.sh
# 退出码：透传被调用脚本的退出码（0 = 全部通过）。
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PLASMA_DIR="${WALLPAPEREM_PLASMA_DIR:-$(cd "$REPO/.." && pwd)/WallpaperEM-Plasma}"

if [ ! -d "$PLASMA_DIR" ]; then
  printf '[FAIL] 找不到独立插件仓库：%s\n' "$PLASMA_DIR" >&2
  printf '       克隆 oneincase/WallpaperEM-Plasma 到主仓同级目录，或用 WALLPAPEREM_PLASMA_DIR 指定路径。\n' >&2
  exit 1
fi

case "${1:-}" in
  --real|-r) shift; exec bash "$PLASMA_DIR/scripts/plasma-real-test.sh" "$@" ;;
  *)         exec bash "$PLASMA_DIR/scripts/verify.sh" "$@" ;;
esac
