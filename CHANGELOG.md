# 更新日志 / Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 格式。

## [Unreleased]

### ✨ 新增 / Added

- **应用内无缝更新（下载进度 → 静默安装 → 重启即新版）**：更新流程从「下载安装包 →
  手动打开安装器」升级为官方 `tauri-plugin-updater`：设置 → 关于 里检查更新后，直接
  在应用内下载（实时进度条）、校验 minisign 签名、按平台原地静默安装，最后点
  「重启并完成更新」即进入新版（Windows 由 NSIS 安装器自动重启）。发版 CI 同步产出
  updater 签名产物与 `latest-<平台>.json` 更新清单；已知限制：deb 安装的 Linux 用户
  不支持应用内升级（走「前往下载页」兜底）。

### 🛠 改变 / Changed

- 更新检查从 GitHub API（未认证每小时 60 次限流）改为直读 Release 里的更新清单端点；
  「关于」页移除「打开安装包」流程，改为「下载更新并安装 / 重启并完成更新」。

## [v1.1.0] - 2026-09-25

### ✨ 新增 / Added

- **同步 webwallgl 1.4.1 渲染核心**：
  - **WE 官方素材通路（local-assets）**：本机安装 Wallpaper Engine 时，场景壁纸的
    粒子精灵、光效/渐变贴图、法线参考与文字字体改用官方原版像素渲染（此前为内置程序化
    复刻，观感接近但不完全一致）。自动探测所有 Steam 库的
    `wallpaper_engine/assets`（支持 libraryfolders.vdf 多库），也可在「设置 → 性能」
    手动指定 assets 根；默认开启，探测不到素材时仅一次轻量请求、零额外开销，可随时关闭。
  - 随库更新获得：资源分辨率倍率与压缩纹理直传（最坏单墙纹理显存 1721MB → 332MB
    @省电档）、CPU 副本上传即释放；程序化粒子贴图（雨/雾/水花/涟漪/光轴/气泡）按官方
    素材实测重建、序列帧帧间混合与 3D 旋转对齐官方；TEXV0004 旧容器、合成源层缩放、
    DXT1 透明黑块、BGM 静音失效/重挂丢音量、帧率上限相位调度等一批观感与稳定性修复。
- **网页壁纸外部滚轮 / 触控板手势注入（pushWheel）**：壁纸在桌面图标下方时收不到
  滚轮事件，网页壁纸（全景、OrbitControls、幻灯片等）的滚动/缩放/翻页此前全部不可用。
  现在由宿主捕获系统级滚轮推进渲染器：macOS 走 CGEventTap（触控板双指滚动给像素级
  连续 delta；**双指捏合缩放**以系统合成的 ctrl+滚轮原样转发，首次使用网页壁纸时按需
  启动并请求「输入监控」权限，拒绝后可随时在系统设置授权、自动重试自愈），Windows 走
  WH_MOUSE_LL 低级钩子（无需权限，同样识别捏合）；仅在播放网页壁纸、非交互态且桌面
  活动时转发，交互态仍走窗口真实事件。Linux 暂无全局手势源。
- **多显示器管理（分期 P1）**：新增「显示器」页（侧边栏 → 系统）：布局缩略图 + 每屏卡片
  （名称 / 分辨率 / 倍率 / 当前壁纸），单屏「更换壁纸」「停止」「同步到所有屏」与
  「全部停止」；**统一 / 独立双模式**开关（统一 = 应用即刷全部屏；独立 = 每屏各自指定）。
  独立模式下本地库 / 详情 / 工坊 / 发现的「应用」弹出目标屏选择（记住上次选择）；显示器页
  「更换壁纸」锁定目标屏后跳本地库挑选。后端：`wallpaper_apply_item` 支持 `display_id`
  按屏应用（MCP `wallpaper_apply` 同步支持 `displayId`），新增 `wallpaper_displays_list` /
  MCP `displays_list`（id / 名称 / 位置 / 倍率 / 主屏 + 每屏会话摘要）；**显示器 id 改为
  跨重启稳定标识**（macOS 用显示器 UUID 哈希，替代重启后会漂移的 CGDirectDisplayID，
  存量会话启动时自动迁移）——多屏重启后壁纸不再错位；拔掉的屏插回来时精确恢复该屏
  自己的壁纸（不再退化为「最近一次全局配置」）。应用 / 停止 / 热插拔推送
  `sessions-changed` / `displays-changed` 事件，「已应用」徽章与显示器页实时对齐。
- **切换列表 + 自动轮播（分期 P2）**：新增「切换列表」页（侧边栏 → 库）：列表卡片
  （数量 / 间隔 / 随机标记 / 使用中徽章 / 封面条）与编辑弹窗（改名、间隔、**随机播放**、
  条目**拖拽排序** / 增删、从本地库搜索添加）；本地库卡片与详情页新增「加入切换列表」。
  运行条显示当前列表、进度与**倒计时**，提供 上一张 / 下一张 / 暂停轮播 / 停止轮播；
  托盘新增「轮播」子菜单（当前项 + 上一张 / 下一张 / 暂停），全局快捷键 ⌘⇧R 暂停/恢复轮播
  （⌘⇧P 是暂停壁纸渲染，两回事）。引擎侧：**手动切换重置轮播计时**（不再固定节拍叠加）；
  暂停轮播持久化、恢复后满间隔再切；随机采用**洗牌队列**（一轮内不重复、可回退，跨轮
  不与上一张连续重复）；切换/激活时自动剪掉文件已丢失的条目；删除激活列表会一并停止轮播。
  设置页新增「轮播」组（新建列表的默认间隔 / 默认随机）。MCP 同步补齐
  `playlist_list/get/create/update/delete/apply/stop/status`、`wallpaper_next/prev`、
  `wallpaper_rotation_set` 工具。DB 迁移 v10（playlists 加 shuffle / updated_at）。
- **独立模式每屏轮播 + 仅充电时轮播（分期 P3）**：独立模式下每块屏可**各自绑定**
  一个切换列表（或「固定不轮播」），按各自的列表、进度与节奏独立轮播 —— 显示器页
  每屏新增「轮播列表」绑定选择，卡片直接显示该屏进度与倒计时并可单屏 上一张/下一张；
  切换列表页「启用」在独立模式下弹出目标屏选择，卡片显示「已绑定：显示器名」。
  `wallpaper_next/prev` 支持 `displayId` 单屏切歌（缺省按模式驱动全部活跃上下文）；
  手动应用壁纸自动解绑该屏（回到固定单张）；删除列表会清掉它的全部绑定。
  设置页「轮播」组新增**仅充电时轮播**：电池供电时暂缓自动切换、手动切换不受影响
  （macOS 用 IOPowerSources 读供电状态；Windows/Linux 暂不拦）。
  引擎侧轮播状态重构为「上下文」（列表快照 + 进度 + 洗牌队列），统一/每屏共用同一套
  读写与走步逻辑（走步纯函数化并覆盖单测）。MCP 新增 `display_binding_set` 工具。
- **无缝切换 + 渐入过渡**：换壁纸（手动 / 定时轮播）改为「旧窗保持播放 → 新窗后台
  加载 → 首帧就绪 → 0.7s 渐入 → 渐入走完才收旧窗」——**资源没准备好、首帧没出来
  之前，屏上一直是上一张壁纸**，慢加载（大 scene.pkg 冷启动 30s+）不再露出加载
  空白；新壁纸**加载失败时旧壁纸留在屏上**（不再换成错误占位图），并推送
  `wallpaper-load-failed` 事件；切换采用**叠化渐入**（渲染器透明背景 + wrap opacity
  0→1，就绪后渐显），渐入前旧窗音量归零避免双声；快速连切时未就绪的半成品直接作废、
  只加载最后一张。实现为「窗口双变体交替」（基 label 与 `-b` 后缀交替承担新旧角色，
  逻辑侧 label 语义不变），全平台统一路径（不再 macOS 销毁重建 / 其它就地导航分叉）；
  首帧判定复用截图那套 ready 回流（含条目归属校验与 95s 超时兜底）。
- **壁纸 ID 搜索 / ID 直下载**：工坊与本地库搜索框输入纯数字即按创意工坊 ID 精确
  直达（本地库同时支持 ID 子串模糊）；下载页新增**壁纸ID下载**（工具栏内联输入，
  回车即下）——入队零元数据依赖，steamcmd 按 ID 直取，**国内免代理环境下也能稳定
  下载**（详情接口被墙不影响内容获取）；下载任务对网络瞬断类失败（下载/启动工具/
  IO）**自动重试两轮**，验证码/登录类不重试。
- **类型筛选新增「预设」**（`Preset` 标签，与 场景/视频/网页 同级）：一键直达可玩
  预设类壁纸（如载具预设场景）。注意与「题材 → 载具」（`Vehicle`，载具主题壁纸）
  是两个维度；本地库多选按钮正名「开启多选」。
- **切换列表并入本地库**：独立「切换列表」页移除，功能全部内联进本地库 ——
  顶部 chips 行切换/新建列表（点击即筛选该列表条目），列表上下文条内联改名/调
  间隔/切随机/启用轮播/删除；卡片开启**选择模式**后点选（对勾圆章 + 描边高亮），
  底部浮动操作条一键批量加入列表 / 移出列表，全程**零弹框**（锚定下拉菜单 +
  内联输入）；轮播运行指示与快捷控制（上一张/下一张/暂停/停止）也合并进本地库。
- **显示器页视觉重做**：布局舞台改为「微缩屏 + 真实壁纸封面」（主屏高亮环 +
  浮动徽标、轮播中脉动点与距下次切换的细进度线、悬停抬升微交互）；设备卡改为
  封面 + 信息 + 轮播胶囊（倒计时 + 单屏切歌 + 进度条）+ 紧凑操作簇；统一/独立
  模式改为滑块分段控件；「轮播列表」绑定改为锚定菜单。

### 🔧 变更 / Changed

