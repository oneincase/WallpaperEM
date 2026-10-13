#!/usr/bin/env bash
# 探测 KWin 的 Wayland 全局接口清单（只读，不改仓库、不改用户配置）。
#
# 为什么需要它：方案 §4.1 的 Wayland 路径依赖三个协议是否真被 KWin 广告出来
#   * zwlr_layer_shell_v1        —— 把壁纸窗口放进 Background/Bottom 层
#   * ext_background_effect_manager_v1 —— Plasma 6.7+ 的模糊替代（旧 org_kde_kwin_blur 被移除）
#   * org_kde_kwin_idle / ext_idle_notifier_v1 —— 空闲/熄屏（自动暂停）
# 上游源码能证明「代码存在」，不能证明「本机这台合成器广告了它」。本脚本用
# wayland-info 连上真实的 KWin Wayland socket，把 version 号打出来。
#
# 用法：
#   bash scripts/kde-wayland-globals.sh              # 自带一个嵌套 KWin（--virtual，无需显卡/登录会话）
#   WAYLAND_DISPLAY=wayland-0 bash scripts/kde-wayland-globals.sh --attach   # 探测已在跑的会话
#
# 退出码：0 = 已取到全局清单（无论有没有目标协议）；1 = 起不来/连不上。
set -uo pipefail

SOCK="${SOCK:-wallpaperem-wlprobe}"
ATTACH=0
[ "${1:-}" = "--attach" ] && ATTACH=1

KD=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
LOG="${LOG:-/tmp/kde-wayland-globals.log}"
FAILED=0

say()  { printf '%s\n' "$*"; }
ok()   { printf '[ OK ] %s\n' "$*"; }
warn() { printf '[WARN] %s\n' "$*"; }
bad()  { printf '[FAIL] %s\n' "$*"; FAILED=1; }

for c in wayland-info; do
  command -v "$c" >/dev/null 2>&1 || { bad "缺 $c（apt-get install -y wayland-utils）"; exit 1; }
done
[ -d "$KD" ] || { bad "XDG_RUNTIME_DIR=$KD 不存在（非图形登录会话？）"; exit 1; }

cleanup() {
  if [ "$ATTACH" = 0 ] && [ -n "${KWIN_PID:-}" ]; then
    kill "$KWIN_PID" 2>/dev/null || true
    wait "$KWIN_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

say "=== KWin Wayland 全局探测 ==="
say "XDG_RUNTIME_DIR = $KD"
say "模式            = $([ "$ATTACH" = 1 ] && echo 'attach（用现有 WAYLAND_DISPLAY）' || echo 'nested（自起 kwin_wayland --virtual）')"

if [ "$ATTACH" = 1 ]; then
  [ -n "${WAYLAND_DISPLAY:-}" ] || { bad 'attach 模式需要 WAYLAND_DISPLAY'; exit 1; }
  TARGET="$WAYLAND_DISPLAY"
  say "WAYLAND_DISPLAY = $TARGET"
else
  command -v kwin_wayland >/dev/null 2>&1 || { bad '缺 kwin_wayland'; exit 1; }
  if [ -e "$KD/$SOCK" ]; then
    bad "$KD/$SOCK 已存在（上次没清干净？换个 SOCK= 再跑）"
    exit 1
  fi
  say "启动            = kwin_wayland --virtual --socket $SOCK（日志 $LOG）"
  : >"$LOG"
  # setsid：SSH 非交互 shell 退出时不要 SIGHUP 带走合成器
  setsid kwin_wayland --virtual --width 1280 --height 800 --socket "$SOCK" --no-lockscreen \
    >>"$LOG" 2>&1 &
  KWIN_PID=$!
  TARGET="$SOCK"
  # 等 socket：kwin 起来后要先建好 $KD/$SOCK 才连得上
  ready=0
  for i in $(seq 1 40); do
    if [ -S "$KD/$SOCK" ]; then ready=$i; break; fi
    kill -0 "$KWIN_PID" 2>/dev/null || break
    sleep 0.5
  done
  if [ "$ready" = 0 ]; then
    bad "20s 内没有出现 $KD/$SOCK（kwin_wayland 可能起不来）"
    tail -n 20 "$LOG" 2>/dev/null | sed 's/^/       | /'
    exit 1
  fi
  ok "socket 就绪（第 ${ready} 次探测，约 $(awk "BEGIN{print $ready*0.5}")s）"
fi

RAW="$(WAYLAND_DISPLAY="$TARGET" XDG_RUNTIME_DIR="$KD" wayland-info 2>&1)"
rc=$?
# wayland-info 把接口名写成 'name'（带单引号）——去掉引号再匹配，否则全是假阴性
GLOBALS="$(printf '%s\n' "$RAW" | tr -d "'")"
if [ $rc -ne 0 ] || [ -z "$GLOBALS" ]; then
  bad "wayland-info 连不上（rc=$rc）"
  printf '%s\n' "$GLOBALS" | head -n 10 | sed 's/^/       | /'
  exit 1
fi
ok "已取到全局清单"

say ""
say "--- 与 KDE 适配相关的接口（原始行）---"
# 注意：Wayland 全局里带对象后缀的是「工厂」（manager / shell），
# 例如 blur 的全局叫 org_kde_kwin_blur_manager、分数缩放叫 wp_fractional_scale_manager_v1
for iface in wl_compositor wl_shm xdg_wm_base zwlr_layer_shell_v1 \
             ext_background_effect_manager_v1 org_kde_kwin_blur_manager \
             org_kde_kwin_idle ext_idle_notifier_v1 wp_fractional_scale_manager_v1 \
             wp_viewporter zxdg_output_manager_v1 zwp_linux_dmabuf_v1 \
             zwp_keyboard_shortcuts_inhibit_manager_v1 wp_tearing_control_manager_v1 \
             kde_output_management_v2 org_kde_plasma_shell; do
  line="$(printf '%s\n' "$GLOBALS" | grep -E "^interface: *${iface}[, ]" | head -n 1)"
  if [ -n "$line" ]; then
    ver="$(printf '%s' "$line" | sed -n 's/.*version: *\([0-9][0-9]*\).*/\1/p')"
    ok "$(printf '%-36s v%s' "$iface" "${ver:-?}")"
  else
    warn "$(printf '%-36s 未广告' "$iface")"
  fi
done

say ""
say "--- 全部接口名（供人工比对）---"
printf '%s\n' "$GLOBALS" | grep -E '^interface: ' | sed 's/interface: //' | sort | tr '\n' ' ' | fold -s -w 100 | sed 's/^/       /'
say ""

# 判定：方案 §4.1 Wayland 路径的前置协议
if printf '%s\n' "$GLOBALS" | grep -qE "^interface: *zwlr_layer_shell_v1[, ]"; then
  ok "layer-shell 可用 → Wayland 的层归属路径在本机成立（但「层归属」不等于「拿到桌面图标之下」，见方案 §2.5）"
else
  warn "layer-shell 不可用 → 本机 Wayland 连层归属都拿不到"
fi

say ""
if [ "$FAILED" = 0 ]; then say "结论：探测完成（详细日志 $LOG）"; else say "结论：探测失败（详细日志 $LOG）"; fi
exit "$FAILED"
