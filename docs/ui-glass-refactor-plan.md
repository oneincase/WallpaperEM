# UI 玻璃化大重构方案（透明磨砂 + 顶部胶囊导航 + 分享前置 + 全平台无边框）

> 状态：**已实施**（2026-09-26，P1-P4 全部落地，typecheck + cargo check 双绿，真机/浏览器截图核验通过）。
> 实施时相对本稿的三处修订：
> 1. 胶囊标签展开为**三层结构**（`.pill-label` 轨道层 > `.pill-clip` 无 padding 裁剪层 > 文字 span）——
>    两层结构里内层 span 的 padding 会撑住轨道最小宽度，折叠态漏 2px 文字残影（WebKit 实测）；
> 2. 新增 `force_dark_appearance`（lib.rs）：把 AppKit 外观钉成 darkAqua，否则系统浅色模式下
>    vibrancy 材质发白、深色 tint 盖不住、白字对比度掉档；
> 3. `decorations:false` 走**平台配置文件**（tauri.{windows,linux}.conf.json 已存在，往里补全量
>    `app.windows` 数组——平台合并是数组整替，不能只写增量；macOS 保持 Overlay 不受影响）。
>
> **二轮修订（同日，按用户反馈）**：① 玻璃再拉透（`--content` 0.60→0.42、`--card` 0.82→0.55），
> 新增 `--card-border` 白描边 token，`.card` 与全部悬浮面板（弹窗/toast/菜单/详情抽屉）统一换用；
> ② **macOS 也 `decorations:false`**（去红绿灯，`shadow:true` 保留系统投影），CSS 圆角全平台统一，
> vibrancy 材质圆角 16→12 与 CSS 对齐，窗口控制全平台走自绘 WindowControls（顶栏 logo/名称删除）；
> ③ macOS 系统菜单裁到「app 子菜单 + Edit」——File/View/Window/Help 全删（⌘W/⌘M 随之消失），
> Edit 用系统预定义项重建（撤销/剪切/拷贝/粘贴/全选，输入框快捷键依赖它），⌘H=最小化主窗口保留。
> **三轮修订（同日，按用户反馈「磨砂值太高 / 失焦不通透」）**：实测发现系统材质是瓶
> 颈 —— NSVisualEffectView 暗色材质自重着色 ~75%，叠 rgba 只能「更黑」，blur 值完全
> 不可调，聚焦/失焦还有观感差异。主界面玻璃改为**页内自绘**：新命令
> `wallpaper::ui_backdrop`（复用 `item_cover_summary`）返回当前应用壁纸的封面 URL，
> App.tsx 监听 `sessions-changed`/`displays-changed` 刷新，`.app-backdrop` 层做
> CSS `blur(var(--backdrop-blur, 44px)) brightness(0.5)`（**磨砂值可调**）+ tint。
> 页面自绘后与窗口聚焦状态彻底解耦，三平台观感一致；系统材质仅在未应用任何
> 壁纸时兜底透出。props 窗仍走原生材质。
>
> **四轮修订（自适应 tint）**：`library::cover_luminance`（封面缩 16×16 取 Rec.709 平均亮度，按 item_id 记忆；image crate 为此补了 gif 解码）+ `ui_backdrop` 返回 `{url, luminance}`；前端按亮度写 `--glass-lift`（0.45 以下 0、0.8 以上 1，线性），`--content/--card/--sidebar` 三个 alpha 在 index.css 里 `calc(基数 + lift * 幅度)` 联动抬升，tint 变化带 0.45s 过渡。暗壁纸保持 0.22/0.34/0.38 的轻透，亮壁纸最高抬到 0.50/0.62/0.60。
>
> **五轮修订（稳定性 + 跟随窗口）**：①多屏下背景条目改取「主窗口中心点命中的显示器」的会话（`platform::hit_screen_id`，坐标系 tauri 物理左上原点 → macOS 全局逻辑点左下原点，y 翻转用主屏高），命中失败兜底 `ORDER BY updated_at DESC`——修掉 DISTINCT 不带序导致多屏间「背景/浓度自己跳」；②亮度失败结果短缓存 10s（封面可能还在生成，但两次刷新不能一会有一会无）；③自适应曲线放缓：起点 0.5、拉满 0.9，幅度 content/card 0.22、sidebar 0.18，tint 过渡 0.6s；④前端监听 `onMoved` 防抖 350ms，窗口拖动落定（含跨屏）后刷新背景与 tint。
>
> **六轮修订（右键菜单全屏蔽）**：WKWebView 的「Reload / Inspect Element」右键菜单来自 devtools（`developerExtrasEnabled`，debug 构建默认开）—— 三份配置（tauri.conf.json + 两个平台 conf）加 `devtools:false`，三处 WebviewWindowBuilder（main_window 重建 / props_window / wallpaper）加 `.devtools(false)`；再加渲染器全局 `contextmenu` preventDefault（main.tsx + props-main.tsx）把 WebKit 原生项（Look Up/Copy 等）一并屏蔽。文本编辑仍可用 ⌘C/⌘V。
>
> **七轮修订（mac 窗口控制归位 + 填充统一 + QR 弹层）**：① macOS 自绘窗口控制移左上角、顺序照红绿灯「关闭→最小化→最大化」（`WindowControls variant="traffic"`，26px/颗 ≈ 红绿灯组宽），Win/Linux 保持右上 min/max/close 44px；② `--accent-strong` 不再做填充，新增 `--accent-fill`（white/22）统一激活填充（主按钮/开关/档位/勾选/进度条），accent-strong 只留聚焦环与浅灰文字；③ 二维码统一弹层：抽共用 `components/QrModal.tsx`，设置页与 Shares 复用。
> **八轮修订（设置窗抽屉化 + 浮点修）**：玻璃背景/自适应 tint 抽成 `hooks/useWallpaperBackdrop.ts`（主窗/props 窗共用）；props 窗 = 无边框阴影窗贴屏幕右侧（`WebviewWindowBuilder::position` 吃**逻辑坐标** f64，监视器几何是物理像素要除 scale）+ `.props-slide` 滑入 + 根壳圆角白描边；清晰度「跟随全局」浮点 bug：全局 renderDpr 经 f32 往返是 0.85000002…，DPR_TIERS 匹配必须容差 <0.001。
> **九轮（关闭按钮 ACL + 后处理 off）**：**capabilities 的 windows 列表不含 `props-*` 时，props 窗里一切 plugin:window IPC 被 ACL 静默拒绝**，void promise 吞掉 =「按钮无反应」（已改 `["main","props-*"]`）；后处理 off 档恢复（四档 + 滑条 max=3，WebWallGL 原生支持 off=效果链直通）。
>
> **十轮修订（Win/Linux 无边框补完，2026-09-26）**：把「去原生标题/菜单栏」在 Win/Linux 收口到可交付状态 ——
> ① 新增 `src/lib/platform.ts`：`useOs`（navigator 同步猜测 + `app_info` 校准；修掉 TopBar 窗口控制首帧闪一帧 macOS 左侧布局的问题）、`useWindowRounded`（主窗口/props 窗共用的「最大化去圆角」）。
> ② 新增 `src/components/ResizeHandles.tsx`：Win/Linux 页面边缘 5px 边条 + 12px 四角隐形缩放把手，按下走 `startResizeDragging` → tao 的拖拽缩放（capabilities 补 `core:window:allow-start-resize-dragging`）；最大化/全屏收起。GTK 无 CSD 没有原生缩放热区，这圈把手是 **Linux 的唯一缩放入口**；Windows 侧起兜底作用（thickframe 隐形边框是否被 WebView 盖住不赌）。macOS 不挂（tao `drag_resize_window` 是空实现）。
> ③ props 设置窗 Win/Linux 补完：头部挂 `data-tauri-drag-region="deep"`（macOS 保持 movableByWindowBackground 背景拖动，两套并存会打架）；⚠️ 拖拽区自带双击最大化，React 的 `onDoubleClick` 绝不能同挂（两次 toggle 相互抵消，窗口不动）；右侧 × 换自绘 WindowControls（min/max/close）；embedded 头部定高 48px，控制按钮通栏 hover 才像原生标题条。
> ④ props 窗也接 `useWindowRounded` —— 此前只有主窗口处理最大化去圆角，设置窗最大化时四角会透出桌面。
> **已知边界**：Linux 无系统投影（GTK 忽略 shadow），边界靠 1px 白描边交代；Win11 悬停最大化按钮不弹 snap 布局面板（自绘按钮没有这个系统钩子）；「原生菜单栏」本就不存在 —— `app.set_menu` 仅 macOS 分支调用，Win/Linux 没有任何系统菜单。
> 参考视觉：DeepSeek 桌面端设置窗 —— 悬浮圆角面板、深色磨砂玻璃、白色文字。
> 关联文档：[network-service-and-sharing.md](./network-service-and-sharing.md)（分享全链路，本方案只挪入口不动机房）。

