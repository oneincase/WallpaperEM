// ---- 画质档位预设（与 Rust 的 PRESET_LOW/MEDIUM/HIGH 逐字段镜像）----
// 选中即**整体覆盖**六个画质参数；手调任意参数后按值反推为「自定义」。
// 抗锯齿不进表：锁定期恒为 off、禁止更改（方案优化中）。
// 设置页与「壁纸设置」窗口的播放设置共用这一份，避免两处拷贝走偏。
export const QUALITY_PRESETS = {
  low: { renderDpr: 0.75, sceneFps: 15, particles: "low", post: "low", resources: 0.6, resourcesNormal: 0.75 },
  medium: { renderDpr: 0.85, sceneFps: 30, particles: "medium", post: "medium", resources: 0.8, resourcesNormal: 1 },
  high: { renderDpr: 1, sceneFps: 30, particles: "high", post: "high", resources: 1, resourcesNormal: 1 },
} as const;

export type QualityPresetId = keyof typeof QUALITY_PRESETS;
export type QualityPresetKey = QualityPresetId | "custom";

export const PRESET_LABELS: Record<QualityPresetKey, string> = {
  low: "低",
  medium: "中",
  high: "高",
  custom: "自定义",
};

// 粒子/后处理的滑条档位（滑条由左到右 = 由省到费）
export const PARTICLE_STOPS = ["off", "low", "medium", "high"] as const;
export const POST_STOPS = ["off", "low", "medium", "high"] as const;
export type QualityStop = (typeof PARTICLE_STOPS)[number];

/** 档位滑条的停靠点 → 中文短标签 */
export function stopLabel(v: string): string {
  return v === "high" ? "高" : v === "medium" ? "中" : v === "low" ? "低" : "关";
}

/** 六个画质参数的当前生效值 */
export interface QualityValues {
  renderDpr: number;
  sceneFps: number;
  particles: string;
  post: string;
  resources: number;
  resourcesNormal: number;
}

/**
 * 画质参数 → 档位。与托盘、Rust 的 derive_quality_preset_id 同一规则：
 * 与某个预设**逐字段相等**才算档位，手调任意参数即「自定义」。
 * 值应先归一（清晰度 0=自动→1、贴图 auto→1、帧率钳 15–60）再比。
 */
export function deriveQualityPreset(v: QualityValues): QualityPresetKey {
  for (const id of ["low", "medium", "high"] as const) {
    const p = QUALITY_PRESETS[id];
    if (
      v.renderDpr === p.renderDpr &&
      v.sceneFps === p.sceneFps &&
      v.particles === p.particles &&
      v.post === p.post &&
      v.resources === p.resources &&
      v.resourcesNormal === p.resourcesNormal
    ) {
      return id;
    }
  }
  return "custom";
}
