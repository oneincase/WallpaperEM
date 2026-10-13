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
#   bash scripts/kde-vm-session.sh belowtest   # 置底可行性：DESKTOP/NORMAL + BELOW 三态对照
#   bash scripts/kde-vm-session.sh ruleinstall # KWin 窗口规则实测（kwritconfig 写 + reconfigure + 回读）
#   bash scripts/kde-vm-session.sh p1e         # keep-below × 透明桌面容器组合测试（P1 spike 第五轮）
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

# qdbus 在不同发行版/版本上叫 qdbus5/qdbus6/qdbus。Plasma 5 是 qdbus5，Plasma 6 是 qdbus6，
# 但 Ubuntu 的 qt5-tools 也提供名为 qdbus 的二进制。逐个探测，别写死。
QDBUS_BIN=""
for c in qdbus qdbus6 qdbus5 qdbus-qt5 qdbus-qt6; do
  command -v "$c" >/dev/null 2>&1 && { QDBUS_BIN="$c"; break; }
done
echo "  qdbus 二进制: ${QDBUS_BIN:-（未找到）}"

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
  ruleinstall)
    # KWin 窗口规则实测（P2）：桌面层壁纸窗口能不能靠 kwinrulesrc 被压到 plasmashell
    # 桌面视图之下——那是「桌面图标可见」的充要条件。
    #
    # 三段对照，全在同一个会话里：
    #   A 基线：只装规则（Layer 未设）→ 看壁纸与桌面视图的名次
    #   B 装规则 + Layer=4（Force，即 Keep-below）→ 期望壁纸沉到桌面视图之下
    #   C 用 kwriteconfig 写一份不存在的键，证明「写了不等于 KWin 认了」
    #
    # 回读用 kwriteconfig5/6 --list（KDE 自己解析），不用 grep —— 顺手避免 grep 的
    # 管道退出码问题。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "窗口规则实测 APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    KW=""
    for c in kwriteconfig6 kwriteconfig5; do
      command -v "$c" >/dev/null 2>&1 && { KW="$c"; break; }
    done
    [ -n "$KW" ] || { bad "找不到 kwriteconfig5/6"; exit 2; }
    echo "  用 $KW 读写 ~/.config/kwinrulesrc"

    # 备份用户既有规则（只在本脚本内恢复；这是验证机，但别毁掉别人的配置）
    RULES="$HOME/.config/kwinrulesrc"
    RULES_BAK="$LOGDIR/kwinrulesrc.bak"
    if [ -f "$RULES" ]; then cp -f "$RULES" "$RULES_BAK"; else : > "$RULES_BAK"; fi

    stack_of() { # 输出 stacking 顺序（自底向上），每行 "#名次 windowid type"
      local i=0
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        echo "#$i $w $(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')"
      done
    }
    name_of() { # WM_CLASS 的实例名（第二个字段），用于找 plasmashell 的桌面视图
      DISPLAY=$DISP xprop -id "$1" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/'
    }
    rank_of() {
      local target="$1" i=0 r=""
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1)); [ "$w" = "$target" ] && r="$i"
      done
      printf '%s' "${r:-?}"
    }
    evidence() { # KWin 把结果反射到 X11 属性上的证据（规则有没有被采纳看这里）
      local w="$1"
      printf '        _NET_WM_STATE            = %s\n' "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_STATE 2>/dev/null | cut -d= -f2- | tr -d ' ')"
      printf '        _NET_WM_DESKTOP          = %s\n' "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_DESKTOP 2>/dev/null | cut -d= -f2-)"
      printf '        _NET_WM_WINDOW_TYPE      = %s\n' "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | tr -d ' ')"
    }
    xprop_hash() { # 一个窗口的全部属性做哈希：属性变了说明 KWin 真采纳了规则
      DISPLAY=$DISP xprop -id "$1" 2>/dev/null | sed "s/$1/WIN/g" | sort | md5sum | cut -c1-12
    }
    wait_wallpaper() { # 等应用建出 desktop 窗口
      local pid="$1" i w
      for i in $(seq 1 60); do
        sleep 0.5
        kill -0 "$pid" 2>/dev/null || return 1
        for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
          case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
            *DESKTOP*)
              [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$pid" ] && { printf '%s' "$w"; return 0; }
              ;;
          esac
        done
      done
      return 1
    }
    start_app() {
      setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
        "$APP" > "$LOGDIR/ruleinstall-app.log" 2>&1 < /dev/null &
      APP_PID=$!
      WP="$(wait_wallpaper "$APP_PID")" || { bad "壁纸窗口没出现"; APPLICATION_READY=0; return; }
      APPLICATION_READY=1
      echo "  壁纸窗口 $WP (pid $APP_PID)"
    }
    stop_app() {
      [ -n "${APP_PID:-}" ] && kill "$APP_PID" 2>/dev/null
      sleep 2
      APP_PID=""
    }

    # ---- A 基线：先清掉我们可能留下的规则，起应用 ----
    rm -f "$RULES"
    "$KW" --file kwinrulesrc --group General --key count 0 >/dev/null 2>&1
    DISPLAY=$DISP qdbus org.kde.KWin /KWin reconfigure >/dev/null 2>&1
    sleep 1
    start_app
    if [ "${APPLICATION_READY:-0}" = 1 ]; then
      echo "--- A. 基线（无规则）stacking ---"; stack_of | sed 's/^/     /'
      echo "    壁纸名次(底=1): $(rank_of "$WP")"
      WP_RANK_A="$(rank_of "$WP")"
      echo "    规则前 xprop 证据："; evidence "$WP"
      WP_HASH_BEFORE="$(xprop_hash "$WP")"
      echo "    规则前属性哈希: $WP_HASH_BEFORE"
      # plasmashell 的桌面视图：class 为 plasmashell 且 type=DESKTOP 的窗口（排除我们自己的）
      DESKTOP_VIEW=""
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*)
            case "$(name_of "$w")" in
              plasmashell*) DESKTOP_VIEW="$w" ;;
            esac
            ;;
        esac
      done
      echo "    plasmashell 桌面视图窗口: ${DESKTOP_VIEW:-（没找到）}"
      [ -n "$DESKTOP_VIEW" ] && echo "    桌面视图名次(底=1): $(rank_of "$DESKTOP_VIEW")"
      stop_app
    fi

    # ---- B 装规则（Layer=Force Keep-below + skipTaskbar + 不接收焦点 + 全桌面）----
    echo
    echo "--- B. 写入窗口规则 ---"
    "$KW" --file kwinrulesrc --group General --key count 1 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key Description "WallpaperEM KDE 验证规则" >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key wmclass WallpaperEM >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key wmclassmatch 3 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key layer 4 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key layerrule 1 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key skiptaskbar true >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key skiptaskbarrule 2 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key acceptfocus false >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key acceptfocusrule 2 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key desktops 0 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-verify --key desktopsrule 1 >/dev/null 2>&1
    echo "  KDE 回读（cat ~/.config/kwinrulesrc；kwriteconfig5 没有 --list）："
    cat "$RULES" 2>/dev/null | sed 's/^/    /'
    RECONF="$(DISPLAY=$DISP qdbus org.kde.KWin /KWin reconfigure 2>&1)"
    echo "  qdbus reconfigure → ${RECONF:-（无输出 = 正常）}"
    sleep 2
    DISPLAY=$DISP qdbus org.kde.KWin /KWin supportInformation > "$LOGDIR/kwin-support.txt" 2>/dev/null
    if [ -s "$LOGDIR/kwin-support.txt" ]; then
      echo "  supportInformation 里含规则的行："
      grep -iE "rule|kwinrules" "$LOGDIR/kwin-support.txt" | head -8 | sed 's/^/    /'
    else
      echo "  [WARN] supportInformation 取不到（KWin 5.27 上可能没这个 D-Bus 方法）"
    fi
    start_app
    if [ "${APPLICATION_READY:-0}" = 1 ]; then
      echo "--- B. 规则生效后 stacking ---"; stack_of | sed 's/^/     /'
      echo "    壁纸名次(底=1): $(rank_of "$WP")"
      WP_RANK_B="$(rank_of "$WP")"
      echo "    规则后 xprop 证据（同一次启动，比 before/after 才有意义）："; evidence "$WP"
      WP_HASH_AFTER="$(xprop_hash "$WP")"
      echo "    规则后属性哈希: $WP_HASH_AFTER"
      [ "${WP_HASH_BEFORE:-x}" = "$WP_HASH_AFTER" ] && echo "    → 属性无变化：KWin 可能没采纳规则" || echo "    → 属性有变化：KWin 采纳了规则"
      if [ -n "${DESKTOP_VIEW:-}" ]; then
        echo "    桌面视图名次(底=1): $(rank_of "$DESKTOP_VIEW")"
      fi
      stop_app
    fi

    # ---- C 反例：写一个 KWin 不认识的键，看它是否被接受 ----
    echo
    echo "--- C. 反例（KWin 不认识的键 + 非法 layer 值）---"
    "$KW" --file kwinrulesrc --group wallpaperem-bogus --key layer 99 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-bogus --key layerrule 7 >/dev/null 2>&1
    "$KW" --file kwinrulesrc --group wallpaperem-bogus --key totallyfakekey yes >/dev/null 2>&1
    cat "$RULES" 2>/dev/null | sed 's/^/    /'
    DISPLAY=$DISP qdbus org.kde.KWin /KWin reconfigure >/dev/null 2>&1
    sleep 1
    echo "  kwin.log 里与规则相关的行（末 10 行）："
    grep -iE "rule|wallpaperem" "$LOGDIR/kwin.log" 2>/dev/null | tail -10 | sed 's/^/    /' || echo "    （无）"

    # 恢复用户配置
    cp -f "$RULES_BAK" "$RULES" 2>/dev/null || rm -f "$RULES"
    DISPLAY=$DISP qdbus org.kde.KWin /KWin reconfigure >/dev/null 2>&1

    echo
    echo "=== 结论数据 ==="
    echo "  基线名次(底=1)        : ${WP_RANK_A:-?}"
    echo "  基线属性哈希          : ${WP_HASH_BEFORE:-?}"
    echo "  规则后属性哈希        : ${WP_HASH_AFTER:-?}"
    echo "  装规则后名次(底=1)    : ${WP_RANK_B:-?}"
    echo "  判读：> 说明规则把壁纸压得更低了；= 说明规则没影响层序（KWin 5.27 上"
    echo "        Layer 规则可能是 unused，真机结论要按 Plasma 6 复核）"
    echo "  用户 kwinrulesrc 已从 $RULES_BAK 恢复"
    RC=0
    ;;
  layer)
    # P1 的决定性实验：KWin 到底把壁纸窗口放在哪一层。
    #
    # 为什么这是决定性的：KDE 上「桌面图标会不会被盖住」只由**层**决定，而层不是
    # kwinrulesrc 能直接指定的（layer 规则长期 unused）。KWin 自己的判定只能问 KWin：
    # 用它的脚本 API 读出每个窗口的 layer / windowType，再对着 ek 的 X11 属性交叉验证。
    # 判读：
    #   wallpaper 在 DesktopLayer(5) 且 plasmashell 桌面视图在 BelowLayer(6) 之上 ⇒ 图标被盖
    #   wallpaper 在 BelowLayer(6)（桌面视图之下）⇒ 图标可见，机制①成立
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "KWin 层实测 APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    RC=0
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$LOGDIR/layer-app.log" 2>&1 < /dev/null &
    APP_PID=$!
    WP=""
    for i in $(seq 1 60); do
      sleep 0.5
      kill -0 "$APP_PID" 2>/dev/null || break
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*)
            [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$APP_PID" ] && { WP="$w"; break; }
            ;;
        esac
      done
      [ -n "$WP" ] && break
    done
    if [ -z "$WP" ]; then
      bad "壁纸窗口没出现；应用日志尾部："; tail -20 "$LOGDIR/layer-app.log"
      RC=1
    else
      ok "壁纸窗口 $WP (pid $APP_PID)"
      echo
      echo "--- stacking（自底向上 #名次 windowid type）---"
      i=0
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '  #%d %s %s  class=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')"
      done
      echo
      echo "--- 有 KWin 脚本 API 读层（唯一权威来源）---"
      DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" XDG_RUNTIME_DIR="$RUNTIME" \
        KWIN_LAYER_MATCH=WallpaperEM bash "$REPO/scripts/kde-kwin-layer.sh" || RC=1
      echo
      echo "--- 截图（人工看桌面图标是否可见）---"
      if command -v import >/dev/null 2>&1; then
        DISPLAY=$DISP import -window root "$LOGDIR/kde-layer-screen.png" 2>/dev/null \
          && ok "已存 $LOGDIR/kde-layer-screen.png" || echo "  截图失败"
      fi
    fi
    kill "$APP_PID" 2>/dev/null
    sleep 1
    ;;
  p1)
    # P1 spike：桌面层落位矩阵。回答的问题只有一个——
    #   KDE 上能不能做到「壁纸可见 **且** 桌面图标浮在壁纸之上可点」？
    #
    # 已知事实（上一轮实测）：我们的窗口与 plasmashell 的桌面视图**都在 DesktopLayer**，
    # 且我们排在它上面 ⇒ 图标（由 plasmashell 的桌面视图绘制）被盖住。
    # 因此只有三条出路：
    #   ① 让我们的窗口沉到 plasmashell 桌面视图**之下**（同层内的相对次序）
    #   ② 让 plasmashell 桌面视图的背景变透明（暴露出它下面的我们）
    #   ③ 让用户关掉桌面图标（降级，必须显式告知）
    # 本模式逐条实测①②，并全程截图留证。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "P1 桌面层落位矩阵 APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    RC=0
    SHOTS="$LOGDIR/p1"
    mkdir -p "$SHOTS"
    # 桌面图标：确保 ~/Desktop 有可见对象（Plasma 默认 containment 是 Folder View）
    mkdir -p "$HOME/Desktop"
    for n in 1 2; do : > "$HOME/Desktop/P1-测试图标-$n.txt"; done
    echo "  ~/Desktop: $(ls "$HOME/Desktop" | tr '\n' ' ')"

    shot() { # 截图 + 打印当前 stacking
      local tag="$1"
      DISPLAY=$DISP import -window root "$SHOTS/$tag.png" 2>/dev/null && echo "  [shot] $SHOTS/$tag.png"
      local i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '        #%d %s %s class=%s name=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_NAME 2>/dev/null | cut -d'"' -f2)"
      done
    }
    desktop_view_of() { # plasmashell 的桌面视图窗口 id
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*)
            case "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null)" in
              *plasmashell*) printf '%s' "$w"; return 0 ;;
            esac ;;
        esac
      done
    }
    rank_of() {
      local t="$1" i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1)); [ "$w" = "$t" ] && { printf '%s' "$i"; return 0; }
      done
      printf '?'
    }
    find_wp() {
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$1" ] && { printf '%s' "$w"; return 0; }
      done
    }

    # ---- A. 基线：只有 plasmashell，没有我们的应用 ----
    echo
    echo "--- A. 基线（无应用）：桌面图标应该可见 ---"
    sleep 3
    shot "A-baseline"
    DV="$(desktop_view_of)"; echo "  plasmashell 桌面视图: ${DV:-（未找到）} 名次=$(rank_of "$DV")"

    # ---- B. 起应用：现状（我们的桌面窗口在桌面视图之上） ----
    echo
    echo "--- B. 现状：应用在跑 ---"
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$LOGDIR/p1-app.log" 2>&1 < /dev/null &
    APP_PID=$!
    WP=""
    for i in $(seq 1 60); do
      sleep 0.5
      kill -0 "$APP_PID" 2>/dev/null || break
      WP="$(find_wp "$APP_PID")"; [ -n "$WP" ] && break
    done
    if [ -z "$WP" ]; then
      bad "壁纸窗口没出现"; tail -20 "$LOGDIR/p1-app.log"; RC=1
    else
      echo "  壁纸窗口 $WP 名次=$(rank_of "$WP")  桌面视图名次=$(rank_of "$DV")"
      shot "B-app"
      P1_B_WP_RANK="$(rank_of "$WP")"
      P1_B_DV_RANK="$(rank_of "$DV")"

      # ---- C. 机制①：把我们的窗口压到桌面视图之下（同层相对次序） ----
      echo
      echo "--- C. 机制① XLowerWindow（xdotool windowlower）---"
      DISPLAY=$DISP xdotool windowlower "$WP" 2>&1 | sed 's/^/    /'
      sleep 2
      echo "  壁纸名次=$(rank_of "$WP")  桌面视图名次=$(rank_of "$DV")"
      shot "C-lower"
      P1_C_WP_RANK="$(rank_of "$WP")"

      # ---- D. 机制①变体：把我们的窗口抬到桌面视图之上再试 _NET_WM_STATE_BELOW ----
      echo
      echo "--- D. 机制①变体 _NET_WM_STATE_BELOW（KeepBelow）---"
      DISPLAY=$DISP xdotool windowraise "$WP" 2>/dev/null
      sleep 1
      DISPLAY=$DISP xprop -id "$WP" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_BELOW 2>&1 | sed 's/^/    /'
      sleep 2
      echo "  _NET_WM_STATE = $(DISPLAY=$DISP xprop -id "$WP" _NET_WM_STATE 2>/dev/null | cut -d= -f2-)"
      echo "  壁纸名次=$(rank_of "$WP")  桌面视图名次=$(rank_of "$DV")"
      shot "D-below"
      P1_D_WP_RANK="$(rank_of "$WP")"

      # ---- E. 机制②：让桌面视图的背景透明（evaluateScript 若干变体） ----
      echo
      echo "--- E. 机制② 桌面背景透明化（evaluateScript 变体）---"
      DV_APP="$DV"
      for variant in 1 2 3; do
        case "$variant" in
          1) JS='var ds=desktops(); for (var i=0;i<ds.length;i++){ ds[i].wallpaper=""; } print("P1 variant1 done");' ; desc='wallpaper=""（移除壁纸插件）' ;;
          2) JS='var ds=desktops(); for (var i=0;i<ds.length;i++){ var w=ds[i].wallpaper; if (w) { w.visible=false; } } print("P1 variant2 done");' ; desc='wallpaper.visible=false' ;;
          3) JS='var ds=desktops(); for (var i=0;i<ds.length;i++){ ds[i].wallpaper="org.kde.image"; ds[i].writeConfig("Wallpaper","Image",""); ds[i].reloadConfig(); } print("P1 variant3 done");' ; desc='换回 org.kde.image 并清空图片路径（黑底？）' ;;
        esac
        echo "  变体 $variant: $desc"
        DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" "$QDBUS_BIN" org.kde.plasmashell /PlasmaShell \
          org.kde.PlasmaShell.evaluateScript "$JS" 2>&1 | sed 's/^/    ret= /'
        sleep 3
        shot "E$variant-transparent"
        echo "    壁纸名次=$(rank_of "$WP")  桌面视图名次=$(rank_of "$DV_APP")"
      done
      echo
      echo "  plasmashell 日志尾部（看变异是否生效/报错）："
      tail -6 "$LOGDIR/plasmashell.log" | sed 's/^/    /'

      kill "$APP_PID" 2>/dev/null
      sleep 1
    fi

    echo
    echo "=== P1 结论数据（名次：自底向上，数字越大越靠上）==="
    echo "  桌面视图名次(A 无应用)     : ${P1_B_DV_RANK:-?}"
    echo "  壁纸名次(B 现状)           : ${P1_B_WP_RANK:-?}"
    echo "  壁纸名次(C XLowerWindow)   : ${P1_C_WP_RANK:-?}"
    echo "  壁纸名次(D _NET_WM_STATE_BELOW): ${P1_D_WP_RANK:-?}"
    echo "  判读：壁纸名次 < 桌面视图名次 ⇒ 机制①成功（图标应在壁纸上可见）"
    echo "  截图全部在 $SHOTS/（需人工判读图标可见性）"
    echo "  ~/Desktop 测试文件保留，供人工复核"
    ;;
  p1b)
    # P1 spike 第二轮（修正上一轮的两个测量错误）：
    #   错误 a：find_wp 抓到了应用 NORMAL 窗口而不是 DESKTOP 窗口 → 对错窗口设了
    #           _NET_WM_STATE_BELOW，D 组结论无效。
    #   错误 b：窗口一出现就截图，WebView 还没绘制 → B 组「现状」看起来像没生效。
    # 本轮额外回答一个关键问题：**桌面容器能不能变透明**（机制②），以及
    # **同层内的相对次序能不能改**（机制①）。判读靠截图。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "P1b 桌面层落位矩阵（修正版）APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    RC=0
    SHOTS="$LOGDIR/p1b"; mkdir -p "$SHOTS"
    mkdir -p "$HOME/Desktop"
    for n in 1 2; do : > "$HOME/Desktop/P1-测试图标-$n.txt"; done

    shot() { DISPLAY=$DISP import -window root "$SHOTS/$1.png" 2>/dev/null && echo "  [shot] $SHOTS/$1.png"; }
    stack() {
      local i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '        #%d %s %-8s class=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')"
      done
    }
    rank() { local i=0 w; for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do i=$((i+1)); [ "$w" = "$1" ] && { printf '%s' "$i"; return; }; done; printf '?'; }
    dv() { # plasmashell 桌面视图
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null):$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null)" in
          *DESKTOP*:*plasmashell*) printf '%s' "$w"; return ;;
        esac
      done
    }
    wp_of() { # 我们自己的 DESKTOP 类型窗口（修正 a：只要 DESKTOP）
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$1" ] || continue
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*) printf '%s' "$w"; return ;;
        esac
      done
    }

    # 0. 基线（无应用）
    echo; echo "--- 0. 基线（无应用）---"
    sleep 3; shot "0-baseline"; DV="$(dv)"; echo "  桌面视图=$DV 名次=$(rank "$DV")"; stack

    # 1. 起应用，等 WebView 真正画出来（15s），再截图
    echo; echo "--- 1. 现状（应用已绘制）---"
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$LOGDIR/p1b-app.log" 2>&1 < /dev/null &
    APP_PID=$!
    WP=""
    for i in $(seq 1 80); do sleep 0.5; kill -0 "$APP_PID" 2>/dev/null || break; WP="$(wp_of "$APP_PID")"; [ -n "$WP" ] && break; done
    if [ -z "$WP" ]; then
      bad "DESKTOP 类型壁纸窗口没出现"; tail -20 "$LOGDIR/p1b-app.log"; RC=1
    else
      echo "  壁纸窗口(DESKTOP) $WP  等待 WebView 绘制 15s…"
      sleep 15
      DV="$(dv)"
      echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"
      shot "1-app-painted"; stack

      # 2. 机制①：把 _NET_WM_STATE_BELOW 设在**正确的**（DESKTOP）窗口上
      echo; echo "--- 2. 机制① _NET_WM_STATE_BELOW 设在 DESKTOP 窗口 ---"
      DISPLAY=$DISP xprop -id "$WP" -f _NET_WM_STATE 32a -set _NET_WM_STATE _NET_WM_STATE_BELOW 2>&1 | sed 's/^/    /'
      sleep 3
      echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "2-below-on-desktop"; stack

      # 3. 机制①变体：抬高 plasmashell 桌面视图（可用工具能做到的「相对重排」）
      echo; echo "--- 3. 机制①变体 xdotool windowraise 桌面视图 ---"
      DISPLAY=$DISP xdotool windowraise "$DV" 2>&1 | sed 's/^/    /'
      sleep 3
      echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "3-raise-dv"; stack

      # 4. 机制②：把 Plasma 桌面壁纸换成全透明 PNG，看容器能否透出下层
      echo; echo "--- 4. 机制② 桌面壁纸换全透明 PNG（透明化容器）---"
      TRANS="$LOGDIR/transparent.png"
      if command -v convert >/dev/null 2>&1; then
        convert -size 1280x800 xc:none "$TRANS" 2>/dev/null && echo "  生成 $TRANS ($(stat -c%s "$TRANS") B)"
      fi
      if [ -s "$TRANS" ] && command -v plasma-apply-wallpaperimage >/dev/null 2>&1; then
        DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$TRANS" 2>&1 | sed 's/^/    /'
        sleep 4; shot "4-transparent-wallpaper"; stack
        echo "  -- 现在抬高桌面视图到我们之上，重排后应是「我们的壁纸透出来 + 图标可见」 --"
        DISPLAY=$DISP xdotool windowraise "$DV" 2>/dev/null; sleep 3
        echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "4b-raise-dv-transparent"; stack
      else
        echo "  [WARN] 缺 convert 或 plasma-apply-wallpaperimage，跳过机制②"
      fi

      # 5. 恢复原壁纸
      echo; echo "--- 5. 恢复 Plasma 默认壁纸 ---"
      DEF="$(ls /usr/share/wallpapers/*/contents/images/*.png 2>/dev/null | head -1)"
      if [ -n "$DEF" ]; then
        DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$DEF" 2>&1 | sed 's/^/    /'
        echo "  已恢复: $DEF"
      fi
      kill "$APP_PID" 2>/dev/null
      sleep 1
    fi
    echo; echo "=== P1b 截图在 $SHOTS/ ==="
    ;;
  p1c)
    # P1 spike 第三轮：**决定性**问题——能不能把我们的窗口放到 plasmashell 桌面视图
    # 之下？p1b 已证明：① `_NET_WM_STATE_BELOW` 对 DESKTOP 类型窗口无效（名次不变）；
    # ② `xdotool windowraise` 抬 plasmashell 桌面视图也无效。但那两个都不是「明确的
    # 重排请求」。这一轮用 C 直接发 XConfigureWindow(sibling, Below) 与 EWMH
    # `_NET_RESTACK_WINDOW`（detail=Below, sibling=桌面视图）——这是 X11 里唯一能表达
    # 「排到某个兄弟窗口之下」的原语。成功则机制①成立，再配合机制②（桌面容器换全透明
    # PNG）即可做到「壁纸可见 + 图标在壁纸之上」。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "P1c 决定性重排测试 APP=$APP"
    [ -x "$APP" ] || { bad "二进制不存在: $APP"; exit 2; }
    RC=0
    SHOTS="$LOGDIR/p1c"; mkdir -p "$SHOTS"
    mkdir -p "$HOME/Desktop"
    for n in 1 2 3; do : > "$HOME/Desktop/P1c-图标-$n.txt"; done

    # 编译重排小工具（Xlib，唯一目的：发 CWSibling|CWStackMode 与 _NET_RESTACK_WINDOW）
    XR="$LOGDIR/xrestack"
    cat > "$LOGDIR/xrestack.c" <<'XEOF'
