#!/usr/bin/env bash
# 在 KDE 验证机上把当前源码打成 .deb、装进系统，用来"装最新版本测一遍"。
# 由宿主 rsync 源码后执行：ssh 'bash -s' < scripts/kde-vm-deb.sh
#
# 为什么不用 `pnpm tauri build`（官方 bundler）：
#   tauri-cli 的预编译产物没有 aarch64-unknown-linux-gnu —— GitHub release 的
#   cargo-tauri-*.tgz 只发 x86_64 与 riscv64（2.11.5/2.11.4/2.11.0/2.10/2.9 都查过），
#   而这台 VM 是 arm64；从源码编译 tauri-cli 在 2 vCPU 上代价太大。
#   所以这里按 tauri-bundler 的 deb 布局手工组装：
#     usr/bin/<exe>、usr/lib/<exe>/<resources...>
#   依据是 tauri 2.11.5 自己的文档注释（src/path/desktop.rs:222-231）：
#     Linux 非 AppImage 下 resource_dir() == /usr/lib/${exe_name}
#   —— 应用找打包进去的 plasma 壁纸包、renderer、libsteam_api.so 都走这个路径，
#   所以布局必须和 bundler 一致；资源清单直接从两份 tauri 配置里读，不写死。
#   CI（.github/workflows/build-linux.yml，x64 + arm64 runner 上有 CLI）出的才是
#   官方 bundler 产物；本脚本产物仅用于本机测试。
#
# 用法：REPO=~/WallpaperEM OUT=~/deb-out bash scripts/kde-vm-deb.sh [--no-build]

set -uo pipefail

REPO="${REPO:-$HOME/WallpaperEM}"
OUT="${OUT:-$HOME/deb-out}"
STAGE="/tmp/wem-deb-stage"
ARCH="$(dpkg --print-architecture)"
DO_BUILD=1
[ "${1:-}" = "--no-build" ] && DO_BUILD=0

step() { printf '\n=== %s ===\n' "$*"; }
info() { printf '  %s\n' "$*"; }
ok() { printf '  [OK]    %s\n' "$*"; }
die() { printf '  [失败]  %s\n' "$*"; exit 1; }

step "解析 tauri 配置（base + linux 覆盖；resources 为深合并）"
[ -f "$REPO/src-tauri/tauri.conf.json" ] || die "找不到 $REPO/src-tauri/tauri.conf.json（先 rsync 源码）"
RES_MAP="$(python3 - "$REPO" <<'PY'
import json, os, re, sys
repo = sys.argv[1]
base = json.load(open(os.path.join(repo, "src-tauri/tauri.conf.json"), encoding="utf-8"))
linux = json.load(open(os.path.join(repo, "src-tauri/tauri.linux.conf.json"), encoding="utf-8"))
def bundle(cfg):
    return cfg.get("bundle", {}) or {}
# 平台覆盖是深合并：对象里的键取并集
resources = dict(bundle(base).get("resources") or {})
resources.update(bundle(linux).get("resources") or {})
for src, dst in resources.items():
    print("RES\t%s\t%s" % (src, dst))
# deb 元数据在 base 的 bundle.linux.deb 里（tauri.conf.json 的 linux 块），覆盖文件里也可能有
linuxblock = dict(bundle(base).get("linux") or {})
linuxblock.update(bundle(linux).get("linux") or {})
deb = dict(linuxblock.get("deb") or {})
deb.update(bundle(linux).get("deb") or {})
# tauri-bundler 会在这两条之外自动追加 WebKitGTK/GTK 运行时依赖，手工组装要对齐
depends = list(deb.get("depends") or [])
for extra in ("libwebkit2gtk-4.1-0", "libgtk-3-0"):
    if extra not in depends:
        depends.append(extra)
product = base.get("productName", "")
# tauri-bundler 的报名 = productName 转 kebab-case（WallpaperEM → wallpaper-em）。
# 必须与官方包一致，否则 apt 当两个包处理、撞 /usr/bin/WallpaperEM。
pkg = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "-", product).lower()
print("META\t%s\t%s\t%s" % (product, base.get("version", ""), base.get("identifier", "")))
print("PKG\t%s" % pkg)
print("DEB\t%s\t%s" % (", ".join(depends), ", ".join(deb.get("recommends") or [])))
print("CAT\t%s" % (bundle(linux).get("category") or bundle(base).get("category") or ""))
PY
)" || die "解析 tauri 配置失败"
PRODUCT=""; VERSION=""; IDENT=""; DEPENDS=""; RECOMMENDS=""; CATEGORY=""; PKG=""
while IFS=$'\t' read -r kind a b c; do
  case "$kind" in
    META) PRODUCT="$a"; VERSION="$b"; IDENT="$c" ;;
    PKG)  PKG="$a" ;;
    DEB)  DEPENDS="$a"; RECOMMENDS="$b" ;;
    CAT)  CATEGORY="$a" ;;
  esac