---

## 0. 需求 → 方案映射

| # | 需求原文 | 方案落点 |
|---|---------|---------|
| 1 | 移除主题切换，整个页面透明 + 磨砂背景 + 白色字体 | §2 玻璃主题：删主题系统，token 全量改「深色玻璃」单套 |
| 2 | 移除左侧菜单栏，改顶部居中菜单；方形+部分圆角、段间小间隔锯齿、悬浮/选中拉长+磨砂变深、取消选中缩回、动画丝滑 | §3 顶部胶囊导航 PillNav（含动画规格） |
| 3 | 设置中的分享单独提到顶部菜单栏；分享列表加二维码按钮 | §4 新增「分享」页 SharesPage + 每行 QR 按钮（复用 QrImage） |
| 4 | 无边框设计，多平台统一 | §5 全平台无边框壳：Win/Linux 去原生标题栏 + 自绘窗口控制，macOS 保持 Overlay 红绿灯 |

## 1. 现状盘点（实施前必读的事实）

### 1.1 已经具备的底座（不要重造）

- **窗口已经是透明的**：`tauri.conf.json` 主窗口 `transparent: true` + `macOSPrivateApi: true`；macOS 另有 `titleBarStyle: Overlay` + `hiddenTitle`（红绿灯悬浮在内容上，无标题栏）。**Windows / Linux 目前仍带原生标题栏**，这是「多平台不统一」的根源。
- **三平台系统级磨砂已就绪**（`lib.rs` `apply_backdrop` / `blur.rs`）：
  - macOS：`window-vibrancy` NSVisualEffectView（Sidebar 材质，圆角 16）；
  - Windows：Acrylic（深色 tint `(28,28,32,180)`，失败退 blur）；
  - Linux：KDE KWin 合成器模糊（X11 属性 + Wayland `org_kde_kwin_blur`），GNOME 等降级为半透明不模糊。
