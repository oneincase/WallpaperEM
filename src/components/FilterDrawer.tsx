// 筛选抽屉：左侧滑出的半透明浮层，覆盖在内容网格之上
//
// 呈现方式与详情页抽屉一致（遮罩淡入 + 面板 translate-x 滑入 + 绝对定位覆盖），
// 只是方向相反：详情从右进，筛选从左进。
//
// 用覆盖而不是挤压布局：筛选是「调完就收」的临时操作，挤压会让整个网格
// 重排一次，视觉抖动比覆盖更扰人。半透明 + 背景模糊让下层壁纸仍隐约可见，
// 不至于失去上下文。
import { useEffect, useState, type ReactNode } from "react";

export function FilterDrawer({
  open,
  onClose,
  title = "筛选",
  /** 标题右侧的附加信息（如结果总数） */
  meta,
  activeCount = 0,
  onReset,
  children,
}: {
  open: boolean;
  onClose: () => void;
  title?: string;
  meta?: ReactNode;
  activeCount?: number;
  onReset?: () => void;
  children: ReactNode;
}) {
  // 进场动画：挂载后下一帧才切到终态，否则 transition 不会被触发
  const [shown, setShown] = useState(false);
  useEffect(() => {
    if (!open) {
      setShown(false);
      return;
    }
    const raf = requestAnimationFrame(() =>
      requestAnimationFrame(() => setShown(true)),
    );
    return () => cancelAnimationFrame(raf);
  }, [open]);

  // Esc 关闭
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <>
      <div
        onClick={onClose}
        className={`absolute inset-0 z-20 bg-black/25 transition-opacity duration-300 ${
          shown ? "opacity-100" : "opacity-0"
        }`}
      />
      <div
        className={`absolute inset-y-0 left-0 z-30 flex w-64 max-w-[85%] flex-col border-r border-[var(--separator)] bg-[var(--content)]/92 shadow-2xl backdrop-blur-2xl backdrop-saturate-150 transition-transform duration-300 ease-out ${
          shown ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <div className="flex shrink-0 items-center gap-2 border-b border-[var(--separator)] px-4 py-3">
          <span className="shrink-0 text-[13.5px] font-semibold">{title}</span>
          {meta && (
            <span className="truncate text-[11.5px] text-[var(--text-2)]">{meta}</span>
          )}
          <div className="ml-auto flex shrink-0 items-center gap-2">
            {onReset && activeCount > 0 && (
              <button
                className="text-[11.5px] text-[var(--text-2)] transition-colors hover:text-[var(--accent)]"
                onClick={onReset}
              >
                重置 {activeCount}
              </button>
            )}
            <button
              className="rounded p-0.5 text-[var(--text-2)] transition-colors hover:text-[var(--text-1)]"
              onClick={onClose}
              aria-label="关闭筛选"
            >
              <svg
                width="15"
                height="15"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth={2.2}
                strokeLinecap="round"
              >
                <path d="M6 6l12 12M18 6L6 18" />
              </svg>
            </button>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">{children}</div>
      </div>
    </>
  );
}

/** 打开筛选抽屉的按钮（放在工具栏里） */
export function FilterButton({
  onClick,
  activeCount = 0,
}: {
  onClick: () => void;
  activeCount?: number;
}) {
  return (
    <button
      className={`relative flex shrink-0 items-center gap-1.5 rounded-lg border px-2.5 py-1.5 text-[12.5px] font-medium transition-colors ${
        activeCount > 0
          ? "border-[var(--accent)] bg-[var(--accent)]/10 text-[var(--accent)]"
          : "border-[var(--separator)] text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/10"
      }`}
      onClick={onClick}
    >
      <svg
        width="15"
        height="15"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth={2.1}
        strokeLinecap="round"
        strokeLinejoin="round"
        style={{ flexShrink: 0 }}
      >
        <path d="M4.5 6.2h15" />
        <path d="M7.4 12h9.2" />
        <path d="M10.2 17.8h3.6" />
      </svg>
      筛选
      {activeCount > 0 && (
        <span className="rounded-full bg-[var(--accent)] px-1.5 text-[10px] font-bold text-white">
          {activeCount}
        </span>
      )}
    </button>
  );
}

/** 可折叠的筛选分组 */
export function FilterSection({
  label,
  /** 该组已选数量，折叠时也能看出有筛选 */
  count = 0,
  children,
  defaultOpen = true,
}: {
  label: string;
  count?: number;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  return (
    <details open={defaultOpen} className="group mb-2.5 last:mb-0">
      <summary className="flex cursor-pointer list-none items-center gap-1.5 py-1 text-[12px] font-semibold text-[var(--text-2)] marker:hidden">
        <svg
          width="11"
          height="11"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth={2.6}
          strokeLinecap="round"
          strokeLinejoin="round"
          className="shrink-0 transition-transform group-open:rotate-90"
        >
          <path d="M9 5l7 7-7 7" />
        </svg>
        {label}
        {count > 0 && (
          <span className="rounded bg-[var(--accent)]/12 px-1.5 text-[10px] font-semibold text-[var(--accent)]">
            {count}
          </span>
        )}
      </summary>
      <div className="mt-1.5 flex flex-wrap gap-1.5 pl-[17px]">{children}</div>
    </details>
  );
}

/** 标签胶囊：三态（未选 / 选中 / 排除） */
export function TagChip({
  label,
  state,
  onClick,
  title,
}: {
  label: string;
  state: "off" | "on" | "excluded";
  onClick: () => void;
  title?: string;
}) {
  const cls =
    state === "on"
      ? "border-[var(--accent)] bg-[var(--accent)] text-white"
      : state === "excluded"
        ? "border-red-500/50 bg-red-500/12 text-red-500 line-through"
        : "border-[var(--separator)] text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/10";
  return (
    <button
      className={`rounded-lg border px-2 py-[3px] text-[11.5px] transition-colors ${cls}`}
      onClick={onClick}
      title={title}
    >
      {label}
    </button>
  );
}
