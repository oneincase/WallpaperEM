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
  /**
   * 视频纹理上传倍率（库 2.0.0+）：0 = 自动（库内帧率守门按实测帧率压），
   * 正数 = 固定（1 = 不压最清晰 / 0.5 = 半幅）。见画质页「视频纹理清晰度」。
   * WKWebView 下逐帧 texImage2D(视频帧) 要同步跨进程取像素，代价随像素数线性。
   */
  videoTexScale?: number;
  /** 全局滤镜 id（见 WALLPAPER_FILTERS 白名单），未知 id 按无滤镜处理 */
  filter?: string;
  /**
   * 无缝切换的过渡效果 id（见 REVEAL_FX 白名单）：新窗在旧窗上方**显形**的方式。
   * 动画时长见 REVEAL_MS 表；宿主按同一份时长 + 裕量等收尾才收旧窗
   * （wallpaper::reveal_fx_wait_ms，两边同步改）。未知 id / 缺省回落叠化（旧行为）。
   */
  reveal?: string;
  /**
   * 渲染质量档位（库 1.3.23+）：抗锯齿 aa（off/fxaa/msaa2/msaa4）、
   * 粒子 particles（off/low/medium/high）、后处理 postProcessing（同档）。
   * query 键是 aa/pq/pp（与上游 bench 约定）；setWallpaper 下发的 JSON 键与
   * 这里的字段名一致（Rust serde camelCase）。缺省 = 库默认（off/high/high）。
   */
  aa?: string;
  particles?: string;
  postProcessing?: string;
  /**
   * 本壁纸的精确音量（0..1；0 即静音）。挂载时一律先静音（muted），壁纸完全
   * 加载完成（首帧/ready）后再按这个值统一起音量 —— 避免「静音壁纸在暂停
   * 恢复/唤醒重挂后先响一段」以及加载途中就出声。
   * 缺省（旧会话无记录）时回落 muted 布尔：false=1，true=0。
   */
  volume?: number;
  muted?: boolean;
  loop?: boolean;
  /** 内容服务器媒体基址：http://127.0.0.1:<port>/media/<token>（scene 拉取 pkg 用） */
  mediaBase?: string;
  /** project.json `file` 声明的场景 pkg 相对路径（命名/布局不规范时给拉取指路） */
  scenePkg?: string;
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
// 纯黑「黑边兜底」在 wrap 上（不在页面背景上）：缩放（contain）模式下画布/视频
// 留边的区域靠 wrap 的 #000 兜底 —— WebGL 侧 contain 留边是 clearColor(0,0,0,0)
// 的透明像素，合成时落到 wrap 背景上，两处一起兜成黑边。页面背景保持**透明**：
// 无缝切换时新窗口在旧窗口上方后台加载，首帧前必须透出旧壁纸（渐入也靠 wrap 的
// opacity 0→1 完成）；reveal 之后 wrap 连同黑边一起显形，黑边语义不变。
document.documentElement.style.cssText = "margin:0;height:100%;background:transparent;";
document.body.style.cssText =
  "margin:0;width:100vw;height:100vh;overflow:hidden;background:transparent;position:relative;";

const wrap: HTMLDivElement = (() => {
  const existing = document.getElementById("wrap");
  if (existing instanceof HTMLDivElement) return existing;
  const el = document.createElement("div");
  el.id = "wrap";
  el.style.cssText =
    "position:fixed;inset:0;overflow:hidden;background:#000;opacity:0;transition:opacity .7s ease;";
  document.body.appendChild(el);
  return el;
})();

/**
 * 首帧就绪后显形（0.7s）。整页生命周期只显形一次：同一窗口内的热更新
 * （setWallpaper 重挂）不该反复淡入淡出。无缝切换的宿主侧在 ready 后等显形
 * 走完才收旧窗，视觉上是「切换效果」而不是跳变。
 */
let revealed = false;
function reveal() {
  if (revealed) return;
  revealed = true;
  shareLoaderHide(); // 分享页：壁纸显形即收加载层（桌面页无此层，空操作）
  runRevealFx(shareRevealFx()); // 竖屏旋转下强制叠化（transform 冲突，见 shareApplyOrient）
}

// ---- 无缝切换的过渡效果（新窗在旧窗上方「显形」的方式）----
//
// 新窗口透明起步，wrap 的显形动画就是用户看到的切换效果：默认叠化沿用 wrap
// 自带的 opacity transition；其余效果用 Web Animations API 在 wrap 上跑一次性
// 关键帧（transform / filter / clip-path），结束后**写回内联终态再 cancel** ——
// fill 若留在元素上会永久盖住 style.filter（全局滤镜）等内联改动，cancel 让
// 关键帧彻底放手、由内联样式接管。
// 时长按效果单独定：叠化是旧有行为保持 0.7s；位移/裁剪/模糊类必须 1s+ 才能
// 「看清」——动作与淡入同时发生，时长太短就只剩隐约一顿（实测反馈）。宿主
// 收旧窗的等待见 wallpaper::reveal_fx_wait_ms，与这份表**必须同步改**。
const REVEAL_MS: Record<string, number> = {
  fade: 700,
  zoom: 1300,
  blur: 1300,
  depth: 1400,
  circle: 1200,
  wipe: 1000,
  slide: 1200,
};
/** 动画缓动：起步平缓、收尾减速 —— 动作铺满全程，而不是挤在前 1/3 就结束 */
const REVEAL_EASING = "cubic-bezier(0.35, 0.1, 0.25, 1)";

