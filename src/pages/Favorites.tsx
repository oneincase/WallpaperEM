// 收藏页（T4）
import { useCallback, useEffect, useState } from "react";
import { api, TYPE_LABELS, type FavoriteItem } from "../api/steam";

export function FavoritesPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [items, setItems] = useState<FavoriteItem[]>([]);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      setItems(await api.favoritesList());
    } catch (e) {
      console.warn(e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 mb-4">
        <h1 className="text-[22px] font-bold tracking-tight">收藏</h1>
        <p className="text-[13px] text-[var(--text-2)] mt-1">收藏的工坊壁纸</p>
      </div>

      {loading && <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">加载中…</div>}
      {!loading && items.length === 0 && (
        <div className="shrink-0 card p-12 text-center text-[13px] text-[var(--text-2)] mb-4">
          暂无收藏 —— 在壁纸详情页点击「收藏」
        </div>
      )}

      {/* 内容网格 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        <div className="grid grid-cols-4 gap-4">
          {items.map((item) => (
            <button
              key={item.itemId}
              onClick={() => onOpenDetail(item.itemId)}
              className="card group overflow-hidden text-left transition-transform hover:-translate-y-0.5"
            >
              <div className="aspect-[16/10] overflow-hidden bg-black/10">
                {item.previewUrl ? (
                  <img src={item.previewUrl} alt={item.title} loading="lazy" className="h-full w-full object-cover" />
                ) : (
                  <div className="flex h-full items-center justify-center text-[var(--text-2)]">无预览</div>
                )}
              </div>
              <div className="p-2.5">
                <div className="truncate text-[12.5px] font-medium">{item.title}</div>
                <div className="mt-1.5 flex items-center justify-between">
                  <span className="rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent)]">
                    {TYPE_LABELS[item.type]}
                  </span>
                  <button
                    className="text-[11px] text-[var(--text-2)] hover:text-red-500"
                    onClick={(e) => {
                      e.stopPropagation();
                      api.favoriteRemove(item.itemId).then(refresh);
                    }}
                  >
                    取消收藏
                  </button>
                </div>
              </div>
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
