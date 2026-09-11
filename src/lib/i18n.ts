// 多语言运行时（i18n）：语言状态 + 取词 + 后端消息翻译
//
// 设计取舍 —— **中文原文当键**：
//   这套界面是先有中文、后加多语言的，文案量在千条量级。用「中文原文 → 其它语言」
//   的映射表，而不是给每条文案发明一个 key，换来三件事：
//     1. 迁移成本低且不容易错：调用点写 `tr("下载")`，漏掉的地方照样显示中文，
//        不会出现 key 拼错导致的空白文案；
//     2. 新增语言 = 新增一个「中文 → 该语言」的表（见 src/locales/）；
//     3. 中文是默认语言，查表直接短路（identity），零开销、零维护。
//   代价：改中文原文会让对应译文失效（自动退回中文，不会崩）。对单人项目
//   这个取舍划算；真要严格化，把 src/locales/en-US.ts 的键换成语义 key 即可。
//
// 两个入口：
//   tr(中文, 参数?)  —— 界面文案，参数用 `{name}` 占位
//   trMsg(字符串)    —— **后端消息**（Rust 返回的错误/状态文本）。后端为了能带
//                      运行时数值，用的是 format! 拼接，前端拿到的已经是成品句子，
//                      所以这里按「中文模板 + 占位符」做匹配翻译（见 BACKEND_CATALOG）。
//                      匹配不上就原样返回中文 —— 宁可露出中文，也不要吞掉错误。
import { useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { EN_US, EN_US_BACKEND } from "../locales/en-US";

export type Locale = "zh-CN" | "en-US";

/** 可选语言。标签用各自的语言写（不翻译）—— 语言名要能被不懂当前语言的人认出来 */
export const LOCALES: { value: Locale; label: string }[] = [
  { value: "zh-CN", label: "简体中文" },
  { value: "en-US", label: "English" },
];

const STORAGE_KEY = "we.locale";

export function isLocale(v: unknown): v is Locale {
  return LOCALES.some((l) => l.value === v);
}

/** 界面文案表。中文是原文（identity），这里只放其它语言 */
const UI_CATALOGS: Partial<Record<Locale, Record<string, string>>> = {
  "en-US": EN_US,
};

/**
 * 首次启动跟随系统语言：中文环境给中文，其余给英文。
 * 只作为**初始默认值** —— 用户在设置里选过就永远听用户的，不再跟随系统
 * （系统语言变了不该把用户选好的语言改掉）。
 */
function detectSystemLocale(): Locale {
  try {
    const langs = navigator.languages?.length ? navigator.languages : [navigator.language];
    for (const l of langs) {
      if (!l) continue;
      if (/^zh\b/i.test(l)) return "zh-CN";
      if (/^(en|de|fr|es|pt|it|nl|ru|ja|ko)\b/i.test(l)) return "en-US";
    }
  } catch {
    /* ignore */
  }
  return "zh-CN";
}

function readInitial(): Locale {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (isLocale(raw)) return raw;
  } catch {
    /* ignore */
  }
  return detectSystemLocale();
}

let current: Locale = readInitial();
const listeners = new Set<() => void>();

// 模块加载即对齐 <html lang>：语言影响字体回退与断行，越早设越少闪烁
applyDocumentLang();

// 多窗口同源（主窗口 + 独立「壁纸设置」窗口）时，别的窗口改了 localStorage
// 会派发 storage 事件 —— 跟着切，否则在设置里换了语言，已经开着的设置窗口还是旧语言。
// 只认自己的键，并且忽略 nativeEvent 缺失（非浏览器环境）。
try {
  window.addEventListener("storage", (e) => {
    if (e.key !== STORAGE_KEY || !isLocale(e.newValue) || e.newValue === current) return;
    current = e.newValue;
    applyDocumentLang();
    for (const cb of listeners) cb();
  });
} catch {
  /* 非浏览器环境（测试/SSR）忽略 */
}

export function getLocale(): Locale {
  return current;
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  return () => listeners.delete(cb);
}

/**
 * 订阅当前语言。**在整棵 UI 的根组件里调一次**即可（App 的 Shell / 壁纸设置窗口）：
 * 根组件重渲染会带着整棵树一起重渲染，页面里的 `tr()` 才会重新求值。
 * 子组件不必各自订阅 —— 本项目没有用 React.memo 截断渲染。
 */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribe, getLocale, getLocale);
}

/** 切换语言：落盘 → 同步 <html lang> → 通知 UI → 推给后端（托盘/系统弹窗用） */
export function setLocale(next: Locale): void {
  if (!isLocale(next) || next === current) return;
  current = next;
  try {
    localStorage.setItem(STORAGE_KEY, next);
  } catch {
    /* ignore */
  }
  applyDocumentLang();
  for (const cb of listeners) cb();
  pushLocaleToBackend();
}

/** 语言会影响断行/字体回退，<html lang> 必须跟着走 */
export function applyDocumentLang(): void {
  try {
    document.documentElement.lang = current;
  } catch {
    /* ignore */
  }
}

/**
 * 把语言推给 Rust：托盘菜单 / 原生文件选择框 / 系统通知这些不经前端的文案
 * 由后端自己按同一个语言取词。启动时也要推一次（后端默认跟随系统，可能不一致）。
 */
