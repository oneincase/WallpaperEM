// 壁纸信息面板（本地库卡片「壁纸信息」按钮）：
//
// 卡片上只看得到封面 + 标题 + 类型/大小，想知道「这张从哪来、占多少空间、有没有丢文件、
// 挂在哪个轮播列表里」就得点开这里。数据全部来自 library_list 已返回的字段，不再额外
// 请求后端；唯一需要额外动作的是底部「打开文件位置」（复用 library_open_folder）。
import { useState, type ReactNode } from "react";
import { api, TYPE_LABELS, type LibraryItem } from "../api/steam";
import { formatBytes } from "../lib/format";
import { tr } from "../lib/i18n";
import { useMessage } from "./Message";
import { IconInfo, IconOpenFile } from "./icons";

/**
 * 入库时间：后端 `downloaded_at` 写的是 SQLite `unixepoch()`（**秒**），
 * 但历史数据里混过毫秒值，这里按量级判定一次，免得 1970 年的日期跑出来。
 */
function formatStamp(v: number): string {
  if (!Number.isFinite(v) || v <= 0) return tr("未知");
  const d = new Date(v > 1e12 ? v : v * 1000);
  return Number.isNaN(d.getTime()) ? tr("未知") : d.toLocaleString();
}

/** 一行「标签 + 值」；值默认可换行长文本，`mono` 用于 id / 路径这类需要逐字符读的内容 */
function Row({
  label,
  children,
  mono = false,
}: {
  label: string;
  children: ReactNode;
  mono?: boolean;
}) {
  return (
    <div className="flex gap-3 py-1.5">
      {/* 72px：容得下最长的「工坊条目 ID」（4 个汉字 + 空格 + ID ≈ 65px），
          不留 whenrap 就不会因为几像素的字体度量差异折成两行 */}
      <span className="w-[72px] shrink-0 whitespace-nowrap text-[12px] text-[var(--text-2)]">
        {label}
      </span>
      <span
        className={`min-w-0 flex-1 break-all text-[12.5px] ${mono ? "font-mono text-[11.5px]" : ""}`}
      >
        {children}
      </span>
    </div>
  );
}

/** 可复制的值（条目 id / 工坊 id / 路径）：点右侧小按钮复制 */
function Copyable({ text, onCopied }: { text: string; onCopied: () => void }) {
  return (
    <span className="flex items-start gap-1.5">
      <span className="min-w-0 flex-1 break-all font-mono text-[11.5px]" title={text}>
        {text}
      </span>
      <button
        className="shrink-0 rounded border border-[var(--separator)] px-1.5 py-px text-[10.5px] text-[var(--text-2)] transition-colors hover:border-[var(--accent-strong)] hover:text-[var(--accent-strong)]"
        onClick={() => {
          void navigator.clipboard.writeText(text).then(onCopied);
        }}
      >
        {tr("复制")}
      </button>
    </span>
  );
}

