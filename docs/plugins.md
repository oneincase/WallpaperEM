# 插件系统（内置 / 第三方）

顶栏胶囊里「收藏」之后的那一格。页面分两栏：**已安装**与**插件市场**；插件分两类，
**每个插件点开都是一扇新窗口**：

| 类别 | 谁实现 | 点开的窗口 |
| --- | --- | --- |
| 内置插件 | 本应用自己（Rust `src-tauri/src/plugin/*`） | 应用内新窗口（`WebviewWindow`，label = `plugin-<id>`） |
| 第三方插件 | **声明式清单**（本地一份 JSON），本应用只解释它 | 系统浏览器新窗口；清单写 `open: "window"` 时开应用内窗口 |

外链默认**不**装进应用窗口：那些页面没有我们的窗口控制与导航条，塞进无边框的应用窗口
等于给用户一个既退不回、也刷新不了的死页面。内置插件不一样 —— 它的地址与生命周期都在
我们手里，放进应用窗口才谈得上「一体化」。

## 第三方插件与热插拔

`src-tauri/src/plugin/store.rs`。第三方插件是**一份声明式清单，不是可执行代码**：

```text
<appData>/plugins/<id>/wem-plugin.json          # macOS: ~/Library/Application Support/io.github.oneincase.wallpaperem/plugins/
```

```json
{
  "schemaVersion": 1,
  "id": "wem-shadertoy",
  "name": "Shadertoy",
  "summary": "一句话说明",
  "category": "创作工具",
  "icon": "✨",
  "author": "someone",
  "version": "1.0.0",
  "homepage": "https://github.com/someone/wem-shadertoy",
  "minAppVersion": "2.0.0",
  "capabilities": ["open-url"],
  "entry": { "type": "url", "url": "https://www.shadertoy.com/", "open": "external" }
}
```

**完整的接入协议（字段规范、能力矩阵、规则 R1–R10、错误码、发布清单、演进政策）在
[`docs/plugin-protocol.md`](plugin-protocol.md)，机器可读约束在
[`docs/wem-plugin.schema.json`](wem-plugin.schema.json)。**这里只说宿主侧行为。

声明式清单让「热插拔」退化成纯粹的**增删一个目录**：

- 市场里点「安装」→ 写一份清单 → 已安装栏立刻出现；
- 点「卸载」→ 删掉目录 → 立刻消失；
- 直接把一个插件文件夹拷进 plugins 目录 → 点「重新扫描」→ 立刻出现；
- 全程**不需要重启应用**，也不给第三方代码任何权限。

宿主只解释清单：不做代码执行、界面注入、IPC、文件读写、后台常驻。校验按协议来 ——
只认 `http(s)` 入口、能力必须在白名单内、`schemaVersion` 比宿主新就拒绝、同一个 id 被
别的来源占用就拒绝覆盖；非法/半截清单目录跳过并记日志，用户手放错一个文件不该让整页报错。

**特权能力 `dsh-profile`**：入口类型写 `"type": "dsh"` + `packages: [...]`，需要本机已有 dsh
命令行 + pnpm（`dsh plugin … add` 靠 pnpm 装包；**缺 pnpm 时插件页可一键安装** ——
`npm install -g pnpm`，装完自动接着把插件装进 profile）。打开时宿主会
`dsh plugin --profile wallpallperem add <包…>` 把包装进本应用维护的干净 profile，装完重启
harness 再开窗（干净 profile 没开 HMR，不重启不生效）。这是**唯一会引入第三方代码**的能力
（代码跑在 harness 进程里），因此：卡片带特权标记、首次打开弹确认、包名只放行 npm 注册表
规格；已经装过的包会跳过，第二次打开不再跑 pnpm。

## 插件市场

市场以 **GitHub 话题**为唯一来源：`topic:wem-plugin` 的仓库搜索（`plugin_market_search`），
安装即拉仓库根的 `wem-plugin.json` 落地。已安装的插件会并入同一份列表（标「已安装」），
所以装过的不需要回话题里找。

| 来源 | 从哪来 | 安装做什么 |
| --- | --- | --- |
| GitHub | `topic:wem-plugin` 的仓库搜索（`plugin_market_search`） | 拉仓库根的 `wem-plugin.json` 落地 |

- **发布一个第三方插件** = 仓库打上 `wem-plugin` 话题 + 根目录放一份 `wem-plugin.json`。
  市场条目里的 `manifestUrl` 用 `raw.githubusercontent.com/<owner>/<repo>/HEAD/wem-plugin.json`
  （用 `HEAD` 省一次 API 调用，也不需要知道默认分支是 main 还是 master）。
- 搜索与排序：关键词进 GitHub 的 `q`（`topic:wem-plugin <关键词>`），排序映射到
  `sort=stars|updated`；「名称」排序 GitHub 不支持，在合并后的列表里本地排。
- 限流：匿名搜索 10 次/分钟 → 前端 500ms 防抖 + Rust 侧 90s 结果缓存；撞限流/离线时市场
  **只提示不报错**（失败载荷带 `error` 字段，不是 reject），已安装的插件照常可打开。
- 手贴一个清单地址也能装（市场页的输入框），方便自测未发布的话题。

