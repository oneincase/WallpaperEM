// 通用确认弹框（Tauri WebView 禁用了原生 confirm()，自绘 macOS 风格替代）
import { tr } from "../lib/i18n";

export function ConfirmModal({
  title,
  message,
  confirmText,
  cancelText,
  danger = false,
  onConfirm,
  onCancel,
}: {
  /** 标题/正文由调用方给（调用方负责 tr()）；按钮文案不给则用通用词 */
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  // 默认值在渲染时求值（不能写成默认参数 —— 那样会在模块加载时冻结成中文）
  const cancelLabel = cancelText ?? tr("取消");
  const confirmLabel = confirmText ?? tr("确认");
  return (
    <div
      className="animate-overlay fixed inset-0 z-[60] flex items-center justify-center bg-black/30"
      onClick={onCancel}
    >
      <div
        className="card animate-modal-pop w-80 p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">{title}</h3>
        <p className="mt-2 text-[12.5px] leading-relaxed text-[var(--text-2)]">{message}</p>
        <div className="mt-4 flex justify-end gap-2">
          <button className="btn" onClick={onCancel}>
            {cancelLabel}
          </button>
          <button className={`btn ${danger ? "btn-danger" : "btn-primary"}`} onClick={onConfirm}>
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
