// T0.5 壁纸引擎原型验证面板：Canvas / 视频 / GIF / Web 上桌面
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { IconPause, IconPlay, IconStop } from "./icons";

type WallpaperConfig = {
  type: "canvas" | "video" | "gif" | "web";
  src?: string;
  fit?: string;
  muted?: boolean;
  loop?: boolean;
};

export function WallpaperTestPanel() {
  const [active, setActive] = useState(false);
  const [current, setCurrent] = useState<string>("—");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  const refresh = async () => {
    try {
      const info = await invoke<{ active: boolean; sessions: Record<string, WallpaperConfig> }>(
        "wallpaper_list_sessions",
      );
      setActive(info.active);
      const s = Object.values(info.sessions)[0];
      setCurrent(s ? `${s.type}${s.src ? " · " + s.src : ""}` : "—");
    } catch (e) {
      setMessage(String(e));
    }
  };

  useEffect(() => {
    refresh();
    const t = setInterval(refresh, 3000);
    return () => clearInterval(t);
  }, []);

  const apply = async (cfg: WallpaperConfig) => {
    setBusy(true);
    setMessage("");
    try {
      await invoke("wallpaper_apply", { config: cfg });
      setMessage(`已应用：${cfg.type}${cfg.src ? " · " + cfg.src : ""}`);
      await refresh();
    } catch (e) {
      setMessage(String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = (fn: string) => async () => {
    try {
      await invoke(fn);
      await refresh();
    } catch (e) {
      setMessage(String(e));
    }
  };

  return (
    <div className="card p-5">
      <div className="flex items-center justify-between mb-1">
        <h3 className="text-[15px] font-semibold">壁纸引擎原型（T0.5）</h3>
        <span
          className={`inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium ${
            active
              ? "bg-green-500/15 text-green-600 dark:text-green-400"
              : "bg-gray-500/15 text-[var(--text-2)]"
          }`}
        >
          <span
            className={`h-1.5 w-1.5 rounded-full ${active ? "bg-green-500" : "bg-gray-400"}`}
          />
          {active ? "桌面壁纸运行中" : "未运行"}
        </span>
      </div>
      <p className="text-[12.5px] text-[var(--text-2)] mb-4">
        当前：{current} —— 验证 kCGDesktopWindowLevel 透明窗口 + WKWebView 实时渲染
      </p>

      <div className="flex flex-wrap gap-2">
        <button className="btn" disabled={busy} onClick={() => apply({ type: "canvas" })}>
          Canvas 动画
        </button>
        <button
          className="btn"
          disabled={busy}
          onClick={() => apply({ type: "video", src: "/test-media/test.mp4" })}
        >
          视频壁纸
        </button>
        <button
          className="btn"
          disabled={busy}
          onClick={() => apply({ type: "gif", src: "/test-media/test.gif" })}
        >
          GIF 壁纸
        </button>
        <button
          className="btn"
          disabled={busy}
          onClick={() => apply({ type: "web", src: "/test-media/web-test/" })}
        >
          Web 壁纸
        </button>
      </div>

      <div className="flex flex-wrap gap-2 mt-3">
        <button className="btn" onClick={run("wallpaper_pause_all")}>
          <IconPause size={14} /> 暂停
        </button>
        <button className="btn" onClick={run("wallpaper_resume_all")}>
          <IconPlay size={14} /> 恢复
        </button>
        <button className="btn btn-danger" onClick={run("wallpaper_stop")}>
          <IconStop size={14} /> 停止（关闭桌面窗口）
        </button>
      </div>

      {message && (
        <p className="mt-3 text-[12.5px] text-[var(--text-2)] break-all">{message}</p>
      )}
    </div>
  );
}
