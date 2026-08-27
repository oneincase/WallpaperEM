// 工坊页：搜索 / 排序 / 类型筛选 / 分页网格（T1）
import { useEffect, useState } from "react";
import {
  api,
  SORTS,
  TYPE_LABELS,
  type WallpaperType,
  type WorkshopSearchResult,
} from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";

const TYPE_FILTERS: (WallpaperType | "")[] = [
  "",
  "video",
  "scene",
  "web",
];

export function WorkshopPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [type, setType] = useState<WallpaperType | "">("");
  const [sort, setSort] = useState("trend");
  const [page, setPage] = useState(1);
  const [data, setData] = useState<WorkshopSearchResult | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const { appliedItems, downloadedItems } = useWallpaperMeta();

  useEffect(() => {
    // 防抖：停止输入 500ms 后才更新搜索词，避免每次按键都发请求
    const t = setTimeout(() => setDebounced(query.trim()), 500);
    return () => clearTimeout(t);
  }, [query]);

  // 搜索词/条件变化（防抖后）→ 回到第 1 页，并只发一次搜索
  useEffect(() => {
    let cancelled = false;
    setPage(1);
    setLoading(true);
    setError("");
    api
      .workshopSearch({ query: debounced || undefined, type, sort, page: 1 })
      .then((d) => {
        if (!cancelled) setData(d);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [debounced, type, sort]);

  // 翻页：直接按指定页码请求（不触发上面的 effect，避免重复请求）
  const goPage = (p: number) => {
    const target = Math.max(1, p);
    if (target === page) return;
    setPage(target);
    setLoading(true);
    setError("");
    api
      .workshopSearch({ query: debounced || undefined, type, sort, page: target })
      .then(setData)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  const pages = data?.total ? Math.min(1000, Math.ceil(data.total / 30)) : 0;

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 工具栏 - 固定在顶部 */}
      <div className="shrink-0 flex items-center gap-3 flex-wrap mb-4">
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
          {SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        <div className="flex rounded-lg border border-[var(--separator)] overflow-hidden">
          {TYPE_FILTERS.map((t) => (
            <button
              key={t || "all"}
              onClick={() => setType(t)}
              className={`px-3 py-1.5 text-[12.5px] transition-colors ${
                type === t
                  ? "bg-[var(--accent)] text-white"
                  : "bg-[var(--card)] hover:bg-black/5 dark:hover:bg-white/10"
              }`}
            >
              {t === "" ? "全部" : TYPE_LABELS[t]}
            </button>
          ))}
        </div>
        {data && (
          <span className="text-[12px] text-[var(--text-2)]">
            共 {data.total.toLocaleString()} 个结果
          </span>
        )}
      </div>

      {/* 状态 */}
      {error && (
        <div className="shrink-0 rounded-xl border border-red-500/30 bg-red-500/10 px-4 py-3 text-[13px] text-red-500 mb-4">
          {error} —— 请检查网络/代理（设置 → 下载 → 代理）
        </div>
      )}
      {loading && (
        <div className="shrink-0 grid grid-cols-4 gap-4 mb-4">
          {Array.from({ length: 8 }).map((_, i) => (
            <div
              key={i}
              className="card aspect-[16/10] animate-pulse"
              style={{ background: "var(--card)" }}
            />
          ))}
        </div>
      )}

      {/* 网格 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        {!loading && data && (
          <div className="grid grid-cols-4 gap-4">
            {data.items.map((item) => (
              <button
                key={item.id}
                onClick={() => onOpenDetail(item.id)}
                className="card group overflow-hidden text-left transition-transform hover:-translate-y-0.5"
              >
                <div className="relative aspect-[16/10] overflow-hidden bg-black/10">
                  {item.previewUrl ? (
                    <img
                      src={item.previewUrl}
                      alt={item.title}
                      loading="lazy"
                      className="h-full w-full object-cover"
                    />
                  ) : (
                    <div className="flex h-full items-center justify-center text-[var(--text-2)]">
                      无预览
                    </div>
                  )}
                  {(appliedItems.has(item.id) || downloadedItems.has(item.id)) && (
                    <div className="absolute left-1.5 top-1.5 z-10 flex flex-col gap-1">
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
                    </div>
                  )}
                </div>
                <div className="p-2.5">
                  <div className="truncate text-[12.5px] font-medium">{item.title}</div>
                  <div className="mt-1.5 flex items-center justify-between">
                    <span className="rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent)]">
                      {TYPE_LABELS[item.type]}
                    </span>
                    <span className="text-[11px] text-[var(--text-2)]">
                      {item.subscriptions ? `⬇ ${item.subscriptions.toLocaleString()}` : ""}
                    </span>
                  </div>
                </div>
              </button>
            ))}
          </div>
        )}

        {!loading && data && data.items.length === 0 && !error && (
          <div className="py-20 text-center text-[var(--text-2)]">没有匹配的壁纸</div>
        )}

        {/* 分页 */}
        {!loading && data && data.items.length > 0 && (
          <div className="flex items-center justify-center gap-3 pt-2">
            <button
              className="btn"
              disabled={page <= 1}
              onClick={() => goPage(page - 1)}
            >
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
  );
}
