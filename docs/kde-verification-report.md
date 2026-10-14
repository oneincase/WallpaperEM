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
| P9 插件在真实 plasmashell 里当壁纸 | ✅ 18 项断言全过，图标层逐像素不变 | 强（见 §6.5） |
| 引擎侧帧导出驱动插件（`frame_export`） | ✅ 端到端 19 项断言全过 | 强（见 §6.6） |
| 应用内「一键启用 KDE 插件」 | ✅ 后端+打包+前端落地，真机 38 项断言全过（含装成 deb 复验） | 强（见 §6.7/§6.8） |
| 设置页按钮的视觉确认（截图） | ⚠️ 本机抓屏恒全黑，未能留图 | 弱（环境限制） |
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

`packaging/plasma-wallpaper/io.github.oneincase.wallpaperem` —— **12 项断言全过**：

| 断言 | 结果 |
| --- | --- |
| 包结构（`metadata.json` + `contents/config/main.xml` + `contents/ui/{main,config}.qml`） | ✅ |
| `metadata.json` 必备键（`Id`/`Name`/`ServiceTypes=["Plasma/Wallpaper"]`/`X-KDE-ParentApp=org.kde.plasmashell`，键集照抄 `/usr/share/plasma/wallpapers/org.kde.image/metadata.json`） | ✅ |
| `kpackagetool5 --type Plasma/Wallpaper --install` | ✅ 落到 `~/.local/share/plasma/wallpapers/io.github.oneincase.wallpaperem` |
| `--list` 可见 / 卸载后不可见 | ✅（脚本跑完自动卸载，不留痕迹） |
| QML 显示引擎写出的帧 | ✅ 帧文件为品红时，`qmlscene`+Xvfb 截图取样得 `srgb(255,0,255)` |
| 帧文件缺失时兜底 | ✅ 取样得 `#101014`，且只产生设计内的 `QML Image: Cannot open`，不崩 |

过程中修掉的**真 bug**（记下来是因为它只在真 Qt 上才炸）：`main.qml` 的独立运行桩最初写成 `property string FramePath`，Qt 报 `Property names cannot begin with an upper case letter` —— QML 自定义属性不能首字母大写，配置键 `FramePath` 的大小写只存在于 Plasma 注入的 QObject 上，因此桩用小写、读取时显式映射。

**仍只是「显示端」**：真 plasmashell 里选为壁纸、引擎侧帧传输（DMA-BUF/socket）、多屏按实例分帧都未做（见 §8）。其中「真 plasmashell 里选为壁纸」已由 §6.5 补上。

### 6.4 前台窗口分类的数据源（`frontmost_kind`，X11）

本轮把 `wallpaper/linux.rs::frontmost_kind` 从「恒 `Unknown`」补成 X11 读 EWMH
`_NET_ACTIVE_WINDOW`（再看该窗口的 `_NET_WM_PID` 与 `_NET_WM_WINDOW_TYPE`）。
真机（Xvfb + `kwin_x11 --replace` + `plasmashell --no-respawn`）取证：

```
--- WM ---
_NET_SUPPORTING_WM_CHECK(WINDOW): window id # 0x20000b
--- ACTIVE (root) ---
_NET_ACTIVE_WINDOW(WINDOW): window id # 0x400011
--- ACTIVE pid/type ---
_NET_WM_PID(CARDINAL) = 62739
_NET_WM_WINDOW_TYPE(ATOM) = _NET_WM_WINDOW_TYPE_DESKTOP
WM_CLASS(STRING) = "plasmashell", "plasmashell"
--- stacking types ---
0x400011 pid=62739 type=_NET_WM_WINDOW_TYPE_DESKTOP
0x400027 pid=62739 type=_NET_WM_WINDOW_TYPE_DOCK, _KDE_NET_WM_WINDOW_TYPE_OVERRIDE, _NET_WM_WINDOW_TYPE_NORMAL
```

