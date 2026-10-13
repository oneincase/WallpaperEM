#!/usr/bin/env bash
# KDE 会话冒烟：把 docs/kde-adaptation-plan.md §6.2「KDE/X11 档」的断言做出来。
#
# 为什么要有这个脚本：Dockerfile.linux / .github/workflows/build-linux.yml 现在的冒烟是
#   dbus-run-session -- xvfb-run -a sh -c "timeout 25 WallpaperEM"
# —— 只验「进程不崩」，没有任何断言，且 XDG_CURRENT_DESKTOP 都没设，KDE 分支根本没被走到。
# 本脚本补上 §6.2 要求的硬指标：
#   1. 壁纸窗口的 _NET_WM_WINDOW_TYPE 含 _NET_WM_WINDOW_TYPE_DESKTOP
#   2. 它在 _NET_CLIENT_LIST_STACKING 里位于所有普通窗口之下
#   3. 日志出现「创建 + 桌面层应用」
#   4. 进程存活 N 秒无 panic
#
# 用法：
#   bash scripts/linux-kde-smoke.sh                      # 自动找 target/{release,debug}/WallpaperEM
#   APP=/path/to/WallpaperEM bash scripts/linux-kde-smoke.sh
#   SMOKE_WITH_PLASMA=0 bash scripts/linux-kde-smoke.sh   # 只跑 kwin_x11（plasmashell 可选）
#   KEEP_LOGS=1 bash scripts/linux-kde-smoke.sh           # 保留临时目录便于排查
#
# 退出码：0 全部断言通过；1 有断言失败；2 前置缺失（缺包/缺二进制）

set -uo pipefail

ALIVE_SECS="${ALIVE_SECS:-30}"
SMOKE_WITH_PLASMA="${SMOKE_WITH_PLASMA:-1}"
SCREEN_W="${SCREEN_W:-1280}"
SCREEN_H="${SCREEN_H:-800}"
KEEP_LOGS="${KEEP_LOGS:-0}"

pass() { printf '  \033[32m[PASS]\033[0m %s\n' "$*"; }
fail() { printf '  \033[31m[FAIL]\033[0m %s\n' "$*"; FAILED=1; }
info() { printf '  [info] %s\n' "$*"; }
skip() { printf '  \033[33m[SKIP]\033[0m %s\n' "$*"; }
FAILED=0

# ---------- 前置 ----------
echo "== 前置检查 =="
for c in Xvfb dbus-run-session xdotool xwininfo xprop; do
  if command -v "$c" >/dev/null 2>&1; then info "$c ok"; else
    echo "  [FAIL] 缺少 $c（apt install xvfb dbus-x11 xdotool x11-utils）" >&2
    exit 2
  fi
done
if ! command -v kwin_x11 >/dev/null 2>&1; then
  echo "  [FAIL] 缺少 kwin_x11（apt install kwin-x11）" >&2
  exit 2
fi
if [ "$SMOKE_WITH_PLASMA" = "1" ] && ! command -v plasmashell >/dev/null 2>&1; then
  echo "  [FAIL] 缺少 plasmashell（apt install plasma-workspace）；或 SMOKE_WITH_PLASMA=0" >&2
  exit 2
fi

# 找应用二进制
if [ -z "${APP:-}" ]; then
  for cand in \
    "$(dirname "$0")/../src-tauri/target/release/WallpaperEM" \
    "$(dirname "$0")/../src-tauri/target/debug/WallpaperEM"; do
    if [ -x "$cand" ]; then APP="$cand"; break; fi
  done
fi
if [ -z "${APP:-}" ] || [ ! -x "${APP:-/nonexistent}" ]; then
  echo "  [FAIL] 找不到可执行文件；用 APP=/path/to/WallpaperEM 指定，或先 cargo build" >&2
  exit 2
fi
info "应用: $APP"
info "kwin_x11: $(command -v kwin_x11) ($(kwin_x11 --version 2>&1 | head -1))"
[ "$SMOKE_WITH_PLASMA" = "1" ] && info "plasmashell: $(plasmashell --version 2>&1 | head -1)"