#include <X11/Xlib.h>
#include <string.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    if (argc < 5) { fprintf(stderr, "usage: xrestack cfg|net <win> <sibling> above|below\n"); return 2; }
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "XOpenDisplay failed\n"); return 3; }
    Window w = (Window)strtoul(argv[2], NULL, 0);
    Window sib = (Window)strtoul(argv[3], NULL, 0);
    int below = (strcmp(argv[4], "below") == 0);
    if (strcmp(argv[1], "cfg") == 0) {
        XWindowChanges c; memset(&c, 0, sizeof c);
        c.sibling = sib; c.stack_mode = below ? Below : Above;
        XConfigureWindow(d, w, CWSibling | CWStackMode, &c);
        printf("sent XConfigureWindow stack_mode=%s sibling=0x%lx\n", below ? "Below" : "Above", sib);
    } else {
        XEvent e; memset(&e, 0, sizeof e);
        e.xclient.type = ClientMessage;
        e.xclient.window = w;
        e.xclient.message_type = XInternAtom(d, "_NET_RESTACK_WINDOW", False);
        e.xclient.format = 32;
        e.xclient.data.l[0] = 2;            /* source: pager */
        e.xclient.data.l[1] = 0;            /* timestamp */
        e.xclient.data.l[2] = (long)sib;
        e.xclient.data.l[3] = below ? 1 : 0; /* detail: 1=Below 0=Above */
        XSendEvent(d, DefaultRootWindow(d), False,
                   SubstructureRedirectMask | SubstructureNotifyMask, &e);
        printf("sent _NET_RESTACK_WINDOW detail=%s sibling=0x%lx\n", below ? "Below" : "Above", sib);
    }
    XSync(d, False); XCloseDisplay(d); return 0;
}
XEOF
    if gcc -O0 -o "$XR" "$LOGDIR/xrestack.c" -lX11 2>"$LOGDIR/gcc.log"; then
      ok "编译重排工具 $XR"
    else
      bad "gcc 编译失败（缺 libx11-dev?）"; cat "$LOGDIR/gcc.log"; RC=1
    fi

    shot() { DISPLAY=$DISP import -window root "$SHOTS/$1.png" 2>/dev/null && echo "  [shot] $SHOTS/$1.png"; }
    stack() {
      local i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '        #%d %s %-8s class=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')"
      done
    }
    rank() { local i=0 w; for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do i=$((i+1)); [ "$w" = "$1" ] && { printf '%s' "$i"; return; }; done; printf '?'; }
    dv() {
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null):$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null)" in
          *DESKTOP*:*plasmashell*) printf '%s' "$w"; return ;;
        esac
      done
    }
    wp_of() {
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$1" ] || continue
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*) printf '%s' "$w"; return ;;
        esac
      done
    }

    echo; echo "--- 0. 基线 ---"
    sleep 3; shot "0-baseline"; DV="$(dv)"; echo "  桌面视图=$DV 名次=$(rank "$DV")"; stack

    echo; echo "--- 1. 起应用 + 等 WebView 绘制 ---"
    setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
      "$APP" > "$LOGDIR/p1c-app.log" 2>&1 < /dev/null &
    APP_PID=$!
    WP=""
    for i in $(seq 1 80); do sleep 0.5; kill -0 "$APP_PID" 2>/dev/null || break; WP="$(wp_of "$APP_PID")"; [ -n "$WP" ] && break; done
    if [ -z "$WP" ]; then
      bad "DESKTOP 壁纸窗口没出现"; tail -20 "$LOGDIR/p1c-app.log"; RC=1
    else
      sleep 15; DV="$(dv)"
      echo "  壁纸=$WP 名次=$(rank "$WP")；桌面视图=$DV 名次=$(rank "$DV")"; shot "1-painted"; stack

      # 机制②先做：桌面容器换全透明 PNG（若容器不透明，则即使重排成功也白搭）
      echo; echo "--- 2. 机制② 桌面壁纸换全透明 PNG ---"
      TRANS="$LOGDIR/transparent.png"
      command -v convert >/dev/null 2>&1 && convert -size 1280x800 xc:none "$TRANS" 2>/dev/null
      [ -s "$TRANS" ] && DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$TRANS" 2>&1 | sed 's/^/    /'
      sleep 4; shot "2-transparent"; stack

      # 机制①：两种明确的「排到兄弟之下」原语
      echo; echo "--- 3. 机制① cfg: XConfigureWindow(sibling=桌面视图, Below) ---"
      if [ -x "$XR" ] && [ -n "$DV" ]; then
        DISPLAY=$DISP "$XR" cfg "$WP" "$DV" below | sed 's/^/    /'
        sleep 3
        echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "3-cfg-below"; stack
        echo; echo "--- 4. 机制① net: _NET_RESTACK_WINDOW(detail=Below, sibling=桌面视图) ---"
        DISPLAY=$DISP "$XR" net "$WP" "$DV" below | sed 's/^/    /'
        sleep 3
        echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "4-net-below"; stack
        echo; echo "--- 5. 对照：net Above（应该能排到桌面视图之上，用来证明工具本身有效） ---"
        DISPLAY=$DISP "$XR" net "$WP" "$DV" above | sed 's/^/    /'
        sleep 3
        echo "  壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV")"; shot "5-net-above"; stack
      else
        bad "重排工具缺失或桌面视图未找到，跳过机制①"
      fi

      echo; echo "--- 6. 恢复 Plasma 默认壁纸 ---"
      DEF="$(ls /usr/share/wallpapers/*/contents/images/*.png 2>/dev/null | head -1)"
      [ -n "$DEF" ] && DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$DEF" >/dev/null 2>&1 && echo "  已恢复: $DEF"
      kill "$APP_PID" 2>/dev/null; sleep 1
    fi
    echo; echo "=== P1c 截图在 $SHOTS/ ==="
    ;;
  p1d)
    # P1 spike 第四轮：桌面型窗口（DESKTOP 类型）的重排已被 p1b/p1c 证伪
    # （BELOW 无效、XConfigureWindow BadMatch、_NET_RESTACK_WINDOW 无效）。
    # 这一轮换个身份：**override-redirect（不受 WM 管理）窗口**。KWin 的层枚举里
    # unmanaged 窗口是 UnknownLayer，排序值可能低于 DesktopLayer；若成立，配合
    # 机制②（Plasma 桌面壁纸换全透明 PNG）就能做到「壁纸在图标之下」。
    # 判读方法（两阶段截图，肉眼即可）：
    #   A 阶段（Plasma 默认壁纸）：若满屏是品红色 ⇒ OR 窗口在桌面视图**之上**（失败）；
    #                              若仍是雪山壁纸+图标 ⇒ OR 窗口在其**之下**（有戏）。
    #   B 阶段（桌面壁纸换全透明 PNG）：若能看到品红色且图标浮在上面 ⇒ **成功**。
    say "P1d override-redirect 置底测试"
    RC=0
    SHOTS="$LOGDIR/p1d"; mkdir -p "$SHOTS"
    ORC="$LOGDIR/orwin"
    cat > "$LOGDIR/orwin.c" <<'XEOF'
