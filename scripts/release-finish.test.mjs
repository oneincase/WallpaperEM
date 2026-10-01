#!/usr/bin/env node
// release-finish.mjs 的测试：纯函数直接测，CLI 端到端用**假 gh**（--gh 指到临时目录里的
// stub 脚本）跑 —— 不联网、不碰真仓库，stub 把「写进去的东西」留在临时目录里供断言。
//
// 跑法：node --test scripts/*.test.mjs   （package.json: pnpm test:scripts）

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import {
  APP_NOTES_START,
  APP_NOTES_END,
  defaultNotesPath,
  parseCommand,
  patchManifest,
  pickManifestAssets,
  renderNotes,
  stripMarkdown,
} from "./release-finish.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(HERE, "release-finish.mjs");

// ------------------------------------------------------------------ 纯函数

const NOTES = `## WallpaperEM v2.0.0

本次重点：多屏轮播。
完整更新日志见 [CHANGELOG.md](https://github.com/o/r/blob/v2.0.0/CHANGELOG.md)。

### ✨ 主要新功能

- **每屏独立**：手动设过的屏会被钉住。

<!-- app-notes:start -->
v2.0.0 要点（完整日志见 CHANGELOG.md）

· 多显示器每屏独立设置
<!-- app-notes:end -->
`;

