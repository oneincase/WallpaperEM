# 跨平台适配调研：Linux / 鸿蒙 PC

> 调研日期：2026-09-10，基于 v0.4.0（Tauri 2.11.2 + React 19 + Rust）。
> 平台策略：**macOS（现状）→ Linux（近期目标）→ 鸿蒙 PC（长期观察）**。
> Windows 不做 —— Wallpaper Engine 官方客户端已覆盖，没有差异化价值；
> Linux 与鸿蒙 PC 上都没有成熟的 WE 兼容动态壁纸方案，是真正的空白市场。
>
> 结论先行：
> - **Linux**：主要成本不在代码量（macOS 专属代码仅约 2600 行），而在桌面环境
>   碎片化。收窄支持范围到 **X11 会话 + GNOME/KDE**，约 2~3 周可落地。
> - **鸿蒙 PC**：存在三个**硬性卡点**（无桌面壁纸开放能力、Tauri 无鸿蒙后端、
>   steamcmd 无 ARM/OHOS 版本），现阶段不具备可行性，建议作为长期观察项，
>   仅以最小成本做技术验证跟踪，不投入正式开发。

---

## 1. 现状盘点：哪些已经是跨平台的

约 12.6k 行 Rust 中，**核心价值的 70% 与平台无关**，无需改动即可在 Linux 编译运行：

| 模块 | 行数 | 跨平台就绪度 |
| --- | --- | --- |
| 前端全部（React 29 个文件、renderer、webwallgl 场景渲染） | — | ✅ 纯 Web 技术（鸿蒙可走 ArkWeb 复用，见 §4） |
| `steam/` 工坊浏览 / `workshop.rs` | ~700 | ✅ reqwest(rustls) 纯 HTTP |
| `content_server.rs` 本地内容服务器 | 1437 | ✅ axum，含 SSE 推送通道 |
| `library.rs` / `db.rs`（rusqlite bundled） | ~1500 | ✅ |
| `secure_store.rs` 本地加密凭据（已是 Keychain 替代方案） | 135 | ✅ 纯 sha2/rand |
| `we_props.rs` / `we_shim.js` WE 属性桥 | ~1270 | ✅ |
| `download/pty.rs` POSIX 伪终端 | 262 | ✅ Linux 直接复用（steamcmd 同样要求 TTY） |
| 托盘 / 全局快捷键（tauri 插件） | — | ⚠️ Linux 需能力降级，见 §3.2 |
| `tauri-plugin-autostart` | — | ✅ Linux = XDG autostart .desktop |
| `tauri-plugin-desktop-underlay` 桌面层窗口 | — | ⚠️ 官方标注 **Linux 未测试**，需验证或自研 |

## 2. macOS 专属代码清单（适配工作量来源）

