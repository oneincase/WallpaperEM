# 第三方插件接入协议 / Plugin Protocol v1

> 适用：WallpaperEM 2.0.0+（`schemaVersion: 1`）。协议 v1.1 起支持 `dsh` 入口（特权能力）。
> 机器可读的清单约束：[`docs/wem-plugin.schema.json`](wem-plugin.schema.json)。
> 实现：`src-tauri/src/plugin/store.rs`（校验与规则）、`src/pages/Plugins.tsx`（呈现）。

## 0. 一句话

第三方插件 = **一份 `wem-plugin.json` 声明** + 一个 `http(s)` 入口地址。
宿主只解释这份声明，**从不执行第三方代码**。

## 1. 信任模型（为什么是声明式）

从网上拉一个包进本机执行，需要一整套沙箱、权限、签名与撤销机制，而这个应用的插件
能力（在浏览器窗口里打开一个地址）根本用不到那些。所以协议刻意停在**数据**这一层：

- 宿主不做：加载插件 JS、注入应用界面、读写文件、访问 IPC、后台常驻、联网代理；
- 宿主只做：按清单校验 → 落地一个目录 → 在窗口里打开一个 URL。

代价是能力弱，换来的是：**装一个第三方插件不需要信任作者**，卸载也不需要清理任何
残留（删掉目录就是全部）。

## 2. 接入流程

```text
作者：仓库打 topic:wem-plugin + 根目录放 wem-plugin.json
   ↓
市场：GitHub 搜索 topic:wem-plugin → 列表条目（含 manifestUrl）
   ↓  用户点「安装」
宿主：GET manifestUrl → 校验（协议/能力/地址/id）→ 写 <appData>/plugins/<id>/wem-plugin.json
   ↓  用户点「打开」
宿主：按 entry.open 在系统浏览器或应用内新窗口打开 entry.url
```

- **清单地址约定**：`https://raw.githubusercontent.com/<owner>/<repo>/HEAD/wem-plugin.json`
  （`HEAD` 省一次 API 调用，也不需要知道默认分支名）。市场条目自动按此拼装。
- **手动安装**：市场上贴任意 `wem-plugin.json` 的 http(s) 地址即可（含自测未发布的话题）。
- **热插拔**：把一个插件目录拷进 `<appData>/plugins/`，点「重新扫描」即生效 ——
  装、卸、改都不需要重启应用。
- **`dsh` 型（特权）**：清单不指向某个网址，而是声明要装进 DeepSeek Harness 的包。
  用户点「打开」时：确认 → `dsh plugin --profile wallpallperem add <包…>` → 重启 harness
  → 打开 dsh 窗口。

## 3. 清单规范

最小可用清单（只有 `name` 与入口是必填）：

```json
{
  "name": "Shadertoy",
  "entry": { "type": "url", "url": "https://www.shadertoy.com/", "open": "external" }
}
```

`dsh` 型（往 DeepSeek Harness 装包）：

```json
{
  "name": "DSH 壁纸引擎插件",
  "category": "AI 助手",
  "capabilities": ["dsh-profile"],
  "entry": { "type": "dsh", "packages": ["dsh-plugin-wallpaper-engine"] }
}
```

完整清单：

```json
{
  "schemaVersion": 1,
  "id": "wem-shadertoy",
  "name": "Shadertoy",
  "summary": "全屏着色器特效，可直接改成本应用的场景壁纸",
  "description": "挑一个效果，在本应用里新建场景工程，把 shader 搬进 .frag 就是一张实时壁纸。",
  "category": "创作工具",
  "icon": "✨",
  "author": "your-github-name",
  "version": "1.0.0",
  "homepage": "https://github.com/you/wem-shadertoy",
  "minAppVersion": "2.0.0",
  "capabilities": ["open-url"],
  "entry": { "type": "url", "url": "https://www.shadertoy.com/", "open": "external" }
}
```

