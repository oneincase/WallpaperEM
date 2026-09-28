#!/usr/bin/env node
// 提交信息 / PR 标题规则校验 —— 零依赖，本地 commit-msg 钩子与 CI 跑的是**同一份规则**。
//
// 规则全文见 docs/pr-rules.md。这里只做**能被机器判定**的部分：
//   头部形状（type(scope): 主题）、类型白名单、scope 拼写、主题是否空话、头部长度、
//   头部与正文之间的空行、正文行宽（中文行豁免）、PR 正文必备小节。
// 判定不了的部分（"验证证据是否真的验过"）留给 PR 模板的人工清单 —— 机器硬判只会
// 逼出"为了过检查而写"的假证据。
//
// 用法：
//   node scripts/commit-msg-lint.mjs <commit-msg 文件>          # 本地钩子（自动剥注释行）
//   node scripts/commit-msg-lint.mjs --message "feat(ui): …"    # 单条提交信息
//   node scripts/commit-msg-lint.mjs --title "feat(ui): …"      # PR 标题（只校验头部）
//   node scripts/commit-msg-lint.mjs --range origin/main..HEAD  # 批量校验一个范围
//   node scripts/commit-msg-lint.mjs --body-file pr-body.md --require-sections
//
// 选项：
//   --require-sections   校验 PR 正文是否含「改动」「验证」小节（配合 PR 模板用）
//   --strict-scope       未在建议清单里的 scope 也算错误（默认只是提示）
//   --max-header <n>     头部长度上限，默认 120（超 100 起警告）
//   --help
//
// 退出码：0 通过（允许有警告）/ 1 有错误 / 2 用法错误
//
// 豁免（不算不合规，直接跳过）：Merge / Revert / fixup! / squash! / Release 提交。
// 历史遗留提交（如 tag 时代的 `v0.4.0`、初始提交）不在校验范围内：CI 只校验 PR 范围。

import fs from "node:fs";
import { execFileSync } from "node:child_process";

// ------------------------------------------------------------------ 规则表

/** 允许的类型 → 中文释义。写在一起是为了报错时能直接把清单打给用户。 */
const TYPES = new Map([
  ["feat", "新功能"],
  ["fix", "修缺陷"],
  ["docs", "文档"],
  ["style", "格式（不改语义）"],
  ["refactor", "重构（行为不变）"],
  ["perf", "性能"],
  ["test", "测试"],
  ["build", "构建 / 依赖 / 打包"],
  ["ci", "CI / 工作流"],
  ["chore", "杂务（版本、脚本、清理）"],
  ["revert", "回滚"],
  ["release", "发布 / 版本标记"],
]);

/** 明确禁止的类型：给出替代写法，而不是干巴巴一句"类型不合法"。 */
const BANNED_TYPES = new Map([
  ["wip", "在制快照不进 main：请拆成可评审的提交，或放在 wip/ 分支上不开 PR"],
  ["del", "删除类改动按性质归入 chore / docs / refactor"],
  ["update", "请用具体类型：feat / fix / perf / chore …"],
  ["tmp", "临时提交请先 squash 掉"],
  ["temp", "临时提交请先 squash 掉"],
  ["misc", "请用具体类型，并写清改动范围"],
]);

/** scope 建议清单：来自本仓库既有提交史与代码布局，不是硬约束。 */
const SCOPE_HINTS = [
  "wallpaper", "playlist", "share", "workshop", "download", "render", "props",
  "apply", "hotkeys", "tray", "theme", "update", "db", "library", "network",
  "mcp", "steam", "i18n", "ui", "deps", "build", "bundle", "ci", "pr", "release",
  "scripts", "dev", "docs", "readme", "changelog", "windows", "macos", "linux",
];

/** 空话主题：读的人从标题里得不到任何信息。 */
const VAGUE_SUBJECT = [
  /^(更新|修改|优化|调整|修复|重构|完善|处理)(一下)?(代码|项目|工程|文件|内容|逻辑|细节)?$/u,
  /^(若干|一些|部分|多处)(修改|改动|更新|调整|修复)$/u,
  /^(update|fix|change|tweak|misc|stuff|wip|temp|minor|cleanup|improve)(\s+(code|stuff|things|files?))?$/i,
];

const HEADER_RE = /^(?<type>[A-Za-z]+)(?:\((?<scope>[^()]*)\))?(?<breaking>!)?:(?<rest>.*)$/u;
// scope 只对**结构性**问题报错（空括号、含空格/逗号等）；大小写与未知范围是风格问题，
// 降级成提示 —— 历史里出现过 `UI`、`macOS`、`渲染器`，那些不该被规则判为不合规。
const SCOPE_RE = /^[A-Za-z0-9\u4e00-\u9fff][A-Za-z0-9\u4e00-\u9fff._/-]*$/u;
const SCOPE_LOWER_RE = /^[a-z0-9\u4e00-\u9fff._/-]*$/u;

