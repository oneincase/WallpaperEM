// Wallpaper Engine 工坊标签目录（单一真源）
//
// 标签名必须与 Steam 工坊完全一致（大小写、空格都不能改）—— 它们直接作为
// `requiredtags[]` / `excludedtags[]` 的值发给 Steam，写错就是静默零结果。
//
// 筛选语义（与 Steam 原生 requiredtags 的严格 AND 不同，见 workshop.rs）：
// 组内多选是**并集**（选 Anime + Girls = 任一命中即显示），组间是交集，
// 一个都不选 = 不约束（都显示）。每个标签只有选中/未选中两态，
// **每个分组都能任选多个** —— 类型/分级/分类/分辨率也不例外。
//
// 分组与顺序对齐 WE 官方工坊筛选面板，但单选/多选不再跟官方走：官方面板把
// 类型/分级/分类/分辨率做成单选，多选在这里更有用（「想看场景和视频」是个
// 正常诉求），且组内并集的语义在本地库与工坊两条链路上都天然成立。
import { getLocale } from "./i18n";

export type TagGroupKind =
  | "type"
  | "rating"
  | "category"
  | "genre"
  | "resolution"
  | "misc";

export type TagGroup = {
  kind: TagGroupKind;
  /** 面板上的分组标题 */
  label: string;
  /** 标签英文原名（发给 Steam 的值） → 中文显示名 */
  tags: { name: string; label: string; libraryOnly?: boolean }[];
};

/**
 * 「本地导入」分类标签（仅本地库筛选）：匹配 item_id 为 custom-* 的本地导入条目。
 * 不是真实工坊标签，值特意用 $ 开头避免与任何 Steam 标签撞名；
 * 后端 library_list 见到它会翻译成 `item_id LIKE 'custom-%'`。
 */
export const LOCAL_IMPORT_TAG = "$local";

export const TAG_GROUPS: TagGroup[] = [
  {
    kind: "type",
    label: "类型",
    tags: [
      { name: "Scene", label: "场景" },
      { name: "Video", label: "视频" },
      { name: "Web", label: "网页" },
      // 预设类内容（可玩预设壁纸，如载具预设场景）：Steam 侧就是 `Preset` 标签，
      // 与 场景/视频/网页 同级并列（WE 工坊的类型维度）。与下方「分类」组的
      // 预设同值 —— 官方 Type / Category 两个维度都含它，选中态自然联动。
      { name: "Preset", label: "预设" },
      // 刻意不放 Application：本地 12k 条工坊缓存里零条，选了必然空结果。
      // 也不放 Vehicle：它是官方「题材/Genre」维度（载具主题壁纸），
      // 入口在下方题材组的「载具」
    ],
  },
  {
    kind: "rating",
    label: "年龄分级",
    tags: [
      { name: "Everyone", label: "大众级" },
      { name: "Questionable", label: "指导级" },
      { name: "Mature", label: "成人级" },
    ],
  },
  {
    kind: "category",
    label: "分类",
    tags: [
      { name: "Wallpaper", label: "壁纸" },
      { name: "Preset", label: "预设" },
      { name: "Asset", label: "素材" },
      // 仅本地库筛选面板展示（工坊没有这个维度）
      { name: LOCAL_IMPORT_TAG, label: "本地导入", libraryOnly: true },
    ],
  },
  {
    kind: "genre",
    label: "题材",
    tags: [
      { name: "Abstract", label: "抽象" },
      { name: "Animal", label: "动物" },
      { name: "Anime", label: "动漫" },
      { name: "Cartoon", label: "卡通" },
      { name: "CGI", label: "CGI" },
      { name: "Cyberpunk", label: "赛博朋克" },
      { name: "Fantasy", label: "奇幻" },
      { name: "Game", label: "游戏" },
      { name: "Girls", label: "女性" },
      { name: "Guys", label: "男性" },
      { name: "Landscape", label: "风景" },
      { name: "Medieval", label: "中世纪" },
      { name: "Memes", label: "梗图" },
      { name: "MMD", label: "MMD" },
      { name: "Music", label: "音乐" },
      { name: "Nature", label: "自然" },
      { name: "Pixel art", label: "像素画" },
      { name: "Relaxing", label: "解压" },
      { name: "Retro", label: "复古" },
      { name: "Sci-Fi", label: "科幻" },
      { name: "Sports", label: "运动" },
      { name: "Technology", label: "科技" },
      { name: "Television", label: "影视" },
      { name: "Vehicle", label: "载具" },
      { name: "Unspecified", label: "未分类" },
    ],
  },
  {
    kind: "resolution",
    label: "分辨率",
    tags: [
      { name: "Standard Definition", label: "标清" },
      { name: "1280 x 720", label: "1280 × 720" },
      { name: "1366 x 768", label: "1366 × 768" },
      { name: "1920 x 1080", label: "1920 × 1080" },
      { name: "2560 x 1440", label: "2560 × 1440" },
      { name: "3840 x 2160", label: "3840 × 2160（4K）" },
      { name: "Ultrawide Standard Definition", label: "带鱼屏 标清" },
      { name: "Ultrawide 2560 x 1080", label: "带鱼屏 2560 × 1080" },
      { name: "Ultrawide 3440 x 1440", label: "带鱼屏 3440 × 1440" },
      { name: "Dual Standard Definition", label: "双屏 标清" },
      { name: "Dual 3840 x 1080", label: "双屏 3840 × 1080" },
      { name: "Dual 5120 x 1440", label: "双屏 5120 × 1440" },
      { name: "Dual 7680 x 2160", label: "双屏 7680 × 2160" },
      { name: "Triple Standard Definition", label: "三屏 标清" },
      { name: "Triple 4096 x 768", label: "三屏 4096 × 768" },
      { name: "Triple 5760 x 1080", label: "三屏 5760 × 1080" },
      { name: "Triple 7680 x 1440", label: "三屏 7680 × 1440" },
      { name: "Triple 11520 x 2160", label: "三屏 11520 × 2160" },
      { name: "Portrait Standard Definition", label: "竖屏 标清" },
      { name: "Portrait 720 x 1280", label: "竖屏 720 × 1280" },
      { name: "Portrait 1080 x 1920", label: "竖屏 1080 × 1920" },
      { name: "Portrait 1440 x 2560", label: "竖屏 1440 × 2560" },
      { name: "Portrait 2160 x 3840", label: "竖屏 2160 × 3840" },
      { name: "Other resolution", label: "其他分辨率" },
      { name: "Dynamic resolution", label: "动态分辨率" },
    ],
  },
  {
    kind: "misc",
    label: "功能特性",
    tags: [
      { name: "Approved", label: "官方推荐" },
      { name: "Audio responsive", label: "音频响应" },
      { name: "Customizable", label: "可自定义" },
      { name: "3D", label: "3D" },
      { name: "Puppet Warp", label: "骨骼动画" },
      { name: "HDR", label: "HDR" },
      { name: "Media Integration", label: "媒体集成" },
      { name: "User Shortcut", label: "快捷键" },
      { name: "Video Texture", label: "视频纹理" },
      { name: "Asset Pack", label: "素材包" },
    ],
  },
];

