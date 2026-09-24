// 工坊页：完整筛选（对齐 WE 官方）+ 搜索 / 排序 + 虚拟滚动无限加载
//
// 两列结构：左列是「工具栏 + 筛选面板」，右列是内容网格。
// 筛选一律走 API 级（Steam 的 requiredtags[]/excludedtags[]），不在前端二次过滤 ——
// Steam 侧每页只有 30 条，本地过滤会让「共 N 个结果」和实际展示对不上。
//
// 列表是「追加式无限滚动」：VirtualGrid 只渲染视口内的行，滚动接近底部时
// （onNearEnd）懒加载下一页并 append。分页器已移除 —— Steam 的翻页只是
// cursor 递进，逐页累积和翻页看到的内容一致，无限滚动不需要牺牲浏览连续性。
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  TYPE_LABELS,
  type WorkshopItemSummary,
  type WorkshopSearchResult,
} from "../api/steam";
import { TAG_GROUPS, TREND_DAYS, WORKSHOP_SORTS } from "../lib/tags";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";
import { useWorkshopFilter } from "../hooks/useWorkshopFilter";
import { VirtualGrid } from "../components/VirtualGrid";
import { WallpaperCard, TypeChip, CoverCountBadge } from "../components/WallpaperCard";
import { EmptyState } from "../components/EmptyState";
import { useMessage } from "../components/Message";
import {
  FilterDrawer,
  FilterButton,
  FilterSection,
  TagChip,
} from "../components/FilterDrawer";
import { readSnapshot, writeSnapshot, SNAPSHOT_KEYS } from "../lib/cache-snapshots";
import { tr, trMsg } from "../lib/i18n";
import { tagLabel } from "../lib/tags";

/** Steam 翻页上限（后端同款口径：cursor 页码最多 1000） */
const MAX_PAGES = 1000;
/**
 * 快照最多落盘多少条。条目带预览图 URL 等字段，几百 KB 的 localStorage 写入
 * 每页一次太重；300 条（10 页）足够还原「上次逛到哪」，更深的进度放弃还原
 * （继续从下一页懒加载，内容不丢）。
 */
const SNAPSHOT_ITEM_CAP = 300;

/** 落盘的会话快照：已累积的结果 + 加载进度 + 滚动位置 + 条件指纹 */
type WorkshopSnapshot = {
  items: WorkshopItemSummary[];
  page: number;
  total: number;
  hasMore: boolean;
  truncated?: boolean;
  query: string;
  conditionKey: string;
  scrollTop: number;
};

/**
 * 条件指纹。只有它变了才需要重新请求 —— 判断「快照能否直接用」和
 * 「条件是否变化」是同一件事，所以必须用同一个函数算，不能两处各写一遍。
 */
function conditionKeyOf(
  query: string,
  sort: string,
  days: number,
  tagGroups: string[][],
): string {
  return JSON.stringify([query, sort, days, tagGroups]);
}

