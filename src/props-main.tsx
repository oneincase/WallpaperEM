// 独立「壁纸设置」窗口入口（托盘右键 → 壁纸设置）。
//
// 不挂载整个 App（侧边栏/页面/路由），只渲染配置面板本身 —— 这是与主窗口
// React 弹窗方案的区别：托盘唤起时不带出主界面，窗口里只有设置。
// itemId 经 URL query 传入（?item=<id>），由 Rust 开窗时决定。
import ReactDOM from "react-dom/client";
import { useEffect, useState } from "react";
import { WallpaperPropsPanel } from "./components/WallpaperPropsModal";
import { ResizeHandles } from "./components/ResizeHandles";
import { api } from "./api/steam";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { pushLocaleToBackend, tr, useLocale } from "./lib/i18n";
import { useWindowRounded } from "./lib/platform";
import { useWallpaperBackdrop } from "./hooks/useWallpaperBackdrop";
import "./index.css";

// 同主窗口入口：全屏蔽浏览器默认右键菜单（详见 main.tsx 注释）
document.addEventListener("contextmenu", (e) => e.preventDefault());

function PropsWindow() {
  // 独立窗口也要订阅语言：切换后标题与文案跟着变（窗口标题由 Rust 设，见下）
  useLocale();
  // 与主窗口同一套玻璃：当前壁纸模糊背景 + 按封面亮度自适应 tint
  const backdrop = useWallpaperBackdrop();
  // 最大化/全屏时去圆角与描边（同主窗口壳）
  const rounded = useWindowRounded();
  const [itemId] = useState(() => new URLSearchParams(location.search).get("item") ?? "");
  const [title, setTitle] = useState(itemId);

  // 窗口标题（任务栏/窗口列表里显示的那个）
  useEffect(() => {
    document.title = tr("壁纸设置");
  }, []);

  // 该窗口可能先于主窗口打开（托盘入口），自己推一次语言，保证 Rust 文案一致
  useEffect(() => {
    pushLocaleToBackend();
  }, []);

  useEffect(() => {
    if (!itemId) return;
    // 标题异步查；查不到时组件内部本就回退 itemId
    api
      .libraryItemTitle(itemId)
      .then(setTitle)
      .catch(() => {});
  }, [itemId]);

  if (!itemId) {
    return (
      <div className="flex h-full items-center justify-center text-[13px] text-[var(--text-2)]">
        {tr("缺少壁纸 ID")}
      </div>
    );
  }

  // 关闭面板 = 关闭整个窗口；embedded：铺满窗口而非卡片弹层。
  // 整面板（含玻璃背景）带 props-slide 动画从右缘滑入 —— 窗口本体贴屏幕
  // 右侧（props_window.rs），观感即「从右侧向左划出」
  return (
    <div
      className={`props-slide relative h-screen overflow-hidden ${
        rounded ? "rounded-[12px] ring-1 ring-[var(--card-border)]" : ""
      } ${backdrop ? "" : "bg-[rgba(18,18,22,0.92)]"}`}
    >
      {/* 无边框窗口的边缘缩放把手（Win/Linux；macOS 靠系统） */}
      <ResizeHandles enabled={rounded} />
      <div
        className="app-backdrop"
        aria-hidden
        style={backdrop ? ({ "--backdrop-img": `url("${backdrop}")` } as React.CSSProperties) : undefined}
      />
      <div className="relative z-10 h-full">
        <WallpaperPropsPanel
          itemId={itemId}
          title={title}
          embedded
          onClose={() => void getCurrentWindow().close()}
        />
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(<PropsWindow />);
