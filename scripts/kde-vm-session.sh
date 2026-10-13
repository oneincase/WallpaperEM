#!/usr/bin/env bash
# 在**无头** VM 里拉起一个真实的 KDE Plasma/X11 会话，供 kde-probes.sh 与
# linux-kde-smoke.sh 使用。把「SSH 进来只有 tty」变成「有 KWin 在跑」。
#
# 三个必须踩对的点（都栽过）：
#   1) 用**真实 systemd 用户会话总线**（/run/user/1000/bus），不要用 dbus-run-session
#      起私有总线——私有总线上没有 kglobalaccel / kactivitymanagerd / KScreen 的
#      激活文件，它们会立刻 SIGABRT，plasmashell 则直接 "Aborting shell load"。
#   2) Xvfb 与 KWin 必须 setsid 脱离 SSH 会话，否则登录 shell 退出时 SIGHUP 把它们带走，
#      kwin.log 会留下 "The X11 connection broke (error 1). Did the X11 server die?"。
#   3) XDG_RUNTIME_DIR 用 /run/user/1000（systemd 已经建好，权限 700），不要自造。
#
# 用法：
#   bash scripts/kde-vm-session.sh probes      # 机制探针（不需要应用二进制）
#   bash scripts/kde-vm-session.sh realbuscase # 应用冒烟，真实总线（推荐）
#   bash scripts/kde-vm-session.sh smoke       # 应用冒烟，走 linux-kde-smoke.sh（私有 dbus）
#   bash scripts/kde-vm-session.sh shell       # 起会话后进交互 shell
#
# 环境变量：DISP=:99  SCREEN=1280x800x24  KEEP=1  LOGDIR=/tmp/kde-session-$$
# 退出码：0 = 内层成功；1 = 会话起不来或内层失败。

set -uo pipefail

MODE="${1:-probes}"
REPO="${REPO:-$HOME/WallpaperEM}"
DISP="${DISP:-:99}"
SCREEN="${SCREEN:-1280x800x24}"
KEEP="${KEEP:-0}"
LOGDIR="${LOGDIR:-/tmp/kde-session-$$}"
RUNTIME="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"

say() { printf '\n=== %s ===\n' "$*"; }
ok()  { printf '  [OK]   %s\n' "$*"; }
bad() { printf '  [FAIL] %s\n' "$*"; }

for c in Xvfb kwin_x11 xprop; do
  command -v "$c" >/dev/null 2>&1 || { echo "缺少 $c（先跑 scripts/kde-vm-build.sh 装依赖）"; exit 1; }
done

mkdir -p "$LOGDIR"
say "会话 $LOGDIR（DISP=$DISP SCREEN=$SCREEN MODE=$MODE）"

# ---- 1. 确认真实用户会话总线可用 ----
say "用户会话总线"
BUS="${DBUS_SESSION_BUS_ADDRESS:-}"
if [ -z "$BUS" ] && [ -S "$RUNTIME/bus" ]; then BUS="unix:path=$RUNTIME/bus"; fi
if [ -z "$BUS" ]; then
  bad "没有 DBUS_SESSION_BUS_ADDRESS，也没有 $RUNTIME/bus"
  echo "  KDE 服务需要真实用户会话总线。请确认以该用户登录的系统会话在跑："
  echo "    systemctl --user is-system-running"
  exit 1
fi
export DBUS_SESSION_BUS_ADDRESS="$BUS"
echo "  DBUS_SESSION_BUS_ADDRESS=$BUS"
if systemctl --user is-system-running >/dev/null 2>&1; then
  ok "systemd 用户会话: $(systemctl --user is-system-running 2>&1)"
else
  echo "  [WARN] systemctl --user 不可用，仍继续"
fi

# ---- 2. 起 Xvfb（setsid 脱离 SSH）----
say "启动 Xvfb $DISP"
XVFB_PID=""
if DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1; then
  echo "  $DISP 上已有 X server，复用"
else
  setsid Xvfb "$DISP" -screen 0 "$SCREEN" +extension GLX +render -noreset > "$LOGDIR/xvfb.log" 2>&1 < /dev/null &
  XVFB_PID=$!
  for i in $(seq 1 40); do
    DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1 && break
    sleep 0.5
  done
  if DISPLAY="$DISP" xdpyinfo >/dev/null 2>&1; then
    ok "Xvfb 就绪（pid $XVFB_PID）"
  else
    bad "Xvfb 起不来"; tail -10 "$LOGDIR/xvfb.log"; exit 1
  fi
fi

