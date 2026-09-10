// 本地库页：已下载壁纸管理 + 应用到桌面（T4）
import { useCallback, useEffect, useMemo, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  api,
  TYPE_LABELS,
  type ImportBatchResult,
  type LibraryItem,
  type WallpaperType,
} from "../api/steam";
import { PreviewModal } from "../components/PreviewModal";
import { ConfirmModal } from "../components/ConfirmModal";
import { WallpaperCard, TypeChip } from "../components/WallpaperCard";
import { useMessage } from "../components/Message";
import { EmptyState } from "../components/EmptyState";
import {
  FilterDrawer,
  FilterButton,
  FilterSection,
  TagChip,
} from "../components/FilterDrawer";
import { TAG_GROUPS, LIBRARY_SORTS } from "../lib/tags";
import { readState, writeState } from "../lib/cache-snapshots";
import { IconPreview, IconApply, IconOpenFile, IconTrash } from "../components/icons";

type TagState = Record<string, "on" | "excluded">;

/** 筛选条件持久化：窗口闲置 3s 即被释放重建（全新 JS 上下文），不落盘的话
    用户调好的标签/排序会静默回到默认值（与工坊页 useWorkshopFilter 同一套
    做法与键约定） */
const FILTER_STATE_KEY = "filter.library";

type PersistedFilter = {
  search: string;
  sort: string;
  tagState: TagState;
  onlyMissing: boolean;
};

const FILTER_DEFAULTS: PersistedFilter = {
  search: "",
  sort: "downloaded_desc",
  tagState: {},
  onlyMissing: false,
};

function readPersistedFilter(): PersistedFilter {
  const raw = readState<Partial<PersistedFilter> | null>(FILTER_STATE_KEY, null);
  if (!raw || typeof raw !== "object") return { ...FILTER_DEFAULTS, tagState: {} };
  // 逐字段校验：localStorage 的内容可能来自旧版本，形状不能假定
  const tagState: TagState = {};
  if (raw.tagState && typeof raw.tagState === "object") {
    for (const [k, v] of Object.entries(raw.tagState)) {
      if (v === "on" || v === "excluded") tagState[k] = v;
    }
  }
  return {
    search: typeof raw.search === "string" ? raw.search : FILTER_DEFAULTS.search,
    // 排序值可能来自旧版本已删除的选项，必须仍合法
    sort:
      typeof raw.sort === "string" && LIBRARY_SORTS.some((s) => s.value === raw.sort)
        ? raw.sort
        : FILTER_DEFAULTS.sort,
    tagState,
    onlyMissing: raw.onlyMissing === true,
  };
}

