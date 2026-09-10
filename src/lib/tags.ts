// Wallpaper Engine 工坊标签目录（单一真源）
//
// 标签名必须与 Steam 工坊完全一致（大小写、空格都不能改）—— 它们直接作为
// `requiredtags[]` / `excludedtags[]` 的值发给 Steam，写错就是静默零结果。
//
// ⚠️ Steam 对 requiredtags[] 是**严格 AND**，组内也不例外：
// 同时选 Anime 和 Girls 得到的是「既是 Anime 又是 Girls」的极少量结果，
// 不是两者的并集。UI 上要让用户理解「每多选一个标签，结果只会更少」。
//
// 分组与顺序对齐 WE 官方工坊筛选面板。

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
  /** 单选（select 语义）还是多选（checkbox 语义）—— 与 WE 官方面板一致 */
  multi: boolean;
  /** 标签英文原名（发给 Steam 的值） → 中文显示名 */
  tags: { name: string; label: string }[];
};

export const TAG_GROUPS: TagGroup[] = [
  {
    kind: "type",
    label: "类型",
    multi: false,
    tags: [
      { name: "Scene", label: "场景" },
      { name: "Video", label: "视频" },
      { name: "Web", label: "网页" },
      // 刻意不放 Application：本地 12k 条工坊缓存里零条，选了必然空结果
    ],
  },
  {
    kind: "rating",
    label: "年龄分级",
    multi: false,
    tags: [
      { name: "Everyone", label: "大众级" },
      { name: "Questionable", label: "指导级" },
      { name: "Mature", label: "成人级" },
    ],
  },
  {
    kind: "category",
    label: "分类",
    multi: false,
    tags: [
      { name: "Wallpaper", label: "壁纸" },
      { name: "Preset", label: "预设" },
      { name: "Asset", label: "素材" },
    ],
  },
  {
    kind: "genre",
    label: "题材",
    multi: true,
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
    multi: false,
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
    multi: true,
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

/**
 * 默认筛选条件：默认只看大众级内容。
 *
 * 注意这与 Steam 网页端不同 —— 实测 Steam 默认不做任何年龄过滤。
 * 这里刻意收紧，避免首屏直接推成人内容；用户可在筛选面板里改。
 */
export const DEFAULT_TAG_STATE: Record<string, "on" | "excluded"> = {
  Everyone: "on",
};
