// 发现页：随机壁纸推荐 —— 上方大屏预览 + 下方直排壁纸列表（左右箭头切换）
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  TYPE_LABELS,
  type WorkshopItemSummary,
} from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";
import { useItemProps } from "../hooks/useItemProps";
import { WallpaperPropsModal } from "../components/WallpaperPropsModal";
import { IconSliders } from "../components/icons";
import { useMessage } from "../components/Message";
import { useWorkshopFilter } from "../hooks/useWorkshopFilter";
import { readSnapshot, writeSnapshot, SNAPSHOT_KEYS } from "../lib/cache-snapshots";

// 会话快照：首次成功获取后把随机壁纸列表与选中位置落到 localStorage。
//
// 之前这里用的是模块级变量，只能扛住「切页导致组件卸载」；窗口被释放后重建
// （main_window.rs 的 RELEASE_AFTER）是全新 JS 上下文，模块级变量归零，
// 于是每次重开都要等三次串行网络请求（拉总数 → 拉随机页 → enrich 元数据）。
//
// 刻意不做 TTL、也不后台静默刷新：重开窗口直接显示上次那一批，只有点
// 「换一批」才真正重随机。代价是内容不再「每次打开都新鲜」，换来的是秒开。
type HomeSnapshot = {
  items: WorkshopItemSummary[];
  index: number;
  /** 拍快照时的筛选条件指纹；与当前不一致说明用户在工坊页改过条件 */
  filterKey: string;
};

export function HomePage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  // 与工坊页共用的筛选条件。放在最前面：下面的初始 state 要用它算条件指纹
  const { sort, days, tags, excludedTags } = useWorkshopFilter();
  // 筛选条件的指纹：变了就说明用户在工坊页调过条件，快照里的推荐已不符合预期
  const filterKey = JSON.stringify([sort, days, tags, excludedTags]);

  // useState 的惰性初始化：readSnapshot 只在首次渲染跑一次。
  // 直接写在函数体里会每次渲染都读一遍 localStorage 并 JSON.parse 整个列表。
  const [restored] = useState(() => readSnapshot<HomeSnapshot>(SNAPSHOT_KEYS.home));
  const usable = restored !== null && restored.filterKey === filterKey;

  // 初始值取自快照：可用时不显示骨架屏、不重新请求。
  // 指纹不匹配时当作无缓存处理 —— 内容确实是错的，显示骨架比显示旧结果诚实
  const [items, setItems] = useState<WorkshopItemSummary[]>(usable ? restored.items : []);
  const [index, setIndex] = useState(usable ? restored.index : 0);
  const [loading, setLoading] = useState(!usable);
  const [error, setError] = useState("");
  const [applying, setApplying] = useState(false);
  const [enqueuing, setEnqueuing] = useState(false);
  const msg = useMessage();
  const [faved, setFaved] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [propsItemId, setPropsItemId] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();

  const current = items[index];
  const currentApplied = current ? appliedItems.has(current.id) : false;
  const currentDownloaded = current ? downloadedItems.has(current.id) : false;
  // 已下载壁纸的操作条提供「壁纸配置」快捷入口
  const currentPropDefs = useItemProps(currentDownloaded ? current?.id : null);
  const currentCustomizable = (currentPropDefs?.length ?? 0) > 0;

  // 加载随机推荐壁纸
  const load = useCallback(async (fresh = false) => {
    setLoading(!fresh && items.length === 0);
    setRefreshing(fresh);
    setError("");
    try {
      // 复用工坊页的筛选条件：用户在那边排除了成人内容，这里也不该再推
      const res = await api.workshopRandom({
        sort,
        days: sort === "trend" && days > 0 ? days : undefined,
        tags,
        excludedTags,
      });
      const list = res.items;
      if (list.length === 0) {
        setError("没有获取到壁纸，请重试");
        return;
      }
      setItems(list);
      setIndex(0);
      writeSnapshot<HomeSnapshot>(SNAPSHOT_KEYS.home, { items: list, index: 0, filterKey });
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sort, days, tags, excludedTags, filterKey]);

  // 快照可用时首帧已经渲染了正确内容 —— 跳过挂载时这次请求
  const skipFirstFetch = useRef(usable);
  useEffect(() => {
    if (skipFirstFetch.current) {
      skipFirstFetch.current = false;
      return;
    }
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filterKey]);

  // 选中位置同步到快照，切页或重开窗口后恢复到同一张。
  // items 走 ref 读取：这个 effect 只该被 index 触发，不想因 items 变化多写一次
  const itemsRef = useRef(items);
  itemsRef.current = items;
  useEffect(() => {
    if (itemsRef.current.length === 0) return;
    writeSnapshot<HomeSnapshot>(SNAPSHOT_KEYS.home, {
      items: itemsRef.current,
      index,
      filterKey,
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
  };

  const select = (i: number) => {
    setIndex(i);
  };

  const scrollRow = (dir: 1 | -1) => {
    const el = listRef.current;
    if (!el) return;
    el.scrollBy({ left: dir * 240, behavior: "smooth" });
  };

  const apply = async () => {
    if (!current) return;
    setApplying(true);
    try {
      await api.wallpaperApplyItem(current.id);
      await refreshApplied();
    } catch (e) {
      msg.error(String(e));
    } finally {
      setApplying(false);
    }
  };

  const enqueue = async () => {
    if (!current) return;
    setEnqueuing(true);
    try {
      await api.downloadEnqueue(current.id);
      msg.success("已加入下载队列");
    } catch (e) {
      msg.error(String(e));
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
          {error} —— 请检查网络/代理（设置 → 网络 → 代理）
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
            {currentDownloaded && currentCustomizable && (
              <button
                className="btn"
                onClick={() => current && setPropsItemId(current.id)}
                title="壁纸配置"
              >
                <IconSliders size={14} />
                壁纸配置
              </button>
            )}
            <button className={`btn ${faved ? "btn-danger" : ""}`} onClick={toggleFav}>
              {faved ? "★ 已收藏" : "☆ 收藏"}
            </button>
            <button className="btn" onClick={() => onOpenDetail(current.id)}>
              查看详情 ↗
            </button>
          </div>
        </div>
      )}

      {/* 壁纸配置弹窗（从操作条打开） */}
      {propsItemId && (
        <WallpaperPropsModal
          itemId={propsItemId}
          title={items.find((i) => i.id === propsItemId)?.title ?? propsItemId}
          onClose={() => setPropsItemId(null)}
        />
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
