// 主窗口里的**离屏预览**：把一张壁纸挂在一张看不见的画布上渲染，抓帧回传宿主。
//
// 为什么预览放在主窗口、而不是另开一扇窗：macOS 上「不可见的窗口」会被 WebKit 判定为
// 遮挡（occlusionState 没有 visible 位）而**停帧** —— 实测预览窗把 scene.pkg 全部解析
// 完、贴图也上传了，却永远等不到首帧 ready（诊断停在 mount 中途）。而主窗口本来就是
// 可见且在跑的页面，画布里跑的渲染循环照常走，把它们放进一个屏外/透明的容器里，
// 用户什么也看不到，三平台代码完全一致。
//
// 与渲染器页（`renderer/src/main.ts`）的分工：那边是「桌面壁纸窗口」，要处理音频、
// 无缝切换、属性热更等一整套宿主适配；这边只求**把画面画出来看一眼**，所以用最小
// 挂载（source + fit + 画质 + 用户属性覆盖），牺牲的一点保真度换来的是零窗口管理。
import { api, type WallpaperConfig, type WebPropValues } from "../api/steam";

type PreviewInstance = {
  destroy?: (opts?: { releasePkgCache?: boolean }) => void;
  canvas?: HTMLCanvasElement;
};

let host: HTMLDivElement | null = null;
let inst: PreviewInstance | null = null;
/** 装载序号：连发两次 start 时，前一次晚到的实例必须被丢弃并销毁（否则泄漏 WebGL 上下文） */
let seq = 0;
/**
 * 空闲自动释放定时器。
 *
 * `keepOpen:true` 是给调试用的：宿主不会主动 stop，那份 WebGL 上下文 + 解析好的
 * pkg 缓存就**一直挂在主窗口里**。所以这里自带一道保险 —— 一段时间没有抓帧请求就
 * 自行收摊，避免「调试完忘了关」把内存占住。
 */
let idleTimer: number | null = null;
const IDLE_RELEASE_MS = 180_000;

function armIdleRelease() {
  if (idleTimer !== null) window.clearTimeout(idleTimer);
  idleTimer = window.setTimeout(() => {
    if (!inst && !host) return;
    diag(`空闲 ${IDLE_RELEASE_MS / 1000}s 无抓帧请求，自动释放预览`);
    teardown();
  }, IDLE_RELEASE_MS);
}

function clearIdleRelease() {
  if (idleTimer !== null) {
    window.clearTimeout(idleTimer);
    idleTimer = null;
  }
}
/** 当前挂载的完成信号：宿主抓帧要等它（失败也要让它落定，否则抓帧会干等） */
let mounting: Promise<void> | null = null;
let lastError: string | null = null;

/** 内容服务器基址（抓帧回传送这里；主窗口与它跨源，所以走绝对地址 + 简单请求） */
async function contentBase(): Promise<string> {
  const st = await api.contentServerStatus();
  return st.base.replace(/\/$/, "");
}

let diagBase: string | null = null;
/**
 * 预览链路自己的诊断上报。
 *
 * 主窗口原本**没有**任何诊断通道（/diag 一直只有渲染器页在用），于是「预览不出图」
 * 在宿主侧只能看到一个超时，分不清是「指令没到页面」「挂载失败」还是「回传失败」。
 * 这里按 `win=main` 分档上报，renderer_diag 工具能直接看到。
 */
function diag(msg: string) {
  const send = (base: string) => {
    void fetch(
      `${base}/diag?msg=${encodeURIComponent(`[preview] ${msg}`)}&win=main`,
      { cache: "no-store" },
    ).catch(() => {});
  };
  if (diagBase) return send(diagBase);
  void contentBase()
    .then((b) => {
      diagBase = b;
      send(b);
    })
    .catch(() => {});
}

function teardown() {
  clearIdleRelease();
  if (inst?.destroy) {
    try {
      inst.destroy({ releasePkgCache: true });
    } catch {
      /* 忽略 */
    }
  }
  inst = null;
  host?.remove();
  host = null;
  mounting = null;
}

/**
 * 开始挂载一张壁纸。`itemId` 是本地库条目 id（`library_preview` 拿它的渲染配置）。
 * 重复调用会先收掉上一次。
 */
