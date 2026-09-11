#!/bin/bash
# MCP 端到端自检（macOS 验收用）：准备应用 → 读端口/令牌 → 跑 scripts/mcp-e2e.mjs
#
#   bash scripts/mcp-e2e-run.sh            # 场景壁纸闭环
#   bash scripts/mcp-e2e-run.sh web        # 网页壁纸闭环
#   RESTART=1 bash scripts/mcp-e2e-run.sh  # 应用比二进制旧时由脚本接管重启（仅独立进程）
#   KEEP=1 bash scripts/mcp-e2e-run.sh     # 跑完保留自检条目与桌面壁纸（人工看效果用）
#   APP_BIN=src-tauri/target/release/WallpaperEM bash scripts/mcp-e2e-run.sh
#
# 做的事：
#   1) 若应用没在跑，先把 settings 表里的 mcp.enabled 置 1，再启动应用；
#   1.5) 若在跑的应用比二进制旧（内存里是旧代码），提示重启 / 按 RESTART=1 代重启。
#        由 tauri dev / cargo run 托管的进程**不代重启**——kill 它会带走 vite，见守卫里的注释；
#   1.6) 调试构建（target/debug）的前端来自 vite，确认它活着再往下走，否则只会白屏；
#   2) 等 MCP 端口可连，从 app.db 读出端口与令牌拼出带令牌的 URL；
#   3) 跑 mcp-e2e.mjs 走完整闭环（截图落在临时目录），跑完停止壁纸并删掉自检条目
#      （自检不该污染本地库、更不该把自检壁纸挂在桌面上；KEEP=1 可保留）；
#   4) 只 kill 本脚本自己拉起来的那个进程。
# 注意：第 1 步会**持久化**打开 MCP 开关（设置页可随时关掉）。
set -uo pipefail
# 中文标点紧跟变量时必须写 ${VAR}：macOS 自带 bash 3.2 在 UTF-8 locale 下会把
# ≥0x80 的字节当成变量名字符，"$PID，" 会被解析成变量 "PID<0xef>" → unbound variable。
cd "$(dirname "$0")/.."

KIND="${1:-scene}"
APP_BIN="${APP_BIN:-src-tauri/target/debug/WallpaperEM}"
DB="$HOME/Library/Application Support/io.github.oneincase.wallpaperem/app.db"
LOG="$(mktemp -t wpem-mcp-e2e)"
STARTED=0

cleanup() {
  if [ "$STARTED" = "1" ]; then
    echo "· 关闭本次启动的应用（pid ${APP_PID}）"
    kill "$APP_PID" 2>/dev/null
    wait "$APP_PID" 2>/dev/null
  fi
}
trap cleanup EXIT

if [ ! -x "$APP_BIN" ]; then
  echo "✗ 找不到可执行文件 ${APP_BIN}（先 cargo build / pnpm build）"
  exit 1
fi
# 绝对路径：用来在进程表里认出「跑的是这个可执行文件」，同时避免把工程目录名
# （wallpaper 工程本身就叫 WallpaperEM）当成匹配依据
APP_BIN_ABS="$(cd "$(dirname "$APP_BIN")" && pwd)/$(basename "$APP_BIN")"
if [ ! -f "$DB" ]; then
  echo "✗ 找不到应用数据库 ${DB}（先运行一次应用）"
  exit 1
fi

# ---- 1) 应用在跑吗？MCP 开着吗？
# 不用 `pgrep -f WallpaperEM`：-f 匹配的是整条命令行，`tauri dev` 下 node/vite 的命令行里
# 就带着工程路径，会把「前端 dev server 在跑」误判成「应用在跑」，于是脚本跳过启动、
# 拿一个不存在的服务去连。这里先按进程名精确匹配，拿不到再退回「命令行里带着本工程的
# 可执行文件路径」——两条都不可能命中 vite。
app_pid() {
  local p
  p="$(pgrep -x WallpaperEM 2>/dev/null | head -1)"
  [ -n "$p" ] || p="$(pgrep -f "$APP_BIN_ABS" 2>/dev/null | head -1)"
  printf '%s' "$p"
}
RUNNING=0
if [ -n "$(app_pid)" ]; then
  RUNNING=1
fi

