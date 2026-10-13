# KDE 真机验证报告（Plasma 5.27 / X11）

> 对象：`WallpaperEM` 在 Linux/KDE 上的适配方案（[kde-adaptation-plan.md](kde-adaptation-plan.md)）。
> 本文只记录**实测**结果：测到的、测不出的、以及被证伪的原假设。上游依据见 [kde-plasma-constraints.md](kde-plasma-constraints.md)。
> 日期：2026-10-14。未改任何应用源码，只新增脚本与文档。

## 1. 验证底座

| 项 | 值 | 来源 |
| --- | --- | --- |
| 宿主 → 验证机 | `ssh parallels@10.211.55.5`（免密）；Parallels「Ubuntu 24.04.3 ARM64」 | 实测 |
| 系统 | Ubuntu 24.04.3 LTS (noble)，aarch64，内核 `6.14.0-27-generic` | `lsb_release -a` / `uname -r` |
| 桌面栈 | `kubuntu-desktop`：`plasmashell`/`plasma-desktop 4:5.27.12-0ubuntu0.1`、`kwin 5.27.11`、`sddm 0.20.0-2ubuntu4.2` | `/root/kde-version-report.txt` |
| 应用二进制 | `~/WallpaperEM/src-tauri/target/release/WallpaperEM`，45 MB，`cargo build --release --locked` 原生 arm64 构建（rustc 1.98.1） | 实测 |
| 显示 | `Xvfb :99 -screen 0 1280x800x24 +extension GLX +render -noreset`（llvmpipe，Mesa 25.2.8） | 会话脚本 |
| 窗口管理器 | `kwin_x11 --replace`，会话内 `XDG_CURRENT_DESKTOP=KDE`、`KDE_FULL_SESSION=true`、`KDE_SESSION_VERSION=5`、`XDG_SESSION_TYPE=x11` | 会话脚本 |
| 会话总线 | **`/run/user/1000/bus`（真实 systemd 用户会话）**，不是 `dbus-run-session` 私有总线 | 见 §4 |
| 复现入口 | `bash scripts/kde-vm-session.sh {probes\|realbuscase\|belowtest}` | 新增脚本 |

**Plasma 6 在这台机器上不可得**（三条路实测堵死：官方仓库只有 5.27；Kubuntu Backports PPA noble 的 `binary-{amd64,arm64}/Packages.gz` 均为 20 字节空索引；KDE neon 只有 amd64 ISO）。因此 `zwlr_layer_shell_v1` v5、`ext_background_effect_manager_v1`、portal `Wallpaper` 等 Plasma 6 专属项**本报告不覆盖**，已在方案 §7 单独标注。

## 2. 结论一览

| 机制 | 结论 | 证据强度 |
| --- | --- | --- |
| ① `_NET_WM_WINDOW_TYPE_DESKTOP` → 进入 DesktopLayer | ✅ 成立 | 强（`xprop` + stacking） |
| ① 让桌面图标浮在壁纸之上 | ❌ **证伪**（X11 上） | 强（stacking + 像素哈希） |
| ① 用 `_NET_WM_STATE_BELOW` 把壁纸压到桌面视图下 | ❌ 证伪 | 强（`belowtest` 三组对照） |
| KDE 服务可用性（`org.kde.*`） | ✅ 需真实用户总线 | 强（present/absent 对照） |
| `org.freedesktop.ScreenSaver` 判空闲 | ❌ 本机 `NotSupported` | 强 |
| `kscreen-doctor -j` 作每屏默认数据源 | ⚠️ 无头下无输出 | 中 |
| KWin 5 的 `Layer=Desktop` 窗口规则 | ⚠️ 未能证实 | 弱（仅二进制字符串） |
| `scripts/linux-kde-smoke.sh` 在真机会话里跑通 | ✅ 5/5 断言 PASS（连跑 2 次一致） | 强（见 §6.1） |
| Plasma 6 全部专属机制 | ⛔ 本机不可验 | — |

## 3. 核心发现：壁纸窗口的落点

`bash scripts/kde-vm-session.sh realbuscase` 的实测输出（`ALIVE_SECS=25`）：