即：数据源存在、字段可读，分类结果为 `Desktop`（`_NET_WM_WINDOW_TYPE_DESKTOP` 命中）
→ 自动暂停决策表给出 `Play`（壁纸露着就该播）——从「恒 `Unknown`/`Keep`」变成
有依据的判定。分类逻辑本身是纯函数（`classify_frontmost`），5 个单测在
Linux 目标上通过；`cargo test --lib frontmost` 在 VM 上
`5 passed; 0 failed; 364 filtered out`。

注意两点边界：**Wayland 会话仍恒 `Unknown`**（协议无「当前活动窗口」全局概念，
KWin 的 D-Bus 在 5.27 上也不提供，只有需注入脚本的 `workspace.activeClient`）；
`pid` 读不到时判 `Other`（宁可少播也不漏暂停）。

另外确认了一个「面板也带 NORMAL」的边角：KWin 面板窗的 `_NET_WM_WINDOW_TYPE`
同时含 `DOCK` 与 `NORMAL`，判定取「含 `DOCK` 即 `Desktop`」是正确的（面板在前台
说明壁纸基本露着）。

### 6.5 P9 插件在**真实 Plasma 桌面**里被选为壁纸（决定性验证）

§6.3 只证明「QML 显示端行为正确」，证明不了「plasmashell 认不认这个包、认不认这些配置键、
帧到不到桌面、图标还在不在」。为此做了对照实验并复用**真实登录会话**（不新起合成器、不杀进程）：
Ubuntu 24.04.3 / Plasma 5.27.11 / X11 / 1024x768，plasmashell pid 2347 常驻 `:0`。

步骤：`org.kde.image` 铺**纯洋红**取基线截图 → 用官方脚本接口
（`qdbus org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.evaluateScript`，
与 `plasma-apply-wallpaperimage` 同一条路：`d.wallpaperPlugin = '<id>'` +
`d.currentConfigGroup = ['Wallpaper','<id>','General']` + `d.writeConfig(k,v)`）
把壁纸换成 `io.github.oneincase.wallpaperem`，帧文件是**同一张纯洋红** → 再截图 →
推序号并把帧换成**纯青** → 再截图。

| 断言 | 实测 |
| --- | --- |
| 基线背景是纯洋红 | 洋红占比 **0.9342** |
| 基线桌面区图标在画（非背景像素数，只统计桌面区 1024x718，排除面板） | **3645** |
| 插件壁纸生效 | 洋红占比 **0.9342**，plasmashell 2347 存活 |
| **桌面图标/面板图层逐像素保持** | 掩码差异 **0 像素**（非背景同为 3645） |
| **序号驱动换帧** | 换青后 青 **0.9342** / 洋红 **0.0039** |

`base.png` 与 `plug.png` 的 md5 相同（`1b0bcb8c7879972eef6a2d4bfc5a13d5`）——
本插件渲染的桌面与 `org.kde.image` 渲染同一张纯色图逐字节一致，换帧后立刻不同。
截图里洋红铺满桌面、左上角 `desktop.ini` 图标可见、底部面板与时钟正常
（证据：独立仓库 `docs/evidence/real-plasma/`）。

**结论**：壁纸插件方案在真桌面上成立——**壁纸层在图标层之下，图标与面板不被遮盖**。
这正是窗口方案（机制①/②）在 X11 上做不到的那一点，`P9` 因此从「演进彩蛋」升为战略路径。

复现：独立仓库 `bash scripts/plasma-real-test.sh`（18 项断言，自动发现 `:0` 或按 `DISP=` 自起会话；
跑完还原原壁纸并卸载插件）。脚本里为此修掉三个坑：截图前需用 `_NET_SHOWING_DESKTOP=1`
把压住桌面的普通窗口最小化（面板是 DOCK 不受影响）；已有 plasmashell 时第二个实例
抢不到 `org.kde.plasmashell` 会立刻退出，必须复用活会话；ImageMagick 掩码必须
「先把非目标色涂白、再把仍是目标色的涂黑」，顺序反了会把非背景像素数虚报成整块面积。

### 6.6 引擎侧帧导出（`frame_export`）驱动插件（端到端 19 项断言全过）

