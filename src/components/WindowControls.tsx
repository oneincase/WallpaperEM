// 自绘窗口控制（最小化 / 最大化 / 关闭）：
// - macOS（variant="traffic"）：放窗口左上角，顺序与原生红绿灯一致
//   （红=关闭、黄=最小化、绿=全屏→本应用的最大化），按钮紧凑
// - Windows/Linux（默认 variant）：右上角，顺序 min/max/close（平台习惯）
// close() 走 CloseRequested → main_window.rs register_close_to_release 的
// 「关闭即释放内存」语义，与任何关闭入口完全同路，不绕过。
import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { tr } from "../lib/i18n";

/** 浏览器直开渲染器页（无 Tauri IPC）时不接线、不渲染 */
const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export function WindowControls({
  variant = "windows",
}: {
  variant?: "windows" | "traffic";
}) {
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    if (!inTauri) return;
    const win = getCurrentWindow();
    let un: (() => void) | null = null;
    const check = () => void win.isMaximized().then(setMaximized).catch(() => {});
    // 拖到顶栏自动最大化 / 还原都走 Resized 事件，图标跟着切
    void win
      .onResized(() => check())
      .then((f) => (un = f))
      .catch(() => {});
    check();
    return () => un?.();
  }, []);

  if (!inTauri) return null;
  const win = getCurrentWindow();
  const cls = variant === "traffic" ? "win-ctrl win-ctrl-traffic" : "win-ctrl";

  const minBtn = (
    <button key="min" aria-label={tr("最小化")} className={cls} onClick={() => void win.minimize()}>
      <svg width="10" height="10" viewBox="0 0 10 10">
        <path d="M1 5.5h8" stroke="currentColor" strokeWidth="1" />
      </svg>
    </button>
  );
  const maxBtn = (
    <button
      key="max"
      aria-label={maximized ? tr("还原") : tr("最大化")}
      className={cls}
      onClick={() => void win.toggleMaximize()}
    >
      {maximized ? (
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1">
          <rect x="0.5" y="2.5" width="7" height="7" rx="1.2" />
          <path d="M2.5 2.5v-1a1 1 0 0 1 1-1h5a1 1 0 0 1 1 1v5a1 1 0 0 1-1 1h-1" />
        </svg>
      ) : (
        <svg width="10" height="10" viewBox="0 0 10 10" fill="none" stroke="currentColor" strokeWidth="1">
          <rect x="0.5" y="0.5" width="9" height="9" rx="1.2" />
        </svg>
      )}
    </button>
  );
  const closeBtn = (
    <button
      key="close"
      aria-label={tr("关闭")}
      className={`${cls} win-ctrl-close`}
      onClick={() => void win.close()}
    >
      <svg width="10" height="10" viewBox="0 0 10 10" stroke="currentColor" strokeWidth="1">
        <path d="M1.2 1.2l7.6 7.6M8.8 1.2L1.2 8.8" />
      </svg>
    </button>
  );

  return (
    <div className="flex h-full items-stretch">
      {/* macOS：红绿灯次序 关闭(红) → 最小化(黄) → 最大化(绿) */}
      {variant === "traffic" ? [closeBtn, minBtn, maxBtn] : [minBtn, maxBtn, closeBtn]}
    </div>
  );
}
