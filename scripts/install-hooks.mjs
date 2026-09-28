#!/usr/bin/env node
// 安装 / 卸载本仓库的 commit-msg 钩子（提交信息规则校验）。
//
// 为什么需要这个脚本，而不是直接往 .git/hooks 里丢文件：
//   1. .git/hooks 不受版本控制 —— 新克隆的仓库没有钩子，必须有个可重跑的安装入口
//      （package.json 的 `prepare` 会调本脚本，`pnpm install` 后自动就位）；
//   2. 本仓库的 post-checkout / post-commit / post-merge / pre-push 已被 **git-lfs 占用**，
//      脚本绝不触碰它们，只接管 commit-msg；
//   3. 已经存在别人写的 commit-msg 钩子时不能默默覆盖 —— 先备份再替换。
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
// 不该因为环境缺 node 就把提交卡死；真正的强制由 CI 兜底。

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";

const MARKER = "# WE-HOOK-MANAGED: commit-msg";
const HOOK_NAME = "commit-msg";
const SCRIPT_REL = "scripts/commit-msg-lint.mjs";

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

function hookBody() {
  return `#!/bin/sh
${MARKER}
# 由 scripts/install-hooks.mjs 生成，请勿手工编辑（改动会被下次安装覆盖）。
# 规则见 docs/pr-rules.md；绕过：git commit --no-verify
root=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
script="$root/${SCRIPT_REL}"
[ -f "$script" ] || exit 0

node_bin="\${WE_NODE:-$(command -v node 2>/dev/null)}"
if [ -z "$node_bin" ] || [ ! -x "$node_bin" ]; then
  echo "提示：未找到 node，跳过提交信息校验（CI 侧仍会校验）" >&2
  exit 0
fi

exec "$node_bin" "$script" "$1"
`;
}

function isManaged(file) {
  try {
    return fs.readFileSync(file, "utf8").includes(MARKER);
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
  const target = path.join(hooksDir, HOOK_NAME);

  if (!fs.existsSync(path.join(root, SCRIPT_REL))) {
    warn(`找不到 ${SCRIPT_REL}，跳过安装（是不是只拷了部分文件？）`);
    return flag("--if-possible") ? 0 : 1;
  }

  fs.mkdirSync(hooksDir, { recursive: true });

  // 已有非本脚本的钩子：备份后替换（除非只想看看状态）
  if (fs.existsSync(target) && !isManaged(target)) {
    if (!flag("--force")) {
      warn(`${target} 已存在且不是本脚本安装的：`);
      say(`  ${c("2", "先看一眼它做了什么，再决定：")}`);
      say(`  ${c("2", `node scripts/install-hooks.mjs --force   # 备份原文件后替换（原文件保留为 ${HOOK_NAME}.bak-<时间戳>）`)}`);
      return 1;
    }
    const backup = `${target}.bak-${Date.now()}`;
    fs.copyFileSync(target, backup);
    warn(`已备份原有钩子到 ${c("2", path.relative(root, backup))}`);
  }

  fs.writeFileSync(target, hookBody(), { mode: 0o755 });
  fs.chmodSync(target, 0o755);
  say(`${c("32", "✓")} 已安装 ${c("1", path.relative(root, target))}（提交时校验提交信息）`);
  say(c("2", `  规则：docs/pr-rules.md　试跑：node ${SCRIPT_REL} --message "fix(ui): 修按钮"`));
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
  const target = path.join(hooksDir, HOOK_NAME);
  if (!fs.existsSync(target)) {
    say(c("2", "没有安装钩子，无需卸载"));
    return 0;
  }
  if (!isManaged(target)) {
    warn(`${path.relative(root, target)} 不是本脚本安装的，未做改动`);
    return 1;
  }
  fs.rmSync(target);
  say(`${c("32", "✓")} 已移除 ${path.relative(root, target)}`);

  // 还原最近一次备份
  const backups = fs
    .readdirSync(hooksDir)
    .filter((f) => f.startsWith(`${HOOK_NAME}.bak-`))
    .sort();
  const last = backups.at(-1);
  if (last) {
    fs.copyFileSync(path.join(hooksDir, last), target);
    fs.chmodSync(target, 0o755);
    say(c("2", `  已还原备份 ${last}`));
  }
  return 0;
}

function status() {
  const loc = locate();
  if (!loc) {
    say("当前目录不是 git 仓库");
    return 1;
  }
  const target = path.join(loc.hooksDir, HOOK_NAME);
  if (!fs.existsSync(target)) say(`${c("33", "未安装")} commit-msg 钩子（${path.relative(loc.root, target)}）`);
  else if (isManaged(target)) say(`${c("32", "已安装")} commit-msg 钩子 → ${SCRIPT_REL}`);
  else say(`${c("33", "存在")} 非本脚本管理的 commit-msg 钩子`);

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
