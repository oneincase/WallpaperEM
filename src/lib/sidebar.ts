// 侧边栏透明度（--sidebar-alpha）共享 helper
// localStorage 持久化 + 应用到 <html> 的 CSS 变量
const SIDEBAR_ALPHA_KEY = "we.sidebar.opacity";

/** 默认透明度（浅色 0.5 / 深色 0.55，但统一用 0.5 作为持久化默认） */
export const SIDEBAR_ALPHA_DEFAULT = 0.5;

export function getSidebarAlpha(): number {
  try {
    const v = Number(localStorage.getItem(SIDEBAR_ALPHA_KEY));
    if (!Number.isNaN(v) && v >= 0.2 && v <= 1) return v;
  } catch {
    /* ignore */
  }
  return SIDEBAR_ALPHA_DEFAULT;
}

export function setSidebarAlpha(alpha: number): number {
  const clamped = Math.min(1, Math.max(0.2, alpha));
  try {
    localStorage.setItem(SIDEBAR_ALPHA_KEY, String(clamped));
  } catch {
    /* ignore */
  }
  applySidebarAlpha(clamped);
  return clamped;
}

/** 把 alpha 应用到 <html> 的 --sidebar-alpha */
export function applySidebarAlpha(alpha: number) {
  document.documentElement.style.setProperty("--sidebar-alpha", String(alpha));
}
