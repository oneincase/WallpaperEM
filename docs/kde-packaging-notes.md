# KDE 打包与依赖声明（deb / AppImage / CI / Docker）

> 对应方案条目：`docs/kde-adaptation-plan.md:259` 的 **P8**（打包与依赖声明）。
> 相关文件：`docs/kde-smoke-setup.md`（真机装配）、`docs/kde-verification-report.md`（实测结论）、`scripts/linux-kde-smoke.sh`（冒烟断言本体）。
>
> 本文记录的改动：`src-tauri/tauri.conf.json`、`.github/workflows/build-linux.yml`、`Dockerfile.linux`、`scripts/linux-docker-entrypoint.sh`、本文件。
> `src-tauri/tauri.linux.conf.json` **已核对、无需改动**（理由见 §1.6）。
>
> ⚠️ 阅读约定：标「**未验证**」的条目表示本机（macOS 宿主机）拿不到证据，不要当成已验证结论；§6.2 有完整清单。

---

## 1. deb 依赖声明（`src-tauri/tauri.conf.json:57-75`）

### 1.1 `depends`（硬依赖，缺了装不上/起不来）

| 包 | 说明 | 依据 |
| --- | --- | --- |
| `gstreamer1.0-plugins-base` | 视频壁纸播放链路必需 element（`appsink`、`playbin`） | `src-tauri/src/lib.rs:1346-1350` 的 element→包名映射表 |
| `gstreamer1.0-plugins-good` | 音频输出 element（`autoaudiosink`） | 同上 |
| `gstreamer1.0-plugins-bad` | 部分解码/特效 element | 既有声明（本次未动） |
| `gstreamer1.0-libav` | 常见编码（H.264 等）的 libav 软解 | 既有声明（本次未动） |
| **`libayatana-appindicator3-1`** | SNI 托盘运行库。构建期用 `-dev`，运行期要的是带 `libayatana-appindicator3.so.0` 的这个包 | 方案 `docs/kde-adaptation-plan.md:71,224`；CI 装的是 `libayatana-appindicator3-dev`（`.github/workflows/build-linux.yml:79`）。打包器是否会自动追补**未能查证**（CLI 是预编译二进制），按「显式补齐」处理 |

### 1.2 `recommends`（可选建议，本次新增 4 条）

| 包 | 提供什么 / 为什么建议 | 缺了会怎样（代码里的真实退路） |
| --- | --- | --- |
| `plasma-workspace` | `plasmashell`、`plasma-apply-wallpaperimage`——KDE 静态壁纸回退链的**第 2 级**（`src-tauri/src/system_wallpaper.rs:790`） | 回退链少一级：由第 3 级 `qdbus` 兜底；再失败只 `warn` 并跳过（不影响应用内壁纸） |
| `kscreen` | `kscreen-doctor`——`kscreen_monitors()` 用它 `-j` 读 KDE 侧屏幕几何/缩放（`src-tauri/src/kwin.rs:760-773`） | 返回空 `Vec`，调用方回落 GDK 枚举屏幕（`src-tauri/src/kwin.rs:758-759` 明确写了「绝不因为 KDE 侧查询失败而拿不到任何屏幕」），功能不中断 |
| `qt6-tools` \| `qt5-tools` | `qdbus`——回退链**第 3 级** `evaluateScript`（`src-tauri/src/system_wallpaper.rs:797`） | 前两级都失败时无法同步锁屏/登录背景；应用内壁纸不受影响。包名归属见 §6.2 第 5 条（**未验证**） |
| `pulseaudio-utils` \| `pipewire-bin` | `parec`/`pactl`（PulseAudio）与 `pw-record`/`pw-cli`（PipeWire）：音频可视化与「正在播放」的采集、默认 sink 轮询实现在 media-bridge | 音频可视化/「正在播放」拿不到数据（方案 `docs/kde-adaptation-plan.md:77,340` 的开放项 ⓫：**KDE 下可用，但依赖未声明**——本次补的就是这一条） |

### 1.3 为什么不写 `suggests`

`tauri-utils 2.9.3` 的 `DebConfig`（`/Users/oneincase/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tauri-utils-2.9.3/src/config.rs:338`，其上有 `#[serde(rename_all = "camelCase", deny_unknown_fields)]`，同文件 `:337`）字段只有：
`depends`、`recommends`、`provides`、`conflicts`、`replaces`、`files`、`section`、`priority`、`changelog`、`desktopTemplate`、`preInstallScript`、`postInstallScript`、`preRemoveScript`、`postRemoveScript`。

