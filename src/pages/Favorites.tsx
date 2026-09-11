// 收藏页（T4）
import { useCallback, useEffect, useState } from "react";
import { api, TYPE_LABELS, type FavoriteItem } from "../api/steam";
import { WallpaperCard, TypeChip, CoverCountBadge } from "../components/WallpaperCard";
import { EmptyState } from "../components/EmptyState";
import { tr } from "../lib/i18n";

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
        <h1 className="text-[22px] font-bold tracking-tight">{tr("收藏")}</h1>
        <p className="text-[13px] text-[var(--text-2)] mt-1">{tr("收藏的工坊壁纸")}</p>
      </div>

      {loading && (
        <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">{tr("加载中…")}</div>
      )}
      {!loading && items.length === 0 && (
        <div className="shrink-0 card mb-4">
          <EmptyState
            art="favorite"
            title={tr("还没有收藏")}
            hint={tr("在壁纸详情页点击「收藏」，喜欢的壁纸会集中到这里")}
          />
        </div>
      )}

      {/* 内容网格 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        <div className="grid grid-cols-4 gap-4">
          {items.map((item) => (
            <WallpaperCard
              key={item.itemId}
              imageUrl={item.previewUrl ?? undefined}
              title={item.title}
              onOpen={() => onOpenDetail(item.itemId)}
              coverBadgeRight={
                item.subscriptions > 0 ? <CoverCountBadge count={item.subscriptions} /> : undefined
              }
              metaLeft={<TypeChip label={tr(TYPE_LABELS[item.type])} />}
              metaRight={
                <button
                  className="text-[11px] text-[var(--text-2)] hover:text-red-500"
                  onClick={() => {
                    api.favoriteRemove(item.itemId).then(refresh);
                  }}
                >
                  {tr("取消收藏")}
                </button>
              }
            />
          ))}
        </div>
      </div>
    </div>
  );
}
