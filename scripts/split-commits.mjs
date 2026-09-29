#!/usr/bin/env node
// 按功能域拆分一批改动 —— 「一条提交说清一件事」的机器实现（规则见 docs/pr-rules.md §2 提交粒度）。
//
// 三件事：
//   1. 判定：把改动（默认只看暂存区）按**功能域**归堆，判定这批改动是否含多个功能；
//   2. 拆分：--apply 时按域生成多条提交，每条只含一个域的文件；
//   3. 拦下：--hook 供 pre-commit 钩子调用，多功能时阻断提交并给出下一步命令。
//
// 为什么叫"域"而不是"功能"：机器读不出意图，只能按**路径 + 改动量占比**近似。所以这里的取舍是
//   - 判定保守（阈值高）：宁可漏报，也不要在你写一个功能时反复打扰；
//   - 分组只是草案：默认走计划文件（.git/we-split-plan.json），标题由人来写；
//     只有 Plan 里的标题改过（不再是草案）或显式 --auto 才允许提交 —— 标题是 `git log`
//     唯一值钱的部分，自动生成的东西不该冒充它。
//   - 同一文件里混着两件事时拆不开（那要按 hunk 做手术，§0.1 已明确否掉）。拆分粒度是**整文件**。
//
// 用法：
//   node scripts/split-commits.mjs                      # 看分组草案（只读改动，写计划文件）
//   node scripts/split-commits.mjs --apply              # 按计划拆分提交（标题还是草案时拒绝）
//   node scripts/split-commits.mjs --apply --auto       # 不审计划，直接按草案拆分提交
//   node scripts/split-commits.mjs --hook               # pre-commit 钩子：0 放行 / 3 拦下
//   node scripts/split-commits.mjs --worktree           # 连未暂存的改动一起分析（--apply 时一并提交）
//   node scripts/split-commits.mjs --range HEAD~5..HEAD # 只读：看历史上某条/某段提交会被怎么归堆
//   node scripts/split-commits.mjs --explain src-tauri/src/mcp/tools.rs
//   node scripts/split-commits.mjs --json
//
// 选项：--granularity loose|strict（默认 strict）、--force、--max-groups <n>、--no-sign、--no-color、--help
//
// 环境变量（钩子用）：
//   WE_ALLOW_MIXED=1     放行本次提交，不做拆分检查（"我知道这是一件事"）
//   WE_AUTO_SPLIT=1      检测到多功能时直接拆（原提交被取消），不再让人确认
//   WE_SPLIT_MAX_GROUPS  拆分组的数量上限，超过就不拦（默认 8：撒这么开通常是全仓整理）
//   WE_SPLIT_INTERNAL=1  内部用：拆分器自己创建的提交，钩子直接放行（防止递归）
//
// 退出码：0 通过 / 已完成　1 用法或环境错误（钩子里会放行并提示）　3 判定为多功能，拦下

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

/** 本脚本所在目录（用 fileURLToPath 而不是 URL.pathname：路径里可能有空格/中文）。 */
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));

// ------------------------------------------------------------------ 域表

