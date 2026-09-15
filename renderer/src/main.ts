// 壁纸渲染器页
// 由壁纸引擎窗口加载；原生侧通过 window.__wp 控制。
//
// 所有壁纸类型（scene / web / video / gif / image）统一走 npm 包 webwallgl，
// 本文件只保留「宿主适配层」职责：
//   - URL query 解析（壁纸引擎窗口创建时注入的初始配置）
//   - window.__wp 原生控制面
//   - Source 适配：Rust 侧已解析好类型与入口 URL，直接告诉库，
//     省掉库自己 fetch project.json 判类型的那一步
//   - 系统音频注入：内容服务器的 /audio-stream SSE → 库的 AudioSource
//   - 默认壁纸降级、诊断上报
//
// canvas（内置演示动画）不是工坊壁纸类型，仍由本文件自绘。

import {
  mount as mountLib,
  type AudioSource,
  type QualityOptions,
  type ResolvedQuality,
  type SceneInstance,
  type Source,
} from "webwallgl";

type WallpaperFit = "cover" | "contain" | "stretch" | "fill" | "fit";

type WallpaperConfig = {
  type: "canvas" | "video" | "gif" | "web" | "scene" | "image";
  src?: string;
  fit?: WallpaperFit;
  /**
   * 渲染分辨率**相对倍率**：0=自动（跟随设备 devicePixelRatio，Retina 原生）；
   * 0.75=省电 / 0.85=标准 / 1=高清（=设备像素比，原生清晰）。
   * 注意这是相对设备 DPR 的倍率，传给 webwallgl 库前经 toAbsoluteDpr 换算成
   * 库使用的绝对 DPR（库 1.3.22+：0=自动，正数=目标 DPR，可高于设备上报值）。
   */
  renderDpr?: number;
  /** 场景壁纸帧率上限（24/30/45/60/120），越低 GPU 占用越低；默认 24 */
  sceneFps?: number;
  /** 全局滤镜 id（见 WALLPAPER_FILTERS 白名单），未知 id 按无滤镜处理 */
  filter?: string;
  /**
   * 渲染质量档位（库 1.3.23+）：抗锯齿 aa（off/fxaa/msaa2/msaa4）、
   * 粒子 particles（off/low/medium/high）、后处理 postProcessing（同档）。
   * query 键是 aa/pq/pp（与上游 bench 约定）；setWallpaper 下发的 JSON 键与
   * 这里的字段名一致（Rust serde camelCase）。缺省 = 库默认（off/high/high）。
   */
  aa?: string;
  particles?: string;
  postProcessing?: string;
  muted?: boolean;
  loop?: boolean;
  /** 内容服务器媒体基址：http://127.0.0.1:<port>/media/<token>（scene 拉取 pkg 用） */
  mediaBase?: string;
};

// 规范化显示模式：兼容旧会话里的 fill（=拉伸）与 fit（=适应）。
// 旧 fill 是"忽略宽高比铺满"（会被拉伸变形），默认迁移到 cover 修复，不再默认拉伸。
function normalizeFit(fit?: WallpaperFit): "cover" | "contain" | "stretch" {
  if (fit === "fit") return "contain"; // 旧"适应"
  if (fit === "fill") return "cover"; // 旧默认"填充"曾是拉伸 → 修复为等比裁切
  return fit === "contain" || fit === "stretch" ? fit : "cover";
}

/**
 * 把设置里的**相对倍率**（0=自动 / 0.75 省电 / 0.85 标准 / 1 高清=原生）
 * 换算成 webwallgl 库使用的**绝对 DPR**：
 *   - 0（自动）→ **直接在渲染层解析成设备 devicePixelRatio**，不把 0 透传给库。
 *     原因：旧版库的 effectiveDpr 是 min(devicePixelRatio, renderDpr)，把 0 传
 *     进去会得到 0 → canvas backing 恒为 1×1 → 永久黑屏（「自动」档的回归）；
 *     解析成正数后新旧库行为一致（自动=原生）。
 *   - 正数 → 倍率 × 设备 DPR。高清 1.0 在 Retina（DPR 2）上即 2，达物理原生。
 * 旧版本存的是绝对 DPR（如 2），无法与新的「倍率 1」区分——统一在 Rust 侧
 * 迁移，这里只认相对倍率。
 */
function toAbsoluteDpr(relative?: number): number {
  const device = window.devicePixelRatio || 1;
  if (relative === undefined || relative === null || relative <= 0) return device; // 自动=设备 DPR
  return Math.max(0.25, relative * device);
}


// ---- 全局滤镜（对齐上游独立测试台的实现）----
//
// 原生侧只传白名单 id，CSS filter 表达式只存在于这里：既避免把任意字符串塞进
// style.filter（url() 可外链资源），也让 URL query 与 __wp.setFilter 共用一套契约。
// 挂在 wrap 容器上而不是 canvas 上：scene 的 WebGL 画布、video/img、网页 iframe
// 都是 wrap 的子节点，一处生效全类型覆盖，且换壁纸（子节点重建）不清除。
const WALLPAPER_FILTERS: Record<string, string> = {
  none: "",
  blur: "blur(14px)",
  grayscale: "grayscale(1)",
  sepia: "sepia(0.75)",
  vivid: "saturate(1.6)",
  warm: "sepia(0.35) saturate(1.35) brightness(1.05)",
  cool: "sepia(0.25) hue-rotate(175deg) saturate(1.3) brightness(1.03)",
  invert: "invert(1)",
  brighten: "brightness(1.3)",
  darken: "brightness(0.72)",
  contrast: "contrast(1.35)",
};

function applyWallpaperFilter(filter?: string) {
  wrap.style.filter = WALLPAPER_FILTERS[filter ?? "none"] ?? "";
}

const state: {
  cfg: WallpaperConfig;
  /** 库实例：scene / web / video / gif / image 全部由它承载 */
  inst?: SceneInstance;
  /** 装载序号：异步 mount 期间若又切了壁纸，靠它丢弃过期结果 */
  seq: number;
  /** canvas 演示动画（非工坊类型，本文件自绘） */
  canvas?: HTMLCanvasElement;
  ctx?: CanvasRenderingContext2D;
  raf?: number;
  /** 降级时占位的 iframe（旧默认页残留字段；当前降级页是内联 SVG，不再用它） */
  iframe?: HTMLIFrameElement;
  /** 全局暂停态：库的 load() 会重置为播放态，切壁纸后需按此重新暂停 */
  paused: boolean;
} = { cfg: { type: "canvas" }, seq: 0, paused: false };

