// 壁纸渲染器页（T0.5–T3）
// 由壁纸引擎窗口加载；原生侧通过 window.__wp 控制。
// 类型：canvas（默认演示动画）/ video / gif / web / scene / image
// scene 经 we-scene（MIT vendored）在浏览器 WebGL 实时渲染（2D；3D/粒子等降级）

import * as pkgMod from "../vendor/we-scene/pkg/container.js";
import * as texMod from "../vendor/we-scene/pkg/texture.js";
import * as sceneMod from "../vendor/we-scene/scene/parse.js";
import * as effMod from "../vendor/we-scene/scene/effects-parse.js";
import * as rndMod from "../vendor/we-scene/render/renderer.js";
import * as noiseMod from "../vendor/we-scene/render/noise.js";
import * as particlesMod from "../vendor/we-scene/render/particles.js";
import * as mdlMod from "../vendor/we-scene/render/mdl.js";
import { WE_SHADER_HEADERS } from "../vendor/we-scene/headers";
import { fitWindow } from "../vendor/we-scene/render/math.js";

const asAny = (m: unknown) => m as unknown as Record<string, any>;
const pkg = asAny(pkgMod);
const tex = asAny(texMod);
const scn = asAny(sceneMod);
const eff = asAny(effMod);
const rnd = asAny(rndMod);
const noise = asAny(noiseMod);
const particles = asAny(particlesMod);
const mdl = asAny(mdlMod);

// 场景壁纸渲染开关（手动测试用）
const SKIP_3D_MODELS = false; // 保留 3D 网格渲染
const SKIP_COMPONENTS = true; // 暂不渲染组件（时钟、天气等）
const SKIP_TEXT = true; // 暂不渲染文字对象
const SKIP_PARTICLES = true; // 暂不渲染粒子（雪 / 雨 / zzz）
// we-scene 对部分工坊 effect（shake/foliagesway/iris/audio-bars 等）支持不完整，
// 逐 pass 渲染会产出灰色遮罩 / 随机颜色多边形。暂改为不应用图层效果，只渲染基础层。
const SKIP_SCENE_EFFECTS = false;

type WallpaperFit = "cover" | "contain" | "stretch" | "fill" | "fit";

type WallpaperConfig = {
  type: "canvas" | "video" | "gif" | "web" | "scene" | "image";
  src?: string;
  fit?: WallpaperFit;
  /** 渲染分辨率上限（有效 devicePixelRatio 封顶），越低越省内存；默认 1 */
  renderDpr?: number;
  /** 场景壁纸帧率上限（30/60/120），越低 GPU 占用越低；默认 60 */
  sceneFps?: number;
  muted?: boolean;
  loop?: boolean;
  /** 内容服务器媒体基址：http://127.0.0.1:<port>/media/<token>（scene/web 拉取资源） */
  mediaBase?: string;
};

// 有效渲染 DPR = min(设备 DPR, renderDpr 上限)，用于压缩画布/纹理内存（Retina 上默认降到 1/4）。
function effectiveDpr(cfg?: WallpaperConfig): number {
  const cap = cfg?.renderDpr ?? state.cfg?.renderDpr ?? 1;
  return Math.min(window.devicePixelRatio || 1, cap);
}

// 规范化显示模式：兼容旧会话里的 fill（=拉伸）与 fit（=适应）。
// 旧 fill 是"忽略宽高比铺满"（会被拉伸变形），默认迁移到 cover 修复，不再默认拉伸。
function normalizeFit(fit?: WallpaperFit): "cover" | "contain" | "stretch" {
  if (fit === "fit") return "contain"; // 旧"适应"
  if (fit === "fill") return "cover"; // 旧默认"填充"曾是拉伸 → 修复为等比裁切
  return fit === "contain" || fit === "stretch" ? fit : "cover";
}

// 视频/GIF/图片的 object-fit 映射：cover 等比铺满裁切、contain 等比留边、stretch 拉伸。
function fitObjectFit(fit?: WallpaperFit): { objectFit: string; background: string } {
  const f = normalizeFit(fit);
  if (f === "contain") return { objectFit: "contain", background: "rgba(10,12,16,0.85)" };
  if (f === "stretch") return { objectFit: "fill", background: "transparent" };
  return { objectFit: "cover", background: "transparent" };
}

const state: {
  cfg: WallpaperConfig;
  video?: HTMLVideoElement;
  img?: HTMLImageElement;
  iframe?: HTMLIFrameElement;
  canvas?: HTMLCanvasElement;
  ctx?: CanvasRenderingContext2D;
  raf?: number;
  sceneCleanup?: () => void;
  sceneAudio?: { setVolume: (vol: number) => void; audios: HTMLAudioElement[] };
  /** 当前场景渲染器（含 dispose 释放 WebGL 上下文） */
  renderer?: { dispose?: () => void };
  /** 待 revoke 的 blob URL（场景视频纹理 + 音效） */
  objectUrls?: string[];
  /** 场景内视频纹理元素（暂停并移除） */
  videoTextures?: HTMLVideoElement[];
} = { cfg: { type: "canvas" } };

document.documentElement.style.cssText = "margin:0;height:100%;background:transparent;";
const root = document.body;
root.style.cssText =
  "margin:0;width:100vw;height:100vh;overflow:hidden;background:transparent;position:relative;";