| 字段 | 类型 | 必填 | 缺省 | 说明 |
| --- | --- | --- | --- | --- |
| `schemaVersion` | int | 否 | `1` | 协议版本。**大于宿主支持的版本直接拒绝**，不按老规则猜 |
| `id` | string | 否 | 目录名/仓库名 | 决定 `<appData>/plugins/<id>/`；会被规整成小写安全目录名 |
| `name` | string | **是** | — | 显示名，≤ 60 字符 |
| `summary` | string | 否 | `""` | 卡片一句话 |
| `description` | string | 否 | `""` | 展开说明 |
| `category` | enum | 否 | `其他` | `AI 助手` / `壁纸资源` / `创作工具` / `其他` |
| `icon` | string | 否 | `🧩` | 单个 emoji |
| `author` | string | 否 | `""` | 作者/组织 |
| `version` | string | 否 | `""` | 插件自身版本（仅展示） |
| `homepage` | uri | 否 | `""` | 主页/仓库页；也是 id 冲突判定的「来源指纹」 |
| `minAppVersion` | string | 否 | `""` | 最低应用版本；不满足 → **可装不可开** |
| `capabilities` | string[] | 否 | 由入口推导 | 见 §4；白名单之外一律拒绝 |
| `entry.type` | enum | 否 | `url` | `url` = 打开地址；`dsh` = 往 dsh profile 装包 |
| `entry.url` | string | `url` 型必填 | — | 只认 `http(s)://` |
| `entry.open` | enum | 否 | `external` | 仅 `url` 型：`external`（系统浏览器） / `window`（应用内窗口） |
| `entry.packages` | string[] | `dsh` 型必填 | — | 1–8 个 npm 包名（可带 `@版本/标签`），白名单见 R11 |
| `url` / `open` | — | 否 | — | 容错写法：等价于 `entry.url` / `entry.open`（两者二选一） |

未知字段被**忽略**（向前兼容）；未知的 `category` 归入「其他」；`capabilities` 里的未知
取值则**拒绝** —— 分类只是归类，能力是宿主给出的承诺，不能装作支持。

## 4. 能力矩阵（支持能力）

| 能力 | 含义 | 状态 | 触发条件 |
| --- | --- | --- | --- |
| `open-url` | 在**系统浏览器**新窗口打开 `entry.url` | ✅ 已实现 | `entry.open = "external"`（缺省） |
| `open-window` | 在**应用内**新窗口打开 `entry.url` | ✅ 已实现 | `entry.open = "window"`；同一插件只允许一个窗口（label = `plugin-<id>`），重复点击聚焦已有窗口 |
| `dsh-profile` | 把 `entry.packages` 装进本应用维护的 DeepSeek Harness profile，并打开 dsh 界面 | ✅ 已实现（**特权**） | `entry.type = "dsh"`；首次打开前弹确认（R12）；需要本机有 `dsh` 命令行与 pnpm（缺 pnpm 时应用会引导一键 `npm install -g pnpm`，装完自动继续） |
| （规划）`open-local-file` | 打开本机文件/目录 | ⛔ 未实现 | 声明它会被 `plugin-capability` 拒绝 |
| （规划）`background` | 常驻后台任务 | ⛔ 未实现 | 同上 |
| （不做）`run-code` / `inject-ui` / `ipc` / `filesystem` | 在本应用进程里执行代码、注入界面、访问 IPC、读写文件 | ❌ 不做 | 见 §1 信任模型 |

> `dsh-profile` 是**唯一**会引入第三方代码的能力，而且那些代码跑在 **DeepSeek Harness
> 的进程里**，不是本应用进程里。它因此被单独标记为特权：声明它的插件在应用里带
> 「特权：会运行第三方代码」标记，首次打开要用户确认，`packages` 也只放行 npm 注册表包名。

规则：**清单声明什么能力，宿主就只给什么**。声明了未实现的能力会被拒绝安装，而不是
装上一个「看起来能用、点了没反应」的插件。

## 5. 规则（R1–R10）

