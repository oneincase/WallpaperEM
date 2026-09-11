// 订阅同步弹窗：登录 Steam（网页会话）→ 列出账号全部已订阅壁纸 →
// 一键下载全部未订阅缺失项，或多选下载。
//
// 登录状态机（对应后端 subscriptions_list / _submit_code 的响应）：
//   loading（登录/拉取中）→ needCode（Steam Guard 验证码）
//   → pendingConfirmation（等手机 App 确认）→ ready（列表）
// 首次验证后 refresh token 已持久化，之后打开直接出列表（免密免码）。
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  api,
  TYPE_LABELS,
  type SubscriptionItem,
} from "../api/steam";
import { useMessage } from "./Message";
import { IconDownload } from "./icons";
import { QrImage } from "./QrImage";
import { VirtualGrid } from "./VirtualGrid";
import { tr, trMsg } from "../lib/i18n";

type Phase =
  | { kind: "loading"; note: string }
  | { kind: "needCode"; codeType: "device" | "email"; message: string }
  | { kind: "pendingConfirmation"; message: string }
  | { kind: "badCredentials"; message: string; username: string }
  /** 扫码登录中：expired=true 停止轮询显示「刷新二维码」覆盖层 */
  | { kind: "qr"; challengeUrl: string; expired: boolean; message?: string }
  | {
      kind: "ready";
      items: SubscriptionItem[];
      /** 订阅总数（0 = 页面没解析出来，退化为「本页不满 30 即到底」） */
      total: number;
      /** 已加载到的页码 */
      page: number;
      loadingMore: boolean;
      /** 加载下一页失败（保留已加载内容，可重试） */
      loadError?: string;
    }
  | { kind: "error"; message: string };

