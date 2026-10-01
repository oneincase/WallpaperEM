#!/usr/bin/env node
// 发布收尾（release finish）—— 把「版本说明」写到用户真正看得到的两处：
//   1) GitHub Release 正文（下载页 / 通知）
//   2) 各平台 updater 清单的 notes 字段（应用内「设置 → 关于 → 软件更新 → 更新内容」）
//
// 为什么需要单这一步：三个 build workflow 生成清单时是「先读 Release 正文、再写进 notes」，
// 而 Release 是构建过程中由 softprops 建的、正文本来就没人写 —— 不补这一步，应用内更新弹窗
// 永远是空的（v1.1.0 / v2.0.0 实测如此，v2.1.0 是人工补的）。
//
// 说明文件的约定：docs/release-notes/<tag>.md
//   · 全文（**去掉** app-notes 区块）→ GitHub Release 正文，Markdown 原样
//   · <!-- app-notes:start --> … <!-- app-notes:end --> 之间 → 清单 notes，
//     应用内是 <pre> 纯文本渲染，别放 Markdown 标记
//   · 没有该区块时退化为「去掉 Markdown 标记的全文」（能跑，但建议显式写区块）
//   · 文件位置可用 --notes-file 覆盖（临时稿放 /tmp 也行）
//
// 幂等：重复跑只写入同样的内容。Release 资产走 CDN，公开地址可能晚几十秒才刷新 ——
// 脚本先按 API 的资产体积硬核对（不受缓存影响），再对公开地址重试核对内容（只提醒不阻断）。
//
// 用法：
//   pnpm release:finish --tag v2.1.0                 # 说明文件取 docs/release-notes/v2.1.0.md
//   node scripts/release-finish.mjs --tag v2.1.0 --dry-run
//   node scripts/release-finish.mjs --tag v2.1.0 --notes-file /tmp/notes.md
// 退出码：0 成功（可有提醒）　1 失败　2 用法错误

import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { isTag } from "./release-preflight.mjs";

const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(SCRIPT_DIR, "..");

export const APP_NOTES_START = "<!-- app-notes:start -->";
export const APP_NOTES_END = "<!-- app-notes:end -->";

/** 说明文件的默认位置：docs/release-notes/<tag>.md（约定见 docs/pr-rules.md §8）。 */
export function defaultNotesPath(root, tag) {
  return path.join(root, "docs", "release-notes", `${tag}.md`);
}

