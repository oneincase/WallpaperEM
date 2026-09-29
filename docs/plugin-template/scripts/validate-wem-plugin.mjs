#!/usr/bin/env node
// wem-plugin.json 自查脚本 —— WallpaperEM 第三方插件接入协议 v1。
//
// 零依赖、可离线：直接 `node scripts/validate-wem-plugin.mjs` 就能在本地或 CI 里
// 检查清单是否符合协议（规则与错误码见 docs/plugin-protocol.md）。
//
//   node scripts/validate-wem-plugin.mjs                 # 校验 ./wem-plugin.json
//   node scripts/validate-wem-plugin.mjs path/to.json    # 校验指定文件
//   node scripts/validate-wem-plugin.mjs --self-test     # 用协议共享用例自检脚本本身
//
// 退出码：0 通过，1 不合规（或用例不通过），2 用法错误。
// 注意：这里必须与宿主实现（src-tauri/src/plugin/store.rs）保持同规则 ——
// 共享用例 protocol-cases.json 就是用来把两边钉在一起的。
import fs from "node:fs";
import path from "node:path";

const SCHEMA_VERSION = 1;
const SUPPORTED_CAPABILITIES = ["open-url", "open-window", "dsh-profile"];
const RESERVED_IDS = ["dsh"];
const MAX_PROFILE_PACKAGES = 8;
const MAX_MANIFEST_BYTES = 128 * 1024;
const CATEGORIES = ["AI 助手", "壁纸资源", "创作工具", "其他"];

/** 错误码 → 怎么改（与协议文档的「错误码」一节一致） */
const HINTS = {
  "plugin-manifest": "补上 name 与 entry.url（或顶层 url）",
  "plugin-name": "名字不能为空、不能超过 60 字符",
  "plugin-url": "入口必须是 http(s):// 开头（file:、javascript: 一律拒绝）",
  "plugin-entry-type": 'entry.type 只能是 "url" 或 "dsh"',
  "plugin-open": 'entry.open 只能是 "external" 或 "window"',
  "plugin-packages": "dsh 型的 packages 要写 npm 包名（可带 @版本，最多 8 个）",
  "plugin-id": "id 只能是字母/数字/短横线/下划线/点",
  "plugin-id-reserved": "换一个 id（内置插件占用了这个）",
  "plugin-schema": `schemaVersion 只能 ≤ ${SCHEMA_VERSION}`,
  "plugin-capability": "capabilities 只能用 open-url / open-window / dsh-profile，且要覆盖入口隐含的那一项",
  "plugin-app-version": 'minAppVersion 写三段数字，如 "2.1.0"',
  "plugin-too-large": "清单要小于 128 KB",
};

const isObj = (v) => typeof v === "object" && v !== null && !Array.isArray(v);

/** 与宿主同规则：小写 + 非法字符合并成 `-`，剥掉两端；中文名退化成内容哈希 */
function slug(raw) {
  let out = "";
  let prevDash = false;
  for (const ch of String(raw).trim()) {
    const c = ch.toLowerCase();
    if (/[a-z0-9_]/.test(c)) {
      out += c;
      prevDash = false;
    } else if (!prevDash && out.length > 0) {
      out += "-";
      prevDash = true;
    }
    if (out.length >= 48) break;
  }
  out = out.replace(/^-+|-+$/g, "");
  if (out.length > 0 && /^[a-z0-9]/.test(out)) return out;
  if (String(raw).trim() === "") return null;
  let h = 2166136261;
  for (const ch of String(raw).trim()) {
    h ^= ch.codePointAt(0);
    h = Math.imul(h, 16777619);
  }
  return `p-${(h >>> 0).toString(16).padStart(8, "0")}`;
}

const text = (v, fallback = "") =>
  typeof v === "string" && v.trim() !== "" ? v.trim() : fallback;

function validNpmName(n) {
  return (
    typeof n === "string" &&
    n.length > 0 &&
    n.length <= 214 &&
    !n.startsWith(".") &&
    !n.startsWith("_") &&
    !n.includes("..") &&
    /^[a-z0-9._-]+$/.test(n)
  );
}

/** 包规格：pkg / @scope/pkg / pkg@^1.2.0 / @scope/pkg@next */
function validPackageSpec(raw) {
  const s = String(raw).trim();
  if (!s || s.length > 214 || s.startsWith("-") || /\s/.test(s) || /[\u0000-\u001f]/.test(s)) {
    return false;
  }
  const at = s.lastIndexOf("@");
  const name = at > 0 ? s.slice(0, at) : s;
  const version = at > 0 ? s.slice(at + 1) : "";
  if (version) {
    if (/[:/]/.test(version) || version.startsWith(".")) return false;
    if (!/^[A-Za-z0-9._+^~*<>=|-]+$/.test(version)) return false;
  }
  if (name.startsWith("@")) {
    const [scope, pkg, ...rest] = name.slice(1).split("/");
    return rest.length === 0 && validNpmName(scope) && validNpmName(pkg);
  }
  return validNpmName(name);
}

