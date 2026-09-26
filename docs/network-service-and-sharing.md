# 网络服务与壁纸分享方案（v1 设计稿，未实现）

> 2026-09-25 起草。目标：把现有 MCP 服务升级为一个**通用网络服务**，在同一端口、
> 同一开关下承载三种能力 —— MCP（agent 用）、REST API（脚本/自动化用）、
> **壁纸分享链接**（浏览器直接渲染壁纸），并支持 本机/局域网/任意 三档网络模式。
> 本文只做方案，不含实现代码。

---

## 1. 需求原文 → 方案映射

| 需求 | 方案落点 |
|---|---|
| 同用 MCP 服务开关 | 一个服务进程/端口（7411）承载 MCP + API + 分享；一个总开关 + 分享子开关（§3） |
| 网络模式：本机/局域网/任意 | `service.net_mode` 三档，绑定策略 + 源地址过滤（§4） |
| API 标准接口 | `/api/v1/*` REST + OpenAPI 3.1 + 内置文档页（§5） |
| 壁纸链接分享（永久/限时） | `shares` 表 + 分享弹窗 + 到期机制（§6.1/§6.5） |
| 分享类型：直链 / iframe 代码 | 同一条分享的三种视图：落地页 / 直链 / iframe 片段（§6.2） |
| 浏览器渲染壁纸 | 访客复用现有渲染器页 + **按分享限定的 media 路由**（§6.3） |
| 竖屏/横屏切换小图标 | 分享页悬浮切换钮，aspect-ratio 容器实现（§6.4） |
| 壁纸属性更改动态更新 | 复用 `updateWebProps` 热更路径 + per-share SSE 推送（§6.6） |

## 2. 现状盘点（设计依据，均为已核实事实）

- **MCP 服务**（`src-tauri/src/mcp/mod.rs`，axum）：Streamable HTTP，`POST/DELETE /mcp`，
  token 鉴权（`?token=` 或 `Authorization: Bearer`），端口可配（默认 7411），
  `mcp.enabled`/`mcp.port`/`mcp.token` 存 settings 表，设置改动热重启。
  已有「拒绝非回环 Origin 的浏览器请求」防护（防 DNS rebinding）——开放局域网/任意
  模式后这条必须改写（§4.3）。
- **内容服务器**（`content_server.rs`，手写 HTTP，绑 `127.0.0.1:0` 随机端口）：
  `/renderer/*` 静态页（prod=资源目录 dist/renderer，dev=vite 代理）、`/diag`、
  `/audio-stream/{tok}` SSE、`/media/{tok}/{item}/{path}`、`/web/{tok}/...`、
  `/props/{tok}/{item}`、`/media-command/{tok}/...`、local-assets。
  **只服务本机壁纸窗口**，token 在路径里。**不直接对外开放**。
- **渲染器页**（`renderer/src/main.ts`）：全部配置读 URL query；`mediaBase` 参与字符串
  拼接（`${mediaBase}/${itemId}/...`）→ 相对路径（同源）理论上可用，需 spike 验证（§8）。
- **壁纸属性（props）**：挂载时渲染器从 `/props/{tok}/{item}` 拉取；运行时宿主在
  props 变更处 eval `window.__wp.updateWebProps(props)` 热更（`library.rs`）。
  **「属性动态更新」的推送端和接收端都已存在**，缺的只是「向分享访客转发」一跳。
- 设置页已有「AI / MCP」与「网络」两个标签；仓库内已有二维码库（`src/lib/qrcode.ts`）。

## 3. 服务形态与开关

### 3.1 一个端口，四族路由

7411（沿用 MCP 端口与现有 `mcp.port` 设置）成为唯一的对外端口：

```
/mcp            JSON-RPC（现有，原样保留）
/api/v1/*       REST 控制接口（新，§5）
/share/*        壁纸分享（新，§6，公开访问但权限只到「看」）
/docs           API 文档页（新，离线内嵌 Scalar 单文件）
```