// 挂载容器：库的 mountLib 与本文件的 canvas/降级页都挂在它下面。
// renderer/index.html 里没有这个节点，运行时创建 —— 重构时误删过这段，
// 后果是 wrap 为 null、所有壁纸静默不显示，故这里用函数保证一定拿到元素。
// 页面背景必须是纯黑而非透明：缩放（contain）模式下画布/视频留边的区域
// 由页面背景兜底。壁纸窗口是 .transparent(true) 的，透明背景会直接漏出
// 后层的系统壁纸；WebGL 侧 contain 留边也是 clearColor(0,0,0,0) 的透明像素，
// 合成时同样落到页面背景上 —— 两处都靠这里的 #000 兜成黑边。
document.documentElement.style.cssText = "margin:0;height:100%;background:#000;";
document.body.style.cssText =
  "margin:0;width:100vw;height:100vh;overflow:hidden;background:#000;position:relative;";

const wrap: HTMLDivElement = (() => {
  const existing = document.getElementById("wrap");
  if (existing instanceof HTMLDivElement) return existing;
  const el = document.createElement("div");
  el.id = "wrap";
  el.style.cssText = "position:fixed;inset:0;overflow:hidden;";
  document.body.appendChild(el);
  return el;
})();

// 屏蔽默认右键菜单（壁纸窗口应只响应用户自定义交互，不弹浏览器/调试菜单）。
// __blockContextMenu 供 iframe 内容调用：库的 attachIframe 会在 load 后调它，
// 把屏蔽延伸到网页壁纸内部
(() => {
  const block = (e: Event) => e.preventDefault();
  window.addEventListener("contextmenu", block, true);
  document.addEventListener("contextmenu", block, true);
  (window as unknown as Record<string, unknown>).__blockContextMenu = (doc: Document) =>
    doc.addEventListener("contextmenu", block, true);
})();

/** 诊断上报：转发到内容服务器的 /diag，由 Rust 侧记进日志 */
function reportDiag(cfg: WallpaperConfig, msg: string) {
  const text = `[${cfg.type}${cfg.src ? ` ${cfg.src}` : ""}] ${msg}`;
  try {
    void fetch(`/diag?msg=${encodeURIComponent(text)}`, { cache: "no-store" });
  } catch {
    /* 上报失败不影响渲染 */
  }
}

// ---------- 系统音频注入 ----------
//
// 内容服务器以 ~30Hz 通过 SSE 推送 64 段频谱（ScreenCaptureKit 系统 loopback → FFT）。
// 库的 AudioSource 只要求一个同步的 snapshot()，所以这里做「SSE 异步写入 → 快照同步读出」
// 的桥：EventSource 收到帧就更新缓冲，库的音频泵逐帧来取最新值。
//
// 后端是单声道（系统混音后的输出），左右声道喂同一份。WE 壁纸绝大多数只用
// 其中一路，少数做左右分离视觉效果的会表现为左右对称 —— 比拿不到数据好。
class SystemAudioSource implements AudioSource {
  /** 复用同一对数组：音频泵每帧都调 snapshot()，新建数组会制造持续的 GC 压力 */
  private readonly bands = new Float32Array(64);
  private es?: EventSource;
  /** 最近一次收到数据的时间戳，用于判断链路是否仍活着 */
  private lastFrameAt = 0;

  snapshot() {
    return { left: this.bands, right: this.bands };
  }

  /** 链路是否有近期数据（2s 内）。捕获关闭或 SSE 断开后为 false */
  get alive() {
    return this.lastFrameAt > 0 && performance.now() - this.lastFrameAt < 2000;
  }

  /** 是否已建立订阅。区别于 alive：订阅成功但捕获还没出首帧时为 true */
  get subscribed() {
    return this.es !== undefined;
  }

  connect(token: string, onDiag: (msg: string) => void) {
    this.close();
    try {
      // 与内容服务器同源，EventSource 直接订阅。捕获未开启时端点返回 503，
      // EventSource 会自动重试 —— 用户之后在设置里打开开关即自愈，无需重挂壁纸
      const es = new EventSource(`/audio-stream/${token}`);
      es.onmessage = (ev) => {
        try {
          const arr = JSON.parse(ev.data) as number[];
          if (!Array.isArray(arr)) return;
          const n = Math.min(arr.length, this.bands.length);
          for (let i = 0; i < n; i++) {
            const v = arr[i];
            this.bands[i] = typeof v === "number" && v >= 0 ? Math.min(1, v) : 0;
          }
          this.lastFrameAt = performance.now();
        } catch {
          /* 单帧解析失败跳过，不打断订阅 */
        }
      };
      es.onerror = () => {
        // 不主动 close：EventSource 自带重连，捕获开关打开后能自愈。
        // 但要把频谱归零，否则画面会冻在断连前的最后一帧上
        this.bands.fill(0);
        this.lastFrameAt = 0;
      };
      this.es = es;
      onDiag("system audio: SSE subscribed");
    } catch (e) {
      onDiag(`system audio: SSE 订阅失败（${(e as Error)?.message ?? e}）`);
    }
  }

  close() {
    try {
      this.es?.close();
    } catch {
      /* 忽略 */
    }
    this.es = undefined;
    this.bands.fill(0);
    this.lastFrameAt = 0;
  }
}

const systemAudio = new SystemAudioSource();

// ---------- 系统「正在播放」注入 ----------
//
// 内容服务器把 macOS 的 Now Playing（歌名/艺人/专辑/进度/封面）经 SSE 推来，
// 桥到库的 MediaSource，壁纸侧就是 WE 的 wallpaperRegisterMediaPropertiesListener
// 等四个回调。库自带一个模拟源作缺省 —— 那份假数据就是「歌曲显示成测试内容」
// 的来源，所以哪怕当前没有媒体在播，也要装上真实源（hasMedia: false）把它顶掉。

