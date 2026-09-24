// 虚拟滚动的方形卡片网格（本地库 / 订阅同步共用）
//
// 为什么需要它：这两个列表的条目数没有上限 —— 本地库实测能到几百项，订阅同步
// 更是「拉到你账号的全部订阅」（几千项）。整表渲染时每个条目都挂着一张
// cdn.steamstatic.com 的预览图 + 悬浮抽屉 DOM，几千个节点会让滚动明显掉帧、
// 内存也跟着涨。这里只渲染视口内的那几行（上下各多留 overscan 行），DOM 数量
// 与列表长度脱钩，只与窗口高度有关。
//
// 布局口径与原来的 CSS 网格保持一致：`repeat(auto-fill, minmax(minW, 1fr))`，
// 即列数 = 能塞下几个 minW，列宽 = 均分剩余空间；卡片是 1:1 方形（与原实现同）。
//
// 稳定性教训（第一版滚动会「一片白 + 整页卡住」，下面全是踩过的坑）：
//   ① 滚动偏移不能直接进 state：scroll 事件比帧还密，逐事件 setState 会重渲染整片
//      卡片（本地库 306 项 × 悬浮抽屉 + GIF 预览），主线程追不上事件队列。现在滚动
//      只在 rAF 里合并处理，且「窗口没变就原样返回旧 state」，React 直接跳过渲染。
//   ② 每跨一行不能重渲染全部格子：缓存每个格子的 React 元素，只重建新进窗口的那几格
//      —— 窗口内其它格子的元素引用不变时 React 会整棵子树跳过（同引用 bailout）。
//      否则一次跨行要把几十个卡片全渲染一遍，快速滑动时每帧几十毫秒，主线程必然
//      跟不上合成器的滚动位置（表现就是前缘白带 + 整页卡死）。
//   ③ 尺寸测量只接受有效值（> 0 且有限）：一旦把布局未就位时的 0 写进 state，列宽
//      就是 0，整片网格空白且不会自己恢复。
//   ④ 列表变短后必须自己把 scrollTop 夹回滚动范围：不能指望浏览器「自动夹回 + 派发
//      scroll 事件」—— 那一步不保证发生，一旦没发生，行窗口会整块落在列表之外 →
//      全白，而且内容不足一屏连滚动事件都发不出来（就是「卡住」的感觉）。
//   ⑤ scroll 事件可能被合并甚至丢掉（动量滚动 / 滚动被合成器接管时），只在事件里
//      读一次 scrollTop 会让行窗口停在旧位置。这里用 wheel + 「偏移还在变就继续跟」
//      的 rAF 轮询兜底，停手后再收尾校准一次。
//   ⑥ 朝滑动方向多渲染几行（前瞻）：合成器上的滚动位置可能领先主线程读到的
//      scrollTop，只按当前偏移算窗口，前缘就会露出没渲染的白带。前瞻行数按本帧位移
//      估算，停手后收回。
//   ⑦ 再加一道低频看门狗：合成器忙的时候 rAF 会被饿死（滚动照常、DOM 却不动），
//      定时器不受影响；看门狗每 400ms 校验一次「屏幕上那批格子还盖得住当前偏移吗」，
//      盖不住就补一次校准，同时把现场写进应用日志（临时诊断，定位完可去掉）。
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";

/** 视口尺寸；只有量到有效值才会写进 state */
type Box = { width: number; height: number };
/** 需要渲染的行窗口（闭区间） */
type Range = { first: number; last: number };
/** 供滚动回调使用的几何量 */
type Metrics = { rowHeight: number; rowCount: number; height: number };

const EMPTY_RANGE: Range = { first: 0, last: -1 };
const sameRange = (a: Range, b: Range) => a.first === b.first && a.last === b.last;
/** 布局未就位时 clientWidth/clientHeight 可能是 0 甚至 NaN，一律当作「没量到」 */
const usable = (n: number) => Number.isFinite(n) && n > 0;
/** 单次滑动最多前瞻几行（再多就是白挂图片了） */
const MAX_LEAD_ROWS = 3;