**没有 `suggests`**，而且 `deny_unknown_fields` 会让未知键直接解析失败。所以「可选依赖」只能落在 `recommends`。

### 1.4 `recommends` 的真实语义（要跟用户讲清楚）

- Debian/Ubuntu 的 `apt install ./WallpaperEM_x.y.z_amd64.deb` **默认会装 recommends**（除非显式加 `--no-install-recommends`）。所以这 4 条的实际效果是「默认装上、但不参与依赖求解」。
- `plasma-workspace` 会带进整套 Plasma 桌面组件（数百 MB）。GNOME 用户装这个 deb 会额外拉一批 KDE 包——这是本文最需要发布决策的一条，取舍见 §6.3。

### 1.5 `a | b` alternatives 语法

`qt6-tools | qt5-tools`、`pulseaudio-utils | pipewire-bin` 用的是 deb control 的「或」语法。Tauri 只把这些字符串原样拼进 Depends/Recommends 字段，语法由 dpkg 解释，不需要 Tauri 侧支持。
但「原样输出」这一点本机验证不了（见 §6.2 第 3 条）。

### 1.6 为什么不动 `tauri.linux.conf.json`

`bundle.linux.deb` 写在**基础配置**里；Linux 构建时 `tauri.linux.conf.json` 只覆盖 `bundle.targets`（`["deb","appimage"]`）。平台配置是**合并**而不是替换，deb 段照旧生效——本地实测见 §6.1 第 [1] 项（`tauri.linux.conf.json` 的 keys 只有 `$schema`/`app`/`bundle`，base 的 deb 字段原样可读）。
把同一份依赖再抄进 linux 配置只会造出两个事实来源（改一处忘一处）。

---

## 2. 外部命令 → 缺了会怎样 → 装哪个包

应用在 Linux/KDE 上会 shell out 的命令，按「缺了之后的用户可见后果」排列：

| 命令 | 用在哪（代码位置） | 缺了会怎样（真实退路） | 该装的包 |
| --- | --- | --- | --- |
| `plasma-apply-wallpaperimage` | `src-tauri/src/system_wallpaper.rs:790`（回退链第 2 级） | 自动落到第 3 级 `qdbus`；三级全失败只 `warn` + 跳过（应用内壁纸照常） | `plasma-workspace`（recommends） |
| `qdbus` | `src-tauri/src/system_wallpaper.rs:797`（回退链第 3 级 `evaluateScript`） | 前两级都失败时锁屏/登录背景不同步 | `qt6-tools`/`qt5-tools`（recommends，**包名归属未验证**） |
| `parec` / `pactl` | media-bridge `crates/media-bridge/src/audio/linux.rs:5-6,27`（音频采集、默认 sink 轮询） | 音频可视化/「正在播放」拿不到数据（`pw-record`/`wpctl` 兜底） | `pulseaudio-utils`（recommends） |
| `pw-record` / `pw-cli` | 同上（PipeWire 优先路径） | 同上 | `pipewire-bin`（recommends） |
| `wpctl` | 默认 sink 轮询（`pactl` 失败后的兜底） | 同上 | `wireplumber`（**未列入**：只在 `pactl` 也缺时才用到） |
| `kscreen-doctor` | `src-tauri/src/kwin.rs:764` | `kscreen_monitors()` 返回空 → 回落 GDK 枚举（只少一份 KDE 几何/缩放信息） | `kscreen`（recommends） |
| `ffmpeg` / `ffprobe` | `src-tauri/src/` 的转码与探测（`ffmpeg.rs` 等） | 部分视频壁纸无法预处理/探测 | `ffmpeg`（recommends，既有） |
| `gst-inspect-1.0` | `src-tauri/src/lib.rs:1338`（`check_gstreamer_elements()` 体检） | 只影响体检报告（代码自带文案：`gst-inspect-1.0 不可用，无法检测 GStreamer 插件完整性；…`），不影响播放 | `gstreamer1.0-tools`（**未列入**：诊断工具，不是运行依赖） |

GStreamer「element → 包名」的真值来自代码自带的表（`src-tauri/src/lib.rs:1346-1350`）：
`appsink`/`playbin` → `gstreamer1.0-plugins-base`（视频无法渲染/媒体无法播放）；`autoaudiosink` → `gstreamer1.0-plugins-good`（无声）。三者都已在 `depends`。

---

## 3. AppImage：没有任何依赖求解

AppImage 不携带包元数据，用户双击时**不会**自动安装任何东西——§1 的 `depends`/`recommends` 对 AppImage 用户完全无效。

