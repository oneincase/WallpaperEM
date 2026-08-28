// 本地库壁纸预览弹框（按类型渲染：视频/图片/网页/场景）
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api, type LibraryItem, type WallpaperConfig } from "../api/steam";

export function PreviewModal({
  item,
  onClose,
}: {
  item: LibraryItem;
  onClose: () => void;
}) {
  const [cfg, setCfg] = useState<WallpaperConfig | null>(null);
  const [err, setErr] = useState("");
  // 场景预览复用渲染器页：读取全局清晰度/帧率设置，与桌面壁纸观感一致
  const [renderDpr, setRenderDpr] = useState(1);
  const [sceneFps, setSceneFps] = useState(60);

  useEffect(() => {
    setErr("");
    setCfg(null);
    api
      .libraryPreview(item.itemId)
      .then(setCfg)
      .catch((e) => setErr(String(e)));
    invoke<string | null>("settings_get", { key: "wallpaper_render_dpr" })
      .then((v) => {
        const n = Number(v);
        if (Number.isFinite(n) && n > 0) setRenderDpr(n);
      })
      .catch(() => {});
    invoke<string | null>("settings_get", { key: "wallpaper_scene_fps" })
      .then((v) => {
        const n = Number(v);
        if (n === 30 || n === 60 || n === 120) setSceneFps(n);
      })
      .catch(() => {});
  }, [item.itemId]);

  const renderBody = () => {
    if (err) return <div className="text-[13px] text-red-500 px-4">{err}</div>;
    if (!cfg) return <div className="text-[13px] text-[var(--text-2)]">加载中…</div>;

    if (cfg.type === "video") {
      return (
        <video
          key={cfg.src}
          src={cfg.src}
          autoPlay
          loop
          muted
          controls
          playsInline
          className="max-h-full max-w-full"
        />
      );
    }
    if (cfg.type === "gif" || cfg.type === "image") {
      return <img src={cfg.src} alt={item.title} className="max-h-full max-w-full object-contain" />;
    }
    if (cfg.type === "web") {
      return (
        <iframe
          key={cfg.src}
          src={cfg.src}
          sandbox="allow-scripts allow-same-origin"
          title={item.title}
          className="h-full w-full"
        />
      );
    }
    if (cfg.type === "scene" && cfg.mediaBase) {
      // 复用渲染器页（与桌面壁纸同一渲染管线），注入全局清晰度/帧率
      const origin = new URL(cfg.mediaBase).origin;
      const q = new URLSearchParams({
        type: "scene",
        src: cfg.src ?? "",
        fit: "fill",
        mediaBase: cfg.mediaBase,
        renderDpr: String(renderDpr),
        sceneFps: String(sceneFps),
      });
      return (
        <iframe
          key={cfg.src}
          src={`${origin}/renderer/index.html?${q}`}
          title={item.title}
          className="h-full w-full"
        />
      );
    }
    return <div className="text-[13px] text-[var(--text-2)]">该类型暂不支持预览</div>;
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-8"
      onClick={onClose}
    >
      <div
        className="card flex h-[70vh] w-full max-w-4xl flex-col overflow-hidden"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-[var(--separator)] px-4 py-2.5">
          <div className="truncate text-[14px] font-semibold">{item.title}</div>
          <button className="btn !py-1" onClick={onClose}>
            关闭
          </button>
        </div>
        <div className="flex flex-1 items-center justify-center overflow-hidden bg-black/20">
          {renderBody()}
        </div>
      </div>
    </div>
  );
}
