// 工坊筛选条件的全局共享状态
//
// 工坊页与发现页共用同一套条件：用户在工坊里排除了成人内容，
// 发现页的随机推荐也不该再推给他。放在 Context 里而不是各页自己 useState，
// 就是为了这个一致性。
//
// 只共享「内容筛选」（标签/排序/时间范围），不共享搜索词与分页 ——
// 那两个是页面局部的浏览位置，跨页带过去只会造成困惑。
//
// 条件落 localStorage：窗口被释放后重建是全新 JS 上下文，不持久化的话用户
// 精心调好的一串标签会静默回到默认值，而且他很可能没注意到，只觉得
// 「结果怎么变了」。
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { DEFAULT_TAG_STATE } from "../lib/tags";
import { readState, writeState } from "../lib/cache-snapshots";

/** 一个标签的三态：未选 / 必需 / 排除 */
export type TagState = Record<string, "on" | "excluded">;

type Ctx = {
  tagState: TagState;
  /** 点击循环：未选 → 必需 → 排除 → 未选 */
  cycleTag: (name: string) => void;
  sort: string;
  setSort: (v: string) => void;
  /** 趋势时间范围（天）；-1 = 全部时间 */
  days: number;
  setDays: (v: number) => void;
  reset: () => void;
  /** 派生：必需标签 */
  tags: string[];
  /** 派生：排除标签 */
  excludedTags: string[];
  /** 派生：生效的条件数（用于面板角标） */
  activeCount: number;
};

const FilterCtx = createContext<Ctx | null>(null);

/** 持久化载体：三个字段一起存，避免读到「标签是新的、排序是旧的」这种半截状态 */
const FILTER_STATE_KEY = "filter.workshop";

type Persisted = { tagState: TagState; sort: string; days: number };

const FILTER_DEFAULTS: Persisted = {
  tagState: { ...DEFAULT_TAG_STATE },
  sort: "trend",
  days: -1,
};

function readPersisted(): Persisted {
  const raw = readState<Partial<Persisted> | null>(FILTER_STATE_KEY, null);
  if (!raw || typeof raw !== "object") return { ...FILTER_DEFAULTS, tagState: { ...DEFAULT_TAG_STATE } };
  // 逐字段校验：localStorage 的内容可能来自旧版本，形状不能假定
  const tagState: TagState = {};
  if (raw.tagState && typeof raw.tagState === "object") {
    for (const [k, v] of Object.entries(raw.tagState)) {
      if (v === "on" || v === "excluded") tagState[k] = v;
    }
  }
  return {
    tagState,
    sort: typeof raw.sort === "string" ? raw.sort : FILTER_DEFAULTS.sort,
    days: typeof raw.days === "number" ? raw.days : FILTER_DEFAULTS.days,
  };
}

export function useWorkshopFilter(): Ctx {
  const v = useContext(FilterCtx);
  if (!v) throw new Error("useWorkshopFilter 必须在 <WorkshopFilterProvider> 内使用");
  return v;
}

export function WorkshopFilterProvider({ children }: { children: ReactNode }) {
  const initial = readPersisted();
  const [tagState, setTagState] = useState<TagState>(initial.tagState);
  const [sort, setSort] = useState(initial.sort);
  const [days, setDays] = useState(initial.days);

  // 任一条件变化就落盘。合成一个 effect 而不是在每个 setter 里写，
  // 免得漏掉将来新增的入口（比如 reset）
  useEffect(() => {
    writeState<Persisted>(FILTER_STATE_KEY, { tagState, sort, days });
  }, [tagState, sort, days]);

  const cycleTag = useCallback((name: string) => {
    setTagState((prev) => {
      const next = { ...prev };
      if (!next[name]) next[name] = "on";
      else if (next[name] === "on") next[name] = "excluded";
      else delete next[name];
      return next;
    });
  }, []);

  const reset = useCallback(() => {
    setTagState({ ...DEFAULT_TAG_STATE });
    setDays(-1);
  }, []);

  const tags = useMemo(
    () => Object.entries(tagState).filter(([, v]) => v === "on").map(([k]) => k),
    [tagState],
  );
  const excludedTags = useMemo(
    () => Object.entries(tagState).filter(([, v]) => v === "excluded").map(([k]) => k),
    [tagState],
  );

  const value = useMemo<Ctx>(
    () => ({
      tagState,
      cycleTag,
      sort,
      setSort,
      days,
      setDays,
      reset,
      tags,
      excludedTags,
      activeCount: tags.length + excludedTags.length + (days > 0 ? 1 : 0),
    }),
    [tagState, cycleTag, sort, days, reset, tags, excludedTags],
  );

  return <FilterCtx.Provider value={value}>{children}</FilterCtx.Provider>;
}
