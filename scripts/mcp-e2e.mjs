#!/usr/bin/env node
/*
 * WallpaperEM MCP 端到端自检脚本
 *
 * 跑完整闭环：建工程 → 写素材 → 校验 → 冻结一版（版本历史）→ 打 scene.pkg →
 * 安装本地库 → 应用到桌面 → 截图（PNG 落到 <tmp>/mcp-e2e-<时间>.png）→ 回退到那一版。
 * 用来验收「MCP 服务 + AI 壁纸工作区」这条链路，也用来在改动渲染/打包后快速回归。
 *
 * 用法（先在 设置 → AI / MCP 打开服务，点「复制地址」拿到带令牌的 URL）：
 *   node scripts/mcp-e2e.mjs "http://127.0.0.1:7411/mcp?token=<令牌>"
 *   MCP_URL="http://127.0.0.1:7411/mcp?token=..." node scripts/mcp-e2e.mjs
 *   node scripts/mcp-e2e.mjs "<url>" --type web      # 换成网页壁纸闭环
 *
 * 退出码：0 全通过；1 某一步失败（失败步骤与原始返回都打印出来）。
 *
 * 跑完会**收尾**：停止壁纸并删掉这次自检装进本地库的条目（工程目录保留）。
 * 想留着条目和桌面上的效果做人工验收，加 KEEP=1。
 */
import { writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const args = process.argv.slice(2);
const typeIdx = args.indexOf("--type");
const kind = typeIdx >= 0 ? args[typeIdx + 1] : "scene";
const url = (typeIdx >= 0 ? args.filter((_, i) => i !== typeIdx && i !== typeIdx + 1) : args)[0]
  || process.env.MCP_URL;

if (!url) {
  console.error("缺少 MCP 地址。用法: node scripts/mcp-e2e.mjs \"http://127.0.0.1:7411/mcp?token=...\"");
  process.exit(1);
}
if (kind !== "scene" && kind !== "web") {
  console.error(`--type 只支持 scene / web，收到: ${kind}`);
  process.exit(1);
}

let id = 0;
let session = null;

async function rpc(method, params) {
  const res = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      ...(session ? { "mcp-session-id": session } : {}),
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }),
  });
  const sid = res.headers.get("mcp-session-id");
  if (sid) session = sid;
  const text = await res.text();
  if (!res.ok) throw new Error(`HTTP ${res.status} ${method}: ${text.slice(0, 400)}`);
  const body = JSON.parse(text);
  if (body.error) throw new Error(`${method} 失败: ${JSON.stringify(body.error)}`);
  return body.result;
}

/** 调工具：工具内部失败以 isError 返回（会话不中断），这里当失败处理 */
async function tool(name, input = {}) {
  const r = await rpc("tools/call", { name, arguments: input });
  const blocks = r.content || [];
  const textBlock = blocks.find((b) => b.type === "text");
  let parsed = null;
  try {
    parsed = textBlock ? JSON.parse(textBlock.text) : null;
  } catch {
    /* 非 JSON 文本（不该出现，但别因此炸掉） */
  }
  if (r.isError) throw new Error(`${name} 执行失败: ${textBlock?.text?.slice(0, 600)}`);
  return { blocks, json: parsed };
}

const step = (n, msg) => console.log(`\n[${n}] ${msg}`);