function runRevealFx(fx: string) {
  const plainFade = () => {
    requestAnimationFrame(() => {
      wrap.style.opacity = "1";
    });
  };
  if (fx === "fade") {
    plainFade(); // 默认叠化：CSS transition 负责，不引入关键帧
    return;
  }
  // blur/景深的 filter 关键帧要带上当前全局滤镜的表达式：WAAPI 对函数列表
  // 不匹配的 filter（如 blur(44px) → sepia(0.75)）只能离散跳变，两端都凑成
  // 「blur(N) + 同一段滤镜」才能连续插值；收尾 style.filter 接管也不跳帧
  const base = WALLPAPER_FILTERS[state.cfg.filter ?? "none"] ?? "";
  const blurPair = (px: number): [string, string] =>
    base ? [`blur(${px}px) ${base}`, `blur(0px) ${base}`] : [`blur(${px}px)`, "blur(0px)"];
  const keyframes: Keyframe[] | null = (() => {
    switch (fx) {
      case "zoom": // 推近：从 1.3 倍缩回原位
        return [
          { opacity: 0, transform: "scale(1.3)" },
          { opacity: 1, transform: "scale(1)" },
        ];
      case "blur": {
        // 模糊：重失焦到聚焦（44px 在低透明度下也读得出）
        const [from, to] = blurPair(44);
        return [
          { opacity: 0, filter: from },
          { opacity: 1, filter: to },
        ];
      }
      case "depth": {
        // 景深：推近与失焦同时收拢
        const [from, to] = blurPair(30);
        return [
          { opacity: 0, transform: "scale(1.35)", filter: from },
          { opacity: 1, transform: "scale(1)", filter: to },
        ];
      }
      // 以下两端都写 opacity:1 —— 只动裁剪/位移，被裁掉的区域透出旧壁纸
      case "circle": // 圆形揭示：光圈从中心展开（85% 保证盖到四角）
        return [
          { opacity: 1, clipPath: "circle(0% at 50% 50%)" },
          { opacity: 1, clipPath: "circle(85% at 50% 50%)" },
        ];
      case "wipe": // 横向擦除：左 → 右
        return [
          { opacity: 1, clipPath: "inset(0 100% 0 0)" },
          { opacity: 1, clipPath: "inset(0 0% 0 0)" },
        ];
      case "slide": // 滑入：整幅从右滑进，未覆盖处透出旧壁纸
        return [
          { opacity: 1, transform: "translateX(100%)" },
          { opacity: 1, transform: "translateX(0%)" },
        ];
      default: // 未知 id：回落叠化
        return null;
    }
  })();
  if (!keyframes) {
    plainFade();
    return;
  }
  const anim = wrap.animate(keyframes, {
    duration: REVEAL_MS[fx] ?? REVEAL_MS.fade,
    easing: REVEAL_EASING,
    fill: "both",
  });
  // 先写回内联终态再 cancel：放手瞬间画面不变。finished 因 cancel reject 时
  // 同样要走收尾，两路都指向 done
  const done = () => {
    wrap.style.opacity = "1";
    // 恢复的是常驻旋转（竖屏分享模式）而非清空 —— 清空会丢失方向
    wrap.style.transform = shareWrapTransform();
    wrap.style.clipPath = "";
    try {
      anim.cancel();
    } catch {
      /* 已收尾的动画不可再 cancel，防御一层 */
    }
  };
  anim.finished.then(done, done);
}

// ---- 分享页加载指示（仅 share 域挂载显示；桌面壁纸窗口不受影响）----
//
// 分享渲染页要拉的是几十 MB 级资源（scene.pkg / 视频），裸等就是一块黑屏。
// 加载层提供：中央彩色音符条（SVG 均衡器动画）+ 实时进度条。进度是**真**的：
// - video / gif / image：渲染器自己流式预取主文件（逐块计字节）后转 blob URL
//   直接喂库 —— 进度全程真实，不双倍下载；
// - scene：同样流式预取 scene.pkg，但产物交给浏览器 HTTP 缓存（分享路由带
//   Cache-Control），库随后的同 URL 请求秒回缓存；
// - web：多小文件没有单一总量，进度按「已见资源字节」爬行、封顶 90%，如实不造假。
//
// 注意这段代码跑在**所有**壁纸页里，必须保持零副作用：非 share 域不会创建任何
// 节点、不发起任何预取。

function isShareMount(cfg: WallpaperConfig): boolean {
  return (cfg.mediaBase ?? "").startsWith("/share/");
}

type ShareLoaderState = {
  root: HTMLDivElement;
  fill: HTMLDivElement;
  pct: HTMLSpanElement;
  stage: HTMLDivElement;
  /** 爬行进度（无总量时的乐观推进），封顶 0.9 */
  crawl: number;
  /** 预取实测进度（0..1） */
  real: number;
  timer: number;
  hidden: boolean;
};

let shareLoader: ShareLoaderState | null = null;

function shareLoaderShow(): void {
  if (shareLoader) return;
  const root = document.createElement("div");
  root.id = "share-loader";
  root.style.cssText =
    "position:fixed;inset:0;z-index:9;display:grid;place-items:center;" +
    "background:radial-gradient(120% 120% at 50% 38%, #10141f 0%, #06080d 72%);" +
    "transition:opacity .45s ease;";
  root.innerHTML = `
  <style>
    #share-loader .eqb { transform-box: fill-box; transform-origin: center bottom;
      animation: eqwave .9s ease-in-out infinite alternate; }
    #share-loader .e2 { animation-delay: .12s } #share-loader .e3 { animation-delay: .24s }
    #share-loader .e4 { animation-delay: .36s } #share-loader .e5 { animation-delay: .48s }
    @keyframes eqwave { from { transform: scaleY(.2) } to { transform: scaleY(1) } }
    #share-loader .eqn { animation: eqfloat 1.8s ease-in-out infinite; }
    #share-loader .n2 { animation-delay: .55s }
    @keyframes eqfloat { 0%, 100% { transform: translateY(0); opacity: .45 }
      50% { transform: translateY(-7px); opacity: 1 } }
  </style>
  <div style="display:flex;flex-direction:column;align-items:center;gap:18px;user-select:none">
    <svg width="132" height="72" viewBox="0 0 132 72" fill="none" aria-hidden="true">
      <defs>
        <linearGradient id="eqg1" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#22d3ee"/><stop offset="1" stop-color="#38bdf8"/></linearGradient>
        <linearGradient id="eqg2" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#34d399"/><stop offset="1" stop-color="#a3e635"/></linearGradient>
        <linearGradient id="eqg3" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#818cf8"/><stop offset="1" stop-color="#a78bfa"/></linearGradient>
        <linearGradient id="eqg4" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#fb7185"/><stop offset="1" stop-color="#f472b6"/></linearGradient>
        <linearGradient id="eqg5" x1="0" y1="1" x2="0" y2="0"><stop offset="0" stop-color="#fbbf24"/><stop offset="1" stop-color="#fb923c"/></linearGradient>
      </defs>
      <rect class="eqb"    x="14" y="8" width="12" height="56" rx="6" fill="url(#eqg1)"/>
      <rect class="eqb e2" x="33" y="8" width="12" height="56" rx="6" fill="url(#eqg2)"/>
      <rect class="eqb e3" x="52" y="8" width="12" height="56" rx="6" fill="url(#eqg3)"/>
      <rect class="eqb e4" x="71" y="8" width="12" height="56" rx="6" fill="url(#eqg4)"/>
      <rect class="eqb e5" x="90" y="8" width="12" height="56" rx="6" fill="url(#eqg5)"/>
      <text class="eqn n1" x="16" y="16" fill="#7dd3fc" font-size="14">♪</text>
      <text class="eqn n2" x="100" y="20" fill="#f9a8d4" font-size="16">♫</text>
    </svg>
    <div style="display:flex;align-items:center;gap:10px">
      <div style="width:220px;height:6px;border-radius:999px;background:rgba(255,255,255,.12);overflow:hidden">
        <div id="share-load-fill" style="height:100%;width:3%;border-radius:999px;background:linear-gradient(90deg,#22d3ee,#a78bfa,#f472b6);transition:width .18s ease"></div>
      </div>
      <span id="share-load-pct" style="font:600 12px ui-monospace,SFMono-Regular,Menlo,monospace;color:#cbd5e1;min-width:36px">3%</span>
    </div>
    <div id="share-load-stage" style="font:12.5px ui-sans-serif,system-ui,sans-serif;color:#8b949e">加载资源 · Downloading</div>
  </div>`;
  document.body.appendChild(root);
  shareLoader = {
    root,
    fill: root.querySelector("#share-load-fill") as HTMLDivElement,
    pct: root.querySelector("#share-load-pct") as HTMLSpanElement,
    stage: root.querySelector("#share-load-stage") as HTMLDivElement,
    crawl: 0.03,
    real: 0,
    timer: 0,
    hidden: false,
  };
  // 爬行器：无总量（web 类型 / 总长未知）时按渐近曲线推进，封顶 90%
  shareLoader.timer = window.setInterval(() => {
    if (!shareLoader || shareLoader.hidden) return;
    shareLoader.crawl = Math.min(0.9, shareLoader.crawl + (0.9 - shareLoader.crawl) * 0.035);
    shareLoaderRender();
  }, 130);
}