/** 库要求的媒体配色三元组 */
type MediaColorLike = {
  x: number;
  y: number;
  z: number;
  add(o: MediaColorLike): MediaColorLike;
  subtract(o: MediaColorLike): MediaColorLike;
  multiply(k: number | MediaColorLike): MediaColorLike;
};

// 真实语料里的壁纸脚本会写 `event.primaryColor.subtract(old).multiply(t).add(old)`
// 做换歌配色过渡。给普通数组/对象会 TypeError 熔断整个脚本（症状是「换歌后整层
// 不见了」），所以必须是带链式方法的实例。
function mediaColor(x: number, y: number, z: number): MediaColorLike {
  return {
    x,
    y,
    z,
    add(o) {
      return mediaColor(this.x + o.x, this.y + o.y, this.z + o.z);
    },
    subtract(o) {
      return mediaColor(this.x - o.x, this.y - o.y, this.z - o.z);
    },
    multiply(k) {
      return typeof k === "number"
        ? mediaColor(this.x * k, this.y * k, this.z * k)
        : mediaColor(this.x * k.x, this.y * k.y, this.z * k.z);
    },
  };
}

/** 内容服务器 /now-playing 的载荷（对齐 Rust MediaSnapshot 的 camelCase） */
type MediaWire = {
  hasMedia: boolean;
  state: 0 | 1 | 2;
  title: string;
  artist: string;
  album: string;
  albumArtist: string;
  position: number;
  duration: number;
  hasThumbnail: boolean;
  thumbnail?: string;
  trackIndex: number;
};

type MediaSnapshotLike = Omit<MediaWire, "thumbnail"> & {
  thumbnail: string;
  primaryColor: MediaColorLike;
  secondaryColor: MediaColorLike;
  tertiaryColor: MediaColorLike;
  textColor: MediaColorLike;
  highContrastColor: MediaColorLike;
  lyrics: Array<[number, string]>;
  lyricLine: string;
  lyricIndex: number;
};

const BLACK = () => mediaColor(0, 0, 0);
const WHITE = () => mediaColor(1, 1, 1);

function emptyMediaSnapshot(): MediaSnapshotLike {
  return {
    hasMedia: false,
    state: 0,
    title: "",
    artist: "",
    album: "",
    albumArtist: "",
    position: 0,
    duration: 0,
    hasThumbnail: false,
    thumbnail: "",
    trackIndex: 0,
    primaryColor: BLACK(),
    secondaryColor: BLACK(),
    tertiaryColor: BLACK(),
    textColor: WHITE(),
    highContrastColor: WHITE(),
    lyrics: [],
    lyricLine: "",
    lyricIndex: 0,
  };
}

/**
 * 封面取色：缩到 16×16 后取平均色作主色，按亮度推导文字/高对比色。
 *
 * 放在渲染器而不是 Rust：浏览器天然会解 JPEG，而在 Rust 里解码要拖进
 * image crate 的上百个传递依赖（含 AVIF/AV1 编码器）——只为算五个颜色不值得。
 */
function pickColors(img: HTMLImageElement): Pick<
  MediaSnapshotLike,
  "primaryColor" | "secondaryColor" | "tertiaryColor" | "textColor" | "highContrastColor"
> {
  const N = 16;
  try {
    const cv = document.createElement("canvas");
    cv.width = N;
    cv.height = N;
    const ctx = cv.getContext("2d", { willReadFrequently: true });
    if (!ctx) throw new Error("no 2d ctx");
    ctx.drawImage(img, 0, 0, N, N);
    const d = ctx.getImageData(0, 0, N, N).data;

    let r = 0;
    let g = 0;
    let b = 0;
    // 同时找最亮与最暗像素作为次色/第三色，比单纯把主色调亮调暗更贴合封面
    let maxL = -1;
    let minL = 2;
    let bright = BLACK();
    let dark = WHITE();
    const count = N * N;
    for (let i = 0; i < d.length; i += 4) {
      const pr = d[i] / 255;
      const pg = d[i + 1] / 255;
      const pb = d[i + 2] / 255;
      r += pr;
      g += pg;
      b += pb;
      const l = 0.299 * pr + 0.587 * pg + 0.114 * pb;
      if (l > maxL) {
        maxL = l;
        bright = mediaColor(pr, pg, pb);
      }
      if (l < minL) {
        minL = l;
        dark = mediaColor(pr, pg, pb);
      }
    }
    const primary = mediaColor(r / count, g / count, b / count);
    const lum = 0.299 * primary.x + 0.587 * primary.y + 0.114 * primary.z;
    // 主色偏亮 → 深色文字，偏暗 → 浅色文字（WE 壁纸拿它画歌名）
    const text = lum > 0.55 ? mediaColor(0.05, 0.05, 0.05) : mediaColor(0.96, 0.96, 0.96);
    return {
      primaryColor: primary,
      secondaryColor: bright,
      tertiaryColor: dark,
      textColor: text,
      highContrastColor: lum > 0.55 ? BLACK() : WHITE(),
    };
  } catch {
    // 跨源封面会污染 canvas 让 getImageData 抛错；给一组中性色而不是崩掉
    return {
      primaryColor: BLACK(),
      secondaryColor: mediaColor(0.5, 0.5, 0.5),
      tertiaryColor: BLACK(),
      textColor: WHITE(),
      highContrastColor: WHITE(),
    };
  }
}

class SystemMediaSource {
  private snap: MediaSnapshotLike = emptyMediaSnapshot();
  private es?: EventSource;
  private token = "";
  /** 快照里 position 对应的本地时刻，用于播放中按真实时间外推进度 */
  private positionAt = 0;
  /** 已取色的封面 dataURL，避免同一张封面反复解码取色 */
  private thumbKey = "";
  /** 是否收到过至少一帧。端点在 adapter 不可用时返回 503，此时不该顶掉库的缺省源 */
  private gotFrame = false;

  /**
   * 媒体集成是否真的可用。只看「收到过数据」而不是「已订阅」：
   * adapter 起不来时端点返回 503，EventSource 仍会不停重试（subscribed 恒为
   * true），此时装一个永远空的源不如让库自己兜着。
   */
  get subscribed() {
    return this.gotFrame;
  }