const parseVersion = (s) => {
  const core = String(s).trim().replace(/^v/, "").split(/[-+]/)[0];
  const parts = core.split(".");
  if (parts.length > 3) return null;
  const nums = [0, 0, 0];
  let seen = false;
  for (let i = 0; i < parts.length; i++) {
    const digits = /^\d+/.exec(parts[i]);
    if (!digits) return null;
    nums[i] = Number(digits[0]);
    seen = true;
  }
  return seen ? nums : null;
};

const categoryOf = (raw) => {
  const s = text(raw);
  if (!s) return "其他";
  if (CATEGORIES.includes(s)) return s;
  const words = s.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
  const has = (set) => words.some((w) => set.includes(w));
  if (has(["ai", "agent", "dsh", "llm", "mcp", "copilot"])) return "AI 助手";
  if (has(["wallpaper", "asset", "assets", "resource", "resources", "image", "images"])) {
    return "壁纸资源";
  }
  if (has(["shader", "scene", "effect", "editor", "tool", "tools", "we"])) return "创作工具";
  return "其他";
};

const capabilityFor = (kind, open) =>
  kind === "dsh" ? "dsh-profile" : open === "window" ? "open-window" : "open-url";

/**
 * 校验一份清单。返回 { ok: true, manifest } 或 { ok: false, code, detail }。
 * @param {unknown} v 解析后的 JSON
 * @param {string} idHint id 缺省时的兜底（目录名 / 仓库名）
 */
