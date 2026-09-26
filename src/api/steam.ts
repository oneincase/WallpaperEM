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

/**
 * 作者名片（Steam 个人资料解析结果）。project.json 没有 author 字段：
 * 作者 = 工坊条目 creator(SteamID64) 经后端抓 Steam 资料页解析而来。
 * 本地未发布的壁纸没有作者（接口返回 null）。
 */
export interface AuthorSummary {
  steamId: string;
  name: string;
  avatarUrl: string;
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
  /** 批量扫描时「扫过但不是壁纸工程」的目录数 */
  skippedDirs?: number;
  items?: {
    path: string;
    item_id: string;
    title: string;
    type: string;
    duplicate: boolean;
    /** 引用模式条目（不拷贝，库记录源目录） */
    linked?: boolean;
  }[];
  failed?: { path: string; error: string }[];
};

/** 工坊上传任务状态 */
export interface WorkshopUploadJob {
  jobId: string;
  project?: string;
  itemId?: string;
  title: string;
  /** staging | creating | uploading | committing | done | failed */
  status: string;
  /** 0-100 */
  progress: number;
  publishedfileid?: string;
  error?: string;
  workshopUrl?: string;
  startedAt: number;
}

export const api = {
  // 关于 / 更新
  /** 应用信息（名称/版本/平台/架构） */
  appInfo: () =>
    invoke<{ name: string; version: string; os: string; arch: string; description: string }>(
      "app_info"
    ),
  /** 原生文件夹选择框（单选）：返回选中的绝对路径，用户取消返回 null */
  appPickFolder: () => invoke<string | null>("app_pick_folder"),
  /** 检查更新（读 Release 里的 latest-{target}-{arch}.json 清单，semver 比对） */
  appUpdateCheck: () => invoke<UpdateInfo>("app_update_check"),
  /** 下载新版本（进度走 update:progress 事件）并原地安装；Windows 上装完进程直接退出 */
  appUpdateDownloadInstall: () => invoke<void>("app_update_download_install"),
  /** 安装完成后重启应用（Windows 走安装器自动重启，调不到这个） */
  appUpdateRestart: () => invoke<void>("app_update_restart"),
  // 快捷键
  /** 当前绑定 + 默认值 + 系统保留组合（「快捷键」页一次性拉齐） */
  hotkeysList: () => invoke<HotkeysInfo>("hotkeys_list"),
  /**
   * 设置某动作的绑定（整组替换；空数组 = 清空）。
   * force = 覆盖模式：注册失败（可能被其它应用占用）也照存不误。
   */
  hotkeysSet: (action: string, bindings: string[], force?: boolean) =>
    invoke<void>("hotkeys_set", { action, bindings, force }),
  /** 恢复某动作（或全部）的默认绑定 */
  hotkeysReset: (action?: string) => invoke<void>("hotkeys_reset", { action }),
  /** 录制挂起：摘菜单 + 注销全局热键，让 ⌘+任意键能到达录制框 */
  hotkeysRecordBegin: () => invoke<void>("hotkeys_record_begin"),
  /** 录制结束（成功/取消都要调）：恢复菜单与全部热键 */
  hotkeysRecordEnd: () => invoke<void>("hotkeys_record_end"),
  // 工坊
  workshopSearch: (params: WorkshopSearchParams) =>
    invoke<WorkshopSearchResult>("workshop_search", { params }),
  /** 随机推荐。接受与 workshopSearch 相同的筛选参数（发现页与工坊页共用条件） */
  workshopRandom: (params?: WorkshopSearchParams) =>
    invoke<WorkshopSearchResult>("workshop_random", { params }),
  workshopItem: (id: string) => invoke<WorkshopItem | null>("workshop_item", { id }),
  /** 已知 creator(SteamID64) 时查作者名片；解析失败/资料私密返回 null */
  steamAuthorSummary: (steamId: string) =>
    invoke<AuthorSummary | null>("steam_author_summary", { steamId }),
  /**
   * 本地库条目的作者名片：工坊下载/已上传条目经工坊元数据解析；
   * 本地未发布壁纸返回 null（此时前端隐藏作者行，与 WE 一致）。
   */
  libraryItemAuthor: (itemId: string) =>
    invoke<AuthorSummary | null>("library_item_author", { itemId }),
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
  /**
   * 文件夹选择框。mode="scan"：扫描壁纸工程目录，引用模式入库（添加壁纸目录）；
   * 其他/缺省：所选目录作为单个壁纸拷贝导入（导入文件夹；目录不含 project.json
   * 时退回扫描）
   */
  libraryImportFolderPick: (mode?: "scan" | "copy") =>
    invoke<ImportBatchResult>("library_import_folder_pick", { mode }),
  /**
   * 多选文件夹选择框：只返回选中的绝对路径（不导入），由前端维护待添加列表。
   * 用户取消时返回空数组。
   */
  libraryPickFolders: () => invoke<string[]>("library_pick_folders"),
  /** 批量引用入库：所选文件夹一律以引用方式登记（不拷贝），返回批量导入结果 */
  libraryLinkFolders: (paths: string[]) =>
    invoke<ImportBatchResult>("library_link_folders", { paths }),
  /** 拖拽导入：逐条容错，单条失败不拖垮整批 */
  libraryImportCustomBatch: (paths: string[]) =>
    invoke<ImportBatchResult>("library_import_custom_batch", { paths }),
  // 创意工坊上传
  /** 从本地库条目或 MCP 工程发起工坊上传（二选一），返回 { jobId } */
  workshopUploadStart: (opts: {
    itemId?: string;
    project?: string;
    title?: string;
    description?: string;
    tags?: string[];
    /** public（默认）/ friends / private */
    visibility?: string;
    changelog?: string;
  }) => invoke<{ jobId: string }>("workshop_upload_start", opts),
  /** 查询上传任务（jobId 缺省 = 全部） */
  workshopUploadStatus: (jobId?: string) =>
    invoke<WorkshopUploadJob[]>("workshop_upload_status", { jobId }),
  /**
   * 网页版上传准备：暂存内容到持久目录并用系统浏览器打开 Steam 工坊网页版
   * 上传/编辑页（走浏览器登录态，不依赖 Steam 客户端）。返回页面 URL 与暂存目录。
   */
  workshopWebUploadPrepare: (itemId: string) =>
    invoke<{ url: string; stagedPath: string | null; update: boolean }>(
      "workshop_web_upload_prepare",
      { itemId },
    ),
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
  wallpaperApplyItem: (itemId: string, displayId?: string) =>
    invoke<void>("wallpaper_apply_item", { itemId, displayId }),
  libraryPreview: (itemId: string) => invoke<WallpaperConfig>("library_preview", { itemId }),
  wallpaperApply: (config: WallpaperConfig, displayId?: string) =>
    invoke<void>("wallpaper_apply", { config, displayId }),
  wallpaperStop: (displayId?: string) => invoke<void>("wallpaper_stop", { displayId }),
  wallpaperListSessions: () => invoke<{ active: boolean; paused: boolean; sessions: Record<string, WallpaperConfig> }>("wallpaper_list_sessions"),
  /** 显示器列表 + 每屏当前会话摘要（id 即 apply/stop 的 displayId） */
  wallpaperDisplaysList: () => invoke<DisplaysListResult>("wallpaper_displays_list"),
  /** 内容服务器地址 + token（拼壁纸包内文件 URL 用，如 file 属性缩略图） */
  contentServerStatus: () =>
    invoke<{ port: number; token: string; base: string }>("content_server_status"),
  wallpaperActiveItems: () => invoke<string[]>("wallpaper_active_items"),
  wallpaperPauseAll: () => invoke<void>("wallpaper_pause_all"),
  wallpaperResumeAll: () => invoke<void>("wallpaper_resume_all"),
  wallpaperInteractiveSet: (enabled: boolean) => invoke<void>("wallpaper_interactive_set", { enabled }),
  wallpaperSetFit: (fit: string) => invoke<void>("wallpaper_set_fit", { fit }),
  wallpaperSetRenderDpr: (dpr: number) => invoke<void>("wallpaper_set_render_dpr", { dpr }),
  // 视频纹理上传倍率：0 = 自动（库内帧率守门按实测帧率压），>0 固定（1 = 不压）
  wallpaperSetVideoTexScale: (scale: number) => invoke<void>("wallpaper_set_video_tex_scale", { scale }),
  wallpaperSetLanguage: (language: string) =>
    invoke<void>("wallpaper_set_language", { language }),
  wallpaperSetSceneFps: (fps: number) => invoke<void>("wallpaper_set_scene_fps", { fps }),
  /** 全局抗锯齿（off/fxaa/msaa2/msaa4，库 1.3.23+），持久化并对所有壁纸窗口热切 */
  wallpaperSetAa: (mode: string) => invoke<void>("wallpaper_set_aa", { mode }),
  /** 全局粒子质量档（off/low/medium/high，库 1.3.23+） */
  wallpaperSetParticles: (quality: string) => invoke<void>("wallpaper_set_particles", { quality }),
  /** 全局后处理质量档（off/low/medium/high，库 1.3.23+） */
  wallpaperSetPost: (quality: string) => invoke<void>("wallpaper_set_post", { quality }),
  /** 全局无缝切换效果（叠化/推近/模糊/景深/圆形揭示/横向擦除/滑入），下一次换壁纸生效 */
  wallpaperSetReveal: (reveal: string) => invoke<void>("wallpaper_set_reveal", { reveal }),
  /** 全局滤镜 id（白名单见 Rust WALLPAPER_FILTERS），热切所有壁纸窗口 */
  wallpaperSetFilter: (filter: string) => invoke<void>("wallpaper_set_filter", { filter }),
  /** 全局贴图资源倍率（0.5–1；null = 跟随清晰度档）。挂载期生效，改后壁纸整页重载 */
  wallpaperSetResources: (scale: number | null) =>
    invoke<void>("wallpaper_set_resources", { scale }),
  /** 全局法线/蒙版资源倍率（0.35–1，默认 1 不缩）。挂载期生效，改后壁纸整页重载 */
  wallpaperSetResourcesNormal: (scale: number) =>
    invoke<void>("wallpaper_set_resources_normal", { scale }),
  /** 一键套用画质档位（low/medium/high）：整体覆盖清晰度/帧率/粒子/后处理/资源倍率/法线倍率 */
  wallpaperSetQualityPreset: (preset: string) =>
    invoke<void>("wallpaper_set_quality_preset", { preset }),
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
  /** WE 官方素材通路开关（挂载时生效，切换后现有壁纸整页重载） */
  wallpaperLocalAssetsSet: (enabled: boolean) =>
    invoke<void>("wallpaper_local_assets_set", { enabled }),
  /** 自定义 WE assets 根目录（传空串清除，回到 Steam 库自动探测） */
  wallpaperWeAssetsDirSet: (dir: string) =>
    invoke<void>("wallpaper_we_assets_dir_set", { dir }),
  wallpaperLocalAssetsStatus: () =>
    invoke<LocalAssetsStatus>("wallpaper_local_assets_status"),
  wallpaperNext: (displayId?: string) =>
    invoke<{ itemId: string; index: number; steps: unknown[] }>("wallpaper_next", { displayId }),
  favoritesList: () => invoke<FavoriteItem[]>("favorites_list"),
  favoriteAdd: (itemId: string) => invoke<boolean>("favorite_add", { itemId }),
  favoriteRemove: (itemId: string) => invoke<boolean>("favorite_remove", { itemId }),
  favoriteStatus: (itemId: string) => invoke<boolean>("favorite_status", { itemId }),
  playlistList: () => invoke<Playlist[]>("playlist_list"),
  playlistGet: (id: number) => invoke<Playlist>("playlist_get", { id }),
  playlistCreate: (name: string, itemIds: string[], intervalSec: number, shuffle?: boolean) =>
    invoke<number>("playlist_create", { name, itemIds, intervalSec, shuffle }),
  /** 局部更新（缺省字段不改）；条目/随机变化会重建轮播队列 */
  playlistUpdate: (
    id: number,
    patch: { name?: string; itemIds?: string[]; intervalSec?: number; shuffle?: boolean },
  ) => invoke<Playlist>("playlist_update", { id, ...patch }),
  playlistDelete: (id: number) => invoke<boolean>("playlist_delete", { id }),
  playlistApply: (id: number) => invoke<Playlist>("playlist_apply", { id }),
  /** 停止轮播（清除激活列表；壁纸停在当前这张） */
  playlistStop: () => invoke<void>("playlist_stop"),
  playlistStatus: () => invoke<PlaylistStatus>("playlist_status"),
  /** 上一张（手动切换会重置轮播计时）。displayId = 只切该屏（独立模式） */
  wallpaperPrev: (displayId?: string) =>
    invoke<{ itemId: string; index: number; steps: unknown[] }>("wallpaper_prev", { displayId }),
  /** 暂停/恢复轮播的定时自动切换（不影响壁纸渲染） */
  wallpaperRotationSet: (paused: boolean) => invoke<void>("wallpaper_rotation_set", { paused }),
  /** 绑定/解绑某屏的轮播列表（null = 解绑，回到固定单张） */
  displayBindingSet: (displayId: string, playlistId: number | null) =>
    invoke<void>("display_binding_set", { displayId, playlistId }),
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
  /** 网络模式（loopback/lan/any），切换后服务热重启 */
  mcpSetNetMode: (mode: string) => invoke<McpStatus>("mcp_set_net_mode", { mode }),
  /** 我的分享（设置页管理区） */
  shareList: () => invoke<ShareRecord[]>("share_list"),
  /** 创建分享（expiresInSec 缺省/ null = 永久） */
  shareCreate: (itemId: string, expiresInSec?: number | null, note?: string) =>
    invoke<ShareRecord>("share_create", { itemId, expiresInSec: expiresInSec ?? null, note: note ?? null }),
  shareRemove: (shareId: string) => invoke<boolean>("share_remove", { shareId }),
  shareSetEnabled: (shareId: string, enabled: boolean) =>
    invoke<boolean>("share_set_enabled", { shareId, enabled }),
  /** 分享子开关（默认关：分享暴露的是内容） */
  shareServiceEnabled: () => invoke<boolean>("share_enabled_status"),
  shareSetServiceEnabled: (enabled: boolean) =>
    invoke<void>("share_set_service_enabled", { enabled }),
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
  /** 引用模式条目的源目录（批量导入不拷贝；缺省 = 常规条目） */
  sourcePath?: string;
  /** 已发布/已更新的创意工坊条目 id */
  publishedFileId?: string;
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
  /** 随机播放（洗牌队列：一轮内不重复、可回退） */
  shuffle: boolean;
}

