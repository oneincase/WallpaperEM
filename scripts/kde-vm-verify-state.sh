#!/usr/bin/env bash
# 只读体检：确认真机验证机在 I/O 故障后已恢复到可构建状态。
# 由宿主执行：ssh parallels@10.211.55.5 'bash -s' < scripts/kde-vm-verify-state.sh
#
# 退出码：0 = 全部通过；1 = 存在阻塞项（逐条标 [BLOCK]）。
# 不写任何远程状态、不装包、不构建。

set -uo pipefail

VM_HOST="${VM_HOST:-parallels@10.211.55.5}"
FAIL=0

say()  { printf '\n=== %s ===\n' "$*"; }
ok()   { printf '  [OK]    %s\n' "$*"; }
warn() { printf '  [WARN]  %s\n' "$*"; }
block(){ printf '  [BLOCK] %s\n' "$*"; FAIL=1; }

say "主机 / 内核"
printf '  host: %s\n' "$(hostname)"
printf '  arch: %s\n' "$(uname -m)"
printf '  kernel: %s\n' "$(uname -r)"
printf '  uptime: %s\n' "$(uptime -p 2>/dev/null || uptime)"

say "根分区挂载状态（I/O 故障后必须不再是 emergency_ro）"
ROOTLINE="$(findmnt -no SOURCE,FSTYPE,OPTIONS / 2>/dev/null || mount | grep -E '^/dev/[^ ]+ on / ')"
printf '  %s\n' "$ROOTLINE"
case "$ROOTLINE" in
  *emergency_ro*) block "根分区仍为 emergency_ro，ext4 日志未恢复；先在 VM 内重启/fsck，不要开始构建" ;;
  *ro,*)          block "根分区以只读挂载" ;;
  *)              ok "根分区可写" ;;
esac

say "ext4 / 内核错误（重启后应无新条目）"
if sudo -n dmesg >/dev/null 2>&1; then
  ERRS="$(sudo -n dmesg | grep -Ei 'ext4.*(error|aborted)|i/o error|Input/output error' | tail -8)"
  if [ -n "$ERRS" ]; then
    printf '%s\n' "$ERRS" | sed 's/^/  /'
    warn "上面若时间戳早于本次启动则是旧故障的残留；若仍在增长则是盘还在报错"
  else
    ok "dmesg 无 ext4/I-O 错误"
  fi
else
  warn "无法读 dmesg（sudo -n 不可用？）"
fi

say "磁盘余量"
df -h / /boot 2>/dev/null | sed 's/^/  /'

say "dpkg 未配置包（I/O 故障会留下一堆 unpacked-not-configured）"
AUDIT="$(sudo -n dpkg --audit 2>&1 | head -6)"
if [ -n "$AUDIT" ]; then
  printf '%s\n' "$AUDIT" | sed 's/^/  /'
  block "存在未配置包，先 sudo -n dpkg --configure -a 再验证"
else
  ok "dpkg 状态干净"
fi

say "关键构建依赖"
MISSING=""
for p in build-essential libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
         libxdo-dev xdotool x11-utils imagemagick kwin-x11 plasma-workspace xvfb dbus-x11; do
  dpkg -s "$p" >/dev/null 2>&1 || MISSING="$MISSING $p"
done
if [ -n "$MISSING" ]; then
  warn "缺失:$MISSING（kde-vm-build.sh 会补装，但先确认文件系统可写）"
else
  ok "全部就位"
fi

say "KDE / Plasma 版本"
for c in plasmashell kwin_x11 kwin_wayland qdbus kscreen-doctor plasma-apply-wallpaperimage; do
  p="$(command -v "$c" 2>/dev/null)"
  printf '  %-30s %s\n' "$c" "${p:-<缺失>}"
done
printf '  plasmashell: %s\n' "$(plasmashell --version 2>/dev/null | head -1 || echo '<需图形会话>')"
printf '  kwin:        %s\n' "$(kwin_wayland --version 2>/dev/null | tail -1 || echo '<需图形会话>')"

say "源码与 media-bridge（Cargo path 依赖 ../../media-bridge）"
for p in "$HOME/WallpaperEM/src-tauri/Cargo.toml" "$HOME/WallpaperEM/dist/index.html" \
         "$HOME/media-bridge/crates/media-bridge/Cargo.toml"; do
  if [ -f "$p" ]; then ok "$p"; else block "缺少 $p"; fi
done
if [ -d "$HOME/media-bridge/target" ]; then
  warn "~/media-bridge/target 又出现了（陈旧调试产物，跨平台链接风险）：$(du -sh "$HOME/media-bridge/target" 2>/dev/null | cut -f1)"
else
  ok "~/media-bridge/target 不存在（符合预期）"
fi
printf '  ~/WallpaperEM:   %s\n' "$(du -sh "$HOME/WallpaperEM" 2>/dev/null | cut -f1)"
printf '  ~/media-bridge:  %s\n' "$(du -sh "$HOME/media-bridge" 2>/dev/null | cut -f1)"

say "Rust 工具链"
export PATH="$HOME/.cargo/bin:$PATH"
if command -v cargo >/dev/null 2>&1; then
  printf '  %s\n' "$(rustc --version)"
  printf '  %s\n' "$(cargo --version)"
else
  block "cargo 不在 PATH（~/.cargo/bin）"
fi

say "图形会话"
printf '  XDG_CURRENT_DESKTOP=%s\n' "${XDG_CURRENT_DESKTOP:-<空>}"
printf '  XDG_SESSION_TYPE=%s\n' "${XDG_SESSION_TYPE:-<空>}"
printf '  DISPLAY=%s  WAYLAND_DISPLAY=%s\n' "${DISPLAY:-<空>}" "${WAYLAND_DISPLAY:-<空>}"
loginctl list-sessions --no-legend 2>/dev/null | sed 's/^/  /'

if [ "$FAIL" = "0" ]; then
  printf '\n结论：可进入构建（scripts/kde-vm-build.sh）。\n'
else
  printf '\n结论：仍有阻塞项，先处理上面 [BLOCK] 再构建。\n'
fi
exit "$FAIL"
