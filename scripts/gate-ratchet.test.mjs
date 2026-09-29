#!/usr/bin/env node
// gate-ratchet.mjs 的测试。刻意不跑真 cargo —— 比较逻辑是纯函数，测量逻辑吃注入的
// runner。真 cargo 的那次读数由 `pnpm gate:ratchet --update` 产生（基线文件本身是证据）。
//
// 跑法：node --test scripts/*.test.mjs   （package.json: pnpm test:scripts）

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { evaluate, mergeBaseline, countFmtDiffs, countClippyWarnings, platformKey, METRICS } from "./gate-ratchet.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRIPT = path.join(HERE, "gate-ratchet.mjs");

const ctx = (over = {}) => ({
  metric: "fmt",
  value: 100,
  baseline: 100,
  toolchain: "rustc 1.0 (x 2026-01-01)",
  recordedToolchain: "rustc 1.0 (x 2026-01-01)",
  ...over,
});

// ------------------------------------------------------------------ 比较逻辑

test("棘轮：比基线差 → fail（阻断）；持平 → pass；比基线好 → notice（提示收紧）", () => {
  assert.equal(evaluate(ctx({ value: 120 })).verdict, "fail");
  assert.equal(evaluate(ctx({ value: 120 })).message, "当前 120处 > 基线 100处（新增 20处）：清理它 = 全仓统一格式化，单独一条提交（§5），别混进功能改动");
  assert.equal(evaluate(ctx({ value: 100 })).verdict, "pass");
  const better = evaluate(ctx({ value: 80 }));
  assert.equal(better.verdict, "notice");
  assert.match(better.message, /少了 20处.*gate:ratchet --update/s);
});

test("棘轮：该平台没测过 → 放行并要求收基线（不能因为没基线就红）", () => {
  const v = evaluate(ctx({ baseline: null }));
  assert.equal(v.verdict, "notice");
  assert.match(v.message, /还没有基线读数（当前 100处）/);
});

test("棘轮：工具链变了 → 读数不可比，当次放行（哪怕数值更大）", () => {
  const v = evaluate(ctx({ value: 999, recordedToolchain: "rustc 1.0 (x 2026-01-01)", toolchain: "rustc 2.0 (y 2026-06-01)" }));
  assert.equal(v.verdict, "notice");
  assert.match(v.message, /工具链变了.*不可比.*当次放行/s);
});

test("棘轮：clippy 指标带自己的话术；无读数时不算通过也不算失败", () => {
  assert.match(evaluate(ctx({ metric: "clippy", value: 110 })).message, /新增 10条/);
  assert.match(evaluate(ctx({ value: 110 })).message, /全仓统一格式化/, "fmt 要带自己的清理提示");
  assert.equal(evaluate(ctx({ value: null })).verdict, "pass");
});

test("棘轮：--update 的合并只覆盖被测的指标，别把别的平台/指标清掉", () => {
  const base = { platforms: { "linux-x86_64": { toolchain: "rustc L", fmt: 7, clippy: 3 } } };
  const next = mergeBaseline(base, { platform: "darwin-arm64", toolchain: "rustc M", values: { fmt: 5 } });
  assert.deepEqual(next.platforms["darwin-arm64"], { toolchain: "rustc M", fmt: 5 });
  assert.deepEqual(next.platforms["linux-x86_64"], { toolchain: "rustc L", fmt: 7, clippy: 3 }, "别的平台必须原样保留");
  const same = mergeBaseline(next, { platform: "linux-x86_64", toolchain: "rustc L2", values: { fmt: 6 } });
  assert.deepEqual(same.platforms["linux-x86_64"], { toolchain: "rustc L2", fmt: 6, clippy: 3 }, "没测的指标保留旧值");
  assert.deepEqual(base.platforms["linux-x86_64"], { toolchain: "rustc L", fmt: 7, clippy: 3 }, "不能改到入参");
});

// ------------------------------------------------------------------ 测量