内容服务器**保持回环绑定不动**，继续专供桌面壁纸窗口；分享流量全部走服务服务器的
「按分享限定」路由，**内容 token（content token）永不出现在分享链路里**（§6.3）。

### 3.2 设置键（settings 表）

| 键 | 值 | 默认 | 说明 |
|---|---|---|---|
| `service.enabled` | bool | 迁移自 `mcp.enabled` | **总开关**：关 = 7411 不监听，三族路由全灭 |
| `service.net_mode` | `loopback`/`lan`/`any` | `loopback` | 网络模式，见 §4 |
| `share.enabled` | bool | **false** | 分享子开关：关 = `/share/*` 一律 404（已有分享链接失效） |
| `service.public_base_url` | string | 空 | 「任意」模式下生成分享链接用（§4.4） |
| `mcp.port` / `mcp.token` | — | 沿用 | 不新增键；token = 服务唯一密钥，API 与 MCP 同用 |

语义取舍：**总开关一个**（用户原话「同用mcp服务开关」），MCP 与 API 不再分别开关
（它们是同一服务上的两种编码）；**分享单独一个子开关**——它对外暴露的是「内容」
而非「控制权」，默认关、用才开，风险面不同。

### 3.3 设置 UI（并入现有「网络」标签）

- 服务区：总开关、网络模式（分段控件：本机/局域网/任意）、端口、token（显示/隐藏/
  **一键重生成**）、当前生效地址列表 + **局域网二维码**、防火墙提示条。
- MCP 区（现有调用日志、配置片段）原样保留，token 显示同一处。
- 「我的分享」区：分享列表管理（§6.5）。
- 模式切到局域网/任意时：行内安全警示（不再用弹框，遵守「优雅、不跳框」的既定审美）：
  首次对外监听时 macOS/Windows 防火墙会弹系统授权框，属预期。

### 3.4 托盘

不加。托盘现有快速设置已较满，网络暴露面这类低频高敏感项留在设置页。

## 4. 网络模式

### 4.1 绑定与过滤

| 模式 | 监听 | 访问控制 |
|---|---|---|
| 本机 `loopback` | `127.0.0.1` | 现状（token + 回环 Origin 检查） |
| 局域网 `lan` | `0.0.0.0` | token 照旧 + **中间件按源 IP 过滤**：仅放行私网段（10/8、172.16/12、192.168/16）、链路本地（169.254/16、fe80::/10）、回环、ULA（fc00::/7） |
| 任意 `any` | `0.0.0.0` | 仅 token；设置页红色警示 |

「局域网」选择**应用层源过滤**而非「只绑内网网卡 IP」：多网卡/DHCP 换 IP/虚拟网卡
都会让绑定式方案悄悄失效或启动报错；`0.0.0.0` + 过滤中间件在任意网络环境下语义稳定。
「任意」与「局域网」的差别就在这道过滤上（公网可达与否由路由器/防火墙决定，软件
如实把两种暴露程度的差异讲清楚）。

### 4.2 DNS rebinding 防护的改写

现有逻辑「带浏览器 Origin 且非回环 → 拒绝」只在 `loopback` 模式保留；`lan/any` 模式
改为：Origin 头存在时校验其 host 与请求 Host 一致（防 rebinding 的本质不变，白名单
从「回环」放宽为「本机各地址」）。

### 4.3 token 策略

- 全服务一把密钥（`mcp.token` 沿用），MCP 与 API 同用；支持 `?token=`（MCP 兼容）
  与 `Authorization: Bearer`（API 推荐）。
- 设置页「重生成 token」：生成后所有旧凭据失效（含用户已配置的 MCP 客户端，提示语说明）。
- 从 loopback 切到 lan/any 时不强制重置，但警示条建议重生成。

### 4.4 分享链接的 host 选择