§6.5 证明「插件能当壁纸」，但帧还是帧泵写的。本轮把引擎这一端补上：`src-tauri/src/wallpaper/frame_export.rs`
按插件契约往固定路径写 PPM（原子 `tmp` + `rename`）与序号文件，并在插件模式下把自己那扇壁纸窗口
**整窗透明**。端到端自检 `scripts/kde-frame-export-check.sh`（自建 Xvfb + `kwin_x11` 会话、
web 类型动画壁纸、19 项断言）**全部通过**。

**为什么帧源不是「页面自抓帧」而是 X11 窗口抓帧。** 页面侧 `__wpCapture`（`drawImage(webglCanvas)`
+ `toDataURL`）在这台 Linux/WebKitGTK（Mesa 25.2.8 / llvmpipe 软栈）上返回**全黑**：miku 场景帧
99.96% 纯黑、中心像素 `(0,0,0)`；同一时刻屏幕上的桌面**有内容且在变**（`import -window root` 三次
md5 各不相同、应用日志 `particles live: 79 across 2 systems`）。同一个缺陷也让既有工具
`wallpaper_screenshot` 产出全黑预览（1088x680 JPEG 仅 4652 字节，目视全黑）——**这是既存问题，
不是帧导出引入的**。改用 X11 `GetImage` 抓那扇 `_NET_WM_WINDOW_TYPE_DESKTOP` 窗口后：
`xwd` 4099179 字节（header 107 / depth 32 / 1280x800），采样平均亮度 **192.4**、纯黑占比 **0.000**；
把该窗 `_NET_WM_WINDOW_OPACITY` 设为 0 之后再抓，仍是 **192.3 / 0.000** ——
**窗口整窗透明后后备存储内容还在**，插件模式成立。

**插件模式的窗口必须「透明」而不是「隐藏」**（实测对照，同一场景各 7 秒）：

| 窗口状态 | frames | written | skipped |
| --- | --- | --- | --- |
| 可见 | 41 | 41 | 0 |
| 已映射 + `_NET_WM_WINDOW_OPACITY=0` | 83 | 83 | 0 |
| `xdotool windowunmap`（隐藏） | 140 | **1** | **139** |

隐藏后 WebKitGTK 停掉 rAF，画面冻结；透明不影响出帧。所以 `hideWindow` 走的是「保持映射 + 透明度 0」。

**19 项断言里的关键几项**（原始输出：VM `/tmp/wpem-frame-export.log` 与自检日志）：

| 断言 | 实测 |
| --- | --- |
| 帧文件按契约生成 | `frame.ppm` 头 `P6\n1280 800\n255`，字节数 = 头 + w*h*3；`frame.seq` 自增 |
| 帧内容在变 | 8 秒内 md5 由 `fa2ec9a5…` 变为 `534b9624…`，非黑占比 **1.000** |
| 帧源是 X11 窗口抓帧 | `status.source = "x11-window"` |
| 插件模式生效 | `status.hidden_windows = 1`，X 侧 `_NET_WM_WINDOW_OPACITY=0` |
| 透明期间仍出帧 | 序号 60 → 218（8 秒，rAF 未被停） |
| 停止可逆 | `hidden_windows=0`，不透明度恢复 `4294967295`，序号冻结（219 → 219） |
| 停止不留垃圾 | 无 `*.tmp-*` 残留 |

**本轮修掉的一个真缺陷：`stop` 固定慢 12 秒。** 收尾走的是同步 Condvar 等待（`wait_exit`），
而它跑在 tokio worker 线程上——后台循环恰好需要这条线程才能被调度去读取消标记，于是每次停止
都**稳定等满 12s 上限再 `abort()`**（日志里 `帧导出心跳：… cancelled=false` 在停止后完全停摆、
循环来不及 `mark_done`）。现新增 `FrameExporter::stop_async`（`timeout(JoinHandle)` 让出线程）与
`start_global_async`/`stop_global_async`，MCP 处理器改走异步版；修后同一自检里循环立即退出
（日志 `帧导出循环退出` 与 `帧导出已停止` 同一毫秒级时间戳，不再出现 `12000ms 内未收尾` 告警），
19 项断言仍全过。同步 `stop()` 保留给应用退出等同步路径，注释里写明「别在 async 上下文里调用」。

