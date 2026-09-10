// 下载页：任务列表 / 进度 / Steam Guard 验证码输入 / 取消 / 重试（T2）
import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  DOWNLOAD_STATUS_LABELS,
  type DownloadTask,
} from "../api/steam";
import { ConfirmModal } from "../components/ConfirmModal";
import { EmptyState } from "../components/EmptyState";
import { useMessage } from "../components/Message";

export function DownloadsPage() {
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [guardTaskId, setGuardTaskId] = useState<number | null>(null);
  const [code, setCode] = useState("");
  const [confirmClear, setConfirmClear] = useState(false);
  const msg = useMessage();
  // 正在等待手机 Steam App 确认登录的任务（steamcmd 推手机确认时无需输码，只需提示）
  const [mobileConfirm, setMobileConfirm] = useState<Set<number>>(new Set());
  // 事件回调里要读最新任务列表，但不能把 tasks 放进 effect 依赖，
  // 否则每次进度更新都会拆装 IPC 监听器。
  const tasksRef = useRef<DownloadTask[]>([]);
  tasksRef.current = tasks;

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
      // 只记 id：任务本身可能是刚由依赖补拉入队、尚未出现在列表里的，
      // 此时按 id 存住，等 refresh 回来后弹窗自然能取到标题。
      setGuardTaskId(id);
      setCode("");
      if (!tasksRef.current.some((t) => t.id === id)) refresh();
    });
    return () => {
      un.then((f) => f());
      un2.then((f) => f());
      unMobile.then((f) => f());
    };
  }, [refresh]);

  const guardTask = tasks.find((t) => t.id === guardTaskId) ?? null;

  const submitGuard = async () => {
    if (guardTaskId == null) return;
    await api.downloadSubmitGuard(guardTaskId, code);
    setGuardTaskId(null);
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 flex items-center justify-between mb-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">下载</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">
            下载需在「设置 → 账号」登录 Steam 账号（需拥有 Wallpaper Engine）
          </p>
        </div>
        {tasks.some((t) => t.status === "done" || t.status === "failed") && (
          <button
            className="btn"
            onClick={() => setConfirmClear(true)}
          >
            清空已完成
          </button>
        )}
      </div>

      {loading && <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">加载中…</div>}

      {!loading && tasks.length === 0 && (
        <div className="shrink-0 card mb-4">
          <EmptyState
            art="download"
            title="暂无下载任务"
            hint="在工坊或详情页点击「下载」，任务会出现在这里"
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
                    {t.title}
                    {t.dependency && (
                      <span className="ml-1.5 inline-block translate-y-[-1px] rounded bg-amber-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                        依赖
                      </span>
                    )}
                  </div>
                  <div className="text-[11.5px] text-[var(--text-2)] mt-0.5">
                    {DOWNLOAD_STATUS_LABELS[t.status]}
                    {t.waitingGuard && " · 等待验证码"}
                    {t.status === "failed" && t.errorMsg && ` · ${t.errorMsg}`}
                  </div>
                  {mobileConfirm.has(t.id) && (
                    <div className="mt-1 flex items-center gap-1.5 text-[11.5px] font-medium text-amber-500">
                      <span className="relative flex h-1.5 w-1.5">
                        <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-amber-500 opacity-75" />
                        <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-amber-500" />
                      </span>
                      请在手机 Steam App 中确认本次登录
                    </div>
                  )}
                </div>
                <div className="w-40">
                  <div className="h-1.5 rounded-full bg-black/10 dark:bg-white/10 overflow-hidden">
                    {/* progress < 0：后端（steamcmd）不输出进度且拿不到总大小，显示不确定态 */}
                    {t.status !== "done" && t.progress < 0 ? (
                      <div className="h-full w-1/3 rounded-full bg-[var(--accent)] animate-indeterminate" />
                    ) : (
                      <div
                        className="h-full bg-[var(--accent)] transition-all"
                        style={{ width: `${t.status === "done" ? 100 : Math.max(0, t.progress)}%` }}
                      />
                    )}
                  </div>
                  <div className="text-right text-[11px] text-[var(--text-2)] mt-1">
                    {t.status === "done"
                      ? "100%"
                      : t.progress < 0
                        ? "下载中…"
                        : `${Math.round(t.progress)}%`}
                  </div>
                </div>
                <div className="flex gap-1.5">
                  {t.status === "failed" && (
                    <button className="btn !py-1 text-[11.5px]" onClick={() => api.downloadRetry(t.id).then(refresh)}>
                      重试
                    </button>
                  )}
                  {(t.status === "queued" ||
                    t.status === "authenticating" ||
                    t.status === "downloading" ||
                    t.status === "installing") && (
                    <button className="btn btn-danger !py-1 text-[11.5px]" onClick={() => api.downloadCancel(t.id).then(refresh)}>
                      取消
                    </button>
                  )}
                  {(t.status === "done" || t.status === "failed") && (
                    <button
                      className="btn !py-1 text-[11.5px]"
                      title="从列表移除"
                      onClick={async () => {
                        await api.downloadRemove(t.id).catch((e) => msg.error(String(e)));
                        refresh();
                      }}
                    >
                      移除
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
          title="清空已完成"
          message="将从下载列表移除所有已完成与失败的任务记录。已下载的壁纸文件不受影响。"
          confirmText="清空"
          onCancel={() => setConfirmClear(false)}
          onConfirm={async () => {
            setConfirmClear(false);
            try {
              const n = await api.downloadClearFinished();
              msg.success(`已清空 ${n} 条任务记录`);
            } catch (e) {
              msg.error(String(e));
            }
            refresh();
          }}
        />
      )}

      {/* Steam Guard 验证码弹窗 */}
      {guardTask && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/30">
          <div className="card w-80 p-5">
            <h3 className="text-[14.5px] font-semibold">Steam Guard 验证码</h3>
            <p className="text-[12.5px] text-[var(--text-2)] mt-1.5">
              下载 {guardTask.title} 需要验证码（已发送到邮箱/手机）
            </p>
            <input
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && submitGuard()}
              placeholder="输入验证码"
              className="mt-3 w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-2 text-[13.5px] outline-none focus:border-[var(--accent)]"
            />
            <div className="mt-3 flex justify-end gap-2">
              <button className="btn" onClick={() => setGuardTaskId(null)}>
                取消
              </button>
              <button className="btn btn-primary" onClick={submitGuard}>
                提交
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
