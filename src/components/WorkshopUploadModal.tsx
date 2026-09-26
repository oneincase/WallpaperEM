// 工坊上传弹框：本地库条目 → Steam 创意工坊（新建 / 更新已发布条目）
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type LibraryItem, type WorkshopUploadJob } from "../api/steam";
import { tr, trMsg } from "../lib/i18n";
import { useMessage } from "./Message";

type Phase = "form" | "uploading" | "done" | "failed";

/** 上传进度事件 `workshop-upload-progress` 的载荷（与后端 UploadJob 同构） */
function onProgress(cb: (job: WorkshopUploadJob) => void): () => void {
  let un: (() => void) | undefined;
  void listen<WorkshopUploadJob>("workshop-upload-progress", (e) => cb(e.payload)).then(
    (fn) => {
      un = fn;
    }
  );
  return () => un?.();
}

export function WorkshopUploadModal({
  item,
  onClose,
}: {
  item: LibraryItem;
  onClose: () => void;
}) {
  const msg = useMessage();
  const [title, setTitle] = useState(item.title);
  const [description, setDescription] = useState("");
  const [visibility, setVisibility] = useState<"public" | "friends" | "private">("public");
  const [changelog, setChangelog] = useState("");
  const [phase, setPhase] = useState<Phase>("form");
  const [job, setJob] = useState<WorkshopUploadJob | null>(null);
  const jobRef = useRef<string | null>(null);

  // 已发布过的条目：再次上传 = 更新
  const isUpdate = Boolean(item.publishedFileId);

  useEffect(() => {
    if (phase !== "uploading") return;
    return onProgress((j) => {
      if (j.jobId !== jobRef.current) return;
      setJob(j);
      if (j.status === "done") {
        setPhase("done");
      } else if (j.status === "failed") {
        setPhase("failed");
      }
    });
  }, [phase]);

  const start = async () => {
    try {
      const { jobId } = await api.workshopUploadStart({
        itemId: item.itemId,
        title: title.trim() || undefined,
        description: description.trim() || undefined,
        visibility,
        changelog: changelog.trim() || undefined,
      });
      jobRef.current = jobId;
      setPhase("uploading");
    } catch (e) {
      msg.error(trMsg(String(e)));
    }
  };

  const busy = phase === "uploading";
  const progress = job?.progress ?? 0;

  return (
    <div
      className="animate-overlay fixed inset-0 z-[60] flex items-center justify-center bg-black/25"
      onClick={busy ? undefined : onClose}
    >
      <div
        className="card glass-panel animate-modal-pop w-96 p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">
          {isUpdate ? tr("更新创意工坊条目") : tr("上传到创意工坊")}
        </h3>
        <p className="mt-1 text-[12px] text-[var(--text-2)]">
          {tr("「{title}」 → Steam 创意工坊（Wallpaper Engine）", { title: item.title })}
        </p>

        {phase === "form" && (
          <div className="mt-3 space-y-3">
            <label className="block">
              <span className="text-[12px] text-[var(--text-2)]">{tr("标题")}</span>
              <input
                value={title}
                onChange={(e) => setTitle(e.target.value)}
                className="mt-1 w-full rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
              />
            </label>
            <label className="block">
              <span className="text-[12px] text-[var(--text-2)]">{tr("描述")}</span>
              <textarea
                value={description}
                onChange={(e) => setDescription(e.target.value)}
                rows={3}
                className="mt-1 w-full resize-none rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
              />
            </label>
            <label className="block">
              <span className="text-[12px] text-[var(--text-2)]">{tr("可见性")}</span>
              <select
                value={visibility}
                onChange={(e) =>
                  setVisibility(e.target.value as "public" | "friends" | "private")
                }
                className="mt-1 w-full rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
              >
                <option value="public">{tr("公开")}</option>
                <option value="friends">{tr("仅好友可见")}</option>
                <option value="private">{tr("私密")}</option>
              </select>
            </label>
            {/* 版权/授权提示：上传他人作品需自行确认有授权 */}
            <p className="rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-[11.5px] leading-relaxed text-amber-400">
              {tr(
                "上传他人制作的壁纸前请注意版权/授权：转载他人作品需要获得对方许可，并遵守 Steam 订阅者协议与工坊规则。"
              )}
            </p>
            {isUpdate && (
              <label className="block">
                <span className="text-[12px] text-[var(--text-2)]">{tr("更新说明")}</span>
                <input
                  value={changelog}
                  onChange={(e) => setChangelog(e.target.value)}
                  className="mt-1 w-full rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
                />
              </label>
            )}
            <div className="flex justify-end gap-2 pt-1">
              <button className="btn" onClick={onClose}>
                {tr("取消")}
              </button>
              <button className="btn btn-primary" onClick={start}>
                {isUpdate ? tr("开始更新") : tr("开始上传")}
              </button>
            </div>
          </div>
        )}

        {phase === "uploading" && (
          <div className="mt-4">
            <div className="h-2 w-full overflow-hidden rounded-full bg-[var(--separator)]">
              <div
                className="h-full rounded-full bg-[var(--accent-fill)] transition-all"
                style={{ width: `${Math.min(100, Math.max(2, progress))}%` }}
              />
            </div>
            <p className="mt-2 text-[12px] text-[var(--text-2)]">
              {job?.status === "creating"
                ? tr("正在创建工坊条目…")
                : job?.status === "committing"
                  ? tr("正在提交变更…")
                  : tr("正在上传 {p}%…", { p: Math.floor(progress) })}
            </p>
            <p className="mt-1 text-[11px] text-[var(--text-2)]">{tr("请保持 Steam 在线")}</p>
          </div>
        )}

        {phase === "done" && (
          <div className="mt-3 space-y-3">
            <p className="text-[13px]">{tr("上传完成！条目已在 Steam 创意工坊。")}</p>
            {job?.workshopUrl && (
              <a
                href={job.workshopUrl}
                target="_blank"
                rel="noreferrer"
                className="block break-all text-[12px] text-[var(--accent-strong)] underline"
              >
                {job.workshopUrl}
              </a>
            )}
            <div className="flex justify-end gap-2">
              <button className="btn btn-primary" onClick={onClose}>
                {tr("完成")}
              </button>
            </div>
          </div>
        )}

        {phase === "failed" && (
          <div className="mt-3 space-y-3">
            <p className="text-[13px] text-red-500">{trMsg(job?.error || tr("上传失败"))}</p>
            <div className="flex justify-end gap-2">
              <button className="btn" onClick={onClose}>
                {tr("关闭")}
              </button>
              <button
                className="btn btn-primary"
                onClick={() => {
                  setPhase("form");
                  setJob(null);
                }}
              >
                {tr("重试")}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