```
[应用窗口清单（按 _NET_WM_PID 反查）]
  id=0x600036  _NET_WM_WINDOW_TYPE_DESKTOP  class="WallpaperEM","WallpaperEM"
  id=0x600003  _NET_WM_WINDOW_TYPE_NORMAL   class="WallpaperEM","WallpaperEM"

[壁纸窗口 0x600036]
  _NET_WM_WINDOW_TYPE(ATOM) = _NET_WM_WINDOW_TYPE_DESKTOP
  _NET_WM_STATE(ATOM) = _NET_WM_STATE_STICKY, _NET_WM_STATE_SKIP_TASKBAR, _NET_WM_STATE_SKIP_PAGER
  _NET_WM_PID(CARDINAL) = 20724

[stacking（自底向上）]
  #1 0x400015 type=DESKTOP pid=20607   ← plasmashell 的桌面窗口
  #2 0x600036 type=DESKTOP pid=20724   ← 我们的壁纸窗口
  #3 0x600003 type=NORMAL  pid=20724
  #4 0x40002b type=NORMAL  pid=20607   ← plasmashell 面板

[共存 desktop 窗口数 = 2]
[图标区哈希 有应用=6f431327d0034a9abba84b34309a3b6f05becce02b1d1876220ba9ad7fe78797
             无应用=483a969221ed240f815b6f11a57bca3a86aa3df04c738935ed248c62b44810eb]
```

读法：

1. **好消息**：`tauri-plugin-desktop-underlay` 的 GTK `WindowTypeHint::Desktop` 在 X11 上确实生效（窗口类型、`WM_CLASS`、PID、几何全对），且窗口确实落在普通窗口之下——跳任务栏/跳分页器/跨桌面这些**桌面语义拿到了**。
2. **坏消息**：`_NET_WM_WINDOW_TYPE_DESKTOP` 把我们送进 DesktopLayer，而 **plasmashell 自绘的桌面视图也在同层**，实测它比我们更靠底 → 我们的壁纸**盖在桌面图标容器之上**。像素哈希不同（`6f431327…` ≠ `483a9692…`）证明壁纸真的画进了图标区。
3. 也就是说，方案 §2.1 记录的「KDE 下图标被盖」是**真实且必然**的，且**不是**因为「代码提前 return 没下沉」——即使下沉也超不过 plasmashell（见下）。

## 4. 证伪实验：`_NET_WM_STATE_BELOW`

plasmashell 自己的桌面窗口实测带 `_NET_WM_STATE_BELOW`，所以「给壁纸窗口加同一个 state」看似是天然解法。`bash scripts/kde-vm-session.sh belowtest` 做了三组对照：

```
--- A. 现状（仅 DESKTOP type）---
  #1 0x400015 DESKTOP class=plasmashell   state=_NET_WM_STATE_BELOW
  #2 0x600036 DESKTOP class=WallpaperEM   state=STICKY,SKIP_TASKBAR,SKIP_PAGER
  名次(底=1): 2/4 DESKTOP

--- B. 加 _NET_WM_STATE_BELOW 之后 ---
  #1 0x400015 DESKTOP class=plasmashell   state=_NET_WM_STATE_BELOW
  #2 0x600036 DESKTOP class=WallpaperEM   state=_NET_WM_STATE_BELOW     ← 状态被接受了
  名次(底=1): 2/4 DESKTOP                                              ← 但层序没动

--- C. 去掉 DESKTOP type（改回 NORMAL）+ 保持 BELOW ---
  #1 0x400015 DESKTOP class=plasmashell   state=_NET_WM_STATE_BELOW
  #2 0x600036 NORMAL  class=WallpaperEM   state=_NET_WM_STATE_BELOW
  名次(底=1): 2/4 NORMAL
```

**结论**：在 X11 上，`_NET_WM_STATE_BELOW` 对「相对 plasmashell 桌面窗口的层序」**无效**。C 组更说明问题——去掉 DESKTOP 类型后它只是从 DesktopLayer 挪到 BelowLayer，仍被 plasmashell 的桌面窗口压住。任何「加个 keep-below/BELOW 就能让图标可见」的方案都应停止排期。

## 5. 会话总线：必须用真实用户总线

旧冒烟（`scripts/linux-kde-smoke.sh:244`）与第一版会话脚本都用 `dbus-run-session` 起私有总线，结果是：

