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

export default function App() {
  return <Shell />;
}

function Shell() {
  const [page, setPage] = useState<PageId>("home");
  const [detailId, setDetailId] = useState<string | null>(null);

  const groups: { group: string; items: typeof NAV }[] = ["浏览", "库", "系统"].map((g) => ({
    group: g,
    items: NAV.filter((n) => n.group === g),
  }));

  const navigate = (p: PageId) => {
    setDetailId(null);
    setPage(p);
  };

  return (
    <div className="flex h-full">
      {/* 侧边栏 */}
      <aside className="w-60 shrink-0 flex flex-col bg-[var(--sidebar)] border-r border-[var(--separator)]">
        <div data-tauri-drag-region className="h-10 shrink-0" />

        <div className="px-4 pt-2 pb-2 flex items-center gap-2">
          <div className="h-6 w-6 rounded-[7px] overflow-hidden shadow-sm">
            <img src="/icon/icon_32x32.png" alt="" className="h-full w-full object-contain" />
          </div>
          <span className="text-[13.5px] font-semibold tracking-tight">WallpaperEM</span>
        </div>

        <nav className="flex-1 overflow-y-auto px-3 py-2 space-y-4">
          {groups.map(({ group, items }) => (
            <div key={group}>
              <div className="px-2 pb-1 text-[11px] font-semibold uppercase tracking-wide text-[var(--text-2)]/70">
                {group}
              </div>
              <div className="space-y-0.5">
                {items.map((item) => (
                  <button
                    key={item.id}
                    onClick={() => navigate(item.id)}
                    className={`w-full flex items-center gap-2.5 rounded-[7px] px-2.5 py-[5px] text-[13.5px] transition-colors ${
                      page === item.id && !detailId
                        ? "bg-[var(--accent)] text-white shadow-sm"
                        : "text-[var(--text-1)] hover:bg-black/5 dark:hover:bg-white/8"
                    }`}
                  >
                    {item.icon}
                    {item.label}
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