/** 更新进度。frac=null 表示该阶段无总量（交给爬行器）；stage 换文案。 */
function shareLoaderSet(frac: number | null, stage?: string): void {
  if (!shareLoader || shareLoader.hidden) return;
  if (frac != null && Number.isFinite(frac)) {
    shareLoader.real = Math.max(shareLoader.real, Math.min(1, frac));
  }
  if (stage) shareLoader.stage.textContent = stage;
  shareLoaderRender();
}

function shareLoaderRender(): void {
  if (!shareLoader || shareLoader.hidden) return;
  const display = Math.max(shareLoader.crawl, shareLoader.real);
  const pct = Math.round(display * 100);
  shareLoader.fill.style.width = `${Math.max(3, pct)}%`;
  shareLoader.pct.textContent = `${pct}%`;
}

function shareLoaderHide(): void {
  if (!shareLoader || shareLoader.hidden) return;
  shareLoader.hidden = true;
  window.clearInterval(shareLoader.timer);
  shareLoader.fill.style.width = "100%";
  shareLoader.pct.textContent = "100%";
  shareLoader.stage.textContent = "完成 · Ready";
  const root = shareLoader.root;
  window.setTimeout(() => root.remove(), 480);
}

/** 主资源流式预取：逐块回调进度；keep=true 时聚成 Blob（媒体类喂库用）。 */
async function streamShareResource(
  url: string,
  keep: boolean,
  onProgress: (loaded: number, total: number) => void,
): Promise<Blob | null> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const total = Number(res.headers.get("content-length")) || 0;
  const reader = res.body?.getReader();
  if (!reader) return null; // 无流环境：放弃预取，库自己拉
  const type = res.headers.get("content-type") ?? "application/octet-stream";
  const chunks: Uint8Array[] = [];
  let loaded = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    loaded += value.byteLength;
    if (keep) chunks.push(value);
    onProgress(loaded, total);
  }
  if (!keep) return null;
  return new Blob(chunks as BlobPart[], { type });
}

/** share 域 URL 拼接：除首段（已是路径）外逐段编码（文件名空格/中文）。 */
function shareUrl(base: string, ...segs: string[]): string {
  const path = [base.replace(/\/+$/, ""), ...segs.map((s) => s.split("/").map(encodeURIComponent).join("/"))].join("/");
  return new URL(path, location.href).href;
}

/**
 * 分享挂载的前置准备：把主资源先拉下来（带真实进度），媒体类转 blob 直喂，
 * scene 落 HTTP 缓存供库秒取。失败不阻断挂载（退化为无进度黑屏 → 库自己报错）。
 */
async function prepareSharePrimary(cfg: WallpaperConfig): Promise<void> {
  const base = (cfg.mediaBase ?? "").replace(/\/+$/, "");
  if (cfg.type === "scene" && cfg.src) {
    // 与库同一份候选链：声明优先，缺省 scene.pkg / scenes/scene.pkg
    const candidates = cfg.scenePkg ? [cfg.scenePkg] : [];
    for (const p of ["scene.pkg", "scenes/scene.pkg"]) {
      if (!candidates.includes(p)) candidates.push(p);
    }
    for (const pkg of candidates) {
      const url = shareUrl(base, cfg.src, ...pkg.split("/"));
      try {
        await streamShareResource(url, false, (loaded, total) => {
          shareLoaderSet(total ? loaded / total : null, "加载资源 · Downloading");
        });
        shareLoaderSet(1, "解析挂载 · Preparing");
        return;
      } catch {
        continue; // 试下一个候选名
      }
    }
    shareLoaderSet(null); // 全部候选失败：交给库自己报错，进度走爬行
  } else if ((cfg.type === "video" || cfg.type === "gif" || cfg.type === "image") && cfg.src) {
    const url = new URL(cfg.src, location.href).href;
    const blob = await streamShareResource(url, true, (loaded, total) => {
      shareLoaderSet(total ? loaded / total : null, "加载资源 · Downloading");
    });
    if (blob) cfg.src = URL.createObjectURL(blob);
    shareLoaderSet(1, "解析挂载 · Preparing");
  } else {
    shareLoaderSet(null); // web 等多文件类型：爬行模式
  }
}

/** 分享挂载入口：先显示加载层、预取主资源，再走常规 mount。 */
async function bootstrapShareMount(cfg: WallpaperConfig): Promise<void> {
  // 访客页黑底：竖屏/横屏模式的留边（桌面页必须保持透明，见 wrap 注释）
  document.body.style.background = "#000";
  shareLoaderShow();
  try {
    await prepareSharePrimary(cfg);
  } catch (e) {
    reportDiag(cfg, `share preload failed: ${e instanceof Error ? e.message : String(e)}`);
  }
  const orient = shareOrientEffective();
  shareApplyOrient(orient);
  window.addEventListener("resize", () => {
    shareApplyOrient(shareOrientEffective());
  });
  sharePetShow(cfg, orient);
  mount(cfg);
  shareSubscribeProps(audioToken);
}

// ---- 分享访客 HUD：横竖屏切换 / 全屏 / 静音（仅 share 域显示）----
//
// 横竖屏：三态循环（跟随窗口 → 竖屏 9:16 → 横屏 16:9），实现 = 把 wrap 从
// 「铺满视口」改成「按画幅 contain 居中」，壁纸内容自身 cover 填框 —— 预览
// 手机/桌面两种画幅，不裁剪不变形。偏好记 localStorage（访客本地，不回传）；
// URL ?orient= 优先于记忆（分享者可强制）。