```
kwin.log:  Couldn't start kglobalaccel from org.kde.kglobalaccel.service:
           QDBusError("org.freedesktop.DBus.Error.Spawn.ChildSignaled",
                      "Process org.kde.kglobalaccel received signal 6")
plasmashell.log: Aborting shell load: The activity manager daemon (kactivitymanagerd) is not running.
```

换成 `/run/user/1000/bus` 后（`systemctl --user is-system-running` = `running`）：

```
PROBE bus_org.kde.KWin = present
PROBE bus_org.kde.plasmashell = present
PROBE bus_org.kde.KScreen = present
PROBE bus_org.kde.kglobalaccel = present
PROBE bus_org.freedesktop.portal.Desktop = present
PROBE bus_org.kde.StatusNotifierWatcher = absent
```

**原因**：KDE 的 `kglobalaccel` / `kactivitymanagerd` / `KScreen` 靠 systemd 用户会话的服务激活文件启动，私有总线上没有这些激活文件，于是被信号 6 打死，plasmashell 随之拒绝加载。凡是要跟真 KDE 服务说话的无头验证，都必须挂在真实用户总线上。

顺带两个坑（都已修进脚本）：

- `Xvfb`/`kwin_x11` 必须 `setsid` 脱离 SSH 会话，否则登录 shell 一退出就 SIGHUP 带走它们，`kwin.log` 留下 `The X11 connection broke (error 1). Did the X11 server die?`。
- `xdotool search --pid` **不可靠**：WebKitGTK 的顶层窗口实测匹配到 0 个，而窗口明明在 stacking 里且 `_NET_WM_PID` 就是应用 PID。可靠做法是扫 `_NET_CLIENT_LIST_STACKING` 里的 DESKTOP 窗口再比对 `_NET_WM_PID`。

## 6. 其余实测项

| 探针 | 结果 | 含义 |
| --- | --- | --- |
| `Q1` `PlasmaShell.evaluateScript` 取 `desktops()[0].wallpaper` | 返回空字符串 | 返回值不可用（既有预期）；脚本内 `print` 也未落到 journal/`.xsession-errors` → 机制②需要换取证手段 |
| `Q3` 桌面 containment 配置 | `plugin=org.kde.plasma.folder`、`formfactor=0`、`wallpaperplugin=org.kde.image`，`activityId` 有值 | 机制②的键名确认；`formfactor=0` 即桌面 |
| `Q4` `/KWin` 接口 | `org.kde.KWin,org.freedesktop.DBus.Properties,org.freedesktop.DBus.Introspectable,org.freedesktop.DBus.Peer` | 接口存在 |
| `Q4` KWin 脚本 `client.layer` | `loadScript` 返回 `0` 后被脚本确认框阻塞，`queryWindowInfo` = `NoReply` | **取证失败**：无头会话下 KWin 弹模态框卡住主循环 |
| `Q4` 规则键（二进制） | `libkwin.so.5.27.11` 含 `layer`/`Layer`/`desktops`/`activity`/`skipPager`/`skipTaskbar` | 键存在；是否 unused 未证 |
| 每屏查询 | `kscreen-doctor -j` 无输出；`~/.config/kwinoutputconfig.json` 不存在 | 无头下不可用 |
| 空闲 | `org.freedesktop.ScreenSaver` present 但 `GetActive`/`GetSessionIdleTime` = `NotSupported`；`org.freedesktop.login1` = absent | 需改走 KWin idle / logind `IdleHint` |
| 静态壁纸 CLI | `plasma-apply-wallpaperimage`、`plasma-apply-desktoptheme` 存在；`qdbus6` 缺、`qdbus` 在 | 与 `system_wallpaper.rs` 的既有实现一致 |
| 托盘 | `org.kde.StatusNotifierWatcher` = **absent** | Plasma 5.27 无 SNI 宿主（Plasma 6 才有）→ 托盘在 5.27 上属额外依赖项 |
| 快捷键 | `org.kde.kglobalaccel` = present | X11 可用；实际注册待验 |

### 6.1 KDE 冒烟脚本真机首跑（现已 5/5 通过）

命令（VM 上，`KDE_SMOKE_STRICT` 语义的 X11 档）：

