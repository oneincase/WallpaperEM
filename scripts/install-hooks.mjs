#!/usr/bin/env node
// 安装 / 卸载本仓库的本地钩子（提交信息规则 + 多功能改动拦截）。
//
// 为什么需要这个脚本，而不是直接往 .git/hooks 里丢文件：
//   1. .git/hooks 不受版本控制 —— 新克隆的仓库没有钩子，必须有个可重跑的安装入口
//      （package.json 的 `prepare` 会调本脚本，`pnpm install` 后自动就位）；
//   2. 本仓库的 post-checkout / post-commit / post-merge / pre-push 已被 **git-lfs 占用**，
//      脚本绝不触碰它们，只接管自己的两个：commit-msg 与 pre-commit；
//   3. 已经存在别人写的同名钩子时不能默默覆盖 —— 先备份再替换。
//
// 装的两个：
//   commit-msg  → scripts/commit-msg-lint.mjs          提交信息形状（`type(scope): 主题`）
//   pre-commit  → scripts/repo-hygiene.mjs --staged    §9 违禁物（先跑：纯 git、毫秒级）
//               → scripts/split-commits.mjs --hook     §2 多功能改动拦截
//
// 用法：
//   node scripts/install-hooks.mjs              # 安装（幂等，可重复跑）
//   node scripts/install-hooks.mjs --uninstall  # 卸载（有备份则自动还原）
//   node scripts/install-hooks.mjs --status     # 看当前装了什么
//   node scripts/install-hooks.mjs --if-possible # 供 `prepare` 调用：不在 git 仓库里就静默跳过
//   node scripts/install-hooks.mjs --force      # 覆盖非本脚本安装的同名钩子（会先备份）
//
// 钩子里**不写死** node 的绝对路径：GUI 客户端（VS Code / Tower / GitHub Desktop）
// 启动 git 时的 PATH 往往没有 nvm 的 node，写死 DSH 自带的 node 路径更会在升级后失效。
// 因此钩子在运行时自己找 node，找不到就放行并提示 —— 钩子是"帮你不犯错"，
// 不该因为环境缺 node 就把提交卡死；真正的强制由 CI 兜底（提交信息那部分）。

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const MARKER_PREFIX = "# WE-HOOK-MANAGED";

/**
 * 本脚本接管的钩子。lfs 的那四个（post-*、pre-push）不在这里，也不许加进来。
 * 一个钩子可以串多条命令（`runs`，按序执行，任一失败即停）—— pre-commit 就串了两道
 * 检查：先跑纯 git、毫秒级的违禁物检查，再跑提交粒度检查。
 */
const HOOKS = [
  {
    name: "commit-msg",
    what: "提交信息校验",
    hint: '试跑：node scripts/commit-msg-lint.mjs --message "fix(ui): 修按钮"',
    runs: [{ script: "scripts/commit-msg-lint.mjs", args: '"$1"' }],
  },
  {
    name: "pre-commit",
    what: "违禁物检查 + 多功能改动拦截",
    hint: "试跑：node scripts/repo-hygiene.mjs --staged　·　node scripts/split-commits.mjs --hook　规则：docs/pr-rules.md §2 §9",
    runs: [
      { script: "scripts/repo-hygiene.mjs", args: "--staged" },
      { script: "scripts/split-commits.mjs", args: "--hook" },
    ],
  },
];

const allScripts = (hook) => hook.runs.map((r) => r.script);

const argv = process.argv.slice(2);
const flag = (name) => argv.includes(name);

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const say = (msg) => console.log(msg);
const warn = (msg) => console.warn(c("33", msg));