| 模式 | 链接 host |
|---|---|
| 本机 | `127.0.0.1:port`（仅本机可开，UI 说明） |
| 局域网 | 自动取**物理网卡的私网 IPv4**（UI 可改选其它地址）+ 二维码 |
| 任意 | `service.public_base_url`（用户手填域名/公网地址）；未配置时回落内网 IP 并提示「外网访问需自行端口映射」 |

**局域网 IP 探测的实现要点（P1 实机教训）**：不能用「UDP 连 8.8.8.8 读出口 IP」——
挂代理/加速器的机器默认路由走 utun 隧道（fake-ip 网段 198.18.0.0/15），探测到的是
无用的隧道地址。已改为 `if-addrs` 枚举网卡，按接口名过滤虚拟网卡（lo/utun/tun/tap/
bridge/awdl/llw/ap/docker/veth），取物理网卡（en*/eth*/wl* 优先）上的私网 IPv4，
UDP 探测仅作兜底。该判定源同时服务于：设置页局域网地址展示、二维码、Origin 校验
（`is_local_host_name`）、后续 P3 的分享链接 host。

## 5. REST API v1（`/api/v1`）

### 5.1 原则

- **薄封装**：全部端点映射到现有 tauri command / MCP tool 的内部实现，不重写业务逻辑；
  MCP 工具与 REST 是同一命令层的两种编码。
- 鉴权：Bearer token；CORS 显式放开（`Access-Control-Allow-Origin: *`，凭据仍必须），
  方便网页端/快捷指令调用。
- 错误统一 JSON：`{"error": "..."}` + 合适状态码（401 未鉴权 / 404 不存在 /
  410 已过期 / 429 限流）。
- 自描述：`GET /api/v1/openapi.json`（OpenAPI 3.1）+ `GET /docs` 内嵌离线文档页
  （vendor Scalar 单文件，不引 CDN，符合「国内免代理可用」的既有取向）。

### 5.2 端点清单（v1）

```
GET    /api/v1/status                 版本、运行态、当前壁纸、暂停态
GET    /api/v1/displays               显示器列表 + 每屏会话摘要
GET    /api/v1/library?query=&limit=&offset=   本地库（复用 library_list）
GET    /api/v1/library/{itemId}       条目详情（含 props 快照）
POST   /api/v1/apply                  {itemId, displayId?} 应用壁纸
POST   /api/v1/pause | /resume | /next | /prev    播放控制
GET    /api/v1/settings/{key}         读设置（白名单键，见下）
PUT    /api/v1/settings/{key}         写设置（白名单 + notify_setting_changed 同链路）
GET    /api/v1/playlists              轮播列表
POST   /api/v1/playlists/{id}/apply   激活轮播
GET    /api/v1/shares                 我的分享
POST   /api/v1/shares                 {itemId, expiresInSec?, note?} 创建分享
DELETE /api/v1/shares/{shareId}       删除分享
GET    /api/v1/screenshot/{itemId}    实拍截图（JPEG）
GET    /api/v1/openapi.json
```

设置读写**白名单**（fit/renderDpr/sceneFps/reveal/muted 等渲染类），不许任意键写入——
REST 暴露面比 MCP 进程内调用更广，防误用/滥用。

### 5.3 典型用法（写进 /docs）

```bash
curl -H "Authorization: Bearer $TOKEN" http://192.168.1.10:7411/api/v1/status
curl -H "Authorization: Bearer $TOKEN" -X POST .../api/v1/apply -d '{"itemId":"3293156956"}'
```

iOS 快捷指令 / Home Assistant RESTful / Raycast 脚本示例各一条。

## 6. 壁纸分享

### 6.1 数据模型（DB 迁移 v12）

```sql
CREATE TABLE shares (
  id            TEXT PRIMARY KEY,   -- 128bit 随机 → base62（不可枚举，即访客凭据）
  item_id       TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  expires_at    INTEGER,            -- NULL = 永久
  enabled       INTEGER NOT NULL DEFAULT 1,
  note          TEXT,               -- 分享备注（仅本机可见）
  orientation   TEXT DEFAULT 'auto',-- auto|portrait|landscape（访客默认值）
  views         INTEGER NOT NULL DEFAULT 0
);
```

