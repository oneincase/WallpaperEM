// 锚定式下拉菜单（不是弹框）：贴着触发元素展开的小卡片，点外部/ESC 收起。
// 所有「选个目标/选个列表」类交互统一走它 —— 视觉轻、不打断浏览。
import { useEffect, useRef, type ReactNode } from "react";

export function AnchoredMenu({
  x,
  y,
  align = "left",
  drop = "down",
  onClose,
  children,
  width = 240,
}: {
  /** 触发元素左下角坐标（getBoundingClientRect） */
  x: number;
  y: number;
  /** 菜单相对锚点的水平对齐（防止超出右边界时用 right） */
  align?: "left" | "right";
  /** 展开方向：down 从锚点向下；up 从锚点向上（底部工具条用） */
  drop?: "down" | "up";
  onClose: () => void;
  children: ReactNode;
  width?: number;
}) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // 右边界越界时翻转对齐
  const overflowRight = x + width > window.innerWidth - 12;
  const left = align === "right" || overflowRight ? undefined : x;
  const right = align === "right" || overflowRight ? window.innerWidth - x : undefined;

  return (
    <div
      ref={ref}
      className="animate-modal-pop fixed z-[70] overflow-y-auto rounded-xl border border-[var(--separator)] bg-[var(--card)] py-1 shadow-xl"
      style={{
        left,
        right,
        top: drop === "down" ? y + 6 : undefined,
        bottom: drop === "up" ? window.innerHeight - y + 6 : undefined,
        width,
        maxHeight: 320,
      }}
    >
      {children}
    </div>
  );
}

/** 菜单里的一行（列表项） */
export function MenuItem({
  onClick,
  selected,
  children,
  disabled,
}: {
  onClick: () => void;
  selected?: boolean;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      disabled={disabled}
      onClick={onClick}
      className={`flex w-full items-center justify-between gap-3 px-3 py-1.5 text-left text-[13px] transition-colors hover:bg-black/5 disabled:opacity-40 dark:hover:bg-white/10 ${
        selected ? "text-[var(--accent-strong)]" : ""
      }`}
    >
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {selected && <span className="shrink-0 text-[11px]">✓</span>}
    </button>
  );
}

/** 菜单里的内联新建行（输入 + 确认，回车提交） */
export function MenuInputRow({
  value,
  onChange,
  onSubmit,
  placeholder,
  submitText,
}: {
  value: string;
  onChange: (v: string) => void;
  onSubmit: () => void;
  placeholder: string;
  submitText: string;
}) {
  return (
    <div className="mt-1 flex items-center gap-1.5 border-t border-[var(--separator)] px-2 pt-2 pb-1">
      <input
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") onSubmit();
          e.stopPropagation();
        }}
        placeholder={placeholder}
        className="min-w-0 flex-1 rounded-md border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
      />
      <button className="btn btn-primary !py-0.5 text-[11.5px]" onClick={onSubmit}>
        {submitText}
      </button>
    </div>
  );
}
