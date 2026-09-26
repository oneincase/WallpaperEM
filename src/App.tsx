import { useEffect, useRef, useState } from "react";
import { HomePage } from "./pages/Home";
import { WorkshopPage } from "./pages/Workshop";
import { DetailPage } from "./pages/Detail";
import { DownloadsPage } from "./pages/Downloads";
import { LibraryPage } from "./pages/Library";
import { FavoritesPage } from "./pages/Favorites";
import { DisplaysPage } from "./pages/Displays";
import { SharesPage } from "./pages/Shares";
import { HotkeysPage } from "./pages/Hotkeys";
import { SettingsPage } from "./pages/Settings";
import { TopBar, type PageId } from "./components/TopBar";
import { GuardDialogs } from "./components/GuardDialogs";
import { ResizeHandles } from "./components/ResizeHandles";
import { useWallpaperBackdrop } from "./hooks/useWallpaperBackdrop";
import { useWindowRounded } from "./lib/platform";
import { readState, writeState } from "./lib/cache-snapshots";
import { pushLocaleToBackend, useLocale } from "./lib/i18n";

const PAGE_STORAGE_KEY = "nav.page";

/**
 * 恢复上次所在页面。窗口被释放后重建是全新 JS 上下文，不持久化就必然落回
 * 发现页 —— 而发现页恰好是最慢的链路（workshop_random 三次串行网络请求）。
 * 「设置」与「分享」刻意不恢复：那是一次性操作页，下次进来想看的多半是内容。
 */
function readInitialPage(): PageId {
  const p = readState<string>(PAGE_STORAGE_KEY, "home");
  const valid: PageId[] = [
    "home",
    "workshop",
    "downloads",
    "library",
    "favorites",
    "displays",
  ];
  return (valid as string[]).includes(p) ? (p as PageId) : "home";
}

/** 浏览器直开渲染器页（无 Tauri IPC）时不接窗口事件 —— 见 lib/platform.ts */

export default function App() {
  return <Shell />;
}

