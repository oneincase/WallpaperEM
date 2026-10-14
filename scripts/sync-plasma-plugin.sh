#!/usr/bin/env bash
# 把独立仓库 WallpaperEM-Plasma 的插件包同步进本仓 packaging/。
#
# 为什么要同步：应用内「一键启用 KDE 插件」按的是**随安装包分发**的那份包
# （`src-tauri/tauri.conf.json` 的 resources 把 `../packaging/plasma-wallpaper`
# 映射成 `$RESOURCE/plasma-wallpaper`，运行时由 `kde_plugin::resolve_package_dir`
# 找到）。插件的开发主体在同级独立仓，所以这里只做单向搬运：
# 独立仓 → 本仓 packaging/，并保留本仓 packaging 里那份「说明用」的 README。
#
# 用法：
#   bash scripts/sync-plasma-plugin.sh                 # 默认取 ../WallpaperEM-Plasma
#   bash scripts/sync-plasma-plugin.sh /path/to/repo   # 显式指定源仓库
# 检查（CI/发版前）：同步后 `git status` 应无 packaging 改动，有改动说明漏同步。
set -euo pipefail

SRC="${1:-$(cd "$(dirname "$0")/.." && pwd)/../WallpaperEM-Plasma}"
DST="$(cd "$(dirname "$0")/.." && pwd)/packaging/plasma-wallpaper/io.github.oneincase.wallpaperem"
PLUGIN_ID="io.github.oneincase.wallpaperem"

if [ ! -f "$SRC/metadata.json" ]; then
  echo "源仓库里没有 metadata.json：$SRC" >&2
  echo "（先 clone/更新同级独立仓 WallpaperEM-Plasma，或把路径作为第一个参数传进来）" >&2
  exit 1
fi

# 包体 = metadata.json + contents/；其余（README/docs/scripts/tools/LICENSE）不打包
# （macOS 的 BSD install 没有 -D，所以用 mkdir + cp，保证在 mac/Linux 上都能跑）
mkdir -p "$DST"
cp -f "$SRC/metadata.json" "$DST/metadata.json"
rsync -a --delete --exclude '.DS_Store' "$SRC/contents/" "$DST/contents/"

# 自查：id 必须与引擎侧常量一致，否则装了也切不过去
if ! grep -q "\"Id\"[[:space:]]*:[[:space:]]*\"$PLUGIN_ID\"" "$DST/metadata.json"; then
  echo "metadata.json 里的 KPlugin.Id 不是 $PLUGIN_ID，拒绝同步" >&2
  exit 1
fi
for f in contents/config/main.xml contents/ui/main.qml contents/ui/config.qml; do
  [ -f "$DST/$f" ] || {
    echo "同步后缺文件：$f" >&2
    exit 1
  }
done

# 权限归一：这些文件是**随安装包分发**的资源，运行时会被普通用户读走、并拷进
# ~/.local/share/plasma/wallpapers/。umask 077 的检出会留下 0600，
# 装成 deb（root:root 0600）后普通用户 fs::copy 直接 Permission denied ——
# 实测踩过（安装包自检里「一键启用」失败）。git 只记 100644/100755，
# 工作区权限不会被 checkout 纠正，所以每次同步都归一。
find "$DST" -type d -exec chmod 755 {} +
find "$DST" -type f -exec chmod 644 {} +

echo "已同步：$SRC → $DST"
find "$DST" -type f -not -name '.DS_Store' | sed "s|$DST/|  |" | sort
