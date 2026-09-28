// 平台窗口材质（macOS vibrancy / Windows Acrylic / Linux KWin blur）是否真的落到了
// 窗口上 —— 决定「没有壁纸封面时要不要自己垫一层不透明底」。
//
// 背景：没有壁纸时 .app-backdrop::before 无图即透明（见 index.css 的说明），
// 只剩 ::after 那 22% 的 tint。材质可用时它叠在真模糊上，是作者要的玻璃观感；
// 材质不可用时（Windows acrylic + blur 都失败、GNOME 等没有窗口模糊能力的合成器）
// 窗口近乎全透，白字直接糊在用户的桌面壁纸上。
//
// 两种取法都做，因为顺序不确定：
//   ① 挂载时查一次（正常情况下 lib.rs setup 里 apply_vibrancy 早于页面加载）；
//   ② 订阅 `backdrop-material` 事件 —— main_window.rs 在主窗口被内存压力回收后
//      重建窗口并再次应用材质，新窗口的 JS 挂载与那次应用谁先谁后说不准。
//
// 取不到（浏览器直开渲染器页 / IPC 不可用）保持 "unknown" = 保守垫底。
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api/steam";

export type BackdropMaterial = "unknown" | "applied" | "unavailable";

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function coerce(v: unknown): BackdropMaterial | null {
  return v === "applied" || v === "unavailable" ? v : null;
}

export function useBackdropMaterial(): BackdropMaterial {
  const [material, setMaterial] = useState<BackdropMaterial>("unknown");

  useEffect(() => {
    if (!inTauri) return;
    let alive = true;
    const apply = (v: unknown) => {
      const m = coerce(v);
      if (alive && m) setMaterial(m);
    };

    void api
      .platformBackdropMaterial()
      .then(apply)
      .catch(() => {
        /* 取不到就保持 unknown（保守垫底），不打断界面 */
      });

    // 卸载后置空的取消函数：listen 是异步的，可能在 cleanup 之后才 resolve
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    void listen<string>("backdrop-material", (e) => apply(e.payload)).then((f) => {
      if (cancelled) f();
      else unlisten = f;
    });

    return () => {
      alive = false;
      cancelled = true;
      unlisten?.();
    };
  }, []);

  return material;
}

/// 无壁纸封面 + 材质没落到窗口 → 必须自己垫底保可读性。
///
/// 注意 "unknown" 也算「没落实到」：Linux 上材质是否被合成器采纳本来就不可探测，
/// 宁可垫底也不让白字压在桌面上（见 lib.rs BackdropMaterial 的说明）。
export function needsReadabilityFloor(
  backdrop: string | null,
  material: BackdropMaterial
): boolean {
  return backdrop === null && material !== "applied";
}