const wrap = document.createElement("div");
wrap.style.cssText = "position:fixed;inset:0;overflow:hidden;";
root.appendChild(wrap);

// 屏蔽默认 Tauri/WebKit 右键菜单（壁纸窗口应只响应用户自定义交互，不弹浏览器/调试菜单）
(() => {
  const block = (e: Event) => e.preventDefault();
  // 顶层文档：捕获+冒泡都拦，确保整页右键不弹菜单
  window.addEventListener("contextmenu", block, true);
  document.addEventListener("contextmenu", block, true);
  // 供 iframe（同源网页壁纸）加载后注入
  (window as unknown as Record<string, unknown>).__blockContextMenu = (doc: Document) =>
    doc.addEventListener("contextmenu", block, true);
})();

function clear() {
  wrap.innerHTML = "";
  if (state.raf !== undefined) cancelAnimationFrame(state.raf);
  state.raf = undefined;
  if (state.sceneCleanup) state.sceneCleanup();
  state.sceneCleanup = undefined;
  // 释放旧场景渲染器（loseContext → 归还 WebGL 上下文与全部纹理/FBO/program/buffer）
  if (state.renderer) {
    state.renderer.dispose?.();
    state.renderer = undefined;
  }
  // 暂停并移除场景内视频纹理元素（避免后台继续解码占用内存）
  for (const v of state.videoTextures ?? []) {
    v.pause();
    v.removeAttribute("src");
    v.load();
    v.remove();
  }
  state.videoTextures = undefined;
  // revoke 所有 blob URL（场景视频纹理 + 音效）
  for (const u of state.objectUrls ?? []) {
    try {
      URL.revokeObjectURL(u);
    } catch {
      /* 忽略 */
    }
  }
  state.objectUrls = undefined;
  if (state.sceneAudio) {
    for (const au of state.sceneAudio.audios) {
      au.pause();
      au.removeAttribute("src");
      au.load();
    }
    state.sceneAudio = undefined;
  }
  state.video = undefined;
  state.img = undefined;
  state.iframe = undefined;
  state.canvas = undefined;
  state.ctx = undefined;
}

// ---------- Canvas 演示动画 ----------

function mountCanvas() {
  clear();
  const c = document.createElement("canvas");
  const dpr = effectiveDpr();
  c.width = Math.max(1, Math.round(innerWidth * dpr));
  c.height = Math.max(1, Math.round(innerHeight * dpr));
  c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;";
  wrap.appendChild(c);
  const ctx = c.getContext("2d");
  if (!ctx) return;
  state.canvas = c;
  state.ctx = ctx;
  startCanvasLoop();
}

function startCanvasLoop() {
  if (state.raf !== undefined) return;
  const c = state.canvas;
  const ctx = state.ctx;
  if (!c || !ctx) return;
  const t0 = performance.now();
  const draw = (t: number) => {
    const s = (t - t0) / 1000;
    const w = c.width;
    const h = c.height;
    const g = ctx.createLinearGradient(0, 0, w, h);
    g.addColorStop(0, `hsl(${(s * 40) % 360}, 78%, 56%)`);
    g.addColorStop(1, `hsl(${(s * 40 + 120) % 360}, 78%, 44%)`);
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);
    const cx = w * (0.5 + 0.34 * Math.sin(s * 0.8));
    const cy = h * (0.5 + 0.28 * Math.cos(s * 0.6));
    const r = Math.min(w, h) * 0.16;
    ctx.beginPath();
    ctx.arc(cx, cy, r, 0, Math.PI * 2);
    ctx.fillStyle = "rgba(255,255,255,0.9)";
    ctx.fill();
    ctx.fillStyle = "rgba(0,0,0,0.55)";
    ctx.font = `bold ${Math.round(h * 0.035)}px -apple-system, sans-serif`;
    ctx.textAlign = "center";
    ctx.fillText("WE WALLPAPER · DESKTOP TEST", w / 2, h * 0.1);
    ctx.fillText(new Date().toLocaleTimeString(), w / 2, h * 0.1 + Math.round(h * 0.045));
    state.raf = requestAnimationFrame(draw);
  };
  state.raf = requestAnimationFrame(draw);
}

// ---------- 视频 / GIF / Web ----------

function mountVideo(cfg: WallpaperConfig) {
  clear();
  const v = document.createElement("video");
  v.autoplay = true;
  v.muted = cfg.muted !== false;
  v.loop = cfg.loop !== false;
  v.playsInline = true;
  v.style.cssText =
    "position:absolute;inset:0;width:100%;height:100%;" +
    `object-fit:${fitObjectFit(cfg.fit).objectFit};background:${fitObjectFit(cfg.fit).background};`;
  v.src = cfg.src ?? "";
  v.addEventListener("error", () => {
    console.warn("video error, fallback to default wallpaper", v.error);
    mountDefaultWallpaper();
  });
  wrap.appendChild(v);
  state.video = v;
  const play = () => v.play().catch(() => {});
  v.addEventListener("canplay", play, { once: true });
}

function mountGif(cfg: WallpaperConfig) {
  clear();
  const img = document.createElement("img");
  const fit = fitObjectFit(cfg.fit);
  img.style.cssText =
    "position:absolute;inset:0;width:100%;height:100%;" +
    `object-fit:${fit.objectFit};background:${fit.background};`;
  img.src = cfg.src ?? "";
  img.addEventListener("error", () => mountDefaultWallpaper());
  wrap.appendChild(img);
  state.img = img;
}

