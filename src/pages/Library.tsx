// 本地库页：已下载壁纸管理 + 应用到桌面（T4）
import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  api,
  TYPE_LABELS,
  type DisplayInfo,
  type ImportBatchResult,
  type LibraryItem,
  type Playlist,
  type PlaylistStatus,
  type WallpaperType,
} from "../api/steam";
import { PreviewModal } from "../components/PreviewModal";
import { ConfirmModal } from "../components/ConfirmModal";
import { WallpaperCard, TypeChip, CoverSizeBadge } from "../components/WallpaperCard";
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
import {
  IconPreview,
  IconApply,
  IconOpenFile,
  IconTrash,
  IconUpload,
} from "../components/icons";
import { AnchoredMenu, MenuItem, MenuInputRow } from "../components/AnchoredMenu";
import { formatCountdown } from "../lib/format";
import { SubscriptionsModal } from "../components/SubscriptionsModal";
import { LibraryImportModal } from "../components/LibraryImportModal";
import { WorkshopUploadModal } from "../components/WorkshopUploadModal";
import {
  WorkshopUploadChoiceModal,
  type UploadMethod,
} from "../components/WorkshopUploadChoiceModal";
import { WorkshopWebUploadModal } from "../components/WorkshopWebUploadModal";
import { VirtualGrid } from "../components/VirtualGrid";
import { useApplyWallpaper } from "../hooks/useApplyWallpaper";
import { cancelApplyTarget, useArmedApplyTarget } from "../lib/apply-target";
import { tr, trMsg } from "../lib/i18n";

/** 筛选条件持久化：窗口会在内存压力下被回收重建（全新 JS 上下文），不落盘的话
    用户调好的标签/排序会静默回到默认值（与工坊页 useWorkshopFilter 同一套
    做法与键约定） */
const FILTER_STATE_KEY = "filter.library";

/** 上传到创意工坊：功能代码（弹框 / state / 后端）完整保留，入口暂时隐藏，
    待功能优化后把此常量改回 true 即恢复卡片右上角的上传按钮（与大小徽标并排） */