type ShareOrient = "auto" | "portrait" | "landscape";
const SHARE_ORIENT_KEY = "wpem.share.orient";

function shareOrientEffective(): ShareOrient {
  const q = new URLSearchParams(location.search).get("orient");
  if (q === "portrait" || q === "landscape" || q === "auto") return q;
  const ls = localStorage.getItem(SHARE_ORIENT_KEY);
  if (ls === "portrait" || ls === "landscape" || ls === "auto") return ls;
  return "auto";
}

function shareApplyOrient(o: ShareOrient): void {
  // 竖屏：内容按「横屏视口」尺寸渲染（壁纸以为自己在横屏），整体 rotate 90°
  // 落进竖屏 —— 构图完整无裁切，方向跟着画幅切（移动端核心诉求）。
  // wrap 的 transform 同时被 reveal 动画使用：竖屏下 reveal 强制 fade
  //（见 shareRevealFx），动画收尾恢复的是这个常驻旋转而不是清空。
  if (o === "portrait") {
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    wrap.style.left = `${Math.round((vw - vh) / 2)}px`;
    wrap.style.top = `${Math.round((vh - vw) / 2)}px`;
    wrap.style.right = "auto";
    wrap.style.bottom = "auto";
    wrap.style.width = `${vh}px`;
    wrap.style.height = `${vw}px`;
    wrap.style.transform = "rotate(90deg)";
  } else {
    wrap.style.left = "0";
    wrap.style.top = "0";
    wrap.style.right = "0";
    wrap.style.bottom = "0";
    wrap.style.width = "";
    wrap.style.height = "";
    wrap.style.transform = "";
  }
  // wrap 尺寸/朝向变了要库自己重排画布：借窗口 resize 事件触发它的 re-fit
  window.dispatchEvent(new Event("resize"));
}

/** 竖屏模式下 wrap 的常驻旋转量（reveal 动画收尾时恢复用） */
function shareWrapTransform(): string {
  return shareOrientEffective() === "portrait" ? "rotate(90deg)" : "";
}

/** 竖屏下 reveal 不能用 transform 类效果（会跟常驻旋转打架），强制叠化 */
function shareRevealFx(): string {
  const fx = state.cfg.reveal ?? "fade";
  return shareOrientEffective() === "portrait" ? "fade" : fx;
}

const SHARE_PET_KEY = "wpem.share.pet";
/** 手机上宠物与按钮都要更大（用户实测：小屏差点看不到） */
const PET_SIZE = window.innerWidth < 560 ? 78 : 60;
const HUD_BTN = window.innerWidth < 560 ? 48 : 40;

function petMascotSvg(): string {
  // 兔子音符伙伴：长耳 + 腮红 + ω 嘴，配色跟加载层一致
  return `<svg viewBox="0 0 64 64" width="${PET_SIZE}" height="${PET_SIZE}" aria-hidden="true">
    <defs>
      <radialGradient id="petg" cx="35%" cy="26%" r="85%">
        <stop offset="0" stop-color="#8ff0fb"/><stop offset=".55" stop-color="#8b9cf9"/><stop offset="1" stop-color="#5b5bd6"/>
      </radialGradient>
    </defs>
    <g class="pet-bob">
      <ellipse cx="32" cy="57" rx="14" ry="3.2" fill="rgba(0,0,0,.35)"/>
      <g class="pet-ear ear-l">
        <rect x="20" y="2" width="9" height="22" rx="4.5" fill="url(#petg)"/>
        <rect x="22.4" y="6" width="4.2" height="14" rx="2.1" fill="#f9a8d4" opacity=".85"/>
      </g>
      <g class="pet-ear ear-r">
        <rect x="35" y="2" width="9" height="22" rx="4.5" fill="url(#petg)"/>
        <rect x="37.4" y="6" width="4.2" height="14" rx="2.1" fill="#f9a8d4" opacity=".85"/>
      </g>
      <circle cx="32" cy="42" r="20" fill="url(#petg)"/>
      <g class="pet-eye">
        <ellipse cx="24.6" cy="40" rx="3.8" ry="4.8" fill="#0b1020"/>
        <circle cx="25.9" cy="38.4" r="1.4" fill="#fff"/>
      </g>
      <g class="pet-eye">
        <ellipse cx="39.4" cy="40" rx="3.8" ry="4.8" fill="#0b1020"/>
        <circle cx="40.7" cy="38.4" r="1.4" fill="#fff"/>
      </g>
      <ellipse cx="19.4" cy="46.5" rx="3.6" ry="2.1" fill="#f9a8d4" opacity=".6"/>
      <ellipse cx="44.6" cy="46.5" rx="3.6" ry="2.1" fill="#f9a8d4" opacity=".6"/>
      <path d="M29.5 48c1 1.5 2 1.5 3 0M32.5 48c1 1.5 2 1.5 3 0" stroke="#0b1020" stroke-width="1.6" fill="none" stroke-linecap="round"/>
    </g>
    <g class="pet-note"><text x="47" y="13" font-size="13" fill="#f9a8d4">♪</text></g>
  </svg>`;
}

function hudIcon(paths: string): string {
  return `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">${paths}</svg>`;
}

const SHARE_ICONS = {
  portrait:
    '<rect x="7" y="3" width="10" height="18" rx="2"/><path d="M11 18.2h2"/>',
  landscape:
    '<rect x="3" y="7" width="18" height="10" rx="2"/><path d="M18.2 14v-4"/>',
  auto: '<path d="M14 4h6v6M10 20H4v-6M20 4l-6.5 6.5M4 20l6.5-6.5"/>',
  fullscreen:
    '<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>',
  fsExit: '<path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5"/>',
  soundOn:
    '<path d="M4 9.5v5h3.5L12 19V5L7.5 9.5H4Z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M18 6a8.5 8.5 0 0 1 0 12"/>',
  soundOff: '<path d="M4 9.5v5h3.5L12 19V5L7.5 9.5H4Z"/><path d="M16 9.5l5 5M21 9.5l-5 5"/>',
  props: '<path d="M4 6h10M4 12h16M4 18h7"/><circle cx="17" cy="6" r="2.4"/><circle cx="14" cy="18" r="2.4"/>',
  reload: '<path d="M20 12a8 8 0 1 1-2.3-5.6"/><path d="M20 3v4h-4"/>',
};

let shareHud: {
  pet: HTMLDivElement;
  menu: HTMLDivElement;
  orientBtn: HTMLButtonElement;
  muteBtn: HTMLButtonElement;
  orient: ShareOrient;
  muted: boolean;
  expanded: boolean;
} | null = null;

