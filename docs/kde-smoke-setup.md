# KDE 验证机装配说明（10.211.55.5）

> 配套：`docs/kde-adaptation-plan.md`（方案）、`scripts/kde-verify-preflight.sh`（环境预检）、`scripts/linux-kde-smoke.sh`（冒烟，P3 产物）。

## 0. 这台机器是什么

| 项 | 值 |
| --- | --- |
| 主机名 | `ubuntu-gnu-linux-24-04-3` |
| 系统 | Ubuntu 24.04.3 LTS（noble），**aarch64** |
| 内核 | `6.14.0-27-generic` |
| 资源 | 2 vCPU / 3.8 GiB RAM（约 2.0 GiB 可用）/ 根分区 62 GB（41 GB 可用） |
| 现有桌面 | `ubuntu-desktop-minimal` + **gdm3 46.2**（GNOME），无任何 KDE 包 |
| 当前会话 | `Type=tty`（SSH），**没有图形会话在跑** |
| SSH | `parallels@10.211.55.5:22` 免密可用（宿主 `~/.ssh/id_ed25519`） |
| sudo | ❌ 需要密码，`/etc/sudoers.d/` 里没有 NOPASSWD 规则 |
| 宿主侧 | Parallels `bridge100 = 10.211.55.2`；`.5` 有 ARP 表项（首次 `No route to host` 是 ARP 未解析的假象） |

## 1. ⚠️ 决定性约束：这台机器拿不到 Plasma 6

方案的核心验证点在 **Plasma 6**（Wayland layer-shell v5、`ext_background_effect_manager_v1`、`org.freedesktop.impl.portal.Wallpaper` 6.5.0），但这台机器三条路都走不通：

| 路径 | 结论 | 依据 |
| --- | --- | --- |
| Ubuntu 24.04 官方仓库 | **只有 Plasma 5.27.12** | `apt-cache policy`：`plasma-desktop 4:5.27.12-0ubuntu0.1`、`plasma-workspace 4:5.27.12-0ubuntu0.1`、`kwin-x11/kwin-wayland 4:5.27.11-0ubuntu3` |
| Kubuntu Backports PPA（noble） | **对 arm64 是空的**（amd64 同样为空） | `dists/noble/main/binary-arm64/Packages.gz` 实测 **20 字节**（`gzip, original size 0`）= 零包；索引存在但无内容 |
| KDE neon | **官方只有 amd64 ISO**，arm64 无用户可用镜像 | neon 的 arm64 只出现在 build.neon.kde.org 的构建任务里，没有发布给用户的 arm64 镜像 |

所以在这台 VM 上，**能装的最高版本是 Plasma 5.27.12**。这不是"装完再说"的问题，它直接决定验证覆盖率。

### 覆盖率对照

| 方案要求 | Plasma 5.27 (这台 VM 可做) | 需要 Plasma 6 (这台 VM 做不到) |
| --- | --- | --- |
| GTK `_NET_WM_WINDOW_TYPE_DESKTOP` 在 KWin/X11 下的落位与可点击穿透 | ✅ 可验证 | — |
| plasmashell 桌面窗口自绘背景 → 图标被盖（§2.1 的现状描述） | ✅ 可验证（`plasmashell --version` 5.27） | — |
| 机制① 透明化 plasmashell 桌面背景（`evaluateScript` 拿 containment root） | ⚠️ 部分可验证（5.27 与 6.x 的 PlasmaScript 不同，**结论不能外推到 6**） | — |
| 机制② 关闭桌面容器 | ✅ 可验证 | — |
| `Layer=Desktop` KWin 窗口规则（规则能覆盖窗口层级） | ✅ 可验证 | — |
| `AcceptFocus=false` / `SkipTaskbar` / `Desktops` / `Activity` 规则字段 | ✅ 可验证 | — |
| **`zwlr_layer_shell_v1` Background 层（P1 的 Wayland 进阶）** | ❌ 5.27 的 KWin 有 layer-shell，但**方案针对的是 Plasma 6 的 v5 + 无白名单**，且 Wayland 会话入口本身要看 kwin-wayland 版本 | ⬅ 核心缺口 |
| **`ext_background_effect_manager_v1`（模糊新协议）** | ❌ 不存在 | ⬅ 缺口 |
| **`org.freedesktop.impl.portal.Wallpaper`（6.5.0 起）** | ❌ 不存在 | ⬅ 缺口 |
| **Plasma 6 无「每虚拟桌面不同壁纸」** | ❌ 该结论本身就是 6.x 的 | ⬅ 缺口 |

