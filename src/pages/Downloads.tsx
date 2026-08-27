// 下载页：任务列表 / 进度 / Steam Guard 验证码输入 / 取消 / 重试（T2）
import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  DOWNLOAD_STATUS_LABELS,
  type DownloadTask,
} from "../api/steam";

export function DownloadsPage() {
  const [tasks, setTasks] = useState<DownloadTask[]>([]);
  const [loading, setLoading] = useState(true);
  const [guardTask, setGuardTask] = useState<DownloadTask | null>(null);
  const [code, setCode] = useState("");

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
        setTasks((prev) =>
          prev.map((t) =>
            t.id === p.taskId
              ? { ...t, status: p.status as DownloadTask["status"], progress: p.progress }
              : t,
          ),
        );
      },
    );
    const un2 = listen<{ taskId: number }>("download:guard-required", (e) => {
      setTasks((prev) =>
        prev.map((t) =>
          t.id === e.payload.taskId ? { ...t, waitingGuard: true } : t,
        ),
      );
      setGuardTask(tasks.find((t) => t.id === e.payload.taskId) ?? null);
      setCode("");
    });
    return () => {
      un.then((f) => f());
      un2.then((f) => f());
    };
  }, [refresh, tasks]);

  const submitGuard = async () => {
    if (!guardTask) return;
    await api.downloadSubmitGuard(guardTask.id, code);
    setGuardTask(null);
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 flex items-center justify-between mb-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">下载</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">
            下载需设置里配置正确的steam账号和密码
          </p>
        </div>
        {tasks.some((t) => t.status === "done" || t.status === "failed") && (
          <button
            className="btn"
            onClick={async () => {
              await api.downloadClearFinished();
              refresh();
            }}
          >
            清空已完成
          </button>
        )}
      </div>

      {loading && <div className="shrink-0 text-[13px] text-[var(--text-2)] mb-4">加载中…</div>}

      {!loading && tasks.length === 0 && (
        <div className="shrink-0 card p-12 text-center text-[13px] text-[var(--text-2)] mb-4">
          暂无下载任务 —— 在工坊详情页点击「下载」入队
        </div>
      )}

      {/* 任务列表 - 可滚动区域 */}
      <div className="flex-1 overflow-y-auto">
        <div className="space-y-2.5">
          {tasks.map((t) => (
            <div key={t.id} className="card px-4 py-3">
              <div className="flex items-center gap-3">
                <div className="flex-1 min-w-0">
                  <div className="truncate text-[13.5px] font-medium">{t.title}</div>
                  <div className="text-[11.5px] text-[var(--text-2)] mt-0.5">
                    {DOWNLOAD_STATUS_LABELS[t.status]}
                    {t.waitingGuard && " · 等待验证码"}
                    {t.status === "failed" && t.errorMsg && ` · ${t.errorMsg}`}
                  </div>
                </div>
                <div className="w-40">
                  <div className="h-1.5 rounded-full bg-black/10 dark:bg-white/10 overflow-hidden">
                    <div
                      className="h-full bg-[var(--accent)] transition-all"
                      style={{ width: `${t.status === "done" ? 100 : t.progress}%` }}
                    />
                  </div>
                  <div className="text-right text-[11px] text-[var(--text-2)] mt-1">
                    {t.status === "done" ? "100%" : `${Math.round(t.progress)}%`}
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
                        await api.downloadRemove(t.id).catch((e) => alert(String(e)));
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
              <button className="btn" onClick={() => setGuardTask(null)}>
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