// kind 的三档，决定了它在判定与分组里的角色：
//   feature —— 可以独立成一条提交，参与"这批改动含几个功能"的判定
//   glue    —— 集成面（命令层/入口/共享 UI/词条）：改了它不代表多了一个功能，永远并进主域
//   meta    —— 文档与工程配置：只有在确实要拆时，才单独成一条（收尾）
//
// 表是**有序**的，先命中先算数：前端那几个功能性模块必须排在前端通用层的兜底规则之前。
// 路径对不上任何一条时落到 core（glue）—— 新模块出现时最坏的结果是"少拆"，不是"乱拆"，
// 并且 `--explain` 会明说"未登记"。
const RULES = [
  // —— meta：文档与工程 ——
  [/^\.github\//, "ci"],
  [/^scripts\//, "scripts"],
  [/^proto-video-loop\//, "scripts"],
  [/^docs\//, "docs"],
  [/^CHANGELOG\.md$/, "docs"],
  [/^README\.md$/, "docs"],
  [/\.(md|mdx)$/, "docs"],
  [/^(LICENSE|\.gitignore|\.zcodeignore|\.gitattributes)$/, "chore"],
  [/^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|tsconfig\.json|vite\.config\.ts)$/, "deps"],
  [/^src-tauri\/Cargo\.(toml|lock)$/, "deps"],
  [/^src-tauri\/(build\.rs|tauri\.conf\.json|.*\.plist|capabilities\/|gen\/|icons\/)/, "build"],
  [/^Dockerfile/, "build"],
  [/^public\//, "build"],

  // —— 前端里确有归属的功能模块（必须早于下面的前端兜底）——
  [/^src\/hooks\/useApplyWallpaper/, "apply"],
  [/^src\/lib\/apply-target\.ts$/, "apply"],
  [/^src\/components\/ShareModal|^src\/components\/Qr(Image|Modal)|^src\/components\/SubscriptionsModal/, "share"],
  [/^src\/pages\/Shares\.tsx$/, "share"],
  [/^src\/lib\/qrcode\.ts$/, "share"],
  [/^src\/components\/Workshop|^src\/pages\/Workshop\.tsx$|^src\/hooks\/useWorkshopFilter/, "workshop"],
  [/^src\/components\/WallpaperPropsModal|^src\/hooks\/useItemProps|^src\/lib\/weCondition/, "props"],
  [/^src\/lib\/qualityPresets\.ts$/, "quality"],

  // —— glue：前端通用层（页面、通用组件、入口、IPC、词条）——
  [/^src\/(App|main|props-main)\.tsx$|^src\/index\.css$/, "ui"],
  [/^src\/api\//, "core"],
  [/^src-tauri\/src\/(lib|main|commands|util|misc|workspace|main_window)\.rs$/, "core"],
  [/^src\/lib\/i18n\.ts$|^src\/locales\/|^src-tauri\/src\/i18n\.rs$/, "i18n"],
  [/^src\//, "ui"],
  [/^index\.html$|^props\.html$/, "ui"],

  // —— feature：后端模块即功能边界 ——
  [/^src-tauri\/src\/wallpaper\/|^src-tauri\/src\/system_wallpaper\.rs$|^src-tauri\/src\/blur\.rs$/, "wallpaper"],
  [/^src-tauri\/src\/download\/|^src-tauri\/src\/(ffmpeg|pty)\.rs$/, "download"],
  [/^src-tauri\/src\/mcp\/|^src-tauri\/src\/scene_inspect\.rs$|^src-tauri\/templates\//, "mcp"],
  [/^src-tauri\/src\/steam\//, "steam"],
  [/^src-tauri\/src\/workshop(_upload)?\.rs$/, "workshop"],
  [/^src-tauri\/src\/hotkeys\.rs$/, "hotkeys"],
  [/^src-tauri\/src\/update\.rs$/, "update"],
  [/^src-tauri\/src\/(db\.rs|schema\.sql)$/, "db"],
  [/^src-tauri\/src\/(library|cover_cache)\.rs$/, "library"],
  [/^src-tauri\/src\/(now_playing|media_bridge)\.rs$|^src-tauri\/src\/audio_capture/, "media"],
  [/^src-tauri\/src\/(we_props|props_window)\.rs$/, "props"],
  [/^src-tauri\/src\/content_server\.rs$/, "network"],
  [/^src-tauri\/src\/subscriptions\.rs$/, "share"],
  [/^src-tauri\/src\/(keychain|secure_store)\.rs$/, "security"],
  [/^src-tauri\/src\/(mem_watch|mem_pressure)\.rs$/, "perf"],
  [/^src-tauri\/src\/(we_shim\.(rs|js)|we_assets\.rs)$/, "render"],
  [/^renderer\//, "render"],
];

/** 域 → 角色 / 提交 scope / 中文说明。scope 与 docs/pr-rules.md §2 的建议清单一致。 */
const DOMAINS = {
  // meta
  docs: { kind: "meta", scope: "changelog", label: "文档 / CHANGELOG" },
  deps: { kind: "meta", scope: "deps", label: "依赖清单" },
  build: { kind: "meta", scope: "build", label: "打包 / 图标 / 清单" },
  ci: { kind: "meta", scope: "ci", label: "CI / 工作流" },
  scripts: { kind: "meta", scope: "scripts", label: "脚本 / 工具" },
  chore: { kind: "meta", scope: "chore", label: "仓库杂务" },
  // glue
  core: { kind: "glue", scope: "core", label: "命令层 / 入口（集成面）" },
  ui: { kind: "glue", scope: "ui", label: "页面 / 通用组件（集成面）" },
  i18n: { kind: "glue", scope: "i18n", label: "词条（集成面）" },
  // feature
  wallpaper: { kind: "feature", scope: "wallpaper", label: "壁纸渲染 / 播放" },
  playlist: { kind: "feature", scope: "playlist", label: "轮播" },
  apply: { kind: "feature", scope: "apply", label: "应用到桌面" },
  download: { kind: "feature", scope: "download", label: "下载 / 转码" },
  workshop: { kind: "feature", scope: "workshop", label: "工坊" },
  mcp: { kind: "feature", scope: "mcp", label: "MCP 工具与资源" },
  steam: { kind: "feature", scope: "steam", label: "Steam 接入" },
  share: { kind: "feature", scope: "share", label: "分享 / 订阅" },
  network: { kind: "feature", scope: "network", label: "内容服务 / 分享服务" },
  props: { kind: "feature", scope: "props", label: "壁纸属性（WE props）" },
  quality: { kind: "feature", scope: "quality", label: "画质预设" },
  hotkeys: { kind: "feature", scope: "hotkeys", label: "快捷键" },
  update: { kind: "feature", scope: "update", label: "应用内更新" },
  library: { kind: "feature", scope: "library", label: "本地库" },
  media: { kind: "feature", scope: "media", label: "系统音频 / 正在播放" },
  db: { kind: "feature", scope: "db", label: "数据库" },
  security: { kind: "feature", scope: "security", label: "凭据 / 密钥" },
  // 内存/性能是基建：它总跟着别人的改动走（实测 fix(macos) 那条被误判成 perf 49% + wallpaper 41%）
  perf: { kind: "glue", scope: "perf", label: "内存 / 性能（集成面）" },
  render: { kind: "feature", scope: "render", label: "渲染器 / WE 兼容层" },
};

const UNKNOWN_DOMAIN = "core";

// ------------------------------------------------------------------ 阈值

// 判定"这批改动含多个功能"的三条路（都要 total ≥ minTotalChurn）：
//   duo    —— 两个功能域各占总量 40% 以上（各自 ≥ 40 行）：典型的"两件事各干各的"
//   spread —— 四个以上功能域各占 10% 以上（各自 ≥ 20 行）：撒得开
//   puddle —— 大改动（≥ 600 行）且没有任何一个域能主导（最大域 < 40%），
//             同时至少 3 个"实体域"（功能/集成面）各 ≥ 8%：在制分支一次性落库的典型形状
//
// 这三条是对着本仓库提交历史校准出来的（`--audit` 可复现），校准时的取舍：
//   要拦：97d964a / e45ffb3「在制品一次性落库」（12268 / 9244 行，ui 23% mcp 17% wallpaper 16% render 9%）
//   不拦：一个功能跨多个模块的正常提交 —— feat(playlist)（wallpaper 74% / hotkeys 10%）、
//         fix(mcp)（mcp 54% / network 29%）、perf(wallpaper)（network 60% / render 21% / wallpaper 15%）、
//         fix(macos) 的内存修复（perf 49% + wallpaper 41% —— perf 已按集成面处理）
//   也不拦：发布提交（版本号 + CHANGELOG 全是 meta，没有实体域）
const THRESHOLDS = {
  strict: {
    minTotalChurn: 60,
    coShare: 0.4,
    coMinChurn: 40,
    spreadShare: 0.1,
    spreadMinChurn: 20,
    spreadCount: 4,
    spreadMinTotal: 200,
    puddleMinTotal: 600,
    puddleTopShare: 0.4,
    puddleShare: 0.08,
    puddleCount: 3,
    groupShare: 0.08,
    groupMinChurn: 12,
  },
  loose: {
    minTotalChurn: 20,
    coShare: 0.12,
    coMinChurn: 12,
    spreadShare: 0.08,
    spreadMinChurn: 10,
    spreadCount: 3,
    spreadMinTotal: 60,
    puddleMinTotal: 200,
    puddleTopShare: 0.5,
    puddleShare: 0.05,
    puddleCount: 3,
    groupShare: 0.05,
    groupMinChurn: 8,
  },
};

/** 二进制文件在 numstat 里的行数是 `-`：给个名义权重，别让它把整组压没。 */
const BINARY_CHURN = 10;

const PLAN_FILE = "we-split-plan.json";
const PLAN_VERSION = 1;

// ------------------------------------------------------------------ 小工具

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const green = (s) => c("32", s);
const yellow = (s) => c("33", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

class UsageError extends Error {}

function git(args, opts = {}) {
  return execFileSync("git", args, {
    encoding: opts.encoding ?? "utf8",
    input: opts.input,
    stdio: opts.input ? ["pipe", "pipe", "pipe"] : ["ignore", "pipe", "pipe"],
    env: opts.env ?? process.env,
    maxBuffer: 64 * 1024 * 1024,
  });
}

function gitOut(args, opts = {}) {
  return git(args, opts).trim();
}

function gitTry(args, opts = {}) {
  try {
    return gitOut(args, opts);
  } catch {
    return null;
  }
}

/** 仓库根、.git 目录（worktree 下 .git 是文件，必须问 git 而不是拼路径）。 */
function locate() {
  const root = gitTry(["rev-parse", "--show-toplevel"]);
  if (!root) throw new UsageError("当前目录不是 git 仓库");
  const gitPath = (name) => {
    const p = gitOut(["rev-parse", "--git-path", name]);
    return path.isAbsolute(p) ? p : path.resolve(root, p);
  };
  return { root, gitPath };
}

function headSha() {
  return gitTry(["rev-parse", "--verify", "--quiet", "HEAD"]);
}

/** 拆分器自己创建的提交要让钩子放行，否则 pre-commit → 拆分 → 再 pre-commit 递归。 */
function childEnv(extra = {}) {
  return { ...process.env, WE_SPLIT_INTERNAL: "1", ...extra };
}

/**
 * 正在进行的 git 操作（合并 / 拣选 / 回滚 / 变基 / `merge --squash`）：此时这次提交
 * 是 git 主导的合并动作，边界不该由我们来质疑 —— 撞上就放行，不打扰。
 */
function inProgressOp(gitPath) {
  const marks = [
    ["MERGE_HEAD", "合并"],
    ["SQUASH_MSG", "合并（--squash）"],
    ["CHERRY_PICK_HEAD", "cherry-pick"],
    ["REVERT_HEAD", "revert"],
    ["rebase-merge", "变基"],
    ["rebase-apply", "变基"],
  ];
  for (const [name, label] of marks) {
    if (fs.existsSync(gitPath(name))) return label;
  }
  return null;
}

function hasUnmerged() {
  try {
    const out = gitOut(["diff", "--name-only", "--diff-filter=U"]);
    return out.length > 0;
  } catch {
    return false;
  }
}

// ------------------------------------------------------------------ 收集改动

/**
 * 收集一批改动。mode：
 *   staged   —— 暂存区（默认；钩子看的也是这个）
 *   worktree —— 工作区全部（含未暂存与未跟踪）
 *   rev      —— 历史提交或范围（只读分析用）
 * 返回 [{ path, status, added, deleted, churn, binary }]
 */
function collectChanges(mode, rev = null) {
  const numstatArgs =
    mode === "staged"
      ? ["diff", "--cached", "--numstat", "-z", "--no-renames"]
      : mode === "worktree"
        ? ["diff", "HEAD", "--numstat", "-z", "--no-renames"]
        : rev.includes("..")
          ? ["diff", "--numstat", "-z", "--no-renames", rev]
          : ["show", "--numstat", "-z", "--no-renames", "--format=", rev];

  const statusArgs =
    mode === "staged"
      ? ["diff", "--cached", "--name-status", "-z", "--no-renames"]
      : mode === "worktree"
        ? ["diff", "HEAD", "--name-status", "-z", "--no-renames"]
        : rev.includes("..")
          ? ["diff", "--name-status", "-z", "--no-renames", rev]
          : ["show", "--name-status", "-z", "--no-renames", "--format=", rev];

  const statusByPath = new Map();
  const statusOut = gitOut(statusArgs);
  const st = statusOut.length ? statusOut.split("\0") : [];
  for (let i = 0; i < st.length; ) {
    const code = st[i++];
    if (!code) continue;
    const p = st[i++];
    if (!p) continue;
    statusByPath.set(p, code[0]); // A / M / D / T
  }

  const changes = [];
  const numOut = gitOut(numstatArgs);
  const nt = numOut.length ? numOut.split("\0") : [];
  for (let i = 0; i < nt.length; ) {
    const rec = nt[i++];
    if (!rec) continue;
    const [a, d, ...rest] = rec.split("\t");
    const p = rest.join("\t");
    if (!p) continue;
    const binary = a === "-" || d === "-";
    const added = binary ? 0 : Number(a) || 0;
    const deleted = binary ? 0 : Number(d) || 0;
    changes.push({
      path: p,
      status: statusByPath.get(p) ?? "M",
      added,
      deleted,
      churn: binary ? BINARY_CHURN : added + deleted,
      binary,
    });
  }

  if (mode === "worktree") {
    // 未跟踪的新文件不在 diff 里：算进来（--apply 时也一并提交）
    const others = gitOut(["ls-files", "-z", "--others", "--exclude-standard"]);
    for (const p of others.length ? others.split("\0") : []) {
      if (!p) continue;
      let churn = BINARY_CHURN;
      try {
        const buf = fs.readFileSync(p);
        const text = buf.toString("utf8");
        churn = buf.includes(0) ? BINARY_CHURN : text.split("\n").length;
      } catch {
        /* 读不了就按名义权重算 */
      }
      changes.push({ path: p, status: "A", added: churn, deleted: 0, churn, binary: false });
    }
  }
  return changes;
}

// ------------------------------------------------------------------ 归域与判定

function resolveDomain(p) {
  for (const [re, id] of RULES) {
    if (re.test(p)) return { id, matched: true };
  }
  return { id: UNKNOWN_DOMAIN, matched: false };
}

/** 文件 → 域信息；把域表里的 role/scope/label 带上。 */
function classify(changes) {
  return changes.map((ch) => {
    const { id, matched } = resolveDomain(ch.path);
    const meta = DOMAINS[id] ?? DOMAINS[UNKNOWN_DOMAIN];
    return { ...ch, domain: id, domainKind: meta.kind, scope: meta.scope, label: meta.label, registered: matched };
  });
}

function sumChurn(files) {
  return files.reduce((s, f) => s + f.churn, 0);
}

/**
 * 归堆 + 判定。
 * 返回 { total, domains, groups, multi, reason, features, glue, meta }
 */
function analyze(changes, opts) {
  const th = opts.thresholds;
  const files = classify(changes);
  const total = sumChurn(files);

  const byDomain = new Map();
  for (const f of files) {
    if (!byDomain.has(f.domain)) {
      byDomain.set(f.domain, {
        id: f.domain,
        kind: f.domainKind,
        scope: f.scope,
        label: f.label,
        files: [],
        churn: 0,
      });
    }
    const d = byDomain.get(f.domain);
    d.files.push(f);
    d.churn += f.churn;
  }
  const domains = [...byDomain.values()].sort((a, b) => b.churn - a.churn || a.id.localeCompare(b.id));

  const features = domains.filter((d) => d.kind === "feature");
  const glue = domains.filter((d) => d.kind === "glue");
  const meta = domains.filter((d) => d.kind === "meta");

  const coMin = Math.max(th.coShare * total, th.coMinChurn);
  const spreadMin = Math.max(th.spreadShare * total, th.spreadMinChurn);
  const puddleMin = Math.max(th.puddleShare * total, th.puddleMinChurn ?? 0);
  const duoSet = features.filter((d) => d.churn >= coMin);
  const spreadSet = features.filter((d) => d.churn >= spreadMin);
  const puddleSet = domains.filter((d) => (d.kind === "feature" || d.kind === "glue") && d.churn >= puddleMin);
  const top1 = domains.length ? domains[0].churn / (total || 1) : 0;

  const duo = duoSet.length >= 2;
  const spread = total >= th.spreadMinTotal && spreadSet.length >= th.spreadCount;
  const puddle = total >= th.puddleMinTotal && top1 < th.puddleTopShare && puddleSet.length >= th.puddleCount;
  const multi = total >= th.minTotalChurn && (duo || spread || puddle);

  const reason = multi
    ? duo
      ? `${duoSet.map((d) => d.id).join(" / ")} 各占总改动的 ${Math.round((100 * coMin) / total)}% 以上`
      : spread
        ? `${spreadSet.length} 个功能域各占总改动的 ${Math.round((100 * spreadMin) / total)}% 以上`
        : `大改动（${total} 行）且没有单一主导：${puddleSet
            .slice(0, 4)
            .map((d) => `${d.id} ${pct(d.churn, total)}`)
            .join("、")}`
    : total < th.minTotalChurn
      ? `总改动 ${total} 行，规模太小，不值得拆`
      : `没有第二个够大的功能域（最大的几个：${features.slice(0, 3).map((d) => `${d.id} ${pct(d.churn, total)}`).join("、") || "无"}）`;

  const groups = buildGroups({ domains, features, glue, meta, total, th, multi });

  return { total, domains, features, glue, meta, groups, multi, reason, files };
}

const pct = (part, total) => (total ? `${Math.round((100 * part) / total)}%` : "0%");

function buildGroups({ features, glue, meta, total, th, multi }) {
  const groupMin = Math.max(th.groupShare * total, th.groupMinChurn);
  const groups = [];

  // 够大的功能域各自成组；其余的（含没登记的落点）并进最大的一组
  const big = features.filter((d) => d.churn >= groupMin);
  const small = features.filter((d) => d.churn < groupMin);
  for (const d of big) groups.push(newGroup(d));
  if (!groups.length && small.length) groups.push(newGroup(small[0]));

  const lead = groups[0];
  const leftovers = groups.length ? small : [];
  for (const d of [...leftovers, ...glue]) {
    if (!lead) {
      groups.push(newGroup(d));
      continue;
    }
    lead.files.push(...d.files);
    lead.churn += d.churn;
    lead.merged.push(d.id);
  }

  // meta：只有一个功能域时跟着它走（一次正常的改动 = 功能 + 词条 + CHANGELOG 一条提交）；
  // 确实要拆（multi）时单独收尾，避免把 B 功能的 CHANGELOG 条目塞进 A 的提交里。
  const docsMeta = meta.filter((d) => ["docs"].includes(d.id));
  const engMeta = meta.filter((d) => !docsMeta.includes(d));
  const metaGoesSeparate = multi && groups.length >= 1 && (docsMeta.length || engMeta.length);

  if (metaGoesSeparate) {
    if (engMeta.length) groups.push(newGroup(mergeDomains(engMeta, "eng")));
    if (docsMeta.length) groups.push(newGroup(mergeDomains(docsMeta, "docs")));
  } else {
    for (const d of [...engMeta, ...docsMeta]) {
      if (!groups.length) {
        groups.push(newGroup(d));
        continue;
      }
      groups[0].files.push(...d.files);
      groups[0].churn += d.churn;
      groups[0].merged.push(d.id);
    }
  }

  // 排序：先工程配置（新依赖要先落地），再功能域（大的在前），文档收尾
  const rank = (g) => (g.metaKind === "eng" ? 0 : g.metaKind === "docs" ? 2 : 1);
  groups.sort((a, b) => rank(a) - rank(b) || b.churn - a.churn || a.id.localeCompare(b.id));
  return groups;
}

function mergeDomains(domains, metaKind) {
  const id = metaKind === "docs" ? "docs" : domains.map((d) => d.id).join("+");
  return {
    id,
    kind: "meta",
    scope: domainScope(domains),
    label: metaKind === "docs" ? "文档 / CHANGELOG" : "工程配置（依赖 / 打包 / CI / 脚本）",
    files: domains.flatMap((d) => d.files),
    churn: domains.reduce((s, d) => s + d.churn, 0),
    metaKind,
  };
}

function domainScope(domains) {
  const ids = domains.map((d) => d.id);
  if (ids.length === 1) return DOMAINS[ids[0]].scope;
  if (ids.includes("ci")) return "ci";
  if (ids.includes("deps")) return "deps";
  if (ids.includes("build")) return "build";
  if (ids.includes("scripts")) return "scripts";
  return DOMAINS[ids[0]].scope;
}

function newGroup(domain) {
  return {
    id: domain.id,
    kind: domain.kind,
    scope: domain.scope,
    label: domain.label,
    metaKind: domain.metaKind ?? null,
    files: [...domain.files],
    churn: domain.churn,
    merged: [],
  };
}

// ------------------------------------------------------------------ 草案标题与正文

const TEST_PATH_RE = /(^|\/)(tests?|__tests__)\/|\.(test|spec)\.[a-z]+$|_test\.rs$|\.snap$/;

/** 类型推断：只做"读得出来的"那一半，理由会一起打出来，人可以在计划里改。 */
function inferType(group) {
  const files = group.files;
  const allDocs = files.every((f) => resolveDomain(f.path).id === "docs");
  if (allDocs) return { type: "docs", reason: "全是文档" };
  const allTests = files.every((f) => TEST_PATH_RE.test(f.path));
  if (allTests) return { type: "test", reason: "全是测试" };

  const domains = new Set(files.map((f) => resolveDomain(f.path).id));
  const sizeOnly = [...domains].every((d) => ["build", "deps"].includes(d));
  if (sizeOnly) return { type: "build", reason: "只动了打包/依赖" };
  if (domains.size === 1 && domains.has("ci")) return { type: "ci", reason: "只动了 CI" };
  if (domains.size === 1 && domains.has("scripts")) return { type: "chore", reason: "只动了脚本" };
  const allMeta = files.every((f) => DOMAINS[resolveDomain(f.path).id]?.kind === "meta");
  if (allMeta) return { type: "chore", reason: "只动了仓库杂务" };

  const added = files.some((f) => f.status === "A");
  if (added) return { type: "feat", reason: "含新增文件" };
  const deletedAll = files.every((f) => f.status === "D");
  if (deletedAll) return { type: "chore", reason: "全是删除" };
  const addLines = files.reduce((s, f) => s + f.added, 0);
  const delLines = files.reduce((s, f) => s + f.deleted, 0);
  if (addLines > delLines * 2) return { type: "feat", reason: "以新增为主" };
  if (delLines > addLines * 2) return { type: "chore", reason: "以删除/精简为主" };
  return { type: "fix", reason: "改动量相当（改动/修复都常见，按 fix 起草，请人工确认）" };
}

function mainFile(group) {
  const sorted = [...group.files].sort((a, b) => b.churn - a.churn || a.path.localeCompare(b.path));
  return path.basename(sorted[0].path);
}

function draftSubject(group) {
  const files = group.files;
  const head = mainFile(group);
  const tail = files.length > 1 ? ` 等 ${files.length} 个文件` : "";
  return `${head}${tail}（自动拆分草案，请改成"改了什么 + 什么条件下"）`;
}

function statusWord(status) {
  return { A: "增", D: "删", M: "改", T: "改" }[status] ?? "改";
}

function draftBody(group, ctx) {
  const lines = [];
  lines.push("## 改动");
  lines.push("");
  const shown = group.files.slice(0, 20);
  for (const f of shown) {
    const delta = f.binary ? "二进制" : `+${f.added} −${f.deleted}`;
    lines.push(`- \`${f.path}\`（${statusWord(f.status)}，${delta}）`);
  }
  if (group.files.length > shown.length) lines.push(`- …另有 ${group.files.length - shown.length} 个文件`);
  lines.push("");
  lines.push("## 验证");
  lines.push("");
  lines.push("未运行 —— 这条提交由 `pnpm commit:split` 按功能域拆出，拆分只做分组与提交，不跑测试。");
  lines.push("请把真实验证补在这里（例：`cargo test --lib` 249 passed / 3 failed，与基线逐条相同）。");
  if (ctx) {
    lines.push("");
    lines.push(
      `拆分来源：一次改动含 ${ctx.groupCount} 个功能域（${ctx.domainIds.join("、")}），本提交是其中之一；` +
        `原始改动内容 = 这 ${ctx.groupCount} 条提交之和。`,
    );
  }
  return lines.join("\n");
}

/** 给每条提交生成草案（类型 / 标题 / 正文）。钩子自动拆分与手工流程都要用。 */
function prepareGroups(analysis) {
  const ctx = { groupCount: analysis.groups.length, domainIds: analysis.groups.map((g) => g.scope) };
  for (const g of analysis.groups) {
    const t = inferType(g);
    g.type = t.type;
    g.typeReason = t.reason;
    g.subject = draftSubject(g);
    g.body = draftBody(g, analysis.groups.length > 1 ? ctx : null);
  }
  return analysis;
}

// ------------------------------------------------------------------ 展示

function domainTable(domains, total) {
  const rows = domains.map((d) => [
    d.id,
    `${d.churn}`,
    pct(d.churn, total),
    `${d.files.length}`,
    d.kind,
    d.label,
  ]);
  return renderTable(["域", "改动行", "占比", "文件", "角色", "说明"], rows);
}

function renderTable(head, rows) {
  const widths = head.map((h, i) => Math.max(stringWidth(h), ...rows.map((r) => stringWidth(r[i] ?? ""))));
  const line = (cells) => cells.map((cell, i) => pad(cell ?? "", widths[i])).join("  ");
  return [dim(line(head)), dim(widths.map((w) => "─".repeat(w)).join("  ")), ...rows.map((r) => line(r))].join("\n");
}

/** 中文按两格宽算，否则表格会歪。 */
function stringWidth(s) {
  let w = 0;
  for (const ch of String(s)) w += /[\u1100-\u115f\u2e80-\ua4cf\ua960-\ua97f\uac00-\ud7a3\uf900-\ufaff\ufe10-\ufe19\ufe30-\ufe6f\uff00-\uff60\uffe0-\uffe6]/.test(ch) ? 2 : 1;
  return w;
}

function pad(s, width) {
  return s + " ".repeat(Math.max(0, width - stringWidth(s)));
}

function renderGroup(g, index, total) {
  const out = [];
  out.push(`  ${bold(`${index + 1}. ${g.scope}`)}  ${dim(`（${g.label}${g.merged.length ? ` + 并入 ${g.merged.join("/")}` : ""}）`)}`);
  out.push(`     ${g.type ?? "?"}(${g.scope}): ${g.subject ?? "（未起草）"}`);
  const files = g.files.slice(0, 6).map((f) => f.path);
  for (const f of files) out.push(dim(`       · ${f}`));
  if (g.files.length > files.length) out.push(dim(`       · …另有 ${g.files.length - files.length} 个`));
  out.push(dim(`     改动 ${g.churn} 行 / ${g.files.length} 个文件，占比 ${pct(g.churn, total)}`));
  return out.join("\n");
}

function renderAnalysis(a, opts) {
  const out = [];
  out.push("");
  out.push(bold(`改动归堆（共 ${a.files.length} 个文件，${a.total} 行）`));
  out.push("");
  out.push(domainTable(a.domains, a.total));
  out.push("");
  if (a.multi) {
    const n = a.groups.filter((g) => g.kind !== "meta").length;
    out.push(`${yellow("!")} 判定：这批改动含 ${n} 个功能域 —— ${a.reason}`);
    out.push(dim("  按 docs/pr-rules.md §2：不相干的两件事分开发，回滚与 git log 才会值钱。"));
  } else {
    out.push(`${green("✓")} 判定：单一功能域，不必拆 —— ${a.reason}`);
  }
  out.push("");
  out.push(bold(`拆分方案（${a.groups.length} 条提交，按提交顺序）`));
  out.push("");
  a.groups.forEach((g, i) => {
    out.push(renderGroup(g, i, a.total));
    out.push("");
  });
  if (!opts.quiet && a.groups.length > 8) {
    out.push(yellow("! 组数偏多，可能是全仓整理/格式化：那种情况更适合单独一条提交（§5）。"));
    out.push("");
  }
  return out.join("\n");
}

// ------------------------------------------------------------------ 计划文件

function planPath(gitPath) {
  return gitPath(PLAN_FILE);
}

function buildPlan(analysis, opts, mode) {
  const ctx = {
    groupCount: analysis.groups.length,
    domainIds: analysis.groups.map((g) => g.scope),
  };
  return {
    version: PLAN_VERSION,
    createdAt: new Date().toISOString(),
    base: headSha() ?? "(无 HEAD)",
    mode,
    analysis: { total: analysis.total, multi: analysis.multi, reason: analysis.reason },
    _hint: [
      "标题与类型是给 git log 的人看的，请改成“改了什么 + 什么条件下行为如何”；",
      "改完跑：pnpm commit:split --apply（仍想直接用草案：加 --auto）；",
      "文件归属可以调整（把某个文件从这条挪到那条），但每条至少留一个文件；",
      "提交顺序 = 数组顺序。",
    ],
    groups: analysis.groups.map((g) => ({
      id: g.id,
      scope: g.scope,
      type: g.type,
      subject: g.subject,
      body: g.body,
      draft: true,
      files: g.files.map((f) => f.path),
    })),
  };
}

function writePlan(gitPath, plan) {
  const p = planPath(gitPath);
  fs.writeFileSync(p, JSON.stringify(plan, null, 2) + "\n");
  return p;
}

function readPlan(gitPath) {
  const p = planPath(gitPath);
  if (!fs.existsSync(p)) return null;
  try {
    return JSON.parse(fs.readFileSync(p, "utf8"));
  } catch (e) {
    throw new UsageError(`计划文件损坏（${p}）：${e.message}；删掉它重新跑一次即可`);
  }
}

// ------------------------------------------------------------------ 提交信息校验（复用同一份规则）

function lintMessage(message) {
  const script = path.join(SCRIPT_DIR, "commit-msg-lint.mjs");
  try {
    execFileSync(process.execPath, [script, "--message", message], { stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true };
  } catch (e) {
    const out = `${e.stdout ?? ""}${e.stderr ?? ""}`.toString().trim();
    return { ok: false, detail: out.split("\n").slice(0, 6).join("\n") };
  }
}

function fullMessage(group) {
  return `${group.type}(${group.scope}): ${group.subject}\n\n${group.body}\n`;
}

// ------------------------------------------------------------------ 拆分提交

/**
 * 用临时索引提交一组文件 —— 这样"你只暂存了文件的一部分"也能精确落进对应提交：
 *   新索引 = 当前 HEAD 的树 + 本组文件在暂存区里的内容
 * 真实索引与工作区始终不动（最后只做一次 reset --mixed 把索引对齐到新的 HEAD）。
 */
function commitGroup(group, { mode, stagedTree, noSign }) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "we-split-"));
  const idx = path.join(tmpDir, "index");
  const env = childEnv({ GIT_INDEX_FILE: idx });
  try {
    const head = headSha();
    const baseTree = head ? gitOut(["rev-parse", `${head}^{tree}`]) : gitOut(["hash-object", "-t", "tree", "/dev/null"]);
    git(["read-tree", baseTree], { env });

    const paths = group.files.map((f) => f.path);
    const entries = []; // 用 --index-info 一次喂进去：新增/修改都走它
    const removals = [];
    for (const p of paths) {
      const inStaged = mode === "staged" ? lsTree(stagedTree, p) : null;
      if (inStaged) {
        entries.push(`${inStaged.mode} ${inStaged.sha}\t${p}`);
        continue;
      }
      if (mode === "staged") {
        removals.push(p); // 暂存区里没有 = 这次是删除
        continue;
      }
      // worktree 模式：直接按工作区内容加进去
      git(["add", "--", p], { env, input: undefined });
    }
    if (entries.length) {
      git(["update-index", "-z", "--index-info"], { env, input: Buffer.from(entries.map((e) => `${e}\0`).join("")) });
    }
    if (removals.length) {
      git(["update-index", "-z", "--force-remove", "--stdin"], { env, input: Buffer.from(removals.map((p) => `${p}\0`).join("")) });
    }

    // 本组在这棵树上确实有变化才提交，否则 git 会以 "nothing to commit" 打断整个流程
    let unchanged = false;
    try {
      git(["diff-index", "--cached", "--quiet", baseTree, "--"], { env });
      unchanged = true; // 退出码 0 = 与基线树没有差异
    } catch {
      unchanged = false;
    }
    if (unchanged) return { committed: false };

    const msg = fullMessage(group);
    const args = ["commit", "--no-verify", "-m", msg];
    if (noSign) args.splice(1, 0, "-c", "commit.gpgsign=false");
    git(args, { env });
    return { committed: true, sha: gitOut(["rev-parse", "HEAD"]) };
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

function lsTree(tree, p) {
  const out = gitTry(["ls-tree", "-z", tree, "--", p]);
  if (!out) return null;
  const rec = out.split("\0").find(Boolean);
  if (!rec) return null;
  const [meta, ...rest] = rec.split("\t");
  const [mode, , sha] = meta.split(" ");
  return { mode, sha, path: rest.join("\t") };
}

function stagedTreeOf() {
  return gitOut(["write-tree"]);
}

function applyPlan(plan, opts) {
  const { gitPath, root } = locate();

  // —— 前置检查：任何一条不满足都不值得冒险改历史 ——
  const op = inProgressOp(gitPath);
  if (op) throw new UsageError(`正在${op}，拆分提交会打乱 git 自己的提交边界；先完成或放弃这次${op}`);

  const head = headSha();
  if (!head) throw new UsageError("仓库还没有任何提交（HEAD 未出生）；第一次提交先按原样提交，之后的批次再用拆分");
  if (plan.base !== head && !opts.force) {
    throw new UsageError(`HEAD 已经不是 ${String(plan.base).slice(0, 8)} 了（现在是 ${head.slice(0, 8)}）：改动集合变过，重跑一次分析更安全（--force 可强行按计划执行）`);
  }
  if (!gitTry(["symbolic-ref", "--quiet", "--short", "HEAD"]) && !opts.force) {
    throw new UsageError("当前处于游离 HEAD（detached），拆分出的提交会没有分支指向（--force 可强行执行）");
  }
  if (hasUnmerged()) throw new UsageError("索引里有未解决的冲突（unmerged），先解决冲突再拆");

  const mode = plan.mode === "worktree" ? "worktree" : "staged";
  const current = collectChanges(mode);
  const currentPaths = new Set(current.map((c) => c.path));
  const planPaths = new Set(plan.groups.flatMap((g) => g.files));
  const missing = [...planPaths].filter((p) => !currentPaths.has(p));
  const extra = [...currentPaths].filter((p) => !planPaths.has(p));
  if ((missing.length || extra.length) && !opts.force) {
    const lines = ["改动集合与计划不一致："];
    if (missing.length) lines.push(`  计划里有、现在没有：${missing.slice(0, 5).join("、")}${missing.length > 5 ? ` 等 ${missing.length} 个` : ""}`);
    if (extra.length) lines.push(`  现在有、计划里没有：${extra.slice(0, 5).join("、")}${extra.length > 5 ? ` 等 ${extra.length} 个` : ""}`);
    lines.push("  重新跑一次 `pnpm commit:split` 生成新计划（--force 可按旧计划强行提交）");
    throw new UsageError(lines.join("\n"));
  }

  // —— 信息门槛：草案标题要先改成真标题 ——
  const drafts = plan.groups.filter((g) => g.draft);
  if (drafts.length && !opts.auto) {
    throw new UsageError(
      [
        `${drafts.length} 条提交的标题还是自动草案，先改成"改了什么 + 什么条件下行为如何"：`,
        dim(`  ${planPath(gitPath)}`),
        "改完再跑 `pnpm commit:split --apply`；确实不想写就直接 `--auto`（标题会原样用草案）。",
      ].join("\n"),
    );
  }

  // —— 逐条提交 ——
  const stagedTree = mode === "staged" ? stagedTreeOf() : null;
  const before = head;
  const done = [];
  console.log("");
  console.log(bold(`按功能拆成 ${plan.groups.length} 条提交${mode === "worktree" ? "（含未暂存改动）" : ""}`));
  console.log("");
  for (const g of plan.groups) {
    if (!g.files?.length) throw new UsageError(`计划里有一条没有文件（id=${g.id}）`);
    const msg = fullMessage(g);
    const lint = lintMessage(msg);
    if (!lint.ok) {
      throw new UsageError(`第 ${done.length + 1} 条的提交信息不合规，已停下（前面 ${done.length} 条已提交）：\n${lint.detail}`);
    }
    const res = commitGroup({ ...g, files: g.files.map((p) => ({ path: p })) }, { mode, stagedTree, noSign: opts.noSign });
    if (!res.committed) {
      console.log(`  ${dim("跳过")} ${g.type}(${g.scope}): ${g.subject} ${dim("（这条在 HEAD 上已无变化）")}`);
      continue;
    }
    done.push({ ...g, sha: res.sha });
    console.log(`  ${green("✓")} ${res.sha.slice(0, 8)}  ${g.type}(${g.scope}): ${g.subject}`);
  }
  console.log("");

  // 索引对齐到新的 HEAD：不这么做，暂存区会停在拆分前的样子，status 里全是假的"已暂存"
  git(["reset", "-q", "--mixed", "HEAD"]);

  if (!done.length) {
    console.log(dim("没有任何一条真正提交（改动可能已经等于 HEAD）。"));
    return 0;
  }

  console.log(`${green("完成")}：${done.length} 条提交，索引已对齐到新的 HEAD。`);
  console.log(dim(`  回退：git reset --soft ${before.slice(0, 8)}   （回到拆分前，改动原样留在暂存区）`));
  console.log(dim("  下一步：按 §2 的习惯，PR/推送前跑一遍对应门禁（至少 cargo check --all-targets、pnpm typecheck）"));
  console.log("");
  return 0;
}

// ------------------------------------------------------------------ 钩子模式

function hookMode(opts) {
  if (process.env.WE_SPLIT_INTERNAL === "1") return 0; // 拆分器自己创建的提交
  if (process.env.WE_ALLOW_MIXED || process.env.WE_NO_SPLIT_CHECK) return 0;

  const { gitPath } = locate();
  const op = inProgressOp(gitPath);
  if (op) return 0; // 合并/拣选/变基：提交内容是 git 定的，不插手

  const changes = collectChanges("staged");
  if (!changes.length) return 0;

  const analysis = analyze(changes, opts);
  if (!analysis.multi) return 0;
  prepareGroups(analysis);

  const maxGroups = Number(process.env.WE_SPLIT_MAX_GROUPS ?? opts.maxGroups ?? 8) || 8;
  if (analysis.groups.length > maxGroups) {
    console.log(yellow(`! 这批改动横跨 ${analysis.groups.length} 个域（> ${maxGroups}），像是整理/格式化，本次不拦。`));
    console.log(dim("  真要拆：pnpm commit:split；不想再看到提示：WE_ALLOW_MIXED=1 git commit …"));
    return 0;
  }

  if (process.env.WE_AUTO_SPLIT) {
    console.log(bold("检测到多功能改动：按功能域自动拆分…"));
    try {
      const plan = buildPlan(analysis, opts, "staged");
      const rc = applyPlan(plan, { ...opts, force: false, auto: true });
      console.log("");
      console.log(`${green("已按功能拆成")} ${analysis.groups.length} 条提交${green("，本次 git commit 已取消")}`);
      console.log(dim("  （取消是为了避免再产生一条把同样内容又提交一遍的重复提交）"));
      console.log(dim("  想改标题：git rebase -i 或 git commit --amend 逐条改；想回退：见上面那行 reset --soft"));
      return rc === 0 ? 3 : 1;
    } catch (e) {
      console.log(red(`自动拆分失败：${e.message}`));
      console.log(dim("  本次提交仍被拦下，避免把多功能改动塞进一条。手工拆：pnpm commit:split"));
      return 3;
    }
  }

  // 默认：拦住 + 给出下一步（不替人写提交）
  console.log("");
  console.log(`${red("✗")} 这批改动跨 ${analysis.groups.length} 个功能域，建议拆成 ${analysis.groups.length} 条提交 ${dim("（规则：docs/pr-rules.md §2 提交粒度）")}`);
  console.log(dim(`  判定依据：${analysis.reason}`));
  console.log("");
  console.log(renderAnalysis(analysis, { quiet: true }));
  console.log("  下一步（任选）：");
  const rows = [
    ["pnpm commit:split", "看草案并写计划文件 → 改标题 → pnpm commit:split --apply"],
    ["pnpm commit:split --apply --auto", "不看计划，直接按草案拆成多条提交"],
    ["WE_ALLOW_MIXED=1 git commit …", "确实是一件事（如全仓整理）：本次放行"],
    ["git commit --no-verify", "绕过全部本地钩子（不推荐）"],
  ];
  const w = Math.max(...rows.map(([cmd]) => stringWidth(cmd)));
  for (const [cmd, desc] of rows) console.log(`    ${bold(pad(cmd, w))}  ${dim(desc)}`);
  console.log("");
  return 3;
}

// ------------------------------------------------------------------ range 模式

function rangeMode(rev, opts) {
  const changes = collectChanges("rev", rev);
  if (!changes.length) {
    if (!opts.json) console.log(dim(`${rev} 没有改动（或不是提交/范围）`));
    return 0;
  }
  const analysis = analyze(changes, opts);
  if (opts.json) {
    console.log(
      JSON.stringify(
        {
          rev,
          analysis: { total: analysis.total, multi: analysis.multi, reason: analysis.reason },
          domains: analysis.domains.map((d) => ({ id: d.id, kind: d.kind, churn: d.churn })),
        },
        null,
        2,
      ),
    );
    return 0;
  }
  prepareGroups(analysis);
  console.log(bold(`=== ${rev} ===`));
  if (analysis.multi) console.log(yellow("（按现行阈值：会被判定为多功能 → 钩子会拦）"));
  console.log(renderAnalysis(analysis, { quiet: true }));
  return 0;
}

/**
 * 拿历史自查阈值：逐条提交跑一遍判定，看有多少会被拦、都是些什么提交。
 * 阈值调了之后应该重跑它 —— 规则要能对着真实历史解释，不能靠感觉。
 */
function auditMode(revRange, opts) {
  const shas = gitOut(["log", "--no-merges", "--format=%H", revRange])
    .split("\n")
    .filter(Boolean);
  if (!shas.length) {
    console.log(dim(`${revRange} 内没有提交`));
    return 0;
  }
  const rows = [];
  let flagged = 0;
  let counted = 0;
  const flaggedList = [];
  for (const sha of shas) {
    const changes = collectChanges("rev", sha);
    if (!changes.length) continue;
    counted++;
    const a = analyze(changes, opts);
    const subj = gitOut(["show", "-s", "--format=%s", sha]);
    const feats = a.features
      .filter((d) => d.churn >= 10)
      .slice(0, 3)
      .map((d) => `${d.id} ${pct(d.churn, a.total)}`)
      .join("  ");
    if (a.multi) {
      flagged++;
      flaggedList.push(`  ${sha.slice(0, 8)}  ${subj.slice(0, 60)}\n      ${dim(a.reason)}`);
    }
    rows.push([sha.slice(0, 8), a.multi ? red("拦下") : dim("放行"), String(a.total), feats || dim("（无非集成面改动）"), subj.slice(0, 40)]);
  }
  console.log("");
  console.log(renderTable(["sha", "判定", "改动行", "主要功能域", "标题"], rows));
  console.log("");
  console.log(
    `${bold("汇总")}：${counted} 条提交（${opts.granularity} 档），判定为多功能 ${flagged} 条 —— ${pct(flagged, counted)}`,
  );
  if (flaggedList.length) {
    console.log("");
    console.log(bold("被拦下的："));
    console.log(flaggedList.join("\n"));
  }
  console.log("");
  return 0;
}

// ------------------------------------------------------------------ 入口

function usage() {
  const src = fs.readFileSync(path.join(SCRIPT_DIR, "split-commits.mjs"), "utf8");
  console.log(
    src
      .split("\n")
      .filter((l) => /^\/\/|^$/.test(l))
      .slice(0, 34)
      .join("\n")
      .replace(/^\/\/ ?/gm, ""),
  );
}

function parseArgs(argv) {
  const opts = {
    apply: false,
    auto: false,
    hook: false,
    worktree: false,
    force: false,
    json: false,
    quiet: false,
    noSign: false,
    explain: null,
    range: null,
    audit: null,
    granularity: "strict",
    maxGroups: 8,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--apply") opts.apply = true;
    else if (a === "--auto") opts.auto = true;
    else if (a === "--hook") opts.hook = true;
    else if (a === "--worktree") opts.worktree = true;
    else if (a === "--force") opts.force = true;
    else if (a === "--json") opts.json = true;
    else if (a === "--quiet") opts.quiet = true;
    else if (a === "--no-sign") opts.noSign = true;
    else if (a === "--explain") opts.explain = argv[++i];
    else if (a === "--range") opts.range = argv[++i];
    else if (a === "--audit") opts.audit = argv[++i];
    else if (a === "--granularity") opts.granularity = argv[++i];
    else if (a === "--max-groups") opts.maxGroups = Number(argv[++i]);
    else if (a === "--help" || a === "-h") opts.help = true;
    else if (a.startsWith("-")) throw new UsageError(`未知选项：${a}（--help 看用法）`);
    else throw new UsageError(`多余的参数：${a}（--help 看用法）`);
  }
  if (!THRESHOLDS[opts.granularity]) throw new UsageError(`--granularity 只能是 strict / loose`);
  opts.thresholds = THRESHOLDS[opts.granularity];
  return opts;
}

function main(argv) {
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (e) {
    console.error(red(`✗ ${e.message}`));
    return 1;
  }
  if (opts.help) {
    usage();
    return 0;
  }

  try {
    if (opts.explain) {
      const { id, matched } = resolveDomain(opts.explain);
      const d = DOMAINS[id];
      console.log(`${opts.explain} → ${bold(id)}（${d.kind}，scope=${d.scope}）${matched ? "" : yellow("  ← 未登记，按 core 兜底；建议补进 RULES")}`);
      return 0;
    }
    if (opts.hook) return hookMode(opts);
    if (opts.range) return rangeMode(opts.range, opts);
    if (opts.audit) return auditMode(opts.audit, opts);

    const { gitPath } = locate();
    const mode = opts.worktree ? "worktree" : "staged";
    const changes = collectChanges(mode);
    if (!changes.length) {
      console.log(dim(mode === "worktree" ? "工作区没有任何改动。" : "暂存区是空的（先 git add；想看未暂存的改动加 --worktree）。"));
      return 0;
    }

    const analysis = prepareGroups(analyze(changes, opts));

    if (opts.json) {
      console.log(
        JSON.stringify(
          {
            analysis: { total: analysis.total, multi: analysis.multi, reason: analysis.reason },
            domains: analysis.domains.map((d) => ({
              id: d.id,
              kind: d.kind,
              churn: d.churn,
              share: analysis.total ? Number((d.churn / analysis.total).toFixed(3)) : 0,
              files: d.files.length,
            })),
            groups: analysis.groups.map((g) => ({
              id: g.id,
              type: g.type,
              scope: g.scope,
              subject: g.subject,
              churn: g.churn,
              files: g.files.map((f) => f.path),
            })),
          },
          null,
          2,
        ),
      );
      return 0;
    }

    console.log(renderAnalysis(analysis, opts));

    if (opts.apply) {
      // 计划文件是"人写标题"的载体：--apply 必须用它（否则人改的标题会被重新起草覆盖）。
      // --auto 是"不看计划"的口子：没有可用计划时现场起草；有计划时仍优先用计划里写的。
      const stored = readPlan(gitPath);
      const usable = stored && stored.mode === mode;
      if (!usable && !opts.auto) {
        throw new UsageError(
          stored
            ? `计划文件是 ${stored.mode} 模式生成的，与本次（${mode}）不符：重跑一次 \`pnpm commit:split${mode === "worktree" ? " --worktree" : ""}\``
            : `还没有计划文件：先跑 \`pnpm commit:split\` 生成并写标题（不想写标题就加 --auto）`,
        );
      }
      const plan = usable ? stored : buildPlan(analysis, opts, mode);
      if (analysis.groups.length < 2 && !opts.auto && !usable) {
        console.log(dim("只有一组，无需拆分（--auto 也救不了：真的只有一件事）。"));
        return 0;
      }
      return applyPlan(plan, opts);
    }

    const p = writePlan(gitPath, buildPlan(analysis, opts, mode));
    console.log(bold("下一步"));
    console.log(`  1. 改标题（类型/scope/主题/正文都可以改）：${dim(path.relative(process.cwd(), p) || p)}`);
    console.log(`  2. 落库：${bold("pnpm commit:split --apply")}   ${dim("（不想改标题就直接 --apply --auto）")}`);
    console.log("");
    return 0;
  } catch (e) {
    if (e instanceof UsageError) {
      console.error(red(`✗ ${e.message}`));
      return 1;
    }
    // 钩子模式下"自己坏了"不该卡住提交：放行并把原因打出来
    console.error(red(`✗ 拆分检查自身出错：${e.stack ?? e.message}`));
    if (opts?.hook) {
      console.error(dim("  （未阻断本次提交；上面是脚本的 bug，不是你的改动有问题）"));
      return 0;
    }
    return 1;
  }
}

process.exit(main(process.argv.slice(2)));
