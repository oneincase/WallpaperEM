// Steam / 工坊 API 封装（镜像 shared 类型 + Rust 命令签名）
import { invoke } from "@tauri-apps/api/core";

export type WallpaperType =
  | "video"
  | "scene"
  | "web"
  | "gif"
  | "application"
  | "unknown";

export const TYPE_LABELS: Record<WallpaperType, string> = {
  video: "视频",
  scene: "场景",
  web: "网页",
  gif: "GIF",
  application: "程序",
  unknown: "未知",
};

export const SORTS = [
  { value: "trend", label: "趋势" },
  { value: "totaluniquesubscribers", label: "最多订阅" },
  { value: "totalfavorited", label: "最多收藏" },
  { value: "timecreated", label: "最新" },
];

export interface WorkshopItemSummary {
  id: string;
  title: string;
  previewUrl: string;
  tags: string[];
  type: WallpaperType;
  subscriptions?: number;
  favorited?: number;
  timeCreated?: number;
}

export interface WorkshopItem extends WorkshopItemSummary {
  description: string;
  fileUrl?: string;
  fileSize?: number;
  creator?: string;
  timeCreated?: number;
  timeUpdated?: number;
}

export interface WorkshopSearchParams {
  query?: string;
  type?: WallpaperType | "";
  /** 必需标签。Steam 侧是**严格 AND**，多选只会让结果更少 */
  tags?: string[];
  /** 排除标签（命中任一即排除） */
  excludedTags?: string[];
  sort?: string;
  /** 趋势时间范围（天）。仅 sort=trend 时生效 */
  days?: number;
  createdAfter?: number;
  createdBefore?: number;
  updatedAfter?: number;
  updatedBefore?: number;
  page?: number;
}

/** 本地库筛选。全部可选，未提供即不约束该维度 */
export interface LibraryFilter {
  type?: WallpaperType | "";
  /** 标题模糊搜索 */
  query?: string;
  tags?: string[];
  excludedTags?: string[];
  minSize?: number;
  maxSize?: number;
  downloadedAfter?: number;
  downloadedBefore?: number;
  /** 只看文件已丢失的条目 */
  onlyMissing?: boolean;
  sort?: string;
}

export interface WorkshopSearchResult {
  items: WorkshopItemSummary[];
  total: number;
  page: number;
  pageSize: number;
  hasMore: boolean;
}

export type DownloadStatus =
  | "queued"
  | "authenticating"
  | "downloading"
  | "installing"
  | "done"
  | "failed";

export const DOWNLOAD_STATUS_LABELS: Record<DownloadStatus, string> = {
  queued: "排队中",
  authenticating: "登录 Steam",
  downloading: "下载中",
  installing: "安装中",
  done: "完成",
  failed: "失败",
};

export interface DownloadTask {
  id: number;
  itemId: string;
  title: string;
  status: DownloadStatus;
  progress: number;
  errorCode?: string;
  errorMsg?: string;
  waitingGuard: boolean;
  /** 依赖补拉任务（主壁纸缺依赖时自动入队） */
  dependency?: boolean;
  createdAt: number;
  startedAt?: number;
  finishedAt?: number;
}

/** 下载工具（Valve 官方 steamcmd）安装状态 */
export interface DownloadToolStatus {
  /** 已下载且已完成首次自更新，可用于下载 */
  installed: boolean;
  /** 已下载解压（可能尚未完成自更新） */
  downloaded: boolean;
  /** 已完成首次自更新（生成 steamclient.dylib） */
  warmed: boolean;
  path?: string;
  version?: string | null;
  /** Apple Silicon 上缺少 Rosetta 2：官方引导程序是 x86_64，首次启动需要它 */
  rosettaMissing: boolean;
}

/** steamcmd 安装进度事件 `steamcmd:install-progress` 的载荷 */
export interface SteamcmdInstallProgress {
  phase: "download" | "extract" | "warmup";
  progress: number;
  message: string;
}

