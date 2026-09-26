// 工坊条目详情（T1/T2）：以右侧抽屉呈现（滑入动画由 App 的 DetailDrawer 负责）。
// 预览图不再整幅重复展示（卡片网格里已可见），仅在标题旁保留一枚小缩略图作定位参照。
import { useEffect, useState } from "react";
import { api, TYPE_LABELS, type WorkshopItem } from "../api/steam";
import { useWallpaperMeta } from "../hooks/useWallpaperMeta";
import { useApplyWallpaper } from "../hooks/useApplyWallpaper";
import { useItemProps } from "../hooks/useItemProps";
import { WallpaperPropsModal } from "../components/WallpaperPropsModal";
import { ShareModal } from "../components/ShareModal";
import { IconSliders, IconShare } from "../components/icons";
import { useMessage } from "../components/Message";
import { tr, trMsg } from "../lib/i18n";
import { tagLabel } from "../lib/tags";
import { renderBbcode } from "../lib/bbcode";

export function DetailPage({ id, onBack }: { id: string; onBack: () => void }) {
  const [item, setItem] = useState<WorkshopItem | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [enqueuing, setEnqueuing] = useState(false);
  const msg = useMessage();
  const [faved, setFaved] = useState(false);
  const [applying, setApplying] = useState(false);
  const [showProps, setShowProps] = useState(false);
  const [showShare, setShowShare] = useState(false);
  const { appliedItems, downloadedItems, refreshApplied } = useWallpaperMeta();
  const { apply: applyWithTarget, menuNode: applyMenu } = useApplyWallpaper();
  // 作者名片（Steam 资料页解析）；解析失败回退显示裸 SteamID64
  const [authorName, setAuthorName] = useState<string | null>(null);

  const downloaded = item ? downloadedItems.has(item.id) : false;
  const applied = item ? appliedItems.has(item.id) : false;
  // 是否可自定义由 project.json 决定（general.properties 有无声明），与壁纸类型无关
  const propDefs = useItemProps(downloaded && item ? item.id : null);
  const customizable = (propDefs?.length ?? 0) > 0;

  useEffect(() => {
    setLoading(true);
    setError("");
    setItem(null);
    setAuthorName(null);
    api
      .workshopItem(id)
      .then(setItem)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
    api.favoriteStatus(id).then(setFaved).catch(() => {});
  }, [id]);

  useEffect(() => {
    if (!item?.creator) return;
    let alive = true;
    api
      .steamAuthorSummary(item.creator)
      .then((a) => alive && a && setAuthorName(a.name))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [item?.creator]);

  return (
    <div className="flex h-full flex-col">
      {/* 抽屉头部：标题栏 + 关闭 */}
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-[var(--separator)] px-4">
        <span className="text-[13px] font-semibold tracking-tight">{tr("壁纸详情")}</span>
        <button
          onClick={onBack}
          title={tr("关闭")}
          aria-label={tr("关闭详情")}
          className="ml-auto flex h-6 w-6 items-center justify-center rounded-[6px] text-[var(--text-2)] transition-colors hover:bg-white/8 hover:text-[var(--text-1)]"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round">
            <path d="M2 2l8 8M10 2l-8 8" />
          </svg>
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        {loading && (
          <div className="py-20 text-center text-[13px] text-[var(--text-2)]">{tr("加载中…")}</div>
        )}
        {!loading && (error || !item) && (
          <div className="py-20 text-center text-[13px] text-red-500">
            {error ? trMsg(error) : tr("条目不存在")}
          </div>
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
                  <span className="rounded-full border border-[var(--separator)] bg-[var(--accent)] px-2 py-0.5 text-[11px] font-medium text-[var(--accent-strong)]">
                    {tr(TYPE_LABELS[item.type])}
                  </span>
                  {item.tags.slice(0, 4).map((t) => (
                    <span
                      key={t}
                      className="rounded-full border border-[var(--separator)] px-2 py-0.5 text-[10.5px] text-[var(--text-2)]"
                    >
                      {tagLabel(t)}
                    </span>
                  ))}
                </div>
              </div>
            </div>

            <div className="mt-3 flex gap-4 text-[12px] text-[var(--text-2)] flex-wrap">
              {item.subscriptions !== undefined && (
                <span>
                  ⬇ {tr("订阅")} {item.subscriptions.toLocaleString()}
                </span>
              )}
              {item.favorited !== undefined && (
                <span>
                  ★ {tr("收藏")} {item.favorited.toLocaleString()}
                </span>
              )}
              {item.fileSize !== undefined && (
                <span>📦 {(item.fileSize / 1024 / 1024).toFixed(1)} MB</span>
              )}
              {item.creator && (
                <span>
                  {tr("作者")} {authorName ?? item.creator}
                </span>
              )}
            </div>

            {/* 操作区 */}
            <div className="mt-4 flex gap-2 items-center flex-wrap">
              {downloaded ? (
                <button
                  className="btn !bg-sky-500/15 !text-sky-400 !border-sky-500/30 cursor-default disabled:opacity-75"
                  disabled
                  title={tr("已下载到本地库")}
                >
                  {tr("已下载")}
                </button>
              ) : (
                <button
                  className="btn btn-primary"
                  disabled={enqueuing}
                  onClick={async () => {
                    setEnqueuing(true);
                    try {
                      await api.downloadEnqueue(item.id);
                      msg.success(tr("已加入下载队列，请到「下载」页查看进度"));
                    } catch (e) {
                      msg.error(String(e));
                    } finally {
                      setEnqueuing(false);
                    }
                  }}
                >
                  {enqueuing ? "…" : `⬇ ${tr("下载")}`}
                </button>
              )}
              {applied ? (
                <button
                  className="btn !bg-green-500/15 !text-green-400 !border-green-500/30 hover:opacity-80"
                  title={tr("已应用到桌面（可点击重新应用或指定屏）")}
                  onClick={async (e) => {
                    setApplying(true);
                    try {
                      const r = await applyWithTarget(item.id, e.currentTarget);
                      if (r === "done") await refreshApplied();
                    } catch (err) {
                      msg.error(String(err));
                    } finally {
                      setApplying(false);
                    }
                  }}
                >
                  {applying ? "…" : tr("已应用")}
                </button>
              ) : (
                <button
                  className="btn"
                  disabled={applying}
                  title={tr("需先下载到本地库")}
                  onClick={async (e) => {
                    setApplying(true);
                    try {
                      const r = await applyWithTarget(item.id, e.currentTarget);
                      if (r === "done") await refreshApplied();
                    } catch (err) {
                      msg.error(String(err));
                    } finally {
                      setApplying(false);
                    }
                  }}
                >
                  {applying ? "…" : `🖥 ${tr("应用到桌面")}`}
                </button>
              )}
              {downloaded && customizable && (
                <button className="btn" onClick={() => setShowProps(true)} title={tr("壁纸配置")}>
                  <IconSliders size={14} />
                  {tr("壁纸配置")}
                  {propDefs!.some((d) => d.overridden) && (
                    <span className="text-[11px] text-[var(--accent-strong)]">•</span>
                  )}
                </button>
              )}
              {downloaded && item && (
                <button
                  className="btn"
                  onClick={() => setShowShare(true)}
                  title={tr("生成浏览器可打开的分享链接")}
                >
                  <IconShare size={14} />
                  {tr("分享")}
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
                {faved ? `★ ${tr("已收藏")}` : `☆ ${tr("收藏")}`}
              </button>
            </div>

            {item.description && (
              <div className="mt-5">
                <div className="text-[13px] font-semibold mb-1.5">{tr("描述")}</div>
                {/* 工坊描述是 BBCode（[h1]/[b]/[url=] 等），渲染成富文本而不是原文 */}
                <div className="whitespace-pre-wrap text-[13px] leading-relaxed text-[var(--text-2)]">
                  {renderBbcode(item.description)}
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      {/* 壁纸配置弹窗（作者属性 + 每壁纸播放设置） */}
      {showProps && item && (
        <WallpaperPropsModal
          itemId={item.id}
          title={item.title}
          onClose={() => setShowProps(false)}
        />
      )}

      {/* 分享弹窗（落地页/直链/iframe + 二维码） */}
      {showShare && item && (
        <ShareModal
          itemId={item.id}
          title={item.title}
          wtype={item.type}
          onClose={() => setShowShare(false)}
        />
      )}

      {applyMenu}
    </div>
  );
}
