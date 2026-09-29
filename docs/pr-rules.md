# 提交与 PR 规则 / Engineering PR Rules

> 本文件是本仓库提交与评审的**唯一规则来源**。所有自动化（本地钩子、CI）都只是这份
> 规则的实现，实现位置见文末[附录](#附录规则实现在哪)。
>
> 适用：所有进 `main` 的改动。**2026-09-29 起取消「一个功能一个 PR」**（见 §0.1）——
> 改完可以直接推 `main`；分支与 PR 变成可选手段。

## 0. 一句话版

| 事项 | 规则 |
| --- | --- |
| 分支 | `feat/` `fix/` `refactor/` `perf/` `docs/` `chore/` `release/` `hotfix/` + kebab-case；`wip/` 不准开 PR |
| 提交粒度 | 一批改动含多个功能时，本地 `pre-commit` 拦下；`pnpm commit:split` 按功能域自动拆成多条（§2 提交粒度） |
| 提交信息 | `type(scope): 中文主题`，主题说清"改了什么 + 什么条件下行为如何" |
| 正文 | 空行分隔；推荐四段：背景 / 改动 / 验证 / 影响面 |
| PR 标题 | 与提交信息同格式（squash 后它就是 `main` 上的标题） |
| PR 描述 | 「改动」「验证」两节必填且有实际内容（CI 校验）；「背景 / 动机」强烈建议 |
| 推送 | **`main` 由维护者直接推**；分支 + PR 是可选的（§0.1） |
| 用 PR 时 | Squash and merge；合并后删分支；描述按模板写给未来的自己看 |
| 门禁 | 本地钩子（提交时）+ CI（PR 时）；**本地能过，CI 就能过** —— 同一份规则 |
| 发布 | 只推 `v*` 标签触发打包；推 tag 就是人工确认那一步 |

### 0.1 2026-09-29：取消「任何功能都要单开 PR」

维护者的决定，原规定作废。理由是它把「一条命令」变成了「三遍流程」：为了把一个在制分支的
几千行拆成合规的 PR，要按 hunk 做手术、每个 PR 再各写一遍描述与验证 —— 成本远高于收益，
而它想换来的东西（可回滚、可读的 `git log`）**靠提交粒度就能拿到**。

现在的做法：

- 改完**直接推 `main`**；不再需要 PR、不再需要写「为什么属例外」。
- 分支 + PR 是**可选的**：想让 CI 跑一遍、想把这件事单独留成一条可回滚的记录、想给未来的
  自己写一份长描述时，再开。
- 体积上限（400 行 / 15 文件）与「一个 PR 一件事」的机器闸门**取消**（§5 / §6）。
- 保留的只有「提交信息怎么写」（§2）—— 它便宜，且是 `git log` 唯一的值钱部分。

三条底线，其余都是细节：

1. **一条提交说清一件事。**（原「一个 PR 一件事」已取消，见 §0.1）两件不相干的事分开提交 ——
   它们会共享同一次回滚、同一条"修复了什么"的记录，这是唯一还需要守的粒度约束，
   也是唯一被本地钩子盯着的一条（§2 提交粒度）。
2. **说清为什么，而不只是改了什么。** diff 自己会说"改了什么"，只有你能说"为什么"。
3. **证据先于结论。** 写"应该没问题"不如写"跑了什么命令、结果是多少、和基线差在哪"。

## 1. 分支

- `main` 是唯一长期分支。维护者**可以直接推**（§0.1）；用 PR 时仍建议 squash 让历史尽量线性，
  但线性是习惯，不再是硬要求。
- 从**最新** `main` 开分支；`main` 有更新时 rebase（不要把 `main` merge 进特性分支）。
- 命名：`<type>/<kebab-case>`，`type` 用第 2 节的类型名。

```
feat/playlist-manual-single-exits-rotation
fix/wallpaper-black-flash-on-switch
refactor/window-per-display
release/v2.0.0
```

- `wip/<主题>` 用来放未完成的在制快照。**它存在的意义就是不开 PR** —— 一旦要评审，
  先整理成能独立说明的提交（或干脆 squash 成一条），改名到 `feat/` / `fix/` 再开。

## 2. 提交信息

### 形状

```
type(scope): 主题

<空行>
正文（可选但强烈建议）
```

`scope` 可省略；多级范围用 `/`，例如 `fix(windows/linux):`。

### 类型

只有这 12 种，小写：

| 类型 | 用于 | 类型 | 用于 |
| --- | --- | --- | --- |
| `feat` | 新功能 | `test` | 测试 |
| `fix` | 修缺陷 | `build` | 构建 / 依赖 / 打包 |
| `docs` | 文档 | `ci` | CI / 工作流 |
| `style` | 格式（不改语义） | `chore` | 杂务（版本、脚本、清理） |
| `refactor` | 重构（行为不变） | `revert` | 回滚 |
| `perf` | 性能 | `release` | 发布 / 版本标记 |

**不允许**：`wip`、`del`、`update`、`misc`、`tmp`、`temp`。它们不是"不合法"，是**没有信息量**：
`wip:` 说明作者自己还没想清楚这次改动是什么，这种提交没有评审价值。

### scope

建议清单（来自本仓库既有提交与代码布局，不是硬约束）：

```
wallpaper playlist share workshop download render props apply hotkeys tray theme
update db library network mcp steam i18n ui core media quality security perf
deps build bundle ci pr release
scripts dev docs readme changelog windows macos linux
```

scope 检查只对**结构性**问题报错（空括号、含空格或逗号）。以下都**只是提示、不报错**：

- 清单外的 scope —— 新模块出现时应该能自然加进来；
- 用了大写（`fix(UI):`）—— 习惯上全小写，便于对应代码模块名；
- 用了中文（`fix(渲染器):`）—— 历史上有过，建议改用英文以便对应代码模块。

### 主题怎么写

一句话原则：**读完标题就知道"什么条件下、什么行为变了"**，不需要打开 diff。

| | 例子 |
| --- | --- |
| ❌ 空话 | `chore: 更新代码`、`fix: 修 bug`、`feat(ui): 优化界面` |
| ❌ 只说动作 | `fix(playlist): 修改轮播逻辑` |
| ✅ 说清条件与行为 | `fix(playlist): 手动设单张后仍按轮播上下文取下一张` |
| ✅ 说清机制 | `perf(wallpaper): 首帧就绪改事件推送 + 渲染器帧外收尾` |
| ✅ 说清边界 | `fix(bundle): Linux ARM64 包里装了 x64 的 libsteam_api.so —— 两架构共用配置写死了 x64 路径` |

（后三条取自本仓库真实提交，可直接照这个手感写。）

- 长度：建议 ≤ 100 字符，> 120 报错（长说明挪正文，别塞标题）。
- 末尾不加句号（中文顿号、破折号可以用）。
- 用中文写。专有名词、命令、标识符保留原文。

### 正文

头部与正文之间**必须空行**。推荐四段（可裁剪，但"验证"别省）：

```markdown
## 背景
旧实现为什么会出问题 / 用户报障的现象与复现条件。

## 改动
- 分条列改了什么；有取舍就写为什么选这个方案。

## 验证
跑了什么命令、结果是多少、与基线差在哪。

## 影响面
平台、数据库/配置迁移、破坏性变更。
```

本仓库里写得最好的一条提交（`ef93848`）长这样，可以直接当模板：

```
test(wallpaper): 给「用户显式清除过的屏」补回归测试 + 修 id 迁移漏搬标记

这套机制此前是**零测试覆盖**，而它的失效方式全是「不报错、两秒后行为不对」……

## 改动
- 抽 parse_stopped / encode_stopped（编解码；编码前排序 —— 集合迭代序不定……）
- **修缺口**：migrate_display_ids 此前只迁移 wallpaper_sessions，没搬「已清除」标记……

## 验证
`cargo test --lib`：

| | passed | failed |
|---|---|---|
| 基线 | 244 | 3 |
| 本提交 | **249** | 3（逐条相同） |
```

注意最后那张表：**基线 vs 本提交**的对比，比单说"测试通过"有说服力得多。

### footer（可选）

```
BREAKING CHANGE: <影响与迁移方式>
Closes #123
Refs #456
```

带 `!`（如 `feat(api)!: …`）或写 `BREAKING CHANGE:` 时，正文里要给出迁移方式。

### 提交粒度（2026-09-29 起有机器兜底）

规则本身没变：**一条提交说清一件事**。"改功能 + 顺手格式化整个文件"是两个提交，不是一个；
不相干的两件事挤在一起，回滚时会连带把别的改动一起带走 —— 这是取消 PR 闸门（§0.1）之后
**唯一还需要守的粒度约束**。变的是它现在不用靠记性：

**一、本地 `pre-commit` 会拦。** 提交时把暂存区按**功能域**归堆（域表 = 后端模块 + 前端
确有归属的功能模块，见 `scripts/split-commits.mjs` 的 `RULES`），命中下面任一条就拦下来：

| 触发 | 形状 | 例子 |
| --- | --- | --- |
| 双峰 | 两个功能域各占改动量 40% 以上（各自 ≥ 40 行） | steam 60 行 + quality 60 行 |
| 撒开 | 四个以上功能域各占 10% 以上（各自 ≥ 20 行），总量 ≥ 200 行 | 一次动 5 个模块各几十行 |
| 摊大饼 | 总量 ≥ 600 行且没有任何域能主导（最大域 < 40%），至少 3 个实体域 ≥ 8% | 「在制品一次性落库」 |

阈值是**对着本仓库全部 133 条提交校准**出来的（`node scripts/split-commits.mjs --audit <范围>`
可复现，当前 8/133 = 6% 被拦，全是标题里自己就写着多件事的那种）。校准时要保证**不拦**这些
正常提交：一个功能跨多个模块（`feat(playlist)`：wallpaper 74% / hotkeys 10%；`fix(mcp)`：
mcp 54% / network 29%）、集成面跟随（页面 / 词条 / 命令层 / 内存性能基建）、发布提交
（版本号 + CHANGELOG 全是 meta，天然不触发）。

**二、拆是一行命令。** 拦住之后：

```bash
pnpm commit:split              # 看分组草案；同时写出计划文件 .git/we-split-plan.json
#   → 改里面的标题（类型/scope/主题/正文都能改，文件也能在组间挪）
pnpm commit:split --apply      # 按计划落库；标题还是草案时会拒绝，这是刻意的
pnpm commit:split --apply --auto   # 不想写标题：直接按草案拆（标题会带"自动拆分草案"字样）
pnpm commit:split --worktree   # 连未暂存/未跟踪的改动一起纳入
```

- 拆分粒度是**整文件**：同一文件里混着两件事时拆不开（按 hunk 做手术被 §0.1 明确否掉了），
  这种情况脚本会把文件给"主域"，并让你在计划文件里改。
- 提交用**临时索引**做，所以"只暂存了文件的一部分"也会精确落进对应提交：已暂存的那部分
  进提交，未暂存的部分原样留在工作区。拆完索引会与新的 HEAD 对齐，`git status` 干净。
- 每条拆分提交的信息都要先过 §2 的校验器（同一份 `commit-msg-lint.mjs`），不合规就整批停下，
  不会写出一条坏历史。回退：`git reset --soft <拆分前的 sha>`（脚本会把这行打给你）。
- 默认**先出计划、人来写标题**：标题是 `git log` 唯一值钱的部分，机器起草的东西不该冒充它。
  急着提交时 `--auto` 才是"不改计划直接拆"的口子。

**三、不想拆怎么办。** 三种都行，代价不同：

```bash
WE_ALLOW_MIXED=1 git commit …   # 本次放行（"我知道这是一件事"）—— 首选，留痕最多
git commit --no-verify          # 绕过全部本地钩子（提交信息也不校验了）
WE_AUTO_SPLIT=1 git commit …    # 反过来：钩子检测到多功能就直接拆，原提交被取消（全自动）
```

在制快照（`wip/` 分支上的 `wip:` 提交）照旧可以随手写，但**落 `main` 前要整理**：
`git rebase -i` 合并/改写，让每条提交都能独立读懂；`--audit` 能先告诉你哪几条会被判成多功能。

**为什么 CI 不管这条。** CI 看到的是提交结果，看不到你当时把哪些文件放在一起暂存的 ——
判定必须发生在 `pre-commit`（本地）。所以这是本仓库少见的"**本地严、CI 松**"的一处：
别指望 CI 兜底，`--no-verify` 是真的绕过去了。

## 3. PR 标题

格式与提交信息完全相同，因为 **squash 合并后 PR 标题会成为 `main` 上的提交标题**。
CI 用同一份规则（`commit-msg-lint.mjs --title`）校验，所以标题写成
`feat(playlist): 手动设单张退出该屏轮播` 这种形式。

若 PR 内有多种类型改动，选**主导**的那一个；`feat` 里夹带 `fix` 是常态，不必纠结 —— 但
"夹带"指同一件事的收尾，不是把两件不相干的功能塞进一个 PR（见第 5 节）。

## 4. PR 描述

用仓库模板：`.github/PULL_REQUEST_TEMPLATE.md`（开 PR 时自动带出）。

- **必填**：「改动」「验证」—— 必须有实际内容，**只留模板占位符会被 CI 拦下**。
- **强烈建议**：「背景 / 动机」—— 缺少时 CI 只警告，但评审人最需要的就是这一节。
- 模板里的自检清单请真的勾；留着 `- [ ]` 时 CI 会提示。

机器只能判断"填没填"，判断不了"填得对不对"。**验证一节是写给人看的**：写"已验证"等于没写，
写"`cargo test --lib` 244 → 249 passed，3 例环境失败与基线逐条相同"才是证据。

## 5. 体积与拆分

**不设上限**（2026-09-29 取消闸门，见 §0.1）：想拆就拆、不想拆就直推，由维护者自己判断。

仍然建议（只是建议，没有机器拦）：

- 一条提交说清一件事；不相干的两件事分开提交 —— 回滚与 `git log` 都还在。
- 重构与行为变更尽量分成两条提交（回看时能分清"行为不变"的承诺）。
- 全仓格式化单独一条提交，别混进逻辑改动里。

要自查体积时仍可用（它只报数，不再拦任何东西）：

```bash
node scripts/pr-size-check.mjs --range origin/main..HEAD
```

## 6. 门禁

### 本地（提交时）

`pnpm install` 会自动装好两个钩子（`package.json` 的 `prepare` 调 `scripts/install-hooks.mjs`）。
手动装/查/卸：

```bash
pnpm hooks:install     # 安装（幂等）
pnpm hooks:status      # 看装了什么
pnpm hooks:uninstall   # 卸载（有备份则还原）
```

| 钩子 | 指向 | 管什么 | 绕过 |
| --- | --- | --- | --- |
| `commit-msg` | `scripts/commit-msg-lint.mjs` | §2 提交信息形状 | `git commit --no-verify`（**CI 仍会拦**） |
| `pre-commit` | `scripts/split-commits.mjs --hook` | §2 提交粒度（一批改动含多个功能就拦） | `WE_ALLOW_MIXED=1`，或 `--no-verify`（**CI 拦不住**，见 §2） |

这两个之外不碰别的钩子：`post-checkout` / `post-commit` / `post-merge` / `pre-push`
是 **git-lfs 的**，安装脚本不碰它们。钩子在运行时自己找 node，找不到就放行并提示 ——
钩子是"帮你不犯错"，不该因为环境缺 node 就把提交卡死。

回归测试（`node:test`，零依赖，全在临时仓库里做）：

```bash
pnpm test:scripts      # 门禁脚本的回归测试：拆分器（内容守恒 / 部分暂存 / 拒绝路径 / 钩子自动拆分）+ 棘轮（比较逻辑与测量）
```

### CI（PR 时）

`.github/workflows/pr-check.yml`，按改动路径只跑该跑的：

| 检查 | 命令 | 触发路径 | 档位 | 当前基线 |
| --- | --- | --- | --- | --- |
| 提交信息 + PR 标题 | `commit-msg-lint.mjs` | 全部 | **阻断** | 通过 |
| PR 描述小节 | `commit-msg-lint.mjs --require-sections` | 全部 | **阻断** | 通过 |
| 版本号一致性 | `check-versions.mjs` | 全部 | **阻断**¹ | 通过 |
| 类型检查 | `pnpm typecheck` | `src/` 等 | **阻断** | 通过 |
| 中英词条审计 | `i18n-audit.mjs` | `src/` 等 | **阻断** | 通过 |
| 前端构建 | `pnpm build` | `src/` 等 | **阻断** | 通过 |
| Cargo.lock 一致性 | `cargo metadata --locked` | `src-tauri/` | **阻断** | 通过 |
| 编译全部目标 | `cargo check --all-targets` | `src-tauri/` | **阻断** | 通过 |
| rustfmt | `gate-ratchet.mjs --only fmt` | `src-tauri/` | **棘轮**³ | ⚠️ 551 处差异（`gate-baseline.json`） |
| clippy | `gate-ratchet.mjs --only clippy` | `src-tauri/` | **棘轮**³ | ⚠️ 50 条警告（去重后） |
| 单元测试 | `cargo test --lib` | `src-tauri/` | 报告 | 本机 255 passed / 0 failed / 2 ignored² |
| CHANGELOG 提醒 | 文件比对 | `src/`、`src-tauri/src/` | 提醒 | — |

¹ 三处权威版本（`package.json` / `tauri.conf.json` / `Cargo.toml`）不一致即失败；
`bin-info.plist`（dev 版内嵌）不一致只警告。
² 本机（macOS）2026-09-29 实测：**255 passed / 0 failed / 2 ignored**，那 2 个 `#[ignore]`
   是设备依赖（`audio_restart_resumes_frames_after_stop`）与网络依赖（`real_subs_page_probe`）
   —— 依赖机器的测试不能当门禁，`#[ignore]` 是正确归宿。**Linux 侧基线待第一次 push 校验
   量出来**（量完这条就翻成阻断，见下面的翻正式条件）。文档里的数字**以 `gate-baseline.json` 为准**，
   这里手抄的会过期 —— 2026-09-29 改这条时就发现它还写着 249/3。
³ **棘轮（ratchet）**：读数进 `scripts/gate-baseline.json`，**比基线差才阻断**，比基线好提示
   收紧（`pnpm gate:ratchet --update`，基线只许收紧）。它是"报告"与"一把梭阻断"之间的第三档：
   见下文。

### 为什么有的是"报告"，有的是"棘轮"

**因为它们的基线本来就不过。** 把 `cargo fmt --check` 设成阻断，只会让每个 PR 都红、
然后所有人学会无视 CI —— 那比没有 CI 更糟。

**但纯"报告"也守不住**：日志没人看，数字只会悄悄往上涨（文档里那句"604 处差异"自己掉到
551，不是因为有东西在守）。所以 fmt / clippy 用**棘轮**：

- 读数进 `scripts/gate-baseline.json`（按 `os-arch` 分平台，工具链版本一起记）；
- **比基线差 → 阻断**；比基线好 → 提示 `pnpm gate:ratchet --update` 收紧；
- 工具链变了（CI 用 `@stable`，浮动）或该平台没测过 → **当次放行并打印读数**，不硬拦 ——
  拿旧尺子量新工具的读数没有意义，别把漂移当成回归；
- 清理存量（全仓格式化、逐条修 clippy）仍是**独立一件事**，单独提交，别混进功能改动（§5）。

命令：`pnpm gate:ratchet`（全量）、`pnpm gate:ratchet --only fmt`、`pnpm gate:ratchet --update`。

**单元测试**仍是报告档，**证据在本地** —— PR 描述里贴本地 `cargo test --lib`
的通过/失败数与自己基线的对比（见第 2 节 `ef93848` 的写法）。

**翻正式的条件**：把基线清干净（测试档：Linux 侧量出来也是 0 failed），再删掉对应的
`continue-on-error: true`。一次一条，别一口气全开。

### 不在 PR 门禁里的事

三平台整包构建是**手动**触发的 `.github/workflows/build-test.yml`（只出 artifact、
不发布）。PR 门禁只求快速反馈，不为出包正确性背书 —— 出包正确性由发布流程负责。

**直推 `main` 不跑任何门禁**：`pr-check.yml` 只挂在 `pull_request` 事件上（§0.1 之后的常态）。
想让 CI 过一遍就开个 PR；否则推送前把对应命令在本地跑一遍 —— 至少 `cargo check --all-targets`、
`pnpm typecheck`、`cargo test --lib`。

## 7. 合并

- **Squash and merge**：一条 PR 变成 `main` 上的一条提交 —— **标题取 PR 标题、正文取 PR 描述**
  （仓库设置 `squash_merge_commit_title=PR_TITLE`、`squash_merge_commit_message=PR_BODY`，
  2026-09-28 设定；**GitHub 会在标题后追加上 `(#PR号)`**，例如 `… (#7)` —— 不想要这个后缀就
  合并时显式传 `--subject`）。所以：
  - "PR 标题也要合规"的全部原因，就是它会成为 `main` 上的提交标题；
  - **PR 描述就是 `main` 上的提交正文** —— 按"给未来 `git log` 的人看"来写；模板里的 HTML
    注释与"本 PR 后续再改"这类只在评审时有意义的话，合并前删掉；
  - PR 里那几条提交信息**不会**进 `main`（不会再被拼成一坨）。多提交不是问题，有问题的是
    把两件不相干的事放进同一个 PR。
- 合并后删除远程分支。
- 合并前需满足：CI 全绿 + 描述「改动」「验证」填实 + 无未勾选的自检项。
- **直接 push `main` 是允许的**（§0.1）：不再要求先开 PR、也不再需要声明"为什么属例外"。
  用 PR 是自选的场景：想让 CI 跑一遍、想把这件事单独留成一条可回滚的记录、想给未来的自己
  写一份长描述。
- 没开 PR 的改动，请自己在推送前跑一遍对应门禁（§6 那几条）。开 PR 的仍按上面的流程来。
- 发布（§8）与 hotfix 照旧：tag 就是人工确认那一步。

### 分支保护建议（GitHub 设置里配，不在版本控制内）

- 保护分支：`main`
- 必需检查：**`PR 校验汇总`**（`pr-check.yml` 的汇总 job —— 分档逻辑变化时不用改设置）
- 禁止 force push、要求分支为最新后再合并
- **不要**勾「禁止直接 push」：维护者要能直推 `main`。当前未启用保护（2026-09-29 查
  `repos/oneincase/WallpaperEM/branches/main/protection` 为 404）—— 启用保护时记住这条。

## 8. 发布与 CHANGELOG

### CHANGELOG

`CHANGELOG.md` 遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
未发布的改动写进 `## [Unreleased]`。**用户可见**的变更（功能、行为、修复、界面、
依赖升级带来的差异）都要有一条；纯内部重构、测试、注释可以省。

CI 检测到 `src/` 或 `src-tauri/src/` 有改动而 `CHANGELOG.md` 没动时会提醒（不阻断）。

### 版本号

版本号散在四处，**权威三处必须一致**：

| 文件 | 作用 |
| --- | --- |
| `package.json` | 前端包版本 |
| `src-tauri/tauri.conf.json` | **安装包版本**（打包实际用这个） |
| `src-tauri/Cargo.toml` | Rust crate 版本 |
| `src-tauri/bin-info.plist` | dev 二进制内嵌的 `CFBundleShortVersionString` 与 `CFBundleVersion`（两个键，仅提示） |

`pnpm versions:check` 一条命令查完。`bin-info.plist` 的两个键历来同值，但**只查一个就会漏掉
另一个**：2.0.0 发布时两个键一起停在 `1.1.0`，dev 版 app 显示的版本号偏旧 —— 现在两个键都查、
不一致都提示（仍只提示不阻断：它只进 dev 二进制，打包版用 `tauri.conf.json`）。

### 发布流程

```bash
# 1. 版本号四处对齐（pnpm versions:check 应为全绿）
# 2. CHANGELOG：把 [Unreleased] 归到 ## [x.y.z] - YYYY-MM-DD
# 3. 发布提交（走 main，见第 7 节的例外）
git commit -m 'release: v2.0.0（本次要点的短清单）'
# 4. 推 tag —— 这一步就是人工确认
git tag v2.0.0 && git push origin v2.0.0
```

推 `v*` 标签会触发 `build-dmg` / `build-linux` / `build-windows` 出安装包。
想在发布前先验三平台能否构建，手动跑 `build-test.yml`（只出 artifact，不建 Release）。

### hotfix

从 `main` 开 `hotfix/<问题>`，改完按正常 PR 流程合并（不要因为"急"就跳过验证），
然后按上面的发布流程出补丁版本。补丁版本必须在 CHANGELOG 里注明修了什么。

## 9. 不许进仓库的东西

- **构建产物**：`dist/`、`src-tauri/target/`、`src-tauri/bundled/`、`.pnpm-store/`、`.cargo-home/`
- **依赖目录**：`node_modules/`
- **本机数据**：`*.db`、`credentials.dat`、钥匙串导出
- **工具工作目录**：`.ui-shots/`、`.v2c/`、`.video_agent/`、`.zcode/`、`proto-video-loop/clips/`
- **系统垃圾**：`.DS_Store`、`*.bak`、`*~`
- **大二进制**（> 5 MB 的必要二进制）：必须走 **Git LFS** 并在 `.gitattributes` 里登记。
  注：当前 `.gitattributes` 为空、也没有 LFS 追踪的文件（`git lfs ls-files` 无输出），
  所以这条是预防性的 —— 真要加大文件时，先 `git lfs track` 再 `git add`。

`.gitignore` 已经覆盖上面绝大多数；改动确实需要新忽略项时，**同时**更新
`.gitignore` 与 `.zcodeignore`（后者供编码工具用，两者历来保持一致）。

## 10. 常见的被打回原因

1. 提交信息是 `wip:` / `update` / `修改代码` 这类没有信息量的标题。
2. PR 描述只有模板骨架，或「验证」写着"已测试"却没有命令与结果。
3. 夹带无关改动：顺手重构、整文件格式化、临时调试代码、本地绝对路径。
4. 不相干的两件事挤在**同一条提交**里：回滚时会连带把别的改动一起带走（体积不再设限，这条仍守；
   本地 `pre-commit` 会拦，见 §2 提交粒度）。
5. 改了 `src/` 或 `src-tauri/src/` 但 `CHANGELOG.md` 没动（用户可见变更）。
6. 引入新 warning（clippy / tsc）而不解释。
7. 修了 bug 但没补能复现它的测试 —— 这类修复的失效方式是"不报错、过一会行为不对"，
   只能靠测试兜住。
8. 版本号只改了一处。

## 附录：规则实现在哪

| 文件 | 职责 |
| --- | --- |
| `docs/pr-rules.md` | 本文件：规则的唯一来源 |
| `scripts/commit-msg-lint.mjs` | 提交信息 / PR 标题 / PR 正文校验（**零依赖**，本地与 CI 共用） |
| `scripts/split-commits.mjs` | 提交粒度：多功能改动的判定、按功能域拆分、`pre-commit` 拦截（**零依赖**） |
| `scripts/split-commits.test.mjs` | 上者的回归测试（`pnpm test:scripts`，全在临时仓库里跑） |
| `scripts/gate-ratchet.mjs` | 门禁棘轮：fmt / clippy 的「不新增」闸门（读数比基线差才红） |
| `scripts/gate-baseline.json` | 棘轮基线（按 `os-arch` 分平台 + 工具链版本）；**收紧 = 改这个文件** |
| `scripts/install-hooks.mjs` | 安装 / 卸载 / 查看 `commit-msg` + `pre-commit` 钩子（不碰 git-lfs 的钩子） |
| `scripts/check-versions.mjs` | 四处版本号一致性 |
| `scripts/pr-size-check.mjs` | 体积自查（**不再拦 PR**，2026-09-29 取消闸门） |
| `.github/PULL_REQUEST_TEMPLATE.md` | PR 描述模板（CI 据此校验小节） |
| `.github/workflows/pr-check.yml` | PR 门禁（规则 + 前端 + Rust，按路径触发） |

自查一条命令搞定：

```bash
pnpm lint:commit --message "feat(playlist): 手动设单张退出该屏轮播"
pnpm lint:commit --range origin/main..HEAD        # 校验整个分支
pnpm commit:split                                 # 提交粒度：看当前暂存区会被怎么拆（不会提交）
node scripts/split-commits.mjs --audit origin/main..HEAD    # 拿历史回看这条规则拦了谁（只读）
node scripts/split-commits.mjs --explain <文件>   # 某个文件算哪个功能域
pnpm test:scripts                                 # 拆分器 + 棘轮等门禁脚本的回归测试
pnpm gate:ratchet                                 # fmt/clippy 棘轮（--update 收紧基线，--only 单测一项）
node scripts/pr-size-check.mjs --range origin/main..HEAD    # 体积自查（可选，不拦）
pnpm versions:check
```