done <<< "$RES_MAP"
[ -n "$PRODUCT" ] && [ -n "$VERSION" ] && [ -n "$PKG" ] || die "配置里读不到 productName/version"
info "product=$PRODUCT package=$PKG version=$VERSION arch=$ARCH category=${CATEGORY:-（无）}"
info "depends=$DEPENDS"
info "recommends=$RECOMMENDS"

# resources 里混着 **build.rs 生成的产物**（`bundled/libsteam_api.so`，见
# src-tauri/build.rs:155）：它被 .gitignore 忽略、所以不随 rsync 来；若某次同步把它删了，
# 而这次又传了 --no-build，下面「组装目录树」就会以「资源缺失」失败。所以先探一遍：
# 缺了就强制构建，并且 touch build.rs —— 否则 cargo 认为 build script 仍是新鲜的，
# 不会重跑、也就不会重写 bundled/（这正是刚才踩到的坑）。
MISSING_RES=""
while IFS=$'\t' read -r kind a b; do
  [ "$kind" = "RES" ] || continue
  [ -e "$REPO/src-tauri/$a" ] || MISSING_RES="$MISSING_RES $a"
done <<< "$RES_MAP"
if [ -n "$MISSING_RES" ]; then
  info "这些资源由构建生成、当前缺失：$MISSING_RES → 强制重新构建"
  DO_BUILD=1
  touch "$REPO/src-tauri/build.rs"
fi

if [ "$DO_BUILD" = "1" ]; then
  step "构建 release 二进制（前端 dist 编进二进制，所以宿主必须先 rsync 新 dist）"
  cd "$REPO/src-tauri" || die "找不到 $REPO/src-tauri"
  export PATH="$HOME/.cargo/bin:$PATH"
  BUILD_LOG=/tmp/wem-deb-build.log
  # CARGO_BUILD_JOBS=2：VM 只有 2 vCPU，避免并行链接把 3.8 GiB 吃爆
  # --features tauri/custom-protocol：没有它编出来是**开发模式**二进制，主窗口连
  # devUrl（localhost:1420）→「Connection refused」（tauri-2.11.5/build.rs:256 `dev = !custom_protocol`）
  CARGO_BUILD_JOBS=2 cargo build --release --locked --features tauri/custom-protocol 2>&1 | tee "$BUILD_LOG" | tail -3
  RC="${PIPESTATUS[0]}"
  [ "$RC" = "0" ] || { tail -30 "$BUILD_LOG"; die "cargo build 失败（rc=$RC）"; }
fi

BIN="$REPO/src-tauri/target/release/$PRODUCT"
[ -x "$BIN" ] || die "没产出 $BIN"
ok "二进制 $BIN（$(du -h "$BIN" | cut -f1)）"
[ -f "$REPO/dist/index.html" ] || die "缺 $REPO/dist/index.html（宿主先 pnpm build 再 rsync）"

step "组装目录树（镜像 tauri-bundler：usr/bin + usr/lib/$PRODUCT）"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/usr/bin" "$STAGE/usr/lib/$PRODUCT" \
  "$STAGE/usr/share/applications" \
  "$STAGE/usr/share/icons/hicolor/32x32/apps" \
  "$STAGE/usr/share/icons/hicolor/128x128/apps" \
  "$STAGE/usr/share/icons/hicolor/256x256@2/apps"
install -m 0755 "$BIN" "$STAGE/usr/bin/$PRODUCT"

while IFS=$'\t' read -r kind a b; do
  [ "$kind" = "RES" ] || continue
  SRC="$REPO/src-tauri/$a"
  DST="$STAGE/usr/lib/$PRODUCT/$b"
  [ -e "$SRC" ] || die "资源缺失：$SRC（tauri.linux.conf.json 引用）"
  mkdir -p "$(dirname "$DST")"
  cp -a "$SRC" "$DST"
  # 权限归一：安装包里的资源要能被任意用户读（app 以普通用户跑，还会把它们
  # 拷进 ~/.local/share/plasma/wallpapers/）。源工作区若是 umask 077 检出的
  # 0600，装成 root:root 0600 后普通用户读不了 —— 实测踩过。
  find "$DST" -type d -exec chmod 755 {} +
  find "$DST" -type f -exec chmod 644 {} +
  ok "资源 $a → usr/lib/$PRODUCT/$b"
