#!/usr/bin/env node
// repo-hygiene.mjs 的测试：每个用例一个临时 git 仓库（§9 的闸门必须证明它能抓住
// `git add -f` 塞进来的那类东西 —— 那正是 .gitignore 拦不住的场景）。
//
// 跑法：node --test scripts/*.test.mjs   （package.json: pnpm test:scripts）

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { forbiddenReason, MAX_SIZE } from "./repo-hygiene.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(HERE, "repo-hygiene.mjs");

function sh(dir, cmd, args) {
  const r = spawnSync(cmd, args, { cwd: dir, encoding: "utf8" });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}
const git = (dir, ...args) => {
  const r = sh(dir, "git", args);
  assert.equal(r.code, 0, `git ${args.join(" ")}：${r.out}`);
  return r.out.trim();
};

function makeRepo(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-hygiene-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  git(dir, "init", "-q", "-b", "main");
  git(dir, "config", "user.email", "t@example.com");
  git(dir, "config", "user.name", "t");
  git(dir, "config", "commit.gpgsign", "false");
  fs.mkdirSync(path.join(dir, "src"), { recursive: true });
  fs.writeFileSync(path.join(dir, "src", "app.js"), "// 项目文件\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-qm", "chore: 基线");
  return dir;
}

const run = (dir, args) => sh(dir, process.execPath, [SCRIPT, ...args]);

function forceAdd(dir, rel, content) {
  const p = path.join(dir, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, content);
  // 故意 -f：这正是 §9 要防的绕过 .gitignore 的手法
  git(dir, "add", "-f", "--", rel);
}

// ------------------------------------------------------------------ 路径表

test("违禁路径表：§9 列的每类都认得出来，正常路径不误伤", () => {
  const bad = {
    "dist/index.html": "构建产物",
    "a/b/dist/x.js": "嵌套 dist",
    "src-tauri/target/debug/foo": "Rust target",
    "node_modules/react/index.js": "依赖目录",
    ".DS_Store": "系统垃圾",
    "notes.bak": "备份",
    "app.js~": "编辑器残留",
    "data/foo.db": "数据库",
    "credentials.dat": "凭据",
    "src-tauri/bundled/lib.dylib": "打包中间产物",
    "proto-video-loop/clips/a.mp4": "生成物",
    ".ui-shots/x.png": "工具工作目录",
    "id_rsa": "私钥",
  };
  for (const [p, why] of Object.entries(bad)) assert.ok(forbiddenReason(p), `没认出来：${p}（应报：${why}）`);
  for (const ok of ["src/app.js", "docs/pr-rules.md", "promo/gifs/04-lonely-cat-visualizer.gif", "src-tauri/Cargo.toml", "dist-packages/tool.zip"]) {
    assert.equal(forbiddenReason(ok), null, `误伤：${ok}`);
  }
});

// ------------------------------------------------------------------ 三种模式

test("暂存区干净 → 0", (t) => {
  const dir = makeRepo(t);
  fs.appendFileSync(path.join(dir, "src", "app.js"), "// 改动\n");
  git(dir, "add", "-A");
  const r = run(dir, ["--staged"]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /干净/);
});

test("git add -f 塞进来的构建产物 → 1（这正是 .gitignore 拦不住的场景）", (t) => {
  const dir = makeRepo(t);
  forceAdd(dir, "dist/index.html", "<html></html>");
  const r = run(dir, ["--staged"]);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /dist\/index\.html/);
  assert.match(r.out, /违禁路径/);
  assert.match(r.out, /§9/);
});

test("暂存区里只有删除违禁文件 → 放行（删除是好事）", (t) => {
  const dir = makeRepo(t);
  forceAdd(dir, "dist/old.js", "x");
  git(dir, "commit", "-qm", "chore: 混进来一个");
  git(dir, "rm", "-q", "dist/old.js");
  const r = run(dir, ["--staged"]);
  assert.equal(r.code, 0, r.out);
});

test("新增 > 5MB 且没走 LFS → 1；.gitattributes 登记过 → 0", (t) => {
  const dir = makeRepo(t);
  forceAdd(dir, "promo/big.bin", Buffer.alloc(MAX_SIZE + 1024));
  const bad = run(dir, ["--staged"]);
  assert.equal(bad.code, 1, bad.out);
  assert.match(bad.out, /没有走 Git LFS/);
  assert.match(bad.out, /6\.0MB|5\.0MB/);

  fs.writeFileSync(path.join(dir, ".gitattributes"), "*.bin filter=lfs diff=lfs merge=lfs -text\n");
  const ok = run(dir, ["--staged"]);
  assert.equal(ok.code, 0, ok.out);
});

test("恰好不超过 5MB → 放行（阈值边界）", (t) => {
  const dir = makeRepo(t);
  forceAdd(dir, "promo/ok.bin", Buffer.alloc(MAX_SIZE));
  const r = run(dir, ["--staged"]);
  assert.equal(r.code, 0, r.out);
});

test("--range：按范围内状态判定（旧提交里的违禁物不在本次范围就不算）", (t) => {
  const dir = makeRepo(t);
  forceAdd(dir, "dist/legacy.js", "旧的");
  git(dir, "commit", "-qm", "chore: 混进历史");
  const old = git(dir, "rev-parse", "--short", "HEAD");

  fs.appendFileSync(path.join(dir, "src", "app.js"), "// 干净改动\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-qm", "chore: 干净提交");

  const clean = run(dir, ["--range", `${old}..HEAD`]);
  assert.equal(clean.code, 0, clean.out);

  forceAdd(dir, "dist/new.js", "新的");
  git(dir, "commit", "-qm", "chore: 又混一个");
  const bad = run(dir, ["--range", `${old}..HEAD`]);
  assert.equal(bad.code, 1, bad.out);
  assert.match(bad.out, /dist\/new\.js/);
});

test("--all：全仓扫路径（抓被 --no-verify 推进来的），但不查大小（历史 GIF 是既成事实）", (t) => {
  const dir = makeRepo(t);
  const clean = run(dir, ["--all"]);
  assert.equal(clean.code, 0, clean.out);

  forceAdd(dir, ".DS_Store", "");
  git(dir, "commit", "-qm", "chore: 混进历史");
  const bad = run(dir, ["--all"]);
  assert.equal(bad.code, 1, bad.out);
  assert.match(bad.out, /\.DS_Store/);

  // 历史大文件（非 LFS、非新增）在 --all 下不拦 —— 本仓库有 4 个 5–16MB 的 GIF
  forceAdd(dir, "promo/huge.bin", Buffer.alloc(MAX_SIZE * 2));
  git(dir, "commit", "-qm", "chore: 大文件进历史");
  const sizeOnly = run(dir, ["--all"]);
  assert.equal(sizeOnly.code, 1, `--all 应因 .DS_Store 仍失败：${sizeOnly.out}`);
  assert.doesNotMatch(sizeOnly.out, /没有走 Git LFS/, "--all 不查大小");
});

test("--all：空仓库没有违禁文件 → 0", (t) => {
  const dir = makeRepo(t);
  assert.equal(run(dir, ["--all"]).code, 0);
});

test("CLI：--help 走 0、未知选项走 2、--range 缺范围走 2", (t) => {
  const dir = makeRepo(t);
  assert.equal(sh(dir, process.execPath, [SCRIPT, "--help"]).code, 0);
  assert.equal(run(dir, ["--nope"]).code, 2);
  assert.equal(run(dir, ["--range"]).code, 2);
});
