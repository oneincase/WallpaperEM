import { useEffect, useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  api,
  type CacheStats,
  type DownloadToolStatus,
  type FfmpegInstallProgress,
  type FfmpegStatus,
  type McpConfigSnippet,
  type McpStatus,
  type SteamcmdInstallProgress,
  type UpdateInfo,
} from "../api/steam";
import { getSidebarAlpha, setSidebarAlpha } from "../lib/sidebar";
import { formatBytes } from "../lib/format";
import { clearSnapshotCaches } from "../lib/cache-snapshots";
import { LOCALES, setLocale, tr, trMsg, useLocale, type Locale } from "../lib/i18n";
import { ConfirmModal } from "../components/ConfirmModal";
import { useMessage } from "../components/Message";

// 设置页标签：账号 / 通用 / AI / MCP / 网络 / 关于
// （id 沿用 "download"：该页内容是下载工具安装 + 下载账号登录，改 id 无收益）
type SettingsTab = "download" | "general" | "performance" | "mcp" | "network" | "about";
const SETTINGS_TABS: { id: SettingsTab; label: string }[] = [
  { id: "download", label: "账号" },
  { id: "general", label: "通用" },
  { id: "performance", label: "性能" },
  { id: "mcp", label: "AI / MCP" },
  { id: "network", label: "网络" },
  { id: "about", label: "关于" },
];