- §2 表里的命令，AppImage 用户必须自己装；缺了之后的降级行为与 deb 用户一致（表里列的就是真实退路）。
- 发布说明/README 里建议给出的安装行：

  ```bash
  sudo apt install plasma-workspace qdbus kscreen pulseaudio-utils ffmpeg \
    gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
  ```

  （`qdbus` 在 Ubuntu 上的实际提供包见 §6.2 第 5 条。）
- 另一个**未启用**的可选项：`bundle.linux.appimage.bundleMediaFramework: true`（tauri-utils `AppImageConfig`，`…/tauri-utils-2.9.3/src/config.rs:321-325`，官方注释标注 +15~35MB）会把 GStreamer 依赖塞进 AppImage。它只解决播放链路，解决不了 §2 的外部命令，且改产物体积/行为 → 本次不动，留给发布决策（§6.3）。

---

## 4. CI：新增 `kde-smoke` 作业（`.github/workflows/build-linux.yml:181` 起）

```yaml
kde-smoke:
  needs: [preflight, build-linux]
  continue-on-error: true
  runs-on: ubuntu-22.04
  name: "KDE-X11-smoke (x64)"
```

7 个步骤：检出代码 → 下载 x64 产物 → 安装 KDE/X11 冒烟依赖 → 校验 deb 依赖声明 → 选定被测二进制 → 跑冒烟 → 上传日志/截图。

设计取舍：

- **只跑 x64**：KDE 会话逻辑与架构无关，arm64 再跑一遍不产生新信息。
- **测「已上传的 AppImage 产物」而不是重新编译**：省掉第二次 20 分钟编译，而且验的是**用户实际拿到的那个二进制**。runner 无 FUSE，用 AppImage 运行时的 `APPIMAGE_EXTRACT_AND_RUN=1` 自解包直跑（与 build 作业里 linuxdeploy 的兜底开关同一机制）。
- **两个真断言**：
  1. `dpkg-deb -f *.deb Depends` 输出里必须真的有 `libayatana-appindicator3-1`——防「配置写了但没进 control 文件」这种最典型的假绿；
  2. `scripts/linux-kde-smoke.sh` 的桌面层窗口断言（`_NET_WM_WINDOW_TYPE_DESKTOP` + 堆叠顺序 + 存活无 panic，见 `docs/kde-adaptation-plan.md:302`）。
- 依赖包在 workflow 里现装（`xvfb dbus-x11 xdotool x11-utils imagemagick kwin-x11 plasma-workspace`），用 runner 而不是镜像。
- **`continue-on-error: true`**：这是新增作业，Plasma 5.27 底座上「真起 kwin/plasmashell 再断言」的路径还没有 CI 实测记录；先只标红本作业，**不阻塞 tag 发布**（发布门禁在 `preflight`）。CI 跑稳后删掉这一行即转为硬门禁。
- 日志与截图 `if: always()` 上传（artifact `kde-smoke-logs-<ref>`），失败时不用重跑就能看 `kwin.log`/`app.log`/`screen.png`。

---

## 5. Docker：一次构建 + 真断言冒烟

### 5.1 `Dockerfile.linux` 新增的包

apt 安装块里新增一行（`Dockerfile.linux:26-30`，包名在第 30 行、上面 4 行是理由注释）：

```
kwin-x11 plasma-workspace xdotool x11-utils x11-apps imagemagick
```

- `kwin-x11` + `plasma-workspace`：让容器里能真起 WM 与 plasmashell（`scripts/linux-kde-smoke.sh` 的前置检查会对它们报 `exit 2`）。
- `xdotool`/`x11-utils`：断言工具（`xdotool`/`xwininfo`/`xprop`）。
- `imagemagick`（`import`）与 `x11-apps`（`xwd`）：截图取证，后者是前者的兜底（`scripts/linux-kde-smoke.sh:230-231`）。
- **代价**：镜像大概 +1GB（Plasma 组件很多；这个数字是估计，本机无 docker 无法实测）。
- **不想要这一档时**：整段删掉即可——entrypoint 检测到缺件会打印 `SKIP`（退出码 2）而不是失败。

### 5.2 `scripts/linux-docker-entrypoint.sh` 的档位

| 变量 | 取值 | 行为 |
| --- | --- | --- |
| `SMOKE_TEST` | `0` | 两档全跳过（`scripts/build-linux-docker.sh` 传的就是这个变量） |
| `SMOKE_MODE` | `x11` | 只跑第一档（原有「起得来就算过」） |
| | `kde` | 只跑第二档（`scripts/linux-kde-smoke.sh` 真断言） |
| | `all`（默认） | 两档都跑 |
| `KDE_SMOKE_STRICT` | `1` | 第二档断言失败（rc=1）时让本次构建失败；默认 `0` |