/** 批量导入结果（文件多选/文件夹/拖拽共用）：逐条容错 */
export type ImportBatchResult = {
  cancelled?: boolean;
  /** 新导入条数 */
  imported?: number;
  /** 重复复用条数（同名同大小已在库，未产生副本） */
  duplicates?: number;
  items?: { path: string; item_id: string; title: string; type: string; duplicate: boolean }[];
  failed?: { path: string; error: string }[];
};

export const api = {
  // 工坊
  workshopSearch: (params: WorkshopSearchParams) =>
    invoke<WorkshopSearchResult>("workshop_search", { params }),
  /** 随机推荐。接受与 workshopSearch 相同的筛选参数（发现页与工坊页共用条件） */
  workshopRandom: (params?: WorkshopSearchParams) =>
    invoke<WorkshopSearchResult>("workshop_random", { params }),
  workshopItem: (id: string) => invoke<WorkshopItem | null>("workshop_item", { id }),
  // 下载
  downloadToolStatus: () => invoke<DownloadToolStatus>("download_tool_status"),
  steamcmdInstall: (force?: boolean) =>
    invoke<{ installed: boolean; skipped?: boolean; path?: string; version?: string }>(
      "steamcmd_install_tool",
      { force }
    ),
  steamcmdUninstall: () => invoke<void>("steamcmd_uninstall_tool"),
  downloadCredentialsSet: (username: string, password: string) =>
    invoke<{ ok: boolean; username: string }>("download_credentials_set", {
      username,
      password,
    }),
  downloadCredentialsStatus: () =>
    invoke<{ configured: boolean; username?: string }>("download_credentials_status"),
  downloadCredentialsClear: () => invoke<void>("download_credentials_clear"),
  downloadEnqueue: (itemId: string) => invoke<number>("download_enqueue", { itemId }),
  downloadList: () => invoke<DownloadTask[]>("download_list"),
  downloadCancel: (id: number) => invoke<boolean>("download_cancel", { id }),
  downloadRetry: (id: number) => invoke<boolean>("download_retry", { id }),
  downloadSubmitGuard: (id: number, code: string) =>
    invoke<boolean>("download_submit_guard", { id, code }),
  downloadRemove: (id: number) => invoke<boolean>("download_remove", { id }),
  downloadClearFinished: () => invoke<number>("download_clear_finished"),
  // 本地库 / 收藏 / 壁纸
  libraryList: (type?: WallpaperType | "", filter?: LibraryFilter) =>
    invoke<LibraryItem[]>("library_list", { type, filter }),
  libraryDelete: (itemId: string) => invoke<boolean>("library_delete", { itemId }),
  /** 与磁盘对账：清理「有数据库记录但壁纸文件已丢失」的条目，返回被清理的 id */
  libraryPrune: () => invoke<string[]>("library_prune"),
  libraryOpenFolder: (itemId: string) => invoke<boolean>("library_open_folder", { itemId }),
  libraryImportFromWeb: (webDataDir: string) =>
    invoke<{
      imported: number;
      skipped: number;
      /** 逐条容错：失败条目不拖垮整批，逐条带回 { id, error } */
      failed?: { id: string; error: string }[];
    }>("library_import_from_web", { webDataDir }),
  libraryImportCustom: (sourcePath: string) =>
    invoke<{ imported: number; itemId: string; title: string; type: string; duplicate?: boolean }>(
      "library_import_custom",
      { sourcePath }
    ),
  /** 文件选择框（支持多选）→ 批量导入 */
  libraryImportCustomPick: () => invoke<ImportBatchResult>("library_import_custom_pick"),
  /** 文件夹选择框 → 作为完整壁纸目录导入（WE 工程目录） */
  libraryImportFolderPick: () => invoke<ImportBatchResult>("library_import_folder_pick"),
  /** 拖拽导入：逐条容错，单条失败不拖垮整批 */
  libraryImportCustomBatch: (paths: string[]) =>
    invoke<ImportBatchResult>("library_import_custom_batch", { paths }),
  // WE 网页壁纸用户属性
  libraryItemProps: (itemId: string) => invoke<WebPropDef[]>("library_item_props", { itemId }),
  /** 按 itemId 查标题（托盘入口给配置弹窗用；查不到返回 itemId） */
  libraryItemTitle: (itemId: string) => invoke<string>("library_item_title", { itemId }),
  librarySetItemProps: (itemId: string, values: WebPropValues) =>
    invoke<void>("library_set_item_props", { itemId, values }),
  librarySetItemPropFile: (itemId: string, propName: string) =>
    invoke<{ cancelled?: boolean; value?: string }>("library_set_item_prop_file", { itemId, propName }),
  libraryResetItemProps: (itemId: string) =>
    invoke<void>("library_reset_item_props", { itemId }),
  wallpaperApplyItem: (itemId: string) => invoke<void>("wallpaper_apply_item", { itemId }),
  libraryPreview: (itemId: string) => invoke<WallpaperConfig>("library_preview", { itemId }),
  wallpaperApply: (config: WallpaperConfig, displayId?: string) =>
    invoke<void>("wallpaper_apply", { config, displayId }),
  wallpaperStop: (displayId?: string) => invoke<void>("wallpaper_stop", { displayId }),
  wallpaperListSessions: () => invoke<{ active: boolean; paused: boolean; sessions: Record<string, WallpaperConfig> }>("wallpaper_list_sessions"),
  /** 内容服务器地址 + token（拼壁纸包内文件 URL 用，如 file 属性缩略图） */
  contentServerStatus: () =>
    invoke<{ port: number; token: string; base: string }>("content_server_status"),
  wallpaperActiveItems: () => invoke<string[]>("wallpaper_active_items"),
  wallpaperPauseAll: () => invoke<void>("wallpaper_pause_all"),
  wallpaperResumeAll: () => invoke<void>("wallpaper_resume_all"),
  wallpaperInteractiveSet: (enabled: boolean) => invoke<void>("wallpaper_interactive_set", { enabled }),
  wallpaperSetFit: (fit: string) => invoke<void>("wallpaper_set_fit", { fit }),
  wallpaperSetRenderDpr: (dpr: number) => invoke<void>("wallpaper_set_render_dpr", { dpr }),
  wallpaperSetLanguage: (language: string) =>
    invoke<void>("wallpaper_set_language", { language }),
  wallpaperSetSceneFps: (fps: number) => invoke<void>("wallpaper_set_scene_fps", { fps }),
  /** 读某壁纸的播放设置覆盖 + 当前全局默认（用于把未覆盖项显示成「跟随全局」） */
  wallpaperItemPlayConfig: (itemId: string) =>
    invoke<{ override: ItemPlayConfig; globals: PlayConfigGlobals }>(
      "wallpaper_item_play_config",
      { itemId },
    ),
  /** 写某壁纸的播放设置覆盖（该壁纸正在播放时立即生效，否则下次应用时生效） */
  wallpaperItemPlayConfigSet: (itemId: string, config: ItemPlayConfig) =>
    invoke<void>("wallpaper_item_play_config_set", { itemId, config }),
  wallpaperAudioProcessingSet: (enabled: boolean) =>
    invoke<AudioProcessingStatus>("wallpaper_audio_processing_set", { enabled }),
  wallpaperAudioProcessingStatus: () =>
    invoke<AudioProcessingStatus>("wallpaper_audio_processing_status"),
  wallpaperNext: () => invoke<{ itemId: string; index: number }>("wallpaper_next"),
  favoritesList: () => invoke<FavoriteItem[]>("favorites_list"),
  favoriteAdd: (itemId: string) => invoke<boolean>("favorite_add", { itemId }),
  favoriteRemove: (itemId: string) => invoke<boolean>("favorite_remove", { itemId }),
  favoriteStatus: (itemId: string) => invoke<boolean>("favorite_status", { itemId }),
  playlistList: () => invoke<Playlist[]>("playlist_list"),
  playlistCreate: (name: string, itemIds: string[], intervalSec: number) =>
    invoke<number>("playlist_create", { name, itemIds, intervalSec }),
  playlistDelete: (id: number) => invoke<boolean>("playlist_delete", { id }),
  playlistApply: (id: number) => invoke<Playlist>("playlist_apply", { id }),
  networkProbe: () => invoke<{ results: NetcheckItem[]; allOk: boolean; hint: string }>("network_probe"),
  diagnosticsExport: () => invoke<string>("diagnostics_export"),
};