| # | 模块 | 行数 | macOS 依赖 | Linux 替代方案 |
| --- | --- | --- | --- | --- |
| 1 | `wallpaper/macos.rs` 屏幕枚举/睡眠检测/桌面层级 | 257 | CoreGraphics C API + underlay 插件 | X11：x11rb 自研桌面层窗口（见 §3.1）；屏幕枚举换 tauri `Monitor` API（跨平台）；睡眠检测可降级或读 logind |
| 2 | `wallpaper/pointer.rs` 指针注入 | 161 | CGEventSource 只读光标 API（免权限） | X11 `XQueryPointer`（免权限）；**Wayland 无全局光标协议 → 功能降级** |
| 3 | `system_wallpaper.rs` 静态壁纸同步 + 视频抽帧 | 244 | NSWorkspace + AVAssetImageGenerator | 按 DE 分发：GNOME `gsettings` / KDE `qdbus org.kde.plasmashell` / 通用 `feh`；抽帧用 gstreamer 或 WebView 截图 |
| 4 | `audio_capture.rs` 系统音频 loopback → FFT | 666 | ScreenCaptureKit（macOS 13+） | PulseAudio/PipeWire monitor source（`libpulse-binding`/`pipewire` crate，现代发行版免权限） |
| 5 | `now_playing.rs` 正在播放 | 688 | 私有框架 MediaRemote + 借 /usr/bin/perl 绕过 | **MPRIS over D-Bus**（`zbus` 或 `mpris` crate，freedesktop 标准协议，比 macOS 方案干净得多） |
| 6 | `download/steamcmd_install.rs` | 388 | `steamcmd_osx.tar.gz` + Rosetta 检测 + bsdtar | 换 `steamcmd_linux.tar.gz`；注意官方 Linux 版仍为 32 位 x86，需检测 multilib 并引导安装 |
| 7 | `keychain.rs` | 44 | keyring `apple-native` | 加 `linux-native` feature（secret-service，需 gnome-keyring/KWallet）；secure_store 已是首选，此项仅回退 |
| 8 | `lib.rs`/`main_window.rs` vibrancy 侧栏材质 | ~30 | NSVisualEffectView Sidebar | window-vibrancy `apply_blur`（仅 KWin 等部分合成器有效），降级为纯色/半透明 |
| 9 | `commands.rs` KeepAlive 注入 | ~40 | LaunchAgent plist 手改 | 不需要（无此机制），cfg 掉即可 |
| 10 | tauri.conf `titleBarStyle: Overlay` + `hiddenTitle` + 前端 h-10 拖拽区 | — | macOS 红绿灯布局 | Linux 下这两个键被忽略；需补自定义窗口按钮（GTK CSD 风格）或退回原生标题栏 |
| 11 | bundle 配置 `targets: ["app","dmg"]` + entitlements + mediaremote-adapter resource | — | macOS 签名 | 加 `deb`/`appimage`（+可选 `rpm`）；mediaremote-adapter 按 target 裁剪；托盘运行库依赖写入安装说明 |

另：`macOSPrivateApi: true`（透明窗口用）在非 macOS 目标上是 no-op，不阻塞编译。

## 3. Linux 适配方案

### 3.1 根本难点：桌面环境碎片化

macOS 的「桌面层窗口」有唯一官方路径，Linux 没有 —— 这是整个适配中最核心、风险最高的一项：

| 环境 | 桌面层窗口方案 | 可行性 |
| --- | --- | --- |
| X11（任意 WM/DE） | `_NET_WM_WINDOW_TYPE_DESKTOP` 或 override_redirect + lower，x11rb 实现 <100 行 | ✅ **通用，推荐主路径** |
| Wayland - wlroots 系（Sway/Hyprland） | wlr-layer-shell（background 层），有现成 crate | ✅ 可做，优先级低 |
| Wayland - KDE Plasma | layer-shell 支持良好 | ✅ 可做，优先级低 |
| Wayland - GNOME | **无 layer-shell 协议，GNOME 官方明确拒绝桌面层窗口**，只能走 Shell 扩展 | ❌ 不支持，写进 README |

其他能力差异：

| 能力 | X11 | Wayland | 降级策略 |
| --- | --- | --- | --- |
| 指针注入（视差/hover） | ✅ XQueryPointer | ❌ 无全局光标协议 | Wayland 下关闭，前端隐藏开关 |
| 全局快捷键 | ✅ XRecord/XGrabKey | ⚠️ 需 XDG GlobalShortcuts portal（GNOME 46+/KDE 支持） | 探测失败则关闭，托盘仍可控 |
| 系统托盘 | ⚠️ 需 libappindicator；GNOME 默认无托盘，要 AppIndicator 扩展 | 同左 | 无托盘时退化为普通窗口驻留 |
| 静态壁纸同步 | GNOME gsettings / KDE qdbus / feh | 同左（按 DE 不按协议） | 未识别 DE 时关闭该功能 |
| 视频壁纸解码 | **WebKitGTK 的 h264 依赖系统 gstreamer 插件，部分发行版默认缺失 → 黑屏** | 同左 | 启动时探测 gst 插件，缺失则引导安装；`.mov` 直接标记不支持 |

