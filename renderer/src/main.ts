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
import * as particleTexMod from "../vendor/we-scene/render/particle-textures.js";
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
const ptex = asAny(particleTexMod);
const mdl = asAny(mdlMod);

// 场景壁纸渲染开关（手动测试用）
const SKIP_3D_MODELS = false; // puppet 骨骼网格（人物模型）
const SKIP_COMPONENTS = true; // 暂不渲染组件（时钟、天气等）
const SKIP_TEXT = true; // 暂不渲染文字对象
const SKIP_PARTICLES = false; // 粒子（雪 / 雨 / 火花 / 雾 / 光轴）
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
  /** 场景内视频纹理循环对（每纹理一个） */
  videoPairs?: VideoLoopPair[];
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
  // 停掉场景视频纹理循环对（取消 rAF 交换驱动 + 暂停解码）
  // （视频壁纸本身已是单 video + 原生 loop，无需在此处理）
  for (const p of state.videoPairs ?? []) p.destroy();
  state.videoPairs = undefined;
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
  if (state.video) {
    // 非循环视频：同样清 src + load() 释放解码器（仅 remove 节点/pause 不足）
    state.video.pause();
    state.video.removeAttribute("src");
    try {
      state.video.load();
    } catch {
      /* 忽略 */
    }
    state.video.remove();
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

// ---------- 无缝循环视频（A/B 双元素：预热-保温-结尾交接） ----------
// WebKit 的 <video loop> 在循环点会重置解码管线（ended → seek 0 → 重新起播），
// 造成 0.1~0.5s 的短暂冻结（缓冲再充分也躲不掉）。方案：主/备两个同源 <video>：
//   1) 预热：主元素临近结尾时，备用元素起播一两帧后**暂停保温**——解码器已热、
//      合成器已持有其当前帧（交接零延迟），且不会长时间双路解码；
//   2) 交接：主元素真正到达结尾的一瞬（≤2~5 帧内）恢复备用播放并交换主备，
//      切换点即真实循环点，内容不跳跃；
//   3) 旧主元素暂停归零静音保温，成为下一个备用（保温期静音避免双声）。
// 备用未及时就绪时退回原生 loop 兜底（只是旧式微卡顿，不会中断）。

/// 预热窗口（秒）：主元素剩多少秒时启动备用预热。
const LOOP_PREROLL_SEC = 0.5;
/// 预热起播到多少秒后暂停保温（≈1~2 帧，帧已解码）。
const LOOP_HOLD_SEC = 0.04;
/// 主元素离结尾多少秒内执行交接（≈2~5 帧）。
const LOOP_SWAP_EPS = 0.08;

type VideoLoopPair = {
  readonly active: HTMLVideoElement;
  readonly standby: HTMLVideoElement;
  /** 每次主备交换回调（参数为新主元素），用于同步可见性/纹理引用 */
  onSwap?: (active: HTMLVideoElement) => void;
  /** 兜底触发回调：备用未就绪、发生原生循环回绕时调用 */
  onFallback?: () => void;
  setVolume(vol: number): void;
  pause(): void;
  resume(): void;
  destroy(): void;
};

function createLoopingVideo(src: string, opts: { muted: boolean }): VideoLoopPair {
  const make = (): HTMLVideoElement => {
    const v = document.createElement("video");
    v.src = src;
    v.muted = opts.muted;
    v.playsInline = true;
    // preload=metadata：只拉元数据，避免 WebKit 预下载整个视频文件进内存
    v.preload = "metadata";
    // 限制解码分辨率：按「窗口尺寸 × 有效 dpr」解码，而非视频原始分辨率。
    // WebKit 对超出显示尺寸的 video 会分配等比缩小的解码缓冲（4K 源在 1080p 窗口上
    // 解码缓冲约为 1/4），显著降低内存。不影响显示清晰度（object-fit 在 CSS 层面缩放）。
    const dpr = Math.min(window.devicePixelRatio || 1, state.cfg.renderDpr || 1);
    const maxW = Math.max(1, Math.round(innerWidth * dpr));
    const maxH = Math.max(1, Math.round(innerHeight * dpr));
    v.width = maxW;
    v.height = maxH;
    // 兜底：预热/交接失败时退回原生循环（只是旧式微卡顿，不会中断或黑屏）
    v.loop = true;
    return v;
  };

  let active = make();
  let standby = make();
  let userVolume = 1;
  let userMuted = opts.muted;
  standby.muted = true; // 备用恒静音，避免交接期出双声
  let arming = false; // 备用已起播，等第一帧出现后暂停保温
  let held = false; // 备用已保温（暂停在起点附近、解码器热）
  let prevTime = -1; // 主元素上一帧时间，用于检测原生循环回绕
  let raf = 0;
  let destroyed = false;

  const disarm = () => {
    arming = false;
    held = false;
    if (!standby.paused) {
      standby.pause();
      try {
        standby.currentTime = 0;
      } catch {
        /* 忽略 */
      }
    }
  };

  const doSwap = () => {
    const prev = active;
    active = standby;
    standby = prev;
    arming = false;
    held = false;
    // 新主元素从保温点（≈循环点）恢复播放：解码器热、合成器已有帧 → 零延迟
    void active.play().catch(() => {});
    active.muted = userMuted;
    active.volume = userVolume;
    // 旧主元素暂停归零静音保温，成为下一个备用
    prev.pause();
    try {
      prev.currentTime = 0;
    } catch {
      /* 忽略 */
    }
    prev.muted = true;
    prevTime = -1;
    pair.onSwap?.(active);
  };

  const tick = () => {
    if (destroyed) return;
    raf = requestAnimationFrame(tick);
    const d = active.duration;
    if (!isFinite(d) || d <= 0) return;
    const t = active.currentTime;
    // 时间回绕 = 交接失败、发生了原生兜底循环：备用已保温则立刻交接（仍近乎无缝），
    // 否则复位等下一圈（一次性上报 onFallback 便于诊断）
    if (prevTime >= 0 && t < prevTime - 0.05) {
      const hadStandby = held;
      disarm();
      prevTime = t;
      if (!hadStandby) pair.onFallback?.();
      return;
    }
    prevTime = t;
    const remaining = d - t;
    // 远离结尾：复位预热状态（兜底循环后 / seek 后都会经过这里）
    if (remaining > LOOP_PREROLL_SEC + 0.5) {
      if (arming || held) disarm();
      return;
    }
    if (held) {
      // 备用保温待命：主元素到达结尾即交接
      if (remaining <= LOOP_SWAP_EPS || t <= LOOP_SWAP_EPS) doSwap();
      return;
    }
    if (arming) {
      if (!standby.paused) {
        // 起播出现帧即暂停保温
        if (standby.currentTime >= LOOP_HOLD_SEC * 0.5) {
          standby.pause();
          arming = false;
          held = true;
        }
      } else {
        // 起播被拒/未开始：重试（远离结尾时由上方复位）
        void standby.play().catch(() => {});
      }
      return;
    }
    // 临近结尾：备用解码器就绪才预热，否则交给原生 loop 兜底
    if (remaining <= LOOP_PREROLL_SEC && standby.readyState >= 2) {
      try {
        standby.currentTime = 0;
      } catch {
        /* 忽略 */
      }
      void standby.play().catch(() => {});
      arming = true;
    }
  };
  raf = requestAnimationFrame(tick);

  const pair: VideoLoopPair = {
    get active() {
      return active;
    },
    get standby() {
      return standby;
    },
    onSwap: undefined,
    onFallback: undefined,
    setVolume(vol: number) {
      userVolume = Math.max(0, Math.min(1, vol));
      userMuted = vol <= 0;
      active.muted = userMuted;
      active.volume = userVolume;
      // 备用保持静音（交接期防双声），但音量同步，交接后立即正确
      standby.muted = true;
      standby.volume = userVolume;
    },
    pause() {
      // 放弃未完成的预热：备用停掉归零，恢复时重新走预热流程
      disarm();
      active.pause();
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
    },
    resume() {
      if (destroyed) return;
      void active.play().catch(() => {});
      if (!raf) raf = requestAnimationFrame(tick);
    },
    destroy() {
      destroyed = true;
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
      for (const v of [active, standby]) {
        v.pause();
        // 关键：清掉 src 并 load()，触发 WebKit 释放解码器/解码缓冲区（仅 pause 不会归还内存）
        v.removeAttribute("src");
        try {
          v.load();
        } catch {
          /* 忽略 */
        }
        v.remove();
      }
    },
  };
  return pair;
}

// ---------- 视频 / 图片 / GIF：走场景引擎渲染 ----------
//
// 视频与图片类型壁纸不再各自维护一条 DOM 渲染路径（<video object-fit> / <img object-fit>），
// 而是合成一个「单图层场景」交给 we-scene 渲染。这样 fit / renderDpr / sceneFps / 效果链
// 只有一份实现，媒体类壁纸自动获得与场景壁纸一致的行为；后续要给媒体加效果
// （模糊、色调、粒子叠加）也只是往这个合成场景里加层，不必再碰 DOM 分支。
//
// 三种媒体的纹理供给方式不同，但都不需要给 vendor 打新补丁：
//   video —— 渲染器本身就有视频纹理分支（帧时间戳变化时上传当前帧）；
//   gif   —— 每帧由本文件把 <img> 重新上传（GIF 动画由浏览器内部推进，
//            texImage2D 取到的即当前帧）；
//   image —— 一次性上传位图。
//
// WebGL2 不可用时回退原来的 DOM 路径（mountVideoDom / mountGifDom），
// 保证无 WebGL 环境里媒体壁纸仍可显示。

/** 合成单图层场景：投影尺寸取媒体自身像素，fit 交给 buildCamera/fitWindow（语义同 object-fit） */
function buildMediaScene(width: number, height: number, textureName: string) {
  return {
    camera: null,
    // contain 模式的留边由 clearcolor 填充（对应 DOM 路径里的深色背景）
    general: { orthogonalprojection: { width, height }, clearenabled: true, clearcolor: "0 0 0" },
    layers: [
      {
        id: 0,
        name: "media",
        visible: true,
        image: textureName,
        textureName,
        particle: null,
        puppet: null,
        solid: false,
        isContainer: false,
        isPostProcess: false,
        isText: false,
        isSound: false,
        isComponent: false,
        sound: [],
        // 铺满整个投影：层中心在投影中心、尺寸等于投影尺寸
        origin: [width / 2, height / 2, 0],
        scale: [1, 1, 1],
        angles: [0, 0, 0],
        size: [width, height],
        alignment: "center",
        color: [1, 1, 1],
        alpha: 1,
        brightness: 1,
        colorBlendMode: 0,
        copybackground: false,
        parallaxDepth: null,
        animationLayers: [],
        effects: [],
      },
    ],
    properties: {},
  };
}

/**
 * 用 ImageDecoder 把 GIF 解成一组 ImageBitmap（各帧带自己的时长）。
 *
 * 为什么不能直接把 `<img src=*.gif>` 当纹理源逐帧上传：GIF 的动画只推进**用于合成显示**
 * 的那份帧，`drawImage` / `texImage2D` 取到的始终是首帧。实测三种摆放（脱离文档、
 * 屏幕外、可见 64×64）在 1.5s 内取到的像素**完全无变化**，90 帧的 GIF 渲染成静止画。
 * ImageDecoder 是显式的逐帧解码接口，能拿到真实帧与 `duration`。
 *
 * 代价：整段动画的位图常驻内存（256×256×90 帧 ≈ 23MB）。GIF 壁纸通常是小尺寸预览级
 * 素材，可接受；解码失败或无 ImageDecoder 时回退静态首帧（画面不动但不黑屏）。
 */
async function decodeGifFrames(
  src: string,
): Promise<{ width: number; height: number; frames: { bitmap: ImageBitmap; durationMs: number }[] } | null> {
  const resp = await fetch(src);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  const dec = new (window as any).ImageDecoder({
    data: await resp.arrayBuffer(),
    type: "image/gif",
  });
  // tracks.ready 先于 completed：轨道未就绪时 selectedTrack 为 null
  await dec.tracks.ready;
  await dec.completed;
  const track = dec.tracks.selectedTrack;
  if (!track || !track.frameCount) return null;
  // 帧数上限：防病态素材（上千帧）把内存吃光；超出部分截断（动画变短，不影响可用）
  const count = Math.min(track.frameCount, 300);
  const frames: { bitmap: ImageBitmap; durationMs: number }[] = [];
  let width = 0;
  let height = 0;
  for (let i = 0; i < count; i++) {
    const { image } = await dec.decode({ frameIndex: i });
    width = image.displayWidth;
    height = image.displayHeight;
    frames.push({
      bitmap: await createImageBitmap(image),
      // duration 单位是微秒；缺失/为 0 的帧按 GIF 惯例给 100ms
      durationMs: image.duration ? image.duration / 1000 : 100,
    });
    image.close();
  }
  dec.close?.();
  return { width, height, frames };
}

function mountMedia(cfg: WallpaperConfig) {
  clear();
  if (!cfg.src) {
    mountDefaultWallpaper();
    return;
  }
  const isVideo = cfg.type === "video";
  const isGif = cfg.type === "gif";

  const c = document.createElement("canvas");
  const dpr = effectiveDpr(cfg);
  c.width = Math.max(1, Math.round(innerWidth * dpr));
  c.height = Math.max(1, Math.round(innerHeight * dpr));
  c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;";
  const gl2 = c.getContext("webgl2", {
    premultipliedAlpha: false,
    antialias: false,
    alpha: false,
    preserveDrawingBuffer: true,
  });
  if (!gl2) {
    // 无 WebGL2：回退 DOM 路径，媒体壁纸照常显示（只是拿不到场景引擎的能力）
    reportDiag(cfg, `media ${cfg.type}: WEBGL2_UNAVAILABLE，回退 DOM 渲染`);
    if (isVideo) mountVideoDom(cfg);
    else mountGifDom(cfg);
    return;
  }
  wrap.appendChild(c);
  state.canvas = c;
  let disposed = false;
  state.sceneCleanup = () => {
    disposed = true;
    if (state.renderer) {
      state.renderer.dispose?.();
      state.renderer = undefined;
    }
  };

  const fail = (why: string) => {
    if (disposed) return;
    disposed = true;
    reportDiag(cfg, `media ${cfg.type} 失败: ${why}`);
    mountDefaultWallpaper();
  };

  void (async () => {
    try {
      const renderer = rnd.createRenderer(c, {
        diag: (msg: string) => reportDiag(cfg, `renderer: ${msg}`),
        fboCapFactor: 0,
      });
      state.renderer = renderer;
      if (disposed) return;

      const TEX = "__media";
      const textures = new Map<string, any>();
      /** 每帧刷新纹理（gif 用；video 由渲染器内部按 currentTime 上传） */
      let refreshTex: (() => void) | undefined;
      let mediaW = 0;
      let mediaH = 0;

      if (isVideo) {
        const v = document.createElement("video");
        v.autoplay = true;
        v.loop = cfg.loop !== false; // 默认循环；loop=false 则播完即停
        v.muted = cfg.muted !== false;
        v.playsInline = true;
        // preload=metadata：只拉元数据，避免 WebKit 预下载整个视频文件进内存
        v.preload = "metadata";
        // 限制解码分辨率：按画布尺寸而非视频原始分辨率解码，4K 源在 1080p 窗口上
        // 解码缓冲约降到 1/4（清晰度由采样阶段的缩放决定）
        v.width = c.width;
        v.height = c.height;
        // 画面走 WebGL 纹理，元素自身不参与显示（但必须在文档内才会持续解码）
        v.style.cssText =
          "position:fixed;left:-9999px;top:-9999px;width:2px;height:2px;opacity:0;pointer-events:none";
        v.src = cfg.src!;
        document.body.appendChild(v);
        state.video = v;
        (state.videoTextures ??= []).push(v);
        await new Promise<void>((ok, err) => {
          v.addEventListener("loadedmetadata", () => ok(), { once: true });
          v.addEventListener("error", () => err(new Error(`video error ${v.error?.code ?? "?"}`)), {
            once: true,
          });
        });
        if (disposed) return;
        mediaW = v.videoWidth || c.width;
        mediaH = v.videoHeight || c.height;
        // entry.video 交给渲染器的视频纹理分支：帧时间戳变化时自动上传
        textures.set(TEX, {
          video: v,
          glTex: rnd.makeTexture(renderer.gl, new Uint8Array([0, 0, 0, 255]), 1, 1),
          width: mediaW,
          height: mediaH,
          rg88: false,
          lastUploaded: -1,
        });
        void v.play().catch(() => {});
        reportDiag(cfg, `media video ${mediaW}x${mediaH} → scene 渲染`);
      } else {
        // GIF 优先走 ImageDecoder 逐帧解码（见下），失败或非 GIF 才用 <img> 位图。
        let decoded = false;
        if (isGif && typeof (window as any).ImageDecoder === "function") {
          try {
            const gifFrames = await decodeGifFrames(cfg.src!);
            if (gifFrames && gifFrames.frames.length > 1) {
              mediaW = gifFrames.width;
              mediaH = gifFrames.height;
              const gl = renderer.gl;
              const entry = {
                glTex: rnd.makeTexture(gl, null, 0, 0, gifFrames.frames[0].bitmap),
                width: mediaW,
                height: mediaH,
                rg88: false,
              };
              textures.set(TEX, entry);
              // 按各帧自己的 duration 推进（GIF 每帧时长可不同），到末帧回环
              let idx = 0;
              let nextAt = performance.now() + gifFrames.frames[0].durationMs;
              refreshTex = () => {
                const now = performance.now();
                if (now < nextAt) return;
                idx = (idx + 1) % gifFrames.frames.length;
                nextAt = now + gifFrames.frames[idx].durationMs;
                gl.bindTexture(gl.TEXTURE_2D, entry.glTex);
                try {
                  gl.texImage2D(
                    gl.TEXTURE_2D,
                    0,
                    gl.RGBA,
                    gl.RGBA,
                    gl.UNSIGNED_BYTE,
                    gifFrames.frames[idx].bitmap,
                  );
                } catch {
                  /* 单帧上传失败：保留上一帧 */
                }
              };
              // 卸载时释放解码出的位图（每帧一张 ImageBitmap，不释放会积压显存）
              const prev = state.sceneCleanup;
              state.sceneCleanup = () => {
                prev?.();
                for (const f of gifFrames.frames) f.bitmap.close();
              };
              decoded = true;
              reportDiag(
                cfg,
                `media gif ${mediaW}x${mediaH} ${gifFrames.frames.length} 帧 → scene 渲染`,
              );
            }
          } catch (e) {
            reportDiag(cfg, `gif 解码失败，回退静态首帧: ${String((e as Error).message).slice(0, 80)}`);
          }
        }
        if (!decoded) {
          const img = new Image();
          // 同源媒体端点；crossOrigin 让将来分端口调试时也能进 WebGL（否则画布被污染）
          img.crossOrigin = "anonymous";
          img.src = cfg.src!;
          await new Promise<void>((ok, err) => {
            img.addEventListener("load", () => ok(), { once: true });
            img.addEventListener("error", () => err(new Error("image load error")), { once: true });
          });
          if (disposed) return;
          mediaW = img.naturalWidth || 1;
          mediaH = img.naturalHeight || 1;
          state.img = img;
          textures.set(TEX, {
            glTex: rnd.makeTexture(renderer.gl, null, 0, 0, img),
            width: mediaW,
            height: mediaH,
            rg88: false,
          });
          reportDiag(cfg, `media ${cfg.type} ${mediaW}x${mediaH} → scene 渲染`);
        }
      }

      if (disposed) return;
      const scene = buildMediaScene(mediaW, mediaH, TEX);
      const start = performance.now();
      let lastRender = -Infinity;
      const renderLoop = (now: number) => {
        if (disposed) return;
        // 帧率上限：比目标帧更快的帧直接跳过（不渲染、只继续排队），降低 GPU 占用
        const fps = state.cfg.sceneFps || 60;
        if (now - lastRender >= 1000 / fps) {
          lastRender = now;
          refreshTex?.();
          void renderer
            .render(
              scene,
              textures,
              c.width,
              c.height,
              (now - start) / 1000,
              normalizeFit(state.cfg.fit),
            )
            .then(() => {
              if (disposed) return;
              state.raf = requestAnimationFrame(renderLoop);
            })
            .catch((e: Error) => fail(String(e.message || e).slice(0, 200)));
        } else {
          state.raf = requestAnimationFrame(renderLoop);
        }
      };
      state.raf = requestAnimationFrame(renderLoop);
    } catch (e) {
      fail(String((e as Error).message || e).slice(0, 200));
    }
  })();
}

// ---------- 视频 / 图片：DOM 回退路径（无 WebGL2 时使用）----------

function mountVideoDom(cfg: WallpaperConfig) {
  clear();
  if (!cfg.src) {
    mountDefaultWallpaper();
    return;
  }
  const fit = fitObjectFit(cfg.fit);
  const css =
    "position:absolute;inset:0;width:100%;height:100%;" +
    `object-fit:${fit.objectFit};background:${fit.background};`;
  let fellBack = false;
  const onErr = () => {
    if (fellBack) return;
    fellBack = true;
    console.warn("video error, fallback to default wallpaper");
    mountDefaultWallpaper();
  };

  // 单视频 + 原生 loop（放弃无缝循环双元素方案）。
  // 内存减半（不再有备用 standby 解码器）；代价是循环点 0.1~0.5s 轻微冻结。
  const v = document.createElement("video");
  v.autoplay = true;
  v.loop = cfg.loop !== false; // 默认循环；loop=false 则播完即停
  v.muted = cfg.muted !== false;
  v.playsInline = true;
  // preload=metadata：只拉元数据，避免 WebKit 预下载整个视频文件进内存
  v.preload = "metadata";
  // 限制解码分辨率：按「窗口尺寸 × 有效 dpr」解码，而非视频原始分辨率。
  // WebKit 对超出显示尺寸的 video 会分配等比缩小的解码缓冲（4K 源在 1080p 窗口上
  // 解码缓冲约为 1/4），显著降低内存。不影响显示清晰度（object-fit 在 CSS 层面缩放）。
  const dprV = Math.min(window.devicePixelRatio || 1, state.cfg.renderDpr || 1);
  v.width = Math.max(1, Math.round(innerWidth * dprV));
  v.height = Math.max(1, Math.round(innerHeight * dprV));
  v.style.cssText = css;
  v.src = cfg.src;
  v.addEventListener("error", onErr);
  wrap.appendChild(v);
  state.video = v;
  v.addEventListener("canplay", () => v.play().catch(() => {}), { once: true });
  reportDiag(cfg, "video mounted (single + native loop)");
}

function mountGifDom(cfg: WallpaperConfig) {
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
  // 同源网页壁纸：加载后注入 GPU 降级脚本 + 屏蔽右键菜单
  const inject = () => {
    try {
      const doc = f.contentDocument;
      if (!doc) return;
      (window as any).__blockContextMenu?.(doc);
      injectGpuThrottle(f, doc);
    } catch {
      /* 跨源/沙箱不可访问则忽略 */
    }
  };
  f.addEventListener("load", inject);
  state.iframe = f;
}

/// 调用库内网页壁纸 iframe 里的 WE 兼容 shim 控制接口（同源；跨源/无 shim 静默忽略）
function weShimCall(call: (win: any) => void) {
  try {
    const win = (state.iframe as HTMLIFrameElement | null)?.contentWindow as any;
    if (win) call(win);
  } catch {
    /* 非同源 iframe 不可访问则忽略 */
  }
}

/// 向网页壁纸 iframe 注入 GPU 降级：requestAnimationFrame 帧率节流 → 限制
/// WebGL/canvas 动画到 sceneFps（默认 30），显著降低 GPU 占用。
/// 注意：不做 CSS transform 缩放（会破坏壁纸布局）；仅节流 rAF，安全且有效。
function injectGpuThrottle(f: HTMLIFrameElement, _doc: Document) {
  const win = f.contentWindow;
  if (!win) return;
  // WE shim 已接管节流（注入时机更早且支持运行时调 fps），避免双层节流把帧率减半
  if ((win.requestAnimationFrame as any)?.__weThrottled) return;
  const fps = state.cfg.sceneFps || 30;
  if (fps >= 60) return; // 60fps 已是目标上限，无需节流
  const interval = 1000 / fps;
  try {
    const origRaf = win.requestAnimationFrame.bind(win);
    const origCaf = win.cancelAnimationFrame.bind(win);
    const rafMap = new Map<number, number>();
    let counter = 0;
    // 用 setTimeout(≈fps 间隔) 替代原生 60fps rAF，callback 转发回 origRaf（保证仍同步到帧）。
    // WebKit 会按显示时机光栅化，帧率降半 → WebGL/canvas 动画 GPU 占用约减半。
    (win as any).requestAnimationFrame = (cb: FrameRequestCallback) => {
      const id = ++counter;
      const to = win.setTimeout(() => {
        rafMap.delete(id);
        origRaf((now: number) => {
          try {
            cb(now);
          } catch {
            /* 壁纸内部 rAF callback 抛错忽略 */
          }
        });
      }, interval);
      rafMap.set(id, to as unknown as number);
      return id;
    };
    (win as any).cancelAnimationFrame = (id: number) => {
      const to = rafMap.get(id);
      if (to !== undefined) {
        win.clearTimeout(to);
        rafMap.delete(id);
      }
    };
  } catch {
    /* 忽略 */
  }
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
  // 粒子系统注册的 mousemove 监听（控制点跟随鼠标）；卸载时必须摘掉，
  // 否则重挂场景会在 window 上累积监听器，旧回调还持有已释放的 GL 资源。
  let particleCleanup: (() => void) | undefined;
  state.sceneCleanup = () => {
    disposed = true;
    if (particleCleanup) {
      particleCleanup();
      particleCleanup = undefined;
    }
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
        // 效果链 FBO 降采样：0=全质量，0.5≈效果分辨率减半→内存约 1/4。
        // 用全质量：降采样会让「层 alpha 再乘一张羽化 mask」的效果（opacity）在
        // 低分辨率下把过渡带插得更淡，再经后续 waterwaves 的 UV 位移搬移、
        // 最后放大回屏幕，就在网格交界处（如 3113287126 头发与手臂交汇）看到发虚透明。
        // 代价：该场景效果链 FBO 由 3.5MB 升到约 100MB。
        fboCapFactor: 0,
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
          // 无缝循环对：WebKit 原生 loop 在循环点会冻结一瞬，双元素交接可消除
          const pair = createLoopingVideo(url, { muted: true });
          for (const v of [pair.active, pair.standby]) {
            v.style.cssText =
              "position:fixed;left:-9999px;top:-9999px;width:2px;height:2px;opacity:0;pointer-events:none";
            document.body.appendChild(v);
            // 登记以便 clear()/页面卸载时暂停移除元素并 revoke blob URL
            (state.videoTextures ??= []).push(v);
          }
          (state.objectUrls ??= []).push(url);
          (state.videoPairs ??= []).push(pair);
          pair.active.addEventListener("loadedmetadata", () => {
            reportDiag(cfg, `video tex '${name}': ${pair.active.videoWidth}x${pair.active.videoHeight}`);
          });
          pair.active.addEventListener("error", () => {
            reportDiag(cfg, `video tex '${name}' ERROR: ${pair.active.error?.code}`);
          });
          void pair.active.play().catch((e) => reportDiag(cfg, `video play fail '${name}': ${String(e).slice(0, 80)}`));
          // entry.video 用 getter 指向当前主元素：交换后渲染器每帧自动采样新元素
          const entry = {
            get video() {
              return pair.active;
            },
            glTex: rnd.makeTexture(renderer.gl, new Uint8Array([0, 0, 0, 0]), 1, 1),
            width: m.width,
            height: m.height,
            rg88: false,
            lastUploaded: -1,
          };
          let texSwapCount = 0;
          pair.onSwap = () => {
            entry.lastUploaded = -1;
            // 诊断：首次 + 每 10/100 次上报，确认纹理无缝交换持续生效
            texSwapCount++;
            if (texSwapCount === 1 || texSwapCount === 10 || texSwapCount === 100) {
              reportDiag(cfg, `video tex '${name}' loop swap ok x${texSwapCount}`);
            }
          };
          pair.onFallback = () => {
            reportDiag(cfg, `video tex '${name}' loop fallback (native loop used)`);
          };
          textures.set(name, entry);
          return entry;
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
        // 序列帧表（.tex 的 TEXS 段）：粒子据此切 sprite sheet。
        // 没有它就只能按 sequencemultiplier 猜 N×N 方格，对横排/竖排 sheet 会采错图块。
        if (parsedTex.frames?.list?.length) entry.frames = parsedTex.frames.list;
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
      // 加载粒子模型 json + 材质 + 贴图，构造 ParticleSystem，注入 renderer。
      //
      // 贴图有两个来源：pkg 内嵌（工坊自制素材）与 WE 内置资源。后者（particle/halo、
      // particle/fog/fog1 …）不在 pkg 里 —— 全库 33 张被引用的粒子贴图有 24 张属于
      // 内置资源。没有 WE 安装目录可回退，故用 particle-textures.js 按名字语义
      // 程序化生成近似素材，否则整个粒子系统无贴图可用、只能整体跳过。
      const particleSystems: any[] = [];
      let builtinTexCount = 0;

      // 取粒子贴图：先查 pkg，缺失则程序化生成（生成结果并入 textures 缓存复用）
      const loadParticleTex = async (name: string): Promise<any | null> => {
        const inPkg = await loadTex(name);
        if (inPkg) return inPkg;
        const gen = ptex.buildBuiltinParticleTexture(name);
        if (!gen) return null;
        const entry = {
          glTex: rnd.makeTextureMip(renderer.gl, [gen], false),
          width: gen.width,
          height: gen.height,
          rg88: false,
          mips: [gen],
          generated: true,
        };
        textures.set(name, entry);
        builtinTexCount++;
        return entry;
      };

      // 递归构造粒子系统：children 是子发射器（如 ghost1 → 光晕/尾迹/本体三层）。
      // WE 的 eventfollow 子系统跟随父粒子；这里降级为「与父同图层的独立系统」——
      // 位置不跟随单个父粒子，但视觉上的分层叠加（本体+光晕+尾迹）得以保留。
      const buildParticleSystem = async (
        particlePath: string,
        layer: any,
        override: any,
        depth: number,
      ): Promise<void> => {
        if (depth > 3) return; // children 可嵌套，设上限防病态数据造成指数展开
        const modelEntry = pkg.getEntry(parsedPkg, particlePath);
        if (!modelEntry) {
          reportDiag(cfg, `particle '${particlePath}' 不在 pkg，跳过`);
          return;
        }
        const model = JSON.parse(readText(modelEntry));
        // 图层变换（origin/scale/angles）必须传进去：WE 语义里图层 origin 是发射器的
        // 世界位置、scale 缩放整个系统。不传会让所有粒子堆在世界原点(0,0)。
        const ps = new particles.ParticleSystem(renderer.gl, model, override, layer);
        let texName: string | null = null;
        if (model.material) {
          const matEntry = pkg.getEntry(parsedPkg, model.material);
          if (matEntry) {
            const mat = JSON.parse(readText(matEntry));
            ps.setMaterial(mat);
            texName = mat?.passes?.[0]?.textures?.[0] || null;
          }
        }
        // 材质缺失或未声明贴图时，用通用光晕兜底（宁可近似也不整层消失）
        const te = await loadParticleTex(texName || "particle/halo");
        if (!te) {
          reportDiag(cfg, `particle '${particlePath}' 无贴图可用，跳过`);
          return;
        }
        ps.setTexture({ glTex: te.glTex, width: te.width, height: te.height, frames: te.frames });
        ps.setVisible(true);
        particleSystems.push(ps);

        for (const ch of model.children || []) {
          if (!ch || typeof ch.name !== "string") continue;
          // 子系统继承父图层的世界变换，叠加自身的局部 origin/scale/angles
          const cOrigin = String(ch.origin ?? "0 0 0").trim().split(/\s+/).map(Number);
          const cScale = String(ch.scale ?? "1 1 1").trim().split(/\s+/).map(Number);
          const cAngles = String(ch.angles ?? "0 0 0").trim().split(/\s+/).map(Number);
          const childLayer = {
            ...layer,
            origin: [
              (layer.origin?.[0] || 0) + (cOrigin[0] || 0),
              (layer.origin?.[1] || 0) + (cOrigin[1] || 0),
              (layer.origin?.[2] || 0) + (cOrigin[2] || 0),
            ],
            scale: [
              (layer.scale?.[0] ?? 1) * (cScale[0] || 1),
              (layer.scale?.[1] ?? 1) * (cScale[1] || 1),
              (layer.scale?.[2] ?? 1) * (cScale[2] || 1),
            ],
            angles: [
              (layer.angles?.[0] || 0) + (cAngles[0] || 0),
              (layer.angles?.[1] || 0) + (cAngles[1] || 0),
              (layer.angles?.[2] || 0) + (cAngles[2] || 0),
            ],
          };
          await buildParticleSystem(ch.name, childLayer, ch.instanceoverride || null, depth + 1);
        }
      };

      if (!SKIP_PARTICLES) {
        for (const layer of scene.layers) {
          if (!layer.particle || !layer.visible) continue;
          try {
            await buildParticleSystem(layer.particle, layer, layer.instanceoverride, 0);
          } catch (e) {
            console.warn(`粒子图层 ${layer.name} 加载失败: ${(e as Error).message}`);
            reportDiag(cfg, `particle '${layer.name}' FAIL: ${(e as Error).message.slice(0, 80)}`);
          }
        }
      }
      // 场景卸载时释放粒子系统的 GL 资源（每系统一套 program/VAO/VBO）
      if (particleSystems.length > 0) {
        const prevCleanup = particleCleanup;
        particleCleanup = () => {
          prevCleanup?.();
          for (const ps of particleSystems) ps.dispose();
        };
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
      // 鼠标世界位置：locktopointer 的控制点用它做吸引/排斥（controlpointattract）。
      // 屏幕坐标 → 世界坐标需要相机的可见窗口（cam.offX/viewW），故在回调里换算。
      const pointerScreen = { x: -1, y: -1, has: false };
      if (particleSystems.length > 0) {
        const onMove = (ev: MouseEvent) => {
          pointerScreen.x = ev.clientX / (window.innerWidth || 1);
          pointerScreen.y = ev.clientY / (window.innerHeight || 1);
          pointerScreen.has = true;
        };
        window.addEventListener("mousemove", onMove, { passive: true });
        const prevCleanup = particleCleanup;
        particleCleanup = () => {
          prevCleanup?.();
          window.removeEventListener("mousemove", onMove);
        };
      }
      let particleDiagFrame = 0;
      renderer.setParticleRenderer((cam: any, viewProj: any, w: number, h: number) => {
        const now = performance.now();
        // dt 封顶 50ms：标签页切回或掉帧时的大 dt 会让粒子瞬移一大段
        const pdt = Math.min(0.05, (now - lastPt) / 1000);
        lastPt = now;
        if (pointerScreen.has) {
          const wx = cam.offX + pointerScreen.x * cam.viewW;
          const wy = cam.offY + pointerScreen.y * cam.viewH;
          for (const ps of particleSystems) ps.setPointer(wx, wy);
        }
        for (const ps of particleSystems) ps.advance(pdt);
        for (const ps of particleSystems) ps.render(viewProj, w, h, cam.projW, cam.projH);
        // 首帧后上报一次实际存活粒子数，用于确认系统真的在发射（而非静默空转）
        if (particleDiagFrame < 2) {
          particleDiagFrame++;
          if (particleDiagFrame === 2) {
            const live = particleSystems.reduce((s, ps) => s + ps.liveCount(), 0);
            reportDiag(cfg, `particles live: ${live} across ${particleSystems.length} systems`);
          }
        }
      });
      reportDiag(
        cfg,
        `particles: ${particleSystems.length} systems, ${builtinTexCount} builtin tex generated`,
      );
      // 调试出口：测试台/控制台可读粒子系统状态（存活数、世界包围盒、尺寸区间），
      // 用于确认粒子真的落在可见区域内、尺寸量级合理，而不是堆在原点或大到糊屏。
      (window as unknown as Record<string, unknown>).__particleStats = () =>
        particleSystems.map((ps) => {
          let live = 0;
          let minX = Infinity;
          let maxX = -Infinity;
          let minY = Infinity;
          let maxY = -Infinity;
          let minS = Infinity;
          let maxS = -Infinity;
          for (const p of ps.pool) {
            if (!p.alive) continue;
            live++;
            const px = ps.originX + p.x * ps.scaleX;
            const py = ps.originY + p.y * ps.scaleY;
            if (px < minX) minX = px;
            if (px > maxX) maxX = px;
            if (py < minY) minY = py;
            if (py > maxY) maxY = py;
            const s = Math.abs(p.size) * ps.sysScale;            if (s < minS) minS = s;
            if (s > maxS) maxS = s;
          }
          return {
            live,
            max: ps.maxCount,
            blend: ps.blend,
            renderer: ps.renderers.map((r: any) => r.kind).join("+"),
            origin: [Math.round(ps.originX), Math.round(ps.originY)],
            bbox: live ? [Math.round(minX), Math.round(minY), Math.round(maxX), Math.round(maxY)] : null,
            size: live ? [Math.round(minS), Math.round(maxS)] : null,
          };
        });
      // 调试出口：整体开关粒子可见性，用于「开/关两帧对比」量化粒子对画面的实际贡献
      // （验证是否出现方块边界、是否把画面冲白、是否堆成一团）。
      // 传索引则只显示该系统，用于逐系统定位过曝来源。
      (window as unknown as Record<string, unknown>).__particleToggle = (
        on: boolean,
        onlyIndex?: number,
      ) => {
        for (let i = 0; i < particleSystems.length; i++) {
          particleSystems[i].setVisible(onlyIndex === undefined ? on : i === onlyIndex);
        }
        return particleSystems.length;
      };

      // ---- puppet 骨骼网格图层 ----
      // model json 有 puppet 字段 → 解析 MDL（网格 + 骨架 + MDLA 动画），挂到图层上。
      // 渲染由 renderer 的图层循环调用（按 z 序、可走效果链），不再作为叠加层单独绘制。
      const mdlItems: { mdl: any; tex: any; layer: any }[] = [];
      for (const layer of scene.layers) {
        if (!layer.image || !layer.visible) continue;
        if (SKIP_3D_MODELS) continue;
        try {
          if (eff.BUILTIN_MODELS[layer.image]) continue; // 内置模型无 puppet
          const modelEntry = pkg.getEntry(parsedPkg, layer.image);
          if (!modelEntry) continue;
          const model = JSON.parse(readText(modelEntry));
          if (!model.puppet) continue;
          const mdlEntry = pkg.getEntry(parsedPkg, model.puppet);
          if (!mdlEntry) continue;
          const mdlObj = mdl.parseMDL(new Uint8Array(mdlEntry as ArrayBuffer));
          // 贴图：与普通图层同一条材质链，已在上面的循环里 loadTex 过
          const texObj = layer.textureName ? textures.get(layer.textureName) : null;
          if (!texObj) {
            reportDiag(cfg, `puppet '${layer.name}' 无贴图，跳过`);
            continue;
          }
          layer.puppet = mdlObj;
          mdlItems.push({ mdl: mdlObj, tex: texObj, layer });
          const an = mdlObj.animations[0];
          reportDiag(
            cfg,
            `puppet '${layer.name}' v=${mdlObj.vertexCount} bones=${mdlObj.bones.length}` +
              (an ? ` anim='${an.name}' ${an.fps}fps×${an.frameCount}` : " 无动画"),
          );
        } catch (e) {
          console.warn(`puppet 图层 ${layer.name} 加载失败: ${(e as Error).message}`);
          reportDiag(cfg, `puppet '${layer.name}' 失败: ${(e as Error).message}`);
        }
      }
      let mdlRenderer: any = null;
      if (mdlItems.length > 0) {
        try {
          mdlRenderer = mdl.createMDLRenderer(renderer.gl);
          for (const item of mdlItems) mdlRenderer.upload(item.mdl);
          // 注入绘制回调：renderer 在图层循环里按 z 序调用
          const byLayer = new Map<any, { mdl: any; tex: any; layer: any }>();
          for (const item of mdlItems) byLayer.set(item.layer, item);
          renderer.setPuppetRenderer((layer: any, mvp: any, o: any) => {
            const item = byLayer.get(layer);
            if (!item) return;
            mdlRenderer.draw(
              mvp,
              item.mdl,
              {
                time: o.time,
                animLayers: layer.animationLayers,
                color: [
                  layer.color[0] * layer.brightness,
                  layer.color[1] * layer.brightness,
                  layer.color[2] * layer.brightness,
                  layer.alpha,
                ],
              },
              item.tex,
            );
          });
        } catch (e) {
          console.warn(`puppet 渲染器初始化失败: ${(e as Error).message}`);
          reportDiag(cfg, `puppet renderer 失败: ${(e as Error).message}`);
          mdlRenderer = null;
          for (const item of mdlItems) item.layer.puppet = null;
        }
      }
      if (disposed) return;
      reportDiag(cfg, `puppet: ${mdlItems.length} meshes`);

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
  // video / gif / image 统一走场景引擎（mountMedia 内部在无 WebGL2 时回退 DOM）
  if ((cfg.type === "video" || cfg.type === "gif" || cfg.type === "image") && cfg.src) {
    mountMedia(cfg);
  } else if (cfg.type === "web" && cfg.src) mountWeb(cfg);
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
      /** 热更新 WE 网页壁纸用户属性（wire 格式：{name: {value: ...}}） */
      updateWebProps(props: Record<string, { value: unknown }>): void;
    };
  }
}

window.__wp = {
  setWallpaper(cfg: WallpaperConfig) {
    state.cfg = cfg;
    mount(cfg);
  },
  pause() {
    for (const p of state.videoPairs ?? []) p.pause();
    weShimCall((w) => w.__weSetPaused?.(true));
    // 媒体壁纸的画面由 rAF 渲染循环驱动，但解码器是独立的：只停 rAF 会让视频
    // 在后台继续解码（白耗 CPU），故一并暂停元素
    state.video?.pause();
    if (state.raf !== undefined) {
      cancelAnimationFrame(state.raf);
      state.raf = undefined;
    }
  },
  resume() {
    for (const p of state.videoPairs ?? []) p.resume();
    weShimCall((w) => w.__weSetPaused?.(false));
    if (state.cfg.type === "canvas") startCanvasLoop();
    else if (
      state.cfg.type === "scene" ||
      state.cfg.type === "video" ||
      state.cfg.type === "gif" ||
      state.cfg.type === "image"
    ) {
      // scene / 媒体壁纸的渲染循环持有各自闭包内的 disposed 标志，无法从外部重启，
      // 故按当前配置重新挂载（sceneCleanup 已被 pause 前的 clear() 或此处的 mount 处理）
      mount(state.cfg);
    }
  },
  setFit(fit: string) {
    state.cfg.fit = fit as WallpaperFit;
    // DOM 回退路径（无 WebGL2）才需要改 object-fit；走场景引擎时 fit 由渲染循环
    // 每帧读 state.cfg.fit 传给 fitWindow，无需重挂载即可实时切换
    const obj = state.video ?? state.img;
    if (obj && obj.isConnected && obj.parentElement === wrap) {
      const f = fitObjectFit(fit as WallpaperFit);
      obj.style.objectFit = f.objectFit;
      obj.style.background = f.background;
    }
  },
  setVolume(volume: number) {
    if (state.video) {
      state.video.volume = Math.max(0, Math.min(1, volume));
      state.video.muted = volume <= 0;
    }
    if (state.sceneAudio) {
      state.sceneAudio.setVolume(volume);
    }
    weShimCall((w) => w.__weSetVolume?.(Math.max(0, Math.min(1, volume))));
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
    weShimCall((w) => w.__weSetFps?.(fps));
  },
  // 热更新 WE 网页壁纸用户属性（属性编辑保存后由原生侧调用，免刷新生效）
  updateWebProps(props: Record<string, { value: unknown }>) {
    weShimCall((w) => w.__weApplyProps?.(props));
    // 场景壁纸的属性作用在 scene.json 的字段绑定上（图层可见性/颜色/位置…），
    // 这些值在 parseScene 时已烘进图层对象，无法逐字段热改 → 重挂载重新解析。
    // 覆盖值由宿主合并进 project.json 响应，重挂载即读到新值。
    if (state.cfg.type === "scene") mount(state.cfg);
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