test("fmt 计数：认 `Diff in` 行；退出码 1（有差异）是正常读数", () => {
  const seen = [];
  const runner = (cmd, args) => {
    seen.push(`${cmd} ${args.join(" ")}`);
    return { stdout: "Diff in /a/b.rs at line 3\n@@@\nDiff in /a/b.rs at line 9\n", stderr: "", code: 1 };
  };
  assert.equal(countFmtDiffs(runner), 2);
  assert.match(seen[0], /cargo fmt --all --check --manifest-path .*Cargo\.toml/);
});

test("fmt 计数：退出码 2（跑坏了）→ 报测量失败，绝不当 0 处", () => {
  const runner = () => ({ stdout: "", stderr: "error: could not find `Cargo.toml`", code: 2 });
  assert.throws(() => countFmtDiffs(runner), /退出码 2.*没有可信读数/s);
  assert.throws(() => countFmtDiffs(() => { throw new Error("cargo 不在 PATH"); }), /cargo 起不来/);
});

test("clippy 计数：按 code@文件:行 去重（--all-targets 会重复报同处）", () => {
  const line = (code, file, n) =>
    JSON.stringify({ reason: "compiler-message", message: { code: { code }, spans: [{ is_primary: true, file_name: file, line_start: n }] } });
  const stdout = [
    line("clippy::needless_range_loop", "src/a.rs", 10),
    line("clippy::needless_range_loop", "src/a.rs", 10), // 同一处，lib test 里再报一次
    line("clippy::needless_range_loop", "src/b.rs", 20), // 另一处
    line("unused_variables", "src/a.rs", 30), // rustc 自己的告警不算 clippy
    JSON.stringify({ reason: "compiler-artifact" }), // 非诊断行
    "not json at all",
  ].join("\n");
  const runner = () => ({ stdout, stderr: "", code: 0 });
  assert.equal(countClippyWarnings(runner), 2);
});

test("clippy 计数：cargo 退出 1（编译错误）→ 测量失败", () => {
  const runner = () => ({ stdout: "", stderr: "error[E0433]: failed to resolve", code: 1 });
  assert.throws(() => countClippyWarnings(runner), /退出码 1/);
});

test("平台键：darwin-arm64 / linux-x86_64", () => {
  assert.equal(platformKey("darwin", "arm64"), "darwin-arm64");
  assert.equal(platformKey("linux", "x64"), "linux-x64");
});

// ------------------------------------------------------------------ CLI

test("CLI：--help 走 0，未知选项走 2（都不碰 cargo）", () => {
  const help = spawnSync(process.execPath, [SCRIPT, "--help"], { encoding: "utf8" });
  assert.equal(help.status, 0, help.stderr);
  assert.match(help.stdout, /门禁棘轮/);
  const bad = spawnSync(process.execPath, [SCRIPT, "--only", "bogus"], { encoding: "utf8" });
  assert.equal(bad.status, 2);
  assert.match(bad.stderr, /--only 只能是/);
});

test("跨平台回填：参数不全 / 数值不对 → 2（这条会写基线文件，所以只测拒绝路径）", () => {
  // 少 --toolchain
  const missing = spawnSync(process.execPath, [SCRIPT, "--update", "--platform", "linux-x64", "--fmt", "551", "--clippy", "49"], { encoding: "utf8" });
  assert.equal(missing.status, 2, missing.stdout + missing.stderr);
  assert.match(missing.stderr, /--toolchain/);
  // 只给了一半的指标
  const half = spawnSync(process.execPath, [SCRIPT, "--update", "--platform", "linux-x64", "--toolchain", "rustc X", "--fmt", "551"], { encoding: "utf8" });
  assert.equal(half.status, 2, half.stdout + half.stderr);
  // 非整数
  const nan = spawnSync(process.execPath, [SCRIPT, "--update", "--platform", "x", "--toolchain", "t", "--fmt", "abc", "--clippy", "1"], { encoding: "utf8" });
  assert.equal(nan.status, 2, nan.stdout + nan.stderr);
  assert.match(nan.stderr, /非负整数/);
});