- **主窗口内存语义**：关闭/最小化 = 销毁窗口并结束 WebContent 进程（`main_window.rs` `register_close_to_release`）；托盘/Dock 唤起走 `ensure_main_window` **按 Rust 侧硬编码配置重建窗口** —— 改窗口配置必须同步改这里（见 §5.4）。
- **二维码链路现成**：`lib/qrcode.ts`（零依赖矩阵生成）+ `QrImage` canvas 组件（高清、静区、pixelated）。分享弹窗、设置页局域网地址都在用。
- **分享数据链路现成**：`api.shareList / shareRemove / shareSetEnabled / shareSetServiceEnabled`、`mcp.lanUrl` 作链接 base、`settings-changed` 事件同步 `share.enabled`。

### 1.2 必须绕开的坑

- **透明 WKWebView 里 `backdrop-filter` 不稳定**（`index.css` `.props-tint` 注释，实测结论）：「刚开是磨砂、一两秒后变纯半透明」。→ **新增的一切玻璃面（导航胶囊、卡片、弹层）一律用 rgba 半透明色 + 系统窗口材质透出，禁止用 backdrop-filter 造磨砂**。页面内的 backdrop-filter 只有在「身后有页面内容」时才允许（如详情抽屉压在网格上），且要有 alpha 兜底。
- **`main_window.rs` 与 `tauri.conf.json` 双份窗口配置**：只改 JSON 不改 Rust，回收后重建的窗口会退回旧样式。
- Linux 无边框窗口的**边缘拖拽缩放依赖 WM**（§5.5 已知边界）。
- CI 三平台编译：窗口配置改动涉及 `#[cfg]` 分支，注意非 macOS 目标的编译错误（历史踩过）。

