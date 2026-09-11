// 会话快照：把「上次看到的内容」落到 localStorage，跨窗口销毁/重建后立即还原。
//
// 为什么必须落盘、模块级变量不行：
//  1. main_window.rs 在系统内存压力下走 webview.destroy()，重建时是
//     WebviewUrl::App("index.html") —— 全新 JS 上下文，模块级变量一律归零。
//  2. 即使窗口没被销毁，App.tsx 的冻结自检发现时间跳变 >30s 会
//     window.location.reload()，同样清空内存。
// 两条路径都只有 localStorage 能活下来。
//
// 设计取舍：**缓存即用，不自动刷新**。重开窗口直接显示上次内容，不发请求，
// 用户点「换一批」/翻页/改条件时才走网络。所以这里没有 TTL —— 有意为之，
// 不是漏了。工坊/发现的内容变化以天计，用户感知不到陈旧，但能立刻感知等待。

/** localStorage 键名前缀，沿用项目既有的 `we.<域>.<字段>` 约定 */
const PREFIX = "we.cache.";

/**
 * 快照结构版本。改动任一快照的字段含义时递增，旧数据会被自动丢弃 ——
 * 比逐个字段做兼容判断更省事，代价只是用户丢一次缓存。
 */
const SCHEMA = 2;

type Envelope<T> = { v: number; savedAt: number; data: T };

/**
 * 读取快照。任何异常（无痕模式禁用 storage、JSON 损坏、版本不符）都返回 null，
 * 让调用方退回「无缓存」的正常首屏路径。
 */
export function readSnapshot<T>(key: string): T | null {
  try {
    const raw = localStorage.getItem(PREFIX + key);
    if (!raw) return null;
    const env = JSON.parse(raw) as Envelope<T>;
    if (env?.v !== SCHEMA) return null;
    return env.data ?? null;
  } catch {
    return null;
  }
}

/**
 * 写入快照。写失败（多为配额耗尽）时清掉本键再试一次；仍失败就放弃 ——
 * 缓存是纯优化，永远不该让它的失败影响主流程。
 */
export function writeSnapshot<T>(key: string, data: T): void {
  const env: Envelope<T> = { v: SCHEMA, savedAt: Date.now(), data };
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(env));
  } catch {
    try {
      localStorage.removeItem(PREFIX + key);
      localStorage.setItem(PREFIX + key, JSON.stringify(env));
    } catch {
      /* 配额实在不够，放弃缓存 */
    }
  }
}

/** 各快照的键名（集中定义，避免散落的字符串字面量写错却无人发现） */
export const SNAPSHOT_KEYS = {
  /** 发现页：随机推荐列表 + 选中位置 + 条件指纹 */
  home: "home",
  /** 工坊页：搜索结果 + 页码 + 搜索词 + 条件指纹 */
  workshop: "workshop",
} as const;

/** 导航位置与筛选条件属于「界面状态」而非内容缓存，用 we.<域> 前缀另存 */
const STATE_PREFIX = "we.";

export function readState<T>(key: string, fallback: T): T {
  try {
    const raw = localStorage.getItem(STATE_PREFIX + key);
    if (!raw) return fallback;
    return (JSON.parse(raw) as T) ?? fallback;
  } catch {
    return fallback;
  }
}

export function writeState<T>(key: string, value: T): void {
  try {
    localStorage.setItem(STATE_PREFIX + key, JSON.stringify(value));
  } catch {
    /* ignore */
  }
}

/**
 * 清除内容快照（工坊/发现的列表缓存 —— 设置页「清除缓存」的浏览器侧那半）。
 *
 * 只删 `we.cache.` 前缀的键：同前缀之外还有界面状态（导航位置、筛选条件、
 * 侧边栏透明度等，见 STATE_PREFIX），那些是用户的设置不是缓存，清掉会让
 * 用户「清个缓存把调好的筛选也弄丢了」。
 */
export function clearSnapshotCaches(): void {
  try {
    const keys: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && k.startsWith(PREFIX)) keys.push(k);
    }
    for (const k of keys) localStorage.removeItem(k);
  } catch {
    /* 无痕模式禁用 storage 等情况：没有缓存可清，忽略 */
  }
}
