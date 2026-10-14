#!/usr/bin/env bash
# 前台窗口分类（wallpaper/linux.rs::frontmost_kind）的数据源取证。
#
# 起一个 Xvfb + kwin_x11 + plasmashell 的 KDE/X11 会话，然后读 EWMH 的
# _NET_ACTIVE_WINDOW，并把该窗口的 _NET_WM_PID / _NET_WM_WINDOW_TYPE 打出来，
# 最后按 stacking 逐个列出「pid + 窗口类型」。
#
# 为什么要用它：自动暂停的「前台应用」判据在 Linux 上原本恒 Unknown（判定表恒
# Keep，等于不生效）。本脚本证明 X11 会话里数据源存在、字段可读、分类有依据；
# 证据实例见 docs/kde-verification-report.md §6.4。
#
# 只读：不写仓库、不写用户配置；结束时把拉起的进程都收掉。
# 用法：bash scripts/kde-activewin-check.sh [显示号，默认 :98]
set -u

DISP="${1:-:98}"
NUM="${DISP#:}"

cleanup() {
  pkill -x plasmashell >/dev/null 2>&1
  pkill -x kwin_x11 >/dev/null 2>&1
  pkill -x Xvfb >/dev/null 2>&1
}
trap cleanup EXIT

command -v kwin_x11 >/dev/null || { echo "[SKIP] 没有 kwin_x11（非 KDE 机器）"; exit 0; }
command -v xprop >/dev/null || { echo "[SKIP] 没有 xprop（缺 x11-utils）"; exit 0; }

# 陈旧的 X socket 会让 Qt 的 xcb 插件连不上（抽象 socket 通、文件 socket 没建好）
[ -e "/tmp/.X11-unix/X$NUM" ] && { echo "[INFO] 清理陈旧的 /tmp/.X11-unix/X$NUM"; rm -f "/tmp/.X11-unix/X$NUM"; }

setsid Xvfb "$DISP" -screen 0 1280x800x24 +extension GLX +render -noreset >/tmp/kde-activewin-xvfb.log 2>&1 &
for _ in $(seq 1 40); do [ -S "/tmp/.X11-unix/X$NUM" ] && break; sleep 0.25; done
[ -S "/tmp/.X11-unix/X$NUM" ] || { echo "[FAIL] Xvfb 没起来（看 /tmp/kde-activewin-xvfb.log）"; exit 1; }

export DISPLAY="$DISP" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1

setsid kwin_x11 --replace >/tmp/kde-activewin-kwin.log 2>&1 &
for _ in $(seq 1 40); do
  xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q "window id" && break
  sleep 0.25
done
xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q "window id" || {
  echo "[FAIL] kwin_x11 未成为 WM（看 /tmp/kde-activewin-kwin.log）"; exit 1; }

setsid plasmashell --no-respawn >/tmp/kde-activewin-plasma.log 2>&1 &
sleep 12

echo "--- WM ---"
xprop -root _NET_SUPPORTING_WM_CHECK | head -1
echo "--- ACTIVE (root) ---"
xprop -root _NET_ACTIVE_WINDOW
AW="$(xprop -root _NET_ACTIVE_WINDOW | sed 's/.*# //; s/,.*//')"
echo "active = [$AW]"
if [ -n "$AW" ] && [ "$AW" != "0x0" ]; then
  echo "--- ACTIVE pid/type ---"
  xprop -id "$AW" _NET_WM_PID _NET_WM_WINDOW_TYPE WM_CLASS 2>&1 | head -4
fi
echo "--- stacking: pid + type ---"
for w in $(xprop -root _NET_CLIENT_LIST_STACKING | sed 's/.*# //; s/,/ /g'); do
  t="$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | sed 's/.*= //')"
  p="$(xprop -id "$w" _NET_WM_PID 2>/dev/null | awk '{print $3}')"
  echo "$w pid=$p type=$t"
done
echo "--- 判定（与 classify_frontmost 同一套规则）---"
if [ -z "$AW" ] || [ "$AW" = "0x0" ]; then
  echo "FrontKind::Unknown  → 决策 Keep（不改变播放状态）"
elif xprop -id "$AW" _NET_WM_WINDOW_TYPE 2>/dev/null | grep -qE 'DESKTOP|DOCK'; then
  echo "FrontKind::Desktop  → 决策 Play（壁纸露着）"
else
  echo "FrontKind::Other    → 决策 Pause（被别的应用盖住）"
fi