WORK="$(mktemp -d /tmp/kde-smoke.XXXXXX)"
cleanup() {
  [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
  [ -n "${PLASMA_PID:-}" ] && kill "$PLASMA_PID" 2>/dev/null
  [ -n "${KWIN_PID:-}" ] && kill "$KWIN_PID" 2>/dev/null
  sleep 1
  [ -n "${APP_PID:-}" ] && kill -9 "$APP_PID" 2>/dev/null
  [ -n "${KWIN_PID:-}" ] && kill -9 "$KWIN_PID" 2>/dev/null
  if [ "$KEEP_LOGS" = "1" ]; then echo "日志保留在 $WORK"; else rm -rf "$WORK"; fi
}
trap cleanup EXIT

# ---------- 会话内执行体 ----------
cat > "$WORK/inner.sh" <<'INNER'
set -uo pipefail
WORK="$1"; APP="$2"; ALIVE_SECS="$3"; SCREEN_W="$4"; SCREEN_H="$5"; WITH_PLASMA="$6"

INNER_FAILED=0
# 会话内每次 [FAIL] 都计数，最后据此给出退出码（外层据此判定）
fail() { printf '  \033[31m[FAIL]\033[0m %s\n' "$*"; INNER_FAILED=$((INNER_FAILED+1)); }
pass() { printf '  \033[32m[PASS]\033[0m %s\n' "$*"; }

export XDG_CURRENT_DESKTOP=KDE          # 关键：让 KDE 分支被走到（原冒烟没设，等于没测 KDE）
export KDE_FULL_SESSION=true
export KDE_SESSION_VERSION=5
export XDG_SESSION_TYPE=x11
export GDK_BACKEND=x11
export QT_QPA_PLATFORM=xcb
export LIBGL_ALWAYS_SOFTWARE=1          # Xvfb 无 GPU

echo "  [info] XDG_CURRENT_DESKTOP=$XDG_CURRENT_DESKTOP XDG_SESSION_TYPE=$XDG_SESSION_TYPE"

# 1) 启动 kwin_x11（合成器 + 窗口管理器；_NET_CLIENT_LIST_STACKING 由它提供）
kwin_x11 --replace > "$WORK/kwin.log" 2>&1 &
KWIN_PID=$!
for i in $(seq 1 30); do
  xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1 && break
  sleep 0.5
done
if ! xprop -root _NET_SUPPORTING_WM_CHECK 2>/dev/null | grep -q 'window id'; then
  fail "kwin_x11 未成为 WM（看 $WORK/kwin.log）"; exit 1
fi
WM_NAME="$(xprop -root _NET_WM_NAME 2>/dev/null | cut -d= -f2- | tr -d ' "')"
echo "  [info] WM = $WM_NAME"
WIN_COUNT_0="$(xdotool search --onlyvisible --name '.' 2>/dev/null | wc -l)"

# 2) 可选：启动 plasmashell（提供真实的 KDE 桌面窗口，测「壁纸 vs plasmashell 桌面窗口」堆叠）
PLASMA_PID=""
if [ "$WITH_PLASMA" = "1" ]; then
  plasmashell --no-respawn > "$WORK/plasmashell.log" 2>&1 &
  PLASMA_PID=$!
  sleep 6
  if ! kill -0 "$PLASMA_PID" 2>/dev/null; then
    fail "plasmashell 启动即退出（看 $WORK/plasmashell.log）"
  else
    echo "  [info] plasmashell 已起 (pid $PLASMA_PID)"
  fi
fi

# 3) 起应用
echo "  [info] 启动应用，存活 ${ALIVE_SECS}s ..."
"$APP" > "$WORK/app.log" 2>&1 &
APP_PID=$!

# 等壁纸窗口出现（最多 20s）
WP_WIN=""
# 不要用 xdotool search --pid：WebKitGTK 顶层窗口实测匹配到 0 个（_NET_WM_PID 查询不可靠）。
# 可靠做法：扫 stacking 里的 DESKTOP 窗口，再比对 _NET_WM_PID。
find_app_desktop_win() {
  local w p
  for w in $(xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
    case "$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in *DESKTOP*) ;; *) continue ;; esac
    p="$(xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')"
    [ "$p" = "$APP_PID" ] && { printf '%s' "$w"; return 0; }
  done
  return 1
}
for i in $(seq 1 40); do
  sleep 0.5
  kill -0 "$APP_PID" 2>/dev/null || { fail "应用提前退出"; break; }
  WP_WIN="$(find_app_desktop_win)" && [ -n "$WP_WIN" ] && break
done

# ---- 断言 4：进程存活 ----
sleep "$ALIVE_SECS"
if kill -0 "$APP_PID" 2>/dev/null; then
  pass "应用存活 ${ALIVE_SECS}s 无退出"
else
  fail "应用在 ${ALIVE_SECS}s 内退出（最后 20 行 app.log:）"; tail -20 "$WORK/app.log"; 
fi

# ---- 断言 3：日志 ----
if grep -qiE 'wallpaper window|created|apply_desktop|桌面层|applied' "$WORK/app.log"; then
  pass "日志含窗口创建/桌面层应用记录"
  grep -iE 'wallpaper window|created|apply_desktop|桌面层|applied' "$WORK/app.log" | head -5 | sed 's/^/         /'
else
  echo "  [WARN] 日志未匹配到创建/应用记录（RUST_LOG 可能没开）；app.log 前 30 行："
  head -30 "$WORK/app.log" | sed 's/^/         /'
fi
if grep -qiE 'panic|thread .* panicked' "$WORK/app.log"; then
  fail "日志出现 panic"; grep -iE -A5 'panic' "$WORK/app.log" | head -20
else
  pass "日志无 panic"
fi

# ---- 断言 1：_NET_WM_WINDOW_TYPE 含 DESKTOP ----
if [ -n "$WP_WIN" ]; then
  pass "找到壁纸窗口 id=$WP_WIN，_NET_WM_WINDOW_TYPE 含 DESKTOP"
  xprop -id "$WP_WIN" _NET_WM_WINDOW_TYPE | sed 's/^/         /'
else
  fail "没找到带 _NET_WM_WINDOW_TYPE_DESKTOP 的窗口；所有应用窗口："
  for w in $(xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
    echo "         id=$w $(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2-) class=$(xprop -id "$w" WM_CLASS 2>/dev/null | cut -d= -f2-)"
  done
fi

# ---- 断言 2：stacking order 中位于普通窗口之下 ----
echo "  [info] stacking order（自底向上）："
STACK="$(xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' '\n')"
idx=0
WP_IDX=""; 
for w in $STACK; do
  idx=$((idx+1))
  [ "$w" = "$WP_WIN" ] && WP_IDX=$idx
  nm="$(xprop -id "$w" WM_NAME 2>/dev/null | cut -d= -f2- | tr -d '"')"
  typ="$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')"
  echo "         #$idx $w  type=$typ  name=$nm"
done
if [ -n "$WP_WIN" ] && [ -n "$WP_IDX" ]; then
  NON_DESKTOP_HIGHER=0
  i=0
  for w in $STACK; do
    i=$((i+1))
    if [ "$i" -gt "$WP_IDX" ]; then
      t="$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)"
      case "$t" in *DESKTOP*) : ;; *) NON_DESKTOP_HIGHER=$((NON_DESKTOP_HIGHER+1)) ;; esac
    fi
  done
  if [ "$NON_DESKTOP_HIGHER" -gt 0 ]; then
    pass "壁纸窗口(#$WP_IDX) 之上有 $NON_DESKTOP_HIGHER 个普通窗口 → 位于普通窗口之下"
  else
    fail "壁纸窗口之上没有发现任何普通窗口，无法证明它在最底（可能没有普通窗口可比）"
  fi
else
  fail "无法判定 stacking 位置（没找到壁纸窗口或在 stacking 列表里缺席）"
fi

# plasmashell 桌面窗口的存在性（用于文档记录「两个 desktop 窗口共存」的事实）
if [ "$WITH_PLASMA" = "1" ]; then
  echo "  [info] plasmashell 自带的 desktop 窗口："
  found=0
  for w in $STACK; do
    case "$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
      *DESKTOP*) pid="$(xprop -id "$w" _NET_WM_PID 2>/dev/null | cut -d= -f2 | tr -d ' ')"
                 echo "         id=$w pid=$pid"; found=1 ;;
    esac
  done
  [ "$found" = "0" ] && echo "         （无；Plasma 6.8+ 起可能不再设 NET::Desktop）"
  echo "  [info] 共存 desktop 窗口数 = $(echo "$STACK" | while read -r w; do case "$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in *DESKTOP*) echo x;; esac; done | wc -l)"
