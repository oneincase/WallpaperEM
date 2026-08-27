// 工坊条目详情页（T1/T2）
import { useEffect, useState } from "react";
import { api, TYPE_LABELS, type WorkshopItem } from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";

export function DetailPage({ id, onBack }: { id: string; onBack: () => void }) {
  const [item, setItem] = useState<WorkshopItem | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [enqueuing, setEnqueuing] = useState(false);
  const [msg, setMsg] = useState("");
  const [faved, setFaved] = useState(false);
  const [applying, setApplying] = useState(false);
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();

  const downloaded = item ? downloadedItems.has(item.id) : false;
  const applied = item ? appliedItems.has(item.id) : false;

  useEffect(() => {
    setLoading(true);
    setError("");
    api
      .workshopItem(id)
      .then(setItem)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
    api.favoriteStatus(id).then(setFaved).catch(() => {});
  }, [id]);

  if (loading) {
    return <div className="py-20 text-center text-[var(--text-2)]">加载中…</div>;
  }
  if (error || !item) {
    return (
      <div className="py-20 text-center text-[13px] text-red-500">
        {error || "条目不存在"}
        <div className="mt-3">
          <button className="btn" onClick={onBack}>
            返回
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="mx-auto w-full max-w-4xl">
      <button className="btn mb-4" onClick={onBack}>
        ← 返回工坊
      </button>

      <div className="card overflow-hidden">
        {item.previewUrl && (
          <div className="aspect-video w-full bg-black/10">
            <img src={item.previewUrl} alt={item.title} className="h-full w-full object-cover" />
          </div>
        )}
        <div className="p-6">
          <div className="flex items-start justify-between gap-4">
            <div>
              <h1 className="text-[20px] font-bold tracking-tight">{item.title}</h1>
              <div className="mt-2 flex items-center gap-2 flex-wrap">
                <span className="rounded-full bg-[var(--accent)]/10 px-2.5 py-0.5 text-[12px] font-medium text-[var(--accent)]">
                  {TYPE_LABELS[item.type]}
                </span>
                {item.tags.map((t) => (
                  <span
                    key={t}
                    className="rounded-full border border-[var(--separator)] px-2.5 py-0.5 text-[11.5px] text-[var(--text-2)]"
                  >
                    {t}
                  </span>
                ))}
              </div>
              <div className="mt-3 flex gap-5 text-[12.5px] text-[var(--text-2)]">
                {item.subscriptions !== undefined && (
                  <span>⬇ 订阅 {item.subscriptions.toLocaleString()}</span>
                )}
                {item.favorited !== undefined && (
                  <span>★ 收藏 {item.favorited.toLocaleString()}</span>
                )}
                {item.fileSize !== undefined && (
                  <span>📦 {(item.fileSize / 1024 / 1024).toFixed(1)} MB</span>
                )}
                {item.creator && <span>作者 {item.creator}</span>}
              </div>
            </div>
          </div>

          <div className="mt-5 flex gap-2 items-center flex-wrap">
          {downloaded ? (
            <button
              className="btn !bg-sky-500/15 !text-sky-600 dark:!text-sky-400 !border-sky-500/30 cursor-default disabled:opacity-75"
              disabled
              title="已下载到本地库"
            >
              已下载
            </button>
          ) : (
            <button
              className="btn btn-primary"
              disabled={enqueuing}
              onClick={async () => {
                setEnqueuing(true);
                setMsg("");
                try {
                  await api.downloadEnqueue(item.id);
                  setMsg("✅ 已加入下载队列，请到「下载」页查看进度");
                } catch (e) {
                  setMsg(String(e));
                } finally {
                  setEnqueuing(false);
                }
              }}
            >
              {enqueuing ? "…" : "⬇ 下载"}
            </button>
          )}
          {applied ? (
            <button
              className="btn !bg-green-500/15 !text-green-600 dark:!text-green-400 !border-green-500/30 cursor-default disabled:opacity-75"
              disabled
              title="已应用到桌面"
            >
              已应用
            </button>
          ) : (
            <button
              className="btn"
              disabled={applying}
              title="需先下载到本地库"
              onClick={async () => {
                setApplying(true);
                setMsg("");
                try {
                  await api.wallpaperApplyItem(item.id);
                  await refreshApplied();
                } catch (e) {
                  setMsg(String(e));
                } finally {
                  setApplying(false);
                }
              }}
            >
              {applying ? "…" : "🖥 应用到桌面"}
            </button>
          )}
            <button
              className={`btn ${faved ? "btn-danger" : ""}`}
              onClick={async () => {
                if (faved) {
                  await api.favoriteRemove(item.id);
                  setFaved(false);
                } else {
                  await api.favoriteAdd(item.id);
                  setFaved(true);
                }
              }}
            >
              {faved ? "★ 已收藏" : "☆ 收藏"}
            </button>
            {msg && <span className="text-[12.5px] text-[var(--text-2)]">{msg}</span>}
          </div>

          {item.description && (
            <div className="mt-5">
              <div className="text-[13px] font-semibold mb-1.5">描述</div>
              <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-[var(--text-2)] max-h-64 overflow-y-auto">
                {item.description}
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
