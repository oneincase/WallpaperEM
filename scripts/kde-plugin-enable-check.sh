#!/usr/bin/env bash
# 应用内「一键启用 KDE 壁纸插件」的真机自检（MCP 驱动，跑在 KDE 验证机上）
#
# 验的是**产品路径**（不是模块单测）：用户在设置页点一次「一键启用」，应用要
#   装插件包 → 用 plasmashell 的脚本接口把桌面壁纸切到本插件 → 起帧导出（插件模式）
# 点「关闭并还原」要能反过来：停导出 → 把原壁纸写回 → 不透明度还回去。
#
# 为什么自建会话：VM 的 :0 经常处于锁屏（kscreenlocker 全屏），而且真会话里切壁纸会
# 动到用户自己的桌面配置。这里用独立 Xvfb + kwin_x11 + plasmashell，全程可复现，
# 收尾把 appletsrc 与 app.db 里相关设置原样还原。
#
# 用法（宿主机 rsync+构建之后，在 VM 上跑）：
#   ssh parallels@10.211.55.5 'bash -s' < scripts/kde-plugin-enable-check.sh
#   APP_BIN=... SECS=8 XVFB_DISPLAY=:104 bash scripts/kde-plugin-enable-check.sh
#
# 依赖：Xvfb / kwin_x11 / plasmashell / dbus-launch / qdbus（或 gdbus）/ xprop / xwininfo /
#        python3 / curl，以及已构建的应用二进制。
set -uo pipefail

APP_BIN="${APP_BIN:-$HOME/WallpaperEM/src-tauri/target/release/WallpaperEM}"
PROJECT="${PROJECT:-plugin-enable-check}"
SECS="${SECS:-8}"
FPS="${FPS:-20}"
DIR="${DIR:-/dev/shm/wallpaperem}"
XVFB_DISPLAY="${XVFB_DISPLAY:-:104}"
PLUGIN_ID="io.github.oneincase.wallpaperem"
PLUGIN_DIR="$HOME/.local/share/plasma/wallpapers/$PLUGIN_ID"
APP_CFG="$HOME/.config/plasma-org.kde.plasma.desktop-appletsrc"
DB="${DB:-$HOME/.local/share/io.github.oneincase.wallpaperem/app.db}"
LOG=/tmp/wpem-plugin-enable.log
WORK="$(mktemp -d /tmp/wem-plugin-check.XXXXXX)"
export DISPLAY="$XVFB_DISPLAY"

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  [PASS] %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [info] %s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }

