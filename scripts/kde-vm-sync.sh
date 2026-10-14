#!/usr/bin/env bash
# 把源码同步到 KDE 验证机（10.211.55.5），供 scripts/kde-vm-build.sh 构建。
#
# 为什么单独成脚本：之前用裸 rsync --exclude 'target/' 同步时，media-bridge 根级的
# target/ 没有被排除（相对模式匹配不到传输根，且 --delete 又与时间戳比较交互），
# 结果把 2.9G 跨平台的陈旧调试产物带进了 VM。这里改用 **锚定传输根** 的
# --exclude=/target/（仅根级），并在 media-bridge 上额外加 --delete-excluded
# 把目标端可能已存在的 target/ 清掉。
#
# 用法：
#   bash scripts/kde-vm-sync.sh              # 同步仓库 + media-bridge
#   bash scripts/kde-vm-sync.sh --repo-only  # 只同步仓库
#   BW=0 bash scripts/kde-vm-sync.sh         # 不限速（默认可控，避免与 VM 上其它 I/O 抢）
#
# 退出码：0 = 同步完成且校验通过；1 = 同步或校验失败（失败提示见结尾）。

set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MEDIA_BRIDGE="${MEDIA_BRIDGE_DIR:-$REPO/../media-bridge}"
VM_HOST="${VM_HOST:-parallels@10.211.55.5}"
VM_REPO="${VM_REPO:-/home/parallels/WallpaperEM}"
VM_MEDIA_BRIDGE="${VM_MEDIA_BRIDGE:-/home/parallels/media-bridge}"
BW="${BW:-8000}"

REPO_ONLY=0
[ "${1:-}" = "--repo-only" ] && REPO_ONLY=1

say() { printf '\n=== %s ===\n' "$*"; }

# 只锚定传输根，不递归命中子目录里的 target/
#
# **必须显式排除 `src-tauri/target/`**（2026-10-14 实测事故）：`--exclude=/target/` 只匹配
# 传输根的 target/，于是 `src-tauri/target/`（宿主 macOS 的 16G 构建产物）会被一起同步，
# 而 `--delete` 会顺手删掉 VM 上 Linux 的增量构建缓存（日志里能看到
# `cannot delete non-empty directory: src-tauri/target/debug/build/ring-*/out`），
# 既毁缓存又把两平台产物混在一起。同步前请确保宿主没有正在跑的 cargo（文件被并发
# 写会让 rsync 报 rc=23 partial transfer）。
EXCLUDES=(
  --exclude=/target/
  --exclude=/src-tauri/target/
  --exclude=/media-bridge/target/
  --exclude=/node_modules/
  --exclude=/.git/
  --exclude=/.DS_Store
  --exclude=/.svelte-kit/
  --exclude=*.o
  --exclude=*.rlib
  --exclude=*.rmeta
)

BW_FLAG=()
[ "$BW" != "0" ] && BW_FLAG=(--bwlimit="$BW")

say "连通性"
if ! ssh -o BatchMode=yes -o ConnectTimeout=8 "$VM_HOST" 'echo reachable' >/dev/null 2>&1; then
  echo "无法免密连接 $VM_HOST。确认 VM 已开机、SSH 可用（ssh-copy-id 过的公钥仍在）。"
  exit 1
fi
echo "  SSH OK: $VM_HOST"

say "同步仓库 → $VM_REPO"
rsync -az --delete --partial "${BW_FLAG[@]}" "${EXCLUDES[@]}" \
  "$REPO/" "$VM_HOST:$VM_REPO/"
RC_REPO=$?
echo "  rsync rc=$RC_REPO"
[ "$RC_REPO" != "0" ] && { echo "仓库同步失败（rc=$RC_REPO）"; exit 1; }

if [ "$REPO_ONLY" = "0" ]; then
  say "同步 media-bridge → $VM_MEDIA_BRIDGE（Cargo path 依赖 ../../media-bridge）"
  if [ ! -d "$MEDIA_BRIDGE" ]; then
    echo "找不到 $MEDIA_BRIDGE。它是 src-tauri/Cargo.toml:69 的 path 依赖，必须存在。"
    exit 1
  fi
  # --delete-excluded：顺手清掉目标端可能残留的 target/
  rsync -az --delete --delete-excluded --partial "${BW_FLAG[@]}" "${EXCLUDES[@]}" \
    "$MEDIA_BRIDGE/" "$VM_HOST:$VM_MEDIA_BRIDGE/"
  RC_MB=$?
  echo "  rsync rc=$RC_MB"
  [ "$RC_MB" != "0" ] && { echo "media-bridge 同步失败（rc=$RC_MB）"; exit 1; }
fi

say "远端校验"
ssh -o BatchMode=yes "$VM_HOST" bash -s <<EOF
set -uo pipefail
fail=0
for p in "$VM_REPO/src-tauri/Cargo.toml" "$VM_REPO/dist/index.html" "$VM_MEDIA_BRIDGE/crates/media-bridge/Cargo.toml"; do
  if [ -f "\$p" ]; then printf '  [OK]    %s\n' "\$p"; else printf '  [BLOCK] 缺少 %s\n' "\$p"; fail=1; fi
done
if [ -d "$VM_MEDIA_BRIDGE/target" ]; then
  printf '  [WARN]  %s/target 仍存在：%s（应为空）\n' "$VM_MEDIA_BRIDGE" "\$(du -sh "$VM_MEDIA_BRIDGE/target" 2>/dev/null | cut -f1)"
else
  printf '  [OK]    %s/target 不存在\n' "$VM_MEDIA_BRIDGE"
fi
printf '  repo %s: %s\n' "$VM_REPO" "\$(du -sh "$VM_REPO" 2>/dev/null | cut -f1)"
printf '  media-bridge: %s\n' "\$(du -sh "$VM_MEDIA_BRIDGE" 2>/dev/null | cut -f1)"
printf '  根分区: %s\n' "\$(df -h / | tail -1)"
exit \$fail
EOF
RC_VERIFY=$?

if [ "$RC_VERIFY" = "0" ]; then
  printf '\n同步完成。下一步：ssh %s %s < scripts/kde-vm-build.sh\n' "$VM_HOST" "'bash -s'"
else
  printf '\n同步有缺失项，见上面 [BLOCK]。\n'
fi
exit "$RC_VERIFY"