export interface LibraryItem {
  itemId: string;
  title: string;
  type: WallpaperType;
  previewUrl?: string;
  tags: string[];
  sizeBytes: number;
  fileCount: number;
  downloadedAt: number;
  /** 磁盘上的壁纸文件已丢失，条目仅剩数据库记录（需清理或重新下载） */
  missing: boolean;
}

export interface FavoriteItem {
  itemId: string;
  title: string;
  previewUrl?: string;
  type: WallpaperType;
  createdAt: number;
}

export interface NetcheckItem {
  host: string;
  label: string;
  ok: boolean;
  ms: number;
}

export interface WallpaperConfig {
  type: "canvas" | "video" | "gif" | "web" | "scene" | "image";
  src?: string;
  fit?: string;
  muted?: boolean;
  loop?: boolean;
  /** 内容服务器媒体基址：http://127.0.0.1:<port>/media/<token> */
  mediaBase?: string;
}

export interface Playlist {
  id: number;
  name: string;
  itemIds: string[];
  intervalSec: number;
}

// ---------- WE 网页壁纸用户属性 ----------

export interface ComboOption {
  label: string;
  /** 保留 project.json 声明的类型（同一 combo 内数字/布尔/字符串混用是常态） */
  value: string | number | boolean;
  /** 选项级显隐条件 */
  condition?: string;
}

