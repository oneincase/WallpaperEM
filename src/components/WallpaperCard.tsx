// 壁纸卡片（工坊 / 本地库 / 收藏共用）：
// 预览图为 1:1，object-cover 充满整卡；详情/操作容器是「向上抽屉」——
// 悬浮在卡片上时从底边滑出覆盖在图片上方，鼠标移出自动滑回隐藏。
import type { ReactNode, SyntheticEvent } from "react";
import { formatBytes, formatCount } from "../lib/format";
import { tr } from "../lib/i18n";

export function WallpaperCard({
  imageUrl,
  title,
  onOpen,
  badges,
  coverBadgeRight,
  metaLeft,
  metaRight,
  actions,
  alt,
  eager = false,
}: {
  /** 预览图 URL（1:1 裁切充满整卡）；空则显示占位 */
  imageUrl?: string;
  title: string;
  /** 点击卡片主体（图片区）打开详情 */
  onOpen: () => void;
  /** 图片左上角常驻状态徽标（已应用/已下载等） */
  badges?: ReactNode;
  /** 图片右上角常驻徽标（下载量等），与左下角操作无关、始终可见 */
  coverBadgeRight?: ReactNode;
  /** 抽屉内标题下方的左侧元信息（类型徽标等） */
  metaLeft?: ReactNode;
  /** 抽屉内标题下方的右侧元信息（订阅数/大小等） */
  metaRight?: ReactNode;
  /** 抽屉底部操作按钮行（本地库的五连按钮等）；无则不渲染该行 */
  actions?: ReactNode;
  alt?: string;
  /**
   * 立即加载预览图（虚拟滚动列表专用）。
   * lazy 的「是否进入视口」判定依赖浏览器自己的时机，而虚拟列表里的格子是随滚动
   * 挂载/卸载的：判定一旦被跳过，格子会一直停在空白状态（就是「滚过去一片白」）。
   * 这类列表本来就只挂视口内十几个格子，全部立即加载反而更稳。
   */
  eager?: boolean;
}) {
  // 抽屉内的点击/键盘操作不应冒泡到卡片主体（避免误触打开详情）
  const stop = (e: SyntheticEvent) => e.stopPropagation();

  return (
    <div
      className="card group relative aspect-square overflow-hidden transition-transform duration-200 hover:-translate-y-0.5"
    >
      {/* 1:1 预览图：充满整卡，点击打开详情；悬停轻微放大 */}
      <button
        onClick={onOpen}
        className="absolute inset-0 block h-full w-full cursor-default focus:outline-none"
        aria-label={title}
      >
        {imageUrl ? (
          <img
            src={imageUrl}
            alt={alt ?? title}
            loading={eager ? "eager" : "lazy"}
            decoding="async"
            className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.04]"
            draggable={false}
          />
        ) : (
          <span className="flex h-full w-full items-center justify-center text-[13px] text-[var(--text-2)]">
            {tr("无预览")}
          </span>
        )}
      </button>

      {/* 常驻状态徽标 */}
      {badges && <div className="absolute left-1.5 top-1.5 z-10 flex flex-col gap-1">{badges}</div>}

      {/* 右上角常驻徽标（下载量等） */}
      {coverBadgeRight && (
        <div className="absolute right-1.5 top-1.5 z-10">{coverBadgeRight}</div>
      )}

      {/* 向上抽屉：默认沉在底边外，悬停/键盘聚焦时滑出覆盖在图上 */}
      <div
        className="absolute inset-x-0 bottom-0 z-20 translate-y-full opacity-0 transition-all duration-300 ease-out group-focus-within:translate-y-0 group-focus-within:opacity-100 group-hover:translate-y-0 group-hover:opacity-100"
        onClick={stop}
        onKeyDown={stop}
      >
        <div className="border-t border-[var(--separator)] bg-[var(--card)]/95 p-2.5 backdrop-blur-md">
          <button
            onClick={onOpen}
            // card-title-btn：标题块的**外层**两行硬封顶（普通块级盒，max-height
            // 稳定生效）。内层 span 的 -webkit-line-clamp 负责省略号，但它在
            // WKWebView 的抽屉动画/虚拟列表重挂场景偶发按未截断高度排版（就是
            // 「标题下面多出空行」的元凶），外层这层把高度钉死。见 index.css。
            className="card-title-btn w-full text-left text-[12.5px] font-medium leading-snug hover:text-[var(--accent-strong)]"
            title={title}
          >
            {/* 截断落在外层按钮里的 span 上，而不是按钮自己：
                ① -webkit-line-clamp 要求 display:-webkit-box，而 <button> 在
                   WebKit 里会把内容包进匿名块，clamp 不保证生效；
                ② span 上的 clamp 只负责省略号，高度封顶交给外层 card-title-btn */}
            <span className="card-title-clamp">{title}</span>
          </button>
          {(metaLeft || metaRight) && (
            <div className="mt-1.5 flex items-center justify-between gap-2">
              <span className="min-w-0">{metaLeft}</span>
              <span className="shrink-0 text-[11px] text-[var(--text-2)]">{metaRight}</span>
            </div>
          )}
          {actions && <div className="mt-2">{actions}</div>}
        </div>
      </div>
    </div>
  );
}

/** 抽屉元信息区通用的类型小徽标 */
export function TypeChip({ label }: { label: string }) {
  return (
    <span className="inline-block rounded-full border border-[var(--separator)] bg-[var(--accent)] px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent-strong)]">
      {label}
    </span>
  );
}

/**
 * 卡片封面右上角的下载量徽标（工坊口径叫「订阅数」）—— 工坊/收藏共用同一观感。
 * 常驻显示，不走悬浮抽屉：它是挑壁纸时最先看的数字之一。
 */
export function CoverCountBadge({ count }: { count: number }) {
  return (
    <span className="flex items-center gap-1 rounded bg-black/55 px-1.5 py-0.5 text-[9.5px] font-semibold text-white backdrop-blur-sm">
      <svg
        width="9"
        height="9"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={2.6}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M12 3.6v10.2" />
        <path d="M7.6 10.2 12 14.4l4.4-4.2" />
        <path d="M4.4 17.2v1.4a2 2 0 0 0 2 2h11.2a2 2 0 0 0 2-2v-1.4" />
      </svg>
      {formatCount(count)}
    </span>
  );
}

/**
 * 卡片封面右上角的文件大小徽标（本地库用）—— 与 CoverCountBadge 同一观感。
 * 常驻显示，不走悬浮抽屉。
 */
export function CoverSizeBadge({ bytes }: { bytes: number }) {
  return (
    <span className="flex items-center gap-1 rounded bg-black/55 px-1.5 py-0.5 text-[9.5px] font-semibold text-white backdrop-blur-sm">
      <svg
        width="9"
        height="9"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={2.6}
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <ellipse cx="12" cy="5.6" rx="7.4" ry="2.8" />
        <path d="M4.6 5.6v6.2c0 1.55 3.31 2.8 7.4 2.8s7.4-1.25 7.4-2.8V5.6" />
        <path d="M4.6 11.8v6.2c0 1.55 3.31 2.8 7.4 2.8s7.4-1.25 7.4-2.8v-6.2" />
      </svg>
      {formatBytes(bytes)}
    </span>
  );
}
