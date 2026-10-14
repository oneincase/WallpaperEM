#!/usr/bin/env bash
# 帧导出端到端自检（KDE 验证机 / X11 活会话）
#
# 验的是「Plasma 壁纸插件模式」的引擎侧数据通路：应用把壁纸窗口的画面按帧写成
# `<dir>/frame.ppm` + `<dir>/frame.seq`（插件读这两个文件就能显示）。断言全是
# **文件层面的客观量**：序号在长、PPM 头正确、尺寸合理、停掉后不再长、临时文件不残留。
#
# 用法（宿主 rsync 源码 + 构建后，在 VM 上跑）：
#   ssh parallels@10.211.55.5 'bash -s' < scripts/kde-frame-export-check.sh
#   USE_XVFB=1 APP_BIN=... bash scripts/kde-frame-export-check.sh   # 自建 Xvfb+KWin 会话（推荐）
#   APP_BIN=... FPS=20 SECS=8 bash scripts/kde-frame-export-check.sh
#
# 依赖：应用二进制、X11 会话（DISPLAY=:0 或 USE_XVFB=1 自建）、python3（读写 app.db 与解析 JSON）、curl。
# USE_XVFB=1 时还需要 Xvfb / kwin_x11 / dbus-launch（自建会话在干净环境里跑，见下）。
# 跑完收尾：停壁纸、删掉自检装进本地库的条目、关掉自己拉起来的应用进程。
set -uo pipefail

APP_BIN="${APP_BIN:-$HOME/WallpaperEM/src-tauri/target/release/WallpaperEM}"
DIR="${DIR:-/dev/shm/wallpaperem}"
FPS="${FPS:-20}"
SECS="${SECS:-8}"
PROJECT="${PROJECT:-frame-export-check}"
DISPLAY_="${DISPLAY_:-:0}"
# 窗口抓帧（X11 GetImage 抓引擎壁纸窗口）：Linux 上必须开，否则页面侧读回是全黑
WINDOW_CAPTURE="${WINDOW_CAPTURE:-1}"
# 插件模式：抓帧期间把引擎窗口整窗透明（帧必须照样出，否则插件模式不成立）
HIDE_WINDOW="${HIDE_WINDOW:-1}"
export DISPLAY="$DISPLAY_"

PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); printf '  [PASS] %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf '  [FAIL] %s\n' "$*"; }
info() { printf '  [info] %s\n' "$*"; }
step() { printf '\n=== %s ===\n' "$*"; }

