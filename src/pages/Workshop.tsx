// 工坊页：完整筛选（对齐 WE 官方）+ 搜索 / 排序 / 分页网格
//
// 两列结构：左列是「工具栏 + 筛选面板」，右列是内容网格。
// 筛选一律走 API 级（Steam 的 requiredtags[]/excludedtags[]），不在前端二次过滤 ——
// 每页只有 30 条，本地过滤会让「共 N 个结果」和实际展示对不上。
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  TYPE_LABELS,
  type WorkshopSearchResult,
} from "../api/steam";
import { TAG_GROUPS, TREND_DAYS, WORKSHOP_SORTS } from "../lib/tags";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";
import { useWorkshopFilter } from "../hooks/useWorkshopFilter";
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
import { formatCount } from "../lib/format";
import { tr, trMsg } from "../lib/i18n";
import { tagLabel } from "../lib/tags";

/** 落盘的会话快照：结果 + 浏览位置 + 拍快照时的条件指纹 */
type WorkshopSnapshot = {
  data: WorkshopSearchResult;
  page: number;
  query: string;
  conditionKey: string;
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
  // 快照只在「筛选条件没变」时可用。条件变了就当无缓存 —— 显示骨架屏比
  // 显示一屏不符合当前条件的结果诚实
  const usable =
    restored !== null &&
    restored.conditionKey ===
      conditionKeyOf(restored.query, sort, days, tagGroups);

  // 首屏直接吃快照：窗口重建后立刻显示上次的结果，不发请求、不闪骨架屏。
  // query 与 debounced 必须一起初始化 —— 只恢复 query 的话，debounced 从空串
  // 起步会先用「无关键词」发一次请求，500ms 后防抖落地又发第二次。
  const [query, setQuery] = useState(usable ? restored.query : "");
  const [debounced, setDebounced] = useState(usable ? restored.query : "");

  const [filterOpen, setFilterOpen] = useState(false);
  const [page, setPage] = useState(usable ? restored.page : 1);
  const [data, setData] = useState<WorkshopSearchResult | null>(usable ? restored.data : null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
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

  // 单一请求入口：所有条件变化都经这里，避免「effect 一条路 + 翻页另一条路」两份参数拼装
  const runSearch = useCallback(
    (targetPage: number) => {
      setLoading(true);
      setError("");
      return api
        .workshopSearch({
          query: debounced || undefined,
          sort,
          // days 只在趋势排序下有意义（实测其他排序完全忽略），-1 表示全部时间
          days: sort === "trend" && days > 0 ? days : undefined,
          tagGroups,
          page: targetPage,
        })
        .then((res) => {
          setData(res);
          writeSnapshot<WorkshopSnapshot>(SNAPSHOT_KEYS.workshop, {
            data: res,
            page: targetPage,
            query: debounced,
            conditionKey: conditionKeyOf(debounced, sort, days, tagGroups),
          });
        })
        .catch((e) => setError(String(e)))
        .finally(() => setLoading(false));
    },
    [debounced, sort, days, tagGroups],
  );

  // 条件变化 → 回到第 1 页重搜。用 ref 存 runSearch 避免它进依赖数组导致重复触发
  const runRef = useRef(runSearch);
  runRef.current = runSearch;
  const conditionKey = conditionKeyOf(debounced, sort, days, tagGroups);
  // 快照可用时首帧已经渲染了正确内容 —— 跳过挂载时这次请求。
  // 用 ref 而非 state：它只需在首个 effect 里被读一次，不参与渲染
  const skipFirstFetch = useRef(usable);
  useEffect(() => {
    if (skipFirstFetch.current) {
      skipFirstFetch.current = false;
      return;
    }
    setPage(1);
    runRef.current(1);
  }, [conditionKey]);

  const goPage = (p: number) => {
    const target = Math.max(1, p);
    if (target === page) return;
    setPage(target);
    runSearch(target);
  };

  const pages = data?.total ? Math.min(1000, Math.ceil(data.total / 30)) : 0;

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
        {data && (
          <span className="text-[12px] text-[var(--text-2)]">
            {tr("共 {n} 个结果", { n: data.total.toLocaleString() })}
          </span>
        )}
      </div>

      <FilterDrawer
        open={filterOpen}
        onClose={() => setFilterOpen(false)}
        meta={data ? tr("共 {n} 个结果", { n: data.total.toLocaleString() }) : undefined}
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
        {data?.truncated && (
          <div className="mb-3 shrink-0 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-2.5 text-[12.5px] text-amber-700 dark:text-amber-300">
            {tr("选中的标签组合太多，只查询了其中一部分，结果可能不全 —— 建议每组少选几个标签。")}
          </div>
        )}
        <div className="min-h-0 flex-1 overflow-y-auto">
          {loading && (
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

          {!loading && data && (
            <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(168px,1fr))]">
              {data.items.map((item) => (
                /* 下载量（工坊叫订阅数）常驻封面右上角：它是挑壁纸时最先看的
                   数字之一，原先塞在悬浮抽屉里，鼠标不悬停就看不见 */
                <WallpaperCard
                  key={item.id}
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
              ))}
            </div>
          )}

          {!loading && data && data.items.length === 0 && !error && (
            <EmptyState
              art="search"
              title={tr("没有匹配的壁纸")}
              hint={
                activeCount > 0
                  ? tr("同组多选是「任一命中」，试试减少几个筛选条件")
                  : tr("换个关键词试试")
              }
            />
          )}

          {!loading && data && data.items.length > 0 && (
            <div className="flex items-center justify-center gap-3 pt-4">
              <button className="btn" disabled={page <= 1} onClick={() => goPage(page - 1)}>
                {tr("上一页")}
              </button>
              <span className="text-[13px] text-[var(--text-2)]">
                {page} / {Math.max(pages, 1)}
              </span>
              <button className="btn" disabled={!data.hasMore} onClick={() => goPage(page + 1)}>
                {tr("下一页")}
              </button>
            </div>
          )}
        </div>
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