### 1.3 将被拆除的东西

| 现状 | 位置 |
|-----|-----|
| 主题三态循环按钮（跟随系统/浅色/深色） | `App.tsx` 侧边栏底部 |
| 主题系统（localStorage `we.theme` + `data-theme`） | `src/lib/theme.ts`、`index.css` 浅色/深色双套 token |
| 壁纸设置窗跟随主题 | `props-main.tsx` `useGlobalTheme()` |
| 左侧边栏（分组导航 + 收缩态 78px 对齐红绿灯） | `App.tsx` `<aside>` |
| 侧边栏透明度基建（默认 0.78、调节入口已注释隐藏） | `src/lib/sidebar.ts`、`Settings.tsx` 隐藏滑条 |
| 设置页「网络与服务」里的分享 Group | `Settings.tsx`（服务开关 + 我的分享列表） |

## 2. 玻璃主题（需求 1）

### 2.1 策略：改 token 值，不大改组件

全站组件都吃 `--content / --card / --separator / --text-1 / --text-2 / --accent*` 这组变量。**保留变量名、重定义取值为「深色玻璃」单套**，浅色/深色/跟随系统三分支整体删除。组件层几乎零改动即可换肤，随后再做逐页对比度微调。

### 2.2 新 token 表（`index.css` 重写 `:root`）

```css
:root {
  color-scheme: dark;              /* 强制深色，删掉 prefers-color-scheme 全部分支 */
  /* 玻璃三级：越浮层越实。全部是深色底 + alpha，磨砂由系统窗口材质提供 */
  --content:   rgba(18, 18, 22, 0.60);  /* 页面基底：壁纸透出最明显 */
  --sidebar:   rgba(18, 18, 22, 0.72);  /* 顶栏 / 导航条（沿用变量名，语义=顶栏底） */
  --card:      rgba(28, 28, 34, 0.82);  /* 卡片 / 弹层 / 抽屉 / 输入框 */
  --sidebar-sel: rgba(255, 255, 255, 0.14);  /* 选中填充 */
  --separator: rgba(255, 255, 255, 0.10);
  --text-1: #f5f5f7;
  --text-2: rgba(245, 245, 247, 0.62);
  --accent:        rgba(255, 255, 255, 0.12); /* 填充选中底 */
  --accent-fg:     #ffffff;
  --accent-fill: rgba(255,255,255,0.22);  /* 激活填充：主按钮/开关/选中/进度条（七轮统一切换） */
  --accent-strong: #e8e8ed;              /* 仅聚焦环与浅灰文字，不再做填充 */
  --shadow: 0 2px 8px rgba(0,0,0,.35), 0 12px 40px rgba(0,0,0,.40);
  /* 新增：胶囊导航/交互态 */
  --glass-hover:  rgba(255, 255, 255, 0.08);
  --glass-active: rgba(255, 255, 255, 0.14);
}
```

- `.btn` / `.btn-primary` 自动继承新值：主按钮变「近白底 + 深字」，正是参考图的按钮观感。
- 对比度红线：`--content` alpha 不低于 0.55、`--card` 不低于 0.75，保证浅色壁纸/纯白壁纸上白字可读。**上线前用纯白、纯黑、高噪三张壁纸各过一遍全部页面**。
- 可读性兜底（可选后续项）：把 `sidebar.ts` 的 alpha 基建翻新成「玻璃浓度」全局设置（现成 localStorage + CSS 变量链路，调节入口目前是注释掉的）。本期不做。

### 2.3 主题系统拆除清单