APP_PID=""
# 自建会话用（USE_XVFB=1）；提前声明，好让 EXIT 陷阱在任何早期退出路径上都安全
XVFB_PID=""; KWIN_PID=""; DBUS_PID=""
cleanup() {
  [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null
  sleep 1
  [ -n "$KWIN_PID" ] && kill "$KWIN_PID" 2>/dev/null
  [ -n "$XVFB_PID" ] && kill "$XVFB_PID" 2>/dev/null
  [ -n "$DBUS_PID" ] && kill "$DBUS_PID" 2>/dev/null
  return 0
}
trap cleanup EXIT

# ---------------------------------------------------------------- 0) 前置
step "前置检查"
[ -x "$APP_BIN" ] || { echo "✗ 找不到可执行文件 $APP_BIN"; exit 1; }
info "APP_BIN=$APP_BIN"

# ------------------------------------------------ 可选：自建 Xvfb + KWin 会话（USE_XVFB=1）
# 为什么需要它：VM 的 :0 会话经常处于锁屏（kscreenlocker 全屏盖住一切），而「窗口被遮挡/
# 被取消映射」恰恰是本自检最要躲开的环境 —— 见 §3.5：unmap 会让 WebKitGTK 停掉页面 rAF。
# 自建会话里没有别的东西抢屏，壁纸窗口就是唯一可见窗口，出帧行为可复现。
if [ "${USE_XVFB:-0}" = "1" ]; then
  # 注意：不能用上面的 DISPLAY_ 默认值（那是 :0，会把自建会话的 Xvfb/KWin 打到真会话上，
  # 实测后果是「在锁屏的 :0 上跑」，帧当然不动）。这里用独立屏号。
  DISPLAY_="${XVFB_DISPLAY:-:101}"
  export DISPLAY="$DISPLAY_"
  unset XAUTHORITY
  if [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && command -v dbus-launch >/dev/null 2>&1; then
    eval "$(dbus-launch --sh-syntax)"
    DBUS_PID="${DBUS_SESSION_BUS_PID:-}"
  fi
  Xvfb "$DISPLAY" -screen 0 1280x800x24 -nolisten tcp >/tmp/wpem-xvfb.log 2>&1 & XVFB_PID=$!
  for _ in $(seq 1 60); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.25; done
  xdotool getdisplaygeometry >/dev/null 2>&1 || { echo "✗ Xvfb $DISPLAY 起不来（看 /tmp/wpem-xvfb.log）"; exit 1; }
  kwin_x11 --replace >/tmp/wpem-kwin.log 2>&1 & KWIN_PID=$!
  sleep 3
  xdotool getdisplaygeometry >/dev/null 2>&1 || { echo "✗ KWin 没起来（看 /tmp/wpem-kwin.log）"; exit 1; }
  info "自建会话 DISPLAY=$DISPLAY 尺寸 $(xdotool getdisplaygeometry)"
fi

XA=""
for f in "/run/user/$(id -u)"/xauth_* "$HOME/.Xauthority"; do
  [ -f "$f" ] && { XA="$f"; break; }
done
[ -n "$XA" ] && [ "${USE_XVFB:-0}" != "1" ] && { export XAUTHORITY="$XA"; info "XAUTHORITY=$XA"; }

xdotool getdisplaygeometry >/dev/null 2>&1 || { echo "✗ $DISPLAY 上没有可用的 X 会话（先登录桌面）"; exit 1; }
info "屏幕尺寸 $(xdotool getdisplaygeometry)"
# 本自检只验引擎侧出帧通路（文件层面），不依赖 plasmashell：壁纸窗口只需要一扇 X 会话 + WM。
pgrep -x plasmashell >/dev/null || info "（提示）plasmashell 没在跑 —— 不影响本自检"

DB="${DB:-$HOME/.local/share/io.github.oneincase.wallpaperem/app.db}"
[ -f "$DB" ] || { echo "✗ 找不到应用数据库 $DB（先运行一次应用）"; exit 1; }
info "DB=$DB"

# 关掉上一次残留的应用（自检自己起进程，不碰用户手开的：靠可执行文件绝对路径认）
APP_BIN_ABS="$(cd "$(dirname "$APP_BIN")" && pwd)/$(basename "$APP_BIN")"
pkill -f "$APP_BIN_ABS" 2>/dev/null && { info "关掉上一次自检起的应用"; sleep 1; }

# ---------------------------------------------------------------- 1) 打开 MCP 服务
step "打开 MCP 服务并起应用"
python3 - "$DB" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
db.execute("INSERT INTO settings(key,value) VALUES('service.enabled','1') "
           "ON CONFLICT(key) DO UPDATE SET value='1'")
db.commit()
PY
info "service.enabled=1"

setsid "$APP_BIN" >/tmp/wpem-frame-export.log 2>&1 &
APP_PID=$!
info "应用 pid=$APP_PID，日志 /tmp/wpem-frame-export.log"

read -r PORT TOKEN < <(python3 - "$DB" <<'PY'
import sqlite3, sys
db = sqlite3.connect(sys.argv[1])
q = lambda k, d: (db.execute("SELECT value FROM settings WHERE key=?", (k,)).fetchone() or [d])[0]
print(q("mcp.port", "7411"), q("mcp.token", ""))
PY
)
URL="http://127.0.0.1:${PORT}/mcp?token=${TOKEN}"
info "MCP URL=$URL"

deadline=$(( $(date +%s) + 30 ))
until curl -sf -m 2 -X POST "$URL" -H 'content-type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' >/dev/null 2>&1; do
  [ "$(date +%s)" -lt "$deadline" ] || { bad "30s 内 MCP 端口 ${PORT} 没起来"; tail -20 /tmp/wpem-frame-export.log; exit 1; }
  sleep 1
done
ok "MCP 服务可达（端口 $PORT）"

mcp() { # mcp <tool> <json-args>
  curl -s -m 120 -X POST "$URL" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$1\",\"arguments\":$2}}"
}
# tools/call 的返回是 MCP content 数组，正文在 content[0].text（JSON 字符串）里。
# isError / JSON-RPC error 时把正文打到 stderr 并返回非 0，调用方 `|| { bad ...; exit 1; }`。
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
# 请求体已经落成文件时用这个（整页 HTML 这种大 payload 直接塞命令行会被引号/长度坑）
mcp_text_body() {
  curl -s -m 120 -X POST "$URL" -H 'content-type: application/json' --data-binary @"$1" | _mcp_body
}

# 从工具返回的 JSON 里取一个键（找不到就空串）
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

# 应用刚起来时 GTK/tao 可能还没枚举出显示器（引擎已改成不缓存空结果），这里等它出现：
# 屏没出现就 apply 会直接报「未找到目标显示器」（实测就是这么踩到的）。
deadline=$(( $(date +%s) + 30 ))
while :; do
  DISPLAYS="$(mcp_text displays_list '{}')" || { bad "displays_list 调用失败：$DISPLAYS"; exit 1; }
  if printf '%s' "$DISPLAYS" | python3 -c 'import json,sys; sys.exit(0 if json.load(sys.stdin).get("displays") else 1)'; then
    break
  fi
  [ "$(date +%s)" -lt "$deadline" ] || { bad "30s 内没枚举到显示器：$DISPLAYS"; exit 1; }
  sleep 1
done
DNAME="$(printf '%s' "$DISPLAYS" | python3 -c 'import json,sys; print(",".join(x.get("name","?") for x in json.load(sys.stdin)["displays"]))')"
ok "枚举到显示器：$DNAME"

# ---------------------------------------------------------------- 2) 挂一张会动的壁纸
# 用 **web 壁纸**（CSS 动画 + setInterval 转色）：它不依赖 WebGL，动画一定在跑，适合当
# 「帧该在变」的参照物；而且它正好是页面自抓帧抓不到、只有窗口抓帧能拿到的类型。
step "建工程 → 写会动的页面 → 安装 → 应用到桌面"
mcp_text project_create "{\"name\":\"$PROJECT\",\"type\":\"web\",\"force\":true}" >/dev/null \
  || { bad "project_create 失败"; exit 1; }
python3 - "$PROJECT" > /tmp/wem-frame-export-page.json <<'PY'
import json, sys
html = """<!doctype html>
<html><head><meta charset="utf-8"><style>
html,body{margin:0;height:100%;background:#000;overflow:hidden}
#bg{position:absolute;inset:0;animation:sw 4s linear infinite}
@keyframes sw{0%{background:#ff00ff}50%{background:#00ffff}100%{background:#ff00ff}}
#t{position:absolute;left:24px;top:24px;color:#fff;font:700 64px monospace}
</style></head><body>
<div id="bg"></div><div id="t">WallpaperEM</div>
<script>
// 不依赖 rAF：setInterval 每 100ms 改一次色相 + 旋转，保证逐帧有差异
var d = document.createElement('div');
d.style.cssText = 'position:absolute;left:50%;top:50%;width:220px;height:220px;border-radius:50%';
document.body.appendChild(d);
var n = 0;
setInterval(function () {
  n = (n + 7) % 360;
  d.style.background = 'hsl(' + n + ',100%,50%)';
  d.style.transform = 'translate(-50%,-50%) rotate(' + n + 'deg)';
}, 100);
</script>
</body></html>
"""
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                  "params": {"name": "project_write_file",
                             "arguments": {"project": sys.argv[1], "path": "index.html",
                                           "content": html}}}))
