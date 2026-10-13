# KDE Plasma 5/6 适配约束调研（WallpaperEM / Tauri 2 壁纸引擎）

调研方式：直接读 KDE 上游源码（invent.kde.org，master 与分支快照，clone 到本地 + GitLab API 取特定版本文件）与 KDE Bugzilla / 上游 issue。所有结论都附源码行号或链接；查不到的写「未能查证」。
版本口径：`master` = 6.8 开发版（kwin/kscreen/kglobalacceld 的 `PROJECT_VERSION` 均为 6.8.80）；分支快照按 `Plasma/6.4 … 6.8` 区分。文中「Plasma 6.7 起」等说法来自「该提交已并入哪些分支」的核对。

---

## 1. 桌面层窗口：X11 `_NET_WM_WINDOW_TYPE_DESKTOP`

- Plasma 自己的 X11 做法（可照抄的最小组合）：`setFlags(Qt::Window | Qt::FramelessWindowHint)`，然后在 `ensureWindowType()` 里 `KWindowSystem::setType(winId(), NET::Desktop); KWindowSystem::setState(winId(), NET::KeepBelow);` —— [plasma-workspace Plasma/5.27 shell/desktopview.cpp#L57-L59, #L218-L221](https://invent.kde.org/plasma/plasma-workspace/-/blob/Plasma/5.27/shell/desktopview.cpp#L218)（同一文件还有 `WindowedDesktop` 变体：`NET::Normal` + 清掉 FullScreen，用于「把桌面做成普通窗口」模式）。
- KWin 侧语义：`isDesktop()` 就是 `windowType() == WindowType::Desktop`（[kwin src/window.h#L2178](https://invent.kde.org/plasma/kwin/-/blob/master/src/window.h#L2178)）；`Window::belongsToLayer()` 中 `isDesktop() → DesktopLayer`；全层级自下而上为 `DesktopLayer, BelowLayer, NormalLayer, DockLayer, AboveLayer, NotificationLayer`（[src/layers.cpp#L26-L31](https://invent.kde.org/plasma/kwin/-/blob/master/src/layers.cpp#L26)）。注意「keep-below 的普通窗口」落在 BelowLayer，**在 DesktopLayer 之上**——想贴桌面就不能用 keep-below 冒充。
- KWin 不管桌面窗口的摆放：`placement.cpp` 里 `if (isDesktop()) { /* KWin doesn't manage desktop windows */ placementDone = true; }`（[src/placement.cpp#L500](https://invent.kde.org/plasma/kwin/-/blob/master/src/placement.cpp#L500)）→ 尺寸/位置、多屏铺满全部要自己算。
- 「所有虚拟桌面」在 X11 是内建的：desktop 类型窗口初始桌面集合为空（= 所有桌面），见 [src/x11window.cpp#L708-L717](https://invent.kde.org/plasma/kwin/-/blob/master/src/x11window.cpp#L708)。
- 「谁是桌面」只认一个：`Workspace::findDesktop()` 从 stacking order 顶部往下找第一个满足 `isDesktop() && isShown() && wantsInput()` 的窗口（[src/layers.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/layers.cpp)）→ 第三方再插一个 desktop 窗口后，KWin 的「显示桌面/最小化所有」等语义可能指向你的窗口而不是 plasmashell 的。
- Plasma 6 的「desktop 类型」不只是 X11 属性：KWin 现在把 namespace 为 `desktop` 的 layer-shell 窗口也当桌面窗口处理，见 bug [525792](https://bugs.kde.org/show_bug.cgi?id=525792)（kwin/core，RESOLVED FIXED，2026-09-15）。
- 真实冲突案例：linux-wallpaperengine 在 Plasma 6.3.4 Wayland 下「桌面图标消失、桌面右键失效」，见 [Almamu/linux-wallpaperengine#370](https://github.com/Almamu/linux-wallpaperengine/issues/370)。这是「第三方桌面窗口 + plasmashell 桌面窗口/图标层」共存的直接后果。
- 能否让 plasmashell 的桌面背景透明/关掉：**未能查证**任何官方支持的开关（Plasma 6 的 DesktopView 始终创建桌面 containment 窗口，见 [master shell/desktopview.cpp](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/shell/desktopview.cpp)，Wayland 分支走 LayerShellQt `LayerBackground` + scope `desktop`）。绕开冲突的官方路线是「换壁纸插件」（第 3 节），不是叠加窗口。

> 对壁纸引擎意味着：X11 下必须用 `NET::Desktop`（自绘 Frameless + KeepBelow），并且要主动规避与 plasmashell 桌面窗口争抢「桌面」角色；纯叠加窗口方案在 KWin 上注定与桌面图标/右键交互打架。

## 2. Plasma 6 Wayland：layer-shell 与可达路径

- **KWin 确实实现 `zwlr_layer_shell_v1`（v5），且没有客户端白名单**：协议内嵌在 KWin 仓库（[src/wayland/protocols/wlr-layer-shell-unstable-v1.xml](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/protocols/wlr-layer-shell-unstable-v1.xml)，`:28 <interface name="zwlr_layer_shell_v1" version="5">`）；`layershell_v1.cpp:22 static const int s_version = 5;`、在 [layershellv1integration.cpp#L25](https://invent.kde.org/plasma/kwin/-/blob/master/src/layershellv1integration.cpp#L25) 实例化 `new LayerShellV1Interface(waylandServer()->display(), this)`。我在 layershell_v1.cpp / plasmashell.cpp 里没找到任何 allowlist / 权限检查。
- layer-shell → KWin 层的映射（[src/layershellv1window.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/layershellv1window.cpp)）：`scopeToType()` 把 namespace 映射为 WindowType（`desktop`→Desktop、`dock`、`notification`、`tooltip`、`on-screen-display`、`dialog`、`splash`、`utility`，其余 Normal）；`belongsToLayer()`：Background→DesktopLayer、Bottom→BelowLayer、Top→AboveLayer、Overlay→OverlayLayer；另 `setSkipSwitcher(!isDesktop())`。
- **KDE 官方给第三方的 Qt 封装是 LayerShellQt**（[invent.kde.org/plasma/layer-shell-qt](https://invent.kde.org/plasma/layer-shell-qt)，镜像 [KDE/layer-shell-qt](https://github.com/KDE/layer-shell-qt)）：README 要求在创建窗口前调 `LayerShellQt::Shell::useLayerShell()`，再用 `LayerShellQt::Window::get(window)` 设置 layer/anchors/exclusiveZone/keyboardInteractivity。→ 在 Wayland 下「贴桌面、所有窗口之下、不占任务栏」用这条即可。
- 已知坑（都还在上游挂着）：[518060](https://bugs.kde.org/show_bug.cgi?id=518060)（`ON_DEMAND` 键盘交互总是抢焦点，2026-03 仍 CONFIRMED）、[503121](https://bugs.kde.org/show_bug.cgi?id=503121)（layer surface unmap 后不再发 configure）、[502497](https://bugs.kde.org/show_bug.cgi?id=502497)（在 layer surface 上 `start_drag` 导致 KWin 崩溃，已修）、[480795](https://bugs.kde.org/show_bug.cgi?id=480795)（切 layer 不立即生效）、[526999](https://bugs.kde.org/show_bug.cgi?id=526999)（同时改 buffer 尺寸与 anchor 会误绘一帧，WONTFIX）。
- **窗口规则路线（不需要任何协议，X11/Wayland 通用）**：KWin 有 `Layer` 强制规则，KCM 可选值是 Desktop / Below / Normal / Above / Notification / Fullscreen / Popup / Critical notification / On-screen display / Overlay（[src/kcms/rules/rulesmodel.cpp#L916-L930](https://invent.kde.org/plasma/kwin/-/blob/master/src/kcms/rules/rulesmodel.cpp#L916)），作用点是 `Window::layer()`：`m_layer = rules()->checkLayer(belongsToLayer())`（[src/window.cpp#L519, #L529](https://invent.kde.org/plasma/kwin/-/blob/master/src/window.cpp#L519)）。规则字段全集见 [src/rules.h `Rules::Type`](https://invent.kde.org/plasma/kwin/-/blob/master/src/rules.h#L88)：Position/Size/**Desktops**/Maximize/Minimize/**SkipTaskbar/SkipPager/SkipSwitcher/Above/Below**/Fullscreen/**DecorationPolicy**/Opacity/**Activity**/Screen/DesktopFile/**Layer**。→ 「所有桌面、不占任务栏、不占分页器、无边框、鼠标穿透（不取焦点）」几乎都能用一条规则表达。
- 规则落地方式：写 `~/.config/kwinrulesrc` 后 `qdbus org.kde.KWin /KWin reconfigure`（方法见 [src/org.kde.KWin.xml](https://invent.kde.org/plasma/kwin/-/blob/master/src/org.kde.KWin.xml)）；动态脚本走 `org.kde.kwin.Scripting`（[src/scripting/scripting.h#L317](https://invent.kde.org/plasma/kwin/-/blob/master/src/scripting/scripting.h#L317)）。
- **`org_kde_kwin_plasma_shell` 不能给第三方用**：协议头部原文 *“This interface is used by KF5 powered Wayland shells … can only be bound one time. Warning! … Regular clients must not use this protocol.”*（[plasma-shell.xml](https://invent.kde.org/libraries/plasma-wayland-protocols/-/blob/master/src/protocols/plasma-shell.xml)）；KWin 实现里 `role_desktop` 存在（[src/wayland/plasmashell.cpp#L171-L205](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/plasmashell.cpp#L171)）但这条路是 Plasma 5 Wayland 的旧机制，且官方明令禁用。
- 其它 `org_kde_kwin_*`：layer-shell 之外**没有**「置底 / 鼠标穿透 / 全桌面」专用协议。plasma-wayland-protocols 提供 appmenu、blur、contrast、dpms、fake-input、idle、keystate、kde-output-{device-v2,management-v2,order-v1,primary-output-v1}、plasma-shell、plasma-window-management、plasma-virtual-desktop、shadow、slide、server-decoration(-palette)、screen-edge 等（[协议目录](https://invent.kde.org/libraries/plasma-wayland-protocols/-/tree/master/src/protocols)）。
- **跨虚拟桌面**：layer-shell 没有「桌面归属」维度；Wayland 下我**未能查证**「让 layer-shell/无边框窗口出现在所有虚拟桌面」的官方机制。可行替代是上面那条 `Layer` + `Desktops`/`Activity` 窗口规则组合。

> 对壁纸引擎意味着：Wayland 主路径 = LayerShellQt（Background 层 + namespace/scope），但要接受 KWin 的 layer-shell bug 与「无法自动跨桌面」；退路 = 让用户/安装器写入 KWin 窗口规则（Layer + SkipTaskbar + SkipSwitcher + Desktops/Activity + DecorationPolicy=None）；不要碰 plasma-shell。

## 3. Plasma 壁纸插件路线

- 机制：壁纸插件是 KPackage，`KPackageStructure: "Plasma/Wallpaper"`，默认装到 `plasma/wallpapers/`，必需文件 `ui/main.qml`（mainscript），可选 `images/ theme/ config/ ui/ code/ locale/`（[libplasma …/qmlWallpaper/wallpaper.cpp](https://invent.kde.org/plasma/libplasma/-/blob/master/src/plasma/packagestructure/qmlWallpaper/wallpaper.cpp)）。QML 里拿到的是 `WallpaperItem`（[plasmaquick/plasmoid/wallpaperitem.h](https://invent.kde.org/plasma/libplasma/-/blob/master/src/plasmaquick/plasmoid/wallpaperitem.h)，import `org.kde.plasma.plasmoid`），有 `configuration`(KConfigPropertyMap)。KF6 libplasma 里**没有 C++ `Plasma::Wallpaper` 基类** → 插件是纯 QML，跑在 plasmashell 进程内。
- 换壁纸的命令行：`plasma-apply-wallpaperimage` 的实现就是拼 JS 调 `org.kde.plasmashell /PlasmaShell` 的 `evaluateScript`：`for (var key in desktops()) { var d = desktops()[key]; d.wallpaperPlugin='org.kde.image'; d.currentConfigGroup=[...]; d.writeConfig('Image','file://…'); }`，另有 `--fill-mode`（[wallpapers/image/plasma-apply-wallpaperimage.cpp#L84-L122](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/wallpapers/image/plasma-apply-wallpaperimage.cpp#L84)）；KPackage 目录含 `metadata.desktop` **或** `metadata.json` 都认。
- `plasma-apply-*` 全集（master 实测）：`plasma-apply-wallpaperimage`、`plasma-apply-cursortheme`、`plasma-apply-desktoptheme`、`plasma-apply-colorscheme`、`plasma-apply-lookandfeel`（[kcms/lookandfeel/tool/CMakeLists.txt](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/kcms/lookandfeel/tool/CMakeLists.txt)）。**没有**通用的「启用任意壁纸插件」CLI。
- PlasmaShell D-Bus 在 Plasma 6 仍在：`evaluateScript(s)->s`、`setWallpaper(plugin, a{sv}, screen)`、`wallpaper(screen)`、signal `wallpaperChanged(screen)`（[shell/dbus/org.kde.PlasmaShell.xml](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/shell/dbus/org.kde.PlasmaShell.xml)，实现见 [shell/shellcorona.cpp#L1927, #L1988](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/shell/shellcorona.cpp#L1988)）。文件里 `grabContainmentImage` 等被标注 *“Internal API! Do not use in third party software”* → 只用 evaluateScript / setWallpaper / wallpaper 这三个。
- 第三方实例 A（纯 plasmashell 内渲染）：[luisbocanegra/plasma-smart-video-wallpaper-reborn](https://github.com/luisbocanegra/plasma-smart-video-wallpaper-reborn) 用 `QtMultimedia` 在 QML 里播视频，用 `org.kde.plasma.plasma5support`(P5Support) 的 `executable` data engine 起外部命令（该引擎在独立仓库 [plasma/plasma5support/src/dataengines/executable](https://invent.kde.org/plasma/plasma5support/-/tree/master/src/dataengines/executable)），支持锁屏、按「最大化/全屏/活动窗口/屏幕关闭」暂停。
- 第三方实例 B（外部进程 + 共享纹理，最可借鉴）：[waywallen-display](https://github.com/waywallen/waywallen-display)（旧 catsout/wallpaper-engine-kde-plugin 的后继）——外部渲染进程通过自定义协议把 **DMA-BUF 帧 + acquire/release sync fence** 交给客户端，并提供 Qt6 QML 插件 `Waywallen.Display`（`WaywallenSurface`）、**Plasma 6 壁纸扩展（kpackage）**、GNOME 扩展、以及给 wlroots 系合成器用的独立 layer-shell 客户端。安装即 `kpackagetool6 --type Plasma/Wallpaper -i waywallen-kde-*.zip` + `systemctl --user restart plasma-plasmashell.service`。→ 「外部引擎渲染 + 零拷贝进壁纸插件」在 Plasma 6 已被验证。
- 新的官方通道：xdg-desktop-portal-kde 的 `org.freedesktop.impl.portal.Wallpaper` 已实现（[data/kde.portal](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/data/kde.portal) 声明该接口；实现 [src/wallpaper.cpp](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/src/wallpaper.cpp)），Plasma/6.4 分支没有该文件、Plasma/6.5 有 → **Plasma 6.5 起可用**；对应 bug [485966](https://bugs.kde.org/show_bug.cgi?id=485966) RESOLVED FIXED。门户语义是「设一张图片为壁纸」，**不能**用来启用第三方插件壁纸。
- 局限：① 插件在 plasmashell 进程内，插件崩溃会带走 plasmashell；② **每虚拟桌面独立壁纸在 Plasma 6 仍未实现**（[bug 341143](https://bugs.kde.org/show_bug.cgi?id=341143) CONFIRMED，version 6.0.4，2025-02 仍在；相关 [367666](https://bugs.kde.org/show_bug.cgi?id=367666) 2025-11 仍 CONFIRMED）；③ 每屏一套壁纸是支持的（containment 按屏）。

> 对壁纸引擎意味着：KDE 下「正路」是把 WebKitGTK 渲染结果作为帧送进一个自研壁纸插件（waywallen 模式），或用 Tauri 窗口 + layer-shell/窗口规则硬贴；`evaluateScript`/`setWallpaper` 用于启用自己的插件；跨桌面壁纸不要承诺「每桌面不同」。

## 4. 窗口背景模糊

- X11 原子**仍在**：KWin master 里 `static const QByteArray s_blurAtomName = QByteArrayLiteral("_KDE_NET_WM_BLUR_BEHIND_REGION");`（[src/plugins/blur/blur.cpp#L58](https://invent.kde.org/plasma/kwin/-/blob/master/src/plugins/blur/blur.cpp#L58)）。但行为有历史坑：[409524](https://bugs.kde.org/show_bug.cgi?id=409524)（`=0` 不总是触发模糊，REOPENED）、[482294](https://bugs.kde.org/show_bug.cgi?id=482294)（Plasma 6 Wayland 上 `enableBlurBehind()` 区域不再相对客户区左上角，已修）、[510818](https://bugs.kde.org/show_bug.cgi?id=510818)（6.4.91 修亮度异常）。
- Wayland 的**新**接口：KWin 实现 `ext_background_effect_manager_v1`（[src/wayland/backgroundeffect_v1.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/backgroundeffect_v1.cpp)：`QtWaylandServer::ext_background_effect_manager_v1(*display, 1)` + `capability_blur`），2026-01-28 加入。
- `org_kde_kwin_blur` **已被 KWin 移除**：commit [`7f9e0aa1`](https://invent.kde.org/plasma/kwin/-/commit/7f9e0aa1) “wayland: remove support for the kde blur protocol”（2026-02-13），该提交在分支 `Plasma/6.7`、`Plasma/6.8` 中，**不在** `Plasma/6.6` → **Plasma 6.7 起改用 ext-background-effect-v1**。协议文件本身还留在 plasma-wayland-protocols（[blur.xml](https://invent.kde.org/libraries/plasma-wayland-protocols/-/blob/master/src/protocols/blur.xml)），但 KWin 不再提供服务端。第三方佐证：[ghostty#13041](https://github.com/ghostty-org/ghostty/discussions/13041)、[zed#49805](https://github.com/zed-industries/zed/discussions/49805)。
- 模糊效果默认开：`BlurEffect::enabledByDefault()` 在非软件渲染、且不是过老 Intel / Mali / VideoCore 时直接 `return true`（[blur.cpp#L411-L424](https://invent.kde.org/plasma/kwin/-/blob/master/src/plugins/blur/blur.cpp#L411)）；`supported()` 要求 OpenGL 合成。用户可以全局关掉。
- 第三方：`kwin-effects-forceblur`（[taj-ny/kwin-effects-forceblur](https://github.com/taj-ny/kwin-effects-forceblur)）说明「强制模糊是 KWin 自身缺的能力」。

> 对壁纸引擎意味着：模糊必须显式请求并做降级；X11 用原子、Wayland 6.7+ 用 ext-background-effect-v1、6.6 及以前才可能用 org_kde_kwin_blur；不能假定效果开启。

## 5. 虚拟桌面 / Activities

- 概念差异：虚拟桌面是 KWin 的（[src/virtualdesktops.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/virtualdesktops.cpp)，D-Bus 接口 `org.kde.KWin.VirtualDesktopManager`，[dbusinterface.h#L182](https://invent.kde.org/plasma/kwin/-/blob/master/src/dbusinterface.h#L182)）；Activities 是 KActivities 的，plasmashell 的 containment（承载壁纸）按 **activity + 屏幕** 组织（DesktopView 连接 `KActivities::Controller::activityAdded/Removed`，见 master [shell/desktopview.cpp](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/shell/desktopview.cpp)）。
- 「所有桌面同一壁纸」在 Plasma 6 是**默认且唯一**行为：按虚拟桌面分别设壁纸的功能被移除且未恢复（[bug 341143](https://bugs.kde.org/show_bug.cgi?id=341143)）。
- X11 下第三方要「所有桌面 + 所有活动」：`NET::Desktop` 类型即天然全桌面（[x11window.cpp#L708](https://invent.kde.org/plasma/kwin/-/blob/master/src/x11window.cpp#L708)）；普通窗口则用窗口规则的 `Desktops` / `Activity`（[rules.h](https://invent.kde.org/plasma/kwin/-/blob/master/src/rules.h#L88)）。
- Wayland 下：layer-shell 无桌面维度；组合 `Layer` + `Desktops`/`Activity` 窗口规则是可行路线（见第 2 节）。**未能查证**更「原生」的接口。
- 脚本维度：`evaluateScript` 里可用 `desktops()`（官方 CLI 就是循环它写配置），Activities 侧有独立的 activity 维度对象；`plasma-apply-wallpaperimage` 只循环 `desktops()`、不循环 activities，所以「换全部 activity 的壁纸」在这条 CLI 上得不到保证（**未能查证**确切语义）。

> 对壁纸引擎意味着：可以承诺「所有虚拟桌面显示同一壁纸」（X11 内建 / Wayland 靠规则），但不要承诺每桌面不同壁纸；Activities 维度需要自己遍历处理。

## 6. 多屏与缩放

- 客户端看到的 `wl_output.scale` 是整数：`d->scale = std::ceil(handle->scale())`（[src/wayland/output_interface.cpp#L204, #L231](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/output_interface.cpp#L204)）→ 1.25 分数缩放下普通客户端默认拿到 scale=2，需要自己用 viewporter 修正。
- 分数缩放走 `wp_fractional_scale_v1`：`send_preferred_scale(std::round(scale * 120))`（[src/wayland/fractionalscale_v1.cpp#L73](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/fractionalscale_v1.cpp#L73)，即 1.25→150）；另有 `surface_set_buffer_scale`（[surface.cpp#L401](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/surface.cpp#L401)）与 `send_preferred_buffer_scale(std::ceil(scale))`（同文件 #L1231）；KWin 还带 `xx-fractional-scale-v2`（`src/wayland/protocols/xx-fractional-scale-v2.xml`）。
- layer-shell 坐标是**逻辑坐标**，KWin 内部除以 client→compositor 缩放：`pending->desiredSize = QSizeF(width / surface->clientToCompositorScale(), height / …)`，margins/exclusiveZone 同理（[src/wayland/layershell_v1.cpp#L177-L241](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/layershell_v1.cpp#L177)）→ 壁纸窗口应按 configure 给出的逻辑尺寸布局，物理缓冲按 `preferred_scale/120` 算；不要自己拍 ceil。
- 已知缩放坑：[475325](https://bugs.kde.org/show_bug.cgi?id=475325)（W/scale 或 H/scale 非整数导致模糊，6.0，已修）、[486626](https://bugs.kde.org/show_bug.cgi?id=486626)（分数缩放 + Wobbly Windows 模糊，已修）。
- 每屏查询的现实手段：
  - `kscreen-doctor`（[kscreen/console](https://invent.kde.org/plasma/kscreen/-/tree/master/console)）命令为 `bug|config|outputs|monitor|json`，`json` 直接打印当前 KScreen 配置（[console/main.cpp](https://invent.kde.org/plasma/kscreen/-/blob/master/console/main.cpp)）。
  - `org.kde.KScreen` KDED 守护进程：`Q_CLASSINFO("D-Bus Interface", "org.kde.KScreen")`（[kded/daemon.h#L34](https://invent.kde.org/plasma/kscreen/-/blob/Plasma/6.7/kded/daemon.h#L34)）；scale 以 1/10000 精度写入配置（[kded/output.cpp#L453](https://invent.kde.org/plasma/kscreen/-/blob/Plasma/6.3/kded/output.cpp#L453)）。**存在性按版本分叉**：Plasma/6.4、6.5、6.6、6.7 分支都还有 `add_subdirectory(kded)`，**Plasma/6.8 与 master 已移除 kded 模块**（改为 `kscreenctl`）→ 依赖 `org.kde.KScreen` 的方案必须按版本判断。每屏子对象的接口名与属性清单**未能查证**（6.7 的 `kded/device.h` 里没有 Q_CLASSINFO/Q_PROPERTY）。
  - C++ 侧可用 `plasma/libkscreen`（独立仓库，[项目页](https://invent.kde.org/plasma/libkscreen)）拿输出配置；壁纸引擎若不想链接它，退化方案是解析 `~/.config/kwinoutputconfig.json`（kscreen-doctor `json` 打印的就是它）。

> 对壁纸引擎意味着：铺满屏幕的窗口用 layer-shell 的 configure 逻辑尺寸 + `wp_fractional_scale`/viewporter 换算物理缓冲；不要用 `wl_output.scale` 直接乘；每屏几何/缩放查询优选 kscreen-doctor（注意 Plasma 6.8 起 D-Bus 路径没了）。

## 7. 合成器/会话探测

- `XDG_CURRENT_DESKTOP`：Plasma 6 的 `setupPlasmaEnvironment()` 设置 `KDE_FULL_SESSION=true`、`KDE_SESSION_VERSION=6`、`KDE_SESSION_UID`、`XDG_CURRENT_DESKTOP=KDE`（另设 `XDG_MENU_PREFIX=plasma-`）—— [startkde/startplasma.cpp#L411-L415](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/startkde/startplasma.cpp#L411)。现代源码里**没有** `KDE:Plasma` 这个字符串（grep 为空），但 KDE 自己的检测是**按冒号拆分后大小写不敏感地找 `kde`**（[startkde/systemd/kde-systemd-start-condition.cpp#L20](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/startkde/systemd/kde-systemd-start-condition.cpp#L20)）→ 第三方也应这么做以兼容历史值。
- 会话类型：`XDG_SESSION_TYPE=wayland` 由 startplasma 自己设置（[startplasma.cpp#L852](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/startkde/startplasma.cpp#L852)）；`WAYLAND_DISPLAY` 非空也代表 Wayland；`KDE_SESSION_VERSION` 为 `6`（Plasma 6）/`5`（Plasma 5）。
- KWin D-Bus：服务名 `org.kde.KWin`，主接口 `org.kde.KWin`、对象 `/KWin`，方法有 `reconfigure`、`killWindow`、`setCurrentDesktop`/`currentDesktop`/`nextDesktop`/`previousDesktop`、`activeOutputName`、`supportInformation`、`queryWindowInfo`/`getWindowInfo`、`showDesktop`，信号 `reloadConfig`、`showingDesktopChanged`（[src/org.kde.KWin.xml](https://invent.kde.org/plasma/kwin/-/blob/master/src/org.kde.KWin.xml)、[dbusinterface.h#L42](https://invent.kde.org/plasma/kwin/-/blob/master/src/dbusinterface.h#L42)）。其它可用接口：`org.kde.kwin.Compositing`（#L105）、`org.kde.KWin.VirtualDesktopManager`（#L182）、`org.kde.KWin.Plugins`（#L254）、`org.kde.kwin.Scripting`（scripting.h#L317）、`org.kde.kwin.Effects`、`org.kde.KWin.Session`、`org.kde.KWin.InputDeviceManager`、`org.kde.kappmenu`。
- 建议判定链：`XDG_CURRENT_DESKTOP` 拆分含 `kde`（或 `KDE_FULL_SESSION`）→ `KDE_SESSION_VERSION` 定 5/6 → `XDG_SESSION_TYPE` / `WAYLAND_DISPLAY` 定 X11/Wayland → 再用 D-Bus 上是否存在 `org.kde.KWin` + `/KWin` 确认 KWin 真在跑（避免在非 Plasma 的 KDE 会话里误判）。

> 对壁纸引擎意味着：探测不要用 `== "KDE"`，要拆分+大小写不敏感；`KDE_SESSION_VERSION` 是区分 5/6 的权威位；D-Bus 存在性可作为二次确认。

## 8. 显示器空闲 / 睡眠

- KWin 同时实现两套 idle 协议：`org_kde_kwin_idle`（[src/wayland/idle.cpp#L19](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/idle.cpp#L19)）与 `ext-idle-notify-v1`（`src/wayland/idlenotify_v1.cpp`，2022 加入，2025-03 起增加 `get_input_idle_notification`；协议里 `idled`/`resumed` 事件可订阅）→ 客户端可**订阅**空闲/恢复，不必轮询。
- logind 也可订阅：`org.freedesktop.login1` 的 Session/User 对象有 `IdleHint`(b)、`IdleSinceHint`(t)、`IdleSinceHintMonotonic`(t)，并有 `SetIdleHint(in b)`，属性走 PropertiesChanged（[systemd 官方 man](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html)，已核到原文）。
- KDE 会话提供 `org.freedesktop.ScreenSaver`（对象 `/ScreenSaver`，含 `GetActive`/`SessionIdleTime`）：KDE 自己的会话管理代码就在用它（[libkworkspace/sessionmanagement.cpp#L151, #L269, #L285](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/libkworkspace/sessionmanagement.cpp#L151)、[kdisplaymanager.cpp#L839](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/libkworkspace/kdisplaymanager.cpp#L839)）→ 「屏幕是否锁/空闲」的 X11 兼容路径。
- KWin 也实现 `org_kde_kwin_dpms`（[src/wayland/dpms.cpp](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/dpms.cpp)）与 `zwp_idle_inhibit_manager_v1`；但**「屏幕是否已 DPMS 关闭」是否可查询**未能查证（该协议偏设置语义）。

> 对壁纸引擎意味着：Wayland 用 `ext-idle-notify-v1`（或退回 `org_kde_kwin_idle`）订阅；跨会话通用的是 logind `IdleHint`；需要「屏幕黑了就暂停」时不要依赖可查询的 DPMS 状态。

## 9. 全局快捷键

- X11 与 Wayland 都由 KWin 内嵌的 KGlobalAccelD 统一处理：`#include <kglobalacceld.h>` + `m_kglobalAccel = std::make_unique<KGlobalAccelD>();`（[kwin src/globalshortcuts.cpp#L20, #L97](https://invent.kde.org/plasma/kwin/-/blob/master/src/globalshortcuts.cpp#L97)），守护进程仓库是 [plasma/kglobalacceld](https://invent.kde.org/plasma/kglobalacceld)（D-Bus 名 `org.kde.kglobalaccel`，接口 `org.kde.kglobalaccel` 与 `org.kde.kglobalaccel.Component`，见 `src/kglobalacceld.cpp`、`src/component.h`）。KDE 自身占用大量组合键 → 第三方在 X11 直接抢键会冲突，需要走 KGlobalAccel 的注册/冲突机制。**X11 具体抓键调用位置未能查证**（我在 kglobalacceld 与 KF6 `frameworks/kglobalaccel` 里都没找到 `XGrabKey`；KF6 侧只有时间戳相关的 X11 代码）。
- Wayland 只有一条正路：`org.freedesktop.portal.GlobalShortcuts`。KDE 后端在 [xdg-desktop-portal-kde/src/globalshortcuts.cpp](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/src/globalshortcuts.cpp)（基于 KGlobalAccel），且 [data/kde.portal](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/data/kde.portal) 已声明该接口；首个实现提交 2022-11-30、首个 release tag `v5.27.0` → **Plasma 5.27 起可用**，并持续维护（2026-07-15 仍有提交 “Do not register shortcuts on session creation”）。
- 已知问题：[bug 498575](https://bugs.kde.org/show_bug.cgi?id=498575)（GlobalShortcuts `BindShortcuts` 一系列问题，6.2.5 FIXED）；KDE 邮件列表也在讨论第三方 dock 如何同时支持屏幕边缘与全局快捷键（[kwin 邮件 2024-10](https://mail.kde.org/pipermail/kwin/2024-October/005342.html)）。
- 门户语义与自我抢键的差别：键位由桌面/用户确认与记忆，应用无法保证拿到某个具体组合，且可能未绑定；门户外再 XGrabKey 在 Wayland 无效。

> 对壁纸引擎意味着：X11 用 KGlobalAccel（或接受冲突）；Wayland 必须实现 portal GlobalShortcuts（Plasma ≥5.27 有后端），并把「快捷键可能没绑定」当作可用状态。

## 10. 系统音频采集（可选）

- **门户侧没有音频**：`grep -i audio` 在 xdg-desktop-portal-kde 的 `src/` 全空（实测）；ScreenCast 门户官方文档也不含 audio 字样（[ScreenCast 文档](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)）→ 不要指望用 ScreenCast/RemoteDesktop 门户拿系统音频。
- PipeWire/PulseAudio 兼容层仍能录 monitor：`pactl` 支持特殊名 `@DEFAULT_MONITOR@`，并可列 `.monitor` source（[pactl(1)](https://man.archlinux.org/man/pactl.1.en) 已核到原文）→ `parec -d <sink>.monitor` 这类路径可用。`pw-record` 的具体 target 语法**未能查证**（docs.pipewire.org 的 `page_man_pw-record_1.html` 与 man.archlinux.org 的 `pw-record.1` 都是 404）；建议运行时用 `pactl list short sources` / `pw-cli ls Source` 选 monitor 源。
- 「Plasma 6 默认 PipeWire/WirePlumber」的**官方原文未能找到**（kde.org 6.0 megarelease 页未含该字样；community.kde.org 被 Cloudflare 拦）；旁证是 Arch Wiki KDE 页建议把 pulseaudio 替换为 pipewire-pulse（[Arch Wiki KDE](https://wiki.archlinux.org/title/KDE)）。
- Flatpak 打包会显著受限：沙箱内既拿不到 monitor，也没有门户音频通道。

> 对壁纸引擎意味着：桌面版可以按「找默认 sink 的 monitor 源」实现音频可视化，但不要依赖门户、不要写死 `pw-record` 参数；Flatpak 版应把音频可视化标为不支持或需要额外权限。

## 11. 托盘（StatusNotifierItem）

- Plasma 6 自带 SNI：plasmashell 提供 `org.kde.StatusNotifierWatcher`（[statusnotifierwatcher/](https://invent.kde.org/plasma/plasma-workspace/-/tree/master/statusnotifierwatcher)），界面在 `applets/systemtray/`；另有 `xembed-sni-proxy` 兼容旧 XEmbed 图标。AppIndicator/ayatana 图标不是必须的（SNI 是原生路径）。
- 图标来源与重着色：`loadIcon("IconName","IconPixmap")`（[statusnotifieritemsource.cpp#L412](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/applets/systemtray/statusnotifieritemsource.cpp#L412)），随后用 `KIconEngine(iconName, KIconColors(globalPalette()), iconLoader(), {overlayIconName})`（#L392、#L467）→ 会按当前配色主题做符号化重着色。`IconThemePath` 属性已被支持（#L172-L174 与 #L324-L352）。
- 历史坑：**Plasma 6 早期不遵守 `IconThemePath`** —— [bug 479712](https://bugs.kde.org/show_bug.cgi?id=479712)（v5.92.0 FIXED，2024-03）→ 自带图标库的应用在 Plasma 5.92 之前可能显示空白/错误图标（template 图标尤其明显）。Plasma ≥6.0 正常。
- 是否需要 `.desktop`：图标的**显示不需要** .desktop（标题/图标来自 SNI 属性）；KService 只在「后台应用」列表解析应用名时用到，且找不到会兜底（[systemtraymodel.cpp#L521-L524](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/applets/systemtray/systemtraymodel.cpp#L521)）。但要让应用在托盘菜单里显示正确名称/图标、能被「退出」操作识别，仍应安装 .desktop。
- 模板图标：既然 Plasma 6 走 `KIconEngine` + 全局配色，模板/symbolic 图标通常不会「隐形」；想保留原色图标的做法是传 `IconPixmap` 位图（避免被当 symbolic 重着色）——**这一点是从代码推断，未经权威文档确认**。

> 对壁纸引擎意味着：Wayland 下用 SNI + DBusMenu 即可，不必依赖 ayatana；图标应同时提供 `IconName`（主题内 symbol）与 `IconPixmap`（原色兜底），并按 Plasma 6 的调色板预期测试模板图标。

## 12. 无边框 / 自带窗口装饰

- KWin 的判据：`bool Window::noBorder() const { return decorationPolicy() == DecorationPolicy::None; }`（[src/window.cpp#L4241](https://invent.kde.org/plasma/kwin/-/blob/master/src/window.cpp#L4241)），窗口规则字段 `DecorationPolicy`（[rules.h](https://invent.kde.org/plasma/kwin/-/blob/master/src/rules.h#L100)），用户操作入口在 `useractions.cpp` 的 `NoBorderOp`（[#L1165-L1171](https://invent.kde.org/plasma/kwin/-/blob/master/src/useractions.cpp#L1165)）。
- **无装饰窗口没有 KWin 提供的边缘 resize**：`Window::borderLeft()` 在未装饰时返回 0（[window.cpp#L2402-L2404](https://invent.kde.org/plasma/kwin/-/blob/master/src/window.cpp#L2402)），交互式缩放由 decoration 的 `resizeOnlyBorders()` 驱动（[#L2728-L2731](https://invent.kde.org/plasma/kwin/-/blob/master/src/window.cpp#L2728)）→ GTK/WebKitGTK 必须自己做 CSD 边缘命中。
- 移动/缩放仍有快捷键兜底：KWin 默认 `CommandAllKey=Meta`、`CommandAll1=Activate, raise and move`、`CommandAll3=Activate, raise and resize`（[src/kwin.kcfg#L12-L58](https://invent.kde.org/plasma/kwin/-/blob/master/src/kwin.kcfg#L12)，即 Meta+左键移动、Meta+右键缩放，用户可改），实现见 `Window::performMousePressCommand(Options::MouseResize, …)`（[useractions.cpp#L1117](https://invent.kde.org/plasma/kwin/-/blob/master/src/useractions.cpp#L1117)）。
- 双击标题栏最大化对无边框窗口没有意义；KWin 有独立设置 `DoubleClickBorderToMaximize`（默认 true，[kwin.kcfg](https://invent.kde.org/plasma/kwin/-/blob/master/src/kwin.kcfg#L55-L57)），作用于**边框**区域。
- CSD 阴影：KWin 实现 `org_kde_kwin_shadow_manager`（[shadow_interface.cpp#L18, #L32, #L81](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/shadow_interface.cpp#L18)），协议定义在 [plasma-wayland-protocols/shadow.xml](https://invent.kde.org/libraries/plasma-wayland-protocols/-/blob/master/src/protocols/shadow.xml) → CSD 应用可上传阴影位图让 KWin 绘制；不主动用就完全没有阴影。圆角同理：我没找到 KWin 对 CSD 强制圆角的逻辑（`src/window.h/.cpp` 无 cornerRadius），即圆角由客户端负责。

> 对壁纸引擎意味着：透明无边框窗口要自己实现拖拽/缩放命中（或用 Meta 快捷键兜底），不要依赖 KWin 画阴影/圆角；配合窗口规则设 DecorationPolicy=None + SkipTaskbar/SkipPager/SkipSwitcher + 不取焦点，避免污染任务栏与焦点链。

---

## 关键结论（可直接写进方案）

1. **X11 只有一个正确轨道**：`_NET_WM_WINDOW_TYPE_DESKTOP` + `NET::KeepBelow` + Frameless（照抄 Plasma 自身 [desktopview.cpp](https://invent.kde.org/plasma/plasma-workspace/-/blob/Plasma/5.27/shell/desktopview.cpp#L218)）；普通窗口 + keep-below 会盖住桌面图标且不属于 DesktopLayer。
2. **桌面窗口是「单例语义」**：KWin 只认 stacking order 最上面那个 wantsInput 的 desktop 窗口（[layers.cpp findDesktop](https://invent.kde.org/plasma/kwin/-/blob/master/src/layers.cpp)），第三方再插一个会与 plasmashell 争抢，实证是 [linux-wallpaperengine#370](https://github.com/Almamu/linux-wallpaperengine/issues/370) 的图标/右键失效。
3. **Plasma 6 Wayland 有 layer-shell（zwlr_layer_shell_v1 v5，无白名单）**，官方 Qt 封装是 [LayerShellQt](https://invent.kde.org/plasma/layer-shell-qt)；Background 层 + scope `desktop` 就是「贴桌面」的等价物。但要吃下 [518060](https://bugs.kde.org/show_bug.cgi?id=518060)/[503121](https://bugs.kde.org/show_bug.cgi?id=503121) 等已知 bug。
4. **layer-shell 不提供跨虚拟桌面/活动的维度**；要「所有桌面」必须叠加 KWin 窗口规则（`Layer=Desktop|Below` + `Desktops`/`Activity`），规则字段全集见 [rules.h](https://invent.kde.org/plasma/kwin/-/blob/master/src/rules.h#L88)，KCM 的 Layer 取值见 [rulesmodel.cpp#L916](https://invent.kde.org/plasma/kwin/-/blob/master/src/kcms/rules/rulesmodel.cpp#L916)。
5. **`org_kde_kwin_plasma_shell` 官方明令第三方不得使用**（协议注释原文），不要拿它当「变成桌面」的后门。
6. **模糊在 Plasma 6.7 换了协议**：`org_kde_kwin_blur` 已从 KWin 移除（[commit 7f9e0aa1](https://invent.kde.org/plasma/kwin/-/commit/7f9e0aa1)，并入 Plasma/6.7+），新接口是 `ext_background_effect_manager_v1`；X11 原子仍在但历史 bug 多（[409524](https://bugs.kde.org/show_bug.cgi?id=409524)）。
7. **官方「设壁纸」API 只有三个**：PlasmaShell D-Bus 的 `evaluateScript` / `setWallpaper` / `wallpaper`（[org.kde.PlasmaShell.xml](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/shell/dbus/org.kde.PlasmaShell.xml)），`grabContainmentImage` 等被标为 Internal；没有通用 `plasma-apply-<plugin>`。
8. **「把自己的渲染器做成 Plasma 壁纸插件」是 KDE 上唯一能同时拿到桌面层 + 跨桌面 + 不冲突的正路**，外部 DMA-BUF 帧注入已被 [waywallen-display](https://github.com/waywallen/waywallen-display) 验证（Plasma 6 kpackage + QML `WaywallenSurface`）；代价是跑在 plasmashell 进程里、崩溃会带崩 plasmashell。
9. **缩放：客户端不能直接乘 `wl_output.scale`**（KWin 给的是 `ceil(scale)`，[output_interface.cpp#L204](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/output_interface.cpp#L204)）；要用 `wp_fractional_scale_v1` 的 `preferred_scale/120` + viewporter，layer-shell 的尺寸是逻辑坐标（[layershell_v1.cpp#L177](https://invent.kde.org/plasma/kwin/-/blob/master/src/wayland/layershell_v1.cpp#L177)）。
10. **每屏查询要按 Plasma 版本分叉**：`org.kde.KScreen` KDED 守护进程在 6.4–6.7 存在、**6.8/master 已删除**（master CMakeLists 无 kded），稳妥退路是 `kscreen-doctor json`（解析 `kwinoutputconfig.json`）。
11. **会话探测不要写 `XDG_CURRENT_DESKTOP == "KDE"`**：按冒号拆分、大小写不敏感找 `kde`（KDE 自己就这么做，[kde-systemd-start-condition.cpp#L20](https://invent.kde.org/plasma/plasma-workspace/-/blob/master/startkde/systemd/kde-systemd-start-condition.cpp#L20)），配合 `KDE_SESSION_VERSION`(5/6) 与 `XDG_SESSION_TYPE`/`WAYLAND_DISPLAY`。
12. **Wayland 全局快捷键只能走 `org.freedesktop.portal.GlobalShortcuts`**（KDE 后端 Plasma 5.27 起，[globalshortcuts.cpp](https://invent.kde.org/plasma/xdg-desktop-portal-kde/-/blob/master/src/globalshortcuts.cpp)）；键位由桌面决定，必须容忍未绑定。空闲用 `ext-idle-notify-v1` 或 logind `IdleHint`。托盘用原生 SNI（Plasma ≥6.0 已修 [479712](https://bugs.kde.org/show_bug.cgi?id=479712)）。

### 明确的「未能查证」清单
- plasmashell 桌面背景变透明/关闭的官方开关（第 1 节）。
- 让 layer-shell/无边框窗口出现在**所有虚拟桌面**的官方机制（第 2 节）。
- `org.kde.KScreen` 每屏子对象的 D-Bus 接口名与属性清单（第 6 节）。
- Plasma 6「默认 PipeWire/WirePlumber」的 KDE 官方原文；`pw-record` 的 monitor target 语法（第 10 节）。
- `IconPixmap` 是否被 KWin/plasmashell 排除在 symbolic 重着色之外（第 11 节，属代码推断）。
- Plasma 6 Wayland 下 X11 抓键（XGrabKey）实现所在位置（第 9 节）。
- `evaluateScript` 里 `desktops()` 是否覆盖所有 Activities（第 5 节）。
