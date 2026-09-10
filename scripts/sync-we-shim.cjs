// 从 webwallgl 包里提取库自带的 WE 网页壁纸 shim，写入 src-tauri/src/we_shim.js。
//
// 为什么要这么做：库的 mountWeb 对**同源** iframe 只做 attachIframe（保留原 URL，
// 壁纸的相对资源才能加载），shim 注入交给 host。所以 Rust 的内容服务器必须注入
// 与库配套的那份 shim —— 库的音频/媒体泵调 __wePushAudio / __wePushMedia /
// __wePushPointer，host 注入的若是别的实现，这些调用会全部落空（症状：壁纸能显示
// 但音频可视化不动、Now Playing 不更新）。
//
// shim 源码在 bundle 里是私有常量 shimSource，没有作为公共 API 导出，
// 只能这样提取。升级 webwallgl 后跑一次：
//   node scripts/sync-we-shim.cjs
// content_server.rs 里有测试守卫（shim_matches_library_bundle）会在漂移时报错。

const fs = require("fs");
const path = require("path");

const repo = path.resolve(__dirname, "..");
const BUNDLE = path.join(repo, "node_modules/webwallgl/webwallgl.mjs");
const DEST = path.join(repo, "src-tauri/src/we_shim.js");

/** 从 bundle 里抠出 shimSource 字符串字面量并还原转义 */
function extractShim(bundlePath) {
  const src = fs.readFileSync(bundlePath, "utf8");
  const KEY = "const shimSource = ";
  const at = src.indexOf(KEY);
  if (at < 0) {
    throw new Error(
      `未在 ${bundlePath} 找到 shimSource 定义 —— 库的内部结构可能变了，需人工确认`,
    );
  }
  const start = at + KEY.length;
  const quote = src[start];
  if (quote !== "'" && quote !== '"') {
    throw new Error(`shimSource 不是字符串字面量（起始字符 ${JSON.stringify(quote)}）`);
  }

  // 逐字符扫到未转义的收尾引号
  let i = start + 1;
  let raw = "";
  while (i < src.length) {
    const c = src[i];
    if (c === "\\") {
      raw += c + src[i + 1];
      i += 2;
      continue;
    }
    if (c === quote) break;
    raw += c;
    i++;
  }
  if (i >= src.length) throw new Error("shimSource 字符串未闭合");

  // 还原转义序列。不用 JSON.parse：内容里有裸双引号和反引号，包不成合法 JSON
  const ESC = {
    "\\n": "\n",
    "\\t": "\t",
    "\\r": "\r",
    "\\\\": "\\",
    "\\'": "'",
    '\\"': '"',
    "\\0": "\0",
  };
  let out = "";
  for (let j = 0; j < raw.length; j++) {
    if (raw[j] === "\\" && j + 1 < raw.length) {
      const seq = raw[j] + raw[j + 1];
      out += ESC[seq] !== undefined ? ESC[seq] : seq;
      j++;
    } else {
      out += raw[j];
    }
  }
  return out;
}

const code = extractShim(BUNDLE);

// 基本健全性检查：库的泵依赖这些接口，缺任何一个都说明提取出错或库改了契约
const REQUIRED = [
  "__wePushAudio",
  "__wePushMedia",
  "__wePushPointer",
  "__weSeedProps",
  "__weApplyProps",
  "__weSetFps",
  "__weSetVolume",
  "__weSetPaused",
];
const missing = REQUIRED.filter((n) => !code.includes(n));
if (missing.length) {
  console.error("提取出的 shim 缺少必需接口:", missing.join(", "));
  process.exit(1);
}

const prev = fs.existsSync(DEST) ? fs.readFileSync(DEST, "utf8") : "";
if (prev === code) {
  console.log("we_shim.js 已是最新（与库 bundle 一致）");
} else {
  fs.writeFileSync(DEST, code);
  console.log(`已更新 ${path.relative(repo, DEST)}（${code.length} 字节）`);
}
console.log("父页控制接口:", [...new Set(code.match(/__we[A-Za-z]+/g) || [])].sort().join(" "));
