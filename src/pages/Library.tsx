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
import { WallpaperPropsModal } from "../components/WallpaperPropsModal";
import { fetchItemPropsCached } from "../hooks/useItemProps";
import { WallpaperCard, TypeChip } from "../components/WallpaperCard";
import { IconPreview, IconApply, IconOpenFile, IconTrash, IconSliders } from "../components/icons";

const FILTERS: (WallpaperType | "")[] = ["", "video", "scene", "web"];

export function LibraryPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [items, setItems] = useState<LibraryItem[]>([]);
  const [type, setType] = useState<WallpaperType | "">("");
  const [loading, setLoading] = useState(true);
  const [msg, setMsg] = useState("");
  const [previewItem, setPreviewItem] = useState<LibraryItem | null>(null);
  const [deleteItem, setDeleteItem] = useState<LibraryItem | null>(null);
  // 自定义属性：project.json 是否声明了可配置项（决定卡片「属性」按钮可用性）+ 当前编辑条目
  const [customizable, setCustomizable] = useState<Record<string, boolean>>({});
  const [propsItem, setPropsItem] = useState<LibraryItem | null>(null);
  const [appliedItems, setAppliedItems] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);

  const importCustom = async () => {
    setImporting(true);
    setMsg("");
    try {
      const r = await api.libraryImportCustomPick();
      if (!r.cancelled && r.imported) {
        setMsg(`✅ 已导入「${r.title ?? ""}」`);
        refresh();
      }
    } catch (e) {
      setMsg(String(e));
    } finally {
      setImporting(false);
    }
  };

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

  // 逐条读 project.json（带会话级缓存）判断哪些壁纸可自定义属性
  useEffect(() => {
    let alive = true;
    Promise.all(
      items.map(async (it) => [it.itemId, (await fetchItemPropsCached(it.itemId)).length > 0] as const)
    ).then((pairs) => {
      if (alive) setCustomizable(Object.fromEntries(pairs));
    });
    return () => {
      alive = false;
    };
  }, [items]);

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
          <h1 className="text-[22px] font-bold tracking-tight">
            本地库
            {!loading && items.length > 0 && (
              <span className="ml-2 align-middle rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[13px] font-semibold text-[var(--accent)]">
                {items.length} 张
              </span>
            )}
          </h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">
            已下载壁纸（可在详情页应用为桌面壁纸）
          </p>
        </div>
        <div className="flex items-center gap-3 ml-auto">
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 dark:hover:bg-white/10 disabled:opacity-60"
            onClick={importCustom}
            disabled={importing}
          >
            {importing ? "导入中…" : "＋ 导入本地"}
          </button>
          <div className="flex rounded-lg border border-[var(--separator)] overflow-hidden">
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
            <WallpaperCard
              key={item.itemId}
              imageUrl={item.previewUrl ?? undefined}
              title={item.title}
              onOpen={() => onOpenDetail(item.itemId)}
              badges={
                appliedItems.has(item.itemId) ? (
                  <span className="rounded bg-green-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                    已应用
                  </span>
                ) : undefined
              }
              metaLeft={<TypeChip label={TYPE_LABELS[item.type]} />}
              metaRight={`${(item.sizeBytes / 1024 / 1024).toFixed(1)} MB`}
              actions={
                <div className="grid grid-cols-5 gap-2">
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-1 py-1.5 text-[var(--text-2)] hover:text-[var(--accent)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => setPreviewItem(item)}
                    data-tip="预览"
                  >
                    <IconPreview />
                  </button>
                  <button
                    className={`flex items-center justify-center rounded-lg border px-1 py-1.5 ${
                      customizable[item.itemId]
                        ? "border-[var(--separator)] text-[var(--text-2)] hover:text-[var(--accent)] hover:bg-black/5 dark:hover:bg-white/10"
                        : "border-[var(--separator)] text-[var(--text-2)]/40 cursor-not-allowed"
                    }`}
                    disabled={customizable[item.itemId] === false}
                    onClick={() => setPropsItem(item)}
                    data-tip={
                      customizable[item.itemId] === false ? "该壁纸无可自定义配置" : "自定义属性"
                    }
                  >
                    <IconSliders />
                  </button>
                  {appliedItems.has(item.itemId) ? (
                    <button
                      className="flex items-center justify-center rounded-lg border border-green-500/30 px-1 py-1.5 !text-green-600 dark:!text-green-400 bg-green-500/10 cursor-default disabled:opacity-75"
                      disabled
                      data-tip="已应用到桌面"
                    >
                      <IconApply />
                    </button>
                  ) : (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--accent)] px-1 py-1.5 text-white bg-[var(--accent)] hover:opacity-90"
                      onClick={() => apply(item.itemId)}
                      data-tip="应用到桌面"
                    >
                      <IconApply />
                    </button>
                  )}
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-1 py-1.5 text-[var(--text-2)] hover:text-[var(--accent)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => api.libraryOpenFolder(item.itemId)}
                    data-tip="打开文件所在位置"
                  >
                    <IconOpenFile />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-1 py-1.5 text-[var(--text-2)] hover:text-red-500 hover:border-red-500/40 hover:bg-red-500/10"
                    onClick={() => setDeleteItem(item)}
                    data-tip="删除"
                  >
                    <IconTrash />
                  </button>
                </div>
              }
            />
          ))}
        </div>
      </div>

      {previewItem && <PreviewModal item={previewItem} onClose={() => setPreviewItem(null)} />}

      {propsItem && (
        <WallpaperPropsModal
          itemId={propsItem.itemId}
          title={propsItem.title}
          onClose={() => setPropsItem(null)}
        />
      )}

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
