// 下载页：任务列表 / 进度 / Steam Guard 验证码输入 / 取消 / 重试（T2）
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  DOWNLOAD_STATUS_LABELS,
  type DownloadTask,
} from "../api/steam";
import { ConfirmModal } from "../components/ConfirmModal";
import { downloadDisplayTitle } from "../components/GuardDialogs";
import { EmptyState } from "../components/EmptyState";
import { useMessage } from "../components/Message";
import { tr, trMsg } from "../lib/i18n";

export function DownloadsPage() {
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [loading, setLoading] = useState(true);

  const [confirmClear, setConfirmClear] = useState(false);
  // 壁纸 ID 直下载：内联输入（不弹框）。元数据零依赖入队，steamcmd 按 ID 直取，
  // 国内免代理也能下（详情接口被墙不影响）
  const [idOpen, setIdOpen] = useState(false);
  const [idInput, setIdInput] = useState("");
  const [idBusy, setIdBusy] = useState(false);
  const msg = useMessage();
  // 正在等待手机 Steam App 确认登录的任务（steamcmd 推手机确认时无需输码，只需提示）
  const [mobileConfirm, setMobileConfirm] = useState<Set<number>>(new Set());
  // 事件回调里要读最新任务列表，但不能把 tasks 放进 effect 依赖，
  // 否则每次进度更新都会拆装 IPC 监听器。
  const tasksRef = useRef<DownloadTask[]>([]);
  tasksRef.current = tasks;

  const enqueueById = async () => {
    const id = idInput.trim();
    if (!/^\d{3,}$/.test(id)) {
      msg.error(tr("请输入数字壁纸 ID（创意工坊条目 ID）"));
      return;
    }
    setIdBusy(true);
    try {
      await api.downloadEnqueue(id);
      msg.success(tr("已加入下载队列：{id}", { id }));
      setIdOpen(false);
      setIdInput("");
      await refresh();
    } catch (e) {
      msg.error(String(e));
    } finally {
      setIdBusy(false);
    }
  };

  const refresh = useCallback(async () => {
    try {
      setTasks(await api.downloadList());
    } catch (e) {
      console.warn(e);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const un = listen<{ taskId: number; status: string; progress: number }>(
      "download:progress",
      (e) => {
        const p = e.payload;
        // 状态推进即说明已不在等待手机确认
        if (p.status !== "authenticating") {
          setMobileConfirm((prev) => {
            if (!prev.has(p.taskId)) return prev;
            const next = new Set(prev);
            next.delete(p.taskId);
            return next;
          });
        }
        // 依赖补拉等后台自动入队的新任务不在当前列表里：拉一次全量
        if (!tasksRef.current.some((t) => t.id === p.taskId)) {
          refresh();
          return;
        }
        setTasks((prev) =>
          prev.map((t) =>
            t.id === p.taskId
              ? { ...t, status: p.status as DownloadTask["status"], progress: p.progress }
              : t,
          ),
        );
      },
    );
    const unMobile = listen<{ taskId: number }>("download:mobile-confirm", (e) => {
      const id = e.payload.taskId;
      setMobileConfirm((prev) => new Set(prev).add(id));
      if (!tasksRef.current.some((t) => t.id === id)) refresh();
    });
    const un2 = listen<{ taskId: number }>("download:guard-required", (e) => {
      const id = e.payload.taskId;
      setTasks((prev) =>
        prev.map((t) => (t.id === id ? { ...t, waitingGuard: true } : t)),
      );
      // 验证码弹窗已全局化（GuardDialogs），这里只维护列表状态
      if (!tasksRef.current.some((t) => t.id === id)) refresh();
    });
    return () => {
      un.then((f) => f());
      un2.then((f) => f());
      unMobile.then((f) => f());
    };
  }, [refresh]);

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 flex items-center justify-between mb-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">{tr("下载")}</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">
            {tr("下载需在「设置 → 账号」登录 Steam 账号（需拥有 Wallpaper Engine）")}
          </p>
        </div>
        <div className="flex items-center gap-2">
          {idOpen ? (
            <span className="flex items-center gap-1.5 rounded-lg border border-[var(--accent-strong)]/50 bg-[var(--card)] px-2 py-1 shadow-sm">
              <input
                autoFocus
                value={idInput}
                onChange={(e) => setIdInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void enqueueById();
                  if (e.key === "Escape") setIdOpen(false);
                }}
                placeholder={tr("壁纸 ID（创意工坊数字 ID）")}
                className="w-52 bg-transparent text-[13px] outline-none placeholder:text-[var(--text-2)]"
              />
              <button
                className="btn btn-primary !py-0.5 text-[11.5px]"
                disabled={idBusy}
                onClick={() => void enqueueById()}
              >
                {idBusy ? "…" : tr("下载")}
              </button>
            </span>
          ) : (
            <button
              className="btn"
              title={tr("输入创意工坊壁纸 ID 直接下载（无需搜索，网络受限时也能用）")}
              onClick={() => {
                setIdInput("");
                setIdOpen(true);
              }}
            >
              {tr("壁纸ID下载")}
            </button>
          )}
          {tasks.some((t) => t.status === "done" || t.status === "failed") && (
            <button
              className="btn"
              onClick={() => setConfirmClear(true)}
            >
              {tr("清空已完成")}
            </button>
          )}
        </div>
      </div>

      {loading && (
        <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">{tr("加载中…")}</div>
      )}

      {!loading && tasks.length === 0 && (
        <div className="shrink-0 card mb-4">
          <EmptyState
            art="download"
            title={tr("暂无下载任务")}
            hint={tr("在工坊或详情页点击「下载」，任务会出现在这里")}
          />
        </div>
      )}

      {/* 任务列表 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        <div className="space-y-2.5">
          {tasks.map((t) => (
            <div key={t.id} className="card px-4 py-3">
              <div className="flex items-center gap-3">
                <div className="flex-1 min-w-0">
                  <div className="truncate text-[13.5px] font-medium">
                    {downloadDisplayTitle(t)}
                    {t.dependency && (
                      <span className="ml-1.5 inline-block translate-y-[-1px] rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                        {tr("依赖")}
                      </span>
                    )}
                  </div>
                  <div className="text-[11.5px] text-[var(--text-2)] mt-0.5">
                    {tr(DOWNLOAD_STATUS_LABELS[t.status])}
                    {t.waitingGuard && ` · ${tr("等待验证码")}`}
                    {t.status === "failed" && t.errorMsg && ` · ${trMsg(t.errorMsg)}`}
                  </div>
                  {mobileConfirm.has(t.id) && (
                    <div className="mt-1 flex items-center gap-1.5 text-[11.5px] font-medium text-amber-500">
                      <span className="relative flex h-1.5 w-1.5">
                        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-amber-500 opacity-75" />
                        <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-amber-500" />
                      </span>
                      {tr("请在手机 Steam App 中确认本次登录")}
                    </div>
                  )}
                </div>
                <div className="w-40">
                  <div className="h-1.5 rounded-full bg-white/10 overflow-hidden">
                    {/* progress < 0：后端（steamcmd）不输出进度且拿不到总大小，显示不确定态 */}
                    {t.status !== "done" && t.progress < 0 ? (
                      <div className="h-full w-1/3 rounded-full bg-[var(--accent-fill)] animate-indeterminate" />
                    ) : (
                      <div
                        className="h-full bg-[var(--accent-fill)] transition-all"
                        style={{ width: `${t.status === "done" ? 100 : Math.max(0, t.progress)}%` }}
                      />
                    )}
                  </div>
                  <div className="text-right text-[11px] text-[var(--text-2)] mt-1">
                    {t.status === "done"
                      ? "100%"
                      : t.progress < 0
                        ? t.itemId === "verify:login"
                          ? tr("验证中…")
                          : tr("下载中…")
                        : `${Math.round(t.progress)}%`}
                  </div>
                </div>
                <div className="flex gap-1.5">
                  {t.status === "failed" && (
                    <button className="btn !py-1 text-[11.5px]" onClick={() => api.downloadRetry(t.id).then(refresh)}>
                      {tr("重试")}
                    </button>
                  )}
                  {(t.status === "queued" ||
                    t.status === "authenticating" ||
                    t.status === "downloading" ||
                    t.status === "installing") && (
                    <button className="btn btn-danger !py-1 text-[11.5px]" onClick={() => api.downloadCancel(t.id).then(refresh)}>
                      {tr("取消")}
                    </button>
                  )}
                  {(t.status === "done" || t.status === "failed") && (
                    <button
                      className="btn !py-1 text-[11.5px]"
                      title={tr("从列表移除")}
                      onClick={async () => {
                        await api.downloadRemove(t.id).catch((e) => msg.error(String(e)));
                        refresh();
                      }}
                    >
                      {tr("移除")}
                    </button>
                  )}
                </div>
              </div>
            </div>
          ))}
        </div>
      </div>

      {confirmClear && (
        <ConfirmModal
          title={tr("清空已完成")}
          message={tr("将从下载列表移除所有已完成与失败的任务记录。已下载的壁纸文件不受影响。")}
          confirmText={tr("清空")}
          onCancel={() => setConfirmClear(false)}
          onConfirm={async () => {
            setConfirmClear(false);
            try {
              const n = await api.downloadClearFinished();
              msg.success(tr("已清空 {n} 条任务记录", { n }));
            } catch (e) {
              msg.error(String(e));
            }
            refresh();
          }}
        />
      )}

    </div>
  );
}
