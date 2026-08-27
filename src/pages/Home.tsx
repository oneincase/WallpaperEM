// 发现页：随机壁纸推荐 —— 上方大屏预览 + 下方直排壁纸列表（左右箭头切换）
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  TYPE_LABELS,
  type WorkshopItemSummary,
} from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";

// 模块级缓存：首次成功获取后保存随机壁纸列表与选中位置。
// 切换页面导致组件重新挂载时直接复用缓存、不再发请求；
// 只有点击「换一批」时才请求新一批并更新缓存。
let cachedItems: WorkshopItemSummary[] | null = null;
let cachedIndex = 0;

export function HomePage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  // 初始值取自缓存：有缓存时不显示骨架屏、不重新请求
  const [items, setItems] = useState<WorkshopItemSummary[]>(cachedItems ?? []);
  const [index, setIndex] = useState(cachedIndex);
  const [loading, setLoading] = useState(cachedItems === null);
  const [error, setError] = useState("");
  const [applying, setApplying] = useState(false);
  const [enqueuing, setEnqueuing] = useState(false);
  const [msg, setMsg] = useState("");
  const [faved, setFaved] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const listRef = useRef<HTMLDivElement>(null);
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();

  const current = items[index];
  const currentApplied = current ? appliedItems.has(current.id) : false;
  const currentDownloaded = current ? downloadedItems.has(current.id) : false;

  // 加载随机推荐壁纸
  const load = useCallback(async (fresh = false) => {
    setLoading(!fresh && items.length === 0);
    setRefreshing(fresh);
    setError("");
    try {
      const res = await api.workshopRandom("trend");
      const list = res.items;
      if (list.length === 0) {
        setError("没有获取到壁纸，请重试");
        return;
      }
      setItems(list);
      setIndex(0);
      // 写入模块缓存，切换页面回来时复用
      cachedItems = list;
      cachedIndex = 0;
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    // 仅首次（无缓存）时请求；切回页面时复用缓存，不触发刷新
    if (cachedItems === null) load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 选中位置同步到模块缓存，切换页面后回来时恢复
  useEffect(() => {
    cachedIndex = index;
  }, [index]);

  // 收藏状态
  useEffect(() => {
    if (!current) {
      setFaved(false);
      return;
    }
    api.favoriteStatus(current.id).then(setFaved).catch(() => setFaved(false));
  }, [current?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // 选中项在直排列表中保持可见
  useEffect(() => {
    const el = listRef.current?.querySelector<HTMLElement>(`[data-idx="${index}"]`);
    el?.scrollIntoView({ behavior: "smooth", inline: "center", block: "nearest" });
  }, [index]);

  const go = (dir: 1 | -1) => {
    if (!items.length) return;
    const next = (index + dir + items.length) % items.length;
    setIndex(next);
    setMsg("");
  };

  const select = (i: number) => {
    setIndex(i);
    setMsg("");
  };

  const scrollRow = (dir: 1 | -1) => {
    const el = listRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * 240, behavior: "smooth" });
  };

  const apply = async () => {
    if (!current) return;
    setApplying(true);
    setMsg("");
    try {
      await api.wallpaperApplyItem(current.id);
      await refreshApplied();
    } catch (e) {
      setMsg(String(e));
    } finally {
      setApplying(false);
    }
  };

  const enqueue = async () => {
    if (!current) return;
    setEnqueuing(true);
    setMsg("");
    try {
      await api.downloadEnqueue(current.id);
      setMsg("✅ 已加入下载队列");
    } catch (e) {
      setMsg(String(e));
    } finally {
      setEnqueuing(false);
    }
  };

  const toggleFav = async () => {
    if (!current) return;
    if (faved) {
      await api.favoriteRemove(current.id);
      setFaved(false);
    } else {
      await api.favoriteAdd(current.id);
      setFaved(true);
    }
  };

  return (
    <div className="h-full flex flex-col gap-3 min-h-0 px-7 py-5">
      <div className="flex items-center justify-between shrink-0">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">发现</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-0.5">
            随机壁纸推荐，点击下方列表或箭头切换
          </p>
        </div>
        <button className="btn" disabled={refreshing} onClick={() => load(true)}>
          {refreshing ? "刷新中…" : "↻ 换一批"}
        </button>
      </div>

      {error && (
        <div className="rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-[13px] text-red-500 shrink-0">
          {error} —— 请检查网络/代理（设置 → 下载 → 代理）
        </div>
      )}

      {loading && (
        <div className="flex flex-col gap-3 flex-1 min-h-0">
          {/* 上方大屏预览骨架：与加载后布局一致（占满剩余高度 + 底部操作条高度） */}
          <div className="card overflow-hidden flex flex-col min-h-0 flex-1">
            <div className="relative flex-1 min-h-0" style={{ background: "var(--card)" }}>
              <div className="absolute inset-0 animate-pulse" style={{ background: "var(--separator)" }} />
            </div>
            <div className="flex items-center gap-2 px-5 py-3 shrink-0">
              <div className="h-[30px] w-[130px] animate-pulse rounded-lg" style={{ background: "var(--separator)" }} />
              <div className="h-[30px] w-[90px] animate-pulse rounded-lg" style={{ background: "var(--separator)" }} />
              <div className="h-[30px] w-[90px] animate-pulse rounded-lg" style={{ background: "var(--separator)" }} />
            </div>
          </div>
          {/* 下方直排列表骨架：与加载后一致（16/10 图 + 标题行） */}
          <div className="flex items-center gap-2 shrink-0">
            <div className="h-[30px] w-[30px] shrink-0 animate-pulse rounded-lg" style={{ background: "var(--separator)" }} />
            <div className="flex flex-1 gap-3 overflow-hidden">
              {Array.from({ length: 6 }).map((_, i) => (
                <div key={i} className="w-[136px] shrink-0" style={{ background: "var(--card)" }}>
                  <div className="aspect-[16/10] animate-pulse" style={{ background: "var(--separator)" }} />
                  <div className="h-[14px] mt-1 mx-1.5 mb-1.5 animate-pulse rounded" style={{ background: "var(--separator)" }} />
                </div>
              ))}
            </div>
            <div className="h-[30px] w-[30px] shrink-0 animate-pulse rounded-lg" style={{ background: "var(--separator)" }} />
          </div>
        </div>
      )}

      {/* 上方大屏预览（弹性占满剩余高度，不出滚动条） */}
      {!loading && current && (
        <div className="card overflow-hidden flex flex-col min-h-0 flex-1">
          <div className="relative flex-1 min-h-0 bg-black/10">
            <button className="absolute left-3 top-1/2 -translate-y-1/2 btn !p-1.5 !rounded-full opacity-80 hover:opacity-100" onClick={() => go(-1)} title="上一张">
              ‹
            </button>
            <button className="absolute right-3 top-1/2 -translate-y-1/2 btn !p-1.5 !rounded-full opacity-80 hover:opacity-100" onClick={() => go(1)} title="下一张">
              ›
            </button>
            {current.previewUrl ? (
              <img
                key={current.id}
                src={current.previewUrl}
                alt={current.title}
                className="h-full w-full object-cover"
              />
            ) : (
              <div className="flex h-full items-center justify-center text-[var(--text-2)]">无预览</div>
            )}
            <div className="absolute left-3 top-3 flex gap-1.5 z-10">
              {currentApplied && (
                <span className="rounded-full bg-green-500/85 px-2 py-0.5 text-[10.5px] font-semibold text-white">
                  已应用
                </span>
              )}
              {currentDownloaded && (
                <span className="rounded-full bg-sky-500/85 px-2 py-0.5 text-[10.5px] font-semibold text-white">
                  已下载
                </span>
              )}
            </div>
            <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/70 to-transparent p-5 pt-16">
              <h2 className="text-[20px] font-bold text-white truncate">{current.title}</h2>
              <div className="mt-2 flex items-center gap-2 flex-wrap">
                <span className="rounded-full bg-white/20 px-2.5 py-0.5 text-[11.5px] font-medium text-white">
                  {TYPE_LABELS[current.type]}
                </span>
                {current.subscriptions !== undefined && current.subscriptions > 0 && (
                  <span className="text-[11.5px] text-white/85">
                    ⬇ {current.subscriptions.toLocaleString()} 订阅
                  </span>
                )}
                {current.favorited !== undefined && current.favorited > 0 && (
                  <span className="text-[11.5px] text-white/85">
                    ★ {current.favorited.toLocaleString()}
                  </span>
                )}
              </div>
            </div>
          </div>

          {/* 操作条 */}
          <div className="flex items-center gap-2 px-5 py-3 flex-wrap shrink-0">
            {currentApplied ? (
              <button
                className="btn !bg-green-500/15 !text-green-600 dark:!text-green-400 !border-green-500/30 cursor-default disabled:opacity-75"
                disabled
                title="已应用到桌面"
              >
                已应用
              </button>
            ) : (
              <button className="btn btn-primary" disabled={applying} onClick={apply}>
                {applying ? "…" : "🖥 应用到桌面"}
              </button>
            )}
            {currentDownloaded ? (
              <button
                className="btn !bg-sky-500/15 !text-sky-600 dark:!text-sky-400 !border-sky-500/30 cursor-default disabled:opacity-75"
                disabled
                title="已下载到本地库"
              >
                已下载
              </button>
            ) : (
              <button className="btn" disabled={enqueuing} onClick={enqueue}>
                {enqueuing ? "…" : "⬇ 下载"}
              </button>
            )}
            <button className={`btn ${faved ? "btn-danger" : ""}`} onClick={toggleFav}>
              {faved ? "★ 已收藏" : "☆ 收藏"}
            </button>
            <button className="btn" onClick={() => onOpenDetail(current.id)}>
              查看详情 ↗
            </button>
            {msg && <span className="text-[12.5px] text-[var(--text-2)]">{msg}</span>}
          </div>
        </div>
      )}

      {/* 下方直排壁纸列表 */}
      {!loading && items.length > 0 && (
        <div className="flex items-center gap-2 shrink-0">
          <button className="btn shrink-0 !px-2.5" onClick={() => scrollRow(-1)} title="向左滚动">
            ‹
          </button>
          <div
            ref={listRef}
            className="flex-1 overflow-x-auto scroll-smooth"
            style={{ scrollbarWidth: "thin" }}
          >
            <div className="flex gap-3 w-max">
              {items.map((item, i) => (
                <button
                  key={item.id}
                  data-idx={i}
                  onClick={() => select(i)}
                  className={`group relative w-[136px] shrink-0 overflow-hidden rounded-lg border text-left transition-all ${
                    i === index
                      ? "border-[var(--accent)] ring-2 ring-[var(--accent)]/40"
                      : "border-transparent hover:border-[var(--separator)]"
                  }`}
                >
                  <div className="aspect-[16/10] bg-black/10">
                    {item.previewUrl ? (
                      <img src={item.previewUrl} alt={item.title} loading="lazy" className="h-full w-full object-cover" />
                    ) : (
                      <div className="flex h-full items-center justify-center text-[var(--text-2)] text-[10px]">无</div>
                    )}
                    {(appliedItems.has(item.id) || downloadedItems.has(item.id)) && (
                      <div className="absolute left-1 top-1 z-10 flex flex-col gap-1">
                        {appliedItems.has(item.id) && (
                          <span className="rounded bg-green-500/90 px-1 py-0.5 text-[8.5px] font-semibold text-white">
                            已应用
                          </span>
                        )}
                        {downloadedItems.has(item.id) && (
                          <span className="rounded bg-sky-500/90 px-1 py-0.5 text-[8.5px] font-semibold text-white">
                            已下载
                          </span>
                        )}
                      </div>
                    )}
                  </div>
                  <div className="truncate px-1.5 py-1 text-[11px] text-[var(--text-1)]">
                    {item.title}
                  </div>
                </button>
              ))}
            </div>
          </div>
          <button className="btn shrink-0 !px-2.5" onClick={() => scrollRow(1)} title="向右滚动">
            ›
          </button>
        </div>
      )}
    </div>
  );
}
