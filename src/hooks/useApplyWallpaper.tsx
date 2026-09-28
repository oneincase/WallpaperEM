// 「应用到桌面」的统一入口：目标选择（统一/独立模式语义）+ 显示器坞「更换壁纸」的锁定目标。
//
// 语义（P1）：
//   - 锁定目标（armed）最优先：显示器坞点过「更换壁纸」，下一次应用只去那一屏；
//   - 单屏：一键应用到全部显示器（现状行为，零打扰）；
//   - 多屏（无论统一/独立模式）：弹出目标选择菜单（全部 / 各屏），记住上次选择；
//   - 菜单里点「正在播放本张壁纸」的显示器 = 切换为停止该屏播放（再点可再应用）。
//
// 用法：const { apply, menuNode } = useApplyWallpaper({ onApplied });
//   onClick={(e) => apply(itemId, e.currentTarget)}，并把 {menuNode} 渲染出来。
// apply 返回 "done" | "stopped" | "cancelled"（"stopped" = 停了某屏的播放，调用方同样
// 要刷新已应用集合，但不要弹「已应用」提示；菜单被点掉 = cancelled，不要报成功）。
import { useCallback, useRef, useState, type ReactNode } from "react";
import { api, type DisplayInfo } from "../api/steam";
import {
  consumeApplyTarget,
  readLastTarget,
  writeLastTarget,
} from "../lib/apply-target";
import { tr } from "../lib/i18n";

type MenuState = {
  x: number;
  y: number;
  displays: DisplayInfo[];
  /** 本次待应用的条目；各屏的 itemId 与之相同 = 正在播放本张，点击应停止 */
  itemId: string;
  /** "" = 上次选的全部显示器；null = 从没选过 */
  last: string | null;
} | null;

export type ApplyResult = "done" | "stopped" | "cancelled";

export function useApplyWallpaper(opts: { onApplied?: (itemId: string) => void } = {}) {
  const [menu, setMenu] = useState<MenuState>(null);
  const pendingRef = useRef<{
    itemId: string;
    resolve: (r: ApplyResult) => void;
    reject: (e: unknown) => void;
  } | null>(null);
  // onApplied 每次渲染都可能变，走 ref 避免 apply 回调身份抖动
  const onAppliedRef = useRef(opts.onApplied);
  onAppliedRef.current = opts.onApplied;

  const apply = useCallback(
    (itemId: string, anchor?: HTMLElement): Promise<ApplyResult> =>
      new Promise((resolve, reject) => {
        void (async () => {
          try {
            // ① 显示器坞锁定的目标屏
            const armed = consumeApplyTarget();
            if (armed) {
              await api.wallpaperApplyItem(itemId, armed.id);
              onAppliedRef.current?.(itemId);
              resolve("done");
              return;
            }
            // ② 单屏：全部显示器
            const { displays } = await api.wallpaperDisplaysList();
            if (displays.length < 2 || !anchor) {
              await api.wallpaperApplyItem(itemId);
              onAppliedRef.current?.(itemId);
              resolve("done");
              return;
            }
            // ③ 多屏：目标选择菜单（统一/独立模式都弹，可选指定屏）
            const rect = anchor.getBoundingClientRect();
            pendingRef.current = { itemId, resolve, reject };
            setMenu({
              x: Math.min(rect.left, window.innerWidth - 240),
              y: Math.min(rect.bottom + 6, window.innerHeight - 40 - displays.length * 34),
              displays,
              itemId,
              last: readLastTarget(),
            });
          } catch (e) {
            reject(e);
          }
        })();
      }),
    [],
  );

  const choose = useCallback(
    (targetId: string | null, playingItemId?: string | null) => {
      const p = pendingRef.current;
      pendingRef.current = null;
      setMenu(null);
      if (!p) return;
      // 点的是「正在播放本张壁纸」的显示器：切换为停止该屏（不改「上次目标」，
      // 停止不代表用户下次想去那屏）
      if (targetId && playingItemId === p.itemId) {
        void (async () => {
          try {
            await api.wallpaperStop(targetId);
            p.resolve("stopped");
          } catch (e) {
            p.reject(e);
          }
        })();
        return;
      }
      writeLastTarget(targetId ?? "");
      void (async () => {
        try {
          await api.wallpaperApplyItem(p.itemId, targetId ?? undefined);
          onAppliedRef.current?.(p.itemId);
          p.resolve("done");
        } catch (e) {
          p.reject(e);
        }
      })();
    },
    [],
  );

  const dismiss = useCallback(() => {
    const p = pendingRef.current;
    pendingRef.current = null;
    setMenu(null);
    p?.resolve("cancelled");
  }, []);

  const menuNode: ReactNode =
    menu === null ? null : (
      <>
        {/* 点击外部关闭（全屏透明捕获层，z 低于菜单本体） */}
        <div className="fixed inset-0 z-40" onClick={dismiss} onContextMenu={dismiss} />
        <div
          className="fixed z-50 min-w-[210px] overflow-hidden rounded-xl border border-[var(--separator)] bg-[var(--card)] py-1 shadow-xl"
          style={{ left: menu.x, top: menu.y }}
        >
          <div className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-[var(--text-2)]/70">
            {tr("应用到哪块屏？")}
          </div>
          <button
            className="flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] hover:bg-[var(--glass-hover)]"
            onClick={() => choose(null)}
          >
            <span>{tr("全部显示器")}</span>
            {menu.last === "" && <span className="text-[11px] text-[var(--accent-strong)]">✓ {tr("上次")}</span>}
          </button>
          {menu.displays.map((d) => {
            // 该屏正在播放本次要应用的那张：同一项再点 = 停止该屏播放
            const playingThis = d.itemId != null && d.itemId === menu.itemId;
            return (
              <button
                key={d.id}
                className="flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] hover:bg-[var(--glass-hover)]"
                title={playingThis ? tr("点击停止该屏的壁纸播放") : undefined}
                onClick={() => choose(d.id, playingThis ? d.itemId : null)}
              >
                <span className="truncate">
                  {d.name}
                  {d.isPrimary && (
                    <span className="ml-1.5 rounded bg-[var(--accent)]/15 px-1 py-px text-[10px] text-[var(--accent-strong)]">
                      {tr("主屏")}
                    </span>
                  )}
                </span>
                {playingThis && (
                  <span className="shrink-0 text-[11px] text-[var(--accent-strong)]">
                    ● {tr("播放中")}
                  </span>
                )}
                {menu.last === d.id && (
                  <span className="shrink-0 text-[11px] text-[var(--accent-strong)]">
                    ✓ {tr("上次")}
                  </span>
                )}
              </button>
            );
          })}
        </div>
      </>
    );

  return { apply, menuNode };
}
