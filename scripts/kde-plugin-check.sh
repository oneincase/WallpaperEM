#!/usr/bin/env bash
# 验证 P9 的 Plasma 壁纸插件包：结构 → 安装/注册 → 帧显示 → 兜底 → 卸载。
#
# 这是**插件显示端**的验证，不含引擎侧的帧传输（DMA-BUF/socket 那条路要 Plasma 6，
# 见 docs/kde-verification-report.md 的「未验证」清单）。本脚本证明的是：
#   1. KPackage 结构被真实 Plasma 工具链接受（kpackagetool5 安装+列表）；
#   2. contents/ui/main.qml 能把「引擎写出的帧文件」显示出来（qmlscene + Xvfb 截图取样）；
#   3. 帧文件不存在时走纯色兜底，不报错、不崩（这是 plasmashell 内跑的硬要求）。
#
# 用法：bash scripts/kde-plugin-check.sh
# 退出码：0 = 全部通过（会自行卸载插件，不留痕迹）；1 = 有失败项。
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PKG="$REPO/packaging/plasma-wallpaper/io.github.oneincase.wallpaperem"
PLUGIN_ID="io.github.oneincase.wallpaperem"
KPT="$(command -v kpackagetool5 || command -v kpackagetool6 || true)"
QTYPE="Plasma/Wallpaper"
WORK="$(mktemp -d /tmp/kde-plugin-check.XXXXXX)"
FAILED=0
PASSED=0

