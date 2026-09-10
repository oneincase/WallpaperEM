// 独立「壁纸设置」窗口入口（托盘右键 → 壁纸设置）。
//
// 不挂载整个 App（侧边栏/页面/路由），只渲染配置面板本身 —— 这是与主窗口
// React 弹窗方案的区别：托盘唤起时不带出主界面，窗口里只有设置。
// itemId 经 URL query 传入（?item=<id>），由 Rust 开窗时决定。
import ReactDOM from "react-dom/client";
import { useEffect, useState } from "react";
import { WallpaperPropsPanel } from "./components/WallpaperPropsModal";
import { api } from "./api/steam";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./index.css";

function PropsWindow() {
  const [itemId] = useState(() => new URLSearchParams(location.search).get("item") ?? "");
  const [title, setTitle] = useState(itemId);

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
        缺少壁纸 ID
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
