#!/usr/bin/env bash
# KDE 验证机装配（在 10.211.55.5 上以 root 执行；由宿主经 ssh 'sudo -n bash -s' 注入）
# 目的：装上 KDE 桌面，供 docs/kde-adaptation-plan.md 的真机验证使用。
# 注意：本脚本只做「装」，不做断言；验证断言在 scripts/linux-kde-smoke.sh。
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive
LOG=/root/kde-setup.log
exec > >(tee -a "$LOG") 2>&1

step() { printf '\n=== [%s] %s ===\n' "$(date +%H:%M:%S)" "$*"; }

step "apt update"
apt-get update -y

step "记录装前状态（便于回滚对比）"
dpkg --get-selections > /root/pre-kde-selections.txt
apt-mark showmanual > /root/pre-kde-manual.txt
echo "已记录 $(wc -l < /root/pre-kde-selections.txt) 条 selection"

step "基础工具 xvfb / dbus-x11 / qdbus"
apt-get install -y --no-install-recommends xvfb dbus-x11

step "声明默认 DM = gdm3（非交互，避免弹窗挂死；保留 GNOME 会话入口）"
echo "gdm3 shared/default-x-display-manager select gdm3" | debconf-set-selections
echo "sddm shared/default-x-display-manager select gdm3" | debconf-set-selections

step "安装 kubuntu-desktop（大件，1.5-2GB 下载）"
apt-get install -y kubuntu-desktop

step "安装 Wayland 会话与嵌套合成器支持"
apt-get install -y plasma-workspace-wayland kwin-wayland || echo "WARN: plasma-workspace-wayland/kwin-wayland 安装失败，继续"

step "收集版本事实"
{
  echo "--- plasmashell ---"; plasmashell --version 2>&1 || true
  echo "--- kwin_x11 ---"; kwin_x11 --version 2>&1 || true
  echo "--- kwin_wayland --version ---"; kwin_wayland --version 2>&1 || true
  echo "--- kwin_wayland --virtual 支持 ---"; kwin_wayland --help 2>&1 | grep -n -- '--virtual' || echo "NO --virtual"
  echo "--- 会话入口 ---"; ls -l /usr/share/xsessions/ /usr/share/wayland-sessions/ 2>&1 || true
  echo "--- 关键包版本 ---"
  dpkg-query -W -f='${Package} ${Version}\n' plasma-desktop plasma-workspace kwin-x11 kwin-wayland sddm plasma-workspace-wayland 2>&1 || true
  echo "--- qdbus / kscreen-doctor ---"
  command -v qdbus qdbus6 kscreen-doctor plasma-apply-wallpaperimage 2>&1 || true
  echo "--- 磁盘 ---"; df -h / | tail -1
} | tee /root/kde-version-report.txt

step "完成（日志 $LOG，版本报告 /root/kde-version-report.txt）"