export function SubscriptionsModal({
  onClose,
  /** 有下载入队后回调（本地库/徽标需要刷新） */
  onChanged,
}: {
  onClose: () => void;
  onChanged: () => void;
}) {
  const msg = useMessage();
  const [phase, setPhase] = useState<Phase>({ kind: "loading", note: "正在登录 Steam…" });
  const [code, setCode] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [downloading, setDownloading] = useState(false);
  // badCredentials 阶段的重输表单
  const [credUser, setCredUser] = useState("");
  const [credPass, setCredPass] = useState("");
  const [credSaving, setCredSaving] = useState(false);

  const handleResp = useCallback(
    (r: Awaited<ReturnType<typeof api.subscriptionsPage>>) => {
      if (r.status === "ok")
        setPhase({
          kind: "ready",
          items: r.items,
          total: r.total,
          page: r.page,
          loadingMore: false,
        });
      else if (r.status === "needCode")
        setPhase({ kind: "needCode", codeType: r.codeType, message: r.message });
      else if (r.status === "badCredentials")
        setPhase({
          kind: "badCredentials",
          message: r.message,
          username: r.username ?? "",
        });
      else if (r.status === "pendingConfirmation")
        setPhase({ kind: "pendingConfirmation", message: r.message });
      // confirmed 由扫码/验证码的调用方直接处理（先给「登录成功」反馈再 start）
    },
    [],
  );

  const start = useCallback(async () => {
    setPhase({ kind: "loading", note: "正在登录 Steam 并读取订阅列表…" });
    try {
      handleResp(await api.subscriptionsPage(1));
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [handleResp]);

  /** 扫码/验证码确认成功：先给反馈再拉第一页（避免长加载期间界面毫无反应） */
  const onConfirmed = useCallback(() => {
    msg.success(tr("登录成功，正在获取订阅列表…"));
    void start();
  }, [msg, start]);

  /** 懒加载下一页：滚动接近底部时触发；失败保留已加载内容可重试 */
  const loadingMoreRef = useRef(false);
  const loadMore = useCallback(async () => {
    if (loadingMoreRef.current) return;
    const p = phase;
    if (p.kind !== "ready" || p.loadingMore) return;
    // total=0 表示页面没解析出总数：最后一页不足 30 条视为到底
    const hasMore = p.total > 0 ? p.items.length < p.total : p.items.length % 30 === 0;
    if (!hasMore) return;
    loadingMoreRef.current = true;
    setPhase({ ...p, loadingMore: true, loadError: undefined });
    try {
      const r = await api.subscriptionsPage(p.page + 1);
      if (r.status === "ok") {
        setPhase((cur) => {
          if (cur.kind !== "ready") return cur;
          // 按 id 去重（极端情况下翻页间订阅变动导致条目串页）
          const added = r.items.filter((i) => !cur.items.some((x) => x.id === i.id));
          return {
            kind: "ready",
            items: [...cur.items, ...added],
            // 这一页一条新的都没有 = 真的到底了：total 解析不出来时「本页不满 30
            // 即到底」的判据在整页都是重复项时会永远成立，滚动到底就会无限翻页
            // （每页都请求 + 重渲染，主线程直接被拖死）。这里把 total 收敛成
            // 「已加载数」，hasMore 随之变 false。
            total: added.length === 0 ? cur.items.length : r.total,
            page: r.page,
            loadingMore: false,
          };
        });
      } else {
        // 登录态失效等：不清空已加载内容，底部显示错误 + 重试
        const errText =
          r.status === "badCredentials" || r.status === "needCode" || r.status === "pendingConfirmation"
            ? "登录状态已变化，请关闭后重新打开同步"
            : "加载失败";
        setPhase((cur) =>
          cur.kind === "ready" ? { ...cur, loadingMore: false, loadError: errText } : cur,
        );
      }
    } catch (e) {
      setPhase((cur) =>
        cur.kind === "ready" ? { ...cur, loadingMore: false, loadError: String(e) } : cur,
      );
    } finally {
      loadingMoreRef.current = false;
    }
  }, [phase]);

  // 翻页触发由 VirtualGrid 的 onNearEnd 负责（滚到底部前 400px 回调一次），
  // loadMore 自己带 loadingMoreRef 去重，重复触发是安全的

  useEffect(() => {
    void start();
  }, [start]);

  /** 开启扫码登录：拿到 challengeUrl 进入 qr 阶段，由下面的 effect 链式轮询 */
  const startQr = useCallback(async () => {
    setPhase({ kind: "loading", note: "正在生成二维码…" });
    try {
      const r = await api.subscriptionsQrBegin();
      if (r.status === "confirmed") onConfirmed();
      else setPhase({ kind: "qr", challengeUrl: r.challengeUrl, expired: false });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [onConfirmed]);

  // 扫码轮询：链式调用后端（单次最多阻塞 ~8s），同一批二维码 120s 未确认标记过期。
  // 依赖 phase：二维码被服务端轮换（challengeUrl 变化）时整个计时/轮询重置；
  // challengeUrl 没变时 phase 对象不变，链式 setTimeout 自己接力
  const qrTicket = useRef(0);
  useEffect(() => {
    if (phase.kind !== "qr" || phase.expired) return;
    const ticket = ++qrTicket.current;
    const deadline = Date.now() + 120_000;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      if (qrTicket.current !== ticket) return;
      if (Date.now() >= deadline) {
        setPhase((p) => (p.kind === "qr" ? { ...p, expired: true } : p));
        return;
      }
      try {
        const r = await api.subscriptionsQrPoll();
        if (qrTicket.current !== ticket) return;
        if (r.status === "confirmed") {
          onConfirmed();
          return;
        }
        setPhase((p) =>
          p.kind === "qr" && p.challengeUrl !== r.challengeUrl
            ? { kind: "qr", challengeUrl: r.challengeUrl, expired: false }
            : p,
        );
        timer = setTimeout(() => void tick(), 1000);
      } catch (e) {
        if (qrTicket.current !== ticket) return;
        // 会话失效/网络错误：停轮询，让用户决定刷新还是换密码登录
        setPhase((p) => (p.kind === "qr" ? { ...p, expired: true, message: String(e) } : p));
      }
    };
    timer = setTimeout(() => void tick(), 800);
    return () => {
      qrTicket.current++;
      if (timer) clearTimeout(timer);
    };
  }, [phase, msg, onConfirmed]);

  const submitCode = useCallback(async () => {
    if (!code.trim()) return;
    setPhase({ kind: "loading", note: "正在验证…" });
    try {
      const r = await api.subscriptionsSubmitCode(code.trim());
      setCode("");
      if (r.status === "confirmed") onConfirmed();
      else handleResp(r);
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }, [code, handleResp, onConfirmed]);

  // Esc 关闭（下载中不关，避免误触）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !downloading) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, downloading]);

  const items = phase.kind === "ready" ? phase.items : [];
  const missing = useMemo(() => items.filter((i) => !i.downloaded), [items]);
  const selectedMissing = useMemo(
    () => missing.filter((i) => selected.has(i.id)),
    [missing, selected],
  );

  /** 批量入队下载：逐条容错（已在队列/失败不拖垮整批） */
  const enqueueMany = useCallback(
    async (targets: SubscriptionItem[]) => {
      if (!targets.length || downloading) return;
      setDownloading(true);
      let ok = 0;
      const failed: string[] = [];
      for (const t of targets) {
        try {
          await api.downloadEnqueue(t.id);
          ok++;
        } catch (e) {
          // 「已在下载队列中」不算失败，静默跳过
          if (!String(e).includes("已在下载队列")) failed.push(`${t.title}：${String(e)}`);
        }
      }
      setDownloading(false);
      if (failed.length) {
        msg.error(tr("入队 {ok} 个，{n} 个失败：{detail}", { ok, n: failed.length, detail: failed[0] }));
      } else if (ok) {
        msg.success(tr("已把 {n} 个壁纸加入下载队列", { n: ok }));
      } else {
        msg.info(tr("没有需要下载的壁纸（都已在队列中）"));
      }
      if (ok) {
        onChanged();
        // 本地把已入队的标成已下载视觉态（实际下载完成以本地库为准，
        // 这里只是防止用户重复点同步）
        setPhase((p) =>
          p.kind === "ready"
            ? {
                ...p,
                items: p.items.map((i) =>
                  targets.some((t) => t.id === i.id) ? { ...i, downloaded: true } : i,
                ),
              }
            : p,
        );
        setSelected(new Set());
      }
    },
    [downloading, msg, onChanged],
  );

  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const allMissingSelected = missing.length > 0 && selectedMissing.length === missing.length;

  return createPortal(
    <div
      className="animate-overlay fixed inset-0 z-[80] flex items-center justify-center bg-black/50 p-8"
      onClick={() => !downloading && onClose()}
    >
      <div
        className="card animate-modal-pop flex h-[78vh] w-full max-w-3xl flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        {/* 标题栏 */}
        <div className="flex shrink-0 items-center justify-between gap-3 border-b border-[var(--separator)] px-5 py-3">
          <span className="flex items-center gap-2 text-[14px] font-semibold">
            <IconDownload />
            {tr("订阅同步")}
          </span>
          <span className="truncate text-[11.5px] text-[var(--text-2)]">
            {tr("拉取登录账号在 Wallpaper Engine 工坊的全部订阅，批量下载缺失项")}
          </span>
          <button
            className="shrink-0 rounded-lg px-2 py-0.5 text-[18px] leading-none text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/10"
            onClick={() => !downloading && onClose()}
            aria-label={tr("关闭")}
          >
            ×
          </button>
        </div>

        {phase.kind === "loading" && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 text-[13px] text-[var(--text-2)]">
            <span className="h-5 w-5 animate-spin rounded-full border-2 border-[var(--separator)] border-t-[var(--accent-strong)]" />
            {trMsg(phase.note)}
          </div>
        )}

        {phase.kind === "error" && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8 text-center">
            <div className="text-[13px] leading-relaxed text-red-500">{trMsg(phase.message)}</div>
            <div className="flex gap-2">
              <button className="btn" onClick={() => void start()}>
                {tr("重试")}
              </button>
              <button className="btn" onClick={onClose}>
                {tr("关闭")}
              </button>
            </div>
          </div>
        )}

        {phase.kind === "needCode" && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8">
            <div className="text-[13px] text-[var(--text-1)]">{trMsg(phase.message)}</div>
            <div className="text-[11.5px] text-[var(--text-2)]">
              {tr("只需验证一次：之后同步订阅免密免验证码")}
            </div>
            <input
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && void submitCode()}
              placeholder={
                phase.codeType === "device" ? tr("手机令牌 5 位验证码") : tr("邮件验证码")
              }
              className="w-56 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-2 text-center text-[14px] tracking-widest outline-none focus:border-[var(--accent-strong)]"
            />
            <button className="btn btn-primary" disabled={!code.trim()} onClick={() => void submitCode()}>
              {tr("验证并继续")}
            </button>
            <button
              className="text-[12px] text-[var(--accent-strong)] hover:underline"
              onClick={() => void startQr()}
            >
              {tr("收不到验证码？改用扫码登录")}
            </button>
          </div>
        )}

        {phase.kind === "badCredentials" && (
          <BadCredentialsForm
            message={phase.message}
            initialUsername={phase.username}
            credUser={credUser}
            setCredUser={setCredUser}
            credPass={credPass}
            setCredPass={setCredPass}
            saving={credSaving}
            onSwitchQr={() => void startQr()}
            onSubmit={async () => {
              if (!credUser.trim() || !credPass) return;
              setCredSaving(true);
              try {
                // 新密码同时写回下载凭据（steamcmd 与网页会话共用一份账号密码）
                await api.downloadCredentialsSet(credUser.trim(), credPass);
                setCredPass("");
                await start();
              } catch (e) {
                setPhase({ kind: "error", message: String(e) });
              } finally {
                setCredSaving(false);
              }
            }}
          />
        )}

        {phase.kind === "pendingConfirmation" && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8 text-center">
            <div className="text-[13px] leading-relaxed text-[var(--text-1)]">
              {trMsg(phase.message)}
            </div>
            <button className="btn btn-primary" onClick={() => void start()}>
              {tr("继续")}
            </button>
          </div>
        )}

        {phase.kind === "qr" && (
          <div className="flex flex-1 flex-col items-center justify-center gap-3 px-8">
            <div className="relative">
              <QrImage text={phase.challengeUrl} size={190} />
              {phase.expired && (
                <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 rounded-xl bg-white/95">
                  <span className="text-[12px] text-neutral-600">{tr("二维码已过期")}</span>
                  <button
                    className="btn btn-primary !py-1 !text-[12px]"
                    onClick={() => void startQr()}
                  >
                    {tr("刷新二维码")}
                  </button>
                </div>
              )}
            </div>
            <div className="text-[13px] font-medium text-[var(--text-1)]">
              {tr("用 Steam 手机 App 扫码确认登录")}
            </div>
            <div className="max-w-sm text-center text-[11.5px] leading-relaxed text-[var(--text-2)]">
              {tr(
                "打开 Steam App →「扫码」（手机令牌页）→ 扫上面的二维码 → 确认登录。免输账号密码，也不受令牌验证码格式困扰；确认后自动读取订阅列表",
              )}
            </div>
            {phase.message && (
              <div className="max-w-sm text-center text-[11.5px] text-amber-500">
                {trMsg(phase.message)}
              </div>
            )}
            <button
              className="text-[12px] text-[var(--accent-strong)] hover:underline"
              onClick={() => void start()}
            >
              {tr("改用账号密码登录")}
            </button>
          </div>
        )}

        {phase.kind === "ready" && (
          <>
            {/* 工具条：统计 + 全选未下载 */}
            <div className="flex shrink-0 items-center gap-3 border-b border-[var(--separator)] px-5 py-2 text-[12px] text-[var(--text-2)]">
              <span>
                {phase.total > 0
                  ? tr("已加载 {n} / {total} 项订阅", { n: items.length, total: phase.total })
                  : tr("已加载 {n} 项订阅", { n: items.length })}
                {" · "}
                {tr("未下载")} {missing.length} {tr("项")}
              </span>
              {missing.length > 0 && (
                <button
                  className="ml-auto text-[var(--accent-strong)] hover:underline"
                  onClick={() =>
                    setSelected(allMissingSelected ? new Set() : new Set(missing.map((i) => i.id)))
                  }
                >
                  {allMissingSelected
                    ? tr("取消全选")
                    : tr("全选未下载（{n}）", { n: missing.length })}
                </button>
              )}
            </div>

            {items.length === 0 ? (
              <div className="flex flex-1 items-center justify-center text-[13px] text-[var(--text-2)]">
                {tr("该账号还没有订阅任何壁纸")}
              </div>
            ) : (
              /* 虚拟滚动：订阅数没有上限（实测能到几千项），整表渲染会卡 */
              <VirtualGrid
                className="flex-1 overflow-y-auto p-4"
                items={items}
                minColumnWidth={150}
                gap={12}
                keyOf={(it) => it.id}
                onNearEnd={() => void loadMore()}
                renderItem={(it) => {
                  const checked = selected.has(it.id);
                  return (
                    <div
                      className={`card group relative aspect-square overflow-hidden ${
                        it.downloaded ? "opacity-70" : "cursor-pointer"
                      }`}
                      onClick={() => !it.downloaded && toggle(it.id)}
                    >
                      {it.previewUrl ? (
                        <img
                          src={it.previewUrl}
                          alt={it.title}
                          loading="lazy"
                          className="h-full w-full object-cover"
                          draggable={false}
                        />
                      ) : (
                        <span className="flex h-full w-full items-center justify-center text-[12px] text-[var(--text-2)]">
                          {tr("无预览")}
                        </span>
                      )}
                      {/* 选中勾 */}
                      {!it.downloaded && (
                        <span
                          className={`absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-full border text-[11px] ${
                            checked
                              ? "border-[var(--accent-strong)] bg-[var(--accent)] text-[var(--accent-fg)]"
                              : "border-white/60 bg-black/35 text-transparent group-hover:border-white"
                          }`}
                        >
                          ✓
                        </span>
                      )}
                      {it.downloaded && (
                        <span className="absolute left-1.5 top-1.5 rounded bg-sky-500/90 px-1.5 py-0.5 text-[9px] font-semibold text-white">
                          {tr("已下载")}
                        </span>
                      )}
                      <div className="absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/75 to-transparent p-2 pt-6">
                        <div className="line-clamp-2 text-[11.5px] font-medium leading-snug text-white">
                          {it.title}
                        </div>
                        <div className="mt-0.5 text-[10px] text-white/70">
                          {tr(TYPE_LABELS[it.type] ?? "未知")}
                        </div>
                      </div>
                    </div>
                  );
                }}
                /* 状态行放在网格之后：滚到底部就能看到「加载中/失败重试/全部加载完」 */
                footer={
                  <div className="flex flex-col items-center gap-2 py-4">
                    {phase.loadingMore && (
                      <span className="flex items-center gap-2 text-[12px] text-[var(--text-2)]">
                        <span className="h-3.5 w-3.5 animate-spin rounded-full border-2 border-[var(--separator)] border-t-[var(--accent-strong)]" />
                        {tr("正在加载更多…")}
                      </span>
                    )}
                    {phase.loadError && !phase.loadingMore && (
                      <>
                        <span className="max-w-md text-center text-[12px] text-red-500">
                          {trMsg(phase.loadError)}
                        </span>
                        <button className="btn !py-1 !text-[12px]" onClick={() => void loadMore()}>
                          {tr("重试")}
                        </button>
                      </>
                    )}
                    {!phase.loadingMore &&
                      !phase.loadError &&
                      phase.total > 0 &&
                      items.length >= phase.total && (
                        <span className="text-[11.5px] text-[var(--text-2)]">
                          {tr("已加载全部 {n} 项订阅", { n: phase.total })}
                        </span>
                      )}
                  </div>
                }
              />
            )}

            {/* 底栏：多选下载 + 一键同步 */}
            <div className="flex shrink-0 items-center gap-2 border-t border-[var(--separator)] px-5 py-3">
              <span className="text-[12px] text-[var(--text-2)]">
                {selectedMissing.length > 0
                  ? tr("已选 {n} 项", { n: selectedMissing.length })
                  : tr("点选卡片可多选")}
              </span>
              <div className="ml-auto flex items-center gap-2">
                <button
                  className="btn !py-1 !text-[12px]"
                  disabled={selectedMissing.length === 0 || downloading}
                  onClick={() => void enqueueMany(selectedMissing)}
                >
                  {tr("下载选中（{n}）", { n: selectedMissing.length })}
                </button>
                <button
                  className="btn btn-primary !py-1 !text-[12px]"
                  disabled={missing.length === 0 || downloading}
                  onClick={() => void enqueueMany(missing)}
                >
                  {downloading
                    ? tr("正在入队…")
                    : phase.total > 0 && items.length < phase.total
                      ? tr("同步已加载的未下载（{n}）", { n: missing.length })
                      : tr("一键同步全部未下载（{n}）", { n: missing.length })}
                </button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>,
    document.body,
  );
}

/** 密码被 Steam 拒绝时的原地重输表单（免跳设置页） */
function BadCredentialsForm({
  message,
  initialUsername,
  credUser,
  setCredUser,
  credPass,
  setCredPass,
  saving,
  onSwitchQr,
  onSubmit,
}: {
  message: string;
  initialUsername: string;
  credUser: string;
  setCredUser: (v: string) => void;
  credPass: string;
  setCredPass: (v: string) => void;
  saving: boolean;
  onSwitchQr: () => void;
  onSubmit: () => Promise<void>;
}) {
  // 初始用户名只填一次（用户可能改过输入框再触发重渲染）
  const [filled, setFilled] = useState(false);
  useEffect(() => {
    if (!filled && initialUsername) {
      setCredUser(initialUsername);
      setFilled(true);
    }
  }, [filled, initialUsername, setCredUser]);

  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-3 px-10">
      <div className="text-[13px] font-medium text-red-500">{trMsg(message)}</div>
      <div className="max-w-md text-center text-[11.5px] leading-relaxed text-[var(--text-2)]">
        {tr(
          "注意：steamcmd 下载用的是它自己缓存的登录态，本地保存的密码即使已过期或有误也能照常下载 —— 所以「能下载」不代表存的密码是对的。账号名必须是 Steam 登录账号名（不是邮箱、不是昵称！Steam 客户端登录框接受邮箱，但这个登录接口不接受）。查看方式：Steam 客户端 → 右上角点你的昵称 →「账户明细」→ 页面顶部的「帐户名称」。",
        )}
      </div>
      <input
        value={credUser}
        onChange={(e) => setCredUser(e.target.value)}
        placeholder={tr("Steam 登录账号名（非邮箱/昵称）")}
        autoComplete="username"
        className="w-64 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-2 text-[13px] outline-none focus:border-[var(--accent-strong)]"
      />
      {credUser.includes("@") && (
        <div className="text-[11.5px] text-amber-500">
          {tr("⚠ 这看起来是邮箱地址 —— 该登录接口只接受 Steam 登录账号名")}
        </div>
      )}
      <input
        type="password"
        value={credPass}
        onChange={(e) => setCredPass(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && void onSubmit()}
        placeholder={tr("密码")}
        autoComplete="current-password"
        className="w-64 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-2 text-[13px] outline-none focus:border-[var(--accent-strong)]"
      />
      <button
        className="btn btn-primary"
        disabled={!credUser.trim() || !credPass || saving}
        onClick={() => void onSubmit()}
      >
        {saving ? tr("正在验证…") : tr("保存并重试")}
      </button>
      <div className="mt-1 flex flex-col items-center gap-1">
        <span className="text-[11px] text-[var(--text-2)]">{tr("开了手机令牌登不上？")}</span>
        <button className="btn !py-1 !text-[12px]" onClick={onSwitchQr}>
          📱 {tr("改用扫码登录（推荐，免账号密码）")}
        </button>
      </div>
    </div>
  );
}
