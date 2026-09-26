// 分享管理页：分享功能总开关 + 我的分享列表（复制链接 / 二维码 / 启停 / 删除）。
// 从「设置 → 网络与服务」迁出为一级导航页（docs/ui-glass-refactor-plan.md §4）；
// 分享的创建入口仍在库卡片与详情页的 ShareModal，本页只做管理。
import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api, type McpStatus, type ShareRecord } from "../api/steam";
import { tr } from "../lib/i18n";
import { QrModal } from "../components/QrModal";
import { Switch } from "../components/Switch";
import { useMessage } from "../components/Message";

export function SharesPage({
  onNavigate,
}: {
  /** 服务未开启时「去设置」的跳转 */
  onNavigate: (p: "settings") => void;
}) {
  const [shareOn, setShareOn] = useState(false);
  const [shares, setShares] = useState<ShareRecord[]>([]);
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [qr, setQr] = useState<ShareRecord | null>(null);
  const msg = useMessage();

  // 进页拉一次：分享列表无实时诉求，不做轮询
  const refresh = useCallback(() => {
    api.shareServiceEnabled().then(setShareOn).catch(() => {});
    api.shareList().then(setShares).catch(() => {});
    api.mcpStatus().then(setMcp).catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // 分享开关被托盘 / MCP 侧改动时跟上（同一份广播由 Rust notify_setting_changed 发出）
  useEffect(() => {
    const un = listen<{ key: string; value: string }>("settings-changed", (e) => {
      if (e.payload.key === "share.enabled") {
        setShareOn(e.payload.value === "1" || e.payload.value === "true");
      }
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  // 链接 base 与 ShareModal/copyShareLink 同源：局域网/任意模式优先 lanUrl，
  // 本机模式回落 127.0.0.1（只有本机打得开，弹层里说明）
  const base = mcp?.lanUrl ?? (mcp ? new URL(mcp.url).origin : "");
  const landingOf = (s: ShareRecord) => `${base}/share/${s.shareId}`;

  const copyText = async (text: string, okText: string) => {
    try {
      await navigator.clipboard.writeText(text);
      msg.success(okText);
    } catch {
      msg.error(tr("复制失败，请手动选中复制"));
    }
  };

  const toggleShareService = async (next: boolean) => {
    try {
      await api.shareSetServiceEnabled(next);
      setShareOn(next);
    } catch (e) {
      msg.error(String(e));
    }
  };

  const toggleShareEnabled = async (shareId: string, next: boolean) => {
    try {
      await api.shareSetEnabled(shareId, next);
      setShares((cur) => cur.map((s) => (s.shareId === shareId ? { ...s, enabled: next } : s)));
    } catch (e) {
      msg.error(String(e));
    }
  };

  const removeShare = async (shareId: string) => {
    try {
      await api.shareRemove(shareId);
      setShares((cur) => cur.filter((s) => s.shareId !== shareId));
      msg.success(tr("分享已删除"));
    } catch (e) {
      msg.error(String(e));
    }
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      <h1 className="text-[22px] font-bold tracking-tight">{tr("分享")}</h1>
      <p className="mt-0.5 text-[13px] text-[var(--text-2)]">
        {tr("把壁纸变成一个链接，任何浏览器打开都能渲染（访客渲染在自己的设备上，不影响你的桌面）")}
      </p>

      {mcp && !mcp.enabled && (
        <div className="mt-4 flex items-center justify-between gap-3 rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-2.5 text-[12.5px] text-amber-400">
          <span>{tr("网络服务未开启，分享链接暂时无法访问（设置 → 网络与服务）")}</span>
          <button className="btn !py-1 text-[11.5px]" onClick={() => onNavigate("settings")}>
            {tr("去设置")}
          </button>
        </div>
      )}

      <section className="card mt-4 overflow-hidden">
        <div className="flex items-center justify-between gap-4 border-b border-[var(--separator)] px-5 py-3.5">
          <div>
            <div className="text-[14px] font-medium">{tr("分享功能")}</div>
            <div className="mt-0.5 text-[12px] text-[var(--text-2)]">
              {shareOn
                ? tr(
                    "开启：库卡片与详情页的「分享」可生成链接，浏览器打开即可渲染壁纸。shareId 即访问凭据，仅限看这一张壁纸",
                  )
                : tr("关闭（默认）：不对外提供分享页。要分享壁纸时再打开，减少暴露面")}
            </div>
          </div>
          <Switch checked={shareOn} onChange={(v) => void toggleShareService(v)} />
        </div>

        {shareOn && shares.length > 0 && (
          <div className="space-y-1.5 p-3">
            {shares.map((s) => (
              <div
                key={s.shareId}
                className="flex items-center gap-2 rounded-lg border border-[var(--separator)] px-3 py-2"
              >
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[12.5px]">{s.title || s.itemId}</div>
                  <div className="truncate text-[11.5px] text-[var(--text-2)]">
                    /share/{s.shareId} ·{" "}
                    {s.expiresAt
                      ? tr("到 {time} 过期", {
                          time: new Date(s.expiresAt * 1000).toLocaleString(),
                        })
                      : tr("永久")}
                    {` · ${tr("浏览 {n}", { n: s.views })}`}
                  </div>
                </div>
                <button
                  className="btn !py-1 text-[11.5px]"
                  onClick={() => void copyText(landingOf(s), tr("已复制分享页链接"))}
                >
                  {tr("复制链接")}
                </button>
                <button className="btn !py-1 text-[11.5px]" onClick={() => setQr(s)}>
                  {tr("二维码")}
                </button>
                <Switch
                  checked={s.enabled}
                  onChange={(v) => void toggleShareEnabled(s.shareId, v)}
                />
                <button
                  className="btn !py-1 text-[11.5px] hover:!border-red-500/40 hover:!text-red-500"
                  onClick={() => void removeShare(s.shareId)}
                >
                  {tr("删除")}
                </button>
              </div>
            ))}
          </div>
        )}
        {shareOn && shares.length === 0 && (
          <div className="px-5 py-4 text-[12px] text-[var(--text-2)]">
            {tr("还没有分享。在库卡片或详情页点「分享」即可创建（永久或限时）")}
          </div>
        )}
        {!shareOn && shares.length > 0 && (
          <div className="px-5 py-4 text-[12px] text-[var(--text-2)]">
            {tr("分享功能已关闭：列表保留，打开上方开关即可恢复访问")}
          </div>
        )}
      </section>

      {qr && (
        <QrModal
          title={qr.title || qr.itemId}
          text={landingOf(qr)}
          hint={
            mcp?.lanUrl
              ? undefined
              : tr("当前为本机模式：只有这台电脑打得开；切到局域网/任意模式后手机才能扫码访问")
          }
          copyOkText={tr("已复制分享页链接")}
          onClose={() => setQr(null)}
        />
      )}
    </div>
  );
}
