# WallpaperEM

**WallpaperEM** 是一款开源的 macOS 动态壁纸引擎 —— 浏览并下载 Steam 创意工坊（Wallpaper Engine）壁纸，并把它们应用到桌面。视频 / GIF / 网页 / 场景（WebGL）/ 图片壁纸都支持，多显示器，带托盘与全局快捷键。

**WallpaperEM** is an open-source dynamic wallpaper engine for macOS — browse and download Wallpaper Engine workshop wallpapers, then apply them to your desktop. It supports video / GIF / web / scene (WebGL) / image wallpapers across multiple displays, with a tray icon and global shortcuts.

---

<p align="center">
  <img src="public/icon/icon_512x512.png" width="120" alt="WallpaperEM logo"/>
</p>

---

## ✨ 功能特性 / Features

- 🖼️ **多类型壁纸**：视频（mp4/webm/mov）、GIF、网页（HTML）、场景（`we-scene` WebGL 渲染器）、静态图片。
- 🌐 **Steam 创意工坊**：搜索 / 排序（趋势 / 最多订阅 / 最多收藏 / 最新）/ 类型筛选 / 分页浏览。
- ⬇️ **下载**：通过 DepotDownloader sidecar 下载工坊内容，支持 Steam Guard 验证码、串行队列、断点/重试，账号密码**本地加密存储**（不依赖系统钥匙串授权）。
- 🗂️ **本地库**：管理已下载壁纸（预览 / 应用到桌面 / 打开目录 / 删除），收藏。
- 🖥️ **多显示器**：每屏一个桌面级窗口，置底到桌面图标之下（可切换「交互模式」让壁纸在图标之上并接收鼠标）。
- 🎨 **内置默认壁纸**：未下载任何壁纸时展示精美观感的内置 HTML 壁纸。
- 🎞️ **轮播播放列表**：定时在本地库壁纸间切换。
- 🔔 **托盘 + 全局快捷键**：⌘⇧P 暂停/恢复、⌘⇧N 下一张（轮播）。
- ⚙️ **设置**：开机自启、下载账号、代理、壁纸显示模式（填充 / 适应 / 拉伸 / 平铺）、壁纸交互开关。
- 🧱 **macOS 原生**：透明无边框桌面级窗口、桌面层合成、系统感知。

- 🖼️ **Multiple wallpaper types**: video (mp4/webm/mov), GIF, web (HTML), scene (`we-scene` WebGL renderer), and static images.
- 🌐 **Steam Workshop**: search, sort (Trend / Most Subscribed / Most Favorited / Newest), type filter, paginated browsing.
- ⬇️ **Download**: fetch workshop content via a DepotDownloader sidecar, with Steam Guard support, a serial queue, retry, and **locally-encrypted** account credentials (no dependency on the macOS Keychain auth prompt).
- 🗂️ **Local library**: manage downloaded wallpapers (preview / apply to desktop / open folder / delete), favorites.
- 🖥️ **Multi-display**: one desktop-level window per screen, placed below the desktop icons (with an optional "interactive" mode that sits above the icons and accepts mouse).
- 🎨 **Built-in default wallpaper**: a polished built-in HTML wallpaper when nothing is downloaded yet.
- 🎞️ **Playlist / rotation**: rotate between local-library wallpapers on a timer.
- 🔔 **Tray + global shortcuts**: ⌘⇧P pause/resume, ⌘⇧N next (rotation).
- ⚙️ **Settings**: launch at login, download account, proxy, wallpaper display mode (fill / fit / stretch / tile), wallpaper-interactivity toggle.
- 🧱 **macOS native**: transparent borderless desktop-level windows, desktop-level compositing, display-aware.

---

## 🛠️ 技术栈 / Tech Stack

| 层 | 技术 |
| --- | --- |
| 前端 | React 19 · TypeScript · Vite 6 · Tailwind CSS 4 |
| 桌面壳 | Tauri 2.11 · Rust |
| 渲染器 | 自研 WKWebView 渲染页 + `we-scene`（WebGL 场景渲染，MIT） |
| 存储 | SQLite（rusqlite）· 本地加密凭据 |
| 下载 | DepotDownloader sidecar（串行队列 + Steam Guard） |
| 系统集成 | macOS 桌面层窗口 · LaunchAgent 自启 · 托盘 · 全局快捷键 |

