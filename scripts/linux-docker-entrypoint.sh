#!/usr/bin/env bash
# 容器内构建入口：拷贝源码 → 装依赖 → 测试 → 打包 → 冒烟测试
# （由 scripts/build-linux-docker.sh 调用；源码以只读挂载进 /src）
set -euo pipefail

TARGET_DIR="${CARGO_TARGET_DIR:-/build-target}"
echo "==> 准备源码（隔离 macOS 产物：node_modules / target / .git 不拷贝）"
mkdir -p /build/WallpaperEM /build/webwallgl-github
(cd /src/app && tar \
  --exclude=./node_modules --exclude=./src-tauri/target --exclude=./.git \
  --exclude=./dist --exclude=./dist-linux --exclude=./.pnpm-store \
  --exclude=./.cargo-home --exclude=./.pnpm-store \
  -cf - .) | (cd /build/WallpaperEM && tar xf -)
# webwallgl 的 dist/lib 是纯 JS 构建产物（跨平台），直接复用宿主编译结果
cp -a /src/webwallgl/dist /build/webwallgl-github/dist
# media-bridge 是 Cargo path 依赖（src-tauri/Cargo.toml 里 ../../media-bridge），
# 必须与 WallpaperEM 平级落在 /build 下，否则 cargo 解析不到（源码很小，整棵拷）
cp -a /src/media-bridge /build/media-bridge

cd /build/WallpaperEM
echo "==> pnpm install"
pnpm install --frozen-lockfile

echo "==> 前端构建（tsc + vite）"
pnpm build

echo "==> Rust 测试（release  profile，与打包共用同一编译产物）"
cd src-tauri
CARGO_TARGET_DIR="$TARGET_DIR" cargo test --release
cd ..

echo "==> Tauri 打包（deb + AppImage）"
pnpm tauri build --bundles deb,appimage

mkdir -p /out
cp -v "$TARGET_DIR"/release/bundle/deb/*.deb /out/ 2>/dev/null || true
cp -v "$TARGET_DIR"/release/bundle/appimage/*.AppImage /out/ 2>/dev/null || true

if [ "${SMOKE_TEST:-1}" = "1" ]; then
  echo "==> 冒烟测试：dbus 会话 + Xvfb 无头启动 25s"
  set +e
  dbus-run-session -- xvfb-run -a sh -c "timeout 25 '$TARGET_DIR/release/WallpaperEM'" \
    > /out/smoke-test.log 2>&1
  set -e
  echo "---- smoke test 关键日志 ----"
  grep -E "setup complete|wallpaper engine ready|now playing|tray|error|panic" \
    /out/smoke-test.log | head -30 || tail -30 /out/smoke-test.log
fi

echo "==> 完成。产物在 /out："
ls -lh /out