1. `App.tsx`：删 `theme` state、`applyTheme` effect、`cycleTheme`、底部主题按钮及 `IconSun/IconMoon/IconAuto` 引用。
2. `src/lib/theme.ts`：整文件删除。
3. `props-main.tsx`：删 `useGlobalTheme()` 调用（`data-os` 探测**保留**，`.props-tint` 还要用）。
4. `index.css`：删两套主题分支与 `data-theme` 选择器；`.props-tint` 改为引用 `--card`（保留 `data-os` 分支的 alpha 差异逻辑）。
5. `icons.tsx`：删 `IconSun / IconMoon / IconAuto / IconSidebarCollapse / IconSidebarExpand`。
6. `en-US.ts`：删主题相关键，新增键见 §6。
7. 兼容处理：老用户 localStorage 里残留的 `we.theme` 无消费者，无需清理代码。

### 2.4 壁纸设置窗（props-*）

共享 `index.css`，自动变玻璃。验收时单独过一遍：托盘右键 → 壁纸设置，确认输入框/滑条/下拉在玻璃底上可读（该窗口有自己的原生 vibrancy/acrylic）。

## 3. 顶部胶囊导航（需求 2）

### 3.1 新壳布局

```
┌────────────────────────────────────────────────────────────┐
│ TopBar h-11（整条拖拽区）                                     │
│ [mac:78px 红绿灯留白 + logo]   [居中 PillNav]   [分享快捷?][窗口控制] │
├────────────────────────────────────────────────────────────┤
│ 页面内容（Discover/Workshop/.../Shares/Settings）              │
└────────────────────────────────────────────────────────────┘
```

- `App.tsx` 整个 `<aside>` 删除，换 `<TopBar>`；`main` 的 40px 头部拖拽条删除（拖拽职责并入 TopBar）。
- 拖拽规则：TopBar 根节点与空白 spacer 挂 `data-tauri-drag-region`（双击最大化 Tauri 自动处理）；胶囊、按钮不挂。
- 页面恢复逻辑不变（`nav.page` 快照）；`shares` 与 `settings` 同策略、不恢复。
- 分组概念（浏览/库/系统）在顶部导航中取消，8 项平铺：发现 · 工坊 · 下载 · 本地库 · 收藏 · 显示器 · **分享** · 设置。

### 3.2 胶囊规格（「方形 + 部分圆角 + 小间隔锯齿」）

- 容器：高 44px，`rounded-[12px]`（部分圆角，非全胶囊），`bg-[var(--sidebar)]`，内衬 `p-[3px]`，**段间 gap 2px** —— 视觉上就是「一排方齿、齿间留缝」。
- 段（item）三态：

| 态 | 尺寸 | 底色 | 文字 |
|----|------|------|------|
| 默认 | 32×32 方段（只显示图标），`rounded-[8px]` | 透明 | `--text-2` |
| 悬浮 | 横向拉长露出文字标签 | `--glass-hover` | `--text-1` |
| 选中 | 横向拉长 + **高度 32→38px 微隆起** | `--glass-active` + 内描边 `white/10` + `--shadow` | 白，加粗 |

- 悬浮与选中同样拉长（预览文字），选中额外更深底色 + 隆起；鼠标移出悬浮段缩回，**取消选中时原段缩回 32×32 默认态**。

### 3.3 动画规格（丝滑的关键实现）

标签展开用 **grid 0fr→1fr 技巧**（宽度随内容、无需测量文本、不抖动），叠加缓动分层：

```css
.pill-label { display:grid; grid-template-columns:0fr; transition:grid-template-columns .32s cubic-bezier(.22,1,.36,1); }
.pill-item:hover .pill-label, .pill-item[data-active=true] .pill-label { grid-template-columns:1fr; }
.pill-label > span { overflow:hidden; white-space:nowrap; min-width:0; }
.pill-item { transition: background-color .2s ease, color .2s ease, height .32s cubic-bezier(.22,1,.36,1), box-shadow .25s ease; }
```

- 容器用 `flex justify-center`，子段宽度过渡时整条胶囊自然平滑变宽、保持居中。
- 高度隆起 32→38 只在选中态；数值越大越明显，但 >4px 会显晃，定 6px 差值。
- `prefers-reduced-motion: reduce` 时全部过渡关掉（沿用仓库现成约定）。
- 禁用项：不要给宽度/布局属性加 spring 回弹（标签文字会被裁切），弹性感交给高度和阴影。

