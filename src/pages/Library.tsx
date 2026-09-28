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
  IconShare,
  IconSliders,
  IconRemove,
  IconInfo,
} from "../components/icons";
import { AnchoredMenu, MenuItem, MenuInputRow } from "../components/AnchoredMenu";
import { ShareModal } from "../components/ShareModal";
import { WallpaperPropsModal } from "../components/WallpaperPropsModal";
import { WallpaperInfoModal } from "../components/WallpaperInfoModal";
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
import { DisplayDock } from "../components/DisplayDock";
import { useApplyWallpaper } from "../hooks/useApplyWallpaper";
import { cancelApplyTarget, useArmedApplyTarget } from "../lib/apply-target";
import { tr, trMsg } from "../lib/i18n";

/** 筛选条件持久化：窗口会在内存压力下被回收重建（全新 JS 上下文），不落盘的话
    用户调好的标签/排序会静默回到默认值（与工坊页 useWorkshopFilter 同一套
    做法与键约定）。

    **按列表上下文各存一份**：过滤「全部」和每个播放列表各有独立的搜索/排序/
    标签/只看失效，互不影响 —— 之前在切换列表时会 `clearFilters()`，等于把用户
    在 A 列表调好的条件清掉，切回来已经没了。 */
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

/** 列表上下文 → 持久化键。全部 = "all"，某播放列表 = "pl-<id>"。
    id 是数据库自增主键、删除后不会复用，所以直接拿 id 当键是稳的。 */
function filterKeyOf(listId: number | null): string {
  return listId === null ? "all" : `pl-${listId}`;
}

/** 逐字段校验一份筛选：localStorage 的内容可能来自旧版本，形状不能假定。
    旧版三态（"on"/"excluded"）里只有 "on" 迁移为选中；"excluded" 两态化后无对应物，丢弃 */
function normalizeFilter(raw: unknown): PersistedFilter {
  const o = (raw && typeof raw === "object" ? raw : {}) as Partial<PersistedFilter> & {
    tagState?: Record<string, string>;
  };
  const selected: TagSelection = {};
  const rawSel = o.selected && typeof o.selected === "object" ? o.selected : (o.tagState ?? {});
  for (const [k, v] of Object.entries(rawSel)) {
    if (v === true || v === "on") selected[k] = true;
  }
  return {
    search: typeof o.search === "string" ? o.search : FILTER_DEFAULTS.search,
    // 排序值可能来自旧版本已删除的选项，必须仍合法
    sort:
      typeof o.sort === "string" && LIBRARY_SORTS.some((s) => s.value === o.sort)
        ? o.sort
        : FILTER_DEFAULTS.sort,
    selected,
    onlyMissing: o.onlyMissing === true,
  };
}

/** 读出全部列表上下文的筛选表。
    兼容旧格式：早期这里直接存一份 PersistedFilter（带 search/sort 字段），
    那种情况整体当作「全部」的一份，用户原来的习惯不会丢。 */
function readFilterMap(): Record<string, PersistedFilter> {
  const raw = readState<unknown>(FILTER_STATE_KEY, null);
  if (!raw || typeof raw !== "object") return {};
  const obj = raw as Record<string, unknown>;
  // 旧格式判别：顶层直接有筛选字段，而不是「键 → 筛选」的映射
  if ("search" in obj || "sort" in obj || "selected" in obj || "onlyMissing" in obj) {
    return { all: normalizeFilter(obj) };
  }
  const out: Record<string, PersistedFilter> = {};
  for (const [k, v] of Object.entries(obj)) out[k] = normalizeFilter(v);
  return out;
}

/** 读某个列表上下文的筛选；没存过就是默认值（各列表互不影响） */
function readFilterFor(listId: number | null): PersistedFilter {
  return readFilterMap()[filterKeyOf(listId)] ?? { ...FILTER_DEFAULTS, selected: {} };
}