- **全平台媒体桥接统一为 [media-bridge](https://github.com/oneincase/media-bridge)**：
  「正在播放」元数据/封面与系统音频频谱改由同一个进程内 Rust 引擎提供，对壁纸侧
  契约（`/now-playing`、`/audio-stream`、`/media-command`）零改动。
  - **Linux 新支持**：MPRIS「正在播放」此前已接入但封面/可用性不稳，系统音频采集
    （频谱可视化）此前完全不支持；现在 Linux 走 MPRIS + PulseAudio/PipeWire
    monitor（`parec`/`pw-record` 自动探测，无需虚拟声卡或立体声混音），三平台
    元数据 / 封面 / 播放控制 / 频谱行为统一。Windows「正在播放」此前因可用性闸口
    未打开实际不生效，现随统一引擎一并生效。
  - macOS 15.4+ 的 MediaRemote 权限通道改为内嵌 helper（运行时落盘经
    `/usr/bin/perl` 代读），不再随包分发第三方 `mediaremote-adapter`；
    构建时由 cargo 自动先编 helper，应用包仍是单文件。
  - 播放控制改为「能力位 + 回执」：播放器不支持的操作不会假成功；频谱保留旧版
    AGC 动态增益与峰值保持观感（FFT/采集下沉到 media-bridge）。
  - 新增能力储备：歌词（本地/内嵌/LRCLIB 在线）、循环/随机/定位/音量等扩展
    控制命令、远端封面下载（Linux），后续版本接到壁纸 wire。
- **本地库卡片封面改为「本地优先」**：壁纸目录里已有 `preview.jpg`/`preview.gif` 就
  直接用本地那份（经内容服务器下发），不再优先挂 Steam CDN 的远端图 —— 离线可用，
  卡片也不会因为 CDN 抽风而裂图。目录里确实没有封面的条目仍先用远端图顶上，同时由
  后台任务取一份写回壁纸目录（png / 静态 webp 转成 jpg，动图 gif 原样保留），下次
  进库就是本地那张。**不覆盖**已有封面（WE 工程自带、用户手放的都保持原样）。

### 🐛 修复 / Fixes

- **主窗口最小化现在与关闭一样立即释放内存**：黄色最小化按钮 / ⌘M 此前只把
  窗口藏起（主界面那几十～几百 MB 的 React WebContent 进程常驻），要等系统内存
  压力才回收。现在最小化即销毁窗口、结束其 WebContent 进程，内存立刻归还；从
  Dock（Reopen）/ 托盘重开时按原配置重建。macOS 上最小化只触发 NSWindow
  miniaturize、tauri 不发 Resized 事件，故改用 `NSWindowDidMiniaturizeNotification`
  通知监听（已验证重建后的主窗口正常渲染、不空白）。
- **切换壁纸后旧 WebContent 进程与内存可靠回收**：macOS 日常换壁纸此前走
  「同窗口整页导航」，连续切换（尤其视频↔4K 场景）时旧页的 WebGL 上下文 / JS
  堆回收不干净，新页加载失败、渲染器最终卡死（真机复现：场景→视频正常，
  视频→4K 场景即冻死），残留以 `http://127.0.0.1:<port>` 记名的 WebContent
  进程只能手动杀。现改为「销毁旧窗口（显式结束其 WebContent 进程）→ 同名
  重建」，进程数恒定、旧内存实打实归还。非 macOS（WebView2 / WebKitGTK 的导航
  会连带销毁旧文档）仍走同窗口导航，导航失败时兜底销毁重建。

## [v1.0.0] - 2026-09-15

### ✨ 新增 / Added

- **渲染质量三档设置**：设置页新增「性能」标签，可独立调节**抗锯齿**（关 / FXAA /
  MSAA×2 / MSAA×4）、**粒子质量**与**后处理质量**（高 / 中 / 低 / 关）。档位存为全局
  设置，对运行中的壁纸**热生效、不重挂载**（webwallgl 1.3.23+ `setQuality` 部分更新），
  重挂载路径（切清晰度/恢复）也会保持当前档位。
- **壁纸作者名片**：工坊条目与已下载/已上传的本地壁纸，详情页显示作者 Steam 头像与昵称，
  可点放大镜按作者浏览其作品。project.json 没有作者字段——作者由工坊条目的
  creator（SteamID64）经后端抓 Steam 资料页解析（XML 优先、HTML 兜底，资料私密或
  解析失败则不显示）。DB v9 新增 `author_profiles` 缓存表：成功结果缓存 7 天、
  失败负缓存 12 小时，避免重复请求。
- **工坊上传新增网页版路径**：上传时可选择「Steam 客户端直传（ISteamUGC）」或
  「网页版上传」。网页版会把内容暂存到持久目录（scene 工程贴图同样自动转 `.tex`），
  用系统浏览器打开 Steam 工坊新建/编辑页并在文件管理器中定位暂存目录，浏览器里提交后
  即可发布——适合 ISteamUGC 直传不可用的账号/环境。
- **工坊描述 BBCode 富文本渲染**：工坊作者用 `[h1]`/`[b]`/`[url=]`/`[img]`/`[list]`
  等 BBCode 排版的描述，详情页现在按富文本展示而非原文标记。自写小解析器产出 React
  元素（结构上免疫 XSS），所有链接经系统浏览器打开，未识别标签保留其文本内容。
- **WE 属性面板大幅增强**：
  - 按 WE 语义支持 `type=group` 可折叠分节；`condition` 条件显隐按当前草稿即时联动
    （组条件作用整节），并提示「N 项因条件隐藏」；
  - 面板内搜索过滤（分节名命中保留整组），显示匹配数；
  - 渲染属性文案里的分隔图/赞助图/示意图（`<img>`，外链直用、包内路径走内容服务器，
    qpic 等图床自动去 Referer；`<a>` 包裹的图点击走系统浏览器），显示名保留作者的
    `<br>` 中英对照换行；
  - 面板顶部显示作者行。
- **添加壁纸目录支持多选**：可一次选择多个文件夹，并跨多次选择追加到「待添加」列表、
  逐个移除，确认后统一以引用模式入库（不复制文件）；含子目录时递归扫描其中的壁纸工程。
- **全局浅色 / 深色 / 跟随系统主题**：主窗口与独立的壁纸设置窗口共享同一主题，主窗口
  切换后其他已开窗口实时跟随，无需重开。

### 🔧 变更 / Changed

- **清晰度设置改为四档相对倍率，默认「自动」**：原三档（省电 0.8 / 标准 1 / 高清 2，
  绝对 DPR）改为 **自动 0（=屏幕像素比，Retina 原生清晰）/ 省电 0.75 / 标准 0.85 /
  高清 1（=100% 像素比）**。存储值是相对设备 `devicePixelRatio` 的倍率，渲染页换算成
  webwallgl 库的绝对 DPR——配合库 1.3.22+「目标 DPR 可高于宿主上报值」，修复壁纸宿主
  WKWebView 把 `devicePixelRatio` 报成 1 时高清档仍被钉在逻辑像素、3.5K/Retina 屏
  只渲染物理面积 1/4 的问题。
  - DB v8 迁移：旧绝对 DPR 就近映射（2→1 高清观感一致、1→0.85、0.8→0.75），
    含每壁纸覆盖 `play_cfg:*`；
  - 设置页、每壁纸播放设置、托盘菜单三处同步为四档；i18n 补「自动 / Auto」。
- 本地库卡片封面新增文件大小徽标；卡片标题两行截断在 WKWebView 下加 max-height
  硬封顶，修复悬浮抽屉动画/虚拟列表重挂时偶发露出第三行空行。

## [v0.5.2] - 2026-09-13（重发布，合并原 v0.5.3 全部修复）

### ✨ 新增 / Added

- **本地壁纸批量导入（递归扫描 + 引用模式）**：「导入文件夹」现在会递归扫描所选目录树，
  每个带 `project.json` 的目录各自导入为一张壁纸（上限 500 个）；引用模式下壁纸内容
  **留在源目录、零副本**（库条目登记 `source_path`），同一目录重复导入幂等。拖拽导入
  同样支持目录树扫描。
- **创意工坊上传**：基于 Steamworks SDK（ISteamUGC）的完整上传链路——内容暂存（scene
  贴图自动转 `.tex`）、`CreateItem` / `SubmitItemUpdate`、实时进度、成功后回写
  `publishedfileid`（库条目进 DB，工程写 `project.json` 的 `workshop.fileId`）。
  入口：MCP 工具 `workshop_upload` / `workshop_upload_status` + 本地库卡片右上角上传按钮
  （scene / web / video；视频与单文件网页上传时自动合成最小 `project.json`）。
  上传对话框含版权/授权提示。需要 Steam 客户端运行且账号拥有 Wallpaper Engine。
- **本地库「导入壁纸」弹框**：工具栏收敛为单个入口，弹框内含「添加壁纸目录 / 导入文件夹 /
  导入文件」与已导入的壁纸目录列表（可移除，只删库记录不动源文件）。

### 🐛 修复 / Fixes

- **本地库页面卡死（含重启后依旧卡死）**：`library_list` 原是同步命令跑在主线程，
  全库磁盘对账 + 缺封面视频的同步抽帧（失败还会无限重试）会把 UI 整个冻死。现在
  列表查询移入阻塞线程池，视频封面改为启动后的后台补齐任务，失败条目记账不再重试。
- **退出软件后被 launchd 反复拉起**：「开机自启」注入的 launchd plist 用了无条件
  `KeepAlive`，用户主动退出也被立刻复活。改为条件化（仅崩溃 / 被信号杀死时拉起），
  应用启动时自动升级存量 plist。
- **CI 安装包每次都要重新授权录屏权限**：macOS TCC 权限绑定代码签名，ad-hoc 签名
  每次构建都不同。CI 支持固定签名证书（Secrets：`MACOS_CERTIFICATE_P12` /
  `MACOS_CERTIFICATE_PWD`），同一把证书的更新包授权跨版本保留。
- 音频频谱 AGC 目标值修正（0.7，留 0.3 余量防止普遍削顶），静音回落与快攻慢放行为不变。
- **Windows 安装后壁纸 404（renderer/index.html 找不到）**：平台配置里的 resources
  写成数组，把主配置的 renderer / assets / mediaremote-adapter 资源整个覆盖掉了。
  平台配置现携带完整资源清单。
- **Linux 启动报 `libsteam_api.so: cannot open shared object file`**：同一根因的另一半——
  steam 库装进了 `sdk/` 子目录而二进制 RUNPATH 指向资源根，且 ARM64 包误装了 x64 的库
  （两架构共用配置写死了 x64 路径）。现在库按目标架构生成并落在资源根。

### 🔧 变更 / Changed

- **MCP 移除版本管理**：`project_snapshot` / `project_versions` / `project_rollback`
  三个工具及其 `.version/` 机制移除，工程版本由创作者自行维护（git 等）。
- **帧率上限新增 15 FPS 档并设为默认**；设置页与每壁纸「播放设置」的「帧率限制」
  统一更名为「帧率上限」。
- **全局消息提示**统一右上角 3 秒自动消失（原成功类 2.6s、错误类需手动关闭）。
- 本地库上传按钮移至卡片右上角，操作行保留原 4 按钮。
- Steam API 动态库随应用分发（macOS `Contents/MacOS`、Windows 资源、Linux rpath）。
- **默认帧率上限由 15 FPS 调回 24 FPS**（15 档保留可选；从未手动改过帧率的用户生效）。
  Default FPS cap back to 24 (15 stays available as an option).

## [v0.5.1] - 2026-09-12

### 🐛 修复 / Fixes

- **换壁纸不再新建窗口、也不再累积渲染进程（含 macOS）**：原先每次换壁纸都是
  「销毁窗口 + 同名重建」，每切一张都要付一次建窗代价（新渲染进程、重新挂载桌面层、
  新建 GL 上下文并重传全部纹理，GPU 被顶成连续尖峰）。现在所有平台统一成**在同一块
  窗口里整页导航**到新的渲染器 URL —— 换的是文档，渲染进程数不随切换次数增长
  （每个显示器恒为 1 个）。渲染器的配置本来就全在 URL query 里（`initialCfg`），
  `pagehide` 的 teardown 也早已齐全，换页等于「干干净净地重新挂载」；连切仍由
  防抖 + 单飞收敛到最后一张。
- **换壁纸后旧壁纸的内存不释放（macOS 实测，第三轮结论）**：第二轮让 macOS 换壁纸
  走「销毁窗口 + 删掉该窗口独占的 `WKWebsiteDataStore`」，指望存储一删、压着旧壁纸
  的 WebContent 进程就退出。实测（macOS 26，11 次换壁纸）**只有三成兑现**：其余 7 次
  `removeDataStoreForIdentifier` 报 `Data store is in use`，旧的渲染进程（活动监视器
  里 `about:` / `http://127.0.0.1:<port>`，单个几百 MB～1.4GB）连同它在
  `~/Library/WebKit/<app>/WebsiteDataStore/<uuid>` 下的目录一起留下。最小复现确认了
  机制：只要 UI 进程里还活着一个引用那份存储的 `WKWebsiteDataStore`（销毁后的
  WKWebView 及其 configuration 仍被 WebKit 攥着，不随 `destroy()` 立刻 dealloc），
  WebKit 就拒绝删除 —— 等 5s 也等不到，加长重试只是延缓。既然删存储这条路不可靠，
  macOS 也回到「同窗口换文档」（见上一条），从根上不再制造多余的渲染进程。
- **销毁窗口时的数据存储回收改为尽力而为 + 周期清扫**（仅 macOS，用于停止壁纸 /
  拔显示器 / 导航失败降级重建）：销毁前先换到 `about:blank`，让页面跑完 `pagehide`
  teardown（销毁库实例、释放 pkg 缓存、`loseContext`、撤销 blob）再 `destroy()`；
  之后按标识 `removeDataStoreForIdentifier`，重试窗口从 4.8s 拉到 20s，失败后交给
  每分钟一次的清扫任务，以及下次启动的清扫（清掉没登记在案的存储）。日志里带上
  数据存储的 uuid，便于直接对照磁盘残留目录排查。副作用：壁纸页的 localStorage /
  IndexedDB 每块窗口（每次重建）都是全新的（WE 的用户属性走 project.json + `/props`
  注入，不依赖它）。
- **销毁壁纸窗口前先等 `about:blank` 真的换上**（仅 macOS，停止壁纸 / 拔显示器 /
  降级重建这三条路）：原先导航后固定等 200ms 就 `destroy()`，4K 场景页常常还没提交
  换页就被摘掉 —— WebKit 留在进程池里的那份进程便仍压着**整张壁纸**：活动监视器
  实测同一批残留里，提交成功的只占 37～66MB，没提交的以 `about:` 记名常驻 1.39GB。
  改成轮询到 URL 真变成 `about:` 再销毁（上限 2.5s，页面僵死则到点照旧销毁），
  并打出实际等待耗时（提交成功记 INFO，2.5s 到点记 WARN）—— 这条日志能直接回答
  「抢跑还发不发生」。
- **主线程停顿的判定改成两档、并报出实测时延**：原先「监控派发 100ms 未被主线程
  接手」一律记 ERROR，而主线程在 WebKit 重内容合成 / 系统壁纸截图 / 4K 视频首帧
  解码期间偶发 100～300ms 停顿是正常的 —— 2026-09-11 日志里 30 次触发全部落在重
  壁纸挂载后的那 1～3s 内（界面并无卡顿），这类噪声还把人引偏过一次排查方向。现在
  100ms～1s 记 WARN 并带上实测毫秒数，超过 1s（真正的无响应级别）才记 ERROR。
- **主窗口不再「隐藏 3s 就回收」，改成只在系统内存压力下回收**（全平台）：那条
  3s 闲置回收在 macOS 上**回收不了内存** —— 主窗口用的是默认（共享）
  `WKWebsiteDataStore`，没有按标识删除的 API，`destroy()` 只是把 WKWebView 从窗口
  上摘下来，WebContent 进程连页面一起留在 WebKit 的进程池里，重建时又落回同一个
  池子。收益接近于零，代价却是每次重开一次页面重载，外加隐藏瞬间的主线程停顿
  （同上一条：那 30 次「100ms 未接手」全部落在窗口被隐藏的那一刻）。现在：
  隐藏满 5s **且**系统报告内存压力时才销毁窗口（新增 `src-tauri/src/mem_pressure.rs`
  —— macOS 读 `kern.memorystatus_vm_pressure_level`、Windows 读
  `GlobalMemoryStatusEx().dwMemoryLoad ≥ 90%`、Linux 读 `/proc/meminfo` 的
  `MemAvailable < 8%`；读不到就按「无压力」处理，即不回收）；压力下窗口若正可见
  则不动它，只打一条 WARN 留痕（那种情况下 WebKit 也可能自行结束该进程，界面变空白
  时关掉重开即可）。探测本身也留痕：首次读到读数记一条 INFO（`内存压力探测就绪：…`），
  连续 3 次读不到记一条 WARN —— 否则「一直没看到回收」到底是没压力还是信号失效，
  日志里分不出来。
- **构建报错直接指明该跑什么**：`dist/` 是 gitignore 的前端产物，直接 `cargo build` /
  `cargo check`（不跑 beforeBuildCommand）时会以一句 `resource path "../dist/renderer"
  doesn't exist` 收场，看不出该补什么。现在 `src-tauri/build.rs` 先检查该目录，缺失时
  打出 `cargo:warning` 指明命令（`pnpm --filter @we/desktop build`；`pnpm tauri build` /
  `pnpm tauri dev` 会自动构建，不必手动跑）。
- **内存「漏在哪」现在由应用自己报数**：新增 `src-tauri/src/mem_watch.rs`，用 libproc
  读本进程与每个 WebKit 进程的 `phys_footprint`（与活动监视器「内存」列同源），
  每 60s 一次，外加每次换壁纸 / 新建窗口 / 销毁窗口各一次，汇总成一行日志
  （如 `本进程 198MB；归属进程 4 个合计 1.45GB / WebContent2 1.41GB / GPU1 22MB`）；
  总量的 1.5GB、单个进程 800MB 为 WARN 阈值，超了还会单列最大那个进程。以前
  这只能靠活动监视器截图判断，既不能 grep、也说不清是哪一次换壁纸之后涨的。
- **停止壁纸时真的把 WebContent 进程结束掉**（macOS，本轮泄漏的正主）：`destroy()`
  只是把 WKWebView 从窗口上摘下来，进程连同整页留在 WebKit 的进程池里，而池里的
  进程**不保证**把内存还给系统 —— 实测每停一次留下一个 700MB～1.2GB 的残留，
  连切 8 次能堆到 11 个进程 / 4.3GB。这一版改成分三步：**销毁前**从 WKWebView 问出
  该窗口的 WebContent pid（`_webProcessIdentifier`）、`destroy()` 摘窗口、**等窗口
  真的从注册表消失之后**再对那个 pid 发 `SIGKILL`。实测换一次壁纸的 800MB 在进程
  消失后立刻归还（`SIGKILL，11ms 后消失`），反复「应用 → 停止」6 轮后占用稳定在
  ~400MB、WebContent 恒为 1 个（主窗口）。
  两个反直觉的坑，都写在代码注释里：①WebKit 自带的 `_killWebContentProcess` 私有
  API 在这版系统上**调用有返回、进程却纹丝不动**（实测一路堆到 4.3GB 一个没少），
  只有自己发信号才真还内存；②杀得**太早**反而会漏 —— `destroy()` 是异步投递的，
  刚调用完 WKWebView 还在，此时杀它的进程等于告诉 WebKit「这个还活着的页面崩了」，
  它会立刻补一个空进程（~9MB），每停一次多一个，12 轮下来池子里躺了 19 个。等窗口
  消失再杀，池子就不补人了。
  顺带：进程既然已经显式结束，那条「按标识删数据存储」在快路上就不再做了（它当初的
  存在理由就是「删掉存储才能让进程退出」）—— 进程刚死的这一刻删必撞 `DataStoreInUse`，
  实测每次白等 20~30s 再记一条「回收失败」，而磁盘目录交给既有的 60s 一轮
  `sweep_stale_data_stores` 与下次启动清理即可。主窗口在内存压力下被回收时同样会把
  它的进程结束掉（且回收后立刻记一行内存观测，能直接看到这次还回来多少），但只在
  没有别的共享存储窗口时才动手 —— 主窗口与「壁纸设置」窗可能共用同一个 WebContent 进程。
- **复用进程超预算时改走销毁重建**：换壁纸默认仍是「同窗口换文档」（进程数不随切换
  次数增长），但换文档后旧文档的内存何时还给系统由 WebKit 说了算（实测会长期停在
  峰值）。现在每次换壁纸前量一次 WebContent 合计占用，超过 1.2GB 就这一次改走
  「销毁 + 重建」把内存实打实收回来；没超预算不重建 —— 重建要付一次页面加载与
  GPU 峰值，不该为几十 MB 反复付。

## [v0.5.0] - 2026-09-12

### 🪟 多架构发布与 Windows 真机修复 / Multi-arch & Windows fixes

- **发布矩阵扩到五条**：macOS 通用二进制（Apple Silicon + Intel，`lipo` 合并）、
  Linux x64 / ARM64（各自原生 arm runner）、Windows x64 / ARM64（x64 runner 交叉编译
  到 `aarch64-pc-windows-msvc`，并钉死 MSVC 的 ARM64 `link.exe` —— 否则会被 Git 自带的
  同名程序抢占）。
- **修 Windows「应用 / 预览壁纸」卡死**：Tauri 的同步命令运行在主线程的 WebView2 IPC
  回调里，而 `run_on_main_thread` 在主线程上会**内联执行**闭包，于是 `WebviewWindowBuilder::build()`
  变成「在 WebView2 自己的回调里再创建 WebView2」，违反其线程模型造成重入死锁，整个应用
  冻结（macOS 的 WKWebView 无此限制，所以此前只在 Windows 暴露）。相关命令改为 `async`
  在异步线程池执行，建窗回到事件循环顶层。
- **重做 Windows 桌面层级**：弃用 `tauri-plugin-desktop-underlay`（它只用窗口 label 记录
  「是否已下沉」且销毁时不清理，切壁纸的「销毁 + 同名重建」会让新窗口被误判为已下沉而跳过
  `SetParent`，表现为**新设置的壁纸全屏盖在最顶层**）。改为自行父子化并用 `GetParent` 校验：
  Win11 的 raised-desktop 结构（`Progman` 带 `WS_EX_NOREDIRECTIONBITMAP`）下按微软给第三方
  壁纸程序的指引，把窗口挂成 `Progman` 的子窗口、紧贴 `SHELLDLL_DefView` 之下，并把承载
  静态壁纸的 `WorkerW` 压到子窗口 Z 序最底；经典结构（Win10）仍走兄弟 `WorkerW`。跨进程
  `SetParent` 在两进程 DPI 感知级别不一致时会失败，按 `UNAWARE` / `SYSTEM_AWARE` 依次重试；
  任一步校验不过就退回 Z 序最底，保证「最坏只是图标被盖住，绝不出现壁纸盖住整个桌面」。
- **steamcmd 不再弹控制台窗口**：所有子进程加 `CREATE_NO_WINDOW`（下载 / 校验 / 预热三条路径）。
- **启动更快**：首次显示器枚举撞上驱动就绪过渡期（Windows 实测启动首帧返回空）时做有界短重试，
  不再干等监控的 2s tick。
- **设置 → 关于**：新增软件更新检查与一键下载（按 GitHub Release 校验）。

### 🐛 修复 / Fixed（订阅同步手机确认、MCP 状态英文、壁纸语言默认）

- **订阅同步：在 Steam 手机 App 点「允许」后，页面不再卡在「请输入令牌验证码」**。
  手机令牌账号登录时 Steam 会同时下发「验证码」和「App 确认」两条通道，之前只走验证码
  分支：用户明明已在手机上允许，页面仍停在输码页。现在进入码页后前端每 ~3s 后台轮询
  登录状态，后端顺带查一次手机确认，确认即自动继续；码页同时提示「也可在手机 App 上点
  「允许」」。另外修掉了后台轮询取走等待中会话、用户此刻手动提交验证码会误报「没有等待
  验证码的登录会话」的竞态。
- **「AI / MCP」页的状态描述在英文界面下不再永远显示 Starting…**：状态值比较误用了
  翻译后的文案，英文环境下分支全不命中。改为用中文原文当比较键、只在渲染时翻译。
- **「设置 → 通用 → 壁纸语言」默认改为 English**（此前为简体中文）；壁纸自带
  `language` 属性时仍以壁纸为准。

### ✨ 新增与改进 / Added & Improved（界面多语言：中文 / English）

- **设置 → 通用 →「界面语言」可切换整机语言，立即生效**：界面、托盘菜单、独立「壁纸设置」
  窗口标题、原生文件选择框、后端错误提示一起跟着变。首次启动跟随系统语言
  （中文环境 → 中文，其余 → 英文），用户选过之后不再跟随系统（系统语言变了不该
  把用户的选择改掉）；语言存在 `localStorage`，`<html lang>` 同步更新（影响字体
  回退与断行）。
- **做法：中文原文当键**（`src/lib/i18n.ts` 头部有完整取舍说明）。界面里写
  `tr("下载")`，查 `src/locales/en-US.ts` 的中→英表；查不到就**原样显示中文**——
  漏翻只会露出中文，不会出现空白文案或 key 泄漏。新增语言 = 新增一张中→该语言的表。
  没有引入 i18n 依赖，几十行运行时（`tr` / `trMsg` / `useLocale`）够用。
- **后端文案走同一套约定**（`src-tauri/src/i18n.rs`）：托盘菜单项是原生绘制的，语言
  一变就**原地重写文字**（`set_text`，不重建菜单 —— 重建会让 `on_menu_event` 里捕获
  的勾选句柄失效）；语言由前端通过 `app_set_locale` 推给后端，后端只认「是不是英文」。
- **后端消息（错误/进度）不逐个改 Rust**，改成前端一张「中文模板 → 英文」的
  `EN_US_BACKEND` 表（`trMsg`）：Rust 用 `format!` 拼出的成品句子按 `{}`/`{name}`
  切分回填，支持 `{:?}` 这类格式说明符，并**递归翻译**捕获到的片段，所以
  `拷贝 {} 失败: {e}` 这种嵌套消息整句都是英文。277 条覆盖下载/steamcmd·ffmpeg
  安装/本地库导入/Steam 登录/订阅同步/系统壁纸同步等用户可见路径。
- **标签名不翻译**：题材/分级/分辨率这些 Steam 标签在非中文语言下**回落 Steam 官方
  英文原名**（`tagLabel()`），而不是自造译名 —— 用户要拿这些名字去别处搜同一张壁纸。
  分组标题（类型/题材/分辨率……）和排序项照常翻译。
- **窗口间同步**：主窗口改语言，已打开的独立「壁纸设置」窗口通过 `storage` 事件跟着切。
- **顺带统一了「显示模式」的说法**：独立「壁纸设置」窗口里的「填充 / 适应」改成与设置页、
  托盘菜单一致的「裁剪 / 缩放」（英文 Crop / Fit）—— 同一个值在三处叫两个名字，用户会
  以为是两个不同的选项。
- 说明：`tracing` 日志、MCP 工具描述、工坊脚本等**开发者/接口面向**的文案保持中文，
  不进文案表。

### ✨ 新增与改进 / Added & Improved（筛选标签全面多选）

- **工坊 / 本地库筛选：每个分组都能多选**。原先只有「题材」「功能特性」可多选，
  类型 / 年龄分级 / 分类 / 分辨率是单选（选一个顶替同组其它）。语义不变 —— 组内并集
  （任一命中）、组间交集、一个都不选 = 不约束：选「场景 + 视频」= 两者都看，再叠
  「动漫」= 只看动漫题材的场景或视频。
  - 单选是照 WE 官方面板抄的，但对本应用没好处（「想看场景和视频」是个正常诉求），
    而且后端两条链路本来就支持组内并集：工坊按组拆成多次 Steam 查询再合并，
    本地库是 SQL `OR`。所以 `TagGroup.multi` 这个开关纯粹是前端自缚，已删除
    （`src/lib/tags.ts` 的 `toggleTagSelection` 顺势简化成纯切换）。
- **工坊组合数上限 8 → 16，并新增 `truncated` 标记**。组内并集在 Steam 侧只能拆成
  「每组取一个标签」的多次查询，组合数是各组的乘积；超上限旧代码只写日志就截断，
  用户看到的是「多选之后结果莫名变少」。现在结果里回传 `truncated`，工坊页顶一条
  黄色提示让用户少选几个标签 —— 宁可说「结果可能不全」，也不假装结果是对的。

### ✨ 新增与改进 / Added & Improved（抽帧组件 ffmpeg 一键安装）

- **托管 ffmpeg**（新增 `src-tauri/src/ffmpeg.rs`）：视频/GIF 抽首帧（系统壁纸同步、
  本地库导入视频的封面）不再要求用户自己装 ffmpeg，改成应用内一键安装静态构建到
  `<app_data>/ffmpeg/`。**不内置进安装包**的理由写在模块头：静态构建解压后 80~90MB
  且上游是 GPL，随包分发要一并处理源码/许可义务；让用户从上游官方地址下载则不涉及再分发。
  与 steamcmd 同一套范式（下载 → 校验体积 → 解压 → 落盘 → 设置页可见状态/进度/卸载），
  用户只需要理解一次。
- **查找与降级**：托管副本优先（应用内装过就以它为准，静态构建行为确定），其次才用
  **系统 PATH 上的 ffmpeg**（发行版仓库装过就不必再下 90MB）。抽帧入口拿不到 `AppHandle`，
  所以路径在启动时缓存（`ffmpeg::init`）、安装成功后立即补写，**不需要重启**。
- **下载源**（主源失败自动回退镜像）：Windows = gyan.dev essentials → BtbN win64-lgpl；
  Linux x86_64 = johnvansickle 静态构建 → BtbN linux64-gpl（arm64 各有对应源）。
  下载走**流式写盘 + 百分比进度**（`ffmpeg:install-progress`：`download` / `extract`），
  并复用设置页里的下载代理。
- **解压按扩展名分派**（而不是按平台）：`zip` 用依赖里已有的 `zip` crate（含路径穿越防护），
  `tar.xz` 用系统 tar（缺 xz 时给出可照做的提示）。`find_binary` 兼容两种上游包布局
  （`<ver>/bin/ffmpeg[.exe]` 与 `<ver>-static/ffmpeg`），安装后**跑 `-version` 自检**，
  避免「文件在但不可执行」被当成成功。
- **抽帧调用点改造**：`system_wallpaper.rs` 的 Linux / Windows 实现改用 `ffmpeg::command()`，
  找不到时的提示从「请自行安装（apt / winget 命令）」改成指向 **设置 → 通用 → 抽帧组件**。
  本地库导入视频的封面走同一条路径，一并受益。
- **设置页新增「抽帧组件（ffmpeg）」行**（仅 Linux/Windows 显示）：状态（未安装 / 已就绪 /
  系统已装）、版本、占用体积、安装 / 修复 / 卸载按钮与实时进度；「自动设置系统壁纸」的说明
  同步指向它。
- **Linux 打包**：deb 的 `Recommends` 加入 `ffmpeg`，apt 装机时顺带装好，无需应用内下载。
- 顺带把「下载代理」的读取收敛成 `download::read_proxy`，供 steamcmd 引导包与 ffmpeg
  两条下载链路共用（原先埋在 `steamcmd_install.rs` 里）。

### ✨ 新增与改进 / Added & Improved（Windows 支持）

- **Windows 平台接入**（`src-tauri/src/wallpaper/windows.rs`）：壁纸窗口经
  `tauri-plugin-desktop-underlay` `SetParent` 到资源管理器的 `WorkerW`（`Progman` 之下、
  桌面图标之下），并按父窗口客户区原点换算定位（虚拟屏原点可能为负）；「壁纸交互」态
  脱离 `WorkerW` 压到 Z 序最底。屏幕枚举用 Tauri `available_monitors`（物理像素 +
  per-monitor DPI 换算逻辑坐标），显示器睡眠用 `GUID_CONSOLE_DISPLAY_STATE` 电源通知
  （事件驱动，零轮询），自动暂停用 `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`
  （语义对齐 macOS 的 `NSWorkspaceDidActivateApplicationNotification`），指针状态用
  `GetCursorPos` + `GetAsyncKeyState`（免权限）。已知限制：重启资源管理器会重建
  `WorkerW`，需重新应用壁纸才能回到桌面层。
- **系统音频捕获（WASAPI loopback）**（`src-tauri/src/audio_capture/windows.rs`）：
  `IAudioClient` + `AUDCLNT_STREAMFLAGS_LOOPBACK` 抓取系统混音，浮点交错下混单声道，
  **不需要任何授权**（`screen_recording_granted()` 恒为 true，设置页也不再提示屏幕录制）；
  输出设备切换/插拔时自动重开（2s 退避）。频谱计算抽到共享的
  `audio_capture/spectrum.rs`（FFT + 对数分频 + 峰值衰减），macOS 与 Windows 产出同一条
  64 段曲线，避免两个平台观感不一致。
- **正在播放（GSMTC）**（`src-tauri/src/now_playing/windows.rs`）：轮询
  `Windows.Media.Control`（1s），封面按曲目缓存、按魔数嗅探 MIME 后转 data URL；
  MTA 线程内 `CoInitializeEx`，复用 `windows-future` 的 `.get()`。
- **steamcmd 运行时安装支持 Windows**（`download/steamcmd_install.rs`）：改用官方
  `steamcmd.zip`（`media.steampowered.com` / `steamcdn-a.akamaihd.net` 双源），用既有
  `zip` crate 在 `spawn_blocking` 里解压，产物换成 `steamcmd.exe`、预热判据换成
  `steamclient.dll` / `steamclient64.dll`，HOME 环境变量按平台取 `USERPROFILE`。
- **伪终端层跨平台重写**（`download/pty.rs`）：从 `open`/`attach` 改为
  `Pty` + `Writer` + `channel()`（spawn 之后再接线），POSIX 走原有 pty，Windows 用三个
  匿名管道并把 stdout/stderr 合并成一条事件流；字节读取新增 `\r` 分行，steamcmd 自更新
  的单行刷新进度（`\r` 无 `\n`）也能逐行流式上报。
- **静态壁纸同步支持 Windows**（`system_wallpaper.rs`）：沿用 Linux 的抽帧策略
  （scene/web 用工坊预览图，视频/GIF 走 ffmpeg），落盘后
  `SystemParametersInfoW(SPI_SETDESKWALLPAPER, ...)`（去掉 `\\?\` 前缀 + UTF-16 终止符）。
- **窗口外观**（`lib.rs`）：macOS 走 `apply_vibrancy`，Windows 走 `apply_acrylic`
  （tint `28,28,32,180`，失败回退 `apply_blur`），供主窗口与「壁纸设置」窗口共用。
- **全局快捷键**：macOS 保持 ⌘⇧P / ⌘⇧N，Windows 与 Linux 改用 **Ctrl+Shift+P / Ctrl+Shift+N**
  （`cmd` 修饰键在非 macOS 上注册不生效）。
- **打包与 CI**：`tauri.conf.json` 加入 `nsis` / `msi` 目标与 Windows 安装器配置
  （NSIS `currentUser` 免管理员安装 + 简中/英文、WebView2 `downloadBootstrapper`），
  图标列表补 `icons/icon.ico`；新增 `.github/workflows/build-windows.yml`
  （`windows-latest`，产出 NSIS `.exe` 与 `.msi` 并附加到 tag Release）。
- **前端文案按平台分支**：设置页的音频捕获、权限提示、代理、自启、静态壁纸与「关于」
  全部按 `os` 分支（macOS / Windows / 其他），系统音频说明明确 Windows 无需授权；
  「壁纸设置」窗口的磨砂底色在 Windows 下同样降到 0.78 alpha（acrylic）。

### 🐛 修复 / Fixed（自检脚本把 dev 会话搞挂）

- **自检脚本跑完会收尾**：原先跑完把条目留在本地库、壁纸也一直挂在桌面上
  （`wallpaper_sessions` 会在每次启动时把它恢复成桌面壁纸持续渲染），很容易被当成
  「应用卡顿」。现在结束时 `wallpaper_stop` + `library_delete` 掉本次自检的条目
  （工程目录保留），`KEEP=1` 可保留做人工验收。
- **`scripts/mcp-e2e-run.sh` 不再托管重启开发进程**：`RESTART=1` 原本直接 kill 应用，
  但父进程是 `tauri dev` / `cargo run` 时，dev CLI 会跟着退出并把它拉起的 vite 一起带走；
  脚本再裸起 debug 二进制就只剩白屏（主界面和壁纸页都加载不出来，壁纸页连 `/diag` 都发不出来），
  表现为「截图等待渲染器就绪超时 + 渲染器未上报任何诊断」这种极难定位的假失败。现在检测到开发托管
  的进程会拒绝代重启并提示去 dev 终端重启。
- **脚本新增调试构建的前端前置检查**：`target/debug` 构建从 vite（`127.0.0.1:1420`）取前端，
  连不上时不再傻起一个白屏应用，直接说明原因。
- **失败时给出可判读的提示**：日志里没有 `[renderer diag]` 说明壁纸页根本没加载，
  而不是渲染慢——脚本会点出来。
- 顺带修掉脚本自身在 macOS bash 3.2 下的 `unbound variable`：UTF-8 locale 里 bash 会把
  中文标点的字节当成变量名字符，`$PID，` 会被解析成 `PID<0xef>`，变量展开后紧跟非 ASCII
  字符的位置全部改用 `${VAR}`。

### 🐛 修复 / Fixed（MCP 截图「等待渲染器就绪超时」高频出现）

- **渲染器不再用 10s 硬超时判 mount 失败**（`renderer/src/main.ts`）：旧实现 `Promise.race`
  10s 一到就放弃，于是**永远不上报 ready**（截图侧只能干等到超时），**且把还在解析的实例丢在一边**
  （漏 WebGL 上下文，下次挂载又要从零解析）。现在 10s 只上报一条「仍在解析」诊断，
  真正的兜底放到 90s 并销毁晚到的实例。这是「截图超时」反复出现的根因。
- **同一张壁纸重复截图不再重新挂载**：`wallpaper_screenshot` 发现该条目已经就绪时跳过 `apply`，
  省掉整轮 pkg 解析与 WebGL 重建（返回里的 `applied` 字段说明是否真的重挂过）。
- **截图默认上限 30s → 60s**：大 `scene.pkg` 冷启动常到 30s+，旧上限会把正在成功的挂载判成超时。
- **超时错误可读**：内容服务器新增渲染器诊断快照，超时时附带「渲染器自报失败：…」或最近一条诊断，
  并区分「未找到壁纸窗口」与「还在解析」；窗口探测改为等待期间反复重试（建窗有几十到几百毫秒延迟）。
- **截图不再冻结主线程**：WebKit 的 completion 回调跑在主线程，3024×1964 的 PNG deflate 实测 >100ms，
  压在主线程既冻 UI/动画，又会被壁纸监控的「事件循环卡死」探测抓成
  `main thread did not process monitor dispatch within 100ms (UI event loop wedged?)`。
  现在主线程只做 `NSImage → TIFF` 位图抽取（NSImage 不是线程安全的，必须就地取），
  TIFF→PNG 编码丢给后台线程；并新增一条 DEBUG 耗时日志便于后续定位。
- **自动设置系统壁纸的抽帧也挪出主线程**：video/gif 的 `poster_frame` 里除硬件解码外还有
  PNG 编码（4K 一帧可到几百 ms），原先由 `apply_on_main` 在主线程同步跑，同样会触发上面的
  告警。现在抽帧整体走后台线程，只有 `set_all_screens` 回主线程（`NSWorkspace` 有主线程要求）；
  抽帧写盘加了一把锁串行化，避免两个 apply 撞在一起互相截断缓存 PNG。

### ✨ 新增与改进 / Added & Improved（AI / MCP 创作）

- **应用内置 MCP 服务**：新增 `src-tauri/src/mcp/`（基于既有依赖 `axum`，不引入新 crate），
  在 `127.0.0.1:<端口>` 上提供 `POST/GET/DELETE /mcp`（JSON-RPC，支持 batch；
  `Accept: text/event-stream` 时回 SSE）。默认**关闭**，启用后仅监听回环 + 令牌鉴权
  （`?token=` 或 `Authorization: Bearer`），非回环 `Origin` 一律拒绝（防 DNS rebinding）。
  端口默认 `7411`，设置变更热重启，端口占用在设置页显式报错。
- **31 个 MCP 工具 + resources + prompts**：覆盖「建工程 → 写素材 → 校验 → 冻结版本 →
  打 scene.pkg → 安装本地库 → 应用到桌面 → 截图自检 → 回退」的完整闭环，并复用既有能力（本地库 / 工坊 / 下载 /
  属性 / 会话）。另有 `wallpaperem://docs/{web,scene}-project`、`wallpaperem://templates/*`
  与 `create_web_wallpaper` / `create_scene_wallpaper` 两个引导 prompt。
- **AI 壁纸工程工作区**（`src-tauri/src/workspace.rs`）：工程落在
  `<系统文档目录>/WallpaperEM/Projects/<工程名>/`，用户可见可改；MCP 的文件工具只能读写
  该根目录内的路径，`..`、绝对路径与符号链接逃逸一律拒绝；写入支持 `utf8` 与 `base64`
  （生成图片走 base64）。随附 `web` / `scene` 两套可直接跑通的模板。
- **`scene.pkg` 打包器与工程校验器**：遍历工程把 `materials/**` 下的 PNG/JPEG 就地转成
  `.tex`（`TEXV0005/TEXI0001/TEXB0004` + `freeImageFormat` 原样内嵌），再按 `PKGV0022`
  容器写出 `<工程>/scene.pkg`（幂等覆盖、包内自洽）；校验器输出 `errors`/`warnings`
  列表而不抛异常，覆盖 `project.json` 属性白名单与
  `image → models → materials → passes → textures` 全链路引用与 `.tex` 存在性。
- **`wallpaper_screenshot` 截图自检**（macOS）：复用内容服务器 `/diag` 的 ready 信号 +
  WKWebView 截图，工具返回 `image/png` 内容块，agent 可直接看到自己做的画面。
- **设置页新增「AI / MCP」标签页**（排在「通用」之后）：开关、端口、令牌（可轮换 / 复制带令牌地址）、
  运行状态（运行中 / 端口占用 / 未启动）、一键复制的客户端配置片段（`mcpServers` JSON
  与 codex / claude CLI 命令），以及内存环形缓冲的最近调用日志（工具名 / 耗时 / 结果）。
  agent 产物直接进本地库，不新开页面。
- **文档**：新增 `docs/mcp-authoring-web.md` / `docs/mcp-authoring-scene.md` 两份创作规范
  （同时作为 MCP 资源下发），README 增补「MCP 服务」章节与客户端接入方法。

### ✨ 新增与改进 / Added & Improved（MCP 版本历史与年龄分级）

- **`project.json` 新增必填的 `version`**（≥ 1 的整数）：它就是 `.version/<编号>/` 的目录名，
  发布与回退都靠它定位。只接受整数——`"1.0.0"` 这种字符串没法当目录名，版本比较也会退化成
  字符串比较。
- **工程自带版本历史（`.version/`）**：新增 `project_snapshot` / `project_versions` /
  `project_rollback` 三个 MCP 工具。冻结时把源文件完整拷进 `.version/<编号>/`
  （`scene.pkg` 等生成物不进历史，能重建的东西进历史只会让体积翻倍），并把工作副本的
  `version` 推进一版；编号被占用时**自动顺延、绝不覆盖历史**，所以回退过的工程还能继续往前走。
  回退会把工程根还原成那一版（该版没有的源文件一并删掉）并清空 `scene.pkg`；默认拒绝在
  「有未冻结改动」时回退，避免把没进历史的改动悄悄丢掉。
- **`project.json` 的 `tags` 必须有年龄分级标签**：`Everyone`（大众级，默认）/
  `Questionable`（指导级）/ `Mature`（成人级），取值与 Steam 工坊标签逐字符一致，
  且只能有一个；校验器对缺失/多个/非整数版本号一律报 error，模板默认写 `["Everyone"]`。
- **分级标签随安装进本地库**：`project_install` / `project_update` 会把工程自带的分类标签
  并进库内条目的 `tags`（保留已有标签，只补缺的），本地库「年龄分级」筛选因此对 AI 造的
  壁纸同样可用。
- **`.version/` 不进包、不进库**：`scene_pack`、`project_list_files`、本地库导入的拷贝与
  体积预检都跳过它；网页入口的兜底扫描也不再会把历史副本里的 `index.html` 当成入口。
  `project_write_file` 直接写 `.version/...` 会被拒绝（历史只由快照工具维护）。
- **文档**：两份创作规范补充 `version` / `tags` 的必填规则、`.version/` 的用途与快照/回退闭环。

### 🐛 修复 / Fixed（MCP 闭环）

- **启用 MCP 后应用启动即崩溃**：`mcp::init` 在 Tauri setup 钩子里（主线程、没有 Tokio
  运行时）调用 `tokio::net::TcpListener::from_std`，panic「there is no reactor running」。
  现在绑定仍用同步 `std::net::TcpListener`（端口被占用才能当场报回设置页），转异步监听
  挪进 `tauri::async_runtime::spawn` 的任务里；并对 `AddrInUse` 做有界重试，避免
  `stop()` 之后套接字还没释放就误报「端口被占用」。
- **模板里的 PNG 被打包器当文本写坏**：`project_create` 一律 `String::from_utf8_lossy`
  再替换 `{{TITLE}}`，二进制素材里的非法字节被换成 U+FFFD，建出来的工程贴图全废，
  表现为 `scene_pack` 报「不是合法 PNG」。现在按「是否合法 UTF-8」分流：只有文本才做
  占位符替换，二进制原样落盘。
- **坏源图拖到打包阶段才报错**：校验器现在先读一次图片头，源图无法解码时在
  `project_validate` 就给出 error（而不是 `scene_pack` 才失败）；errors / warnings
  一并去重，同一处问题不再报两遍。
- **`project_update` 失败会连旧版本一起丢**：原实现先 `remove_dir_all` 旧目录再导入，
  新的导入一旦失败（校验不过、磁盘不足）就等于「更新失败 = 本地库里的壁纸没了」。
  现在先把旧目录改名让位 —— 原 id 腾空，导入才不会因 `unique_dest_dir` 退到 `-1` 后缀
  而丢掉用户改过的属性覆盖 —— 导入成功再删备份，失败则把旧版本原样放回。
- **`itemId` 路径穿越**：`library_delete` / `library_open_folder` / `project_update` 的
  itemId 现在可能来自外部 agent，而 `Path::join` 遇到绝对路径会整体替换前缀、`..` 也能
  爬出库根，下游的 `remove_dir_all` 会删到库外（用户文档 / 应用数据目录）。现在统一在
  `item_dir` 这一层校验：只接受一层普通目录名。
- **端到端自检踩到「陈旧进程」**：`scripts/mcp-e2e-run.sh` 原来只要看到应用在跑就直接用
  它，重建后旧进程仍在内存里跑旧代码，于是「已经修好的 bug 又复现」。现在比对进程启动
  时间与二进制 mtime，发现陈旧就提示重启（或 `RESTART=1` 交给脚本接手）；同时把
  「应用在跑吗」的判定从 `pgrep -f WallpaperEM` 换成按进程名精确匹配，不再把
  `tauri dev` 下的 vite/node 误判成应用。

### ✨ 新增与改进 / Added & Improved（列表与缓存体验）

- **发现页：应用 / 下载合并成一个按钮**：本地库里已经有这张壁纸就直接显示
  「🖥 应用到桌面」，没有才显示「⬇ 下载」。原先两个按钮并排摆着，已下载的条目上
  那个「已下载」是死的、未下载的条目上「应用到桌面」点了必然失败 —— 合并后按钮
  永远只指向当前唯一有意义的动作（已应用仍是只读态）。
- **工坊 / 收藏：下载量常驻封面右上角**：订阅数（= 下载量）原来只在悬浮抽屉里可见，
  鼠标不悬停就看不见；现在与「已应用 / 已下载」一样常驻封面右上角，≥1 万折算成
  `12k` 以免把角标撑成一条。收藏页的 `favorites_list` 一并补出该字段
  （`workshop_items.subscriptions`）。
- **本地库 / 订阅同步：卡片列表改虚拟滚动**：新增 `VirtualGrid`，只渲染视口内的行
  （上下各多渲染 2 行），DOM 数量与列表长度脱钩。本地库几百项、订阅同步几千项时
  不再一次性挂载全部预览图与悬浮抽屉。列宽 / 间距与原 CSS 网格同一口径
  （`repeat(auto-fill, minmax(168|150px, 1fr))`）；订阅同步的翻页哨兵改为
  「滚动接近底部」触发，内容不足一屏时也会自动续拉下一页。
- **设置：新增「缓存」分组**：显示当前缓存占用（预览图与网页缓存 + 壁纸首帧封面，
  分项列出），可一键清除。清除同时会清掉工坊 / 发现页的 localStorage 列表快照
  （`we.cache.*`），不动用户调好的筛选条件与其它设置；已下载的壁纸与数据库不受影响。
- **托盘菜单：新增「全局滤镜效果」（排在「帧率上限」下面）**：无 / 高斯模糊 / 黑白 /
  怀旧 / 鲜艳 / 暖色 / 冷色 / 反色 / 提亮 / 压暗 / 高对比（文案与上游测试台一致）。
  与上游独立测试台同一套实现 —— 原生侧只传白名单 id，CSS filter 表达式只存在于
  渲染器里（避免任意字符串流进 `style.filter`），挂在渲染容器上，scene / web /
  video / gif / image 各类型热切生效且不重挂壁纸。**只作用于桌面壁纸窗口，
  壁纸预览不受影响**。

### 🐛 修复 / Fixed（本轮）

- **卡片标题不再无限撑高**：悬浮抽屉里的标题虽然写了 `line-clamp-2`，但同一个元素上
  还挂着 Tailwind 的 `.block`（`display:block`），而构建产物里 `.block` 排在
  `.line-clamp-2` 之后，把它自带的 `display:-webkit-box` 覆盖掉了 —— 两行截断被静默
  取消，长标题会一直往下长。现在截断落在外层按钮里的 span 上（`<button>` 在 WebKit 里
  会把内容包进匿名块，clamp 不保证生效），且元素上不再挂任何 display 工具类：
  标题严格最多两行，超出显示省略号。
- **本地导入的壁纸按「视频 / 场景 / 网页」筛不到**：`library_items.tags` 在导入路径上
  从来没被写过，而筛选面板的「类型」组读的是 `l.tags ∪ w.tags`，本地导入在工坊元数据
  缓存里又没有记录 —— 导入一段视频后按「视频」筛选结果是空的。现在导入时按类型写入
  与工坊同名的标签（Video / Scene / Web / GIF / Application），并加 db v6 迁移为
  存量条目按 `type` 列回填。
- **虚拟滚动「滚过去一片白 + 页面卡住」**（第二轮：白带仍偶发，定位到「DOM 窗口
  落后于真实滚动位置」）——实测截图里前缘空白约 1.2 行、而滚动条显示已滑过约 20%，
  即渲染出的行窗口比真实偏移落后 3～4 行。成因是三条叠加：
  ① 合成器接管滚动时，画面上滚过的距离会领先主线程读到的 `scrollTop`；
  ② 动量滚动/合成器滚动期间 `scroll` 事件可能被合并甚至丢掉，只靠事件读一次偏移，
  窗口就停在旧位置；
  ③ 每跨一行要把窗口内几十个卡片全部重渲染，主线程被占满 → rAF 也被饿死，DOM
  越来越跟不上（就是「一片白 + 卡住」）。
  对策：**滚动跟踪改为 rAF 轮询**（`scroll`/`wheel` 只当叫醒信号，偏移还在变就一直
  跟，停手后再收尾校准）；**按滑动方向前瞻多渲染几行**并加滞回（只扩不缩，停手
  250ms 后收回）；**缓存每个格子的 React 元素**，窗口跨行时只重建新进的那几格，
  其余整棵子树靠「同引用 bailout」跳过，把每帧渲染成本从几十毫秒压到几毫秒；再加
  一道 400ms 看门狗（定时器不会像 rAF 那样被饿死）对账「屏幕上的窗口还盖得住当前
  偏移吗」，盖不住就自愈并写日志。
  另外上一轮已修的三处仍然生效：滚动偏移不直接进 state（窗口没变就跳过渲染）、
  虚拟列表里的卡片改用 `eager` 加载（`lazy` 的进入视口判定在虚拟列表里会被跳过，
  图片一直白着）、尺寸测量只接受有效值且列表变短后显式把 `scrollTop` 夹回
  滚动范围（不再指望浏览器「自动夹回 + 补发事件」）。订阅同步的翻页在「整页都是
  重复条目」时会一直续拉（每页一次请求 + 一次重渲染），现在翻到没有任何新条目就
  直接判定到底。

### 🛠️ 健壮性 / Robustness（壁纸导入重构）

导入逻辑整体重写（`library.rs`），修复多个潜在数据损坏点：

- **递归自拷防护**：误选壁纸库目录本身或其祖先目录时，旧逻辑会递归拷贝
  自身直到磁盘爆满；现在 canonicalize 后做双向重叠校验，直接拒绝
- **失败可回滚**：拷贝/入库任何一步失败都会清掉半成品目录，不再留下
  无数据库记录的孤儿目录；空目录、空文件、不支持的扩展名提前报错
- **批量导入容错**：Web 版数据导入逐条 try，单个条目失败记入 `failed`
  数组并继续，不再一个坏条目拖垮整批；同名残留文件先清理再拷贝
- **预览图 bug**：旧实现把同一图片源同时写成 `preview.gif` 和 `preview.png`
  两个扩展名对不上的文件；现在按真实扩展名只写一份（含 webp 支持）
- **重复导入幂等**：同名同大小文件再次导入直接复用已有条目，不再产生
  `name-2`/`name-3` 垃圾副本（前端提示「已在库中」）
- **中文文件名**：旧逻辑清洗后 item_id 退化成 `----` 且全叫 "custom"；
  现在用名称的稳定短哈希兜底（`custom-<hash>`），标题保留原始文件名
- **类型推断确定性**：混合内容目录按 video > gif > image > web 优先级判定，
  与 read_dir 枚举顺序无关；`preview.*` 不再参与类型推断
- **不卡 UI**：两个导入命令改为 async + spawn_blocking，大目录拷贝不再
  冻结主线程；新增 20 GiB 上限与磁盘剩余空间预检（fs2）
- **符号链接**：拷贝时跳过 symlink 与特殊文件，避免循环链接/拷入链接目标
- 新增 12 个导入核心单元测试（macOS 90/90、Linux 80/80 全过）
- **本地导入三入口**（同一健壮核心，逐条容错）：
  - 文件选择框支持**多选**批量导入；新增**文件夹选择**入口（WE 工程目录）
  - **拖拽导入**：文件/文件夹拖到窗口任意位置即可批量导入（悬停显示提示遮罩）
  - 批量结果统一汇报「导入 N / 已在库 M / 失败 K（附首条失败原因）」
  - 新增 `library_import_folder_pick` / `library_import_custom_batch` 命令

### 🐛 修复 / Fixed

- **自定义导入视频本地库卡片无封面**：此前导入侧注释「视频没有抽帧能力，
  暂不支持预览」，视频条目永远没有 preview.*，卡片无封面。现在复用系统
  壁纸同步的抽帧实现（macOS AVFoundation / Linux 系统 ffmpeg）：单文件
  视频导入与无 preview.* 的视频目录导入都会抽首帧生成 preview.png；
  本功能上线前导入的存量视频，在库列表加载时惰性补抽（一次成型）。
  mkv/avi 等系统解不了的格式抽帧失败只记日志，不影响导入

- **macOS 交互态新建壁纸窗口可能黑屏（竞态加固）**：壁纸窗口是 visible(false)
  创建的，建窗瞬间 apply 桌面属性时 occlusionState 尚无 visible 位（实测 8192），
  页面若恰在此时装载，WebKit 会把它判为不可见而停帧（黑屏、时现时好）。
  现在 show() 之后按既有经验重断言一次桌面属性（重设层级 +
  orderFrontRegardless，幂等），遮挡态与合成层级都被矫正
- **macOS 交互态自动暂停不恢复（两处信号补齐）**：「隐藏图标」开启后，点击
  桌面点在本应用的壁纸窗口上 —— 无边框窗不能成为 key window，点击不一定
  触发应用激活，前台应用可能一直停在别的应用上，「回桌面恢复」依赖的前台
  切换通知永远不来。现在双保险：①自身前台化且设置窗不可见时按桌面语义
  恢复；②新增 local event monitor，落在桌面级（壁纸）窗口上的鼠标按下
  直接视为回到桌面并恢复自动暂停挂起的播放（不依赖前台切换，无需权限）
- **自动暂停时切换新壁纸以暂停态挂载**：自动暂停挂起期间应用新壁纸，新壁纸
  会以暂停态出现（看起来像壁纸坏了）。现在用户显式应用（wallpaper_apply /
  wallpaper_apply_item）成功时，若暂停是「自动暂停」挂的则立即恢复播放；
  用户手动暂停不受影响，轮播（next → apply_item_inner）不触发 —— 后台
  自动切换不该在用户看不见时恢复播放白烧 GPU
- **macOS 交互态自动暂停不恢复**：「隐藏图标」开启后，点击桌面实际点在本应用
  的壁纸窗口上（位于图标之上且接收鼠标），前台应用变成 WallpaperEM 自己而非
  Finder —— 自动暂停的「回桌面恢复」分支只认 Finder，导致切走暂停后点桌面
  永不恢复。现在自身前台化且主设置窗/属性窗均不可见时按桌面语义处理
  （恢复自动暂停 + 视为桌面活动）；设置窗可见时保持原有的中性语义
- **macOS 交互态右键弹原生菜单**：「隐藏图标」开启后壁纸窗口位于桌面图标
  之上、直接接收原生鼠标事件，右键会弹出 WKWebView 的默认上下文菜单
  （重新载入 / 存储图像 / 检查元素等）——渲染器 JS 层的 contextmenu
  preventDefault 拦不住 UI 进程组装的原生菜单。现在给 wry 的 WryWebView
  类挂 willOpenMenu:withEvent: 覆写，桌面级窗口（level < 0）清空菜单项
  不再弹出，普通窗口（主设置窗等 level = 0）仍转发原实现，保留输入框
  的复制粘贴菜单；rightMouseDown / DOM contextmenu 事件照常到达页面

- **Linux 桌面图标被壁纸盖住（GNOME）**：`_NET_WM_WINDOW_TYPE_DESKTOP` 只保证
  壁纸窗口低于普通窗口，桌面层内部仍按映射顺序堆叠——DING（GNOME 桌面图标
  扩展）等图标覆盖窗口先映射，壁纸窗口会把图标整个盖住。现在壁纸窗口映射后
  显式 `XLowerWindow` 沉到桌面层底部，图标恢复浮在壁纸之上。仅对「图标是
  独立透明覆盖层」的 DE（GNOME/Budgie/Pantheon，按 `XDG_CURRENT_DESKTOP`
  判定）下沉；KDE/XFCE 的桌面窗口自绘不透明背景，沉底会让壁纸不可见，
  这些 DE 保持「壁纸在上」的既定降级（KDE 的图标与壁纸同窗口绘制，
  纯堆叠无解，需 Plasma 壁纸插件）
- **Linux 视频壁纸黑屏/无声（GStreamer 缺失）**：WebKitGTK 的音视频播放依赖系统
  GStreamer 插件，而 libwebkit2gtk-4.1-0 对它们仅是 Recommends，最小化安装的系统
  缺失时只有 WebKit 进程里一行 `GStreamer element appsink not found`，视频壁纸黑屏。
  - deb 包 Depends 写入 `gstreamer1.0-plugins-base/good/bad` + `gstreamer1.0-libav`
    （H.264 解码），Recommends 写入 `gstreamer1.0-pipewire`
  - 新增 Linux 启动体检 `check_gstreamer_elements`：探测 appsink/autoaudiosink/
    playbin/H.264 解码器（avdec_h264 或 openh264dec 二选一），缺什么把
    Debian/Fedora/Arch 的安装命令直接写进日志
  - README Linux 章节补充各发行版手动安装命令（AppImage 无法捆绑系统插件）

### 🐧 新平台 / Platform（Linux 适配）

首个 Linux 版本（X11 会话 + GNOME/KDE/Cinnamon/MATE/XFCE 为主路径）。
macOS 行为零变化（全部单测通过），平台代码收敛为统一门面层。

- **架构**：新增 `wallpaper/platform.rs` 平台门面（`active_screens` / `apply_desktop_window`
  / `cursor_state` / 前台观察者等统一签名），`wallpaper/macos.rs` 收编为 macOS 后端，
  新增 `wallpaper/linux.rs` 后端；`audio_capture.rs`、`now_playing.rs` 拆为
  公共层 + 平台子模块（`audio_capture/{macos,other}.rs`、`now_playing/{macos,linux,other}.rs`）
- **桌面层窗口（Linux）**：X11 `_NET_WM_WINDOW_TYPE_DESKTOP`（经
  tauri-plugin-desktop-underlay 的 GTK type hint）+ 跨工作区可见 + 鼠标穿透
  （对应 macOS 的 underlay 层级与 ignoresMouseEvents）；屏幕枚举换 Tauri Monitor
  API（per-monitor scale 换算逻辑坐标），显示器 id 用连接器名哈希保证热插拔稳定
- **指针注入（Linux）**：经 Tauri/GDK 全局光标查询（X11 有效；Wayland 协议不
  允许读全局光标，该功能自动降级关闭，交互模式不受影响）
- **主窗口磨砂（Linux）**：`window-vibrancy` 在 Linux 是空实现，主界面此前只剩
  半透明；新增 `blur.rs` 对接 KDE KWin 的合成器模糊——X11 设
  `_KDE_NET_WM_BLUR_BEHIND_REGION`（空区域 = 整窗模糊），Wayland 走
  `org_kde_kwin_blur_manager` 协议（复用 GDK 的 wl_display 连接，主窗口
  realize 后应用，闲置重建后重挂）；GNOME/Hyprland 等无对应能力的合成器
  自动降级为半透明，不报错、不影响功能
- **正在播放（Linux）**：数据源从 macOS MediaRemote 绕行换成标准 **MPRIS D-Bus**
  （`mpris` crate，1s 轮询 + 封面 file/http 拉取转 data URL），反向控制
  （播放/暂停/上下曲）同样走 MPRIS —— 比 macOS 方案干净得多
- **系统壁纸同步（Linux）**：抽帧改用系统 ffmpeg；按 `XDG_CURRENT_DESKTOP`
  分发 GNOME/Cinnamon（gsettings）/ MATE / XFCE（xfconf-query 逐监视器）/
  KDE（plasma-apply-wallpaperimage 或 qdbus）/ swww / feh
- **steamcmd（Linux）**：切换 `steamcmd_linux.tar.gz`，预热产物检测改为
  linux32/linux64 的 steamclient.so；新增 32 位 multilib 缺失的前置检测与
  分发行版安装提示（`MULTILIB_REQUIRED`）；Rosetta 检测限定 macOS
- **音频捕获（Linux）**：暂不支持（待接入 PipeWire），开关给出明确提示而非
  权限报错；`AudioStatus` 新增 `supported` 字段，设置页据此降级
- **打包**：`bundle.targets` 改为 `["app","dmg","deb","appimage"]`，新增
  `bundle.linux` 元数据与 `bundle.category`；新增 GitHub Actions
  `build-linux.yml`（ubuntu-22.04，含 webwallgl 平级检出构建、deb+AppImage
  产物上传与 Release 附件）
- **设置页**：平台感知文案（自启/代理/系统壁纸/关于），不再硬编码 macOS

已知限制（Linux 首版）：Wayland 仅部分支持（壁纸窗口表现为置底全工作区窗口，
无严格桌面层；GNOME Wayland 无法桌面层为上游限制）；「自动暂停」与「前台
应用检测」暂无 Linux 实现（开关不生效）；系统音频捕获待 PipeWire 接入；
托盘在 GNOME 下需 AppIndicator 扩展。


### ✨ 新功能 / Features（系统「正在播放」媒体集成）

- **壁纸可读到系统正在播放的歌曲**：歌名 / 艺人 / 专辑 / 播放进度 / 封面，
  对应 WE 的 `wallpaperRegisterMediaPropertiesListener` 等四个回调
  （webwallgl 侧的 `SceneInstance.setMedia`）。此前这块从未接入，
  壁纸看到的是库内置模拟源的**测试假数据**
- **覆盖所有播放器**，含 Electron 应用（Apple Music、Spotify 之外，
  网易云 / LX Music 这类既不响应 Apple Event 也不广播分发通知的也能读到）
- **反向控制**：壁纸里的播放/暂停、上一曲/下一曲按钮会真的控制到系统播放器
- **封面取色**：主色 / 次色 / 第三色 / 文字色 / 高对比色，供壁纸做配色过渡

实现要点：数据源是 macOS 私有框架 MediaRemote。自 macOS 15.4 起
`mediaremoted` 按**调用进程的 bundle id** 鉴权（硬编码 `com.apple.` 前缀），
本应用直接 dlopen 调用只能拿到空字典。绕行办法是借系统自带的
`/usr/bin/perl`（bundle id `com.apple.perl`）加载
[mediaremote-adapter](https://github.com/ungive/mediaremote-adapter)（BSD-3-Clause，
已 vendor 到 `src-tauri/vendor/`）—— 限制是进程级而非签名级，
所以 adhoc / 开发者签名的构建同样可用，且无需用户额外安装任何东西。

已知限制：这是私有 API 的绕行，Apple 可能在未来版本封掉（届时降级为
「无媒体」而非报错）；App Sandbox 会阻止 spawn perl，故不适用于 Mac App Store 分发。

### ✨ 新功能 / Features（视频壁纸 WebCodecs 渲染路径）

- **静音循环视频壁纸改走 WebCodecs 逐帧调度**（webwallgl 新增 mediabunny
  demux → VideoDecoder → canvas 渲染路径），元素级 `<video>` API 在 WKWebView
  的整类不确定性（冷管线 ~1s、ended 丢失、静默暂停）从根上消失：
  - 循环点 = 队列消费回绕，帧级精确，没有任何事件/状态机可丢失；
    上一圈遗留尾帧优先丢弃防堵队，解码前瞻队列循环点空窗自适应加深（6→16 帧）
  - **帧率上限对视频壁纸真正生效**（`<video>` 的解码率由内容决定，HTML 无
    限帧 API；WebCodecs 路径的呈现节奏完全自控，跟随场景帧率设置 30/60/120）
  - 暂停/恢复是纯时钟操作；fit 热更每帧读取立即生效
  - 有声壁纸仍走 A/B `<video>` 路径（本路径整体忽略音轨）；运行中取消静音
    会自动回退 A/B 让声音回来；WebCodecs 不可用或解码失败自动回退 A/B
- 兼容性：WebCodecs VideoDecoder 需 Safari 16.4+；编码不可解（canDecode
  检查失败）时启动即回退，不会黑屏
- 附带：内容服务器 Range 解析去重（改写/流式两路径共享一份）；视频拉流中止
  Range 请求产生的 Broken pipe / Connection reset 日志降为 trace（拉流缓冲
  够了就取消请求是常态，不再刷 DEBUG 日志）

### ✨ 新功能 / Features（系统壁纸同步）

- **「自动设置系统壁纸」选项（设置 → 通用，默认开启）**：应用动态壁纸后自动
  抽首帧设为 macOS 静态壁纸 —— 锁屏、登录窗口、壁纸引擎未运行时与桌面
  视觉一致
  - 抽帧按类型：video 用 AVAssetImageGenerator 精确首帧（硬件解码、容差 0）；
    gif 取首帧转 PNG；image 原图直用；scene/web 等渲染器挂载成功后对壁纸
    窗口 WKWebView **实拍截图**（takeSnapshotWithConfiguration）—— 工坊预览图
    与场景实际渲染差距太大，已弃用
  - 抽帧产物按条目缓存（`system-wallpaper-<id>.png`），源文件更新才重抽；
    对所有显示器生效；失败只记日志不影响应用流程
  - scene/web 截图编排（异步，不阻塞 apply）：渲染器 mount 成功时经 /diag
    回流 ready 信号，内容服务器记时间戳；tokio 任务等「新一次」ready（60s
    超时）→ 多等 1.5s 让场景充分渲染 → 主线程对 WKWebView 截图（completion
    经 mpsc 回传，WebContent 僵死 15s 兜底）→ 写缓存 → 主线程 setDesktopImageURL
  - 实现注：走 NSWorkspace setDesktopImageURL（Apple 已标记废弃但目前唯一
    无需系统扩展的途径，vidwall 等同类项目同路线）

### ✨ 新功能 / Features（其他）

- **自动暂停（设置页 + 托盘菜单双开关，默认关）**：切到非桌面应用自动暂停
  壁纸，切回桌面（Finder 成为前台）自动播放。实现：NSWorkspace
  didActivateApplicationNotification 观察者（即时响应，不走 2s 轮询）；
  只恢复本功能自己挂的暂停（引擎新增 auto_paused 标志），用户手动暂停
  不受前台切换影响；本应用自身前台化算中性（设置窗口/托盘操作不动播放
  状态）；关闭开关时若正挂在自动暂停上立即恢复
- **托盘菜单大改版**：新增全局快速设置子菜单 —— 显示模式（裁剪/缩放/拉伸）、
  清晰度（省电/标准/高清）、帧率上限（30/60/120），单选勾选与设置页同一份
  持久化（settings 表），点击即实时下发到所有壁纸窗口；手动「暂停/播放」
  菜单项已随「自动暂停」上线而移除（全局快捷键 ⌘⇧P 仍保留）
- **本地库筛选条件持久化**（搜索词 / 排序 / 标签三态 / 仅看失效）：窗口闲置
  释放重建后完整还原筛选现场，与工坊页 `useWorkshopFilter` 同一套
  localStorage 做法与 `we.filter.*` 键约定；旧版本遗留的非法字段逐项校验
  丢弃（含已删除排序选项的回退）

### 🐛 修复 / Fixes

- **⌘H 不再把桌面壁纸一起隐藏**：⌘H 从 macOS 默认的「隐藏应用」改为
  「最小化主窗口」（应用菜单的 Hide 项替换为自定义项，Builder 级
  on_menu_event 处理）；同时桌面壁纸窗口设 `canHide=false` 兜底 ——
  任何隐藏路径（⌘H / Dock→隐藏）都不会再把壁纸窗口带走

### 🎨 界面 / UI

- 设置页「显示模式」选项文字改为**裁剪 / 缩放 / 拉伸**（原 填充/适应/拉伸）
- 设置页「侧边栏透明度」调节入口注释隐藏（代码保留），默认透明度回到 55%
- 壁纸预览弹窗参数固定写死、不跟随全局设置：**裁剪 + 省电清晰度 + 30 FPS**，
  预览只为确认内容/试调属性，压低开销保证主窗口与桌面壁纸流畅
- 独立「壁纸设置」窗口改磨砂质感（底色 alpha 0.3 → 0.88，太透看不清文字；
  仍不用 backdrop-filter —— 透明 WKWebView 里 backdrop 层会被 WebKit 丢弃）；
  标题栏改单行（图标 + 壁纸配置 + 壁纸名截断），整行放在红绿灯正下方，
  去掉旧的 pl-20 让位缩进

### ⚡ 性能 / Performance

- **主窗口闲置回收激进化：隐藏 3s 即释放 WebContent 进程**（原 60s）。
  主界面是纯管理面板，重建成本仅一次页面加载（<1s），常驻 WebContent
  进程（数百 MB）远比这个贵；下载 / Steam Guard / 扫码登录期间看门狗
  仍会跳过释放，激进值不会打断进行中的流程

### 🐛 修复 / Fixes

- **视频壁纸（WebCodecs 路径）切换到其他类型壁纸后不释放、一直占用内存**
  - 根因：销毁时靠「生成器 `return()` 透传」终止 mediabunny 解码泵；若解码泵
    正挂在一个永不 settle 的 `next()` 上（壁纸窗口被完全遮挡时 WKWebView 会
    节流 VideoDecoder 回调 —— 而切壁纸恰恰常发生在用户打开主界面、壁纸被
    遮挡的时刻），透传的 `return` 永远排在那个 `next` 后面，解码器 / 前瞻
    样本队列 / 输入源全部泄漏
  - 修复：改为直接持有并终止内层解码迭代器（`return()` 置 terminated 并唤醒
    解码泵退出，mediabunny 在 finally 里 close 解码器），外加 `input.dispose()`
    双保险（挂起的读取以 InputDisposedError 收场）；销毁途中到达的样本补
    `close()`（VideoFrame 是 GPU 资源）；初始化中途被销毁时同样释放输入
- **视频壁纸每圈循环交接处卡 ~1s（应用内明显，浏览器里几乎不可见）**
  - 根因一（内容服务器整读）：`/media` 文件服务对每个请求都整读文件再切
    Range，而 WebKit 播放期发大量小段 Range 请求 —— 4K 视频每次请求都付出
    整盘读取的磁盘/内存代价；交接处备用元素的首批取数被拖慢 ~1s。修复：
    媒体文件改流式服务（Range 只 seek + 读请求区间，完整请求也流式写），
    仅 HTML 注入 shim / project.json 合并属性这类需改写响应体的小文件整读
  - 根因二（webwallgl 交接时序按 Chrome 设计）：WKWebView 冷管线从 play()
    到真正出帧可达 ~1s（Chrome 几乎即时），0.12s 的爬行提前量远不够，
    淡出一完成露出的是还没出帧的定格首帧。修复（video-loop v5）：爬行
    提前量按实测出帧延迟自适应放大（≤1s，回绕未确认则逐圈翻倍）；备用
    确认出帧前不关主元素原生 loop（宁愿原生回绕微卡顿，不露定格首帧）；
    提前量放大后爬行速率自动降档（1/8→1/16），交接跳跃压回 ~2 帧
  - 根因三（自愈看门狗误伤交接）：WKWebView 把爬行拨回 1x 后帧推进有暖管
    延迟，看门狗在交接瞬间误判「假播放」，踢 play → seek → 重载轮番干预，
    把 ~0.1s 暖管拖成 ~1s 卡顿。修复：交接后给看门狗留实测级宽限期
  - 附带：帧率表改按 requestVideoFrameCallback 真实呈现帧打点（WKWebView
    不回调，rAF+currentTime 兜底去重），120Hz 屏上 30fps 视频不再报成 120。
    注：帧率上限语义仍只作用于场景渲染循环 —— DOM 直显的原生视频解码率
    由内容本身决定，无法也不需限帧
- **视频壁纸正常播放一段时间后永久卡住（webwallgl A/B 无缝循环看门狗盲区）**
  - 根因一（静默暂停）：元素被系统节能/遮挡策略直接 pause 时没有任何可靠
    事件可听，而旧看门狗只检测 `!paused` 的「假播放」形态，对这种永远
    不作为 —— 壁纸永久定格在某一帧
  - 根因二（ended 丢失）：爬行交接期间 `ended` 事件被节流/合并丢失时，
    「等 ended 精确交接」变成永久等待，主元素停在末帧（paused=true，
    看门狗同样够不着）
  - 根因三（恢复手段太弱）：假播放的唯一恢复动作是 pause→play，解码器
    上下文真坏死时这一脚踢不醒，每 500ms 重复同一无效动作直到永远
  - 修复：看门狗改为覆盖「假播放 + 静默暂停」两种形态（连续 ~500ms 无
    进展即发现）；恢复改为逐级升级 —— 踢 play → seek 强制重建解码上下文
    → 重载媒体管线并 seek 回原位（最后手段，闪一次首帧但好过永久冻结）；
    爬行交接新增 ended 丢失兜底（主元素已自然结束或爬行超 ~1.5s 未交接
    时强制补交接）；每次自愈动作经 `onRecover` 上报诊断日志
- **预设型壁纸黑屏**：WE「另存为预设」的物品（project.json 只有扁平 `preset`、
  没有 `general.properties`）属性解析结果为空，壁纸拿不到任何配置，
  表现为整页黑屏。现在从 `dependency` 指向的基础壁纸继承属性类型定义，
  用 preset 的值覆盖
- **切换壁纸后系统音频没接上**：补装音频源的轮询写在模块顶层且是一次性的，
  切壁纸后的新实例永远拿不到音频。纯音频可视化壁纸因此一直是空画面

### ✨ 新功能 / Features（完整筛选，对齐 Wallpaper Engine）

- **工坊页：API 级完整筛选**
  - 新增共享标签目录 `src/lib/tags.ts`，复刻 WE 官方标签体系六个分组：
    类型 / 年龄分级 / 分类 / 题材（25 项）/ 分辨率（25 项）/ 功能特性（10 项）
  - 标签三态：点击循环「选中 → 排除 → 取消」，分别映射到 Steam 的
    `requiredtags[]` 与 `excludedtags[]`
  - 新增排序项（最新发布 / 最近更新 / 最高评分）与趋势时间范围
  - 默认：趋势排序、全部时间、**默认只看大众级**。
    注意这一条与 Steam 网页端不同 —— 实测 Steam 默认不做任何年龄过滤，
    这里刻意收紧以免首屏直接推成人内容，用户可在面板里改
  - **筛选条件全局共享**：发现页的随机推荐复用工坊页的条件，
    在工坊排除了成人内容，发现页也不会再推；条件变化时发现页自动重新取一批
    （后端 `workshop_random` 相应改为接受完整筛选参数）
- **本地库页：数据库级完整筛选**
  - 标签交集/排除、标题搜索、只看失效条目、6 种排序
  - 筛选全部下推到 SQL，不在前端过滤

### 💄 界面 / UI

- 筛选改为**左侧滑出的半透明抽屉**，覆盖在内容之上（呈现方式与详情页抽屉一致：
  遮罩淡入 + 面板 translate-x 滑入，只是方向相反）。半透明 + 背景模糊让下层壁纸
  仍隐约可见，点遮罩或按 Esc 关闭
- 工具栏保留「筛选」按钮（带生效条件数角标）+ 搜索 + 排序；移除与标签面板重复的
  类型按钮组
- 类型分组去掉「程序」：本地 12k 条工坊缓存里零条，选了必然空结果
- 年龄分级改名为大众级 / 指导级 / 成人级
- 侧边栏收缩态宽度 52px → 78px，对齐 macOS 红绿灯按钮组（原先红绿灯会越过
  边界压在内容区上）
- 网格列数改为自适应（2/3/4 列），避免筛选列占位后卡片被压扁

### 🐛 修复 / Fixes

- **`library_items.tags` 从未被写入**（实测 244 行全空），本地标签筛选原本
  必然失效。已在三处入库补写，并加 db v4 迁移从工坊缓存回填 + 建索引
- **`library_list` 的 SQL 由字符串拼接改为参数绑定**。原实现把类型值直接拼进
  WHERE，加入标题搜索这类自由文本后即成注入面；排序键改用白名单映射
- **`actualsort` 参数实测完全无效**（传 `toprated` 仍返回 trend 序），不再发送
- **`days` 仅对 `trend` 排序生效**，其他排序下不再发送，UI 同步禁用并说明
- 补齐 `WorkshopItemSummary.timeCreated`（Rust 侧一直有，TS 类型漏了）

### 💄 界面 / UI（图标与空状态）

- **图标集改为圆润可爱线性风**（`components/icons.tsx`，19 个图标全部重绘）
  - 描边 1.8 → 2.1px、端点与拐角全圆角，形状整体饼度化（矩形圆角加大、
    直角改弧线、比例更矮胖）
  - 关键图标加了克制的点缀：工坊四格补一颗四角星、月亮配双星、
    收藏心形加高光、预览眼睛加瞳孔高光、设置改花瓣状齿轮、
    跟随系统改「左半太阳右半月」
  - 保持 `currentColor` + 24 viewBox + 原有导出名，**调用方一行未改**，
    仍能跟随主题明暗与 hover 变色
- **新增 EmptyState 组件**，空状态从一行灰字改为「插图 + 标题 + 引导」
  - 纯文字的空页面容易被误读成加载失败；插图能明确传达「这里本来就是空的」
  - 5 张内联 SVG 插图（睡觉的云朵 / 空相框 / 空心 / 放大镜 / 滑杆），
    与图标集同一套描边语言，跟随主题变色、零体积、不依赖外部资源
  - 接入 6 处：下载队列为空、本地库为空、收藏为空、工坊搜索无结果、
    壁纸无可自定义属性、属性搜索无结果

### ✨ 新功能 / Features（消息提示与操作确认）

- **新增通用 Message 组件（toast），替换散落各页的灰字条**
  - 原实现是每页各自维护 `const [msg, setMsg]` + `{msg && <div>…</div>}`：
    挤占布局、不会自动消失、成功和失败长得一样（靠 ✅ emoji 区分）
  - 现为右上角浮层：语义色条 + 线性图标（跟随主题，不用 emoji）、
    毛玻璃卡片、进出场动画、最多堆叠 3 条。成功/提示 2.6 秒自动淡出，
    **错误类不自动消失**（失败信息通常需要看清并处理），可手动关闭
  - 用法：根部挂 `<MessageProvider>`，页面里 `const msg = useMessage()`
    然后 `msg.success(...)` / `msg.error(...)` / `msg.info(...)`
  - 本地库、发现、详情、设置、下载五个页面全部迁移，`setMsg` 已清零
- **危险操作补齐确认弹框**，防止误触：
  - 本地库「清理失效条目」—— 会连带删除自定义配置，不可恢复
  - 设置「登出」—— 会清除账号密码与 steamcmd 登录态，需重新过 Steam Guard
  - 下载「清空已完成」—— 批量删除任务记录
  - （「删除壁纸」原本已有确认，保持不变）
- **移除下载页残留的原生 `alert()`**，改用统一的错误提示

### 🐛 修复 / Fixes（本地库与磁盘对账）

- **壁纸文件被手动删除后，数据库仍留着过时记录**
  - 现象：条目照常出现在本地库（有工坊元数据时连预览图都正常），点「应用」
    报的却是「未找到视频文件」「不支持的壁纸类型」这类语义错位的错误；
    若该条目曾被应用过，还会一直挂着绿色「已应用」徽章且应用按钮被永久禁用
  - `library_list` 现在与磁盘对账并标记 `missing`，本地库顶部出现提示条，
    卡片打「文件丢失」徽章、应用按钮置灰
  - 新增 `library_prune` 命令与「清理失效条目」按钮，由用户显式触发。
    **刻意不自动删**：删除不可撤销且会连带丢掉用户的属性覆盖
  - 安全闸：壁纸库根目录不可读时（数据目录未就绪、TCC 权限缺失）一律跳过
    判定与清理，避免把整个库误判为失效后一键清空
  - 判据是「目录存在**且非空**」而非仅 `is_dir()`：只剩空壳目录同样是不可用状态
- **删除壁纸时残留数据未清干净**
  - `web_props:{item_id}` 属性覆盖未清 —— 删掉再重新下载同一壁纸，
    上次的自定义配置会「复活」
  - 播放列表的 `item_ids` 与 `settings.active_playlist` 快照未同步摘除该条目，
    轮播每轮都会在失效条目上空转一次（且失败只记 debug 日志，用户看不到）
  - 已结束的下载历史（done/failed）未清；进行中的任务保持不动
  - 上述清理抽成 `purge_item_records`，`library_delete` 与 `library_prune` 共用，
    保证两条路径语义一致。`workshop_items`（元数据缓存）与 `favorites`（用户意图）
    刻意保留
- **应用已丢失文件的壁纸时错误信息误导**：`resolve_item_config` 补上目录存在校验，
  直接报「壁纸文件已丢失（可能被手动删除）」
- **`wallpaper_active_items` 过滤已丢失条目**，修掉「已应用」徽章骗人且无法重新应用的问题
- **本地库删除按钮缺少错误处理**：`remove_dir_all` 失败会产生未捕获的 rejected promise

### 💥 破坏性变更 / Breaking（移除 DepotDownloader 后端）

- **下载工具只保留 Valve 官方 steamcmd，DepotDownloader 后端整体移除**
  - 移除 82 MB 的 `src-tauri/binaries/depot-downloader-aarch64-apple-darwin`
    及配套的 `externalBin` 打包项、`.gitattributes` 的 Git LFS 规则、
    CI 的 `lfs: true` —— 仓库不再依赖 Git LFS
  - 代码层拍平：`Backend` 枚举与 `Downloader` trait 抽象在只剩一个实现后已无价值，
    `backend.rs` 直接暴露具体的 `SteamCmd` 类型，去掉一层间接
  - 随之移除：扫码登录整条链路（`qr_login` / `spawn_qr_reader` / `QrEvent` /
    五个 `download:qr-*` 事件 / 设置页的扫码弹窗与登录方式切换 UI）、
    后端选择器 UI、`download_backend_get`/`set` 与三个 QR 命令（共 5 个 tauri command）、
    `Writer::Pipe` 管道分支（steamcmd 必须走 PTY，该分支永不执行）
  - entitlements 精简：`allow-jit` 与 `allow-unsigned-executable-memory` 是
    DepotDownloader 的 .NET CoreCLR 所需，一并删除；保留
    `disable-library-validation`（steamcmd 自更新后会 dlopen Valve 签名的
    `steamclient.dylib`）
  - **登录方式变化**：不再支持扫码登录，改为账号密码 + Steam Guard 验证码。
    老用户升级后需重新登录一次
  - 一次性迁移：DB 迁移到 v3 清除遗留的 `download_backend` 设置键；
    启动时删除旧的 `<app_data>/dd-config` 登录令牌目录

### ♻️ 重构 / Refactor（渲染器改为 npm 依赖）

- **场景渲染改用独立维护的 [`webwallgl`](https://www.npmjs.com/package/webwallgl) 库**
  - `renderer/src/main.ts` 从 1798 行压到 544 行，只保留宿主适配层职责：
    URL query 解析、`window.__wp` 控制面、默认壁纸降级、诊断上报
  - 删除 `renderer/vendor/`（27 个文件，约 6000 行 vendored we-scene 引擎），
    其中的踩坑记录存档到 `docs/we-scene-notes.md`
  - Rust 侧零改动：URL query、`window.__wp` 接口、`/media` 与 `/web` 路由、
    project.json 覆盖值合并（含 `userOverridden` 语义）全部沿用原契约
  - scene.pkg 的三种布局（`scene.pkg` / `scenes/scene.pkg` / `gifscene.pkg`）
    改由库内 `httpSource` 逐个回退，且每次 fetch 单独 try/catch —— 顺带修掉
    Tauri 自定义协议对不存在路径抛 `TypeError` 而非 404、一抛就整场失败的问题
  - **属性热更不再重挂载**：改属性走库的 `setProperties()`，不必像旧实现
    那样重新下载解析上百 MB 的 scene.pkg
  - 仍留在本仓库的两条路径（待上游补齐后收敛）：
    - `web`：库不提供 GPU 节流与右键屏蔽注入，这是本宿主能力
    - `video`：库 1.1.0 的公共 API 只按 `project.type` 分流出 scene/web，
      video 会掉进 scene 分支去拉不存在的 scene.pkg

### 🐛 修复 / Fixes（构建与类型）

- **dev 模式下壁纸窗口全黑、预览与应用都无反应**
  - 根因：渲染器改用 npm 依赖后，vite 把裸模块重写成
    `/node_modules/.vite/deps/webwallgl.js?v=<hash>` 绝对路径，而
    `content_server` 的路由白名单只有 `/renderer`、`/assets` 等前缀，
    该请求 404 → 渲染页首个 import 就失败、整个模块不执行。
    症状具有迷惑性：连 `/diag` 诊断都发不出来，日志里干净得像没加载过
  - 修复：dev 下放行 `/node_modules`、`/@vite`、`/@id` 前缀转发给 vite；
    同时让 `proxy_renderer` 原样透传 query（vite 预打包依赖靠 `?v=<hash>`
    区分版本，丢掉会拿到 404 或过期产物）
  - 仅影响 dev：prod 由 vite 打包进 `/assets`，已在白名单内
- **webwallgl 的类型声明在消费侧静默失效**
  - 根因：`webwallgl.d.ts` 写的是 `export { type X } from "./types"`，
    这只做转发导出、**不把名字引入本文件作用域**，下方函数签名引用到未定义
    标识符（`tsc` 报 7 个 TS2304）。消费方普遍开着 `skipLibCheck`，报错被吞掉，
    后果是库的所有导出静默退化成 `any` —— 看起来能用，实则零类型检查
  - 已在上游改为 `import type` + `export type` 并重建产物；下游用
    `patches/webwallgl@1.1.0.patch`（pnpm patch）固化，待上游发版后移除
  - 同时关闭本项目的 `skipLibCheck`，避免同类问题再被静默吞掉

### ✨ 新功能 / Features（下载后端可切换：steamcmd）

- **下载工坊内容改用 Valve 官方 steamcmd，并保留 DepotDownloader 为可选后端**
  - 抽象：新增 `download/backend.rs`，把两个工具的差异（命令行拼装、输出正则、
    产物布局、进程收尾方式、是否需要 PTY、是否支持扫码）收敛到 `Downloader` trait；
    队列、状态机、产物收编与依赖补拉逻辑完全复用，与后端无关
  - 切换：设置 → 下载新增后端选择器，选择存 `download_backend`（默认 `steamcmd`）；
    有任务进行中时拒绝切换。两个后端的登录态互不相通，切换后会提示需重新登录
  - 安装：新增 `download/steamcmd_install.rs`，首次使用时从 Valve 官方源下载
    引导包（约 2.5 MB）解压到 `<app_data>/steamcmd/`，去 quarantine 后预热自更新
    （落地约 85 MB）；解压复用系统 `tar`，未引入 flate2/tar 依赖
  - Rosetta：steamcmd 的官方引导二进制仍是 x86_64，Apple Silicon 上首次启动依赖
    Rosetta 2。安装前显式检测，缺失时给出 `softwareupdate --install-rosetta` 提示
    而不是让子进程无声失败（首次自更新后 steamcmd 即以原生 arm64 运行）
  - PTY：新增 `download/pty.rs`。steamcmd 检测到 stdin 非 TTY 会直接
    `cannot read from the console` 退出，故用 `posix_openpt` 分配伪终端，
    子进程 `setsid` + `TIOCSCTTY` 取得控制终端，验证码经 master 端写回
  - 凭据隔离：steamcmd 没有 configdir 参数，登录态固定写在 `$HOME` 下，
    因此给子进程重定向 `HOME` 到 `<app_data>/steamcmd-home/`，
    既不污染用户真实 Steam 配置，登出时也只需删该目录
  - 收尾：macOS 上 steamcmd 下载成功后常因 Steam API 拆卸线程竞争而不响应 `+quit`，
    故匹配到 `Success. Downloaded item` 即主动结束进程，不依赖退出码；
    看门狗对 steamcmd 放宽到 30 分钟（其下载期间完全静默）
  - 进度：`workshop_download_item` 期间 steamcmd 无任何进度输出，改用
    「轮询产物目录体积 ÷ 条目 `file_size`」估算；总大小取不到时下发 -1，
    前端渲染为不确定态进度条
  - 错误：steamcmd 把「账号未拥有 WE」和「真的断网」都报成 `No Connection`，
    无法区分，错误文案同时列出两种可能并引导去做连通性探测
  - 限制：steamcmd 不支持扫码登录，选中该后端时设置页隐藏扫码入口；
    且同账号在 Steam 客户端与 steamcmd 中只能登录一处，下载会挤掉运行中的客户端

### 🐛 修复 / Fixes（下载页）

- **下载完成后整个应用卡死（steamcmd 后端，实测复现）**
  - 根因：安装完成后写 `download_has_token` 的临界区里又调了 `backend_kind()`，
    而后者内部会 lock 同一把 `std::sync::Mutex<Connection>`（非重入）→ 自锁死。
    DB 连接被永久占住，任务停在「安装中」，全应用随之无响应。
    日志上表现为 `installed item ...` 之后再无任何输出
  - 修复：后端类型在取锁前先算好；补回归测试（自锁死时以超时明确失败而非挂住）
- **steamcmd 推送「手机端确认登录」时界面无任何提示**
  - 原实现只写了一行日志，用户看到任务卡在「登录 Steam」却不知要去手机点确认
  - 现发 `download:mobile-confirm` 事件，下载页显示呼吸点提示；
    状态推进后自动消除，且同一任务只提示一次（steamcmd 会反复刷该提示）
- **重启后已装好的任务被判失败，诱导重下大文件**
  - 卡在 `installing` 但产物已入 `library_items` 的任务，重启恢复时改判为成功
- **依赖补拉任务的验证码弹窗不弹出**
  - 根因：`download:guard-required` 回调用了闭包捕获的旧 `tasks`，对刚由依赖补拉
    入队、尚未进入列表的任务 `find` 返回 undefined，弹窗被置空，任务一直卡在
    authenticating 直到 5 分钟超时
  - 改为只记录 taskId，并在任务不在列表时触发一次全量刷新
- **每秒重建 IPC 监听器**：`useEffect` 依赖数组含 `tasks`，进度事件每秒触发一次
  监听器拆装。改用 ref 持有最新列表，依赖数组仅保留 `refresh`
- **`downloads.started_at` 从未写入**：前端拿到的 `startedAt` 恒为 null，
  现于任务开始时写入
- **输出解析正则每行现场编译**：一行输出要编 4 个以上正则，现按模式缓存

### ✨ 新功能 / Features（工坊依赖自动补拉）

- **壁纸下载完成后自动识别并补拉依赖工坊内容**
  - 修复：原实现只解析 `dependency`（单数，仅预设物品用），而 WE 编辑器主流写出的
    是 `dependencies`（复数数组，资产包依赖），导致自动补拉对真实壁纸基本不生效；
    现取两者并集（去重、过滤自身与非数字 ID），并兼容带 BOM 的 project.json
  - 入队：安装完成后对每个声明且本地缺失的依赖自动创建下载任务（队列去重），
    `target_dir` 记录合并目标；该依赖已在队列中时仅补记合并目标，不重复下载
  - 合并：依赖下载完成后按"缺则补"整目录回填主壁纸（不再只拷 scene.pkg 单文件，
    不覆盖主壁纸自身文件）；依赖链（依赖又声明依赖）经 target_dir 传递逐级回填
  - 自愈：主壁纸安装/应用时若依赖已在本地，直接合并补齐而不再报错；应用时缺依赖
    自动入队补拉并提示"下载完成后重新应用即可"
  - 下载页：依赖任务显示「依赖」徽标，后台自动补拉依赖条目标题（不再显示裸 ID），
    新入队任务即时出现在列表

### 🐛 修复 / Fixes

- **修复主窗口最小化后点击托盘/重新打开无效（窗口停在 Dock，软件看似无响应）**
  - 根因：macOS 上对已 miniaturize（最小化到 Dock）的窗口调用 `show()`
    （orderFront）不会自动 deminiaturize——托盘「显示主窗口」执行后窗口仍留在
    Dock 里，用户侧表现为整个软件无响应
  - 修复：`ensure_main_window` 在显示已存在的主窗口前先 `unminimize()`

- **修复合盖→重开后壁纸冻结、主界面无法打开（显示状态判定失真 + 监控任务脆弱）**
  - 根因一（关键）：`CGDisplayIsAsleep` 的进程内状态在合盖→重开后会**卡死在
    true**（后台线程轮询拿到的 CG 显示状态不随实际唤醒更新；进程外查询同刻为
    0）——`restore()` 永不执行，壁纸冻结在 released 状态；`ensure_windows` 的
    asleep 守卫也永久跳过窗口同步
  - 修复：睡眠判定改为复合信号「主线程 CG 查询 && 音频样本未流动」——
    ScreenCaptureKit 样本恢复流动即证明系统实际已唤醒，覆盖 CG 谎报；
    ensure_windows 的守卫同步使用该有效值
  - 根因二：监控任务（单 tokio 任务承载睡眠唤醒/窗口同步/僵死检测/音频看门狗）
    出现过静默 panic 死亡（tokio 任务 panic 无任何日志），死亡后全部监控职责失效
  - 修复：每个 tick 包 `catch_unwind`，panic 记录 `monitor tick panicked` 并继续
    运行；新增 60s 心跳日志（含 display_asleep 实时值）便于观察任务存活性
  - 根因三：合盖期间看门狗重启捕获会失败（SCStream「流播放无法启动音频」），
    相位进 FAILED 后无人重试——开盖后系统音频可视化永久失效
  - 修复：FAILED 相位且曾成功工作过（`ever_received`）时每 ~30s 自动重试启动，
    开盖后 ≤30s 自愈；从未成功（无权限）不重试以免反复弹授权
  - 附带：web 壁纸页活性检测（SSE 客户端数连续 3 tick 为 0 → 强制重载）与睡眠
    转换事件解耦，页面无论因何冻结均可自愈；主窗口新增冻结自检（定时器实际
    间隔远超周期即自刷新）

- **修复显示器短暂休眠后 web 壁纸冻结在最后一帧（无响应）**
  - 现象：睡眠/唤醒（或锁屏闪断）触发 release/restore 后，壁纸静止不动、
    不再响应音频与鼠标；实测 WebContent/GPU 进程 0% CPU、内容服务器无任何
    该壁纸页的连接
  - 根因：快速睡眠/唤醒序列下 WebKit 未恢复 restore() 新挂载页面的 JS 执行
    （rAF/网络全停）。restore 的 eval 已执行、iframe 已重挂，但页面加载后
    处于停滞状态
  - 修复：利用新 shim「web 壁纸页加载后必然持有 1 条 /audio-stream SSE 连接」
    的特性——内容服务器新增 SSE 客户端计数；监控任务在唤醒 5s 后检查，若
    计数为 0（页面僵死）则用当前会话配置强制 `location.replace` 重载壁纸页，
    自愈恢复动画与音频响应
  - 注：唤醒后「audio capture stalled; restarting」为音频看门狗按设计重启
    休眠期间停发样本的 SCStream，属正常现象

- **修复主窗口闲置释放后「显示主窗口」无效（主界面出不来/不渲染）**
  - 根因：watchdog 对隐藏窗口 `destroy()` 后，Tauri 注册表的条目移除依赖 macOS
    windowWillClose 事件链，orderOut 窗口不保证送达——注册表留下僵尸条目。
    此后 `ensure_main_window` 里 `get_webview_window("main")` 拿到僵尸并对其
    `show()`（静默失败），永远不重建。实测复现：释放后多次重开无任何窗口、无日志
  - 修复：自维护 `released` 标志（watchdog 销毁成功后置位）；`ensure_main_window`
    检测到该标志时把注册表条目视为僵尸——先再 `destroy()` 清场，然后带重试地
    重建（最多 10×100ms，等 label 真正释放），成功后复位标志
  - 附带修复：单实例回调（非主线程）触发的重建中 vibrancy 应用失败
    （"apply_vibrancy() can only be used on the main thread"），改经
    `run_on_main_thread` 执行
  - 附带防御：transparent WKWebView 在「销毁后重建」序列下偶发首帧不合成
    （整窗透明，观感即不渲染）——创建后 150ms 做一次 1px 尺寸扰动强制
    AppKit/CA 重新合成
  - 澄清：日志里 `audio sse client disconnected: Broken pipe` 是内容服务器对已
    消亡 SSE 客户端（预览页关闭/窗口销毁/页面重载）写入失败的常规记录，为 DEBUG
    级噪声，服务器本身并发模型为每连接独立任务，实测响应 ~1ms，无需处理

- **修复重启/唤醒后音频可视化壁纸无效果（注入时序竞态）**
  - 根因：壁纸 HTML 由内容服务器在服务时刻写入 `systemAudio` 一次性快照，而系统
    音频捕获固定延迟 3s 才启动且启动后对壁纸窗口零通知——壁纸页拿到的快照恒为
    false，shim 据此永不建立 SSE 订阅，整段会话无可视化（日志实证：壁纸窗口
    14:07:43.7 创建，捕获 14:07:46.9 才就绪）
  - 修复：启动顺序改为「先音频后壁纸」——`start_if_enabled` 提前至 `wallpaper::init`
    之前并同步标记 STARTING 相位；`wallpaper::init` 有界等待（≤6s）捕获进入
    RUNNING 后再创建壁纸窗口（新增捕获相位状态机 `wait_until_ready`，失败/超时
    不阻塞渲染）
  - 修复：shim 不再以快照作订阅门——无条件建立 `/audio-stream` EventSource，
    未就绪时端点 503、EventSource 自动重试，捕获随后就绪/设置中途打开/唤醒后
    重启均自愈，无需重载壁纸
  - 修复：唤醒后 SCStream 可能停发样本但相位仍为 RUNNING——监控任务新增看门狗，
    频谱序号连续 ~10s 不动且曾收到过样本则自动重启捕获（SSE 重连对壁纸无感知）

- **修复 VU Meter 等部分网页壁纸完全不渲染（837056186 / 893418273）**
  - 根因一：部分壁纸在赋值 `wallpaperPropertyListener` 时才初始化关键全局量
    （Circular Visualizer 在 `window.onload` 里才给 `canvas` 赋值），shim 在赋值瞬间
    同步回调 `applyUserProperties` 会抛错（读 undefined 属性），整段属性处理中断
    ——bass 系数等保持 undefined → 绘图半径为 NaN，`arc()` 静默跳过，太阳/频谱
    整体不绘制只剩背景。已在浏览器中复现并验证
  - 根因二：VU Meter 类壁纸在构造函数末尾把「已收到配置」标志复位
    （`gotSettings=false`），只回调一次则渲染循环永远跳帧，canvas 整页黑屏
  - 修复：shim 除赋值即发外，在宏任务与 `load` 之后补发属性；补发若仍抛错
    （壁纸尚未就绪）以 100ms 周期重试（上限 ~5s），全部成功即停——等价
    WE 桌面端在壁纸就绪后多次下发属性的契约
  - 根因三：`wallpaperRegisterAudioListener` 为 push 累积语义，而 WE 是单槽位替换；
    该类壁纸每次属性回调都重新注册，回调随属性下发次数堆积，同一帧音频被重复消费
  - 修复：改为 WE 替换语义（重复注册替换、传 null 清除）

- **修复 Customizable Module Visualizer 全黑（1081733658，配置问题）**
  - 排查结论：代码渲染链路无缺陷——该条目下载的 project.json 把全部模块设为
    `enabled: false`（90 个 bool 属性为 false，含 `visual_bar_enabled` /
    `clock_enable` 等），按此配置壁纸在 WE 桌面端同样全黑
  - 处理：经浏览器实测确认开启模块后渲染正常（彩虹圆环频谱 + 数字时钟，与
    预览一致）；已通过应用自带的属性覆盖机制（`web_props:1081733658`）默认
    开启频谱与时钟，可在壁纸属性编辑器中修改或清除覆盖

- **修复音频可视化无数据（系统音频捕获全零）**
  - 根因：SCStreamConfiguration 自定义 `setChannelCount(1)` / `setSampleRate(48000)`
    使音频轨静默失败——流正常启动但从不产出音频采样，频谱恒为全零
  - 修复：回退 ScreenCaptureKit 默认音频格式（48kHz/双声道）；新增采样回调、
    提取状态与频谱峰值（30s 周期）诊断日志
  - 已实测端到端：系统播放声音 → 捕获频谱非零 → SSE → 壁纸页面收到真实音频数据

- **修复开启音频可视化后应用启动即崩溃（SIGSEGV）**
  - 根因：`getShareableContent` completion 回调给出的 `SCShareableContent` 是
    +0 autoreleased 引用，误按 +1 消费（`from_raw`），回调返回后 autorelease pool
    排空即释放对象，随后的 `displays()` 对已释放对象发消息 → `objc_msgSend` 段错误
  - 修复：改为 `Retained::retain` 主动持有；启停流程包入 `autoreleasepool`
    （tokio 阻塞线程无 ObjC pool）；补 SCShareableContent 错误诊断日志

- **修复签名后 DepotDownloader 无法启动（扫码/下载失败）**
  - 根因：改用真实证书签名后 tauri 自动启用 hardened runtime，CoreCLR 的 JIT
    （W+X 内存）被拦 → `Failed to create CoreCLR, HRESULT: 0x80070008`，进程退出
  - 修复：新增 `entitlements.plist`（`allow-jit` + `allow-unsigned-executable-memory`
    + `disable-library-validation`），构建签名时应用到全部二进制

- **修复「系统反复要求屏幕录制授权」**
  - 根因：ad-hoc 签名每次构建都变，TCC 授权绑定签名 → 授权随构建失效；
    多个不同签名的副本并存进一步加剧
  - 修复：`tauri.conf.json` 固定 `signingIdentity` 为本地自签证书
    （"WallpaperEM Dev Signer"），授权跨构建持久有效（已实测：重签重部署后授权保留）
  - 注意：从旧版本升级到本版本需要重新授权一次（签名身份变更，最后一次）；
    设置页授权失败提示改为三步指引（允许 → ⌘Q 重开 → 移除重加）

### ✨ 新功能 / Features（二期：系统音频 + file 属性 + 合成音频捕获）

- **音频可视化接入系统全局声音（ScreenCaptureKit loopback，macOS 13+）**
  - 新增 `audio_capture` 模块：SCStream 音频捕获（排除本进程自身，避免反馈；
    2×2 视频占位仅取音频），Rust 端 rustfft 做 2048 点 FFT → 64 段对数频谱（30Hz~16kHz）
  - 内容服务器新增 `/audio-stream/{token}` SSE 端点，~30Hz 推送频谱；
    壁纸页内 shim 经 EventSource 订阅，未开启时 503 + 自动重连（开开关即自愈）
  - 双数据源融合：系统声音（外部帧）与壁纸自播音乐（一期 WebAudio 本地分析）逐段取最大
  - 设置页新增「音频可视化（系统声音）」开关（`wallpaper_audio_processing`），
    开启时触发屏幕录制授权提示；支持启动自启；未授权时给出明确指引
  - WebAudio 合成音频捕获：包装 `AudioContext` 构造器 + `AudioNode.connect` 旁路 tap，
    壁纸用振荡器等合成的声音同样驱动可视化

- **file 类型用户属性编辑**
  - 属性面板 file 类型新增「选择文件…」：系统选择器 → 拷入壁纸 `we-props/` 子目录 →
    写覆盖值并实时生效
  - file 属性值按入口 HTML 所在目录自动补相对前缀（`web/`、`pages/` 等），与 WE 相对路径语义一致

### ✨ 新功能 / Features（一期：WE 网页壁纸适配）

- **Wallpaper Engine 网页壁纸适配（WE Web API 兼容层）**
  - **用户属性生效**：`project.json` 中 `general.properties` 的属性值（主题色 `schemecolor`、
    开关、滑条、下拉、文本等）经内容服务器注入壁纸 HTML，通过官方
    `window.wallpaperPropertyListener`（`applyUserProperties` / `applyGeneralProperties`）
    回调下发给壁纸；传值格式与 WE 一致（颜色 `"r g b"` 浮点串、bool 布尔、slider 数值）
  - **属性可视化编辑**：详情页新增「壁纸属性」面板（取色器 / 开关 / 滑条 / 下拉 / 文本框），
    自定义值持久化（settings 表 `web_props:{item_id}`），保存后对已应用壁纸**免刷新实时生效**，
    支持单属性与整体「恢复默认」；file 类型属性本期只读
  - **音频可视化 API**：实现 `window.wallpaperRegisterAudioListener`，按 WE 格式每帧推送
    128 长度数组（64 段对数频谱 + 镜像）；本期数据源为壁纸自身的 `<audio>` 元素
    （WebAudio AnalyserNode 分析），系统全局音频（ScreenCaptureKit loopback）列入下期
  - **入口文件修正**：网页壁纸入口优先取 project.json `"file"` 字段（WE 权威入口），
    无 project.json / 无字段 / 文件缺失时回退原探测逻辑（`web/index.html` → `index.html` → 递归找首个 HTML）
  - **帧率 / 暂停 / 音量对齐**：shim 内置 rAF 节流（注入时机早于壁纸脚本，更可靠；
    渲染器原加载后注入保留为外部 URL 兜底并防双层节流）；`applyGeneralProperties({fps})`
    随设置实时联动；`__wp.pause/resume/setVolume` 同步到壁纸内 `<audio>`（暂停冻结动画并暂停声音）
  - 预览弹窗与桌面渲染共用注入链路，行为一致；下期规划：系统音频、file 属性编辑、
    WebAudio 合成音频捕获、scene 类型用户属性

---

## [v0.4.0] - 2026-08-29

### ✨ 性能优化 / Performance

- **视频壁纸改为单 `<video>` + 原生 `loop`（省内存）**
  - 放弃原先「active/standby」双视频元素无缝循环方案，改为单个 `<video loop>`
  - 采用 `preload=metadata` + 限制解码分辨率（窗口 × dpr），4K 视频源内存占用约降低 **75%**
  - 移除 `videoPair` 引用，场景内视频纹理（`videoPairs`）不受影响

- **网页壁纸 `requestAnimationFrame` 帧率节流（降 GPU）**
  - 对同源注入的网页壁纸按 `sceneFps`（默认 30）限帧
  - WebGL / canvas 动画 GPU 占用约减半，且不影响壁纸布局
  - 60fps 时则不注入，无需节流

### 🐛 修复 / Fixes

- **休眠时壁纸软件退出问题修复**
- **视频壁纸切换后彻底释放解码器，修复内存持续增长**
  - `createLoopingVideo.destroy()` 改为 `pause + removeAttribute(src) + load() + remove()` 完整释放流程
  - `clear()` 中非循环视频同样清 `src + load() + remove()`，避免 WebKit 保留已暂停 `<video>` 的解码器与解码缓冲区
- **应用 / 预览新壁纸后彻底释放 WebGL 与 blob 资源，修复内存持续增长**
  - 渲染器新增 `dispose()`（`WEBGL_lose_context`），归还 WebGL 上下文与全部纹理 / FBO / program / buffer
  - 暂停并移除场景内视频纹理元素，避免后台继续解码
  - `revokeObjectURL` 释放全部 blob URL
  - 新增 `pagehide / beforeunload` 兜底，覆盖预览 iframe 关闭与壁纸窗口销毁
- **修复重建主窗口丢失 `transparent` 属性**：补 `.transparent(true)`，恢复侧栏磨砂透明效果
- **应用名称修正**：Cargo 包名 `we-wallpaper` → `WallpaperEM`

---

## [v0.3.0] - 2026-08-27

### ✨ 新功能 / Features

- **UI**
  - 侧边栏圆滑收缩 / 展开
  - 侧栏底部主题切换按钮
  - 侧边栏磨砂质感 + 透明度调节
- **本地库**
  - 新增「导入本地壁纸」功能 + 卡片操作 SVG 图标与 tooltip
  - 标题后显示当前壁纸总数
- **设置**
  - 扫码 / 账号密码登录互斥
- **其他**
  - 发现页缓存：首次加载「发现」后缓存壁纸列表与当前选中项，切换页面直接复用、不再重复请求；仅「换一批」才会重新拉取并刷新缓存
  - 设置页改版：标签式分组（下载 / 通用 / 网络 / 关于），分类更清晰、定位更快捷；头部与标签栏与内容列同宽、整体居中，切换标签时填写状态不丢失
  - 调整默认渲染分辨率上限为 2×（原 1×），画面更清晰，仍可通过设置降低以节省内存
  - 主窗口闲置释放：隐藏 10 秒后回收多余 WebContent 进程
- **修复**
  - 修复「幽灵菜单栏图标」问题：清理旧版托盘菜单残留，菜单栏不再出现多余 / 失效的图标
  - 移除托盘右键菜单中的「下一张（轮播）」菜单项
  - v0.1.0 版本安装新版本后需迁移旧数据

---

## [v0.2.0]

### 🎨 UI 优化

- 侧栏 UI 优化与交互改进
- 设置页优化

## [v0.1.0]

### 🚀 首个版本

- 初始版本发布
