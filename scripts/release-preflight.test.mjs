#!/usr/bin/env node
// release-preflight.mjs 的测试：纯函数直接测，CLI 走夹具仓库（--root 指到临时目录，
// 因此版本检查会用夹具里的文件，而不是本仓库的）。
//
// 跑法：node --test scripts/*.test.mjs   （package.json: pnpm test:scripts）

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { isTag, hasChangelogSection, unreleasedItems } from "./release-preflight.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(HERE, "release-preflight.mjs");

function write(dir, rel, content) {
  const p = path.join(dir, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, content);
}

/** 一个最小可检仓库：三处权威版本 + CHANGELOG。 */
function makeFixture(t, { version = "2.0.0", cargoVersion, changelog } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-preflight-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  write(dir, "package.json", `${JSON.stringify({ name: "fixture", version }, null, 2)}\n`);
  write(dir, "src-tauri/tauri.conf.json", `${JSON.stringify({ version }, null, 2)}\n`);
  write(dir, "src-tauri/Cargo.toml", `[package]\nname = "fixture"\nversion = "${cargoVersion ?? version}"\n\n[dependencies]\n`);
  if (changelog !== undefined) write(dir, "CHANGELOG.md", changelog);
  return dir;
}

const GOOD_CHANGELOG = `## [Unreleased]

## [v2.0.0] - 2026-09-27

- 一点东西
`;

function run(dir, args, env = {}) {
  const r = spawnSync(process.execPath, [SCRIPT, "--root", dir, ...args], {
    encoding: "utf8",
    env: { ...process.env, GITHUB_REF_NAME: "", ...env },
  });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
}

// ------------------------------------------------------------------ 纯函数

test("tag 形状：三段式，允许预发布后缀，其余都算不对", () => {
  for (const ok of ["v2.0.0", "v0.5.3", "v2.1.0-beta.1"]) assert.equal(isTag(ok), true, ok);
  for (const bad of ["main", "2.0.0", "v2.0", "v2.0.0.1", "refs/tags/v2.0.0", "release-2.0.0"]) {
    assert.equal(isTag(bad), false, bad);
  }
  assert.equal(isTag(null), false);
  assert.equal(isTag(undefined), false);
});

test("CHANGELOG 段落：标题后允许跟日期与备注；前缀相近的版本不算命中", () => {
  assert.equal(hasChangelogSection(GOOD_CHANGELOG, "v2.0.0"), true);
  assert.equal(hasChangelogSection("## [v0.5.2] - 2026-09-13（重发布，合并原 v0.5.3）\n", "v0.5.2"), true);
  assert.equal(hasChangelogSection(GOOD_CHANGELOG, "v2.0.1"), false);
  assert.equal(hasChangelogSection(GOOD_CHANGELOG, "v2.0"), false, "v2.0 不能被 v2.0.0 的标题蒙混");
  assert.equal(hasChangelogSection(GOOD_CHANGELOG, "v[2.0.0]"), false, "标签里的正则元字符要转义");
});

test("[Unreleased] 条目计数：有条目算、空段不算、HTML 注释不算", () => {
  assert.equal(unreleasedItems(GOOD_CHANGELOG), 0, "空 Unreleased");
  assert.equal(unreleasedItems("## [Unreleased]\n\n- 新东西\n\n## [v2.0.0]\n- 老的\n"), 1);
  assert.equal(unreleasedItems("## [Unreleased]\n\n<!-- 占位：\n- 注释里的横杠\n-->\n"), 0);
  assert.equal(unreleasedItems("# 没有 Unreleased 段\n"), 0);
});

// ------------------------------------------------------------------ CLI（夹具）

test("正式（tag 推送）：版本一致 + tag 对 + 有段落 → 0", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG });
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.0.0" });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /发布前置全过/);
  assert.match(r.out, /可以出包了/);
});

test("正式：CHANGELOG 没有该版本段落 → 阻断（1）", (t) => {
  const dir = makeFixture(t, { changelog: "## [Unreleased]\n" });
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.0.0" });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /CHANGELOG 有 v2\.0\.0 段/);
  assert.match(r.out, /没通过 1 项/);
});

test("正式：推的 tag 与三处版本对不上 → 阻断（1）", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG });
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.1.0" });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /发错号了/);
});

test("正式：三处版本不一致 → 阻断（1），并且是 check-versions 的话术", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG, cargoVersion: "1.9.9" });
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.0.0" });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /版本一致（三处权威）/);
  assert.match(r.out, /权威三处必须完全相同/, "版本规则来自 check-versions.mjs，不该有两份");
});

test("正式：CHANGELOG.md 整个缺失 → 阻断", (t) => {
  const dir = makeFixture(t);
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.0.0" });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /CHANGELOG\.md 存在/);
});

test("彩排（手动触发、分支 ref）：段落没归、Unreleased 有条目都只是提醒 → 0", (t) => {
  const dir = makeFixture(t, { changelog: "## [Unreleased]\n\n- 还没归\n" });
  const r = run(dir, [], { GITHUB_REF_NAME: "main" });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /彩排模式只提醒/);
  assert.match(r.out, /剩余条目.*1 条/s);
  assert.match(r.out, /另有 2 条提醒/);
});

test("彩排：给的是非 tag 的 ref 也不该判「tag 形状」错（那是分支，符合预期）", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG });
  const r = run(dir, [], { GITHUB_REF_NAME: "release/v2" });
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /彩排模式/);
});

test("CLI：--tag 显式传入优先于环境；--help 走 0、未知选项走 2", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG });
  assert.equal(run(dir, ["--tag", "v2.0.0"], { GITHUB_REF_NAME: "main" }).code, 0);
  assert.equal(run(dir, ["--tag", "v9.9.9"], { GITHUB_REF_NAME: "main" }).code, 1, "显式 tag 对不上要拦");

  const help = spawnSync(process.execPath, [SCRIPT, "--help"], { encoding: "utf8" });
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /发布前置检查/);
  const bad = spawnSync(process.execPath, [SCRIPT, "--nope"], { encoding: "utf8" });
  assert.equal(bad.status, 2);
});

test("歪标签推送：workflow 的 v* 会匹配 v2.0.0.1，形状必须拦住（不能当彩排放过）", (t) => {
  const dir = makeFixture(t, { changelog: GOOD_CHANGELOG });
  const r = run(dir, [], { GITHUB_REF_NAME: "v2.0.0.1" });
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /tag 形状/);
  assert.match(r.out, /不是 vX\.Y\.Z/);
});