try {
  step(1, "initialize");
  const init = await rpc("initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "wallpaperem-mcp-e2e", version: "1" },
  });
  console.log(`    协议 ${init.protocolVersion} · 服务端 ${init.serverInfo?.name} ${init.serverInfo?.version} · 会话 ${session}`);

  step(2, "tools/list");
  const tools = (await rpc("tools/list", {})).tools || [];
  console.log(`    ${tools.length} 个工具`);

  const project = `e2e-${kind}-${Date.now().toString(36)}`;
  step(3, `project_create(type=${kind}, name=${project})`);
  const created = await tool("project_create", { name: project, type: kind, title: "MCP 自检" });
  console.log(`    落盘：${(created.json?.files || []).join(", ") || "（见返回）"}`);

  step(4, "project_write_file（改一处画面参数）");
  const entry = kind === "scene" ? "scene.json" : "index.html";
  const before = (await tool("project_read_file", { project, path: entry })).json;
  if (before?.encoding !== "utf8") throw new Error(`入口 ${entry} 应为文本文件，实际 ${before?.encoding}`);
  const text = before.content ?? "";
  let patched;
  if (kind === "scene") {
    const scene = JSON.parse(text);
    scene.general.clearcolor = "0.55 0.10 0.20"; // 暗红：截图里一眼能认出来
    patched = JSON.stringify(scene, null, 2);
  } else {
    patched = text.replace("</body>", "  <div style=\"position:fixed;left:0;right:0;bottom:0;height:12vh;background:#8b1030\"></div>\n</body>");
  }
  await tool("project_write_file", { project, path: entry, content: patched, encoding: "utf8" });
  console.log(`    已改写 ${entry}（${patched.length} 字节）`);

  step(5, "project_validate");
  const report = (await tool("project_validate", { project })).json;
  if (!report?.ok) throw new Error(`校验未通过: ${JSON.stringify(report?.errors)}`);
  console.log(`    ok · type=${report.type} entry=${report.entry}${report.warnings?.length ? ` · warnings: ${report.warnings.join(" / ")}` : ""}`);
  step(6, "project_snapshot + project_versions（版本历史）");
  const snap = (await tool("project_snapshot", { project, note: "e2e 初版" })).json;
  if (!snap?.frozen) throw new Error(`冻结失败: ${JSON.stringify(snap)}`);
  console.log(`    冻结第 ${snap.version} 版 → .version/${snap.version}（工作副本进入第 ${snap.current} 版）`);
  const versions = (await tool("project_versions", { project })).json;
  if (!versions?.versions?.some((v) => v.version === snap.version)) {
    throw new Error(`历史里找不到刚冻结的第 ${snap.version} 版: ${JSON.stringify(versions)}`);
  }
  console.log(`    历史 ${versions.count} 个版本 · 当前第 ${versions.current} 版`);

  if (kind === "scene") {
    step(7, "scene_pack");
    const packed = (await tool("scene_pack", { project })).json;
    console.log(`    ${packed?.bytes} 字节 · ${packed?.entries?.length} 个条目 · 转换贴图 ${(packed?.convertedTextures || []).join(", ") || "无"}`);
  } else {
    step(7, "（网页壁纸不需要打包）");
  }

  step(8, "project_install");
  const installed = (await tool("project_install", { project })).json;
  console.log(`    itemId=${installed?.itemId}`);
  const itemId = installed?.itemId;
  if (!itemId) throw new Error("安装没有返回 itemId");

  step(9, "wallpaper_apply + wallpaper_screenshot");
  // 首次应用：冷启动要解析 scene.pkg + 贴图，给足 60s（与工具默认一致）
  const t1 = Date.now();
  const shot = await tool("wallpaper_screenshot", { itemId, settleMs: 2500, timeoutMs: 60000 });
  const image = shot.blocks.find((b) => b.type === "image");
  if (!image) throw new Error("截图没有返回 image 内容块");
  const out = join(tmpdir(), `mcp-e2e-${kind}-${Date.now()}.png`);
  writeFileSync(out, Buffer.from(image.data, "base64"));
  console.log(`    首次挂载 ${Date.now() - t1}ms · ${shot.json?.bytes} 字节 → ${out}`);
  console.log(`    预览：${shot.json?.previewPath ?? "（未存盘）"}`);

  // 紧接着再拍一张：同一张壁纸已就绪，必须走「跳过重复应用」快路径
  // （不跳过的话每次都会重新解析 pkg，这也是「截图超时」高频出现的来源之一）
  const t2 = Date.now();
  const shot2 = await tool("wallpaper_screenshot", { itemId, settleMs: 500 });
  if (shot2.json?.applied !== false) {
    throw new Error(`连拍没有复用已挂载的壁纸（applied=${shot2.json?.applied}）`);
  }
  if (!shot2.blocks.some((b) => b.type === "image")) throw new Error("连拍没有返回 image 内容块");
  console.log(`    连拍（复用已挂载）${Date.now() - t2}ms · applied=${shot2.json?.applied}`);

  // 安装后条目应当带着工程自带的分类标签（含年龄分级），否则本地库按分级筛不到
  const lib = (await tool("library_list", { query: "MCP 自检", limit: 20 })).json;
  const row = (lib?.items || []).find((it) => it.itemId === itemId);
  const tags = row?.tags || [];
  if (row && tags.length === 0) throw new Error("本地库条目没有带上工程标签");
  console.log(`    本地库标签：${tags.join(", ") || "（未回读）"}`);

  // 回退只动工作区里的工程，本地库那份副本已经装好，不受影响
  step(10, "project_rollback（历史回退）");
  const back = (await tool("project_rollback", { project, version: snap.version, force: true })).json;
  if (back?.version !== snap.version) throw new Error(`回退结果不对: ${JSON.stringify(back)}`);
  console.log(`    已退回第 ${back.version} 版 · 还原 ${back.restored} 个文件 · 删除 ${back.removed?.length ?? 0} 个`);

  // 收尾：自检不该污染用户的本地库，更不该把自检壁纸一直挂在桌面上。
  // 留下的条目会跟着 wallpaper_sessions 在每次启动时被恢复成桌面壁纸持续渲染，
  // 用户很容易把它当成「应用卡顿」。想留着看效果就 KEEP=1。
  if (process.env.KEEP === "1") {
    console.log(`\n（KEEP=1）保留条目 ${itemId} 与已应用的壁纸；工程目录 ${project}`);
  } else {
    step(11, "收尾：停止壁纸 + 删除自检条目");
    await tool("wallpaper_stop", {});
    const del = (await tool("library_delete", { itemId })).json;
    console.log(`    已停止壁纸 · 条目 ${itemId} 已删除（deleted=${del?.deleted}）`);
    console.log("    工程目录保留（还想再装一次就 project_install，或直接删掉这个目录）");
  }

  console.log(`\n✅ 闭环通过（${kind}）· 工程 ${project} · 条目 ${itemId} · 历史 ${versions.count} 个版本`);
} catch (e) {
  console.error(`\n❌ ${e.message}`);
  process.exit(1);
}
