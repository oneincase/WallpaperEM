// 工坊筛选条件的全局共享状态
//
// 工坊页与发现页共用同一套条件（见文件底部的持久化说明）。
// 只共享「内容筛选」（标签/排序/时间范围），不共享搜索词与分页 ——
// 那两个是页面局部的浏览位置，跨页带过去只会造成困惑。
//
// 标签语义：两态（选中/未选中）；组内多选为并集、组间为交集、全不选为不约束。
// 所有分组都可以多选（类型/分级/分类/分辨率也不例外），见 toggleTagSelection。
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import {
  DEFAULT_TAG_SELECTION,
  toggleTagSelection,
  selectedTagGroups,
  type TagSelection,
} from "../lib/tags";
import { readState, writeState } from "../lib/cache-snapshots";
import { tr } from "../lib/i18n";

type Ctx = {
  /** 已选中的标签（值为 true） */
  selected: TagSelection;
  /** 切换选中态（两态；任何分组都可自由叠加） */
  toggleTag: (name: string) => void;
  sort: string;
  setSort: (v: string) => void;
  /** 趋势时间范围（天）；-1 = 全部时间 */
  days: number;
  setDays: (v: number) => void;
  reset: () => void;
  /** 派生：分组标签（组内 OR、组间 AND，空组剔除） */
  tagGroups: string[][];
  /** 派生：生效的条件数（用于面板角标） */
  activeCount: number;
};

const FilterCtx = createContext<Ctx | null>(null);

/** 持久化载体：三个字段一起存，避免读到「标签是新的、排序是旧的」这种半截状态 */
const FILTER_STATE_KEY = "filter.workshop";

type Persisted = { selected: TagSelection; sort: string; days: number };

const FILTER_DEFAULTS: Persisted = {
  selected: { ...DEFAULT_TAG_SELECTION },
  sort: "trend",
  days: -1,
};

function readPersisted(): Persisted {
  const raw = readState<Partial<Persisted> | null>(FILTER_STATE_KEY, null);
  if (!raw || typeof raw !== "object")
    return { ...FILTER_DEFAULTS, selected: { ...DEFAULT_TAG_SELECTION } };
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
    selected,
    sort: typeof raw.sort === "string" ? raw.sort : FILTER_DEFAULTS.sort,
    days: typeof raw.days === "number" ? raw.days : FILTER_DEFAULTS.days,
  };
}

export function useWorkshopFilter(): Ctx {
  const v = useContext(FilterCtx);
  if (!v) throw new Error(tr("useWorkshopFilter 必须在 <WorkshopFilterProvider> 内使用"));
  return v;
}

export function WorkshopFilterProvider({ children }: { children: ReactNode }) {
  const initial = readPersisted();
  const [selected, setSelected] = useState<TagSelection>(initial.selected);
  const [sort, setSort] = useState(initial.sort);
  const [days, setDays] = useState(initial.days);

  // 任一条件变化就落盘。合成一个 effect 而不是在每个 setter 里写，
  // 免得漏掉将来新增的入口（比如 reset）
  useEffect(() => {
    writeState<Persisted>(FILTER_STATE_KEY, { selected, sort, days });
  }, [selected, sort, days]);

  const toggleTag = useCallback((name: string) => {
    setSelected((prev) => toggleTagSelection(prev, name));
  }, []);

  const reset = useCallback(() => {
    setSelected({ ...DEFAULT_TAG_SELECTION });
    setDays(-1);
  }, []);

  const tagGroups = useMemo(() => selectedTagGroups(selected), [selected]);

  const value = useMemo<Ctx>(
    () => ({
      selected,
      toggleTag,
      sort,
      setSort,
      days,
      setDays,
      reset,
      tagGroups,
      activeCount: Object.keys(selected).length + (days > 0 ? 1 : 0),
    }),
    [selected, toggleTag, sort, days, reset, tagGroups],
  );

  return <FilterCtx.Provider value={value}>{children}</FilterCtx.Provider>;
}