KWIN_PID=""; PLASMA_PID=""
cleanup() {
  [ -n "${KWIN_PID:-}" ] && kill "$KWIN_PID" 2>/dev/null
  [ -n "${PLASMA_PID:-}" ] && kill "$PLASMA_PID" 2>/dev/null
  [ -n "${XVFB_PID:-}" ] && kill "$XVFB_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT

# ---- 3. 起 KWin（setsid），等它接管 WM ----
say "启动 kwin_x11 --replace"
setsid env DISPLAY="$DISP" XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true \
  KDE_SESSION_VERSION=5 XDG_SESSION_TYPE=x11 GDK_BACKEND=x11 \
  LIBGL_ALWAYS_SOFTWARE=1 \
  kwin_x11 --replace > "$LOGDIR/kwin.log" 2>&1 < /dev/null &
KWIN_PID=$!
for i in $(seq 1 60); do
  DISPLAY="$DISP" xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1 && break
  sleep 0.5
done
if DISPLAY="$DISP" xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1; then
  ok "KWin 接管 WM: $(DISPLAY=$DISP xprop -root _NET_WM_NAME 2>/dev/null | head -1)"
else
  bad "KWin 没接管 WM；kwin.log 尾部："; tail -8 "$LOGDIR/kwin.log"
fi

# ---- 4. 起 plasmashell ----
say "启动 plasmashell"
if command -v plasmashell >/dev/null 2>&1; then
  setsid env DISPLAY="$DISP" XDG_CURRENT_DESKTOP=KDE KDE_FULL_SESSION=true \
    KDE_SESSION_VERSION=5 XDG_SESSION_TYPE=x11 QT_QPA_PLATFORM=xcb \
    LIBGL_ALWAYS_SOFTWARE=1 \
    plasmashell --no-respawn > "$LOGDIR/plasmashell.log" 2>&1 < /dev/null &
  PLASMA_PID=$!
  sleep 20
  if kill -0 "$PLASMA_PID" 2>/dev/null; then
    ok "plasmashell 存活（pid $PLASMA_PID）"
  else
    bad "plasmashell 退出；日志尾部："; tail -12 "$LOGDIR/plasmashell.log"
  fi
else
  echo "  plasmashell 不存在，跳过"
fi

say "会话快照"
DISPLAY="$DISP" xprop -root 2>/dev/null | grep -E '_NET_(CLIENT_LIST|SUPPORTING|DESKTOP|NUMBER_OF_DESKTOPS|CURRENT_DESKTOP)' | sed 's/^/  /'

# ---- 5. 按模式执行 ----
INNER_ENV=(DISPLAY="$DISP" DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_CURRENT_DESKTOP=KDE
           KDE_FULL_SESSION=true KDE_SESSION_VERSION=5 XDG_SESSION_TYPE=x11
           XDG_RUNTIME_DIR="$RUNTIME")
case "$MODE" in
  probes)
    say "kde-probes.sh"
    env "${INNER_ENV[@]}" bash "$REPO/scripts/kde-probes.sh"; RC=$?
    ;;
  smoke)
    say "linux-kde-smoke.sh"
    env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb \
      bash "$REPO/scripts/linux-kde-smoke.sh"; RC=$?
    ;;
  realbuscase)
    # 真实会话总线上的应用冒烟：断言与 linux-kde-smoke.sh 同义，但不另起
    # dbus-run-session（KDE 服务需要真实总线），直接复用本脚本已拉起的
    # Xvfb + KWin + plasmashell。X11 侧「壁纸窗口落位」的唯一实测手段。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "应用冒烟（真实总线）APP=$APP"
    if [ ! -x "$APP" ]; then bad "二进制不存在: $APP（先在 VM 上 cargo build --release）"; exit 2; fi
    RC=0
    INNER_FAILED=0
    fpass() { printf '  \033[32m[PASS]\033[0m %s\n' "$*"; }
    ffail() { printf '  \033[31m[FAIL]\033[0m %s\n' "$*"; INNER_FAILED=$((INNER_FAILED+1)); }
    finfo() { printf '  [info] %s\n' "$*"; }
    APPORT="$LOGDIR/realbuscase.log"
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb \
      LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$APPORT" 2>&1 < /dev/null &
    APP_PID=$!
    # 找我们的壁纸窗口：**按 _NET_WM_PID 反查，不要用 xdotool search --pid**。
    # WebKitGTK 的顶层窗口常常没有设 _NET_WM_PID（本轮实测：PID 匹配到 0 个，而
    # 窗口明明在 stacking 里且 type=DESKTOP、pid 就是我们）。扫 stacking 里的
    # DESKTOP 窗口再比对 pid 才可靠。
    find_app_desktop_win() {
      local w p
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in *DESKTOP*) ;; *) continue ;; esac
        p="$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')"
        [ "$p" = "$APP_PID" ] && { printf '%s' "$w"; return 0; }
      done
      return 1
    }

    shot() { DISPLAY=$DISP import -window root "$1" 2>/dev/null \
             || DISPLAY=$DISP xwd -root -silent > "${1%.png}.xwd" 2>/dev/null || true; }
    # 桌面图标区（避开底部 panel）：截图该区域并算哈希，用于「上/下」判定
    icon_hash() {
      local img="$1"
      [ -f "$img" ] || { printf 'no-shot'; return; }
      convert "$img" -crop 640x400+0+0 +repage -colorspace Gray -format '%#' info: 2>/dev/null \
        || md5sum "$img" | cut -c1-12
    }

    WP_WIN=""
    for i in $(seq 1 60); do
      sleep 0.5
      kill -0 "$APP_PID" 2>/dev/null || { ffail "应用提前退出（见 $APPORT）"; break; }
      WP_WIN="$(find_app_desktop_win)" && [ -n "$WP_WIN" ] && break
    done
    ALIVE="${ALIVE_SECS:-30}"
    finfo "存活观察 ${ALIVE}s"
    sleep "$ALIVE"

    # 先截「壁纸窗口在世」这张（后面的断言还要用 stacking，所以此处不杀应用）
    shot "$LOGDIR/screen-with-app.png"
    HASH_WITH="$(icon_hash "$LOGDIR/screen-with-app.png")"

    if kill -0 "$APP_PID" 2>/dev/null; then fpass "应用存活 ${ALIVE}s 无退出"
    else ffail "应用在 ${ALIVE}s 内退出"; tail -20 "$APPORT" | sed 's/^/         /'; fi

    if grep -qiE 'panic|thread .* panicked' "$APPORT"; then
      ffail "日志出现 panic"; grep -iE -A5 'panic' "$APPORT" | head -20
    else fpass "日志无 panic"; fi

    finfo "应用窗口清单（按 _NET_WM_PID 反查）"
    for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
      p="$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')"
      [ "$p" = "$APP_PID" ] && echo "         id=$w $(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2-) class=$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | cut -d= -f2-)"
    done
    if [ -n "$WP_WIN" ]; then
      fpass "壁纸窗口 id=$WP_WIN 的 _NET_WM_WINDOW_TYPE 含 DESKTOP"
      DISPLAY=$DISP xprop -id "$WP_WIN" _NET_WM_WINDOW_TYPE | sed 's/^/         /'
      DISPLAY=$DISP xprop -id "$WP_WIN" _NET_WM_STATE WM_CLASS WM_NAME _NET_WM_PID 2>/dev/null | sed 's/^/         /'
    else
      ffail "未找到 _NET_WM_WINDOW_TYPE_DESKTOP 窗口"
    fi

    finfo "stacking（自底向上）"
    STACK_OUT="$(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' '\n')"
    idx=0; WP_IDX=""
    for w in $STACK_OUT; do
      idx=$((idx+1)); [ "$w" = "$WP_WIN" ] && WP_IDX=$idx
      echo "         #$idx $w type=$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//') pid=$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | cut -d= -f2 | tr -d ' ')"
    done
    if [ -n "$WP_IDX" ]; then
      HIGH=0; i=0
      for w in $STACK_OUT; do
        i=$((i+1))
        [ "$i" -gt "$WP_IDX" ] && case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*) : ;; *) HIGH=$((HIGH+1)) ;; esac
      done
      if [ "$HIGH" -gt 0 ]; then fpass "壁纸窗口(#$WP_IDX) 之上有 $HIGH 个普通窗口 → 位于普通窗口之下"
      else ffail "壁纸窗口之上无普通窗口，无法证明在最底"; fi
    else ffail "壁纸窗口不在 stacking 列表中"; fi

    finfo "共存 desktop 窗口数 = $(echo "$STACK_OUT" | while read -r w; do case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in *DESKTOP*) echo x;; esac; done | grep -c x)"
    # 每个 desktop 窗口的 pid/class/state：判定「谁压着谁」靠这两张图 + 这份属性
    finfo "所有 desktop 窗口属性"
    for w in $STACK_OUT; do
      case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
        *DESKTOP*) echo "         $w pid=$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$') class=$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | cut -d= -f2-) state=$(DISPLAY=$DISP xprop -id "$w" _NET_WM_STATE 2>/dev/null | cut -d= -f2-)"
                   echo "            frame=$(DISPLAY=$DISP xdotool getwindowgeometry "$w" 2>/dev/null | tr '\n' ' ')" ;;
      esac
    done
    # 像素级取证：杀掉壁纸窗口后再截一张。两张图里桌面图标区必须不同——
    # 不同 ⇒ 壁纸确实画在最上层（图标被盖住，即「壁纸在上」）；
    # 相同 ⇒ plasmashell 的桌面窗口压着壁纸，壁纸根本不可见。
    kill "$APP_PID" 2>/dev/null; sleep 3
    shot "$LOGDIR/screen-without-app.png"
    HASH_WITHOUT="$(icon_hash "$LOGDIR/screen-without-app.png")"
    finfo "图标区哈希 有应用=$HASH_WITH 无应用=$HASH_WITHOUT"
    if [ "$HASH_WITH" != "$HASH_WITHOUT" ] && [ "$HASH_WITH" != "no-shot" ]; then
      ffail "壁纸窗口改变了桌面图标区像素 → 壁纸盖在 plasmashell 桌面视图之上（图标被盖，本轮为已知降级）"
    else
      fpass "杀掉壁纸窗口前后图标区像素一致 → 本轮壁纸未盖住桌面视图"
    fi
    # 有应用 / 无应用两张全屏图（图标区哈希已在上方算出）
    [ -f "$LOGDIR/screen-with-app.png" ] && cp -f "$LOGDIR/screen-with-app.png" /tmp/kde-smoke-screen.png 2>/dev/null
    finfo "截图: $LOGDIR/screen-with-app.png（壁纸窗口在世）与 screen-without-app.png（已杀）"
    [ "$INNER_FAILED" -gt 0 ] && RC=1
    echo "  应用日志: $APPORT"
    ;;
  shell)
    say "交互 shell（exit 结束会话）"
    env "${INNER_ENV[@]}" bash -i; RC=0
    ;;
  belowtest)
    # 置底可行性实测：plasmashell 自己的 desktop 窗口带 _NET_WM_STATE_BELOW
    # （实测属性），所以给壁纸窗口加同一个 state，看 KWin 会不会把它压到
    # plasmashell 的桌面视图之下——那正是「图标可见」的必要条件。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "置底实测 APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    stack_rank() { # 输出 "名次 总窗口数 该窗口 type"（自底向上 #1 为底）
      local target="$1" i=0 tot=0 r=""
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1)); tot=$i
        [ "$w" = "$target" ] && r="$i"
      done
      printf '%s/%s %s' "${r:-?}" "$tot" "$(DISPLAY=$DISP xprop -id "$target" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')"
    }
    dump_stack() {
      local i=0
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        echo "         #$i $w $(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//') class=$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | cut -d= -f2-) state=$(DISPLAY=$DISP xprop -id "$w" _NET_WM_STATE 2>/dev/null | cut -d= -f2-)"
      done
    }
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$LOGDIR/belowtest.log" 2>&1 < /dev/null &
    APP_PID=$!
    WP=""
    for i in $(seq 1 60); do
      sleep 0.5
      kill -0 "$APP_PID" 2>/dev/null || break
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in *DESKTOP*) ;; *) continue ;; esac
        [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$APP_PID" ] && { WP="$w"; break 2; }
      done
    done
    if [ -z "$WP" ]; then bad "没找到壁纸窗口"; dump_stack; exit 1; fi
    echo "  壁纸窗口 $WP"
    echo "--- A. 现状（仅 DESKTOP type）---"; dump_stack
    A="$(stack_rank "$WP")"; echo "  名次(底=1): $A"
    echo
    echo "--- B. 加 _NET_WM_STATE_BELOW 之后 ---"
    DISPLAY=$DISP xprop -id "$WP" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_BELOW 2>/dev/null
    sleep 2; dump_stack
    B="$(stack_rank "$WP")"; echo "  名次(底=1): $B"
    echo
    echo "--- C. 去掉 DESKTOP type（改回 NORMAL）+ 保持 BELOW ---"
    DISPLAY=$DISP xprop -id "$WP" -f _NET_WM_WINDOW_TYPE 32a -set _NET_WM_WINDOW_TYPE _NET_WM_WINDOW_TYPE_NORMAL 2>/dev/null
    DISPLAY=$DISP xprop -id "$WP" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_BELOW 2>/dev/null
    sleep 2; dump_stack
    C="$(stack_rank "$WP")"; echo "  名次(底=1): $C"
    echo
    echo "=== 结论数据 ==="
    echo "  A 仅 DESKTOP      : $A"
    echo "  B DESKTOP+BELOW   : $B"
    echo "  C NORMAL+BELOW    : $C"
    echo "  （plasmashell 桌面窗口通常在最底；名次越小越接近「壁纸在桌面视图之下」）"
    kill "$APP_PID" 2>/dev/null
    RC=0
    ;;
  *) echo "未知模式: $MODE"; exit 1 ;;
esac

say "日志尾部"
echo "--- kwin.log（末 12 行）---"; tail -12 "$LOGDIR/kwin.log" 2>/dev/null
echo "--- plasmashell.log（末 12 行）---"; tail -12 "$LOGDIR/plasmashell.log" 2>/dev/null

say "完成"
echo "  内层退出码: $RC"
echo "  日志: $LOGDIR$([ "$KEEP" = "1" ] && echo '（已保留）' || echo '（KEEP=1 可保留）')"
exit "$RC"
