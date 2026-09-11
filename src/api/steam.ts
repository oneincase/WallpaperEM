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
  /**
   * 分组标签：组内并集（OR）、组间交集（AND），空数组/全空组 = 不约束。
   * 后端按组的笛卡尔积拆成多次 Steam 查询再合并（Steam 原生只支持严格 AND）。
   */
  tagGroups?: string[][];
  /** 旧版平面必需标签（严格 AND），等价于每个标签自成一组；新代码请用 tagGroups */
  tags?: string[];
  /** 排除标签（命中任一即排除）。两态筛选 UI 已不下发，仅为兼容保留 */
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
  /** 分组标签：组内并集、组间交集；"$local" 匹配本地导入（custom-*）条目 */
  tagGroups?: string[][];
  /** 旧版平面必需标签（AND），新代码请用 tagGroups */
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
  /**
   * 标签组合数超出后端上限：只查了一部分标签组合，结果不完整。
   * 可选 —— 旧版本写入的页面快照里没有这个字段。
   */
  truncated?: boolean;
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

/** 抽帧组件（ffmpeg）状态：视频/GIF 壁纸抽首帧、本地库视频封面都依赖它 */
export interface FfmpegStatus {
  /** 当前平台是否提供托管 ffmpeg（macOS 走 AVFoundation 抽帧，恒 false） */
  supported: boolean;
  /** 应用内安装的托管副本已就绪 */
  managed: boolean;
  /** 系统 PATH 上检测到可用的 ffmpeg */
  system: boolean;
  /** 实际生效的来源（托管副本优先） */
  active: "managed" | "system" | null;
  version: string | null;
  path: string | null;
  /** 托管副本占用的字节数 */
  sizeBytes: number;
  /** 托管目录（应用数据目录下） */
  dir: string | null;
  /** 预计下载体积（平台不同，仅用于文案） */
  expectedDownloadBytes: number;
}

/** ffmpeg 安装进度事件 `ffmpeg:install-progress` 的载荷 */
export interface FfmpegInstallProgress {
  phase: "download" | "extract";
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
  // 关于 / 更新
  /** 应用信息（名称/版本/平台/架构） */
  appInfo: () =>
    invoke<{ name: string; version: string; os: string; arch: string; description: string }>(
      "app_info"
    ),
  /** 检查更新（读 GitHub Releases 最新版并与当前版本比对） */
  appUpdateCheck: () => invoke<UpdateInfo>("app_update_check"),
  /** 下载匹配当前平台的安装包到缓存目录，返回落地路径（进度走 update:progress 事件） */
  appUpdateDownload: (url: string, name: string) =>
    invoke<string>("app_update_download", { url, name }),
  /** 打开已下载的安装包，返回给用户看的操作提示 */
  appUpdateOpen: (path: string) => invoke<string>("app_update_open", { path }),
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
  // 抽帧组件（ffmpeg）：Linux/Windows 上「视频/GIF → 静态壁纸」与本地库视频封面要用
  ffmpegStatus: () => invoke<FfmpegStatus>("ffmpeg_status"),
  ffmpegInstall: (force?: boolean) =>
    invoke<{ installed: boolean; skipped?: boolean; version?: string; path?: string }>(
      "ffmpeg_install",
      { force }
    ),
  ffmpegUninstall: () => invoke<void>("ffmpeg_uninstall"),
  downloadCredentialsSet: (username: string, password: string) =>
    invoke<{ ok: boolean; username: string }>("download_credentials_set", {
      username,
      password,
    }),
  downloadCredentialsStatus: () =>
    invoke<{ configured: boolean; username?: string }>("download_credentials_status"),
  downloadCredentialsClear: () => invoke<void>("download_credentials_clear"),
  /** 入队「登录验证」任务（steamcmd +login +quit），交互走全局 Guard 弹窗 */
  downloadVerifyLogin: () => invoke<number>("download_verify_login"),
  /** 只建立网页会话（保存凭据时的双验证用），不拉订阅列表 */
  accountWebLoginStart: () => invoke<AccountWebLoginResponse>("account_web_login_start"),
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
  // 订阅同步（网页会话登录 Steam → 拉取已订阅工坊列表）
  subscriptionsStatus: () =>
    invoke<{ configured: boolean; username?: string; hasSession: boolean }>(
      "subscriptions_status",
    ),
  /** 拉取一页订阅（page 从 1 开始，每页 30 条；响应带 total 总数供懒加载判断） */
  subscriptionsPage: (page: number) =>
    invoke<SubscriptionsResponse>("subscriptions_page", { page }),
  subscriptionsSubmitCode: (code: string) =>
    invoke<SubscriptionsResponse>("subscriptions_submit_code", { code }),
  subscriptionsLogout: () => invoke<void>("subscriptions_logout"),
  /** 开启扫码登录，返回要渲染成二维码的 challengeUrl */
  subscriptionsQrBegin: () => invoke<QrResponse>("subscriptions_qr_begin"),
  /** 扫码轮询（单次最多阻塞 ~8s）：拿到订阅列表返回 ok，否则回传二维码继续等 */
  subscriptionsQrPoll: () => invoke<QrResponse>("subscriptions_qr_poll"),
  networkProbe: () => invoke<{ results: NetcheckItem[]; allOk: boolean; hint: string }>("network_probe"),
  diagnosticsExport: () => invoke<string>("diagnostics_export"),
  /** 当前缓存占用（预览图/网页缓存 + 壁纸首帧封面） */
  cacheStats: () => invoke<CacheStats>("cache_stats"),
  /** 清除缓存，返回实际释放的字节数 */
  cacheClear: () => invoke<number>("cache_clear"),
  // MCP（AI agent 接入，见 README「MCP 服务」）
  mcpStatus: () => invoke<McpStatus>("mcp_status"),
  mcpSetEnabled: (enabled: boolean) => invoke<McpStatus>("mcp_set_enabled", { enabled }),
  mcpSetPort: (port: number) => invoke<McpStatus>("mcp_set_port", { port }),
  mcpRotateToken: () => invoke<McpStatus>("mcp_rotate_token"),
  mcpConfigSnippet: () => invoke<McpConfigSnippet>("mcp_config_snippet"),
};

