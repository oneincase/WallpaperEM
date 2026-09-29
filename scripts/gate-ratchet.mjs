#!/usr/bin/env node
// 门禁棘轮（ratchet）—— rustfmt / clippy 的「不新增」闸门。
//
// 为什么不是「一把梭的阻断」：这两项的基线本来就不干净 —— 全仓统一格式化、逐条修
// clippy 都是**独立一件事**（§5），设成阻断只会让每个 PR 都红，然后所有人学会无视
// CI（§6 的分档原则就是这么写的）。
//
// 为什么也不只是「报告」：报告档没人看。基线从文档里写的 604 处自己掉到 551，不是
// 因为有东西在守，是因为有人碰巧改到了 —— 没人会发现它其实还在涨。
//
// 所以：**基线进仓库（scripts/gate-baseline.json）**，当次测量值比基线差 → 阻断；
// 比基线好 → 提示收紧（`pnpm gate:ratchet --update`）。基线只许收紧，不许放宽。
//
// 漂移处理（重要，别当 bug 修）：
//   · CI 用 `dtolnay/rust-toolchain@stable`，工具链是浮动的：新版 rustfmt/clippy 会
//     带来与代码无关的数值变化。基线里记了工具链版本，**版本对不上时当次放行并打印
//     新数字**，让人跑 `--update` 收进去 —— 拿旧尺子量新工具的读数没有意义。
//   · 平台同理：基线按 `os-arch` 存，`null` = 该平台还没测过（首次运行放行并打印数字）。
//
// 单元测试里不跑真 cargo：测量函数接受注入的 runner，比较逻辑是纯函数。
//
// 用法：
//   node scripts/gate-ratchet.mjs                 # 两项都测、都比
//   node scripts/gate-ratchet.mjs --only fmt      # 只测一项（CI 分步调用，Actions UI 里看得清）
//   node scripts/gate-ratchet.mjs --update        # 测量并把新基线写回 gate-baseline.json
//   node scripts/gate-ratchet.mjs --update --platform linux-x64 --toolchain "rustc 1.98.1 (…)" \
//        --fmt 551 --clippy 49                    # 跨平台写入：CI 打印的读数直接记到那个平台键下
//                                                  # （在 macOS 上跑 --update 只会写 darwin-arm64）
//   node scripts/gate-ratchet.mjs --help
// 退出码：0 通过（含「提示收紧」「未测量/工具链变了」的放行）　1 数值比基线差　2 用法错误
//         3 测量本身失败（cargo 跑不起来、编译错误等 —— 这时没有可信读数，别装作通过）

import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const SCRIPT_DIR = path.dirname(new URL(import.meta.url).pathname);
const ROOT = path.resolve(SCRIPT_DIR, "..");
export const BASELINE_PATH = path.join(SCRIPT_DIR, "gate-baseline.json");

const MANIFEST = path.join(ROOT, "src-tauri", "Cargo.toml");

/** 指标定义：测量方式 + 人话说明。新增指标只在这里加一项。 */
export const METRICS = {
  fmt: {
    label: "rustfmt 差异",
    hint: "清理它 = 全仓统一格式化，单独一条提交（§5），别混进功能改动",
    unit: "处",
  },
  clippy: {
    label: "clippy 警告",
    hint: "要求：本改动不新增警告；清理存量同样单独一条提交（§5）",
    unit: "条",
  },
};

// ------------------------------------------------------------------ 纯逻辑（可测）

/** `darwin-arm64` / `linux-x86_64` —— 基线按平台存，因为读数可能不同。 */
export function platformKey(os = process.platform, arch = process.arch) {
  return `${os}-${arch}`;
}

/**
 * 比一次测量结果。顺序有讲究：先判「这个数有没有可比性」，再判大小。
 * @returns {{verdict:"pass"|"notice"|"fail", message:string}}
 */