### 3.2 建议的支持矩阵

- **承诺支持**：X11 会话 + GNOME / KDE（覆盖绝大多数桌面 Linux 用户）。
- **尽力而为**：wlroots 系 Wayland（layer-shell 路径）。
- **明确不支持**：GNOME Wayland（无桌面层协议）、`.mov`/HEVC 视频壁纸（WebKitGTK 限制，靠库内 type 过滤，不做转码管线）。
- **能力探测**：启动时读 `XDG_SESSION_TYPE` / `XDG_CURRENT_DESKTOP` + 探测 gstreamer 插件 / D-Bus MPRIS / secret-service，生成能力集暴露给前端，设置页据此动态隐藏不可用项 —— 前端永远面向「能力」而非「平台」。

### 3.3 关键 Spike（建议先花 2~3 天验证，再排正式工期）

1. **桌面层窗口 X11 路径**：验证 underlay 插件在 Linux 是否可用；不可用则用 x11rb 写最小桌面窗口 demo（含多屏、显示桌面等价物、重启后面板/图标层级是否保持）。
2. **WebKitGTK 视频解码**：在干净虚拟机（Ubuntu/Fedora 默认安装）里测 mp4(h264)/webm 播放，确认 gstreamer 插件缺失时的表现与探测方法。
3. **steamcmd Linux 32 位依赖**：在 64 位纯净系统跑 `steamcmd_linux.tar.gz`，列出确切缺失的 multilib 包清单（决定安装引导文案）。
4. **PipeWire/Pulse monitor 捕获**：现代发行版（PipeWire 默认）上 loopback 是否免权限开箱可用。

### 3.4 实施清单

| 阶段 | 内容 | 预估 |
| --- | --- | --- |
| L0 编译通过 | 所有 macOS 模块 `#[cfg(target_os)]` 收编 + 平台 trait 化重构（见 §5）；Cargo feature 按 target 裁剪；WebKitGTK 开发依赖文档 | 3~4 天 |
| L1 核心价值链路 | X11 桌面层窗口 + 屏幕枚举换 Monitor API + steamcmd_linux（含 32 位依赖引导）+ 能力探测框架 | 4~6 天 |
| L2 体验补齐 | X11 指针注入、PipeWire/Pulse 音频 loopback、MPRIS、静态壁纸按 DE 分发 | 4~6 天 |
| L3 打包打磨 | AppImage/deb、托盘 appindicator、编解码探测引导 UI、自定义窗口按钮 | 3~5 天 |

**合计约 2~3 周**（不含 spike）。其中 L1 的桌面层窗口是唯一有「做不出来」风险的项，spike 1 失败则整个项目需要重新评估（备选：GTK layer-shell only + 放弃 GNOME）。

## 4. 鸿蒙 PC 评估（长期观察项）

> 背景：鸿蒙 PC（HarmonyOS 5 PC 版，2025 年随 MateBook Pro / MateBook Fold 发布），
> ARM 架构（麒麟 X90），开发栈为 DevEco Studio + ArkTS/ArkUI + Native Kit（C/C++），
> Web 能力由 ArkWeb（Chromium 内核）提供，应用经 AppGallery 分发（审核 + 签名，侧载受限）。
> 该平台生态信息较新，下文标注「需实机验证」的条目以真机/官方文档确认为准。

### 4.1 三个硬性卡点