一次性分享链接（访问即失效）不做——「永久/指定时间」已覆盖需求，少一个状态机。

### 6.2 一条分享，三种视图

| 视图 | URL | 用途 |
|---|---|---|
| 落地页 | `/share/{id}` | 给人看：标题/预览图/「打开壁纸」/横竖屏切换小图标/全屏/复制与二维码 |
| 直链 | `/share/{id}/render?orient=landscape` | 裸渲染页，直接开在浏览器标签 |
| iframe 代码 | `<iframe src="{base}/share/{id}/embed?orient=…" allow="fullscreen; autoplay" style="…">` | 嵌入他人网页；`/embed` = `/render` 别名（语义化） |

分享弹窗（库卡片右键/详情页「分享…」进入，遵守 AnchoredMenu/内联的零弹框审美之外，
分享确认这种破坏性低但信息密度高的场景用 Modal 合理）：有效期（永久/1 小时/1 天/
7 天/自定义）+ 直链与 iframe 两个复制块 + 局域网二维码 + 「已创建，去『我的分享』管理」。
**web 类型壁纸**弹窗内提示：分享意味着访客浏览器会直接运行该壁纸的网页代码。

### 6.3 访客渲染管线（安全核心）

```
访客浏览器                         服务服务器(7411)                 本机
──────────                        ──────────────────              ──────
GET /share/{id}            →     查 shares 表（enabled/expiry）→ 落地页 HTML
GET /renderer/*            →     静态资源（dev 代理 vite / prod 同 dist）
GET /share/{id}/media/*    →     校验 share→item 映射后，只读服务
                                 该 item 目录内文件（Range/类型白名单/防穿越）
GET /share/{id}/props      →     该 item 当前 props 快照（复用 /props 逻辑）
GET /share/{id}/events     →     SSE（属性推送，§6.6）
```

- 渲染页 query 与桌面完全同构：`type/src/fit/reveal/...` + **`mediaBase=/share/{id}`**
  （相对路径，同源）。**已由 spike 实证可行**（见 §6.7）。
- **官方素材（local-assets）必须跟随分享**：spike 实测同一张 scene 不带
  `localAssets=1` 时渐变/光效回落程序化复刻，观感差异显著（色块化）；带参后与桌面
  完全一致。落地做法：分享路由族内提供 `/share/{id}/api/local-assets` 代理（限只读），
  **是否下发跟随现有「官方素材」全局设置**（`wallpaper_local_assets`）。注意：这会把
  WE 官方素材文件提供给访客 —— 与 WE 官方移动端串流行为一致，但属于既定取舍，写进
  分享弹窗提示。
- **不下发** `audioToken`（系统音频是本机捕获，访客无此数据）；视频类壁纸带原生音轨，
  页面按浏览器自动播放策略**先静音起播 + 悬浮解除静音钮**；iframe 代码带
  `allow="autoplay; fullscreen"`。
- `diag`/`media-command`/`audio-stream` 等其余内容服务器路由**不出现在分享路径**；
  渲染器的 `reportDiag` 走相对 `/diag`，分享路由族内提供一个只写日志的匿名 `/share/{id}/diag`（防 404 报错噪音，可后置）。
- 权限语义：shareId 即凭据，**只读、只到这一张壁纸的文件**；控制 API 与分享路由
  物理隔离（不同前缀、不同中间件），不存在从 share 页面触达控制接口的路径。

### 6.4 竖屏 / 横屏切换

- 落地页与直链页右上角**悬浮小图标**（竖屏 9:16 ↔ 横屏 16:9 ↔ 跟随窗口，三态循环）。
- 实现：渲染区包在 `aspect-ratio` 容器里 contain 居中，容器外留黑——壁纸内容自身
  按 cover 填容器，等于「预览手机/桌面两种画幅」，不裁剪不变形。