| # | 规则 | 违反时 |
| --- | --- | --- |
| **R1** | 入口只认 `http(s)://`。`file://`、`javascript:`、`data:` 一律拒绝 | `plugin-url` |
| **R2** | 清单里不得出现可执行入口：`entry.type` 只能是 `url` | `plugin-entry-type` |
| **R3** | `id` 规整后必须是非空安全目录名；`dsh` 等内置 id 保留 | `plugin-id` / `plugin-id-reserved` |
| **R4** | 同一个 `id` 被**另一个来源**（主页指纹不同）占用时，拒绝覆盖 | `plugin-id-conflict` |
| **R5** | `minAppVersion` 不满足：**可安装、不可打开**（卡片标「需要 App ≥ x」） | `plugin-app-version` |
| **R6** | `schemaVersion` 大于宿主支持的版本：拒绝安装 | `plugin-schema` |
| **R7** | `capabilities` 必须在白名单内，且覆盖 `entry.open` 隐含的能力 | `plugin-capability` |
| **R8** | 清单体积 ≤ 128 KB（元数据不该是个包） | `plugin-too-large` |
| **R9** | 卸载 = 删除插件目录，只动带清单文件的目录 | `plugin-not-installed` |
| **R10** | 非法/半截清单目录**跳过并记日志**，不影响其它插件与整页可用性 | 静默跳过 |
| **R11** | `dsh` 型的 `packages` 只放行 npm 注册表包名（`pkg` / `@scope/pkg` / `pkg@^1.2.0`），1–8 个；`-` 开头（参数注入）、`file:`/`link:`/`git+`/`../x`（任意来源代码）一律拒绝 | `plugin-packages` |
| **R12** | 特权能力（`dsh-profile`）**首次打开前必须用户确认**；卡片上带特权标记；装包失败时不得留下半装状态 | 界面弹 `ConfirmModal` |

补充语义：

- **`dsh` 型的装包语义**：`dsh plugin --profile wallpallperem add <包…>`；dsh 的
  plugin-manager 会在装完后把声明了 `dsh.bundle` 的包追加进 profile 的组合包清单。
  由于干净 profile 没开 HMR，**装完会重启 harness**（先收掉在跑的实例与它的窗口）
  再打开，保证新插件真的生效。
- **幂等**：`packages` 里已经装过的包会被跳过（读 profile 的 `dependencies` 判断），
  所以第二次打开同一插件不会再跑一遍 pnpm。
- **失败不外溢**：市场搜索失败（离线/限流）只在市场顶部提示；已安装的插件照常可打开。
- **无回滚包袱**：安装失败不会留下半个插件（先校验后落盘）。
- **可重复安装**：同来源重复安装 = 原地更新，不产生第二份目录。

## 6. 错误码

宿主返回 `{ code, detail }`；界面把 `code` 翻成本地化文案。作者按这张表自查：

| code | 触发 | 怎么改 |
| --- | --- | --- |
| `plugin-manifest` | 不是合法 JSON / 缺 `name` / 没有入口 | 补 `name` 与 `entry.url`（或顶层 `url`） |
| `plugin-name` | 名字为空或 > 60 字符 | 缩短名字 |
| `plugin-url` | 入口不是 `http(s)` | 换成网页地址 |
| `plugin-entry-type` | `entry.type` 不是 `url` | 删掉该字段或写 `url` |
| `plugin-open` | `entry.open` 取值非法 | 只能是 `external` / `window` |
| `plugin-id` | `id` 规整后为空 | 用字母数字的 id |
| `plugin-id-reserved` | 用了内置插件的 id（如 `dsh`） | 换个 id |
| `plugin-id-conflict` | 同 id 已被另一个主页的插件占用 | 换 id，或把主页指回原仓库 |
| `plugin-schema` | `schemaVersion` 比宿主新（或为 0） | 降到宿主支持的版本，或让用户升级应用 |
| `plugin-capability` | 声明了不支持的能力，或缺了 `entry.open` 隐含的能力 | 删掉该能力或补上对应能力 |
| `plugin-app-version` | `minAppVersion` 格式错 | 写三段数字，如 `2.1.0` |
| `plugin-packages` | `dsh` 型的 `packages` 为空/超 8 个/含非法规格 | 只写 npm 包名（可带 `@版本`），别写路径与 git 源 |
| `plugin-too-large` | 清单 > 128 KB | 描述精简，别塞 base64 |
| `plugin-parse` | 远端返回的不是 JSON | 确认 `wem-plugin.json` 放在了仓库根、且是合法 JSON |
| `plugin-http-status` | 拉清单时远端非 2xx（GitHub 上常见是 404） | 文件名/分支确认：`<repo>/HEAD/wem-plugin.json` |
| `plugin-network` | 网络/代理失败 | 提示用户检查网络或代理 |
| `plugin-rate-limited` | GitHub 搜索限流（匿名 10 次/分钟） | 等一会儿；宿主已加防抖与 90s 缓存 |