第二档退出码语义：`0`=PASS / `1`=FAIL（断言失败）/ `2`=SKIP（前置缺失，不算失败）。
失败时打印 `---- KDE 冒烟：FAIL（rc=1）----`；`KDE_SMOKE_STRICT=0` 时追加一行说明「不让本次打包失败；要当门禁用 `KDE_SMOKE_STRICT=1` 重跑」。

两个必须记住的坑：

1. **外层不能再套 `xvfb-run`/`dbus-run-session`**：`linux-kde-smoke.sh` 自己起 Xvfb 与私有总线；套一层会变成「假 KDE 会话」（方案 `docs/kde-adaptation-plan.md:296`）。脚本里已写明这条注释。
2. **容器里的 `/tmp` 随容器消失**：冒烟的完整日志与截图都在 `/tmp/kde-smoke.*/`（`KEEP_LOGS=1` 才保留）。entrypoint 已把它们拷到 `/out/kde-smoke/` 与 `/out/kde-smoke-screen.png`——`/out` 是 `scripts/build-linux-docker.sh` 唯一挂出来的目录。

**为什么 `SMOKE_MODE` 默认是 `all`**：`scripts/build-linux-docker.sh` 本次未改（也不在写作用域），它只传 `SMOKE_TEST`。默认值必须让「什么都不改」也跑上第二档，否则 Docker 里的 KDE 档等于死代码。

---

## 6. 本地验证与未验证项

### 6.1 本机实跑（宿主 macOS，真实输出）

```
########## [1] tauri conf JSON 解析 + 关键字段 ##########
src-tauri/tauri.conf.json: JSON OK  keys=['$schema', 'productName', 'version', 'identifier', 'build', 'app', 'bundle', 'plugins']
src-tauri/tauri.linux.conf.json: JSON OK  keys=['$schema', 'app', 'bundle']
bundle.linux.deb.depends    = ['gstreamer1.0-plugins-base', 'gstreamer1.0-plugins-good', 'gstreamer1.0-plugins-bad', 'gstreamer1.0-libav', 'libayatana-appindicator3-1']
bundle.linux.deb.recommends = ['gstreamer1.0-pipewire', 'ffmpeg', 'kscreen', 'pulseaudio-utils | pipewire-bin']  # 2026-10-14 裁决后撤下 plasma-workspace 与 qt6-tools|qt5-tools，理由见 §6.3
bundle.linux.appimage       = {}
tauri.linux.conf bundle.targets = ['deb', 'appimage']

########## [2] bash -n ##########
bash -n scripts/linux-docker-entrypoint.sh -> OK (exit 0)

########## [3] workflow YAML 解析（Ruby psych）##########
build-linux.yml: YAML OK (psych 3.1.0)
jobs = ["preflight", "build-linux", "kde-smoke"]
kde-smoke: needs=["preflight", "build-linux"] runs-on="ubuntu-22.04" continue-on-error=true steps=7
  build-linux artifact name = WallpaperEM-linux-${{ matrix.label }}-${{ github.ref_name }}
  kde-smoke download artifact name = WallpaperEM-linux-x64-${{ github.ref_name }}
=> artifact 名完全一致（x64 摊）：产物能被下载到

########## [4] Dockerfile.linux：按 Docker 语义抽 RUN + bash -n ##########
抽出 3 行（3 条 RUN 拼接结果）-> bash -n OK (exit 0)
  apt 新增包: kwin-x11 plasma-workspace xdotool x11-utils x11-apps imagemagick
```

