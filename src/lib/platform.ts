// 平台探测与窗口壳状态（全平台无边框玻璃壳的共用小件）。
//
// - useOs：首帧用 navigator 同步猜测（窗口控制位置、拖拽区这些平台差异 UI
//   不能等异步探测回来，否则启动会闪一下错的布局），app_info 返回后校准一次。
// - useWindowRounded：最大化/全屏时 CSS 圆角与描边必须收掉（透明窗口铺满时
//   露角会透出桌面），主窗口与 props 窗共用。
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

export type OsId = "macos" | "windows" | "linux";

/** 浏览器直开渲染器页（无 Tauri IPC）时不接线、不渲染平台控件 */
export const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function guessOs(): OsId {
  const p = typeof navigator !== "undefined" ? navigator.platform : "";
  if (/mac/i.test(p)) return "macos";
  if (/win/i.test(p)) return "windows";
  return "linux";
}

/** 当前平台。首帧同步猜测，app_info（std::env::consts::OS）返回后校准 */
export function useOs(): OsId {
  const [os, setOs] = useState<OsId>(guessOs);
  useEffect(() => {
    if (!inTauri) return;
    invoke<{ os: string }>("app_info")
      .then((i) => {
        if (i.os === "macos" || i.os === "windows" || i.os === "linux") setOs(i.os);
      })
      .catch(() => {});
  }, []);
  return os;
}

/** 玻璃圆角壳开关：true = 画圆角 + 描边，false = 最大化/全屏铺满（收角） */
export function useWindowRounded(): boolean {
  const [rounded, setRounded] = useState(true);
  useEffect(() => {
    if (!inTauri) return;
    const win = getCurrentWindow();
    let un: (() => void) | null = null;
    const check = () => {
      void (async () => {
        const max = await win.isMaximized().catch(() => false);
        const fs = await win.isFullscreen().catch(() => false);
        setRounded(!max && !fs);
      })();
    };
    void win
      .onResized(() => check())
      .then((f) => (un = f))
      .catch(() => {});
    check();
    return () => un?.();
  }, []);
  return rounded;
}