/**
 * 分享访客 HUD：可拖动的边缘吸附小宠物，点击呼出操作列（画幅/全屏/静音/作者属性/重载）。
 * - 拖动：window 级 pointer 监听（触屏可靠）+ 6px 阈值区分点击；松手吸附较近左右边缘
 *   并记忆位置；默认右上角。
 * - 尺寸自适应：小屏（手机）宠物与按钮都放大。
 * - 「作者属性」打开属性表单（访客本地热更，不回写宿主）。
 */
function sharePetShow(cfg: WallpaperConfig, orient: ShareOrient): void {
  if (shareHud) return;
  const pet = document.createElement("div");
  pet.id = "share-pet";
  const menu = document.createElement("div");
  menu.id = "share-pet-menu";
  menu.innerHTML = `
  <style>
    #share-pet { position:fixed; z-index:11; width:${PET_SIZE}px; height:${PET_SIZE}px;
      cursor:grab; touch-action:none; user-select:none; -webkit-user-select:none;
      filter:drop-shadow(0 5px 12px rgba(0,0,0,.5));
      transition:left .28s cubic-bezier(.2,.9,.25,1.25), top .28s cubic-bezier(.2,.9,.25,1.25), transform .2s ease; }
    #share-pet.dragging { transition:none; cursor:grabbing; transform:scale(1.12) rotate(-4deg); }
    #share-pet.dragging .pet-eye { transform:scaleY(.55); }
    #share-pet.open { transform:scale(1.06); }
    #share-pet .pet-eye { transform-box:fill-box; transform-origin:center;
      animation:petblink 4.2s ease-in-out infinite; }
    @keyframes petblink { 0%,93%,100% { transform:scaleY(1) } 96% { transform:scaleY(.08) } }
    #share-pet .pet-bob { animation:petbob 3s ease-in-out infinite; transform-box:fill-box; }
    @keyframes petbob { 50% { transform:translateY(-3px) } }
    #share-pet .pet-note { transform-box:fill-box; animation:petnote 2.4s ease-in-out infinite; }
    @keyframes petnote { 0%,100% { transform:translate(0,0); opacity:.5 } 50% { transform:translate(-3px,-6px); opacity:1 } }
    #share-pet.open .ear-l { transform-box:fill-box; transform-origin:bottom center; animation:earwig 1s ease-in-out infinite; }
    #share-pet.open .ear-r { transform-box:fill-box; transform-origin:bottom center; animation:earwig 1s ease-in-out .15s infinite reverse; }
    @keyframes earwig { 0%,100% { transform:rotate(0) } 50% { transform:rotate(7deg) } }
    #share-pet-menu { position:fixed; z-index:11; display:flex; flex-direction:column; gap:10px; }
    #share-pet-menu button { width:${HUD_BTN}px; height:${HUD_BTN}px; border-radius:999px; display:grid; place-items:center;
      background:rgba(16,20,31,.78); border:1px solid rgba(255,255,255,.14); color:#e6edf3;
      cursor:pointer; backdrop-filter:blur(6px); transition:background .15s ease, opacity .18s ease, transform .18s ease; }
    #share-pet-menu button:hover { background:rgba(40,48,66,.9); }
    #share-pet-menu.collapsed button { opacity:0; pointer-events:none; transform:scale(.6) translateY(6px); }
  </style>
  ${petMascotSvg()}
  <button id="hud-orient" title=""></button>
  <button id="hud-fs" title="全屏 Fullscreen"></button>
  <button id="hud-mute" title="声音 Sound"></button>
  <button id="hud-props" title="作者属性 Properties">${hudIcon(SHARE_ICONS.props)}</button>
  <button id="hud-reload" title="重载 Reload">${hudIcon(SHARE_ICONS.reload)}</button>`;
  document.body.append(menu, pet);
  const orientBtn = menu.querySelector("#hud-orient") as HTMLButtonElement;
  const fsBtn = menu.querySelector("#hud-fs") as HTMLButtonElement;
  const muteBtn = menu.querySelector("#hud-mute") as HTMLButtonElement;
  shareHud = { pet, menu, orientBtn, muteBtn, orient, muted: cfg.muted !== false, expanded: false };

  // ---- 摆位：默认右上角；记忆位置 → 钳回视口 → 吸附较近边缘 ----
  const clamp = (v: number, lo: number, hi: number) => Math.max(lo, Math.min(hi, v));
  const place = (x: number, y: number) => {
    pet.style.left = `${clamp(Math.round(x), 10, window.innerWidth - PET_SIZE - 10)}px`;
    pet.style.top = `${clamp(Math.round(y), 10, window.innerHeight - PET_SIZE - 10)}px`;
  };
  const saved = (() => {
    try {
      const p = JSON.parse(localStorage.getItem(SHARE_PET_KEY) || "null") as { x: number; y: number } | null;
      if (p && Number.isFinite(p.x) && Number.isFinite(p.y)) return p;
    } catch { /* 忽略坏数据 */ }
    return null;
  })();
  if (saved) place(saved.x, saved.y);
  else place(window.innerWidth - PET_SIZE - 14, 16);
  const snapToEdge = () => {
    const x = parseFloat(pet.style.left) || 0;
    const y = parseFloat(pet.style.top) || 0;
    const edgeX = x + PET_SIZE / 2 < window.innerWidth / 2 ? 10 : window.innerWidth - PET_SIZE - 10;
    place(edgeX, y);
  };
  const layoutMenu = () => {
    const px = parseFloat(pet.style.left) || 0;
    const py = parseFloat(pet.style.top) || 0;
    const onRight = px + PET_SIZE / 2 >= window.innerWidth / 2;
    const mx = onRight ? px - HUD_BTN - 12 : px + PET_SIZE + 12;
    const mh = menu.querySelectorAll("button").length * (HUD_BTN + 10);
    const my = clamp(py, 10, window.innerHeight - mh - 10);
    menu.style.left = `${Math.round(mx)}px`;
    menu.style.top = `${Math.round(my)}px`;
  };

  // ---- 展开/收起 ----
  const setExpanded = (open: boolean) => {
    if (!shareHud) return;
    shareHud.expanded = open;
    menu.classList.toggle("collapsed", !open);
    pet.classList.toggle("open", open);
    if (open) layoutMenu();
  };
  document.addEventListener("pointerdown", (ev) => {
    const target = ev.target as Node;
    if (shareHud?.expanded && !pet.contains(target) && !menu.contains(target)) setExpanded(false);
  });

  // ---- 拖动：window 级监听（触屏可靠），6px 阈值区分点击 ----
  let drag: { sx: number; sy: number; ox: number; oy: number; moved: boolean } | null = null;
  const onMove = (ev: PointerEvent) => {
    if (!drag || !shareHud) return;
    const dx = ev.clientX - drag.sx;
    const dy = ev.clientY - drag.sy;
    if (!drag.moved && Math.hypot(dx, dy) > 6) {
      drag.moved = true;
      setExpanded(false);
      pet.classList.add("dragging");
    }
    if (drag.moved) {
      ev.preventDefault();
      pet.style.left = `${clamp(drag.ox + dx, 10, window.innerWidth - PET_SIZE - 10)}px`;
      pet.style.top = `${clamp(drag.oy + dy, 10, window.innerHeight - PET_SIZE - 10)}px`;
    }
  };
  const onUp = () => {
    if (!drag) return;
    const wasDrag = drag.moved;
    drag = null;
    pet.classList.remove("dragging");
    if (wasDrag) {
      snapToEdge();
      try {
        localStorage.setItem(SHARE_PET_KEY, JSON.stringify({ x: parseFloat(pet.style.left), y: parseFloat(pet.style.top) }));
      } catch { /* 存不了就本次会话有效 */ }
      if (shareHud?.expanded) layoutMenu();
    } else {
      setExpanded(!shareHud?.expanded);
    }
  };
  pet.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0 && ev.pointerType === "mouse") return;
    ev.preventDefault();
    drag = { sx: ev.clientX, sy: ev.clientY, ox: parseFloat(pet.style.left) || 0, oy: parseFloat(pet.style.top) || 0, moved: false };
  });
  window.addEventListener("pointermove", onMove, { passive: false });
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);

  window.addEventListener("resize", () => {
    snapToEdge();
    if (shareHud?.expanded) layoutMenu();
  });

  // ---- 按钮行为 ----
  const ORIENT_TITLE: Record<ShareOrient, string> = {
    auto: "跟随窗口 · Auto",
    portrait: "竖屏（方向跟随）",
    landscape: "横屏 16:9",
  };
  const orientIcon = (o: ShareOrient) =>
    hudIcon(o === "portrait" ? SHARE_ICONS.portrait : o === "landscape" ? SHARE_ICONS.landscape : SHARE_ICONS.auto);
  orientBtn.innerHTML = orientIcon(orient);
  orientBtn.title = `画幅：${ORIENT_TITLE[orient]}`;
  orientBtn.addEventListener("click", () => {
    if (!shareHud) return;
    const order: ShareOrient[] = ["auto", "portrait", "landscape"];
    shareHud.orient = order[(order.indexOf(shareHud.orient) + 1) % 3];
    try {
      localStorage.setItem(SHARE_ORIENT_KEY, shareHud.orient);
    } catch {
      /* 隐私模式等存不了就本次会话有效 */
    }
    orientBtn.innerHTML = orientIcon(shareHud.orient);
    orientBtn.title = `画幅：${ORIENT_TITLE[shareHud.orient]}`;
    shareApplyOrient(shareHud.orient);
  });

  const fsIcon = () =>
    hudIcon(document.fullscreenElement ? SHARE_ICONS.fsExit : SHARE_ICONS.fullscreen);
  fsBtn.innerHTML = fsIcon();
  fsBtn.addEventListener("click", () => {
    if (document.fullscreenElement) void document.exitFullscreen().catch(() => {});
    else void document.documentElement.requestFullscreen().catch(() => {});
  });
  document.addEventListener("fullscreenchange", () => {
    fsBtn.innerHTML = fsIcon();
  });

  const muteIcon = () => hudIcon(shareHud?.muted ? SHARE_ICONS.soundOff : SHARE_ICONS.soundOn);
  muteBtn.innerHTML = muteIcon();
  muteBtn.addEventListener("click", () => {
    if (!shareHud) return;
    shareHud.muted = !shareHud.muted;
    muteBtn.innerHTML = muteIcon();
    window.__wp && window.__wp.setVolume(shareHud.muted ? 0 : 1);
  });

  menu.querySelector("#hud-reload")?.addEventListener("click", () => location.reload());
  menu.querySelector("#hud-props")?.addEventListener("click", () => {
    void sharePropsPanelToggle();
  });

  // 初始收起
  menu.classList.add("collapsed");
  setExpanded(false);
}

