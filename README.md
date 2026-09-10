# WallpaperEM

**WallpaperEM** 是一款开源的 macOS / Linux 动态壁纸引擎 —— 浏览并下载 Steam 创意工坊（Wallpaper Engine）壁纸，并把它们应用到桌面。视频 / GIF / 网页 / 场景（WebGL）/ 图片壁纸都支持，多显示器，带托盘与全局快捷键。

**WallpaperEM** is an open-source dynamic wallpaper engine for macOS and Linux — browse and download Wallpaper Engine workshop wallpapers, then apply them to your desktop. It supports video / GIF / web / scene (WebGL) / image wallpapers across multiple displays, with a tray icon and global shortcuts.

---

<p align="center">
  <img src="public/icon/icon_512x512.png" width="120" alt="WallpaperEM logo"/>
</p>

---

## ✨ 功能特性 / Features

- 🖼️ **多类型壁纸**：视频（mp4/webm/mov）、GIF、网页（HTML）、场景（`webwallgl` WebGL 渲染器）、静态图片。
- 🌐 **Steam 创意工坊**：搜索 / 排序（趋势 / 最多订阅 / 最多收藏 / 最新）/ 类型筛选 / 分页浏览。
- ⬇️ **下载**：通过 Valve 官方 **steamcmd** 下载工坊内容（首次使用时自动安装）；支持 Steam Guard 验证码、串行队列、失败重试，账号密码**本地加密存储**（不依赖系统钥匙串授权）。
- 🗂️ **本地库**：管理已下载壁纸（预览 / 应用到桌面 / 打开目录 / 删除），收藏。
- 🖥️ **多显示器**：每屏一个桌面级窗口，置底到桌面图标之下（可切换「交互模式」让壁纸在图标之上并接收鼠标）。
- 🎨 **内置默认壁纸**：未下载任何壁纸时展示精美观感的内置 HTML 壁纸。
- 🎞️ **轮播播放列表**：定时在本地库壁纸间切换。
- 🔔 **托盘 + 全局快捷键**：⌘⇧P 暂停/恢复、⌘⇧N 下一张（轮播）。
- ⚙️ **设置**：开机自启、下载账号、代理、壁纸显示模式（填充 / 适应 / 拉伸 / 平铺）、壁纸交互开关。
- 🧱 **macOS 原生**：透明无边框桌面级窗口、桌面层合成、系统感知。
- 🐧 **Linux 支持**：X11 桌面层窗口、MPRIS 正在播放、按桌面环境同步静态壁纸（GNOME/KDE/Cinnamon/MATE/XFCE/swww/feh）。

- 🖼️ **Multiple wallpaper types**: video (mp4/webm/mov), GIF, web (HTML), scene (`webwallgl` WebGL renderer), and static images.
- 🌐 **Steam Workshop**: search, sort (Trend / Most Subscribed / Most Favorited / Newest), type filter, paginated browsing.
- ⬇️ **Download**: workshop content is fetched with Valve's official **steamcmd** (installed on first use), with Steam Guard support, a serial queue, retry, and **locally-encrypted** account credentials (no dependency on the macOS Keychain auth prompt).
- 🗂️ **Local library**: manage downloaded wallpapers (preview / apply to desktop / open folder / delete), favorites.
- 🖥️ **Multi-display**: one desktop-level window per screen, placed below the desktop icons (with an optional "interactive" mode that sits above the icons and accepts mouse).
- 🎨 **Built-in default wallpaper**: a polished built-in HTML wallpaper when nothing is downloaded yet.
- 🎞️ **Playlist / rotation**: rotate between local-library wallpapers on a timer.
- 🔔 **Tray + global shortcuts**: ⌘⇧P pause/resume, ⌘⇧N next (rotation).
- ⚙️ **Settings**: launch at login, download account, proxy, wallpaper display mode (fill / fit / stretch / tile), wallpaper-interactivity toggle.
- 🧱 **macOS native**: transparent borderless desktop-level windows, desktop-level compositing, display-aware.
- 🐧 **Linux support**: X11 desktop-level windows, MPRIS Now Playing, per-DE static wallpaper sync (GNOME/KDE/Cinnamon/MATE/XFCE/swww/feh).

