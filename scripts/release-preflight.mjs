#!/usr/bin/env node
// 发布前置检查（release preflight）—— 三平台出包之前先花几秒确认"这次要发的东西是对的"。
//
// 为什么值得单独一个 job：三个 build workflow 是推 v* 标签直接开建、跑在 macos-14 /
// ubuntu / windows 上，一跑几十分钟。版本号漏改一处、CHANGELOG 忘了把 [Unreleased]
// 归段，这些事要等几十分钟后才在产物里现形（历史上 bin-info.plist 就停在 1.1.0 发过一版）。
// 前置检查几秒就能拦住，且**在最便宜的 runner 上拦**。
//
// 检查项（严格度按"离正式发布有多远"分级）：
//   · 版本一致            总是阻断 —— 三处权威版本必须相同（委托给 check-versions.mjs，
//                         规则只有那一份，这里不重写）
//   · tag 形状            有 tag 才查：vX.Y.Z（允许 -预发布后缀）
//   · tag == 版本         有 tag 才查：推的标签必须等于三处版本
//   · CHANGELOG 有该版本的段落   有 tag 时**阻断**；手动触发（彩排出包）时只警告
//   · [Unreleased] 还剩条目       只警告：可能是"归段后又落了新改动"，不一定是忘了归
//
// 触发方式：
//   · 由 release-preflight.yml（workflow_call）被三个 build workflow 引用，`needs:` 它
//   · 本地/手动：node scripts/release-preflight.mjs [--tag v2.0.0] [--root DIR]
//
// 退出码：0 全部通过（允许有警告）　1 有阻断项　2 用法错误

import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const SCRIPT_DIR = path.dirname(new URL(import.meta.url).pathname);

/** tag 形状：v 后跟三段语义化版本，允许 `-预发布` 后缀（v2.0.0-beta.1）。点号后缀（v2.0.0.1）不算。 */
export const TAG_RE = /^v\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/;

export function isTag(ref) {
  return typeof ref === "string" && TAG_RE.test(ref);
}

/** CHANGELOG 里有没有 `## [<tag>]` 段落（标题后面允许跟日期与备注：`## [v2.0.0] - 2026-09-27`）。 */
export function hasChangelogSection(text, tag) {
  const esc = tag.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`^## \\[${esc}\\]`, "m").test(text);
}