export function pushLocaleToBackend(): void {
  invoke("app_set_locale", { locale: current }).catch(() => {
    /* 非 Tauri 环境（纯浏览器调试）忽略 */
  });
}

export type TrParams = Record<string, string | number>;

/** 取界面文案。`{name}` 占位；`|` 分隔单复数（`"{n} 张|{n} 张壁纸"`，按 params.n 选） */
export function tr(source: string, params?: TrParams): string {
  const table = UI_CATALOGS[current];
  const tpl = table?.[source] ?? source;
  return interpolate(plural(tpl, params), params);
}

function plural(tpl: string, params?: TrParams): string {
  const raw = params?.n;
  // 允许 n 以字符串传入（调用点常用 toLocaleString() 加千分位），
  // 取其中的数字判断单复数：`共 1 个结果` / `共 12 个结果` 都要正确
  const n = typeof raw === "number" ? raw : Number(String(raw ?? "").replace(/[^\d.-]/g, ""));
  if (Number.isFinite(n) && tpl.includes("|")) {
    const [one, other] = tpl.split("|");
    return n === 1 ? one : other;
  }
  return tpl;
}

function interpolate(tpl: string, params?: TrParams): string {
  if (!params) return tpl;
  return tpl.replace(/\{(\w+)\}/g, (m, k: string) =>
    params[k] === undefined ? m : String(params[k]),
  );
}

// ---------------------------------------------------------------- 后端消息

type BackendPattern = { re: RegExp; to: string; names: string[] };
let backendPatterns: BackendPattern[] | null = null;
let backendLocale: Locale | null = null;

/**
 * 中文模板 → 正则：`{}` / `{name}` 都当作「捕获任意内容」。
 * Rust 的格式说明符（`{:?}`、`{:.1}`、`{:.0}`）同样是「这里会填东西」，一并当占位符 ——
 * 不认它们的话这些句子永远匹配不上，等于没翻。译文里写 `{}` 即可（说明符只是渲染细节）。
 * 例：`拷贝 {name} 失败: {err}` → /^拷贝 (.+?) 失败: (.+?)$/
 */
const PLACEHOLDER = /\{(\w*)(?::[^}]*)?\}/g;

function compileBackend(): BackendPattern[] {
  const out: BackendPattern[] = [];
  for (const [zh, en] of Object.entries(EN_US_BACKEND)) {
    if (!zh.includes("{")) continue; // 纯静态消息走精确匹配，不必进正则
    const names: string[] = [];
    let src = "^";
    let last = 0;
    for (const m of zh.matchAll(PLACEHOLDER)) {
      src += escapeRe(zh.slice(last, m.index)) + "(.+?)";
      names.push(m[1] || `_${names.length + 1}`);
      last = (m.index ?? 0) + m[0].length;
    }
    src += escapeRe(zh.slice(last)) + "$";
    out.push({ re: new RegExp(src), to: en, names });
  }
  // 长模板优先：更具体的原文先匹配，避免短模板抢走长句
  out.sort((a, b) => b.names.length - a.names.length || b.re.source.length - a.re.source.length);
  return out;
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

/**
 * 翻译后端消息。三级降级：精确命中 → 模板匹配（带数值的句子）→ 原样返回。
 * 中文语言下直接短路（后端本来就是中文）。
 */
export function trMsg(raw: string): string {
  return trMsgDeep(raw, 0);
}

/**
 * 深一层是为了**嵌套消息**：后端常把一条消息拼进另一条
 * （`拷贝 {} 失败: {e}`、`{method} 请求失败: {e}`、`下载 {} 失败：{}`），
 * 捕获到的片段本身可能还是中文，得再翻一次。限制深度避免病态自引用。
 */
function trMsgDeep(raw: string, depth: number): string {
  if (current === "zh-CN") return raw;
  const msg = raw.trim();
  if (!msg) return raw;
  // 先查界面文案表（调用点可能把状态值或后端字符串直接交给这里渲染），
  // 再回落到后端专属表 —— 两张表都不命中就原样返回中文
  const staticHit = UI_CATALOGS[current]?.[msg] ?? EN_US_BACKEND_EXACT[msg];
  if (staticHit) return staticHit;
  if (!backendPatterns || backendLocale !== current) {
    backendPatterns = compileBackend();
    backendLocale = current;
  }
  for (const p of backendPatterns) {
    const m = p.re.exec(msg);
    if (!m) continue;
    const values = m.slice(1);
    let idx = 0;
    // 捕获组按「原文模板里的占位符名」回填到译文的同名占位符；
    // 译文里写 `{}` 则按出现顺序回填（中英占位符顺序通常一致）
    return p.to.replace(/\{(\w*)\}/g, (whole, name: string) => {
      const v = name ? values[p.names.indexOf(name)] : values[idx];
      idx += 1;
      if (v === undefined) return whole;
      return depth < 3 ? trMsgDeep(v, depth + 1) : v;
    });
  }
  return raw;
}

/** 静态后端消息 → 英文（无占位符的那些，精确匹配即可） */
const EN_US_BACKEND_EXACT: Record<string, string> = Object.fromEntries(
  Object.entries(EN_US_BACKEND).filter(([k]) => !k.includes("{")),
);

/** 供测试/自检：当前语言的文案表 */
export function catalogFor(locale: Locale): Record<string, string> {
  return locale === "zh-CN" ? {} : (UI_CATALOGS[locale] ?? {});
}
