// 侧边栏透明度（--sidebar-alpha）共享 helper
// localStorage 持久化 + 应用到 <html> 的 CSS 变量
//
// ⚠️ applySidebarAlpha 写的是 <html> 行内样式，优先级高于 index.css 里的
// `--sidebar-alpha`。也就是说只要用户设置过一次（或本模块应用过默认值），
// CSS 里那份就再也不生效 —— 想改默认观感必须改这里的常量，改 CSS 无效。
const SIDEBAR_ALPHA_KEY = "we.sidebar.opacity";
/** 迁移标记：老默认值（0.5/0.55）抬到新默认只做一次 */
const MIGRATED_KEY = "we.sidebar.opacity.migrated";

/**
 * 默认透明度：55%。
 *
 * 历史：0.5/0.55 → 0.72（浅色壁纸透穿、导航比内容区亮）→ 回到 0.55
 * （设置页的调节入口已注释隐藏，默认观感回到早期的磨砂档位）。
 */
export const SIDEBAR_ALPHA_DEFAULT = 0.55;

export function getSidebarAlpha(): number {
  try {
    const v = Number(localStorage.getItem(SIDEBAR_ALPHA_KEY));
    if (!Number.isNaN(v) && v >= 0.2 && v <= 1) {
      // 一次性迁移：老版本默认值（0.5/0.55）在深色下会被浅色壁纸透穿，
      // 把「没主动调过」的旧值抬到新默认。只做一次，之后尊重用户设置。
      if (!localStorage.getItem(MIGRATED_KEY)) {
        localStorage.setItem(MIGRATED_KEY, "1");
        if (v === 0.5 || v === 0.55) {
          localStorage.setItem(SIDEBAR_ALPHA_KEY, String(SIDEBAR_ALPHA_DEFAULT));
          return SIDEBAR_ALPHA_DEFAULT;
        }
      }
      return v;
    }
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
