#!/usr/bin/env node
// 体积闸门 —— 「一个 PR 一件事」里机器判得了的那部分。
//
// 为什么要有它：docs/pr-rules.md 第 5 节把「有效改动 ≤ 400 行、≤ 15 个文件」定成软上限，
// 超了"要么拆，要么说明理由"。但"软"的执行力接近零 —— 一个 PR 里塞三件事，评审人只能
// 从头读到尾才发现，那时已经费掉一次注意力。机器判断不了"这是不是两件事"，但判断得了
// "大得不像一件事"，并且能逼出一句解释：超上限就必须在描述里写「体积说明」。
//
// 判定：
//   · 用 `git diff --numstat` 统计范围内的改动行（增 + 删）与文件数；
//   · 机械产物不计（与第 5 节一致）：pnpm-lock.yaml、dist/、图片/字体等二进制（截图不计）；
//   · 在上限内 → 通过；
//   · 超上限 → 描述里必须有体积说明：一个小节（标题含「体积 / 拆分 / 规模 / 上限」）或有
//     一行 `体积说明：…`，去掉空白与符号后 ≥ 20 字。没有就报错：要么拆，要么说明理由。
//
// 用法：
//   node scripts/pr-size-check.mjs --range origin/main..HEAD
//   node scripts/pr-size-check.mjs --range origin/main..HEAD --body-file /tmp/pr-body.md
//   node scripts/pr-size-check.mjs --range origin/main..HEAD --max-lines 400 --max-files 15
//
// 退出码：0 通过（含"超上限但已说明理由"）；1 超上限且没说明；2 用法/环境问题。