APP_PID=""; XVFB_PID=""; KWIN_PID=""; PLASMA_PID=""; DBUS_PID=""; PLUGIN_INSTALLED_BEFORE=""
# 全程只在自建会话里动 appletsrc/app.db，收尾一律还原（set -u 下早期退出也要安全）
cleanup() {
  [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null
  sleep 1
  [ -n "$PLASMA_PID" ] && kill "$PLASMA_PID" 2>/dev/null
  [ -n "$KWIN_PID" ] && kill "$KWIN_PID" 2>/dev/null
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  [ -n "$DBUS_PID" ] && kill "$DBUS_PID" 2>/dev/null
  if [ -f "$WORK/appletsrc.bak" ]; then
    cp -f "$WORK/appletsrc.bak" "$APP_CFG" 2>/dev/null && info "已还原 appletsrc"
  fi
  if [ -f "$WORK/settings.bak" ] && [ -f "$DB" ]; then
    python3 - "$DB" "$WORK/settings.bak" <<'PY' 2>/dev/null
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
rows = [l.rstrip("\n").split("\t") for l in open(sys.argv[2]) if l.strip()]
for k, v in rows:
    v = None if v == "\x00NULL" else v
    if v is None:
        db.execute("DELETE FROM settings WHERE key=?", (k,))
    else:
        db.execute("INSERT INTO settings(key,value) VALUES(?,?) "
                   "ON CONFLICT(key) DO UPDATE SET value=?", (k, v, v))
db.commit()
PY
    info "已还原 app.db 里的 kde.plugin.* 设置"
  fi
  [ -n "$PLUGIN_INSTALLED_BEFORE" ] || rm -rf "$PLUGIN_DIR" 2>/dev/null
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

# ---------------------------------------------------------------- 0) 前置
step "前置检查"
[ -x "$APP_BIN" ] || { echo "✗ 找不到可执行文件 $APP_BIN"; exit 1; }
[ -f "$DB" ] || { echo "✗ 找不到应用数据库 $DB（先运行一次应用）"; exit 1; }
for c in Xvfb kwin_x11 plasmashell dbus-launch xdotool xprop xwininfo python3 curl; do
  command -v "$c" >/dev/null 2>&1 || { echo "✗ 缺命令 $c"; exit 1; }
done
# Plasma 脚本通道：qdbus 的各代命令不一定都装（CI 的 ubuntu 镜像里就常常只有 gdbus）。
# 这里只用来「探一下 /PlasmaShell 在不在」，跟引擎侧 kde_plugin::run_plasma_script
# 一样按候选顺序找，gdbus 兜底 —— 否则换个镜像就红在依赖检查上。
QBUS=""
for c in qdbus qdbus-qt5 qdbus6; do
  command -v "$c" >/dev/null 2>&1 && { QBUS="$c"; break; }
done
if [ -z "$QBUS" ] && command -v gdbus >/dev/null 2>&1; then QBUS="gdbus"; fi
[ -n "$QBUS" ] || { echo "✗ 缺命令 qdbus/qdbus-qt5/qdbus6/gdbus（探测 plasmashell 用）"; exit 1; }
plasmashell_reachable() {
  if [ "$QBUS" = "gdbus" ]; then
    gdbus call --session --dest org.kde.plasmashell --object-path /PlasmaShell \
      --method org.freedesktop.DBus.Peer.Ping >/dev/null 2>&1
  else
    "$QBUS" org.kde.plasmashell /PlasmaShell >/dev/null 2>&1
  fi
}
info "APP_BIN=$APP_BIN（Plasma 探测通道：$QBUS）"

# ---------------------------------------------------------------- 1) 自建会话
step "自建会话：Xvfb $DISPLAY + kwin_x11 + plasmashell"
if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && command -v dbus-launch >/dev/null 2>&1; then
  eval "$(dbus-launch --sh-syntax)"
  DBUS_PID="${DBUS_SESSION_BUS_PID:-}"
fi
unset XAUTHORITY   # :0 的 xauth 会让自建屏连不上（实测）
export QT_QPA_PLATFORM=xcb LIBGL_ALWAYS_SOFTWARE=1
Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >"$WORK/xvfb.log" 2>&1 & XVFB_PID=$!
for _ in $(seq 1 60); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.25; done
xdotool getdisplaygeometry >/dev/null 2>&1 || { echo "✗ Xvfb $DISPLAY 起不来（$WORK/xvfb.log）"; exit 1; }
kwin_x11 --replace >"$WORK/kwin.log" 2>&1 & KWIN_PID=$!
for _ in $(seq 1 40); do xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1 && break; sleep 0.25; done
plasmashell --no-respawn >"$WORK/plasmashell.log" 2>&1 & PLASMA_PID=$!
# plasmashell 的脚本接口要等它把 /PlasmaShell 注册到会话总线（这才是「一键启用」真正依赖的东西）
DEADLINE=$(( $(date +%s) + 60 ))
until plasmashell_reachable; do
  [ "$(date +%s)" -lt "$DEADLINE" ] || { bad "60s 内 plasmashell 的 /PlasmaShell 没起来（$WORK/plasmashell.log）"; exit 1; }
  sleep 1
done
ok "plasmashell 已就绪（/PlasmaShell 可 $QBUS 访问，pid=$PLASMA_PID）"

# ---------------------------------------------------------------- 2) 备份真状态
cp -f "$APP_CFG" "$WORK/appletsrc.bak" 2>/dev/null || : > "$WORK/appletsrc.bak"
ORIG_PLUGIN="$(grep -m1 -o 'wallpaperplugin=[^ ]*' "$APP_CFG" 2>/dev/null | cut -d= -f2)"
info "原壁纸插件（自建会话未设置时为 kde 默认）：${ORIG_PLUGIN:-<无>}"
python3 - "$DB" > "$WORK/settings.bak" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
keys = ("kde.plugin.enabled", "kde.plugin.previous", "kde.plugin.frame_dir", "kde.plugin.fps")
rows = db.execute("SELECT key,value FROM settings WHERE key IN (?,?,?,?)", keys).fetchall()
found = {k: v for k, v in rows}
for k in keys:
    print("%s\t%s" % (k, found.get(k, "\x00NULL")))
PY
# 起始状态要求「还没装、没启用」：把插件包与四个设置键清到基线（收尾会还原）
PLUGIN_INSTALLED_BEFORE=""
[ -d "$PLUGIN_DIR" ] && PLUGIN_INSTALLED_BEFORE=1
rm -rf "$PLUGIN_DIR"
python3 - "$DB" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("DELETE FROM settings WHERE key IN "
           "('kde.plugin.enabled','kde.plugin.previous','kde.plugin.frame_dir','kde.plugin.fps')")
db.execute("INSERT INTO settings(key,value) VALUES('service.enabled','1') "
           "ON CONFLICT(key) DO UPDATE SET value='1'")
db.commit()
PY

# ---------------------------------------------------------------- 3) 起应用 + MCP
step "起应用并连通 MCP"
APP_BIN_ABS="$(cd "$(dirname "$APP_BIN")" && pwd)/$(basename "$APP_BIN")"
# 只杀「上一次自检起的应用」。**不能用 `pkill -f "$APP_BIN_ABS"`**：脚本最常见的调法
# 是 `ssh <vm> 'APP_BIN=/usr/bin/WallpaperEM bash -s' < 脚本`，调用者那条 shell 的
# cmdline 里就带着这个路径，pkill -f 会把自己的调用者（连同整条 ssh 会话）一起杀掉，
# 现象是 ssh 返回 255、输出半截 —— 实测踩过。按 /proc/<pid>/exe 精确匹配真正的二进
# 制，既不会误杀，也不依赖 cmdline 文本。
for pid in $(pgrep -f "$APP_BIN_ABS" 2>/dev/null || true); do
  [ "$(readlink -f "/proc/$pid/exe" 2>/dev/null)" = "$APP_BIN_ABS" ] || continue
  kill "$pid" 2>/dev/null && { info "关掉上一次自检起的应用（pid=$pid）"; }
done
sleep 1
setsid "$APP_BIN" >"$LOG" 2>&1 &
APP_PID=$!
read -r PORT TOKEN < <(python3 - "$DB" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
q = lambda k, d: (db.execute("SELECT value FROM settings WHERE key=?", (k,)).fetchone() or [d])[0]
print(q("mcp.port", "7411"), q("mcp.token", ""))
PY
)
URL="http://127.0.0.1:${PORT}/mcp?token=${TOKEN}"
DEADLINE=$(( $(date +%s) + 30 ))
until curl -sf -m 2 -X POST "$URL" -H 'content-type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' >/dev/null 2>&1; do
  [ "$(date +%s)" -lt "$DEADLINE" ] || { bad "30s 内 MCP 端口 ${PORT} 没起来"; tail -20 "$LOG"; exit 1; }
  sleep 1
done
ok "MCP 服务可达（端口 $PORT）"

mcp() { curl -s -m 180 -X POST "$URL" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"; }
_mcp_body() { python3 -c '
import json, sys
d = json.load(sys.stdin)
if "error" in d:
    print(json.dumps(d["error"], ensure_ascii=False)); raise SystemExit(1)
r = d.get("result") or {}
text = ((r.get("content") or [{}])[0]).get("text", "")
if r.get("isError"):
    print(text, file=sys.stderr); raise SystemExit(1)
print(text)'; }
mcp_text() { mcp "$1" "$2" | _mcp_body; }
mcp_text_body() { curl -s -m 180 -X POST "$URL" -H 'content-type: application/json' --data-binary @"$1" | _mcp_body; }
jget() { python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print(""); raise SystemExit
for k in sys.argv[1:]:
    if isinstance(d, dict) and d.get(k) not in (None, ""):
        print(d[k]); raise SystemExit
print("")' "$@"; }
# 按点号路径取值（支持嵌套，如 export.running），布尔统一输出 true/false
jbool() { python3 -c '
import json, sys
d = json.load(sys.stdin)
for k in sys.argv[1].split("."):
    d = d.get(k) if isinstance(d, dict) else None
print("true" if d is True else ("false" if d is False else ("" if d is None else d)))' "$1"; }

DEADLINE=$(( $(date +%s) + 30 ))
while :; do
  DISPLAYS="$(mcp_text displays_list '{}')" || { bad "displays_list 调用失败：$DISPLAYS"; exit 1; }
  printf '%s' "$DISPLAYS" | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("displays") else 1)' && break
  [ "$(date +%s)" -lt "$DEADLINE" ] || { bad "30s 内没枚举到显示器：$DISPLAYS"; exit 1; }
  sleep 1
done
ok "枚举到显示器"

# 安装产物自检：确认「跑起来的这份二进制」对应的前端 bundle 里真的有 KDE 组与
# 三个命令名。装完 deb 却跑的是旧 renderer（或者 renderer 没进包）时，界面上的
# 按钮根本不存在 —— 这条断言让那种情况直接红，而不是靠人眼。
# 资源目录语义（tauri 2.11.5 Linux 非 AppImage）= <exe 所在目录>/../lib/<exe 名>。
RES_ROOT="$(cd "$(dirname "$APP_BIN")/../lib/$(basename "$APP_BIN")" 2>/dev/null && pwd || true)"
# 注意资源布局：renderer 是 HTML 外壳，真正的 JS 打包产物在同级 assets/ 里 ——
# 只 grep renderer 会假红（实测踩过：一开始就漏了 assets）。中文串按原文找，
# 另外顺手认一下 \u 转义形式：打包器 charset 一改就会把中文写成转义
# （本项目 Vite 现在输出 UTF-8 原文）。
if [ -d "$RES_ROOT/renderer" ] || [ -d "$RES_ROOT/assets" ]; then
  step "安装产物自检：前端 bundle 里含 KDE 组与 kde_plugin_* 命令"
  FRONT_DIRS=""
  [ -d "$RES_ROOT/renderer" ] && FRONT_DIRS="$RES_ROOT/renderer"
  [ -d "$RES_ROOT/assets" ] && FRONT_DIRS="$FRONT_DIRS $RES_ROOT/assets"
  # shellcheck disable=SC2086
  HIT_LABEL="$(grep -rl -e 'KDE 桌面集成' -e 'u684C' $FRONT_DIRS 2>/dev/null | head -1)"
  HIT_CMD="$(grep -rl 'kde_plugin_status' $FRONT_DIRS 2>/dev/null | head -1)"
  [ -n "$HIT_LABEL" ] && ok "bundle 含「KDE 桌面集成」文案（$(basename "$HIT_LABEL")）" \
    || bad "bundle 里没有 KDE 组文案（找过：${RES_ROOT}/renderer、assets）"
  [ -n "$HIT_CMD" ] && ok "bundle 含命令名 kde_plugin_status（$(basename "$HIT_CMD")）" \
    || bad "bundle 里没有 kde_plugin_status"
  for cmd in kde_plugin_enable kde_plugin_disable; do
    grep -rq "$cmd" $FRONT_DIRS 2>/dev/null && ok "bundle 里有 invoke(\"$cmd\")" \
      || bad "bundle 里没有 $cmd"
  done
  # 二进制必须是**生产构建**：tauri 的 build.rs 里 `dev = !custom_protocol`
  # （tauri-2.11.5/build.rs:256-260），漏了 `--features tauri/custom-protocol` 编出来的
  # 是开发模式，主窗口会去连 devUrl（localhost:1420）→ 用户看到就是「Could not connect
  # to localhost: Connection refused」。判据：内嵌资源里必有 assets 目录下的
  # index-<hash>.js 文件名（生产构建才把它编进二进制）。
  ASSET_NAME="$(basename "$(ls "$RES_ROOT"/assets/index-*.js 2>/dev/null | head -1)")"
  if [ -n "$ASSET_NAME" ]; then
    # 直接让 grep 读文件：`cat huge | grep -q` 在 `set -o pipefail` 下会因为 grep 提前
    # 命中、cat 收到 SIGPIPE（141）而把整条管道判成失败 —— 断言自己假红（实测踩过）。
    if grep -qa -- "$ASSET_NAME" "$APP_BIN" 2>/dev/null; then
      ok "二进制是生产构建（内嵌 $ASSET_NAME）"
    else
      bad "二进制里没有内嵌前端资源 —— 像是开发模式构建（主窗口会连 localhost:1420 报「Connection refused」）"
    fi
  fi
else
  info "跳过安装产物自检：$RES_ROOT/renderer 与 assets 都不存在（开发态二进制）"
fi

# ---------------------------------------------------------------- 4) 挂一张会动的壁纸
step "建 web 工程 → 写会动的页面 → 安装 → 应用到桌面（enable 要求有在播的壁纸）"
mcp_text project_create "{\"name\":\"$PROJECT\",\"type\":\"web\",\"force\":true}" >/dev/null \
  || { bad "project_create 失败"; exit 1; }
python3 - "$PROJECT" > "$WORK/page.json" <<'PY'
import json, sys
html = """<!doctype html>
<html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:#000;overflow:hidden}
#bg{position:absolute;inset:0;animation:sw 4s linear infinite}
@keyframes sw{0%{background:#ff00ff}50%{background:#00ffff}100%{background:#ff00ff}}
</style></head><body><div id="bg"></div>
<script>
var d=document.createElement('div');
d.style.cssText='position:absolute;left:50%;top:50%;width:220px;height:220px;border-radius:50%';
document.body.appendChild(d);
var n=0;
setInterval(function(){n=(n+7)%360;d.style.background='hsl('+n+',100%,50%)';
d.style.transform='translate(-50%,-50%) rotate('+n+'deg)';},100);
</script></body></html>
"""
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                  "params": {"name": "project_write_file",
                             "arguments": {"project": sys.argv[1], "path": "index.html",
                                           "content": html}}}))
PY
mcp_text_body "$WORK/page.json" >/dev/null || { bad "project_write_file 失败"; exit 1; }
INSTALLED="$(mcp_text project_install "{\"project\":\"$PROJECT\"}")" || { bad "project_install 失败：$INSTALLED"; exit 1; }
ITEM="$(printf '%s' "$INSTALLED" | jget itemId item_id id)"
[ -n "$ITEM" ] || { bad "project_install 没回 itemId：$INSTALLED"; exit 1; }
info "itemId=$ITEM"
mcp_text wallpaper_apply "{\"itemId\":\"$ITEM\"}" >/dev/null || { bad "wallpaper_apply 失败"; exit 1; }
sleep 6   # 等壁纸窗口建出来、页面加载完

# ---------------------------------------------------------------- 5) 启用前的 status
step "kde_plugin_status（启用前）"
ST="$(mcp_text kde_plugin_status '{}')" || { bad "kde_plugin_status 失败：$ST"; exit 1; }
printf '  [info] %.400s\n' "$(printf '%s' "$ST" | tr -d '\n')"
field() { printf '%s' "$ST" | python3 -c 'import json,sys; d=json.load(sys.stdin); v=d.get(sys.argv[1]); print("" if v is None else (str(v).lower() if isinstance(v,bool) else v))' "$1"; }
[ "$(field supported)" = "true" ] && ok "supported=true" || bad "supported=$(field supported)（原因：$(field unsupported_reason)）"
[ "$(field session)" = "x11" ] && ok "session=x11" || bad "session=$(field session)"
[ "$(field plasmashell)" = "true" ] && ok "plasmashell=true" || bad "plasmashell=$(field plasmashell)"
[ "$(field installed)" = "false" ] && ok "installed=false（测试前已清干净）" || bad "installed=$(field installed)"
[ "$(field exporting)" = "false" ] && ok "exporting=false" || bad "exporting=$(field exporting)"
[ "$(field engine_playing)" = "true" ] && ok "engine_playing=true（有在播的壁纸）" || bad "engine_playing=$(field engine_playing)"

# ---------------------------------------------------------------- 6) 一键启用
step "kde_plugin_enable（一键启用）"
EN="$(mcp_text kde_plugin_enable '{}')" || { bad "kde_plugin_enable 失败：$EN"; tail -30 "$LOG"; exit 1; }
printf '  [info] %.300s\n' "$(printf '%s' "$EN" | tr -d '\n')"
[ "$(printf '%s' "$EN" | jbool installed)" = "true" ] && ok "enable 返回 installed=true" || bad "enable 返回里 installed 不是 true"
SW="$(printf '%s' "$EN" | jget switched_desktops)"
[ "${SW:-0}" -ge 1 ] 2>/dev/null && ok "switched_desktops=$SW（桌面数）" || bad "switched_desktops=${SW:-0}"
[ "$(printf '%s' "$EN" | jbool export.running)" = "true" ] && ok "enable 返回里 export.running=true" || bad "enable 返回里 export.running 不是 true"
[ -d "$PLUGIN_DIR" ] && ok "插件包已装到 $PLUGIN_DIR" || bad "插件包没装到位：$PLUGIN_DIR"
[ -f "$PLUGIN_DIR/contents/ui/main.qml" ] && ok "包里有 contents/ui/main.qml" || bad "包里缺 main.qml"
grep -q "wallpaperplugin=$PLUGIN_ID" "$APP_CFG" \
  && ok "appletsrc 里桌面壁纸已切到 $PLUGIN_ID" \
  || bad "appletsrc 里没找到 wallpaperplugin=$PLUGIN_ID"

sleep 3
[ -f "$DIR/frame.ppm" ] && ok "frame.ppm 已生成" || bad "frame.ppm 没生成"
[ -f "$DIR/frame.seq" ] && ok "frame.seq 已生成" || bad "frame.seq 没生成"
# 帧内容必须不是黑的（Linux 上页面读回全黑，只有 X11 窗口抓帧才有画面）
python3 - "$DIR/frame.ppm" <<'PY'
import sys
try:
    d = open(sys.argv[1], "rb").read()
    i = 0; nl = 0
    while nl < 3:
        if d[i:i+1] == b"\n": nl += 1
        i += 1
    px = d[i:]
    n = min(len(px), 60000) // 3 * 3
    dark = sum(1 for j in range(0, n, 3) if px[j] < 8 and px[j+1] < 8 and px[j+2] < 8)
    frac = dark / max(1, n // 3)
    print("black_frac=%.3f" % frac)
    raise SystemExit(0 if frac < 0.95 else 1)
except SystemExit:
    raise
except Exception as e:
    print("read_error=%s" % e); raise SystemExit(1)
PY
[ $? -eq 0 ] && ok "帧内容不是全黑（有画面）" || bad "帧内容基本全黑（画面没抓到）"

SEQ1="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
sleep "$SECS"
SEQ2="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
[ "$SEQ2" -gt "$SEQ1" ] && ok "序号在长：$SEQ1 → $SEQ2（${SECS}s）" || bad "序号没动：$SEQ1 → $SEQ2"

ST2="$(mcp_text kde_plugin_status '{}')"
f2() { printf '%s' "$ST2" | python3 -c 'import json,sys; d=json.load(sys.stdin); v=d.get(sys.argv[1]); print("" if v is None else (str(v).lower() if isinstance(v,bool) else v))' "$1"; }
[ "$(f2 active)" = "true" ] && ok "status.active=true（桌面就是这个插件）" || bad "status.active=$(f2 active)"
[ "$(f2 exporting)" = "true" ] && ok "status.exporting=true" || bad "status.exporting=$(f2 exporting)"
[ "$(f2 enabled)" = "true" ] && ok "status.enabled=true（已持久化）" || bad "status.enabled=$(f2 enabled)"
[ "$(f2 installed_version)" != "" ] && ok "status.installed_version=$(f2 installed_version)" || bad "读不到已装版本"

# 插件模式：引擎自己的壁纸窗口要整窗透明（不是隐藏 —— 隐藏会停 rAF），且透明期间仍在出帧
desktop_wins() {
  local w pid type
  for w in $(xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' '\n'); do
    type="$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)"
    case "$type" in *DESKTOP*) ;; *) continue ;; esac
    pid="$(xprop -id "$w" _NET_WM_PID 2>/dev/null | awk '{print $NF}')"
    [ "$pid" = "$APP_PID" ] && printf '%s\n' "$w"
  done
}
WINS="$(desktop_wins)"
if [ -n "$WINS" ]; then
  OP="$(xprop -id "$(printf '%s\n' "$WINS" | head -1)" _NET_WM_WINDOW_OPACITY 2>/dev/null | sed 's/.*= *//')"
  [ "${OP:-x}" = "0" ] && ok "引擎壁纸窗口已整窗透明（_NET_WM_WINDOW_OPACITY=0）" || bad "引擎壁纸窗口不透明度=${OP:-读不到}（期望 0）"
else
  bad "找不到本应用的 DESKTOP 窗口（pid=$APP_PID）"
fi

# ---------------------------------------------------------------- 7) 关闭并还原
step "kde_plugin_disable（关闭并还原）"
DIS="$(mcp_text kde_plugin_disable '{}')" || { bad "kde_plugin_disable 失败：$DIS"; exit 1; }
printf '  [info] %.300s\n' "$(printf '%s' "$DIS" | tr -d '\n')"
[ "$(printf '%s' "$DIS" | jbool restored)" = "true" ] && ok "disable 返回 restored=true" || bad "disable 返回 restored 不是 true"
[ "$(printf '%s' "$DIS" | jbool stopped)" = "true" ] && ok "disable 返回 stopped=true" || bad "disable 返回 stopped 不是 true"
if [ -n "$ORIG_PLUGIN" ]; then
  grep -q "wallpaperplugin=$ORIG_PLUGIN" "$APP_CFG" && ok "原壁纸已写回（$ORIG_PLUGIN）" || bad "没写回原壁纸 $ORIG_PLUGIN"
else
  ! grep -q "wallpaperplugin=$PLUGIN_ID" "$APP_CFG" && ok "appletsrc 里已不再是本插件" || bad "appletsrc 里还是本插件"
fi
ST3="$(mcp_text kde_plugin_status '{}')"
f3() { printf '%s' "$ST3" | python3 -c 'import json,sys; d=json.load(sys.stdin); v=d.get(sys.argv[1]); print("" if v is None else (str(v).lower() if isinstance(v,bool) else v))' "$1"; }
[ "$(f3 exporting)" = "false" ] && ok "关闭后 status.exporting=false" || bad "关闭后 exporting=$(f3 exporting)"
[ "$(f3 active)" = "false" ] && ok "关闭后 status.active=false" || bad "关闭后 active=$(f3 active)"
[ "$(f3 installed)" = "true" ] && ok "关闭后插件包保留（重启用不必重装）" || bad "关闭后包被删了（不该）"
WINS2="$(desktop_wins)"
if [ -n "$WINS2" ]; then
  OPR="$(xprop -id "$(printf '%s\n' "$WINS2" | head -1)" _NET_WM_WINDOW_OPACITY 2>/dev/null | sed 's/.*= *//')"
  [ -z "$OPR" ] || [ "$OPR" = "4294967295" ] && ok "不透明度已恢复（${OPR:-属性已移除}）" || bad "不透明度没恢复：$OPR"
fi
S1="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
sleep 3
S2="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
[ "$S2" = "$S1" ] && ok "关闭后序号不再增长（$S1）" || bad "关闭后序号还在长：$S1 → $S2"

# ---------------------------------------------------------------- 8) 收尾
step "收尾"
mcp_text wallpaper_stop '{}' >/dev/null 2>&1 || info "wallpaper_stop 失败（忽略）"
mcp_text library_delete "{\"itemId\":\"$ITEM\"}" >/dev/null 2>&1 || info "library_delete 失败（忽略）"
ok "自检条目已清理"

printf '\n==== 结果：%d 项通过 / %d 项失败 ====\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || { info "应用日志尾部："; tail -25 "$LOG"; }
exit $(( FAIL > 0 ? 1 : 0 ))
