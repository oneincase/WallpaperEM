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
import { WallpaperCard, TypeChip } from "../components/WallpaperCard";
import { EmptyState } from "../components/EmptyState";
import {
  FilterDrawer,
  FilterButton,
  FilterSection,
  TagChip,
} from "../components/FilterDrawer";
import { readSnapshot, writeSnapshot, SNAPSHOT_KEYS } from "../lib/cache-snapshots";

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
  tags: string[],
  excludedTags: string[],
): string {
  return JSON.stringify([query, sort, days, tags, excludedTags]);
}

export function WorkshopPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  // 筛选条件全局共享：发现页复用同一套（见 useWorkshopFilter）。
  // 放在最前面：下面的初始 state 要用它算条件指纹
  const {
    tagState,
    cycleTag,
    sort,
    setSort,
    days,
    setDays,
    reset: resetFilters,
    tags,
    excludedTags,
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
      conditionKeyOf(restored.query, sort, days, tags, excludedTags);

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
  const { appliedItems, downloadedItems } = useWallpaperMeta();

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
          tags,
          excludedTags,
          page: targetPage,
        })
        .then((res) => {
          setData(res);
          writeSnapshot<WorkshopSnapshot>(SNAPSHOT_KEYS.workshop, {
            data: res,
            page: targetPage,
            query: debounced,
            conditionKey: conditionKeyOf(debounced, sort, days, tags, excludedTags),
          });
        })
        .catch((e) => setError(String(e)))
        .finally(() => setLoading(false));
    },
    [debounced, sort, days, tags, excludedTags],
  );

  // 条件变化 → 回到第 1 页重搜。用 ref 存 runSearch 避免它进依赖数组导致重复触发
  const runRef = useRef(runSearch);
  runRef.current = runSearch;
  const conditionKey = conditionKeyOf(debounced, sort, days, tags, excludedTags);
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
          placeholder="搜索壁纸…"
          className="w-64 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent)]"
        />
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value)}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none"
        >
          {WORKSHOP_SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        {/* 时间范围只对趋势排序有效（实测其他排序完全忽略），此处禁用并说明 */}
        <select
          value={days}
          onChange={(e) => setDays(Number(e.target.value))}
          disabled={sort !== "trend"}
          title={sort === "trend" ? "趋势时间范围" : "时间范围仅在「趋势」排序下有效"}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none disabled:opacity-45"
        >
          {TREND_DAYS.map((d) => (
            <option key={d.value} value={d.value}>
              {d.label}
            </option>
          ))}
        </select>
        {data && (
          <span className="text-[12px] text-[var(--text-2)]">
            共 {data.total.toLocaleString()} 个结果
          </span>
        )}
      </div>

      <FilterDrawer
        open={filterOpen}
        onClose={() => setFilterOpen(false)}
        meta={data ? `共 ${data.total.toLocaleString()} 个结果` : undefined}
        activeCount={activeCount}
        onReset={resetFilters}
      >
        {TAG_GROUPS.map((g) => {
          const count = g.tags.filter((t) => tagState[t.name]).length;
          return (
            <FilterSection
              key={g.kind}
              label={g.label}
              count={count}
              defaultOpen={g.kind === "type" || g.kind === "genre"}
            >
              {g.tags.map((t) => (
                <TagChip
                  key={t.name}
                  label={t.label}
                  state={tagState[t.name] ?? "off"}
                  onClick={() => cycleTag(t.name)}
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
            {error} —— 请检查网络/代理（设置 → 网络 → 代理）
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
                <WallpaperCard
                  key={item.id}
                  imageUrl={item.previewUrl ?? undefined}
                  title={item.title}
                  onOpen={() => onOpenDetail(item.id)}
                  badges={
                    <>
                      {appliedItems.has(item.id) && (
                        <span className="rounded bg-green-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                          已应用
                        </span>
                      )}
                      {downloadedItems.has(item.id) && (
                        <span className="rounded bg-sky-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                          已下载
                        </span>
                      )}
                    </>
                  }
                  metaLeft={<TypeChip label={TYPE_LABELS[item.type]} />}
                  metaRight={item.subscriptions ? `⬇ ${item.subscriptions.toLocaleString()}` : ""}
                />
              ))}
            </div>
          )}

          {!loading && data && data.items.length === 0 && !error && (
            <EmptyState
              art="search"
              title="没有匹配的壁纸"
              hint={
                activeCount > 0
                  ? "多个标签是「同时满足」，试试减少几个筛选条件"
                  : "换个关键词试试"
              }
            />
          )}

          {!loading && data && data.items.length > 0 && (
            <div className="flex items-center justify-center gap-3 pt-4">
              <button className="btn" disabled={page <= 1} onClick={() => goPage(page - 1)}>
                上一页
              </button>
              <span className="text-[13px] text-[var(--text-2)]">
                {page} / {Math.max(pages, 1)}
              </span>
              <button className="btn" disabled={!data.hasMore} onClick={() => goPage(page + 1)}>
                下一页
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
