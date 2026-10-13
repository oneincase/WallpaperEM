#!/usr/bin/env bash
# 实测壁纸窗口在 KWin 里到底属于哪一层（DesktopLayer / BelowLayer / NormalLayer ...）。
#
# 为什么需要它：KDE 下「桌面图标会不会被壁纸盖住」完全由**层**决定，而层不是
# `kwinrulesrc` 里能直接写的数——`layer` 规则在 KWin 里长期是 unused（写进去也被忽略），
# KWin 是按窗口类型/角色自己判的。所以唯一可靠的做法是把 KWin 的判定读出来：
# 用 KWin 的脚本 API `workspace.clientList()` 取每个窗口的 `layer` / `windowType` /
# `resourceClass`，print 到日志里再捞回来。
#
# 用法（在已经跑起来 KDE 会话的机器上）：
#   bash scripts/kde-kwin-layer.sh            # 查所有窗口
#   KWIN_LAYER_MATCH=WallpaperEM bash ...      # 只关心某个 WM_CLASS
#
# 退出码：0 = 成功取到数据；1 = 起不来脚本 / 取不到输出。
set -uo pipefail

MATCH="${KWIN_LAYER_MATCH:-}"
TAG="KWINLAYER"
STAGE="$(mktemp -d /tmp/kwin-layer-XXXXXX)"
say() { printf '\n=== %s ===\n' "$*"; }
ok()  { printf '  [OK]   %s\n' "$*"; }
bad() { printf '  [FAIL] %s\n' "$*"; }

[ -n "${DISPLAY:-}" ] || { bad "DISPLAY 未设置（要在 KDE 会话里跑）"; exit 1; }
command -v qdbus >/dev/null 2>&1 || command -v qdbus6 >/dev/null 2>&1 || { bad "缺 qdbus/qdbus6"; exit 1; }
QDBUS="$(command -v qdbus || command -v qdbus6)"

say "KWin 层实测（DISPLAY=$DISPLAY MATCH=${MATCH:-全部}）"

# KWin 脚本必须是**一个目录**（含 metadata.desktop + main.js），loadScript 收目录路径。
# 传单个 .js 文件路径会被静默忽略——这个坑踩过一次。
mkdir -p "$STAGE/layerprobe"
cat > "$STAGE/layerprobe/metadata.desktop" <<'EOF'
[Desktop Entry]
Name=WallpaperEM Layer Probe
Type=Service
X-Plasma-API=javascript
X-Plasma-MainScript=main.js
X-KDE-PluginInfo-Name=wallpaperem-layerprobe
X-KDE-PluginInfo-Version=1
EOF
cat > "$STAGE/layerprobe/main.js" <<'EOF'
// print() 会进 KWin 的日志（journalctl --user -t kwin_x11 / ~/.xsession-errors）
var layerNames = {
  0: "NormalLayer", 1: "AboveLayer", 2: "NotificationLayer",
  3: "ActiveLayer", 4: "DockLayer", 5: "DesktopLayer", 6: "BelowLayer"
};
function dump() {
  var list = workspace.clientList();
  print("KWINLAYER count=" + list.length);
  for (var i = 0; i < list.length; i++) {
    var w = list[i];
    var layer = -1;
    try { layer = w.layer; } catch (e) { layer = -99; }
    print("KWINLAYER win=" + w.resourceClass
        + " caption=" + w.caption
        + " layer=" + layer + "(" + (layerNames[layer] || "?") + ")"
        + " type=" + w.windowType
        + " normalWindow=" + w.normalWindow
        + " desktopWindow=" + w.desktopWindow
        + " dock=" + w.dock
        + " keepBelow=" + w.keepBelow
        + " onAllDesktops=" + w.onAllDesktops
        + " skipTaskbar=" + w.skipTaskbar);
  }
}
dump();
EOF

say "加载 KWin 脚本"
if ! "$QDBUS" org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$STAGE/layerprobe" 2>&1 | sed 's/^/  loadScript → /'; then
  bad "loadScript 失败"; exit 1
fi
"$QDBUS" org.kde.KWin /Scripting org.kde.kwin.Scripting.start 2>&1 | sed 's/^/  start → /'
sleep 3

say "取回输出"
OUT=""
for src in "journalctl --user -t kwin_x11 --since -2min --no-pager" \
           "journalctl --user --since -2min --no-pager" \
           "cat $HOME/.xsession-errors"; do
  RAW="$($src 2>/dev/null | grep "$TAG" || true)"
  if [ -n "$RAW" ]; then
    echo "  来源: ${src%% *}"
    OUT="$RAW"
    break
  fi
done

if [ -z "$OUT" ]; then
  bad "没捞到 $TAG 输出。可能原因：KWin 的 print 不进这个日志源"
  echo "  手工排查："
  echo "    journalctl --user -t kwin_x11 --since -5min | tail -40"
  echo "    grep $TAG ~/.xsession-errors"
  echo "  脚本目录留在 $STAGE（可重跑 $QDBUS org.kde.KWin /Scripting ... loadScript $STAGE/layerprobe）"
  exit 1
fi

echo "$OUT" | sed "s/^$TAG /  /" | sed 's/^/  /'

if [ -n "$MATCH" ]; then
  say "只看 $MATCH"
  echo "$OUT" | grep -F "$MATCH" | sed 's/^/  /' || bad "没有匹配 $MATCH 的窗口"
fi

say "卸载脚本"
"$QDBUS" org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript wallpaperem-layerprobe >/dev/null 2>&1
rm -rf "$STAGE"
ok "完成"