```bash
cd ~/WallpaperEM && SMOKE_WITH_PLASMA=1 bash scripts/linux-kde-smoke.sh
```

结果（连跑两次一致）：

| 断言 | 结果 |
| --- | --- |
| 应用存活 30s 无退出 | ✅ PASS |
| 日志含窗口创建/桌面层应用记录 | ✅ PASS |
| 日志无 `panic` | ✅ PASS |
| 壁纸窗口 `_NET_WM_WINDOW_TYPE` 含 `DESKTOP` | ✅ PASS（`id=0x600036`） |
| 壁纸窗口之上存在普通窗口（证明在普通窗口之下） | ✅ PASS |

同一轮的 stacking（自底向上，plasmashell 也在跑）：

```
#1 0x400015  type=DESKTOP  name= Desktop @ QRect(0,0 1280x800)   ← plasmashell 桌面视图
#2 0x600036  type=DESKTOP  name=                                 ← 我们的壁纸窗口
#3 0x600003  type=NORMAL   name= WallpaperEM
#4 0x40002b  type=NORMAL   name=                                 ← Plasma 面板
```

`共存 desktop 窗口数 = 2` —— 即 §3 的核心发现在冒烟里可被自动复现。

首跑失败并暴露了**两个真缺陷**（都已修入 `scripts/linux-kde-smoke.sh`）：

1. **kwin 启动等待循环形同虚设**：`xprop -root _NET_SUPPORTING_WM_CHECK >/dev/null 2>&1 && break` —— `xprop` 对「属性不存在」同样返回 0，循环第一次就 `break`，kwin 完全没有启动宽限期。改为看输出内容（`| grep -q 'window id'`）。
2. **只等 `xdpyinfo` 不足以判定 X 可用**：`xdpyinfo` 走 Xlib，**抽象 unix socket 也认**；而 kwin 用的 Qt5 `xcb` 插件只认文件 socket `/tmp/.X11-unix/X<N>`。只等 `xdpyinfo` 会撞上「抽象 socket 已通、文件 socket 还没建好（或残留陈旧文件）」的窗口，症状是 `qt.qpa.xcb: could not connect to display :99` + `Could not load the Qt platform plugin "xcb"`，而同一时刻 `xdpyinfo` 明明成功。现在改为**显式等文件 socket**，超时则直接报错并提示 `rm -f /tmp/.X11-unix/X99`。

诊断方法（可复用）：在失败分支打印 `xdpyinfo` 首行、`pgrep -a -x Xvfb`、`ss -xl | grep X11-unix`、`ls -la /tmp/.X11-unix/`，四条一起看就能区分「X 没起来 / X 起来了但没有文件 socket / X 被别人占着」。

### 6.2 KWin 的 Wayland 全局清单（实测，`scripts/kde-wayland-globals.sh`）

在 VM 上起嵌套合成器 `kwin_wayland --virtual --width 1280 --height 800 --socket wallpaperem-wlprobe --no-lockscreen`（socket 约 1s 就绪），用 `wayland-info` 连上其 Wayland socket 取全局清单：

| 接口 | 本机实测（kwin_wayland **5.27.11**，aarch64） | 意义 |
| --- | --- | --- |
| `zwlr_layer_shell_v1` | ✅ **v3** | 方案 §4.1 的 Wayland「层归属」路径在本机成立（上游源码在 Plasma 6 上是 v5，版本随 Plasma 递进） |
| `ext_background_effect_manager_v1` | ❌ 未广告 | 印证方案 §3 约束 6：新模糊协议是 Plasma 6.7+ 才有 |
| `org_kde_kwin_blur_manager` | ✅ v1 | 旧协议在 5.27 上确实还在 → `blur.rs` 的回退分支是本机主路径 |
| `org_kde_kwin_idle` / `ext_idle_notifier_v1` | ✅ v1 / ✅ v1 | 自动暂停的空闲源在 Wayland 上可选（比 `ScreenSaver.GetActive` 可靠，后者 5.27 上返回 `NotSupported`） |
| `zwp_linux_dmabuf_v1` | ✅ **v4** | P9 插件做零拷贝帧传输（DMA-BUF + sync fence）在协议层可行 |
| `wp_fractional_scale_manager_v1` / `wp_viewporter` | ✅ v1 / ✅ v1 | 分数缩放路径可用（客户端不要自己乘 `wl_output.scale`） |
| `zxdg_output_manager_v1` | ✅ v3 | 每屏枚举可用 |
| `zwp_keyboard_shortcuts_inhibit_manager_v1` | ✅ v1 | 录制快捷键时可抑制桌面快捷键（Wayland 上的正确做法） |
| `wp_tearing_control_manager_v1` | ✅ v1 | 以后若要低延迟整帧撕裂控制可用 |
| `org_kde_plasma_shell` | ✅ v8（**第三方不得使用**，仅作对照） | 方案 §3 约束 5 |