PY
mcp_text_body /tmp/wem-frame-export-page.json >/dev/null \
  || { bad "project_write_file（写会动的 index.html）失败"; exit 1; }
INSTALLED="$(mcp_text project_install "{\"project\":\"$PROJECT\"}")" \
  || { bad "project_install 失败：$INSTALLED"; exit 1; }
ITEM="$(printf '%s' "$INSTALLED" | jget itemId item_id id)"
[ -n "$ITEM" ] || { bad "project_install 没回 itemId：$INSTALLED"; exit 1; }
info "install: $(printf '%s' "$INSTALLED" | tr -d '\n' | head -c 150)"
info "itemId=$ITEM"

APPLY="$(mcp_text wallpaper_apply "{\"itemId\":\"$ITEM\"}")" || { bad "wallpaper_apply 失败：$APPLY"; exit 1; }
printf '  [info] apply: %.120s\n' "$APPLY"
# 壁纸窗口要建出来、页面要加载完场景（WebGL 首帧 + 贴图）才抓得到帧
sleep 6
info "sessions: $(mcp_text list_sessions '{}' | tr -d '\n' | head -c 120)"

# ---------------------------------------------------------------- 3) 帧导出
step "起帧导出（dir=$DIR fps=$FPS windowCapture=$WINDOW_CAPTURE hideWindow=$HIDE_WINDOW）"
rm -rf "$DIR"
START="$(mcp_text frame_export_start "{\"dir\":\"$DIR\",\"fps\":$FPS,\"maxWidth\":1280,\"windowCapture\":$([ "$WINDOW_CAPTURE" = "1" ] && echo true || echo false),\"hideWindow\":$([ "$HIDE_WINDOW" = "1" ] && echo true || echo false)}")"
printf '  [info] start: %.200s\n' "$START"
sleep 2

