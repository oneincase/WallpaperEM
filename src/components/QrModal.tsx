// 共用二维码弹层：标题 + 二维码 + 链接 + 复制/关闭。
// 设置页「局域网地址」与分享页「二维码」共用（此前分享页内联了一份，
// 设置页是行内展开 —— 统一为弹层展示）。
import { tr } from "../lib/i18n";
import { QrImage } from "./QrImage";
import { useMessage } from "./Message";

export function QrModal({
  title,
  text,
  hint,
  copyOkText,
  onClose,
}: {
  title: string;
  /** 二维码编码的内容（链接） */
  text: string;
  /** 二维码下方的说明（可选） */
  hint?: string;
  /** 复制成功的提示文案（缺省「已复制」） */
  copyOkText?: string;
  onClose: () => void;
}) {
  const msg = useMessage();

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      msg.success(copyOkText ?? tr("已复制"));
    } catch {
      msg.error(tr("复制失败，请手动选中复制"));
    }
  };

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/25 p-4" onClick={onClose}>
      <div
        className="glass-panel w-[340px] max-w-full rounded-2xl border border-[var(--card-border)] p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 truncate text-[15px] font-semibold">{title}</div>
        <div className="flex flex-col items-center gap-3">
          <QrImage text={text} size={200} />
          <code
            className="w-full truncate rounded-lg border border-[var(--separator)] bg-white/5 px-2 py-1.5 text-center text-[11.5px]"
            title={text}
          >
            {text}
          </code>
          {hint && (
            <p className="w-full text-center text-[11.5px] leading-relaxed text-[var(--text-2)]">
              {hint}
            </p>
          )}
          <div className="flex gap-2">
            <button className="btn btn-primary" onClick={() => void copy()}>
              {tr("复制链接")}
            </button>
            <button className="btn" onClick={onClose}>
              {tr("关闭")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