function mountWeb(cfg: WallpaperConfig) {
  clear();
  const f = document.createElement("iframe");
  // allow-same-origin：Spine 等 WebGL 壁纸需同源加载纹理（图片污染画布 → texImage2D 报错）。
  // 安全：壁纸窗口本身无任何 Tauri IPC/能力，独立源隔离不降级。
  f.setAttribute("sandbox", "allow-scripts allow-same-origin");
  f.style.cssText =
    "position:absolute;inset:0;width:100%;height:100%;border:none;background:transparent;";
  f.src = cfg.src ?? "";
  wrap.appendChild(f);
  // 同源网页壁纸：加载后注入屏蔽右键菜单（跨源/沙箱不可访问则忽略）
  const blockIframe = () => {
    try {
      const doc = f.contentDocument;
      if (doc) (window as any).__blockContextMenu?.(doc);
    } catch {
      /* 忽略 */
    }
  };
  f.addEventListener("load", blockIframe);
  state.iframe = f;
}

/// 默认壁纸：无壁纸/加载失败时，展示内置的精美 HTML 壁纸（由内容服务器提供，与渲染器同源）
function mountDefaultWallpaper() {
  clear();
  const f = document.createElement("iframe");
  f.setAttribute("sandbox", "allow-scripts");
  f.style.cssText =
    "position:absolute;inset:0;width:100%;height:100%;border:none;background:transparent;";
  try {
    f.src = new URL("/default-wallpaper/index.html", location.origin).toString();
  } catch {
    f.src = "/default-wallpaper/index.html";
  }
  wrap.appendChild(f);
  const blockIframe = () => {
    try {
      const doc = f.contentDocument;
      if (doc) (window as any).__blockContextMenu?.(doc);
    } catch {
      /* 忽略 */
    }
  };
  f.addEventListener("load", blockIframe);
  state.iframe = f;
}

const utf8 = new TextDecoder();
const readText = (bytes: Uint8Array) => utf8.decode(bytes).replace(/^\uFEFF/, "");

// ---------- Scene（we-scene WebGL） ----------

/** 渲染器诊断上报（经内容服务器 /diag 打进应用日志；用 <img> 免 CORS） */
function reportDiag(cfg: WallpaperConfig, msg: string) {
  try {
    const origin = cfg.mediaBase ? new URL(cfg.mediaBase).origin : "";
    if (origin) {
      const img = new Image();
      img.src = `${origin}/diag?msg=${encodeURIComponent(`scene ${cfg.src ?? "?"}: ${msg.slice(0, 500)}`)}`;
    }
  } catch {
    /* 忽略 */
  }
}

// 解析文字对象的动态文本（时钟/日期/星期等组件 + 自定义 textScript）
function resolveText(layer: any, now: Date): string | null {
  // 优先用 name 关键字识别内置组件
  const name = (layer.name || "").toLowerCase();
  const base = layer.text ?? "";
  if (name.includes("clock") || name.includes("时间")) {
    const h = now.getHours();
    const m = now.getMinutes();
    const s = now.getSeconds();
    return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
  }
  if (name.includes("date") || name.includes("日期")) {
    return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
  }
  if (name.includes("weekday") || name.includes("星期")) {
    const wd = ["日", "一", "二", "三", "四", "五", "六"][now.getDay()];
    return `星期${wd}`;
  }
  // 有 textScript：尝试评估 update 函数
  if (layer.textScript) {
    try {
      const fn = evalTextUpdate(layer.textScript);
      if (fn) {
        const r = fn(base);
        return typeof r === "string" ? r : base;
      }
    } catch (e) {
      /* 脚本失败回退静态文本 */
    }
  }
  return base;
}

// 评估 WE 文本脚本的 update(value) 函数（ES module → 提取函数体）
function evalTextUpdate(script: string): ((v: string) => string) | null {
  try {
    const body = script
      .replace(/export\s+function\s+update/, "function update")
      .replace(/export\s+let\s+\w+\s*=[\s\S]*?;\n?/g, "")
      .replace(/^.*createScriptProperties[\s\S]*?\.finish\(\);\s*/g, "")
      .replace(/^.*scriptProperties\s*=.*$/gm, "");
    const factory = new Function("engine", `${body}; return update;`);
    const engine = { userProperties: {} };
    const update = factory(engine);
    return typeof update === "function" ? update : null;
  } catch (e) {
    return null;
  }
}