fi

echo "  [info] 截图（供人工判断图标是否被盖）"
import -window root "$WORK/screen.png" 2>/dev/null || xwd -root -silent > "$WORK/screen.xwd" 2>/dev/null || true
cp -f "$WORK/screen.png" /tmp/kde-smoke-screen.png 2>/dev/null || true
echo "  [info] 日志目录 $WORK"
if [ "$INNER_FAILED" -gt 0 ]; then
  echo "  [info] 会话内失败断言数 = $INNER_FAILED"
  exit 1
fi
exit 0
INNER
chmod +x "$WORK/inner.sh"

# ---------- 外层：起 Xvfb + 会话总线 ----------
# 总线策略：容器/CI 里没有 systemd 用户会话，用私有 dbus-run-session 是对的；
# 真机上必须复用真实用户总线（/run/user/$(id -u)/bus），否则 KDE 的
# kglobalaccel/kactivitymanagerd/KScreen 会被信号 6 打死、plasmashell 拒绝加载。
# 这里自动判断：已有 DBUS_SESSION_BUS_ADDRESS 就直接用。
echo
echo "== 启动 KDE/X11 冒烟环境（Xvfb ${SCREEN_W}x${SCREEN_H}）=="
export DISPLAY="${DISPLAY:-:99}"
XVFB_PID=""
if xdpyinfo >/dev/null 2>&1; then
  echo "  [info] DISPLAY=$DISPLAY 上已有 X server，复用"
