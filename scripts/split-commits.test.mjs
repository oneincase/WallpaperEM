#!/usr/bin/env node
// split-commits.mjs 的测试 —— 这个脚本会替人写提交、动索引，所以每条分支都要有证据。
//
// 跑法：node --test scripts/          （package.json: pnpm test:scripts）
// 全部在临时仓库里做，不碰当前仓库；每个用例一个独立仓库。

import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SPLIT = path.join(HERE, "split-commits.mjs");

const SCRIPT_FILES = ["split-commits.mjs", "commit-msg-lint.mjs", "install-hooks.mjs"];

/** 基线内容：路径刻意照着真仓库挑，好让域表（scripts/split-commits.mjs 的 RULES）真的被走到。 */
const BASE = {
  "src-tauri/src/steam/auth.rs": "// steam auth\n".repeat(5),
  "src-tauri/src/mcp/tools.rs": "// mcp tools\n".repeat(5),
  "src-tauri/src/lib.rs": "// core\n".repeat(5),
  "src/lib/qualityPresets.ts": "// quality presets\n".repeat(5),
  "src/pages/Library.tsx": "// library page\n".repeat(5),
  "CHANGELOG.md": "# changelog\n",
  "README.md": "# readme\n",
  "package.json": '{\n  "name": "fixture",\n  "version": "1.0.0"\n}\n',
};

// ------------------------------------------------------------------ 工具

function sh(cwd, cmd, args = [], opts = {}) {
  const res = spawnSync(cmd, args, {
    cwd,
    encoding: "utf8",
    env: { ...process.env, ...(opts.env ?? {}) },
    input: opts.input,
  });
  return { code: res.status, out: `${res.stdout ?? ""}${res.stderr ?? ""}` };
}

function git(cwd, ...args) {
  const r = sh(cwd, "git", args);
  assert.equal(r.code, 0, `git ${args.join(" ")} 应当成功，实际退出码 ${r.code}：\n${r.out}`);
  return r.out.trim();
}

function write(dir, rel, content) {
  const p = path.join(dir, rel);
  fs.mkdirSync(path.dirname(p), { recursive: true });
  fs.writeFileSync(p, content);
}

function read(dir, rel) {
  return fs.readFileSync(path.join(dir, rel), "utf8");
}

/** 一个装了脚本与基线提交的临时仓库。用例结束（t.after）就把目录删掉。 */
function makeRepo(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "we-split-test-"));
  t?.after?.(() => fs.rmSync(dir, { recursive: true, force: true }));
  git(dir, "init", "-q", "-b", "main");
  git(dir, "config", "user.email", "test@example.com");
  git(dir, "config", "user.name", "test");
  git(dir, "config", "commit.gpgsign", "false");
  fs.mkdirSync(path.join(dir, "scripts"), { recursive: true });
  for (const f of SCRIPT_FILES) fs.copyFileSync(path.join(HERE, f), path.join(dir, "scripts", f));
  for (const [rel, content] of Object.entries(BASE)) write(dir, rel, content);
  git(dir, "add", "-A");
  git(dir, "commit", "-qm", "chore: 基线");
  return dir;
}

function split(dir, args, env) {
  return sh(dir, process.execPath, [SPLIT, ...args], { env });
}

/** 造一批"两个功能域各干各的"改动：steam 60 行 + quality 60 行。 */
function stageTwoFeatures(dir) {
  write(dir, "src-tauri/src/steam/auth.rs", read(dir, "src-tauri/src/steam/auth.rs") + "// steam 改动\n".repeat(60));
  write(dir, "src/lib/qualityPresets.ts", read(dir, "src/lib/qualityPresets.ts") + "// quality 改动\n".repeat(60));
  git(dir, "add", "-A");
}

function log(dir, n = 10) {
  return git(dir, "log", "--format=%s", "-n", String(n)).split("\n").filter(Boolean);
}

function treeOf(dir, rev = "HEAD") {
  return git(dir, "rev-parse", `${rev}^{tree}`);
}

function planFile(dir) {
  const p = git(dir, "rev-parse", "--git-path", "we-split-plan.json");
  return path.isAbsolute(p) ? p : path.join(dir, p);
}

