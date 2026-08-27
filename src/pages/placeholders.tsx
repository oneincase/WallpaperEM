import { PlaceholderPage } from "./PlaceholderPage";

export function WorkshopPage() {
  return (
    <PlaceholderPage
      title="工坊"
      desc="Steam 登录后浏览 / 搜索 / 筛选 Wallpaper Engine 创意工坊壁纸（T1）"
      hint="T1 · Rust Steam 客户端"
    />
  );
}

export function DownloadsPage() {
  return (
    <PlaceholderPage
      title="下载"
      desc="下载队列、进度与 Steam Guard 验证码交互（DepotDownloader 内部集成，T2）"
      hint="T2 · 下载引擎"
    />
  );
}

export function LibraryPage() {
  return (
    <PlaceholderPage
      title="本地库"
      desc="已下载壁纸管理：过滤、删除、打开目录、应用到桌面（T4）"
      hint="T4 · 本地库"
    />
  );
}

export function FavoritesPage() {
  return (
    <PlaceholderPage
      title="收藏"
      desc="收藏的壁纸列表（本地收藏表）"
      hint="T4 · 收藏"
    />
  );
}
