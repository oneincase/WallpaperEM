# 提交与 PR 规则 / Engineering PR Rules

> 本文件是本仓库提交与评审的**唯一规则来源**。所有自动化（本地钩子、CI）都只是这份
> 规则的实现，实现位置见文末[附录](#附录规则实现在哪)。
>
> 适用：所有进 `main` 的改动。个人在 `wip/` 分支上的在制快照不受约束，但**不得开 PR**。

## 0. 一句话版

| 事项 | 规则 |
| --- | --- |
| 分支 | `feat/` `fix/` `refactor/` `perf/` `docs/` `chore/` `release/` `hotfix/` + kebab-case；`wip/` 不准开 PR |
| 提交信息 | `type(scope): 中文主题`，主题说清"改了什么 + 什么条件下行为如何" |
| 正文 | 空行分隔；推荐四段：背景 / 改动 / 验证 / 影响面 |
| PR 标题 | 与提交信息同格式（squash 后它就是 `main` 上的标题） |
| PR 描述 | 「改动」「验证」两节必填且有实际内容（CI 校验）；「背景 / 动机」强烈建议 |
| 体积 | 有效改动 ≤ 400 行、≤ 15 个文件；超了要么拆，要么在描述里写「体积说明」（CI 阻断） |
| 合并 | Squash and merge；合并后删分支；禁止直接 push `main` |
| 门禁 | 本地钩子（提交时）+ CI（PR 时）；**本地能过，CI 就能过** —— 同一份规则 |
| 发布 | 只推 `v*` 标签触发打包；推 tag 就是人工确认那一步 |

三条底线，其余都是细节：

1. **一个 PR 一件事，禁止捆绑功能。** 评审人要在一次阅读里判断对错；混进来的无关改动会稀释
   注意力。squash 之后一个 PR 就是 `main` 上的一条提交，所以"一个功能一个提交"是靠
   "一个功能一个 PR"落地的 —— 两件不相干的事捆在一起，等于让它们共享同一条提交、同一次回滚、
   同一条"修复了什么"的记录。收尾性的小改（同一件事的后续、顺手补的文档）不算捆绑。
2. **说清为什么，而不只是改了什么。** diff 自己会说"改了什么"，只有你能说"为什么"。
3. **证据先于结论。** 写"应该没问题"不如写"跑了什么命令、结果是多少、和基线差在哪"。

## 1. 分支

- `main` 是唯一长期分支，**历史上保持线性**（无 merge commit）—— 靠 squash 合并维持。
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
update db library network mcp steam i18n ui deps build bundle ci pr release
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

### 提交粒度

- 一个提交一件事。"改功能 + 顺手格式化整个文件"是两个提交，不是一个。
- 在制快照（`wip:`）可以随手提交，但**开 PR 前要整理**：`git rebase -i` 合并/改写，
  让每条提交都能独立读懂。CI 校验的是 PR 范围内的**每一**条提交。

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

**一个功能一个 PR** —— 上限是它的机器化代理：**有效改动 ≤ 400 行、≤ 15 个文件**
（`dist/`、`pnpm-lock.yaml`、图片/字体等机械产物不计）。

判定在 CI 里（`scripts/pr-size-check.mjs`，见第 6 节），本地同一条命令自查：

```bash
node scripts/pr-size-check.mjs --range origin/main..HEAD --body-file /tmp/pr-body.md
```

**超过上限**时二选一：拆成几个 PR，或在描述里写「体积说明」讲清为什么没法拆（≥ 20 字）。
三种写法都认：一个标题含「体积 / 拆分 / 规模 / 上限」的小节、标题行上直接写
（`## 体积说明：…`）、或独立一行（`体积说明：…`）。没写就是 CI 红。

**必须拆开的情况**：

- **两件及以上互不相干的功能/修复** —— 这就是"捆绑"：squash 之后它们会是**同一条**提交，
  共享同一次回滚、同一条"修复了什么"的记录。
- 重构 + 行为变更（评审人分不清哪部分是"行为不变"的承诺）
- 全仓格式化 + 逻辑改动（diff 被噪声淹没，等于不可评审）
- 依赖升级 + 适配代码（升级本身要能单独回滚）

允许大 PR 的情况：机械性改动（重命名、批量迁移、CHANGELOG 汇总），但要在描述里**说明它是机械的**
—— 那句话就是「体积说明」，CI 会要它。

## 6. 门禁

### 本地（提交时）

`pnpm install` 会自动装好 `commit-msg` 钩子（`package.json` 的 `prepare` 调
`scripts/install-hooks.mjs`）。手动装/查/卸：

```bash
pnpm hooks:install     # 安装（幂等）
pnpm hooks:status      # 看装了什么
pnpm hooks:uninstall   # 卸载（有备份则还原）
```

钩子只接管 `commit-msg`；`post-checkout` / `post-commit` / `post-merge` / `pre-push`
是 **git-lfs 的**，安装脚本不碰它们。

急着提交时 `git commit --no-verify` 可以绕过钩子 —— 但 **CI 会拦**。绕过只该用于
"我知道这条不合规且有意为之"（例如回滚提交）。

### CI（PR 时）

`.github/workflows/pr-check.yml`，按改动路径只跑该跑的：

| 检查 | 命令 | 触发路径 | 档位 | 当前基线 |
| --- | --- | --- | --- | --- |
| 提交信息 + PR 标题 | `commit-msg-lint.mjs` | 全部 | **阻断** | 通过 |
| PR 描述小节 | `commit-msg-lint.mjs --require-sections` | 全部 | **阻断** | 通过 |
| 体积（一个 PR 一件事） | `pr-size-check.mjs` | 全部 | **阻断** | 通过³ |
| 版本号一致性 | `check-versions.mjs` | 全部 | **阻断**¹ | 通过 |
| 类型检查 | `pnpm typecheck` | `src/` 等 | **阻断** | 通过 |
| 中英词条审计 | `i18n-audit.mjs` | `src/` 等 | **阻断** | 通过 |
| 前端构建 | `pnpm build` | `src/` 等 | **阻断** | 通过 |
| Cargo.lock 一致性 | `cargo metadata --locked` | `src-tauri/` | **阻断** | 通过 |
| 编译全部目标 | `cargo check --all-targets` | `src-tauri/` | **阻断** | 通过 |
| rustfmt | `cargo fmt --all --check` | `src-tauri/` | 报告 | ❌ 41 文件 / 604 处差异 |
| clippy | `cargo clippy --all-targets` | `src-tauri/` | 报告 | ⚠️ 57 条警告 |
| 单元测试 | `cargo test --lib` | `src-tauri/` | 报告 | 249 passed / 3 failed² |
| CHANGELOG 提醒 | 文件比对 | `src/`、`src-tauri/src/` | 提醒 | — |

¹ 三处权威版本（`package.json` / `tauri.conf.json` / `Cargo.toml`）不一致即失败；
`bin-info.plist`（dev 版内嵌）不一致只警告。
² 这是**本机（macOS）**基线；Linux CI 上是 211 passed / 2 failed（平台专属断言没按平台 gate +
   mock 服务器连不上），两个数都对，只是平台不同。报告档不挡人，清干净才能翻成阻断。
³ 超上限时描述里写「体积说明」即通过（当前通过依据：小 PR 本就在上限内；1396 行那一次写了理由）。

### 为什么有的是"报告"而不是"阻断"

**因为它们的基线本来就不过。** 把 `cargo fmt --check` 设成阻断，只会让每个 PR 都红、
然后所有人学会无视 CI —— 那比没有 CI 更糟。

所以这几项按"**不新增**"来要求：

- **rustfmt**：新增/改动的代码要合规（编辑器保存时格式化即可）。
  全仓统一格式化是**独立一件事**，单独开 PR 做，别混进功能 PR。
- **clippy**：本 PR 不新增警告。
- **单元测试**：CI 上是参考，**证据在本地** —— PR 描述里贴本地 `cargo test --lib`
  的通过/失败数与自己基线的对比（见第 2 节 `ef93848` 的写法）。

**翻正式的条件**：把基线清干净，然后在 `pr-check.yml` 里删掉对应的
`continue-on-error: true`。一次一条，别一口气全开。

### 不在 PR 门禁里的事

三平台整包构建是**手动**触发的 `.github/workflows/build-test.yml`（只出 artifact、
不发布）。PR 门禁只求快速反馈，不为出包正确性背书 —— 出包正确性由发布流程负责。

## 7. 合并

- **Squash and merge**：一条 PR 变成 `main` 上的一条提交 —— **标题取 PR 标题、正文取 PR 描述**
  （仓库设置 `squash_merge_commit_title=PR_TITLE`、`squash_merge_commit_message=PR_BODY`，
  2026-09-28 设定）。所以：
  - "PR 标题也要合规"的全部原因，就是它会成为 `main` 上的提交标题；
  - **PR 描述就是 `main` 上的提交正文** —— 按"给未来 `git log` 的人看"来写；模板里的 HTML
    注释与"本 PR 后续再改"这类只在评审时有意义的话，合并前删掉；
  - PR 里那几条提交信息**不会**进 `main`（不会再被拼成一坨）。多提交不是问题，有问题的是
    把两件不相干的事放进同一个 PR。
- 合并后删除远程分支。
- 合并前需满足：CI 全绿 + 描述「改动」「验证」填实 + 无未勾选的自检项。
- **禁止直接 push `main`**。例外只有两类，且都要在提交信息里写明：(a) 发布提交
  （版本号 + CHANGELOG）；(b) 紧急 hotfix（事后补一条说明性提交或 PR 记录）。
- 单人项目的现实：作者不能真的"批准"自己的 PR。此时 CI + 模板自检清单就是那道关卡 ——
  这也是为什么门禁要尽量机器化，而不是依赖"另一个人会看"。

### 分支保护建议（GitHub 设置里配，不在版本控制内）

- 保护分支：`main`
- 必需检查：**`PR 校验汇总`**（`pr-check.yml` 的汇总 job —— 分档逻辑变化时不用改设置）
- 禁止直接 push、禁止 force push、要求分支为最新后再合并

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
4. 一个 PR 干了两件不相关的事，或者大到没法逐行看 —— 后者会被体积闸门拦下（要你在描述里写
   「体积说明」）；前者机器判不了，只能靠这条规则与模板自检清单。
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
| `scripts/install-hooks.mjs` | 安装 / 卸载 / 查看 `commit-msg` 钩子（不碰 git-lfs 的钩子） |
| `scripts/check-versions.mjs` | 四处版本号一致性 |
| `scripts/pr-size-check.mjs` | 体积闸门：超出 400 行 / 15 个文件时，要求在描述里写「体积说明」 |
| `.github/PULL_REQUEST_TEMPLATE.md` | PR 描述模板（CI 据此校验小节） |
| `.github/workflows/pr-check.yml` | PR 门禁（规则 + 前端 + Rust，按路径触发） |

自查一条命令搞定：

```bash
pnpm lint:commit --message "feat(playlist): 手动设单张退出该屏轮播"
pnpm lint:commit --range origin/main..HEAD        # 校验整个分支
pnpm size:check --range origin/main..HEAD --body-file /tmp/pr-body.md   # 体积（一个 PR 一件事）
pnpm versions:check
```
