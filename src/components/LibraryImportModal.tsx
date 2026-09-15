// 本地库「导入壁纸」弹框：
// - 添加壁纸目录：多选文件夹，先收集进「待添加文件夹」列表，可继续追加 / 逐个移除，
//   确认后统一以引用方式入库（扫描工程目录，不复制文件）
// - 导入文件夹（单目录拷贝）／导入文件
// - 下方列出已导入的壁纸目录（引用条目），可移除（只删库记录，不动源文件）
import { useCallback, useEffect, useState } from "react";
import {
  api,
  type ImportBatchResult,
  type LibraryItem,
} from "../api/steam";
import { tr, trMsg } from "../lib/i18n";
import { useMessage } from "./Message";
import { IconTrash, IconUpload } from "./icons";

export function LibraryImportModal({
  onClose,
  onImported,
}: {
  onClose: () => void;
  /** 导入动作完成后回调（结果汇报 + 刷新列表由调用方处理） */
  onImported: (r: ImportBatchResult) => void;
}) {
  const msg = useMessage();
  const [dirs, setDirs] = useState<LibraryItem[]>([]);
  const [importing, setImporting] = useState(false);
  // 待添加文件夹：用户多选收集、尚未入库的绝对路径（可跨多次选择追加）
  const [pending, setPending] = useState<string[]>([]);

  // 引用条目列表：sourcePath 非空即「壁纸目录」
  const refreshDirs = useCallback(async () => {
    try {
      const items = await api.libraryList("", {});
      setDirs(items.filter((i) => i.sourcePath));
    } catch {
      // 列表刷新失败不打断弹框（导入结果有独立汇报）
    }
  }, []);

  useEffect(() => {
    void refreshDirs();
  }, [refreshDirs]);

  const run = async (fn: () => Promise<ImportBatchResult>) => {
    setImporting(true);
    try {
      onImported(await fn());
    } catch (e) {
      msg.error(trMsg(String(e)));
    } finally {
      setImporting(false);
      await refreshDirs();
    }
  };

  // 多选文件夹 → 追加进待添加列表（去重：同一路径不重复出现；已在库的也不重复）
  const pickFolders = async () => {
    try {
      const picked = await api.libraryPickFolders();
      if (!picked.length) return;
      setPending((prev) => {
        const have = new Set(prev);
        for (const p of picked) if (!have.has(p)) have.add(p);
        return [...have];
      });
    } catch (e) {
      msg.error(trMsg(String(e)));
    }
  };

  const removePending = (path: string) =>
    setPending((prev) => prev.filter((p) => p !== path));

  // 把待添加列表统一以引用方式入库；成功/取消后清空待添加列表
  const linkPending = async () => {
    const paths = pending;
    if (!paths.length) return;
    setImporting(true);
    try {
      const r = await api.libraryLinkFolders(paths);
      if (!r.cancelled) {
        onImported(r);
        setPending([]);
      }
    } catch (e) {
      msg.error(trMsg(String(e)));
    } finally {
      setImporting(false);
      await refreshDirs();
    }
  };

  const removeDir = async (it: LibraryItem) => {
    try {
      await api.libraryDelete(it.itemId);
      msg.info(tr("已移除壁纸目录「{t}」（源文件未删除）", { t: it.title }));
      await refreshDirs();
    } catch (e) {
      msg.error(trMsg(String(e)));
    }
  };

  const btnCls =
    "flex-1 rounded-lg border border-[var(--separator)] px-3 py-2 text-left text-[12.5px] font-medium hover:bg-black/5 disabled:opacity-60 dark:hover:bg-white/10";

  return (
    <div
      className="animate-overlay fixed inset-0 z-[60] flex items-center justify-center bg-black/30"
      onClick={importing ? undefined : onClose}
    >
      <div
        className="card animate-modal-pop flex max-h-[80vh] w-[560px] flex-col p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <h3 className="text-[14.5px] font-semibold">{tr("导入壁纸")}</h3>

        <div className="mt-3 flex gap-2">
          <button
            className={btnCls}
            onClick={() => void pickFolders()}
            disabled={importing}
            title={tr("多选文件夹，先加入下方列表，确认后以引用方式统一入库（不复制文件）")}
          >
            <span className="flex items-center gap-1.5">
              <IconUpload />
              {tr("添加壁纸目录")}
            </span>
            <p className="mt-1 text-[11px] font-normal text-[var(--text-2)]">
              {tr("多选文件夹，引用入库不复制")}
            </p>
          </button>
          <button
            className={btnCls}
            onClick={() => void run(() => api.libraryImportFolderPick("copy"))}
            disabled={importing}
            title={tr("把所选目录作为一个壁纸拷贝进库")}
          >
            {tr("导入文件夹")}
            <p className="mt-1 text-[11px] font-normal text-[var(--text-2)]">
              {tr("整个目录拷贝为一张壁纸")}
            </p>
          </button>
          <button
            className={btnCls}
            onClick={() => void run(() => api.libraryImportCustomPick())}
            disabled={importing}
            title={tr("支持多选；也可以直接把文件/文件夹拖进窗口")}
          >
            {tr("导入文件")}
            <p className="mt-1 text-[11px] font-normal text-[var(--text-2)]">
              {tr("视频 / GIF / 图片 / 网页文件")}
            </p>
          </button>
        </div>
        {importing && (
          <p className="mt-2 text-[12px] text-[var(--text-2)]">{tr("导入中…")}</p>
        )}

        <div className="mt-4 flex min-h-0 flex-1 flex-col gap-3">
          {/* 待添加文件夹：多选取回但尚未入库，可逐个移除 / 继续追加 */}
          <div className="flex shrink-0 flex-col">
            <div className="flex items-center justify-between">
              <p className="text-[12px] font-medium text-[var(--text-2)]">
                {tr("待添加文件夹")}
                <span className="ml-1 font-normal">
                  {tr("（引用方式，可添加多个不同文件夹后统一导入）")}
                </span>
              </p>
              {pending.length > 0 && (
                <button
                  className="rounded-lg border border-[var(--accent-strong)] bg-[var(--accent)] px-2.5 py-1 text-[11.5px] font-medium text-[var(--accent-fg)] hover:opacity-90 disabled:opacity-60"
                  onClick={() => void linkPending()}
                  disabled={importing}
                >
                  {tr("导入这 {n} 个文件夹", { n: pending.length })}
                </button>
              )}
            </div>
            <div className="mt-2 max-h-36 space-y-1.5 overflow-y-auto pr-1">
              {pending.length === 0 ? (
                <p className="rounded-lg border border-dashed border-[var(--separator)] px-3 py-3 text-center text-[12px] text-[var(--text-2)]">
                  {tr("点上方「添加壁纸目录」选择一个或多个文件夹")}
                </p>
              ) : (
                pending.map((p) => (
                  <div
                    key={p}
                    className="flex items-center gap-2 rounded-lg border border-[var(--separator)] px-2.5 py-1.5"
                  >
                    <p className="min-w-0 flex-1 truncate text-[12px]" title={p}>
                      {p}
                    </p>
                    <button
                      className="shrink-0 rounded-lg border border-[var(--separator)] px-2 py-1 text-[var(--text-2)] hover:border-red-500/40 hover:bg-red-500/10 hover:text-red-500"
                      onClick={() => removePending(p)}
                      data-tip={tr("从待添加列表移除（尚未入库，不影响源文件夹）")}
                      disabled={importing}
                    >
                      <IconTrash />
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>

          {/* 已导入的壁纸目录（引用条目） */}
          <div className="flex min-h-0 flex-1 flex-col">
            <p className="shrink-0 text-[12px] font-medium text-[var(--text-2)]">
              {tr("已导入的壁纸目录")}
              <span className="ml-1 font-normal">
                {tr("（引用方式，删除目录前请先移除）")}
              </span>
            </p>
            <div className="mt-2 min-h-0 flex-1 space-y-1.5 overflow-y-auto pr-1">
              {dirs.length === 0 ? (
                <p className="rounded-lg border border-dashed border-[var(--separator)] px-3 py-4 text-center text-[12px] text-[var(--text-2)]">
                  {tr("暂无壁纸目录")}
                </p>
              ) : (
                dirs.map((d) => (
                  <div
                    key={d.itemId}
                    className="flex items-center gap-2 rounded-lg border border-[var(--separator)] px-2.5 py-1.5"
                  >
                    <div className="min-w-0 flex-1">
                      <p className="truncate text-[12.5px] font-medium">{d.title}</p>
                      <p className="truncate text-[11px] text-[var(--text-2)]" title={d.sourcePath}>
                        {d.sourcePath}
                      </p>
                    </div>
                    {d.missing && (
                      <span className="shrink-0 rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                        {tr("文件丢失")}
                      </span>
                    )}
                    <button
                      className="shrink-0 rounded-lg border border-[var(--separator)] px-2 py-1 text-[var(--text-2)] hover:border-red-500/40 hover:bg-red-500/10 hover:text-red-500"
                      onClick={() => void removeDir(d)}
                      data-tip={tr("移除壁纸目录（不动源文件）")}
                    >
                      <IconTrash />
                    </button>
                  </div>
                ))
              )}
            </div>
          </div>
        </div>

        <div className="mt-4 flex justify-end">
          <button className="btn" onClick={onClose}>
            {tr("关闭")}
          </button>
        </div>
      </div>
    </div>
  );
}