| # | 卡点 | 说明 | 严重程度 |
| --- | --- | --- | --- |
| H1 | **桌面动态壁纸能力未开放** | 鸿蒙 PC 的系统主题/壁纸体系面向官方主题商店，第三方「在桌面层持续渲染动态内容」的公开 API 目前**不存在**（类似 GNOME Wayland 的处境，但更封闭 —— 连扩展机制都没有）。需实机验证是否有企业/系统级 API | 🔴 没有绕过的技术路径 |
| H2 | **Tauri 无鸿蒙后端** | Tauri 依赖 wry/tao，均无 HarmonyOS 后端；官方 roadmap 未覆盖。意味着整个 UI 壳（主界面 + 壁纸窗口 + 托盘）需用 **ArkUI/ArkTS 重写**，前端 React 代码只能经 ArkWeb 复用「壁纸渲染器」部分 | 🔴 架构级重写，非移植 |
| H3 | **steamcmd 无 ARM/OHOS 版本** | Valve 只发布 x86 Windows / x86_64 macOS / 32 位 x86 Linux 三个版本，鸿蒙 PC（ARM + 非 glibc 环境）无法运行，也无官方 x86 模拟层。工坊下载链路整体断裂，且无官方 REST 下载端点可替代 | 🔴 核心价值（下载工坊壁纸）缺失 |

### 4.2 可复用资产（若未来卡点解除）

- **Rust 核心可交叉编译**：Rust 已有 `aarch64-unknown-linux-ohos` target（OpenHarmony，tier 2），`steam/`、`workshop.rs`、`library.rs`、`db.rs`、`secure_store.rs` 等纯逻辑模块理论上可编译为 `.so` 经 NAPI 供 ArkTS 调用 —— 但需逐一验证依赖（rusqlite bundled C 编译、reqwest/rustls 的 TLS、tokio 的 socket 支持）在鸿蒙 NDK 上的可用性；
- **壁纸渲染器可经 ArkWeb 复用**：renderer + webwallgl（WebGL）是纯 Web 技术，ArkWeb 内核足够新，视频/场景/网页壁纸的渲染层大概率可直接跑（WebGL 性能需实测）；
- **系统集成的鸿蒙对应物**（若 H1 解除）：正在播放 → AVSession（类比 MPRIS/GSMTC，ArkTS 标准 API）；音频可视化 → AudioKit（loopback 权限需验证）；自启/常驻 → 受鸿蒙后台管控严格限制。

### 4.3 结论与建议

**现阶段不投入正式开发**，理由：H1 决定了产品形态不成立（做不了动态壁纸），H3 决定了内容来源断裂 —— 两个卡点都不在自己掌控内，投入产出比远低于 Linux。

建议以**最小成本跟踪**：
1. **一次实机验证（1~2 天）**：确认 H1（有无壁纸/桌面层 API）与 H3（有无 x86 兼容层或 ARM Linux 容器能力，若有则 steamcmd 有解）；两项任一打通，重新评估；
2. **关注两个信号**：鸿蒙 PC 开放主题/壁纸第三方能力；OpenHarmony 社区出现 Tauri/wry 移植（H2 解除，社区已有 OpenHarmony 上的 Rust GUI 探索）；
3. **架构红利**：§5 的 trait 化重构对鸿蒙同样有效 —— Rust 核心与 UI 壳解耦得越干净，未来鸿蒙（或任何新平台）只需要重写「壳」，核心逻辑直接交叉编译复用。

## 5. 架构改造建议（L0 前置重构）

把目前散落的 `#[cfg(target_os = "macos")]` 收敛为**平台能力 trait**：

```
src-tauri/src/platform/
├─ mod.rs        # pub trait DesktopLayer / AudioLoopback / NowPlaying / SystemWallpaper / PointerSource
│                # + Capability 探测结果类型（暴露给前端）
├─ macos.rs      # 现有实现整体迁入（纯搬迁，不改逻辑）
└─ linux.rs      # L1~L2 增量实现（含能力探测，不支持的返回 Unsupported）
```

原则：
- `wallpaper/mod.rs`、`commands.rs` 等业务代码只面向 trait，编译期 `cfg` 选实现；
- 每个能力返回 `Result<_, Unsupported>`，前端据此做功能降级而不是平台判断；
- 先在 macOS 单平台上完成 trait 化（纯重构、行为不变、可直接验证回归），再开始 Linux 实现，避免并行改两套逻辑；
- Rust 核心逻辑（steam/library/db/download）保持零平台依赖 —— 这是未来鸿蒙等任何新平台的复用前提。