export function VirtualGrid<T>({
  items,
  minColumnWidth,
  gap = 12,
  overscan = 2,
  className,
  renderItem,
  keyOf,
  onNearEnd,
  nearEndThreshold = 400,
  footer,
  initialScrollTop,
  onScrollTop,
}: {
  items: T[];
  /** 单列最小宽度（对应原 minmax(minW, 1fr)） */
  minColumnWidth: number;
  gap?: number;
  /** 视口外上下各预渲染的行数：滚动时留出图片加载的提前量，太大则失去意义 */
  overscan?: number;
  /** 滚动容器类名（需自带 overflow-y-auto / flex-1 之类的尺寸约束） */
  className?: string;
  renderItem: (item: T, index: number) => ReactNode;
  /** 列表项的稳定 key（默认用下标）；有筛选/排序时务必传 id，否则复用错位 */
  keyOf?: (item: T, index: number) => string | number;
  /** 滚动接近底部时回调（订阅同步的翻页哨兵） */
  onNearEnd?: () => void;
  nearEndThreshold?: number;
  /** 网格之后的固定内容（如「正在加载更多…」），仍在同一个滚动容器内 */
  footer?: ReactNode;
  /** 挂载后恢复到的滚动位置（会话快照还原用）；只在首次布局生效一次 */
  initialScrollTop?: number;
  /** 滚动偏移上报（rAF 节流；调用方应写 ref 而非 state，避免逐帧重渲染） */
  onScrollTop?: (top: number) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef<HTMLDivElement>(null);
  const [box, setBox] = useState<Box>({ width: 0, height: 0 });
  const [range, setRange] = useState<Range>(EMPTY_RANGE);
  // 回调与几何都走 ref：调用方多半传内联箭头函数（每次渲染都是新引用），直接进依赖
  // 数组会让滚动监听器每帧解绑重绑；几何进 ref 则让滚动监听器只挂一次
  const nearEndRef = useRef(onNearEnd);
  nearEndRef.current = onNearEnd;
  const thresholdRef = useRef(nearEndThreshold);
  thresholdRef.current = nearEndThreshold;
  const metricsRef = useRef<Metrics>({ rowHeight: 0, rowCount: 0, height: 0 });
  const scrollTopRef = useRef(0);
  /** 当前「已经画在屏幕上」的窗口（看门狗要拿它和真实偏移对账） */
  const rangeRef = useRef<Range>(EMPTY_RANGE);
  /** 前瞻 / 滞回撑大的窗口：滑块停手前只扩不缩，避免来回抖动反复挂载卸载 */
  const stickyRef = useRef<Range | null>(null);
  const measureRef = useRef<() => void>(() => {});
  /** 快照滚动位置只在首次布局生效一次；onScrollTop 回调走 ref 避免监听器重绑 */
  const initialScrollRef = useRef(initialScrollTop ?? 0);
  const onScrollTopRef = useRef(onScrollTop);
  onScrollTopRef.current = onScrollTop;

  // 列宽由容器宽度推导，容器宽度又取决于窗口大小 —— 用 ResizeObserver 同时盯
  // 滚动容器（高度）与内容区（宽度，已扣掉 padding）
  useLayoutEffect(() => {
    const outer = scrollRef.current;
    const inner = contentRef.current;
    if (!outer || !inner) return;
    let retryRaf = 0;
    let retries = 0;
    const measure = () => {
      retryRaf = 0;
      const width = inner.clientWidth;
      const height = outer.clientHeight;
      if (!usable(width) || !usable(height)) {
        // 布局还没就位：绝不把 0 写进 state（见文件头 ③），下一帧再量
        if (retries++ < 30) retryRaf = requestAnimationFrame(measure);
        return;
      }
      setBox((prev) =>
        prev.width === width && prev.height === height ? prev : { width, height },
      );
    };
    measureRef.current = measure;
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(outer);
    ro.observe(inner);
    return () => {
      ro.disconnect();
      if (retryRaf) cancelAnimationFrame(retryRaf);
    };
  }, []);

  // ResizeObserver 之外再挂一层 window.resize 兜底：万一漏报一次尺寸，网格会一直按
  // 旧列宽排版（右侧多半列空白、卡片也被拉伸）
  useEffect(() => {
    const onResize = () => measureRef.current();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  const columns = usable(box.width)
    ? Math.max(1, Math.floor((box.width + gap) / (minColumnWidth + gap)))
    : 0;
  const cardWidth = columns > 0 ? (box.width - gap * (columns - 1)) / columns : 0;
  const rowHeight = cardWidth > 0 ? cardWidth + gap : 0;
  const rowCount = cardWidth > 0 ? Math.ceil(items.length / columns) : 0;
  const totalHeight = rowCount > 0 ? Math.max(0, rowCount * rowHeight - gap) : 0;
  metricsRef.current = { rowHeight, rowCount, height: box.height };
  rangeRef.current = range;

  /** 由滚动偏移推出要渲染的行窗口（夹在合法行范围内，绝不会 first > last） */
  const resolveRange = useCallback(
    (rh: number, rows: number, height: number, scrollTop: number): Range => {
      if (rh <= 0 || rows <= 0) return EMPTY_RANGE;
      const first = Math.min(Math.max(0, Math.floor(scrollTop / rh) - overscan), rows - 1);
      const last = Math.min(
        rows - 1,
        Math.max(first, Math.ceil((scrollTop + height) / rh) + overscan),
      );
      return { first, last };
    },
    [overscan],
  );

  // 窗口没变时必须原样返回旧对象：React 见到相同引用会跳过这次渲染
  const commit = useCallback((next: Range) => {
    setRange((prev) => (sameRange(prev, next) ? prev : next));
  }, []);

  /** 按当前真实滚动偏移重算窗口（滚动中每帧调用一次，代价很小） */
  const sync = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const m = metricsRef.current;
    const top = el.scrollTop;
    const delta = top - scrollTopRef.current;
    scrollTopRef.current = top;
    if (m.rowHeight <= 0 || m.rowCount <= 0) {
      commit(EMPTY_RANGE);
      return;
    }
    const base = resolveRange(m.rowHeight, m.rowCount, m.height, top);
    // 前瞻（见文件头 ⑥）：按本帧位移折算成行数，朝滑动方向多铺几行
    const lead = Math.min(MAX_LEAD_ROWS, Math.round(Math.abs(delta) / m.rowHeight));
    let first = base.first;
    let last = base.last;
    if (delta > 0) last = Math.min(m.rowCount - 1, last + lead);
    else if (delta < 0) first = Math.max(0, first - lead);
    const sticky = stickyRef.current;
    if (sticky) {
      first = Math.min(first, sticky.first);
      last = Math.max(last, sticky.last);
    }
    const next = { first, last };
    // 只在真的比常规窗口更大时才记为「滞回窗口」，否则停手后收不回来
    stickyRef.current = first < base.first || last > base.last ? next : null;
    commit(next);
    onScrollTopRef.current?.(top);
    if (
      nearEndRef.current &&
      el.scrollHeight - top - el.clientHeight < thresholdRef.current
    ) {
      nearEndRef.current();
    }
  }, [commit, resolveRange]);
  const syncRef = useRef(sync);
  syncRef.current = sync;

  // 几何变化（改尺寸 / 换筛选 / 条目增删）后重新定位窗口，并把越界的 scrollTop 夹回来
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    // 条目变少后旧 scrollTop 会落到滚动范围之外，必须自己夹回（见文件头 ④）
    const max = Math.max(0, el.scrollHeight - el.clientHeight);
    if (el.scrollTop > max) el.scrollTop = max;
    // 快照还原的滚动位置：等首次有效布局（行高/条目就绪）后应用一次。
    // 放在这里而不是 mount effect，是因为 mount 时 box 还是 0、行高未知，
    // 直接设 scrollTop 会被随后的窗口重算覆盖
    if (initialScrollRef.current > 0) {
      el.scrollTop = Math.min(initialScrollRef.current, max);
      initialScrollRef.current = 0;
    }
    scrollTopRef.current = el.scrollTop;
    stickyRef.current = null;
    syncRef.current();
    // items.length 一并进依赖：行数不变但条目换了（等长筛选）时也要重算
  }, [rowHeight, rowCount, box.height, items.length, resolveRange]);

  // 滚动跟踪：scroll/wheel 都只是「叫醒」信号，真正的偏移在 rAF 里读。
  // 偏移只要还在变就一直跟（动量滚动期间 scroll 事件可能被合并/丢掉），
  // 停手后再多跟 250ms 收尾校准，然后收回前瞻窗口。
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    let raf = 0;
    let settleUntil = 0;
    let lastSeen = el.scrollTop;
    const tick = () => {
      raf = 0;
      const top = el.scrollTop;
      if (top !== lastSeen) {
        lastSeen = top;
        settleUntil = performance.now() + 250;
      }
      measureRef.current();
      syncRef.current();
      if (performance.now() < settleUntil) {
        raf = requestAnimationFrame(tick);
      } else if (stickyRef.current) {
        stickyRef.current = null;
        syncRef.current();
      }
    };
    const kick = () => {
      settleUntil = performance.now() + 400;
      if (!raf) raf = requestAnimationFrame(tick);
    };
    el.addEventListener("scroll", kick, { passive: true });
    el.addEventListener("wheel", kick, { passive: true });
    return () => {
      el.removeEventListener("scroll", kick);
      el.removeEventListener("wheel", kick);
      if (raf) cancelAnimationFrame(raf);
    };
  }, []);

  // 内容不足一屏时滚动事件永远不会触发，翻页也就永远不来（订阅只有一页时，
  // 第一页拉回来可能刚好塞不满视口 → 后续页永远不加载）。列表长度或视口尺寸
  // 变化后补一次「是否已在底部附近」检查；是否真的翻页由调用方自己防重。
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || !nearEndRef.current) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < thresholdRef.current) {
      nearEndRef.current();
    }
  }, [items.length, box.width, box.height]);

  // 看门狗（见文件头 ⑦）：rAF 被合成器饿死时，滚动照常在走但 DOM 不更新，前缘会
  // 露出没渲染的白带。定时器不跟着掉帧，用它低频对账一次并自愈。
  useEffect(() => {
    const id = window.setInterval(() => {
      const el = scrollRef.current;
      const m = metricsRef.current;
      if (!el || m.rowHeight <= 0) return;
      const top = el.scrollTop;
      // 屏幕上这批格子的第一行若落在视口顶部之下，前缘就是白带
      const band = rangeRef.current.first * m.rowHeight - top;
      const missed = top !== scrollTopRef.current;
      if (band <= m.rowHeight * 0.5 && !missed) return;
      if (band > m.rowHeight * 0.5) {
        reportGridStall({
          band: Math.round(band),
          top: Math.round(top),
          first: rangeRef.current.first,
          last: rangeRef.current.last,
          rowHeight: Math.round(m.rowHeight),
          rows: m.rowCount,
          missed,
        });
      }
      syncRef.current();
    }, 400);
    return () => window.clearInterval(id);
  }, []);

  // 格子元素缓存（见文件头 ②）。缓存以 renderItem 的引用为界：调用方每次重渲染都会
  // 传入新的闭包，缓存随之失效，不会留下旧数据（「已应用」徽标、选中态这些依赖调用方
  // state 的内容照样会更新）
  const cellsRef = useRef(new Map<string, ReactNode>());
  const renderItemRef = useRef(renderItem);
  if (renderItemRef.current !== renderItem) {
    renderItemRef.current = renderItem;
    cellsRef.current.clear();
  }

  const cells: ReactNode[] = [];
  const usedKeys: string[] = [];
  if (cardWidth > 0 && range.last >= range.first) {
    for (let row = range.first; row <= range.last; row++) {
      const y = Math.round(row * rowHeight);
      for (let col = 0; col < columns; col++) {
        const index = row * columns + col;
        if (index >= items.length) break;
        const item = items[index];
        // key 里带上 index：排序变化时即使 item id 不变也会重建元素，不会残留旧下标
        const cacheKey = `${keyOf ? keyOf(item, index) : index}#${index}`;
        usedKeys.push(cacheKey);
        let cell = cellsRef.current.get(cacheKey);
        if (cell === undefined) {
          cell = renderItem(item, index);
          cellsRef.current.set(cacheKey, cell);
        }
        cells.push(
          <div
            key={cacheKey}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: cardWidth,
              height: cardWidth,
              // 格子尺寸固定，声明布局隔离：内部内容变化（悬停抽屉展开等）不会
              // 让整个网格跟着重新布局
              contain: "layout",
              // 用 transform 定位而非 top/left：滚动时只是位置变化，不必为每个
              // 格子重跑绝对定位布局；取整避免 1px 边框被半像素糊掉
              transform: `translate(${Math.round(col * (cardWidth + gap))}px, ${y}px)`,
            }}
          >
            {cell}
          </div>,
        );
      }
    }
  }
  // 缓存只留当前窗口内的格子，不随列表长度无限增长
  if (cellsRef.current.size > usedKeys.length) {
    const keep = new Set(usedKeys);
    for (const k of cellsRef.current.keys()) {
      if (!keep.has(k)) cellsRef.current.delete(k);
    }
  }

  return (
    <div ref={scrollRef} className={className}>
      <div ref={contentRef}>
        <div style={{ position: "relative", height: totalHeight }}>{cells}</div>
        {footer}
      </div>
    </div>
  );
}

/**
 * 临时诊断：把「前缘白带 / 漏滚动事件」的现场转发到内容服务器的 /diag，由 Rust 侧
 * 记进应用日志（主窗口与内容服务器跨源，响应会被 CORS 拦掉，但请求发得出去、日志
 * 记得到）。定位完成后整段可删。
 */
let diagBase: string | null = null;
let diagAt = 0;
let diagLoading = false;
function reportGridStall(info: Record<string, unknown>) {
  const now = Date.now();
  if (now - diagAt < 2000) return;
  diagAt = now;
  const send = (base: string) => {
    const msg = `[grid] 前缘白带 ${JSON.stringify(info)}`;
    void fetch(`${base}/diag?msg=${encodeURIComponent(msg)}`, { cache: "no-store" }).catch(
      () => {},
    );
  };
  if (diagBase) {
    send(diagBase);
    return;
  }
  if (diagLoading) return;
  diagLoading = true;
  void import("../api/steam")
    .then(({ api }) => api.contentServerStatus())
    .then((s) => {
      diagBase = s.base;
      send(diagBase);
    })
    .catch(() => {})
    .finally(() => {
      diagLoading = false;
    });
}