## 第一个内置插件：DeepSeek Harness（dsh）

`src-tauri/src/plugin/dsh.rs`。打开它做四件事，全部幂等：

1. **扫描**（`dsh_scan`）：PATH → 常见 npm 全局目录（`~/.npm-global/bin`、`/opt/homebrew/bin`、
   `/usr/local/bin`、volta/bun/pnpm 的 bin…）找 `dsh` CLI；顺手找 node（GUI 进程的 PATH
   通常只有 `/usr/bin:/bin`，这是最容易踩的一脚）、桌面版安装位置、`$DSH_HOME`。
   **没装**就把引导摊开（装 Node → `npm install -g @deepseek-ai/dsh` → 重新检测），
   引导里带官网下载页；**装了**就复用，不再问。
2. **准备干净 profile**：`$DSH_HOME/profiles/wallpallperem`。文件写法与 dsh 自己的
   `initProfile()` 逐字一致（`package.json` 只列 `@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-web-app`、
   `pnpm-workspace.yaml`、`cordis.patch.yml`），**已存在的一律不动**。不复用用户的 `web`
   profile 是因为那份里是用户装的插件与模型配置，塞进来既污染它，也让「打开插件」的结果
   随用户改动漂移。
3. **嵌入本应用 MCP**：把 `http://127.0.0.1:<port>/mcp?token=<token>` 写成 profile 的
   patch 层条目（`@deepseek-ai/dsh-mcp-client`，`serverName: wallpaperem`）。MCP 服务没开
   就顺手开（用户不必先去设置页点一下）。于是 dsh 里的模型多出 `mcp__wallpaperem__*`
   一整套壁纸工具。
4. **启动并开窗**（`dsh_open`）：`dsh --profile wallpallperem --no-open --port <空闲端口>`，
   从它打印的 `dsh web: <url>` 行拿到**带进程令牌**的地址，再开一个应用窗口加载它。
   `--no-open` 是必须的（否则系统浏览器会再开一份），自己挑端口是为了不被默认端口的占用
   卡住。

### 托管但不重置

`cordis.patch.yml` 只有在「缺失 / 还是 dsh 的初始模板 / 带 `# managed-by: wallpaperem-plugin`
标记」时才由我们重写。用户一旦手改过它，MCP 层改写到同目录的
`wallpaperem-mcp.patch.yml`，启动时用 `--patch` 挂载 —— **绝不覆盖用户内容**。

### 进程回收

dsh 子进程句柄存在 `DshHost` 状态里，三个口子都会收：插件窗口销毁（`on_window_event`）、
托盘退出/应用退出（`RunEvent::Exit`）、以及界面上的「停止后台进程」。不收的话每开一次
插件就在用户桌面上留一个看不见的 node 进程。

### 命令与文件

| 命令 | 作用 |
| --- | --- |
| `dsh_scan` | 环境 + profile + MCP 状态（页面据此决定「打开」还是「引导安装」） |
| `dsh_open` | 准备 profile → 注入 MCP → 启动 → 开窗；已在跑就只聚焦 |
| `dsh_stop` | 停掉后台 dsh 进程 |
| `plugin_installed` | 扫本地插件目录（热插拔的「当前状态」） |
| `plugin_install_url` | 从 `wem-plugin.json` 地址安装（GitHub raw / 任意 http(s)） |
| `plugin_uninstall` | 删掉插件目录 |
| `plugin_market_search` | GitHub `topic:wem-plugin` 搜索（失败回带 `error` 的载荷，不抛错） |
| `plugin_open_window` | 清单要求应用内窗口时开窗 |
| `plugin_open_dir` | 打开插件目录（手放文件夹的口子） |

| 文件 | 位置 |
| --- | --- |
| profile | `$DSH_HOME/profiles/wallpallperem`（默认 `~/.dsh/...`） |
| MCP 层 | profile 内 `cordis.patch.yml`，或用户自定义后的 `wallpaperem-mcp.patch.yml` |
| 第三方插件 | `<appData>/plugins/<id>/wem-plugin.json` |

## 加一个插件

- **内置插件**：在 `src-tauri/src/plugin/` 下加模块（照 `dsh.rs` 的 `scan/open/stop` 三段），
  在 `lib.rs` 注册命令，再在 `src/lib/plugins.ts` 的 `BUILTIN_PLUGINS` 里加一条
  `open: "dsh"` 的条目 + 在 `src/pages/Plugins.tsx` 里给它一张卡片。
- **第三方插件（社区发布）**：用 [`docs/plugin-template/`](plugin-template/README.md)
  起步（模板含零依赖自查脚本 + GitHub Actions 工作流），仓库打上 `wem-plugin` 话题，
  根目录放一份 `wem-plugin.json`（字段、能力、规则与自查表见
  [`docs/plugin-protocol.md`](plugin-protocol.md)）。市场搜到之后「安装」即落地到本机。

新增文案都要进 `src/locales/en-US.ts`（`node scripts/i18n-audit.mjs` 会盯着）；分类词表
在 `src/lib/plugins.ts` 与 `src-tauri/src/plugin/store.rs` **两处同源**，加分类要一起改；
能力白名单只有 `store.rs` 一处。
