// 无边框窗口的边缘缩放把手（Windows/Linux）。
//
// GTK 无 CSD 的窗口没有原生缩放热区；Windows 那圈隐形 thickframe 边框是否
// 盖得住 WebView 也不可赌 —— 统一在页面边缘垫一圈隐形把手，按下即把缩放
// 交给系统（startResizeDragging → tao 的拖拽缩放），与原生行为同路。
// macOS 不挂：tao 的 drag_resize_window 是空实现（系统自有一套边缘缩放），
// 挂了也只是压住内容边角的死区。最大化/全屏时收起（enabled=false），
// 与原生「铺满后无缩放光标」一致。
//
// 位置约定：边条 5px、四角 12px 方块压在边条之上（对角方向优先）；
// 边条与拖拽区/内容的分工和原生一样 —— 最外圈是缩放，往里才是拖动与内容。
import type { CSSProperties, MouseEvent } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useOs } from "../lib/platform";

/** 与 tauri_runtime::ResizeDirection 的 serde 变体名逐字一致 */
type Dir =
  | "North"
  | "South"
  | "East"
  | "West"
  | "NorthEast"
  | "NorthWest"
  | "SouthEast"
  | "SouthWest";

const HANDLE_STYLE: CSSProperties = { position: "absolute", pointerEvents: "auto" };

const EDGE: { dir: Dir; cls: string }[] = [
  { dir: "North", cls: "top-0 left-3 right-3 h-[5px] cursor-ns-resize" },
  { dir: "South", cls: "bottom-0 left-3 right-3 h-[5px] cursor-ns-resize" },
  { dir: "West", cls: "left-0 top-3 bottom-3 w-[5px] cursor-ew-resize" },
  { dir: "East", cls: "right-0 top-3 bottom-3 w-[5px] cursor-ew-resize" },
];

const CORNER: { dir: Dir; cls: string }[] = [
  { dir: "NorthWest", cls: "top-0 left-0 h-3 w-3 cursor-nwse-resize" },
  { dir: "NorthEast", cls: "top-0 right-0 h-3 w-3 cursor-nesw-resize" },
  { dir: "SouthWest", cls: "bottom-0 left-0 h-3 w-3 cursor-nesw-resize" },
  { dir: "SouthEast", cls: "bottom-0 right-0 h-3 w-3 cursor-nwse-resize" },
];

export function ResizeHandles({ enabled = true }: { enabled?: boolean }) {
  const os = useOs();
  if (os === "macos" || !enabled) return null;

  const start = (dir: Dir) => (e: MouseEvent) => {
    if (e.button !== 0) return;
    // 防止按下时选中内容/触发页面内拖拽语义
    e.preventDefault();
    void getCurrentWindow()
      .startResizeDragging(dir)
      .catch(() => {});
  };

  return (
    <div className="pointer-events-none absolute inset-0 z-[60]" aria-hidden>
      {[...EDGE, ...CORNER].map(({ dir, cls }) => (
        <div key={dir} className={cls} style={HANDLE_STYLE} onMouseDown={start(dir)} />
      ))}
    </div>
  );
}