test("stripMarkdown：标题 / 加粗 / 行内代码 / 链接 / 引用 / 列表 都能退成纯文本", () => {
  const out = stripMarkdown(
    "## 标题\n\n> 引用一行\n\n- **粗**与`码`\n- 看 [日志](https://e.invalid/x)\n- 看 [本地](docs/a.md)\n<!-- 注释 -->\n",
  );
  assert.match(out, /^标题/m);
  assert.match(out, /^· 粗与码$/m);
  assert.match(out, /· 看 日志（https:\/\/e\.invalid\/x）/);
  assert.match(out, /· 看 本地$/m, "相对链接只留文字");
  assert.doesNotMatch(out, /#|>|\*\*|`|<!--/);
});

test("renderNotes：区块给应用内、其余给正文；没区块时退化成去标记的全文", () => {
  const { body, appNotes } = renderNotes(NOTES);
  assert.match(body, /^## WallpaperEM v2\.0\.0\n/);
  assert.match(body, /CHANGELOG\.md/);
  assert.doesNotMatch(body, /app-notes|· 多显示器/, "区块（含标记）不进正文");
  assert.doesNotMatch(body, /\n{3,}/, "去掉区块后不留一串空行");
  assert.equal(appNotes, "v2.0.0 要点（完整日志见 CHANGELOG.md）\n\n· 多显示器每屏独立设置\n");

  const plain = renderNotes("## 标题\n\n- **粗**\n");
  assert.equal(plain.body, "## 标题\n\n- **粗**\n", "没有区块时正文字面保留");
  assert.equal(plain.appNotes, "标题\n\n· 粗\n");
});

test("renderNotes：有 start 没 end 要当场抛错，不猜着发", () => {
  assert.throws(() => renderNotes(`正文\n${APP_NOTES_START}\n半截\n`), /必须成对/);
});

test("patchManifest：只动 notes，其余字段与缩进/行尾保持生成器的形状", () => {
  const before = `{\n  "version": "2.0.0",\n  "notes": "\\n",\n  "pub_date": "2026-09-27T11:55:43.000Z",\n  "platforms": {\n    "darwin-aarch64": {\n      "signature": "sig",\n      "url": "https://e.invalid/a"\n    }\n  }\n}\n`;
  const after = patchManifest(before, "新说明\n");
  const a = JSON.parse(after);
  const b = JSON.parse(before);
  assert.equal(a.notes, "新说明\n");
  for (const k of ["version", "pub_date", "platforms"]) {
    assert.deepEqual(a[k], b[k], `${k} 不该被碰到`);
  }
  assert.match(after, /^  "version"/m, "还是两空格缩进");
  assert.ok(after.endsWith("}\n"), "还是以换行收尾");
  assert.throws(() => patchManifest("不是 JSON", "x"), SyntaxError);
});

test("pickManifestAssets：只认 latest-*.json，按名字排序，安装包不动", () => {
  const assets = [
    { name: "WallpaperEM_2.0.0_universal.dmg" },
    { name: "latest-windows-x86_64.json" },
    { name: "latest-darwin-aarch64.json" },
    { name: "notes.txt" },
  ];
  assert.deepEqual(pickManifestAssets(assets), ["latest-darwin-aarch64.json", "latest-windows-x86_64.json"]);
  assert.deepEqual(pickManifestAssets(undefined), []);
  assert.deepEqual(pickManifestAssets(["latest-b.json", "latest-a.json"]), ["latest-a.json", "latest-b.json"]);
});

test("parseCommand / defaultNotesPath：带引号的命令拆得开，默认路径是约定位置", () => {
  assert.deepEqual(parseCommand('"C:\\p ath\\node.exe" "C:\\t\\stub.mjs"'), ["C:\\p ath\\node.exe", "C:\\t\\stub.mjs"]);
  assert.deepEqual(parseCommand("gh"), ["gh"]);
  assert.equal(defaultNotesPath("/repo", "v2.0.0"), path.join("/repo", "docs", "release-notes", "v2.0.0.md"));
});

// ------------------------------------------------------------------ CLI（假 gh）

/** 测试用假 gh：认 release view / edit / download / upload 与 repo view，状态全在 STUB_DIR。 */
const STUB_GH = `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";

const dir = process.env.STUB_DIR;
const stateFile = path.join(dir, "release.json");
const manifestsDir = path.join(dir, "manifests");
const args = process.argv.slice(2);
const die = (msg) => { console.error(msg); process.exit(1); };
const flag = (name) => { const i = args.indexOf(name); return i === -1 ? null : args[i + 1]; };

/** 只支持 latest-*.json 这种形状，够用且没有正则转义坑。 */
function globMatch(pattern, name) {
  const [pre, ...rest] = pattern.split("*");
  if (rest.length === 0) return pattern === name;
  const post = rest.join("*");
  return name.startsWith(pre) && name.endsWith(post) && name.length >= pre.length + post.length;
}

function assets() {
  if (!fs.existsSync(manifestsDir)) return [];
  return fs.readdirSync(manifestsDir).sort().map((name) => {
    const lie = process.env.STUB_SIZE_LIE ? process.env.STUB_SIZE_LIE.split(":") : null;
    const size = lie && lie[0] === name ? Number(lie[1]) : fs.statSync(path.join(manifestsDir, name)).size;
    return { name, size };
  });
}

const [cmd, sub] = args;
if (args[0] === "--version") { console.log("stub gh 1.0.0"); process.exit(0); }
if (cmd === "repo" && sub === "view") { console.log("stub/stub-repo"); process.exit(0); }
if (cmd === "release" && sub === "view") {
  if (!fs.existsSync(stateFile)) die("release not found: stub 里没建这个 Release");
  const st = JSON.parse(fs.readFileSync(stateFile, "utf8"));
  console.log(JSON.stringify({ url: st.url, body: st.body, assets: assets() }));
  process.exit(0);
}
if (cmd === "release" && sub === "edit") {
  const notesFile = flag("--notes-file");
  if (!notesFile) die("edit 少了 --notes-file");
  const body = fs.readFileSync(notesFile, "utf8");
  fs.writeFileSync(path.join(dir, "body.md"), body);
  const st = JSON.parse(fs.readFileSync(stateFile, "utf8"));
  st.body = body;
  fs.writeFileSync(stateFile, JSON.stringify(st));
  console.log("stub: body updated");
  process.exit(0);
}
if (cmd === "release" && sub === "download") {
  const pattern = flag("-p");
  const dest = flag("-D");
  const names = assets().map((a) => a.name).filter((n) => globMatch(pattern, n));
  if (names.length === 0) die("no assets matched " + pattern);
  fs.mkdirSync(dest, { recursive: true });
  for (const n of names) fs.copyFileSync(path.join(manifestsDir, n), path.join(dest, n));
  console.log("stub: downloaded " + names.length);
  process.exit(0);
}
if (cmd === "release" && sub === "upload") {
  const files = args.filter((a) => !a.startsWith("-") && a !== "upload" && fs.existsSync(a));
  fs.mkdirSync(manifestsDir, { recursive: true });
  for (const f of files) fs.copyFileSync(f, path.join(manifestsDir, path.basename(f)));
  console.log("stub: uploaded " + files.length);
  process.exit(0);
}
die("stub gh 不认识的调用: " + args.join(" "));
`;

const manifestJson = (notes = "\n") =>
  `${JSON.stringify(
    {
      version: "2.0.0",
      notes,
      pub_date: "2026-09-27T11:55:43.000Z",
      platforms: { "darwin-aarch64": { signature: "sig", url: "https://e.invalid/a" } },
    },
    null,
    2,
  )}\n`;

function mkdtemp(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-finish-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

/** 夹具：临时目录里放说明文件 + 假 gh（状态目录 stub/，含 Release 状态与清单资产）。 */
function makeFixture(t, { tag = "v2.0.0", manifests = ["latest-darwin-aarch64.json"], notes = NOTES, withRelease = true } = {}) {
  const dir = mkdtemp(t);
  const stubDir = path.join(dir, "stub");
  const manifestsDir = path.join(stubDir, "manifests");
  fs.mkdirSync(manifestsDir, { recursive: true });
  for (const name of manifests) fs.writeFileSync(path.join(manifestsDir, name), manifestJson());
  // 非清单资产（安装包）也在 assets 里，用来证明脚本不碰它
  fs.writeFileSync(path.join(manifestsDir, "WallpaperEM_2.0.0_universal.dmg"), "假安装包");
  if (withRelease) {
    fs.writeFileSync(path.join(stubDir, "release.json"), JSON.stringify({ url: `https://e.invalid/${tag}`, body: "旧正文\n" }));
  }
  const stub = path.join(dir, "stub-gh.mjs");
  fs.writeFileSync(stub, STUB_GH);
  const notesFile = path.join(dir, "notes.md");
  if (notes !== null) fs.writeFileSync(notesFile, notes);
  return {
    dir,
    stubDir,
    manifestsDir,
    notesFile,
    // 路径都用引号包起来：parseCommand 负责拆（Windows 的临时目录可能带空格）
    ghArgs: ["--gh", `"${process.execPath}" "${stub}"`],
  };
}

function run(fx, args, env = {}) {
  const r = spawnSync(process.execPath, [SCRIPT, ...args, ...fx.ghArgs, "--skip-verify"], {
    encoding: "utf8",
    env: { ...process.env, STUB_DIR: fx.stubDir, GITHUB_ACTIONS: "", NO_COLOR: "1", GITHUB_REF_NAME: "", ...env },
  });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

test("CLI：正文 + 三份清单都写成，安装包不动，退出码 0", (t) => {
  const fx = makeFixture(t, { manifests: ["latest-darwin-aarch64.json", "latest-linux-x86_64.json", "latest-windows-x86_64.json"] });
  const r = run(fx, ["--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo"]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /写 Release 正文/);
  assert.match(r.out, /重传清单\s+3 份/);
  assert.match(r.out, /发布说明已就位/);

  const { body, appNotes } = renderNotes(NOTES);
  assert.equal(fs.readFileSync(path.join(fx.stubDir, "body.md"), "utf8"), body, "正文 = 说明文件去掉区块");

  for (const name of ["latest-darwin-aarch64.json", "latest-linux-x86_64.json", "latest-windows-x86_64.json"]) {
    const m = JSON.parse(fs.readFileSync(path.join(fx.manifestsDir, name), "utf8"));
    assert.equal(m.notes, appNotes, `${name} 的 notes 应是应用内那份`);
    assert.equal(m.version, "2.0.0");
    assert.deepEqual(m.platforms, { "darwin-aarch64": { signature: "sig", url: "https://e.invalid/a" } });
  }
  assert.equal(fs.readFileSync(path.join(fx.manifestsDir, "WallpaperEM_2.0.0_universal.dmg"), "utf8"), "假安装包", "非清单资产不该被下载重传");
});

test("CLI：--dry-run 只说不做（正文与清单原封不动）", (t) => {
  const fx = makeFixture(t);
  const before = fs.readFileSync(path.join(fx.manifestsDir, "latest-darwin-aarch64.json"), "utf8");
  const r = run(fx, ["--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo", "--dry-run"]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /什么都没写/);
  assert.equal(fs.existsSync(path.join(fx.stubDir, "body.md")), false);
  assert.equal(fs.readFileSync(path.join(fx.manifestsDir, "latest-darwin-aarch64.json"), "utf8"), before);
});

test("CLI：说明文件缺失 → 退出码 1 并给出约定路径与骨架", (t) => {
  const fx = makeFixture(t, { notes: null });
  const r = run(fx, ["--tag", "v2.0.0", "--root", fx.dir, "--repo", "stub/stub-repo"]);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /说明文件不存在/);
  assert.match(r.out, /docs[\\/]release-notes[\\/]v2\.0\.0\.md/, "要指到默认位置");
  assert.match(r.out, /app-notes:start/, "骨架里要带区块写法");
});

test("CLI：Release 还不存在（构建没跑完）→ 退出码 1，提示先等构建", (t) => {
  const fx = makeFixture(t, { withRelease: false });
  const r = run(fx, ["--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo"]);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /读不到 Release v2\.0\.0/);
  assert.match(r.out, /收尾发生在推 tag/);
});

test("CLI：还没有 latest-*.json 也不崩 —— 正文照写，提醒稍后重跑", (t) => {
  const fx = makeFixture(t, { manifests: [] });
  const r = run(fx, ["--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo"]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /还没有 latest-\*\.json/);
  assert.equal(fs.readFileSync(path.join(fx.stubDir, "body.md"), "utf8"), renderNotes(NOTES).body);
});

test("CLI：清单体积对不上 → 退出码 1（写没写进去不能靠猜）", (t) => {
  const fx = makeFixture(t);
  const r = run(fx, ["--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo"], {
    STUB_SIZE_LIE: "latest-darwin-aarch64.json:1",
  });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /体积与本地不符/);
  assert.match(r.out, /有 1 项没写成/);
});

test("CLI：--tag 缺失或形状不对 → 退出码 2（收尾不接受含糊输入）", (t) => {
  const fx = makeFixture(t);
  const miss = run(fx, ["--notes-file", fx.notesFile]);
  assert.equal(miss.code, 2, miss.out);
  assert.match(miss.out, /得给 --tag/);

  const bad = run(fx, ["--tag", "2.0.0", "--notes-file", fx.notesFile]);
  assert.equal(bad.code, 2, bad.out);
  assert.match(bad.out, /不是 vX\.Y\.Z 形状/);

  const help = run(fx, ["--help"]);
  assert.equal(help.code, 0, help.out);
  assert.match(help.out, /发布收尾/);
  assert.match(help.out, /--notes-file/);
});

test("CLI：环境变量兜底（GITHUB_REF_NAME 当 tag、GITHUB_REPOSITORY 当仓库）", (t) => {
  const fx = makeFixture(t);
  const r = run(fx, ["--notes-file", fx.notesFile], { GITHUB_REF_NAME: "v2.0.0", GITHUB_REPOSITORY: "stub/stub-repo" });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /仓库 stub\/stub-repo/);
});

test("CLI：gh 本身跑不起来 → 一句话说清楚，别拿「读不到 Release」误导人", (t) => {
  const fx = makeFixture(t);
  const r = spawnSync(process.execPath, [SCRIPT, "--tag", "v2.0.0", "--notes-file", fx.notesFile, "--repo", "stub/stub-repo", "--gh", "/nonexistent/gh-bin"], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_ACTIONS: "", NO_COLOR: "1", GITHUB_REF_NAME: "" },
  });
  assert.equal(r.status, 1);
  assert.match(`${r.stdout}${r.stderr}`, /gh 跑不起来/);
  assert.match(`${r.stdout}${r.stderr}`, /gh auth login/);
});
