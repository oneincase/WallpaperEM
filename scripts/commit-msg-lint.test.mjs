#!/usr/bin/env node
// commit-msg-lint.mjs 的回归测试 —— 这是**用得最多的门禁**（本地 commit-msg 钩子 + CI
// 的 PR 标题/范围/正文三处都跑它），它坏了会同时坏在两边，此前却零测试。
// 它没有导出（CLI 是它的接口），所以全部走子进程。
//
// 跑法：node --test scripts/*.test.mjs   （package.json: pnpm test:scripts）

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(HERE, "commit-msg-lint.mjs");

const run = (...args) => {
  const opts = typeof args[0] === "object" && args[0] !== null ? args[0] : null;
  const realArgs = opts ? args.slice(1) : args;
  const r = spawnSync(process.execPath, [SCRIPT, ...realArgs], { encoding: "utf8", cwd: opts?.cwd });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
};

function git(dir, ...args) {
  const r = spawnSync("git", args, { cwd: dir, encoding: "utf8" });
  assert.equal(r.status, 0, `git ${args.join(" ")}：${r.stderr}`);
  return r.stdout.trim();
}

function makeRepo(t, commits) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-msglint-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  git(dir, "init", "-q", "-b", "main");
  git(dir, "config", "user.email", "t@example.com");
  git(dir, "config", "user.name", "t");
  git(dir, "config", "commit.gpgsign", "false");
  fs.writeFileSync(path.join(dir, "a.txt"), "0\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-qm", "chore: 基线");
  for (const msg of commits) {
    fs.appendFileSync(path.join(dir, "a.txt"), "x\n");
    git(dir, "add", "-A");
    // -m 会把 # 开头的行当注释清掉，正文里的 ## 小节要走 -F 才保得住
    const f = path.join(dir, ".commit-msg-tmp");
    fs.writeFileSync(f, msg);
    git(dir, "commit", "-q", "-F", f);
    fs.rmSync(f);
  }
  return dir;
}

// ------------------------------------------------------------------ --message

test("合规的提交信息 → 0", () => {
  const r = run("--message", "feat(playlist): 手动设单张后仍按轮播上下文取下一张");
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /✓/);
});

test("结构性错误一律拦：非形状 / 大写类型 / 空 scope / scope 逗号", () => {
  for (const bad of [
    "修个 bug", // 不是 type(scope): 主题
    "Feat(ui): 改按钮", // 大写
    "feat(): 改按钮", // 空 scope
    "feat(ui,theme): 改按钮", // 逗号不是合法分隔
    "feat(ui):", // 没有主题
  ]) {
    const r = run("--message", bad);
    assert.equal(r.code, 1, `该拦没拦：${bad}\n${r.out}`);
  }
});

test("禁止的类型与空话主题 → 1（并且话术有替代建议）", () => {
  const banned = run("--message", "wip: 改点东西");
  assert.equal(banned.code, 1, banned.out);
  assert.match(banned.out, /不允许的类型/);
  assert.match(banned.out, /拆成可评审的提交/);

  const vague = run("--message", "fix: 修改代码");
  assert.equal(vague.code, 1, vague.out);
  assert.match(vague.out, /空话/);
});

test("主题末尾句号：英文句号拦（1），中文句号只警告（0）", () => {
  assert.equal(run("--message", "fix(ui): 修掉按钮错位.").code, 1);
  const cn = run("--message", "fix(ui): 修掉按钮错位。");
  assert.equal(cn.code, 0, cn.out);
  assert.match(cn.out, /习惯是/);
});

test("头部长度：>120 拦，>100 只警告", () => {
  const long = `fix(ui): ${"长".repeat(130)}`;
  assert.equal(run("--message", long).code, 1);
  const mid = `fix(ui): ${"长".repeat(110)}`;
  const r = run("--message", mid);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /偏长/);
});

test("正文：头部与正文之间必须空行；段落小节缺『验证』在 --require-sections 下才拦", () => {
  const noBlank = "fix(ui): 修按钮\n直接接正文";
  assert.equal(run("--message", noBlank).code, 1);
  assert.equal(run("--message", "fix(ui): 修按钮\n\n## 改动\n- 一条").code, 0);

  const body = "## 改动\n把按钮的错位与禁用态都修掉了\n\n## 验证\n在 macOS 上点了三次";
  const f = path.join(os.tmpdir(), `we-pr-body-${Date.now()}.md`);
  fs.writeFileSync(f, body);
  assert.equal(run("--body-file", f, "--require-sections").code, 0);

  fs.writeFileSync(f, "## 改动\n把按钮的错位与禁用态都修掉了\n"); // 没有验证小节
  assert.equal(run("--body-file", f, "--require-sections").code, 1);
  fs.rmSync(f);
});

test("豁免提交：Merge / Revert / fixup! / Release 直接放行", () => {
  for (const ok of [
    "Merge branch 'main' into feat/x",
    "Revert \"feat(ui): 改按钮\"",
    "fixup! feat(ui): 改按钮",
    "Release v2.0.0",
  ]) {
    assert.equal(run("--message", ok).code, 0, ok);
  }
});

test("--title 模式只看头部（正文有毛病也不管 PR 标题）", () => {
  assert.equal(run("--title", "feat(ui): 改按钮", ).code, 0);
  // 正文空行问题在标题模式下不该判错
  assert.equal(run("--title", "feat(ui): 改按钮\n坏正文").code, 0);
});

// ------------------------------------------------------------------ --range / --help

test("--range：混合好坏提交时逐条报，最后给出不合规条数（退出 1）", (t) => {
  const dir = makeRepo(t, ["feat(ui): 好的", "wip: 坏的", "fix(apply): 也好"]);
  const r = run({ cwd: dir }, "--range", "HEAD~3..HEAD");
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /3 个提交/);
  assert.match(r.out, /1 个不合规/);

  // 只取最后一条（wip 那条在 HEAD~1，别把它圈进来）
  const clean = run({ cwd: dir }, "--range", "HEAD~1..HEAD");
  assert.equal(clean.code, 0, clean.out);
});

test("--range：范围内没有提交 → 0（不算失败）", (t) => {
  const dir = makeRepo(t, []);
  assert.equal(run({ cwd: dir }, "--range", "HEAD..HEAD").code, 0);
});

test("--help 走 0、未知选项走 2、空信息文件走 1", (t) => {
  assert.equal(run("--help").code, 0);
  assert.equal(run("--nope").code, 2);

  const dir = makeRepo(t, []);
  const empty = path.join(dir, "empty-msg");
  fs.writeFileSync(empty, "\n# 全是注释\n");
  const r = run(empty);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /提交信息为空/);
});
