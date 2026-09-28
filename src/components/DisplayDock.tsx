// 库页底部的显示器坞：鼠标扫到屏幕底边即从下方滑出，列出当前连接的显示器。
//
// 它承接了原「显示器」页的全部日常操作，所以那一页已移除：
//   - 选屏应用：点卡片 = 锁定应用目标（复用 lib/apply-target 的 armed 机制），
//     之后在网格里点任意壁纸「应用」就只落这一屏；再点一次卡片取消。
//   - 统一 / 独立模式切换（settings_set display_mode）
//   - 每屏的轮播绑定（displayBindingSet）/ 清除该屏壁纸（wallpaperStop）
// 原页面里的布局缩略图舞台没有搬过来 —— 卡片列表已经表达了「哪块屏、什么壁纸」，
// 舞台只是同一信息的可视化重排。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { api, type DisplayInfo, type Playlist } from "../api/steam";
import { armApplyTarget, cancelApplyTarget, useArmedApplyTarget } from "../lib/apply-target";
import { tr } from "../lib/i18n";
import { useMessage } from "./Message";
import { AnchoredMenu } from "./AnchoredMenu";
import { IconMonitor } from "./icons";

/** 收起延迟：给鼠标从屏幕底边移到面板上的时间，否则滑出瞬间就缩回去 */
const CLOSE_DELAY_MS = 260;