/** 轮播运行状态（无激活列表时仅 active/paused 两字段） */
export interface PlaylistStatus {
  active: boolean;
  paused: boolean;
  /** unified=全局一份；independent=每屏各自绑定 */
  mode?: string;
  id?: number;
  name?: string;
  shuffle?: boolean;
  /** 当前项下标（0 起） */
  index?: number;
  total?: number;
  intervalSec?: number;
  /** 下次自动切换时刻（ms）；暂停中为 null */
  nextAtMs?: number | null;
}

// ---------- 多显示器 ----------

/** 某屏的轮播绑定摘要（独立模式每屏上下文） */
export interface DisplayBinding {
  playlistId: number;
  playlistName: string;
  /** 当前项下标（0 起） */
  index: number;
  total: number;
  intervalSec: number;
  /** 下次自动切换时刻（ms）；暂停中为 null */
  nextAtMs: number | null;
}

/** 一块显示器及其当前壁纸会话摘要 */
export interface DisplayInfo {
  /** 稳定显示器 id（apply/stop 的 displayId） */
  id: string;
  name: string;
  /** 逻辑坐标帧（左上原点，可为负） */
  x: number;
  y: number;
  w: number;
  h: number;
  scale: number;
  isPrimary: boolean;
  /** 当前会话的本地库条目（无会话/非库壁纸为 null） */
  itemId: string | null;
  title: string | null;
  previewUrl: string | null;
  /** 该屏的轮播绑定（未绑定为 null） */
  binding: DisplayBinding | null;
}