[4] 的做法：按 Docker 语义（先去掉注释行、再把 `\` 续行拼成一条）抽出 3 条 `RUN`，拼成的 shell 片段过 `bash -n`。这验证的是「`RUN` 拼接后仍是合法 shell」，**不是** Docker 真构建。

[5] 入口脚本冒烟分支行为测试：把 `scripts/linux-docker-entrypoint.sh:37-83` 整段抽出来，stub 掉 `dbus-run-session` 与 `linux-kde-smoke.sh`（用一个按环境变量返回 0/1/2 的假脚本），在宿主 bash 下跑 7 个档位（临时 harness 在 `/tmp/wpem-harness`，未入库）：

```
t1_default              rc=0   OK   两档都跑，KDE 档 PASS
t2_kde_fail_nonstrict   rc=0   OK   KDE 档 FAIL，但 KDE_SMOKE_STRICT=0 → 构建不失败
t3_kde_fail_strict      rc=1   OK   KDE_SMOKE_STRICT=1 → 构建失败
t4_prereq_missing       rc=0   OK   KDE 档 SKIP（rc=2 不当失败）
t5_mode_x11             rc=0   OK   只第一档
t6_mode_kde             rc=0   OK   只第二档
t7_smoke_test_0         rc=0   OK   两档全跳（无任何冒烟输出）
```

同 harness 还验证了取证回传：造出 `/tmp/kde-smoke.TEST/{kwin.log,app.log,screen.png}` + `/tmp/kde-smoke-screen.png` 后跑默认档，`/out` 下得到
`/out/kde-smoke/{kwin.log,app.log,screen.png}`、`/out/kde-smoke-screen.png`、`/out/kde-smoke.log`、`/out/smoke-test.log`。

> 顺带修掉一个真实可移植性缺陷：`echo "…（rc=$kde_rc）----"` 里变量紧邻全角 `）`，在 macOS 的 bash 3.2 下会被当成变量名的一部分（`kde_rc\uFFFD: unbound variable`）。已改为 `${kde_rc}`。容器里是 bash 5 不受影响，但花括号让它在任何 bash 上都对。

### 6.2 未验证（本次拿不到证据，别当成已验证）

1. **Docker 构建本身、容器内 KDE 冒烟**：宿主是 macOS，无 docker。§5.1 的「+1GB」是估计值。
2. **GitHub Actions 的 `kde-smoke` 作业**：无 runner。YAML 解析通过 ≠ Actions 接受这个作业；`continue-on-error` 的实际效果、artifact 下载、`APPIMAGE_EXTRACT_AND_RUN=1` 直跑 AppImage，都只能在首次真跑时确认。
3. **deb control 文件里 depends/recommends 的最终字符串**：打包器是 npm 预编译的 tauri CLI，源码不在本机 registry 快照里，看不到它怎么写字段（`a | b` 是否原样、长字段是否折行）。
4. **容器里 KDE 组件能否以 root 起**：Docker 默认 root，`kwin_x11`/`plasmashell` 对 root 会话可能告警甚至拒绝——这正是第二档默认不因断言失败而让构建变红（`KDE_SMOKE_STRICT=0`）的原因。
5. **包名是否真的提供对应命令**：本机没有可 `apt` 的 Linux 环境。具体存疑：Ubuntu 上 `/usr/bin/qdbus` 也可能来自 `qtchooser`（另有 `qttools5-dev-tools`/`qt6-tools-dev-tools` 提供 `/usr/lib/qt5/bin/qdbus`）；`plasma-apply-wallpaperimage` 是否确实在 `plasma-workspace` 包内也未在本机核对。**recommends 写错只会「没帮上忙」，不会让安装失败**（这也是把它们放 recommends 而不是 depends 的理由之一）。
6. **Plasma 6.5+ 的 portal 回退链第 1 级**：与本文件无关（属代码 task），此处不重复。

### 6.3 需要发布决策的两点

1. **`plasma-workspace` 是否留在 recommends**（`src-tauri/tauri.conf.json:70`）：
   - 留着：apt 装 deb 的 GNOME 用户会额外拉整套 Plasma 桌面组件；
   - 挪出去：KDE 用户少一条自动依赖（但 portal / `qdbus` 仍能工作，只是回退链少一级）。
   - 两个方向的改动都是一行。建议：发布说明里写明 `--no-install-recommends` 这个逃生口，再决定是否保留。
   - **裁决（Lead，2026-10-14）：已撤下 `plasma-workspace` 与 `qt6-tools | qt5-tools`，只保留 `kscreen` 与 `pulseaudio-utils | pipewire-bin`。** 理由：Plasma 用户机器上 `plasma-workspace`/`qdbus` 本来就在（KDE 桌面自身依赖），把它们写进 recommends 只对**非 KDE 用户**产生「拉一整套 Plasma / Qt 工具链」的副作用，而那条回退链对非 KDE 用户本来就用不上（§2 的表逐条已说明：命令缺失只降级、不报错）。`kscreen`（`kscreen-doctor`）与音频的 `pactl`/`pw-record` 体积小且非 KDE 也可能有用，故保留。撤下的两条在 §2 表里仍列出「缺了会怎样」，只是不再自动安装。
2. **AppImage 是否打开 `bundle.linux.appimage.bundleMediaFramework: true`**（`…/tauri-utils-2.9.3/src/config.rs:321-325`，官方标注 +15~35MB）：只解决播放链路的 GStreamer 自带，解决不了 §2 的外部命令。本次**未打开**（改产物体积与行为，超出 P8 范围）。
