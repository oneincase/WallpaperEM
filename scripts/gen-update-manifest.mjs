#!/usr/bin/env node
/*
 * 生成 tauri-plugin-updater 的更新清单（latest-<target>-<arch>.json）。
 *
 * 官方 updater 的端点（tauri.conf.json plugins.updater.endpoints）按
 *   https://github.com/oneincase/WallpaperEM/releases/latest/download/latest-{{target}}-{{arch}}.json
 * 拼 URL，所以每个平台只需要一个清单文件、内容是该平台的下载地址 + minisign 签名。
 * 三个发版 workflow 各自只写自己的文件名（darwin/windows/linux × 架构），
 * 互不覆盖，天然无竞态。
 *
 * 签名取自 `--asset` 同目录下的 `<asset>.sig`（`tauri build` 在
 * bundle.createUpdaterArtifacts: true 时自动生成，密钥来自 CI 环境变量
 * TAURI_SIGNING_PRIVATE_KEY）。
 *
 * 用法（发版 workflow 里 softprops 附件上传之后、tag 触发时）：
 *   node scripts/gen-update-manifest.mjs \
 *     --repo oneincase/WallpaperEM --tag v1.2.0 \
 *     --file latest-darwin-aarch64.json \
 *     --target darwin-aarch64 --target darwin-x86_64 \
 *     --asset src-tauri/target/.../bundle/macos/WallpaperEM.app.tar.gz \
 *     --notes-file "$RUNNER_TEMP/release-notes.md"
 * 然后 `gh release upload <tag> <file> --clobber` 挂到同一 Release。
 */

import fs from "node:fs";
import path from "node:path";

function parseArgs(argv) {
  const args = { targets: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const next = () => {
      const v = argv[++i];
      if (v === undefined) {
        console.error(`缺少 ${a} 的值`);
        process.exit(1);
      }
      return v;
    };
    switch (a) {
      case "--repo": args.repo = next(); break;
      case "--tag": args.tag = next(); break;
      case "--file": args.file = next(); break;
      case "--target": args.targets.push(next()); break;
      case "--asset": args.asset = next(); break;
      case "--notes-file": args.notesFile = next(); break;
      case "--pub-date": args.pubDate = next(); break;
      default:
        console.error(`未知参数: ${a}`);
        process.exit(1);
    }
  }
  for (const key of ["repo", "tag", "file", "asset"]) {
    if (!args[key]) {
      console.error(`缺少必填参数 --${key}`);
      process.exit(1);
    }
  }
  if (args.targets.length === 0) {
    console.error("至少要一个 --target（平台键，如 darwin-aarch64）");
    process.exit(1);
  }
  return args;
}

const args = parseArgs(process.argv.slice(2));

// tag（v1.2.0）→ 清单 version（semver，无 v 前缀）
const version = args.tag.replace(/^v/, "");
if (!/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`tag ${args.tag} 不是形如 v1.2.0 的版本号`);
  process.exit(1);
}

const assetPath = args.asset;
if (!fs.existsSync(assetPath)) {
  console.error(`安装包不存在: ${assetPath}`);
  process.exit(1);
}
const sigPath = `${assetPath}.sig`;
if (!fs.existsSync(sigPath)) {
  console.error(
    `签名不存在: ${sigPath}\n（需要 bundle.createUpdaterArtifacts: true 且构建时设置了 TAURI_SIGNING_PRIVATE_KEY）`
  );
  process.exit(1);
}
const signature = fs.readFileSync(sigPath, "utf8").trim();
if (!signature) {
  console.error(`签名文件为空: ${sigPath}`);
  process.exit(1);
}

const notes = args.notesFile && fs.existsSync(args.notesFile)
  ? fs.readFileSync(args.notesFile, "utf8")
  : "";

// 资产 URL：Release 固定附件地址（文件名做 URL 编码，实际都是安全字符）
const base = path.basename(assetPath);
const url = `https://github.com/${args.repo}/releases/download/${args.tag}/${encodeURIComponent(base)}`;

const platforms = {};
for (const t of args.targets) {
  if (platforms[t]) continue;
  platforms[t] = { signature, url };
}

const manifest = {
  version,
  notes,
  pub_date: args.pubDate || new Date().toISOString(),
  platforms,
};

fs.writeFileSync(args.file, JSON.stringify(manifest, null, 2) + "\n");
console.log(`已生成 ${args.file}：version=${version} targets=${args.targets.join(",")} asset=${base}`);
