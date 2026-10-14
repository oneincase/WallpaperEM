#!/usr/bin/env bash
# 在 KDE 验证机上构建 WallpaperEM（供 scripts/linux-kde-smoke.sh 使用）。
# 由宿主 rsync 源码后执行：ssh 'bash -s' < scripts/kde-vm-build.sh
#
# 为什么不用 tauri build：本机（arm64 VM / 2 vCPU / 3.8 GiB）只需**可执行的二进制**来验窗口语义，
# 不需要 deb/AppImage 打包链。且前端 dist/ 由宿主侧已构建好一并 rsync 过来，VM 上不需要 node/pnpm
# —— 因此只装 Rust 侧的系统依赖。若将来要打 deb，再改用 `pnpm tauri build`。
#
# 用法：bash scripts/kde-vm-build.sh [--debug]
#   --debug 用 debug profile（编译快 2-3 倍，代价是运行时慢、体积大，冒烟够用）

set -uo pipefail

REPO="${REPO:-$HOME/WallpaperEM}"
PROFILE="release"
PROFILE_FLAG="--release"
[ "${1:-}" = "--debug" ] && { PROFILE="debug"; PROFILE_FLAG=""; }

step() { printf '\n=== %s ===\n' "$*"; }

step "系统依赖（Tauri/WebKitGTK 构建链）"
# 与 .github/workflows/build-linux.yml 的 apt 清单保持一致，另加 x11-utils/xdotool 供冒烟断言
sudo -n apt-get install -y --no-install-recommends \
  build-essential curl wget file \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
  libxdo-dev librsvg2-dev patchelf pkg-config \
  libssl-dev libdbus-1-dev \
  libglib2.0-dev libjavascriptcoregtk-4.1-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-libav \
  libx11-dev libxcb1-dev \
  xvfb dbus-x11 xdotool x11-utils imagemagick kwin-x11 plasma-workspace \
  || { echo "apt 安装失败"; exit 1; }

step "确认关键 dev 包"
for p in libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev libxdo-dev kwin-x11; do
  dpkg -s "$p" >/dev/null 2>&1 && echo "  OK  $p" || echo "  !!  $p 缺失"
done

step "检查源码"
if [ ! -d "$REPO/src-tauri" ]; then echo "找不到 $REPO/src-tauri（先 rsync 源码）"; exit 1; fi
if [ ! -f "$REPO/dist/index.html" ]; then echo "警告：$REPO/dist/index.html 缺失，前端资源可能没 rsync 过来"; fi
cd "$REPO"
echo "  repo: $(pwd)"
echo "  git: $(git log -1 --format='%h %s' 2>/dev/null || echo 'no git')"
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version; cargo --version

step "cargo build $PROFILE_FLAG（2 vCPU，预计 30-90 分钟）"
cd "$REPO/src-tauri"
BUILD_LOG="/tmp/wallpaperem-build.log"
# CARGO_BUILD_JOBS=2：VM 只有 2 vCPU，避免并行链接把 3.8 GiB 吃爆
# 输出同时进日志与 stdout（tail -f 不适合无人值守；用 tee 让宿主后台作业能看到进度）
#
# ⚠️ `--features tauri/custom-protocol` 不能省：tauri 的 build.rs 是
#    `let dev = !custom_protocol;`（tauri-2.11.5/build.rs:256-260），没有这个 feature
#    编出来的二进制**是开发模式**，主窗口会去连 `devUrl`（localhost:1420）→ 用户看到的
#    是「Could not connect to localhost: Connection refused」。官方 `tauri build` 会自动
#    带上它，手工 cargo build 必须自己带（真机踩过：装好的 deb 打开就是连接被拒）。
CARGO_BUILD_JOBS=2 cargo build $PROFILE_FLAG --locked --features tauri/custom-protocol 2>&1 | tee "$BUILD_LOG"
RC=${PIPESTATUS[0]}
if [ "$RC" != "0" ]; then
  echo
  echo "构建失败（rc=$RC），最后 40 行："
  tail -40 "$BUILD_LOG"
  echo "若是 --locked 因 Cargo.lock 与源码不匹配，可去掉 --locked 重试。"
  exit "$RC"
fi

BIN="$REPO/src-tauri/target/$PROFILE/WallpaperEM"
if [ -x "$BIN" ]; then
  echo
  echo "构建成功: $BIN"
  ls -lh "$BIN"
  "$BIN" --version 2>&1 | head -2 || true
else
  echo "构建返回 0 但找不到 $BIN"; ls target/ 2>/dev/null; exit 1
fi
