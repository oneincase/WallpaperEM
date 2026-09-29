// 插件目录（前端清单 + 合并/筛选/排序）。
//
// 插件有三种来源，最终都归一成同一个 `PluginEntry` 形状，页面只渲染一套卡片：
//   1. **内置**（`source: "builtin"`）：由本应用实现，点开 = 走 Rust 宿主命令；
//   2. **GitHub 话题**（`market: "github"`）：市场里唯一的来源 —— `topic:wem-plugin`
//      的仓库搜索结果（Rust `plugin_market_search`），安装 = 拉该仓库根目录的
//      `wem-plugin.json` 落地；
//   3. **已安装**（`market: "local"`）：Rust 从 `<appData>/plugins/` 扫出来的清单。
//
// 已安装的第三方插件由 Rust 从 `<appData>/plugins/*/wem-plugin.json` 扫出来
// （`market: "local"`），所以**装/卸/手放一个目录都立刻生效** —— 这就是热插拔：
// 插件是声明式清单，不是代码，增删一个目录不需要重启应用。
//
// `open` 决定点开时的落点：
//   - `"dsh"`：内置插件的宿主命令（准备环境 + 开应用内窗口）；
//   - `"external"`：系统浏览器新窗口（外部站点没有我们的窗口控制与导航条）；
//   - `"window"`：应用内新窗口（清单显式要求时）。
//
// 文案都用中文原文写，渲染时统一走 `tr()`（键在 src/locales/en-US.ts）。
import { tr } from "./i18n";

/** 分类词表（筛选条按这个顺序出胶囊）；「其他」是兜底，清单里的未知分类都归它 */
export const PLUGIN_CATEGORIES = ["AI 助手", "壁纸资源", "创作工具", "其他"] as const;
export type PluginCategory = (typeof PLUGIN_CATEGORIES)[number];

/** 内置 = 本应用自己实现并托管；第三方 = 外部站点/包 */
export type PluginSource = "builtin" | "third";
/** 点开插件时的落点 */
export type PluginOpen = "dsh" | "external" | "window";
/** 条目的来源渠道：市场搜索命中 / 已装到本机 */
export type PluginMarket = "github" | "local";

export interface PluginEntry {
  id: string;
  name: string;
  /** 卡片上的一句话（截两行） */
  summary: string;
  /** 展开后的完整说明 */
  description: string;
  source: PluginSource;
  category: PluginCategory;
  author: string;
  version?: string;
  /** 列表图标：emoji —— 比再画一套 SVG 便宜，也不会和壁纸缩略图抢注意力 */
  icon: string;
  open: PluginOpen;
  /** 入口类型：`url`（打开地址，缺省）| `dsh`（往 DeepSeek Harness profile 装包） */
  kind?: "url" | "dsh";
  /** 点开的目标地址（`dsh` 型为空） */
  url?: string;
  /** `dsh` 型要装进 profile 的 npm 包名 */
  packages?: string[];
  /** 项目主页 / 仓库页 */
  homepage?: string;
  market?: PluginMarket;
  /** 第三方插件是否已落地到本机（内置插件恒为 true） */
  installed?: boolean;
  /** GitHub 条目的清单地址（安装时拉这个） */
  manifestUrl?: string;
  /** GitHub 星数（本地已安装条目没有；要更新得回市场查） */
  stars?: number;
  /** GitHub 最近更新时间（ISO） */
  updatedAt?: string;
  archived?: boolean;
  /** 已安装插件的本地目录 */
  dir?: string;
  /** 清单协议版本（宿主只认 ≤ 自己的版本） */
  schemaVersion?: number;
  /** 清单声明需要的能力：open-url / open-window */
  capabilities?: string[];
  /** 清单要求的最低应用版本 */
  minAppVersion?: string;
  /** 当前应用版本是否满足 minAppVersion（false = 可装不可开） */
  compatible?: boolean;
  /** 宿主判定的特权标记（清单声明了 dsh-profile）；随包官方条目没有这个字段，走本地判定 */
  privileged?: boolean;
}

/** 能力 → 界面文案的键（见 docs/plugin-protocol.md 的能力矩阵） */
export const CAPABILITY_LABELS: Record<string, string> = {
  "open-url": "打开网页",
  "open-window": "应用内窗口",
  "dsh-profile": "装进 DeepSeek Harness",
};

/**
 * 特权能力：声明它的插件会往 DeepSeek Harness 的 profile 里装第三方包 —— 那些包是
 * 会被 dsh 执行的代码。界面必须显式提示并让用户确认（协议规则 R12）。
 */
export const PRIVILEGED_CAPABILITIES = new Set(["dsh-profile"]);