// ---- 作者属性表单（访客本地热更；不回写宿主 —— 分享面是只读的）----

let sharePropsPanel: HTMLDivElement | null = null;

/** wire value 里的可编辑标量（{type,value} 包裹或裸值两种形态） */
function wireScalar(v: unknown): unknown {
  return v && typeof v === "object" ? (v as { value?: unknown }).value : v;
}
/** 改值后按原形态包回去（保持 type 等元信息） */
function wireWrap(orig: unknown, newVal: unknown): unknown {
  return orig && typeof orig === "object"
    ? { ...(orig as Record<string, unknown>), value: newVal }
    : newVal;
}
/** WE 颜色 "R G B"（0..1 浮点）→ #rrggbb */
function colorToHex(rgb: string): string {
  const parts = rgb.trim().split(/\s+/).map(Number);
  if (parts.length < 3 || parts.some((n) => !Number.isFinite(n))) return "#7c8cf8";
  const hex = (n: number) => Math.max(0, Math.min(255, Math.round(n * 255))).toString(16).padStart(2, "0");
  return `#${hex(parts[0])}${hex(parts[1])}${hex(parts[2])}`;
}
function hexToColor(hex: string): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return "0.5 0.5 0.5";
  const n = parseInt(m[1], 16);
  const f = (v: number) => (v / 255).toFixed(5);
  return `${f((n >> 16) & 255)} ${f((n >> 8) & 255)} ${f(n & 255)}`;
}

