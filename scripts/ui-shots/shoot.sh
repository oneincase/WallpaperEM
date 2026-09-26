#!/bin/bash
# 界面截图（README / 商店页宣传用）：按窗口 2x 抓拍主界面，可选直接落 docs/img。
#
# 为什么是这套做法（都是趟过的坑，改脚本前先读）：
#  - **切页走 AX 按标题点导航胶囊**：胶囊选中/悬浮会横向拉长（grid 0fr→1fr），
#    坐标点击等于打移动靶；AX 不移动指针、不抢前台应用，一边干活一边抓也不打架。
#  - **开窗走单实例回调**：主窗口被关掉/被内存压力回收后，再跑一次应用二进制会触发
#    tauri-plugin-single-instance 回调 ensure_main_window（src-tauri/src/lib.rs），
#    不必去点托盘菜单（那会抢前台）。多出来的实例由本脚本兜底 kill。
#  - **抓图必须在窗口可见时做**：最小化 / 位于别的 Space 时 screencapture 不报错，
#    只给一张空窗图；所以每张都过一遍 verify.py，std≈0 就说明没抓到。
#  - **透明圆角要自己压平**：见 pipeline.py（sips 会把圆角外压成白色，深色玻璃上一眼假）。
#
# 注意：要兼容 macOS 自带的 bash 3.2 —— 变量紧跟中文字符时必须写 ${var}，否则中文
#       的字节会被并进变量名（`$key（` 报 unbound variable），上面几处别改回去。
# 依赖：macOS（CGWindowList / AX / screencapture）、swiftc（Xcode CLT）、python3 + Pillow。
#
# 用法：
#   ./shoot.sh                      # 抓页面表里的全部页 → <repo>/.ui-shots/raw/*.png
#   ./shoot.sh home library         # 只抓指定页（key 见下方 SHOTS）
#   ./shoot.sh --emit home shares   # 抓完直接压成 JPEG 落 docs/img/shot-<key>.jpg
#   ./shoot.sh --list               # 打印页面表
# 环境变量：
#   UI_SHOTS_WORK  工作目录（默认 <repo>/.ui-shots）
#   UI_SHOTS_OUT   --emit 输出目录（默认 <repo>/docs/img）
#   WEM_APP_BIN    要唤起/抓取的应用二进制（默认 src-tauri/target/debug/WallpaperEM）
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
BIN_DIR="$HERE/.bin"
WORK="${UI_SHOTS_WORK:-$REPO/.ui-shots}"
APP_BIN="${WEM_APP_BIN:-$REPO/src-tauri/target/debug/WallpaperEM}"
RAW="$WORK/raw"
THUMBS="$WORK/thumbs"
OUT="${UI_SHOTS_OUT:-$REPO/docs/img}"   # --emit 的输出目录（改它可以在不碰仓库图的前提下试跑）

# 页面表：key|导航胶囊标题|设置内标签(可空)|渲染等待(ms)
#  key 同时决定 --emit 的输出名（docs/img/shot-<key>.jpg），改名要同步 README。
SHOTS=(
  "home|发现||3500"           # 发现页是三次 workshop_random 网络请求
  "workshop|工坊||6000"       # 工坊首屏是一次 Steam 搜索
  "library|本地库||2500"
  "displays|显示器||2000"
  "shares|分享||2500"
  "hotkeys|快捷键||2000"
  "settings|设置|通用|2000"
  "performance|设置|画质|1800"
  "network|设置|网络与服务|2500"
  "about|设置|关于|0"         # 0 = 按下就抓：躲开「更新清单 404」的报错态；发版后可改 2500 抓「已是最新」
)

usage() {
  sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'
}

log() { printf '%s\n' "$*" >&2; }

ensure_helpers() {
  mkdir -p "$BIN_DIR" "$RAW" "$THUMBS"
  command -v swiftc >/dev/null || { log "缺少 swiftc（装 Xcode Command Line Tools：xcode-select --install）"; exit 1; }
  command -v screencapture >/dev/null || { log "缺少 screencapture（这不是 macOS？）"; exit 1; }
  python3 -c "import PIL" 2>/dev/null || { log "缺少 Pillow（pip3 install pillow）"; exit 1; }
  for src in axdrive windowlist; do
    if [ ! -x "$BIN_DIR/$src" ] || [ "$HERE/$src.swift" -nt "$BIN_DIR/$src" ]; then
      log "compiling $src.swift"
      swiftc -O "$HERE/$src.swift" -o "$BIN_DIR/$src"
    fi
  done
}

