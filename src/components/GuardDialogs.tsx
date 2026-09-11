// 全局下载交互弹窗：Steam Guard 验证码 + 手机 App 确认。
// 挂在 App 根部——steamcmd 的交互（下载任务 / 设置页的登录验证任务）可能在
// 任何页面触发，弹窗不依附下载页，避免「在设置页验证账号时弹窗开在看不见的下载页」。
//
// 手机确认弹窗的关闭语义（用户明确要求）：
//   - 不会几秒后自动消失——只在用户手动点「我知道了」，
//     或登录真的推进（开始下载/完成/失败）时才关闭。
import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { listen } from "@tauri-apps/api/event";
import { api, type DownloadTask } from "../api/steam";
import { tr } from "../lib/i18n";

/** 登录验证任务（非工坊条目）的显示名 */
export function downloadDisplayTitle(t: Pick<DownloadTask, "itemId" | "title">): string {
  return t.itemId === "verify:login" ? tr("账号登录验证") : t.title;
}

export function GuardDialogs() {
  const [guardTaskId, setGuardTaskId] = useState<number | null>(null);
  const [guardTitle, setGuardTitle] = useState("");
  const [code, setCode] = useState("");
  // taskId → 任务标题（手机确认只需提示无需输入，可多个并存，一次展示一个）
  const [mobileConfirm, setMobileConfirm] = useState<Map<number, string>>(new Map());

  const resolveTitle = useCallback(async (id: number): Promise<string> => {
    try {
      const list = await api.downloadList();
      const t = list.find((x) => x.id === id);
      if (t) return downloadDisplayTitle(t);
    } catch {
      /* 列表拉不到就用通用名 */
    }
    return tr("下载任务");
  }, []);

  useEffect(() => {
    const un1 = listen<{ taskId: number }>("download:guard-required", (e) => {
      const id = e.payload.taskId;
      setGuardTaskId(id);
      setCode("");
      void resolveTitle(id).then(setGuardTitle);
    });
    const un2 = listen<{ taskId: number }>("download:mobile-confirm", (e) => {
      const id = e.payload.taskId;
      void resolveTitle(id).then((title) =>
        setMobileConfirm((prev) => new Map(prev).set(id, title)),
      );
    });
    const unGuard = listen<{ taskId: number }>("download:guard-required", (e) => {
      // 改为索要验证码：手机确认提示退场，避免两个弹窗来回切换
      const id = e.payload.taskId;
      setMobileConfirm((prev) => {
        if (!prev.has(id)) return prev;
        const next = new Map(prev);
        next.delete(id);
        return next;
      });
    });
    const un3 = listen<{ taskId: number; status: string }>("download:progress", (e) => {
      const { taskId, status } = e.payload;
      // 关键：只在登录真的推进时才自动收起——状态在 authenticating 附近
      // 抖动不收起（曾出现提示 ~1s 就消失，用户来不及在手机上确认）
      const progressed =
        status === "downloading" || status === "installing" || status === "done" || status === "failed";
      if (!progressed) return;
      setMobileConfirm((prev) => {
        if (!prev.has(taskId)) return prev;
        const next = new Map(prev);
        next.delete(taskId);
        return next;
      });
      // 验证码弹窗在任务终结时收起（失败再输码无意义；成功则由后端继续）
      if (status === "done" || status === "failed") {
        setGuardTaskId((cur) => (cur === taskId ? null : cur));
      }
    });
    return () => {
      void un1.then((f) => f());
      void un2.then((f) => f());
      void unGuard.then((f) => f());
      void un3.then((f) => f());
    };
  }, [resolveTitle]);

  const submitGuard = useCallback(async () => {
    if (guardTaskId == null || !code.trim()) return;
    try {
      await api.downloadSubmitGuard(guardTaskId, code.trim());
    } finally {
      setGuardTaskId(null);
    }
  }, [guardTaskId, code]);

  const mobileEntry = mobileConfirm.entries().next().value as [number, string] | undefined;

  return createPortal(
    <>
      {/* Steam Guard 验证码弹窗 */}
      {guardTaskId != null && (
        <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/30">
          <div className="card w-80 p-5">
            <h3 className="text-[14.5px] font-semibold">{tr("Steam Guard 验证码")}</h3>
            <p className="mt-1.5 text-[12.5px] text-[var(--text-2)]">
              {tr("「{title}」需要验证码（已发送到邮箱/手机令牌）", { title: guardTitle })}
            </p>
            <input
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void submitGuard()}
              placeholder={tr("输入验证码")}
              className="mt-3 w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-2 text-[13.5px] outline-none focus:border-[var(--accent-strong)]"
            />
            <div className="mt-3 flex justify-end gap-2">
              <button className="btn" onClick={() => setGuardTaskId(null)}>
                {tr("取消")}
              </button>
              <button
                className="btn btn-primary"
                disabled={!code.trim()}
                onClick={() => void submitGuard()}
              >
                {tr("提交")}
              </button>
            </div>
          </div>
        </div>
      )}

      {/* 手机 App 确认弹窗：手动关闭或登录推进才消失 */}
      {mobileEntry && guardTaskId == null && (
        <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/30">
          <div className="card w-96 p-5">
            <h3 className="text-[14.5px] font-semibold">{tr("等待手机确认")}</h3>
            <p className="mt-1.5 text-[12.5px] leading-relaxed text-[var(--text-2)]">
              {tr(
                "「{title}」正在等待登录确认。请打开 Steam 手机 App，在「确认」页面批准本次登录，批准后会自动继续。若手机未收到确认请求，可能是网络较慢，请稍候或检查网络/代理。",
                { title: mobileEntry[1] },
              )}
            </p>
            <div className="mt-3 flex items-center gap-2 text-[12px] text-amber-500">
              <span className="relative flex h-1.5 w-1.5">
                <span className="absolute inline-flex h-full w-full animate-ping rounded-full bg-amber-500 opacity-75" />
                <span className="relative inline-flex h-1.5 w-1.5 rounded-full bg-amber-500" />
              </span>
              {tr("正在等待确认…")}
            </div>
            <div className="mt-4 flex justify-end">
              <button
                className="btn"
                onClick={() =>
                  setMobileConfirm((prev) => {
                    const next = new Map(prev);
                    next.delete(mobileEntry[0]);
                    return next;
                  })
                }
              >
                {tr("我知道了（后台继续等待）")}
              </button>
            </div>
          </div>
        </div>
      )}
    </>,
    document.body,
  );
}