[ -f "$DIR/frame.ppm" ] && ok "frame.ppm 已生成" || { bad "frame.ppm 没生成"; tail -20 /tmp/wpem-frame-export.log; }
[ -f "$DIR/frame.seq" ] && ok "frame.seq 已生成" || bad "frame.seq 没生成"

SEQ1="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
MD5A="$(md5sum "$DIR/frame.ppm" 2>/dev/null | awk '{print $1}')"
sleep "$SECS"
SEQ2="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
MD5B="$(md5sum "$DIR/frame.ppm" 2>/dev/null | awk '{print $1}')"
info "序号 $SEQ1 → $SEQ2（${SECS}s 内）"
[ "$SEQ2" -gt "$SEQ1" ] && ok "序号在增长（帧真的在落盘）" || bad "序号 ${SECS}s 内没动（$SEQ1 → $SEQ2）"
[ -n "$MD5A" ] && [ "$MD5A" != "$MD5B" ] && ok "帧内容在变（md5 ${MD5A:0:8} → ${MD5B:0:8}）" \
  || bad "帧内容没变（md5 ${MD5A:0:8} → ${MD5B:0:8}）—— 画面静止或抓帧拿到的是恒定图"

# 帧**内容**必须是真实像素：Linux 上页面自抓帧（drawImage(glCanvas)）返回全黑，
# 这条断言就是用来钉住那个回归的（非黑像素占比过低 = 黑帧）。
python3 - "$DIR/frame.ppm" <<'PY' && ok "帧内容是真实像素（非黑占比足够）" || bad "帧内容疑似全黑（页面读回黑帧 / 窗口抓帧没生效）"
import sys
p = sys.argv[1]
try:
    with open(p, "rb") as f:
        data = f.read()
except FileNotFoundError:
    print("无 frame.ppm"); raise SystemExit(1)
# 跳过 PPM 头（P6\n<w> <h>\n255\n）
head = 0
for _ in range(3):
    nl = data.find(b"\n", head)
    if nl < 0: raise SystemExit(1)
    head = nl + 1
