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
  tag?: string;
  sort?: string;
  page?: number;
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
  createdAt: number;
  startedAt?: number;
  finishedAt?: number;
}

export interface DownloadToolStatus {
  installed: boolean;
  path?: string;
  sizeMB?: number;
  version?: string;
}

export const api = {
  // 工坊
  workshopSearch: (params: WorkshopSearchParams) =>
    invoke<WorkshopSearchResult>("workshop_search", { params }),
  workshopRandom: (sort?: string) =>
    invoke<WorkshopSearchResult>("workshop_random", { sort }),
  workshopItem: (id: string) => invoke<WorkshopItem | null>("workshop_item", { id }),
  // 下载
  downloadToolStatus: () => invoke<DownloadToolStatus>("download_tool_status"),
  downloadCredentialsSet: (username: string, password: string) =>
    invoke<{ ok: boolean; username: string }>("download_credentials_set", {
      username,
      password,
    }),
  downloadCredentialsStatus: () =>
    invoke<{ configured: boolean; username?: string }>("download_credentials_status"),
  downloadEnqueue: (itemId: string) => invoke<number>("download_enqueue", { itemId }),
  downloadList: () => invoke<DownloadTask[]>("download_list"),
  downloadCancel: (id: number) => invoke<boolean>("download_cancel", { id }),
  downloadRetry: (id: number) => invoke<boolean>("download_retry", { id }),
  downloadSubmitGuard: (id: number, code: string) =>
    invoke<boolean>("download_submit_guard", { id, code }),
  downloadRemove: (id: number) => invoke<boolean>("download_remove", { id }),
  downloadClearFinished: () => invoke<number>("download_clear_finished"),
  // 本地库 / 收藏 / 壁纸
  libraryList: (type?: WallpaperType | "") => invoke<LibraryItem[]>("library_list", { type }),
  libraryDelete: (itemId: string) => invoke<boolean>("library_delete", { itemId }),
  libraryOpenFolder: (itemId: string) => invoke<boolean>("library_open_folder", { itemId }),
  libraryImportFromWeb: (webDataDir: string) =>
    invoke<{ imported: number; skipped: number }>("library_import_from_web", { webDataDir }),
  wallpaperApplyItem: (itemId: string) => invoke<void>("wallpaper_apply_item", { itemId }),
  libraryPreview: (itemId: string) => invoke<WallpaperConfig>("library_preview", { itemId }),
  wallpaperApply: (config: WallpaperConfig, displayId?: string) =>
    invoke<void>("wallpaper_apply", { config, displayId }),
  wallpaperStop: (displayId?: string) => invoke<void>("wallpaper_stop", { displayId }),
  wallpaperListSessions: () => invoke<{ active: boolean; paused: boolean; sessions: Record<string, WallpaperConfig> }>("wallpaper_list_sessions"),
  wallpaperActiveItems: () => invoke<string[]>("wallpaper_active_items"),
  wallpaperPauseAll: () => invoke<void>("wallpaper_pause_all"),
  wallpaperResumeAll: () => invoke<void>("wallpaper_resume_all"),
  wallpaperInteractiveSet: (enabled: boolean) => invoke<void>("wallpaper_interactive_set", { enabled }),
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
