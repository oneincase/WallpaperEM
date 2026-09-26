// 页内玻璃背景（.app-backdrop）的数据源：当前应用壁纸的封面 + 自适应 tint。
// 主窗口与「壁纸设置」独立窗共用（两边都是同一套玻璃观感）：
// - 返回封面 URL，调用方注入 `.app-backdrop` 的 --backdrop-img；
// - 按封面平均亮度写 --glass-lift（0..1，index.css 里三个玻璃 alpha 随之抬升），
//   曲线：亮度 0.5 以下轻透、0.9 以上拉满，线性 —— 变化应是「呼吸感」而非跳变。
// 刷新时机：挂载、sessions-changed（应用/停止/轮播）、displays-changed，
// 以及本窗口拖动落定（onMoved，跨屏后背景跟随目标屏的壁纸）。
//
// ⚠️ 防黑闪三件套（实测「不时黑闪一下」的根源就在这条链路）：
//   ① stale-while-revalidate：新壁纸的封面常在后台生成，这时后端返回空 url，
//      绝不能清掉旧背景 —— 只有「真没壁纸了」（itemId 为 None）才清；
//   ② 换图先预加载：CSS background 换 url 后加载完成前是空白（透出兜底深底），
//      必须等 Image() onload 再切换；
//   ③ 防抖串行化：sessions-changed/onMoved 等触发合并成单飞请求，天然无响应乱序。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";

const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

interface UiBackdropResp {
  url: string | null;
  luminance: number | null;
  itemId: string | null;
}

/** 预加载图片：成功/失败都 resolve（失败就维持旧背景，不清空） */
function preload(url: string): Promise<void> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve();
    img.onerror = () => resolve();
    img.src = url;
  });
}

function liftOf(luminance: number | null): number {
  return luminance == null ? 0 : Math.min(1, Math.max(0, (luminance - 0.5) / 0.4));
}

export function useWallpaperBackdrop(): string | null {
  const [backdrop, setBackdrop] = useState<string | null>(null);
  const moveTimer = useRef<number | null>(null);
  const debounceTimer = useRef<number | null>(null);
  // 单飞防串行：请求在飞时置忙，完成后再补一次（合并期间积压的触发）
  const busy = useRef(false);
  const rerun = useRef(false);

  const doRefresh = useCallback(async () => {
    if (busy.current) {
      rerun.current = true;
      return;
    }
    busy.current = true;
    try {
      const r = await invoke<UiBackdropResp>("wallpaper_ui_backdrop");
      if (r.itemId == null) {
        // 真没壁纸了：清背景与浓度
        setBackdrop(null);
        document.documentElement.style.setProperty("--glass-lift", "0");
      } else {
        // 有壁纸：亮度就绪才更新浓度（封面暂缺时保留旧值）
        if (r.luminance != null) {
          document.documentElement.style.setProperty(
            "--glass-lift",
            liftOf(r.luminance).toFixed(3),
          );
        }
        if (r.url && r.url !== "") {
          // ② 预加载完成才换图，杜绝加载期的空窗闪烁
          await preload(r.url);
          setBackdrop(r.url);
        }
        // url 暂缺（封面生成中）→ 什么都不动，旧背景继续用
      }
    } catch {
      /* 查询失败维持现状 */
    } finally {
      busy.current = false;
      if (rerun.current) {
        rerun.current = false;
        void doRefresh();
      }
    }
  }, []);

  // ③ 所有触发统一防抖 200ms 合并
  const schedule = useCallback(() => {
    if (debounceTimer.current) clearTimeout(debounceTimer.current);
    debounceTimer.current = window.setTimeout(() => {
      debounceTimer.current = null;
      void doRefresh();
    }, 200);
  }, [doRefresh]);

  useEffect(() => {
    schedule();
    if (!inTauri) return;
    const un1 = listen("sessions-changed", schedule);
    const un2 = listen("displays-changed", schedule);
    let unMoved: (() => void) | null = null;
    void getCurrentWindow()
      .onMoved(() => {
        if (moveTimer.current) clearTimeout(moveTimer.current);
        moveTimer.current = window.setTimeout(schedule, 350);
      })
      .then((f) => (unMoved = f))
      .catch(() => {});
    return () => {
      void Promise.all([un1, un2]).then(([f1, f2]) => {
        f1();
        f2();
      });
      unMoved?.();
      if (moveTimer.current) clearTimeout(moveTimer.current);
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
    };
  }, [schedule]);

  return backdrop;
}