function mountScene(cfg: WallpaperConfig) {
  clear();
  const c = document.createElement("canvas");
  const dpr = effectiveDpr(cfg);
  c.width = Math.max(1, Math.round(innerWidth * dpr));
  c.height = Math.max(1, Math.round(innerHeight * dpr));
  c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;";
  wrap.appendChild(c);
  state.canvas = c;
  let disposed = false;
  state.sceneCleanup = () => {
    disposed = true;
    // 兜底：若 clear() 因 disposed 早退未走到 renderer.dispose，这里也释放 WebGL 上下文
    if (state.renderer) {
      state.renderer.dispose?.();
      state.renderer = undefined;
    }
  };

  void (async () => {
    try {
      if (!cfg.mediaBase || !cfg.src) throw new Error("场景壁纸缺少 mediaBase/src");
      reportDiag(cfg, "mountScene start");
      const base = `${cfg.mediaBase}/${cfg.src}`;
      // 兼容 scenes/scene.pkg 与根目录 scene.pkg 两种布局
      let pkgResp = await fetch(`${base}/scenes/scene.pkg`);
      if (!pkgResp.ok) {
        pkgResp = await fetch(`${base}/scene.pkg`);
      }
      reportDiag(cfg, `pkg fetch: HTTP ${pkgResp.status}`);
      if (!pkgResp.ok) throw new Error(`scene.pkg 加载失败（HTTP ${pkgResp.status}）`);
      const parsedPkg = pkg.parsePkg(new Uint8Array(await pkgResp.arrayBuffer()));
      if (disposed) return;

      // project.json（可选）
      let project: unknown = null;
      try {
        const pr = await fetch(`${base}/project.json`);
        if (pr.ok) project = await pr.json();
      } catch {
        /* 忽略 */
      }

      // WebGL2 可用性预检（we-scene 需要 webgl2；用与 createRenderer 相同的属性创建，
      // getContext 幂等，createRenderer 会拿到同一上下文）
      const gl2 = c.getContext("webgl2", {
        premultipliedAlpha: false,
        antialias: false,
        alpha: false,
        preserveDrawingBuffer: true,
      });
      if (!gl2) throw new Error("WEBGL2_UNAVAILABLE");

      const sceneEntry = pkg.getEntry(parsedPkg, "scene.json");
      if (!sceneEntry) throw new Error("pkg 中没有 scene.json（不是场景壁纸？）");
      const scene = scn.parseScene(JSON.parse(readText(sceneEntry)), project);
      // 暂不渲染 3D 模型 / 组件（时钟、天气）/ 文字对象：先从中筛掉这些图层
      if (SKIP_COMPONENTS || SKIP_TEXT) {
        scene.layers = scene.layers.filter((l: any) => {
          if (SKIP_COMPONENTS && l.isComponent) return false;
          if (SKIP_TEXT && l.isText) return false;
          return true;
        });
      }
      // 暂不应用图层效果（见 SKIP_SCENE_EFFECTS 注释）：清空 effects，避免灰色遮罩/随机多边形
      if (SKIP_SCENE_EFFECTS) {
        for (const l of scene.layers as any[]) {
          l.effects = [];
        }
      }

      const shaderResolver = (rel: string): Promise<string | null> => {
        const inner = rel.startsWith("shaders/") ? rel : "shaders/" + rel;
        const file = rel.startsWith("shaders/") ? rel.slice("shaders/".length) : rel;
        const e = pkg.getEntry(parsedPkg, inner);
        if (e) return Promise.resolve(readText(e));
        return Promise.resolve(WE_SHADER_HEADERS[file] ?? null);
      };
      const renderer = rnd.createRenderer(c, {
        shaderResolver,
        diag: (msg: string) => reportDiag(cfg, `renderer: ${msg}`),
        // 效果链 FBO 降采样：降低 GPU 缓冲内存（全质量=0，0.5≈效果分辨率减半→内存约 1/4）
        fboCapFactor: 0.5,
      });
      // 立即登记渲染器：即使后续异步加载中途被 clear()，也能正确释放该 WebGL 上下文
      state.renderer = renderer;
      if (disposed) return;

      const textures = new Map<string, any>();
      textures.set("util/white", {
        glTex: rnd.makeTexture(renderer.gl, new Uint8Array([255, 255, 255, 255]), 1, 1),
        width: 1,
        height: 1,
        rg88: false,
      });
      textures.set("util/noflow", {
        glTex: rnd.makeTexture(renderer.gl, new Uint8Array([127, 127, 127, 255]), 1, 1),
        width: 1,
        height: 1,
        rg88: false,
      });
      textures.set("util/noise", {
        glTex: rnd.makeTexture(renderer.gl, noise.generateNoiseTexture(), 256, 256),
        width: 256,
        height: 256,
        rg88: false,
        mips: null,
      });

      const loadTex = async (name: string): Promise<any | null> => {
        if (textures.has(name)) return textures.get(name);
        const texEntry = pkg.getEntry(parsedPkg, `materials/${name}.tex`);
        if (!texEntry) return null;
        const parsedTex = tex.parseTex(texEntry);
        const m = tex.decodeMip0(parsedTex);
        const rg88 = parsedTex.format === 8;
        let entry: any = null;
        if (m.video !== undefined) {
          const url = URL.createObjectURL(new Blob([m.video], { type: "video/mp4" }));
          const videoEl = document.createElement("video");
          videoEl.src = url;
          videoEl.loop = true;
          videoEl.muted = true;
          videoEl.playsInline = true;
          videoEl.style.cssText =
            "position:fixed;left:-9999px;top:-9999px;width:2px;height:2px;opacity:0;pointer-events:none";
          document.body.appendChild(videoEl);
          // 登记以便 clear()/页面卸载时暂停移除元素并 revoke blob URL
          (state.videoTextures ??= []).push(videoEl);
          (state.objectUrls ??= []).push(url);
          videoEl.addEventListener("loadedmetadata", () => {
            reportDiag(cfg, `video tex '${name}': ${videoEl.videoWidth}x${videoEl.videoHeight}`);
          });
          videoEl.addEventListener("error", () => {
            reportDiag(cfg, `video tex '${name}' ERROR: ${videoEl.error?.code}`);
          });
          void videoEl.play().catch((e) => reportDiag(cfg, `video play fail '${name}': ${String(e).slice(0, 80)}`));
          entry = {
            video: videoEl,
            glTex: rnd.makeTexture(renderer.gl, new Uint8Array([0, 0, 0, 0]), 1, 1),
            width: m.width,
            height: m.height,
            rg88: false,
            lastUploaded: -1,
          };
        } else if (m.png !== undefined || (m.image !== undefined && m.fif === tex.FIF.JPEG)) {
          const blob = new Blob([(m.png || m.image) as BlobPart], {
            type: m.png ? "image/png" : "image/jpeg",
          });
          const bmp = await createImageBitmap(blob);
          entry = {
            glTex: rnd.makeTexture(renderer.gl, null, 0, 0, bmp),
            width: bmp.width,
            height: bmp.height,
            rg88,
          };
        } else if (m.image !== undefined) {
          return null;
        } else {
          const m0 = tex.decodeMip0(parsedTex) as { width: number; height: number; rgba: Uint8Array };
          entry = {
            glTex: rnd.makeTextureMip(renderer.gl, [m0], rg88),
            width: m0.width,
            height: m0.height,
            rg88,
            mips: [m0],
          };
        }
        if (!entry) return null;
        textures.set(name, entry);
        return entry;
      };

      let loadedTex = 0;
      for (let li = 0; li < scene.layers.length; li++) {
        const layer = scene.layers[li];
        if (!layer.image) continue;
        try {
          let model: unknown;
          if (eff.BUILTIN_MODELS[layer.image]) {
            model = eff.BUILTIN_MODELS[layer.image];
          } else {
            const modelEntry = pkg.getEntry(parsedPkg, layer.image);
            if (!modelEntry) continue;
            model = JSON.parse(readText(modelEntry));
          }
          const mat = scn.resolveMaterial(model);
          if (!mat) continue;
          let material: unknown;
          if (eff.BUILTIN_MATERIALS[mat.materialPath]) {
            material = eff.BUILTIN_MATERIALS[mat.materialPath];
          } else {
            const matEntry = pkg.getEntry(parsedPkg, mat.materialPath);
            if (!matEntry) continue;
            material = JSON.parse(readText(matEntry));
          }
          const pass = (material as { passes?: Array<{ textures?: string[] }> }).passes?.[0];
          const texName = pass?.textures?.[0];
          if (texName) {
            if (!textures.has(texName) && (await loadTex(texName))) {
              layer.textureName = texName;
              loadedTex++;
            } else if (textures.has(texName)) {
              layer.textureName = texName;
              loadedTex++;
            }
          }
          for (const e of layer.effects || []) {
            eff.resolveEffectChain(parsedPkg, e, readText);
          }
          for (const e of layer.effects || []) {
            for (const p of e.passes || []) {
              for (const tn of p.textures || []) {
                if (
                  typeof tn === "string" &&
                  tn !== "" &&
                  !tn.startsWith("util/") &&
                  !tn.startsWith("_rt_")
                ) {
                  await loadTex(tn);
                }
              }
            }
          }
        } catch (e) {
          console.warn(`图层 ${layer.name || li} 加载失败: ${(e as Error).message}`);
        }
      }

      // ---- 粒子系统（particle 图层）----
      // 加载粒子模型 json + 材质 + 贴图，构造 ParticleSystem，注入 renderer
      const particleSystems: any[] = [];
      for (const layer of scene.layers) {
        if (!layer.particle || !layer.visible) continue;
        if (SKIP_PARTICLES) continue; // 暂不渲染粒子（雪/雨/zzz）
        try {
          const modelEntry = pkg.getEntry(parsedPkg, layer.particle);
          if (!modelEntry) continue;
          const model = JSON.parse(readText(modelEntry));
          const ps = new particles.ParticleSystem(renderer.gl, model, layer.instanceoverride);
          // 材质（决定混合模式）
          if (model.material) {
            const matEntry = pkg.getEntry(parsedPkg, model.material);
            if (matEntry) ps.setMaterial(JSON.parse(readText(matEntry)));
            // 贴图：材质 pass 纹理槽 0
            const mat = model.material
              ? JSON.parse(readText(pkg.getEntry(parsedPkg, model.material)))
              : null;
            const pass = mat?.passes?.[0];
            const texName = pass?.textures?.[0];
            if (texName) {
              const te = await loadTex(texName);
              if (te) ps.setTexture({ glTex: te.glTex, width: te.width, height: te.height });
            }
          }
          ps.setVisible(true);
          particleSystems.push(ps);
        } catch (e) {
          console.warn(`粒子图层 ${layer.name} 加载失败: ${(e as Error).message}`);
        }
      }
      // ---- 声音图层（sound 对象）----
      // 从 scene.pkg 提取声音文件 → Blob → audio 播放；受 cfg.muted 控制
      const soundAudios: HTMLAudioElement[] = [];
      for (const layer of scene.layers) {
        if (!layer.sound || !layer.sound.length) continue;
        try {
          for (const snd of layer.sound) {
            const entry = pkg.getEntry(parsedPkg, snd);
            if (!entry) continue;
            // 尝试多种 mime（WE 声音多为 wav/mp3/ogg/flac）
            const ext = (snd.split(".").pop() || "").toLowerCase();
            const mime =
              ext === "mp3" ? "audio/mpeg" : ext === "ogg" ? "audio/ogg" : ext === "flac" ? "audio/flac" : "audio/wav";
            const blob = new Blob([entry as BlobPart], { type: mime });
            const url = URL.createObjectURL(blob);
            // 登记以便 clear()/页面卸载时 revoke blob URL
            (state.objectUrls ??= []).push(url);
            const au = document.createElement("audio");
            au.src = url;
            au.loop = layer.soundprops?.playbackmode === "loop";
            au.volume = Math.max(0, Math.min(1, layer.soundprops?.volume ?? 1));
            au.muted = cfg.muted !== false;
            // 延迟启动（startsilent 或默认不立即响）
            au.play().catch(() => {});
            // 循环播放时循环
            soundAudios.push(au);
            break; // 每个图层播放第一个声音
          }
        } catch (e) {
          console.warn(`声音图层 ${layer.name} 加载失败: ${(e as Error).message}`);
        }
      }
      // 提供 setVolume 控制（含 muted 切换）
      const setSceneVolume = (vol: number) => {
        for (const au of soundAudios) {
          au.volume = Math.max(0, Math.min(1, vol));
          au.muted = vol <= 0;
        }
      };
      state.sceneAudio = { setVolume: setSceneVolume, audios: soundAudios };
      if (disposed) return;
      // 粒子每帧推进 + 渲染（叠加在场景之上，同投影）
      let lastPt = performance.now();
      renderer.setParticleRenderer((cam: any, viewProj: any, w: number, h: number) => {
        const now = performance.now();
        const pdt = Math.min(0.05, (now - lastPt) / 1000);
        lastPt = now;
        for (const ps of particleSystems) ps.advance(pdt);
        for (const ps of particleSystems) ps.render(viewProj, w, h, cam.projW, cam.projH);
        // 叠加 mdl 网格
        if (mdlRenderer && mdlItems.length > 0) {
          try {
            for (const item of mdlItems) {
              if (!item.tex) continue;
              const l = item.layer;
              // 放置：网格中心对齐 layer origin，缩放匹配 layer scale
              const b = item.mdl.bounds;
              const cx = (b.minX + b.maxX) / 2;
              const cy = (b.minY + b.maxY) / 2;
              const w0 = b.maxX - b.minX || 1;
              const h0 = b.maxY - b.minY || 1;
              const targetW = (l.size?.[0] || w0) * (l.scale?.[0] || 1);
              const targetH = (l.size?.[1] || h0) * (l.scale?.[1] || 1);
              const sx = targetW / w0;
              const sy = targetH / h0;
              const ox = (l.origin?.[0] || 0) - cx * sx;
              const oy = (l.origin?.[1] || 0) - cy * sy;
              mdlRenderer.draw(viewProj, item.mdl, { scaleX: sx, scaleY: sy, offsetX: ox, offsetY: oy }, item.tex.glTex);
            }
          } catch (e) {
            console.warn("mdl 渲染失败:", (e as Error).message);
          }
        }
      });
      reportDiag(cfg, `particles: ${particleSystems.length} systems`);

      // ---- 3D 模型图层（puppet mdl）----
      // model json 有 puppet 字段 → 解析 mdl 网格 + 材质贴图，叠加渲染
      // 按需求暂不渲染 3D 模型：mdlItems 不变为对象，直接保持空（后续按需开启 SKIP_3D_MODELS）
      const mdlItems: { mdl: any; tex: any; layer: any }[] = [];
      for (const layer of scene.layers) {
        if (!layer.image || !layer.visible) continue;
        if (SKIP_3D_MODELS) continue; // 忽略 3D 模型图层
        try {
          // 加载 model json 检查 puppet
          let model: any;
          if (eff.BUILTIN_MODELS[layer.image]) continue; // 内置模型非 puppet
          const modelEntry = pkg.getEntry(parsedPkg, layer.image);
          if (!modelEntry) continue;
          model = JSON.parse(readText(modelEntry));
          const puppet = model.puppet;
          if (!puppet) continue;
          const mdlEntry = pkg.getEntry(parsedPkg, puppet);
          if (!mdlEntry) continue;
          const mdlObj = mdl.parseMDL(new Uint8Array(mdlEntry as ArrayBuffer));
          // 材质贴图
          const mat = scn.resolveMaterial(model);
          let texObj: any = null;
          if (mat) {
            const matEntry = pkg.getEntry(parsedPkg, mat.materialPath);
            if (matEntry) {
              const material = JSON.parse(readText(matEntry));
              const pass = material.passes?.[0];
              const texName = pass?.textures?.[0];
              if (texName) {
                const te = await loadTex(texName);
                if (te) texObj = te;
              }
            }
          }
          mdlItems.push({ mdl: mdlObj, tex: texObj, layer });
          reportDiag(cfg, `mdl '${layer.name}' bounds=(${mdlObj.bounds.minX.toFixed(0)},${mdlObj.bounds.minY.toFixed(0)})-(${mdlObj.bounds.maxX.toFixed(0)},${mdlObj.bounds.maxY.toFixed(0)}) v=${mdlObj.vertexCount}`);
        } catch (e) {
          console.warn(`3D 图层 ${layer.name} 加载失败: ${(e as Error).message}`);
        }
      }
      let mdlRenderer: any = null;
      if (mdlItems.length > 0) {
        try {
          mdlRenderer = mdl.createMDLRenderer(renderer.gl);
          for (const item of mdlItems) mdlRenderer.upload(item.mdl);
        } catch (e) {
          console.warn(`3D 渲染器初始化失败: ${(e as Error).message}`);
          mdlRenderer = null;
        }
      }
      // 在 renderScene 后叠加绘制 mdl
      if (disposed) return;
      reportDiag(cfg, `mdl: ${mdlItems.length} meshes`);

      // ---- 文字对象 / 组件（时钟、日期、星期等动态文本）----
      // 用 2D overlay canvas 叠加在 WebGL 之上绘制；字体从 pkg 的 fonts/*.ttf 加载
      const textLayers = scene.layers.filter((l: any) => l.isText && l.visible);
      let textOverlay: HTMLCanvasElement | null = null;
      let textCtx: CanvasRenderingContext2D | null = null;
      const loadedFonts = new Map<string, string>(); // fontPath -> family
      if (textLayers.length > 0) {
        const ov = document.createElement("canvas");
        ov.width = c.width;
        ov.height = c.height;
        ov.style.cssText = "position:absolute;inset:0;width:100%;height:100%;pointer-events:none;";
        wrap.appendChild(ov);
        textOverlay = ov;
        textCtx = ov.getContext("2d");
        // 加载场景用到的字体
        const fontPaths = new Set<string>();
        for (const l of textLayers) if (l.textFont) fontPaths.add(l.textFont);
        for (const fp of fontPaths) {
          try {
            const fe = pkg.getEntry(parsedPkg, fp);
            if (!fe) continue;
            const fam = "wefont_" + fp.split("/").pop()!.replace(/[^a-zA-Z0-9]/g, "_");
            const ff = new FontFace(fam, URL.createObjectURL(new Blob([fe as BlobPart])));
            await ff.load();
            document.fonts.add(ff);
            loadedFonts.set(fp, fam);
          } catch (e) {
            console.warn(`字体加载失败 ${fp}: ${(e as Error).message}`);
          }
        }
      }
      if (disposed) return;
      // 每帧绘制文字
      // 投影换算：世界坐标 → 屏幕物理像素。场景投影(projW×projH)经 fitWindow 裁切/留边后映射到画布。
      const ortho = (scene as any).general?.orthogonalprojection;
      const projW = ortho?.width || c.width;
      const projH = ortho?.height || c.height;
      const drawTextFrame = () => {
        if (!textOverlay || !textCtx) return;
        const ctx = textCtx;
        ctx.clearRect(0, 0, textOverlay.width, textOverlay.height);
        const win = fitWindow(normalizeFit(state.cfg.fit), projW, projH, c.width, c.height);
        const scaleX = c.width / win.viewW;
        const scaleY = c.height / win.viewH;
        const nowDate = new Date();
        for (const layer of textLayers) {
          try {
            const txt = resolveText(layer, nowDate);
            if (txt === null || txt === undefined || txt === "") continue;
            // 世界坐标(y向下) → 画布物理像素：经可见窗口偏移后映射，裁切/留边时文字位置仍正确
            const wx = layer.origin[0];
            const wy = layer.origin[1];
            const px = (wx - win.offX) * scaleX;
            const py = (win.offY + win.viewH - wy) * scaleY;
            const sx = layer.scale[0] || 1;
            const sy = layer.scale[1] || 1;
            const basePts = layer.textPointsize || 24;
            const pts = basePts * sy * scaleY;
            const fam = layer.textFont ? (loadedFonts.get(layer.textFont) || "sans-serif") : "sans-serif";
            const color = layer.textColor;
            const align = layer.textHAlign || "center";
            const valign = layer.textVAlign || "center";
            ctx.save();
            ctx.font = `400 ${Math.max(1, pts)}px "${fam}", sans-serif`;
            ctx.fillStyle = `rgba(${Math.round(color[0] * 255)},${Math.round(color[1] * 255)},${Math.round(color[2] * 255)},${layer.alpha ?? 1})`;
            ctx.textAlign = align === "left" ? "left" : align === "right" ? "right" : "center";
            ctx.textBaseline = valign === "top" ? "top" : valign === "bottom" ? "bottom" : "middle";
            ctx.fillText(txt, px, py);
            ctx.restore();
          } catch (e) {
            /* 单文字失败不影响 */
          }
        }
      };
      if (disposed) return;
      const start = performance.now();
      let lastRender = -Infinity;
      const renderLoop = (now: number) => {
        if (disposed) return;
        // 帧率上限：比目标帧更快的帧直接跳过（不渲染、只继续排队），降低 GPU 占用。
        const fps = state.cfg.sceneFps || 60;
        const interval = 1000 / fps;
        if (now - lastRender >= interval) {
          lastRender = now;
          const t = (now - start) / 1000;
          void renderer
            .render(scene, textures, c.width, c.height, t, normalizeFit(state.cfg.fit))
            .then(() => {
              if (disposed) return;
              try { drawTextFrame(); } catch (e) { /* 文字绘制失败忽略 */ }
              state.raf = requestAnimationFrame(renderLoop);
            })
            .catch((e: Error) => {
              console.warn("scene render error:", e);
              reportDiag(cfg, `render: ${String(e.message || e).slice(0, 200)}`);
              disposed = true;
            });
        } else {
          state.raf = requestAnimationFrame(renderLoop);
        }
      };
      state.raf = requestAnimationFrame(renderLoop);
      reportDiag(cfg, `renderer started: ${scene.layers.length} layers`);
    } catch (e) {
      // 场景加载/渲染失败：diag 上报 + 降级画布演示
      console.warn("scene render failed:", e);
      reportDiag(cfg, `failed: ${String((e as Error).message || e).slice(0, 200)}`);
      mountDefaultWallpaper();
    }
  })();
}

