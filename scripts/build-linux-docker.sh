#!/usr/bin/env bash
# 本地 Docker 打包/测试 Linux 版（默认 linux/arm64，与宿主机同架构速度快；
# x86_64 包请走 GitHub Actions build-linux.yml，或加 --amd64 走 QEMU/Rosetta 模拟）
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
WEBWALLGL="${WEBWALLGL_DIR:-$REPO/../webwallgl-github}"
PLATFORM="linux/arm64"
TAG="wpem-linux-build:arm64"
TARGET_VOL="wpem-target-linux-arm64"

if [ "${1:-}" = "--amd64" ]; then
  PLATFORM="linux/amd64"
  TAG="wpem-linux-build:amd64"
  TARGET_VOL="wpem-target-linux-amd64"
fi

[ -d "$WEBWALLGL/dist/lib" ] || { echo "缺少 $WEBWALLGL/dist/lib（webwallgl 库产物）"; exit 1; }

mkdir -p "$REPO/dist-linux"

echo "==> 构建镜像 $TAG ($PLATFORM)"
docker build --platform "$PLATFORM" -f "$REPO/Dockerfile.linux" -t "$TAG" "$REPO"

echo "==> 容器内构建（cargo/pnpm 缓存走 named volume，二次构建会很快）"
docker run --rm --platform "$PLATFORM" \
  -v "$REPO":/src/app:ro \
  -v "$WEBWALLGL":/src/webwallgl:ro \
  -v wpem-cargo-registry:/root/.cargo/registry \
  -v wpem-pnpm-store:/root/.local/share/pnpm \
  -v "$TARGET_VOL":/build-target \
  -v "$REPO/dist-linux":/out \
  -e CARGO_TARGET_DIR=/build-target \
  -e SMOKE_TEST="${SMOKE_TEST:-1}" \
  "$TAG"

echo "==> 产物已输出到 $REPO/dist-linux"
ls -lh "$REPO/dist-linux"