### 3.4 交互细节

- 键盘可达：`button` + `:focus-visible` 全局环（已有）。
- 详情抽屉打开时导航选中态维持原语义（`page === id && !detailId`）。
- 收缩态无了：`SIDEBAR_STORAGE_KEY`、`readInitialCollapsed`、`toggleCollapsed` 一并删除；macOS 红绿灯避让从「侧栏宽度」改为「TopBar 左侧 78px 固定留白」（沿用现有常量注释）。

## 4. 分享一级页 + 二维码（需求 3）

### 4.1 新页面 `src/pages/Shares.tsx`

从 `Settings.tsx` 迁出（代码搬家，接口不动）：

- 页头：标题 + **分享功能总开关**（`share.enabled`，原设置页同款 Switch，继续监听 `settings-changed` 与托盘/MCP 侧改动同步）。
- 列表行：`标题 · /share/{id} · 过期时间 · 浏览数` + 操作组：
  - **复制链接**（原样迁移，base = `mcp.lanUrl ?? origin`）
  - **二维码**（新增）：弹出轻量弹层，`<QrImage text={landing} size={180} />` + 链接文本 + 复制按钮；弹层复用 `ConfirmModal` 的壳样式
  - 启停 Switch、删除（原样迁移）
- 空态：沿用现文案「在库卡片或详情页点分享即可创建」。
- 依赖提示：分享链接可用要求网络服务开启；`mcp.enabled === false` 时页头下挂一条警示 + 「去设置」跳转（`navigate("settings")`，TopBar 入参透传）。
- 轮询：进页拉一次 `mcpStatus + shareList` 即可（分享列表无实时诉求，不搬 4s 轮询）。

### 4.2 设置页瘦身

- 「网络与服务」标签删掉「分享」Group（网络服务/代理/诊断不动）；`shareOn/shares/refreshShares/...` 状态与方法整段迁走。
- `SETTINGS_TABS` 文案不变。

### 4.3 导航接入

- `NAV` 数组加 `{ id: "shares", label: "分享", icon: <IconShare/> }`（图标现成）。
- `icons.tsx` 新增 `IconQr`（二维码按钮用；若无合适现成图标）。

## 5. 全平台无边框（需求 4）

### 5.1 决策：macOS 保留原生红绿灯，Win/Linux 自绘控制

| 平台 | 标题栏 | 圆角 | 理由 |
|------|--------|------|------|
| macOS | 保持 `Overlay` + `hiddenTitle`（已是无边框观感） | 系统原生 | 自绘红绿灯会丢失原生交互与质感，参考图本身也是原生红绿灯 |
| Windows | `decorations: false`（新增）+ `WindowControls` 自绘最小化/最大化/关闭 | CSS `rounded-[12px]`（透明窗口 + 根容器圆角，Win10/11 通吃） | Windows 原标题栏是「不统一」的唯一来源 |
| Linux | `decorations: false`（新增）+ 同款 `WindowControls` | CSS 同上（合成器支持时） | 同上 |

统一的是「壳」（无边框 + 圆角玻璃面板 + 顶部导航），窗口控制按钮按平台习惯渲染。

### 5.2 根容器（圆角 + 描边）

```tsx
// App 根节点
<div className={`h-full flex flex-col bg-[var(--content)] rounded-[12px] overflow-hidden
                 ring-1 ring-white/10 ${maximized ? "rounded-none" : ""}`}>
```

- 窗口透明，圆角由 CSS 画；最大化/全屏时去圆角（`onResized` 里查 `isMaximized()/isFullscreen()`）。
- 无 OS 阴影（透明窗口固有），用 1px 内描边交代边界；如需投影，再给根容器外垫 padding + box-shadow（备选，先不做）。

### 5.3 `WindowControls`（仅 Win/Linux 挂载）

