// 顶部导航条：拖拽区 + 居中胶囊导航 + 窗口控制。
// 纯无边框玻璃面板：窗口自身不带任何 chrome（logo/名称/红绿灯全无），
// 顶栏只剩居中胶囊；关闭/最小化用自绘 WindowControls（全平台一致）。
// - 胶囊 = 「方形段 + 部分圆角」容器，段间 2px 小间隔（锯齿观感）；
// - 悬浮/选中横向拉长露出文字标签（grid 0fr→1fr，动画见 index.css .pill-*），
//   选中段加深磨砂并微微隆起（32→38px）；取消选中/移出悬浮即缩回图标方段。
import { type ReactNode } from "react";
import { tr } from "../lib/i18n";
import { useOs } from "../lib/platform";
import {
  IconHome,
  IconGrid,
  IconDownload,
  IconLibrary,
  IconHeart,
  IconMonitor,
  IconShare,
  IconGear,
  IconKeyboard,
} from "./icons";
import { WindowControls } from "./WindowControls";

export type PageId =
  | "home"
  | "workshop"
  | "downloads"
  | "library"
  | "favorites"
  | "displays"
  | "shares"
  | "hotkeys"
  | "settings";

const NAV: { id: PageId; label: string; icon: ReactNode }[] = [
  { id: "home", label: "发现", icon: <IconHome /> },
  { id: "workshop", label: "工坊", icon: <IconGrid /> },
  { id: "downloads", label: "下载", icon: <IconDownload /> },
  { id: "library", label: "本地库", icon: <IconLibrary /> },
  { id: "favorites", label: "收藏", icon: <IconHeart /> },
  { id: "displays", label: "显示器", icon: <IconMonitor /> },
  { id: "shares", label: "分享", icon: <IconShare /> },
  { id: "hotkeys", label: "快捷键", icon: <IconKeyboard /> },
  { id: "settings", label: "设置", icon: <IconGear /> },
];

export function TopBar({
  activeId,
  onNavigate,
}: {
  /** 详情抽屉打开时传 null：所有段退回未选中态 */
  activeId: PageId | null;
  onNavigate: (p: PageId) => void;
}) {
  // macOS：窗口控制放左上角、顺序照红绿灯（红=关闭 → 黄=最小化 → 绿=最大化）；
  // Windows/Linux：右上角，顺序 min/max/close（平台习惯）。浏览器直开不渲染。
  // useOs 首帧同步判平台，窗口控制不会先闪一帧 macOS 的左侧布局
  const isMac = useOs() === "macos";

  return (
    <header
      data-tauri-drag-region
      className="relative z-10 flex h-[52px] shrink-0 items-center px-3"
    >
      {isMac && <WindowControls variant="traffic" />}

      <div data-tauri-drag-region className="flex-1 self-stretch" />

      {/* 居中胶囊：绝对定位居中，段的拉长/缩回不会引起整条左右位移 */}
      <nav
        className="absolute left-1/2 top-1/2 flex h-11 -translate-x-1/2 -translate-y-1/2 items-center gap-[2px] rounded-[12px] bg-[var(--sidebar)] px-[3px] ring-1 ring-[var(--card-border)]"
        aria-label={tr("导航")}
      >
        {NAV.map((item) => {
          const active = activeId === item.id;
          return (
            <button
              key={item.id}
              data-active={active}
              onClick={() => onNavigate(item.id)}
              className={`pill-item flex items-center rounded-[8px] px-2 font-medium ${
                active
                  ? "h-[38px] bg-[var(--glass-active)] text-[var(--text-1)] shadow-[0_2px_10px_rgba(0,0,0,0.35)] ring-1 ring-white/10"
                  : "h-8 text-[var(--text-2)] hover:bg-[var(--glass-hover)] hover:text-[var(--text-1)]"
              }`}
            >
              {item.icon}
              <span className="pill-label">
                <span className="pill-clip">
                  <span className="pl-1.5 pr-0.5 text-[13px]">{tr(item.label)}</span>
                </span>
              </span>
            </button>
          );
        })}
      </nav>

      <div data-tauri-drag-region className="flex-1 self-stretch" />

      {!isMac && <WindowControls />}
    </header>
  );
}