export function DisplayDock({
  displays,
  mode,
  playlists,
  onReload,
}: {
  displays: DisplayInfo[];
  /** unified = 所有屏同壁纸；independent = 每屏各自指定 */
  mode: string;
  /** 轮播绑定菜单的候选列表 */
  playlists: Playlist[];
  /** 坞内改了状态（模式/绑定/清除）后，通知父级重新拉取 */
  onReload: () => void;
}) {
  const msg = useMessage();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [bindAt, setBindAt] = useState<{ x: number; y: number; d: DisplayInfo } | null>(null);
  const armed = useArmedApplyTarget();
  const closeTimer = useRef<number | undefined>(undefined);

  const cancelClose = useCallback(() => {
    if (closeTimer.current !== undefined) {
      window.clearTimeout(closeTimer.current);
      closeTimer.current = undefined;
    }
  }, []);

  const scheduleClose = useCallback(() => {
    cancelClose();
    closeTimer.current = window.setTimeout(() => setOpen(false), CLOSE_DELAY_MS);
  }, [cancelClose]);

  useEffect(() => cancelClose, [cancelClose]);

  // 显示器插拔：坞里自己听（父级另有 20s 轮询兜底）
  useEffect(() => {
    let un: (() => void) | undefined;
    void listen("displays-changed", () => onReload()).then((f) => {
      un = f;
    });
    return () => un?.();
  }, [onReload]);

  const toggleTarget = (d: DisplayInfo) => {
    if (armed?.id === d.id) cancelApplyTarget();
    else armApplyTarget({ id: d.id, name: d.name });
  };

  const setMode = async (m: "unified" | "independent") => {
    if (m === mode) return;
    try {
      await invoke("settings_set", { key: "display_mode", value: m });
      msg.success(m === "unified" ? tr("已切换到统一模式") : tr("已切换到独立模式"));
      onReload();
    } catch (e) {
      msg.error(String(e));
    }
  };

  const stopWallpaper = async (d: DisplayInfo) => {
    setBusy(d.id);
    try {
      await api.wallpaperStop(d.id);
      onReload();
    } catch (e) {
      msg.error(String(e));
    } finally {
      setBusy(null);
    }
  };

  const bind = async (d: DisplayInfo, playlistId: number | null) => {
    setBindAt(null);
    try {
      await api.displayBindingSet(d.id, playlistId);
      msg.success(
        playlistId === null
          ? tr("「{name}」已固定为当前壁纸", { name: d.name })
          : tr("「{name}」开始轮播", { name: d.name }),
      );
      onReload();
    } catch (e) {
      msg.error(String(e));
    }
  };

  const single = displays.length <= 1;

  return (
    <>
      <div
        className="pointer-events-none fixed inset-x-0 bottom-0 z-30"
        onMouseEnter={() => {
          cancelClose();
          setOpen(true);
        }}
        // 绑定菜单挂在坞外，鼠标移过去会离开坞 —— 菜单开着时不收起
        onMouseLeave={bindAt ? undefined : scheduleClose}
      >
        {/* 铺满屏幕底边的触发带：鼠标扫过任意位置都滑出，不必对准把手 */}
        <div className="absolute inset-x-0 bottom-0 h-3" />

        <div className="flex flex-col items-center px-6 pb-2">
          <div
            aria-hidden={!open}
            className={`pointer-events-auto mb-2 w-full max-w-[min(1080px,100%)] rounded-2xl border border-[var(--separator)] bg-[var(--card)]/95 p-3 shadow-2xl backdrop-blur transition-all duration-300 ${
              open ? "translate-y-0 opacity-100" : "pointer-events-none translate-y-5 opacity-0"
            }`}
          >
            {/* 顶部：模式切换 + 提示 */}
            <div className="mb-2.5 flex flex-wrap items-center gap-x-2.5 gap-y-1.5 px-0.5">
              <span className="flex items-center gap-1.5 text-[12.5px] font-semibold">
                <IconMonitor />
                {tr("显示器")}
                <span className="rounded bg-[var(--glass-hover)] px-1.5 py-px text-[11px] font-normal">
                  {displays.length}
                </span>
              </span>

              {!single && (
                <div className="flex items-center gap-0.5 rounded-lg border border-[var(--separator)] p-0.5">
                  {(
                    [
                      ["unified", tr("统一模式")],
                      ["independent", tr("独立模式")],
                    ] as const
                  ).map(([m, label]) => (
                    <button
                      key={m}
                      title={
                        m === "unified"
                          ? tr("应用壁纸时同步替换所有显示器的壁纸")
                          : tr("每块屏可以各自设置壁纸与轮播")
                      }
                      onClick={() => void setMode(m)}
                      className={`rounded-md px-2 py-0.5 text-[11.5px] transition-colors ${
                        mode === m
                          ? "bg-[var(--accent-strong)] text-white"
                          : "text-[var(--text-2)] hover:bg-[var(--glass-hover)]"
                      }`}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              )}

              <span className="text-[11.5px] text-[var(--text-2)]">
                {single
                  ? tr("仅检测到一块显示器")
                  : tr("选中一块屏后，在库里点「应用」就只设置该屏")}
              </span>

              {armed && (
                <button
                  className="ml-auto rounded-lg border border-[var(--separator)] px-2 py-0.5 text-[11.5px] hover:border-[var(--accent-strong)]"
                  onClick={cancelApplyTarget}
                >
                  {tr("取消选择「{name}」", { name: armed.name })}
                </button>
              )}
            </div>

            {displays.length === 0 ? (
              <div className="py-6 text-center text-[12.5px] text-[var(--text-2)]">
                {tr("未检测到显示器")}
              </div>
            ) : (
              <div className="flex gap-2.5 overflow-x-auto pb-1">
                {displays.map((d) => {
                  const isArmed = armed?.id === d.id;
                  return (
                    // 卡片用 div 而非 button：内部还要放「清除 / 轮播」按钮，button 不能嵌套
                    <div
                      key={d.id}
                      role="button"
                      tabIndex={0}
                      aria-pressed={isArmed}
                      onClick={() => toggleTarget(d)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          toggleTarget(d);
                        }
                      }}
                      className={`group relative flex w-[196px] shrink-0 cursor-pointer flex-col gap-1.5 rounded-xl border p-2.5 text-left transition-colors ${
                        isArmed
                          ? "border-[var(--accent-strong)] bg-[var(--accent)]/10"
                          : "border-[var(--separator)] bg-[var(--content)]/60 hover:border-[var(--accent-strong)]/50"
                      }`}
                    >
                      <div className="relative h-[88px] w-full overflow-hidden rounded-lg bg-black/40">
                        {d.previewUrl ? (
                          <img
                            src={d.previewUrl}
                            alt=""
                            className="h-full w-full object-cover"
                            draggable={false}
                          />
                        ) : (
                          <div className="flex h-full w-full items-center justify-center text-[var(--text-2)]/40">
                            <IconMonitor />
                          </div>
                        )}
                        {isArmed && (
                          <span className="absolute right-1.5 top-1.5 rounded bg-[var(--accent-strong)] px-1.5 py-0.5 text-[10px] font-medium text-white">
                            {tr("已选中")}
                          </span>
                        )}

                        {/* 卡片内操作条：hover 才出，避免平时干扰选屏 */}
                        <div className="absolute inset-x-1.5 bottom-1.5 flex gap-1.5 opacity-0 transition-opacity group-hover:opacity-100">
                          <button
                            title={tr("为该屏选择轮播列表")}
                            onClick={(e) => {
                              e.stopPropagation();
                              const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                              setBindAt({ x: r.left, y: r.top, d });
                            }}
                            className="rounded bg-black/65 px-1.5 py-0.5 text-[10px] text-white/85 hover:bg-[var(--accent-strong)] hover:text-white"
                          >
                            {tr("轮播")}
                          </button>
                          {d.itemId && (
                            <button
                              title={tr("清除该屏壁纸")}
                              disabled={busy === d.id}
                              onClick={(e) => {
                                e.stopPropagation();
                                void stopWallpaper(d);
                              }}
                              className="rounded bg-black/65 px-1.5 py-0.5 text-[10px] text-white/85 hover:bg-red-600 hover:text-white disabled:opacity-40"
                            >
                              {busy === d.id ? "…" : tr("清除")}
                            </button>
                          )}
                        </div>
                      </div>

                      <div className="flex items-center gap-1.5">
                        <span className="truncate text-[12.5px] font-medium">{d.name}</span>
                        {d.isPrimary && (
                          <span className="shrink-0 rounded bg-[var(--accent)]/15 px-1 py-px text-[10px] text-[var(--accent-strong)]">
                            {tr("主屏")}
                          </span>
                        )}
                      </div>
                      <div className="text-[11px] text-[var(--text-2)]">
                        {d.w}×{d.h}
                        {d.scale && d.scale !== 1 ? ` @${d.scale}x` : ""}
                      </div>
                      <div className="truncate text-[11px] text-[var(--text-2)]">
                        {d.title ?? tr("未设置壁纸")}
                        {d.binding && (
                          <span className="ml-1 text-[var(--accent-strong)]">
                            · {tr("轮播")} {d.binding.index + 1}/{d.binding.total}
                          </span>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* 把手：面板收起时的常驻入口，也给鼠标一个明确的落点 */}
          <button
            className="pointer-events-auto flex items-center gap-1.5 rounded-full border border-[var(--separator)] bg-[var(--card)]/95 px-3 py-1 text-[11.5px] text-[var(--text-2)] shadow-lg backdrop-blur transition-colors hover:text-[var(--text)]"
            onClick={() => setOpen((v) => !v)}
          >
            <IconMonitor />
            {tr("显示器")} · {displays.length}
            <span className={`transition-transform duration-300 ${open ? "rotate-180" : ""}`}>▲</span>
          </button>
        </div>
      </div>

      {/* 轮播绑定菜单：坞在屏幕底部，所以向上展开 */}
      {bindAt && (
        <AnchoredMenu x={bindAt.x} y={bindAt.y} drop="up" onClose={() => setBindAt(null)}>
          <div className="px-3 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-[var(--text-2)]/70">
            {tr("「{name}」的轮播", { name: bindAt.d.name })}
          </div>
          <button
            className="flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] hover:bg-[var(--glass-hover)]"
            onClick={() => void bind(bindAt.d, null)}
          >
            <span>{tr("不轮播（固定当前壁纸）")}</span>
            {bindAt.d.binding == null && (
              <span className="text-[11px] text-[var(--accent-strong)]">✓</span>
            )}
          </button>
          {playlists.length === 0 ? (
            <div className="px-3 py-2 text-[11.5px] text-[var(--text-2)]">
              {tr("还没有切换列表 —— 先在本地库顶部新建一个")}
            </div>
          ) : (
            playlists.map((p) => (
              <button
                key={p.id}
                className="flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] hover:bg-[var(--glass-hover)]"
                onClick={() => void bind(bindAt.d, p.id)}
              >
                <span className="truncate">{p.name}</span>
                <span className="shrink-0 text-[11px] text-[var(--text-2)]">
                  {p.itemIds.length}
                  {bindAt.d.binding?.playlistId === p.id && (
                    <span className="ml-1 text-[var(--accent-strong)]">✓</span>
                  )}
                </span>
              </button>
            ))
          )}
        </AnchoredMenu>
      )}
    </>
  );
}