function mount(cfg: WallpaperConfig) {
  if (cfg.type === "video" && cfg.src) mountVideo(cfg);
  else if ((cfg.type === "gif" || cfg.type === "image") && cfg.src) mountGif(cfg);
  else if (cfg.type === "web" && cfg.src) mountWeb(cfg);
  else if (cfg.type === "scene" && cfg.src) mountScene(cfg);
  else mountDefaultWallpaper(); // 无壁纸/默认配置 → 精美 HTML 默认壁纸
}

// 原生控制接口
declare global {
  interface Window {
    __wp?: {
      setWallpaper(cfg: WallpaperConfig): void;
      pause(): void;
      resume(): void;
      setFit(fit: string): void;
      setVolume(volume: number): void;
      release(): void;
      restore(): void;
      setRenderDpr(dpr: number): void;
      setSceneFps(fps: number): void;
    };
  }
}

window.__wp = {
  setWallpaper(cfg: WallpaperConfig) {
    state.cfg = cfg;
    mount(cfg);
  },
  pause() {
    state.video?.pause();
    if (state.raf !== undefined) {
      cancelAnimationFrame(state.raf);
      state.raf = undefined;
    }
  },
  resume() {
    state.video?.play().catch(() => {});
    if (state.cfg.type === "canvas") startCanvasLoop();
    else if (state.cfg.type === "scene") {
      // 场景暂停后重挂 rAF：sceneCleanup 已被 clear() 触发，重新挂载
      if (!state.sceneCleanup && state.canvas) {
        // 简单恢复：重新挂载
        mount(state.cfg);
      }
    }
  },
  setFit(fit: string) {
    state.cfg.fit = fit as WallpaperFit;
    const obj = state.video ?? state.img;
    if (obj) {
      const f = fitObjectFit(fit as WallpaperFit);
      obj.style.objectFit = f.objectFit;
      obj.style.background = f.background;
    }
    // 场景壁纸：fit 由渲染循环每帧读取 state.cfg.fit 并传给 fitWindow，无需重挂载即可实时切换
  },
  setVolume(volume: number) {
    if (state.video) {
      state.video.volume = Math.max(0, Math.min(1, volume));
      state.video.muted = volume <= 0;
    }
    if (state.sceneAudio) {
      state.sceneAudio.setVolume(volume);
    }
  },
  // 释放壁纸渲染资源（画布/WebGL/视频/iframe），归还内存；保留 state.cfg 供 restore() 重建
  release() {
    clear();
  },
  // 重新挂载上次配置（显示器睡眠后唤醒、或 release() 之后恢复）
  restore() {
    if (state.cfg) mount(state.cfg);
  },
  // 动态调整渲染分辨率上限：需重建画布，重挂当前配置
  setRenderDpr(dpr: number) {
    state.cfg.renderDpr = dpr;
    mount(state.cfg);
  },
  // 调整场景帧率：渲染循环每帧读取 state.cfg.sceneFps，无需重挂载即可实时生效
  setSceneFps(fps: number) {
    state.cfg.sceneFps = fps;
  },
};