export function SettingsPage() {
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [interactive, setInteractive] = useState(false);
  const [autoSystemStatic, setAutoSystemStatic] = useState(true);
  const [autoPause, setAutoPause] = useState(false);
  const [audioProcessing, setAudioProcessing] = useState(false);
  const [audioMsg, setAudioMsg] = useState("");
  // 全局壁纸显示模式（cover/contain/stretch），默认 cover 等比铺满裁切
  const [fit, setFit] = useState<"cover" | "contain" | "stretch">("cover");
  const [fitMsg, setFitMsg] = useState("");
  // 全局清晰度（相对设备像素比的倍率）。四档：0 自动（=设备 DPR）/
  // 0.75 省电 / 0.85 标准 / 1 高清（=原生），默认 0 自动（与 Rust
  // DEFAULT_RENDER_DPR 一致）。renderer 会乘 devicePixelRatio 换算成绝对 DPR。
  const [renderDpr, setRenderDpr] = useState<number>(0);
  const [renderDprMsg, setRenderDprMsg] = useState("");
  // 全局场景帧率上限（15/24/30/45/60/120，越低 GPU 占用越低），默认 24
  const [sceneFps, setSceneFps] = useState<number>(24);
  const [sceneFpsMsg, setSceneFpsMsg] = useState("");
  // 渲染质量档位（库 1.3.23+）：抗锯齿 off 默认 / fxaa / msaa2 / msaa4
  const [aa, setAa] = useState<string>("off");
  const [aaMsg, setAaMsg] = useState("");
  // 粒子质量档 high 默认 / medium / low / off
  const [particles, setParticles] = useState<string>("high");
  const [particlesMsg, setParticlesMsg] = useState("");
  // 后处理质量档 high 默认 / medium / low / off
  const [post, setPost] = useState<string>("high");
  const [postMsg, setPostMsg] = useState("");
  // 壁纸语言（只影响壁纸的 language 属性，不是软件本体语言）。默认英文
  const [language, setLanguage] = useState<string>("english");
  // 界面语言（i18n）：这里只是订阅，取词走模块级 tr()；订阅是为了本页文案跟着变
  const locale = useLocale();
  const [sidebarAlpha, setSidebarAlphaState] = useState<number>(getSidebarAlpha);
  // 下载账号（steamcmd 只支持账号密码登录）
  const [cred, setCred] = useState<{ configured: boolean; username?: string } | null>(null);
  const [editingCred, setEditingCred] = useState(false);
  const [tool, setTool] = useState<DownloadToolStatus | null>(null);
  // steamcmd 安装
  const [installing, setInstalling] = useState(false);
  const [confirmLogout, setConfirmLogout] = useState(false);
  const msg = useMessage();
  const [installMsg, setInstallMsg] = useState("");
  const [dlUser, setDlUser] = useState("");
  const [dlPass, setDlPass] = useState("");
  const [credMsg, setCredMsg] = useState("");
  // 代理
  const [proxy, setProxy] = useState("");
  const [proxyMsg, setProxyMsg] = useState("");
  const [followSystemProxy, setFollowSystemProxy] = useState(true);
  // 当前激活的设置标签页（状态都在本组件顶层，切换标签不丢失）
  const [tab, setTab] = useState<SettingsTab>("download");
  // 运行平台（macos/linux/windows）：平台相关文案与不可用功能的提示
  const [os, setOs] = useState<string>("macos");
  // 平台判断：文案里平台分支很多（权限、抽帧、代理、自启），散着写 os === "..." 容易漏改
  const isMac = os === "macos";
  const isWin = os === "windows";
  // 系统音频捕获平台支持性（Linux 暂未支持，开关给出明确提示而不是报权限错误）
  const [audioSupported, setAudioSupported] = useState(true);
  // 抽帧组件（ffmpeg）：Linux/Windows 上视频/GIF 抽首帧要用它，可应用内一键安装
  const [fm, setFm] = useState<FfmpegStatus | null>(null);
  const [fmBusy, setFmBusy] = useState(false);
  const [fmMsg, setFmMsg] = useState("");
  // MCP 服务（AI agent 接入）：开关/端口/令牌 + 最近调用
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [mcpSnippet, setMcpSnippet] = useState<McpConfigSnippet | null>(null);
  const [mcpPort, setMcpPort] = useState("");
  const [mcpBusy, setMcpBusy] = useState(false);
  const [mcpShowToken, setMcpShowToken] = useState(false);
  const [mcpShowSnippet, setMcpShowSnippet] = useState(false);
  // 缓存占用（预览图/网页缓存 + 壁纸首帧封面），进入设置页时统计一次
  const [cache, setCache] = useState<CacheStats | null>(null);
  const [cacheMsg, setCacheMsg] = useState("");
  const [clearingCache, setClearingCache] = useState(false);
  const [confirmClearCache, setConfirmClearCache] = useState(false);

  useEffect(() => {
    invoke<boolean>("autostart_status").then(setAutostart).catch(console.warn);
    invoke<{ os: string }>("app_info")
      .then((i) => setOs(i.os))
      .catch(console.warn);
    api
      .wallpaperAudioProcessingStatus()
      .then((s) => setAudioSupported(s.supported))
      .catch(console.warn);
    api.ffmpegStatus().then(setFm).catch(console.warn);
    invoke<string | null>("settings_get", { key: "wallpaper_interactive" })
      .then((v) => setInteractive(v === "true" || v === "1"))
      .catch(() => { });
    // 默认关闭：键未写入过视为 false
    invoke<string | null>("settings_get", { key: "wallpaper_auto_pause" })
      .then((v) => setAutoPause(v === "true" || v === "1"))
      .catch(() => { });
    // 默认开启：键未写入过视为 true
    invoke<string | null>("settings_get", { key: "wallpaper_auto_system_static" })
      .then((v) => setAutoSystemStatic(v == null || v === "true" || v === "1"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_fit" })
      .then((v) => setFit((v as "cover" | "contain" | "stretch") || "cover"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_render_dpr" })
      // 未设置/非法值回落到 0（自动）
      .then((v) => {
        const n = Number(v);
        setRenderDpr(Number.isFinite(n) && n >= 0 ? n : 0);
      })
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_scene_fps" })
      .then((v) => setSceneFps(Number(v) || 24))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_aa" })
      .then((v) => setAa(v || "off"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_particles" })
      .then((v) => setParticles(v || "high"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_post" })
      .then((v) => setPost(v || "high"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "language" })
      .then((v) => v && setLanguage(v))
      .catch(() => { });
    api
      .wallpaperAudioProcessingStatus()
      .then((s) => setAudioProcessing(s.enabled))
      .catch(() => { });
    api
      .downloadCredentialsStatus()
      .then((c) => {
        setCred(c);
        setDlUser(c.username ?? "");
      })
      .catch(console.warn);
    api.downloadToolStatus().then(setTool).catch(console.warn);
    invoke<string | null>("settings_get", { key: "follow_system_proxy" })
      .then((v) => setFollowSystemProxy(v == null || v === "true" || v === "1"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "download_proxy" })
      .then((p) => setProxy(p ?? ""))
      .catch(() => { });
  }, []);

  // ---- 缓存（预览图/网页缓存 + 壁纸首帧封面）----
  // 统计要递归遍历目录，几百 MB 时是毫秒级但不是零成本，所以只在进设置页
  // 与清除后各跑一次，不做定时轮询
  const loadCache = useCallback(
    () =>
      api
        .cacheStats()
        .then(setCache)
        .catch((e) => setCacheMsg(String(e))),
    [],
  );
  useEffect(() => {
    void loadCache();
  }, [loadCache]);

  /** 清缓存：磁盘上的交后端删，浏览器侧的内容快照在这里清 */
  const clearCache = useCallback(async () => {
    setClearingCache(true);
    setCacheMsg("");
    try {
      const freed = await api.cacheClear();
      clearSnapshotCaches();
      await loadCache();
      setCacheMsg(
        freed > 0 ? tr("已清除 {size}", { size: formatBytes(freed) }) : tr("缓存已经是空的"),
      );
    } catch (e) {
      setCacheMsg(String(e));
    } finally {
      setClearingCache(false);
    }
  }, [loadCache]);

  // 「保存凭据」时的双验证状态：steamcmd（后台任务）+ 网页会话
  const [credSaving, setCredSaving] = useState(false);
  type WebVerify =
    | { state: "checking" }
    | { state: "ok" }
    | { state: "needCode"; codeType: "device" | "email"; message: string }
    | { state: "pending"; message: string }
    | { state: "error"; message: string };
  const [webVerify, setWebVerify] = useState<WebVerify | null>(null);
  const [steamVerifyQueued, setSteamVerifyQueued] = useState(false);
  const [webCode, setWebCode] = useState("");
  const [webCodeBusy, setWebCodeBusy] = useState(false);

  const runWebVerify = useCallback(async () => {
    setWebVerify({ state: "checking" });
    try {
      const r = await api.accountWebLoginStart();
      if (r.status === "ok") setWebVerify({ state: "ok" });
      else if (r.status === "needCode")
        setWebVerify({ state: "needCode", codeType: r.codeType, message: r.message });
      else if (r.status === "pendingConfirmation")
        setWebVerify({ state: "pending", message: r.message });
      else setWebVerify({ state: "error", message: r.message });
    } catch (e) {
      setWebVerify({ state: "error", message: String(e) });
    }
  }, []);

  const saveCredentials = async () => {
    setCredMsg("");
    if (!dlUser || !dlPass) {
      setCredMsg(tr("请填写用户名与密码"));
      return;
    }
    setCredSaving(true);
    setWebVerify(null);
    setSteamVerifyQueued(false);
    try {
      await api.downloadCredentialsSet(dlUser, dlPass);
      setCred({ configured: true, username: dlUser });
      setDlPass("");
      setCredMsg(tr("✅ 已保存，正在验证两条登录通道…"));
      // 双验证：① steamcmd 登录验证任务（验证码/手机确认走全局弹窗，
      // 验证通过后下载免密免验证码）② 网页会话（订阅同步用）
      api.downloadVerifyLogin().catch(console.warn);
      setSteamVerifyQueued(true);
      await runWebVerify();
      setCredMsg(tr("✅ 已保存"));
    } catch (e) {
      setCredMsg(String(e));
    } finally {
      setCredSaving(false);
    }
  };

  const submitWebCode = async () => {
    if (!webCode.trim() || webCodeBusy) return;
    setWebCodeBusy(true);
    try {
      const r = await api.subscriptionsSubmitCode(webCode.trim());
      setWebCode("");
      if (r.status === "confirmed") setWebVerify({ state: "ok" });
      else if (r.status === "needCode")
        setWebVerify({ state: "needCode", codeType: r.codeType, message: r.message });
      else if (r.status === "pendingConfirmation")
        setWebVerify({ state: "pending", message: r.message });
      else if (r.status === "badCredentials")
        setWebVerify({ state: "error", message: r.message });
      else setWebVerify({ state: "error", message: tr("验证返回了未知状态，请重试") });
    } catch (e) {
      setWebVerify({ state: "error", message: String(e) });
    } finally {
      setWebCodeBusy(false);
    }
  };

  const logout = async () => {
    try {
      await api.downloadCredentialsClear();
      setCred({ configured: false });
      setDlUser("");
      setDlPass("");
      setCredMsg("");
      msg.success(tr("已登出，账号与登录态已清除"));
    } catch (e) {
      msg.error(String(e));
    }
  };

  // 安装 / 修复 steamcmd
  const installSteamcmd = async (force = false) => {
    setInstalling(true);
    setInstallMsg(tr("准备安装…"));
    try {
      await api.steamcmdInstall(force);
      setTool(await api.downloadToolStatus());
      setInstallMsg(tr("✅ steamcmd 已就绪"));
    } catch (e) {
      const raw = String(e);
      // 后端用 "CODE|中文说明" 传递可识别的失败原因
      setInstallMsg(raw.includes("|") ? raw.slice(raw.indexOf("|") + 1) : raw);
    } finally {
      setInstalling(false);
    }
  };

  useEffect(() => {
    const unInstall = listen<SteamcmdInstallProgress>("steamcmd:install-progress", (e) => {
      const { phase, message } = e.payload;
      const label =
        phase === "download" ? tr("下载中") : phase === "extract" ? tr("解压中") : tr("初始化中");
      setInstallMsg(`${label}：${trMsg(message)}`);
    });
    return () => {
      unInstall.then((f) => f());
    };
  }, []);

  // 抽帧组件（ffmpeg）安装 / 卸载
  const installFfmpeg = async (force = false) => {
    setFmBusy(true);
    setFmMsg(tr("准备安装…"));
    try {
      await api.ffmpegInstall(force);
      setFm(await api.ffmpegStatus());
      setFmMsg(tr("✅ 抽帧组件已就绪"));
    } catch (e) {
      setFmMsg(String(e));
    } finally {
      setFmBusy(false);
    }
  };

  const uninstallFfmpeg = async () => {
    setFmBusy(true);
    setFmMsg("");
    try {
      await api.ffmpegUninstall();
      setFm(await api.ffmpegStatus());
      setFmMsg(tr("已卸载（系统 ffmpeg 不受影响）"));
    } catch (e) {
      setFmMsg(String(e));
    } finally {
      setFmBusy(false);
    }
  };

  useEffect(() => {
    const unInstall = listen<FfmpegInstallProgress>("ffmpeg:install-progress", (e) => {
      const { phase, message } = e.payload;
      setFmMsg(
        `${phase === "download" ? tr("下载中") : tr("解压中")}: ${trMsg(message)}`,
      );
    });
    return () => {
      unInstall.then((f) => f());
    };
  }, []);

  const saveProxy = async () => {
    setProxyMsg("");
    try {
      await invoke("settings_set", {
        key: "follow_system_proxy",
        value: followSystemProxy ? "true" : "false",
      });
      await invoke("settings_set", { key: "download_proxy", value: proxy.trim() });
      setProxyMsg(tr("✅ 已保存（工坊访问重启应用后生效）"));
    } catch (e) {
      setProxyMsg(String(e));
    }
  };

  const toggleFollowSystemProxy = async () => {
    const next = !followSystemProxy;
    try {
      await invoke("settings_set", {
        key: "follow_system_proxy",
        value: next ? "true" : "false",
      });
      setFollowSystemProxy(next);
    } catch {
      // 失败则不变
    }
  };

  // 网络探测
  const [probe, setProbe] = useState<{ results: { host: string; label: string; ok: boolean; ms: number }[]; hint: string } | null>(null);
  const [probing, setProbing] = useState(false);
  const runProbe = async () => {
    setProbing(true);
    setProbe(null);
    try {
      setProbe(await api.networkProbe());
    } catch (e) {
      setProbe({ results: [], hint: String(e) });
    } finally {
      setProbing(false);
    }
  };

  // 诊断包
  const [diagMsg, setDiagMsg] = useState("");
  const doDiagnostics = async () => {
    setDiagMsg("");
    try {
      const p = await api.diagnosticsExport();
      setDiagMsg(tr("✅ 诊断包已导出：{path}", { path: p }));
    } catch (e) {
      setDiagMsg(String(e));
    }
  };

  const toggleAutostart = async () => {
    if (autostart === null) return;
    const next = !autostart;
    const ok = await invoke<boolean>("autostart_set", { enabled: next }).catch(() => null);
    if (ok !== null) setAutostart(ok);
  };

  // 自动设置系统壁纸：应用动态壁纸后抽首帧/代表帧设为系统静态壁纸
  const toggleAutoSystemStatic = async () => {
    const next = !autoSystemStatic;
    try {
      await invoke("settings_set", {
        key: "wallpaper_auto_system_static",
        value: next ? "true" : "false",
      });
      setAutoSystemStatic(next);
    } catch {
      // 失败则不变
    }
  };

  // 自动暂停：切到非桌面应用自动暂停壁纸，切回桌面自动播放（默认关）
  const toggleAutoPause = async () => {
    const next = !autoPause;
    try {
      await invoke("settings_set", {
        key: "wallpaper_auto_pause",
        value: next ? "true" : "false",
      });
      setAutoPause(next);
    } catch {
      // 失败则不变
    }
  };

  const toggleInteractive = async () => {
    const next = !interactive;
    try {
      await api.wallpaperInteractiveSet(next);
      setInteractive(next);
    } catch {
      // 失败则不变
    }
  };

  // 音频可视化（系统音频处理）：macOS 需屏幕录制授权（走系统音频 tap），
  // Windows 走 WASAPI loopback（无需任何授权），Linux 待接入 PipeWire
  const toggleAudioProcessing = async () => {
    const next = !audioProcessing;
    setAudioMsg("");
    if (next && !audioSupported) {
      setAudioMsg(
        tr(
          "当前平台暂不支持系统音频捕获（macOS CoreAudio / Windows WASAPI 已支持；Linux 待后续版本接入 PipeWire）",
        ),
      );
      return;
    }
    try {
      const s = await api.wallpaperAudioProcessingSet(next);
      setAudioProcessing(s.enabled);
      if (s.enabled && !s.granted) {
        setAudioMsg(
          isMac
            ? tr(
                "⚠️ 权限尚未生效：① 系统设置 → 隐私与安全性 → 屏幕录制 → 允许 WallpaperEM；② 完全退出应用（⌘Q）再重新打开（运行中的进程不会自动获得新授权）；③ 若列表里已开启但重启后仍无效，先在列表中选中 WallpaperEM 按「−」移除，再重新添加并允许",
              )
            : tr("⚠️ 权限尚未生效：请在系统设置中允许本应用捕获系统音频，然后完全退出应用再重新打开"),
        );
      } else if (s.enabled && s.running) {
        setAudioMsg(tr("✅ 系统音频分析已开启"));
      }
    } catch (e) {
      setAudioMsg(String(e));
    }
  };


  // 侧边栏透明度：设置即生效 + 持久化
  const changeSidebarAlpha = (v: number) => {
    const next = setSidebarAlpha(v);
    setSidebarAlphaState(next);
  };

  const changeFit = async (next: "cover" | "contain" | "stretch") => {
    setFitMsg("");
    try {
      await api.wallpaperSetFit(next);
      setFit(next);
    } catch (e) {
      setFitMsg(String(e));
    }
  };

  const changeRenderDpr = async (next: number) => {
    setRenderDprMsg("");
    try {
      await api.wallpaperSetRenderDpr(next);
      setRenderDpr(next);
    } catch (e) {
      setRenderDprMsg(String(e));
    }
  };

  const changeSceneFps = async (next: number) => {
    setSceneFpsMsg("");
    try {
      await api.wallpaperSetSceneFps(next);
      setSceneFps(next);
    } catch (e) {
      setSceneFpsMsg(String(e));
    }
  };

  const changeAa = async (next: string) => {
    setAaMsg("");
    try {
      await api.wallpaperSetAa(next);
      setAa(next);
    } catch (e) {
      setAaMsg(String(e));
    }
  };

  const changeParticles = async (next: string) => {
    setParticlesMsg("");
    try {
      await api.wallpaperSetParticles(next);
      setParticles(next);
    } catch (e) {
      setParticlesMsg(String(e));
    }
  };

  const changePost = async (next: string) => {
    setPostMsg("");
    try {
      await api.wallpaperSetPost(next);
      setPost(next);
    } catch (e) {
      setPostMsg(String(e));
    }
  };

  // 语言在挂载时才注入（壁纸 project.json 没有自带 language 属性时生效），
  // 改完需要重新应用壁纸；不像清晰度/帧率能实时热切
  const changeLanguage = async (next: string) => {
    const prev = language;
    setLanguage(next); // 乐观更新
    try {
      await api.wallpaperSetLanguage(next);
    } catch (e) {
      setLanguage(prev);
      msg.error(tr("语言设置失败：{err}", { err: trMsg(String(e)) }));
    }
  };

  // ---- MCP 服务 ----

  const refreshMcp = useCallback(async () => {
    try {
      const st = await api.mcpStatus();
      setMcp(st);
      // 端口输入框没被用户改过时跟随服务端（改过就保留草稿）
      setMcpPort((cur) => (cur === "" ? String(st.port) : cur));
      setMcpSnippet(await api.mcpConfigSnippet());
    } catch (e) {
      console.warn("mcp_status 失败", e);
    }
  }, []);

  // 只在「AI / MCP」标签页里轮询：最近调用列表需要刷新，其余时间不必打扰后端
  useEffect(() => {
    if (tab !== "mcp") return;
    void refreshMcp();
    const t = setInterval(() => void refreshMcp(), 4000);
    return () => clearInterval(t);
  }, [tab, refreshMcp]);

  const applyMcp = async (fn: () => Promise<McpStatus>, okText: string) => {
    setMcpBusy(true);
    try {
      const st = await fn();
      setMcp(st);
      setMcpPort(String(st.port));
      setMcpSnippet(await api.mcpConfigSnippet());
      if (st.lastError) {
        msg.error(trMsg(st.lastError));
      } else {
        msg.success(okText);
      }
    } catch (e) {
      msg.error(String(e));
    } finally {
      setMcpBusy(false);
    }
  };

  const toggleMcp = (next: boolean) => {
    void applyMcp(() => api.mcpSetEnabled(next), next ? tr("MCP 服务已启用") : tr("MCP 服务已关闭"));
  };

  const commitMcpPort = () => {
    const port = Number(mcpPort.trim());
    if (!Number.isInteger(port) || port <= 1024 || port > 65535) {
      msg.error(tr("端口需是 1025–65535 之间的整数"));
      setMcpPort(String(mcp?.port ?? ""));
      return;
    }
    if (mcp && port === mcp.port) return;
    void applyMcp(() => api.mcpSetPort(port), tr("端口已改为 {port}", { port }));
  };

  const rotateMcpToken = () => {
    void applyMcp(() => api.mcpRotateToken(), tr("令牌已轮换，客户端需重新配置"));
  };

  const copyText = async (text: string, okText: string) => {
    try {
      await navigator.clipboard.writeText(text);
      msg.success(okText);
    } catch {
      msg.error(tr("复制失败，请手动选中复制"));
    }
  };

  /** 状态：未启动 / 运行中 / 启动失败 / 启动中。
   *  这里存**中文原文**（不 tr）：下面的分支靠它做相等判断，一旦 tr 成译文，
   *  英文环境下四个分支全不命中，整行会永远显示「启动中 / Starting…」 */
  const mcpState = !mcp?.enabled
    ? "未启动"
    : mcp.running
      ? "运行中"
      : mcp.lastError
        ? "启动失败"
        : "启动中";

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部 + 标签栏：与内容列同宽、整体居中 */}
      <div className="shrink-0 w-full">
        <div className="flex items-center justify-between gap-4 flex-wrap">
          <div>
            <h1 className="text-[22px] font-bold tracking-tight">{tr("设置")}</h1>
            <p className="text-[13px] text-[var(--text-2)] mt-0.5">
              {tr("按标签切换设置分类")}
            </p>
          </div>
        </div>
      </div>
      <div className="w-full mt-5 inline-flex gap-0.5 p-0.5 items-center justify-center">
        {SETTINGS_TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`rounded-[7px] px-4 py-[5px] text-[13px] font-medium transition-colors ${tab === t.id
              ? "bg-[var(--accent)] text-[var(--accent-fg)] shadow-sm"
              : "text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/8"
              }`}
          >
            {tr(t.label)}
          </button>
        ))}
      </div>
      {/* 内容区域：整列水平居中（不贴最左边）；内容不足一屏时垂直居中，超出则滚动 */}
      <div className="min-h-0 overflow-y-auto flex flex-col">
        <div className="w-full p-5  mx-auto my-auto space-y-5">
          {tab === "download" && (
            <Group title={tr("下载账号")}>
              <Row
                label={tr("下载工具")}
                desc={
                  tool?.rosettaMissing
                    ? tr(
                        "缺少 Rosetta 2：steamcmd 的官方引导程序是 x86_64，首次启动需要它。请在终端执行 softwareupdate --install-rosetta --agree-to-license 后重试（首次自更新后 steamcmd 即以原生 arm64 运行）",
                      )
                    : tool?.installed
                      ? `${tr("steamcmd 已就绪")}${tool.version ? ` · ${tr("版本")} ${tool.version}` : ""}${tool.path ? ` · ${tool.path}` : ""}`
                      : tool?.downloaded
                        ? tr("已下载但未完成初始化，请点击「修复」重试")
                        : tr(
                            "Valve 官方 steamcmd。尚未安装，点击「安装」从官方源下载（约 2.5 MB 引导包，初始化后约 85 MB）",
                          )
                }
                control={
                  <div className="flex items-center gap-2">
                    <span
                      className={`text-[12px] font-medium ${tool?.installed ? "text-green-500" : "text-red-500"}`}
                    >
                      {tool?.installed ? tr("就绪") : tr("未就绪")}
                    </span>
                    <button
                      className="btn !py-1 text-[11.5px]"
                      disabled={installing || tool?.rosettaMissing}
                      onClick={() => installSteamcmd(!!tool?.installed)}
                    >
                      {installing ? tr("安装中…") : tool?.installed ? tr("修复") : tr("安装")}
                    </button>
                  </div>
                }
              />
              {installMsg && (
                <div className="mt-1.5 break-all text-[12px] text-[var(--text-2)]">{installMsg}</div>
              )}

              <Row
                label={tr("下载账号")}
                desc={
                  cred?.configured
                    ? tr("已登录：{user}（需拥有 Wallpaper Engine，下载不再重复验证）", {
                        user: cred.username ?? "",
                      })
                    : tr(
                        "下载工坊内容需拥有 WE 的 Steam 账号。首次下载会要求输入 Steam Guard 验证码，之后记住登录态。注意：steamcmd 登录会挤掉你正在运行的 Steam 客户端（同账号同时只能登录一处）",
                      )
                }
                control={
                  <div className="flex items-center gap-2 flex-wrap">
                    <span
                      className={`text-[12px] font-medium ${cred?.configured ? "text-green-500" : "text-[var(--text-2)]"}`}
                    >
                      {cred?.configured ? tr("已登录") : tr("未登录")}
                    </span>
                    {cred?.configured && !editingCred && (
                      <button
                        className="btn !py-1 text-[11.5px]"
                        onClick={() => {
                          setEditingCred(true);
                          setDlUser(cred.username ?? "");
                          setDlPass("");
                          setCredMsg("");
                        }}
                      >
                        {tr("重新配置")}
                      </button>
                    )}
                    {cred?.configured && (
                      <button
                        className="btn btn-danger !py-1 text-[11.5px]"
                        onClick={() => setConfirmLogout(true)}
                      >
                        {tr("登出")}
                      </button>
                    )}
                  </div>
                }
              />
              {(!cred?.configured || editingCred) && (
                <div className="mt-3 rounded-xl border border-[var(--separator)] p-3 space-y-2">
                  <input
                    value={dlUser}
                    onChange={(e) => setDlUser(e.target.value)}
                    placeholder={tr("Steam 账号（登录名或邮箱）")}
                    className="w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
                  />
                  <input
                    type="password"
                    value={dlPass}
                    onChange={(e) => setDlPass(e.target.value)}
                    placeholder={tr("密码")}
                    className="w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
                  />
                  <div className="flex items-center gap-2">
                    <button
                      className="btn btn-primary"
                      disabled={credSaving}
                      onClick={saveCredentials}
                    >
                      {credSaving ? tr("保存并验证中…") : tr("保存凭据并验证")}
                    </button>
                    {editingCred && (
                      <button
                        className="btn"
                        onClick={() => {
                          setEditingCred(false);
                          setDlUser(cred?.username ?? "");
                          setDlPass("");
                          setCredMsg("");
                        }}
                      >
                        {tr("取消")}
                      </button>
                    )}
                    {credMsg && <span className="text-[12px] text-[var(--text-2)]">{credMsg}</span>}
                  </div>
                  <p className="text-[11.5px] text-[var(--text-2)]">
                    {tr(
                      "密码本地加密存储。保存后会立即验证两条登录通道（steamcmd 下载 + 订阅同步网页会话），验证通过后续使用免密免验证码。",
                    )}
                  </p>

                  {/* 双验证状态 */}
                  {(steamVerifyQueued || webVerify) && (
                    <div className="space-y-1.5 rounded-lg bg-[var(--content)] p-2.5 text-[12px]">
                      <div className="flex items-center gap-2">
                        <span className="text-[var(--text-2)]">
                          {tr("① steamcmd 下载通道：")}
                        </span>
                        <span className="text-amber-500">{tr("验证任务已入队")}</span>
                      </div>
                      <p className="text-[11px] text-[var(--text-2)]">
                        {tr(
                          "若需要 Steam Guard 验证码或手机确认，会弹出全局窗口提示（也可到「下载」页查看进度）",
                        )}
                      </p>
                      <div className="flex items-center gap-2">
                        <span className="text-[var(--text-2)]">
                          {tr("② 订阅同步网页会话：")}
                        </span>
                        {webVerify?.state === "checking" && (
                          <span className="text-[var(--text-2)]">{tr("正在验证…")}</span>
                        )}
                        {webVerify?.state === "ok" && (
                          <span className="text-green-500">{tr("✅ 已建立，后续免密")}</span>
                        )}
                        {webVerify?.state === "error" && (
                          <span className="text-red-500">{trMsg(webVerify.message)}</span>
                        )}
                        {webVerify?.state === "pending" && (
                          <span className="text-amber-500">{trMsg(webVerify.message)}</span>
                        )}
                        {webVerify?.state === "needCode" && (
                          <span className="text-amber-500">{tr("需要验证码")}</span>
                        )}
                      </div>
                      {webVerify?.state === "needCode" && (
                        <div className="flex items-center gap-2 pt-1">
                          <input
                            value={webCode}
                            onChange={(e) => setWebCode(e.target.value)}
                            onKeyDown={(e) => e.key === "Enter" && void submitWebCode()}
                            placeholder={
                              webVerify.codeType === "device"
                                ? tr("手机令牌验证码")
                                : tr("邮箱验证码")
                            }
                            className="w-40 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1 text-[12px] outline-none focus:border-[var(--accent-strong)]"
                          />
                          <button
                            className="btn btn-primary !py-1 !text-[12px]"
                            disabled={!webCode.trim() || webCodeBusy}
                            onClick={() => void submitWebCode()}
                          >
                            {webCodeBusy ? tr("验证中…") : tr("验证")}
                          </button>
                          <span className="text-[11px] text-[var(--text-2)]">
                            {trMsg(webVerify.message)}
                          </span>
                        </div>
                      )}
                      {webVerify?.state === "pending" && (
                        <div className="pt-1">
                          <button
                            className="btn !py-1 !text-[12px]"
                            onClick={() => void runWebVerify()}
                          >
                            {tr("我已在手机上确认，继续")}
                          </button>
                        </div>
                      )}
                      {webVerify?.state === "error" && (
                        <div className="pt-1">
                          <button
                            className="btn !py-1 !text-[12px]"
                            onClick={() => void runWebVerify()}
                          >
                            {tr("重试网页验证")}
                          </button>
                        </div>
                      )}
                    </div>
                  )}
                </div>
              )}
            </Group>
          )}

          {tab === "network" && (
            <>
              <Group title={tr("代理")}>
                <Row
                  label={tr("跟随系统代理")}
                  desc={
                    followSystemProxy
                      ? isMac
                        ? tr(
                            "开启：自动使用 macOS「系统设置 → 网络 → 代理」中的配置访问创意工坊（默认开启）",
                          )
                        : tr("开启：自动使用系统代理环境变量中的配置访问创意工坊（默认开启）")
                      : tr("关闭：绕过系统代理，直连网络访问创意工坊")
                  }
                  control={<Switch checked={followSystemProxy} onChange={toggleFollowSystemProxy} />}
                />
                <Row
                  label={tr("手动代理")}
                  desc={tr(
                    "可选，优先级高于系统代理；如 http://127.0.0.1:7890（大陆网络访问 Steam 建议配置）；工坊访问重启应用后生效",
                  )}
                  control={null}
                />
                <div className="mt-1 flex items-center gap-2">
                  <input
                    value={proxy}
                    onChange={(e) => setProxy(e.target.value)}
                    placeholder="http://127.0.0.1:7890"
                    className="flex-1 rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
                  />
                  <button className="btn" onClick={saveProxy}>
                    {tr("保存")}
                  </button>
                  {proxyMsg && <span className="text-[12px] text-[var(--text-2)]">{proxyMsg}</span>}
                </div>
              </Group>

              <Group title={tr("网络与诊断")}>
                <Row
                  label={tr("Steam 网络连通性")}
                  desc={tr("逐主机 TLS 探测（社区 / API / CDN / 内容服务器）")}
                  control={
                    <button className="btn" onClick={runProbe} disabled={probing}>
                      {probing ? tr("探测中…") : tr("开始探测")}
                    </button>
                  }
                />
                {probe && (
                  <div className="mt-2 space-y-1">
                    {probe.results.map((r) => (
                      <div key={r.host} className="flex items-center gap-2 text-[12.5px]">
                        <span className={`h-2 w-2 rounded-full ${r.ok ? "bg-green-500" : "bg-red-500"}`} />
                        <span className="w-64 truncate">{trMsg(r.label)}</span>
                        <span className="text-[var(--text-2)]">
                          {r.ok ? `${r.ms}ms` : tr("不通")}
                        </span>
                      </div>
                    ))}
                    <div className="text-[12px] text-[var(--text-2)] pt-1">
                      {trMsg(probe.hint)}
                    </div>
                  </div>
                )}
                <Row
                  label={tr("导出诊断包")}
                  desc={tr("日志 + 数据库结构 + 环境信息 + 网络探测 → zip（排障用）")}
                  control={
                    <button className="btn" onClick={doDiagnostics}>
                      {tr("导出")}
                    </button>
                  }
                />
                {diagMsg && <div className="mt-1 text-[12px] text-[var(--text-2)] break-all">{diagMsg}</div>}
              </Group>
            </>
          )}

          {tab === "general" && (
            <Group title={tr("通用")}>
              <Row
                label={tr("开机自启")}
                desc={
                  isMac
                    ? tr("登录 macOS 时自动启动本应用")
                    : isWin
                      ? tr("登录 Windows 时自动启动本应用")
                      : tr("登录系统时自动启动本应用")
                }
                control={<Switch checked={autostart === true} onChange={toggleAutostart} />}
              />
              <Row
                label={tr("界面语言")}
                desc={tr(
                  "软件界面语言，切换后立即生效。托盘菜单等系统级文案由后端绘制，会在下次打开菜单时跟随",
                )}
                control={
                  <select
                    value={locale}
                    onChange={(e) => setLocale(e.target.value as Locale)}
                    className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                  >
                    {LOCALES.map((l) => (
                      <option key={l.value} value={l.value}>
                        {l.label}
                      </option>
                    ))}
                  </select>
                }
              />
              <Row
                label={tr("壁纸语言")}
                desc={tr(
                  "壁纸的语言偏好（与界面语言无关）。壁纸自带语言设置时以壁纸为准；仅对没有该设置的壁纸生效，改动需重新应用壁纸",
                )}
                control={
                  <select
                    value={language}
                    onChange={(e) => void changeLanguage(e.target.value)}
                    className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                  >
                    <option value="simplifiedchinese">简体中文</option>
                    <option value="traditionalchinese">繁體中文</option>
                    <option value="english">English</option>
                    <option value="japanese">日本語</option>
                    <option value="korean">한국어</option>
                    <option value="german">Deutsch</option>
                  </select>
                }
              />
              <Row
                label={tr("音频可视化（系统声音）")}
                desc={
                  audioSupported
                    ? tr(
                        "开启后壁纸可响应整个系统的声音（如音乐软件），与 WE 桌面端一致；macOS 需授予屏幕录制权限，Windows 无需授权。壁纸自带的音乐无需此开关也会可视化",
                      )
                    : tr("当前平台暂不支持系统音频捕获（Linux 待接入 PipeWire）；壁纸自带的音乐无需此开关也会可视化")
                }
                control={<Switch checked={audioProcessing} onChange={toggleAudioProcessing} />}
              />
              {audioMsg && (
                <div className="text-[12px] text-[var(--text-2)]">{trMsg(audioMsg)}</div>
              )}
              <Row
                label={tr("自动设置系统壁纸")}
                desc={
                  isMac
                    ? tr(
                        "应用动态壁纸后，自动抽首帧（scene/web 用工坊预览图）设为 macOS 静态壁纸：锁屏、登录窗口与壁纸引擎未运行时保持视觉一致",
                      )
                    : isWin
                      ? tr(
                          "应用动态壁纸后，自动抽首帧（scene/web 用工坊预览图）设为 Windows 静态壁纸：锁屏与壁纸引擎未运行时保持视觉一致（视频/GIF 需要下方「抽帧组件」）",
                        )
                      : tr(
                          "应用动态壁纸后，自动抽首帧设为系统静态壁纸：壁纸引擎未运行时保持视觉一致（视频/GIF 需要下方「抽帧组件」，桌面环境接口决定能否设置）",
                        )
                }
                control={<Switch checked={autoSystemStatic} onChange={toggleAutoSystemStatic} />}
              />
              {/* 抽帧组件：只在需要 ffmpeg 的平台出现（macOS 走 AVFoundation，隐藏） */}
              {fm?.supported && (
                <>
                  <Row
                    label={tr("抽帧组件（ffmpeg）")}
                    desc={
                      fm.managed
                        ? `${tr("已安装托管副本")}${fm.version ? ` · ${fm.version}` : ""} · ${tr("约")} ${formatBytes(fm.sizeBytes)}${fm.dir ? ` · ${fm.dir}` : ""}`
                        : fm.system
                          ? `${tr("检测到系统 ffmpeg")}${fm.version ? `（${fm.version}）` : ""}${tr("：视频/GIF 抽首帧与导入视频的封面都直接用它，无需再装")}`
                          : tr(
                              "视频/GIF 壁纸抽首帧、本地库视频封面需要它。点「安装」从上游官方源下载静态构建（约 {size}，一次性；也可自行安装 ffmpeg）",
                              { size: formatBytes(fm.expectedDownloadBytes) },
                            )
                    }
                    control={
                      <div className="flex items-center gap-2">
                        <span
                          className={`text-[12px] font-medium ${fm.managed || fm.system ? "text-green-500" : "text-[var(--text-2)]"}`}
                        >
                          {fm.managed
                            ? tr("已就绪")
                            : fm.system
                              ? tr("系统已装")
                              : tr("未安装")}
                        </span>
                        {fm.managed && (
                          <button
                            className="btn !py-1 text-[11.5px]"
                            disabled={fmBusy}
                            onClick={uninstallFfmpeg}
                          >
                            {tr("卸载")}
                          </button>
                        )}
                        {!fm.system && (
                          <button
                            className="btn !py-1 text-[11.5px]"
                            disabled={fmBusy}
                            onClick={() => installFfmpeg(!!fm.managed)}
                          >
                            {fmBusy ? tr("安装中…") : fm.managed ? tr("修复") : tr("安装")}
                          </button>
                        )}
                      </div>
                    }
                  />
                  {fmMsg && (
                    <div className="mt-1.5 break-all text-[12px] text-[var(--text-2)]">
                      {trMsg(fmMsg)}
                    </div>
                  )}
                </>
              )}
              <Row
                label={tr("自动暂停")}
                desc={tr("切到非桌面应用时自动暂停壁纸，切回桌面时自动播放（手动暂停不受影响）")}
                control={<Switch checked={autoPause} onChange={toggleAutoPause} />}
              />
              <Row
                label={tr("隐藏图标")}
                desc={
                  interactive
                    ? tr("开启：壁纸窗口位于桌面图标之上（会盖住桌面图标，用于场景视差/网页互动）")
                    : tr("关闭：壁纸窗口位于桌面图标下方、壁纸上方，桌面图标正常显示（默认）")
                }
                control={<Switch checked={interactive} onChange={toggleInteractive} />}
              />
              {/* 侧边栏透明度：入口暂时隐藏（默认固定 78%，见 lib/sidebar.ts）。
                  代码保留，需要恢复时去掉这层注释即可（changeSidebarAlpha 也在）。
              <Row
                label="侧边栏透明度"
                desc={`调节左侧菜单栏的半透明/磨砂质感（${Math.round(sidebarAlpha * 100)}%）。默认 78%`}
                control={
                  <div className="flex items-center gap-2 w-48">
                    <input
                      type="range"
                      min={0.2}
                      max={1}
                      step={0.05}
                      value={sidebarAlpha}
                      onChange={(e) => changeSidebarAlpha(Number(e.target.value))}
                      className="flex-1"
                    />
                    <span className="w-10 text-right text-[12px] text-[var(--text-2)]">
                      {Math.round(sidebarAlpha * 100)}%
                    </span>
                  </div>
                }
              />
              */}
            </Group>
          )}

          {tab === "performance" && (
            <Group title={tr("性能")}>
              <Row
                label={tr("显示模式")}
                desc={
                  fit === "cover"
                    ? tr("等比铺满并居中裁切溢出（不变形、无黑边，默认）")
                    : fit === "contain"
                      ? tr("等比缩放完整显示，边缘留暗边（不变形）")
                      : tr("忽略宽高比铺满（会拉伸变形，旧行为）")
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={fit}
                      onChange={(e) => changeFit(e.target.value as "cover" | "contain" | "stretch")}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value="cover">{tr("裁剪")}</option>
                      <option value="contain">{tr("缩放")}</option>
                      <option value="stretch">{tr("拉伸")}</option>
                    </select>
                    {fitMsg && <span className="text-[12px] text-red-500">{fitMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("清晰度")}
                desc={tr("自动=跟随屏幕像素比（Retina 原生清晰，默认）；高清=100% 像素比；标准/省电逐级降低分辨率以省显存。宿主窗口像素比异常时自动档也能识别")}
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={renderDpr}
                      onChange={(e) => changeRenderDpr(Number(e.target.value))}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value={0}>{tr("自动")}</option>
                      <option value={0.75}>{tr("省电")}</option>
                      <option value={0.85}>{tr("标准")}</option>
                      <option value={1}>{tr("高清")}</option>
                    </select>
                    {renderDprMsg && <span className="text-[12px] text-red-500">{renderDprMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("帧率上限")}
                desc={
                  sceneFps <= 15
                    ? tr("15 FPS：最省电")
                    : sceneFps <= 24
                      ? tr("24 FPS：默认，GPU 占用最低，最省电")
                      : sceneFps <= 30
                        ? tr("30 FPS：流畅，GPU 占用低")
                        : sceneFps <= 45
                          ? tr("45 FPS：流畅度与功耗折中")
                          : sceneFps >= 120
                            ? tr("120 FPS：最流畅，GPU 占用最高（需高刷屏才看得出）")
                            : tr("60 FPS：画质与 GPU 占用均衡")
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={sceneFps}
                      onChange={(e) => changeSceneFps(Number(e.target.value))}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value={15}>15 FPS</option>
                      <option value={24}>24 FPS</option>
                      <option value={30}>30 FPS</option>
                      <option value={45}>45 FPS</option>
                      <option value={60}>60 FPS</option>
                      <option value={120}>120 FPS</option>
                    </select>
                    {sceneFpsMsg && <span className="text-[12px] text-red-500">{sceneFpsMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("抗锯齿")}
                desc={
                  aa === "off"
                    ? tr("关闭（默认，最省性能）")
                    : aa === "fxaa"
                      ? tr("FXAA：帧末一次后处理，平滑所有边缘（含贴图边缘），成本低")
                      : aa === "msaa2"
                        ? tr("MSAA 2x：多重采样，只平滑几何边缘（图层/粒子边缘），画质最正")
                        : tr("MSAA 4x：多重采样 4 倍，几何边缘最平滑，GPU 占用最高")
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={aa}
                      onChange={(e) => void changeAa(e.target.value)}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value="off">{tr("关")}</option>
                      <option value="fxaa">FXAA</option>
                      <option value="msaa2">MSAA 2x</option>
                      <option value="msaa4">MSAA 4x</option>
                    </select>
                    {aaMsg && <span className="text-[12px] text-red-500">{aaMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("粒子")}
                desc={
                  particles === "high"
                    ? tr("高（默认）：完整粒子数量（雨/雪/火花/雾/光轴）")
                    : particles === "medium"
                      ? tr("中：粒子数量与发射率 ×0.7，GPU/CPU 占用降低")
                      : particles === "low"
                        ? tr("低：粒子数量与发射率 ×0.4，明显省性能")
                        : tr("关：不渲染也不模拟粒子，最省性能")
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={particles}
                      onChange={(e) => void changeParticles(e.target.value)}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value="high">{tr("高")}</option>
                      <option value="medium">{tr("中")}</option>
                      <option value="low">{tr("低")}</option>
                      <option value="off">{tr("关")}</option>
                    </select>
                    {particlesMsg && <span className="text-[12px] text-red-500">{particlesMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("后处理")}
                desc={
                  post === "high"
                    ? tr("高（默认）：效果链全分辨率（辉光/模糊/水波等画面效果）")
                    : post === "medium"
                      ? tr("中：效果链分辨率压到屏幕尺寸以内，显存占用降低")
                      : post === "low"
                        ? tr("低：效果链分辨率减半，显存占用约 1/4")
                        : tr("关：图层效果链直通 + 跳过整屏后期 + 关辉光，最省性能")
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={post}
                      onChange={(e) => void changePost(e.target.value)}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value="high">{tr("高")}</option>
                      <option value="medium">{tr("中")}</option>
                      <option value="low">{tr("低")}</option>
                      <option value="off">{tr("关")}</option>
                    </select>
                    {postMsg && <span className="text-[12px] text-red-500">{postMsg}</span>}
                  </div>
                }
              />
            </Group>
          )}

          {tab === "mcp" && (
            <Group title={tr("AI / MCP")}>
              <Row
                label={tr("MCP 服务")}
                desc={
                  mcpState === "未启动"
                    ? tr(
                        "已关闭：AI 客户端（Codex / Claude 等）无法连接。开启后仅监听本机回环，连接需带访问令牌",
                      )
                    : mcpState === "运行中"
                      ? `${tr("运行中")} · ${mcp?.url}${tr("（仅本机，需令牌）")}`
                      : mcpState === "启动失败"
                        ? `${tr("启动失败")}: ${trMsg(mcp?.lastError ?? "")}`
                        : tr("正在启动…")
                }
                control={<Switch checked={mcp?.enabled === true} onChange={toggleMcp} />}
              />
              {mcp?.enabled && (
                <>
                  <Row
                    label={tr("端口")}
                    desc={tr("MCP 服务监听端口（默认 7411，改完自动热重启）。被占用时上方会显示失败原因")}
                    control={
                      <input
                        value={mcpPort}
                        disabled={mcpBusy}
                        inputMode="numeric"
                        onChange={(e) => setMcpPort(e.target.value.replace(/[^0-9]/g, ""))}
                        onBlur={commitMcpPort}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") commitMcpPort();
                        }}
                        className="w-24 rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] tabular-nums outline-none focus:border-[var(--accent-strong)]"
                      />
                    }
                  />
                  <Row
                    label={tr("访问令牌")}
                    desc={tr("客户端通过 ?token= 或 Authorization: Bearer 携带。轮换后已连接的客户端需重新配置")}
                    control={
                      <div className="flex items-center gap-2">
                        <code className="max-w-[180px] truncate font-mono text-[12px] text-[var(--text-2)]">
                          {mcpShowToken ? mcp.token : "•".repeat(Math.min(mcp.token.length, 24))}
                        </code>
                        <button
                          className="btn !py-1 text-[11.5px]"
                          onClick={() => setMcpShowToken((v) => !v)}
                        >
                          {mcpShowToken ? tr("隐藏") : tr("显示")}
                        </button>
                        <button
                          className="btn !py-1 text-[11.5px]"
                          onClick={() => copyText(mcp.urlWithToken, tr("已复制带令牌的连接地址"))}
                        >
                          {tr("复制地址")}
                        </button>
                        <button
                          className="btn !py-1 text-[11.5px]"
                          disabled={mcpBusy}
                          onClick={rotateMcpToken}
                        >
                          {tr("轮换")}
                        </button>
                      </div>
                    }
                  />
                  <Row
                    label={tr("客户端配置")}
                    desc={tr(
                      "直接发给 AI 客户端，或用命令行一键添加（Codex / Claude Code 等支持 MCP 的工具）",
                    )}
                    control={
                      <button
                        className="btn !py-1 text-[11.5px]"
                        onClick={() => setMcpShowSnippet((v) => !v)}
                      >
                        {mcpShowSnippet ? tr("收起") : tr("查看配置")}
                      </button>
                    }
                  />
                  {mcpShowSnippet && mcpSnippet && (
                    <div className="space-y-2 pb-3">
                      <SnippetBlock label="mcpServers JSON" text={mcpSnippet.json} onCopy={copyText} />
                      <SnippetBlock label="Codex CLI" text={mcpSnippet.codexCli} onCopy={copyText} />
                      <SnippetBlock label="Claude CLI" text={mcpSnippet.claudeCli} onCopy={copyText} />
                    </div>
                  )}
                  <div className="pt-3">
                    <div className="mb-1.5 text-[12px] text-[var(--text-2)]">
                      {tr("最近调用")}
                      {mcp.calls.length > 0 ? `（${mcp.calls.length}）` : ""}
                    </div>
                    {mcp.calls.length === 0 ? (
                      <div className="text-[12px] text-[var(--text-2)]">
                        {tr("AI 客户端连上后，这里会显示每次工具调用的耗时与结果")}
                      </div>
                    ) : (
                      <div className="space-y-1">
                        {mcp.calls.slice(0, 8).map((c, i) => (
                          <div key={`${c.at}-${i}`} className="flex items-baseline gap-2 text-[12px]">
                            <span className="shrink-0 tabular-nums text-[var(--text-2)]">
                              {clockOf(c.at)}
                            </span>
                            <span className={`shrink-0 font-mono ${c.ok ? "" : "text-[#ff453a]"}`}>
                              {c.tool}
                            </span>
                            <span className="shrink-0 tabular-nums text-[var(--text-2)]">{c.ms}ms</span>
                            <span className="truncate text-[var(--text-2)]">{c.summary}</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                </>
              )}
            </Group>
          )}

          {tab === "general" && (
            <Group title={tr("缓存")}>
              <Row
                label={tr("当前缓存")}
                desc={
                  cache
                    ? `${formatBytes(cache.bytes)} · ${cache.entries
                        .map((e) => `${tr(e.label)} ${formatBytes(e.bytes)}`)
                        .join(" + ")}${tr("。清除后预览图需要重新下载、壁纸封面会重新生成，不影响已下载的壁纸与设置")}`
                    : tr("统计中…")
                }
                control={
                  <div className="flex items-center gap-2">
                    {cacheMsg && <span className="text-[12px] text-[var(--text-2)]">{cacheMsg}</span>}
                    <button
                      className="btn !py-1 text-[11.5px]"
                      disabled={clearingCache || !cache}
                      onClick={() => setConfirmClearCache(true)}
                    >
                      {clearingCache ? tr("清除中…") : tr("清除缓存")}
                    </button>
                  </div>
                }
              />
            </Group>
          )}

          {tab === "about" && <AboutPanel />}
        </div>
      </div>

      {confirmLogout && (
        <ConfirmModal
          title={tr("登出下载账号")}
          message={tr(
            "将清除本地保存的账号密码、steamcmd 登录态与订阅同步的网页登录会话。下次下载和订阅同步都需要重新登录并再过一次验证。",
          )}
          confirmText={tr("登出")}
          danger
          onCancel={() => setConfirmLogout(false)}
          onConfirm={() => {
            setConfirmLogout(false);
            logout();
          }}
        />
      )}

      {confirmClearCache && (
        <ConfirmModal
          title={tr("清除缓存")}
          message={
            cache
              ? tr(
                  "将删除 {size} 的预览图/网页缓存与壁纸首帧封面，并清空工坊、发现页的列表快照。已下载的壁纸与各项设置不受影响。",
                  { size: formatBytes(cache.bytes) },
                )
              : tr("将删除预览图/网页缓存与壁纸首帧封面，并清空工坊、发现页的列表快照。")
          }
          confirmText={tr("清除")}
          onCancel={() => setConfirmClearCache(false)}
          onConfirm={() => {
            setConfirmClearCache(false);
            void clearCache();
          }}
        />
      )}
    </div>
  );
}

/** 仓库 / 文档链接（关于页使用） */
const REPO_URL = "https://github.com/oneincase/WallpaperEM";
const RELEASES_URL = `${REPO_URL}/releases`;
const CHANGELOG_URL = `${REPO_URL}/blob/main/CHANGELOG.md`;
const ISSUES_URL = `${REPO_URL}/issues`;
const LICENSE_URL = `${REPO_URL}/blob/main/LICENSE`;
const MEDIAREMOTE_URL = "https://github.com/ungive/mediaremote-adapter";

/** 「关于」面板上次的检查结果：切标签会重挂组件，缓存一下避免每次进页面都打 GitHub API */
let aboutCheckCache: UpdateInfo | null = null;

/**
 * 「关于」面板：应用信息 + 更新检查 / 下载 / 安装 + 相关链接。
 *
 * 更新走「GitHub Releases 最新版 → 比对版本 → 下载当前平台安装包 → 打开安装器」，
 * 不依赖官方 updater 需要的签名密钥与 latest.json（见 src-tauri/src/update.rs）。
 */
function AboutPanel() {
  const [info, setInfo] = useState<{
    name: string;
    version: string;
    os: string;
    arch: string;
  } | null>(null);
  const [upd, setUpd] = useState<UpdateInfo | null>(aboutCheckCache);
  const [phase, setPhase] = useState<"idle" | "checking" | "latest" | "available" | "error">(
    aboutCheckCache ? (aboutCheckCache.hasUpdate ? "available" : "latest") : "idle"
  );
  const [errMsg, setErrMsg] = useState("");
  const [prog, setProg] = useState<{ received: number; total: number } | null>(null);
  const [file, setFile] = useState("");
  const [busy, setBusy] = useState(false);
  const [hint, setHint] = useState("");

  const check = useCallback(async () => {
    setPhase("checking");
    setErrMsg("");
    setHint("");
    // 重新检查后旧安装包可能已不对应（换版本），清掉避免误开
    setFile("");
    try {
      const r = await api.appUpdateCheck();
      aboutCheckCache = r;
      setUpd(r);
      setPhase(r.hasUpdate ? "available" : "latest");
    } catch (e) {
      setErrMsg(String(e));
      setPhase("error");
    }
  }, []);

  useEffect(() => {
    api.appInfo().then(setInfo).catch(console.warn);
    // 有缓存就不再自动打一次 API（切标签回来时）
    if (!aboutCheckCache) void check();
  }, [check]);

  // 下载进度由 Rust 侧 `update:progress` 事件推进
  useEffect(() => {
    const un = listen<{ received: number; total: number }>("update:progress", (e) =>
      setProg(e.payload)
    );
    return () => {
      void un.then((f) => f());
    };
  }, []);

  const download = useCallback(async () => {
    if (!upd?.asset) return;
    setBusy(true);
    setErrMsg("");
    setHint("");
    setFile("");
    setProg({ received: 0, total: upd.asset.size });
    try {
      const path = await api.appUpdateDownload(upd.asset.url, upd.asset.name);
      setFile(path);
      setHint(tr("下载完成，点「打开安装包」继续"));
    } catch (e) {
      setErrMsg(String(e));
    } finally {
      setBusy(false);
      setProg(null);
    }
  }, [upd]);

  const openInstaller = useCallback(async () => {
    setErrMsg("");
    try {
      setHint(await api.appUpdateOpen(file));
    } catch (e) {
      setErrMsg(String(e));
    }
  }, [file]);

  const pct = prog && prog.total > 0 ? Math.min(100, Math.round((prog.received / prog.total) * 100)) : null;
  const osLabel =
    info?.os === "macos" ? "macOS" : info?.os === "windows" ? "Windows" : info?.os === "linux" ? "Linux" : (info?.os ?? "");
  const pubDate = (() => {
    if (!upd?.publishedAt) return "";
    const d = new Date(upd.publishedAt);
    return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString();
  })();

  const linkBtn =
    "rounded-lg border border-[var(--separator)] px-2.5 py-1 text-[12.5px] text-[var(--text-2)] transition-colors hover:bg-black/5 hover:text-[var(--text-1)] dark:hover:bg-white/8";
  const primaryBtn =
    "rounded-lg bg-[var(--accent-strong)] px-3 py-1.5 text-[12.5px] font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-40";

  return (
    <div className="space-y-3">
      {/* 应用信息 */}
      <div className="rounded-xl border border-[var(--separator)] p-4">
        <div className="flex items-center gap-3.5">
          <img
            src="/icon/icon_128x128.png"
            width={56}
            height={56}
            alt="WallpaperEM"
            className="h-14 w-14 shrink-0 rounded-[13px]"
            draggable={false}
          />
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <span className="text-[16px] font-semibold tracking-tight">WallpaperEM</span>
              <span className="rounded-full border border-[var(--separator)] bg-[var(--accent)] px-2 py-0.5 text-[11px] font-medium text-[var(--accent-strong)]">
                v{info?.version ?? "…"}
              </span>
            </div>
            <div className="mt-1 text-[12px] text-[var(--text-2)]">
              {info?.os === "macos"
                ? tr("macOS 动态壁纸引擎 · 浏览/下载并应用 Steam 创意工坊壁纸")
                : tr("跨平台动态壁纸引擎 · 浏览/下载并应用 Steam 创意工坊壁纸")}
            </div>
            <div className="mt-1 text-[11.5px] text-[var(--text-2)] opacity-80">
              {tr("平台")} {osLabel} · {info?.arch ?? ""}
            </div>
          </div>
        </div>
      </div>

      {/* 更新 */}
      <div className="rounded-xl border border-[var(--separator)] p-4">
        <div className="flex items-center justify-between gap-3">
          <div className="text-[14px] font-medium">{tr("软件更新")}</div>
          <button
            className={primaryBtn}
            disabled={phase === "checking" || busy}
            onClick={() => void check()}
          >
            {phase === "checking" ? tr("正在检查…") : tr("检查更新")}
          </button>
        </div>

        <div className="mt-2 text-[12.5px] text-[var(--text-2)]">
          {phase === "idle" && tr("尚未检查更新")}
          {phase === "checking" && tr("正在检查…")}
          {phase === "latest" && tr("已是最新版本")}
          {phase === "error" && `${tr("检查更新失败")}${errMsg ? `：${trMsg(errMsg)}` : ""}`}
          {phase === "available" && upd && tr("发现新版本 {v}", { v: `v${upd.latest}` })}
        </div>

        {phase === "available" && upd && (
          <div className="mt-3 space-y-2.5">
            {upd.name && <div className="text-[13px] font-medium">{upd.name}</div>}
            {pubDate && (
              <div className="text-[11.5px] text-[var(--text-2)] opacity-80">
                {tr("发布于 {date}", { date: pubDate })}
              </div>
            )}
            {upd.notes.trim() !== "" && (
              <div className="rounded-lg border border-[var(--separator)] bg-black/5 dark:bg-white/5">
                <div className="border-b border-[var(--separator)] px-2.5 py-1.5 text-[11.5px] text-[var(--text-2)]">
                  {tr("更新内容")}
                </div>
                <pre className="max-h-44 overflow-auto whitespace-pre-wrap px-2.5 py-2 text-[11.5px] leading-relaxed text-[var(--text-2)]">
                  {upd.notes.trim()}
                </pre>
              </div>
            )}

            {prog && (
              <div className="space-y-1">
                <div className="h-1.5 w-full overflow-hidden rounded-full bg-[var(--separator)]">
                  <div
                    className="h-full rounded-full bg-[var(--accent-strong)] transition-[width] duration-200"
                    style={{ width: pct === null ? "30%" : `${pct}%` }}
                  />
                </div>
                <div className="text-[11.5px] text-[var(--text-2)]">
                  {tr("下载中…")}
                  {prog.total > 0
                    ? ` ${formatBytes(prog.received)} / ${formatBytes(prog.total)}${pct !== null ? ` · ${pct}%` : ""}`
                    : ` ${formatBytes(prog.received)}`}
                </div>
              </div>
            )}

            <div className="flex flex-wrap items-center gap-2">
              {upd.asset ? (
                <button className={primaryBtn} disabled={busy} onClick={() => void download()}>
                  {busy ? tr("下载中…") : tr("下载更新")}
                </button>
              ) : (
                <span className="text-[12px] text-[var(--text-2)]">
                  {tr("当前平台没有可直接下载的安装包")}
                </span>
              )}
              {file && (
                <button className={primaryBtn} onClick={() => void openInstaller()}>
                  {tr("打开安装包")}
                </button>
              )}
              <button className={linkBtn} onClick={() => void openUrl(upd.htmlUrl || RELEASES_URL)}>
                {tr("前往下载页")}
              </button>
            </div>

            {hint && <div className="text-[12px] text-[var(--text-2)]">{trMsg(hint)}</div>}
            {errMsg && phase === "available" && (
              <div className="text-[12px] text-red-500">{trMsg(errMsg)}</div>
            )}
          </div>
        )}

        {phase === "error" && (
          <div className="mt-2 flex flex-wrap items-center gap-2">
            <span className="text-[12px] text-[var(--text-2)]">
              {tr("可前往下载页手动下载")}
            </span>
            <button className={linkBtn} onClick={() => void openUrl(RELEASES_URL)}>
              {tr("前往下载页")}
            </button>
          </div>
        )}
      </div>

      {/* 链接 */}
      <div className="rounded-xl border border-[var(--separator)] p-4">
        <div className="mb-2.5 text-[14px] font-medium">{tr("相关链接")}</div>
        <div className="flex flex-wrap gap-2">
          <button className={linkBtn} onClick={() => void openUrl(REPO_URL)}>
            {tr("GitHub 仓库")}
          </button>
          <button className={linkBtn} onClick={() => void openUrl(RELEASES_URL)}>
            {tr("更新日志")}
          </button>
          <button className={linkBtn} onClick={() => void openUrl(CHANGELOG_URL)}>
            {tr("完整更新记录")}
          </button>
          <button className={linkBtn} onClick={() => void openUrl(ISSUES_URL)}>
            {tr("问题反馈")}
          </button>
          <button className={linkBtn} onClick={() => void openUrl(LICENSE_URL)}>
            {tr("开源许可")}
          </button>
          <button className={linkBtn} onClick={() => void openUrl(MEDIAREMOTE_URL)}>
            {tr("第三方组件")}
          </button>
        </div>
        <div className="mt-3 text-[11.5px] leading-relaxed text-[var(--text-2)] opacity-80">
          {tr("以 MIT 许可开源。内含第三方组件 mediaremote-adapter（BSD-3-Clause），用于读取系统「正在播放」信息。")}
        </div>
        <div className="mt-2 text-[11.5px] text-[var(--text-2)] opacity-60">
          © {new Date().getFullYear()} oneincase · MIT License
        </div>
      </div>
    </div>
  );
}

/** 一行可复制的配置片段（MCP 客户端接入用） */
function SnippetBlock({
  label,
  text,
  onCopy,
}: {
  label: string;
  text: string;
  onCopy: (text: string, okText: string) => void;
}) {
  return (
    <div className="rounded-xl border border-[var(--separator)] p-2.5">
      <div className="mb-1.5 flex items-center justify-between gap-3">
        <span className="text-[12px] font-medium text-[var(--text-2)]">{label}</span>
        <button
          className="btn !py-1 text-[11.5px]"
          onClick={() => onCopy(text, tr("已复制 {label}", { label }))}
        >
          {tr("复制")}
        </button>
      </div>
      <pre className="max-h-32 overflow-auto whitespace-pre-wrap break-all font-mono text-[11.5px] leading-relaxed text-[var(--text-2)]">
        {text}
      </pre>
    </div>
  );
}

/** unix 毫秒 → HH:MM:SS（最近调用列表的时间列） */
function clockOf(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="card overflow-hidden">
      <div className="px-5 pt-4 pb-1 text-[13px] font-semibold text-[var(--text-2)]">
        {title}
      </div>
      <div className="px-5 pb-3">{children}</div>
    </section>
  );
}

function Row({
  label,
  desc,
  control,
}: {
  label: string;
  desc: string;
  control: React.ReactNode;
}) {
  return (
    <div className="flex items-center justify-between gap-4 py-3 border-b border-[var(--separator)] last:border-0">
      <div>
        <div className="text-[14px] font-medium">{label}</div>
        <div className="text-[12px] text-[var(--text-2)] mt-0.5">{desc}</div>
      </div>
      {control}
    </div>
  );
}

function Switch({
  checked,
  onChange,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative inline-flex h-[22px] w-[38px] shrink-0 items-center rounded-full transition-colors ${checked ? "bg-[var(--accent-strong)]" : "bg-[var(--separator)]"
        }`}
    >
      <span
        className={`inline-block h-[18px] w-[18px] transform rounded-full bg-[var(--content)] shadow transition-transform ${checked ? "translate-x-[18px]" : "translate-x-[2px]"
          }`}
      />
    </button>
  );
}