export function evaluate({ metric, value, baseline, toolchain, recordedToolchain }) {
  const m = METRICS[metric];
  if (value === null || value === undefined) {
    return { verdict: "pass", message: `${m.label} 测量失败（无读数）` };
  }
  if (baseline === null || baseline === undefined) {
    return {
      verdict: "notice",
      message:
        `该平台还没有基线读数（当前 ${value}${m.unit}）：` +
        `在这个平台上跑 \`pnpm gate:ratchet --update\`；或拿 CI 打印的读数回填 ——` +
        ` \`--update --platform <键> --toolchain "<rustc --version>" --fmt N --clippy N\`（在别的机器上跑 --update 只会写本机的键）`,
    };
  }
  if (recordedToolchain !== toolchain) {
    return {
      verdict: "notice",
      message: `工具链变了（基线 ${recordedToolchain ?? "未记"} → 现在 ${toolchain}），读数不可比 —— 当次放行；跑 \`pnpm gate:ratchet --update\` 记下新读数 ${value}${m.unit}`,
    };
  }
  if (value > baseline) {
    return {
      verdict: "fail",
      message: `当前 ${value}${m.unit} > 基线 ${baseline}${m.unit}（新增 ${value - baseline}${m.unit}）：${m.hint}`,
    };
  }
  if (value < baseline) {
    return {
      verdict: "notice",
      message: `当前 ${value}${m.unit} < 基线 ${baseline}${m.unit}（少了 ${baseline - value}${m.unit}）：可以收紧，跑 \`pnpm gate:ratchet --update\``,
    };
  }
  return { verdict: "pass", message: `当前 ${value}${m.unit} = 基线 ${baseline}${m.unit}` };
}

/** `--update` 用：只回写被测量的那几个指标，没测的保持原样（别把别的平台清成 null）。 */
export function mergeBaseline(baseline, { platform, toolchain, values }) {
  const next = structuredClone(baseline ?? {});
  next.platforms ??= {};
  const cur = structuredClone(next.platforms[platform] ?? {});
  cur.toolchain = toolchain;
  for (const [k, v] of Object.entries(values)) cur[k] = v;
  next.platforms[platform] = cur;
  return next;
}

// ------------------------------------------------------------------ 测量（可注入 runner）

/**
 * runner 契约：`(cmd, args) => {stdout, stderr, code}`（也接受直接返回 string）。
 * **非 0 退出不抛**——`cargo fmt --check` 有差异时就是退出 1，那是正常读数；
 * 只有 spawn 本身失败（没装 cargo）才会抛。测量失败的判定交给 `cargo()`。
 */
const defaultRunner = (cmd, args) => {
  try {
    const stdout = execFileSync(cmd, args, { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], maxBuffer: 256 * 1024 * 1024 });
    return { stdout, stderr: "", code: 0 };
  } catch (e) {
    if (e.code === "ENOENT") throw new Error(`${cmd} 不在 PATH —— 先装 Rust 工具链（rustup）`);
    return { stdout: e.stdout ?? "", stderr: e.stderr ?? "", code: e.status ?? 1 };
  }
};

/**
 * 跑一条 cargo 并按「哪些退出码算正常读数」判定。
 * allow 之外的退出码（编译错误、manifest 坏了…）说明读数不可信 —— 抛错，
 * 绝不能把「跑不起来」当「0 条差异」糊过去。
 */
function cargo(runner, args, { allow = [0] } = {}) {
  let r;
  try {
    r = runner("cargo", args);
  } catch (e) {
    throw new Error(`cargo 起不来（${args[0]}）：${e.message}`);
  }
  const out = typeof r === "string" ? r : `${r.stdout ?? ""}\n${r.stderr ?? ""}`;
  const code = typeof r === "string" ? 0 : (r.code ?? 0);
  if (!allow.includes(code)) {
    const tail = out
      .split("\n")
      .filter(Boolean)
      .slice(-6)
      .join("\n  ");
    throw new Error(`cargo ${args[0]} 退出码 ${code}，没有可信读数：\n  ${tail}`);
  }
  return out;
}

