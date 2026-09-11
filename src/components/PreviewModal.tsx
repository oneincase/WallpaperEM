// 本地库壁纸预览弹框
//
// 所有类型（scene / web / video / gif / image）统一复用渲染器页 ——
// 与桌面壁纸走完全同一条渲染管线（webwallgl 库），所以预览里看到的效果
// 就是应用后的效果。之前各类型分头用 <video>/<img>/<iframe> 直接渲染，
// 预览与实际观感会有差异（缺 fit 归一化、缺 WE shim 的属性与音频注入）。
import { useEffect, useRef, useState } from "react";
import { api, type LibraryItem, type WallpaperConfig } from "../api/steam";
import { tr, trMsg } from "../lib/i18n";

export function PreviewModal({
  item,
  onClose,
}: {
  item: LibraryItem;
  onClose: () => void;
}) {
  const [cfg, setCfg] = useState<WallpaperConfig | null>(null);
  const [err, setErr] = useState("");

  useEffect(() => {
    setErr("");
    setCfg(null);
    api
      .libraryPreview(item.itemId)
      .then(setCfg)
      .catch((e) => setErr(String(e)));
  }, [item.itemId]);

  // 关闭前先把 iframe 导航到 about:blank：WKWebView 对带活动文档的 iframe 回收
  // 迟缓（库的 web 卸载路径同样处理），仅从 DOM 移除会让整个预览渲染器 ——
  // WebGL 上下文、scene.pkg 解析缓存（几百 MB）、解码器 —— 在后台多活很久，
  // 表现为"关了预览内存不降"。导航触发文档立即拆毁，资源随之释放。
  const frameRef = useRef<HTMLIFrameElement | null>(null);
  useEffect(() => {
    return () => {
      const f = frameRef.current;
      if (f) {
        try {
          f.contentWindow?.location.replace("about:blank");
        } catch {
          /* 跨源时用 src 导航兜底（设置 src 属性跨源也允许） */
        }
        f.removeAttribute("src");
        f.src = "about:blank";
      }
    };
  }, []);

  const renderBody = () => {
    if (err) return <div className="text-[13px] text-red-500 px-4">{trMsg(err)}</div>;
    if (!cfg) return <div className="text-[13px] text-[var(--text-2)]">{tr("加载中…")}</div>;
    // mediaBase 由 Rust 的 resolve_item_config 对所有类型统一下发；
    // 缺它说明内容服务器没起来，渲染器页也拉不到资源
    if (!cfg.mediaBase) {
      return (
        <div className="text-[13px] text-[var(--text-2)]">{tr("内容服务器未就绪，无法预览")}</div>
      );
    }

    const origin = new URL(cfg.mediaBase).origin;
    const q = new URLSearchParams({
      type: cfg.type,
      src: cfg.src ?? "",
      // 预览固定写死低开销参数，不跟随全局设置：裁剪 + 省电清晰度 + 30fps。
      // 预览只为确认内容/试调属性，压低开销才能让主窗口与桌面壁纸保持流畅
      fit: "cover",
      mediaBase: cfg.mediaBase,
      renderDpr: "0.8",
      sceneFps: "30",
    });
    return (
      <iframe
        ref={frameRef}
        key={`${cfg.type}:${cfg.src}`}
        src={`${origin}/renderer/index.html?${q}`}
        title={item.title}
        className="h-full w-full"
      />
    );
  };

  return (
    <div
      className="animate-overlay fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-8"
      onClick={onClose}
    >
      <div
        className="card animate-modal-pop flex h-[70vh] w-full max-w-4xl flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-[var(--separator)] px-4 py-2.5">
          <div className="truncate text-[14px] font-semibold">{item.title}</div>
          <button className="btn !py-1" onClick={onClose}>
            {tr("关闭")}
          </button>
        </div>
        <div className="flex flex-1 items-center justify-center overflow-hidden bg-black/20">
          {renderBody()}
        </div>
      </div>
    </div>
  );
}