- 记忆：`localStorage`（同源偏好）+ URL 参数 `?orient=` 覆盖；访客设备竖屏时默认竖屏。
- 该偏好属**访客本地**，不回写 `shares.orientation`（后者只是分享者设置的默认值）。

### 6.5 到期、停用与管理

- 校验：**懒校验为主**（每次 `/share/{id}/*` 访问查表：过期→410、停用→403、不存在→404），
  每 10 分钟清扫任务删除过期行（顺带写日志）。
- 「我的分享」（设置页「网络」标签内）：缩略图、链接（复制）、类型图标、到期时间、
  浏览计数、启停 toggle、删除。分享列表变化通过现有 `settings-changed` 类广播通知
  打开中的设置页。
- `views` 在落地页与 render 命中时 +1（同一 IP 5 分钟去重，防刷）。

### 6.6 属性更改动态更新（复用既有热更链路）

```
本机 props 修改（属性面板/props 窗口）
  → library.rs 现有热更点：eval __wp.updateWebProps(props) 到桌面壁纸窗口
  → 【新增一跳】同 props JSON 发布到「该 item 的全部 share 订阅」
      /share/{id}/events SSE（复用 content server 的 SSE 写法，每 share 一个频道）
  → 访客页 EventSource 收到 → 调渲染器已暴露的 __wp.updateWebProps
```

接收端 API 已存在、桌面端已在用，新增的只有「发布一跳」和访客页订阅十几行逻辑。
一期只推 props；fit/reveal 等全局设置变化不推（属渲染偏好，访客页有自己的展示框）。

### 6.7 Spike 实测结论（2026-09-25，已验证 ✅）

借内容服务器 origin（同源渲染页 + 媒体）模拟分享链路，真实浏览器（WebKit）验证：

1. **相对 `mediaBase` 同源渲染：通过**。`/renderer/index.html?type=scene&src=<id>&
   mediaBase=/media/<tok>` 直开，scene.pkg 下载→浏览器内解包→WebGL 渲染全链路成功，
   diag 回流 `ready`，库日志显示 14 张贴图全载入、16 层、粒子系统在跑。库内部对
   `mediaBase` 无绝对 URL 假设的担心排除。
2. **官方素材影响观感（新发现）**：同一张 scene 不带 `localAssets=1` 渐变/光效回落
   程序化复刻（色块化明显）；带参后与桌面端**完全一致**（辉光/色彩逐像素对上）。
   → 分享路由族需包含 local-assets 代理（§6.3，跟随 `wallpaper_local_assets` 设置）。
3. **跨源 iframe 内嵌：通过**。内容服务器无 `X-Frame-Options`/`frame-ancestors` 头
   （CORS 已是 `*`），外部站点 iframe（allow="autoplay; fullscreen"）内完整渲染，
   WebGL/贴图/动画正常。
4. **首载时长**：14MB scene 冷加载（下载+解包+建层）≈ 14s 出首帧 —— 落地页先显
   预览图 + 加载进度的设计（§8.2）有必要。

## 7. 里程碑与依赖