/** 写某个列表上下文的筛选（读-改-写整张表，只覆盖自己那一格） */
function persistFilter(listId: number | null, f: PersistedFilter): void {
  const map = readFilterMap();
  map[filterKeyOf(listId)] = f;
  writeState(FILTER_STATE_KEY, map);
}

/** 丢弃某个列表上下文的筛选（列表被删除时清掉，免得留一份永远读不到的孤儿） */
function forgetFilter(listId: number): void {
  const map = readFilterMap();
  delete map[filterKeyOf(listId)];
  writeState(FILTER_STATE_KEY, map);
}

export function LibraryPage({ onOpenDetail }: { onOpenDetail: (id: string) => void }) {
  const [items, setItems] = useState<LibraryItem[]>([]);
  // 类型筛选已并入标签面板的「类型」组；这里保留常量空值兼容 libraryList 签名
  const type = "" as WallpaperType | "";
  const [loading, setLoading] = useState(true);
  // 筛选：本地库全部走数据库查询，不在前端过滤
  const [filterOpen, setFilterOpen] = useState(false);
  // 初始进入「全部」上下文，用它自己那份筛选（各列表独立持久化，见 readFilterFor）
  const [initialFilter] = useState(() => readFilterFor(null));
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
  // 从本地库移除（保留壁纸文件）的确认目标
  const [removeItem, setRemoveItem] = useState<LibraryItem | null>(null);
  // 壁纸属性面板（作者属性 + 播放设置）
  const [propsItem, setPropsItem] = useState<LibraryItem | null>(null);
  // 壁纸信息面板
  const [infoItem, setInfoItem] = useState<LibraryItem | null>(null);
  // 批量操作确认（删除 = 连文件一起删；移除 = 保留文件）
  const [bulkConfirm, setBulkConfirm] = useState<"delete" | "remove" | null>(null);
  const [bulkBusy, setBulkBusy] = useState(false);
  // 分享弹窗目标（卡片分享按钮 → ShareModal）
  const [shareItem, setShareItem] = useState<LibraryItem | null>(null);
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

  // 当前列表上下文（null = 全部）。声明必须早于筛选持久化 effect —— 它是那个
  // effect 的依赖（各列表各存一份筛选）。
  const [listFilter, setListFilter] = useState<number | null>(null);

  // 任一条件变化就落盘 —— 只写**当前列表上下文**那一格（各列表独立）。
  // 合成一个 effect 而不是在每个 setter 里写，免得漏掉将来新增的入口。
  // 依赖里带 listFilter：切列表后这一跑会把新载入的值写到新键上（幂等）；
  // 被切走的那个列表的值由 switchList 在切换前显式落盘。
  useEffect(() => {
    persistFilter(listFilter, {
      search: search.trim(),
      sort,
      selected,
      onlyMissing,
    });
  }, [search, sort, selected, onlyMissing, listFilter]);

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
  const [selectMode, setSelectMode] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  // 锚定菜单（加入列表 / 独立模式启用选屏）与内联新建
  const [addMenuAt, setAddMenuAt] = useState<{ x: number; y: number } | null>(null);
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

  /** 切到列表上下文：清掉当前上下文的筛选（只影响这一份，其它列表各自留着） */
  const clearFilters = useCallback(() => {
    setSearch("");
    setDebouncedSearch("");
    setSelected({});
    setOnlyMissing(false);
  }, []);

  /**
   * 切换列表上下文（「全部」↔ 某个播放列表）。
   *
   * 各列表的筛选**独立且持久化**：先把当前这份落盘，再读目标列表自己那份填回去。
   * 之前这里是 `clearFilters()` 一刀切 —— 用户在 A 列表调好的条件会被清掉，
   * 切回来已经没了，也就是「筛选条件相互影响」。
   *
   * 排序也跟着切：它同样属于「这个列表怎么看」的偏好，不是全局设置。
   */
  const switchList = useCallback(
    (next: number | null) => {
      if (next === listFilter) return;
      // 先存当前列表（此刻 state 还是旧列表的值）
      persistFilter(listFilter, { search: search.trim(), sort, selected, onlyMissing });
      // 再载入目标列表自己那份
      const f = readFilterFor(next);
      setSearch(f.search);
      setDebouncedSearch(f.search);
      setSort(f.sort);
      setSelected(f.selected);
      setOnlyMissing(f.onlyMissing);
      setListFilter(next);
    },
    [listFilter, search, sort, selected, onlyMissing],
  );

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
      // 应用/停止后须重取权威的已应用集合（多屏时可同时有多条「已应用」，
      // 停掉一屏也会让条目退出集合），否则旧状态残留（前端只 add 不删除旧 id）。
      if (r !== "cancelled") await loadApplied();
    } catch (e) {
      msg.error(String(e));
    }
  };

  /** 单条：把壁纸从本地库移除（**保留磁盘文件**，与「删除」相对） */
  const removeOne = async (item: LibraryItem) => {
    setRemoveItem(null);
    try {
      await api.libraryRemove(item.itemId);
      msg.success(tr("已移除「{title}」（壁纸文件保留）", { title: item.title }));
    } catch (e) {
      msg.error(String(e));
    }
    refresh();
    loadApplied();
  };

  /**
   * 批量删除 / 批量移除选中条目。
   *
   * 后端命令是**以条目为单位**的（每条要走一次停屏 + 清库记录，还要处理引用模式
   * 不删文件的分支），所以这里串行逐条调用而不是并发：条目数上限也就是几百，
   * 串行还能保证失败时前面的结果已经落库，并且能逐条收集错误原因。
   * 失败不中断整批 —— 一条删不掉（比如文件被占用）不该阻止其余条目。
   */
  const runBulk = async (mode: "delete" | "remove") => {
    const ids = [...picked];
    setBulkConfirm(null);
    if (!ids.length) return;
    setBulkBusy(true);
    const titles = new Map(items.map((it) => [it.itemId, it.title]));
    let ok = 0;
    let firstErr = "";
    for (const id of ids) {
      try {
        if (mode === "delete") await api.libraryDelete(id);
        else await api.libraryRemove(id);
        ok += 1;
      } catch (e) {
        if (!firstErr) firstErr = `${titles.get(id) ?? id} — ${trMsg(String(e))}`;
      }
    }
    setBulkBusy(false);
    setPicked(new Set());
    const failed = ids.length - ok;
    if (ok) {
      msg.success(
        mode === "delete"
          ? tr("已删除 {n} 张壁纸", { n: ok })
          : tr("已移除 {n} 张（壁纸文件保留）", { n: ok }),
      );
    }
    if (failed) msg.error(tr("{n} 张失败：{err}", { n: failed, err: firstErr }));
    refresh();
    loadApplied();
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
      {/* 显示器坞「更换壁纸」锁定的目标屏提示（应用后或点取消自动解除） */}
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
              ? "border-[var(--accent-strong)] bg-[var(--accent-fill)] text-[var(--text-1)]"
              : "border-[var(--separator)] hover:bg-[var(--glass-hover)]"
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
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-[var(--glass-hover)] disabled:opacity-60"
            onClick={() => setSubsOpen(true)}
            title={tr("拉取登录账号的全部订阅，一键下载缺失壁纸（也可多选下载）")}
          >
            ⇓ {tr("同步订阅")}
          </button>
          <button
            className="rounded-lg border border-[var(--separator)] px-3 py-1.5 text-[12.5px] font-medium hover:bg-[var(--glass-hover)] disabled:opacity-60"
            onClick={() => setImportOpen(true)}
            title={tr("添加壁纸路径 / 导入单个壁纸文件夹 / 导入单个文件")}
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
          onClick={() => switchList(null)}
          className={`rounded-full border px-2.5 py-0.5 text-[12px] transition-colors ${
            listFilter === null
              ? "border-[var(--accent-strong)] bg-[var(--accent-fill)] text-[var(--text-1)]"
              : "border-[var(--separator)] text-[var(--text-2)] hover:border-[var(--accent-strong)]/50"
          }`}
        >
          {tr("全部")}
        </button>
        {playlists.map((p) => (
          <button
            key={p.id}
            onClick={() => {
              // 再点当前选中项 = 回到「全部」；两者各自恢复自己的筛选
              switchList(listFilter === p.id ? null : p.id);
            }}
            className={`rounded-full border px-2.5 py-0.5 text-[12px] transition-colors ${
              listFilter === p.id
                ? "border-[var(--accent-strong)] bg-[var(--accent-fill)] text-[var(--text-1)]"
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

        {/* 轮播运行指示 + 快捷控制（唯一的启动/暂停入口：启用轮播按钮已移除） */}
        {(plStatus?.active || boundNames.size > 0 || activeList) && (() => {
          // 有没有可切换的上下文（暂停不算 —— 暂停的只是定时自动切换，上一张/下一张
          // 照旧能点）；手动设过单张壁纸后后端会清掉上下文，这里跟着变回「未在轮播」
          const running = Boolean(plStatus?.switchable ?? (plStatus?.active || boundNames.size > 0));
          return (
          <span className="ml-auto flex items-center gap-2 rounded-full bg-[var(--accent)]/10 px-3 py-0.5 text-[12px] text-[var(--text-2)]">
            <span className="text-[var(--accent-strong)]">▶</span>
            <span className="font-medium text-[var(--text-1)]">
              {!running && activeList
                ? tr("「{name}」未在轮播", { name: activeList.name })
                : plStatus?.active && plStatus.mode !== "independent"
                  ? tr("轮播：{name} {i}/{t}", {
                      name: plStatus.name ?? "",
                      i: (plStatus.index ?? 0) + 1,
                      t: plStatus.total ?? 0,
                    })
                  : tr("{n} 块屏在轮播", { n: boundNames.size })}
            </span>
            {running &&
              plStatus?.active &&
              plStatus.mode !== "independent" &&
              (plStatus.paused ? (
                <span>{tr("已暂停")}</span>
              ) : plStatus.nextAtMs ? (
                <span>{formatCountdown(plStatus.nextAtMs - rotNow)}</span>
              ) : null)}
            <span className="mx-0.5 h-3 w-px bg-[var(--separator)]" />
            {running && (
              <>
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
              </>
            )}
            {running ? (
              <>
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
              </>
            ) : dispMode === "independent" ? (
              <span className="opacity-80">
                {tr("在底部显示器坞里，为某块屏选择该列表开始轮播")}
              </span>
            ) : (
              <button
                title={tr("启用轮播")}
                className="text-[var(--accent-strong)]"
                onClick={() =>
                  void plAct(() => api.playlistApply(activeList!.id), tr("已启用轮播"))
                }
              >
                ▶
              </button>
            )}
          </span>
          );
        })()}
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
                ? "border-[var(--accent-strong)] bg-[var(--accent-fill)] text-[var(--text-1)]"
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
            {/* 启用/暂停统一收到上方轮播条（功能与它重合，按钮移除）；删除保留 */}
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
        <label className="mb-2.5 flex cursor-pointer items-center gap-2 rounded-lg px-1 py-1 text-[12px] hover:bg-[var(--glass-hover)]">
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
                  title={t.hint ? tr(t.hint) : t.name}
                />
              ))}
            </FilterSection>
          );
        })}
      </FilterDrawer>

      <div className="flex min-h-0 flex-1 flex-col">
        {missingItems.length > 0 && (
          <div className="mb-3 flex shrink-0 items-center gap-3 rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-2.5">
            <span className="text-[12.5px] text-amber-300">
              {tr("有 {n} 个壁纸的本地文件已丢失（可能被手动删除），仅剩数据库记录。", {
                n: missingItems.length,
              })}
            </span>
            <button
              className="ml-auto shrink-0 rounded-lg border border-amber-500/40 px-3 py-1 text-[12px] font-medium text-amber-300 hover:bg-amber-500/15 disabled:opacity-60"
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
                /* 8 个按钮排 2×4。顺序 = 「看/配这张壁纸」在前、「文件/分享/移出库」
                   在后，破坏性最强的删除压轴；两个「移出库」语义靠图标 + tooltip
                   区分（移除＝保留文件，删除＝连文件一起删） */
                <div className="grid grid-cols-4 gap-1">
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-[var(--glass-hover)]"
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
                      className="flex items-center justify-center rounded-lg border border-green-500/30 px-0.5 py-1 !text-green-400 bg-green-500/10 hover:opacity-80"
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
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-[var(--glass-hover)]"
                    onClick={() => setPropsItem(item)}
                    data-tip={tr("壁纸属性（作者属性与播放设置）")}
                  >
                    <IconSliders />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-[var(--glass-hover)]"
                    onClick={() => setInfoItem(item)}
                    data-tip={tr("壁纸信息")}
                  >
                    <IconInfo />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-[var(--glass-hover)]"
                    onClick={() => api.libraryOpenFolder(item.itemId)}
                    data-tip={tr("打开文件所在位置")}
                  >
                    <IconOpenFile />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-[var(--accent-strong)] hover:bg-[var(--glass-hover)]"
                    onClick={() => setShareItem(item)}
                    data-tip={tr("分享")}
                  >
                    <IconShare />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-amber-400 hover:border-amber-500/40 hover:bg-amber-500/10"
                    onClick={() => setRemoveItem(item)}
                    data-tip={tr("移出本地库（保留壁纸文件）")}
                  >
                    <IconRemove />
                  </button>
                  <button
                    className="flex items-center justify-center rounded-lg border border-[var(--separator)] px-0.5 py-1 text-[var(--text-2)] hover:text-red-500 hover:border-red-500/40 hover:bg-red-500/10"
                    onClick={() => setDeleteItem(item)}
                    data-tip={tr("删除（同时删除壁纸文件）")}
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

      {shareItem && (
        <ShareModal
          itemId={shareItem.itemId}
          title={shareItem.title}
          wtype={shareItem.type}
          onClose={() => setShareItem(null)}
        />
      )}

      {/* 壁纸属性（作者属性 + 每壁纸播放设置）：与详情页共用同一个面板 */}
      {propsItem && (
        <WallpaperPropsModal
          itemId={propsItem.itemId}
          title={propsItem.title}
          onClose={() => setPropsItem(null)}
        />
      )}

      {infoItem && (
        <WallpaperInfoModal
          item={infoItem}
          applied={appliedItems.has(infoItem.itemId)}
          playlists={playlists.filter((p) => p.itemIds.includes(infoItem.itemId)).map((p) => p.name)}
          onClose={() => setInfoItem(null)}
        />
      )}

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

      {removeItem && (
        <ConfirmModal
          title={tr("移除壁纸")}
          message={tr(
            "确定把「{title}」从本地库移除？磁盘上的壁纸文件会保留，但该壁纸的自定义属性、所属切换列表与下载记录会被清除。此操作不可恢复。",
            { title: removeItem.title },
          )}
          confirmText={tr("移除")}
          danger
          onCancel={() => setRemoveItem(null)}
          onConfirm={() => void removeOne(removeItem)}
        />
      )}

      {bulkConfirm === "delete" && (
        <ConfirmModal
          title={tr("批量删除壁纸")}
          message={tr(
            "确定删除选中的 {n} 张壁纸及其本地文件？此操作不可恢复。",
            { n: picked.size },
          )}
          confirmText={tr("批量删除")}
          danger
          onCancel={() => setBulkConfirm(null)}
          onConfirm={() => void runBulk("delete")}
        />
      )}

      {bulkConfirm === "remove" && (
        <ConfirmModal
          title={tr("批量移除壁纸")}
          message={tr(
            "确定把选中的 {n} 张壁纸从本地库移除？磁盘上的壁纸文件会保留，但它们的自定义属性、所属切换列表与下载记录会被清除。此操作不可恢复。",
            { n: picked.size },
          )}
          confirmText={tr("批量移除")}
          danger
          onCancel={() => setBulkConfirm(null)}
          onConfirm={() => void runBulk("remove")}
        />
      )}

      {/* 批量选择浮动条 + 「加入切换列表」上拉菜单（锚定菜单，不是弹框） */}
      {selectMode && picked.size > 0 && (
        <div className="pointer-events-none fixed inset-x-0 bottom-6 z-[65] flex justify-center">
          {/* max-w + flex-wrap：批量条上的按钮比原先多了两个，列表名又可能很长，
              不封顶时窄窗口里会被顶出可视区（贴边即换行，不会溢出） */}
          <div className="pointer-events-auto flex max-w-[calc(100vw-2.5rem)] animate-modal-pop flex-wrap items-center justify-center gap-2 rounded-2xl border border-[var(--separator)] bg-[var(--card)]/95 px-4 py-2 shadow-xl backdrop-blur">
            <span className="text-[13px] font-medium">
              {tr("已选 {n} 张", { n: picked.size })}
            </span>
            <span className="mx-1 h-4 w-px bg-[var(--separator)]" />
            <button
              className="btn btn-primary !py-1 text-[12px] disabled:opacity-60"
              disabled={bulkBusy}
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
                className="btn !py-1 text-[12px] disabled:opacity-60"
                disabled={bulkBusy}
                onClick={() => void removeSelectionFromList()}
              >
                {tr("移出「{name}」", { name: activeList.name })}
              </button>
            )}
            <span className="mx-1 h-4 w-px bg-[var(--separator)]" />
            {/* 库级批量操作：移除保留文件、删除连文件一起删，两者都要二次确认 */}
            <button
              className="btn !py-1 text-[12px] !border-amber-500/40 !text-amber-400 hover:!bg-amber-500/10 disabled:opacity-60"
              disabled={bulkBusy}
              title={tr("从本地库移除选中的壁纸，不删除磁盘文件")}
              onClick={() => setBulkConfirm("remove")}
            >
              {bulkBusy ? tr("处理中…") : tr("批量移除")}
            </button>
            <button
              className="btn btn-danger !py-1 text-[12px] disabled:opacity-60"
              disabled={bulkBusy}
              title={tr("从本地库删除选中的壁纸，并删除磁盘文件")}
              onClick={() => setBulkConfirm("delete")}
            >
              {tr("批量删除")}
            </button>
            <button
              className="btn !py-1 text-[12px] disabled:opacity-60"
              disabled={bulkBusy}
              onClick={() => setPicked(new Set())}
            >
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
            // 先离开它的上下文（switchList 会把当前筛选存进 target 那一格），
            // 删除成功后再把那格丢掉
            if (listFilter === target.id) switchList(null);
            void plAct(
              () =>
                api
                  .playlistDelete(target.id)
                  .then(() => forgetFilter(target.id))
                  .catch((e) => {
                    // 删除失败就保留那份筛选：列表还在，用户切回去时偏好不该丢
                    throw e;
                  }),
              tr("已删除「{name}」", { name: target.name }),
            );
          }}
        />
      )}

      {applyMenu}

      {/* 底部显示器坞：鼠标扫到屏幕底边滑出。它已承接原「显示器」页的日常操作
          （选屏应用 / 统一独立模式 / 每屏轮播绑定），所以那一页已移除 */}
      <DisplayDock
        displays={displays}
        mode={dispMode}
        playlists={playlists}
        onReload={() => void loadPlaylists()}
      />
    </div>
  );
}