/** 英文标签名 → 中文显示名（渲染已选标签用） */
export const TAG_LABEL: Record<string, string> = Object.fromEntries(
  TAG_GROUPS.flatMap((g) => g.tags.map((t) => [t.name, t.label])),
);

/**
 * 标签展示名：中文界面用中文标签，其它语言直接回落到 **Steam 原始英文标签名**。
 *
 * 刻意不走 tr() 文案表 —— 标签的英文名（`Anime` / `Pixel art` / `Video`）就是
 * 工坊里的规范写法，再翻译一遍容易和工坊对不上，也白多维护几十条映射。
 * 调用点都在渲染期，语言切换时随根组件重渲染自动更新。
 */
export function tagLabel(name: string): string {
  const label = TAG_LABEL[name];
  if (!label) return name;
  return getLocale() === "zh-CN" ? label : name;
}

/** 工坊排序。实测只有 browsesort 生效，actualsort 完全无效 */
export const WORKSHOP_SORTS = [
  { value: "trend", label: "趋势" },
  { value: "mostrecent", label: "最新发布" },
  { value: "lastupdated", label: "最近更新" },
  { value: "totaluniquesubscribers", label: "最多订阅" },
  { value: "toprated", label: "最高评分" },
] as const;

/** 趋势时间范围。实测 days 只对 browsesort=trend 生效，其他排序下无任何效果 */
export const TREND_DAYS = [
  { value: 1, label: "今天" },
  { value: 7, label: "本周" },
  { value: 30, label: "本月" },
  { value: 90, label: "三个月" },
  { value: 180, label: "半年" },
  { value: 365, label: "一年" },
  { value: -1, label: "全部时间" },
] as const;

/** 本地库排序（纯本地字段，与工坊排序不同源） */
export const LIBRARY_SORTS = [
  { value: "downloaded_desc", label: "最近下载" },
  { value: "downloaded_asc", label: "最早下载" },
  { value: "title_asc", label: "名称 A→Z" },
  { value: "title_desc", label: "名称 Z→A" },
  { value: "size_desc", label: "体积从大到小" },
  { value: "size_asc", label: "体积从小到大" },
] as const;

export type LibrarySort = (typeof LIBRARY_SORTS)[number]["value"];

/** 标签选择状态：两态（选中/未选中），选中的标签值为 true */
export type TagSelection = Record<string, true>;

/**
 * 默认筛选条件：默认只看大众级内容。
 *
 * 注意这与 Steam 网页端不同 —— 实测 Steam 默认不做任何年龄过滤。
 * 这里刻意收紧，避免首屏直接推成人内容；用户可在筛选面板里改（全不选 = 都显示）。
 */
export const DEFAULT_TAG_SELECTION: TagSelection = {
  Everyone: true,
};

/**
 * 切换一个标签的选中态（两态：选中 ⇄ 未选中）。
 * 任何分组都自由叠加（组内并集），不顶替同组其它标签。
 */
export function toggleTagSelection(sel: TagSelection, name: string): TagSelection {
  const next = { ...sel };
  if (next[name]) {
    delete next[name];
    return next;
  }
  next[name] = true;
  return next;
}

/**
 * 选择状态 → 分组标签列表（组内并集 OR、组间交集 AND）。
 * 顺序固定随 TAG_GROUPS，保证指纹/缓存键稳定。空组剔除。
 */
export function selectedTagGroups(sel: TagSelection): string[][] {
  return TAG_GROUPS.map((g) => g.tags.map((t) => t.name).filter((n) => sel[n])).filter(
    (tags) => tags.length > 0,
  );
}

/** 选中标签总数（筛选角标用） */
export function selectedTagCount(sel: TagSelection): number {
  return Object.keys(sel).length;
}