/** `[Unreleased]` 段落里还剩几条条目（HTML 注释不算 —— 模板注释里也有横杠）。 */
export function unreleasedItems(text) {
  const clean = text.replace(/<!--[\s\S]*?-->/g, "");
  const m = /^## \[Unreleased\]/m.exec(clean);
  if (!m) return 0;
  const rest = clean.slice(m.index + m[0].length);
  const next = rest.search(/^## \[/m);
  const section = next === -1 ? rest : rest.slice(0, next);
  return (section.match(/^\s*[-*] /gm) ?? []).length;
}

/** 三处权威版本是否一致：直接问 check-versions.mjs（唯一真源），不自己重写规则。 */
export function runVersionCheck(root) {
  const r = spawnSync(process.execPath, [path.join(SCRIPT_DIR, "check-versions.mjs"), "--root", root], {
    encoding: "utf8",
  });
  return { ok: r.status === 0, out: `${r.stdout ?? ""}${r.stderr ?? ""}`.trim() };
}

function packageVersion(root) {
  try {
    return JSON.parse(fs.readFileSync(path.join(root, "package.json"), "utf8")).version ?? null;
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ 输出

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const green = (s) => c("32", s);
const yellow = (s) => c("33", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

function annotate(level, title, msg) {
  if (process.env.GITHUB_ACTIONS !== "true") return;
  console.log(`::${level} title=${title}::${String(msg).replace(/\r?\n/g, " ")}`);
}

export function main(argv) {
  const opts = { tag: null, root: path.resolve(SCRIPT_DIR, "..") };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--tag") opts.tag = argv[++i];
    else if (a === "--root") opts.root = path.resolve(argv[++i] ?? "");
    else if (a === "--help" || a === "-h") {
      const src = fs.readFileSync(path.join(SCRIPT_DIR, "release-preflight.mjs"), "utf8");
      console.log(
        src
          .split("\n")
          .filter((l) => /^\/\/|^$/.test(l))
          .slice(0, 22)
          .join("\n")
          .replace(/^\/\/ ?/gm, ""),
      );
      return 0;
    } else {
      console.error(red(`✗ 未知选项：${a}（--help 看用法）`));
      return 2;
    }
  }

  // 没显式给 tag 就看环境：tag 推送时 GITHUB_REF_NAME 就是标签名；手动触发时它是分支名。
  // 严格模式 = 显式 --tag，或环境 ref 看起来就是个 tag（`v` 开头 —— workflow 的
  // `push: tags: v*` 匹配到 `v2.0.0.1` 这种歪标签时必须拦住，不能悄悄当彩排放过去）。
  const ref = opts.tag ?? (process.env.GITHUB_REF_NAME || null);
  const looksLikeTag = typeof ref === "string" && ref.startsWith("v");
  const strict = Boolean(opts.tag) || looksLikeTag;
  const onTag = strict && isTag(ref);
  const changelogPath = path.join(opts.root, "CHANGELOG.md");
  const changelog = fs.existsSync(changelogPath) ? fs.readFileSync(changelogPath, "utf8") : null;
  const ver = packageVersion(opts.root);

  console.log(bold(`发布前置检查`) + dim(`　ref=${ref ?? "(无 tag，彩排模式)"}　root=${opts.root}`));

  const fails = [];
  const warns = [];
  const note = (ok, label, detail) => {
    const mark = ok ? green("✓") : red("✗");
    console.log(`  ${mark} ${label.padEnd(26)} ${detail}`);
    if (!ok) fails.push(`${label}：${detail}`);
  };
  const warn = (label, detail) => {
    console.log(`  ${yellow("!")} ${label.padEnd(26)} ${detail}`);
    warns.push(`${label}：${detail}`);
  };

  // 1. 版本一致（总是阻断）
  const v = runVersionCheck(opts.root);
  if (v.ok) console.log(`  ${green("✓")} ${"版本一致（三处权威）".padEnd(26)} ${dim("package.json / tauri.conf.json / Cargo.toml")}`);
  else {
    note(false, "版本一致（三处权威）", v.out.split("\n").slice(-2).join(" / "));
  }

  // 2. tag 形状（严格模式下必须合法；彩排模式下 ref 是分支，压根不查）
  if (strict) {
    note(isTag(ref), "tag 形状", isTag(ref) ? `${ref} ✓` : `${ref} 不是 vX.Y.Z —— 发布标签必须是三段语义化版本`);
  }

  // 3. tag == 版本（形状对了才比，否则比出来也是噪音）
  if (onTag) {
    const tagVer = ref.slice(1);
    note(tagVer === ver, "tag == 版本", tagVer === ver ? `${ref} = ${ver}` : `推的是 ${ref}，但三处版本是 ${ver} —— 发错号了`);
  }

  // 4. CHANGELOG 段落：严格模式阻断；彩排模式（分支 ref）只提醒 —— 归段本来就是
  //    发布提交那一步做的事，彩排时还没归是正常的。
  if (changelog === null) {
    if (strict) note(false, "CHANGELOG.md 存在", "文件缺失");
  } else if (onTag) {
    const ok = hasChangelogSection(changelog, ref);
    note(ok, `CHANGELOG 有 ${ref} 段`, ok ? "已归段" : `没找到 \`## [${ref}]\` —— 发布前把 [Unreleased] 归到该段`);
  } else if (ver && !hasChangelogSection(changelog, `v${ver}`)) {
    warn(`CHANGELOG 有 v${ver} 段`, "还没归段（彩排模式只提醒，推 tag 时会变成阻断）");
  }

  // 5. Unreleased 剩余条目（只警告）
  if (changelog !== null) {
    const n = unreleasedItems(changelog);
    if (n > 0) warn("[Unreleased] 剩余条目", `${n} 条 —— 是归段时忘了，还是归段之后又落了新改动？`);
  }

  if (warns.length) {
    console.log("");
    for (const w of warns) annotate("warning", "发布前置检查（提醒）", w);
  }
  if (fails.length) {
    console.log("");
    for (const f of fails) annotate("error", "发布前置检查（阻断）", f);
    console.log(red(`✗ 没通过 ${fails.length} 项 —— 别急着打 tag，先修（规则见 docs/pr-rules.md §8）`));
    return 1;
  }
  console.log(`\n${green("✓ 发布前置全过")}${warns.length ? yellow(`（另有 ${warns.length} 条提醒）`) : ""} —— ${onTag ? "可以出包了" : "彩排模式：推 v* 标签才会真正发布"}`);
  return 0;
}

const isDirectRun = process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isDirectRun) process.exit(main(process.argv.slice(2)));