/** `cargo fmt --all --check` 的 diff 块数。退出码 1 = 有差异（正常读数）。 */
export function countFmtDiffs(runner = defaultRunner) {
  const out = cargo(runner, ["fmt", "--all", "--check", "--manifest-path", MANIFEST], { allow: [0, 1] });
  return (out.match(/^Diff in /gm) ?? []).length;
}

/**
 * 去重后的 clippy 诊断数。
 * `--all-targets` 会把同一处警告在 lib / lib-test 等 target 里各报一遍，不去重的话
 * 数字会随 target 数漂移 —— 按 `code@文件:行` 去重才是「有几处问题」的真实读数。
 */
export function countClippyWarnings(runner = defaultRunner) {
  const out = cargo(runner, ["clippy", "--all-targets", "--message-format=json", "--manifest-path", MANIFEST]);
  const seen = new Set();
  for (const line of out.split("\n")) {
    if (!line.startsWith("{")) continue;
    try {
      const o = JSON.parse(line);
      if (o.reason !== "compiler-message") continue;
      const code = o.message?.code?.code;
      if (!code || !code.startsWith("clippy::")) continue;
      const sp = o.message.spans?.find((x) => x.is_primary);
      seen.add(`${code}@${sp ? `${sp.file_name}:${sp.line_start}` : "?"}`);
    } catch {
      /* 非 JSON 行直接跳过 */
    }
  }
  return seen.size;
}

export function toolchainVersion(runner = defaultRunner) {
  try {
    const r = runner("rustc", ["--version"]);
    const out = typeof r === "string" ? r : (r.stdout ?? "");
    const v = out.trim();
    return v || null;
  } catch {
    return null;
  }
}

const MEASURERS = { fmt: countFmtDiffs, clippy: countClippyWarnings };

// ------------------------------------------------------------------ 输出

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const red = (s) => c("31", s);
const green = (s) => c("32", s);
const yellow = (s) => c("33", s);
const dim = (s) => c("2", s);
const bold = (s) => c("1", s);

/** CI 里要出注解（红/黄卡片），本地就只是颜色。 */
function annotate(level, title, msg) {
  if (process.env.GITHUB_ACTIONS !== "true") return;
  console.log(`::${level} title=${title}::${msg.replace(/\r?\n/g, " ")}`);
}

function loadBaseline() {
  if (!fs.existsSync(BASELINE_PATH)) return { platforms: {} };
  try {
    return JSON.parse(fs.readFileSync(BASELINE_PATH, "utf8"));
  } catch (e) {
    throw new Error(`基线文件损坏（${BASELINE_PATH}）：${e.message}`);
  }
}

function usage() {
  const src = fs.readFileSync(path.join(SCRIPT_DIR, "gate-ratchet.mjs"), "utf8");
  console.log(
    src
      .split("\n")
      .filter((l) => /^\/\/|^$/.test(l))
      .slice(0, 30)
      .join("\n")
      .replace(/^\/\/ ?/gm, ""),
  );
}

// ------------------------------------------------------------------ 入口

