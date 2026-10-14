#!/usr/bin/env bash
# 托盘菜单语言自检（跑在 KDE 验证机的**真实会话**上，默认 DISPLAY=:0）
#
# 验的是用户报的那条：「界面切成中文后，托盘菜单部分还是英文」。
# 抓的是**真实托盘菜单**：从 org.kde.StatusNotifierWatcher 找到本应用的
# StatusNotifierItem，再走 com.canonical.dbusmenu.GetLayout 把菜单标签逐条读出来
# —— 也就是用户在面板上点开托盘看到的那份文字，不是应用内部的字符串表。
#
# 两条路径都验：
#   A. **运行中切换**：先把 settings 表里的 app.locale 改成与界面相反的语言（模拟
#      「上次启动是另一种语言」），启动后前端会推来界面语言 → 后端必须**整份重建**
#      托盘菜单（Linux 上 Submenu::set_text 静默无效，子菜单标题只能靠重建，见
#      src-tauri/src/lib.rs 的 rebuild_tray_menu）。断言：重建日志出现 + 菜单
#      （含 5 个子菜单标题）全是界面语言。
#   B. **启动第一帧**：再启一次，断言日志「后端语言取自用户设置」且菜单直接就是
#      界面语言（不用等前端推）。
#
# 用法（在 VM 上跑；宿主 rsync+构建之后）：
#   ssh parallels@10.211.55.5 'bash -s' < scripts/kde-vm-tray-i18n-check.sh
#   APP_BIN=/usr/bin/WallpaperEM SECS=25 bash scripts/kde-vm-tray-i18n-check.sh
#
# 依赖：python3（gi/GLib 的 D-Bus 绑定）、plasmashell（提供 SNI 宿主）、已构建的应用。
# 全程只读真机配置：只改 settings 表的 app.locale，收尾还原。
set -uo pipefail

APP_BIN="${APP_BIN:-$HOME/WallpaperEM/src-tauri/target/release/WallpaperEM}"
DISPLAY="${DISPLAY:-:0}"
export DISPLAY
DB="${DB:-$HOME/.local/share/io.github.oneincase.wallpaperem/app.db}"
LS_DB="${LS_DB:-$(dirname "$DB")/localstorage/tauri_localhost_0.localstorage}"
SECS="${SECS:-25}"
LOG=/tmp/wpem-tray-i18n.log
WORK="$(mktemp -d /tmp/wem-tray-i18n.XXXXXX)"

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  [PASS] %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [info] %s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }

# ---------------------------------------------------------------- 工具
# 读/写 settings 表里的单个键（空值用 \x00NULL 表示「原本没有这个键」）
db_get() {
  python3 - "$DB" "$1" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
row = db.execute("SELECT value FROM settings WHERE key=?", (sys.argv[2],)).fetchone()
print(row[0] if row else "\x00NULL")
PY
}
db_set() {
  python3 - "$DB" "$1" "$2" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("INSERT INTO settings(key,value) VALUES(?,?) "
           "ON CONFLICT(key) DO UPDATE SET value=?", (sys.argv[2], sys.argv[3], sys.argv[3]))
db.commit()
PY
}
db_del() {
  python3 - "$DB" "$1" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("DELETE FROM settings WHERE key=?", (sys.argv[2],))
db.commit()
PY
}
# 界面语言：前端存在 WebKit 的 localStorage（SQLite）里，键 we.locale
ui_locale() {
  python3 - "$LS_DB" <<'PY' 2>/dev/null
import sqlite3, sys
try:
    db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
    row = db.execute("SELECT value FROM ItemTable WHERE key='we.locale'").fetchone()
    if row and row[0]:
        v = row[0]
        print(v.decode() if isinstance(v, bytes) else v)
except Exception:
    pass
PY
}
# 抓托盘菜单：打印标签（按层级缩进），同时写一份纯标签列表到 $WORK/labels.txt
dump_tray() {
  python3 - "$WORK/labels.txt" <<'PY'
import sys
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)

def call(dest, path, iface, method, params, rtype):
    return bus.call_sync(dest, path, iface, method, params, GLib.VariantType(rtype),
                         Gio.DBusCallFlags.NONE, -1, None)

rep = call('org.kde.StatusNotifierWatcher', '/StatusNotifierWatcher',
           'org.freedesktop.DBus.Properties', 'Get',
           GLib.Variant('(ss)', ('org.kde.StatusNotifierWatcher', 'RegisteredStatusNotifierItems')),
           '(v)')