同时也修掉了本脚本自己的一个**假阴性**：`wayland-info` 把接口名写成 `'name'`（带单引号）且名字后面紧跟逗号，原先的 `grep "interface: *name "` 一个都匹配不上，导致首轮输出「全部未广告」。现在先 `tr -d "'"`，并按 `^interface: *NAME[, ]` 匹配（另外全局名是**工厂**名：`org_kde_kwin_blur_manager`、`wp_fractional_scale_manager_v1`）。

### 6.3 P9 插件包的显示端验证（`scripts/kde-plugin-check.sh`）

`packaging/plasma-wallpaper/io.github.oneincase.wallpaperem` —— **11 项断言全过**：

| 断言 | 结果 |
| --- | --- |
| 包结构（`metadata.json` + `contents/config/main.xml` + `contents/ui/{main,config}.qml`） | ✅ |
| `metadata.json` 必备键（`Id`/`Name`/`ServiceTypes=["Plasma/Wallpaper"]`/`X-KDE-ParentApp=org.kde.plasmashell`，键集照抄 `/usr/share/plasma/wallpapers/org.kde.image/metadata.json`） | ✅ |
| `kpackagetool5 --type Plasma/Wallpaper --install` | ✅ 落到 `~/.local/share/plasma/wallpapers/io.github.oneincase.wallpaperem` |
| `--list` 可见 / 卸载后不可见 | ✅（脚本跑完自动卸载，不留痕迹） |
| QML 显示引擎写出的帧 | ✅ 帧文件为品红时，`qmlscene`+Xvfb 截图取样得 `srgb(255,0,255)` |
| 帧文件缺失时兜底 | ✅ 取样得 `#101014`，且只产生设计内的 `QML Image: Cannot open`，不崩 |

过程中修掉的**真 bug**（记下来是因为它只在真 Qt 上才炸）：`main.qml` 的独立运行桩最初写成 `property string FramePath`，Qt 报 `Property names cannot begin with an upper case letter` —— QML 自定义属性不能首字母大写，配置键 `FramePath` 的大小写只存在于 Plasma 注入的 QObject 上，因此桩用小写、读取时显式映射。

**仍只是「显示端」**：真 plasmashell 里选为壁纸、引擎侧帧传输（DMA-BUF/socket）、多屏按实例分帧都未做（见 §8）。

## 7. 对方案的影响（已回填 [kde-adaptation-plan.md](kde-adaptation-plan.md) §2.5）

1. **§4.1 机制①的定义要改**：从「拿桌面层语义并保住图标」改为「拿桌面窗口语义（跳任务栏/全桌面/在普通窗口之下）」，**图标可见性另立一条路**（Plasma 6 Wayland 的 layer-shell `Background`，或 §4.5 壁纸插件）。X11 上无解，别承诺。
2. **`_NET_WM_STATE_BELOW` 从候选方案里删掉**（本机已证伪）。
3. **§4.3 的空闲检测数据源改序**：`ScreenSaver.GetActive` 在 Plasma 5.27 返回 `NotSupported`，不能作为主路径。
4. **§4.3 的每屏查询改序**：KScreen D-Bus 优先，`kscreen-doctor -j` 降为退路。
5. **冒烟脚本统一到真实用户总线**：`linux-kde-smoke.sh` 的 `dbus-run-session` 分支在 Docker/CI（无用户会话）里仍需保留，但**真机验证一律用 `kde-vm-session.sh`**。
6. **`find window` 一律按 `_NET_WM_PID` 反查 stacking**，不要用 `xdotool search --pid`（已修入脚本）。