hdr = data[:head].split()
w, h = int(hdr[1]), int(hdr[2])
px = data[head:]
total = 0; lit = 0
step = max(1, (w * h) // 20000)
for i in range(0, w * h, step):
    o = i * 3
    if o + 3 > len(px): break
    r, g, b = px[o], px[o + 1], px[o + 2]
    total += 1
    if r + g + b > 24:
        lit += 1
frac = lit / max(1, total)
print(f"  尺寸 {w}x{h}，采样 {total} 点，非黑占比 {frac:.3f}")
sys.exit(0 if frac > 0.05 else 1)
PY

# PPM 头 + 尺寸：P6\n<w> <h>\n255\n
read -r HDR W H < <(python3 - "$DIR/frame.ppm" <<'PY'
import sys
p = sys.argv[1]
try:
    with open(p, "rb") as f: head = f.read(64)
except FileNotFoundError:
    print("missing 0 0"); raise SystemExit
parts = head.split(b"\n", 3)
if len(parts) < 3 or parts[0] != b"P6":
    print("bad 0 0"); raise SystemExit
wh = parts[1].split()
print("ok", wh[0].decode(), wh[1].decode())
PY
)
if [ "$HDR" = "ok" ] && [ "${W:-0}" -gt 100 ] && [ "${H:-0}" -gt 100 ]; then
  ok "PPM 头正确且尺寸合理：${W}x${H}"
else
  bad "PPM 头/尺寸不对（$HDR ${W:-?}x${H:-?}）"
fi

# 帧字节数应该 ≈ 头 + w*h*3（未压缩）
python3 - "$DIR/frame.ppm" "${W:-0}" "${H:-0}" <<'PY' && ok "帧字节数 = 头 + w*h*3（未压缩 PPM）" || bad "帧字节数与尺寸不符"
import os, sys
p, w, h = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
size = os.path.getsize(p)
head = 0
with open(p, "rb") as f:
    for _ in range(3):
        line = b""
        while not line.endswith(b"\n"):
            b = f.read(1)
            if not b: break
            line += b
        head += len(line)
sys.exit(0 if size == head + w * h * 3 else 1)
PY

# 统计口径：written>0 且 errors 不增长
STAT="$(mcp_text frame_export_status '{}')"
printf '  [info] status: %.240s\n' "$STAT"
python3 - "$STAT" <<'PY' && ok "status: running=true written>0" || bad "status 不正常（running/written）"
import json, sys
d = json.loads(sys.argv[1])
sys.exit(0 if d.get("running") and d.get("stats", {}).get("written", 0) > 0 else 1)
PY
python3 - "$STAT" <<'PY' && ok "status: 没有抓帧错误" || bad "status: 有抓帧错误（看 last_error）"
import json, sys
d = json.loads(sys.argv[1])
sys.exit(0 if d.get("stats", {}).get("errors", 0) == 0 else 1)
PY
SRC="$(printf '%s' "$STAT" | jget source)"
if [ "$WINDOW_CAPTURE" = "1" ]; then
  [ "$SRC" = "x11-window" ] && ok "status.source=x11-window（走的是 X11 窗口抓帧，不是黑帧的页面读回）" \
    || bad "status.source=${SRC:-空}（期望 x11-window：窗口抓帧没生效，帧很可能是黑的）"
else
  info "status.source=${SRC:-空}（本次 windowCapture=0，按页面抓帧跑）"
fi

# ---------------------------------------------------------------- 3.5) 插件模式：整窗透明而非隐藏
# 为什么必须验这一条：插件模式下引擎自己不该再显示壁纸（否则和插件叠在一起），
# 但**不能用「隐藏窗口」实现** —— 窗口一旦被取消映射（unmap），WebKitGTK 会停掉页面 rAF，
# 抓到的帧永远停在最后一帧（VM 实测：unmap 后 7 秒 140 帧里 139 帧逐字节相同）。
# 正确做法是保持映射 + `_NET_WM_WINDOW_OPACITY=0`（同样实测：7 秒 83 帧全部有变化）。
step "插件模式（hideWindow=true）：整窗透明，但仍要出帧"
mcp_text frame_export_stop '{}' >/dev/null

# 引擎自己的 DESKTOP 类型窗口（按 _NET_WM_PID 认，避免把 plasmashell 的桌面窗口算进来）
desktop_wins() {
  local w pid type
  for w in $(xprop -root _NET_CLIENT_LIST_STACKING 2>/dev/null | sed 's/.*# //' | tr -d ' ' | tr ',' '\n'); do
    type="$(xprop -id "$w" _NET_WM_WINDOW_TYPE 2>/dev/null)"
    case "$type" in *DESKTOP*) ;; *) continue ;; esac
    pid="$(xprop -id "$w" _NET_WM_PID 2>/dev/null | awk '{print $NF}')"
    [ "$pid" = "$APP_PID" ] && printf '%s\n' "$w"
  done
}
win_opacity() { # win_opacity <xid> → 十进制值，读不到打空
  xprop -id "$1" _NET_WM_WINDOW_OPACITY 2>/dev/null | sed 's/.*= *//'
}

