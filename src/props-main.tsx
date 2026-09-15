// 独立「壁纸设置」窗口入口（托盘右键 → 壁纸设置）。
//
// 不挂载整个 App（侧边栏/页面/路由），只渲染配置面板本身 —— 这是与主窗口
// React 弹窗方案的区别：托盘唤起时不带出主界面，窗口里只有设置。
// itemId 经 URL query 传入（?item=<id>），由 Rust 开窗时决定。
import ReactDOM from "react-dom/client";
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { WallpaperPropsPanel } from "./components/WallpaperPropsModal";
import { api } from "./api/steam";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { pushLocaleToBackend, tr, useLocale } from "./lib/i18n";
import { useGlobalTheme } from "./lib/theme";
import "./index.css";

function PropsWindow() {
  // 独立窗口也要订阅语言：切换后标题与文案跟着变（窗口标题由 Rust 设，见下）
  useLocale();
  // 跟随主窗口设置的全局主题（localStorage 跨 WebView 共享 + storage 事件同步）
  useGlobalTheme();
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

  // 平台标记给 CSS：macOS 有原生 vibrancy、Windows 有 acrylic，都是系统级磨砂，
  // props-tint 才能降到 0.78
  // 透出模糊；其他平台保持 0.88 高 alpha 兜底可读性（见 index.css .props-tint）
  useEffect(() => {
    invoke<{ os: string }>("app_info")
      .then((i) => {
        document.documentElement.dataset.os = i.os;
      })
      .catch(() => {});
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

  // 关闭面板 = 关闭整个窗口；embedded：铺满窗口而非卡片弹层
  return (
    <WallpaperPropsPanel
      itemId={itemId}
      title={title}
      embedded
      onClose={() => void getCurrentWindow().close()}
    />
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(<PropsWindow />);