| 阶段 | 内容 | 依赖 |
|---|---|---|
| **P1 服务底座（✅ 2026-09-25 已实施）** | `service.enabled` 总开关（旧 `mcp.enabled` 迁移源）/ `service.net_mode` 三档（loopback 绑 127.0.0.1；lan/any 绑 0.0.0.0）/ 源过滤中间件（`peer_allowed_in_lan`，整棵路由树生效）/ Origin 策略分级（loopback 保持旧行为；lan/any 只认本机地址形态 Origin）/ 设置页「网络与服务」标签（原 AI/MCP 标签并入，网络模式 + 局域网地址 + 二维码）/ `if-addrs` 物理网卡 IP 探测 | 已完成，实机验证通过 |
| **P2 REST API（✅ 2026-09-25 已实施）** | `/api/v1` 端点族（status/displays/library/apply/stop·pause·resume·next·prev/playlists/settings 白名单读写/screenshot/shares）+ `/api/v1/openapi.json` + `/docs`（离线手写 HTML 页，未内嵌 Scalar——描述以 openapi.json 为准，人工页求可读）。全部端点薄封装 `mcp::tools::call`，鉴权复用 `authorize` | 已完成，实机验证通过 |
| **P3 分享基础（✅ 2026-09-25 已实施）** | shares 表（v12 迁移）+ 分享路由族（落地页 / 直链 302→渲染页 / 限目录 ServeDir media / local-assets 代理跟随设置 / `props/{shareId}/{item}` 令牌路由）+ 分享子开关 `share.enabled`（默认关）+ ShareModal（库卡片/详情页入口）+ 设置页「我的分享」管理 | 已完成，实机验证通过 |
| **P4 分享增强（✅ 2026-09-26 已实施）** | 横竖屏三态切换（竖屏 = 内容旋转 90° 跟随画幅，移动端可用）/ 可拖动边缘吸附小宠物 HUD（点击呼出 画幅/全屏/静音/重载）/ props SSE 实时推送（改属性访客秒跟随）/ 浏览计数同 IP 5 分钟去重 / 加载层（音符条 + 真进度）/ 分享域 /diag + SSE stub | P3，全部完成 |

每阶段独立可交付：P1 后即有「局域网用 MCP」这一实际收益；P3 后分享即可用（无横竖屏/推送也能看）。

## 8. 风险与开放问题

1. ~~**相对 mediaBase 兼容性**~~（已消除，见 §6.7 spike 实测 1）：渲染器/库对相对
   路径与同源拉取均正常；剩余实现期注意点仅一个——分享路由的文件服务要把
   `/share/{id}` 前缀映射到该 item 目录并防穿越（本来就在 P3 范围内）。
2. **scene.pkg 体积**：几十 MB 级 pkg 首次加载慢 → 落地页先展示预览图 + 加载进度，
   文档注明「首次打开较慢」。
3. **访客侧性能**：渲染发生在访客 GPU（渲染器本就跑在浏览器里，桌面端同构），服务器
   只是静态文件——无服务端渲染负担，但低端手机开重 scene 会卡，落地页给「轻量预览」
   提示即可。
4. **任意模式的滥用面**：分享链接一旦外泄即公网可访问（这是「分享」的本意），但
   `/api`+token 也同时暴露公网 → 文档强调任意模式建议置于反代/防火墙之后；后续可加
   「分享链接访问速率上限」设置。
5. **web 壁纸的代码执行**：访客浏览器运行壁纸 HTML/JS，与 WE 官方分享行为一致，
   但需在分享弹窗显著提示（尤其自下载的第三方壁纸）。
6. **iframe 全屏/自动播放**：Safari 对 `allow=` 属性与 WebGL 上下文数有限制，P4 真机
   过一遍 Chrome/Safari/Firefox 与移动端。
7. **防火墙首绑弹框**（macOS「是否允许接受传入连接」/Windows 防火墙授权）写进设置页
   提示，避免被当成病毒行为。
8. **端口冲突**：`mcp.port` 现支持自定义；lan/any 模式被占端口时报错需落到设置页
   `last_error`（现有机制）。

## 9. 明确不做（v1 边界）

- 不做分享页的评论/点赞等社交功能。
- 不做工作坊条目转发分享（只分享本地库条目；未下载的工坊条目引导先下载）。
- 不做访客端任何控制能力（暂停/换页/音量是访客本地 UI，不触达宿主）。
- 不做 HTTPS（局域网明文可接受；公网建议用户自备反代 + base URL 填 https 地址）。
- 不做多用户/权限体系（单机软件，一把 token + 不可枚举 shareId 已够）。