复现：`ssh <vm> 'USE_XVFB=1 bash -s' < scripts/kde-frame-export-check.sh`
（依赖应用已构建；脚本自起 Xvfb + kwin_x11，造一张 web 类型动画壁纸，跑完停壁纸并删条目）。

### 6.7 应用内「一键启用 KDE 插件」（端到端 33 项行为断言全过）

设置页「通用 → KDE 集成」（仅 Linux 显示）多了「启用 KDE 插件」按钮：点一次就把插件包装进
plasmashell、用官方脚本接口把桌面壁纸切过去、同时起帧导出；再点「关闭并还原」走反向路径
（停导出 → 写回原壁纸 → 不透明度还回去）。后端是 `src-tauri/src/kde_plugin.rs`，三个 Tauri
命令与三个同名 MCP 工具（`kde_plugin_status`/`enable`/`disable`）走**同一条**代码路径，
所以 MCP 驱动的自检覆盖的就是按钮本身的产品路径。

插件包由 `scripts/sync-plasma-plugin.sh` 从独立仓库 `../WallpaperEM-Plasma` **单向**同步进
`packaging/plasma-wallpaper/io.github.oneincase.wallpaperem/`，再经 `tauri.conf.json` 的
`bundle.resources` 映射到 `$RESOURCE/plasma-wallpaper/`（真机构建后确认该资源目录存在，
`resolve_package_dir` 就是按这个顺序找包：环境变量 → 资源目录 → 源码树）。

真机自检 `scripts/kde-plugin-enable-check.sh`（新，344 行）：自起 `Xvfb :104` + `kwin_x11` +
`plasmashell`，全程只在自建会话里动 `appletsrc`/`app.db`，跑前备份、收尾还原（`set -u` 下早期
退出也安全）。为什么不用 VM 的真实 `:0`：它经常处于 kscreenlocker 锁屏，且真会话里切壁纸会
动到用户自己的桌面配置。**结果 33/33 PASS**，关键几项：

| 断言 | 实测 |
| --- | --- |
| 启用前基线 | `supported=true`、`session=x11`、`plasmashell=true`、`installed=false`、`exporting=false`、`engine_playing=true` |
| 启用返回值 | `installed=true`、`switched_desktops=3`、`export.running=true` |
| 插件包落位 | `~/.local/share/plasma/wallpapers/io.github.oneincase.wallpaperem/`，含 `contents/ui/main.qml` |
| 桌面确实换成本插件 | appletsrc 里 `wallpaperplugin=io.github.oneincase.wallpaperem` |
| 真出帧 | `frame.ppm` + `frame.seq` 生成、帧内容非全黑、序号在长（62 → 218） |
| 状态可读 | `status.active/exporting/enabled=true`、`installed_version=0.1.0` |
| 插件模式生效 | 引擎壁纸窗口 `_NET_WM_WINDOW_OPACITY=0`（整窗透明，不是隐藏） |
| 关闭可逆 | `restored=true`、`stopped=true`、原壁纸写回 `org.kde.image`、appletsrc 里已不是本插件、`exporting/active=false`、包保留（重启用不必重装）、不透明度恢复 `4294967295`、序号不再增长 |
| 收尾干净 | 自检条目已清理 |

复现（宿主机 rsync+构建后）：

```bash
ssh <vm> 'APP_BIN=/home/parallels/WallpaperEM/src-tauri/target/release/WallpaperEM bash -s' \
  < scripts/kde-plugin-enable-check.sh     # 38 项断言；SECS / XVFB_DISPLAY 可调
```

> 首次跑出 29/33，4 条失败全是脚本自身断言函数的 bug（`jget` 把 Python 的 `True` 当字面量
> 漏判、嵌套字段 `export.running` 取不到）——换成支持点号路径 + 小写布尔的 `jbool` 后 33/33。
> 另有一个环境限制：本机 Xvfb+kwin 下 `import`/`xwd` 抓屏拿到的永远是全黑（合成器把窗口内容
> 重定向了），所以设置页截图在**自建 Xvfb 会话里拿不到**；功能正确性靠上面的断言 + 前端
> `pnpm typecheck` + i18n 审计，真机 UI 目视确认留给有物理/远程桌面的会话。