/* 最小 override-redirect 全屏窗口：只为了回答「不受 WM 管理的窗口能不能沉到
    plasmashell 桌面视图之下」。颜色用醒目的品红，便于截图肉眼判读。 */
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    int secs = (argc > 1) ? atoi(argv[1]) : 30;
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "XOpenDisplay failed\n"); return 3; }
    int s = DefaultScreen(d);
    Window root = RootWindow(d, s);
    XSetWindowAttributes a;
    memset(&a, 0, sizeof a);
    a.override_redirect = True;
    a.background_pixel = 0xFF00FF;             /* magenta */
    a.event_mask = ExposureMask;
    Window w = XCreateWindow(d, root, 0, 0, 1280, 800, 0, CopyFromParent, InputOutput,
                             CopyFromParent, CWOverrideRedirect | CWBackPixel | CWEventMask, &a);
    XMapWindow(d, w);
    XLowerWindow(d, w);                        /* 先自己压到最底 */
    XSync(d, False);
    printf("OR window 0x%lx mapped, override_redirect=True, lower sent\n", w);
    fflush(stdout);
    sleep((unsigned)secs);
    XDestroyWindow(d, w);
    XCloseDisplay(d);
    return 0;
}
XEOF
    if gcc -O0 -o "$ORC" "$LOGDIR/orwin.c" -lX11 2>"$LOGDIR/gcc-or.log"; then
      ok "编译 override-redirect 测试窗口 $ORC"
    else
      bad "gcc 编译失败"; cat "$LOGDIR/gcc-or.log"; RC=1
    fi
    shot() { DISPLAY=$DISP import -window root "$SHOTS/$1.png" 2>/dev/null && echo "  [shot] $SHOTS/$1.png"; }
    stack() {
      local i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '        #%d %s %-8s class=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')"
      done
      echo "        （OR 窗口不进 _NET_CLIENT_LIST_STACKING，判读靠截图颜色）"
    }
    mkdir -p "$HOME/Desktop"
    for n in 1 2 3; do : > "$HOME/Desktop/P1d-图标-$n.txt"; done

    echo; echo "--- 0. 基线（Plasma 默认壁纸 + 桌面图标）---"
    sleep 3; shot "0-baseline"
    echo; echo "--- 1. 起 override-redirect 品红全屏窗口（默认壁纸仍在）---"
    setsid env DISPLAY="$DISP" "$ORC" 90 > "$LOGDIR/orwin.log" 2>&1 < /dev/null &
    OR_PID=$!
    sleep 5; cat "$LOGDIR/orwin.log" | sed 's/^/    /'
    shot "1-or-over-default-wallpaper"; stack
    echo; echo "--- 2. 机制②+OR：桌面壁纸换全透明 PNG ---"
    TRANS="$LOGDIR/transparent.png"
    command -v convert >/dev/null 2>&1 && convert -size 1280x800 xc:none "$TRANS" 2>/dev/null
    [ -s "$TRANS" ] && DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$TRANS" 2>&1 | sed 's/^/    /'
    sleep 5; shot "2-or-over-transparent-wallpaper"; stack
    echo "  判读：若 2 图能看到品红背景 + 桌面图标 ⇒ OR 窗口在桌面视图之下，机制②+OR 成立"
    echo; echo "--- 3. 恢复默认壁纸并收尾 ---"
    DEF="$(ls /usr/share/wallpapers/*/contents/images/*.png 2>/dev/null | head -1)"
    [ -n "$DEF" ] && DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$DEF" >/dev/null 2>&1 && echo "  已恢复: $DEF"
    kill "$OR_PID" 2>/dev/null; sleep 1; shot "3-after-restore"
    echo; echo "=== P1d 截图在 $SHOTS/ ==="
    ;;
  p1e)
    # P1 spike 第五轮：p1b/p1c/p1d 已把三条路证伪——
    #   p1b: DESKTOP 类型窗口 + `_NET_WM_STATE_BELOW` → 名次不变
    #   p1c: DESKTOP 类型 + XConfigureWindow/_NET_RESTACK_WINDOW(sibling, Below) → 无效
    #   p1d: override-redirect 全屏窗口 → 盖住整屏（连面板都盖，比原状更糟）
    # 剩下唯一没验的组合是**普通窗口 + keep-below**：KWin 的层枚举里 BelowLayer(6) 在
    # DesktopLayer(5) 之下，所以普通窗口只要 keep-below 就应排在桌面视图后面。但那样
    # Plasma 桌面容器的**不透明背景**会挡住我们 —— 机制②（桌面壁纸换全透明 PNG）于是
    # 成了唯一的关键未知量。这一轮用品红全屏替身窗口把「我们的窗口」抽象出来：
    #   A 阶段（默认壁纸）：品红不可见 ⇒ keep-below 生效
    #   B 阶段（透明壁纸）：品红可见 + desktop.ini 图标仍可见 ⇒ 机制②成立
    # 再用真应用（DESKTOP 类型）复测 keep-below，确认类型与状态的优先级。
    APP="${APP:-$REPO/src-tauri/target/release/WallpaperEM}"
    say "P1e keep-below × 透明桌面容器 组合测试"
    RC=0
    SHOTS="$LOGDIR/p1e"; mkdir -p "$SHOTS"
    PROBE="$LOGDIR/xprobe"
    cat > "$LOGDIR/xprobe.c" <<'XEOF'