rm -f "$DIR/frame.seq"
mcp_text frame_export_start "{\"dir\":\"$DIR\",\"fps\":$FPS,\"maxWidth\":1280,\"hideWindow\":true,\"windowCapture\":$([ "$WINDOW_CAPTURE" = "1" ] && echo true || echo false)}" >/dev/null \
  || { bad "hideWindow=true 起帧导出失败"; exit 1; }
sleep 3
HSTAT="$(mcp_text frame_export_status '{}')"
HID="$(printf '%s' "$HSTAT" | jget hidden_windows)"
[ "${HID:-0}" -ge 1 ] \
  && ok "status.hidden_windows=$HID（引擎壁纸窗口已被整窗透明）" \
  || bad "status.hidden_windows=${HID:-0}：没找到可透明化的引擎窗口（窗口建出来了吗）"

WINS="$(desktop_wins)"
if [ -z "$WINS" ]; then
  bad "在 _NET_CLIENT_LIST_STACKING 里找不到本应用的 DESKTOP 窗口（pid=$APP_PID）"
else
  OP0="$(win_opacity "$(printf '%s\n' "$WINS" | head -1)")"
  [ "${OP0:-x}" = "0" ] \
    && ok "窗口 $(printf '%s' "$WINS" | tr '\n' ' ')_NET_WM_WINDOW_OPACITY=0（X 侧可见为全透明）" \
    || bad "_NET_WM_WINDOW_OPACITY=${OP0:-读不到}（期望 0）"
fi

# 透明化之后帧必须还在变 —— 这条是整段的核心：证明「透明≠停摆」
HS1="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
sleep "$SECS"
HS2="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
info "透明期间序号 $HS1 → $HS2（${SECS}s 内）"
[ "$HS2" -gt "$HS1" ] \
  && ok "整窗透明期间仍在出帧（序号在增长，rAF 没被停）" \
  || bad "整窗透明期间序号没动（$HS1 → $HS2）：检查是不是被 unmap 了"

# 停止必须把不透明度还回去，否则用户看到的是「壁纸不见了」
mcp_text frame_export_stop '{}' >/dev/null
sleep 1
SSTAT="$(mcp_text frame_export_status '{}')"
SHID="$(printf '%s' "$SSTAT" | jget hidden_windows)"
[ "${SHID:-0}" = "0" ] && ok "停止后 status.hidden_windows=0" || bad "停止后 hidden_windows=${SHID:-0}（期望 0）"
WINS2="$(desktop_wins)"
if [ -n "$WINS2" ]; then
  OPR="$(win_opacity "$(printf '%s\n' "$WINS2" | head -1)")"
  [ -z "$OPR" ] || [ "$OPR" = "4294967295" ] \
    && ok "停止后不透明度已恢复（${OPR:-属性已移除}）" \
    || bad "停止后不透明度没恢复：${OPR}"
fi

# ---------------------------------------------------------------- 4) 停
step "停帧导出"
STOP="$(mcp_text frame_export_stop '{}')"
printf '  [info] stop: %.160s\n' "$STOP"
SEQ3="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
sleep 3
SEQ4="$(cat "$DIR/frame.seq" 2>/dev/null || echo 0)"
[ "$SEQ3" = "$SEQ4" ] && ok "停掉后序号不再增长（$SEQ3 → $SEQ4）" || bad "停掉后序号还在长（$SEQ3 → $SEQ4）"

if compgen -G "$DIR/*.tmp-*" >/dev/null; then
  bad "残留临时文件：$(ls "$DIR" | tr '\n' ' ')"
else
  ok "没有残留 *.tmp-* 临时文件"
fi

# ---------------------------------------------------------------- 5) 收尾
step "收尾"
mcp_text wallpaper_stop '{}' >/dev/null
mcp_text library_delete "{\"itemId\":\"$ITEM\"}" >/dev/null
info "已停壁纸、删掉自检条目"

printf '\n=== 结果：%d 项通过 / %d 项失败 ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
exit 0