done <<< "$RES_MAP"

# 目录名对齐官方 deb（tauri-bundler 把 128x128@2x.png 放进 hicolor/256x256@2）
for spec in "32x32/icons/32x32.png" "128x128/icons/128x128.png" "256x256@2/icons/128x128@2x.png"; do
  SIZE="${spec%%/*}"; ICON="${spec#*/}"
  [ -f "$REPO/src-tauri/$ICON" ] || die "缺图标 $REPO/src-tauri/$ICON"
  cp -a "$REPO/src-tauri/$ICON" "$STAGE/usr/share/icons/hicolor/$SIZE/apps/$PRODUCT.png"
done
ok "图标 32x32 / 128x128 / 256x256@2"

cat > "$STAGE/usr/share/applications/$PRODUCT.desktop" <<EOF
[Desktop Entry]
Categories=${CATEGORY:-Utility};
Comment=$PRODUCT
Exec=$PRODUCT
Icon=$PRODUCT
Name=$PRODUCT
StartupWMClass=$PRODUCT
Terminal=false
Type=Application
EOF
ok "桌面项 usr/share/applications/$PRODUCT.desktop"

SIZE_KB="$(du -sk "$STAGE/usr" | cut -f1)"
{
  echo "Package: $PKG"
  echo "Version: $VERSION"
  echo "Architecture: $ARCH"
  echo "Maintainer: oneincase <oneincase@users.noreply.github.com>"
  echo "Depends: $DEPENDS"
  [ -n "$RECOMMENDS" ] && echo "Recommends: $RECOMMENDS"
  echo "Section: utils"
  echo "Priority: optional"
  echo "Installed-Size: $SIZE_KB"
  echo "Description: $PRODUCT"
  echo " Wallpaper Engine 动态壁纸播放器（Tauri + WebKitGTK，含 KDE Plasma 壁纸插件包）。"
} > "$STAGE/DEBIAN/control"
# 与 bundler 一致：装完刷新桌面数据库/图标缓存（没有对应命令就跳过，绝不因此失败）
cat > "$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
exit 0
EOF
cat > "$STAGE/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true
exit 0
EOF
chmod 0755 "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/postrm"
ok "control（Installed-Size=${SIZE_KB}KB）"

step "dpkg-deb 打包"
rm -rf "$OUT"; mkdir -p "$OUT"
DEB="$OUT/${PKG}_${VERSION}_${ARCH}.deb"
dpkg-deb --build --root-owner-group -Zxz "$STAGE" "$DEB" >/dev/null 2>&1 || die "dpkg-deb 打包失败"
ok "$DEB（$(du -h "$DEB" | cut -f1)）"
dpkg-deb -f "$DEB" Package Version Architecture Depends | sed 's/^/    /'

step "apt 安装（走依赖解析）"
# --reinstall：版本号没变时 apt 会认为「已是最新」直接跳过，新装出来的 deb 就白打了
# （实测踩过：权限修复后的重装被跳过，装着的还是旧文件）。
sudo -n apt-get install -y --reinstall --allow-downgrades "$DEB" 2>&1 | tail -4 || die "apt 安装失败"
ok "已安装 $(dpkg-query -W -f='${Package} ${Version}\n' "$PKG" 2>/dev/null)"

step "校验安装后的文件与资源目录"
for p in "/usr/bin/$PRODUCT" \
         "/usr/lib/$PRODUCT/renderer/index.html" \
         "/usr/lib/$PRODUCT/plasma-wallpaper/io.github.oneincase.wallpaperem/metadata.json" \
         "/usr/share/applications/$PRODUCT.desktop"; do
  if [ -e "$p" ]; then ok "$p"; else die "缺少 $p"; fi
done
info "usr/lib/$PRODUCT 内容："
ls -1 "/usr/lib/$PRODUCT" | sed 's/^/    /'
info "resource_dir() 期望值 = /usr/lib/$PRODUCT（tauri 2.11.5 Linux 非 AppImage 语义）"

printf '\n装好了。下一步可用安装后的二进制跑一遍端到端自检：\n'
printf '  APP_BIN=/usr/bin/%s bash -s < scripts/kde-plugin-enable-check.sh\n' "$PRODUCT"
