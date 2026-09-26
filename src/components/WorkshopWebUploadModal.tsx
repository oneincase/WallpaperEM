// 工坊网页版上传弹框：调用后端暂存内容 → 打开 Steam 工坊网页版上传/编辑页 +
// 在文件管理器中定位暂存目录。网页上的提交动作发生在浏览器里，本应用只负责
// 「整理内容 + 跳转 + 指引」，不感知最终发布结果。
import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, type LibraryItem } from "../api/steam";
import { tr, trMsg } from "../lib/i18n";
import { useMessage } from "./Message";

type Phase = "preparing" | "ready" | "failed";

export function WorkshopWebUploadModal({
  item,
  onClose,
}: {
  item: LibraryItem;
  onClose: () => void;
}) {
  const msg = useMessage();
  const [phase, setPhase] = useState<Phase>("preparing");
  const [url, setUrl] = useState("");
  const [stagedPath, setStagedPath] = useState<string | null>(null);
  const [isUpdate, setIsUpdate] = useState(Boolean(item.publishedFileId));
  const [error, setError] = useState("");

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        // 后端会同时打开浏览器页面与暂存目录；这里只接收结果做指引
        const r = await api.workshopWebUploadPrepare(item.itemId);
        if (cancelled) return;
        setUrl(r.url);
        setStagedPath(r.stagedPath);
        setIsUpdate(r.update);
        setPhase("ready");
      } catch (e) {
        if (cancelled) return;
        setError(trMsg(String(e)));
        setPhase("failed");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [item.itemId]);

  return (
    <div
      className="animate-overlay fixed inset-0 z-[60] flex items-center justify-center bg-black/25"
      onClick={phase === "preparing" ? undefined : onClose}
    >
      <div
        className="card glass-panel animate-modal-pop w-[420px] p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">
          {isUpdate ? tr("网页版更新工坊条目") : tr("网页版上传到创意工坊")}
        </h3>
        <p className="mt-1 truncate text-[12px] text-[var(--text-2)]" title={item.title}>
          {tr("「{title}」", { title: item.title })}
        </p>

        {phase === "preparing" && (
          <p className="mt-4 text-[12.5px] text-[var(--text-2)]">
            {tr("正在整理壁纸内容并打开网页…")}
          </p>
        )}

        {phase === "failed" && (
          <div className="mt-4 space-y-3">
            <p className="text-[13px] text-red-500">{error}</p>
            <div className="flex justify-end gap-2">
              <button className="btn" onClick={onClose}>
                {tr("关闭")}
              </button>
            </div>
          </div>
        )}

        {phase === "ready" && (
          <div className="mt-4 space-y-3">
            {isUpdate ? (
              <p className="text-[12.5px] leading-relaxed">
                {tr(
                  "已在浏览器打开该条目的网页版编辑页，在页面上修改信息并提交更新即可。",
                )}
              </p>
            ) : (
              <>
                <p className="text-[12.5px] leading-relaxed">
                  {tr(
                    "已自动整理好上传内容，并在浏览器打开工坊「新建条目」页、在文件管理器中定位到内容文件夹。请在网页上：",
                  )}
                </p>
                <ol className="list-decimal space-y-1 pl-5 text-[12px] leading-relaxed text-[var(--text-2)]">
                  <li>{tr("填写标题、描述等信息")}</li>
                  <li>
                    {tr(
                      "「内容文件夹 / Content folder」选择已为你打开的这个文件夹（内容已按工坊要求整理）",
                    )}
                  </li>
                  <li>
                    {tr(
                      "「预览图 / Preview image」从该文件夹里选择 preview.png / preview.jpg（如有）",
                    )}
                  </li>
                  <li>{tr("同意 Steam 工坊条款后点提交")}</li>
                </ol>
                {stagedPath && (
                  <p
                    className="break-all rounded-lg border border-[var(--separator)] px-2.5 py-1.5 text-[11px] text-[var(--text-2)]"
                    title={stagedPath}
                  >
                    {stagedPath}
                  </p>
                )}
              </>
            )}

            <a
              href={url}
              target="_blank"
              rel="noreferrer"
              className="block break-all text-[12px] text-[var(--accent-strong)] underline"
              onClick={(e) => {
                // 兜底：在部分 WebView 下 target=_blank 不生效，显式走 opener
                e.preventDefault();
                void openUrl(url).catch(() => msg.error(tr("无法打开浏览器")));
              }}
            >
              {url}
            </a>

            <div className="flex justify-end gap-2 pt-1">
              <button className="btn btn-primary" onClick={onClose}>
                {tr("我知道了")}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