function main(argv, runner = defaultRunner) {
  const opts = { only: null, update: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--only") {
      opts.only = argv[++i];
      if (!METRICS[opts.only]) {
        console.error(red(`✗ --only 只能是 ${Object.keys(METRICS).join(" / ")}`));
        return 2;
      }
    } else if (a === "--update") opts.update = true;
    else if (a === "--platform") opts.platform = argv[++i];
    else if (a === "--toolchain") opts.toolchain = argv[++i];
    else if (a === "--fmt" || a === "--clippy") {
      const n = Number(argv[++i]);
      if (!Number.isInteger(n) || n < 0) {
        console.error(red(`✗ ${a} 要一个非负整数`));
        return 2;
      }
      (opts.values ??= {})[a.slice(2)] = n;
    } else if (a === "--help" || a === "-h") return usage(), 0;
    else {
      console.error(red(`✗ 未知选项：${a}（--help 看用法）`));
      return 2;
    }
  }

  // 跨平台回填：拿 CI 打印的读数直接写到那个平台键下（不跑 cargo，秒回）。
  // 少一个前置参数就报用法错误 —— 静默写到错误的键下比不写更糟。
  if (opts.values) {
    const missing = ["update", "platform", "toolchain"].filter((k) => !opts[k]);
    if (missing.length || Object.keys(opts.values).length !== Object.keys(METRICS).length) {
      console.error(red(`✗ 用 \`--fmt N --clippy N\` 回填时必须同时给 --update --platform <键> --toolchain "<rustc --version>"（两项都要给）`));
      return 2;
    }
    const baseline = loadBaseline();
    const next = mergeBaseline(baseline, { platform: opts.platform, toolchain: opts.toolchain, values: opts.values });
    fs.writeFileSync(BASELINE_PATH, JSON.stringify(next, null, 2) + "\n");
    console.log(
      `${green("✓")} 已回填 ${opts.platform}：${Object.entries(opts.values).map(([k, v]) => `${k}=${v}`).join(" ")}　toolchain=${opts.toolchain}`,
    );
    console.log(dim(`  写进了 ${path.relative(process.cwd(), BASELINE_PATH)}，记得随改动一起提交。`));
    return 0;
  }

  const platform = platformKey();
  const baseline = loadBaseline();
  const recorded = baseline.platforms?.[platform] ?? {};
  const toolchain = toolchainVersion(runner);
  const names = opts.only ? [opts.only] : Object.keys(METRICS);

  console.log(bold(`棘轮基线`) + dim(`　platform=${platform}　toolchain=${toolchain ?? "?"}`));
  if (!baseline.platforms?.[platform]) {
    console.log(dim(`  ${platform} 还没记过基线，本次测量只打印不拦（见上面 evaluate 的回填办法）`));
  }

  const values = {};
  let worst = 0;
  for (const name of names) {
    let value;
    try {
      value = MEASURERS[name](runner);
    } catch (e) {
      console.log(`  ${red("✗")} ${METRICS[name].label} ${red("测量失败")} —— ${e.message}`);
      annotate("error", `${METRICS[name].label} 测量失败`, String(e.message).slice(0, 300));
      worst = Math.max(worst, 3);
      continue;
    }
    values[name] = value;
    const v = evaluate({
      metric: name,
      value,
      baseline: recorded[name],
      toolchain,
      recordedToolchain: recorded.toolchain,
    });
    const mark = v.verdict === "fail" ? red("✗") : v.verdict === "notice" ? yellow("!") : green("✓");
    console.log(`  ${mark} ${METRICS[name].label.padEnd(12)} ${v.message}`);
    if (v.verdict === "fail") {
      annotate("error", `门禁棘轮：${METRICS[name].label} 超出基线`, v.message);
      worst = Math.max(worst, 1);
    } else if (v.verdict === "notice") {
      annotate("notice", `门禁棘轮：${METRICS[name].label}`, v.message);
    }
  }

  if (opts.update && Object.keys(values).length) {
    const next = mergeBaseline(baseline, { platform, toolchain, values });
    fs.writeFileSync(BASELINE_PATH, JSON.stringify(next, null, 2) + "\n");
    console.log(`  ${green("✓")} 基线已写回 ${path.relative(ROOT, BASELINE_PATH)}：${Object.entries(values).map(([k, v]) => `${k}=${v}`).join(" ")}`);
    console.log(dim("    记得把改动一起提交 —— 基线在仓库里才对所有人（和 CI）生效。"));
  }
  console.log("");
  return worst;
}

const isDirectRun = process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url;
if (isDirectRun) process.exit(main(process.argv.slice(2)));
