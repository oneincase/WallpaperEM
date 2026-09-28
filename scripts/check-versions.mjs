#!/usr/bin/env node
// 版本号一致性检查 —— 本项目的版本号散在四处，漏改一处就会出现「关于」页、
// 安装包、更新器 manifest 三者互相打架（历史上发生过，见 bin-info.plist 的
// 1.1.0 与其余三处的 2.0.0）。
//
// 权威版本（三处必须一致，不一致即失败）：
//   · package.json                    前端包版本
//   · src-tauri/tauri.conf.json       安装包版本（打包实际用这个）
//   · src-tauri/Cargo.toml            Rust crate 版本
// 建议同步（唯一真源之外的副本，不一致只提示）：
//   · src-tauri/bin-info.plist        两个键：CFBundleShortVersionString 与
//                                     CFBundleVersion（后者是构建号，本项目历来与版本号
//                                     同值；打包版用 tauri.conf.json，所以这里不一致只影响
//                                     `pnpm tauri dev` 出来的 app 显示的版本号）。历史上
//                                     专门发过一条 chore 同步它，故仍纳入检查。
//                                     两个键都要查 —— 只查一个会漏掉另一个的漂移。
//
// 用法：node scripts/check-versions.mjs   （全部一致退出 0，权威版本不一致退出 1）

import fs from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");

const useColor = process.stdout.isTTY && !process.env.NO_COLOR;
const c = (code, s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);

function readVersion(label, file, extract) {
  const abs = path.join(ROOT, file);
  if (!fs.existsSync(abs)) return { label, file, version: null, note: "文件不存在" };
  const m = extract(fs.readFileSync(abs, "utf8"));
  return { label, file, version: m ?? null, note: m ? null : "没解析出版本号" };
}

const authoritative = [
  readVersion("package.json", "package.json", (s) => JSON.parse(s).version),
  readVersion("tauri.conf.json", "src-tauri/tauri.conf.json", (s) => JSON.parse(s).version),
  readVersion("Cargo.toml", "src-tauri/Cargo.toml", (s) => {
    // 只认 [package] 段里的 version，别被依赖的 version = "..." 骗了
    const pkg = /^\[package\][\s\S]*?(?=^\[|\Z)/m.exec(s)?.[0] ?? "";
    return /^version\s*=\s*"([^"]+)"/m.exec(pkg)?.[1];
  }),
];

const advisory = [
  readVersion("bin-info.plist / CFBundleShortVersionString", "src-tauri/bin-info.plist", (s) =>
    /<key>CFBundleShortVersionString<\/key>\s*<string>([^<]+)<\/string>/.exec(s)?.[1],
  ),
  readVersion("bin-info.plist / CFBundleVersion", "src-tauri/bin-info.plist", (s) =>
    /<key>CFBundleVersion<\/key>\s*<string>([^<]+)<\/string>/.exec(s)?.[1],
  ),
];

const versions = authoritative.map((v) => v.version);
const allSame = versions.every((v) => v && v === versions[0]);

console.log("权威版本（必须一致）：");
for (const v of authoritative) {
  const bad = !v.version || v.version !== versions[0];
  const mark = bad ? c("31", "✗") : c("32", "✓");
  console.log(`  ${mark} ${v.version ?? "?"}  ${v.file}${v.note ? c("2", `（${v.note}）`) : ""}`);
}

const advisoryBad = [];
console.log("建议同步：");
for (const v of advisory) {
  const bad = !v.version || v.version !== versions[0];
  if (bad) advisoryBad.push(v.label);
  const mark = bad ? c("33", "!") : c("32", "✓");
  // 建议项用 label 显示（含具体的键名），否则同一文件的两个键会打印成两行一模一样的路径
  console.log(`  ${mark} ${v.version ?? "?"}  ${v.label}${v.note ? c("2", `（${v.note}）`) : ""}`);
}

if (!allSame) {
  console.error(c("31", `\n版本号不一致：权威三处必须完全相同（当前 ${versions.join(" / ")}）`));
  process.exit(1);
}
if (advisoryBad.length) {
  console.warn(c("33", `\n提示：${advisoryBad.join(" / ")} 与权威版本 ${versions[0]} 不一致 ——`));
  console.warn(c("33", "  dev 版 app 显示的版本号会偏旧（打包版用 tauri.conf.json，不受影响）。"));
  console.warn(c("2", "  同步它，或在发布提交里一并改掉。本项不阻断。"));
}
console.log(c("32", `\n版本号 ${versions[0]} 校验完成`));
process.exit(0);