export interface DisplaysListResult {
  /** unified=所有屏同壁纸；independent=每屏各自指定 */
  mode: "unified" | "independent" | (string & {});
  displays: DisplayInfo[];
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

/** 快捷键动作（7 项，见 hotkeys 模块 ACTIONS 表） */
export interface HotkeyItem {
  id: string;
  /** 中文动作名（Rust i18n 键） */
  label: string;
  /** 当前绑定（规范小写写法，如 "cmd+shift+p"）；空数组 = 未绑定 */
  bindings: string[];
  /** 默认绑定（「恢复默认」用） */
  defaults: string[];
}

export interface HotkeysInfo {
  items: HotkeyItem[];
  /** 系统/菜单已占用、不建议覆盖的组合 */
  reserved: string[];
}

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
  /** 抗锯齿（库 1.3.23+） */
  aa?: "off" | "fxaa" | "msaa2" | "msaa4";
  /** 粒子质量（库 1.3.23+） */
  particles?: "off" | "low" | "medium" | "high";
  /** 后处理质量（库 1.3.23+） */
  postProcessing?: "off" | "low" | "medium" | "high";
  /** 贴图资源倍率（0.5–1，挂载期生效，改后重载） */
  resources?: number;
  /** 法线/蒙版资源倍率（0.35–1，挂载期生效，改后重载） */
  resourcesNormal?: number;
}