---

## 🛠️ 技术栈 / Tech Stack

| 层 | 技术 |
| --- | --- |
| 前端 | React 19 · TypeScript · Vite 6 · Tailwind CSS 4 |
| 桌面壳 | Tauri 2.11 · Rust |
| 渲染器 | WKWebView 渲染页 + [`webwallgl`](https://www.npmjs.com/package/webwallgl)（WebGL 场景渲染，MIT，npm 依赖） |
| 存储 | SQLite（rusqlite）· 本地加密凭据 |
| 下载 | steamcmd（Valve 官方，运行时安装）· 串行队列 + Steam Guard |
| 系统集成 | macOS 桌面层窗口 · X11 桌面层窗口（Linux）· 自启 · 托盘 · 全局快捷键 · MPRIS（Linux 正在播放） |

| Layer | Tech |
| --- | --- |
| Frontend | React 19 · TypeScript · Vite 6 · Tailwind CSS 4 |
| Shell | Tauri 2.11 · Rust |
| Renderer | WKWebView page + [`webwallgl`](https://www.npmjs.com/package/webwallgl) (WebGL scene renderer, MIT, npm dependency) |
| Storage | SQLite (rusqlite) · locally-encrypted credentials |
| Download | steamcmd (official Valve, installed at runtime) · serial queue + Steam Guard |
| System | macOS desktop-level windows · X11 desktop-level windows (Linux) · autostart · tray · global shortcuts · MPRIS (Linux Now Playing) |

---

## 🚀 从源码构建 / Build from Source

### 环境要求 / Prerequisites

**macOS**：

- **macOS 13+**（Apple Silicon）
- **Node.js + pnpm**
- **Rust toolchain**（rustup）
- **Xcode Command Line Tools**

**Linux**（主路径：X11 会话 + GNOME/KDE/Cinnamon/MATE/XFCE）：

- 桌面 Linux（x86_64，glibc 发行版）
- **Node.js + pnpm**、**Rust toolchain**（rustup）
- Tauri 系统依赖（Debian/Ubuntu 示例）：

  ```bash
  sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file     libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev libdbus-1-dev
  ```

- 可选：**ffmpeg**（系统壁纸同步抽帧用）；**32 位 multilib**（steamcmd 官方 Linux
  引导程序是 32 位，下载功能需要：`sudo dpkg --add-architecture i386 && sudo apt install libc6:i386 libstdc++6:i386`，应用内安装 steamcmd 时会自动检测并提示）

> **关于 steamcmd / About steamcmd**
> 下载工坊内容使用 Valve 官方 steamcmd，首次在「设置 → 账号」点「安装」即可（从官方源下载约 2.5 MB 引导包，初始化后约 85 MB，装在应用数据目录，不随包分发）。
> 注意其官方引导程序是 x86_64 版本，Apple Silicon 上**首次启动需要 Rosetta 2**（`softwareupdate --install-rosetta --agree-to-license`）；首次自更新后即以原生 arm64 运行。
>
> Workshop downloads use Valve's official steamcmd. Install it once from Settings → Download (a ~2.5 MB bootstrap from the official source, ~85 MB after init, kept in the app data directory — not bundled).
> Its official bootstrap binary is x86_64, so the **first launch on Apple Silicon needs Rosetta 2** (`softwareupdate --install-rosetta --agree-to-license`); it self-updates to a native arm64 build immediately after.

### 安装依赖 / Install dependencies

```bash
git clone https://github.com/oneincase/WallpaperEM.git
cd WallpaperEM
pnpm install
```

### 开发运行 / Run in development

```bash
pnpm tauri dev
```

### 打包 / Build a release

```bash
pnpm tauri build
```

> macOS 产物在 `src-tauri/target/release/bundle/macos/WallpaperEM.app`（或 `.dmg`）；
> Linux 产物在 `src-tauri/target/release/bundle/deb/*.deb` 与 `bundle/appimage/*.AppImage`。
> On macOS the bundle is produced at `src-tauri/target/release/bundle/macos/WallpaperEM.app` (or `.dmg`);
> on Linux at `src-tauri/target/release/bundle/deb/*.deb` and `bundle/appimage/*.AppImage`.

### 🐧 Linux 平台说明 / Linux notes

- **会话类型**：主路径是 **X11**（任意 DE/WM）。Wayland 下壁纸窗口退化为「置底 +
  全工作区」的普通无边框窗口（wlroots 系后续版本再接 layer-shell）；
  **GNOME Wayland 不支持桌面层窗口**（上游协议限制），建议使用 X11 会话。
- **功能差异**（相对 macOS）：系统音频捕获（音频可视化）暂未支持（待接入 PipeWire）；
  「自动暂停」（前台应用切换时暂停壁纸）暂无实现；指针按压跟随效果不可用
  （Wayland 下指针注入整体不可用，协议限制）。
- **托盘**：GNOME 需要 AppIndicator 扩展（如 `gnome-shell-extension-appindicator`）；
  KDE/Cinnamon/XFCE 自带托盘协议支持。
- **视频壁纸编解码（必装）**：WebKitGTK 的音视频播放完全依赖系统 GStreamer 插件，
  缺失时视频壁纸**黑屏/无声**（日志里会出现 `GStreamer element appsink/autoaudiosink
  not found`）。deb 包已把这些写入 Depends 会自动装好；AppImage 无法捆绑系统插件，
  需手动安装：

  ```bash
  # Debian / Ubuntu
  sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
      gstreamer1.0-plugins-bad gstreamer1.0-libav
  # Fedora
  sudo dnf install gstreamer1-plugins-base gstreamer1-plugins-good \
      gstreamer1-plugins-bad-free gstreamer1-libav
  # Arch
  sudo pacman -S gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav
  ```

  应用启动时会自动体检关键 element（appsink / autoaudiosink / H.264 解码器），
  缺什么会把对应的安装命令写进日志。webm/VP9 开箱即用。

---

## 📖 使用说明 / Usage

1. **安装下载工具**：打开 设置 → 账号，点「安装」一次性装好 steamcmd。
   *(Install steamcmd once from Settings → Account.)*
2. **登录下载账号**：填入你的 Steam 账号与密码（需要拥有 Wallpaper Engine）。密码本地加密存储，不会要求 macOS 钥匙串授权。
   *(Set your Steam account + password. You must own Wallpaper Engine. Credentials are stored locally-encrypted.)*
3. **浏览工坊**：在「工坊」页搜索、筛选、排序，看到喜欢的点「下载」。
4. **应用到桌面**：壁纸入库后，在「本地库 / 详情」页点「应用到桌面」；也可在「发现」页快速应用。
5. **开机自启 / 托盘 / 轮播**：在设置里可选，托盘与 ⌘⇧P / ⌘⇧N 快速控制。
6. **壁纸交互（可选）**：设置 → 通用 →「壁纸交互（图标上方）」开启后，场景视差与网页壁纸可接收鼠标；注意这会盖住桌面图标。

---

## 📁 项目结构 / Project Structure

```
WallpaperEM/
├─ public/
│  ├─ icon/                    # 应用图标（抠图、透明）
│  └─ default-wallpaper/       # 内置默认 HTML 壁纸
├─ renderer/                   # 壁纸渲染器页（视频/GIF/网页/场景/图片）
├─ src/                        # Tauri 前端主界面（发现/工坊/下载/本地库/收藏/设置）
├─ src-tauri/
│  ├─ src/
│  │  ├─ steam/                # Steam 客户端：工坊浏览/详情/类型
│  │  ├─ workshop.rs           # 工坊搜索/随机/详情（含缓存）
│  │  ├─ download/             # 下载引擎：steamcmd + 队列 + Guard
│  │  │  ├─ backend.rs         #   steamcmd 命令行拼装与输出解析
│  │  │  ├─ pty.rs             #   伪终端（steamcmd 的 stdin 必须是 TTY）
│  │  │  └─ steamcmd_install.rs#   steamcmd 运行时安装（下载/解压/预热）
│  │  ├─ wallpaper/            # 壁纸引擎：多屏桌面窗口 + 会话持久化 + 轮播
│  │  ├─ content_server.rs     # 本地内容服务器（渲染器/媒体/默认壁纸同源）
│  │  ├─ library.rs            # 本地库
│  │  ├─ db.rs                 # SQLite 初始化/迁移
│  │  └─ secure_store.rs       # 本地加密凭据
│  ├─ icons/                   # 应用图标（icns/png）
│  └─ tauri.conf.json
└─ package.json
```

---

## ⚠️ 注意事项 / Notes

- **Wallpaper Engine 授权**：下载工坊内容需要你的 Steam 账号拥有《Wallpaper Engine》。
  *(Downloading workshop content requires owning *Wallpaper Engine* on your Steam account.)*
- **macOS 局限**：壁纸窗口默认位于桌面图标之下（图标可点击）。开启「壁纸交互」会把壁纸提到图标之上以接收鼠标，但会盖住图标；受 macOS 桌面窗口机制限制，交互体验有限。
  *(macOS limitation: wallpapers sit below the desktop icons by default. The optional "interactive" mode raises the wallpaper above the icons to receive mouse input, at the cost of covering them. Interaction is limited by macOS's desktop-window handling.)*
- **地区/网络**：国内访问 Steam 建议在 设置 → 网络 配置代理。
  *(For restricted networks, configure a proxy under Settings → Network.)*
- **steamcmd 会挤掉 Steam 客户端**：同一账号在 Steam 图形客户端与 steamcmd 中同时只能登录一处，下载时会断开你正在运行的 Steam 客户端（这是 steamcmd 的固有行为，无法规避）。
  *(A Steam account can only be logged in once at a time across the GUI client and steamcmd — downloading will disconnect your running Steam client. This is inherent to steamcmd.)*
- **steamcmd 无下载百分比**：Valve 的 `workshop_download_item` 在下载期间不输出进度，应用改用「已下载体积 ÷ 条目总大小」估算；总大小拿不到时进度条显示为不确定态。
  *(steamcmd emits no progress during `workshop_download_item`; the app estimates it from on-disk size versus the item's reported size, falling back to an indeterminate bar.)*

---

## 📄 许可 / License

本项目目前以 **MIT** 许可证开源（请以仓库实际 LICENSE 文件为准）。

This project is open-sourced under the **MIT** license (see the actual `LICENSE` file in the repository).

本项目内含第三方组件 `src-tauri/vendor/mediaremote-adapter`
（[mediaremote-adapter](https://github.com/ungive/mediaremote-adapter)，BSD-3-Clause，
Copyright (c) 2025 Jonas van den Berg and contributors），用于读取系统「正在播放」信息；
完整许可见该目录下的 `LICENSE`。

This project bundles the third-party component `src-tauri/vendor/mediaremote-adapter`
([mediaremote-adapter](https://github.com/ungive/mediaremote-adapter), BSD-3-Clause)
for reading system Now Playing information; see the `LICENSE` file in that directory.

---

## 🙏 致谢 / Acknowledgements

- [Tauri](https://tauri.app/) · [React](https://react.dev/) · [webwallgl](https://github.com/oneincase/webwallgl)（MIT 场景渲染器）
- [Steam Workshop](https://steamcommunity.com/workshop/) · Wallpaper Engine 及其作者
- [SteamCMD](https://developer.valvesoftware.com/wiki/SteamCMD)（Valve）
- [mediaremote-adapter](https://github.com/ungive/mediaremote-adapter)（BSD-3-Clause，系统「正在播放」数据源）

---

*用 ❤️ 和 Rust 构建 — Built with ❤️ and Rust.*