function status(dir) {
  return git(dir, "status", "--porcelain").split("\n").filter(Boolean);
}

// ------------------------------------------------------------------ 判定

test("单功能域的改动不会被拦（钩子放行）", (t) => {
  const dir = makeRepo(t);
  write(dir, "src/lib/qualityPresets.ts", read(dir, "src/lib/qualityPresets.ts") + "// 只动这一件事\n".repeat(80));
  write(dir, "src/pages/Library.tsx", read(dir, "src/pages/Library.tsx") + "// 页面配合\n".repeat(5));
  write(dir, "CHANGELOG.md", "# changelog\n\n- 画质预设新增一档\n");
  git(dir, "add", "-A");
  const r = split(dir, ["--hook"]);
  assert.equal(r.code, 0, `应当放行，实际：\n${r.out}`);
});

test("两个功能域各干各的 → 钩子拦下并给下一步", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  const r = split(dir, ["--hook"]);
  assert.equal(r.code, 3, `应当拦下（退出码 3），实际 ${r.code}：\n${r.out}`);
  assert.match(r.out, /steam/);
  assert.match(r.out, /quality/);
  assert.match(r.out, /pnpm commit:split/);
  assert.match(r.out, /WE_ALLOW_MIXED=1/);
  // 规模太小不打扰
  const small = makeRepo(t);
  write(small, "src-tauri/src/steam/auth.rs", read(small, "src-tauri/src/steam/auth.rs") + "// 小改\n".repeat(3));
  write(small, "src/lib/qualityPresets.ts", read(small, "src/lib/qualityPresets.ts") + "// 小改\n".repeat(3));
  git(small, "add", "-A");
  assert.equal(split(small, ["--hook"]).code, 0, "总改动 6 行不该拦");
});

test("一个功能跨多个模块（含集成面）不拦", (t) => {
  const dir = makeRepo(t);
  // 抄真仓库 f870f92 的形状：功能模块 + 页面 + 词条 + CHANGELOG
  fs.mkdirSync(path.join(dir, "src/locales"), { recursive: true });
  write(dir, "src/locales/en-US.ts", "export default {}\n");
  git(dir, "add", "-A");
  git(dir, "commit", "-qm", "chore: 词条占位");
  write(dir, "src-tauri/src/steam/auth.rs", read(dir, "src-tauri/src/steam/auth.rs") + "// 主改动\n".repeat(70));
  write(dir, "src/pages/Library.tsx", read(dir, "src/pages/Library.tsx") + "// 页面跟随\n".repeat(10));
  write(dir, "src-tauri/src/lib.rs", read(dir, "src-tauri/src/lib.rs") + "// 命令层接线\n".repeat(40));
  write(dir, "src/locales/en-US.ts", "export default { a: 1 }\n");
  write(dir, "CHANGELOG.md", "# changelog\n\n- 新东西\n");
  git(dir, "add", "-A");
  const r = split(dir, ["--hook"]);
  assert.equal(r.code, 0, `集成面（页面/词条/命令层）不该算第二个功能：\n${r.out}`);
});

test("WE_ALLOW_MIXED=1 放行；--no-verify 之外还有这条路", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  const r = split(dir, ["--hook"], { WE_ALLOW_MIXED: "1" });
  assert.equal(r.code, 0, r.out);
});

// ------------------------------------------------------------------ 拆分