import { execFileSync } from "node:child_process";
import fs from "node:fs";

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const yellow = (s) => c("33", s);
const green = (s) => c("32", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

/** 机械产物不计入"有效改动"——与 docs/pr-rules.md 第 5 节同一份清单。 */
const MECHANICAL = [
  /(^|\/)pnpm-lock\.yaml$/,
  /(^|\/)dist\//,
  /\.(png|jpe?g|gif|webp|svg|ico|icns|pdf|mp4|mov|webm|woff2?|ttf|otf)$/i,
];

const isMechanical = (p) => MECHANICAL.some((re) => re.test(p));

/** 体积说明：小节标题含关键词，或一行 `体积说明：…`。 */
const JUSTIFY_HEADING = /体积|拆分|规模|上限/;
const JUSTIFY_INLINE = /^\s*(体积说明|拆分说明)\s*[:：]\s*(.*)$/m;
const JUSTIFY_MIN = 20; // 少于这个字数等于没说：`\s`、符号都不算

function bodyWithoutComments(raw) {
  return raw.replace(/<!--[\s\S]*?-->/g, "");
}

/** 找体积说明；返回它（用于打印）或 null。 */
function findJustification(raw) {
  const body = bodyWithoutComments(raw);
  const enough = (t) => t.replace(/[\s\-*|>]/g, "").length >= JUSTIFY_MIN;
  const lines = body.split(/\r?\n/);
  const headings = [];
  lines.forEach((line, i) => {
    const m = /^#{1,6}(?:\s+(.*?))?\s*$/.exec(line); // 与 commit-msg-lint 同一口径：`#` 后要有空格
    if (m) headings.push({ i, text: m[1] ?? "" });
  });
  for (let k = 0; k < headings.length; k++) {
    if (!JUSTIFY_HEADING.test(headings[k].text)) continue;
    const start = headings[k].i + 1;
    const end = k + 1 < headings.length ? headings[k + 1].i : lines.length;
    if (enough(lines.slice(start, end).join("\n"))) return `小节「${headings[k].text}」`;
    // 也有人把理由直接写在标题行上：`## 体积说明：全仓重命名 900 行属机械改动…`
    const tail = headings[k].text.split(/[:：]/).slice(1).join("：");
    if (tail && enough(tail)) return `标题行「${headings[k].text}」`;
  }
  const inline = JUSTIFY_INLINE.exec(body);
  if (inline && enough(inline[2])) return `「${inline[1]}」`;
  return null;
}

function numstat(range) {
  const out = execFileSync("git", ["diff", "--numstat", range], { encoding: "utf8" });
  const rows = [];
  for (const line of out.split("\n")) {
    if (!line.trim()) continue;
    const [added, deleted, ...rest] = line.split("\t");
    const path = rest.join("\t");
    if (added === "-" || deleted === "-") continue; // 二进制：行数无意义，直接跳过
    // 重命名会写成 `old => new`（或 `{a => b}/f`）：两侧任一命中机械清单就跳过
    const sides = path.split(" => ").map((s) => s.replace(/[{}]/g, "").trim());
    if (sides.some(isMechanical)) continue;
    rows.push({ path, added: Number(added), deleted: Number(deleted) });
  }
  return rows;
}

function main(argv) {
  const opts = { range: null, bodyFile: null, maxLines: 400, maxFiles: 15 };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--help" || a === "-h") {
      console.log(fs.readFileSync(new URL(import.meta.url), "utf8").split("\n").filter((l) => /^\/\/|^$/.test(l)).slice(0, 30).join("\n").replace(/^\/\/ ?/gm, ""));
      return 0;
    } else if (a === "--range") opts.range = argv[++i];
    else if (a === "--body-file") opts.bodyFile = argv[++i];
    else if (a === "--max-lines") opts.maxLines = Number(argv[++i]);
    else if (a === "--max-files") opts.maxFiles = Number(argv[++i]);
    else return console.error(`未知选项：${a}（--help 看用法）`), 2;
  }
  if (!opts.range) return console.error("缺少 --range（例如 --range origin/main..HEAD）"), 2;

  let rows;
  try {
    rows = numstat(opts.range);
  } catch (e) {
    // 取不到 diff 时报退出码 2：这是环境问题（浅克隆、ref 写错），别伪装成"体积不合格"
    console.error(red(`✗ 取不到 ${opts.range} 的 diff：${(e.stderr ?? e.message).toString().trim()}`));
    return 2;
  }

  const lines = rows.reduce((n, r) => n + r.added + r.deleted, 0);
  const files = rows.length;
  const over = lines > opts.maxLines || files > opts.maxFiles;

  const errors = [];
  const warns = [];
  const hints = [];
  if (over) {
    const justification = opts.bodyFile ? findJustification(fs.readFileSync(opts.bodyFile, "utf8")) : null;
    if (justification) {
      warns.push(`超上限，描述里已说明（${justification}）—— 记得这节要经得起"为什么没法拆"的追问`);
    } else if (!opts.bodyFile) {
      errors.push(`有效改动 ${lines} 行 / ${files} 文件，超过 ${opts.maxLines} 行 / ${opts.maxFiles} 文件的软上限；本次没传 --body-file，无法确认描述里写了理由`);
    } else {
      errors.push(
        `有效改动 ${lines} 行 / ${files} 文件，超过 ${opts.maxLines} 行 / ${opts.maxFiles} 文件的软上限：` +
          `要么拆成几个 PR（一个功能一个），要么在描述里写一节「体积说明」说清为什么没法拆（≥ ${JUSTIFY_MIN} 字）`,
      );
    }
  } else {
    hints.push(`有效改动 ${lines} 行 / ${files} 文件，符合「一个 PR 一件事」的规模`);
  }

  // 最大几个文件：一眼看出"大在哪"
  const top = [...rows].sort((a, b) => b.added + b.deleted - (a.added + a.deleted)).slice(0, 5);

  const mark = errors.length ? red("✗") : warns.length ? yellow("!") : green("✓");
  console.log(`${mark} ${bold("体积（一个 PR 一件事）")}  ${dim(opts.range)}`);
  console.log(`    有效改动：${green(`${lines} 行`)} / ${green(`${files} 文件`)}${dim(`（上限 ${opts.maxLines} 行 / ${opts.maxFiles} 文件）`)}`);
  for (const r of top) console.log(dim(`      +${r.added} -${r.deleted}  ${r.path}`));
  for (const e of errors) console.log(`    ${red("error")}  ${e}`);
  for (const w of warns) console.log(`    ${yellow("warn")}   ${w}`);
  for (const h of hints) console.log(`    ${dim("hint")}   ${h}`);
  return errors.length ? 1 : 0;
}

process.exit(main(process.argv.slice(2)));