// ------------------------------------------------------------------ 工具

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const yellow = (s) => c("33", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

/** 中日韩字符占比 —— 用来豁免"中文长句必然超宽"的假报警。 */
function cjkRatio(line) {
  if (!line) return 0;
  const cjk = (line.match(/[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff]/gu) ?? []).length;
  return cjk / line.length;
}

/** 正文里不该按行宽判定的行：围栏代码、表格、缩进块、URL、脚注。 */
function isExemptBodyLine(line) {
  return (
    /^\s*$/.test(line) ||
    /^\s*(```|~~~|\||>|[-*+] |\d+\. )/.test(line) ||
    /^\s{2,}/.test(line) ||
    /https?:\/\/\S+/.test(line) ||
    /^(BREAKING CHANGE|Closes|Fixes|Refs|Co-authored-by|Signed-off-by):/i.test(line)
  );
}

/** 剥掉 commit-msg 文件里的注释行与 scissors 线（git 自己塞的）。 */
function stripCommitMsg(raw) {
  const lines = raw.split(/\r?\n/);
  const out = [];
  for (const line of lines) {
    if (/^#/.test(line)) continue;
    if (/^\s*-{2,}\s*>8\s*-{2,}/.test(line)) break;
    out.push(line);
  }
  // 去掉尾部空行
  while (out.length && out[out.length - 1].trim() === "") out.pop();
  return out.join("\n");
}

// ------------------------------------------------------------------ 校验

/** 豁免（不算不合规）：合并 / 回滚 / fixup 提交，以及发布提交。 */
function isExempt(header) {
  return /^(Merge |Revert |fixup!|squash!|Release )/i.test(header);
}

function lintHeader(header, opts) {
  const issues = [];
  const add = (level, msg) => issues.push({ level, msg });

  const m = HEADER_RE.exec(header);
  if (!m) {
    add("error", "头部不符合 `type(scope): 主题` 形状（示例：`fix(wallpaper): 换壁纸时不再闪黑`）");
    return issues;
  }
  const { type, scope, breaking, rest } = m.groups;

  if (type !== type.toLowerCase()) add("error", `类型必须全小写：\`${type}\` → \`${type.toLowerCase()}\``);
  const t = type.toLowerCase();

  if (BANNED_TYPES.has(t)) {
    add("error", `不允许的类型 \`${t}\`：${BANNED_TYPES.get(t)}`);
  } else if (!TYPES.has(t)) {
    add("error", `未知类型 \`${t}\`；允许：${[...TYPES.keys()].join(" / ")}`);
  }

  if (!rest.startsWith(" ")) {
    add("error", rest.length ? "类型/scope 后的冒号必须紧跟一个空格" : "冒号后没有主题");
  } else if (rest.trim() === "") {
    add("error", "主题为空：标题要能独立说明这次改动做了什么");
  }

  const subject = rest.trim();
  if (subject) {
    if (subject.length < 4 && !/[\u4e00-\u9fff]/u.test(subject)) {
      add("warn", `主题过短（\`${subject}\`），不足以说明改动`);
    }
    if (VAGUE_SUBJECT.some((re) => re.test(subject))) {
      add("error", `主题是空话（\`${subject}\`）：请写"改了什么 + 为什么/什么条件下"，例如 \`fix(playlist): 手动设单张后仍按轮播上下文取下一张\``);
    }
    if (/\.$/.test(subject)) add("error", "主题末尾不要句号");
    else if (/。$/.test(subject)) add("warn", "主题末尾的习惯是**不加**句号");
    if (subject.toLowerCase() === t) add("error", "主题不能只是类型名");
  }

  if (scope !== undefined) {
    if (!scope) add("error", "scope 括号是空的：要么写清范围，要么整个去掉");
    else if (/\s/.test(scope)) add("error", `scope 里不能有空格：\`${scope}\`（多级范围用 \`/\`，如 \`mcp/tools\`）`);
    else if (!SCOPE_RE.test(scope)) {
      add("error", `scope \`${scope}\` 含不支持的字符（只允许字母、数字、中文与 \`._/-\`）`);
    } else {
      if (!SCOPE_LOWER_RE.test(scope)) {
        add("hint", `scope \`${scope}\` 习惯上全小写（代码模块名都是小写，便于对应）`);
      }
      if (!SCOPE_HINTS.includes(scope.toLowerCase()) && !scope.includes("/")) {
        const hint = `scope \`${scope}\` 不在建议清单里`;
        if (opts.strictScope) add("error", `${hint}（--strict-scope 已开启）`);
        else if (/[\u4e00-\u9fff]/u.test(scope)) add("hint", `${hint}；建议用英文，便于对应代码模块`);
        else add("hint", `${hint}；建议用：${SCOPE_HINTS.join(" ")}`);
      }
    }
  }

  const maxHeader = opts.maxHeader;
  if (header.length > maxHeader) {
    add("error", `头部 ${header.length} 字符，超过上限 ${maxHeader}：把细节挪到正文`);
  } else if (header.length > 100) {
    add("warn", `头部 ${header.length} 字符偏长（建议 ≤ 100）：细节挪到正文更易读`);
  }

  if (breaking) add("hint", "这是破坏性改动：请在正文写 `BREAKING CHANGE: <影响与迁移方式>`");
  return issues;
}

function lintBody(bodyLines, headerLen, opts) {
  const issues = [];
  const add = (level, msg) => issues.push({ level, msg });
  if (!bodyLines.length) return issues;

  if (bodyLines[0].trim() !== "") {
    add("error", "头部与正文之间必须有空行（否则 `git log --oneline` 与各类工具会把正文当标题）");
  }

  let long = 0;
  let inFence = false;
  bodyLines.forEach((line, i) => {
    if (/^\s*(```|~~~)/.test(line)) {
      inFence = !inFence; // 围栏代码块（含内容）不参与行宽判定
      return;
    }
    if (inFence) return;
    if (isExemptBodyLine(line)) return;
    if (cjkRatio(line) > 0.3) return; // 中文长句没有断词点，行宽无意义
    const cap = opts.maxBodyLine;
    if (line.length > cap) {
      long++;
      if (long <= 3) add("warn", `正文第 ${i + 2} 行 ${line.length} 字符，超过 ${cap}（请手工折行）`);
    }
  });
  if (long > 3) add("warn", `正文另有 ${long - 3} 行超宽`);

  const text = bodyLines.join("\n");
  if (/^(see above|同上|见上)$/im.test(text)) {
    add("hint", "正文形同「见上」：评审人看不到上下文，请把结论写进正文");
  }
  return issues;
}

/** PR 正文必备小节（配合 .github/PULL_REQUEST_TEMPLATE.md）。 */
function lintPrSections(raw, opts) {
  const issues = [];
  const add = (level, msg) => issues.push({ level, msg });
  if (!opts.requireSections) return issues;

  // HTML 注释是模板里的填写指引，不算内容 —— 否则"用模板开了个空 PR"也能过检查
  const body = raw.replace(/<!--[\s\S]*?-->/g, "");
  if (!body.trim()) {
    add("error", "PR 描述为空：模板里的「改动」与「验证」是必填项");
    return issues;
  }

  const lines = body.split(/\r?\n/);
  const headings = [];
  lines.forEach((line, i) => {
    const m = /^#{1,6}\s*(.+?)\s*$/.exec(line);
    if (m) headings.push({ i, text: m[1] });
  });

  /** 取某小节标题下的正文；返回 null 表示没有这个小节。 */
  const section = (re) => {
    const k = headings.findIndex((h) => re.test(h.text));
    if (k === -1) return null;
    const start = headings[k].i + 1;
    const end = k + 1 < headings.length ? headings[k + 1].i : lines.length;
    return lines.slice(start, end).join("\n").trim();
  };

  const check = (re, label, min, level) => {
    const text = section(re);
    if (text === null) {
      add(level, `缺少「${label}」小节`);
    } else if (text.replace(/[\s\-*|]/g, "").length < min) {
      add(level, `「${label}」小节是空的：模板占位符不算填`);
    }
    return text;
  };

  check(/改动|变更|做了什么|What changed/i, "改动", 8, "error");
  check(/验证|测试|如何验证|验证证据|Verification/i, "验证", 8, "error");
  check(/背景|动机|为什么|问题|Why|Context/i, "背景 / 动机", 8, "warn");

  if (/^\s*[-*]\s*\[ \]/m.test(body)) {
    add("warn", "自检清单里还有未勾选项（`- [ ]`）");
  }
  return issues;
}

// ------------------------------------------------------------------ 输出

function renderReport(title, header, issues) {
  const errors = issues.filter((i) => i.level === "error");
  const warns = issues.filter((i) => i.level === "warn");
  const hints = issues.filter((i) => i.level === "hint");
  const lines = [];
  const mark = errors.length ? red("✗") : warns.length ? yellow("!") : c("32", "✓");
  lines.push(`${mark} ${bold(title)}`);
  if (header) lines.push(dim(`    ${header.length > 110 ? header.slice(0, 110) + " …" : header}`));
  for (const i of errors) lines.push(`    ${red("error")}  ${i.msg}`);
  for (const i of warns) lines.push(`    ${yellow("warn")}   ${i.msg}`);
  for (const i of hints) lines.push(`    ${dim("hint")}   ${i.msg}`);
  console.log(lines.join("\n"));
  return errors.length;
}

// ------------------------------------------------------------------ 入口

function usage() {
  const src = fs.readFileSync(new URL(import.meta.url), "utf8");
  const help = src.split("\n").filter((l) => /^\/\/|^$/.test(l)).slice(0, 30).join("\n").replace(/^\/\/ ?/gm, "");
  console.log(help);
}

function main(argv) {
  const opts = { maxHeader: 120, maxBodyLine: 120, strictScope: false, requireSections: false };
  let message = null;
  let headerOnly = false;
  let range = null;
  let bodyFile = null;
  let file = null;

  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--help" || a === "-h") {
      usage();
      return 0;
    } else if (a === "--message") message = argv[++i];
    else if (a === "--title") {
      message = argv[++i];
      headerOnly = true;
    } else if (a === "--range") range = argv[++i];
    else if (a === "--body-file") bodyFile = argv[++i];
    else if (a === "--require-sections") opts.requireSections = true;
    else if (a === "--strict-scope") opts.strictScope = true;
    else if (a === "--max-header") opts.maxHeader = Number(argv[++i]);
    else if (a.startsWith("-")) return console.error(`未知选项：${a}（--help 看用法）`), 2;
    else file = a;
  }

  // 模式一：PR 正文单独校验
  if (bodyFile) {
    const body = fs.readFileSync(bodyFile, "utf8");
    const errs = renderReport("PR 描述", null, lintPrSections(body, opts));
    return errs ? 1 : 0;
  }

  // 模式二：单条信息 / 标题
  if (message != null) {
    const lines = message.replace(/\r/g, "").split("\n");
    const header = (lines[0] ?? "").trim();
    if (isExempt(header)) return console.log(dim(`跳过豁免提交：${header}`)), 0;
    let issues = lintHeader(header, opts);
    if (!headerOnly) issues = issues.concat(lintBody(lines.slice(1), header.length, opts));
    const label = headerOnly ? "PR 标题" : "提交信息";
    return renderReport(label, header, issues) ? 1 : 0;
  }

  // 模式三：范围批量
  if (range) {
    const shas = execFileSync("git", ["log", "--no-merges", "--format=%H", range], { encoding: "utf8" })
      .trim()
      .split("\n")
      .filter(Boolean);
    if (!shas.length) return console.log(dim(`范围 ${range} 内没有提交，跳过`)), 0;
    let failed = 0;
    let skipped = 0;
    for (const sha of shas) {
      const raw = execFileSync("git", ["show", "-s", "--format=%B", sha], { encoding: "utf8" });
      const clean = stripCommitMsg(raw);
      const lines = clean.split("\n");
      const header = (lines[0] ?? "").trim();
      if (isExempt(header)) {
        skipped++;
        continue;
      }
      const issues = lintHeader(header, opts).concat(lintBody(lines.slice(1), header.length, opts));
      failed += renderReport(`提交 ${sha.slice(0, 8)}`, header, issues);
    }
    console.log(
      dim(
        `\n范围 ${range}：${shas.length} 个提交，${skipped} 个豁免，${failed ? red(`${failed} 个不合规`) : c("32", "全部合规")}`,
      ),
    );
    return failed ? 1 : 0;
  }

  // 模式四：commit-msg 钩子传入的文件
  if (file) {
    const clean = stripCommitMsg(fs.readFileSync(file, "utf8"));
    const lines = clean.split("\n");
    const header = (lines[0] ?? "").trim();
    if (!header) {
      // 空提交信息：git 自己也会拒绝；这里不静默失败，给一句人话
      console.log(yellow("! 提交信息为空"));
      return 1;
    }
    if (isExempt(header)) return 0;
    const issues = lintHeader(header, opts).concat(lintBody(lines.slice(1), header.length, opts));
    const errs = renderReport("提交信息", header, issues);
    if (errs) {
      console.log(dim("\n规则见 docs/pr-rules.md。修正后重试；确有必要绕过：`git commit --no-verify`（CI 仍会拦）"));
    }
    return errs ? 1 : 0;
  }

  usage();
  return 2;
}

process.exit(main(process.argv.slice(2)));
