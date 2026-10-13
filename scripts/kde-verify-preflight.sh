#!/usr/bin/env bash
# KDE 适配验证：环境预检（只读，不改系统）
#
# 目的：在跑 docs/kde-adaptation-plan.md §6.2 的冒烟之前，先回答「这台机器能把方案里的哪几条
# 断言验证掉、哪几条验证不了」。方案的核心分歧点是 Plasma 6（Wayland layer-shell v5、
# ext_background_effect_manager_v1、org.freedesktop.impl.portal.Wallpaper 6.5.0），
# 而 Ubuntu 24.04 仓库只有 Plasma 5.27 —— 所以版本探测必须先跑。
#
# 用法：
#   bash scripts/kde-verify-preflight.sh              # 本机
#   ssh parallels@10.211.55.5 'bash -s' < scripts/kde-verify-preflight.sh
#
# 退出码：0 = 可以进入冒烟；1 = 缺少前置条件（详见输出里的 [BLOCK]）

set -uo pipefail

pass() { printf '  \033[32m[ OK ]\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m[WARN]\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m[FAIL]\033[0m %s\n' "$*"; }
info() { printf '  [info] %s\n' "$*"; }
block() { printf '  \033[31m[BLOCK]\033[0m %s\n' "$*"; BLOCKED=1; }
BLOCKED=0

echo "== 1. 主机与系统 =="
info "主机名: $(hostname)"
info "系统:   $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME")"
info "内核:   $(uname -r)"
info "架构:   $(uname -m)"
info "内存:   $(free -h 2>/dev/null | awk '/^Mem:/{print $2" total, "$7" available"}')"
info "磁盘 /: $(df -h / | awk 'NR==2{print $4" 可用 ("$5" 已用)"}')"
case "$(uname -m)" in
  aarch64 | arm64) info "arm64：注意 KDE neon 官方 ISO 只有 amd64，本机不能走 neon 路线" ;;
esac

echo
echo "== 2. Plasma / KWin 版本（决定能验证方案里的哪几条）=="
plasma_ver=""
if command -v plasmashell >/dev/null 2>&1; then
  plasma_ver="$(plasmashell --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+(\.[0-9]+)?' | head -1)"
  pass "plasmashell 已安装: ${plasma_ver:-未知版本}"
else
  # 未安装时退回看仓库候选版本
  pkg_ver="$(apt-cache policy plasma-desktop 2>/dev/null | awk '/Candidate/{print $2}')"
  if [ -n "$pkg_ver" ] && [ "$pkg_ver" != "(none)" ]; then
    info "plasmashell 未安装；仓库候选 plasma-desktop = $pkg_ver"
  else
    info "plasmashell 未安装，且仓库没有 plasma-desktop"
  fi
fi

major="$(printf '%s' "$plasma_ver" | cut -d. -f1)"
if [ "$major" = "6" ]; then
  pass "Plasma 6 → 可验证：layer-shell v5、ext_background_effect_manager_v1、portal Wallpaper"
else
  warn "非 Plasma 6（${plasma_ver:-未安装}）→ 方案中「机制①透明化」「layer-shell 进阶」「模糊新协议」「portal 静态壁纸」全部**验证不了**，只能验证 X11 落位/GNOME 式堆叠对照"
fi

if command -v kwin_wayland >/dev/null 2>&1; then
  pass "kwin_wayland 在: $(command -v kwin_wayland)"
  if kwin_wayland --help 2>&1 | grep -q -- '--virtual'; then
    pass "kwin_wayland 支持 --virtual（§6.2 的嵌套 Wayland 冒烟可用）"
  else
    block "kwin_wayland 不支持 --virtual → §6.2 的 nested Wayland 档在这台机器上跑不了"
  fi
else
  warn "kwin_wayland 未安装 → Wayland 冒烟档不可用"
fi

echo
echo "== 3. 会话与显示（决定能否真机验证桌面层）=="
info "XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-<空>}"
info "XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-<空>}"
info "WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-<空>}"
info "DISPLAY=${DISPLAY:-<空>}"
info "loginctl: $(loginctl 2>/dev/null | awk 'NR==2{print "session="$1" user="$3" seat="$4" tty="$5}')"
if [ -n "${WAYLAND_DISPLAY:-}" ]; then
  pass "当前是 Wayland 图形会话"
elif [ -n "${DISPLAY:-}" ]; then
  pass "当前是 X11 图形会话"
else
  warn "当前无图形会话（tty/SSH）→ 桌面层落位的真机验证需要先登录图形会话或起嵌套合成器"
fi

echo
echo "== 4. 方案依赖的外部命令 =="
for c in qdbus qdbus6 kscreen-doctor plasmashell plasma-apply-wallpaperimage; do
  if command -v "$c" >/dev/null 2>&1; then pass "$c → $(command -v "$c")"; else warn "$c 缺失"; fi
done
info "（kwinrulesrc 规则写入不依赖这些命令；qdbus 用于 reconfigure，kscreen-doctor 用于每屏查询）"

echo
echo "== 5. 冒烟测试前置（§6.2 / §6.3）=="
for c in xvfb-run Xvfb dbus-run-session; do
  if command -v "$c" >/dev/null 2>&1; then pass "$c → $(command -v "$c")"; else block "$c 缺失 → X11 冒烟跑不了（apt install xvfb dbus-x11）"; fi
done
if [ -f /usr/share/wayland-sessions/plasmawayland.desktop ]; then
  pass "plasmawayland.desktop 存在"
else
  warn "plasmawayland.desktop 缺失 → 无 Wayland 会话入口（kwin-wayland 可能没装）"
fi
if [ -f /usr/share/xsessions/plasmax11.desktop ]; then
  pass "plasmax11.desktop 存在"
else
  warn "plasmax11.desktop 缺失 → 无 X11 会话入口"
fi

echo
echo "== 6. 权限 =="
if sudo -n true 2>/dev/null; then
  pass "sudo 免密可用 → 可无人值守安装/改配置"
else
  block "sudo 需要密码 → 无法自动安装 KDE；需先配 NOPASSWD（见 docs/kde-smoke-setup.md）"
fi

echo
echo "== 结论 =="
if [ "$BLOCKED" = "1" ]; then
  fail "存在阻塞项，安装/冒烟不能自动进行（见上面的 [BLOCK]）"
  exit 1
fi
pass "预检通过，可以进入 §6.2 冒烟"
exit 0