export function WorkshopPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  // 筛选条件全局共享：发现页复用同一套（见 useWorkshopFilter）。
  // 放在最前面：下面的初始 state 要用它算条件指纹
  const {
    selected,
    toggleTag,
    sort,
    setSort,
    days,
    setDays,
    reset: resetFilters,
    tagGroups,
    activeCount,
  } = useWorkshopFilter();

  // useState 的惰性初始化：readSnapshot 只在首次渲染跑一次。
  // 直接写在函数体里会每次渲染都读一遍 localStorage 并 JSON.parse 整页结果
  const [restored] = useState(() => readSnapshot<WorkshopSnapshot>(SNAPSHOT_KEYS.workshop));
  // 快照只在「筛选条件没变且结构可识别」时可用。条件变了就当无缓存 ——
  // 显示骨架屏比显示一屏不符合当前条件的结果诚实。
  // Array.isArray 兼容旧分页版快照（旧结构没有顶层 items，直接当无缓存丢弃）
  const restoredSnap: WorkshopSnapshot | null = (() => {
    if (restored === null || !Array.isArray(restored.items)) return null;
    return restored.conditionKey === conditionKeyOf(restored.query, sort, days, tagGroups)
      ? restored
      : null;
  })();
  const usable = restoredSnap !== null;

  // 首屏直接吃快照：窗口重建后立刻显示上次的结果，不发请求、不闪骨架屏。
  // query 与 debounced 必须一起初始化 —— 只恢复 query 的话，debounced 从空串
  // 起步会先用「无关键词」发一次请求，500ms 后防抖落地又发第二次。
  const [query, setQuery] = useState(usable ? restoredSnap.query : "");
  const [debounced, setDebounced] = useState(usable ? restoredSnap.query : "");

  const [filterOpen, setFilterOpen] = useState(false);
  // 已累积的列表与加载进度（追加式无限滚动）
  const [items, setItems] = useState<WorkshopItemSummary[]>(usable ? restoredSnap.items : []);
  const [page, setPage] = useState(usable ? restoredSnap.page : 0);
  const [total, setTotal] = useState(usable ? restoredSnap.total : 0);
  const [hasMore, setHasMore] = useState(usable ? restoredSnap.hasMore : false);
  const [truncated, setTruncated] = useState(usable ? !!restoredSnap.truncated : false);
  const [loading, setLoading] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState("");
  // 快照还原的滚动位置：只在挂载后的首次布局生效（条件变化时清零，见下）
  const [restoreScroll, setRestoreScroll] = useState(() =>
    usable ? restoredSnap.scrollTop || 0 : 0,
  );
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();
  const msg = useMessage();
  // 防连点：下载入队中的条目
  const [pendingIds, setPendingIds] = useState<Set<string>>(new Set());

  /** 卡片上的快捷下载：入队即可，下载进度在下载页看 */
  const quickDownload = useCallback(
    async (id: string) => {
      if (pendingIds.has(id)) return;
      setPendingIds((prev) => new Set(prev).add(id));
      try {
        await api.downloadEnqueue(id);
        msg.success(tr("已加入下载队列"));
      } catch (e) {
        msg.error(String(e));
      } finally {
        setPendingIds((prev) => {
          const next = new Set(prev);
          next.delete(id);
          return next;
        });
      }
    },
    [pendingIds, msg],
  );

  /** 卡片上的快捷应用（已下载条目） */
  const quickApply = useCallback(
    async (id: string) => {
      try {
        await api.wallpaperApplyItem(id);
        // 应用会替换所有显示器上的旧壁纸，重取权威集合（与本地库页同一纪律）
        await refreshApplied();
        msg.success(tr("已应用到桌面"));
      } catch (e) {
        msg.error(String(e));
      }
    },
    [refreshApplied, msg],
  );

  useEffect(() => {
    // 防抖：停止输入 500ms 后才更新搜索词，避免每次按键都发请求
    const t = setTimeout(() => setDebounced(query.trim()), 500);
    return () => clearTimeout(t);
  }, [query]);

  // ---- 追加式加载：refresh（条件变化回第 1 页）与 loadMore（懒加载下一页）----
  //
  // seq 是请求代际号：条件一变就自增，旧代际的响应（无论首屏还是追加）一律
  // 丢弃 —— 追加请求比翻页慢得多地滞留空中，没有代际号会把旧条件的下一页
  // 拼进新条件的结果里。

  /** 当前渲染值的镜像：懒加载回调与卸载落盘都从这里读，不进依赖数组 */
  const snapRef = useRef({
    query: debounced,
    conditionKey: conditionKeyOf(debounced, sort, days, tagGroups),
    items,
    page,
    total,
    hasMore,
    truncated,
  });
  snapRef.current = {
    query: debounced,
    conditionKey: conditionKeyOf(debounced, sort, days, tagGroups),
    items,
    page,
    total,
    hasMore,
    truncated,
  };
  const seqRef = useRef(0);
  const loadingRef = useRef(false);
  loadingRef.current = loading;
  const loadingMoreRef = useRef(false);
  /** 有首屏 refresh 在飞（同步置位，不等渲染 —— loading 镜像有滞后窗口） */
  const refreshInFlightRef = useRef(false);
  /** 滚动偏移镜像（VirtualGrid onScrollTop 写入；落盘快照用） */
  const scrollTopRef = useRef(restoreScroll);

  /** 把当前列表状态落盘。条目截断到 SNAPSHOT_ITEM_CAP 控制 localStorage 体积 */
  const persist = useCallback((scrollTop: number) => {
    const s = snapRef.current;
    if (s.items.length === 0) return;
    writeSnapshot<WorkshopSnapshot>(SNAPSHOT_KEYS.workshop, {
      items: s.items.slice(0, SNAPSHOT_ITEM_CAP),
      page: s.page,
      total: s.total,
      hasMore: s.hasMore,
      truncated: s.truncated,
      query: s.query,
      conditionKey: s.conditionKey,
      scrollTop,
    });
  }, []);

  const refresh = useCallback(() => {
    const seq = ++seqRef.current;
    refreshInFlightRef.current = true;
    loadingMoreRef.current = false;
    setLoadingMore(false);
    setLoading(true);
    setError("");
    return api
      .workshopSearch({
        query: snapRef.current.query || undefined,
        sort,
        // days 只在趋势排序下有意义（实测其他排序完全忽略），-1 表示全部时间
        days: sort === "trend" && days > 0 ? days : undefined,
        tagGroups,
        page: 1,
      })
      .then((res: WorkshopSearchResult) => {
        if (seq !== seqRef.current) return;
        setItems(res.items);
        setPage(1);
        setTotal(res.total);
        setHasMore(res.hasMore);
        setTruncated(!!res.truncated);
        persist(0);
      })
      .catch((e) => {
        if (seq === seqRef.current) setError(String(e));
      })
      .finally(() => {
        // 被更新的请求取代（seq 前进）时不复位：清场交给最后一个收场的请求，
        // 否则会出现「新请求还在飞、守卫却已放行下一个」的窗口
        if (seq === seqRef.current) {
          refreshInFlightRef.current = false;
          setLoading(false);
        }
      });
  }, [sort, days, tagGroups, persist]);

  // 条件变化 → 回到第 1 页重搜。用 ref 存 refresh 避免它进依赖数组导致重复触发
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const conditionKey = conditionKeyOf(debounced, sort, days, tagGroups);
  // 快照可用时首帧已经渲染了正确内容 —— 跳过挂载时这次请求。
  // 用 ref 而非 state：它只需在首个 effect 里被读一次，不参与渲染
  const skipFirstFetch = useRef(usable);
  useEffect(() => {
    if (skipFirstFetch.current) {
      skipFirstFetch.current = false;
      return;
    }
    // 条件变了：滚动位置还原作废（VirtualGrid 按 conditionKey 换 key 重挂，
    // 新实例从头渲染），从第 1 页重新累积
    setRestoreScroll(0);
    scrollTopRef.current = 0;
    refreshRef.current();
  }, [conditionKey]);

  /** 滚动接近底部：懒加载下一页并 append。防重入 + 代际号防串页 */
  const loadMore = useCallback(() => {
    if (refreshInFlightRef.current || loadingRef.current || loadingMoreRef.current) return;
    const s = snapRef.current;
    if (!s.hasMore || s.page < 1 || s.page >= MAX_PAGES || s.items.length === 0) return;
    loadingMoreRef.current = true;
    setLoadingMore(true);
    // 追加请求同样递增代际号：若它起飞后条件恰好变了（refresh 会再自增），
    // 这页回来时 seq 对不上，整个结果连同分页进度一起丢弃
    const seq = ++seqRef.current;
    api
      .workshopSearch({
        query: s.query || undefined,
        sort,
        days: sort === "trend" && days > 0 ? days : undefined,
        tagGroups,
        page: s.page + 1,
      })
      .then((res: WorkshopSearchResult) => {
        // 条件已变（seq 前进）：这页属于旧条件，整个结果连同分页进度一起丢弃
        if (seq !== seqRef.current) return;
        setItems((prev) => {
          // Steam 的 cursor 翻页偶发跨页重复（趋势榜重排），按 id 去重后再拼
          const seen = new Set(prev.map((i) => i.id));
          const fresh = res.items.filter((i) => !seen.has(i.id));
          return fresh.length > 0 ? [...prev, ...fresh] : prev;
        });
        setPage(res.page);
        setTotal(res.total);
        setHasMore(res.hasMore);
        setTruncated((t) => t || !!res.truncated);
        persist(scrollTopRef.current);
      })
      .catch((e) => {
        if (seq === seqRef.current) setError(String(e));
      })
      .finally(() => {
        loadingMoreRef.current = false;
        setLoadingMore(false);
      });
  }, [sort, days, tagGroups, persist]);

  // 卸载时落盘一次：跨窗口销毁/重建（内存压力 webview.destroy、时间跳变刷新）
  // 后能回到当前进度与滚动位置。加载中/空列表不写，避免把半截状态存成快照
  useEffect(
    () => () => {
      if (!loadingRef.current) persist(scrollTopRef.current);
    },
    [persist],
  );

  return (
    // relative：筛选抽屉用 absolute 覆盖在本页之上
    <div className="relative flex h-full flex-col overflow-hidden px-7 py-5">
      {/* 工具栏 */}
      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2.5">
        <FilterButton onClick={() => setFilterOpen(true)} activeCount={activeCount} />
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={tr("搜索壁纸…")}
          className="w-64 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
        />
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value)}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none"
        >
          {WORKSHOP_SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {tr(s.label)}
            </option>
          ))}
        </select>
        {/* 时间范围只对趋势排序有效（实测其他排序完全忽略），此处禁用并说明 */}
        <select
          value={days}
          onChange={(e) => setDays(Number(e.target.value))}
          disabled={sort !== "trend"}
          title={sort === "trend" ? tr("趋势时间范围") : tr("时间范围仅在「趋势」排序下有效")}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none disabled:opacity-45"
        >
          {TREND_DAYS.map((d) => (
            <option key={d.value} value={d.value}>
              {tr(d.label)}
            </option>
          ))}
        </select>
        {total > 0 && (
          <span className="text-[12px] text-[var(--text-2)]">
            {tr("共 {n} 个结果", { n: total.toLocaleString() })}
          </span>
        )}
      </div>

      <FilterDrawer
        open={filterOpen}
        onClose={() => setFilterOpen(false)}
        meta={total > 0 ? tr("共 {n} 个结果", { n: total.toLocaleString() }) : undefined}
        activeCount={activeCount}
        onReset={resetFilters}
      >
        {TAG_GROUPS.map((g) => {
          // libraryOnly（本地导入）是本地库独有维度，工坊筛选不展示
          const tags = g.tags.filter((t) => !t.libraryOnly);
          const count = tags.filter((t) => selected[t.name]).length;
          return (
            <FilterSection
              key={g.kind}
              label={tr(g.label)}
              count={count}
              defaultOpen={g.kind === "type" || g.kind === "genre"}
            >
              {tags.map((t) => (
                <TagChip
                  key={t.name}
                  label={tagLabel(t.name)}
                  state={selected[t.name] ? "on" : "off"}
                  onClick={() => toggleTag(t.name)}
                  title={t.name}
                />
              ))}
            </FilterSection>
          );
        })}
      </FilterDrawer>

      <div className="flex min-h-0 flex-1 flex-col">
        {error && (
          <div className="mb-3 shrink-0 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-[13px] text-red-500">
              {trMsg(error)} —— {tr("请检查网络/代理（设置 → 网络 → 代理）")}
          </div>
        )}
        {/* 组内并集在 Steam 侧只能拆成「每组一个标签」的多次查询，组合数是各组的
            乘积；超上限时后端会截断，结果不完整就得说出来，不然用户会以为
            多选没生效 */}
        {truncated && (
          <div className="mb-3 shrink-0 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-2.5 text-[12.5px] text-amber-700 dark:text-amber-300">
            {tr("选中的标签组合太多，只查询了其中一部分，结果可能不全 —— 建议每组少选几个标签。")}
          </div>
        )}

        {loading && items.length === 0 && (
          <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(168px,1fr))]">
            {Array.from({ length: 8 }).map((_, i) => (
              <div
                key={i}
                className="card aspect-square animate-pulse"
                style={{ background: "var(--card)" }}
              />
            ))}
          </div>
        )}

        {!loading && items.length === 0 && !error && (
          <div className="min-h-0 flex-1 overflow-y-auto">
            <EmptyState
              art="search"
              title={tr("没有匹配的壁纸")}
              hint={
                activeCount > 0
                  ? tr("同组多选是「任一命中」，试试减少几个筛选条件")
                  : tr("换个关键词试试")
              }
            />
          </div>
        )}

        {items.length > 0 && (
          /* key = 条件指纹：换条件即换实例，滚动位置自然归零（restoreScroll 只
             在挂载快照还原那一次生效） */
          <VirtualGrid
            key={conditionKey}
            className="min-h-0 flex-1 overflow-y-auto"
            items={items}
            minColumnWidth={168}
            gap={12}
            keyOf={(it) => it.id}
            initialScrollTop={restoreScroll}
            onScrollTop={(top) => {
              scrollTopRef.current = top;
            }}
            onNearEnd={loadMore}
            footer={
              loadingMore ? (
                <div className="flex items-center justify-center py-4 text-[12.5px] text-[var(--text-2)]">
                  {tr("正在加载更多…")}
                </div>
              ) : !hasMore && !loading ? (
                <div className="flex items-center justify-center py-4 text-[12px] text-[var(--text-2)] opacity-70">
                  {tr("已显示全部")}
                </div>
              ) : undefined
            }
            renderItem={(item) => (
              /* 下载量（工坊叫订阅数）常驻封面右上角：它是挑壁纸时最先看的
                 数字之一，原先塞在悬浮抽屉里，鼠标不悬停就看不见 */
              <WallpaperCard
                imageUrl={item.previewUrl ?? undefined}
                title={item.title}
                onOpen={() => onOpenDetail(item.id)}
                badges={
                  <>
                    {appliedItems.has(item.id) && (
                      <span className="rounded bg-green-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                        {tr("已应用")}
                      </span>
                    )}
                    {downloadedItems.has(item.id) && (
                      <span className="rounded bg-sky-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                        {tr("已下载")}
                      </span>
                    )}
                  </>
                }
                metaLeft={<TypeChip label={tr(TYPE_LABELS[item.type])} />}
                coverBadgeRight={
                  item.subscriptions ? (
                    <CoverCountBadge count={item.subscriptions} />
                  ) : undefined
                }
                metaRight={
                  <span className="flex items-center gap-1.5">
                    {/* 快捷操作：未下载 → 下载；已下载 → 应用到桌面 */}
                    {downloadedItems.has(item.id) ? (
                      <button
                        className="rounded-md p-0.5 text-[var(--accent-strong)] transition-colors hover:bg-black/5 dark:hover:bg-white/10"
                        title={tr("应用到桌面")}
                        aria-label={tr("应用到桌面")}
                        onClick={() => void quickApply(item.id)}
                      >
                        <CardActionIcon kind="apply" />
                      </button>
                    ) : (
                      <button
                        className="rounded-md p-0.5 text-[var(--text-2)] transition-colors hover:bg-black/5 hover:text-[var(--accent-strong)] dark:hover:bg-white/10 disabled:opacity-40"
                        title={tr("下载")}
                        aria-label={tr("下载")}
                        disabled={pendingIds.has(item.id)}
                        onClick={() => void quickDownload(item.id)}
                      >
                        <CardActionIcon kind="download" />
                      </button>
                    )}
                  </span>
                }
              />
            )}
          />
        )}
      </div>
    </div>
  );
}

/** 工坊卡片上的快捷操作图标（下载 / 应用到桌面），13px 线性 SVG */
function CardActionIcon({ kind }: { kind: "download" | "apply" }) {
  return (
    <svg
      width="13"
      height="13"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2.2}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      {kind === "download" ? (
        <>
          <path d="M12 3.6v10.2" />
          <path d="M7.6 10.2 12 14.4l4.4-4.2" />
          <path d="M4.4 17.2v1.4a2 2 0 0 0 2 2h11.2a2 2 0 0 0 2-2v-1.4" />
        </>
      ) : (
        <>
          <rect x="2.8" y="4.2" width="18.4" height="12.4" rx="3.2" />
          <path d="M9.2 20.4h5.6" />
          <path d="M12 16.6v3.8" />
        </>
      )}
    </svg>
  );
}