export interface CacheEntry {
  key: string;
  label: string;
  bytes: number;
  path: string;
}

export interface CacheStats {
  bytes: number;
  entries: CacheEntry[];
}

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
  /** 工坊订阅数（= 下载量）。元数据缓存里没有该条目时为 0 */
  subscriptions: number;
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

// ---------- 订阅同步 ----------

export interface SubscriptionItem {
  id: string;
  title: string;
  previewUrl: string;
  type: WallpaperType;
  subscriptions?: number;
  /** 已在本地库（不必再下载） */
  downloaded: boolean;
}

/**
 * 订阅拉取结果：
 * - ok：拿到列表
 * - needCode：需要 Steam Guard 验证码（device=手机令牌 / email=邮箱码）
 * - pendingConfirmation：等待手机 App 确认登录，前端应提示后重试 subscriptionsList
 */
export type SubscriptionsResponse =
  | { status: "ok"; items: SubscriptionItem[]; total: number; page: number }
  | {
      status: "needCode";
      codeType: "device" | "email";
      message: string;
      /** Steam 同时在手机 App 推了「确认登录」：前端可后台轮询，点允许后自动放行 */
      canConfirmOnPhone?: boolean;
    }
  | { status: "pendingConfirmation"; message: string }
  /** 本地保存的账号密码被 Steam 拒绝（EResult 5），需要用户重新输入 */
  | { status: "badCredentials"; message: string; username?: string }
  /** 验证/扫码已成功且凭证已保存，前端应给出「登录成功」反馈后拉取第一页 */
  | { status: "confirmed" };

/** 网页会话建立结果（不含订阅数据）：ok=会话已建立 */
export type AccountWebLoginResponse =
  | { status: "ok" }
  | {
      status: "needCode";
      codeType: "device" | "email";
      message: string;
      /** 同上：手机 App 确认通道可用，可轮询等待 */
      canConfirmOnPhone?: boolean;
    }
  | { status: "pendingConfirmation"; message: string }
  | { status: "badCredentials"; message: string; username?: string };

/** 扫码登录响应：qr=继续展示二维码等待确认；confirmed=已确认并保存凭证 */
export type QrResponse =
  | { status: "qr"; challengeUrl: string }
  | { status: "confirmed" };

/** 系统音频处理（音频可视化）状态 */
export interface AudioProcessingStatus {
  /** 设置开关（持久化） */
  enabled: boolean;
  /** 捕获是否运行中 */
  running: boolean;
  /** 采集权限是否已授予（macOS = 屏幕录制 TCC；Windows WASAPI loopback 无需授权，恒为 true） */
  granted: boolean;
  /** 当前平台是否支持系统音频捕获（macOS CoreAudio / Windows WASAPI 支持；Linux 待接入 PipeWire） */
  supported: boolean;
}

/** 一次 MCP 工具调用记录（内存环形缓冲，最多 50 条） */
export interface McpCallLog {
  tool: string;
  ok: boolean;
  ms: number;
  summary: string;
  /** unix 毫秒时间戳 */
  at: number;
}

/** MCP 服务状态 */
export interface McpStatus {
  /** 应用内是否挂载了 MCP 子系统（未挂载时其余字段缺失） */
  available: boolean;
  enabled: boolean;
  /** HTTP 监听是否真的起来了（端口被占用时为 false，原因见 lastError） */
  running: boolean;
  port: number;
  token: string;
  url: string;
  urlWithToken: string;
  /** 启动失败原因（端口占用等），成功时为 null */
  lastError: string | null;
  calls: McpCallLog[];
}

/** 客户端接入片段（设置页一键复制） */
export interface McpConfigSnippet {
  url: string;
  /** mcpServers JSON 片段 */
  json: string;
  codexCli: string;
  claudeCli: string;
}

/** 更新包（当前平台匹配到的那个） */
export interface UpdateAsset {
  name: string;
  url: string;
  /** 字节数；GitHub 未给时为 0 */
  size: number;
}

/** 更新检查结果（对齐 Rust `UpdateInfo`） */
export interface UpdateInfo {
  current: string;
  latest: string;
  hasUpdate: boolean;
  /** Release 标题 */
  name: string;
  /** Release 说明（Markdown 原文，界面按纯文本展示） */
  notes: string;
  publishedAt: string;
  htmlUrl: string;
  /** 没有匹配当前平台的安装包时为 null（仍可走 htmlUrl 手动下载） */
  asset: UpdateAsset | null;
}