/** 当前全局默认（供 UI 在「跟随全局」时显示实际会用的值） */
export interface PlayConfigGlobals {
  fit: string;
  renderDpr: number;
  sceneFps: number;
  /** 库 1.3.23+ 的全局质量档位（供「跟随全局（{v}）」显示） */
  aa: string;
  particles: string;
  postProcessing: string;
  /** 贴图/法线倍率的全局值（供滑条在「跟随全局」时显示） */
  resources: number | null;
  resourcesNormal: number;
}

/** 文案 HTML 里抽出的图（WE 属性面板会渲染 <img>），由后端 we_props 解析 */
export interface PropMedia {
  src: string;
  /** <a> 包裹时的跳转链接（仅 http(s)） */
  href?: string;
  width?: string;
  height?: string;
}

export interface WebPropDef {
  name: string;
  ptype: WebPropType;
  /** 已解析的显示文案（localization 表 → WE 内建映射 → 属性名；已剥离 HTML，
   *  保留 <br> 换行；纯图横幅时为 ""，图片见 media） */
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
  /** 文案里抽出的图片（分隔图/赞助图/示意图）；渲染在标签上方 */
  media?: PropMedia[];
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

/** WE 官方素材通路状态（webwallgl 1.4.1 local-assets） */
export interface LocalAssetsStatus {
  /** 开关（持久化，默认开） */
  enabled: boolean;
  /** 本机是否探测到可用的 WE assets 根（含 materials/） */
  available: boolean;
  /** 探测到的素材根绝对路径 */
  root: string;
  /** 用户自定义素材根（空串 = Steam 库自动探测） */
  customDir: string;
  /** materials 树下 .tex 数量（素材规模） */
  texCount: number;
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
  /** 网络模式：loopback=本机 / lan=局域网 / any=任意 */
  netMode: "loopback" | "lan" | "any";
  port: number;
  token: string;
  url: string;
  urlWithToken: string;
  /** 局域网/任意模式下的本机局域网地址（如 http://192.168.1.10:7411）；本机模式或探测失败为 null */
  lanUrl: string | null;
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

/** 一条壁纸分享（shareId 即访客凭据，不可枚举） */
export interface ShareRecord {
  shareId: string;
  itemId: string;
  title: string;
  /** epoch 秒 */
  createdAt: number;
  /** epoch 秒；null = 永久 */
  expiresAt: number | null;
  enabled: boolean;
  note: string | null;
  views: number;
}

/** 更新检查结果（对齐 Rust `UpdateInfo`，来自 updater 清单） */
export interface UpdateInfo {
  current: string;
  latest: string;
  hasUpdate: boolean;
  /** 版本标题（如 "v1.2.0"） */
  name: string;
  /** 更新说明（Release 正文快照，界面按纯文本展示） */
  notes: string;
  /** RFC3339；清单没给时为空串 */
  publishedAt: string;
  /** Release 页面地址（检查失败 / 无法应用内更新时的手动下载入口） */
  htmlUrl: string;
}