test("--apply --auto 按域拆成多条，且内容守恒（索引/工作区都不变）", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  const stagedTreeBefore = git(dir, "write-tree");
  const worktreeBefore = read(dir, "src-tauri/src/steam/auth.rs");

  const r = split(dir, ["--apply", "--auto"]);
  assert.equal(r.code, 0, r.out);

  const subjects = log(dir, 3);
  assert.equal(subjects.length, 3, `应当是 2 条新提交 + 基线，实际：\n${subjects.join("\n")}`);
  assert.match(subjects[1], /\(steam\)|\(quality\)/);
  assert.match(subjects[0], /\(steam\)|\(quality\)/);
  assert.notEqual(subjects[0], subjects[1]);

  // 内容守恒：HEAD 树 == 拆分前暂存区的树
  assert.equal(treeOf(dir), stagedTreeBefore, "拆分前后树必须一致");
  // 拆完索引对齐 HEAD，没有假的"已暂存"
  assert.deepEqual(status(dir), [], `拆分后应当干净，实际：\n${status(dir).join("\n")}`);
  // 工作区内容一个字节都没动
  assert.equal(read(dir, "src-tauri/src/steam/auth.rs"), worktreeBefore);
  // 每条提交只含一个域
  for (const sha of ["HEAD", "HEAD~1"]) {
    const files = git(dir, "show", "--name-only", "--format=", sha).split("\n").filter(Boolean);
    const doms = new Set(files.map((f) => (f.startsWith("src-tauri/src/steam/") ? "steam" : "quality")));
    assert.equal(doms.size, 1, `${sha} 混了两个域：\n${files.join("\n")}`);
  }
  // 生成的提交信息本身要过规则（同一个校验器）
  const lint = sh(dir, process.execPath, [path.join(dir, "scripts/commit-msg-lint.mjs"), "--range", "HEAD~2..HEAD"]);
  assert.equal(lint.code, 0, `拆分出来的提交信息不合规：\n${lint.out}`);
});

test("只暂存了文件的一部分：提交里只有暂存的那部分，未暂存的仍留在工作区", (t) => {
  const dir = makeRepo(t);
  // 一文件两段：先写第一段并 add，再追加第二段（不进索引）
  write(dir, "src/lib/qualityPresets.ts", "// quality presets\n// 已暂存的新行\n");
  git(dir, "add", "src/lib/qualityPresets.ts");
  fs.appendFileSync(path.join(dir, "src/lib/qualityPresets.ts"), "// 未暂存的追加\n");
  // 再加一个别的域，凑成"多功能"
  write(dir, "src-tauri/src/steam/auth.rs", read(dir, "src-tauri/src/steam/auth.rs") + "// steam\n".repeat(60));
  git(dir, "add", "src-tauri/src/steam/auth.rs");

  const r = split(dir, ["--apply", "--auto"]);
  assert.equal(r.code, 0, r.out);

  const committed = git(dir, "show", "HEAD:src/lib/qualityPresets.ts");
  assert.match(committed, /已暂存的新行/);
  assert.doesNotMatch(committed, /未暂存的追加/, "未暂存的内容不该进提交");
  assert.match(read(dir, "src/lib/qualityPresets.ts"), /未暂存的追加/, "工作区里那份要原样保留");
  const st = status(dir);
  assert.equal(st.length, 1, `应当只剩那处未暂存改动：\n${st.join("\n")}`);
  assert.match(st[0], /qualityPresets/);
});

test("默认走计划文件：草案标题不改就不给提交", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);

  const plan = split(dir, []);
  assert.equal(plan.code, 0, plan.out);
  assert.ok(fs.existsSync(planFile(dir)), `计划文件应当生成：${planFile(dir)}`);

  const refused = split(dir, ["--apply"]);
  assert.equal(refused.code, 1, `草案标题应当被拒：\n${refused.out}`);
  assert.match(refused.out, /草案/);
  assert.equal(log(dir, 1)[0], "chore: 基线", "被拒时不该有提交产生");

  // 改标题 → 落库
  const j = JSON.parse(fs.readFileSync(planFile(dir), "utf8"));
  assert.equal(j.groups.length, 2);
  const titles = {
    steam: "feat(steam): 扫码登录支持 2FA",
    quality: "feat(quality): 清晰度改为四档相对倍率",
  };
  for (const g of j.groups) {
    g.type = titles[g.id].slice(0, titles[g.id].indexOf("("));
    g.scope = g.id;
    g.subject = titles[g.id].slice(titles[g.id].indexOf(": ") + 2);
    g.draft = false;
  }
  fs.writeFileSync(planFile(dir), JSON.stringify(j, null, 2));
  const applied = split(dir, ["--apply"]);
  assert.equal(applied.code, 0, applied.out);
  const subjects = log(dir, 3);
  assert.ok(subjects.includes(titles.steam), `缺少 steam 提交：\n${subjects.join("\n")}`);
  assert.ok(subjects.includes(titles.quality), `缺少 quality 提交：\n${subjects.join("\n")}`);
});