## 6. 总体路线

```
Linux: Spike 验证(2~3天) → L0 trait 化重构(3~4天) → L1 桌面层窗口+steamcmd(4~6天) → L2 体验补齐(4~6天) → L3 打包(3~5天)
鸿蒙:  实机验证(1~2天，与 Linux 并行不冲突) → 观察 H1/H2/H3 解除信号 → 解除后再评估
```

决策点：
1. **Linux 只做 X11 主路径**，Wayland 留给后续版本；GNOME Wayland 明确不支持并写入 README，而不是硬撑；
2. **能力探测优先于平台判断**：Linux 上任何系统集成都可能缺失，这套探测框架比单个功能实现更重要；
3. **Linux spike 先行**：桌面层窗口（最大风险）、WebKitGTK 编解码、steamcmd 32 位依赖三个验证不过关，工期无意义；
4. **鸿蒙只做验证不做开发**：H1（壁纸能力）与 H3（下载链路）是产品级卡点，均不可控；trait 化重构顺带保住未来的复用可能性即可。

---

## 7. 实施回写（2026-09-10，Linux 首版已落地）

本文 §3/§5 的方案已按「模块门面」变体实施完成（macOS 行为零变化，全部单测通过）：

| 调研项 | 落地情况 |
| --- | --- |
| §5 trait 化重构 | 用**模块门面**替代 trait：`wallpaper/platform.rs` 按 `cfg` 重导出 `macos.rs`/`linux.rs`（业务代码只面向 `platform::`）；`audio_capture`、`now_playing` 拆为公共层 + `imp` 子模块。避免了给 12.6k 行引入 trait 对象的大改，效果等同 |
| §3.1 X11 桌面层窗口 | 未自研 x11rb：desktop-underlay 插件的 Linux 实现就是 `set_type_hint(Desktop)`（= `_NET_WM_WINDOW_TYPE_DESKTOP`），与本调研建议的 <100 行自研等价，直接复用 |
| §2-1 屏幕枚举 | 换 Tauri `Monitor` API（per-monitor scale → 逻辑坐标）；显示器 id = 连接器名 FNV 哈希（保证热插拔后会话恢复）；睡眠检测降级为恒 false |
| §2-2 指针注入 | 未用 x11rb：Tauri `cursor_position`（GDK 封装）X11 有效、Wayland 自动降级；按键状态降级为恒 0 |
| §2-3 静态壁纸 | 按 DE 分发已实现（GNOME/Cinnamon/MATE/XFCE/KDE/swww/feh）；抽帧用 ffmpeg（未引入 gstreamer 依赖） |
| §2-4 音频捕获 | 桩实现（`AudioStatus.supported=false`，前端降级提示）；PipeWire 接入留待后续版本 |
| §2-5 正在播放 | **MPRIS**（`mpris` crate）已实现：1s 轮询 + 封面 file/http 拉取 + 反向控制 |
| §2-6 steamcmd | `steamcmd_linux.tar.gz` 已接入；32 位 multilib 缺失做前置检测 + 分发行版提示（spike 3 的实机验证仍建议在 Ubuntu 24.04 纯净机上跑一次） |
| §2-7 keyring | Linux 用 keyutils（`linux-native`），主凭据本就走 secure_store 本地加密文件 |
| §2-10 窗口装饰 | Linux 退回原生标题栏（未做 GTK CSD 自定义按钮，L3 项） |
| §2-11 打包 | `deb` + `appimage` targets + GitHub Actions `build-linux.yml`（ubuntu-22.04） |

遗留（对应 §3.4 的 L2/L3 未完成项）：PipeWire 音频 loopback、wlroots layer-shell、
前台应用检测（自动暂停）、GNOME 自定义窗口按钮、steamcmd 32 位依赖实机验证。