/** 命令字符串按空格拆参数，支持双引号包起来的路径（`--gh "node /tmp/stub.mjs"`）。 */
export function parseCommand(cmd) {
  return (String(cmd).match(/"[^"]*"|\S+/g) ?? []).map((s) => s.replace(/^"|"$/g, ""));
}

/**
 * 去掉 Markdown 标记，供应用内 <pre> 显示（应用内不能渲染 Markdown，标记会原样露出来）。
 * 只处理版本说明里真会用的那些：注释 / 标题 / 加粗斜体 / 行内代码 / 链接 / 引用 / 列表。
 */
export function stripMarkdown(text) {
  return text
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/^#{1,6}[ \t]+/gm, "")
    // 行首空白只吃空格与制表符：`\s` 会连上一行的换行一起吞掉，把段落间距吃掉
    .replace(/^[ \t]*>[ \t]?/gm, "")
    .replace(/^[ \t]*[-*+][ \t]+/gm, "· ")
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, url) => (/^https?:/i.test(url) ? `${label}（${url}）` : label))
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/__([^_]+)__/g, "$1")
    .replace(/~~([^~]+)~~/g, "$1")
    .replace(/(^|[\s（(])\*([^*\n]+)\*(?=[\s）)。，,、]|$)/gm, "$1$2")
    .replace(/`([^`\n]+)`/g, "$1")
    .replace(/[ \t]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

/**
 * 说明文件 → { body, appNotes }：body 给 Release 正文，appNotes 给清单 notes。
 * 有 start 没 end 时**抛错**（半截区块写不出可信的东西，宁可当场拦下）。
 */
export function renderNotes(text) {
  const start = text.indexOf(APP_NOTES_START);
  if (start === -1) {
    return { body: `${text.trim()}\n`, appNotes: `${stripMarkdown(text)}\n` };
  }
  const end = text.indexOf(APP_NOTES_END, start);
  if (end === -1) {
    throw new Error(`说明文件里有 ${APP_NOTES_START} 却没有 ${APP_NOTES_END} —— 区块必须成对`);
  }
  const appNotes = `${text.slice(start + APP_NOTES_START.length, end).trim()}\n`;
  const body = `${(text.slice(0, start) + text.slice(end + APP_NOTES_END.length))
    .replace(/\n{3,}/g, "\n\n")
    .trim()}\n`;
  return { body, appNotes };
}

/** 清单 notes 换成新的：只动 notes，缩进与行尾跟 gen-update-manifest.mjs 保持一致。 */
export function patchManifest(text, notes) {
  return `${JSON.stringify({ ...JSON.parse(text), notes }, null, 2)}\n`;
}

/** Release 资产里属于 updater 清单的那些（latest-<target>-<arch>.json），按名字排序。 */
export function pickManifestAssets(assets) {
  return (assets ?? [])
    .map((a) => (typeof a === "string" ? a : a?.name))
    .filter((name) => typeof name === "string" && /^latest-.*\.json$/.test(name))
    .sort();
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

const HELP = `发布收尾：把版本说明写进 Release 正文，并同步进各平台 updater 清单的 notes。

用法：
  node scripts/release-finish.mjs --tag vX.Y.Z [选项]

选项：
  --tag <vX.Y.Z>       要收尾的标签（不给时取 GITHUB_REF_NAME，得是 v* 形状）
  --notes-file <路径>  说明文件，默认 docs/release-notes/<tag>.md
  --repo <owner/name>  目标仓库，默认 $GITHUB_REPOSITORY，再不然问 gh
  --dry-run            只打印将要做什么，不写任何东西
  --skip-verify        跳过「公开地址内容核对」那一步（写完就走）
  --verify-attempts N  公开地址核对重试次数，默认 4
  --verify-delay-ms N  重试间隔毫秒，默认 8000
  --gh <命令>          换成别的 gh 命令（测试用）
  --root <目录>        仓库根，默认脚本所在的上一级

说明文件：全文（去掉 app-notes 区块）当 Release 正文；区块内纯文本当应用内
「更新内容」。没有区块时退化为去掉 Markdown 标记的全文。`;

function skeleton(notesPath, tag) {
  const ver = tag.replace(/^v/, "");
  const blob = `https://github.com/oneincase/WallpaperEM/blob/${tag}/CHANGELOG.md`;
  return `${notesPath} 不存在。新建它（约定见 docs/pr-rules.md §8）：

  ## WallpaperEM ${tag}

  本次重点：一句话说清这一版最值得升级的点。
  完整更新日志见 [CHANGELOG.md](${blob})。

  ### ✨ 主要新功能

  - **…**：…

  ### 🐛 主要修复

  - …

  ### 📦 下载

  - **macOS**：\`WallpaperEM_${ver}_universal.dmg\`（Apple Silicon + Intel）

  <!-- app-notes:start -->
  ${tag} 要点（完整日志见 CHANGELOG.md）

  · …
  · …
  <!-- app-notes:end -->
`;
}

// ------------------------------------------------------------------ 主流程

/** 反复问同一个问题，直到拿到 true 或用完次数（公开地址走 CDN，刷新生效要等一会儿）。 */
async function awaitUntil(fn, attempts, delayMs) {
  for (let i = 0; i < attempts; i++) {
    try {
      if (await fn()) return true;
    } catch {
      /* 网络抖动当一次失败，继续重试 */
    }
    if (i < attempts - 1) await new Promise((r) => setTimeout(r, delayMs));
  }
  return false;
}

