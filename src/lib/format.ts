// 展示层的数值格式化（工坊订阅数这类可能上百万的计数）

/**
 * 计数紧凑显示：<1 万原样带千分位（"8,420"），≥1 万折算成 k（"12k" / "1.2k" 同族）。
 *
 * 卡片角落只有几十像素宽，完整数字（"1,234,567"）会把徽标撑成一条横杠；
 * 这些数字原先显示在悬浮抽屉里，位置改成封面角落后必须压短。
 */
export function formatCount(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0";
  if (n < 10_000) return n.toLocaleString();
  const k = n / 1000;
  return `${k >= 100 ? Math.round(k) : k.toFixed(1)}k`;
}

/** 字节数紧凑显示（1 位小数，1024 进制），用于缓存占用这类设置页文案 */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** 轮播倒计时："m:ss"（分钟不补零、秒补零），用于「xx 后切换」 */
export function formatCountdown(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, "0")}`;
}