export function validateManifest(v, idHint = "wem-plugin") {
  const fail = (code, detail = "") => ({ ok: false, code, detail });
  if (!isObj(v)) return fail("plugin-manifest", "清单必须是一个 JSON 对象");

  // 入口归一：entry:{…} 或顶层平铺（url/open/kind/packages）
  let entry = isObj(v.entry) ? v.entry : null;
  if (!entry && (typeof v.url === "string" || v.kind || v.packages)) {
    entry = { type: v.kind ?? v.type, url: v.url, open: v.open, packages: v.packages };
  }
  if (!entry) return fail("plugin-manifest", "缺少 entry（或顶层 url/kind）");

  const name = text(v.name);
  if (!name || name.length > 60) return fail("plugin-name", JSON.stringify(v.name));

  const rawKind = text(entry.type, "url");
  if (rawKind !== "url" && rawKind !== "dsh") return fail("plugin-entry-type", rawKind);

  let url = "";
  let open = "external";
  let packages = [];
  if (rawKind === "dsh") {
    packages = (Array.isArray(entry.packages) ? entry.packages : [])
      .map((p) => text(p))
      .filter(Boolean);
    if (packages.length === 0 || packages.length > MAX_PROFILE_PACKAGES) {
      return fail("plugin-packages", `packages 数量: ${packages.length}`);
    }
    for (const p of packages) {
      if (!validPackageSpec(p)) return fail("plugin-packages", p);
    }
    open = "window";
  } else {
    url = text(entry.url);
    if (!/^https?:\/\//.test(url)) return fail("plugin-url", url);
    const rawOpen = text(entry.open, "external");
    if (rawOpen !== "external" && rawOpen !== "window") return fail("plugin-open", rawOpen);
    open = rawOpen;
  }

  const schemaVersion = v.schemaVersion === undefined ? SCHEMA_VERSION : Number(v.schemaVersion);
  if (!Number.isInteger(schemaVersion) || schemaVersion < 1 || schemaVersion > SCHEMA_VERSION) {
    return fail("plugin-schema", String(v.schemaVersion));
  }

  const implied = capabilityFor(rawKind, open);
  let capabilities;
  if (v.capabilities === undefined) {
    capabilities = [implied];
  } else {
    if (!Array.isArray(v.capabilities)) return fail("plugin-capability", "capabilities 必须是数组");
    capabilities = [...new Set(v.capabilities.map((c) => text(c).toLowerCase()).filter(Boolean))];
    for (const c of capabilities) {
      if (!SUPPORTED_CAPABILITIES.includes(c)) return fail("plugin-capability", c);
    }
    if (!capabilities.includes(implied)) return fail("plugin-capability", `缺少 ${implied}`);
    if (implied === "dsh-profile" && capabilities.length > 1) {
      return fail("plugin-capability", capabilities.join(","));
    }
  }

  const minAppVersion = text(v.minAppVersion);
  if (minAppVersion && parseVersion(minAppVersion) === null) {
    return fail("plugin-app-version", minAppVersion);
  }

  const id = slug(text(v.id, idHint));
  if (!id) return fail("plugin-id", String(v.id ?? idHint));
  if (RESERVED_IDS.includes(id)) return fail("plugin-id-reserved", id);

  return {
    ok: true,
    manifest: {
      schemaVersion,
      kind: rawKind,
      id,
      name,
      summary: text(v.summary),
      description: text(v.description),
      category: categoryOf(v.category),
      icon: text(v.icon, "🧩"),
      author: text(v.author),
      version: text(v.version),
      homepage: text(v.homepage),
      capabilities,
      packages,
      url,
      open,
      minAppVersion,
    },
  };
}

/** 非阻断的提醒（不影响通过与否） */
function warnings(v, m) {
  const out = [];
  if (!m.summary) out.push("建议写 summary：它会直接显示在插件卡片上");
  if (!m.homepage) out.push("建议写 homepage（项目主页/仓库页）");
  if (!m.version) out.push("建议写 version，便于以后做更新检查");
  if (text(v.category) && !CATEGORIES.includes(text(v.category))) {
    out.push(`分类「${text(v.category)}」不在词表里，应用会归入「其他」`);
  }
  if (!m.icon || m.icon === "🧩") out.push("建议给一个 emoji 图标（icon）");
  if (m.kind === "dsh") {
    out.push("⚠️ 这是特权插件（dsh-profile）：它会把 packages 里的包装进 DeepSeek Harness，等于运行第三方代码");
  }
  return out;
}

function report(m, warns) {
  const lines = [
    "✅ 清单符合接入协议 v1",
    `   id          ${m.id}`,
    `   name        ${m.name}`,
    `   入口        ${m.kind === "dsh" ? `dsh: ${m.packages.join(", ")}` : `${m.open}: ${m.url}`}`,
    `   能力        ${m.capabilities.join(", ")}`,
    `   分类        ${m.category}`,
    m.minAppVersion ? `   最低版本    App ≥ ${m.minAppVersion}` : "",
  ].filter(Boolean);
  console.log(lines.join("\n"));
  for (const w of warns) console.log(`   · ${w}`);
}

function selfTest() {
  const casesPath = path.join(import.meta.dirname, "..", "protocol-cases.json");
  if (!fs.existsSync(casesPath)) {
    console.log("ℹ️  找不到 protocol-cases.json，跳过自检（复制模板时请一并带上它）");
    return 0;
  }
  const cases = JSON.parse(fs.readFileSync(casesPath, "utf8"));
  let failed = 0;
  for (const c of cases.accept ?? []) {
    const r = validateManifest(c.manifest, c.idHint);
    if (!r.ok) {
      console.error(`❌ 应当接受（${c.note}），却被 ${r.code} 拒了`);
      failed++;
      continue;
    }
    for (const [key, want] of Object.entries(c.expect ?? {})) {
      const got = r.manifest[key];
      if (JSON.stringify(got) !== JSON.stringify(want)) {
        console.error(`❌ ${c.note}：${key} 期望 ${JSON.stringify(want)}，实际 ${JSON.stringify(got)}`);
        failed++;
      }
    }
  }
  for (const c of cases.reject ?? []) {
    const r = validateManifest(c.manifest, c.idHint);
    if (r.ok) {
      console.error(`❌ 应当拒绝（${c.note}），却通过了`);
      failed++;
    } else if (r.code !== c.code) {
      console.error(`❌ ${c.note}：期望 ${c.code}，实际 ${r.code}`);
      failed++;
    }
  }
  const total = (cases.accept?.length ?? 0) + (cases.reject?.length ?? 0);
  if (failed === 0) console.log(`✅ 自检通过（${total} 条共享用例）`);
  return failed === 0 ? 0 : 1;
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes("--help") || args.includes("-h")) {
    console.log("用法: node scripts/validate-wem-plugin.mjs [wem-plugin.json] [--self-test]");
    return 0;
  }
  if (args.includes("--self-test")) return selfTest();

  const file = args.find((a) => !a.startsWith("-")) ?? "wem-plugin.json";
  if (!fs.existsSync(file)) {
    console.error(`❌ 找不到 ${file}`);
    return 2;
  }
  const size = fs.statSync(file).size;
  if (size > MAX_MANIFEST_BYTES) {
    console.error(`❌ 清单 ${size} 字节，超过 ${MAX_MANIFEST_BYTES} 上限（plugin-too-large）`);
    return 1;
  }
  let parsed;
  try {
    parsed = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (e) {
    console.error(`❌ 不是合法的 JSON（plugin-manifest）：${e.message}`);
    return 1;
  }
  // id 兜底用文件名所在的目录名（仓库根目录名 ≈ 插件名）
  const hint = path.basename(path.resolve(path.dirname(file)));
  const r = validateManifest(parsed, hint);
  if (!r.ok) {
    console.error(`❌ ${r.code}${r.detail ? `：${r.detail}` : ""}`);
    if (HINTS[r.code]) console.error(`   → ${HINTS[r.code]}`);
    console.error("   规则见 docs/plugin-protocol.md");
    return 1;
  }
  report(r.manifest, warnings(parsed, r.manifest));
  return 0;
}

process.exit(main());
