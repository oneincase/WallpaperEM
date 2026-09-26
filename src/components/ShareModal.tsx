// 壁纸分享弹窗：选有效期 → 创建 → 出链接/iframe/二维码。
// 链接的 host 取服务当前生效地址（局域网模式 = lanUrl，本机模式 = 127.0.0.1），
// 访客打开时服务必须开着且分享未过期。

import { useEffect, useState } from "react";
import { api, type ShareRecord } from "../api/steam";
import { tr } from "../lib/i18n";
import { useMessage } from "./Message";
import { QrImage } from "./QrImage";

/** 有效期选项在组件内构造（tr 随当前语言即时取词，放模块级会定格在启动语言） */
function expiryOptions(): { label: string; value: number | null }[] {
  return [
    { label: tr("永久"), value: null },
    { label: tr("1 小时"), value: 3600 },
    { label: tr("1 天"), value: 86400 },
    { label: tr("7 天"), value: 604800 },
  ];
}

export function ShareModal({
  itemId,
  title,
  wtype,
  onClose,
}: {
  itemId: string;
  title: string;
  /** 壁纸类型（web 类型分享前要提示访客浏览器会运行其网页代码） */
  wtype?: string;
  onClose: () => void;
}) {
  const msg = useMessage();
  const [expiresInSec, setExpiresInSec] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<ShareRecord | null>(null);
  const [base, setBase] = useState("");

  useEffect(() => {
    void api.mcpStatus().then((st) => {
      // 分享链接用服务当前对外地址：局域网/任意模式优先 lanUrl（含 QR 场景），
      // 本机模式回落 127.0.0.1:port —— 只有本机浏览器打得开，弹窗里说明
      setBase(st.lanUrl ?? new URL(st.url).origin);
    });
  }, [itemId]);

  const landing = created ? `${base}/share/${created.shareId}` : "";
  const direct = created ? `${base}/share/${created.shareId}/render` : "";
  const iframe = created
    ? `<iframe src="${direct}" style="width:100%;aspect-ratio:16/9;border:0;border-radius:8px" allow="fullscreen; autoplay"></iframe>`
    : "";

  const create = async () => {
    setBusy(true);
    try {
      setCreated(await api.shareCreate(itemId, expiresInSec));
    } catch (e) {
      msg.error(String(e));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (text: string, ok: string) => {
    try {
      await navigator.clipboard.writeText(text);
      msg.success(ok);
    } catch {
      msg.error(tr("复制失败，请手动选中复制"));
    }
  };

  return (
    <div
      className="fixed inset-0 z-50 grid place-items-center bg-black/25 p-4"
      onClick={onClose}
    >
      <div
        className="glass-panel w-[520px] max-w-full rounded-2xl border border-[var(--card-border)] p-5 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <div className="text-[15px] font-semibold">{tr("分享壁纸")}</div>
          <button className="btn !px-2 !py-1 text-[12px]" onClick={onClose}>
            {tr("关闭")}
          </button>
        </div>

        {!created ? (
          <>
            <div className="mb-1 text-[13px] font-medium">{title}</div>
            <div className="mb-4 text-[12px] text-[var(--text-2)]">
              {tr(
                "创建后得到一个链接，任何浏览器打开都能渲染这张壁纸（访客渲染在自己的设备上，不影响你的桌面）",
              )}
            </div>
            {wtype === "web" && (
              <div className="mb-3 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[12px] text-amber-400">
                {tr(
                  "网页类型壁纸会直接在访客浏览器里运行其网页代码（与 Wallpaper Engine 官方分享行为一致），请确认来源可信",
                )}
              </div>
            )}
            <div className="mb-1 text-[12.5px] font-medium">{tr("有效期")}</div>
            <div className="mb-4 flex flex-wrap gap-2">
              {expiryOptions().map((o) => (
                <button
                  key={o.label}
                  className={`rounded-lg border px-3 py-1.5 text-[12.5px] ${
                    expiresInSec === o.value
                      ? "border-[var(--accent-strong)] bg-[var(--accent)] text-[var(--accent-fg)]"
                      : "border-[var(--separator)] text-[var(--text-2)] hover:border-[var(--accent-strong)]"
                  }`}
                  onClick={() => setExpiresInSec(o.value)}
                >
                  {o.label}
                </button>
              ))}
            </div>
            <button className="btn w-full justify-center" disabled={busy} onClick={() => void create()}>
              {busy ? "…" : tr("创建分享链接")}
            </button>
            <div className="mt-3 text-[11.5px] text-[var(--text-2)]">
              {tr("创建后可在「分享」页中启停或删除；分享功能需保持网络服务开启")}
            </div>
          </>
        ) : (
          <>
            <div className="mb-3 space-y-2">
              <CopyRow label={tr("分享页")} text={landing} onCopy={copy} />
              <CopyRow label={tr("直链")} text={direct} onCopy={copy} />
              <CopyRow label="iframe" text={iframe} onCopy={copy} mono />
            </div>
            <div className="flex items-start gap-4">
              <QrImage text={landing} size={140} />
              <div className="text-[12px] leading-relaxed text-[var(--text-2)]">
                <p>{tr("扫码打开分享页")}</p>
                <p className="mt-1">
                  {tr(
                    "过期时间：{time}",
                    { time: created.expiresAt ? new Date(created.expiresAt * 1000).toLocaleString() : tr("永久") },
                  )}
                </p>
                <p className="mt-1">{tr("本机模式下只有这台电脑打得开；局域网/任意模式下同网设备（或公网）可访问")}</p>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function CopyRow({
  label,
  text,
  onCopy,
  mono,
}: {
  label: string;
  text: string;
  onCopy: (t: string, ok: string) => void;
  mono?: boolean;
}) {
  return (
    <div className="flex items-center gap-2">
      <span className="w-14 shrink-0 text-[12px] text-[var(--text-2)]">{label}</span>
      <code
        className={`flex-1 truncate rounded-lg border border-[var(--separator)] bg-white/5 px-2 py-1.5 text-[11.5px] ${
          mono ? "font-mono" : ""
        }`}
        title={text}
      >
        {text}
      </code>
      <button
        className="btn !py-1 text-[11.5px]"
        onClick={() => void onCopy(text, tr("已复制"))}
      >
        {tr("复制")}
      </button>
    </div>
  );
}