  /** 库每帧调用；这里顺带把播放进度按本地时钟推进（后端只在系统通知时更新） */
  update() {
    if (this.snap.state !== 1 || this.snap.duration <= 0) return;
    const drift = (performance.now() - this.positionAt) / 1000;
    this.snap = {
      ...this.snap,
      position: Math.min(this.snap.duration, this.snap.position + drift),
    };
    this.positionAt = performance.now();
  }

  get snapshot(): MediaSnapshotLike {
    return this.snap;
  }

  connect(token: string, onDiag: (msg: string) => void) {
    this.close();
    this.token = token;
    try {
      const es = new EventSource(`/now-playing/${token}`);
      es.onmessage = (ev) => {
        try {
          this.apply(JSON.parse(ev.data) as MediaWire);
        } catch {
          /* 单帧解析失败跳过 */
        }
      };
      this.es = es;
      onDiag("now playing: SSE subscribed");
    } catch (e) {
      onDiag(`now playing: SSE 订阅失败（${(e as Error)?.message ?? e}）`);
    }
  }

  private apply(wire: MediaWire) {
    if (!wire || typeof wire !== "object") return;
    this.gotFrame = true;
    const thumb = wire.thumbnail ?? "";
    const base: MediaSnapshotLike = {
      ...emptyMediaSnapshot(),
      hasMedia: !!wire.hasMedia,
      state: wire.state === 1 || wire.state === 2 ? wire.state : 0,
      title: wire.title ?? "",
      artist: wire.artist ?? "",
      album: wire.album ?? "",
      albumArtist: wire.albumArtist ?? "",
      position: Number.isFinite(wire.position) ? wire.position : 0,
      duration: Number.isFinite(wire.duration) ? wire.duration : 0,
      hasThumbnail: !!wire.hasThumbnail,
      // 库 1.3.6 起直接透传真封面给网页壁纸的 mediaThumbnailChanged(e).thumbnail；
      // 留空则由库画渐变占位图
      thumbnail: thumb,
      trackIndex: Number.isFinite(wire.trackIndex) ? wire.trackIndex : 0,
      // 换歌瞬间先沿用上一轮配色，等新封面解码完再覆盖，避免闪一下黑
      primaryColor: this.snap.primaryColor,
      secondaryColor: this.snap.secondaryColor,
      tertiaryColor: this.snap.tertiaryColor,
      textColor: this.snap.textColor,
      highContrastColor: this.snap.highContrastColor,
    };
    this.snap = base;
    this.positionAt = performance.now();

    if (!thumb) {
      this.thumbKey = "";
      return;
    }
    if (thumb === this.thumbKey) return; // 同一张封面不重复取色
    this.thumbKey = thumb;
    const img = new Image();
    img.onload = () => {
      // 解码期间可能又换了歌：key 变了就丢弃这次结果
      if (this.thumbKey !== thumb) return;
      this.snap = { ...this.snap, ...pickColors(img) };
    };
    img.src = thumb;
  }

  /** 反向控制：壁纸里的播放/暂停、上下一曲转发给真实播放器 */
  private send(cmd: string) {
    if (!this.token) return;
    // GET：内容服务器整体只放行 GET/OPTIONS（端点只监听本机且带 token 鉴权）
    void fetch(`/media-command/${this.token}/${cmd}`).catch(() => {
      /* 控制失败不影响渲染 */
    });
  }

  play() {
    this.send("play");
  }
  pause() {
    this.send("pause");
  }
  playPause() {
    this.send("playPause");
  }
  skipNext() {
    this.send("next");
  }
  skipPrevious() {
    this.send("previous");
  }

  close() {
    try {
      this.es?.close();
    } catch {
      /* 忽略 */
    }
    this.es = undefined;
    this.snap = emptyMediaSnapshot();
    this.thumbKey = "";
    this.gotFrame = false;
  }
}

const systemMedia = new SystemMediaSource();

// ---------- Source 适配 ----------
//
// Rust 的 resolve_item_config 已经按类型解析出入口 URL（video 找到 .mp4、
// web 按 project.json/index.html 优先级定入口、scene 给 itemId）。
// 这里把结论直接交给库，而不是让库再 fetch 一次 project.json 去猜 ——
// 少一次请求，且入口优先级只有一处实现（Rust 侧），不会两边规则漂移。
function buildSource(cfg: WallpaperConfig): Source | null {
  const type = cfg.type;
  if (type === "scene") {
    // scene 的 src 是 itemId，资源目录为 {mediaBase}/{itemId}
    if (!cfg.mediaBase || !cfg.src) return null;
    const base = `${cfg.mediaBase.replace(/\/+$/, "")}/${cfg.src}`;
    return {
      key: base,
      // scene.pkg 的三种布局逐个回退。每次 fetch 单独 try/catch：Tauri 自定义
      // 协议对不存在的路径抛 TypeError 而非返回 404，逐个 try 才不会一抛就整场失败
      async scenePkg(signal) {
        const paths = ["scene.pkg", "scenes/scene.pkg", "gifscene.pkg"];
        let lastStatus: number | null = null;
        for (const p of paths) {
          try {
            const r = await fetch(`${base}/${p}`, { signal });
            if (!r.ok) {
              lastStatus = r.status;
              continue;
            }
            return await r.arrayBuffer();
          } catch (e) {
            if (signal?.aborted) throw e;
          }
        }
        throw new Error(
          lastStatus != null ? `scene.pkg 加载失败（HTTP ${lastStatus}）` : "scene.pkg 加载失败",
        );
      },
      async project(signal) {
        try {
          const r = await fetch(`${base}/project.json`, { signal });
          return r.ok ? await r.json() : null;
        } catch {
          return null;
        }
      },
    };
  }

  if (!cfg.src) return null;

  if (type === "web") {
    return {
      key: cfg.src,
      // web 不会被调用，但 Source 要求这个方法存在
      scenePkg: () => Promise.reject(new Error("网页壁纸无 scene.pkg")),
      // 显式声明类型，库据此走 mountWeb（不再 fetch project.json 判类型）
      project: async () => ({ type: "web" }),
      webEntry: async () => ({ url: cfg.src as string }),
    };
  }

  // video / gif / image：Rust 已给出可直接喂 <video>/<img> 的完整 URL
  return {
    key: cfg.src,
    scenePkg: () => Promise.reject(new Error("媒体壁纸无 scene.pkg")),
    project: async () => ({ type }),
    mediaEntry: async () => ({ url: cfg.src as string, type }),
  };
}

