// 应用目标（哪块屏）的跨组件状态：
// armed —— 显示器坞点卡片锁定的目标屏：下一次「应用到桌面」只落它，然后自动解除；
// 没锁定就是全部显示器（2026-10-01 起目标选择只走显示器坞，不再有弹层菜单）。
import { useSyncExternalStore } from "react";

export type ApplyTarget = { id: string; name: string };

let armed: ApplyTarget | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const l of listeners) l();
}

function subscribe(cb: () => void) {
  listeners.add(cb);
  return () => {
    listeners.delete(cb);
  };
}

function getArmed(): ApplyTarget | null {
  return armed;
}

/** 显示器坞「更换壁纸」：锁定目标屏（供 Library 页横幅展示 + 下一次应用消费） */
export function armApplyTarget(t: ApplyTarget) {
  armed = t;
  emit();
}

/** 用户点横幅「取消」解除锁定 */
export function cancelApplyTarget() {
  armed = null;
  emit();
}

/** 应用动作消费：取出并清除锁定 */
export function consumeApplyTarget(): ApplyTarget | null {
  const t = armed;
  armed = null;
  emit();
  return t;
}

export function useArmedApplyTarget(): ApplyTarget | null {
  return useSyncExternalStore(subscribe, getArmed, getArmed);
}