export async function main(argv) {
  const opts = {
    tag: null,
    notesFile: null,
    repo: null,
    root: REPO_ROOT,
    gh: ["gh"],
    dryRun: false,
    skipVerify: false,
    verifyAttempts: 4,
    verifyDelayMs: 8000,
  };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--tag") opts.tag = argv[++i];
    else if (a === "--notes-file") opts.notesFile = argv[++i];
    else if (a === "--repo") opts.repo = argv[++i];
    else if (a === "--root") opts.root = path.resolve(argv[++i] ?? "");
    else if (a === "--gh") opts.gh = parseCommand(argv[++i] ?? "");
    else if (a === "--dry-run") opts.dryRun = true;
    else if (a === "--skip-verify") opts.skipVerify = true;
    else if (a === "--verify-attempts") opts.verifyAttempts = Number(argv[++i]);
    else if (a === "--verify-delay-ms") opts.verifyDelayMs = Number(argv[++i]);
    else if (a === "--help" || a === "-h") {
      console.log(HELP);
      return 0;
    } else {
      console.error(red(`✗ 未知选项：${a}（--help 看用法）`));
      return 2;
    }
  }

  const envTag = process.env.GITHUB_REF_NAME || null;
  const tag = opts.tag ?? (typeof envTag === "string" && envTag.startsWith("v") ? envTag : null);
  if (!tag) {
    console.error(red("✗ 得给 --tag vX.Y.Z（收尾的是已发布的 Release，不接受“当前分支”这种含糊输入）"));
    return 2;
  }
  if (!isTag(tag)) {
    console.error(red(`✗ ${tag} 不是 vX.Y.Z 形状（规则同 release-preflight.mjs）`));
    return 2;
  }
  if (opts.gh.length === 0) {
    console.error(red("✗ --gh 是空的"));
    return 2;
  }
  if (!Number.isFinite(opts.verifyAttempts) || opts.verifyAttempts < 1) {
    console.error(red("✗ --verify-attempts 得是正整数"));
    return 2;
  }

  const notesPath = opts.notesFile ? path.resolve(opts.notesFile) : defaultNotesPath(opts.root, tag);
  console.log(bold("发布收尾") + dim(`　tag=${tag}　notes=${notesPath}`));

  if (!fs.existsSync(notesPath)) {
    console.error(red(`✗ 说明文件不存在：${notesPath}\n`));
    console.error(skeleton(notesPath, tag));
    return 1;
  }

  let rendered;
  try {
    rendered = renderNotes(fs.readFileSync(notesPath, "utf8"));
  } catch (e) {
    console.error(red(`✗ ${e.message}`));
    return 1;
  }
  if (!rendered.body.trim()) {
    console.error(red("✗ 说明文件是空的（去掉 app-notes 区块后没有正文）"));
    return 1;
  }

  const fails = [];
  const warns = [];
  const note = (ok, label, detail) => {
    console.log(`  ${ok ? green("✓") : red("✗")} ${label.padEnd(24)} ${detail}`);
    if (!ok) fails.push(`${label}：${detail}`);
  };
  const warn = (label, detail) => {
    console.log(`  ${yellow("!")} ${label.padEnd(24)} ${detail}`);
    warns.push(`${label}：${detail}`);
  };

  const ver = tag.slice(1);
  if (!rendered.body.includes(ver)) {
    warn("正文提到版本号", `正文里没有 ${ver} —— 是不是拿错说明文件了？`);
  }
  if (stripMarkdown(rendered.appNotes) !== rendered.appNotes.trim()) {
    warn("应用内版本是纯文本", "app-notes 区块里还有 Markdown 标记（应用内按 <pre> 原样显示）");
  }

  // gh 的包装：所有外部调用都从这里走，方便测试注入假 gh
  const gh = (args) => {
    const [bin, ...pre] = opts.gh;
    const r = spawnSync(bin, [...pre, ...args], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    const out = `${r.stdout ?? ""}${r.stderr ?? ""}`.trim();
    return { ok: r.status === 0, out: r.error ? `${r.error.message}${out ? `\n${out}` : ""}` : out };
  };

  // 全程走 gh CLI：它跑不起来的话后面每一句报错都会指错方向，先单独问一声
  const probe = gh(["--version"]);
  if (!probe.ok) {
    console.error(red(`✗ gh 跑不起来：${probe.out.split("\n")[0] || "(无输出)"}`));
    console.error(dim("  收尾全程走 gh CLI —— 先装好并 gh auth login，再回来跑"));
    return 1;
  }

  // 0. 仓库：--repo → $GITHUB_REPOSITORY → 问 gh
  let repo = opts.repo ?? process.env.GITHUB_REPOSITORY ?? null;
  if (!repo) {
    const r = gh(["repo", "view", "--json", "nameWithOwner", "-q", ".nameWithOwner"]);
    if (!r.ok) {
      console.error(red(`✗ 问不出目标仓库（${r.out.split("\n")[0]}）—— 给 --repo owner/name`));
      return 1;
    }
    repo = r.out.split("\n").pop().trim();
  }
  const repoArgs = ["--repo", repo];
  console.log(dim(`  仓库 ${repo}`));

  // 1. Release 得已经存在（收尾是发布**之后**的事）
  const view = gh(["release", "view", tag, ...repoArgs, "--json", "body,assets,url"]);
  if (!view.ok) {
    const first = view.out.split("\n").find((l) => l.trim()) ?? "(无输出)";
    console.error(red(`✗ 读不到 Release ${tag}：${first}`));
    console.error(dim("  收尾发生在推 tag、三平台构建把 Release 建出来之后 —— 构建还没跑完就先等等"));
    note(false, `Release ${tag} 存在`, "读不到");
    return 1;
  }
  let release;
  try {
    release = JSON.parse(view.out.slice(view.out.indexOf("{")));
  } catch {
    console.error(red("✗ gh 返回的不是 JSON"));
    return 1;
  }
  const manifests = pickManifestAssets(release.assets);
  console.log(
    `  ${green("✓")} ${"Release 存在".padEnd(24)} ${dim(`${release.url ?? ""}　正文现有 ${(release.body ?? "").trim().length} 字符`)}`,
  );
  if (manifests.length === 0) {
    warn("updater 清单", "这个 Release 还没有 latest-*.json（三平台构建可能还没跑完）—— 稍后重跑本脚本即可");
  }

  const ghNote = (label, value) => `  ${dim("·")} ${label.padEnd(20)} ${value}`;
  console.log("");
  console.log(bold("将要写入"));
  console.log(ghNote("Release 正文", `${rendered.body.length} 字符`));
  console.log(ghNote("应用内「更新内容」", `${rendered.appNotes.trim().length} 字符`));
  console.log(ghNote("清单 notes", manifests.length ? `${manifests.length} 份：${manifests.join(" ")}` : "（还没有清单，跳过）"));

  if (opts.dryRun) {
    console.log(`\n${yellow("--dry-run")}：什么都没写。去掉它才会真的执行。`);
    return 0;
  }

  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "we-release-finish-"));
  try {
    // 2. 正文
    const bodyPath = path.join(tmp, "body.md");
    fs.writeFileSync(bodyPath, rendered.body);
    const edit = gh(["release", "edit", tag, ...repoArgs, "--notes-file", bodyPath]);
    note(edit.ok, "写 Release 正文", edit.ok ? `${rendered.body.length} 字符` : edit.out.split("\n")[0]);

    // 3. 清单：下载 → 只换 notes → 传回去
    const expectedSizes = new Map();
    if (manifests.length > 0) {
      const dir = path.join(tmp, "manifests");
      fs.mkdirSync(dir, { recursive: true });
      const dl = gh(["release", "download", tag, ...repoArgs, "-p", "latest-*.json", "-D", dir, "--clobber"]);
      const files = fs.existsSync(dir) ? fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort() : [];
      if (!dl.ok || files.length === 0) {
        note(false, "下载清单", `${dl.out.split("\n")[0] || "什么都没下到"}（期望 ${manifests.length} 份）`);
      } else {
        const patched = [];
        for (const f of files) {
          const p = path.join(dir, f);
          try {
            fs.writeFileSync(p, patchManifest(fs.readFileSync(p, "utf8"), rendered.appNotes));
            expectedSizes.set(f, fs.statSync(p).size);
            patched.push(p);
          } catch (e) {
            note(false, `补 ${f} 的 notes`, e.message);
          }
        }
        const up = gh(["release", "upload", tag, ...repoArgs, ...patched, "--clobber"]);
        note(up.ok && patched.length === files.length, "重传清单", up.ok ? `${patched.length} 份` : up.out.split("\n")[0]);
      }
    }

    // 4. 核对。先按 API 的资产体积硬核对（读的是 Release 元数据，不受 CDN 缓存影响），
    //    再对公开地址重试核对内容 —— 那一步只是为了把「CDN 还没刷新」跟「真没写进去」
    //    分开说清楚，所以只提醒不阻断。
    if (fails.length === 0 && expectedSizes.size > 0) {
      const after = gh(["release", "view", tag, ...repoArgs, "--json", "assets"]);
      let live = new Map();
      try {
        live = new Map(JSON.parse(after.out.slice(after.out.indexOf("{"))).assets.map((a) => [a.name, a.size]));
      } catch {
        /* 拿不到体积就跳过硬核对，不因此判失败 */
      }
      const mismatched = [...expectedSizes].filter(([name, size]) => live.has(name) && live.get(name) !== size).map(([n]) => n);
      if (mismatched.length > 0) note(false, "清单落地核对", `${mismatched.join(" ")} 的体积与本地不符`);
      else if (live.size === 0) warn("清单落地核对", "拿不到资产元数据，跳过（内容仍可稍后用 --dry-run 之外的公开地址自查）");
      else console.log(`  ${green("✓")} ${"清单落地核对".padEnd(24)} ${dim(`${expectedSizes.size} 份体积与本地一致`)}`);

      if (!opts.skipVerify) {
        const stale = [];
        for (const name of expectedSizes.keys()) {
          const ok = awaitUntil(
            async () => {
              const res = await fetch(`https://github.com/${repo}/releases/download/${tag}/${name}`, {
                headers: { "cache-control": "no-cache" },
              });
              if (!res.ok) return false;
              return (await res.text()).includes(rendered.appNotes.trim());
            },
            opts.verifyAttempts,
            opts.verifyDelayMs,
          );
          if (!ok) stale.push(name);
        }
        if (stale.length > 0) {
          warn("公开地址内容核对", `${stale.join(" ")} 还是旧内容 —— Release 资产走 CDN，通常一分钟内刷新，稍后自行复查即可`);
        } else {
          console.log(`  ${green("✓")} ${"公开地址内容核对".padEnd(24)} ${dim("下载页读到的就是新版说明")}`);
        }
      }
    }
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }

  if (warns.length) {
    console.log("");
    for (const w of warns) annotate("warning", "发布收尾（提醒）", w);
  }
  if (fails.length) {
    console.log("");
    for (const f of fails) annotate("error", "发布收尾（失败）", f);
    console.log(red(`✗ 有 ${fails.length} 项没写成 —— 修掉再跑一次（脚本是幂等的）`));
    return 1;
  }
  console.log(`\n${green("✓ 发布说明已就位")}${warns.length ? yellow(`（另有 ${warns.length} 条提醒）`) : ""} —— ${release.url ?? `releases/tag/${tag}`}`);
  return 0;
}

const isDirectRun = process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isDirectRun) process.exit(await main(process.argv.slice(2)));