/** 卸载当前壁纸（库实例 / canvas 循环 / 降级 iframe），为下一次挂载腾干净 */
function clear() {
  state.seq++;
  if (state.inst) {
    try {
      // releasePkgCache：换壁纸的语义是"旧的不回头再挂"。库的 pkg 缓存默认
      // 跨实例留 2 份/512MB（为同壁纸重挂省下载），不显式放弃的话旧壁纸几百
      // MB 的解析包会一直压在内存里，表现为换壁纸后内存不降
      state.inst.destroy({ releasePkgCache: true });
    } catch {
      /* 忽略 */
    }
    state.inst = undefined;
  }
  if (state.raf !== undefined) {
    cancelAnimationFrame(state.raf);
    state.raf = undefined;
  }
  if (state.canvas) {
    state.canvas.remove();
    state.canvas = undefined;
    state.ctx = undefined;
  }
  if (state.iframe) {
    state.iframe.remove();
    state.iframe = undefined;
  }
  wrap.replaceChildren();
}

/**
 * 给刚挂载的实例装上系统音频源。
 *
 * SSE 首帧常晚于 mount 完成（订阅要建连、捕获要出第一帧），所以不能只在挂载瞬间
 * 判一次 alive —— 那样纯音频可视化壁纸（如 865127070）会永远停在空画面。
 * 库的音频泵逐帧 pick()，事后 setAudio 同样生效，这里轮询到有数据为止。
 *
 * 每次挂载都要重新装：切壁纸会换新的 SceneInstance，旧实例上的音频源不继承。
 * seq 变化（又切了壁纸）或装上后即停，不留常驻定时器。
 */
function attachSystemAudio(inst: SceneInstance, seq: number) {
  if (!systemAudio.subscribed) return;
  if (systemAudio.alive) {
    inst.setAudio(systemAudio);
    reportDiag(state.cfg, "system audio: attached to instance");
    return;
  }
  const t = window.setInterval(() => {
    // 壁纸已切走：这个实例不再是当前实例，停掉轮询
    if (seq !== state.seq) {
      window.clearInterval(t);
      return;
    }
    if (systemAudio.alive) {
      window.clearInterval(t);
      inst.setAudio(systemAudio);
      reportDiag(state.cfg, "system audio: attached to instance");
    }
  }, 500);
}

/**
 * 给刚挂载的实例装上系统媒体源。
 *
 * 「无媒体在播」也是合法状态（hasMedia: false），所以只要 SSE 出过一帧就该装上
 * —— 不装的话库会回落到自带的模拟源，壁纸显示测试用的假歌名/假封面。
 * 反之 adapter 不可用（端点 503、一帧都没来）时宁可让库兜着。
 *
 * 首帧可能晚于 mount（SSE 建连要时间），所以同样轮询到有数据为止；
 * 每次挂载都要重装：切壁纸换新实例，媒体源不继承。
 */
function attachSystemMedia(inst: SceneInstance, seq: number) {
  const install = () => {
    try {
      inst.setMedia(systemMedia);
      reportDiag(state.cfg, "now playing: attached to instance");
    } catch (e) {
      reportDiag(state.cfg, `now playing: setMedia 失败（${(e as Error)?.message ?? e}）`);
    }
  };
  if (systemMedia.subscribed) {
    install();
    return;
  }
  const t = window.setInterval(() => {
    if (seq !== state.seq) {
      window.clearInterval(t);
      return;
    }
    if (systemMedia.subscribed) {
      window.clearInterval(t);
      install();
    }
  }, 500);
}

/**
 * 拉取该壁纸的生效属性（project.json 默认 + 用户覆盖 + 全局语言兜底）。
 *
 * 网页壁纸的属性在 HTML 改写时已由 __weSeedProps 注入，不走这里；场景/媒体
 * 壁纸没有入口 HTML 可改写，必须在挂载前显式拉一次，作为 mount() 的 properties
 * 选项 —— 这样首帧就是 project.json 的值（覆盖 scene.pkg 里 scene.json 的默认），
 * 而不是先按 scene.json 渲染再跳变。
 *
 * 返回库需要的扁平 wire 格式 {name: value}；任何失败都静默返回空对象，
 * 属性拉不到不应阻止壁纸本身显示（与网页壁纸 shim 注入失败时的降级一致）。
 */
async function fetchWallpaperProps(cfg: WallpaperConfig): Promise<
  Record<string, boolean | number | string>