/** WE 属性类型。`(string & {})` 兜底未知类型，同时保留字面量的补全与穷尽检查 */
export type WebPropType =
  | "color"
  | "bool"
  | "slider"
  | "combo"
  | "text"
  | "textinput"
  | "file"
  | "directory"
  | "group"
  | (string & {});

/** project.json 属性定义 + 当前值（wire 格式：color="r g b" 浮点串，bool=布尔，slider=数值…） */
/**
 * 单张壁纸的播放设置覆盖。字段缺失 = 跟随全局默认。
 *
 * 是三态而非两态：`undefined` 表示"用户没碰过这项，跟随全局"，有值表示
 * "本壁纸专属"。不能用"等于当前全局值就算跟随"来简化 —— 那样用户特意设成
 * 与全局相同的值，会在全局改动后被意外带走。
 */
export interface ItemPlayConfig {
  fit?: "cover" | "contain" | "stretch";
  renderDpr?: number;
  sceneFps?: number;
  /** 0..1，0 即静音 */
  volume?: number;
}

/** 当前全局默认（供 UI 在「跟随全局」时显示实际会用的值） */
export interface PlayConfigGlobals {
  fit: string;
  renderDpr: number;
  sceneFps: number;
}

export interface WebPropDef {
  name: string;
  ptype: WebPropType;
  /** 已解析的显示文案（localization 表 → WE 内建映射 → 属性名；已剥离 HTML） */
  text: string;
  /** 排序键，可为浮点（壁纸用 32.5 这类细分序） */
  order: number;
  value: string | number | boolean | null;
  default: string | number | boolean | null;
  overridden: boolean;
  /** WE 显隐条件表达式，由 lib/weCondition 按当前草稿求值（仅影响 UI 显隐） */
  condition?: string;
  options: ComboOption[];
  min?: number;
  max?: number;
  step?: number;
  /** slider 显示精度（小数位数）；无 step 时拖动粒度也按它取 */
  precision?: number;
  /** file 属性的期望类别（image/video/audio），决定文件选择器过滤器 */
  fileType?: string;
}

export type WebPropValues = Record<string, string | number | boolean>;

/** 系统音频处理（音频可视化）状态 */
export interface AudioProcessingStatus {
  /** 设置开关（持久化） */
  enabled: boolean;
  /** 捕获是否运行中 */
  running: boolean;
  /** 屏幕录制权限是否已授予 */
  granted: boolean;
  /** 当前平台是否支持系统音频捕获（macOS 支持；Linux 待接入 PipeWire） */
  supported: boolean;
}