## 8. 仍未验证 / 无法验证

- **Plasma 6 全部专属项**：layer-shell `Background` 层（本机 5.27 实测有 `zwlr_layer_shell_v1` **v3**，但 v5 与「Background 层里排到 plasmashell 桌面视图之下」仍未证）、`ext_background_effect_manager_v1`（5.27 实测未广告，符合预期）、portal `Wallpaper`(6.5)、Plasma 6 托盘 SNI、每虚拟桌面独立壁纸。需 Ubuntu 25.04 / Fedora KDE / openSUSE 的 arm64 或 amd64 机器。
- **P9 插件在真 plasmashell 里选为壁纸**：只验到「KPackage 能装能注册 + QML 能把帧画出来」（§6.3）；把插件设成当前桌面壁纸需要能正常登录的图形会话（无头 Xvfb 下的 plasmashell 只被用来做冒烟，不适合做交互式设置）。
- **引擎侧帧传输**：DMA-BUF/socket 那条路一行代码都还没有；`zwp_linux_dmabuf_v1` v4 只是证明协议可用。
- **KWin 5 的 `Layer` 窗口规则是否可用**：需在能弹确认框的交互会话里跑 `qdbus org.kde.KWin /Scripting …`，或直接写 `~/.config/kwinrulesrc` 后 `reconfigure` 再观察。
- **`global-hotkey` 在 KDE 下是否与 KWin 抢键**：需手工按键验证。
- **无边框窗口在 KWin 下的拖拽/缩放/阴影**：无头 Xvfb 里 `Meta+左键` 无法模拟。
- **图标实际可见性的人工确认**：像素哈希已证「壁纸覆盖图标区」，但「图标是否还能被看见/点中」需人工看 `screen-with-app.png`（已留在 VM 的会话目录里）。

## 9. 产物清单（均为未跟踪新文件）

| 文件 | 作用 |
| --- | --- |
| `scripts/kde-verify-preflight.sh` | 只读预检：系统/Plasma 版本/会话/外部命令/sudo 免密 |
| `scripts/kde-vm-setup.sh` | VM 上无人值守装 `kubuntu-desktop`（钉住 gdm3，不抢 GNOME 会话） |
| `scripts/kde-vm-sync.sh` | 宿主 → VM 同步仓库与 `../media-bridge`（排除 target/node_modules/.git） |
| `scripts/kde-vm-build.sh` | VM 上装构建依赖并 `cargo build --release --locked` |
| `scripts/kde-vm-verify-state.sh` | 恢复后体检：根分区 rw、ext4 无新错、包完整性、二进制存在 |
| `scripts/kde-vm-session.sh` | **核心**：真实用户总线上拉起 Xvfb+kwin_x11+plasmashell，提供 `probes` / `realbuscase` / `belowtest` / `smoke` / `shell` 五种模式 |
| `scripts/kde-probes.sh` | 机制探针（`PROBE 名 = 值`），不需要应用二进制 |
| `scripts/linux-kde-smoke.sh` | 有断言的 KDE/X11 冒烟：真机走真实用户总线、CI 无用户会话时自动退回 `dbus-run-session`；已在 VM 上 5/5 通过（见 §6.1） |
| `scripts/kde-wayland-globals.sh` | KWin 的 Wayland 全局清单探测（自起嵌套 `kwin_wayland --virtual` 或 attach 现有会话）；§6.2 的全部数据来自它 |
| `scripts/kde-plugin-check.sh` | P9 插件包验证：结构 → `kpackagetool5` 安装注册 → `qmlscene` 渲染取样 → 卸载（11 项，见 §6.3） |
| `packaging/plasma-wallpaper/io.github.oneincase.wallpaperem/` | P9 的 Plasma 壁纸插件（`metadata.json` + KConfigXT + `main.qml`/`config.qml` + 引擎契约 README） |
| `docs/kde-smoke-setup.md` | 装配说明与覆盖率对照 |
| `docs/kde-adaptation-plan.md` | 适配方案（§2.5 已回填本轮实测） |
| `docs/kde-plasma-constraints.md` | 上游约束调研（12 条硬约束 + 71 链接） |
| `docs/kde-verification-report.md` | 本文件 |