> {
  const token = audioToken;
  if (!token) return {};
  // item_id：scene 是 cfg.src 本身；媒体/web 从 {mediaBase}/<item>/... 反解
  let itemId: string | undefined;
  if (cfg.type === "scene") {
    itemId = cfg.src ?? undefined;
  } else if (cfg.src) {
    // cfg.src 是完整 URL：.../<media|web>/<token>/<item>/...
    const m = cfg.src.match(/\/(?:media|web)\/[^/]+\/([^/?#]+)/);
    itemId = m?.[1];
  }
  if (!itemId) return {};
  try {
    const r = await fetch(`/props/${token}/${encodeURIComponent(itemId)}`, {
      cache: "no-store",
    });
    if (!r.ok) return {};
    const wire: Record<string, { value?: unknown }> = await r.json();
    const flat: Record<string, boolean | number | string> = {};
    for (const [k, entry] of Object.entries(wire ?? {})) {
      const v = entry?.value;
      if (typeof v === "boolean" || typeof v === "number" || typeof v === "string") {
        flat[k] = v;
      }
    }
    return flat;
  } catch {
    return {};
  }
}

// mount 的两级时长阈值（毫秒）：
//   - 超过 SLOW 只上报一条「仍在解析」诊断，**绝不算失败**——大工程冷启动常见 10s+，
//     旧的 10s 硬超时会误判失败，导致 ready 永远不上报（截图侧干等到超时）。
//   - 超过 HARD 才兜底降级到默认壁纸，避免库内部真的卡死时窗口一片空白。
const MOUNT_SLOW_MS = 10_000;
const MOUNT_HARD_MS = 90_000;

/** 经库挂载壁纸（scene / web / video / gif / image 走同一条路） */
function mountViaLib(cfg: WallpaperConfig) {
  clear();
  const source = buildSource(cfg);
  if (!source) {
    reportDiag(cfg, "缺少 src/mediaBase，降级到默认壁纸");
    mountDefaultWallpaper();
    return;
  }
  const seq = state.seq;
  reportDiag(cfg, "mount start");

  void (async () => {
    try {
      // 场景/媒体壁纸挂载前拉取生效属性（project.json 覆盖 scene.json 默认 +
      // 全局语言兜底）。网页壁纸不需要（HTML 改写时已注入 __weSeedProps）。
      // 属性决定首帧，晚到会闪一下，所以先 await 再 mount。
      let properties: Record<string, boolean | number | string> | undefined;
      if (cfg.type !== "web") {
        properties = await fetchWallpaperProps(cfg);
        const n = properties ? Object.keys(properties).length : 0;
        if (n > 0) reportDiag(cfg, `壁纸属性已载入（${n} 项，含 project.json 覆盖与语言）`);
      }
      // 冷启动解析大工程（几十 MB 的 scene.pkg + 贴图）经常明显超过 10s。旧实现用
      // Promise.race 在 10s 直接判失败，代价是双重的：**不上报 ready**（截图侧只能一路
      // 干等到超时，报「等待渲染器就绪超时」），**且把还在解析的实例丢在一边**（漏 WebGL
      // 上下文，下一次挂载又要从零解析）。实测「截图等就绪超时」几乎都是这么来的。
      // 现在 10s 只上报一条「仍在解析」，真正的兜底放到 90s，且兜底后晚到的实例会被销毁。
      const slowWarn = setTimeout(
        () =>
          reportDiag(
            cfg,
            `mount 仍在进行（已超过 ${MOUNT_SLOW_MS / 1000}s，大工程冷启动需要时间）`,
          ),
        MOUNT_SLOW_MS,
      );
      // 注意：**不能**在选项里写 `audio: undefined` / `media: undefined`。
      // 库用 `if ("audio" in o)` 判「是否显式设置」——属性存在即命中，再经
      // `o.audio ?? null` 变成 `null` = **显式禁用**：
      //   - scene：`audioSim.enabled` 在挂载时按 `!rt.audioDisabled` 定死，
      //     之后 `setAudio()` 把 audioDisabled 清回 false 也救不回来，频谱恒为空；
      //   - web：音频泵按 `_webAudio === null || rt.audioDisabled` 直接不启动。
      // 切壁纸会重建窗口（新页面 → SSE 必然重连），首帧常晚于挂载，
      // 于是表现为「音频已在播放，设置壁纸后却没有可视化」。
      // 没有源时**省略这个键**，让库按其内置模拟源起泵，随后由
      // attachSystemAudio / attachSystemMedia 热装上真实源。
      const mountOpts: Parameters<typeof mountLib>[1] = {
        source,
        fit: normalizeFit(cfg.fit),
        renderDpr: toAbsoluteDpr(cfg.renderDpr ?? 0),
        fps: cfg.sceneFps ?? 24,
        volume: cfg.muted === false ? 1 : 0,
        // 渲染质量档位（库 1.3.23+）：键缺省/非法值由库 normalizeQuality 落默认
        quality: {
          antiAliasing: cfg.aa as QualityOptions["antiAliasing"],
          particles: cfg.particles as QualityOptions["particles"],
          postProcessing: cfg.postProcessing as QualityOptions["postProcessing"],
        },
        // 场景壁纸的初始属性（project.json 值，含全局语言）
        properties,
        onDiagnostic: (msg: string, level: string) => reportDiag(cfg, `[${level}] ${msg}`),
        onError: (err: Error) => reportDiag(cfg, `mount error: ${err.message}`),
      };
      if (systemAudio.alive) mountOpts.audio = systemAudio;
      if (systemMedia.subscribed) mountOpts.media = systemMedia;
      const mountPromise = mountLib(wrap, mountOpts);
      const inst = await Promise.race([
        mountPromise,
        new Promise<null>((resolve) => setTimeout(() => resolve(null), MOUNT_HARD_MS)),
      ]);
      clearTimeout(slowWarn);
      if (!inst) {
        // 兜底：库内部卡死时别让壁纸窗口空着，也让截图侧立刻拿到明确原因。
        // 晚到的实例必须销毁，否则又是一次上下文泄漏。
        reportDiag(cfg, `mount 超过 ${MOUNT_HARD_MS / 1000}s 仍未完成（疑似库内部卡住）`);
        mountDefaultWallpaper();
        void mountPromise
          .then((late) => {
            try {
              late.destroy({ releasePkgCache: true });
            } catch {
              /* 忽略 */
            }
          })
          .catch(() => {
            /* 已在上面的 catch 里报过 */
          });
        return;
      }
      // 装载期间又切了壁纸：本次结果作废，直接销毁避免泄漏 WebGL 上下文。
      // 它装载的是被取代的旧壁纸，缓存同样该弃（新的 mount 自己会拉自己的）
      if (seq !== state.seq) {
        try {
          inst.destroy({ releasePkgCache: true });
        } catch {
          /* 忽略 */
        }
        return;
      }
      state.inst = inst;
      // 库首帧后恒为播放态；若当前处于全局暂停（睡眠/用户暂停）需补上
      if (state.paused) inst.pause();
      // SSE 常在 mount 之后才首次收到帧；库的音频泵逐帧选源，此时补装也生效。
      // 每次挂载都要重来一遍 —— 切壁纸会换新实例，旧实例上的音频源不会继承
      attachSystemAudio(inst, seq);
      attachSystemMedia(inst, seq);
      reportDiag(cfg, "ready");
    } catch (e) {
      if (seq !== state.seq) return;
      const msg = String((e as Error)?.message || e).slice(0, 200);
      console.warn("wallpaper mount failed:", e);
      reportDiag(cfg, `failed: ${msg}`);
      mountDefaultWallpaper();
    }
  })();
}

// ---------- canvas 演示动画（非工坊类型，本文件自绘） ----------

function mountCanvas() {
  clear();
  const c = document.createElement("canvas");
  c.style.cssText = "position:absolute;inset:0;width:100%;height:100%;display:block;";
  wrap.appendChild(c);
  state.canvas = c;
  state.ctx = c.getContext("2d") ?? undefined;
  startCanvasLoop();
}

function startCanvasLoop() {
  const c = state.canvas;
  const ctx = state.ctx;
  if (!c || !ctx) return;
  // 相对倍率 → 绝对 DPR（与库渲染路径同一换算）
  const dpr = toAbsoluteDpr(state.cfg.renderDpr ?? 0) || (window.devicePixelRatio || 1);
  const resize = () => {
    c.width = Math.max(1, Math.round(innerWidth * dpr));
    c.height = Math.max(1, Math.round(innerHeight * dpr));
  };
  resize();
  window.addEventListener("resize", resize);
  const t0 = performance.now();
  const draw = (now: number) => {
    state.raf = requestAnimationFrame(draw);
    const t = (now - t0) / 1000;
    const w = c.width;
    const h = c.height;
    const g = ctx.createLinearGradient(0, 0, w, h);
    const hue = (t * 12) % 360;
    g.addColorStop(0, `hsl(${hue}, 55%, 12%)`);
    g.addColorStop(1, `hsl(${(hue + 60) % 360}, 60%, 22%)`);
    ctx.fillStyle = g;
    ctx.fillRect(0, 0, w, h);
    for (let i = 0; i < 40; i++) {
      const x = ((i * 137.5 + t * 30) % (w + 200)) - 100;
      const y = h * 0.5 + Math.sin(t * 0.6 + i) * h * 0.3;
      const r = 20 + (i % 5) * 12;
      ctx.beginPath();
      ctx.arc(x, y, r, 0, Math.PI * 2);
      ctx.fillStyle = `hsla(${(hue + i * 8) % 360}, 70%, 60%, 0.06)`;
      ctx.fill();
    }
  };
  state.raf = requestAnimationFrame(draw);
}

/// 兜底提示：壁纸缺失/加载失败时，铺一层简洁的 SVG + 文案。
/// 不再加载内置 HTML 页 —— 观感差，还多一层 iframe 与资源依赖。
function mountDefaultWallpaper() {
  clear();
  const box = document.createElement("div");
  box.setAttribute("role", "img");
  box.setAttribute("aria-label", "wallpaper unavailable");
  box.style.cssText =
    "position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;" +
    "justify-content:center;gap:16px;background:#0e1013;color:rgba(255,255,255,.62);" +
    "font:13px/1.7 -apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC',sans-serif;" +
    "user-select:none;pointer-events:none;";
  box.innerHTML =
    '<svg width="76" height="76" viewBox="0 0 48 48" fill="none" aria-hidden="true">' +
    '<rect x="5.5" y="9" width="37" height="30" rx="4.5" stroke="currentColor" stroke-opacity=".5" stroke-width="2"/>' +
    '<circle cx="16" cy="19" r="3.2" fill="currentColor" fill-opacity=".5"/>' +
    '<path d="M7.5 34.5l9.5-9.5 5.5 5.5L32 21l8.5 8.5" stroke="currentColor" stroke-opacity=".5" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>' +
    "</svg>" +
    '<div style="text-align:center">壁纸无法加载' +
    '<br><span style="opacity:.55">Wallpaper unavailable</span></div>';
  wrap.appendChild(box);
}

function mount(cfg: WallpaperConfig) {
  // 滤镜挂在 wrap 上（不是壁纸内容上），所以每次挂载都重设一次即可：
  // clear() 只清子节点、不动 wrap 自身的 style
  applyWallpaperFilter(cfg.filter);
  if (cfg.type === "canvas") mountCanvas();
  else if (cfg.src) mountViaLib(cfg);
  else mountDefaultWallpaper(); // 无壁纸/未知类型 → 内联 SVG + 文案提示
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
      /** 渲染质量档位热更（库 1.3.23+）：部分更新、就地生效不重挂载 */
      setQuality(patch: QualityOptions): void;
      /** 当前生效的质量档位（三项齐全；未挂载库实例时为 null） */
      getQuality(): ResolvedQuality | null;
      /** 切换全局滤镜：传白名单 id（WALLPAPER_FILTERS），未知 id 按无滤镜处理 */
      setFilter(filter: string): void;
      /** 热更新 WE 用户属性（wire 格式：{name: {value: ...}}） */
      updateWebProps(props: Record<string, { value: unknown }>): void;
      /** 系统音频捕获开关变化（原生侧在用户切换设置后调用） */
      setSystemAudio(enabled: boolean, token?: string): void;
      /**
       * 外部指针注入。「隐藏图标」关闭时壁纸窗口在桌面图标下方、收不到真实
       * 鼠标事件，由原生侧轮询系统光标推进来（u/v 为窗口内归一化坐标，
       * buttons bit0 = 左键）。开启「隐藏图标」后原生侧会停止调用。
       */
      pushPointer(u: number, v: number, buttons: number): void;
      /** 指针离开本窗口（跨屏或停止注入） */
      pointerLeave(): void;
    };
  }
}

window.__wp = {
  setWallpaper(cfg: WallpaperConfig) {
    state.cfg = cfg;
    mount(cfg);
  },
  pause() {
    state.paused = true;
    state.inst?.pause();
    if (state.raf !== undefined) {
      cancelAnimationFrame(state.raf);
      state.raf = undefined;
    }
  },
  resume() {
    state.paused = false;
    state.inst?.resume();
    if (state.cfg.type === "canvas") startCanvasLoop();
  },
  setFit(fit: string) {
    state.cfg.fit = fit as WallpaperFit;
    state.inst?.setFit(normalizeFit(fit as WallpaperFit));
  },
  setVolume(volume: number) {
    const v = Math.max(0, Math.min(1, volume));
    state.cfg.muted = v <= 0;
    state.inst?.setVolume(v);
  },
  // 释放壁纸渲染资源，归还内存；保留 state.cfg 供 restore() 重建
  release() {
    // 走库的 release：保留已解析的场景数据，restore 时不必重新下载解析
    if (state.inst) {
      state.inst.release();
      return;
    }
    clear();
  },
  // 重新挂载上次配置（显示器睡眠后唤醒、或 release() 之后恢复）
  restore() {
    if (state.inst) {
      state.inst.restore();
      if (state.paused) state.inst.pause();
      return;
    }
    if (state.cfg) mount(state.cfg);
  },
  // 动态调整渲染分辨率（入参是相对倍率：0 自动 / 0.75 / 0.85 / 1 高清）
  setRenderDpr(dpr: number) {
    state.cfg.renderDpr = dpr;
    // 换算成库的绝对 DPR；库内部重挂载，不重新下载解析（source.key 命中缓存）
    const absolute = toAbsoluteDpr(dpr);
    if (state.inst) {
      state.inst.setRenderDpr(absolute);
      if (state.paused) state.inst.pause();
      return;
    }
    mount(state.cfg);
  },
  // 调整帧率上限
  setSceneFps(fps: number) {
    state.cfg.sceneFps = fps;
    state.inst?.setFps(fps);
  },
  // 渲染质量档位热更（抗锯齿/粒子/后处理）：就地生效不重挂载；
  // 写回 state.cfg 让 setRenderDpr/restore 这类重挂路径之后仍保持。只传要改的键。
  setQuality(patch: QualityOptions) {
    if (patch?.antiAliasing !== undefined) state.cfg.aa = patch.antiAliasing;
    if (patch?.particles !== undefined) state.cfg.particles = patch.particles;
    if (patch?.postProcessing !== undefined) state.cfg.postProcessing = patch.postProcessing;
    state.inst?.setQuality(patch);
  },
  getQuality() {
    return state.inst?.getQuality() ?? null;
  },
  // 切换滤镜：纯 CSS 合成层的事，不用重挂壁纸、不用碰库实例
  setFilter(filter: string) {
    state.cfg.filter = filter;
    applyWallpaperFilter(filter);
  },
  // 热更新 WE 用户属性（属性编辑保存后由原生侧调用，免刷新生效）
  updateWebProps(props: Record<string, { value: unknown }>) {
    // 库支持逐属性热更，不必重挂载（旧实现要重新下载解析上百 MB 的 scene.pkg）。
    // 宿主下发的是 wire 格式 {name:{value}}，库的 setProperties 收扁平 {name: value}。
    if (!state.inst) return;
    const flat: Record<string, boolean | number | string> = {};
    for (const [k, v] of Object.entries(props)) {
      const val = v?.value;
      if (typeof val === "boolean" || typeof val === "number" || typeof val === "string") {
        flat[k] = val;
      }
    }
    if (Object.keys(flat).length) state.inst.setProperties(flat);
  },
  // 系统音频捕获开关：开启时订阅 SSE 并热切到真实频谱，关闭时回落库的内置模拟源
  setSystemAudio(enabled: boolean, token?: string) {
    if (enabled && token) {
      systemAudio.connect(token, (m) => reportDiag(state.cfg, m));
      // 此刻还没数据，attachSystemAudio 会等首帧到达后再装
      if (state.inst) attachSystemAudio(state.inst, state.seq);
    } else {
      systemAudio.close();
      state.inst?.setAudio(null);
      reportDiag(state.cfg, "system audio: disabled");
    }
  },
  // 外部指针注入：underlay 层（图标下方）收不到真实鼠标，宿主轮询系统光标后从这里推进来。
  // u/v 是归一化坐标（0..1，相对壁纸窗口）；buttons 为 0 无按键、1 左键按下。
  // 交互态（隐藏图标开启）时不要调用：窗口已在图标之上，能直接收真实事件，
  // 再注入一次会导致同一帧被处理两遍（视差、hover 抖动）。
  pushPointer(u: number, v: number, buttons: number) {
    state.inst?.pushPointer(u, v, buttons);
  },
  pointerLeave() {
    state.inst?.pointerLeave();
  },
};

// 初始配置优先取自 URL query（壁纸引擎窗口创建时注入，同步无竞态）
const params = new URLSearchParams(location.search);
const initialCfg: WallpaperConfig = {
  type: (params.get("type") as WallpaperConfig["type"]) ?? "canvas",
  src: params.get("src") ?? undefined,
  fit: (params.get("fit") as WallpaperConfig["fit"]) ?? "cover",
  // query 未带 renderDpr 时为 0（自动跟随设备 DPR）；query 值是相对倍率
  renderDpr: Number(params.get("renderDpr")) || 0,
  sceneFps: Number(params.get("sceneFps")) || 24,
  filter: params.get("filter") ?? undefined,
  // 渲染质量档位（query 键 aa/pq/pp → cfg 字段 aa/particles/postProcessing）
  aa: params.get("aa") ?? undefined,
  particles: params.get("pq") ?? undefined,
  postProcessing: params.get("pp") ?? undefined,
  muted: params.get("muted") !== "false",
  loop: params.get("loop") !== "false",
  mediaBase: params.get("mediaBase") ?? undefined,
};
state.cfg = initialCfg;

// 系统音频：token 随 URL 下发（与 mediaBase 同源），有则立即订阅。
// 端点在捕获关闭时返回 503，EventSource 自动重试，用户打开开关后自愈
const audioToken = params.get("audioToken");
if (audioToken) {
  systemAudio.connect(audioToken, (m) => reportDiag(initialCfg, m));
  // 「正在播放」复用同一个内容服务器 token。与音频不同它没有开关：
  // 拿不到系统媒体时下发 hasMedia:false，壁纸显示空态而不是库的假数据
  systemMedia.connect(audioToken, (m) => reportDiag(initialCfg, m));
}

mount(initialCfg);

// 页面卸载兜底：预览 iframe 关闭 / 壁纸窗口销毁时释放 WebGL 上下文与 SSE 连接
const teardown = () => {
  systemAudio.close();
  systemMedia.close();
  clear();
};
window.addEventListener("pagehide", teardown);
window.addEventListener("beforeunload", teardown);

export {};