| Layer | Tech |
| --- | --- |
| Frontend | React 19 · TypeScript · Vite 6 · Tailwind CSS 4 |
| Shell | Tauri 2.11 · Rust |
| Renderer | Custom WKWebView page + `we-scene` (WebGL scene renderer, MIT) |
| Storage | SQLite (rusqlite) · locally-encrypted credentials |
| Download | DepotDownloader sidecar (serial queue + Steam Guard) |
| System | macOS desktop-level windows · LaunchAgent autostart · tray · global shortcuts |

---

## 🚀 从源码构建 / Build from Source

### 环境要求 / Prerequisites

- **macOS 13+**（Apple Silicon；Tauri 需要 macOS）
- **Node.js + pnpm**
- **Rust toolchain**（rustup）
- **Xcode Command Line Tools**

> **关于 DepotDownloader sidecar / About the DepotDownloader sidecar**
> 下载工坊内容依赖 `src-tauri/binaries/depot-downloader-aarch64-apple-darwin`（约 85 MB 的编译产物）。该二进制已用 **Git LFS** 纳入版本库，拉取仓库即可用；若 LFS 不可用，请自行编译该 sidecar 放到对应路径。缺少它时其余功能仍可构建运行，仅「下载」不可用。
> Downloading workshop content needs `src-tauri/binaries/depot-downloader-aarch64-apple-darwin` (~85 MB). It is committed via **Git LFS**, so cloning the repo pulls it automatically; if LFS is unavailable, build the sidecar yourself into that path. Without it the app still builds and runs — only "Download" is unavailable.

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

### 打包 / Build a release (.app / .dmg)

```bash
pnpm tauri build
```

> 产物在 `src-tauri/target/release/bundle/macos/WallpaperEM.app`（或 `.dmg`）。
> The bundle is produced at `src-tauri/target/release/bundle/macos/WallpaperEM.app` (or `.dmg`).

---

## 📖 使用说明 / Usage

1. **登录/配置下载账号**：打开 设置 → 下载，填入你的 Steam 账号与密码（需要拥有 Wallpaper Engine）。密码本地加密存储，不会要求 macOS 钥匙串授权。
   *(Set your Steam account + password under Settings → Download. You must own Wallpaper Engine. Credentials are stored locally-encrypted.)*
2. **浏览工坊**：在「工坊」页搜索、筛选、排序，看到喜欢的点「下载」。
3. **应用到桌面**：壁纸入库后，在「本地库 / 详情」页点「应用到桌面」；也可在「发现」页快速应用。
4. **开机自启 / 托盘 / 轮播**：在设置里可选，托盘与 ⌘⇧P / ⌘⇧N 快速控制。
5. **壁纸交互（可选）**：设置 → 通用 →「壁纸交互（图标上方）」开启后，场景视差与网页壁纸可接收鼠标；注意这会盖住桌面图标。

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
│  │  ├─ download/             # 下载引擎：DepotDownloader + 队列 + Guard
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
- **地区/网络**：国内访问 Steam 建议在 设置 → 下载 配置代理。
  *(For restricted networks, configure a proxy under Settings → Download.)*

---

## 📄 许可 / License

本项目目前以 **MIT** 许可证开源（请以仓库实际 LICENSE 文件为准）。

This project is open-sourced under the **MIT** license (see the actual `LICENSE` file in the repository).

---

## 🙏 致谢 / Acknowledgements

- [Tauri](https://tauri.app/) · [React](https://react.dev/) · [we-scene](https://github.com/)（MIT 场景渲染器）
- [Steam Workshop](https://steamcommunity.com/workshop/) · Wallpaper Engine 及其作者
- [DepotDownloader](https://github.com/SteamRE/DepotDownloader)

---

*用 ❤️ 和 Rust 构建 — Built with ❤️ and Rust.*