export function WallpaperInfoModal({
  item,
  applied,
  playlists,
  onClose,
}: {
  item: LibraryItem;
  /** 当前是否正应用到某块屏幕（library 页的 appliedItems 集合） */
  applied?: boolean;
  /** 该条目所在的切换列表名（无则空数组） */
  playlists?: string[];
  onClose: () => void;
}) {
  const msg = useMessage();
  const [opening, setOpening] = useState(false);

  const copied = () => msg.success(tr("已复制"));
  const listNames = playlists ?? [];

  return (
    <div
      className="animate-overlay fixed inset-0 z-[75] flex items-center justify-center bg-black/25 p-6"
      onClick={onClose}
    >
      <div
        className="card glass-panel animate-modal-pop flex max-h-[80vh] w-[460px] max-w-full flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* 头部：小封面 + 标题 + 状态 */}
        <div className="flex shrink-0 items-center gap-3 border-b border-[var(--separator)] px-5 py-3.5">
          {item.previewUrl ? (
            <img
              src={item.previewUrl}
              alt=""
              className="h-11 w-11 shrink-0 rounded-lg object-cover"
              draggable={false}
            />
          ) : (
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg border border-[var(--separator)] text-[var(--text-2)]">
              <IconInfo />
            </span>
          )}
          <div className="min-w-0 flex-1">
            <div className="truncate text-[14px] font-semibold" title={item.title}>
              {item.title}
            </div>
            <div className="mt-0.5 flex items-center gap-1.5">
              <span className="rounded-full border border-[var(--separator)] bg-[var(--accent)] px-2 py-0.5 text-[10.5px] font-medium text-[var(--accent-strong)]">
                {tr(TYPE_LABELS[item.type])}
              </span>
              {/* 状态优先级：文件丢了 > 正在应用 > 库内正常 */}
              {item.missing ? (
                <span className="rounded-full bg-amber-500/15 px-2 py-0.5 text-[10.5px] font-medium text-amber-400">
                  {tr("文件已丢失")}
                </span>
              ) : applied ? (
                <span className="rounded-full bg-green-500/15 px-2 py-0.5 text-[10.5px] font-medium text-green-400">
                  {tr("已应用到桌面")}
                </span>
              ) : (
                <span className="rounded-full bg-[var(--glass-subtle)] px-2 py-0.5 text-[10.5px] text-[var(--text-2)]">
                  {tr("库内正常")}
                </span>
              )}
            </div>
          </div>
          <button
            className="ml-1 flex h-6 w-6 shrink-0 items-center justify-center rounded-[6px] text-[var(--text-2)] transition-colors hover:bg-[var(--glass-hover)] hover:text-[var(--text-1)]"
            onClick={onClose}
            title={tr("关闭")}
            aria-label={tr("关闭")}
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.6"
              strokeLinecap="round"
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </div>

        {/* 明细 */}
        <div className="min-h-0 flex-1 overflow-y-auto px-5 py-2.5">
          <Row label={tr("条目 ID")} mono>
            <Copyable text={item.itemId} onCopied={copied} />
          </Row>
          <Row label={tr("大小")}>
            {formatBytes(item.sizeBytes)}
            <span className="ml-2 text-[11.5px] text-[var(--text-2)]">
              {tr("{n} 个文件", { n: item.fileCount })}
            </span>
          </Row>
          <Row label={tr("入库时间")}>{formatStamp(item.downloadedAt)}</Row>
          <Row label={tr("标签")}>
            {item.tags.length ? (
              <span className="flex flex-wrap gap-1">
                {item.tags.map((t) => (
                  <span
                    key={t}
                    className="rounded-full border border-[var(--separator)] px-2 py-0.5 text-[10.5px] text-[var(--text-2)]"
                  >
                    {t}
                  </span>
                ))}
              </span>
            ) : (
              <span className="text-[var(--text-2)]">{tr("无")}</span>
            )}
          </Row>
          <Row label={tr("所属列表")}>
            {listNames.length ? (
              listNames.join("、")
            ) : (
              <span className="text-[var(--text-2)]">{tr("无")}</span>
            )}
          </Row>
          {item.sourcePath && (
            <Row label={tr("来源目录")}>
              <Copyable text={item.sourcePath} onCopied={copied} />
              <div className="mt-1 text-[11px] text-[var(--text-2)]">
                {tr("引用方式入库：壁纸文件留在源目录，移动/删除源文件夹会导致条目失效")}
              </div>
            </Row>
          )}
          {item.publishedFileId && (
            <Row label={tr("工坊条目 ID")}>
              <Copyable text={item.publishedFileId} onCopied={copied} />
            </Row>
          )}
        </div>

        <div className="flex shrink-0 justify-end gap-2 border-t border-[var(--separator)] px-5 py-3">
          <button
            className="btn !py-1 text-[12px]"
            disabled={opening}
            onClick={() => {
              setOpening(true);
              void api
                .libraryOpenFolder(item.itemId)
                .catch((e) => msg.error(String(e)))
                .finally(() => setOpening(false));
            }}
          >
            <IconOpenFile />
            {tr("打开文件位置")}
          </button>
          <button className="btn btn-primary !py-1 text-[12px]" onClick={onClose}>
            {tr("关闭")}
          </button>
        </div>
      </div>
    </div>
  );
}