### 6.8 装成 deb 之后的复验（33 → 38 项，并抓到两个发布级缺陷）

上一节的 33 项跑的是**开发态二进制**（`src-tauri/target/release/WallpaperEM`）——路径、权限、
资源布局都和用户机上的真实安装不同。补了 `scripts/kde-vm-deb.sh` 把最新版真装进 VM 再复验。

**为什么手工组装 deb**：tauri-cli 官方预编译产物没有 aarch64-linux（GitHub release 资产只有
x86_64/`cargo-tauri` 之类），所以按 tauri-bundler 的布局手工组装：

```
usr/bin/<productName>                              ← install -m 0755
usr/lib/<productName>/{renderer,assets,plasma-wallpaper,libsteam_api.so}
usr/share/{applications/<productName>.desktop,icons/hicolor/*/apps/*}
```

这正是 tauri 2.11.5 在 Linux 非 AppImage 下 `resource_dir()` 的语义（`tauri-2.11.5
src/path/desktop.rs`：`/usr/lib/<exe_name>`）。deb 包名/文件名与官方一致
（`wallpaper-em_2.2.0_arm64.deb`，`Package: wallpaper-em`），Depends/Recommends 从
`tauri.linux.conf.json` 解析后写进 control；产物 9.8M，`apt-get install` 走依赖解析装成。

**抓到的真缺陷（发布级，开发态永远看不到）**：`packaging/plasma-wallpaper/` 里的插件包文件是
`0600`（umask 077 的检出留下来的），`cp -a` 原样带进 deb → 装成 `root:root 0600`。普通用户点
「启用」时 `fs::copy` 立刻 `Permission denied`（自检报 `拷贝 … /README.md 失败: Permission
denied (os error 13)`），4 项红。为什么开发态测试看不见：那会儿源和目标是同一个用户，且
**git 只记 100644/100755，checkout 不会纠正工作区权限**。修法三处：

1. `scripts/sync-plasma-plugin.sh`：同步后 `find -type d chmod 755 / -type f chmod 644`（并写明原因）；
2. `scripts/kde-vm-deb.sh`：staging 之后再归一一次权限；
3. CI 的 deb 校验新增断言：`plasma-wallpaper/**/metadata.json`、`contents/ui/main.qml`、
   `renderer/index.html`、`libsteam_api.so` 必须存在于 deb 且权限是 `-rw-r--r--`。

**另外两个坑（都是「只有装机才出现」）**：

- `rsync --delete` 把 VM 上由 `build.rs` 生成的 `src-tauri/bundled/libsteam_api.so` 删了
  （宿主没有这个被 gitignore 的目录），于是 `--no-build` 装包报「资源缺失」。修法：sync 里
  `--exclude=/src-tauri/bundled/`；deb 脚本探测到资源缺失就强制构建并 `touch build.rs`
  ——否则 cargo 认为 build script 仍是新鲜的、不会重写 `bundled/`。
- `apt-get install` 对**同版本号**会「已是最新」直接跳过：修好权限后重装被跳过一次，装着的还是
  旧文件。改成 `apt-get install --reinstall`。

- `pkill -f "$APP_BIN_ABS"` 会**把调用者自己杀掉**：脚本最常见的调法是
  `ssh <vm> 'APP_BIN=/usr/bin/WallpaperEM bash -s' < 脚本`，而 `pkill -f` 匹配的是整条
  cmdline —— 调用者那条 shell 的 cmdline 里正好带着这个路径，于是自检把自己连着的 ssh
  会话干掉（现象：ssh 返回 255、输出半截）。改成按 `/proc/<pid>/exe` 精确匹配真正的二进制。
  残留进程还会污染下一次自检（`appletsrc` 断言假红），所以自检前必须清干净。

**结果**：`APP_BIN=/usr/bin/WallpaperEM bash scripts/kde-plugin-enable-check.sh` → **38/38 PASS**
（33 项行为断言 + 5 项安装产物自检），且 `kde_plugin_status.package_dir` 实测为
`/usr/lib/WallpaperEM/plasma-wallpaper/…`，即 `resolve_package_dir` 在真实安装下按资源目录找到了包；
安装后的 `metadata.json`/`main.qml` 对 `nobody` 用户可读；bundle 里确认含「KDE 桌面集成」
（`assets/index-1dTsR9_F.js`）与三个 `kde_plugin_*` 命令名（`assets/ui-qH7NIsJZ.js`）。