/** 打开/关闭作者属性面板；首次打开拉取定义并渲染表单 */
async function sharePropsPanelToggle(): Promise<void> {
  if (sharePropsPanel) {
    sharePropsPanel.remove();
    sharePropsPanel = null;
    return;
  }
  const token = audioToken;
  if (!token) return;
  const panel = document.createElement("div");
  panel.id = "share-props";
  panel.style.cssText =
    "position:fixed;z-index:12;top:12px;right:12px;width:min(340px,calc(100vw - 24px));" +
    "max-height:min(76vh,640px);overflow:auto;border-radius:14px;padding:14px 16px;" +
    "background:rgba(13,17,23,.95);border:1px solid rgba(255,255,255,.14);color:#e6edf3;" +
    "font:13px/1.6 ui-sans-serif,system-ui,sans-serif;box-shadow:0 10px 30px rgba(0,0,0,.5);";
  panel.innerHTML = `<div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:8px">
    <div style="font-weight:600">壁纸属性 · Properties</div>
    <button id="share-props-close" style="background:none;border:0;color:#8b949e;font-size:16px;cursor:pointer">✕</button>
  </div><div id="share-props-body" style="color:#8b949e;font-size:12.5px">加载中…</div>`;
  document.body.appendChild(panel);
  sharePropsPanel = panel;
  panel.querySelector("#share-props-close")?.addEventListener("click", () => {
    panel.remove();
    sharePropsPanel = null;
  });
  let defs: Array<Record<string, unknown>> = [];
  try {
    const res = await fetch(`/props-defs/${encodeURIComponent(token)}`);
    defs = res.ok ? ((await res.json()) as Array<Record<string, unknown>>) : [];
  } catch {
    /* 保持空态 */
  }
  const body = panel.querySelector("#share-props-body") as HTMLDivElement;
  const sortable = (v: unknown) => (typeof v === "number" ? v : 0);
  defs.sort((a, b) => sortable(a.order) - sortable(b.order));
  if (!defs.length) {
    body.textContent = "这张壁纸没有可调的作者属性 · No adjustable properties";
    return;
  }
  body.textContent = "";
  body.style.color = "#e6edf3";
  const apply = (name: string, wire: unknown) => {
    window.__wp && window.__wp.updateWebProps({ [name]: { value: wire } });
  };
  for (const d of defs) {
    const name = String(d.name ?? "");
    const ptype = String(d.ptype ?? "other");
    const label = String(d.text ?? "") || name;
    if (ptype === "file" || ptype === "other" || ptype === "text") continue; // 访客侧无法给文件/无控件
    const row = document.createElement("div");
    row.style.cssText = "padding:9px 0;border-bottom:1px solid rgba(255,255,255,.08)";
    const labelEl = document.createElement("div");
    labelEl.textContent = label;
    labelEl.style.cssText = "font-size:12.5px;color:#cbd5e1;margin-bottom:6px;white-space:pre-line";
    row.appendChild(labelEl);
    const orig = d.value;
    const scalar = wireScalar(orig);
    const inputStyle =
      "width:100%;box-sizing:border-box;background:rgba(255,255,255,.08);border:1px solid rgba(255,255,255,.14);" +
      "border-radius:8px;color:#e6edf3;padding:5px 8px;font-size:12.5px;outline:none;accent-color:#8b9cf9";
    if (ptype === "bool") {
      const on = scalar === true;
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = on;
      cb.style.cssText = "width:16px;height:16px;accent-color:#8b9cf9;cursor:pointer";
      cb.addEventListener("change", () => apply(name, wireWrap(orig, cb.checked)));
      row.appendChild(cb);
    } else if (ptype === "color") {
      const str = typeof scalar === "string" ? scalar : "0.5 0.5 0.5";
      const picker = document.createElement("input");
      picker.type = "color";
      picker.value = colorToHex(str);
      picker.style.cssText = "width:100%;height:32px;background:none;border:1px solid rgba(255,255,255,.14);border-radius:8px;cursor:pointer;padding:2px";
      picker.addEventListener("input", () => apply(name, wireWrap(orig, hexToColor(picker.value))));
      row.appendChild(picker);
    } else if (ptype === "slider") {
      const val = typeof scalar === "number" ? scalar : Number(scalar) || 0;
      const min = typeof d.min === "number" ? d.min : 0;
      const max = typeof d.max === "number" ? d.max : 1;
      const step = typeof d.step === "number" && d.step > 0 ? d.step : 0.01;
      const wrap = document.createElement("div");
      wrap.style.cssText = "display:flex;align-items:center;gap:8px";
      const range = document.createElement("input");
      range.type = "range";
      range.min = String(min);
      range.max = String(max);
      range.step = String(step);
      range.value = String(val);
      range.style.cssText = "flex:1;accent-color:#8b9cf9;cursor:pointer";
      const num = document.createElement("span");
      num.textContent = String(val);
      num.style.cssText = "font:11.5px ui-monospace,monospace;color:#8b949e;min-width:34px;text-align:right";
      range.addEventListener("input", () => {
        const v = Number(range.value);
        num.textContent = String(v);
        apply(name, wireWrap(orig, v));
      });
      wrap.append(range, num);
      row.appendChild(wrap);
    } else if (ptype === "combo" && Array.isArray(d.options) && d.options.length) {
      const sel = document.createElement("select");
      sel.style.cssText = inputStyle;
      for (const opt of d.options as Array<Record<string, unknown>>) {
        const ov = opt.value;
        const ovScalar = wireScalar(ov);
        const optEl = document.createElement("option");
        optEl.value = JSON.stringify({ raw: ovScalar });
        optEl.textContent = String(opt.label ?? String(ovScalar));
        if (JSON.stringify(ovScalar) === JSON.stringify(scalar)) optEl.selected = true;
        sel.appendChild(optEl);
      }
      sel.addEventListener("change", () => {
        try {
          const { raw } = JSON.parse(sel.value) as { raw: unknown };
          apply(name, wireWrap(orig, raw));
        } catch { /* 忽略 */ }
      });
      row.appendChild(sel);
    } else if (ptype === "textinput") {
      const inp = document.createElement("input");
      inp.type = "text";
      inp.value = String(scalar ?? "");
      inp.style.cssText = inputStyle;
      inp.addEventListener("change", () => apply(name, wireWrap(orig, inp.value)));
      row.appendChild(inp);
    } else {
      continue;
    }
    body.appendChild(row);
  }
}

// ---- 分享访客的属性实时订阅（宿主改 props → SSE → updateWebProps 热更）----