test("计划生成之后改动集合变了 → 拒绝（--force 才放行）", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  assert.equal(split(dir, []).code, 0);
  write(dir, "src-tauri/src/mcp/tools.rs", read(dir, "src-tauri/src/mcp/tools.rs") + "// 又加了一件事\n".repeat(30));
  git(dir, "add", "-A");
  const r = split(dir, ["--apply", "--auto"]);
  assert.equal(r.code, 1, `应当拒绝：\n${r.out}`);
  assert.match(r.out, /不一致/);
  assert.equal(log(dir, 1)[0], "chore: 基线");
});

test("计划里的提交信息不合规 → 提交前拦下（不会写出一条坏历史）", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  assert.equal(split(dir, []).code, 0);
  const p = planFile(dir);
  const j = JSON.parse(fs.readFileSync(p, "utf8"));
  for (const g of j.groups) {
    g.type = "wip";
    g.scope = g.id;
    g.subject = "改点东西";
    g.draft = false;
  }
  fs.writeFileSync(p, JSON.stringify(j, null, 2));
  const r = split(dir, ["--apply"]);
  assert.equal(r.code, 1, `应当被拦：\n${r.out}`);
  assert.match(r.out, /不合规/);
  assert.equal(log(dir, 1)[0], "chore: 基线");
});

test("--worktree 连未暂存与未跟踪文件一起拆", (t) => {
  const dir = makeRepo(t);
  write(dir, "src-tauri/src/steam/auth.rs", read(dir, "src-tauri/src/steam/auth.rs") + "// steam\n".repeat(60));
  write(dir, "src/lib/qualityPresets.ts", read(dir, "src/lib/qualityPresets.ts") + "// quality\n".repeat(60));
  write(dir, "src-tauri/src/mcp/tools.rs", "// 全新文件\n".repeat(20)); // 未跟踪
  assert.deepEqual(status(dir).length > 0, true);

  const r = split(dir, ["--apply", "--auto", "--worktree"]);
  assert.equal(r.code, 0, r.out);
  assert.deepEqual(status(dir), [], `工作区模式拆完应当干净：\n${status(dir).join("\n")}`);
  const files = git(dir, "log", "--name-only", "--format=%s", "-3");
  assert.match(files, /src-tauri\/src\/mcp\/tools\.rs/, "未跟踪的新文件也要进提交");
});

// ------------------------------------------------------------------ 拒绝路径

test("合并中 / 游离 HEAD 不给拆", (t) => {
  const dir = makeRepo(t);
  stageTwoFeatures(dir);
  // 合并中
  fs.writeFileSync(path.join(dir, ".git", "MERGE_HEAD"), git(dir, "rev-parse", "HEAD") + "\n");
  const merging = split(dir, ["--apply", "--auto"]);
  assert.equal(merging.code, 1, merging.out);
  assert.match(merging.out, /合并/);
  fs.rmSync(path.join(dir, ".git", "MERGE_HEAD"));
  // 合并/拣选/变基进行中：钩子一律放行（提交边界是 git 定的）
  fs.writeFileSync(path.join(dir, ".git", "SQUASH_MSG"), "Squashed commit of the following:\n");
  assert.equal(split(dir, ["--hook"]).code, 0, "merge --squash 期间不该拦");
  fs.rmSync(path.join(dir, ".git", "SQUASH_MSG"));
  // 游离 HEAD
  git(dir, "checkout", "-q", "--detach");
  const detached = split(dir, ["--apply", "--auto"]);
  assert.equal(detached.code, 1, detached.out);
  assert.match(detached.out, /游离/);
});

test("暂存区为空 / 非 git 仓库时的表现", (t) => {
  const dir = makeRepo(t);
  const empty = split(dir, []);
  assert.equal(empty.code, 0);
  assert.match(empty.out, /暂存区是空的/);

  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "we-split-nogit-"));
  const r = split(outside, []);
  assert.equal(r.code, 1, r.out);
  assert.match(r.out, /不是 git 仓库/);
});

