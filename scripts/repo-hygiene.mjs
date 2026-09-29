#!/usr/bin/env node
// §9「不许进仓库的东西」闸门 —— 这条规则此前只是文档里的一段字（`git add -f dist/`
// 或者塞个大二进制，没有任何东西会拦）。
//
// 检查两件事（规则文本见 docs/pr-rules.md §9，实现表在这里，改 §9 时两处同步）：
//   1. 违禁路径：构建产物 / 依赖目录 / 本机数据 / 工具工作目录 / 系统垃圾
//   2. 新增 > 5MB 的二进制必须走 Git LFS（.gitattributes 里登记过，`git check-attr
//      filter` 认得出来）
//
// 两个边界（都是实测出来的，别随手改宽）：
//   · 大文件**只查新增**：`promo/gifs/` 里躺着 4 个 5–16MB 的 GIF（§9 写"这条是预防性的"
//     时没发现它们）—— 全仓扫会当场变红。历史的不追究，**新来的必须拦**。
//   · 违禁路径**全仓也扫**：基线实测是干净的，扫全仓才抓得住被 --no-verify 绕过钩子推进来的。
//
// 用法：
//   node scripts/repo-hygiene.mjs                # 暂存区（pre-commit 默认）
//   node scripts/repo-hygiene.mjs --staged
//   node scripts/repo-hygiene.mjs --range A..B    # CI：这次推送/PR 改了哪些
//   node scripts/repo-hygiene.mjs --all           # CI：全仓追踪文件（只查路径，不查大小）
//   node scripts/repo-hygiene.mjs --help
// 退出码：0 干净　1 有违禁　2 用法错误

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const SCRIPT_DIR = path.dirname(new URL(import.meta.url).pathname);

/** 单文件上限（§9）。 */
export const MAX_SIZE = 5 * 1024 * 1024;

/**
 * 违禁路径（§9）。正则对**仓库内相对路径**匹配。
 * 注意顺序无所谓（各自独立），每条都要写清它是什么，报错时直接引用。
 */