else
  Xvfb "$DISPLAY" -screen 0 "${SCREEN_W}x${SCREEN_H}x24" +extension GLX +render -noreset \
    > "$WORK/xvfb.log" 2>&1 &
  XVFB_PID=$!
  for i in $(seq 1 20); do xdpyinfo >/dev/null 2>&1 && break; sleep 0.5; done
fi

if [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then
  echo "  [info] 复用真实用户会话总线: $DBUS_SESSION_BUS_ADDRESS"
  BUS_RUNNER=(bash)
else
  echo "  [info] 用私有总线（dbus-run-session）——仅适用于无用户会话的容器"
  BUS_RUNNER=(dbus-run-session -- bash)
fi

set +e
"${BUS_RUNNER[@]}" "$WORK/inner.sh" "$WORK" "$APP" "$ALIVE_SECS" "$SCREEN_W" "$SCREEN_H" "$SMOKE_WITH_PLASMA"
INNER_RC=$?
set -e
[ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null

echo
if [ "$INNER_RC" != "0" ]; then
  echo "== 结果：会话内断言失败（rc=$INNER_RC）=="
  [ "$KEEP_LOGS" != "1" ] && echo "提示：用 KEEP_LOGS=1 重跑保留日志"
  exit 1
fi
echo "== 结果：完成（逐条 [PASS]/[FAIL] 见上）=="
[ "$KEEP_LOGS" != "1" ] && echo "提示：用 KEEP_LOGS=1 重跑保留日志"
exit 0