宿主侧/环境类错误（作者不用管，界面同样有本地化文案）：

| code | 触发 |
| --- | --- |
| `plugin-fetch` / `plugin-network` / `plugin-http` | 拉清单时网络失败（含代理不可用） |
| `plugin-write` / `plugin-remove` / `plugin-dir` | 插件目录不可写 / 删不掉 / 取不到 |
| `plugin-not-installed` | 卸载一个并不存在的插件（或目录里没有清单文件） |
| `plugin-pnpm` | 往 dsh profile 装包失败（pnpm 报错/超时 5 分钟） | 看返回的 detail（pnpm 的输出尾巴） |
| `plugin-pnpm-missing` | 本机没有 pnpm（`dsh plugin` 需要它） | 装 pnpm 并确保在 PATH 上 |
| `plugin-open-dir` / `plugin-window` | 打开插件目录 / 插件窗口失败 |
| `dsh-*` / `mcp-*` / `profile-write` | 内置插件 DeepSeek Harness 的环境类错误（见 `docs/plugins.md`） |

## 7. 发布一个第三方插件（作者清单）

0. 用 [`docs/plugin-template/`](plugin-template/README.md) 起步：模板里有一份零依赖的
   自查脚本（`node scripts/validate-wem-plugin.mjs`）与一条 GitHub Actions 工作流
   （`.github/workflows/validate-wem-plugin.yml`），CI 里就能挡住不合规的清单；
   脚本与宿主共用同一份用例 [`protocol-cases.json`](plugin-template/protocol-cases.json)，
   所以「本地过了、装的时候被拒」不会发生。
1. 建仓库，根目录放 `wem-plugin.json`（可用 [`wem-plugin.schema.json`](wem-plugin.schema.json) 在编辑器里校验）；
2. 仓库加上 **`wem-plugin`** 话题（GitHub 仓库页右上角 Topics）；
3. 描述写清「这个插件能给我什么」—— 它会直接显示在卡片上；
4. 图标用单个 emoji，别放远程图片（宿主不下载任何插件资源）；
5. 在 README 里说明需要的最低应用版本（若用到）；
6. 想要分类准确，用中文分类名（`AI 助手`/`壁纸资源`/`创作工具`），否则按仓库 topic 猜。

## 8. 协议演进

- **加字段**：小版本内加**可选**字段（宿主忽略未知字段，老宿主不受影响）。
- **改语义 / 加能力**：能力只增不改，新能力进白名单后需要新版本应用才能用 ——
  作者用 `minAppVersion` 表达这个要求。
- **破坏性变更**：`schemaVersion` +1，宿主**拒绝**比它新的清单而不是猜着解释；
  同时保留对旧版本的兼容（v1 清单在 v2 宿主上继续可用）。
- 能力只增不改：`dsh-profile` 是 v1.1 加进来的，老清单（不声明它）行为不变。
- 宿主当前版本：`SCHEMA_VERSION` / `SUPPORTED_CAPABILITIES` 定义在
  `src-tauri/src/plugin/store.rs` 顶部；分类词表与前端 `src/lib/plugins.ts` **两处同源**，
  改一处必须改另一处。

## 9. 宿主保证

- 插件不会让应用崩溃：任何清单问题都只影响该插件（R10）。
- 插件不会被静默执行：没有代码入口（R1/R2）。
- 用户的插件目录干净可解释：一个 id 一个目录，一份清单，卸载即删除。
- 市场离线可用；联网失败只提示，不阻塞任何本地功能。
- 特权能力有明确前置确认，且只作用于本应用托管的那个 profile（不动用户自己的 profile）。