function Shell() {
  // 界面语言：**只在根组件订阅一次** —— 根重渲染会带整棵树一起重渲染，页面里的
  // tr() 才会重新取词。子组件因此不必各自订阅语言（本项目没有 React.memo 截断渲染）。
  useLocale();
  const [page, setPage] = useState<PageId>(readInitialPage);
  // 详情抽屉：detailId 非 null 时抽屉挂载；shown 控制滑入/滑出（关闭时先滑出、
  // 动画结束再卸载，保证「向右滑动隐藏」可见）
  const [detailId, setDetailId] = useState<string | null>(null);
  const [detailShown, setDetailShown] = useState(false);
  const detailCloseTimer = useRef<number | null>(null);
  // 最大化/全屏时根容器去圆角（透明窗口 + CSS 圆角，铺满时不能露角）
  const rounded = useWindowRounded();
  // 页内玻璃背景 + 自适应 tint（见 hooks/useWallpaperBackdrop.ts）
  const backdrop = useWallpaperBackdrop();

  // 冻结自检：系统睡眠/合盖后 WebKit 可能恢复出一个「卡死」的页面（定时器全部
  // 停摆）。定时器恢复触发时若发现实际流逝时间远超定时周期，说明页面曾被长时间
  // 挂起——强制刷新自身，回到干净状态。
  useEffect(() => {
    let last = Date.now();
    const t = window.setInterval(() => {
      const now = Date.now();
      if (now - last > 30_000) {
        window.location.reload();
        return;
      }
      last = now;
    }, 5_000);
    return () => window.clearInterval(t);
  }, []);

  const navigate = (p: PageId) => {
    if (detailCloseTimer.current) {
      clearTimeout(detailCloseTimer.current);
      detailCloseTimer.current = null;
    }
    setDetailId(null);
    setDetailShown(false);
    setPage(p);
    writeState(PAGE_STORAGE_KEY, p);
  };

  // 打开详情抽屉：从右侧滑入（面板先以滑出位挂载，下一帧再过渡到滑入位）
  const openDetail = (id: string) => {
    if (detailCloseTimer.current) {
      clearTimeout(detailCloseTimer.current);
      detailCloseTimer.current = null;
    }
    if (detailId === null) {
      setDetailId(id);
      setDetailShown(false);
      requestAnimationFrame(() => requestAnimationFrame(() => setDetailShown(true)));
    } else {
      // 已打开时切换条目：面板保持滑入位，仅换内容
      setDetailShown(true);
      setDetailId(id);
    }
  };

  // 关闭详情抽屉：先向右滑出，动画结束后再卸载
  const closeDetail = () => {
    setDetailShown(false);
    if (detailCloseTimer.current) clearTimeout(detailCloseTimer.current);
    detailCloseTimer.current = window.setTimeout(() => {
      detailCloseTimer.current = null;
      setDetailId(null);
    }, 320);
  };

  // Esc 关闭详情抽屉
  useEffect(() => {
    if (detailId === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeDetail();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- closeDetail 依赖 detailId，与之同步重建
  }, [detailId]);

  // 玻璃圆角壳（全平台无边框）：透明窗口 + CSS 圆角 + 白描边交代边界；
  // 最大化/全屏时去圆角。macOS 的窗口阴影由 Tauri shadow(true) 提供
  const chrome = rounded
    ? "rounded-[12px] overflow-hidden ring-1 ring-[var(--card-border)]"
    : "overflow-hidden";

  return (
    <div className={`relative flex h-full flex-col ${chrome} ${backdrop ? "" : "bg-[rgba(18,18,22,0.92)]"}`}>
      {/* 无边框窗口的边缘缩放把手（Win/Linux；macOS 靠系统） */}
      <ResizeHandles enabled={rounded} />
      {/* 页内玻璃背景：当前壁纸高斯模糊 + 深色 tint（样式见 index.css .app-backdrop）。
          绝对定位垫底，TopBar 与内容区各抬一层（z-10） */}
      <div
        className="app-backdrop"
        aria-hidden
        style={backdrop ? ({ "--backdrop-img": `url("${backdrop}")` } as React.CSSProperties) : undefined}
      />

      <TopBar activeId={detailId ? null : page} onNavigate={navigate} />

      <div className="relative z-10 flex-1 flex flex-col min-h-0">
        {page === "home" ? (
          <HomePage onOpenDetail={openDetail} />
        ) : page === "workshop" ? (
          <WorkshopPage onOpenDetail={openDetail} />
        ) : page === "downloads" ? (
          <DownloadsPage />
        ) : page === "library" ? (
          <LibraryPage onOpenDetail={openDetail} />
        ) : page === "favorites" ? (
          <FavoritesPage onOpenDetail={openDetail} />
        ) : page === "displays" ? (
          <DisplaysPage onNavigate={navigate} />
        ) : page === "shares" ? (
          <SharesPage onNavigate={navigate} />
        ) : page === "hotkeys" ? (
          <HotkeysPage />
        ) : (
          <SettingsPage />
        )}

        {/* 详情页以右侧抽屉展示：遮罩淡入 + 面板向左滑入覆盖，关闭时向右滑回隐藏。
            底下列表保持挂载，关闭抽屉后滚动位置/数据不重置 */}
        {detailId && (
          <>
            <div
              onClick={closeDetail}
              className={`absolute inset-0 z-20 bg-black/25 transition-opacity duration-300 ${
                detailShown ? "opacity-100" : "opacity-0"
              }`}
            />
            <div
              className={`glass-panel absolute inset-y-0 right-0 z-30 w-[420px] max-w-[88%] shadow-2xl ring-1 ring-inset ring-[var(--card-border)] transition-transform duration-300 ease-out ${
                detailShown ? "translate-x-0" : "translate-x-full"
              }`}
            >
              <DetailPage id={detailId} onBack={closeDetail} />
            </div>
          </>
        )}
      </div>

      {/* 全局 Steam Guard 验证码 / 手机确认弹窗（不依附下载页） */}
      <GuardDialogs />
    </div>
  );
}
