// 壁纸卡片（工坊 / 本地库 / 收藏共用）：
// 预览图为 1:1，object-cover 充满整卡；详情/操作容器是「向上抽屉」——
// 悬浮在卡片上时从底边滑出覆盖在图片上方，鼠标移出自动滑回隐藏。
import type { ReactNode, SyntheticEvent } from "react";

export function WallpaperCard({
  imageUrl,
  title,
  onOpen,
  badges,
  metaLeft,
  metaRight,
  actions,
  alt,
}: {
  /** 预览图 URL（1:1 裁切充满整卡）；空则显示占位 */
  imageUrl?: string;
  title: string;
  /** 点击卡片主体（图片区）打开详情 */
  onOpen: () => void;
  /** 图片左上角常驻状态徽标（已应用/已下载等） */
  badges?: ReactNode;
  /** 抽屉内标题下方的左侧元信息（类型徽标等） */
  metaLeft?: ReactNode;
  /** 抽屉内标题下方的右侧元信息（订阅数/大小等） */
  metaRight?: ReactNode;
  /** 抽屉底部操作按钮行（本地库的五连按钮等）；无则不渲染该行 */
  actions?: ReactNode;
  alt?: string;
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
            loading="lazy"
            className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.04]"
            draggable={false}
          />
        ) : (
          <span className="flex h-full w-full items-center justify-center text-[13px] text-[var(--text-2)]">
            无预览
          </span>
        )}
      </button>

      {/* 常驻状态徽标 */}
      {badges && <div className="absolute left-1.5 top-1.5 z-10 flex flex-col gap-1">{badges}</div>}

      {/* 向上抽屉：默认沉在底边外，悬停/键盘聚焦时滑出覆盖在图上 */}
      <div
        className="absolute inset-x-0 bottom-0 z-20 translate-y-full opacity-0 transition-all duration-300 ease-out group-focus-within:translate-y-0 group-focus-within:opacity-100 group-hover:translate-y-0 group-hover:opacity-100"
        onClick={stop}
        onKeyDown={stop}
      >
        <div className="border-t border-[var(--separator)] bg-[var(--card)]/95 p-2.5 backdrop-blur-md">
          <button
            onClick={onOpen}
            className="line-clamp-2 block w-full text-left text-[12.5px] font-medium leading-snug hover:text-[var(--accent)]"
            title={title}
          >
            {title}
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
    <span className="inline-block rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent)]">
      {label}
    </span>
  );
}