# ---- 1.5) 陈旧二进制守卫
# 重建之后旧进程仍会继续跑**内存里的旧代码**：拿它做验收会看到「已经修好的 bug 又复现」
# 这种假失败（本项目确实被坑过一次：模板 PNG 被写坏，改好了但测的还是旧进程）。
# 进程启动时间早于二进制 mtime ⇒ 它载入的不是当前这份构建。
#
# RESTART=1 只在「独立进程」上安全。应用的父进程是 `tauri dev` / `cargo run` 时杀掉它是
# 连带事故：dev CLI 会跟着退出，并把它拉起来的 vite 一起带走（实测踩过）。之后无论谁再起
# 这个 debug 二进制，主界面与壁纸页都会是白屏 —— 前端资源不在二进制里（见下面的 1.6）。
DEV_MANAGED=0
PARENT_CMD=""
if [ "$RUNNING" = "1" ]; then
  PID="$(app_pid)"
  PARENT_PID="$(ps -o ppid= -p "$PID" 2>/dev/null | tr -d ' ')"
  if [ -n "$PARENT_PID" ]; then
    PARENT_CMD="$(ps -o command= -p "$PARENT_PID" 2>/dev/null)"
    case "$PARENT_CMD" in
      *tauri*|*cargo*) DEV_MANAGED=1 ;;
    esac
  fi
  START_TIME="$(ps -o lstart= -p "$PID" 2>/dev/null | sed 's/^ *//;s/ *$//')"
  START_TS="$(date -j -f "%a %b %d %T %Y" "$START_TIME" +%s 2>/dev/null)"
  BIN_TS="$(stat -f %m "$APP_BIN" 2>/dev/null)"
  if [ -n "$START_TS" ] && [ -n "$BIN_TS" ] && [ "$START_TS" -lt "$BIN_TS" ]; then
    echo "⚠ 正在运行的应用（pid ${PID}，启动于 ${START_TIME}）比 ${APP_BIN}（$(date -r "$BIN_TS" '+%F %T')）旧，"
    echo "  内存里是旧代码，测出来的失败可能是假失败。"
    if [ "$DEV_MANAGED" = "1" ]; then
      echo "✗ 该进程由开发命令托管（父进程：${PARENT_CMD}），本脚本不代劳重启："
      echo "  kill 它会让 dev CLI 一起退出并带走 vite，之后应用只能加载到白屏。"
      echo "  请到那个终端里重启应用（改 Rust 时 tauri dev 自己会重建重启），然后重跑本脚本。"
      exit 1
    fi
    if [ "${RESTART:-0}" = "1" ]; then
      echo "· RESTART=1：结束旧进程，改用当前二进制重起"
      kill "$PID" 2>/dev/null
      sleep 2
      RUNNING=0
    else
      echo "  请重启应用后重跑，或用 RESTART=1 让本脚本接管（仅独立进程可用）。"
      exit 1
    fi
  fi
fi

# ---- 1.6) 调试构建必须有 vite 供前端
# 调试二进制里**不含**前端资源，主界面与壁纸页都从 127.0.0.1:1420（vite）取。
# 没有 vite 时应用能启动、能连 MCP，但主界面白屏、壁纸页连 /diag 都发不出来，
# 表现为「截图等待渲染器就绪超时 + 渲染器未上报任何诊断」这种极难定位的假失败。
VITE_OK=0
if curl -s -o /dev/null -m 3 http://127.0.0.1:1420/; then
  VITE_OK=1
fi
case "$APP_BIN" in
  */target/debug/*)
    if [ "$VITE_OK" = "0" ]; then
      if [ "$RUNNING" = "1" ]; then
        echo "⚠ 连不上 vite（127.0.0.1:1420）：调试构建的前端由它提供，主界面/壁纸页可能是白屏，"
        echo "  闭环大概率会失败在截图那一步。"
      else
        echo "✗ ${APP_BIN} 是调试构建，主界面与壁纸页的 HTML/JS 都由 vite（127.0.0.1:1420）提供，"
        echo "  二进制里不含前端资源；现在连不上 vite，硬起只会得到一个白屏应用。"
        echo "  请先另开终端跑 pnpm dev，或干脆用 pnpm tauri dev 跑应用，再重跑本脚本。"
        exit 1
      fi
    fi
    ;;
esac

ENABLED="$(sqlite3 "$DB" "select value from settings where key='mcp.enabled';" 2>/dev/null)"
if [ "$ENABLED" != "1" ]; then
  if [ "$RUNNING" = "1" ]; then
    echo "✗ 应用已在运行但 MCP 未启用。请在 设置 → AI / MCP 里打开开关后重跑本脚本。"
    exit 1
  fi
  echo "· 打开 MCP 开关（settings.mcp.enabled=1）"
  sqlite3 "$DB" "insert into settings(key,value) values('mcp.enabled','1') on conflict(key) do update set value='1';"
fi

# ---- 2) 起应用（没在跑才起）
if [ "$RUNNING" = "0" ]; then
  echo "· 启动应用：${APP_BIN}（日志 ${LOG}）"
  "$APP_BIN" >"$LOG" 2>&1 &
  APP_PID=$!
  STARTED=1
fi

# ---- 3) 等端口：令牌可能是这次启动才生成的，轮询到 DB 里两条都在为止
URL=""
for _ in $(seq 1 60); do
  PORT="$(sqlite3 "$DB" "select value from settings where key='mcp.port';" 2>/dev/null)"
  TOKEN="$(sqlite3 "$DB" "select value from settings where key='mcp.token';" 2>/dev/null)"
  PORT="${PORT:-7411}"
  if [ -n "$TOKEN" ] && curl -s -o /dev/null -m 1 "http://127.0.0.1:$PORT/mcp?token=$TOKEN"; then
    URL="http://127.0.0.1:$PORT/mcp?token=$TOKEN"
    break
  fi
  sleep 1
done

if [ -z "$URL" ]; then
  echo "✗ 30 秒内没能连上 MCP 服务，应用日志尾部："
  tail -20 "$LOG"
  exit 1
fi
echo "· MCP 地址 $URL"

# ---- 4) 跑闭环
node scripts/mcp-e2e.mjs "$URL" --type "$KIND"
RC=$?
echo
if [ "$RC" = "0" ]; then
  echo "✅ 端到端自检通过（${KIND}）"
else
  echo "❌ 端到端自检失败（${KIND}），应用日志尾部："
  tail -20 "$LOG"
  if ! grep -q "renderer diag" "$LOG" 2>/dev/null; then
    echo
    echo "提示：日志里没有任何 [renderer diag] —— 壁纸页根本没加载起来（不是渲染慢）。"
    echo "      调试构建的前端由 vite 提供，先确认应用不是「裸起 debug 二进制」跑起来的。"
  fi
fi
exit "$RC"
