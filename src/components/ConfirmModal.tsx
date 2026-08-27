// 通用确认弹框（Tauri WebView 禁用了原生 confirm()，自绘 macOS 风格替代）
export function ConfirmModal({
  title,
  message,
  confirmText = "确认",
  cancelText = "取消",
  danger = false,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: string;
  confirmText?: string;
  cancelText?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/30"
      onClick={onCancel}
    >
      <div
        className="card w-80 p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">{title}</h3>
        <p className="mt-2 text-[12.5px] leading-relaxed text-[var(--text-2)]">{message}</p>
        <div className="mt-4 flex justify-end gap-2">
          <button className="btn" onClick={onCancel}>
            {cancelText}
          </button>
          <button className={`btn ${danger ? "btn-danger" : "btn-primary"}`} onClick={onConfirm}>
            {confirmText}
          </button>
        </div>
      </div>
    </div>
  );
}