items = [str(i) for i in rep.unpack()[0]]
mine = [i for i in items if 'tray_icon_tray_app_main_tray' in i]
if not mine:
    print('未找到本应用的 SNI item；已注册 =', items)
    sys.exit(1)
dest, path = mine[0].split('/', 1)
path = '/' + path
rep = call(dest, path, 'org.freedesktop.DBus.Properties', 'Get',
           GLib.Variant('(ss)', ('org.kde.StatusNotifierItem', 'Menu')), '(v)')
menu_path = rep.unpack()[0]
layout = call(dest, menu_path, 'com.canonical.dbusmenu', 'GetLayout',
              GLib.Variant('(iias)', (0, -1, [])), '(u(ia{sv}av))').unpack()[1]

def un(v):
    return v.unpack() if hasattr(v, 'unpack') else v

labels = []
def walk(node, depth):
    _id, props, children = un(node)
    label = str(un(props.get('label'))) if 'label' in props else ''
    if label:
        labels.append(label)
        print('  ' * depth + label)
    for c in children:
        walk(c, depth + 1)

print(f'# dest={dest} sni={path} menu={menu_path}')
for c in layout[2]:
    walk(c, 0)
with open(sys.argv[1], 'w') as f:
    f.write('\n'.join(labels) + '\n')
PY
}
kill_app() {
  # 不能 pkill -f "$APP_BIN"：调用者那条 shell 的命令行里就带 APP_BIN=...，会连自己一起杀。
  # 比对 /proc/<pid>/exe 的真实指向（注意删掉后软链会带 " (deleted)"）
  for pid in $(pgrep -f "$(basename "$APP_BIN")" 2>/dev/null || true); do
    exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
    real="${exe% (deleted)}"
    if [ "$real" = "$APP_BIN_ABS" ]; then
      kill "$pid" 2>/dev/null
    fi
  done
  sleep 2
}
start_app() {
  : > "$LOG"
  setsid "$APP_BIN" >"$LOG" 2>&1 &
  # 等 SNI 出现在托盘宿主里（应用起来 + 托盘注册），再等 SECS 秒让前端推送落地
  local i
  for i in $(seq 1 30); do
    if grep -qa "tray icon built" "$LOG" 2>/dev/null; then break; fi
    sleep 1
  done
  sleep "$SECS"
}
cleanup() {
  kill_app
  local orig
  orig="$(cat "$WORK/app.locale.bak" 2>/dev/null || echo '\x00NULL')"
  if [ "$orig" = '\x00NULL' ]; then db_del "app.locale"; else db_set "app.locale" "$orig"; fi
  info "已还原 settings 表的 app.locale"
  rm -rf "$WORK"
}
trap cleanup EXIT

# ---------------------------------------------------------------- 0) 前置
step "前置检查"
[ -x "$APP_BIN" ] || { echo "✗ 找不到可执行文件 $APP_BIN"; exit 1; }
APP_BIN_ABS="$(cd "$(dirname "$APP_BIN")" && pwd)/$(basename "$APP_BIN")"
[ -f "$DB" ] || { echo "✗ 找不到 app.db：$DB"; exit 1; }
pgrep -a plasmashell >/dev/null 2>&1 || { echo "✗ 没有 plasmashell（托盘需要 SNI 宿主）"; exit 1; }
python3 -c 'from gi.repository import Gio' 2>/dev/null || { echo "✗ python3 缺少 gi（D-Bus 绑定）"; exit 1; }
kill_app
UI="$(ui_locale)"
if [ -z "$UI" ]; then UI="zh-CN"; info "读不到 localStorage 的 we.locale，按 zh-CN 断言"; fi
case "$UI" in
  en*) OTHER="zh-CN" ;;
  *)   OTHER="en-US" ;;
esac
info "界面语言（localStorage we.locale）= $UI；反语言 = $OTHER"
db_get "app.locale" > "$WORK/app.locale.bak"
info "app.locale 原值 = $(cat "$WORK/app.locale.bak")"

# 期望文案表（与应用 i18n.rs 的表一致）
zh_submenus="轮播 显示器 画质档位 滤镜效果 切换效果"
en_submenus="Slideshow Displays Quality Preset Filter Transition"
zh_leaves="显示主窗口 壁纸设置 自动暂停 退出"
en_leaves="Show Main Window Wallpaper Settings Auto Pause Quit"
if [ "${UI#en}" = "$UI" ]; then  # UI 中文
  want_submenus="$zh_submenus"; want_leaves="$zh_leaves"
  other_submenus="$en_submenus"; lang_name="中文"