**新增 5 项「安装产物自检」**（合计 38 项）：前端 bundle 必须含「KDE 桌面集成」与
`kde_plugin_status`/`kde_plugin_enable`/`kde_plugin_disable` 三个命令名。前两个包在 HTML 外壳的
同级 `assets/` 目录里（**不是** `renderer/`），第一版断言只扫 `renderer` 导致假红 4 项 —— 这也是
「断言自己也会错」的例子。它专防「装完 deb 跑的
还是旧 renderer」这类假绿，**不依赖像素**，截图拿不到也能红。同一条自检现在也接进了 CI 的
`kde-smoke` 作业（先 `apt-get install ./artifacts/*.deb`，再用 `/usr/bin/WallpaperEM` 跑）。

**UI 目视确认的两次额外尝试（都失败，记录以免重复踩）**：

- AT-SPI：VM 装 `at-spi2-core` + `python3-pyatspi` 后能枚举到应用框架（`APP WallpaperEM` →
  `frame | WallpaperEM`），但 WebKitGTK 的网页内容**没有**进无障碍树（伴随
  `A connection to the bus can't be made`），所以无法用无障碍 API 断言/点击按钮。
- 截图：`import -window root` 在真实 `:0`（Xorg 1366x768 + plasmashell）能拍到 plasmashell，
  但我们的窗口区域是黑块；`xwd -root` 能出 4MB 数据、自写 XWD→PNG 解码后平均亮度 0.0
  （kwin 合成器重定向）。Xvfb+kwin 下 `import` 更是恒为 263 字节空白 PNG。

结论：Linux 上的 UI 目视确认需要物理/远程桌面会话；本轮以前端 `pnpm typecheck` + i18n 审计 +
「安装产物里确实有这段界面代码」的断言代替，功能正确性由 38 项端到端断言覆盖（§6.9 补上了第 38 项：产物必须是生产构建）。

### 6.9 用户实测抓到：手工构建漏了 `custom-protocol` → 主窗口连 `localhost:1420`（已修）

装完 deb 在真机打开，主窗口里是 Chromium 的错误页 **`Could not connect to localhost:
Connection refused`**（用户截图，2026-10-14）。查下来不是 UI 代码问题，而是**我给 VM 手工
构建的这份产物是「开发模式」二进制**：

- `src-tauri/tauri.conf.json` 的 `build.devUrl` 是 `http://localhost:1420`（Vite 开发服务器）；
- tauri 2.11.5 的 `build.rs:256-261`：`let dev = !custom_protocol;` 然后 `cargo:dev={dev}`；
  tauri-build 2.6.3 的 `is_dev()`（`src/lib.rs:425-429`）读 `DEP_TAURI_DEV`；
- 于是**不带 `--features tauri/custom-protocol` 的 `cargo build --release` 编出来就是开发模式**，
  主窗口去连 `localhost:1420` → 连接被拒。官方 `tauri build` 会自动带上这个 feature，所以
  **CI 出的官方 deb 不受影响**，受影响的只有 `scripts/kde-vm-deb.sh` 这条手工路径。

**证据（二进制层面，不依赖像素）**：

| 探针 | 开发模式（坏） | 生产构建（好） |
| --- | --- | --- |
| `grep -c 'localhost:1420' <exe>` | `1`（devUrl 被用上） | 也会有 1 条（配置总是内嵌） |
| `grep -c 'index-*.js' <exe>`（`assets/` 里的哈希文件名） | `0` | `≥1`（前端资源被编进二进制） |

实测坏产物：`/usr/bin/WallpaperEM` 与 `target/release/WallpaperEM` 均为
`localhost:1420 → 1`、`index-1dTsR9_F.js → 0`。

**为什么 37 项断言没抓住**：那 4 项安装产物断言读的是**资源目录里的文件**，没有看**二进制
本身**；而窗口加载失败并不影响 MCP 工具与帧导出，所以 33 项行为断言照样全绿。这正是「断言
覆盖不到的地方就是假绿」。