async function start(itemId: string, width: number, height: number): Promise<void> {
  diag(`start item=${itemId} ${width}x${height}`);
  teardown();
  lastError = null;
  const wrap = document.createElement("div");
  // 屏外 + 不可交互：用户看不到（页面上真的没有它的位置），但页面可见 → rAF 照跑
  wrap.style.cssText = [
    "position:fixed",
    "left:-100000px",
    "top:0",
    `width:${Math.max(64, Math.round(width))}px`,
    `height:${Math.max(64, Math.round(height))}px`,
    "overflow:hidden",
    "pointer-events:none",
    "z-index:-1",
  ].join(";");
  document.body.appendChild(wrap);
  host = wrap;

  const mySeq = ++seq;
  armIdleRelease();
  mounting = (async () => {
    // 按需加载：主界面平时不该背渲染库这一兆（构建产物里是独立 chunk）
    const t0 = Date.now();
    diag("按需加载渲染库…");
    const { mount, httpSource } = await import("webwallgl");
    diag(`渲染库就绪（${Date.now() - t0}ms），取条目配置…`);
    const cfg: WallpaperConfig = await api.libraryPreview(itemId);
    if (!cfg?.src || !cfg.mediaBase) {
      throw new Error("该条目没有可预览的资源（mediaBase/src 缺失）");
    }
    if (cfg.type === "web") {
      throw new Error("web 类型是独立 iframe，画布抓不到；请用应用内预览或 wallpaper_screenshot");
    }
    // 用户属性覆盖：预览要反映「用户调过的样子」，否则看到的永远是最初的默认值
    diag(`配置就绪（type=${cfg.type}），取用户属性…`);
    let properties: WebPropValues | undefined;
    try {
      const defs = await api.libraryItemProps(itemId);
      const flat: WebPropValues = {};
      for (const d of defs as { name?: string; value?: WebPropValues[string] }[]) {
        if (d?.name && d.value != null) flat[d.name] = d.value;
      }
      if (Object.keys(flat).length) properties = flat;
    } catch {
      /* 属性读不到不影响预览（用场景默认值） */
    }
    const source = httpSource(`${cfg.mediaBase}/${cfg.src}`);
    diag("开始挂载（等首帧）…");
    const mounted = (await mount(wrap, {
      source,
      fit: (cfg.fit as "cover" | "contain" | "stretch") ?? "cover",
      renderDpr: 1,
      // 预览只为看一眼：帧率压到 30、音量静音（字段名以库的 MountOptions 为准）
      fps: 30,
      volume: 0,
      autoplay: true,
      ...(properties ? { properties } : {}),
    })) as PreviewInstance;
    // 期间又来了新的 start：把这份晚到的实例就地销毁，别抢走 inst
    if (mySeq !== seq) {
      try {
        mounted.destroy?.({ releasePkgCache: false });
      } catch {
        /* 忽略 */
      }
      diag("挂载完成但已被新的预览取代，已销毁");
      return;
    }
    inst = mounted;
    diag(`挂载完成（画布 ${mounted?.canvas?.width ?? 0}x${mounted?.canvas?.height ?? 0}）`);
  })().catch((e) => {
    if (mySeq !== seq) return; // 过期的那次失败不该覆盖当前预览的状态
    lastError = String((e as Error)?.message || e);
    diag(`挂载失败: ${lastError}`);
  });
  return mounting;
}

/**
 * 抓帧（宿主经 `__wpCapture` 调）。等挂载落定再抓：挂载失败/没画布都回一句原因。
 * 与渲染器页共用同一套回传协议（`POST /capture?req=…&mime=…`，失败 `&error=…`）。
 */
async function capture(req: string, maxWidth?: number): Promise<void> {
  diag(`收到抓帧请求 ${req}（maxWidth=${maxWidth ?? 0}）`);
  // 每次抓帧都续期：只要还在用，就不自动释放
  armIdleRelease();
  let base = "";
  try {
    base = await contentBase();
  } catch {
    diag("拿不到内容服务器基址，放弃回传");
    return; // 连基址都拿不到，宿主那边会超时并给出提示
  }
  const fail = (why: string) => {
    diag(`抓帧失败: ${why}`);
    void fetch(`${base}/capture?req=${encodeURIComponent(req)}&error=${encodeURIComponent(why)}`, {
      method: "POST",
      cache: "no-store",
    }).catch(() => {});
  };
  try {
    if (mounting) await mounting;
    if (lastError) return fail(lastError);
    const src = inst?.canvas ?? host?.querySelector("canvas") ?? null;
    const w = src?.width ?? 0;
    const h = src?.height ?? 0;
    if (!src || !w || !h) return fail("预览画布还没画出来（可能还在加载贴图）");
    const limit = maxWidth && maxWidth > 0 ? maxWidth : 0;
    const out = document.createElement("canvas");
    const scale = limit && w > limit ? limit / w : 1;
    out.width = Math.max(1, Math.round(w * scale));
    out.height = Math.max(1, Math.round(h * scale));
    const ctx = out.getContext("2d");
    if (!ctx) return fail("离屏画布不可用");
    ctx.drawImage(src, 0, 0, out.width, out.height);
    const blob = await new Promise<Blob | null>((resolve) =>
      out.toBlob((b) => resolve(b), "image/jpeg", 0.85),
    );
    if (!blob) return fail("编码 JPEG 失败");
    diag(`回传 ${blob.size} 字节（${out.width}x${out.height}）`);
    await fetch(`${base}/capture?req=${encodeURIComponent(req)}&mime=image/jpeg`, {
      method: "POST",
      // text/plain = CORS 简单请求（跨源不触发预检），真实类型走 query
      headers: { "content-type": "text/plain" },
      body: blob,
      cache: "no-store",
    });
  } catch (e) {
    fail(`预览抓帧失败: ${String((e as Error)?.message || e)}`);
  }
}

declare global {
  interface Window {
    /** 宿主控制面：开始/结束一张离屏预览 */
    __wpPreview?: {
      start: (itemId: string, width: number, height: number) => void;
      stop: () => void;
    };
    /** 宿主抓帧入口（与渲染器页同名同协议） */
    __wpCapture?: (req: string, maxWidth?: number) => void;
  }
}

/** 注册控制面（应用启动时调一次） */
export function installPreviewBridge(): void {
  window.__wpPreview = {
    start: (itemId, width, height) => {
      void start(itemId, width, height);
    },
    stop: () => teardown(),
  };
  window.__wpCapture = (req, maxWidth) => {
    void capture(req, maxWidth);
  };
}
