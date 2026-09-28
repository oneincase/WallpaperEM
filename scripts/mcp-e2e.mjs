#!/usr/bin/env node
/*
 * WallpaperEM MCP 端到端自检脚本
 *
 * 跑完整闭环：建工程 → 写素材 → 校验 + 体检 → scene_pack(install=true) →
 * 应用到桌面 → 截图 + 多帧截图 + 读诊断历史 → **改一处画面后增量更新再截图**（PNG 落到
 * <tmp>/mcp-e2e-<时间>.png），验证「改动真的反映到了画面」。
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

  if (kind === "scene") {
    step(5.5, "scene_inspect（体检）");
    const insp = (await tool("scene_inspect", { project })).json;
    if (!insp || insp.layerCount < 1) throw new Error(`体检没有图层: ${JSON.stringify(insp)?.slice(0, 300)}`);
    console.log(
      `    图层 ${insp.layerCount} · 贴图 ${insp.textures?.length ?? 0} 张 ${(insp.textureBytes / 1024).toFixed(0)}KB` +
        ` · 属性绑定 ${Object.keys(insp.properties?.bound || {}).length} 项` +
        ` · 未使用素材 ${(insp.unusedAssets || []).length} 份`,
    );
    step(6, "scene_pack(install=true)（打包并直接装/更新）");
    const packed = (await tool("scene_pack", { project, install: true })).json;
    console.log(
      `    ${packed?.bytes} 字节 · ${packed?.entries?.length} 个条目 · 转换贴图 ${(packed?.convertedTextures || []).join(", ") || "无"}` +
        ` · installed=${JSON.stringify(packed?.installed?.mode ?? packed?.installed?.itemId ?? null)}`,
    );
  } else {
    step(6, "（网页壁纸不需要打包）");
  }

  step(7, "project_install");
  const installed = kind === "scene"
    ? { itemId: (await tool("projects_list", {})).json?.projects?.find((p) => p.project === project)?.installedItemId }
    : (await tool("project_install", { project })).json;
  const installedId = installed?.itemId;
  if (kind !== "scene") console.log(`    itemId=${installedId}`);
  else console.log(`    已由 scene_pack(install=true) 装好：itemId=${installedId}`);
  const itemId = installedId;
  if (!itemId) throw new Error("安装没有返回 itemId");

  step(8, "wallpaper_apply + wallpaper_screenshot");
  // 首次应用：冷启动要解析 scene.pkg + 贴图，给足 60s（与工具默认一致）
  const t1 = Date.now();
  const shot = await tool("wallpaper_screenshot", { itemId, settleMs: 2500, timeoutMs: 60000 });
  const image = shot.blocks.find((b) => b.type === "image");
  if (!image) throw new Error("截图没有返回 image 内容块");
  if (shot.json?.applied !== true) throw new Error(`首次截图没有真正应用（applied=${shot.json?.applied}）`);
  // 抓帧产物现在是 JPEG（渲染器自抓帧，压过体积），文件名跟着 mime 走
  const ext = /jpe?g/.test(image.mimeType || "") ? "jpg" : "png";
  const out = join(tmpdir(), `mcp-e2e-${kind}-${Date.now()}.${ext}`);
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

  // 离屏预览：不碰桌面会话，照样能拿到画面（迭代时的主要入口）
  step(8.2, "wallpaper_preview（离屏预览，不动桌面）");
  const sessBefore = JSON.stringify((await tool("list_sessions", {})).json?.sessions ?? {});
  // 主窗口被关闭/被内存压力回收时没有渲染面：这步**跳过**（不是失败），并提示怎么继续
  let prev = null;
  try {
    prev = await tool("wallpaper_preview", { project, width: 640, height: 360, maxWidth: 640, timeoutMs: 90000 });
  } catch (e) {
    const msg = String(e?.message ?? e);
    if (msg.includes("主窗口")) {
      console.log("    ⏭ 跳过：主窗口当前不在（离屏预览需要它当渲染面；打开主界面或传 openMainWindow=true）");
    } else {
      throw e;
    }
  }
  if (prev) {
    const prevImgs = prev.blocks.filter((b) => b.type === "image");
    if (!prevImgs.length) throw new Error("离屏预览没有返回 image 内容块");
    const sessAfter = JSON.stringify((await tool("list_sessions", {})).json?.sessions ?? {});
    if (sessBefore !== sessAfter) throw new Error("离屏预览改动了桌面壁纸会话（不该碰）");
    console.log(`    ${prevImgs.length} 帧 · ${prev.json?.frames?.map((f) => `${f.bytes}B`).join(" ")} · 桌面会话未变`);
  }

  // 诊断历史：挂载过程至少有若干条（mount start / ready / …）
  step(8.5, "renderer_diag（诊断历史）");
  const diag = (await tool("renderer_diag", { limit: 20 })).json;
  const n = diag?.entries?.length ?? 0;
  if (n === 0) throw new Error("诊断历史是空的（渲染器一条都没回流？）");
  console.log(`    ${n} 条（缓存 ${diag.buffered}/${diag.cap}，累计 ${diag.total}）· 最近: ${diag.entries[n - 1].msg.slice(0, 60)}`);

  // 第二轮迭代：改画面 → 重新打包 → 覆盖安装 → 再截图。
  // 这一段是「改了看不到变化」的回归哨兵：project_update **刻意保持 itemId 不变**，
  // 所以「屏幕上是哪张壁纸」的判据不能只看 itemId —— 旧实现会在这里跳过重新应用，
  // 把上一版的画面（以及 preview.png）当新版交给 agent。断言用 applied 而不是两张图
  // 的字节差异：场景本身一直在动，字节永远不同，拿它当判据等于没判。
  step(9, "改画面 → 重新打包 → project_update → 再截图（必须重新应用）");
  const scenePath = kind === "scene" ? "scene.json" : "index.html";
  const round2 = (await tool("project_read_file", { project, path: scenePath })).json;
  const text2 = round2?.content ?? "";
  let patched2;
  if (kind === "scene") {
    const scene2 = JSON.parse(text2);
    scene2.general.clearcolor = "0.05 0.85 0.95"; // 亮青：与第一轮的暗红完全相反
    patched2 = JSON.stringify(scene2, null, 2);
  } else {
    patched2 = text2.replace("#8b1030", "#057f95");
  }
  await tool("project_write_file", { project, path: scenePath, content: patched2, encoding: "utf8" });
  let updated;
  if (kind === "scene") {
    // 打包 + 增量更新一步到位（同时覆盖 scene_pack(install=true) 的快路径）
    const repacked = (await tool("scene_pack", { project, install: true })).json;
    updated = repacked?.installed ?? {};
    if (updated.mode !== "incremental") {
      throw new Error(`第二步没有走增量更新（mode=${updated.mode}）: ${JSON.stringify(updated).slice(0, 200)}`);
    }
    console.log(`    增量更新：拷 ${updated.copied} / 删 ${updated.removed} / 未变 ${updated.unchanged} 个文件`);
  } else {
    updated = (await tool("project_update", { project })).json;
  }
  // 多帧截图：一次拿两帧（验收时间维度）
  const shot3 = await tool("wallpaper_screenshot", { itemId, settleMs: 800, timeoutMs: 60000, framesMs: [0, 700] });
  const imgs3 = shot3.blocks.filter((b) => b.type === "image");
  if (imgs3.length !== 2) throw new Error(`多帧截图应当返回 2 个 image 块，实际 ${imgs3.length}`);
  console.log(`    多帧：${shot3.json?.frames?.map((f) => `${f.tMs}ms/${f.bytes}B`).join(" ")}`);
  if (shot3.json?.applied !== true) {
    throw new Error(
      `改完工程后截图没有重新应用（applied=${shot3.json?.applied}）—— 画面仍是上一版：` +
        `本地库文件已更新（itemId=${updated?.itemId}），但屏幕挂的还是旧包`,
    );
  }
  console.log(`    重新应用 ok · ${shot3.json?.bytes} 字节 · applied=${shot3.json?.applied}`);

  // 安装后条目应当带着工程自带的分类标签（含年龄分级），否则本地库按分级筛不到
  const lib = (await tool("library_list", { query: "MCP 自检", limit: 20 })).json;
  const row = (lib?.items || []).find((it) => it.itemId === itemId);
  const tags = row?.tags || [];
  if (row && tags.length === 0) throw new Error("本地库条目没有带上工程标签");
  console.log(`    本地库标签：${tags.join(", ") || "（未回读）"}`);

  // 收尾：自检不该污染用户的本地库，更不该把自检壁纸一直挂在桌面上。
  // 留下的条目会跟着 wallpaper_sessions 在每次启动时被恢复成桌面壁纸持续渲染，
  // 用户很容易把它当成「应用卡顿」。想留着看效果就 KEEP=1。
  if (process.env.KEEP === "1") {
    console.log(`\n（KEEP=1）保留条目 ${itemId} 与已应用的壁纸；工程目录 ${project}`);
  } else {
    step(10, "收尾：停止壁纸 + 删除自检条目");
    await tool("wallpaper_stop", {});
    const del = (await tool("library_delete", { itemId })).json;
    console.log(`    已停止壁纸 · 条目 ${itemId} 已删除（deleted=${del?.deleted}）`);
    console.log("    工程目录保留（还想再装一次就 project_install，或直接删掉这个目录）");
  }

  console.log(`\n✅ 闭环通过（${kind}）· 工程 ${project} · 条目 ${itemId}`);
} catch (e) {
  console.error(`\n❌ ${e.message}`);
  process.exit(1);
}
