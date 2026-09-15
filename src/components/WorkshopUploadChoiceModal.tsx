// 工坊上传方式选择：Steam 客户端上传（ISteamUGC，应用内完成）
// 或网页接口上传（系统浏览器打开 Steam 工坊网页版，走浏览器登录态）
import type { LibraryItem } from "../api/steam";
import { tr } from "../lib/i18n";

export type UploadMethod = "steam" | "web";

export function WorkshopUploadChoiceModal({
  item,
  onClose,
  onChoose,
}: {
  item: LibraryItem;
  onClose: () => void;
  onChoose: (method: UploadMethod) => void;
}) {
  const isUpdate = Boolean(item.publishedFileId);

  const cardCls =
    "group flex w-full items-start gap-3 rounded-xl border border-[var(--separator)] p-3.5 text-left transition-colors hover:border-[var(--accent-strong)] hover:bg-black/5 dark:hover:bg-white/10";

  return (
    <div
      className="animate-overlay fixed inset-0 z-[60] flex items-center justify-center bg-black/30"
      onClick={onClose}
    >
      <div
        className="card animate-modal-pop w-[440px] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">{tr("上传到创意工坊")}</h3>
        <p className="mt-1 truncate text-[12px] text-[var(--text-2)]" title={item.title}>
          {tr("「{title}」", { title: item.title })}
          {isUpdate ? tr(" · 更新已发布条目") : ""}
        </p>

        <div className="mt-4 space-y-2.5">
          {/* Steam 客户端上传：应用内走 ISteamUGC，需要 Steam 客户端运行 */}
          <button className={cardCls} onClick={() => onChoose("steam")}>
            <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--accent)] text-[15px]">
              🎮
            </span>
            <span className="min-w-0">
              <span className="block text-[13px] font-medium">
                {tr("Steam 客户端上传")}
              </span>
              <span className="mt-0.5 block text-[11.5px] leading-relaxed text-[var(--text-2)]">
                {tr(
                  "在应用内填写信息并直接提交，可看上传进度；需要本机运行 Steam 客户端、登录账号拥有 Wallpaper Engine",
                )}
              </span>
            </span>
          </button>

          {/* 网页接口上传：浏览器打开工坊网页版 */}
          <button className={cardCls} onClick={() => onChoose("web")}>
            <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-[var(--accent)] text-[15px]">
              🌐
            </span>
            <span className="min-w-0">
              <span className="block text-[13px] font-medium">
                {tr("网页接口上传")}
              </span>
              <span className="mt-0.5 block text-[11.5px] leading-relaxed text-[var(--text-2)]">
                {isUpdate
                  ? tr(
                      "用系统浏览器打开该条目的工坊网页版编辑页，在网页上提交更新（走浏览器里的 Steam 登录态）",
                    )
                  : tr(
                      "自动整理好内容文件夹，并用系统浏览器打开工坊网页版新建页，在网页上选择该文件夹完成发布（不依赖 Steam 客户端）",
                    )}
              </span>
            </span>
          </button>
        </div>

        <div className="mt-4 flex justify-end">
          <button className="btn" onClick={onClose}>
            {tr("取消")}
          </button>
        </div>
      </div>
    </div>
  );
}
