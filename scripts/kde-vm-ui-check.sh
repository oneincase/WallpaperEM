#!/usr/bin/env bash
# 设置页「KDE 桌面集成」分组的 UI 断言 —— 走无障碍（AT-SPI），**不依赖截图**。
#
# 为什么需要它：Linux 上抓屏取证不可用（Xvfb/kwin 下 `import` 恒空白 PNG；真实 `:0` 里
# WebKitGTK 的 GL 窗口在 root grab 里是黑块，见 docs/kde-verification-report.md §6.10）。
# 但 WebKitGTK **会把网页内容暴露给无障碍树**，于是可以：枚举节点 → 点「设置」→ 点
# 「通用」→ 断言分组里的按钮（英文 `Enable` / 中文 `一键启用`）确实在页面上。
#
# 这正是当初「WebKitGTK 不进无障碍树」那个错误结论的真相：那时装的是**开发模式**二进制
# （主窗口连 localhost:1420 失败、页面根本没加载），自然什么都枚举不到。
#
# 用法（在 VM 上，对着真实会话或自建 Xvfb 会话都行）：
#   APP_BIN=/usr/bin/WallpaperEM bash scripts/kde-vm-ui-check.sh
#   APP_BIN=... DISPLAY=:0 bash scripts/kde-vm-ui-check.sh
#
# 依赖：at-spi2-core + python3-pyatspi + xdotool（VM 上已装）
set -uo pipefail

APP_BIN="${APP_BIN:-$HOME/WallpaperEM/src-tauri/target/release/WallpaperEM}"
export DISPLAY="${DISPLAY:-:0}"
unset XAUTHORITY
LOG="${LOG:-/tmp/wpem-ui-check.log}"

[ -x "$APP_BIN" ] || { echo "✗ 找不到可执行文件 $APP_BIN"; exit 1; }
command -v xdotool >/dev/null || { echo "✗ 缺 xdotool"; exit 1; }
python3 -c "import pyatspi" 2>/dev/null || { echo "✗ 缺 python3-pyatspi（apt install python3-pyatspi at-spi2-core）"; exit 1; }

echo "=== 设置页 UI 断言（AT-SPI）==="
echo "  APP_BIN=$APP_BIN"
echo "  DISPLAY=$DISPLAY"

# 只杀「真的是这个可执行文件」的进程。**不能用 `pkill -f "$APP_BIN"`**：调用者那条 shell
# 的命令行里就带着 `APP_BIN=/usr/bin/WallpaperEM ...`，pkill -f 会连自己一起杀掉，
# ssh 于是返回 255（实测踩过，和 kde-plugin-enable-check.sh 同一个坑）。
kill_app() {
  local abs pid
  abs="$(readlink -f "$APP_BIN" 2>/dev/null || echo "$APP_BIN")"
  for pid in $(pgrep -f "$APP_BIN" 2>/dev/null || true); do
    [ "$(readlink -f "/proc/$pid/exe" 2>/dev/null)" = "$abs" ] || continue
    kill "$pid" 2>/dev/null || true
  done
}
trap 'kill_app' EXIT

kill_app
sleep 1
setsid "$APP_BIN" >"$LOG" 2>&1 &
echo "  已启动，等窗口就绪…"
sleep 18

APP_BIN="$APP_BIN" python3 - <<'PY'
import os, subprocess, sys, time
import pyatspi

APP_BIN = os.environ["APP_BIN"]
PASS = 0
FAIL = 0
def ok(m):
    global PASS; PASS += 1; print("  [PASS] " + m)
def bad(m):
    global FAIL; FAIL += 1; print("  [FAIL] " + m)

def walk(node, depth=0, out=None, maxd=16):
    if out is None: out = []
    if depth > maxd: return out
    try:
        name = node.name or ""; role = node.getRoleName()
    except Exception:
        return out
    out.append((depth, role, name, node))
    try:
        for c in node:
            walk(c, depth + 1, out, maxd)
    except Exception:
        pass
    return out

def find_app():
    for a in pyatspi.Registry.getDesktop(0):
        try:
            if "Wallpaper" in (a.name or ""):
                return a
        except Exception:
            pass
    return None

def click(node):
    try:
        node.queryAction().doAction(0); return True
    except Exception:
        return False

def click_name(app, want, tries=40):
    # 名字是给用户看的（多语言），这里做「先找再点」；窗口刚起来时节点可能还没挂上，重试。
    for _ in range(tries):
        for d, r, n, nd in walk(app):
            if n.strip() == want:
                if click(nd):
                    return True
        time.sleep(0.5)
    return False

app = find_app()
if app is None:
    bad("无障碍树里找不到 WallpaperEM 应用节点（应用没起来？at-spi bus 不通？）")
    sys.exit(1)
ok("无障碍树里存在应用节点 WallpaperEM")

nodes = walk(app)
web = [x for x in nodes if x[1] == "document web"]
if web:
    ok("WebKitGTK 网页内容已进无障碍树（document web: %s）—— 页面确实加载了" % web[0][2])
else:
    bad("没有 document web 节点：主窗口没加载到前端页面（开发模式二进制？见报告 §6.9）")

for label, alt in (("Settings", "设置"), ("General", "通用")):
    if click_name(app, label) or click_name(app, alt):
        ok("已点开「%s / %s」" % (label, alt))
    else:
        bad("点不到「%s / %s」" % (label, alt))
time.sleep(2)

names = [n.strip() for d, r, n, nd in walk(app) if n and r == "push button"]
want = [w for w in ("Enable", "一键启用") if w in names]
if want:
    ok("设置页存在 KDE 分组按钮：%s" % "、".join(want))
else:
    # 页面长，按钮可能在视口外；按屏翻页后再找一次（WebKit 有时只暴露渲染过的区域）
    for _ in range(6):
        subprocess.run(["xdotool", "key", "--clearmodifiers", "Next"], capture_output=True)
        time.sleep(0.5)
    names = [n.strip() for d, r, n, nd in walk(app) if n and r == "push button"]
    want = [w for w in ("Enable", "一键启用") if w in names]
    if want:
        ok("翻页后找到 KDE 分组按钮：%s" % "、".join(want))
    else:
        bad("设置页里没有「Enable / 一键启用」按钮（KDE 分组没渲染？）")

# 位置佐证：分组插在「缓存」组之前，所以按钮应紧邻 Clear cache / 清除缓存
if want:
    tail = names[names.index(want[0]):][:3]
    if any(x in ("Clear cache", "清除缓存") for x in tail):
        ok("按钮紧邻「Clear cache / 清除缓存」——位置与设计一致（KDE 组在缓存组之前）")
    else:
        print("  [info] 按钮后续节点：%s" % tail)

print("\n==== 结果：%d 项通过 / %d 项失败 ====" % (PASS, FAIL))
sys.exit(1 if FAIL else 0)
PY
rc=$?
echo "  应用日志尾："
tail -3 "$LOG" | sed 's/^/    /'
exit $rc