function shareSubscribeProps(token: string | null): void {
  if (!token) return;
  const es = new EventSource(`/props-events/${encodeURIComponent(token)}`);
  es.addEventListener("props", (ev) => {
    try {
      const wire = JSON.parse((ev as MessageEvent).data) as Record<
        string,
        { value: unknown }
      >;
      window.__wp && window.__wp.updateWebProps(wire);
    } catch {
      /* 坏帧忽略 */
    }
  });
  // 断线重连由 EventSource 自管；不再消费的连接随页面关闭释放
}

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
// 内容服务器以 ~30Hz 通过 SSE 推送 64 段频谱（CoreAudio 进程 Tap / WASAPI 系统 loopback → FFT）。
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
      // scene.pkg 逐个回退，project.json 声明的入口最优先（主 pkg 命名/布局
      // 不规范的工程只有它指得到）。每次 fetch 单独 try/catch：Tauri 自定义
      // 协议对不存在的路径抛 TypeError 而非返回 404，逐个 try 才不会一抛就整场失败
      async scenePkg(signal) {
        const paths = [cfg.scenePkg, "scene.pkg", "scenes/scene.pkg", "gifscene.pkg"].filter(
          (p): p is string => typeof p === "string" && p.length > 0,
        );
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

/**
 * 本壁纸的目标音量（0..1）。cfg.volume 是播放设置里按壁纸记忆的精确值；
 * 旧会话没有它时回落 muted 布尔（false=1 / true=0）。
 */
function targetVolume(cfg: WallpaperConfig): number {
  const v = cfg.volume;
  if (typeof v === "number" && Number.isFinite(v)) return Math.min(1, Math.max(0, v));
  return cfg.muted === false ? 1 : 0;
}

/**
 * 对刚就绪（或刚重挂/恢复）的实例应用本壁纸的目标音量。
 *
 * 挂载/重挂一律先静音起播（mountOpts.volume = 0），完全加载后再由这里起音量：
 *   - 静音壁纸在任何时刻都不出声（此前唤醒重挂/无缝循环兜底路径会先按元素
 *     默认音量出一段声，再等宿主补 setVolume 才安静）；
 *   - 非静音壁纸也不会在加载途中以错误音量（元素默认 1.0）抢跑。
 * 静音视频例外：元素创建时就是静音态（mountOpts.volume=0），而库对视频的
 * setVolume(0) 会触发 WebCodecs 路径整段重挂（昂贵且无声可纠），跳过不补。
 */
function applyWallpaperVolume(inst: SceneInstance) {
  const v = targetVolume(state.cfg);
  if (v <= 0 && state.cfg.type === "video") return;
  try {
    inst.setVolume(v);
  } catch {
    /* 某些类型的实例在特定阶段可能拒绝 setVolume，静默即可 */
  }
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
    reportDiag(cfg, "failed: 缺少 src/mediaBase，降级到默认壁纸");
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
        // 音量策略：挂载一律 0（静音）起播，加载完成后由 applyWallpaperVolume
        // 按本壁纸的设置值起音量（见其注释）——不能在这里直接给目标音量
        volume: 0,
        // 渲染质量档位（库 1.3.23+）：键缺省/非法值由库 normalizeQuality 落默认
        quality: {
          antiAliasing: cfg.aa as QualityOptions["antiAliasing"],
          particles: cfg.particles as QualityOptions["particles"],
          postProcessing: cfg.postProcessing as QualityOptions["postProcessing"],
        },
        // 视频纹理上传倍率（0 = 自动，交给库内帧率守门）
        videoTexScale: cfg.videoTexScale ?? 0,
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
        // 报 `failed:`：无缝切换据此中止替换、把旧壁纸留在屏上（别换上一张空白）。
        reportDiag(cfg, `failed: mount 超过 ${MOUNT_HARD_MS / 1000}s 仍未完成（疑似库内部卡住）`);
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
      // 壁纸已完全加载（mount 等到首帧才返回）：此刻起音量。
      // 放在 pause() 之后 —— setVolume 的取消静音路径不该把暂停中的壁纸播响
      applyWallpaperVolume(inst);
      // SSE 常在 mount 之后才首次收到帧；库的音频泵逐帧选源，此时补装也生效。
      // 每次挂载都要重来一遍 —— 切壁纸会换新实例，旧实例上的音频源不会继承
      attachSystemAudio(inst, seq);
      attachSystemMedia(inst, seq);
      reportDiag(cfg, "ready");
      reveal();
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
  // canvas 是正式壁纸类型（测试面板）：与库路径同样报 ready + 渐入显形
  reportDiag(state.cfg, "ready");
  reveal();
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
  // 降级页只是「别让窗口空着」的兜底，**不报 ready**（无缝切换据此把旧壁纸
  // 留在屏上，而不是换成一张错误占位图）；但仍要显形，本窗口没有旧壁纸可看时可见。
  reveal();
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
      /** 视频纹理上传倍率热更（0 = 自动交给守门，>0 固定；就地生效不重挂载） */
      setVideoTexScale(scale: number): void;
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
      /**
       * 外部滚轮 / 触控板手势注入（只有 web 壁纸消费；scene 静默无效）。
       * 原生侧已把方向对齐 WheelEvent（dy 正=向下；双指捏合=ctrl 位 mods bit0）。
       * 位置沿用最后一次 pushPointer 的坐标。
       */
      pushWheel(dx: number, dy: number, mode?: number, mods?: number): void;
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
    // 先落音量再放行播放：恢复会重放媒体（场景 BGM / 网页 shim 解冻），虽然
    // 库侧已保证 muted 跨重挂存活，这里仍按「先静音后出声」的纪律再兜一层，
    // 避免恢复路径里任何一环以默认音量抢跑
    if (state.inst) applyWallpaperVolume(state.inst);
    state.inst?.resume();
    if (state.cfg.type === "canvas") startCanvasLoop();
  },
  setFit(fit: string) {
    state.cfg.fit = fit as WallpaperFit;
    state.inst?.setFit(normalizeFit(fit as WallpaperFit));
  },
  setVolume(volume: number) {
    const v = Math.max(0, Math.min(1, volume));
    // 记住精确值（重挂后按它起音量）；muted 同步保持旧字段一致
    state.cfg.volume = v;
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
      // 重挂会重建媒体元素（音量回元素默认值）：先静音起播的纪律在这里补一次
      applyWallpaperVolume(state.inst);
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
      // 库内部重挂重建了媒体元素，音量同样要补
      applyWallpaperVolume(state.inst);
      return;
    }
    mount(state.cfg);
  },
  // 调整帧率上限
  setVideoTexScale(scale: number) {
    state.cfg.videoTexScale = scale;
    state.inst?.setVideoTexScale?.(scale);
  },
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
  pushWheel(dx: number, dy: number, mode = 0, mods = 0) {
    state.inst?.pushWheel(dx, dy, mode, mods);
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
  // 0/缺省 = 自动（守门接管）；正数 = 固定倍率
  videoTexScale: Number(params.get("vidscale")) || 0,
  filter: params.get("filter") ?? undefined,
  // 渲染质量档位（query 键 aa/pq/pp → cfg 字段 aa/particles/postProcessing）
  aa: params.get("aa") ?? undefined,
  particles: params.get("pq") ?? undefined,
  postProcessing: params.get("pp") ?? undefined,
  // 精确音量（0..1）：有壁纸专属记录时随 query 下发；缺省回落 muted 布尔
  volume: params.get("volume") != null ? Number(params.get("volume")) : undefined,
  muted: params.get("muted") !== "false",
  loop: params.get("loop") !== "false",
  mediaBase: params.get("mediaBase") ?? undefined,
  scenePkg: params.get("scenePkg") ?? undefined,
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

// 分享域挂载走 bootstrap（加载层 + 主资源预取）；桌面壁纸窗口直接挂载，
// 零额外开销
if (isShareMount(initialCfg)) void bootstrapShareMount(initialCfg);
else mount(initialCfg);

// 页面卸载兜底：预览 iframe 关闭 / 壁纸窗口销毁时释放 WebGL 上下文与 SSE 连接
const teardown = () => {
  systemAudio.close();
  systemMedia.close();
  clear();
};
window.addEventListener("pagehide", teardown);
window.addEventListener("beforeunload", teardown);


export {};