export const FORBIDDEN = [
  [/(^|\/)dist\//, "构建产物 dist/（pnpm build 的输出）"],
  [/(^|\/)src-tauri\/target\//, "Rust 构建目录 target/"],
  [/(^|\/)src-tauri\/bundled\//, "打包中间产物 bundled/"],
  [/(^|\/)node_modules\//, "依赖目录 node_modules/"],
  [/(^|\/)\.pnpm-store\//, "pnpm 存储 .pnpm-store/"],
  [/(^|\/)\.cargo-home\//, "本机 cargo home .cargo-home/"],
  [/(^|\/)\.ui-shots\//, "工具工作目录 .ui-shots/"],
  [/(^|\/)\.v2c\//, "工具工作目录 .v2c/"],
  [/(^|\/)\.video_agent\//, "工具工作目录 .video_agent/"],
  [/(^|\/)\.zcode\//, "工具工作目录 .zcode/"],
  [/(^|\/)proto-video-loop\/clips\//, "proto-video-loop/clips/（生成物）"],
  [/(^|\/)\.git\//, ".git 目录本身"],
  [/\.DS_Store$/, "系统垃圾 .DS_Store"],
  [/\.bak$/, "备份文件 *.bak"],
  [/~$/, "编辑器残留 *~"],
  [/\.swp$/, "vim 交换文件 *.swp"],
  [/(^|\/)[^/]*\.db$/, "数据库文件 *.db"],
  [/(^|\/)credentials\.dat$/, "凭据 credentials.dat"],
  [/(^|\/)[^/]*(_rsa|id_rsa|\.pem|\.p12|\.pfx|\.key)$/, "私钥/证书类文件"],
];

export function forbiddenReason(p) {
  for (const [re, why] of FORBIDDEN) if (re.test(p)) return why;
  return null;
}

// ------------------------------------------------------------------ git 读取

const git = (args, opts = {}) =>
  execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 64 * 1024 * 1024, ...opts });

/** 暂存区里（A/M/C/T，**不含删除** —— 删掉 dist/x.js 是好事，不能算违禁）。 */
export function stagedEntries() {
  const out = git(["diff", "--cached", "--name-status", "--diff-filter=ACMT", "--no-renames", "-z"]);
  const parts = out.split("\0");
  const files = [];
  for (let i = 0; i < parts.length; ) {
    const code = parts[i++];
    if (!code) continue;
    const p = parts[i++];
    if (!p) continue;
    files.push({ status: code[0], path: p });
  }
  return files;
}

/** 某个范围内（A/M/C/T，同样不含删除）的路径。 */
export function rangeEntries(range) {
  const out = git(["diff", "--name-status", "--diff-filter=ACMT", "--no-renames", "-z", range]);
  const parts = out.split("\0");
  const files = [];
  for (let i = 0; i < parts.length; ) {
    const code = parts[i++];
    if (!code) continue;
    const p = parts[i++];
    if (!p) continue;
    files.push({ status: code[0], path: p });
  }
  return files;
}

/** 全仓追踪文件（无状态概念 → 用 M 表示，跳过"新增大文件"那条）。 */
export function allEntries() {
  return git(["ls-files", "-z"])
    .split("\0")
    .filter(Boolean)
    .map((p) => ({ status: "M", path: p }));
}

/** 暂存区里这个路径的 blob 大小（SHA + size 一次拿全）。 */
export function stagedSize(p) {
  const line = git(["ls-files", "-s", "--", p]).split("\n")[0];
  const sha = line.split(/\s+/)[1];
  if (!sha) return null;
  return Number(git(["cat-file", "-s", sha]).trim());
}

/** 已入 HEAD 的 blob 大小（range 模式用）。 */
export function headSize(p) {
  try {
    return Number(git(["cat-file", "-s", `HEAD:${p}`]).trim());
  } catch {
    return null;
  }
}

/** Git 是否把它当 LFS：`git check-attr filter` 认 .gitattributes，不要求装 lfs 客户端。 */
export function isLfsTracked(p) {
  const out = git(["check-attr", "filter", "--", p]).trim();
  return /:\s*filter:\s*lfs\s*$/.test(out);
}

// ------------------------------------------------------------------ 检查

export function checkEntries(files, { sizeOf }) {
  const violations = [];
  for (const f of files) {
    const why = forbiddenReason(f.path);
    if (why) {
      violations.push({ path: f.path, why: `违禁路径：${why}`, fix: `把它移出仓库，并确认 .gitignore / .zcodeignore 已覆盖（§9）` });
      continue;
    }
    if (f.status !== "A") continue; // 大文件只查新增（历史的 4 个 GIF 见文件头说明）
    const size = sizeOf(f.path);
    if (size !== null && size > MAX_SIZE && !isLfsTracked(f.path)) {
      const mb = (size / 1024 / 1024).toFixed(1);
      violations.push({
        path: f.path,
        why: `新增文件 ${mb}MB > 5MB，且没有走 Git LFS`,
        fix: `git lfs install && git lfs track "**/*.gif"（或该扩展名）→ 把 .gitattributes 一并提交；或压缩/改放仓库外`,
      });
    }
  }
  return violations;
}

// ------------------------------------------------------------------ CLI

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const green = (s) => c("32", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

function annotate(msg) {
  if (process.env.GITHUB_ACTIONS !== "true") return;
  console.log(`::error title=违禁文件（docs/pr-rules.md §9）::${msg.replace(/\r?\n/g, " ")}`);
}

function usage() {
  const src = fs.readFileSync(path.join(SCRIPT_DIR, "repo-hygiene.mjs"), "utf8");
  console.log(
    src
      .split("\n")
      .filter((l) => /^\/\/|^$/.test(l))
      .slice(0, 26)
      .join("\n")
      .replace(/^\/\/ ?/gm, ""),
  );
}

export function main(argv) {
  const opts = { mode: "staged", range: null };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--staged") opts.mode = "staged";
    else if (a === "--range") {
      opts.mode = "range";
      opts.range = argv[++i];
      if (!opts.range) return console.error(red("✗ --range 需要一个范围（如 origin/main~1..HEAD）")), 2;
    } else if (a === "--all") opts.mode = "all";
    else if (a === "--help" || a === "-h") return usage(), 0;
    else return console.error(red(`✗ 未知选项：${a}（--help 看用法）`)), 2;
  }

  const files =
    opts.mode === "staged" ? stagedEntries() : opts.mode === "range" ? rangeEntries(opts.range) : allEntries();
  const sizeOf = opts.mode === "staged" ? stagedSize : headSize;

  if (opts.mode !== "all" && !files.length) {
    console.log(dim(`（${opts.mode === "staged" ? "暂存区" : "范围 " + opts.range} 没有改动文件）`));
    return 0;
  }

  const label = opts.mode === "staged" ? "暂存区" : opts.mode === "range" ? `范围 ${opts.range}` : "全仓追踪文件";
  const violations = checkEntries(files, { sizeOf });
  if (!violations.length) {
    console.log(`${green("✓")} §9 卫生检查（${label}，${files.length} 个文件）：干净`);
    return 0;
  }
  console.log(`${red("✗")} §9 卫生检查（${label}）发现 ${violations.length} 处违禁：`);
  for (const v of violations) {
    console.log(`  ${red("✗")} ${v.path}`);
    console.log(`    ${v.why}`);
    console.log(`    ${dim(v.fix)}`);
    annotate(`${v.path} —— ${v.why}`);
  }
  console.log(dim("  规则见 docs/pr-rules.md §9；确有必要放进仓库的例外，先改规则文档再改实现。"));
  return 1;
}

const isDirectRun = process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isDirectRun) process.exit(main(process.argv.slice(2)));