/* 测试替身窗口，回答两个问题：
   1) 普通窗口(_NET_WM_WINDOW_TYPE_NORMAL) + keep-below(_NET_WM_STATE_BELOW) 是否落到
      KWin 的 BelowLayer（即 DesktopLayer 之下）？
   2) DESKTOP 类型 + keep-below 时，类型与状态谁优先？
   品红背景便于截图肉眼判读。mode=setbelow 只对已有窗口补发 keep-below。 */
#include <X11/Xlib.h>
#include <X11/Xatom.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static Atom A(Display *d, const char *n) { return XInternAtom(d, n, False); }

static void add_below(Display *d, Window w) {
    XEvent e; memset(&e, 0, sizeof e);
    e.xclient.type = ClientMessage;
    e.xclient.window = w;
    e.xclient.message_type = A(d, "_NET_WM_STATE");
    e.xclient.format = 32;
    e.xclient.data.l[0] = 1;                  /* _NET_WM_STATE_ADD */
    e.xclient.data.l[1] = (long)A(d, "_NET_WM_STATE_BELOW");
    e.xclient.data.l[2] = 0;
    e.xclient.data.l[3] = 1;                  /* source indication: application */
    XSendEvent(d, DefaultRootWindow(d), False,
               SubstructureRedirectMask | SubstructureNotifyMask, &e);
    XFlush(d);
}

