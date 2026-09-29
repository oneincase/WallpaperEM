#!/usr/bin/env node
// check-versions.mjs 的回归测试 —— 三处权威版本是阻断档，而它此前零测试。
// 用 --root 指向夹具仓库（--root 这个参数本来就是为了这里加的）。
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
const SCRIPT = path.join(HERE, "check-versions.mjs");

const run = (args) => {
  const r = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });
  return { code: r.status, out: `${r.stdout ?? ""}${r.stderr ?? ""}` };
};

function write(dir, rel, content) {
  const p = path.join(dir, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, content);
}

/** 三处权威版本 + 可选的 bin-info.plist。Cargo.toml 必须有 [package] 之后的段落
 *  （脚本用 `(?=^\[|\Z)` 定位段尾，只有 [package] 一段时会定位不到）。 */
function makeFixture(t, { version = "2.0.0", tauriVersion, cargoVersion, plistVersion, plistBuild } = {}) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-versions-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  write(dir, "package.json", `${JSON.stringify({ name: "fixture", version }, null, 2)}\n`);
  write(dir, "src-tauri/tauri.conf.json", `${JSON.stringify({ version: tauriVersion ?? version }, null, 2)}\n`);
  write(dir, "src-tauri/Cargo.toml", `[package]\nname = "fixture"\nversion = "${cargoVersion ?? version}"\n\n[dependencies]\n`);
  if (plistVersion || plistBuild) {
    write(
      dir,
      "src-tauri/bin-info.plist",
      `<?xml version="1.0"?>\n<dict>\n  <key>CFBundleShortVersionString</key>\n  <string>${plistVersion ?? version}</string>\n  <key>CFBundleVersion</key>\n  <string>${plistBuild ?? version}</string>\n</dict>\n`,
    );
  }
  return dir;
}

test("三处一致 → 0", (t) => {
  const r = run(["--root", makeFixture(t)]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /版本号 2\.0\.0 校验完成/);
});

test("权威三处任意一处不一致 → 1（报错里带上各处的对照）", (t) => {
  const dir = makeFixture(t, { cargoVersion: "1.9.9" });
  const r = run(["--root", dir]);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /权威三处必须完全相同/);
  assert.match(r.out, /1\.9\.9/);
  assert.match(r.out, /2\.0\.0/);
});

test("文件缺失 → 1（当作没解析出版本号）", (t) => {
  const dir = makeFixture(t);
  fs.rmSync(path.join(dir, "src-tauri", "tauri.conf.json"));
  const r = run(["--root", dir]);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /文件不存在/);
});

test("bin-info.plist 只是建议项：不一致只警告，仍退出 0", (t) => {
  const dir = makeFixture(t, { plistVersion: "1.1.0" });
  const r = run(["--root", dir]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /提示：/);
  assert.match(r.out, /1\.1\.0/);
  assert.match(r.out, /本项不阻断/);
});

test("Cargo.toml 只认 [package] 段里的 version（依赖里的 version = 不算）", (t) => {
  const dir = makeFixture(t);
  write(
    dir,
    "src-tauri/Cargo.toml",
    `[package]\nname = "fixture"\nversion = "2.0.0"\n\n[dependencies]\nserde = { version = "1.0.200" }\n`,
  );
  assert.equal(run(["--root", dir]).code, 0);
});

test("不传 --root → 用本仓库（默认行为没变，且本仓库基线是绿的）", () => {
  const r = run([]);
  assert.equal(r.code, 0, r.out);
  assert.match(r.out, /校验完成/);
});
