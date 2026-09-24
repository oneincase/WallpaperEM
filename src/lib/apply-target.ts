// 应用目标（哪块屏）的两项跨组件状态：
// ① armed  —— 显示器页「更换壁纸」锁定的目标屏，下一次应用动作只落它（然后自动解除）；
// ② last   —— 独立模式下「上次选择」（"" = 全部显示器），供目标菜单记忆。
import { useSyncExternalStore } from "react";

const LAST_TARGET_KEY = "we.applyTarget.last";

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

/** 显示器页「更换壁纸」：锁定目标屏（供 Library 页横幅展示 + 下一次应用消费） */
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

/** 上次目标：null=从没选过，""=全部显示器，其余=displayId */
export function readLastTarget(): string | null {
  try {
    return localStorage.getItem(LAST_TARGET_KEY);
  } catch {
    return null;
  }
}

export function writeLastTarget(target: string) {
  try {
    localStorage.setItem(LAST_TARGET_KEY, target);
  } catch {
    /* ignore */
  }
}
