import { useState, type ReactNode } from "react";
import { HomePage } from "./pages/Home";
import { WorkshopPage } from "./pages/Workshop";
import { DetailPage } from "./pages/Detail";
import { DownloadsPage } from "./pages/Downloads";
import { LibraryPage } from "./pages/Library";
import { FavoritesPage } from "./pages/Favorites";
import { SettingsPage } from "./pages/Settings";
import {
  IconHome,
  IconGrid,
  IconDownload,
  IconLibrary,
  IconHeart,
  IconGear,
  IconSidebarCollapse,
  IconSidebarExpand,
} from "./components/icons";

type PageId = "home" | "workshop" | "downloads" | "library" | "favorites" | "settings";

const NAV: { id: PageId; label: string; icon: ReactNode; group: string }[] = [
  { id: "home", label: "发现", icon: <IconHome />, group: "浏览" },
  { id: "workshop", label: "工坊", icon: <IconGrid />, group: "浏览" },
  { id: "downloads", label: "下载", icon: <IconDownload />, group: "浏览" },
  { id: "library", label: "本地库", icon: <IconLibrary />, group: "库" },
  { id: "favorites", label: "收藏", icon: <IconHeart />, group: "库" },
  { id: "settings", label: "设置", icon: <IconGear />, group: "系统" },
];

const SIDEBAR_STORAGE_KEY = "we.sidebar.collapsed";

function readInitialCollapsed(): boolean {
  try {
    return localStorage.getItem(SIDEBAR_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

export default function App() {
  return <Shell />;
}

function Shell() {
  const [page, setPage] = useState<PageId>("home");
  const [detailId, setDetailId] = useState<string | null>(null);
  // 侧边栏是否收缩成图标栏；由用户手动切换，并持久化
  const [collapsed, setCollapsed] = useState<boolean>(readInitialCollapsed);

  const groups: { group: string; items: typeof NAV }[] = ["浏览", "库", "系统"].map((g) => ({
    group: g,
    items: NAV.filter((n) => n.group === g),
  }));

  const navigate = (p: PageId) => {
    setDetailId(null);
    setPage(p);
  };

  const toggleCollapsed = () => {
    setCollapsed((c) => {
      const next = !c;
      try {
        localStorage.setItem(SIDEBAR_STORAGE_KEY, next ? "1" : "0");
      } catch {
        /* ignore */
      }
      return next;
    });
  };

  const width = collapsed ? "w-[52px]" : "w-60";

  return (
    <div className="flex h-full">
      {/* 侧边栏 */}
      <aside
        className={`${width} shrink-0 flex flex-col bg-[var(--sidebar)] border-r border-[var(--separator)] transition-[width] duration-200 ease-out`}
      >
        <div data-tauri-drag-region className="h-10 shrink-0" />

        {/* 顶部：展开态显示 logo+名称+收缩按钮；收缩态 logo 悬浮变展开按钮 */}
        <div
          className={
            collapsed
              ? "px-3 pt-2 pb-2 flex justify-center"
              : "px-3 pt-2 pb-2 flex items-center gap-2"
          }
        >
          {collapsed ? (
            <button
              onClick={toggleCollapsed}
              title="展开侧边栏"
              aria-label="展开侧边栏"
              className="group relative flex h-6 w-6 items-center justify-center rounded-[7px] overflow-hidden text-[var(--text-2)] transition-colors hover:bg-black/5 hover:text-[var(--text-1)] dark:hover:bg-white/8"
            >
              <img
                src="/icon/icon_32x32.png"
                alt=""
                className="h-full w-full object-contain transition-opacity group-hover:opacity-0"
              />
              <span className="absolute inset-0 flex items-center justify-center opacity-0 transition-opacity group-hover:opacity-100">
                <IconSidebarExpand />
              </span>
            </button>
          ) : (
            <>
              <div className="h-6 w-6 shrink-0 rounded-[7px] overflow-hidden shadow-sm">
                <img src="/icon/icon_32x32.png" alt="" className="h-full w-full object-contain" />
              </div>
              <span className="text-[13.5px] font-semibold tracking-tight">WallpaperEM</span>
              <button
                onClick={toggleCollapsed}
                title="收起侧边栏"
                aria-label="收起侧边栏"
                className="ml-auto flex h-6 w-6 items-center justify-center rounded-[6px] text-[var(--text-2)] transition-colors hover:bg-black/5 hover:text-[var(--text-1)] dark:hover:bg-white/8"
              >
                <IconSidebarCollapse />
              </button>
            </>
          )}
        </div>

        <nav className="flex-1 overflow-y-auto px-3 py-2 space-y-4">
          {groups.map(({ group, items }) => (
            <div key={group}>
              {!collapsed && (
                <div className="px-2 pb-1 text-[11px] font-semibold uppercase tracking-wide text-[var(--text-2)]/70">
                  {group}
                </div>
              )}
              <div className="space-y-0.5">
                {items.map((item) => (
                  <button
                    key={item.id}
                    onClick={() => navigate(item.id)}
                    title={collapsed ? item.label : undefined}
                    className={`${
                      collapsed ? "w-full justify-center" : "w-full justify-start gap-2.5 px-2.5"
                    } flex items-center rounded-[7px] py-[5px] text-[13.5px] transition-colors ${
                      page === item.id && !detailId
                        ? "bg-[var(--accent)] text-white shadow-sm"
                        : "text-[var(--text-1)] hover:bg-black/5 dark:hover:bg-white/8"
                    }`}
                  >
                    {item.icon}
                    {!collapsed && item.label}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </nav>
      </aside>

      {/* 内容区 */}
      <main className="flex-1 flex flex-col bg-[var(--content)] min-w-0">
        <header data-tauri-drag-region className="h-10 shrink-0 flex items-center px-4">
          <div data-tauri-drag-region className="flex-1" />
        </header>
        <div className="flex-1 flex flex-col min-h-0 relative">
          {page === "home" ? (
            <HomePage onOpenDetail={setDetailId} />
          ) : page === "workshop" ? (
            <WorkshopPage onOpenDetail={setDetailId} />
          ) : page === "downloads" ? (
            <DownloadsPage />
          ) : page === "library" ? (
            <LibraryPage onOpenDetail={setDetailId} />
          ) : page === "favorites" ? (
            <FavoritesPage onOpenDetail={setDetailId} />
          ) : (
            <SettingsPage />
          )}

          {/* 详情页以覆盖层展示，底下列表保持挂载，返回时不刷新/不重置 */}
          {detailId && (
            <div className="absolute inset-0 z-20 overflow-y-auto bg-[var(--content)]">
              <DetailPage id={detailId} onBack={() => setDetailId(null)} />
            </div>
          )}
        </div>
      </main>
    </div>
  );
}