- 三个按钮：`getCurrentWindow().minimize() / toggleMaximize() / close()`。
- **close 走 `close()` 即可**：会触发既有 `CloseRequested` → `register_close_to_release` 的「关闭即释放内存」语义，不绕过。
- 最大化图标随状态切换（`onResized` → `isMaximized()`）；hover 样式按 Windows 习惯（整格变色，关闭键 hover 红）。

### 5.4 Rust 侧同步（防「回收后重建退回旧样式」）

`main_window.rs` `ensure_main_window` 的 builder 补齐：

```rust
#[cfg(not(target_os = "macos"))]
let builder = builder.decorations(false);
```

macOS 分支保持 `title_bar_style(Overlay) + hidden_title(true)` 不动。`tauri.conf.json` 的 `app.windows[0]` 加 `"decorations": false`（macOS 上该字段与 Overlay 并存无冲突，但为语义清晰可接受）。

### 5.5 平台已知边界（如实告知）

- **Linux 边缘缩放**：GTK 无边框窗口的边缘热区拖拽依赖 WM（KWin/Mutter 可用性不一；备选 Super+拖拽）。若实测不可接受，回退方案是 Linux 单独保留 `decorations: true`（其余平台不变）——实施日先实测再定。
- GNOME 无窗口模糊（既有降级，玻璃 token alpha 已兜底可读性）。
- Windows blur 兜底通道的拖动卡顿是已知上游问题，仅在 Acrylic 失败时出现（维持现状）。

## 6. 实施顺序（四个独立可交付阶段）

| 阶段 | 内容 | 验收 |
|------|------|------|
| P1 无边框壳 | tauri.conf + ensure_main_window + 根容器圆角 + WindowControls + 拖拽区重排 | 三平台启动无原生标题栏；拖动/双击最大化/最小化/关闭（含关闭即释放）/边缘缩放正常；重建窗口样式不回退 |
| P2 玻璃主题 | token 重写 + 主题系统拆除（§2.3 清单）+ 逐页对比度微调 | 无任何主题切换入口；纯白/纯黑/高噪壁纸下全页白字可读；壁纸设置窗、全部弹窗过检 |
| P3 顶部胶囊导航 | TopBar + PillNav + 删侧栏 + 动画打磨 | §3.2 三态与 §3.3 动画达标（60fps、无文字裁切、缩回顺滑）；红绿灯避让正确 |
| P4 分享页 | SharesPage + QR 弹层 + 设置页瘦身 + i18n | 分享创建→列表→二维码/复制/启停/删除全链路；服务关闭时有提示与跳转 |

每阶段：`pnpm typecheck` + `cargo check`（注意 CI 三平台）+ 走本机 `tauri dev` 实例核验；P2/P4 完成后重录 README 截图（GIF 工作流照旧）。

## 7. 文件变更清单

**新增**：`src/components/TopBar.tsx`（含 PillNav）、`src/components/WindowControls.tsx`、`src/pages/Shares.tsx`
**修改**：`src/App.tsx`（壳重写）、`src/index.css`（token 重写）、`src/pages/Settings.tsx`（迁出分享/删侧栏透明度）、`src/props-main.tsx`、`src/components/icons.tsx`（增删图标）、`src/locales/en-US.ts`、`src-tauri/tauri.conf.json`、`src-tauri/src/main_window.rs`
**删除**：`src/lib/theme.ts`、`src/lib/sidebar.ts`
**复用不动**：`src/lib/qrcode.ts`、`src/components/QrImage.tsx`、`ShareModal.tsx`、`src/api/steam.ts` 分享接口、`blur.rs` / `apply_backdrop`

## 8. 风险与回退

1. **backdrop-filter 禁令**（§1.2）：所有新玻璃面只用 rgba；代码评审时按此把关。
2. 对比度不足 → 调 token alpha 而不是加 backdrop-filter；红线值见 §2.2。
3. 虚拟网格滚动性能：玻璃层叠合成成本高于不透明底，P2 验收时压测本地库滚动（VirtualGrid），劣化明显则抬高 `--content` alpha。
4. Linux 缩放不可用 → §5.5 回退方案。
5. `ensure_main_window` 漏同步 → P1 验收项专门包含「回收→托盘唤起」路径。
