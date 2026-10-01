// 「应用到桌面」的统一入口：目标选择收在**库页底部的显示器坞**这一处。
//
// 语义（2026-10-01 简化：不再弹「应用到哪块屏」的目标菜单）：
//   - 锁定目标（armed）最优先：显示器坞点过卡片 ⇒ 下一次应用只去那一屏，然后自动解除；
//   - 没锁定 ⇒ 全部显示器（统一模式下的默认动作）。
// 后端随之把「手动设了单张」的屏钉住：统一列表不再刷它（见 wallpaper 模块的
// screen_pinned），所以「只设一块屏」不会再顺手停掉别的屏的轮播。
//
// 用法：const { apply } = useApplyWallpaper({ onApplied });
//   onClick={() => apply(itemId)}；返回的 Promise 在落屏后 resolve（失败时 reject，
//   调用方弹错误提示）。应用成功后调用方的「已应用」集合要重取。
import { useCallback, useRef } from "react";
import { api } from "../api/steam";
import { armApplyTarget, consumeApplyTarget } from "../lib/apply-target";

export function useApplyWallpaper(opts: { onApplied?: (itemId: string) => void } = {}) {
  // onApplied 每次渲染都可能变，走 ref 避免 apply 回调身份抖动
  const onAppliedRef = useRef(opts.onApplied);
  onAppliedRef.current = opts.onApplied;

  const apply = useCallback(async (itemId: string): Promise<void> => {
    const armed = consumeApplyTarget();
    try {
      await api.wallpaperApplyItem(itemId, armed?.id);
    } catch (e) {
      // 失败不消耗锁定：目标屏还在等这张壁纸，重试不该悄悄退回「全部屏」
      if (armed) armApplyTarget(armed);
      throw e;
    }
    onAppliedRef.current?.(itemId);
  }, []);

  return { apply };
}