const WORKSHOP_UPLOAD_ENABLED = false;

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
  // 正在上传到创意工坊的条目（弹框）
  const [uploadItem, setUploadItem] = useState<LibraryItem | null>(null);
  // 已选过上传方式、进入具体上传流程的条目（Steam 客户端 / 网页版）
  const [uploadMethodItem, setUploadMethodItem] = useState<{
    item: LibraryItem;
    method: UploadMethod;
  } | null>(null);
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
      // 批量扫描根：引用模式提示（壁纸文件留在源目录）
      const linkedHint = r.skippedDirs
        ? tr("（批量扫描跳过 {n} 个非壁纸目录；壁纸以引用方式入库，请勿移动/删除源文件夹）", { n: r.skippedDirs })
        : "";
      if (failedCount) {
        const first = r.failed![0];
        const name = first.path.split("/").pop() ?? first.path;
        msg.error(`${text}：${name} — ${trMsg(first.error)}`);
      } else if (r.imported) {
        msg.success(text + (linkedHint ? " " + linkedHint : ""));
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

  const [importOpen, setImportOpen] = useState(false);

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

  const { apply: applyWithTarget, menuNode: applyMenu } = useApplyWallpaper();
  const armedTarget = useArmedApplyTarget();

  // ---- 切换列表（并入本地库：chips 筛选 + 卡片选中批量加入，全程无弹框）----
  const [playlists, setPlaylists] = useState<Playlist[]>([]);
  const [plStatus, setPlStatus] = useState<PlaylistStatus | null>(null);
  const [boundNames, setBoundNames] = useState<Map<number, string[]>>(new Map());
  const [displays, setDisplays] = useState<DisplayInfo[]>([]);
  const [dispMode, setDispMode] = useState("unified");
  const [listFilter, setListFilter] = useState<number | null>(null);
  const [selectMode, setSelectMode] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  // 锚定菜单（加入列表 / 独立模式启用选屏）与内联新建
  const [addMenuAt, setAddMenuAt] = useState<{ x: number; y: number } | null>(null);
  const [enableAt, setEnableAt] = useState<{ x: number; y: number; p: Playlist } | null>(null);
  const [newName, setNewName] = useState("");
  const [creating, setCreating] = useState(false);
  // 列表上下文条的内联编辑
  const [renameDraft, setRenameDraft] = useState<string | null>(null);
  const [intervalDraft, setIntervalDraft] = useState("");
  const [deleteList, setDeleteList] = useState<Playlist | null>(null);
  const [rotNow, setRotNow] = useState(Date.now());

  const loadPlaylists = useCallback(async () => {
    try {
      const [ls, st, dl] = await Promise.all([
        api.playlistList(),
        api.playlistStatus(),
        api.wallpaperDisplaysList(),
      ]);
      setPlaylists(ls);
      setPlStatus(st);
      setDispMode(dl.mode);
      setDisplays(dl.displays);
      const m = new Map<number, string[]>();
      for (const d of dl.displays) {
        const pid = d.binding?.playlistId;
        if (pid != null) m.set(pid, [...(m.get(pid) ?? []), d.name]);
      }
      setBoundNames(m);
    } catch {
      // 列表功能不可用不影响浏览/筛选
    }
  }, []);

  useEffect(() => {
    void loadPlaylists();
  }, [loadPlaylists]);

  // 轮播倒计时每秒本地走；状态 20s 对齐一次
  useEffect(() => {
    const t1 = window.setInterval(() => setRotNow(Date.now()), 1000);
    const t2 = window.setInterval(() => void loadPlaylists(), 20_000);
    return () => {
      window.clearInterval(t1);
      window.clearInterval(t2);
    };
  }, [loadPlaylists]);

  // 托盘「轮播」子菜单改了暂停状态：轮播条的暂停/恢复钮立即跟上
  // （本地动作走 plAct 已自带刷新，这里只补托盘/MCP 入口）
  useEffect(() => {
    const un = listen<{ key: string }>("settings-changed", (e) => {
      if (e.payload.key === "playlist_rotation_paused") void loadPlaylists();
    });
    return () => {
      void un.then((f) => f());
    };
  }, [loadPlaylists]);

  // 切到某列表时初始化间隔输入框
  useEffect(() => {
    const p = playlists.find((x) => x.id === listFilter);
    setIntervalDraft(p ? String(Math.max(1, Math.round(p.intervalSec / 60))) : "");
    setRenameDraft(null);
  }, [listFilter, playlists]);

  const activeList = useMemo(
    () => playlists.find((x) => x.id === listFilter) ?? null,
    [playlists, listFilter],
  );

  // 列表筛选在服务端筛选结果之上做二次过滤（列表 = 条目 id 集合）
  const shownItems = useMemo(() => {
    if (listFilter == null) return items;
    const p = playlists.find((x) => x.id === listFilter);
    // 按播放顺序显示（itemIds 的顺序即轮播顺序），其它筛选仍在其上收窄
    const order = new Map((p?.itemIds ?? []).map((id, i) => [id, i] as const));
    return items
      .filter((i) => order.has(i.itemId))
      .sort((a, b) => (order.get(a.itemId) ?? 0) - (order.get(b.itemId) ?? 0));
  }, [items, listFilter, playlists]);

  /** 切到列表上下文：清掉其它筛选，保证点开列表一定看到它的壁纸 */
  const clearFilters = useCallback(() => {
    setSearch("");
    setDebouncedSearch("");
    setSelected({});
    setOnlyMissing(false);
  }, []);

  const plAct = useCallback(
    async (fn: () => Promise<unknown>, ok?: string) => {
      try {
        await fn();
        if (ok) msg.success(ok);
        await loadPlaylists();
      } catch (e) {
        msg.error(String(e));
      }
    },
    [msg, loadPlaylists],
  );

  const toggleSelect = useCallback((id: string) => {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  /** 选中条目并入某列表（去重追加） */
  const addSelectionTo = async (pid: number) => {
    const p = playlists.find((x) => x.id === pid);
    if (!p) return;
    setAddMenuAt(null);
    const merged = [...p.itemIds, ...[...picked].filter((id) => !p.itemIds.includes(id))];
    await plAct(
      () => api.playlistUpdate(pid, { itemIds: merged }),
      tr("已把 {n} 张加入「{name}」", { n: picked.size, name: p.name }),
    );
    setPicked(new Set());
  };

  const createListWithSelection = async () => {
    const name = newName.trim();
    if (!name) {
      msg.error(tr("请填写列表名称"));
      return;
    }
    setAddMenuAt(null);
    setCreating(false);
    setNewName("");
    await plAct(
      () => api.playlistCreate(name, [...picked], 600, false),
      tr("已新建「{name}」", { name }),
    );
    setPicked(new Set());
  };

  /** 从当前查看的列表移出选中条目 */
  const removeSelectionFromList = async () => {
    if (!activeList) return;
    const kept = activeList.itemIds.filter((id) => !picked.has(id));
    await plAct(
      () => api.playlistUpdate(activeList.id, { itemIds: kept }),
      tr("已从「{name}」移出 {n} 张", { name: activeList.name, n: picked.size }),
    );
    setPicked(new Set());
  };
  const apply = async (itemId: string, anchor?: HTMLElement) => {
    try {
      const r = await applyWithTarget(itemId, anchor);
      // 应用后须重取权威的已应用集合（多屏时可同时有多条「已应用」），
      // 否则旧壁纸的「已应用」状态会残留（前端只 add 不删除旧 id）。
      if (r === "done") await loadApplied();
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
      {/* 显示器页「更换壁纸」锁定的目标屏提示（应用后或点取消自动解除） */}
      {armedTarget && (
        <div className="mb-3 flex shrink-0 items-center gap-2 rounded-lg border border-[var(--accent-strong)]/40 bg-[var(--accent)]/10 px-3 py-1.5 text-[12.5px]">
          <span className="flex-1 truncate">
            {tr("正在为「{name}」选择壁纸 —— 点「应用」只设置该屏", { name: armedTarget.name })}
          </span>
          <button className="btn !py-0.5 text-[11.5px]" onClick={cancelApplyTarget}>
            {tr("取消")}
          </button>
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
        <button
          className={`rounded-lg border px-3 py-1.5 text-[12.5px] font-medium transition-colors ${
            selectMode
              ? "border-[var(--accent-strong)] bg-[var(--accent-strong)] text-[var(--content)]"
              : "border-[var(--separator)] hover:bg-black/5 dark:hover:bg-white/10"
          }`}
          onClick={() => {
            setSelectMode((v) => !v);
            setPicked(new Set());
          }}
          title={tr("点选卡片批量加入切换列表")}
        >
          ✓ {tr("开启多选")}
        </button>
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
            onClick={() => setImportOpen(true)}
            title={tr("添加壁纸目录 / 导入文件夹 / 导入文件")}
          >
            ＋ {tr("导入壁纸")}
          </button>
        </div>
      </div>

      {/* 切换列表 chips（点击 = 筛选该列表的条目）+ 新建 + 轮播运行指示 */}
      <div className="mb-2 flex shrink-0 flex-wrap items-center gap-1.5">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-[var(--text-2)]/70">
          {tr("切换列表")}
        </span>
        <button
          onClick={() => setListFilter(null)}
          className={`rounded-full border px-2.5 py-0.5 text-[12px] transition-colors ${
            listFilter === null
              ? "border-[var(--accent-strong)] bg-[var(--accent-strong)] text-[var(--content)]"
              : "border-[var(--separator)] text-[var(--text-2)] hover:border-[var(--accent-strong)]/50"
          }`}
        >
          {tr("全部")}
        </button>
        {playlists.map((p) => (
          <button
            key={p.id}
            onClick={() => {
              if (listFilter === p.id) {
                setListFilter(null);
              } else {
                clearFilters();
                setListFilter(p.id);
              }
            }}
            className={`rounded-full border px-2.5 py-0.5 text-[12px] transition-colors ${
              listFilter === p.id
                ? "border-[var(--accent-strong)] bg-[var(--accent-strong)] text-[var(--content)]"
                : "border-[var(--separator)] text-[var(--text-2)] hover:border-[var(--accent-strong)]/50"
            }`}
          >
            {p.name}
            <span className="ml-1 text-[10px] opacity-60">{p.itemIds.length}</span>
          </button>
        ))}
        {creating ? (
          <span className="flex items-center gap-1 rounded-full border border-[var(--accent-strong)]/60 px-2 py-0.5">
            <input
              autoFocus
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void createListWithSelection();
                if (e.key === "Escape") {
                  setCreating(false);
                  setNewName("");
                }
                e.stopPropagation();
              }}
              placeholder={tr("列表名称")}
              className="w-24 bg-transparent text-[12px] outline-none placeholder:text-[var(--text-2)]"
            />
            <button
              className="text-[11px] font-medium text-[var(--accent-strong)]"
              onClick={() => void createListWithSelection()}
            >
              {tr("建")}
            </button>
          </span>
        ) : (
          <button
            onClick={() => {
              setCreating(true);
              if (selectMode) setAddMenuAt(null);
            }}
            title={tr("新建切换列表")}
            className="rounded-full border border-dashed border-[var(--separator)] px-2.5 py-0.5 text-[12px] text-[var(--text-2)] transition-colors hover:border-[var(--accent-strong)]/60 hover:text-[var(--accent-strong)]"
          >
            ＋ {tr("新建")}
          </button>
        )}

        {/* 轮播运行指示 + 快捷控制 */}
        {(plStatus?.active || boundNames.size > 0) && (
          <span className="ml-auto flex items-center gap-2 rounded-full bg-[var(--accent)]/10 px-3 py-0.5 text-[12px] text-[var(--text-2)]">
            <span className="text-[var(--accent-strong)]">▶</span>
            <span className="font-medium text-[var(--text-1)]">
              {plStatus?.active && plStatus.mode !== "independent"
                ? tr("轮播：{name} {i}/{t}", {
                    name: plStatus.name ?? "",
                    i: (plStatus.index ?? 0) + 1,
                    t: plStatus.total ?? 0,
                  })
                : tr("{n} 块屏在轮播", { n: boundNames.size })}
            </span>
            {plStatus?.active &&
              plStatus.mode !== "independent" &&
              (plStatus.paused ? (
                <span>{tr("已暂停")}</span>
              ) : plStatus.nextAtMs ? (
                <span>{formatCountdown(plStatus.nextAtMs - rotNow)}</span>
              ) : null)}
            <span className="mx-0.5 h-3 w-px bg-[var(--separator)]" />
            <button
              title={tr("上一张")}
              className="hover:text-[var(--accent-strong)]"
              onClick={() => void plAct(() => api.wallpaperPrev())}
            >
              ‹
            </button>
            <button
              title={tr("下一张")}
              className="hover:text-[var(--accent-strong)]"
              onClick={() => void plAct(() => api.wallpaperNext())}
            >
              ›
            </button>
            <button
              title={plStatus?.paused ? tr("恢复轮播") : tr("暂停轮播")}
              className="hover:text-[var(--accent-strong)]"
              onClick={() => void plAct(() => api.wallpaperRotationSet(!plStatus?.paused))}
            >
              {plStatus?.paused ? "▶" : "⏸"}
            </button>
            <button
              title={tr("停止轮播")}
              className="hover:text-red-500"
              onClick={() => void plAct(() => api.playlistStop(), tr("已停止轮播"))}
            >
              ⏹
            </button>
          </span>
        )}
      </div>

      {/* 列表上下文条：重命名 / 间隔 / 随机 / 启用 / 删除，全部内联编辑 */}
      {activeList && (
        <div className="mb-3 flex shrink-0 flex-wrap items-center gap-2.5 rounded-xl border border-[var(--separator)] bg-[var(--card)]/60 px-3 py-2 text-[12.5px] backdrop-blur">
          {renameDraft !== null ? (
            <input
              autoFocus
              value={renameDraft}
              onChange={(e) => setRenameDraft(e.target.value)}
              onBlur={() => {
                const name = renameDraft.trim();
                setRenameDraft(null);
                if (name && name !== activeList.name) {
                  void plAct(() => api.playlistUpdate(activeList.id, { name }));
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") setRenameDraft(null);
                e.stopPropagation();
              }}
              className="w-36 rounded-md border border-[var(--accent-strong)]/60 bg-[var(--content)] px-2 py-0.5 text-[13px] font-medium outline-none"
            />
          ) : (
            <button
              className="font-medium hover:text-[var(--accent-strong)]"
              title={tr("重命名")}
              onClick={() => setRenameDraft(activeList.name)}
            >
              {activeList.name} <span className="text-[11px] opacity-50">✎</span>
            </button>
          )}
          <span className="text-[var(--text-2)]">
            {tr("{n} 项", { n: activeList.itemIds.length })}
          </span>
          <label className="flex items-center gap-1 text-[var(--text-2)]">
            {tr("间隔")}
            <input
              type="number"
              min={1}
              value={intervalDraft}
              onChange={(e) => setIntervalDraft(e.target.value)}
              onBlur={() => {
                const m = Math.max(1, Number(intervalDraft) || 1);
                if (m * 60 !== activeList.intervalSec) {
                  void plAct(() => api.playlistUpdate(activeList.id, { intervalSec: m * 60 }));
                }
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                e.stopPropagation();
              }}
              className="w-14 rounded-md border border-[var(--separator)] bg-[var(--content)] px-1.5 py-0.5 text-[12px] outline-none focus:border-[var(--accent-strong)]"
            />
            {tr("分钟")}
          </label>
          <button
            className={`rounded-full border px-2.5 py-0.5 text-[11.5px] transition-colors ${
              activeList.shuffle
                ? "border-[var(--accent-strong)] bg-[var(--accent-strong)] text-[var(--content)]"
                : "border-[var(--separator)] text-[var(--text-2)] hover:border-[var(--accent-strong)]/50"
            }`}
            title={tr("随机播放（一轮内不重复）")}
            onClick={() =>
              void plAct(() => api.playlistUpdate(activeList.id, { shuffle: !activeList.shuffle }))
            }
          >
            ⇄ {tr("随机")}
          </button>
          {boundNames.has(activeList.id) && (
            <span className="text-[11.5px] text-[var(--accent-strong)]">
              {tr("已绑定：{names}", { names: (boundNames.get(activeList.id) ?? []).join("、") })}
            </span>
          )}
          <div className="ml-auto flex items-center gap-2">
            <button
              className="btn btn-primary !py-0.5 text-[11.5px]"
              onClick={(e) => {
                if (dispMode === "independent") {
                  const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                  setEnableAt({ x: r.left, y: r.bottom, p: activeList });
                } else {
                  void plAct(() => api.playlistApply(activeList.id), tr("已启用轮播"));
                }
              }}
            >
              {tr("启用轮播")}
            </button>
            <button
              className="btn btn-danger !py-0.5 text-[11.5px]"
              onClick={() => setDeleteList(activeList)}
            >
              {tr("删除")}
            </button>
          </div>
        </div>
      )}

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

        {/* 列表视图空态：分清「列表本来没壁纸」和「被当前筛选收窄没了」 */}
        {!loading && listFilter != null && shownItems.length === 0 && (
          <div className="shrink-0 card mb-4">
            <EmptyState
              art="library"
              title={
                (activeList?.itemIds.length ?? 0) === 0
                  ? tr("「{name}」还没有壁纸", { name: activeList?.name ?? "" })
                  : tr("当前筛选下没有「{name}」的壁纸", { name: activeList?.name ?? "" })
              }
              hint={
                (activeList?.itemIds.length ?? 0) === 0
                  ? tr("开「选择」点选卡片，底部一键加入本列表")
                  : tr("本列表有 {n} 张，被当前搜索/筛选收窄没了", {
                      n: activeList?.itemIds.length ?? 0,
                    })
              }
            />
            {(activeList?.itemIds.length ?? 0) > 0 && (
              <div className="mb-3 mt-1 flex justify-center">
                <button className="btn !py-1 text-[12px]" onClick={clearFilters}>
                  {tr("清除筛选")}
                </button>
              </div>
            )}
          </div>
        )}

        {/* 虚拟滚动：本地库条目数没有上限（实测几百项），整表渲染会让滚动掉帧 */}
        <VirtualGrid
          className="min-h-0 flex-1 overflow-y-auto"
          items={shownItems}
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
              selectMode={selectMode}
              selected={picked.has(item.itemId)}
              onToggleSelect={() => toggleSelect(item.itemId)}
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
              coverBadgeRight={
                <div className="flex items-center gap-1">
                  <CoverSizeBadge bytes={item.sizeBytes} />
                  {WORKSHOP_UPLOAD_ENABLED &&
                  (item.type === "scene" || item.type === "web" || item.type === "video") &&
                  !item.missing ? (
                    <button
                      className="flex items-center justify-center rounded bg-black/55 px-1.5 py-1 text-white/85 backdrop-blur-sm transition-colors hover:bg-black/75 hover:text-white"
                      onClick={(e) => {
                        e.stopPropagation();
                        setUploadItem(item);
                      }}
                      data-tip={
                        item.publishedFileId
                          ? tr("更新到创意工坊（Steam 客户端 / 网页）")
                          : tr("上传到创意工坊（Steam 客户端 / 网页）")
                      }
                    >
                      <IconUpload />
                    </button>
                  ) : undefined}
                </div>
              }
              metaLeft={<TypeChip label={tr(TYPE_LABELS[item.type])} />}
              actions={
                selectMode ? undefined : (
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
                      className="flex items-center justify-center rounded-lg border border-green-500/30 px-0.5 py-1 !text-green-600 dark:!text-green-400 bg-green-500/10 hover:opacity-80"
                      onClick={(e) => void apply(item.itemId, e.currentTarget)}
                      data-tip={tr("已应用到桌面（可点击重新应用或指定屏）")}
                    >
                      <IconApply />
                    </button>
                  ) : (
                    <button
                      className="flex items-center justify-center rounded-lg border border-[var(--accent-strong)] px-0.5 py-1 text-[var(--accent-fg)] bg-[var(--accent)] hover:opacity-90"
                      onClick={(e) => void apply(item.itemId, e.currentTarget)}
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
                )
              }
            />
          )}
        />
      </div>

      {previewItem && <PreviewModal item={previewItem} onClose={() => setPreviewItem(null)} />}

      {uploadItem && !uploadMethodItem && (
        <WorkshopUploadChoiceModal
          item={uploadItem}
          onClose={() => setUploadItem(null)}
          onChoose={(method) => setUploadMethodItem({ item: uploadItem, method })}
        />
      )}

      {uploadItem && uploadMethodItem?.method === "steam" && (
        <WorkshopUploadModal
          item={uploadMethodItem.item}
          onClose={() => {
            setUploadItem(null);
            setUploadMethodItem(null);
          }}
        />
      )}

      {uploadItem && uploadMethodItem?.method === "web" && (
        <WorkshopWebUploadModal
          item={uploadMethodItem.item}
          onClose={() => {
            setUploadItem(null);
            setUploadMethodItem(null);
          }}
        />
      )}

      {importOpen && (
        <LibraryImportModal
          onClose={() => setImportOpen(false)}
          onImported={(r) => reportImport(r)}
        />
      )}

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

      {/* 批量选择浮动条 + 「加入切换列表」上拉菜单（锚定菜单，不是弹框） */}
      {selectMode && picked.size > 0 && (
        <div className="pointer-events-none fixed inset-x-0 bottom-6 z-[65] flex justify-center">
          <div className="pointer-events-auto flex animate-modal-pop items-center gap-2 rounded-2xl border border-[var(--separator)] bg-[var(--card)]/95 px-4 py-2 shadow-xl backdrop-blur">
            <span className="text-[13px] font-medium">
              {tr("已选 {n} 张", { n: picked.size })}
            </span>
            <span className="mx-1 h-4 w-px bg-[var(--separator)]" />
            <button
              className="btn btn-primary !py-1 text-[12px]"
              onClick={(e) => {
                setNewName("");
                const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
                setAddMenuAt({ x: r.left, y: r.top });
              }}
            >
              {tr("加入切换列表")} ⌃
            </button>
            {activeList && (
              <button
                className="btn !py-1 text-[12px]"
                onClick={() => void removeSelectionFromList()}
              >
                {tr("移出「{name}」", { name: activeList.name })}
              </button>
            )}
            <button className="btn !py-1 text-[12px]" onClick={() => setPicked(new Set())}>
              {tr("清除")}
            </button>
          </div>
        </div>
      )}

      {addMenuAt && (
        <AnchoredMenu x={addMenuAt.x} y={addMenuAt.y} drop="up" onClose={() => setAddMenuAt(null)}>
          {playlists.map((p) => (
            <MenuItem key={p.id} onClick={() => void addSelectionTo(p.id)}>
              {p.name}
            </MenuItem>
          ))}
          {playlists.length === 0 && (
            <div className="px-3 py-1.5 text-[12px] text-[var(--text-2)]">
              {tr("还没有切换列表")}
            </div>
          )}
          <MenuInputRow
            value={newName}
            onChange={setNewName}
            onSubmit={() => void createListWithSelection()}
            placeholder={tr("新列表名称")}
            submitText={tr("建")}
          />
        </AnchoredMenu>
      )}

      {/* 独立模式：启用轮播 = 绑定到目标屏（锚定菜单） */}
      {enableAt && (
        <AnchoredMenu x={enableAt.x} y={enableAt.y} drop="down" onClose={() => setEnableAt(null)}>
          {displays.map((d) => (
            <MenuItem
              key={d.id}
              onClick={() => {
                const p = enableAt.p;
                setEnableAt(null);
                void plAct(
                  () => api.displayBindingSet(d.id, p.id),
                  tr("「{name}」开始轮播", { name: d.name }),
                );
              }}
            >
              {d.name}
            </MenuItem>
          ))}
        </AnchoredMenu>
      )}

      {deleteList && (
        <ConfirmModal
          title={tr("删除切换列表")}
          message={tr("确定删除「{name}」？壁纸本身不受影响。", { name: deleteList.name })}
          confirmText={tr("删除")}
          danger
          onCancel={() => setDeleteList(null)}
          onConfirm={() => {
            const target = deleteList;
            setDeleteList(null);
            if (listFilter === target.id) setListFilter(null);
            void plAct(
              () => api.playlistDelete(target.id),
              tr("已删除「{name}」", { name: target.name }),
            );
          }}
        />
      )}

      {applyMenu}
    </div>
  );
}