export function isPrivileged(p: PluginEntry): boolean {
  // 已安装条目带宿主的判定；随包官方条目只有清单数据，本地按同一份白名单判
  if (typeof p.privileged === "boolean") return p.privileged;
  return (p.capabilities ?? []).some((c) => PRIVILEGED_CAPABILITIES.has(c));
}

/** 能力显示名；未知能力原样显示（正常情况下宿主已把未知能力拒掉了） */
export function capabilityLabel(c: string): string {
  return CAPABILITY_LABELS[c] ? tr(CAPABILITY_LABELS[c]) : c;
}

/** 排序项。名称排序在本地做（GitHub 搜索不支持按名排） */
export const PLUGIN_SORTS = [
  { value: "relevance", label: "相关度" },
  { value: "stars", label: "最热" },
  { value: "updated", label: "最近更新" },
  { value: "name", label: "名称" },
] as const;
export type PluginSort = (typeof PLUGIN_SORTS)[number]["value"];

/** 内置插件：第一个就是 DeepSeek Harness（见 src-tauri/src/plugin/dsh.rs） */
export const BUILTIN_PLUGINS: PluginEntry[] = [
  {
    id: "dsh",
    name: "DeepSeek Harness",
    summary: "把 dsh 智能体接进来：自动准备干净 profile，并注入本应用的 MCP 壁纸工具",
    description:
      "扫描本机 dsh 环境 → 在 ~/.dsh/profiles/wallpallperem 建一个干净 profile → 把本应用的 MCP 服务写进它的 patch 层 → 在新窗口打开 dsh 页面。",
    source: "builtin",
    category: "AI 助手",
    author: "WallpaperEM",
    icon: "🧠",
    open: "dsh",
    installed: true,
    market: "local",
  },
];

/** 关键词匹配：名称/说明/作者/分类，大小写不敏感 */
function matches(p: PluginEntry, q: string): boolean {
  if (!q) return true;
  return [p.name, p.summary, p.description, p.author, p.category]
    .join("\n")
    .toLowerCase()
    .includes(q);
}

/** 按关键词 + 分类过滤。关键词空白 = 不过滤 */
export function filterPlugins(
  list: PluginEntry[],
  query: string,
  category: PluginCategory | "all",
): PluginEntry[] {
  const q = query.trim().toLowerCase();
  return list.filter((p) => (category === "all" || p.category === category) && matches(p, q));
}

/** 去重键：优先主页/入口地址（已安装条目与搜索结果指向同一个仓库时靠它合并），否则用名字 */
function dedupeKey(p: PluginEntry): string {
  const norm = (s?: string) => (s || "").trim().toLowerCase().replace(/\/+$/, "");
  return norm(p.homepage) || norm(p.url) || `name:${p.name.trim().toLowerCase()}`;
}

/**
 * 合并「已安装 + GitHub 搜索结果」。
 *
 * 冲突时优先级：已安装 > GitHub —— 已安装的条目带着本地目录与清单里的正式名字，
 * 比搜索结果（仓库名 + 描述）更权威。
 */
export function mergeMarket(...lists: PluginEntry[][]): PluginEntry[] {
  const rank = (p: PluginEntry) =>
    (p.installed ? 2 : 0) + (p.market === "local" ? 1 : 0);
  const best = new Map<string, PluginEntry>();
  for (const list of lists) {
    for (const p of list) {
      const key = dedupeKey(p);
      const cur = best.get(key);
      if (!cur || rank(p) > rank(cur)) best.set(key, p);
    }
  }
  return [...best.values()];
}

/** 排序。星数/更新时间缺失（多为已安装条目）的一律排后面，避免它们顶在最前又无从解释 */
export function sortPlugins(list: PluginEntry[], sort: PluginSort): PluginEntry[] {
  const byName = (a: PluginEntry, b: PluginEntry) =>
    a.name.toLowerCase().localeCompare(b.name.toLowerCase());
  const out = [...list];
  switch (sort) {
    case "stars":
      out.sort((a, b) => (b.stars ?? -1) - (a.stars ?? -1) || byName(a, b));
      break;
    case "updated":
      out.sort((a, b) => (b.updatedAt ?? "").localeCompare(a.updatedAt ?? "") || byName(a, b));
      break;
    case "name":
      out.sort(byName);
      break;
    default:
      // 相关度：保持调用方给的顺序（GitHub 自己是按相关度返回的）
      break;
  }
  return out;
}

/** 分类胶囊的显示名（分类词表本身也是中文原文，走同一张译文表） */
export function categoryLabel(c: PluginCategory): string {
  return tr(c);
}
