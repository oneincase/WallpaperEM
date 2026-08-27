// 本地库页：已下载壁纸管理 + 应用到桌面（T4）
import { useCallback, useEffect, useState } from "react";
import {
  api,
  TYPE_LABELS,
  type LibraryItem,
  type WallpaperType,
} from "../api/steam";
import { PreviewModal } from "../components/PreviewModal";
import { ConfirmModal } from "../components/ConfirmModal";

const FILTERS: (WallpaperType | "")[] = ["", "video", "scene", "web"];

export function LibraryPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [items, setItems] = useState<LibraryItem[]>([]);
  const [type, setType] = useState<WallpaperType | "">("");
  const [loading, setLoading] = useState(true);
  const [msg, setMsg] = useState("");
  const [previewItem, setPreviewItem] = useState<LibraryItem | null>(null);
  const [deleteItem, setDeleteItem] = useState<LibraryItem | null>(null);
  const [appliedItems, setAppliedItems] = useState<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    try {
      setItems(await api.libraryList(type));
    } catch (e) {
      setMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, [type]);

  const loadApplied = useCallback(async () => {
    try {
      const ids = await api.wallpaperActiveItems();
      setAppliedItems(new Set(ids));
    } catch {
      setAppliedItems(new Set());
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    loadApplied();
  }, [loadApplied]);

  const apply = async (itemId: string) => {
    setMsg("");
    try {
      await api.wallpaperApplyItem(itemId);
      // 应用新壁纸会替换所有显示器上的旧壁纸，须重取权威的已应用集合，
      // 否则旧壁纸的「已应用」状态会残留（前端只 add 不删除旧 id）。
      await loadApplied();
    } catch (e) {
      setMsg(String(e));
    }
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 flex items-center gap-3 mb-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">本地库</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">
            已下载壁纸（可在详情页应用为桌面壁纸）
          </p>
        </div>
        <div className="flex rounded-lg border border-[var(--separator)] overflow-hidden ml-auto">
          {FILTERS.map((t) => (
            <button
              key={t || "all"}
              onClick={() => setType(t)}
              className={`px-3 py-1.5 text-[12.5px] ${
                type === t
                  ? "bg-[var(--accent)] text-white"
                  : "bg-[var(--card)] hover:bg-black/5 dark:hover:bg-white/10"
              }`}
            >
              {t === "" ? "全部" : TYPE_LABELS[t]}
            </button>
          ))}
        </div>
      </div>

      {msg && <div className="shrink-0 text-[12.5px] text-[var(--text-2)] mb-4">{msg}</div>}
      {loading && <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">加载中…</div>}

      {!loading && items.length === 0 && (
        <div className="shrink-0 card p-12 text-center text-[13px] text-[var(--text-2)] mb-4">
          本地库为空 —— 在工坊下载壁纸后会自动入库
        </div>
      )}

      {/* 内容网格 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        <div className="grid grid-cols-4 gap-4">
          {items.map((item) => (
            <div key={item.itemId} className="card group overflow-hidden">
              <button onClick={() => onOpenDetail(item.itemId)} className="block w-full">
                <div className="aspect-[16/10] overflow-hidden bg-black/10">
                  {item.previewUrl ? (
                    <img src={item.previewUrl} alt={item.title} loading="lazy" className="h-full w-full object-cover" />
                  ) : (
                    <div className="flex h-full items-center justify-center text-[var(--text-2)]">
                      无预览
                    </div>
                  )}
                </div>
              </button>
              <div className="p-2.5">
                <button
                  onClick={() => onOpenDetail(item.itemId)}
                  className="block w-full truncate text-left text-[12.5px] font-medium hover:text-[var(--accent)]"
                >
                  {item.title}
                </button>
                <div className="mt-1.5 flex items-center justify-between">
                  <span className="rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent)]">
                    {TYPE_LABELS[item.type]}
                  </span>
                  <span className="text-[11px] text-[var(--text-2)]">
                    {(item.sizeBytes / 1024 / 1024).toFixed(1)} MB
                  </span>
                </div>
                <div className="mt-2 grid grid-cols-4 gap-2">
                  <button
                    className="btn !py-1.5 text-[11px] truncate"
                    onClick={() => setPreviewItem(item)}
                  >
                    预览
                  </button>
                  {appliedItems.has(item.itemId) ? (
                    <button
                      className="btn !py-1.5 text-[11px] truncate !bg-green-500/15 !text-green-600 dark:!text-green-400 !border-green-500/30 cursor-default disabled:opacity-75"
                      disabled
                      title="已应用到桌面"
                    >
                      已应用
                    </button>
                  ) : (
                    <button
                      className="btn btn-primary !py-1.5 text-[11px] truncate"
                      onClick={() => apply(item.itemId)}
                    >
                      应用
                    </button>
                  )}
                  <button
                    className="btn !py-1.5 text-[11px] truncate"
                    onClick={() => api.libraryOpenFolder(item.itemId)}
                  >
                    文件
                  </button>
                  <button
                    className="btn btn-danger !py-1.5 text-[11px] truncate"
                    onClick={() => setDeleteItem(item)}
                  >
                    删除
                  </button>
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {previewItem && <PreviewModal item={previewItem} onClose={() => setPreviewItem(null)} />}

      {deleteItem && (
        <ConfirmModal
          title="删除壁纸"
          message={`确定删除「${deleteItem.title}」及本地文件？此操作不可恢复。`}
          confirmText="删除"
          danger
          onCancel={() => setDeleteItem(null)}
          onConfirm={async () => {
            setDeleteItem(null);
            await api.libraryDelete(deleteItem.itemId);
            refresh();
          }}
        />
      )}
    </div>
  );
}
