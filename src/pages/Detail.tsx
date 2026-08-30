// 工坊条目详情（T1/T2）：以右侧抽屉呈现（滑入动画由 App 的 DetailDrawer 负责）。
// 预览图不再整幅重复展示（卡片网格里已可见），仅在标题旁保留一枚小缩略图作定位参照。
import { useEffect, useState } from "react";
import { api, TYPE_LABELS, type WorkshopItem } from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";
import { useItemProps } from "../hooks/useItemProps";
import { WallpaperPropsModal } from "../components/WallpaperPropsModal";
import { IconSliders } from "../components/icons";

export function DetailPage({ id, onBack }: { id: string; onBack: () => void }) {
  const [item, setItem] = useState<WorkshopItem | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [enqueuing, setEnqueuing] = useState(false);
  const [msg, setMsg] = useState("");
  const [faved, setFaved] = useState(false);
  const [applying, setApplying] = useState(false);
  const [showProps, setShowProps] = useState(false);
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();

  const downloaded = item ? downloadedItems.has(item.id) : false;
  const applied = item ? appliedItems.has(item.id) : false;
  // 是否可自定义由 project.json 决定（general.properties 有无声明），与壁纸类型无关
  const propDefs = useItemProps(downloaded && item ? item.id : null);
  const customizable = (propDefs?.length ?? 0) > 0;

  useEffect(() => {
    setLoading(true);
    setError("");
    setItem(null);
    api
      .workshopItem(id)
      .then(setItem)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
    api.favoriteStatus(id).then(setFaved).catch(() => {});
  }, [id]);

  return (
    <div className="flex h-full flex-col">
      {/* 抽屉头部：标题栏 + 关闭 */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-[var(--separator)] px-4">
        <span className="text-[13px] font-semibold tracking-tight">壁纸详情</span>
        <button
          onClick={onBack}
          title="关闭"
          aria-label="关闭详情"
          className="ml-auto flex h-6 w-6 items-center justify-center rounded-[6px] text-[var(--text-2)] transition-colors hover:bg-black/5 hover:text-[var(--text-1)] dark:hover:bg-white/8"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
            <path d="M2 2l8 8M10 2l-8 8" />
          </svg>
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading && <div className="py-20 text-center text-[13px] text-[var(--text-2)]">加载中…</div>}
        {!loading && (error || !item) && (
          <div className="py-20 text-center text-[13px] text-red-500">{error || "条目不存在"}</div>
        )}

        {!loading && item && (
          <div className="p-5">
            {/* 标题区：小缩略图（非整幅预览）+ 标题 + 类型/标签 */}
            <div className="flex items-start gap-3">
              {item.previewUrl && (
                <img
                  src={item.previewUrl}
                  alt=""
                  className="h-14 w-14 shrink-0 rounded-xl object-cover"
                  draggable={false}
                />
              )}
              <div className="min-w-0">
                <h1 className="text-[17px] font-bold leading-snug tracking-tight">{item.title}</h1>
                <div className="mt-1.5 flex items-center gap-1.5 flex-wrap">
                  <span className="rounded-full bg-[var(--accent)]/10 px-2 py-0.5 text-[11px] font-medium text-[var(--accent)]">
                    {TYPE_LABELS[item.type]}
                  </span>
                  {item.tags.slice(0, 4).map((t) => (
                    <span
                      key={t}
                      className="rounded-full border border-[var(--separator)] px-2 py-0.5 text-[10.5px] text-[var(--text-2)]"
                    >
                      {t}
                    </span>
                  ))}
                </div>
              </div>
            </div>

            <div className="mt-3 flex gap-4 text-[12px] text-[var(--text-2)] flex-wrap">
              {item.subscriptions !== undefined && (
                <span>⬇ 订阅 {item.subscriptions.toLocaleString()}</span>
              )}
              {item.favorited !== undefined && <span>★ 收藏 {item.favorited.toLocaleString()}</span>}
              {item.fileSize !== undefined && (
                <span>📦 {(item.fileSize / 1024 / 1024).toFixed(1)} MB</span>
              )}
              {item.creator && <span>作者 {item.creator}</span>}
            </div>

            {/* 操作区 */}
            <div className="mt-4 flex gap-2 items-center flex-wrap">
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
              {downloaded && customizable && (
                <button className="btn" onClick={() => setShowProps(true)} title="编辑壁纸自定义属性">
                  <IconSliders size={14} />
                  自定义属性
                  {propDefs!.some((d) => d.overridden) && (
                    <span className="text-[11px] text-[var(--accent)]">•</span>
                  )}
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
            </div>
            {msg && <div className="mt-2 text-[12.5px] text-[var(--text-2)]">{msg}</div>}

            {item.description && (
              <div className="mt-5">
                <div className="text-[13px] font-semibold mb-1.5">描述</div>
                <p className="whitespace-pre-wrap text-[13px] leading-relaxed text-[var(--text-2)]">
                  {item.description}
                </p>
              </div>
            )}
          </div>
        )}
      </div>

      {/* 自定义属性弹窗（project.json general.properties，是否可自定义与壁纸类型无关） */}
      {showProps && item && (
        <WallpaperPropsModal
          itemId={item.id}
          title={item.title}
          onClose={() => setShowProps(false)}
        />
      )}
    </div>
  );
}