// 初始配置优先取自 URL query（壁纸引擎窗口创建时注入，同步无竞态）
const params = new URLSearchParams(location.search);
const initialCfg: WallpaperConfig = {
  type: (params.get("type") as WallpaperConfig["type"]) ?? "canvas",
  src: params.get("src") ?? undefined,
  fit: (params.get("fit") as WallpaperConfig["fit"]) ?? "cover",
  renderDpr: Number(params.get("renderDpr")) || 1,
  sceneFps: Number(params.get("sceneFps")) || 60,
  muted: params.get("muted") !== "false",
  loop: params.get("loop") !== "false",
  mediaBase: params.get("mediaBase") ?? undefined,
};
state.cfg = initialCfg;
mount(initialCfg);

// 诊断：确认壁纸窗口是否收到鼠标事件（上报 /diag，仅首次，避免刷屏）
let diagMouseOnce = false;
const diagMouse = (ev: Event, label: string) => {
  if (diagMouseOnce) return;
  diagMouseOnce = true;
  reportDiag(initialCfg, `${label} 收到`);
};
window.addEventListener("mousemove", (e) => diagMouse(e, "mousemove"), { once: true, passive: true });
window.addEventListener("mousedown", (e) => diagMouse(e, "mousedown"), { once: true, passive: true });

// 页面卸载兜底：预览 iframe 关闭 / 壁纸窗口销毁时释放 WebGL 上下文与 blob URL。
// （clear() 内部用 sceneCleanup 置 disposed + renderer.dispose，对已进入卸载流程的 iframe 安全。）
window.addEventListener("pagehide", () => clear());
window.addEventListener("beforeunload", () => clear());

export {};