else
  want_submenus="$en_submenus"; want_leaves="$en_leaves"
  other_submenus="$zh_submenus"; lang_name="英文"
fi

# ---------------------------------------------------------------- 1) 运行中切换
step "A. 运行中切换（先让托盘按$OTHER 建起来，再由前端推成 $UI）"
db_set "app.locale" "$OTHER"
start_app
if grep -qa "后端语言取自用户设置: $OTHER" "$LOG"; then
  ok "启动第一帧用的是 settings 表里的 $OTHER（后端语言取自用户设置: $OTHER）"
else
  bad "日志里没有「后端语言取自用户设置: $OTHER」"
fi
if grep -qa "托盘菜单已按语言重建：locale=$UI" "$LOG"; then
  ok "前端推来 $UI 后走了重建路径（托盘菜单已按语言重建：locale=$UI）"
else
  bad "日志里没有「托盘菜单已按语言重建：locale=$UI」（切换后原生文案没跟着变）"
fi
if dump_tray > "$WORK/tray-a.txt" 2>&1; then
  ok "抓到真实托盘菜单（$(wc -l < "$WORK/labels.txt") 个标签）"
else
  bad "抓不到托盘菜单：$(tail -2 "$WORK/tray-a.txt" | tr '\n' ' ')"
fi
miss=0
for s in $want_submenus; do
  grep -qxF -- "$s" "$WORK/labels.txt" 2>/dev/null || { miss=$((miss+1)); info "缺子菜单标题：$s"; }
done
if [ "$miss" = 0 ]; then
  ok "5 个子菜单标题都是$lang_name：$want_submenus"
else
  bad "有 $miss 个子菜单标题不是$lang_name（这正是用户报的「部分还是英文」）"
fi
stale=0
for s in $other_submenus; do
  grep -qxF -- "$s" "$WORK/labels.txt" 2>/dev/null && { stale=$((stale+1)); info "残留另一种语言：$s"; }
done
if [ "$stale" = 0 ]; then
  ok "没有任何子菜单标题停在$OTHER"
else
  bad "有 $stale 个子菜单标题停在 $OTHER"
fi
miss=0
for s in $want_leaves; do
  grep -qxF -- "$s" "$WORK/labels.txt" 2>/dev/null || { miss=$((miss+1)); info "缺叶子项：$s"; }
done
if [ "$miss" = 0 ]; then
  ok "叶子项抽样都在$lang_name：$want_leaves"
else
  bad "有 $miss 个叶子项不是$lang_name"
fi
info "菜单标签（层级）："; sed 's/^/    /' "$WORK/tray-a.txt"

# ---------------------------------------------------------------- 2) 启动第一帧
step "B. 启动第一帧（settings 表已是 $UI，托盘不必等前端推）"
kill_app
start_app
if grep -qa "后端语言取自用户设置: $UI" "$LOG"; then
  ok "启动第一帧直接取用户设置的语言（后端语言取自用户设置: $UI）"
else
  bad "日志里没有「后端语言取自用户设置: $UI」"
fi
if dump_tray > "$WORK/tray-b.txt" 2>&1; then
  ok "抓到真实托盘菜单（$(wc -l < "$WORK/labels.txt") 个标签）"
else
  bad "抓不到托盘菜单：$(tail -2 "$WORK/tray-b.txt" | tr '\n' ' ')"
fi
miss=0
for s in $want_submenus $want_leaves; do
  grep -qxF -- "$s" "$WORK/labels.txt" 2>/dev/null || { miss=$((miss+1)); info "缺：$s"; }
done
if [ "$miss" = 0 ]; then
  ok "重启后菜单（含子菜单标题）直接就是$lang_name"
else
  bad "重启后仍有 $miss 个标签不是$lang_name（第一帧语言不对）"
fi
stale=0
for s in $other_submenus; do
  grep -qxF -- "$s" "$WORK/labels.txt" 2>/dev/null && stale=$((stale+1))
done
[ "$stale" = 0 ] && ok "重启后没有残留另一种语言" || bad "重启后有 $stale 个标签残留 $OTHER"

# ---------------------------------------------------------------- 3) 结果
printf '\n==== 结果：%d 项通过 / %d 项失败 ====\n' "$PASS" "$FAIL"
[ "$FAIL" = 0 ]
