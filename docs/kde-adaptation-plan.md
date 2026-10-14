# KDE Plasma 适配方案（WallpaperEM / Tauri 2.11）

> 目标：在 **KDE Plasma 5.27 / 6.x**（X11 与 Wayland 两种会话）上，让「动态壁纸铺桌面」这件事达到与 macOS 同等的可用性：
> 壁纸在所有普通窗口之下、桌面图标可点、托盘/快捷键/自启动完整、多屏与分数缩放正确、`自动暂停` 生效、不污染用户会话。
>
> 本文是**实施蓝图**：第 2 节是现状（全部带行号），第 3 节是硬约束，第 4 节是分层方案与取舍，第 5 节是可执行工作包，第 6 节是验收与测试，第 7 节是风险。
> 上游事实依据（KWin/Plasma 源码与 Bugzilla 链接）见配套文档 [kde-plasma-constraints.md](kde-plasma-constraints.md)。

---

## 0. 结论先行（TL;DR）

| 会话 | 现状 | 方案主路径 | 落地工作量 |
| --- | --- | --- | --- |
| **KDE / X11** | **已实测穷尽**（Plasma 5.27）：壁纸窗口拿到 `NET::Desktop` 与桌面语义（跨桌面/跳任务栏/不在任务栏/位于普通窗口与面板之下），但 plasmashell 桌面视图**恒占 stacking 最底**，客户端无法插到它下面（`_NET_WM_STATE_BELOW`、`XConfigureWindow(sibling,Below)`、`_NET_RESTACK_WINDOW`、override-redirect 四种原语全部实测无效）→ **图标必被盖**，且因点击穿透**仍可点中却看不见** | 机制 ①「透明化桌面背景」**在本机已被证明没有意义**（沉底不可能，实测透明前后截图逐像素一致）；窗口方案只剩 ②**让桌面不再是文件夹视图**（用户确认后关闭桌面图标容器，一键可回滚）；要「壁纸+图标」全满足只能走 ③壁纸插件（`P9`，已升级为战略路径） | 小（②的判定与提示已随本轮落地）；**X11 侧不要承诺图标可见** |
| **KDE / Wayland** | 桌面类型 hint 被忽略，壁纸退化为「置底普通窗口」；自动暂停、全局快捷键、熄屏暂停全部失效 | ①**KWin 窗口规则**（`Layer=Desktop` 能覆盖 `belongsToLayer()`，配合 `AcceptFocus=false`/`Skip*`/全桌面）为基线（⚠️ KWin 5 的 `Layer` 是否为 unused **未证**，KWin 6 才是确定路径）；②进阶：`zwlr_layer_shell_v1` `Background` 层（KWin 已实现且无白名单，**需 spike**，这是 X11 那条死路的唯一替代） | 中（1 工作包 + 1 spike） |
| **两者共通的 KDE 集成** | 全仓零 KWin/KScreen/logind/portal 调用 | 新增 `src-tauri/src/kwin.rs` + `kde` 能力探测，承接：会话探测、鼠标穿透、遮挡快照、熄屏、每屏几何、快捷键、静态壁纸 | 中（4 个工作包） |
| **工程化** | Linux 冒烟只有 Xvfb(X11)、无断言、CI 无 KDE | 新增 KDE X11 + KDE Wayland（nested `kwin_wayland --virtual`）两级冒烟，纯函数单测覆盖规则生成/会话探测/配置读写 | 中（2 个工作包） |

**一句话**：KDE 适配的**真正难点是「桌面图标与壁纸在 plasmashell 里是同一个窗口」**——真机四轮实验已证明窗口堆叠技巧**完全解决不了**它（KWin 不让任何客户端窗口排到桌面视图之下）。所以方案只剩两条诚实的路：**② 让桌面不再是文件夹视图**（窗口方案里唯一可行，代价是用户失去桌面图标），或 **③ 做成 Plasma 壁纸插件**（唯一「壁纸+图标」全满足解，`P9` 战略路径）；其余工作是把现在靠 GTK 硬凑的桌面语义换成 KWin 自己的窗口规则 / D-Bus 表达。

---

## 1. 范围与非目标

**范围内**

- 桌面层窗口在 KDE X11 / Wayland 的正确落位（并明确「桌面图标」这一诉求最终由哪种机制满足）。
- 会话探测与能力路由（Plasma 5/6、X11/Wayland、KWin 是否在跑）。
- 主窗口/设置窗口在 KWin 下的无边框表现（拖动、缩放命中、阴影、圆角）。
- 托盘（SNI）、自启动、全局快捷键、熄屏/空闲、多屏与分数缩放、内存压力。
- 系统静态壁纸同步（锁屏/登录/引擎未运行时的视觉一致）。
- 音频可视化与「正在播放」在 KDE 下的可用性确认与依赖声明。
- 打包（deb/AppImage 依赖、`.desktop`）、CI、文档。

**非目标（明确不做，避免范围蔓延）**