# 在屏主窗口：`id x y w h`（没有则空）
main_win() { "$BIN_DIR/windowlist" --main 2>/dev/null || true; }
app_pid() { pgrep -x WallpaperEM | head -1; }

# 主窗口不在屏上就用单实例回调唤起（见文件头说明）
ensure_window() {
  [ -n "$(main_win)" ] && return 0
  [ -x "$APP_BIN" ] || { log "找不到应用二进制：${APP_BIN}（先 pnpm tauri dev，或 WEM_APP_BIN 指定）"; exit 1; }
  local before after p
  before="$(pgrep -x WallpaperEM | tr '\n' ' ' || true)"
  log "主窗口不在屏上：二次启动触发单实例回调唤起…"
  "$APP_BIN" >/dev/null 2>&1 &
  for _ in $(seq 1 20); do
    sleep 0.5
    [ -n "$(main_win)" ] && break
  done
  after="$(pgrep -x WallpaperEM | tr '\n' ' ' || true)"
  for p in $after; do
    case " $before " in *" $p "*) ;; *) log "清掉多出来的实例 $p"; kill "$p" 2>/dev/null || true ;; esac
  done
  [ -n "$(main_win)" ] || { log "主窗口没能起来：确认应用在运行（pnpm tauri dev）"; exit 1; }
}

capture() {
  local key="$1" nav="$2" tab="$3" settle="$4"
  ensure_window
  local pid wid wx wy ww wh ymin ymax
  pid="$(app_pid)"
  [ -n "$pid" ] || { log "$key: 应用没在运行"; return 1; }
  read -r wid wx wy ww wh <<<"$(main_win)"
  ymin=$((wy + 4)); ymax=$((wy + 52))   # 顶部 52px 胶囊条
  "$BIN_DIR/axdrive" "$pid" press "$nav" "$ymin" "$ymax" >/dev/null || { log "${key}: 切页失败（${nav}）"; return 1; }
  sleep 0.9
  if [ -n "$tab" ]; then
    "$BIN_DIR/axdrive" "$pid" press "$tab" >/dev/null || { log "${key}: 切标签失败（${tab}）"; return 1; }
    sleep 0.8
  fi
  [ "$settle" -gt 0 ] && sleep "$(awk -v ms="$settle" 'BEGIN{printf "%.2f", ms/1000}')"
  screencapture -l "$wid" -x -o "$RAW/$key.png"
  python3 "$HERE/verify.py" "$RAW/$key.png" "$THUMBS/$key.png" || log "!! ${key} 疑似空白：窗口被最小化 / 在别的 Space？"
}

emit() {
  python3 "$HERE/pipeline.py" "$RAW/$1.png" "$OUT/shot-$1.jpg"
}

LIST=0; EMIT=0; KEYS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --list) LIST=1 ;;
    --emit) EMIT=1 ;;
    -h|--help) usage; exit 0 ;;
    -*) log "未知参数：${1}（-h 看用法）"; exit 2 ;;
    *) KEYS+=("$1") ;;
  esac
  shift
done

if [ "$LIST" = 1 ]; then
  # 只对齐 ASCII 的 key 列：导航/标签是中文，printf 的 %-Ns 按字符数补空格会歪
  for row in "${SHOTS[@]}"; do IFS='|' read -r k n t s <<<"$row"; printf '%-11s %s%s  %sms\n' "$k" "$n" "${t:+ → $t}" "$s"; done
  exit 0
fi

ensure_helpers

# 没给 key 就全抓
if [ ${#KEYS[@]} -eq 0 ]; then
  for row in "${SHOTS[@]}"; do IFS='|' read -r k _ _ _ <<<"$row"; KEYS+=("$k"); done
fi

rc=0
for key in "${KEYS[@]}"; do
  row=""
  for r in "${SHOTS[@]}"; do IFS='|' read -r k _ _ _ <<<"$r"; [ "$k" = "$key" ] && { row="$r"; break; }; done
  [ -n "$row" ] || { log "未知页面 key：${key}（--list 看可用的）"; rc=1; continue; }
  IFS='|' read -r k nav tab settle <<<"$row"
  log "--- ${key}（${nav}${tab:+ → ${tab}}）"
  capture "$k" "$nav" "$tab" "$settle" || rc=1
  [ "$EMIT" = 1 ] && [ -f "$RAW/$k.png" ] && emit "$k"
done

log ""
log "原图：$RAW/*.png　缩略图：$THUMBS/*.png"
[ "$EMIT" = 1 ] && log "已落 README 图：${OUT}/shot-*.jpg"
exit $rc
