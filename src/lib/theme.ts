// 全局主题（浅色/深色/跟随系统）的存储与应用。
//
// 存储在 localStorage —— Tauri 各 WebView 共享同一数据仓库，所以主窗口写的
// 主题，独立的「壁纸设置」窗口（props-main）也能读到；storage 事件跨窗口触发，
// 主窗口里改主题时其他已开窗口同步跟着变，不用重开。
import { useEffect } from "react";

export type Theme = "system" | "light" | "dark";

export const THEME_STORAGE_KEY = "we.theme";

export function readStoredTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_STORAGE_KEY);
    if (t === "light" || t === "dark" || t === "system") return t;
  } catch {
    /* localStorage 不可用时按跟随系统 */
  }
  return "system";
}

/** 应用到 <html data-theme>；system 交给 CSS 的 prefers-color-scheme 媒体查询 */
export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
}

/** 非主窗口（如独立壁纸设置窗口）：读全局主题并跟随后续变更 */
export function useGlobalTheme() {
  useEffect(() => {
    const sync = () => applyTheme(readStoredTheme());
    sync();
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);
}