**修法**（三处一起堵）：

1. `scripts/kde-vm-build.sh`、`scripts/kde-vm-deb.sh` 的构建命令加
   `--features tauri/custom-protocol`（注释里写明依据的 tauri 源码行号）；
2. `scripts/kde-plugin-enable-check.sh` 新增第 **38** 项断言：**二进制里必须内嵌
   `assets/index-*.js`**（生产构建才有的痕迹），否则直接红并给出「这是开发模式构建」的提示；
3. 文档与 CHANGELOG 记下这条坑（本条）。

**教训**：手工复刻官方构建路径时，**编译期 feature 也是路径的一部分** —— 能跑通 MCP 自检
不等于交付形态正确；凡是「打包/安装」相关的验证，断言要落到**产物本身**（二进制/包内容），
而不是只看旁边放着的文件。

**这条新断言自己也假红过一次**（值得记）：第一版写成 `cat "$APP_BIN" | grep -qa "$ASSET_NAME"`，
而脚本开头是 `set -uo pipefail` —— grep 一命中就提前退出，`cat` 收 SIGPIPE 以 141 结束，
pipefail 把整条管道判成失败，于是「生产构建」明明成立却报红。改成让 grep 直接读文件
（`grep -qa -- "$ASSET_NAME" "$APP_BIN"`）即可。

**复验结果**：修好构建与断言后重新打包装机 →
`APP_BIN=/usr/bin/WallpaperEM bash scripts/kde-plugin-enable-check.sh` →
**38 项通过 / 0 项失败**，其中第 38 项输出「二进制是生产构建（内嵌 `index-CQlQn0nT.js`）」；
二进制 47.8 MB → **49.5 MB**（多出的约 1.1 MB 就是内嵌的前端资源）。

## 7. 对方案的影响（已回填 [kde-adaptation-plan.md](kde-adaptation-plan.md) §2.5）

1. **§4.1 机制①的定义要改**：从「拿桌面层语义并保住图标」改为「拿桌面窗口语义（跳任务栏/全桌面/在普通窗口之下）」，**图标可见性另立一条路**（Plasma 6 Wayland 的 layer-shell `Background`，或 §4.5 壁纸插件）。X11 上无解，别承诺。
2. **`_NET_WM_STATE_BELOW` 从候选方案里删掉**（本机已证伪）。
3. **§4.3 的空闲检测数据源改序**：`ScreenSaver.GetActive` 在 Plasma 5.27 返回 `NotSupported`，不能作为主路径。
4. **§4.3 的每屏查询改序**：KScreen D-Bus 优先，`kscreen-doctor -j` 降为退路。
5. **冒烟脚本统一到真实用户总线**：`linux-kde-smoke.sh` 的 `dbus-run-session` 分支在 Docker/CI（无用户会话）里仍需保留，但**真机验证一律用 `kde-vm-session.sh`**。
6. **`find window` 一律按 `_NET_WM_PID` 反查 stacking**，不要用 `xdotool search --pid`（已修入脚本）。

## 8. 仍未验证 / 无法验证