- ❌ **每虚拟桌面不同壁纸**：Plasma 6 自身都没有该功能（[bug 341143](https://bugs.kde.org/show_bug.cgi?id=341143) CONFIRMED），只承诺「所有虚拟桌面/活动同一张」。
- ❌ **Flatpak 分发**：会丢掉音频 monitor 与壁纸落位的自由度；本文只在第 7 节记为「若将来做，需砍功能」。
- ❌ **依赖 `org_kde_kwin_plasma_shell`**：协议原文明确 “Regular clients must not use this protocol”。
- ❌ **把壁纸塞进 plasmashell 的 QML 插件作为首发方案**：崩溃会带崩 plasmashell，且需要自研帧传输（DMA-BUF + sync fence），作为 `P9` 演进项保留。

---

## 2. 现状评估（代码证据）

> 行号相对仓库根。P0/P1… 为第 5 节的工作包编号。

### 2.1 桌面层窗口

- **建窗参数无会话分支**：`src-tauri/src/wallpaper/mod.rs:1675-1695` 的 `WebviewWindowBuilder` 固定 `decorations(false)`(:1677)、`transparent(true)`(:1678)、`shadow(false)`(:1679)、`visible(false)`(:1680)、`resizable(false)`(:1681)、`skip_taskbar(true)`(:1687)、`focusable(false)`(:1694)、`focused(false)`(:1695)。唯一平台分支是 macOS 的 `data_store_identifier`(:1696-1701) 与 Windows 的延迟重挂(:1753-1777)；**X11/Wayland 走同一条路**。
- **桌面层只靠一个 GTK type hint**：`src-tauri/src/wallpaper/linux.rs:211-229 apply_desktop_window()` → :217 `window.set_desktop_underlay(true)`、:221 `lower_below_desktop_icons()`、:223 `set_visible_on_all_workspaces(true)`、:224 `set_skip_taskbar(true)`、:226 `set_ignore_cursor_events_safe(window, !interactive)`、:227 `set_frame(...)`。
- **插件在 Linux 只有两行**：`.cargo-home/registry/src/index.crates.io-1949cf8c6b5b557f/tauri-plugin-desktop-underlay-0.2.1/src/core/linux.rs:8` 就是 `gtk_window.set_type_hint(WindowTypeHint::Desktop)`（以及 :14 的 `Normal`）；**无 GDK backend 判断、无 Wayland 分支**。
- **KDE 被显式排除在「图标浮在壁纸之上」之外**：`wallpaper/linux.rs:239-242` `lower_below_desktop_icons()` 第一句 `if !shell_icons_are_overlay() { return; }`；白名单 `wallpaper/linux.rs:269-281` 只有 `gnome | budgie | pantheon`。注释 `wallpaper/linux.rs:236-238` 直说 KDE(plasmashell)/XFCE 桌面窗口自绘不透明背景，「保持『壁纸在上、图标被盖』的既定降级」。
- ⚠️ **结论：KDE 下「桌面图标被壁纸盖住」是当前被设计接受的降级**（注释 :236-238 原文即「保持『壁纸在上、图标被盖』的既定降级」）。精确地说，坐落的默认路径是**视觉被盖、点击穿透**：`interactive=false` 时 `set_ignore_cursor_events_safe(window, true)`（:226）让点击落到下层的 plasmashell 桌面窗口，图标因此**仍可被点中，但看不见自己点了什么**——比「不可点」更糟的一类问题（无视觉反馈、右键菜单/框选行为不可预期）。这是 KDE 适配要解决的第一号问题（上游同类问题实证：[linux-wallpaperengine#370](https://github.com/Almamu/linux-wallpaperengine/issues/370)）。**注意**：图标若真能浮在「壁纸之上」（机制 ① 成功后的形态），其实际输入路由（KWin 对 DesktopLayer + 穿透窗口如何处理点击、图标能否响应框选/右键）必须真机实测，本方案不预设结论。
- **陈旧记账兜底已存在**：`wallpaper/mod.rs:1720-1730` 在建窗时清掉同 label 的 underlay 记账（插件按 label 记账、窗口销毁不清理，见插件 `tauri-plugin-desktop-underlay-0.2.1/src/core/mod.rs:73-80 is_desktop_underlay()` / `:82-87 set_desktop_underlay()`）；应用点在 `wallpaper/mod.rs:1732`（建窗后）与 `:1741`（`show()` 后幂等重放），多屏循环在 `:2487`、`:3964`。
- **Wayland 下 `_NET_WM_WINDOW_TYPE_DESKTOP` 无效**：`wallpaper/linux.rs:7-16` 模块头已承认，Wayland 下退化为「置底 + 全工作区的普通无边框窗口」。⚠️ 并且要纠正一个常见误判：`set_visible_on_all_workspaces` / `set_skip_taskbar`（`wallpaper/linux.rs:223-224`）**只经 GTK 调用，仓库里没有任何显式的 X11 原子写入**——它们在 X11 与 Wayland 下的落地语义**都未经实测**（`P1` 必须两档都测），不能假定 X11 侧可靠。
- **遮挡快照在 Wayland 恒失效**：`wallpaper/linux.rs:335-343` → :340 `force_x11 = GDK_BACKEND == "x11"`，:341-343 只要 `WAYLAND_DISPLAY` 存在就 `return None`。理由（:331-334）是 Wayland 无全局窗口清单、Xwayland 清单残缺会误判。数据源实现 `:345-431`（`_NET_CLIENT_LIST_STACKING` :352、过滤自身 PID :382、DESKTOP/DOCK :392、VIEWABLE :400、`covered_ratio` 按 `active_screens()` 还原像素 :419-431）。
- **其余 Linux 平台接口**：`occlusion_snapshot()` 在 X11 会话有真实实现（上一条 `:345-431`），**Wayland 会话恒 `None`**；`frontmost_kind()` 本轮补齐 X11 路径（EWMH `_NET_ACTIVE_WINDOW`，见 §5 `P5`），Wayland 会话恒 `Unknown`；`display_asleep()` 本轮接入 KWin 空闲（`:194-205` → `kwin::idle_seconds`）；仍为空实现的只有 `on_ac_power()` 恒 `None`（`:55`）、`start_auto_pause_observer()`（`:315-317`）、`start_desktop_click_monitor()`（`:320`）。**这些正是 KDE 下用 KWin/D-Bus 可以补齐的部分**（见 §4.4）。

### 2.2 KDE 特有集成点

- **静态壁纸**：`src-tauri/src/system_wallpaper.rs:539-541` 入口读 `XDG_CURRENT_DESKTOP` 并 `to_ascii_uppercase()`；KDE 分支 `:624-629` 用 `plasma-apply-wallpaperimage`，旧版回退 `:631-643` 拼 JS 走 `qdbus org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript`。两条路径都是 `return`，**失败不回退到 swww/feh**（:646-651），最终错误文案见 `:652-654`。依赖声明里**没有** `plasma-workspace`/`qdbus`：`src-tauri/tauri.conf.json:35-38` 的 `bundle.targets` 是 `["app","dmg"]`，Linux 的 `["deb","appimage"]` 在 `src-tauri/tauri.linux.conf.json:20-24`，而两者都不校验外部命令（deb 段见 `tauri.conf.json:58-69`，只有 GStreamer + ffmpeg）。
- **主窗口模糊**：`src-tauri/src/blur.rs` 是**Linux 通用**实现（不是 KDE 专属），按 GDK backend 分派（:53-58）：X11 路径 :63-92 **无条件**写 `_KDE_NET_WM_BLUR_BEHIND_REGION`（空区域 = 整窗），任何 X11 会话（含 XFCE/GNOME-X11）都会写、只是只有 KWin 认；Wayland 路径 :99-173 才门控 `XDG_CURRENT_DESKTOP` 含 `kde`(:103)，经 `org_kde_kwin_blur_manager` :160-162 create+commit。文件头 :1-18 那句「主流桌面中唯一原生支持应用窗口实时模糊的合成器」是**本仓自述**，非上游事实，仅作意图说明。:36 只取 `"main"`。调用点 `src-tauri/src/lib.rs:1175-1185 apply_vibrancy()`（:1177 `blur::apply`），由 `lib.rs:166`（setup）与 `src-tauri/src/main_window.rs:346-347`（主窗口重建后）触发。
  - ⚠️ **`org_kde_kwin_blur` 已在 KWin master 移除，替代是 `ext_background_effect_manager_v1`**（移除发生在 Plasma 6.7/6.8 之间，精确版本待 tag 复核；依据见约束文档 §4）。届时当前实现会静默降级为「无模糊」。
  - ⚠️ 设置面板窗口（`src-tauri/src/props_window.rs:58-61`）也是 `decorations(false)+transparent(true)`，但材质调用 :72-81 被 `#[cfg(any(macos, windows))]` 排除，**KDE 下设置面板没有模糊**。
- **KWin / KScreen / logind / portal 调用：全仓 0 处**（`src-tauri/src` 内 `KWin|kwin` 共 22 次出现、分布在 3 个文件，全部是注释或 `blur.rs` 的协议名；`KScreen` 0、`org.freedesktop.portal` 0）。唯一真实 KDE D-Bus 调用是上面那条 `qdbus org.kde.plasmashell`。

### 2.3 应用壳层

- **托盘**：配置里无 `trayIcon` 段，全部由 Rust 构建（`src-tauri/src/lib.rs:826 build_tray`、:938 `TrayIconBuilder::with_id("main-tray")`）；图标分支 `lib.rs:1118-1141` —— macOS 用 `icons/tray-44.png` + `icon_as_template(true)`(:1123-1127)，**非 macOS 走 `app.default_window_icon()`**(:1128-1141，缺失时只 warn :1139)。⚠️ **运行库依赖未声明**：SNI 需要 `libayatana-appindicator3`，但 `tauri.conf.json:58-69` 的 `deb.depends` 只有 4 个 GStreamer 包。**未能查证**打包器（npm 预编译的 tauri CLI，源码不在本仓 registry 快照内）是否自动追补该依赖 → 按「显式补齐」处理，并入 `P8`。
- **全局快捷键**：`src-tauri/src/hotkeys.rs:26` 用 `tauri-plugin-global-shortcut`（`Cargo.lock` 解析 2.3.2）；默认键位只按 `#[cfg(target_os="macos")]` 二分（:44-59），**无 Wayland 判断**；`init` 失败只 warn（:245-246），`apply` 逐个注册并回滚（:252-297）。底层 `global-hotkey-0.8.0` 的 Linux 后端**只有 X11 `grab_key`**（`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/global-hotkey-0.8.0/src/platform_impl/x11/mod.rs:143`，依赖仅 `x11rb`+`xkeysym`）→ **KDE Wayland 下全局快捷键完全不可用，且界面不会告诉用户**。
- **自启动**：`tauri-plugin-autostart` + `MacosLauncher::LaunchAgent`（`lib.rs:57-58`）；Linux 落地为 `~/.config/autostart/WallpaperEM.desktop`（`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/auto-launch-0.5.0/src/linux.rs:76` 用 `:81-82` 的 `get_dir()` 拼路径），模板无任何 KDE 键（:32-52）。KDE 会读该目录，**功能可用**；缺的是「会话恢复/启动顺序」处理。
- **主窗口无边框**：基线与 Linux 覆盖配置逐字段一致（`tauri.conf.json:23-25` vs `tauri.linux.conf.json:13-15`），主窗口重建同样 `decorations(false)/shadow(true)/transparent(true)`（`src-tauri/src/main_window.rs:321-337`）。前端自绘标题栏：`src/components/TopBar.tsx:58`（`data-tauri-drag-region` 整条 header）+ `:96` 右上角控件、`src/components/WindowControls.tsx:40/51/70`。
  - ⚠️ 按 KWin 的 decoration 逻辑推断，无装饰窗口**不提供边缘 resize、不画阴影、不管圆角**（约束文档 §12 有源码依据；**该结论未逐行核实、待真机验证**，见 §7 开放问题 4）→ 当前 `shadow: true` 在 KDE 上预计无效果；若实测确实不能 resize，只能靠自绘或 KWin 的 `Meta+右键` 兜底。
- **缩放/DPI**：`set_zoom`/`GDK_SCALE`/`GDK_DPI_SCALE` **全仓 0 命中**；显示器换算集中在 `wallpaper/linux.rs:149-193 query_monitors()`（Tauri `available_monitors` × per-monitor scale → 逻辑坐标）。Wayland 分数缩放下 `wl_output.scale` 是 `ceil(scale)`（1.25 → 客户端看到 2），当前实现没有 `wp_fractional_scale_v1` 处理 → **壁纸在 125%/150% 缩放下有渲染模糊/尺寸误差风险**。
- **音频 / 正在播放**：应用侧只做转发（`src-tauri/src/audio_capture.rs:3-4`、`src-tauri/src/now_playing.rs:9-12`），实现在 media-bridge：MPRIS over zbus（`../media-bridge/crates/media-bridge/src/platform/linux.rs:1-25`）、采集优先 `parec` 次选 `pw-record`（`../media-bridge/crates/media-bridge/src/audio/linux.rs:5-6`），纯 PipeWire 不能用 `@DEFAULT_MONITOR@`（:13-16）、默认 sink 轮询走 `pactl` 退回 `wpctl`（:27、:274-282）。**KDE 下可用，但依赖未声明**。
- **内存**：`src-tauri/src/mem_pressure.rs:94-127` Linux 读 `/proc/meminfo`（真实实现）；`src-tauri/src/mem_watch.rs:193-205` 归属观测在非 macOS 是空实现（`snapshot()` 返回空、`attribution_available()` false），故 `WARN_TOTAL=1500MB`/`WARN_SINGLE=800MB`(:208-210) 在 KDE 下形同虚设。

### 2.4 打包与 CI

- **发布基线**：`.github/workflows/build-linux.yml` 原生 `ubuntu-22.04` / `ubuntu-22.04-arm` 双矩阵，产物 `deb` + `AppImage`；apt 清单为 `libwebkit2gtk-4.1-dev … libayatana-appindicator3-dev … xdg-utils`，**无任何 KDE 包**。
- **本地打包/测试**：`Dockerfile.linux` 用 `ubuntu:24.04`（:10），装了 `xvfb dbus-x11 libgl1-mesa-dri libegl-mesa0 libgles2`（:22-23，apt 块 :14-24）供无头冒烟。
- **冒烟测试**：`scripts/linux-docker-entrypoint.sh:39-48` = `dbus-run-session -- xvfb-run -a sh -c "timeout 25 WallpaperEM"`，之后只 `grep -E "setup complete|wallpaper engine ready|now playing|tray|error|panic"`，**无断言、25s 强杀、纯 X11、`XDG_CURRENT_DESKTOP` 未设置**。
- **`.github/**` 内 grep `kde|plasma|kwin|XDG_CURRENT_DESKTOP|weston` → 0 命中**：KDE 的任何会话（X11/Wayland）都没有自动化覆盖，发布流程拦不住 KDE 回归。
- **deb 依赖**：`src-tauri/tauri.conf.json:58-69`（`depends`=4 个 GStreamer 包，`recommends`=`gstreamer1.0-pipewire`/`ffmpeg`）；未声明 `plasma-workspace`、`qdbus`、`pulseaudio-utils`、`pipewire-bin`。
- **文档**：`README.md:290` 把 KDE 列为主路径之一；`README.md:409` 承认「Wayland 退化为普通置底窗口，建议用 X11」；`README.md:410` 说「自动暂停尚未实现」**与代码现状不符**（`wallpaper/linux.rs:335-431` 的 X11 遮挡快照已接入，见 `wallpaper/auto_pause.rs:150/280`）。
- **真机验证底座（已实测，2026-10-14）**：验证机 `parallels@10.211.55.5` = Ubuntu 24.04.3 arm64（3.8 GiB RAM），**拿不到 Plasma 6**：官方仓库只有 `plasma-desktop 4:5.27.12-0ubuntu0.1` / `kwin-* 4:5.27.11`；Kubuntu Backports PPA(noble) 的 `binary-{amd64,arm64}/Packages.gz` **实测均为 20 字节（空索引）**；KDE neon 官方**只有 amd64 ISO**（arm64 无用户可用镜像）。⇒ 这台机器只能验证 **Plasma 5.27 / X11** 侧结论，Plasma 6 专属项（机制①在 6.x 的行为、`zwlr_layer_shell_v1` v5、`ext_background_effect_manager_v1`、portal `Wallpaper`）**在这台机器上无法验证**，需换 Ubuntu 25.04（`plasma-desktop 6.3.4`，有 arm64）或 Fedora KDE/openSUSE。装配步骤见 [kde-smoke-setup.md](kde-smoke-setup.md)。

### 2.5 真机验证结果（Plasma 5.27 / X11，2026-10-14 实测）

底座：Ubuntu 24.04.3 arm64 + `kubuntu-desktop`（`kwin 5.27.11` / `plasma-desktop 4:5.27.12`）+ Xvfb 1280x800 + `kwin_x11 --replace` + `plasmashell --no-respawn`，用**真实用户会话总线**（`/run/user/1000/bus`）起，应用取 `src-tauri/target/release/WallpaperEM`（45 MB，`cargo build --release --locked` 在 VM 上原生构建）。一条命令复现：`bash scripts/kde-vm-session.sh realbuscase`；`belowtest` 与 `probes` 为另两个模式。

**机制①（`NET::Desktop` 型 → DesktopLayer）在 X11 上成立，但落点比预期更糟：**

- 我们的壁纸窗口实测 `_NET_WM_WINDOW_TYPE_DESKTOP`、`WM_CLASS="WallpaperEM","WallpaperEM"`、`_NET_WM_PID` 正确、1280x800 @ (0,0)，`_NET_WM_STATE = STICKY, SKIP_TASKBAR, SKIP_PAGER`，且**位于普通窗口之下**。
- 但 stacking 实测为（自底向上）：`#1 plasmashell 的 DESKTOP 窗口` → **`#2 我们的 DESKTOP 窗口`** → `#3 我们的 NORMAL 窗口` → `#4 plasmashell 面板`。即 **DesktopLayer 在 Plasma 桌面视图之上**，我们的壁纸压在 plasmashell 自绘的图层上。
- 因此最关键的修正：**§4.1 的「机制①能保住桌面图标」这一假设在 X11 上不成立**，需要重新定义为「机制①= 拿桌面窗口语义（跳任务栏/全桌面/在普通窗口之下）」，而**图标可见性必须靠另一条路**（Plasma 6 Wayland 的 layer-shell `Background`，或 §4.5 的壁纸插件）。
- **`_NET_WM_STATE_BELOW` 是无效解**：给壁纸窗口加上 plasmashell 自己用的同一个 state（plasmashell 桌面窗口实测就带 `_NET_WM_STATE_BELOW`）后，KWin 接受了该状态但层序不变——`belowtest` 实测三种组合同为 `名次 2/4`：`A 仅 DESKTOP`、`B DESKTOP+BELOW`、`C NORMAL+BELOW`。⇒ 不要按「加个 BELOW / keep-below 规则就把壁纸压到桌面图标下面」排期，这条路在本机已被证伪。
- 像素取证：壁纸窗口在世 vs 杀掉后，桌面图标区截图哈希不同（`6f431327…` vs `483a9692…`）→ 确认壁纸确实画在桌面视图之上（图标被盖）。截图见 `scripts/kde-vm-session.sh` 输出目录的 `screen-with-app.png` / `screen-without-app.png`。
- 复现该结论的脚本断言在**修复后**应把「壁纸窗口在 stacking 中存在且 type=DESKTOP」判 PASS、把「盖住图标区」记为**已知降级 FAIL**（本轮即如此，`realbuscase` 退出码 1 属预期）。

**机制①（把壁纸压到 plasmashell 桌面视图之下）在 X11 上已被穷尽证伪**（`p1c` 模式，三次独立原语 + 一个对照组）：

- 层序实测（自底向上）：`#1 plasmashell DESKTOP` → `#2 我们的 DESKTOP` → `#3 我们的 NORMAL` → `#4 plasmashell 面板`。等离子桌面视图**始终是最底层**。
- 三种「排到兄弟之下」的原语全部无效：
  1. `_NET_WM_STATE_BELOW` 加在我们的 DESKTOP 窗口上 → 名次不变（p1b/p1c 两次实测，且 plasmashell 桌面窗口本身就带 `BELOW`）。
  2. `XConfigureWindow(sibling=plasmashell 窗口, stack_mode=Below)` → X 服务端直接 `BadMatch (invalid parameter attributes)`，KWin 不接受这种 sibling 重排。
  3. EWMH `_NET_RESTACK_WINDOW`（`_NET_SUPPORTED` 里**确实声明了**该原子）`detail=Below, sibling=plasmashell 桌面视图` → 名次不变；连 `detail=Above` 也同样无效。
- 结论：**KWin 自行管理桌面型窗口的层内次序，客户端无法把窗口插到 plasmashell 桌面视图之下。** 因此在 X11 上「动态壁纸 + 系统桌面图标同时可见」**不可能**通过窗口方案达成——不是「还没试对」，而是已被穷尽。
- 这条结论**不必外推到 Plasma 6/Wayland 的 layer-shell**：那里壁纸走 `zwlr_layer_shell_v1` 的 `Background` 层，与 plasmashell 的 desktop 窗口是否同层、谁在上，仍属 Plasma 6 专属待验项（本机拿不到 Plasma 6）。但**上游实证已给出悲观先例**：linux-wallpaperengine#370（Plasma 6.3.4）就是「第三方桌面层窗口上去之后桌面图标消失」。
- 因此 §4.1 的机制排序必须改：**① 在 X11 上出局**；窗口方案下唯一稳的是 **②（关掉/透明化该层，接受图标取舍）**；而「壁纸+图标都要」只剩 **③ Plasma 壁纸插件**这条路（它不是演进彩蛋，而是该诉求的唯一正解）。

**同一轮证伪/证实的两条早期判断：**

- 会话总线：KDE 服务必须挂在**真实用户总线**上。用 `dbus-run-session` 起私有总线的旧冒烟里，`kglobalaccel`/`kactivitymanagerd`/`KScreen` 全部 `Spawn.ChildSignaled`（signal 6）死亡，`plasmashell` 直接 `Aborting shell load: The activity manager daemon (kactivitymanagerd) is not running`；换成 `/run/user/1000/bus` 后 `org.kde.KWin`/`org.kde.plasmashell`/`org.kde.KScreen`/`org.kde.kglobalaccel`/`org.freedesktop.portal.Desktop` **全部 present**。
- `org.kde.kglobalaccel` 在 Plasma 5.27/X11 会话上**存在**（此前「KDE Wayland 无可用后端」的担心仅针对 Wayland）；但 `global-hotkey 0.8.0` 只走 XGrabKey，实际注册是否与 KDE 抢键仍待验。
- 空闲源实测**不可用**：`org.freedesktop.ScreenSaver.GetActive` 与 `GetSessionIdleTime` 在本机返回 `org.freedesktop.DBus.Error.NotSupported`（「This method is not part of the idle inhibition specification」）→ §4.3 里「用 ScreenSaver 判空闲」的写法在 Plasma 5.27 上会走到 `NotSupported` 分支，需改走 KWin 的 `ext-idle-notify`/`org_kde_kwin_idle` 或 logind `IdleHint`。
- `kscreen-doctor -j` 在无头 Xvfb 下**无输出**（`org.kde.KScreen` 服务在，但命令不吐数据，`~/.config/kwinoutputconfig.json` 也不存在）→ 每屏查询的退路要按「KScreen D-Bus 对象优先、`kscreen-doctor -j` 次之」排序，别把后者当默认。
- `KWin` 窗口规则键在 `libkwin.so.5.27.11` 里可见 `layer` / `Layer` / `desktops` / `activity` / `skipPager` / `skipTaskbar`；但 KWin **5** 的 `Layer` 规则是否为「未实现（unused）」**本机仍未能证**（`kwinrulesrc` 不存在、`/KWin queryWindowInfo` 在 KWin 处理 `loadScript` 的脚本确认框时 `NoReply` 阻塞）。⇒ 此项保留为开放问题，且**不应**作为图标可见性的依赖。

---

## 3. KDE 侧硬约束（写方案必须遵守）

来自 [kde-plasma-constraints.md](kde-plasma-constraints.md)，每条都有上游源码/Bugzilla 依据：

1. **层序**（自下而上）：`DesktopLayer < BelowLayer < NormalLayer < DockLayer < AboveLayer < NotificationLayer < ActiveLayer < CriticalNotificationLayer < OnScreenDisplayLayer`（另有 Popup/Overlay 用于弹出层）。关键推论：`NET::KeepBelow` 落在 **BelowLayer**，它在 DesktopLayer **之上**——所以 keep-below 窗口会盖住桌面图标、**永远进不了桌面层**。想要桌面语义只能进 DesktopLayer。（注意：KWin 规则里可选的 Layer 下拉值**不含 `DockLayer`**。）
2. **KWin 对「桌面窗口」有单例语义**：`findDesktop()` 从 stacking 顶部往下找第一个 `isClient() && isOnDesktop && isOnOutput && isDesktop() && isShown() && wantsInput()` 的窗口，调用点包括「点空桌面得焦点」与「显示桌面」。⚠️ 但这**不是必然争抢**：`wantsInput()` 可由规则 `AcceptFocus=false`（或 `WM_HINTS input=False`）关掉，从而让 `findDesktop()` 跳过我们的窗口——这也是验收项。
3. **Plasma 6 Wayland 有 `zwlr_layer_shell_v1` v5，无客户端白名单**（KWin 内嵌协议 + `LayerShellQt` 官方 Qt 封装）→ Wayland 下「贴桌面」的**协议层是通的**。
4. **layer-shell 没有虚拟桌面/活动维度** → 跨桌面要靠 KWin 窗口规则（规则字段含 `Layer`/`Desktops`/`Activity`/`SkipTaskbar`/`SkipPager`/`SkipSwitcher`/`DecorationPolicy`/`OpacityActive`/`OpacityInactive`；**没有单数 `Opacity`**）。规则类型不同：`Layer` 是 **Force** 型（可覆盖 `belongsToLayer()`），`Desktops`/`Activity` 是 **Set** 型。
5. **`org_kde_kwin_plasma_shell` 第三方禁用**。
6. **模糊协议换代**：X11 原子仍在但历史 bug 多；Wayland `org_kde_kwin_blur` 在 **Plasma 6.7/6.8 之间**（master 已移除该协议，提交 `7f9e0aa1`；精确版本号待 tag 复核）被移除，替代 `ext_background_effect_manager_v1`；模糊效果默认开（`BlurEffect::enabledByDefault()` 在非软件渲染时 true）但**用户可关** → 必须降级。
7. **官方设壁纸 API 只有三个**：PlasmaShell `evaluateScript` / `setWallpaper` / `wallpaper`（**整个 `org.kde.PlasmaShell` 是上游 Internal API**）；`plasma-apply-*` 只有 5 个命令（wallpaperimage/cursortheme/desktoptheme/colorscheme/lookandfeel），**没有**通用「启用任意插件壁纸」的 CLI。Plasma **6.5.0** 起另有 `org.freedesktop.impl.portal.Wallpaper`（只能设图片，[bug 485966](https://bugs.kde.org/show_bug.cgi?id=485966)）。
8. **每虚拟桌面不同壁纸在 Plasma 6 不存在**。
9. **分数缩放**：客户端不能直接乘 `wl_output.scale`（KWin 给 `ceil`），要用 `wp_fractional_scale_v1` 的 `preferred_scale/120` + viewporter；layer-shell 的 size/anchors/margins 是**逻辑坐标**。
10. **每屏查询按版本分叉**：`org.kde.KScreen` 的 KDED 模块 **Plasma 5 起长期存在，6.8 起移除**（master 改为 `kscreenctl`）；稳妥退路是 `kscreen-doctor json`（其与 `~/.config/kwinoutputconfig.json` 的等价性**未查证**）。
11. **会话探测**：`XDG_CURRENT_DESKTOP` 按冒号拆分后大小写不敏感找 `kde`（KDE 自己就这么做），配合 `KDE_FULL_SESSION`、`KDE_SESSION_VERSION`(5/6)、`XDG_SESSION_TYPE`/`WAYLAND_DISPLAY`；再用 `org.kde.KWin` 是否存在二次确认。
12. **全局快捷键**：X11 由 KWin 内嵌 KGlobalAccelD 处理（直接 XGrabKey 会抢键）；**Wayland 只有 `org.freedesktop.portal.GlobalShortcuts`**（KDE 后端自 v5.27.0 起），键位由桌面决定、可能没绑定。托盘用原生 SNI 即可（`IconThemePath` 问题在 Plasma **6.0.1** 修复）；无边框窗口**没有** KWin 的阴影/圆角（由 decoration 提供，`decorationPolicy()==None` 时不画），**边缘 resize 是否完全没有待实测**。

---

## 4. 分层方案

### 4.1 第一层：桌面层落位（本方案的核心）

**先说清 KDE 的结构性障碍（决定整个方案形态）**

Plasma 6 的**桌面图标与桌面壁纸在同一个窗口里**：DesktopView 是 `PlasmaQuick::ContainmentView`，壁纸是宿主注入 containment 根对象的 QQuickItem，图标（plasma-desktop 的 desktop containment = ContainmentItem + FolderViewLayer）在同一窗口内绘制。因此：

> ❌ **已推翻的设想：把壁纸窗口抬（raise）到 plasmashell 桌面窗口之上。** 窗口间 stacking 压不过窗口内绘制顺序——这样做会把 plasmashell 桌面窗口**连同它画的图标一起**盖住，而不是让图标浮到壁纸上；而且按现状壁纸本来就在 plasmashell 之上，raise 不产生任何可观察变化，只留下两个 desktop 窗口并存的副作用。**不要按这个思路排期。**

于是「壁纸在最底、图标浮在壁纸之上」在 KDE 上只剩三条候选机制，而**其中两条已被真机实测改写**：

| 机制 | X11（Plasma 5.27 实测） | Wayland（Plasma 6，本机不可验） | 性质 |
| --- | --- | --- | --- |
| **① 让 plasmashell 桌面背景变透明** | ❌ **无效**：它只在「我们已沉到桌面视图之下」时才有意义，而沉底已被穷尽证伪；单独做透明前后截图逐像素一致 | ❓ 存疑：`zwlr_layer_shell_v1` 的 `Background` 与 plasmashell 的 desktop 窗口谁在上未验 | 不要按「先透明化」排期 |
| **② 关掉 plasmashell 桌面容器**（桌面布局选「桌面」而非「文件夹视图」），让图标不再由 plasmashell 绘制 | ✅ **唯一可行的窗口方案** | ✅ 同样适用 | 100% 支持、可逆；代价：用户失去桌面图标 |
| **③ 做成 Plasma 壁纸插件**（跑在 plasmashell 内，壁纸成为 containment 的背景层） | ✅ 唯一「壁纸+图标」全满足解 | ✅ 同样适用 | 需自研帧传输、崩溃会带崩 plasmashell（`P9`，**本轮升级为战略路径**） |

**KDE / X11 —— 结论：外部窗口方案只能与「无桌面图标」共存**

- **沉底被穷尽证伪**（`p1b`/`p1c`/`p1d`/`p1e` 四轮，全部实测）：`_NET_WM_STATE_BELOW`（DESKTOP 与 NORMAL 两种窗口类型都试过）、`XConfigureWindow(sibling=桌面视图, stack_mode=Below)`（X 服务端 `BadMatch`）、`_NET_RESTACK_WINDOW(detail=Below/Above)`、`xdotool windowraise`（抬 plasmashell 自己）、override-redirect 全屏窗口——**没有一条能把我们的窗口排到 plasmashell 桌面视图之下**。桌面视图自己带 `_NET_WM_STATE_BELOW` 并固定占据 stacking 的 `#1`（最底），KWin 自行维护该层内次序，客户端无法插到它下面。
- **机制① 因此失去意义**：透明化只在「我们已在桌面视图之下」时才产生可观察差异。实测证据：桌面壁纸换成全透明 PNG（`plasma-apply-wallpaperimage` 报 `Successfully set the wallpaper for all desktops`）前后截图**逐像素一致**——品红替身窗口在「默认壁纸」与「全透明壁纸」两个阶段都是整屏 `(255,0,255)`。
- **能拿到的仍然有效**：我们的 `DESKTOP` 类型窗口确实拿到了「桌面窗口语义」——实测 stacking 自底向上为 `#1 plasmashell 桌面视图 → #2 我们的壁纸窗口 → #3 我们的 NORMAL 窗口 → #4 plasmashell 面板`，即**在普通窗口之下、面板之下**，跳任务栏/全桌面/不抢焦点也都在（`_NET_WM_STATE = STICKY, SKIP_TASKBAR, SKIP_PAGER`）。机制①的价值应重述为「拿桌面窗口语义」，**不是**「保住桌面图标」。
- 因此 KDE/X11 的落地选择只剩三条，且必须在设置页如实呈现：
  1. **②「关闭桌面图标」（推荐）**：把桌面 containment 从「文件夹视图」改成纯桌面。此后我们的 DesktopLayer 窗口覆盖的是一块只画壁纸的空白 containment，图标问题不复存在。实现方式：不手改用户配置，改为应用内引导 + 可回滚的一键切换（写前备份）；用户失去桌面图标与桌面右键菜单，需明确告知并确认。
  2. **③ 壁纸插件（`P9`，唯一全满足解）**：见 §4.5。
  3. **默认现状（不推荐，但必须如实告知）**：保留桌面图标 ⇒ 图标被壁纸**盖住**；又因为默认 `interactive=false` 会走 `set_ignore_cursor_events(true)`，**图标仍可点中却看不见**（`p1c` 截图实证）——这比「点不到」更糟。首次在 KDE 上启动时必须提示，并给出上面的 ② 入口。
- `P2` 的窗口规则与 `P1` 的 layer-shell 仍有价值（跳任务栏/全桌面/不抢焦点/层归属），但**都不要指望它们解决图标可见性**。

**KDE / Wayland —— 规则路径（`P2`）为基线，layer-shell（`P1` spike）为进阶**

> ⚠️ 上一条结论是**结构性的**，不是 X11 的实现细节：桌面图标与壁纸画在**同一个窗口**里，所以任何外部窗口要么盖住图标、要么被不透明桌面背景盖住。因此 Wayland 上同样只有 ② 与 ③ 两条路；layer-shell 的价值是「拿到 Background 层归属」，**不等于**「拿到图标之下」。Plasma 6 上 `Background` 层与 plasmashell 桌面窗口的先后关系仍属待验项，且上游已有悲观先例 linux-wallpaperengine#370（Plasma 6.3.4 桌面图标消失）。

- **基线（`P2`）KWin 窗口规则**：`Layer=Desktop` + `SkipTaskbar/SkipPager/SkipSwitcher=true` + `Desktops=所有` + `Activity=所有` + `DecorationPolicy=None` + `AcceptFocus=false`（**注意：KWin 规则里没有 `FocusPolicy` 字段**，不取焦点要写 `AcceptFocus=false`；透明度字段是 `OpacityActive`/`OpacityInactive`，也没有单数 `Opacity`），按窗口 `resourceClass`（GTK 的 `WM_CLASS`；Tauri 是否把它设成 identifier **需实测**，不能假定）写成规则，写入 `~/.config/kwinrulesrc` 后 `qdbus org.kde.KWin /KWin reconfigure` 生效。
  - 已核实：`Layer` 规则的 `checkLayer()` 优先于 `belongsToLayer()`，**能**把普通窗口强制放进 DesktopLayer。
  - 规则字段的规则类型不同，写配置时语义不同：`Layer` 是 **Force** 型（`layerrule(UnusedForceRule)`），`Desktops`/`Activity` 是 **Set** 型（`UnusedSetRule`）。
  - 优点：**不需要任何新协议**，X11/Wayland 通用；不改合成器代码；规则可随「KDE 集成」开关安装/卸载（把我们的规则块与用户既有规则隔离）。
  - 缺点：写用户配置属于「侵入式」；需要幂等、可回滚、`kwinrulesrc` 损坏时能恢复；需要处理用户已手改规则的情况。
  - **必做**：写入前备份、按组 id 前缀识别我们的规则、卸载时精确删除、失败即降级并提示「请手动启用 KDE 集成」。
- **B（进阶，`P1` spike）`zwlr_layer_shell_v1` Background 层**：给壁纸窗口在 **mapped 之前**接上 layer surface（layer=background、anchor 四边、size=0、keyboard-interactivity=none、独占区=0），复用 `blur.rs:130-140` 已有的 `gdk_wayland_display_get_wl_display` / `gdk_wayland_window_get_wl_surface` FFI 取连接与 surface 的套路。
  - **role 问题的真实形态（已核实，与最初设想不同）**：Wayland 的 surface role 一旦设定**永久不可更换**（wayland.xml 原文），KWin 在已有 role 的 surface 上创建 layer surface 会 `wl_resource_post_error(..., error_role=0, "the wl_surface already has a role assigned %s")`（`layershell_v1.cpp:120-124`）→ 协议错误**由客户端自己断连**（不是 KWin 崩）。
  - 但这不是死路：**`gtk-layer-shell` 的既有机制就是 override `GtkWindow` 的 realize 信号**，在 GDK 造出 `xdg_toplevel` 之前改插 custom shell surface；只要在窗口 mapped 之前调用初始化就撞不到 role，layer-shell 不可用时它自己降级回 xdg 并 warn。
  - 因此 spike 要验的**不是「role 能不能抢」**，而是三件事：(1) Tauri/tao 的窗口生命周期能否让我们在 map 前插入该初始化；(2) WebKitGTK 在 layer surface 上的合成是否正常；(3) 尺寸——layer surface 上 `set_default_size` **无效**，必须 `set_size_request(...)` 或四边 anchor（已有 WebKitGTK + gtk-layer-shell 的公开先例：窗口不显示就是尺寸问题）。
  - 备选实现：不碰 GTK 的 surface，另跑一个轻量 Wayland 客户端把 WebKitGTK 的帧传过去（成本高，归入 `P9`）。
  - 已知上游坑（接受）：`ON_DEMAND` 键盘交互抢焦点 [518060](https://bugs.kde.org/show_bug.cgi?id=518060)、unmap 后不再 configure [503121](https://bugs.kde.org/show_bug.cgi?id=503121)、同时改尺寸与 anchor 误绘一帧 [526999](https://bugs.kde.org/show_bug.cgi?id=526999)。
- **B 若可行**，`P2` 的窗口规则降级为兜底（layer-shell 失败的合成器/旧 Plasma）。

**桌面图标交互的语义统一**

现状 macOS 是「鼠标事件由宿主轮询注入」，Linux 走 `set_ignore_cursor_events`（`wallpaper/linux.rs:226`）。KDE 下要区分两档，并在 UI 上如实说明：

- 「壁纸交互（图标上方）」开：壁纸窗口接收事件（`ignore_cursor_events(false)`），图标点不到（既有语义，与 macOS 一致）；
- 关（默认）：鼠标穿透，图标可点、壁纸的视差/hover 依赖指针注入（X11 可读全局光标 `wallpaper/linux.rs:446-461`，**Wayland 只有光标在本应用窗口上才有值** → 视差在 KDE Wayland 下会弱化，需在设置页标注）。
- **第三档（KDE 新增，对应 §4.1 的机制 ②）**：「隐藏系统桌面图标」。用户接受后我们关闭 plasmashell 桌面容器，壁纸成为唯一底层，**图标问题从此不存在**，也不再需要指针注入来「够到」图标。设置页三选一必须互斥且可逆。

### 4.2 第二层：会话探测与能力路由（`P0`，其他工作包的前置）

新增 `src-tauri/src/kde.rs`（或 `platform/kde.rs`），对外只暴露一个能力结构：

```rust
pub struct KdeEnv {
    pub is_kde: bool,
    pub plasma_major: Option<u32>,   // KDE_SESSION_VERSION: 5 / 6
    pub session: SessionKind,        // X11 | Wayland | Unknown
    pub kwin_on_bus: bool,           // org.kde.KWin 存在性（D-Bus 二次确认）
    pub scope: KdeScope,             // Desktop | Backend | None
}
pub fn detect() -> KdeEnv;           // 纯函数入口，输入是 env 快照，便于单测
```

判定链（约束文档 §11）：`XDG_CURRENT_DESKTOP` 拆分含 `kde`（或 `KDE_FULL_SESSION=true`）→ `KDE_SESSION_VERSION` 定 5/6 → `XDG_SESSION_TYPE`/`WAYLAND_DISPLAY` 定会话 → `org.kde.KWin` 存在性确认 KWin 真在跑。**禁止**写成 `XDG_CURRENT_DESKTOP == "KDE"`（历史值 `KDE:Plasma` 也要认）。

所有 KDE 分支都必须经过这里：`linux.rs:269-281 shell_icons_are_overlay()` 现在是自己拼的一套判断，应改为读能力结构（并保持对 GNOME/Budgie/Pantheon 的既有行为不变）。

### 4.3 第三层：D-Bus 能力（`P5`，用 KWin 替换现在的空实现）

新增 `src-tauri/src/kwin.rs`（zbus 直连会话总线；`zbus` 已在依赖图内，建议在 `src-tauri/Cargo.toml` 显式声明同版本，避免依赖 media-bridge 的传递版本）。承接四件事：

| 现有空实现 | KWin/Plasma 侧数据源 | 收益 |
| --- | --- | --- |
| `wallpaper/linux.rs:335-343 occlusion_snapshot()`（Wayland 恒 None） | `org.kde.KWin` 的窗口信息接口（`queryWindowInfo`/`getWindowInfo`），或 `org.kde.KWin.VirtualDesktopManager` + 活动窗口几何；退路 `kscreen-doctor` + 自有窗口枚举 | **KDE Wayland 下自动暂停复活**（当前完全失效） |
| `wallpaper/linux.rs:195 display_asleep()` 恒 false | `ext-idle-notify-v1`（KWin 已实现，可订阅 `idled`/`resumed`）或 logind `IdleHint` | 「熄屏暂停壁纸」在 KDE 成立 |
| `wallpaper/linux.rs:149-193 query_monitors()`（GDK 枚举） | `kscreen-doctor json`（**其与 `~/.config/kwinoutputconfig.json` 的等价性未查证**）；`org.kde.KScreen` 的 KDED 模块作补充——**Plasma 5 起一直有，6.8 起移除** | 每屏几何/缩放/主屏判定权威化，热插拔更准 |
| `frontmost_kind()` 的 Wayland 分支恒 `Unknown`（X11 已在 `P5` 补齐：读 `_NET_ACTIVE_WINDOW` 的 pid/type 分类） | KWin 脚本 API 的 `workspace.activeClient`（需注入脚本，非稳定接口） | 自动暂停「前台是全屏视频/游戏」判据更细 |

统一原则：**D-Bus 调用全部异步 + 超时（≤300ms）+ 失败降级到现状**，绝不因为 D-Bus 不在而阻塞壁纸主循环。

### 4.4 第四层：应用壳层（`P7`）

1. **全局快捷键**：X11 保持 `tauri-plugin-global-shortcut`（可工作，但要处理与 KDE 默认键冲突：注册失败目前只返回 Err 字符串，应在快捷键页明示「已被 KDE 占用」）。**Wayland 必须走 portal `org.freedesktop.portal.GlobalShortcuts`**（Plasma ≥5.27 有后端）：
   - 在插件/自研模块里实现 `CreateSession` → `BindShortcuts` → `ListShortcuts` 流程，拿回 `shortcut id` 映射到现有 7 个动作（`hotkeys.rs:30-38` 的 `ACTIONS`）；
   - 语义差异要在 UI 说清：**键位由用户在系统对话框确认、可能未绑定**，所以「快捷键页」在 Wayland 下要显示「由系统管理」并给出跳转/重新绑定的入口；
   - 门户外再 XGrabKey 在 Wayland 无效，不要做双写。
2. **托盘**：保持 SNI（Plasma 原生），补两件事：给托盘同时提供 `IconPixmap` 位图兜底，避免被 **plasmashell 的 KIconEngine** 按当前配色做 symbolic 重着色而看不清（**该点属代码推断，需在 Plasma 6 上肉眼验证**；`IconThemePath` 相关问题已在 Plasma **6.0.1** 修复，见 [bug 479712](https://bugs.kde.org/show_bug.cgi?id=479712)，所以「应用名/图标解析」不是问题）；以及**在 deb 里显式声明 SNI 运行库** `libayatana-appindicator3-1`（见 §2.3，现在 `deb.depends` 里没有）。不移除 ayatana 依赖。
3. **无边框窗口体验**：KWin 下阴影/圆角由 decoration 提供，`noBorder()`（= `decorationPolicy()==None`）时不画（约束文档 §12）。⚠️「边缘 resize 完全没有」**未能逐行核实**（KWin 的 resize 抓手来自 decoration，按此推论没有，但仍列入 spike 实测）→
   - 自绘 1px 描边与圆角（前端已有 `src/lib/platform.ts:39-60 useWindowRounded`）；
   - 补**边缘命中缩放**（GTK 侧 `begin_resize_drag`，前端 8 向热区），并保留 `Meta+左键移动 / Meta+右键缩放` 的 KWin 原生兜底说明；
   - 双击 header 最大化（自绘，KWin 的 `DoubleClickBorderToMaximize` 只管边框区）。
4. **分数缩放**：把 `query_monitors()` 的缩放来源从 `available_monitors().scale_factor()` 扩展为「Wayland 下优先 `wp_fractional_scale_v1` 的 `preferred_scale/120`」，物理缓冲按之计算（约束文档 §9）；同时补 `GDK_SCALE`/`GDK_DPI_SCALE` 的强制值与日志（现在完全没有），避免用户手工缩放时壁纸与主窗口不一致。
5. **静态壁纸同步**：KDE 分支改为多级：
   `org.freedesktop.impl.portal.Wallpaper`（Plasma 6.5.0 起，见 [bug 485966](https://bugs.kde.org/show_bug.cgi?id=485966)）→ `plasma-apply-wallpaperimage` → `qdbus evaluateScript` → **再不行回退 swww/feh 之外也回退「跳过并只记日志」**（现在两条 KDE 路径失败直接 `return` 错误，不回退）。
   注意 portal 的语义是「设一张图片」，正好符合「引擎未运行时保持视觉一致」的用途；`org.kde.PlasmaShell` 整体是上游 **Internal API**，用它只作最后兜底。
6. **主窗口模糊协议迁移（`P7` 负责，`src-tauri/src/blur.rs`）**：`org_kde_kwin_blur` 已被 KWin master 移除（6.7/6.8 之间，见 §3 第 6 条），需在 Wayland 路径加 `ext_background_effect_manager_v1`（`blur.rs:99-173` 那段结构可直接复用：拿 `wl_display`/`wl_surface` → 创建背景效果对象 → 设置 region → commit）。**不改变既有降级策略**：X11 的 `_KDE_NET_WM_BLUR_BEHIND_REGION` 原子保留（:63-92），模糊始终按「可选装饰」处理——用户关掉 KWin 模糊效果（`BlurEffect::enabledByDefault()` 只在非软件渲染时为 true，用户可关）或协议不可用时，主窗口必须仍然完全可用。**验收**：Plasma 6.6 与 6.8+ 各跑一次，前者走旧协议、后者走新协议，日志能区分「已启用/协议不可用/用户已关闭」三种结果。
7. **自启动**：`~/.config/autostart` 已覆盖 KDE；补「会话恢复」健壮性——KDE 会话重登后壁纸窗口需要能被 `wallpaper/mod.rs` 的监控自愈（现有 2s tick + 显示器变化重建逻辑，需在 KDE 下实测一遍）。

### 4.5 第五层：演进项（`P9`，不在本期）

**自研 Plasma 壁纸插件（waywallen 模式）**：KPackage `Plasma/Wallpaper` + 纯 QML 跑在 plasmashell 内，外部渲染器（WebKitGTK 进程）经 **DMA-BUF + acquire/release sync fence** 把帧零拷贝注入。这是 §4.1 机制 ③，也是唯一「壁纸与图标都被 plasmashell 原生管理、不需要任何窗口堆叠技巧」的解。
- 优点：桌面层、虚拟桌面/活动切换、与 plasmashell 生命周期都原生；上游已验证（waywallen-display）。
- 代价：需要在 Tauri 之外新增一个 Qt/QML 插件构建链；插件崩溃会带走 plasmashell；跨虚拟桌面不同壁纸仍不解决（Plasma 自身就没有）。
- 触发条件：`P1` 机制 ① 实测失败、且用户不接受机制 ② 的「隐藏桌面图标」时启动。

---

## 5. 工作包（可直接排期）

> 每个工作包 = 一个可独立验证的改动集，遵循仓库 `docs/pr-rules.md` 的「单个 PR 只做一件事」。写作用域互不重叠即可并行。

| 编号 | 标题 | 写作用域（主要文件） | 依赖 | 验收要点 |
| --- | --- | --- | --- | --- |
| **P0** | KDE 会话探测与能力路由 | `src-tauri/src/kde.rs`（新）、`src-tauri/src/lib.rs`、`wallpaper/linux.rs`（改 `shell_icons_are_overlay`） | — | 纯函数单测覆盖 `KDE`/`KDE:Plasma`/`KDE_FULL_SESSION`/Plasma5/6/X11/Wayland 组合；`kwin_on_bus` 走 D-Bus 存在性；无 KDE 环境行为零变化 |
| **P1** | **桌面层落位 spike**（决定主线） | `wallpaper/linux.rs`（特性开关后的实验路径）、`wallpaper/mod.rs`（重放点） | P0 | ①**X11 透明化 plasmashell 桌面背景**：`evaluateScript` 能否稳定拿到 desktop containment 的 `wallpaper` 对象并设 `visible=false`／换透明图；成功判据 = 壁纸可见 **且** 桌面图标浮在其上可点。②**Wayland layer-shell**：能否在窗口 **mapped 之前**接上 `gtk-layer-shell` 式 custom shell surface（Tauri 生命周期是否给得到这个点）、WebKitGTK 在 layer surface 上合成是否正常、`set_size_request`+四边 anchor 尺寸是否正确。两步都产出「成功/失败 + 错误码 + 版本范围」的书面结论 |
| **P2** | KWin 窗口规则 + 桌面容器开关 | `src-tauri/src/kwin.rs`（新，规则生成/写入/回滚）、`src-tauri/src/commands.rs`、设置页 UI | P0、P1 | Wayland：`Layer=Desktop`/`Skip*`/`Desktops`/`Activity`/`AcceptFocus=false`/`DecorationPolicy=None` 幂等写入 → `reconfigure` 后壁纸沉到桌面层；用户既有 `kwinrulesrc` 不被破坏；卸载「KDE 集成」后精确还原。X11（机制 ②）：提供「隐藏系统桌面图标」一键切换（写前备份、可逆），关闭后图标问题消失 |
| **P3** | KDE 会话冒烟与回归 | `scripts/linux-kde-smoke.sh`（新）、`scripts/linux-docker-entrypoint.sh`、`Dockerfile.linux`、`.github/workflows/build-linux.yml` | P0、P1 | 见 §6.2；KDE X11 与 KDE Wayland 两档都能让发布流程变红 |
| **P4** | 验收矩阵补全（含 Plasma 6.8） | `docs/kde-adaptation-plan.md`、`README.md` | P1、P3 | §6.3 全部真机跑通；**必须覆盖 Plasma 6.8+ X11**（该版本 plasmashell 已不再给自己的桌面窗口设 `NET::Desktop`，任何「靠找 desktop 窗口」的逻辑都要能优雅失败） |
| **P5** | KWin D-Bus 能力接入 | `src-tauri/src/kwin.rs`、`wallpaper/linux.rs`（`occlusion_snapshot`/`display_asleep`/`frontmost_kind`/`query_monitors`） | P0 | KDE Wayland 下自动暂停判定表出现 `Keep` 以外的状态；拔插显示器后壁纸几何正确；D-Bus 不可用时行为 == 现状 |
| **P6** | 缩放与静态壁纸同步 | `wallpaper/linux.rs`（`query_monitors`）、`system_wallpaper.rs` | P0 | 125%/150% 缩放下壁纸像素对齐；KDE 静态壁纸三级回退（portal → CLI → evaluateScript）各自成功路径可用 |
| **P7** | 快捷键、托盘、无边框与模糊协议 | `src-tauri/src/hotkeys.rs`、`src-tauri/src/lib.rs`（tray）、`src-tauri/src/blur.rs`、`src/components/WindowControls.tsx`、`src/lib/platform.ts`、`src-tauri/src/props_window.rs` | P0 | KDE Wayland 下快捷键可用或明确显示「未绑定/由系统管理」；托盘图标与名称正确；无边框窗口四边可缩放（实测后决定是否自绘）、双击 header 最大化；Wayland 模糊按 `ext_background_effect_manager_v1`/`org_kde_kwin_blur` 分版本走通，不可用时静默降级不报错 |
| **P8** | 打包与依赖声明 | `src-tauri/tauri.conf.json`、`.github/workflows/build-linux.yml`、`Dockerfile.linux` | — | deb 显式补 `libayatana-appindicator3-1`（托盘 SNI 运行库，见 §2.3）与 KDE 相关 `recommends`（`plasma-workspace`、`qdbus`/`qt6-tools`、`pulseaudio-utils`\|`pipewire-bin`、`kscreen`）；AppImage 文档说明外部命令要求 |
| **P9** | Plasma 壁纸插件（**战略路径**：`P1` 已证明窗口方案无法同时保住图标，这是唯一全满足解） | 新目录（KPackage `Plasma/Wallpaper` + QML）、`wallpaper/` 外挂帧传输 | P1 实测结论、P2 | 帧传输（DMA-BUF + sync fence）零拷贝；插件崩溃不拖垮 plasmashell 的隔离方案；跨虚拟桌面限制写入文档 |

**进展（2026-10-14，真机验证底座 = Plasma 5.27/X11 + `kwin 5.27.11`）**

| 工作包 | 状态 | 实际产出 / 证据 |
| --- | --- | --- |
| `P0` | ✅ 已落地 | `src-tauri/src/kde.rs`：`KdeEnv`/`KdeScope`/`env_snapshot`/`detect_with`/`env()`（`OnceLock` 缓存）+ 桌面容器判定（`parse_containments`/`desktop_containment`/`log_desktop_icons_status`）；17 个单测通过；非 KDE 行为零变化 |
| `P1` | ✅ **已定论：窗口方案在此路不通** | 四轮 spike（`p1b`/`p1c`/`p1d`/`p1e`）：透明化桌面背景在「无法沉底」的前提下无意义；四种沉底原语全部无效。结论已写入 §2.5 与 §4.1 |
| `P2` | 🟡 部分落地 | `src-tauri/src/kwin.rs` 规则文件层（合并/幂等/回滚/校验，13 单测）+ `org.kde.KWin reconfigure`。真机实测：KWin **5** 下规则键可写入但不影响层序（`layerrule` 长期 unused）⇒ 该部分价值在 **Plasma 6/Wayland**，不要再当 X11 的图标解药 |
| `P3` | ✅ 真机通过（Plasma 5.27/X11） | `scripts/linux-kde-smoke.sh`（真实总线 + `kwin_x11` + `plasmashell` + 5 条断言）在 VM 上 **5/5 PASS，连跑两次一致**，并自动复现「两个 desktop 窗口共存、我们在 plasmashell 桌面视图之上」；另产出 `scripts/kde-vm-session.sh`（`probes`/`smoke`/`shell`/`realbuscase`/`belowtest`/`ruleinstall`/`p1b`–`p1e`）、`scripts/kde-kwin-layer.sh`、CI `kde-smoke` job。CI 侧仍待真实 Actions 跑一次 |
| `P4` | 🟡 本轮更新 | 本文 §2.5/§4.1/§5 已回填真机结论；`README.md` 增加 KDE 小节（图标取舍 + 冒烟命令）；`CHANGELOG.md` Unreleased 记录用户可见变化；快捷键页显示后端注册状态与 KDE 接管提示 |
| `P5` | 🟡 大部分落地 | `kwin.rs`：KScreen 每屏解析、`idle_seconds`（D-Bus + 2 秒缓存）、`reconfigure`；`wallpaper/linux.rs`：`display_asleep` 接入 KWin 空闲、`frontmost_kind` 补齐 X11（EWMH `_NET_ACTIVE_WINDOW` → 桌面/面板 = 播放、自己 = 播放、别的应用 = 暂停，5 个单测；真机实测 KWin 下活动窗口就是 plasmashell 的 DESKTOP 窗，分类为 `Desktop` → 决策 `Play`，见报告 §6.4）。Wayland 侧两项均无解（无全局活动窗口/窗口清单） |
| `P6` | ✅ 已合入（commit `27d4ccf`） | KDE 静态壁纸三级回退（portal → `plasma-apply-wallpaperimage` → `qdbus evaluateScript`），9 个单测；非 KDE 分支逐字未动 |
| `P7` | 🟡 已落地（Wayland 侧未实测） | 快捷键：KDE Wayland 标记「由桌面接管」+ `HotkeyStatus` 可观测（10 单测），前端已显示状态与接管说明；托盘：Linux 改用彩色 `icons/32x32.png` + 自启补键（12 单测，`unwrap` 仅存在于 `cfg(test)`）；模糊：`ext_background_effect_manager_v1` 优先 + `org_kde_kwin_blur` 回退，X11 原子路径 sha256 逐字不变。三项都缺 Plasma 6.7+ 真机验证 |
| `P8` | 🟡 已交付待复核 | `deb.depends` 补 `libayatana-appindicator3-1`；`recommends` 收窄为 `gstreamer1.0-pipewire`/`ffmpeg`/`kscreen`/`pulseaudio-utils \| pipewire-bin`；`Dockerfile.linux`/entrypoint `SMOKE_MODE`；CI `kde-smoke` |
| `P9` | ✅ 真桌面验证通过（插件已独立成仓） | 本轮结论把它从「演进彩蛋」升级为**唯一全满足解**。插件包已迁到独立仓库 **`../WallpaperEM-Plasma`**（与主仓同级目录）：真机对照实验 18 项断言全过——在**真实 plasmashell**（Plasma 5.27.11 会话 `:0`）里用官方脚本接口把壁纸换成该插件后，帧画到桌面、**桌面图标与面板图层逐像素不变**、推进序号桌面即换色（记录见该仓 `docs/real-plasma-verification.md`，截图在 `docs/evidence/real-plasma/`；本仓报告 §6.5）；显示端另有 16 项 Xvfb+qmlscene 断言（该仓 `scripts/verify.sh`，本仓报告 §6.3）；`zwp_linux_dmabuf_v1` 本机实测 v4，零拷贝帧传输在协议层可行（§6.2）。**未做**：引擎侧帧传输（`frame_export`）、多屏按实例分帧、Plasma 6 实测 |

**推荐顺序**：`P0 → P1(spike，决定主线) → P5 → P6 → P7 → P8 → P2 → P3 → P4 → P9`。
说明：`P1` 是唯一能改变整体形态的实验，必须最先做——**本轮已做完并给出否定结论**，所以后续排期的重心应转向 `P9`（壁纸插件）与「② 让桌面不再是文件夹视图」的用户引导；`P2` 的规则文件层留给 Plasma 6/Wayland 验证。

---

## 6. 验收与测试

### 6.1 单测（随 `P0`/`P1` 落地，纯函数、可跨平台跑）

- `kde.rs::detect` 的环境矩阵：`XDG_CURRENT_DESKTOP` ∈ {`KDE`、`KDE:Plasma`、`GNOME`、`XFCE`、空} × `KDE_SESSION_VERSION` ∈ {`5`,`6`,无} × `XDG_SESSION_TYPE` ∈ {`x11`,`wayland`} × `WAYLAND_DISPLAY` 有无 → 期望的能力结构。注意用注入的 env 快照而不是 `std::env::set_var`（避免测试并发污染）。
- 窗口规则生成：给定应用标识/桌面 id，生成 `kwinrulesrc` 规则块；断言字段（`Layer`(Force) / `SkipTaskbar`/`SkipPager`/`SkipSwitcher` / `Desktops`(Set) / `Activity`(Set) / `DecorationPolicy` / `AcceptFocus=false`，**不要写 `FocusPolicy`、不要写单数 `Opacity`**）与注释锚点；断言与既有内容合并时**不覆盖用户规则**。
- 静态壁纸回退链：给定「portal 不可用 / CLI 不存在 / qdbus 失败」矩阵，断言最终行为与错误信息（把 `system_wallpaper.rs:624-643` 的两条 `return` 改成可测的链）。
- 桌面容器开关（机制 ②）：`plasma-org.kde.plasma.desktop-appletsrc` 的读改写纯函数——断言写前备份、只改目标 key、失败可还原。

### 6.2 KDE 会话冒烟（CI）

在 Linux Docker（`Dockerfile.linux`）里加两个档位，用 `SMOKE_MODE=all` 选择：

1. **KDE/X11 档**：装 `plasma-workspace` + `kwin-x11` + `xdotool`/`x11-utils`，`dbus-run-session -- xvfb-run -a` 启动 `kwin_x11` + `plasmashell`，设 `XDG_CURRENT_DESKTOP=KDE`、`XDG_SESSION_TYPE=x11`，起应用，**断言**：
   - 日志出现 `wallpaper window … created` 与「桌面层/几何已应用」；
   - 用 `xdotool`/`xwininfo` 校验壁纸窗口的 `_NET_WM_WINDOW_TYPE_DESKTOP` 已设，且在 stacking order 中位于**普通窗口之下**（这是唯一能自动断言的硬指标；「图标是否可见」只能靠截图人工判断或像素比对）；
   - 若 `P1` 机制 ① 落地：追加「plasmashell 桌面窗口的截图在壁纸区域为透明/非不透明背景」的像素断言；
   - 进程存活 30s 无 panic。
2. **KDE/Wayland 档**：装 Plasma 6 的 `kwin-wayland` + `grim`，`dbus-run-session -- kwin_wayland --virtual --no-lockscreen --no-xwayland`（nested，无需 GPU），设 `XDG_SESSION_TYPE=wayland`，起应用，**断言**：
   - 应用能创建窗口并渲染（日志 + `grim` 截图非全黑）；
   - 窗口规则写入 `kwinrulesrc` 且 `reconfigure` 返回成功；
   - KWin D-Bus 调用成功（`P5` 后）；
   - 进程存活 30s 无 panic。
3. **回归线**：现有 Xvfb 无 KDE 档（GNOME 语义）必须继续通过——新的实现不得影响 GNOME(DING) 的既有下沉路径（`wallpaper/linux.rs:239-262`）。

> 说明：`kwin_wayland --virtual` 的可用性与需要的包名需在 `P3` 实现时按 Ubuntu 22.04（发布基线）与 24.04（本地 Docker）分别确认；若 22.04 的 Plasma 版本不支持该选项，则 Wayland 档只在 Docker(24.04)/nightly 作业跑，不阻塞 tag 发布。
>
> ⚠️ **实测修正（2026-10-14）**：Ubuntu 22.04/24.04 的官方仓库都只有 **Plasma 5.27**，**Wayland 档的 `zwlr_layer_shell_v1`/模糊新协议断言在这两个底座上根本无法成立**（5.27 的 Wayland 会话里桌面类型 hint 被忽略，layer-shell 路径也不适用）。要真正验 Wayland 档，底座必须换到有 Plasma 6 的发行版（Ubuntu 25.04+、Fedora KDE、openSUSE），详见 [kde-verification-report.md](kde-verification-report.md)。
>
> 不过在 5.27 上仍做了一轮**协议可用性实测**（嵌套 `kwin_wayland --virtual` + `wayland-info`，见报告 §6.2）：`zwlr_layer_shell_v1` **v3** 已存在（上游 Plasma 6 是 v5，版本随发行版递进）、`zwp_linux_dmabuf_v1` **v4**（P9 零拷贝帧传输在协议层可行）、`org_kde_kwin_blur_manager` **v1** 仍在（`blur.rs` 回退分支在本机就是主路径）、`ext_background_effect_manager_v1` **未广告**（符合「Plasma 6.7+ 才有」的判断）、`org_kde_kwin_idle`/`ext_idle_notifier_v1` 均在（Wayland 空闲源可选）。复现：`bash scripts/kde-wayland-globals.sh`。
>
> ⚠️ **实测修正（会话总线）**：第 1 档里的 `dbus-run-session` **只适用于无用户会话的容器**。真机（有 systemd 用户会话）上必须挂在真实总线 `/run/user/$(id -u)/bus`，否则 `kglobalaccel`/`kactivitymanagerd`/`KScreen` 会被信号 6 打死、`plasmashell` 直接 `Aborting shell load`——用私有总线会得到「KDE 全是坏的」这一假结论。真机入口用 `scripts/kde-vm-session.sh`。
>
> **已就绪的脚本（本方案一并产出，见 §8）**：
> - `scripts/kde-verify-preflight.sh` —— 只读环境预检，回答「这台机器能验证哪几条」，并在缺 `xvfb`/无 `sudo`/非 Plasma 6 时明确报 `[BLOCK]`/`[WARN]`；
> - `scripts/kde-probes.sh` —— **不需要应用二进制**，直接在活会话里实测 §7 的开放问题（`evaluateScript` 能不能拿到 desktop containment 的 `wallpaper` 对象、机制② 的配置键、KWin 规则/D-Bus 面、`kscreen-doctor`、空闲源、SNI、portal 快捷键），输出 `PROBE <名> = <值>` 便于回填本文 §7；
> - `scripts/kde-vm-session.sh` —— **真机核心入口**：在真实用户总线上拉起 `Xvfb` + `kwin_x11` + `plasmashell`，提供 `probes` / `realbuscase`（应用冒烟 + 像素取证）/ `belowtest`（置底可行性对照）/ `smoke` / `shell` 五种模式；`Xvfb`/`kwin_x11` 用 `setsid` 脱离 SSH 会话，避免 SIGHUP 带走；
> - `scripts/linux-kde-smoke.sh` —— 上面第 1 档的可执行版本：起 `Xvfb` + `kwin_x11`（可选 `plasmashell`），**显式设 `XDG_CURRENT_DESKTOP=KDE`**（现有 Docker 冒烟漏了这一步，等于没测 KDE），断言 `_NET_WM_WINDOW_TYPE` 含 `_NET_WM_WINDOW_TYPE_DESKTOP`、该窗口在 `_NET_CLIENT_LIST_STACKING` 中位于普通窗口之下、日志有创建/应用记录、进程存活无 panic；每步 `[PASS]/[FAIL]` 计数并据此返回退出码。真机走**真实用户总线**（`/run/user/$(id -u)/bus`），容器/CI 无用户会话时自动退回 `dbus-run-session` 并跳过 `plasmashell`。**已在 Plasma 5.27/X11 VM 上 5/5 通过（连跑两次一致）**，逐条结果与两处被修掉的缺陷（`xprop` 空属性也返回 0 导致 kwin 无宽限期；只等 `xdpyinfo` 会漏掉 Qt 需要的文件 socket）见 [kde-verification-report.md](kde-verification-report.md) §6.1；
> - `scripts/kde-vm-setup.sh` / `kde-vm-build.sh` / `kde-vm-sync.sh` / `kde-vm-verify-state.sh` —— 真机装配、构建、同步、故障后体检（ext4 日志中止那次事故的产物）。

### 6.3 手工验收矩阵（发版前，真机）

> 新增 **Plasma 6.8+ X11** 列：该版本 plasmashell 已不再给桌面窗口设 `NET::Desktop`，任何「查找 desktop 窗口」的实现都要在这里验证为「优雅失败」而非崩溃/静默错位。

| 场景 | Plasma 5.27 X11 | Plasma 6 X11 | Plasma 6.8+ X11 | Plasma 6 Wayland |
| --- | --- | --- | --- | --- |
| 壁纸铺满 + 桌面图标诉求（按 `P1` 结论选定的机制） | ✅ 必测 | ✅ 必测 | ✅ 必测 | ✅ 必测 |
| 面板自动隐藏/显示、任务栏 | 必测 | 必测 | 必测 | 必测 |
| 虚拟桌面切换、Activities 切换 | 必测 | 必测 | 必测 | 必测 |
| 多屏（不同分辨率/主屏切换） | 必测 | 必测 | 必测 | 必测 |
| 分数缩放 125%/150% | — | 必测 | 必测 | 必测 |
| 熄屏/锁屏暂停与恢复 | 必测 | 必测 | 必测 | 必测 |
| 全屏视频/游戏时自动暂停 | 必测 | 必测 | 必测 | 必测 |
| 托盘、快捷键、自启动 | 必测 | 必测 | 必测 | 必测 |
| 主窗口拖动/缩放/最大化/圆角 | 必测 | 必测 | 必测 | 必测 |
| 系统静态壁纸同步（锁屏/登录） | 必测 | 必测 | 必测 | 必测 |
| 音频可视化、「正在播放」 | 必测 | 必测 | 必测 | 必测 |
| 卸载「KDE 集成」后配置还原 | 必测 | 必测 | 必测 | 必测 |

---

## 7. 风险与未验证项

| 风险 | 影响 | 处置 |
| --- | --- | --- |
| **❶ plasmashell 桌面背景无法按预期透明化**（`evaluateScript` 拿不到 desktop containment 的 `wallpaper` 对象；`org.kde.PlasmaShell` 是 Internal API） | 机制 ① 作废 → 「壁纸 + 图标都要」在 X11/Wayland 都无法达成 | `P1` 先 spike；作废则走机制 ②（关闭桌面容器，明确告知用户代价）或 `P9`（壁纸插件）；**不把「图标浮在壁纸上」写进对外承诺** |
| **❷ layer-shell 接不进 Tauri 窗口生命周期**（map 时机、WebKitGTK 合成、尺寸） | Wayland 的 layer-shell **进阶路径**（§4.1 的 B）作废 | `P1` spike；作废则 `P2` 窗口规则就是长期方案（已验证 `Layer` 规则能覆盖 `belongsToLayer()`） |
| **❸ 两个桌面窗口共存的副作用**（KWin `findDesktop()` 会挑中最上、`wantsInput()` 的 desktop 窗口；调用点含 `activation.cpp`、`workspace.cpp` 的「点空桌面得焦点」「显示桌面」） | 点击桌面焦点异常/显示桌面行为异常 | 让壁纸窗口**不取焦点**（`WM_HINTS input=False` / 规则 `AcceptFocus=false`）可让 `findDesktop()` 跳过它；验收覆盖「点空桌面」「显示桌面」 |
| **❹ Plasma 6.8+ 不再给桌面窗口设 `NET::Desktop`** | 任何「靠找 desktop 窗口」的逻辑失效 | 不使用该假设；实现按能力探测而非版本硬编码；`P4` 覆盖 6.8+ 列 |
| **❺ 写 `~/.config/kwinrulesrc` / 桌面容器配置被用户视为侵入** | 用户投诉、配置被改坏 | 幂等 + 精确回滚 + 写前备份 + 开关默认「询问后启用」；不覆盖用户规则；只改目标 key |
| **❻ `org.kde.KScreen` 在 Plasma 6.8 移除** | 每屏查询失效 | 主用 `kscreen-doctor json`（其与 `kwinoutputconfig.json` 的等价性**未查证**，实现时先验），KScreen D-Bus 仅作版本化补充 |
| **❼ 模糊协议换代**（master 已移除 `org_kde_kwin_blur`，精确版本为 6.7/6.8 之间，**待 tag 复核**） | 该版本之后 Plasma 主窗口无模糊 | 由 `P7` 承担（见 §4.4.6，`blur.rs` 已列入其写作用域）：加 `ext_background_effect_manager_v1`；X11 原子保留；全部按「可选装饰」降级 |
| **❽ Wayland 视差/调试弱化**（读不到全局光标） | 视差类壁纸效果打折 | 在设置页如实标注；后续可用 KWin `org_kde_kwin_fake_input` 注入指针位置解决，属独立项 |
| **❾ Wayland 快捷键键位由系统决定** | 用户找不到自定义入口 | UI 明示「由系统管理」+ 跳转系统设置的入口 |
| **❿ 构建底座 22.04 与本地 24.04 不一致** | CI 冒烟与发布包行为差异 | `P3` 优先在 22.04 基线镜像里跑 KDE 档；24.04 作为本地快速回路 |
| **⓫ `pw-record` monitor 参数语法未查证** | KDE/PipeWire-only 环境音频可视化失败 | 运行时用 `pactl list short sources` / `pw-cli ls Source` 选源，不写死参数；设置页给出「装 pulseaudio-utils 或 pipewire-bin」提示 |
| **⓬ README 自相矛盾**（`:410` 称自动暂停未实现，`:220` 又列为已实现；Rust 侧确有实现） | 文档误导 | 归 `P4`（其写作用域含 `README.md`；`P3` 只负责把冒烟命令写进 README 的 Linux 测试段）：以 `:220` 为准改掉 `:410`，并补 KDE 支持矩阵 |

**必须在真机上才能确认的开放问题**（不要在设计里假想答案）

> 标 ✅ 的已在 [kde-verification-report.md](kde-verification-report.md) 里用 Plasma 5.27/X11 实测回答；标 ⛔ 的属 Plasma 6 专属，本机无法验证。

1. ⛔ `evaluateScript` 能否稳定拿到 desktop containment 的 `wallpaper` 对象并透明化；`plasma-apply-wallpaperimage` 换透明图是否可行。（前半已实测：`evaluateScript` **返回空字符串**，`print` 也不落盘 → 取证手段本身要先解决；透明化可行性仍未证。）
2. ⛔ Tauri/tao 的窗口生命周期能否在 **map 之前**插入 gtk-layer-shell 式 custom shell surface；WebKitGTK 在 layer surface 上的合成与尺寸行为。（需 Plasma 6 Wayland。）
3. ⚠️ 关闭 plasmashell 桌面容器（机制 ②）在各 Plasma 版本的确切配置键与副作用。已实测本机键名：`plugin=org.kde.plasma.folder`、`formfactor=0`、`wallpaperplugin=org.kde.image`（含 `activityId`）；**副作用仍未验**。
4. ⛔ KWin 对 `decorations(false)` 的 GTK 窗口是否真的没有边缘 resize（阴影/圆角按 decoration 逻辑推断为无）。无头 Xvfb 无法模拟 `Meta+左键`。
5. ⛔ Plasma 6 托盘对 `default_window_icon()` 图形的重着色结果（是否需要 `IconPixmap` 兜底）。另注：Plasma 5.27 实测 `org.kde.StatusNotifierWatcher` **absent**，托盘在 5.27 上属额外依赖。
6. ✅ `kwin_wayland --virtual` 在 Plasma 5.27（Ubuntu 24.04 noble）上**可用**：`set --virtual` 在 `kwin_wayland 5.27.11` 里被接受，嵌套合成器约 1s 建好 socket，`wayland-info` 能连上取到全局清单（报告 §6.2）。
7. ✅ Tauri 是否把 identifier 设为 GTK 的 `WM_CLASS`？→ 实测壁纸窗口 `WM_CLASS(STRING) = "WallpaperEM", "WallpaperEM"`（**不是** identifier `io.github.oneincase.wallpaperem`）。写 KWin 窗口规则时按 `WM_CLASS=WallpaperEM` 匹配，不要按 identifier。
8. ✅（新增，结论为否定）能否用 `_NET_WM_STATE_BELOW` 把壁纸窗口压到 plasmashell 桌面视图之下？→ **不能**，三组对照实测层序均为 `2/4`，见报告 §4。不要把它当候选方案。
9. ⛔（新增）Plasma 6 Wayland 下 `zwlr_layer_shell_v1` 的 `Background` 层是否真能同时保住「壁纸可见 + 图标可见」。这是 X11 上已被证伪的那条路的唯一替代，必须优先在 Plasma 6 机器上验证。（已测到的只是前置条件：KWin 5.27 广告 `zwlr_layer_shell_v1` **v3**，Plasma 6 上游为 v5；层内先后关系仍未证。）
10. ✅（已解，2026-10-14）P9 插件在**真 plasmashell** 里被选为壁纸后的表现 → 已在真实会话（Plasma 5.27.11，`:0`）上用官方脚本接口实测：帧画到桌面、桌面图标与面板图层**逐像素不变**、序号推进即换色（该仓 `scripts/plasma-real-test.sh` 18 项全过、`docs/real-plasma-verification.md`）。仍未量化的只是**长期帧率与合成开销**（1080p@30fps 的 CPU/GPU 占用、以及插件异常时对 plasmashell 的影响面），需要一段时间的真实使用观察。

---

## 8. 文档与交付物

随各工作包一并产出：

- 本文（方案）、[kde-plasma-constraints.md](kde-plasma-constraints.md)（上游约束调研，含全部链接）与 [kde-verification-report.md](kde-verification-report.md)（Plasma 5.27/X11 真机实测：机制①落点、`BELOW` 证伪、会话总线要求、`kscreen-doctor`/`ScreenSaver`/SNI 实测）；
- [kde-smoke-setup.md](kde-smoke-setup.md)（真机验证底座的装配与**覆盖率边界**：这台 Ubuntu 24.04 arm64 验证机只能覆盖 Plasma 5.27/X11，Plasma 6 需换底座）；
- 脚本：`scripts/kde-verify-preflight.sh`（环境预检）、`scripts/kde-probes.sh`（机制实测探针）、`scripts/kde-vm-session.sh`（**真机核心**：真实用户总线上拉起 Xvfb+KWin+plasmashell，含 `probes`/`realbuscase`/`belowtest` 模式）、`scripts/linux-kde-smoke.sh`（KDE/X11 冒烟断言，CI 用）、`scripts/kde-wayland-globals.sh`（KWin 的 Wayland 全局清单探测：layer-shell/dmabuf/idle/模糊协议版本）、`scripts/kde-plugin-check.sh`（**转发入口**：P9 插件包与其验证脚本已迁到独立仓库 `../WallpaperEM-Plasma`，本脚本转发到该仓的 `scripts/verify.sh`，`--real` 转发到 `scripts/plasma-real-test.sh`）、`scripts/kde-vm-setup.sh` / `kde-vm-build.sh` / `kde-vm-sync.sh` / `kde-vm-verify-state.sh`（真机装配、构建、同步、故障后体检）；
- **独立仓库 `../WallpaperEM-Plasma`（P9 插件的家）**：Plasma 壁纸插件包本体（`metadata.json` + KConfigXT `contents/config/main.xml` + `main.qml`/`config.qml`）、引擎↔插件帧契约 `docs/engine-contract.md`、帧泵参考实现 `tools/frame-pump.py`、验证脚本 `scripts/verify.sh`（16 项 Xvfb+qmlscene）与 `scripts/plasma-real-test.sh`（18 项真桌面）、真桌面验证记录与截图证据；
- `packaging/plasma-wallpaper/io.github.oneincase.wallpaperem/`（**P9 早期骨架，已被独立仓库取代**，仅作历史留存，勿在此继续开发）；
- `README.md` 的 Linux 段重写：KDE 支持矩阵（Plasma 5/6 × X11/Wayland × 功能）、外部命令依赖（`plasma-apply-wallpaperimage`/`qdbus`/`kscreen-doctor`/`parec|pw-record`/`pactl|wpctl`）、Wayland 下的已知限制；**并修掉一处自相矛盾**：`README.md:410` 说「自动暂停尚未实现」，而 `README.md:220` 的功能列表把它列为已实现（Rust 侧确有实现，`README.md:410` 是过时表述，改它而不是改 :220）；
- `CHANGELOG.md` 按仓库实际惯例追加到 `## [Unreleased]` 的中文分类小节（`### 🛠 改变 / Changed` 等），条目形如 `- **短标题**：什么条件下什么行为变了`（本仓不用 `type(scope):` 前缀，全文 0 例）；
- `docs/release-notes/` 在**发布时按实际版本号新建**（`<tag>.md`，见 `scripts/release-finish.mjs:39-41`）的文件里增加一节「KDE Plasma 支持（X11 / Wayland）」，写明 ②/③ 二选一的用户可见后果（是否失去系统桌面图标）。**本轮已先把用户可见变化写进 `CHANGELOG.md` 的 `[Unreleased]`**（仓库惯例），到发版时才落 `<tag>.md`，避免现在猜版本号。
