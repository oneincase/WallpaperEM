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
import {
  TAG_GROUPS,
  LIBRARY_SORTS,
  toggleTagSelection,
  selectedTagGroups,
  tagLabel,
  type TagSelection,
} from "../lib/tags";
import { readState, writeState } from "../lib/cache-snapshots";
import { IconPreview, IconApply, IconOpenFile, IconTrash } from "../components/icons";
import { SubscriptionsModal } from "../components/SubscriptionsModal";
import { VirtualGrid } from "../components/VirtualGrid";
import { tr, trMsg } from "../lib/i18n";

/** 筛选条件持久化：窗口会在内存压力下被回收重建（全新 JS 上下文），不落盘的话
    用户调好的标签/排序会静默回到默认值（与工坊页 useWorkshopFilter 同一套
    做法与键约定） */
const FILTER_STATE_KEY = "filter.library";

type PersistedFilter = {
  search: string;
  sort: string;
  selected: TagSelection;
  onlyMissing: boolean;
};

const FILTER_DEFAULTS: PersistedFilter = {
  search: "",
  sort: "downloaded_desc",
  selected: {},
  onlyMissing: false,
};

function readPersistedFilter(): PersistedFilter {
  const raw = readState<Partial<PersistedFilter> | null>(FILTER_STATE_KEY, null);
  if (!raw || typeof raw !== "object") return { ...FILTER_DEFAULTS, selected: {} };
  // 逐字段校验：localStorage 的内容可能来自旧版本，形状不能假定。
  // 旧版三态（"on"/"excluded"）里只有 "on" 迁移为选中；"excluded" 两态化后无对应物，丢弃
  const selected: TagSelection = {};
  const rawSel =
    raw.selected && typeof raw.selected === "object"
      ? raw.selected
      : ((raw as { tagState?: Record<string, string> }).tagState ?? {});
  for (const [k, v] of Object.entries(rawSel)) {
    if (v === true || v === "on") selected[k] = true;
  }
  return {
    search: typeof raw.search === "string" ? raw.search : FILTER_DEFAULTS.search,
    // 排序值可能来自旧版本已删除的选项，必须仍合法
    sort:
      typeof raw.sort === "string" && LIBRARY_SORTS.some((s) => s.value === raw.sort)
        ? raw.sort
        : FILTER_DEFAULTS.sort,
    selected,
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
  const [selected, setSelected] = useState<TagSelection>(initialFilter.selected);
  const [onlyMissing, setOnlyMissing] = useState(initialFilter.onlyMissing);
  const msg = useMessage();
  const [previewItem, setPreviewItem] = useState<LibraryItem | null>(null);
  const [deleteItem, setDeleteItem] = useState<LibraryItem | null>(null);
  const [appliedItems, setAppliedItems] = useState<Set<string>>(new Set());
  const [importing, setImporting] = useState(false);
  const [pruning, setPruning] = useState(false);
  const [confirmPrune, setConfirmPrune] = useState(false);
  const [subsOpen, setSubsOpen] = useState(false);
  // 文件已丢失的条目（数据库还留着记录）
  const missingItems = items.filter((it) => it.missing);


  const tagGroups = useMemo(() => selectedTagGroups(selected), [selected]);
  const activeCount =
    Object.keys(selected).length + (type ? 1 : 0) + (onlyMissing ? 1 : 0) +
    (debouncedSearch ? 1 : 0);

  const refresh = useCallback(async () => {
    try {
      setItems(
        await api.libraryList("", {
          type,
          query: debouncedSearch || undefined,
          tagGroups,
          onlyMissing,
          sort,
        }),
      );
    } catch (e) {
      msg.error(String(e));
    } finally {
      setLoading(false);
    }
    // tagGroups 是每次渲染新建的数组，用序列化后的值做依赖避免无限重取
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [type, debouncedSearch, sort, onlyMissing, JSON.stringify(tagGroups), msg]);

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
      if (r.imported) parts.push(tr("导入 {n} 个", { n: r.imported }));
      if (r.duplicates) parts.push(tr("{n} 个已在库中", { n: r.duplicates }));
      if (failedCount) parts.push(tr("{n} 个失败", { n: failedCount }));
      if (!parts.length) {
        msg.info(tr("没有可导入的内容"));
        return;
      }
      const text = parts.join("，");
      if (failedCount) {
        const first = r.failed![0];
        const name = first.path.split("/").pop() ?? first.path;
        msg.error(`${text}：${name} — ${trMsg(first.error)}`);
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
      if (removed.length) msg.success(tr("已清理 {n} 个失效条目", { n: removed.length }));
      else msg.info(tr("没有需要清理的条目"));
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
      selected,
      onlyMissing,
    });
  }, [search, sort, selected, onlyMissing]);

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
        <div className="pointer-events-none absolute inset-0 z-40 flex items-center justify-center rounded-xl border-2 border-dashed border-[var(--accent-strong)] bg-[var(--accent)]/70">
          <span className="rounded-lg bg-[var(--content)] px-4 py-2 text-[13px] font-medium shadow-lg">
            {tr("松开导入壁纸（支持文件与文件夹，可多个）")}
          </span>
        </div>
      )}
      {/* 工具栏 */}
      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2.5">
        <FilterButton onClick={() => setFilterOpen(true)} activeCount={activeCount} />
        <input
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder={tr("搜索名称…")}
          className="w-56 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
        />
        <select
          value={sort}
          onChange={(e) => setSort(e.target.value)}
          className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none"
        >
          {LIBRARY_SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {tr(s.label)}
            </option>
          ))}
        </select>
        {!loading && (
          <span className="text-[12px] text-[var(--text-2)]">
            {tr("{n} 张", { n: items.length })}
          </span>
        )}
        <div className="ml-auto flex shrink-0 items-center gap-1.5">
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10"
            onClick={() => setSubsOpen(true)}
            title={tr("拉取登录账号的全部订阅，一键下载缺失壁纸（也可多选下载）")}
          >
            ⇓ {tr("同步订阅")}
          </button>
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10"
            onClick={importFolder}
            disabled={importing}
            title={tr("导入包含 project.json 的 WE 壁纸工程目录")}
          >
            {tr("导入文件夹")}
          </button>
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10"
            onClick={importCustom}
            disabled={importing}
            title={tr("支持多选；也可以直接把文件/文件夹拖进窗口")}
          >
            {importing ? tr("导入中…") : `＋ ${tr("导入文件")}`}
          </button>
        </div>
      </div>

      <FilterDrawer
        open={filterOpen}
        onClose={() => setFilterOpen(false)}
        meta={!loading ? tr("{n} 张", { n: items.length }) : undefined}
        activeCount={activeCount}
        onReset={() => {
          setSelected({});
          setOnlyMissing(false);
          setSearch("");
        }}
      >
        <label className="mb-2.5 flex cursor-pointer items-center gap-2 rounded-lg px-1 py-1 text-[12px] hover:bg-black/5 dark:hover:bg-white/8">
          <input
            type="checkbox"
            checked={onlyMissing}
            onChange={(e) => setOnlyMissing(e.target.checked)}
            className="accent-[var(--accent-strong)]"
          />
          {tr("只看文件已丢失")}
        </label>
        {TAG_GROUPS.map((g) => {
          const count = g.tags.filter((t) => selected[t.name]).length;
          return (
            <FilterSection
              key={g.kind}
              label={tr(g.label)}
              count={count}
              defaultOpen={g.kind === "type" || g.kind === "genre"}
            >
              {g.tags.map((t) => (
                <TagChip
                  key={t.name}
                  label={tagLabel(t.name)}
                  state={selected[t.name] ? "on" : "off"}
                  onClick={() => setSelected((prev) => toggleTagSelection(prev, t.name))}
                  title={t.libraryOnly ? tr("本地导入的壁纸（非工坊下载）") : t.name}
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
              {tr("有 {n} 个壁纸的本地文件已丢失（可能被手动删除），仅剩数据库记录。", {
                n: missingItems.length,
              })}
            </span>
            <button
              className="ml-auto shrink-0 rounded-lg border border-amber-500/40 px-3 py-1 text-[12px] font-medium text-amber-700 hover:bg-amber-500/15 disabled:opacity-60 dark:text-amber-300"
              onClick={() => setConfirmPrune(true)}
              disabled={pruning}
            >
              {pruning ? tr("清理中…") : tr("清理失效条目")}
            </button>
          </div>
        )}

        {loading && (
          <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">{tr("加载中…")}</div>
        )}

        {!loading && items.length === 0 && (
          <div className="shrink-0 card mb-4">
            <EmptyState
              art="library"
              title={tr("本地库还是空的")}
              hint={tr(
                "在工坊下载壁纸后会自动入库；也可以导入本地文件/文件夹，或直接拖拽到这里",
              )}
            />
          </div>
        )}

        {/* 虚拟滚动：本地库条目数没有上限（实测几百项），整表渲染会让滚动掉帧 */}
        <VirtualGrid
          className="min-h-0 flex-1 overflow-y-auto"
          items={items}
          minColumnWidth={168}
          gap={12}
          keyOf={(it) => it.itemId}
          renderItem={(item) => (
            <WallpaperCard
              imageUrl={item.previewUrl ?? undefined}
              title={item.title}
              // 虚拟滚动：格子随滚动挂载/卸载，lazy 的加载判定会被跳过（白块），
              // 直接立即加载
              eager
              onOpen={() => onOpenDetail(item.itemId)}
              badges={
                item.missing ? (
                  <span className="rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                    {tr("文件丢失")}
                  </span>
                ) : appliedItems.has(item.itemId) ? (
                  <span className="rounded bg-green-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                    {tr("已应用")}
                  </span>
                ) : undefined
              }
              metaLeft={<TypeChip label={tr(TYPE_LABELS[item.type])} />}
              metaRight={`${(item.sizeBytes / 1024 / 1024).toFixed(1)} MB`}
              actions={
                <div className="grid grid-cols-4 gap-1">
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => setPreviewItem(item)}
                    data-tip={tr("预览（可在预览中配置）")}
                  >
                    <IconPreview />
                  </button>
                  {item.missing ? (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)]/40 cursor-not-allowed"
                      disabled
                      data-tip={tr("本地文件已丢失，无法应用")}
                    >
                      <IconApply />
                    </button>
                  ) : appliedItems.has(item.itemId) ? (
                    <button
                      className="flex items-center justify-center rounded-lg border border-green-500/30 px-0.5 py-1 !text-green-600 dark:!text-green-400 bg-green-500/10 cursor-default disabled:opacity-75"
                      disabled
                      data-tip={tr("已应用到桌面")}
                    >
                      <IconApply />
                    </button>
                  ) : (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--accent-strong)] px-0.5 py-1 text-[var(--accent-fg)] bg-[var(--accent)] hover:opacity-90"
                      onClick={() => apply(item.itemId)}
                      data-tip={tr("应用到桌面")}
                    >
                      <IconApply />
                    </button>
                  )}
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-black/5 dark:hover:bg-white/10"
                    onClick={() => api.libraryOpenFolder(item.itemId)}
                    data-tip={tr("打开文件所在位置")}
                  >
                    <IconOpenFile />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-red-500 hover:border-red-500/40 hover:bg-red-500/10"
                    onClick={() => setDeleteItem(item)}
                    data-tip={tr("删除")}
                  >
                    <IconTrash />
                  </button>
                </div>
              }
            />
          )}
        />
      </div>

      {previewItem && <PreviewModal item={previewItem} onClose={() => setPreviewItem(null)} />}

      {subsOpen && (
        <SubscriptionsModal
          onClose={() => setSubsOpen(false)}
          onChanged={() => {
            refresh();
            loadApplied();
          }}
        />
      )}

      {confirmPrune && (
        <ConfirmModal
          title={tr("清理失效条目")}
          message={tr(
            "将从本地库移除 {n} 个文件已丢失的条目及其自定义配置。壁纸文件本就不存在，不会删除任何磁盘文件。此操作不可恢复。",
            { n: missingItems.length },
          )}
          confirmText={tr("清理")}
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
          title={tr("删除壁纸")}
          message={tr("确定删除「{title}」及本地文件？此操作不可恢复。", {
            title: deleteItem.title,
          })}
          confirmText={tr("删除")}
          danger
          onCancel={() => setDeleteItem(null)}
          onConfirm={async () => {
            const target = deleteItem;
            setDeleteItem(null);
            try {
              await api.libraryDelete(target.itemId);
              msg.success(tr("已删除「{title}」", { title: target.title }));
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