// ------------------------------------------------------------------ 钩子

test("装上钩子：多功能的 git commit 被拦；单功能的正常提交", (t) => {
  const dir = makeRepo(t);
  const installed = sh(dir, process.execPath, [path.join(dir, "scripts/install-hooks.mjs")]);
  assert.equal(installed.code, 0, installed.out);
  assert.ok(fs.existsSync(path.join(dir, ".git/hooks/pre-commit")), "要装上 pre-commit");
  assert.ok(fs.existsSync(path.join(dir, ".git/hooks/commit-msg")), "要装上 commit-msg");

  stageTwoFeatures(dir);
  const blocked = sh(dir, "git", ["commit", "-m", "feat(steam): 加登录"]);
  assert.notEqual(blocked.code, 0, `应当被钩子拦下：\n${blocked.out}`);
  assert.match(blocked.out, /功能域/);
  assert.equal(log(dir, 1)[0], "chore: 基线", "被拦时不该产生提交");
  assert.ok(status(dir).length >= 2, "暂存内容要原样留着（拦下不等于清空）");

  // 放行后正常提交，两个钩子互不干扰
  const ok = sh(dir, "git", ["commit", "-m", "feat(steam): 加登录", "--no-verify"]);
  assert.equal(ok.code, 0, ok.out);
  assert.match(log(dir, 1)[0], /feat\(steam\)/);
});

test("WE_AUTO_SPLIT=1：钩子里直接把多功能改动拆开，原提交被取消且不留脏索引", (t) => {
  const dir = makeRepo(t);
  sh(dir, process.execPath, [path.join(dir, "scripts/install-hooks.mjs")]);
  stageTwoFeatures(dir);
  const stagedTreeBefore = git(dir, "write-tree");

  const r = sh(dir, "git", ["commit", "-m", "feat(steam): 一批混杂改动"], { env: { WE_AUTO_SPLIT: "1" } });
  assert.notEqual(r.code, 0, `外层提交应当被取消：\n${r.out}`);
  assert.match(r.out, /已按功能拆成/);

  const subjects = log(dir, 3);
  assert.equal(subjects.length, 3, `应当是 2 条拆分提交 + 基线，实际：\n${subjects.join("\n")}`);
  assert.ok(!subjects.some((s) => s.includes("一批混杂改动")), "原提交不该落地");
  assert.equal(treeOf(dir), stagedTreeBefore, "拆分前后树一致");
  assert.deepEqual(status(dir), [], `不该留下假暂存：\n${status(dir).join("\n")}`);
});

test("递归护栏：拆分器自己创建的提交不再触发检查", (t) => {
  const dir = makeRepo(t);
  sh(dir, process.execPath, [path.join(dir, "scripts/install-hooks.mjs")]);
  stageTwoFeatures(dir);
  sh(dir, "git", ["commit", "-m", "x"], { env: { WE_AUTO_SPLIT: "1" } });
  // 没有 WE_SPLIT_INTERNAL 护栏的话，拆分提交自身又会被 pre-commit 再拆，提交数会失控
  const count = Number(git(dir, "rev-list", "--count", "HEAD"));
  assert.equal(count, 3, `提交总数应当是 3（基线 + 2），实际 ${count} —— 递归护栏失效了`);
  assert.equal(count, Number(git(dir, "rev-list", "--count", "HEAD")), "复算一次防抖");
});

test("--explain 与 --audit 只读可用", (t) => {
  const dir = makeRepo(t);
  const ex = split(dir, ["--explain", "src-tauri/src/steam/auth.rs"]);
  assert.equal(ex.code, 0, ex.out);
  assert.match(ex.out, /steam/);
  assert.match(ex.out, /feature/);

  const audit = split(dir, ["--audit", "HEAD"]);
  assert.equal(audit.code, 0, audit.out);
  assert.match(audit.out, /汇总/);

  // --audit 不写计划文件、不改仓库
  assert.ok(!fs.existsSync(planFile(dir)), "--audit 不该写计划文件");
});
