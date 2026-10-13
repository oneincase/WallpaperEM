#!/usr/bin/env bash
# 容器内构建入口：拷贝源码 → 装依赖 → 测试 → 打包 → 冒烟测试
# （由 scripts/build-linux-docker.sh 调用；源码以只读挂载进 /src）
set -euo pipefail

TARGET_DIR="${CARGO_TARGET_DIR:-/build-target}"
echo "==> 准备源码（隔离 macOS 产物：node_modules / target / .git 不拷贝）"
mkdir -p /build/WallpaperEM
(cd /src/app && tar \
  --exclude=./node_modules --exclude=./src-tauri/target --exclude=./.git \
  --exclude=./dist --exclude=./dist-linux --exclude=./.pnpm-store \
  --exclude=./.cargo-home --exclude=./.pnpm-store \
  -cf - .) | (cd /build/WallpaperEM && tar xf -)
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

# 冒烟档位（对齐 docs/kde-adaptation-plan.md §6.2）：
#   SMOKE_TEST=0            → 两档全跳过（scripts/build-linux-docker.sh 传的就是这个变量）
#   SMOKE_MODE=x11          → 只跑第一档（起得来就算过，原有行为）
#   SMOKE_MODE=kde          → 只跑第二档（scripts/linux-kde-smoke.sh 真断言）
#   SMOKE_MODE=all（默认）  → 两档都跑
# 第二档退出码语义：0=通过 / 1=断言失败 / 2=前置缺失（容器里没装 kwin_x11 等 → SKIP 不算失败）
# KDE_SMOKE_STRICT=1 → 断言失败时让本次构建失败（默认 0：新增档位先在容器里攒真实记录）
SMOKE_MODE="${SMOKE_MODE:-all}"

if [ "${SMOKE_TEST:-1}" = "1" ] && [ "$SMOKE_MODE" != "kde" ]; then
  echo "==> 冒烟测试（第一档，无断言）：dbus 会话 + Xvfb 无头启动 25s"
  set +e
  dbus-run-session -- xvfb-run -a sh -c "timeout 25 '$TARGET_DIR/release/WallpaperEM'" \
    > /out/smoke-test.log 2>&1
  set -e
  echo "---- smoke test 关键日志 ----"
  grep -E "setup complete|wallpaper engine ready|now playing|tray|error|panic" \
    /out/smoke-test.log | head -30 || tail -30 /out/smoke-test.log
fi

if [ "${SMOKE_TEST:-1}" = "1" ] && [ "$SMOKE_MODE" != "x11" ]; then
  echo "==> KDE/X11 冒烟（第二档，真断言）：kwin_x11 + plasmashell + 桌面层窗口断言"
  # 注意：linux-kde-smoke.sh 自己起 Xvfb 和 dbus-run-session，这里**不能**再套
  # xvfb-run/dbus-run-session（套了会变成「假 KDE 会话」，见 §6.2 的总线说明）。
  set +e
  KEEP_LOGS=1 APP="$TARGET_DIR/release/WallpaperEM" \
    bash /src/app/scripts/linux-kde-smoke.sh > /out/kde-smoke.log 2>&1
  kde_rc=$?
  set -e
  echo "---- KDE 冒烟日志尾部（完整：/out/kde-smoke.log）----"
  tail -40 /out/kde-smoke.log || true
  # 把容器内 /tmp 的取证材料也带回宿主机（容器一退，/tmp 就没了）
  mkdir -p /out/kde-smoke
  cp -f /tmp/kde-smoke.*/* /out/kde-smoke/ 2>/dev/null || true
  cp -f /tmp/kde-smoke-screen.png /out/ 2>/dev/null || true
  case "$kde_rc" in
    0) echo "---- KDE 冒烟：PASS ----" ;;
    2) echo "---- KDE 冒烟：SKIP（前置缺失，原因见上面 [FAIL] 行）----" ;;
    *)
      echo "---- KDE 冒烟：FAIL（rc=${kde_rc}）----"
      if [ "${KDE_SMOKE_STRICT:-0}" = "1" ]; then
        exit 1
      fi
      echo "     KDE_SMOKE_STRICT=0：不让本次打包失败；要当门禁用 KDE_SMOKE_STRICT=1 重跑"
      ;;
  esac
fi

echo "==> 完成。产物在 /out："
ls -lh /out