int main(int argc, char **argv) {
    if (argc < 3) { fprintf(stderr, "usage: xprobe normal|plain|desktop <secs> | xprobe setbelow <win>\n"); return 2; }
    Display *d = XOpenDisplay(NULL);
    if (!d) { fprintf(stderr, "XOpenDisplay failed\n"); return 3; }
    if (strcmp(argv[1], "setbelow") == 0) {
        Window w = (Window)strtoul(argv[2], NULL, 0);
        add_below(d, w);
        printf("sent _NET_WM_STATE_BELOW to 0x%lx\n", w);
        XSync(d, False); XCloseDisplay(d); return 0;
    }
    int secs = atoi(argv[2]);
    int desktop = (strcmp(argv[1], "desktop") == 0);
    int keepbelow = !(strcmp(argv[1], "plain") == 0);
    int s = DefaultScreen(d);
    Window root = RootWindow(d, s);
    XSetWindowAttributes a; memset(&a, 0, sizeof a);
    a.background_pixel = 0xFF00FF;            /* magenta */
    a.event_mask = ExposureMask;
    Window w = XCreateWindow(d, root, 0, 0, 1280, 800, 0, CopyFromParent, InputOutput,
                             CopyFromParent, CWBackPixel | CWEventMask, &a);
    Atom tp = A(d, "_NET_WM_WINDOW_TYPE");
    Atom tv = A(d, desktop ? "_NET_WM_WINDOW_TYPE_DESKTOP" : "_NET_WM_WINDOW_TYPE_NORMAL");
    XChangeProperty(d, w, tp, XA_ATOM, 32, PropModeReplace, (unsigned char *)&tv, 1);
    if (keepbelow) {
        Atom st = A(d, "_NET_WM_STATE");
        Atom below = A(d, "_NET_WM_STATE_BELOW");
        XChangeProperty(d, w, st, XA_ATOM, 32, PropModeReplace, (unsigned char *)&below, 1);
    }
    XStoreName(d, w, "WallpaperEM-P1e-Probe");
    XMapWindow(d, w);
    if (keepbelow) add_below(d, w);
    XSync(d, False);
    printf("probe window 0x%lx type=%s keepbelow=%d\n", w,
           desktop ? "DESKTOP" : "NORMAL", keepbelow);
    fflush(stdout);
    sleep((unsigned)secs);
    XDestroyWindow(d, w); XCloseDisplay(d); return 0;
}
XEOF
    if gcc -O0 -o "$PROBE" "$LOGDIR/xprobe.c" -lX11 2>"$LOGDIR/gcc-probe.log"; then
      ok "编译替身窗口 $PROBE"
    else
      bad "gcc 编译失败（缺 libx11-dev?）"; cat "$LOGDIR/gcc-probe.log"; exit 1
    fi

    shot() { DISPLAY=$DISP import -window root "$SHOTS/$1.png" 2>/dev/null && echo "  [shot] $SHOTS/$1.png"; }
    stack() {
      local i=0 w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        i=$((i+1))
        printf '        #%d %s %-8s class=%s state=%s\n' "$i" "$w" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null | cut -d= -f2- | sed 's/.*_NET_WM_WINDOW_TYPE_//')" \
          "$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null | sed 's/.*"\([^"]*\)".*/\1/')" \
          "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_STATE 2>/dev/null | cut -d= -f2- | tr -d ' ')"
      done
    }
    rank() { local i=0 w; for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do i=$((i+1)); [ "$w" = "$1" ] && { printf '%s' "$i"; return; }; done; printf '?'; }
    dv() {
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null):$(DISPLAY=$DISP xprop -id "$w" WM_CLASS 2>/dev/null)" in
          *DESKTOP*:*plasmashell*) printf '%s' "$w"; return ;;
        esac
      done
    }
    wp_of() {
      local w
      for w in $(DISPLAY=$DISP xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' ' '); do
        [ "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$')" = "$1" ] || continue
        case "$(DISPLAY=$DISP xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)" in
          *DESKTOP*) printf '%s' "$w"; return ;;
        esac
      done
    }
    pid_of() { DISPLAY=$DISP xprop -id "$1" _NET_WM_PID 2>/dev/null | grep -oE '[0-9]+$'; }
    DEF="$(ls /usr/share/wallpapers/*/contents/images/*.png 2>/dev/null | head -1)"
    TRANS="$LOGDIR/transparent.png"
    command -v convert >/dev/null 2>&1 && convert -size 1280x800 xc:none "$TRANS" 2>/dev/null
    apply() { DISPLAY=$DISP DBUS_SESSION_BUS_ADDRESS="$BUS" plasma-apply-wallpaperimage "$1" 2>&1 | sed 's/^/    /'; }
    probe() {  # probe <mode> <secs> <logname> -> 打印窗口 id
      setsid env DISPLAY="$DISP" "$PROBE" "$1" "$2" > "$LOGDIR/$3.log" 2>&1 < /dev/null &
      echo $! > "$LOGDIR/$3.pid"
      sleep 6
      sed 's/^/    /' "$LOGDIR/$3.log"
      grep -oE '0x[0-9a-f]+' "$LOGDIR/$3.log" | head -1
    }
    mkdir -p "$HOME/Desktop"
    for n in 1 2 3; do : > "$HOME/Desktop/P1e-图标-$n.txt"; done

    echo; echo "--- 0. 基线（Plasma 默认壁纸）---"
    sleep 3; DV="$(dv)"; echo "  桌面视图=$DV 名次=$(rank "$DV")"; shot "0-baseline"; stack

    echo; echo "--- 1. NORMAL + keep-below（默认壁纸）---"
    PW1="$(probe normal 50 probe1)"
    echo "  替身=$PW1 名次=$(rank "$PW1")  桌面视图名次=$(rank "$DV")"; shot "1-normal-keepbelow-default"; stack
    echo "  判读：品红**不可见** ⇒ keep-below 生效（落到桌面视图之下）；品红盖满 ⇒ 无效"

    echo; echo "--- 2. 机制②：桌面壁纸换全透明 PNG（替身仍在）---"
    [ -s "$TRANS" ] && apply "$TRANS"
    sleep 5; shot "2-normal-keepbelow-transparent"; stack
    echo "  判读：品红可见 且 左上角 desktop.ini 图标仍可见 ⇒ 机制②成立，组合可落地"
    kill "$(cat "$LOGDIR/probe1.pid")" 2>/dev/null; sleep 2

    echo; echo "--- 3. 对照：NORMAL **不给** keep-below（默认壁纸）---"
    PW3="$(probe plain 40 probe3)"
    echo "  替身=$PW3 名次=$(rank "$PW3")  桌面视图名次=$(rank "$DV")"; shot "3-plain-default"; stack
    echo "  判读：品红盖满（只剩面板）⇒ 对照组符合预期，反证 keep-below 是必需的"
    kill "$(cat "$LOGDIR/probe3.pid")" 2>/dev/null; sleep 2

    echo; echo "--- 4. DESKTOP 类型 + keep-below（默认壁纸）：类型 vs 状态谁优先 ---"
    PW4="$(probe desktop 40 probe4)"
    echo "  替身=$PW4 名次=$(rank "$PW4")  桌面视图名次=$(rank "$DV")"; shot "4-desktop-keepbelow-default"; stack
    echo "  判读：品红不可见 ⇒ keep-below 压过 DESKTOP 类型；品红盖满 ⇒ 类型优先"
    echo; echo "--- 5. 同一替身 + 透明桌面壁纸 ---"
    [ -s "$TRANS" ] && apply "$TRANS"
    sleep 5; shot "5-desktop-keepbelow-transparent"; stack
    kill "$(cat "$LOGDIR/probe4.pid")" 2>/dev/null; sleep 2

    echo; echo "--- 6. 真应用（DESKTOP 类型）+ 补发 keep-below ---"
    [ -n "$DEF" ] && apply "$DEF"; sleep 2
    if [ -x "$APP" ]; then
      setsid env "${INNER_ENV[@]}" GDK_BACKEND=x11 QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1 \
        "$APP" > "$LOGDIR/p1e-app.log" 2>&1 < /dev/null &
      APP_PID=$!
      WP=""
      for i in $(seq 1 80); do sleep 0.5; kill -0 "$APP_PID" 2>/dev/null || break; WP="$(wp_of "$APP_PID")"; [ -n "$WP" ] && break; done
      if [ -n "$WP" ]; then
        sleep 15
        DV2="$(dv)"
        echo "  壁纸=$WP 名次=$(rank "$WP")；桌面视图=$DV2 名次=$(rank "$DV2")"; shot "6-app-desktop-default"; stack
        DISPLAY=$DISP "$PROBE" setbelow "$WP" | sed 's/^/    /'
        sleep 4
        echo "  补发后：壁纸名次=$(rank "$WP")  桌面视图名次=$(rank "$DV2")"; shot "7-app-keepbelow-default"; stack
        [ -s "$TRANS" ] && apply "$TRANS"
        sleep 5; shot "8-app-keepbelow-transparent"; stack
        echo "  判读：8 图能看到 Miku 壁纸 + desktop.ini 图标 ⇒ 整套组合在真应用上成立"
      else
        bad "DESKTOP 壁纸窗口没出现"; tail -20 "$LOGDIR/p1e-app.log"; RC=1
      fi
      kill "$APP_PID" 2>/dev/null; sleep 1
    else
      bad "二进制不存在: $APP"; RC=1
    fi
    echo; echo "--- 9. 恢复 Plasma 默认壁纸 ---"
    [ -n "$DEF" ] && apply "$DEF"; sleep 1; shot "9-after-restore"
    echo; echo "=== P1e 截图在 $SHOTS/ ==="
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