ok()   { PASSED=$((PASSED+1)); printf '[ OK ] %s\n' "$*"; }
bad()  { FAILED=$((FAILED+1)); printf '[FAIL] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }
say()  { printf '%s\n' "$*"; }

cleanup() {
  if [ -n "${KPT:-}" ] && [ -n "${INSTALLED:-}" ]; then
    "$KPT" --type "$QTYPE" --remove "$PLUGIN_ID" >/dev/null 2>&1 || true
  fi
  [ -n "${QML_PID:-}" ] && kill "$QML_PID" 2>/dev/null || true
  [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

say "=== Plasma 壁纸插件包验证 ==="
say "包目录 = $PKG"
say "插件 id = $PLUGIN_ID"

# ---- 0. 依赖与包结构 ----
[ -d "$PKG" ] || { bad "包目录不存在：$PKG"; exit 1; }
for f in metadata.json contents/config/main.xml contents/ui/main.qml contents/ui/config.qml; do
  [ -f "$PKG/$f" ] && ok "结构存在 $f" || bad "缺文件 $f"
done
if [ -z "$KPT" ]; then
  warn "没有 kpackagetool5/6，跳过安装验证（只做包结构检查）"
fi

# metadata.json 的必备键（照 /usr/share/plasma/wallpapers/org.kde.image/metadata.json 的形状）
if command -v python3 >/dev/null 2>&1; then
  if python3 - "$PKG/metadata.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
k=d.get("KPlugin",{})
assert k.get("Id"), "KPlugin.Id 缺失"
assert k.get("Name"), "KPlugin.Name 缺失"
assert "Plasma/Wallpaper" in k.get("ServiceTypes",[]), "ServiceTypes 必须含 Plasma/Wallpaper"
assert d.get("X-KDE-ParentApp")=="org.kde.plasmashell", "X-KDE-ParentApp 必须是 org.kde.plasmashell"
PY
  then ok "metadata.json 必备键齐全（Id/Name/ServiceTypes/X-KDE-ParentApp）"; else bad "metadata.json 缺必备键"; fi
else
  warn "无 python3，跳过 metadata.json 键校验"
fi

# ---- 1. 安装 + 注册 ----
if [ -n "$KPT" ]; then
  if "$KPT" --type "$QTYPE" --install "$PKG" >"$WORK/install.log" 2>&1; then
    ok "kpackagetool5 安装成功"
    INSTALLED=1
  else
    bad "kpackagetool5 安装失败：$(tail -n 3 "$WORK/install.log" | tr '\n' ' ')"
  fi
  if "$KPT" --type "$QTYPE" --list 2>/dev/null | grep -qF "$PLUGIN_ID"; then
    ok "插件已注册（--list 可见）"
  else
    bad "插件未出现在 --list 中"
  fi
  ls -d "$HOME/.local/share/plasma/wallpapers/$PLUGIN_ID" >/dev/null 2>&1 \
    && ok "落地路径 ~/.local/share/plasma/wallpapers/$PLUGIN_ID" \
    || warn "未在 ~/.local/share/plasma/wallpapers/ 找到（KPackage 安装路径随版本变）"
fi

# ---- 2. 渲染验证：引擎写帧 → 插件显示 ----
need=(Xvfb qmlscene convert)
miss=0
for c in "${need[@]}"; do command -v "$c" >/dev/null 2>&1 || { warn "缺 $c，跳过渲染验证"; miss=1; }; done
if [ "$miss" = 0 ]; then
  # 引擎侧契约：先写 .tmp 再 rename（原子替换）
  mkdir -p /tmp/wallpaperem
  convert -size 400x300 xc:magenta "$WORK/frame.png" 2>/dev/null
  mv "$WORK/frame.png" /tmp/wallpaperem/frame.png

  # 独立渲染用的外壳：main.qml 在没有 plasmashell 时用内部 stub 配置
  cat >"$WORK/harness.qml" <<EOF
import QtQuick 2.15
import QtQuick.Window 2.15
Window {
    width: 400; height: 300; visible: true
    Loader { anchors.fill: parent; source: "file://$PKG/contents/ui/main.qml" }
}
EOF

  shot() { # $1 = 输出 png, $2 = 标签
    local out="$1" label="$2"
    rm -f "$out"
    Xvfb :97 -screen 0 400x300x24 >/dev/null 2>&1 &
    XVFB_PID=$!
    sleep 1
    DISPLAY=:97 qmlscene "$WORK/harness.qml" >"$WORK/qml-$label.log" 2>&1 &
    QML_PID=$!
    sleep 3
    DISPLAY=:97 import -window root "$out" 2>/dev/null
    kill "$QML_PID" 2>/dev/null; wait "$QML_PID" 2>/dev/null
    kill "$XVFB_PID" 2>/dev/null; wait "$XVFB_PID" 2>/dev/null
    unset QML_PID XVFB_PID
    # 帧文件不存在时的 "QML Image: Cannot open" 是设计内的降级路径（下面单独断言
    # 兜底色），不算错误；其余带 文件:行号 的 QML 输出一律当错误。
    err="$(grep -E 'main\.qml:[0-9]+:' "$WORK/qml-$label.log" | grep -v 'QML Image: Cannot open' | head -n 1)"
    if [ -n "$err" ]; then
      bad "QML 加载报错（$label）：$err"
      return 1
    fi
    return 0
  }
  px() { convert "$1" -format "%[pixel:p{20,20}]" info: 2>/dev/null; }

  if shot "$WORK/with-frame.png" frame; then
    v="$(px "$WORK/with-frame.png")"
    case "$v" in
      *255,0,255*|*srgb\(255,0,255\)*|*"magenta"*) ok "帧显示正常（左上角像素 = $v，期望品红）" ;;
      *) bad "帧没有被显示（左上角像素 = $v，期望品红）" ;;
    esac
  fi

  rm -f /tmp/wallpaperem/frame.png
  if shot "$WORK/no-frame.png" noframe; then
    v="$(px "$WORK/no-frame.png")"
    case "$v" in
      *16,16,20*|*"#101014"*) ok "帧缺失时走兜底色（左上角像素 = $v）" ;;
      *) bad "兜底色不对（左上角像素 = $v，期望 #101014 = 16,16,20）" ;;
    esac
  fi
else
  warn "渲染验证已跳过"
fi

# ---- 3. 卸载（trap 里也会兜一次） ----
if [ -n "$KPT" ] && [ -n "${INSTALLED:-}" ]; then
  if "$KPT" --type "$QTYPE" --remove "$PLUGIN_ID" >/dev/null 2>&1; then
    INSTALLED=""
    ok "已卸载，未留痕迹"
    if "$KPT" --type "$QTYPE" --list 2>/dev/null | grep -qF "$PLUGIN_ID"; then
      bad "卸载后仍出现在 --list 中"
    else
      ok "卸载后 --list 已无该插件"
    fi
  else
    bad "卸载失败（手工执行：$KPT --type $QTYPE --remove $PLUGIN_ID）"
  fi
fi

say ""
say "通过 $PASSED 项，失败 $FAILED 项"
[ "$FAILED" = 0 ] || exit 1