export function LibraryPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [items, setItems] = useState<LibraryItem[]>([]);
  // 类型筛选已并入标签面板的「类型」组；这里保留常量空值兼容 libraryList 签名
  const type = "" as WallpaperType | "";
  const [loading, setLoading] = useState(true);
  // 筛选：本地库全部走数据库查询，不在前端过滤
  const [filterOpen, setFilterOpen] = useState(false);
  const [initialFilter] = useState(readPersistedFilter);
  const [search, setSearch] = useState(initialFilter.search);
  // debouncedSearch 直接用还原值初始化：避免首帧先按空查询拉一遍、
  // 400ms 防抖到期后再按还原的搜索词重拉一次
  const [debouncedSearch, setDebouncedSearch] = useState(initialFilter.search.trim());
  const [sort, setSort] = useState<string>(initialFilter.sort);
  const [tagState, setTagState] = useState<TagState>(initialFilter.tagState);
  const [onlyMissing, setOnlyMissing] = useState(initialFilter.onlyMissing);
  const msg = useMessage();
  const [previewItem, setPreviewItem] = useState<LibraryItem | null>(null);
  const [deleteItem, setDeleteItem] = useState<LibraryItem | null>(null);
  const [appliedItems, setAppliedItems] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);
  const [pruning, setPruning] = useState(false);
  const [confirmPrune, setConfirmPrune] = useState(false);
  // 文件已丢失的条目（数据库还留着记录）
  const missingItems = items.filter((it) => it.missing);


  const tags = useMemo(
    () => Object.entries(tagState).filter(([, v]) => v === "on").map(([k]) => k),
    [tagState],
  );
  const excludedTags = useMemo(
    () => Object.entries(tagState).filter(([, v]) => v === "excluded").map(([k]) => k),
    [tagState],
  );
  const activeCount =
    tags.length + excludedTags.length + (type ? 1 : 0) + (onlyMissing ? 1 : 0) +
    (debouncedSearch ? 1 : 0);

  const refresh = useCallback(async () => {
    try {
      setItems(
        await api.libraryList("", {
          type,
          query: debouncedSearch || undefined,
          tags,
          excludedTags,
          onlyMissing,
          sort,
        }),
      );
    } catch (e) {
      msg.error(String(e));
    } finally {
      setLoading(false);
    }
    // tags/excludedTags 是每次渲染新建的数组，用序列化后的值做依赖避免无限重取
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [type, debouncedSearch, sort, onlyMissing, JSON.stringify(tags), JSON.stringify(excludedTags), msg]);

  const loadApplied = useCallback(async () => {
    try {
      const ids = await api.wallpaperActiveItems();
      setAppliedItems(new Set(ids));
    } catch {
      setAppliedItems(new Set());
    }
  }, []);

  /** 批量导入结果汇报：导入 N / 已在库 M / 失败 K（附首条失败原因） */
  const reportImport = useCallback(
    (r: ImportBatchResult) => {
      if (r.cancelled) return;
      const failedCount = r.failed?.length ?? 0;
      const parts: string[] = [];
      if (r.imported) parts.push(`导入 ${r.imported} 个`);
      if (r.duplicates) parts.push(`${r.duplicates} 个已在库中`);
      if (failedCount) parts.push(`${failedCount} 个失败`);
      if (!parts.length) {
        msg.info("没有可导入的内容");
        return;
      }
      const text = parts.join("，");
      if (failedCount) {
        const first = r.failed![0];
        const name = first.path.split("/").pop() ?? first.path;
        msg.error(`${text}：${name} — ${first.error}`);
      } else if (r.imported) {
        msg.success(text);
      } else {
        msg.info(text);
      }
      if (r.imported || r.duplicates) refresh();
    },
    [msg, refresh],
  );

  const runImport = useCallback(
    async (fn: () => Promise<ImportBatchResult>) => {
      setImporting(true);
      try {
        reportImport(await fn());
      } catch (e) {
        msg.error(String(e));
      } finally {
        setImporting(false);
      }
    },
    [msg, reportImport],
  );

  const importCustom = () => runImport(() => api.libraryImportCustomPick());
  const importFolder = () => runImport(() => api.libraryImportFolderPick());

  // 拖拽导入：把文件/文件夹拖到窗口任意位置即可批量导入
  const [dragOver, setDragOver] = useState(false);
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const p = event.payload;
        if (p.type === "enter" || p.type === "over") {
          setDragOver(true);
        } else if (p.type === "leave") {
          setDragOver(false);
        } else if (p.type === "drop") {
          setDragOver(false);
          if (p.paths.length) {
            void runImport(() => api.libraryImportCustomBatch(p.paths));
          }
        }
      })
      .then((fn) => {
        unlisten = fn;
      });
    return () => unlisten?.();
  }, [runImport]);

  const pruneMissing = async () => {
    setPruning(true);
    try {
      const removed = await api.libraryPrune();
      if (removed.length) msg.success(`已清理 ${removed.length} 个失效条目`);
      else msg.info("没有需要清理的条目");
      await refresh();
      await loadApplied();
    } catch (e) {
      msg.error(String(e));
    } finally {
      setPruning(false);
    }
  };

  // 任一条件变化就落盘。合成一个 effect 而不是在每个 setter 里写，
  // 免得漏掉将来新增的入口
  useEffect(() => {
    writeState<PersistedFilter>(FILTER_STATE_KEY, {
      search: search.trim(),
      sort,
      tagState,
      onlyMissing,
    });
  }, [search, sort, tagState, onlyMissing]);

  useEffect(() => {
    const t = setTimeout(() => setDebouncedSearch(search.trim()), 400);
    return () => clearTimeout(t);
  }, [search]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    loadApplied();
  }, [loadApplied]);

  const apply = async (itemId: string) => {
    try {
      await api.wallpaperApplyItem(itemId);
      // 应用新壁纸会替换所有显示器上的旧壁纸，须重取权威的已应用集合，
      // 否则旧壁纸的「已应用」状态会残留（前端只 add 不删除旧 id）。
      await loadApplied();
    } catch (e) {
      msg.error(String(e));
    }
  };

  return (
    // relative：筛选抽屉用 absolute 覆盖在本页之上
    <div className="relative flex h-full flex-col overflow-hidden px-7 py-5">
      {/* 拖拽导入悬停遮罩（仅提示；事件由 webview 拖拽监听处理） */}
      {dragOver && (
        <div className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center rounded-xl border-2 border-dashed border-[var(--accent)] bg-[var(--accent)]/10">
          <span className="rounded-lg bg-[var(--sidebar)] px-4 py-2 text-[13px] font-medium shadow-lg">
            松开导入壁纸（支持文件与文件夹，可多个）
          </span>
        </div>
      )}
      {/* 工具栏 */}
      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2.5">
        <FilterButton onClick={() => setFilterOpen(true)} activeCount={activeCount} />
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="搜索名称…"
          className="w-56 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent)]"
        />
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value)}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none"
        >
          {LIBRARY_SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        {!loading && (
          <span className="text-[12px] text-[var(--text-2)]">{items.length} 张</span>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10"
            onClick={importFolder}
            disabled={importing}
            title="导入包含 project.json 的 WE 壁纸工程目录"
          >
            导入文件夹
          </button>
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10"
            onClick={importCustom}
            disabled={importing}
            title="支持多选；也可以直接把文件/文件夹拖进窗口"
          >
            {importing ? "导入中…" : "＋ 导入文件"}
          </button>
        </div>
      </div>

      <FilterDrawer
        open={filterOpen}
        onClose={() => setFilterOpen(false)}
        meta={!loading ? `${items.length} 张` : undefined}
        activeCount={activeCount}
        onReset={() => {
          setTagState({});
          setOnlyMissing(false);
          setSearch("");
        }}
      >
        <label className="mb-2.5 flex cursor-pointer items-center gap-2 rounded-lg px-1 py-1 text-[12px] hover:bg-black/5 dark:hover:bg-white/8">
          <input
            type="checkbox"
            checked={onlyMissing}
            onChange={(e) => setOnlyMissing(e.target.checked)}
            className="accent-[var(--accent)]"
          />
          只看文件已丢失
        </label>
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
                  onClick={() =>
                    setTagState((prev) => {
                      const next = { ...prev };
                      if (!next[t.name]) next[t.name] = "on";
                      else if (next[t.name] === "on") next[t.name] = "excluded";
                      else delete next[t.name];
                      return next;
                    })
                  }
                  title={t.name}
                />
              ))}
            </FilterSection>
          );
        })}
      </FilterDrawer>

      <div className="flex min-h-0 flex-1 flex-col">
        {missingItems.length > 0 && (
          <div className="mb-3 flex shrink-0 items-center gap-3 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-2.5">
            <span className="text-[12.5px] text-amber-700 dark:text-amber-300">
              有 {missingItems.length} 个壁纸的本地文件已丢失（可能被手动删除），仅剩数据库记录。
            </span>
            <button
              className="ml-auto shrink-0 rounded-lg border border-amber-500/40 px-3 py-1 text-[12px] font-medium text-amber-700 hover:bg-amber-500/15 disabled:opacity-60 dark:text-amber-300"
              onClick={() => setConfirmPrune(true)}
              disabled={pruning}
            >
              {pruning ? "清理中…" : "清理失效条目"}
            </button>
          </div>
        )}

        {loading && <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">加载中…</div>}

        {!loading && items.length === 0 && (
          <div className="shrink-0 card mb-4">
            <EmptyState
              art="library"
              title="本地库还是空的"
              hint="在工坊下载壁纸后会自动入库；也可以导入本地文件/文件夹，或直接拖拽到这里"
            />
          </div>
        )}

        <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(168px,1fr))]">
          {items.map((item) => (
            <WallpaperCard
              key={item.itemId}
              imageUrl={item.previewUrl ?? undefined}
              title={item.title}
              onOpen={() => onOpenDetail(item.itemId)}
              badges={
                item.missing ? (
                  <span className="rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                    文件丢失
                  </span>
                ) : appliedItems.has(item.itemId) ? (
                  <span className="rounded bg-green-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                    已应用
                  </span>
                ) : undefined
              }
              metaLeft={<TypeChip label={TYPE_LABELS[item.type]} />}
              metaRight={`${(item.sizeBytes / 1024 / 1024).toFixed(1)} MB`}
              actions={
                <div className="grid grid-cols-4 gap-1">
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => setPreviewItem(item)}
                    data-tip="预览（可在预览中配置）"
                  >
                    <IconPreview />
                  </button>
                  {item.missing ? (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)]/40 cursor-not-allowed"
                      disabled
                      data-tip="本地文件已丢失，无法应用"
                    >
                      <IconApply />
                    </button>
                  ) : appliedItems.has(item.itemId) ? (
                    <button
                      className="flex items-center justify-center rounded-lg border border-green-500/30 px-0.5 py-1 !text-green-600 dark:!text-green-400 bg-green-500/10 cursor-default disabled:opacity-75"
                      disabled
                      data-tip="已应用到桌面"
                    >
                      <IconApply />
                    </button>
                  ) : (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--accent)] px-0.5 py-1 text-white bg-[var(--accent)] hover:opacity-90"
                      onClick={() => apply(item.itemId)}
                      data-tip="应用到桌面"
                    >
                      <IconApply />
                    </button>
                  )}
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => api.libraryOpenFolder(item.itemId)}
                    data-tip="打开文件所在位置"
                  >
                    <IconOpenFile />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-red-500 hover:border-red-500/40 hover:bg-red-500/10"
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
      </div>

      {previewItem && <PreviewModal item={previewItem} onClose={() => setPreviewItem(null)} />}

      {confirmPrune && (
        <ConfirmModal
          title="清理失效条目"
          message={`将从本地库移除 ${missingItems.length} 个文件已丢失的条目及其自定义配置。壁纸文件本就不存在，不会删除任何磁盘文件。此操作不可恢复。`}
          confirmText="清理"
          danger
          onCancel={() => setConfirmPrune(false)}
          onConfirm={() => {
            setConfirmPrune(false);
            pruneMissing();
          }}
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
            const target = deleteItem;
            setDeleteItem(null);
            try {
              await api.libraryDelete(target.itemId);
              msg.success(`已删除「${target.title}」`);
            } catch (e) {
              msg.error(String(e));
            }
            refresh();
            loadApplied();
          }}
        />
      )}
    </div>
  );
}