- **Plasma 6 全部专属项**：layer-shell `Background` 层（本机 5.27 实测有 `zwlr_layer_shell_v1` **v3**，但 v5 与「Background 层里排到 plasmashell 桌面视图之下」仍未证）、`ext_background_effect_manager_v1`（5.27 实测未广告，符合预期）、portal `Wallpaper`(6.5)、Plasma 6 托盘 SNI、每虚拟桌面独立壁纸。需 Ubuntu 25.04 / Fedora KDE / openSUSE 的 arm64 或 amd64 机器。
- **装成 deb 之后的复验**：✅ 已由 §6.8 补上（真装 `wallpaper-em_2.2.0_arm64.deb`，`/usr/bin/WallpaperEM` 跑 38 项断言全过，并因此修掉两个发布级缺陷（打包权限 0600、手工构建漏 `custom-protocol`））。仍未用**真实安装包**验证的只剩：设置页按钮的目视确认（见下条）、以及 AppImage/官方打包器（本机 arm64 无 aarch64 预编译 tauri-cli）。
- **P9 插件在真 plasmashell 里选为壁纸**：✅ 已由 §6.5 补上（真实会话 `:0`，18 项断言全过，图标层逐像素不变）。仍未量化的只有**长期帧率/合成开销**与**插件异常对 plasmashell 的影响面**（需真实使用观察），以及多屏按实例分帧、Plasma 6 实测。**应用内一键启用**已由 §6.7 补上（33 项断言，走的就是按钮那条产品路径）；剩下没做到的只是**设置页按钮的目视确认**——本机 Xvfb/KWin 下抓屏恒为全黑（合成器把窗口内容重定向了），`import`/`xwd` 都拿不到画面。
- **引擎侧帧传输**：DMA-BUF/socket 那条路一行代码都还没有；`zwp_linux_dmabuf_v1` v4 只是证明协议可用。
- **KWin 5 的 `Layer` 窗口规则是否可用**：需在能弹确认框的交互会话里跑 `qdbus org.kde.KWin /Scripting …`，或直接写 `~/.config/kwinrulesrc` 后 `reconfigure` 再观察。
- **`global-hotkey` 在 KDE 下是否与 KWin 抢键**：需手工按键验证。
- **无边框窗口在 KWin 下的拖拽/缩放/阴影**：无头 Xvfb 里 `Meta+左键` 无法模拟。
- **图标实际可见性的人工确认**：像素哈希已证「壁纸覆盖图标区」，但「图标是否还能被看见/点中」需人工看 `screen-with-app.png`（已留在 VM 的会话目录里）。

## 9. 产物清单（本轮新增；已随本轮提交落库）

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
| `scripts/kde-plugin-check.sh` | P9 插件包验证：结构 → `kpackagetool5` 安装注册 → `qmlscene` 渲染取样 → 卸载（12 项，见 §6.3） |
| `scripts/kde-plugin-enable-check.sh` | **应用内「一键启用」端到端自检**：自起 Xvfb+KWin+plasmashell（MCP 驱动，走按钮同一条产品路径），断言覆盖装包 / 切换 appletsrc / 真出帧 / 状态 / 恢复 / 清理（33 项）+ 安装产物自检（5 项，§6.8/§6.9） |
| `scripts/sync-plasma-plugin.sh` | 把独立仓 `../WallpaperEM-Plasma` 的插件包**单向**同步进 `packaging/plasma-wallpaper/`（真源在插件仓，本仓那份是打包副本，别在本仓直接改） |
| `src-tauri/src/kde_plugin.rs` | 「一键启用」后端：状态探测、装包、切 appletsrc、回滚、设置项持久化（3 个 Tauri 命令 + 3 个同名 MCP 工具） |
| `src/api/kde.ts` + 设置页「KDE 集成」分组 | 前端按钮与状态展示（仅 Linux 渲染），文案在 `src/locales/en-US.ts` |
| `packaging/plasma-wallpaper/io.github.oneincase.wallpaperem/` | P9 的 Plasma 壁纸插件（`metadata.json` + KConfigXT + `main.qml`/`config.qml` + 引擎契约 README）；**由 `scripts/sync-plasma-plugin.sh` 从独立仓单向同步**，并经 `tauri.conf.json` 的 `bundle.resources` 打进安装包（`$RESOURCE/plasma-wallpaper/`） |
| `scripts/kde-vm-deb.sh` | 在 VM 上按 tauri-bundler 布局**手工组装并安装 deb**（arm64 没有官方预编译 tauri-cli）：解析 `tauri.linux.conf.json` 的 resources/依赖 → 组装 `usr/bin` + `usr/lib/<productName>` → `dpkg-deb` → `apt-get install --reinstall`（§6.8） |
| `docs/kde-smoke-setup.md` | 装配说明与覆盖率对照 |
| `docs/kde-adaptation-plan.md` | 适配方案（§2.5 已回填本轮实测） |
| `docs/kde-plasma-constraints.md` | 上游约束调研（12 条硬约束 + 71 链接） |
| `docs/kde-verification-report.md` | 本文件 |