function git(...args) {
  return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

/** 仓库根 + 钩子目录（尊重 core.hooksPath 与 worktree）。 */
function locate() {
  try {
    const root = git("rev-parse", "--show-toplevel");
    const hooksPath = git("rev-parse", "--git-path", "hooks");
    return { root, hooksDir: path.isAbsolute(hooksPath) ? hooksPath : path.resolve(root, hooksPath) };
  } catch {
    return null;
  }
}

function hookBody(hook) {
  // 每条检查单独 `[ -f ]` 判存在（部分检出时跳过而不是把提交卡死），最后一条用 exec
  // 接管进程 —— 这样钩子的退出码就是检查的退出码，也省掉一层 shell。
  const checks = hook.runs
    .map((r, i) => {
      const cmd = `"$node_bin" "$root/${r.script}" ${r.args}`;
      const invoke = i === hook.runs.length - 1 ? `exec ${cmd}` : `${cmd} || exit $?`;
      return `[ -f "$root/${r.script}" ] || exit 0\n${invoke}`;
    })
    .join("\n\n");
  return `#!/bin/sh
${MARKER_PREFIX}: ${hook.name}
# 由 scripts/install-hooks.mjs 生成，请勿手工编辑（改动会被下次安装覆盖）。
# 规则见 docs/pr-rules.md；绕过：git commit --no-verify
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0

node_bin="\${WE_NODE:-$(command -v node 2>/dev/null)}"
if [ -z "$node_bin" ] || [ ! -x "$node_bin" ]; then
  echo "提示：未找到 node，跳过${hook.what}（CI 侧仍会校验提交信息）" >&2
  exit 0
fi

${checks}
`;
}

function isManaged(file) {
  try {
    return fs.readFileSync(file, "utf8").includes(MARKER_PREFIX);
  } catch {
    return false;
  }
}

function install() {
  const loc = locate();
  if (!loc) {
    if (flag("--if-possible")) return 0;
    warn("当前目录不是 git 仓库，无法安装钩子");
    return 1;
  }
  const { root, hooksDir } = loc;
  const missing = HOOKS.flatMap((h) => allScripts(h).map((s) => ({ hook: h.name, script: s }))).filter(
    ({ script }) => !fs.existsSync(path.join(root, script)),
  );
  if (missing.length) {
    warn(`找不到 ${missing.map((m) => m.script).join("、")}，跳过安装（是不是只拷了部分文件？）`);
    return flag("--if-possible") ? 0 : 1;
  }

  fs.mkdirSync(hooksDir, { recursive: true });

  const blocked = [];
  for (const hook of HOOKS) {
    const target = path.join(hooksDir, hook.name);
    // 已有非本脚本的钩子：备份后替换（除非只想看看状态）
    if (fs.existsSync(target) && !isManaged(target)) {
      if (!flag("--force")) {
        blocked.push(hook.name);
        continue;
      }
      const backup = `${target}.bak-${Date.now()}`;
      fs.copyFileSync(target, backup);
      warn(`已备份原有 ${hook.name} 到 ${c("2", path.relative(root, backup))}`);
    }
    fs.writeFileSync(target, hookBody(hook), { mode: 0o755 });
    fs.chmodSync(target, 0o755);
    say(`${c("32", "✓")} 已安装 ${c("1", path.relative(root, target))}（${hook.what}）`);
    say(c("2", `  ${hook.hint}`));
  }

  if (blocked.length) {
    warn(`${blocked.map((n) => path.join(path.relative(root, hooksDir), n)).join("、")} 已存在且不是本脚本安装的：`);
    say(`  ${c("2", "先看一眼它做了什么，再决定：")}`);
    say(`  ${c("2", "node scripts/install-hooks.mjs --force   # 备份原文件后替换（原文件保留为 <钩子名>.bak-<时间戳>）")}`);
    return 1;
  }
  return 0;
}

function uninstall() {
  const loc = locate();
  if (!loc) {
    if (flag("--if-possible")) return 0;
    warn("当前目录不是 git 仓库");
    return 1;
  }
  const { root, hooksDir } = loc;
  let removed = 0;
  for (const hook of HOOKS) {
    const target = path.join(hooksDir, hook.name);
    if (!fs.existsSync(target)) continue;
    if (!isManaged(target)) {
      warn(`${path.relative(root, target)} 不是本脚本安装的，未做改动`);
      continue;
    }
    fs.rmSync(target);
    removed++;
    say(`${c("32", "✓")} 已移除 ${path.relative(root, target)}`);

    // 还原最近一次备份
    const backups = fs
      .readdirSync(hooksDir)
      .filter((f) => f.startsWith(`${hook.name}.bak-`))
      .sort();
    const last = backups.at(-1);
    if (last) {
      fs.copyFileSync(path.join(hooksDir, last), target);
      fs.chmodSync(target, 0o755);
      say(c("2", `  已还原备份 ${last}`));
    }
  }
  if (!removed) say(c("2", "没有安装钩子，无需卸载"));
  return 0;
}

function status() {
  const loc = locate();
  if (!loc) {
    say("当前目录不是 git 仓库");
    return 1;
  }
  for (const hook of HOOKS) {
    const target = path.join(loc.hooksDir, hook.name);
    const rel = path.relative(loc.root, target);
    if (!fs.existsSync(target)) say(`${c("33", "未安装")} ${hook.name} 钩子（${rel}）`);
    else if (isManaged(target)) say(`${c("32", "已安装")} ${hook.name} 钩子 → ${allScripts(hook).join(" → ")}（${hook.what}）`);
    else say(`${c("33", "存在")} 非本脚本管理的 ${hook.name} 钩子`);
  }

  // git-lfs 占用的钩子列出来，避免误以为本脚本漏装
  const lfs = ["post-checkout", "post-commit", "post-merge", "pre-push"].filter((h) =>
    fs.existsSync(path.join(loc.hooksDir, h)),
  );
  if (lfs.length) say(c("2", `  （${lfs.join(" / ")} 由 git-lfs 占用，本脚本不碰）`));
  return 0;
}

if (flag("--uninstall")) process.exit(uninstall());
else if (flag("--status")) process.exit(status());
else process.exit(install());