**结论：这台 VM 装 Plasma 5.27 值得做**（能验证 X11 侧的机制①②、KWin 规则字段、托盘/快捷键/自启动这些与版本无关的集成点），**但它不是"验证等离子 6 方案"的机器**。要验证方案主线（Wayland + Plasma 6），得换基础系统——Ubuntu 25.04（plucky）的 `plasma-desktop` 是 **6.3.4** 且有 arm64，是最省事的候选；或直接换 Fedora KDE / openSUSE（都出 arm64 KDE 镜像）。

## 2. 前置：配置 sudo 免密（一次性，需要你操作）

`parallels` 是 UID 1000 且属于 `sudo` 组，但当前需要密码。在 **VM 里**（Parallels 窗口或现有 SSH 会话）执行一次，输入你的密码：

```bash
echo "parallels ALL=(ALL) NOPASSWD:ALL" | sudo tee /etc/sudoers.d/99-parallels >/dev/null
sudo chmod 0440 /etc/sudoers.d/99-parallels
sudo -n true && echo "NOPASSWD OK"
```

配好后宿主侧验证：

```bash
ssh parallels@10.211.55.5 'sudo -n true && echo OK_NOPASSWD'
```

收尾时若要撤销：`sudo rm /etc/sudoers.d/99-parallels`。

## 3. 安装步骤（sudo 配好后，由宿主 SSH 无人值守执行）

### 3.1 预检

```bash
ssh parallels@10.211.55.5 'bash -s' < scripts/kde-verify-preflight.sh
```

### 3.2 apt 快照与基础包

```bash
ssh parallels@10.211.55.5 'sudo -n bash -c "
  apt-get update -y
  # 记录装前状态，便于回滚/对比
  dpkg --get-selections > /root/pre-kde-selections.txt
  apt-get install -y --no-install-recommends xvfb dbus-x11
"'
```

### 3.3 安装 kubuntu-desktop

**必须先非交互**，否则 `kubuntu-desktop` 会弹 display manager 选择（gdm3 vs sddm）把无人值守安装挂死：

```bash
ssh parallels@10.211.55.5 'sudo -n bash -c "
  export DEBIAN_FRONTEND=noninteractive
  # 默认保留现有 gdm3 作为 DM，避免抢掉 GNOME 会话；显式声明以免弹窗
  echo \"gdm3 shared/default-x-display-manager select gdm3\" | debconf-set-selections
  echo \"sddm shared/default-x-display-manager select gdm3\"   | debconf-set-selections
  apt-get install -y kubuntu-desktop
"'
```

预期下载 1.5–2 GB、安装后占 3–4 GB。2 vCPU/3.8 GiB 下约 15–40 分钟。

### 3.4 解锁 wayland 会话与嵌套合成器

Plasma 5.27 的 Wayland 会话需显式装且在某些镜像上被屏蔽：

```bash
ssh parallels@10.211.55.5 'sudo -n bash -c "
  apt-get install -y plasma-workspace-wayland kwin-wayland
  # 确认会话入口
  ls -l /usr/share/wayland-sessions/plasmawayland.desktop /usr/share/xsessions/plasmax11.desktop
  # 5.27 的嵌套模式：kwin_wayland --virtual（pixel backend）
  kwin_wayland --help 2>&1 | grep -n virtual || echo \"NO --virtual\"
"'
```

### 3.5 关键验证点（装完立刻做）

1. `kwin_wayland --virtual` 是否可用 → 决定 §6.2 的 nested Wayland CI 档能不能落在这台机器上。
2. `/usr/share/{wayland-sessions,xsessions}/*.desktop` 是否存在 → 决定能否真机登录 KDE 会话（否则只能嵌套）。
3. `evaluateScript` 能否拿到 desktop containment 的 `wallpaper` 对象（机制①，**5.27 结论只作参考、不能外推到 6**）：

```bash
qdbus org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript \
  'print(desktops().length); var d = desktops()[0]; print(typeof d.wallpaper); print(d.wallpaper ? d.wallpaper.visible : "NO_WALLPAPER_OBJ");'
```

## 4. 已知风险 / 待观察

- **磁盘**：`kubuntu-desktop` 与 GNOME 并存，41 GB 可用空间够，但不宽裕；如需 Plasma 6 换发行版则要重装 VM。
- **内存**：3.8 GiB 里只有约 2.0 GiB 可用。同时跑 plasmashell + WallpaperEM（WebKitGTK + 视频解码）会紧张，冒烟时建议只起一个会话、关掉 GNOME 会话。
- **DM 冲突**：保留 gdm3 可以同时保留 GNOME 会话入口；若后续要用 SDDM，需再改 `debconf` 并重配。
- **Wayland 真机验证**：当前是 SSH/tty，没有图形会话。要么在 Parallels 窗口里登录一次 KDE 会话，要么全走 `kwin_wayland --virtual` 嵌套。
- **不要**在这台机器上验证 Plasma 6 专属结论（见 §1 覆盖率表），否则会把 5.27 的行为误当成 6 的。
