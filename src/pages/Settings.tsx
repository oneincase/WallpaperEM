import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  api,
  type CacheStats,
  type DownloadToolStatus,
  type FfmpegInstallProgress,
  type FfmpegStatus,
  type LocalAssetsStatus,
  type McpConfigSnippet,
  type McpStatus,
  type SteamcmdInstallProgress,
  type UpdateInfo,
} from "../api/steam";
import { formatBytes } from "../lib/format";
import {
  POST_STOPS,
  PARTICLE_STOPS,
  PRESET_LABELS,
  QUALITY_PRESETS,
  deriveQualityPreset,
  type QualityPresetKey,
} from "../lib/qualityPresets";
import { clearSnapshotCaches } from "../lib/cache-snapshots";
import { LOCALES, setLocale, tr, trMsg, useLocale, type Locale } from "../lib/i18n";
import { ConfirmModal } from "../components/ConfirmModal";
import { QrModal } from "../components/QrModal";
import { Switch } from "../components/Switch";
import { useMessage } from "../components/Message";

// 设置页标签：账号 / 通用 / 画质 / 网络 / 关于
// （id 沿用 "download"：该页内容是下载工具安装 + 下载账号登录，改 id 无收益；
//   "performance" 同理沿用，标签已改名「画质」；
//   "network" 吸收了原「AI / MCP」标签 —— MCP/REST API/壁纸分享共用一个网络服务）
type SettingsTab = "download" | "general" | "performance" | "network" | "about";
const SETTINGS_TABS: { id: SettingsTab; label: string }[] = [
  { id: "download", label: "账号" },
  { id: "general", label: "通用" },
  { id: "performance", label: "画质" },
  { id: "network", label: "网络与服务" },
  { id: "about", label: "关于" },
];

// ---- 画质档位预设（与 Rust 的 PRESET_LOW/MEDIUM/HIGH 逐字段镜像）----
// 选中即**整体覆盖**全局画质参数；手调下方任意参数后按值反推为「自定义」。
// 抗锯齿不进表：锁定期恒为 off、禁止更改（方案优化中）。
// 表本体在 lib/qualityPresets（与「壁纸设置」窗口的播放设置共用）。
// 画质页滑条统一样式（range 一定带 min/max 限制）
const SLIDER_CLS = "w-36 accent-[var(--accent-strong)] disabled:opacity-40";

// 滤镜白名单（与 Rust 的 WALLPAPER_FILTERS 一一对应；CSS 表达式只存在于渲染器）
const FILTER_OPTIONS: Array<{ id: string; label: string }> = [
  { id: "none", label: "无" },
  { id: "blur", label: "高斯模糊" },
  { id: "grayscale", label: "黑白" },
  { id: "sepia", label: "怀旧" },
  { id: "vivid", label: "鲜艳" },
  { id: "warm", label: "暖色" },
  { id: "cool", label: "冷色" },
  { id: "invert", label: "反色" },
  { id: "brighten", label: "提亮" },
  { id: "darken", label: "压暗" },
  { id: "contrast", label: "高对比" },
];

/** 设置值 → 贴图资源倍率。历史的 `auto`/空/非法（「跟随清晰度」档已移除）回落 1（原生） */
/** 视频纹理倍率设置解析：0/非法 = 自动（0）；其余钳到 [0.25, 1]。 */
function parseVideoTexScaleSetting(raw: string | null | undefined): number {
  const v = Number(raw);
  if (!Number.isFinite(v) || v <= 0) return 0;
  return Math.min(1, Math.max(0.25, v));
}

function parseResourcesSetting(v: string | null | undefined): number {
  if (v == null || v === "" || v === "auto") return 1;
  const n = Number(v);
  return Number.isFinite(n) && n >= 0.5 && n <= 1 ? n : 1;
}

/** 设置值 → 清晰度倍率。历史的 0（自动档已移除）归一为 1（原生，视觉等价） */
function parseRenderDprSetting(v: string | null | undefined): number {
  const n = Number(v);
  return Number.isFinite(n) && n > 0 && n <= 1 ? n : 1;
}

/** 设置值 → 帧率上限。范围 15–60，越界（历史 120 等）钳到上限 */
function parseSceneFpsSetting(v: string | null | undefined): number {
  const n = Number(v);
  return Number.isFinite(n) && n > 0 ? Math.min(60, Math.max(15, n)) : 24;
}

export function SettingsPage() {
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [interactive, setInteractive] = useState(false);
  const [autoSystemStatic, setAutoSystemStatic] = useState(true);
  const [autoPause, setAutoPause] = useState(false);
  // 轮播默认值（新建切换列表时的初值）
  const [plDefaultInterval, setPlDefaultInterval] = useState(10);
  const [plDefaultShuffle, setPlDefaultShuffle] = useState(false);
  const [plPowerOnly, setPlPowerOnly] = useState(false);
  // 暂停释放内存：自动暂停的加强形态（默认关）。开启后自动暂停会直接销毁
  // 壁纸渲染窗口（渲染进程结束、内存归还），回到桌面时按会话配置整窗重建
  const [autoPauseRelease, setAutoPauseRelease] = useState(false);
  const [audioProcessing, setAudioProcessing] = useState(false);
  const [audioMsg, setAudioMsg] = useState("");
  // 全局壁纸显示模式（cover/contain/stretch），默认 cover 等比铺满裁切
  const [fit, setFit] = useState<"cover" | "contain" | "stretch">("cover");
  const [fitMsg, setFitMsg] = useState("");
  // 全局清晰度（相对设备像素比的倍率，0.50–1.00，1=原生）。renderer 会乘
  // devicePixelRatio 换算成绝对 DPR。历史的「自动」（0）档已移除：载入时归一为 1
  //（自动≈原生，视觉等价），此后滑条只写具体倍率。
  const [renderDpr, setRenderDpr] = useState<number>(1);
  const [renderDprMsg, setRenderDprMsg] = useState("");
  // 视频纹理上传倍率：0 = 自动（库内帧率守门按实测帧率压）。独立开关，
  // **不参与画质档位反推**（预设一律给自动档），所以不并进 QUALITY_PRESETS。
  const [videoTexScale, setVideoTexScale] = useState<number>(0);
  const [videoTexScaleMsg, setVideoTexScaleMsg] = useState("");
  // 全局场景帧率上限（15–60 任意整数，越低 GPU 占用越低），默认 24
  const [sceneFps, setSceneFps] = useState<number>(24);
  const [sceneFpsMsg, setSceneFpsMsg] = useState("");
  // 粒子质量档 high 默认 / medium / low / off
  const [particles, setParticles] = useState<string>("high");
  const [particlesMsg, setParticlesMsg] = useState("");
  // 后处理质量档 high 默认 / medium / low
  const [post, setPost] = useState<string>("high");
  const [postMsg, setPostMsg] = useState("");
  // 贴图资源倍率（0.5–1，1 = 原生不缩）。历史的「跟随清晰度」（auto）档已移除：
  // 载入时归一为 1，滑条只写具体倍率。挂载期生效，改后壁纸整页重载
  const [resources, setResources] = useState<number>(1);
  const [resourcesMsg, setResourcesMsg] = useState("");
  // 法线/蒙版资源倍率（0.35–1，默认 1 不缩）。同样挂载期生效
  const [resourcesNormal, setResourcesNormal] = useState<number>(1);
  const [resourcesNormalMsg, setResourcesNormalMsg] = useState("");
  // 全局滤镜 id（白名单见 FILTER_OPTIONS；此前只有托盘菜单能改），热切生效
  const [filter, setFilter] = useState<string>("none");
  const [filterMsg, setFilterMsg] = useState("");
  // 画质参数整组到齐后才反推档位（避免半路闪错档位）
  const [qualityReady, setQualityReady] = useState(false);
  const [presetMsg, setPresetMsg] = useState("");
  // 无缝切换效果（叠化/推近/模糊/景深/圆形揭示/横向擦除/滑入），换壁纸瞬间生效
  const [reveal, setReveal] = useState<string>("fade");
  const [revealMsg, setRevealMsg] = useState("");
  // WE 官方素材通路（webwallgl 1.4.1 local-assets）：本机装了 WE 时用官方原版贴图
  const [localAssets, setLocalAssets] = useState<LocalAssetsStatus | null>(null);
  const [localAssetsMsg, setLocalAssetsMsg] = useState("");
  const [localAssetsBusy, setLocalAssetsBusy] = useState(false);
  const [weAssetsDir, setWeAssetsDir] = useState("");
  // 首次拿到状态后才把自定义路径灌进输入框，避免编辑中途被覆盖
  const weAssetsDirHydrated = useRef(false);
  // 壁纸语言（只影响壁纸的 language 属性，不是软件本体语言）。默认英文
  const [language, setLanguage] = useState<string>("english");
  // 界面语言（i18n）：这里只是订阅，取词走模块级 tr()；订阅是为了本页文案跟着变
  const locale = useLocale();
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
  const [mcpShowLanQr, setMcpShowLanQr] = useState(false);
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
    invoke<string | null>("settings_get", { key: "wallpaper_auto_pause_release" })
      .then((v) => setAutoPauseRelease(v === "true" || v === "1"))
      .catch(() => { });
    // 轮播默认值：间隔存秒、展示分钟；未设置用 10 分钟
    invoke<string | null>("settings_get", { key: "playlist_default_interval_sec" })
      .then((v) => {
        const s = Number(v);
        setPlDefaultInterval(s >= 60 ? Math.round(s / 60) : 10);
      })
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "playlist_default_shuffle" })
      .then((v) => setPlDefaultShuffle(v === "true" || v === "1"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "playlist_rotation_power" })
      .then((v) => setPlPowerOnly(v === "true" || v === "1"))
      .catch(() => { });
    // 默认开启：键未写入过视为 true
    invoke<string | null>("settings_get", { key: "wallpaper_auto_system_static" })
      .then((v) => setAutoSystemStatic(v == null || v === "true" || v === "1"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_fit" })
      .then((v) => setFit((v as "cover" | "contain" | "stretch") || "cover"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_video_tex_scale" })
      .then((v) => setVideoTexScale(parseVideoTexScaleSetting(v)))
      .catch(() => { });
    // 画质参数整组载入（清晰度/帧率/粒子/后处理/贴图倍率/法线倍率）：
    // 档位要按整组值反推，逐个 setState 会在半路闪出错误档位，等全部到齐再落
    void Promise.all([
      invoke<string | null>("settings_get", { key: "wallpaper_render_dpr" }),
      invoke<string | null>("settings_get", { key: "wallpaper_scene_fps" }),
      invoke<string | null>("settings_get", { key: "wallpaper_particles" }),
      invoke<string | null>("settings_get", { key: "wallpaper_post" }),
      invoke<string | null>("settings_get", { key: "wallpaper_resources" }),
      invoke<string | null>("settings_get", { key: "wallpaper_resources_normal" }),
    ])
      .then(([dpr, fps, particles, post, res, resn]) => {
        // 未设置/非法值回落默认：1（原生）/ 24 / high / high / 1（原生）/ 1。
        // 历史 0（清晰度自动）与 auto（贴图跟随清晰度）都归一为 1 —— 两档已移除
        setRenderDpr(parseRenderDprSetting(dpr));
        setSceneFps(parseSceneFpsSetting(fps));
        setParticles(particles || "high");
        setPost(post || "high");
        setResources(parseResourcesSetting(res));
        const rn = Number(resn);
        setResourcesNormal(Number.isFinite(rn) && rn >= 0.35 && rn <= 1 ? rn : 1);
        setQualityReady(true);
      })
      .catch(() => setQualityReady(true));
    invoke<string | null>("settings_get", { key: "wallpaper_filter" })
      .then((v) => setFilter(v || "none"))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_reveal" })
      .then((v) => setReveal(v || "fade"))
      .catch(() => { });
    api
      .wallpaperLocalAssetsStatus()
      .then((s) => {
        setLocalAssets(s);
        if (!weAssetsDirHydrated.current) {
          setWeAssetsDir(s.customDir);
          weAssetsDirHydrated.current = true;
        }
      })
      .catch(console.warn);
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

  // 共享设置被其它入口改了（托盘菜单 / MCP）：控件值跟上，避免两边状态不一致。
  // 同一份广播由 Rust 侧 notify_setting_changed 在每次写入后发出
  useEffect(() => {
    const un = listen<{ key: string; value: string }>("settings-changed", (e) => {
      const { key, value } = e.payload;
      switch (key) {
        case "wallpaper_auto_pause":
          setAutoPause(value === "true" || value === "1");
          break;
        case "wallpaper_auto_pause_release":
          setAutoPauseRelease(value === "true" || value === "1");
          break;
        case "wallpaper_fit":
          if (value === "cover" || value === "contain" || value === "stretch") setFit(value);
          break;
        case "wallpaper_render_dpr":
          setRenderDpr(parseRenderDprSetting(value));
          break;
        case "wallpaper_scene_fps":
          setSceneFps(parseSceneFpsSetting(value));
          break;
        case "wallpaper_video_tex_scale":
          setVideoTexScale(parseVideoTexScaleSetting(value));
          break;
        case "wallpaper_particles":
          setParticles(value || "high");
          break;
        case "wallpaper_post":
          setPost(value || "high");
          break;
        case "wallpaper_resources":
          setResources(parseResourcesSetting(value));
          break;
        case "wallpaper_resources_normal": {
          const n = Number(value);
          if (Number.isFinite(n) && n >= 0.35 && n <= 1) setResourcesNormal(n);
          break;
        }
        case "wallpaper_filter":
          setFilter(value || "none");
          break;
        case "wallpaper_reveal":
          setReveal(value || "fade");
          break;
      }
    });
    return () => {
      void un.then((f) => f());
    };
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

  // 自动暂停：看得见就播——几乎被完全遮挡（全屏/最大化/屏保）才暂停，露出即恢复（默认关）
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

  // 暂停释放内存：自动暂停时销毁壁纸渲染器、回桌面完全重载（默认关）。
  // 只写设置——下一次自动暂停生效（已在暂停中不会 retroactive 触发）
  const toggleAutoPauseRelease = async () => {
    const next = !autoPauseRelease;
    try {
      await invoke("settings_set", {
        key: "wallpaper_auto_pause_release",
        value: next ? "true" : "false",
      });
      setAutoPauseRelease(next);
    } catch {
      // 失败则不变
    }
  };

  // 轮播默认值：新建切换列表的初值（间隔存秒、展示分钟）
  const changePlDefaultInterval = async (minutes: number) => {
    const prev = plDefaultInterval;
    setPlDefaultInterval(minutes);
    try {
      await invoke("settings_set", {
        key: "playlist_default_interval_sec",
        value: String(Math.max(30, Math.round(minutes * 60))),
      });
    } catch {
      setPlDefaultInterval(prev); // 失败则回滚
    }
  };

  const togglePlDefaultShuffle = async () => {
    const next = !plDefaultShuffle;
    try {
      await invoke("settings_set", {
        key: "playlist_default_shuffle",
        value: next ? "true" : "false",
      });
      setPlDefaultShuffle(next);
    } catch {
      // 失败则不变
    }
  };

  const togglePlPowerOnly = async () => {
    const next = !plPowerOnly;
    try {
      await invoke("settings_set", {
        key: "playlist_rotation_power",
        value: next ? "true" : "false",
      });
      setPlPowerOnly(next);
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

  // 音频可视化（系统音频处理）：三平台统一由 media-bridge 采集 —— macOS 走
  // CoreAudio 进程 Tap，需要「系统音频录制」授权（系统设置 → 隐私与安全性 →
  // 录屏与系统录音 → 仅系统录音分组），该面板没有自动弹框，需手动开启；
  // Windows 走 WASAPI loopback、Linux 走 PulseAudio/PipeWire monitor，均无需授权
  const toggleAudioProcessing = async () => {
    const next = !audioProcessing;
    setAudioMsg("");
    if (next && !audioSupported) {
      setAudioMsg(
        tr(
          "当前平台暂不支持系统音频捕获（macOS CoreAudio / Windows WASAPI / Linux PulseAudio·PipeWire 均已支持）",
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
                "⚠️ 尚未授权：系统设置 → 隐私与安全性 → 录屏与系统录音 → 「仅系统录音」分组 → 打开 WallpaperEM，然后重新关闭/打开本开关（或重启应用）",
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


  const changeFit = async (next: "cover" | "contain" | "stretch") => {
    setFitMsg("");
    try {
      await api.wallpaperSetFit(next);
      setFit(next);
    } catch (e) {
      setFitMsg(String(e));
    }
  };

  // ---- 画质参数提交（滑条拖动会连续触发 onChange）----
  // 本地值立即跟上让标签实时动，命令去抖 350ms 只发最后一次：清晰度改一次库内
  // 要重挂场景，贴图/法线倍率改一次要整页重载，逐帧下发会把壁纸窗口卡爆。
  const commitTimers = useRef<Record<string, number>>({});
  const commitLater = (key: string, run: () => Promise<void>) => {
    window.clearTimeout(commitTimers.current[key]);
    commitTimers.current[key] = window.setTimeout(() => void run(), 350);
  };

  const changeRenderDpr = (next: number) => {
    setRenderDpr(next);
    setRenderDprMsg("");
    commitLater("renderDpr", async () => {
      try {
        await api.wallpaperSetRenderDpr(next);
      } catch (e) {
        setRenderDprMsg(String(e));
      }
    });
  };

  const changeVideoTexScale = (next: number) => {
    setVideoTexScale(next);
    setVideoTexScaleMsg("");
    commitLater("videoTexScale", async () => {
      try {
        await api.wallpaperSetVideoTexScale(next);
      } catch (e) {
        setVideoTexScaleMsg(String(e));
      }
    });
  };

  const changeSceneFps = (next: number) => {
    setSceneFps(next);
    setSceneFpsMsg("");
    commitLater("sceneFps", async () => {
      try {
        await api.wallpaperSetSceneFps(next);
      } catch (e) {
        setSceneFpsMsg(String(e));
      }
    });
  };

  const changeParticles = (next: string) => {
    setParticles(next);
    setParticlesMsg("");
    commitLater("particles", async () => {
      try {
        await api.wallpaperSetParticles(next);
      } catch (e) {
        setParticlesMsg(String(e));
      }
    });
  };

  const changePost = (next: string) => {
    setPost(next);
    setPostMsg("");
    commitLater("post", async () => {
      try {
        await api.wallpaperSetPost(next);
      } catch (e) {
        setPostMsg(String(e));
      }
    });
  };

  const changeResources = (next: number) => {
    setResources(next);
    setResourcesMsg("");
    commitLater("resources", async () => {
      try {
        await api.wallpaperSetResources(next);
      } catch (e) {
        setResourcesMsg(String(e));
      }
    });
  };

  const changeResourcesNormal = (next: number) => {
    setResourcesNormal(next);
    setResourcesNormalMsg("");
    commitLater("resourcesNormal", async () => {
      try {
        await api.wallpaperSetResourcesNormal(next);
      } catch (e) {
        setResourcesNormalMsg(String(e));
      }
    });
  };

  // 滤镜是纯 CSS 合成层的事：热切、无需重挂/重载，直接提交
  const changeFilter = async (next: string) => {
    setFilterMsg("");
    try {
      await api.wallpaperSetFilter(next);
      setFilter(next);
    } catch (e) {
      setFilterMsg(String(e));
    }
  };

  // 画质档位：一键覆盖全部画质参数（Rust 侧批量写入、只整页重载一次）。
  // 「自定义」是按当前值反推出来的状态，没有可套用的值，点它不做任何事
  const applyQualityPreset = async (id: QualityPresetKey) => {
    if (id === "custom") return;
    setPresetMsg("");
    try {
      await api.wallpaperSetQualityPreset(id);
      const v = QUALITY_PRESETS[id];
      setRenderDpr(v.renderDpr);
      setSceneFps(v.sceneFps);
      setParticles(v.particles);
      setPost(v.post);
      setResources(v.resources);
      setResourcesNormal(v.resourcesNormal);
    } catch (e) {
      setPresetMsg(String(e));
    }
  };

  // 当前画质参数 → 档位：与某个预设逐字段一致 = 该档位，否则「自定义」（手调
  // 任意参数即进入自定义）。整组参数到齐前先按自定义显示，避免闪错档位
  const preset: QualityPresetKey = !qualityReady
    ? "custom"
    : deriveQualityPreset({ renderDpr, sceneFps, particles, post, resources, resourcesNormal });

  // 切换效果不热更活窗口：下一次换壁纸时新窗按新效果显形
  const changeReveal = async (next: string) => {
    setRevealMsg("");
    try {
      await api.wallpaperSetReveal(next);
      setReveal(next);
    } catch (e) {
      setRevealMsg(String(e));
    }
  };

  // WE 官方素材通路：挂载时生效，切换后现有壁纸整页重载一次
  const toggleLocalAssets = async (enabled: boolean) => {
    setLocalAssetsMsg("");
    setLocalAssetsBusy(true);
    const prev = localAssets?.enabled ?? true;
    setLocalAssets((s) => (s ? { ...s, enabled } : s));
    try {
      await api.wallpaperLocalAssetsSet(enabled);
      const s = await api.wallpaperLocalAssetsStatus();
      setLocalAssets(s);
    } catch (e) {
      setLocalAssets((s) => (s ? { ...s, enabled: prev } : s));
      setLocalAssetsMsg(String(e));
    } finally {
      setLocalAssetsBusy(false);
    }
  };

  // 原生文件夹选择框：选中即把绝对路径填入输入框（不直接保存，
  // 允许选完微调；生效仍走「保存」）
  const pickWeAssetsDir = async () => {
    setLocalAssetsMsg("");
    try {
      const dir = await api.appPickFolder();
      if (dir) setWeAssetsDir(dir);
    } catch (e) {
      setLocalAssetsMsg(String(e));
    }
  };

  // 保存自定义 assets 根（其下需直接有 materials/）；空串清除并回到自动探测
  const saveWeAssetsDir = async () => {
    setLocalAssetsMsg("");
    setLocalAssetsBusy(true);
    try {
      await api.wallpaperWeAssetsDirSet(weAssetsDir.trim());
      const s = await api.wallpaperLocalAssetsStatus();
      setLocalAssets(s);
      setWeAssetsDir(s.customDir);
    } catch (e) {
      setLocalAssetsMsg(String(e));
    } finally {
      setLocalAssetsBusy(false);
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

  // 只在「网络与服务」标签页里轮询：最近调用列表需要刷新，其余时间不必打扰后端
  useEffect(() => {
    if (tab !== "network") return;
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
    void applyMcp(
      () => api.mcpSetEnabled(next),
      next ? tr("网络服务已启用") : tr("网络服务已关闭"),
    );
  };

  // 网络模式切换会换绑定地址（回环 ↔ 全接口），后端热重启监听
  const changeMcpNetMode = (next: McpStatus["netMode"]) => {
    const label = next === "loopback" ? tr("本机") : next === "lan" ? tr("局域网") : tr("任意");
    void applyMcp(() => api.mcpSetNetMode(next), tr("网络模式已切换为 {mode}", { mode: label }));
  };

  // 双触发防抖：Enter 提交后紧接着的 blur 会再触发一次 commit —— 此刻 mcp 状态
  // 还没被响应刷新（旧 port 守卫放行），第二次 mcpSetPort 会把刚绑好的监听拆掉
  // 重绑同一端口，撞上 TIME_WAIT 就误报「端口被占用」。记下最近提交的端口，
  // 同值重复提交直接忽略；提交失败（状态没到新端口）时清掉允许原样重试
  const lastPortCommitRef = useRef<number | null>(null);
  const mcpRef = useRef(mcp);
  mcpRef.current = mcp;
  const commitMcpPort = () => {
    const port = Number(mcpPort.trim());
    if (!Number.isInteger(port) || port <= 1024 || port > 65535) {
      msg.error(tr("端口需是 1025–65535 之间的整数"));
      setMcpPort(String(mcp?.port ?? ""));
      return;
    }
    if (port === lastPortCommitRef.current) return;
    lastPortCommitRef.current = port;
    if (mcp && port === mcp.port) return;
    void applyMcp(() => api.mcpSetPort(port), tr("端口已改为 {port}", { port })).then(() => {
      if (mcpRef.current?.port !== port) lastPortCommitRef.current = null;
    });
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
              : "text-[var(--text-2)] hover:bg-white/8"
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
              <Group title={tr("网络服务")}>
                <Row
                  label={tr("网络服务")}
                  desc={
                    mcpState === "未启动"
                      ? tr(
                          "已关闭：AI 客户端（Codex / Claude 等）与 REST API 无法连接。开启后按下方网络模式监听，连接需带访问令牌",
                        )
                      : mcpState === "运行中"
                        ? mcp?.netMode === "loopback"
                          ? `${tr("运行中")} · ${mcp?.url}${tr("（仅本机，需令牌）")}`
                          : `${tr("运行中")} · ${tr("本机")}: ${mcp?.url}${mcp?.lanUrl ? ` · ${tr("局域网")}: ${mcp.lanUrl}` : ""}`
                        : mcpState === "启动失败"
                          ? `${tr("启动失败")}: ${trMsg(mcp?.lastError ?? "")}`
                          : tr("正在启动…")
                  }
                  control={<Switch checked={mcp?.enabled === true} onChange={toggleMcp} />}
                />
                {mcp?.enabled && (
                  <>
                    <Row
                      label={tr("网络模式")}
                      desc={
                        mcp.netMode === "loopback"
                          ? tr("本机（默认）：仅本机可访问")
                          : mcp.netMode === "lan"
                            ? tr("局域网：同一网络内的设备可访问（跨设备调用 API、打开分享链接）；公网来源一律拒绝，访问仍需令牌")
                            : tr("任意：不限来源（公网可达与否取决于路由器/防火墙）。仅建议在防火墙保护下使用；首次对外监听时系统防火墙会弹授权框")
                      }
                      control={
                        <select
                          value={mcp.netMode}
                          onChange={(e) =>
                            changeMcpNetMode(e.target.value as McpStatus["netMode"])
                          }
                          disabled={mcpBusy}
                          className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                        >
                          <option value="loopback">{tr("本机")}</option>
                          <option value="lan">{tr("局域网")}</option>
                          <option value="any">{tr("任意")}</option>
                        </select>
                      }
                    />
                    {mcp.netMode !== "loopback" && mcp.lanUrl && (
                      <>
                        <Row
                          label={tr("局域网地址")}
                          desc={tr("同一网络内的其它设备用这个地址访问服务；分享链接与二维码也基于它生成")}
                          control={
                            <div className="flex items-center gap-2">
                              <code className="max-w-[200px] truncate font-mono text-[12px] text-[var(--text-2)]">
                                {mcp.lanUrl}
                              </code>
                              <button
                                className="btn !py-1 text-[11.5px]"
                                onClick={() => copyText(mcp.lanUrl ?? "", tr("已复制局域网地址"))}
                              >
                                {tr("复制地址")}
                              </button>
                              <button
                                className="btn !py-1 text-[11.5px]"
                                onClick={() => setMcpShowLanQr(true)}
                              >
                                {tr("二维码")}
                              </button>
                            </div>
                          }
                        />
                        {mcpShowLanQr && mcp.lanUrl && (
                          <QrModal
                            title={tr("局域网地址")}
                            text={mcp.lanUrl}
                            hint={tr("手机扫码直达服务（分享页与 API 文档上线后可直接扫码打开）")}
                            copyOkText={tr("已复制局域网地址")}
                            onClose={() => setMcpShowLanQr(false)}
                          />
                        )}
                      </>
                    )}
                    <Row
                      label={tr("端口")}
                      desc={tr("服务监听端口（默认 7411，改完自动热重启）。被占用时上方会显示失败原因")}
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
                        "开启后壁纸可响应整个系统的声音（如音乐软件），与 WE 桌面端一致；macOS 需在「隐私与安全性 → 录屏与系统录音 → 仅系统录音」中允许（无自动弹框，改动后需重新开关或重启应用），Windows 无需授权。壁纸自带的音乐无需此开关也会可视化",
                      )
                    : tr("当前平台暂不支持系统音频捕获（macOS CoreAudio / Windows WASAPI / Linux PulseAudio·PipeWire 均已支持）；壁纸自带的音乐无需此开关也会可视化")
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
                desc={tr("看得见就播：壁纸几乎被完全遮挡（全屏应用、最大化窗口、屏保）时自动暂停，重新露出就自动恢复播放；每块屏幕独立判断，与前台应用无关（手动暂停不受影响）")}
                control={<Switch checked={autoPause} onChange={toggleAutoPause} />}
              />
              <Row
                label={tr("暂停释放内存")}
                desc={
                  autoPauseRelease
                    ? tr("开启：自动暂停时直接结束桌面壁纸渲染器以释放内存，回到桌面后完全重新加载壁纸（需开启「自动暂停」）")
                    : tr("关闭：自动暂停仅暂停渲染，壁纸保持在内存中（默认，回到桌面立即恢复）")
                }
                control={<Switch checked={autoPauseRelease} onChange={toggleAutoPauseRelease} />}
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
            </Group>
          )}

          {tab === "general" && (
            <Group title={tr("轮播")}>
              <Row
                label={tr("默认切换间隔")}
                desc={tr("新建切换列表时的默认切换间隔，单个列表可在编辑时修改")}
                control={
                  <div className="flex items-center gap-1.5">
                    <input
                      type="number"
                      min={1}
                      value={plDefaultInterval}
                      onChange={(e) =>
                        void changePlDefaultInterval(Math.max(1, Number(e.target.value) || 1))
                      }
                      className="w-16 rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    />
                    <span className="text-[12px] text-[var(--text-2)]">{tr("分钟")}</span>
                  </div>
                }
              />
              <Row
                label={tr("默认随机播放")}
                desc={tr("新建切换列表时默认开启随机（洗牌播放，一轮内不重复）")}
                control={<Switch checked={plDefaultShuffle} onChange={togglePlDefaultShuffle} />}
              />
              <Row
                label={tr("仅充电时轮播")}
                desc={tr("开启后电池供电时暂缓自动切换，手动切换不受影响（暂仅 macOS）")}
                control={<Switch checked={plPowerOnly} onChange={togglePlPowerOnly} />}
              />
            </Group>
          )}

          {tab === "performance" && (
            <Group title={tr("画质")}>
              <Row
                label={tr("画质档位")}
                desc={
                  preset === "low"
                    ? tr("低：省电优先 — 清晰度 0.75 · 15 FPS · 粒子/后处理低 · 贴图 60% · 法线 75%")
                    : preset === "medium"
                      ? tr("中：均衡 — 清晰度 0.85 · 30 FPS · 粒子/后处理中 · 贴图 80%")
                      : preset === "high"
                        ? tr("高：画质优先 — 清晰度 1.0 · 30 FPS · 粒子/后处理高 · 贴图/法线原生")
                        : tr("自定义：手动调整下方任意参数即进入自定义。点档位一键套用预设，整体覆盖下方画质参数（显示模式/滤镜等观感设置不动）")
                }
                control={
                  <div className="flex items-center gap-2">
                    <div className="flex overflow-hidden rounded-lg border border-[var(--separator)]">
                      {(["low", "medium", "high", "custom"] as const).map((id) => (
                        <button
                          key={id}
                          type="button"
                          disabled={id === "custom" && preset !== "custom"}
                          onClick={() => void applyQualityPreset(id)}
                          className={`px-3 py-1 text-[12.5px] transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${
                            preset === id
                              ? "bg-[var(--accent-fill)] text-white"
                              : "bg-[var(--content)] text-[var(--text-2)] hover:text-[var(--text)]"
                          }`}
                        >
                          {tr(PRESET_LABELS[id])}
                        </button>
                      ))}
                    </div>
                    {presetMsg && <span className="text-[12px] text-red-500">{presetMsg}</span>}
                  </div>
                }
              />
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
                desc={tr("渲染分辨率相对屏幕像素比的倍率（0.50–1.00，1=原生），越低越省显存")}
                control={
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={0.5}
                      max={1}
                      step={0.05}
                      value={renderDpr}
                      onChange={(e) => changeRenderDpr(Number(e.target.value))}
                      className={SLIDER_CLS}
                    />
                    <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                      {renderDpr >= 1 ? tr("原生") : `×${renderDpr.toFixed(2)}`}
                    </span>
                    {renderDprMsg && <span className="text-[12px] text-red-500">{renderDprMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("视频纹理清晰度")}
                desc={tr(
                  "场景里的视频纹理每帧上传的清晰度。自动 = 按实测帧率往下压（推荐：WKWebView 下逐帧上传要同步取像素，全屏视频层是掉帧主因）；固定档在视频清晰度与流畅度之间手动取舍，改动即时生效",
                )}
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={String(videoTexScale)}
                      onChange={(e) => changeVideoTexScale(Number(e.target.value))}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      <option value="0">{tr("自动（按帧率）")}</option>
                      <option value="1">{tr("高清 ×1.00")}</option>
                      <option value="0.7">{tr("标准 ×0.70")}</option>
                      <option value="0.5">{tr("省电 ×0.50")}</option>
                    </select>
                    {videoTexScaleMsg && (
                      <span className="text-[12px] text-red-500">{videoTexScaleMsg}</span>
                    )}
                  </div>
                }
              />
              <Row
                label={tr("贴图倍率")}
                desc={tr("贴图解码/上传的分辨率倍率（0.50–1.00，1=原生）：去掉看不出来的过采样，画面逐像素不变但省显存。改动后壁纸重载一次")}
                control={
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={0.5}
                      max={1}
                      step={0.05}
                      value={resources}
                      onChange={(e) => changeResources(Number(e.target.value))}
                      className={SLIDER_CLS}
                    />
                    <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                      {resources >= 1 ? tr("原生") : `×${resources.toFixed(2)}`}
                    </span>
                    {resourcesMsg && <span className="text-[12px] text-red-500">{resourcesMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("法线倍率")}
                desc={tr("法线/蒙版贴图的分辨率倍率（0.35–1.00，默认 1 不缩）：折射与光照对模糊敏感，非必要不动。改动后壁纸重载一次")}
                control={
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={0.35}
                      max={1}
                      step={0.05}
                      value={resourcesNormal}
                      onChange={(e) => changeResourcesNormal(Number(e.target.value))}
                      className={SLIDER_CLS}
                    />
                    <span className="w-12 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                      {resourcesNormal >= 1 ? tr("原生") : `×${resourcesNormal.toFixed(2)}`}
                    </span>
                    {resourcesNormalMsg && (
                      <span className="text-[12px] text-red-500">{resourcesNormalMsg}</span>
                    )}
                  </div>
                }
              />
              <Row
                label={tr("帧率上限")}
                desc={tr("场景动画的帧率上限（15–60 FPS 任意值）：越低 GPU 占用越低")}
                control={
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={15}
                      max={60}
                      step={1}
                      value={sceneFps}
                      onChange={(e) => changeSceneFps(Number(e.target.value))}
                      className={SLIDER_CLS}
                    />
                    <span className="w-14 text-right text-[12px] tabular-nums text-[var(--text-2)]">
                      {sceneFps} FPS
                    </span>
                    {sceneFpsMsg && <span className="text-[12px] text-red-500">{sceneFpsMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("抗锯齿")}
                desc={tr("抗锯齿方案优化中：当前所有档位一律关闭且禁止更改（FXAA/MSAA 在部分壁纸上有瑕疵），后续版本开放")}
                control={
                  <select
                    disabled
                    value="off"
                    className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] opacity-60 outline-none"
                  >
                    <option value="off">{tr("关（已锁定）")}</option>
                  </select>
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
                    <input
                      type="range"
                      min={0}
                      max={3}
                      step={1}
                      value={Math.max(
                        0,
                        PARTICLE_STOPS.indexOf(particles as (typeof PARTICLE_STOPS)[number]),
                      )}
                      onChange={(e) => changeParticles(PARTICLE_STOPS[Number(e.target.value)])}
                      className={SLIDER_CLS}
                    />
                    <span className="w-8 text-right text-[12px] text-[var(--text-2)]">
                      {tr(
                        particles === "high"
                          ? "高"
                          : particles === "medium"
                            ? "中"
                            : particles === "low"
                              ? "低"
                              : "关",
                      )}
                    </span>
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
                      : post === "off"
                        ? tr("关：效果链直通（辉光/水波等画面效果全无，最省性能）")
                        : tr("低：效果链分辨率减半，显存占用约 1/4")
                }
                control={
                  <div className="flex items-center gap-2">
                    <input
                      type="range"
                      min={0}
                      max={3}
                      step={1}
                      value={Math.max(0, POST_STOPS.indexOf(post as (typeof POST_STOPS)[number]))}
                      onChange={(e) => changePost(POST_STOPS[Number(e.target.value)])}
                      className={SLIDER_CLS}
                    />
                    <span className="w-8 text-right text-[12px] text-[var(--text-2)]">
                      {tr(
                        post === "high" ? "高" : post === "medium" ? "中" : post === "off" ? "关" : "低",
                      )}
                    </span>
                    {postMsg && <span className="text-[12px] text-red-500">{postMsg}</span>}
                  </div>
                }
              />
              <Row
                label={tr("滤镜")}
                desc={tr("整个画面的色彩效果，实时热切、不重载壁纸；与托盘菜单「滤镜效果」是同一设置")}
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={filter}
                      onChange={(e) => void changeFilter(e.target.value)}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                    >
                      {FILTER_OPTIONS.map((o) => (
                        <option key={o.id} value={o.id}>
                          {tr(o.label)}
                        </option>
                      ))}
                    </select>
                    {filterMsg && <span className="text-[12px] text-red-500">{filterMsg}</span>}
                  </div>
                }
              />
            <Row
              label={tr("切换效果")}
              desc={
                reveal === "fade"
                  ? tr("叠化（默认）：新壁纸淡入盖过旧壁纸。换到另一张壁纸时生效")
                  : reveal === "zoom"
                    ? tr("推近：新壁纸从 130% 缩回原位淡入。换到另一张壁纸时生效")
                    : reveal === "blur"
                      ? tr("模糊：新壁纸由重失焦变清晰淡入。换到另一张壁纸时生效")
                      : reveal === "depth"
                        ? tr("景深：推近与失焦同时收拢，观感更立体。换到另一张壁纸时生效")
                        : reveal === "circle"
                          ? tr("圆形揭示：新壁纸光圈从屏幕中心展开。换到另一张壁纸时生效")
                          : reveal === "wipe"
                            ? tr("横向擦除：新壁纸从左向右擦出，覆盖旧壁纸。换到另一张壁纸时生效")
                            : tr("滑入：新壁纸整幅从右侧滑入，覆盖旧壁纸。换到另一张壁纸时生效")
              }
              control={
                <div className="flex items-center gap-2">
                  <select
                    value={reveal}
                    onChange={(e) => void changeReveal(e.target.value)}
                    className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
                  >
                    <option value="fade">{tr("叠化")}</option>
                    <option value="zoom">{tr("推近")}</option>
                    <option value="blur">{tr("模糊")}</option>
                    <option value="depth">{tr("景深")}</option>
                    <option value="circle">{tr("圆形揭示")}</option>
                    <option value="wipe">{tr("横向擦除")}</option>
                    <option value="slide">{tr("滑入")}</option>
                  </select>
                  {revealMsg && <span className="text-[12px] text-red-500">{revealMsg}</span>}
                </div>
              }
            />
            <Row
              label={tr("官方素材（Wallpaper Engine）")}
                desc={
                  localAssets?.available
                    ? tr(
                        "已探测到 Wallpaper Engine 官方素材树：粒子/光效/渐变与文字字体按原版像素渲染（{count} 张贴图）。关闭后使用内置程序化复刻",
                        { count: localAssets.texCount },
                      )
                    : tr(
                        "本机安装 Wallpaper Engine 后，粒子/光效/渐变与文字字体可按官方原版像素渲染；未安装时使用内置程序化复刻，观感接近但不完全一致。默认开启，探测不到素材时无额外开销",
                      )
                }
                control={
                  <Switch
                    checked={localAssets?.enabled ?? true}
                    disabled={localAssetsBusy}
                    onChange={(v) => void toggleLocalAssets(v)}
                  />
                }
              />
              {localAssets?.enabled && (
                <>
                  <div className="flex items-center gap-2 px-3 pb-1 text-[12px]">
                    {localAssets?.available ? (
                      <span className="break-all text-green-400">
                        {tr("素材根")}: {localAssets.root}
                      </span>
                    ) : (
                      <span className="text-[var(--text-2)]">
                        {tr("未探测到官方素材树（Steam 库的 wallpaper_engine/assets）；可在下方手动指定 assets 目录")}
                      </span>
                    )}
                  </div>
                  <Row
                    label={tr("自定义素材目录")}
                    desc={tr("可选。直接包含 materials/ 子目录的 Wallpaper Engine assets 根；留空则自动探测所有 Steam 库。修改后壁纸重载一次")}
                    control={
                      <div className="flex w-80 items-center gap-2">
                        <input
                          type="text"
                          value={weAssetsDir}
                          spellCheck={false}
                          placeholder={tr("自动探测，可手动填写绝对路径")}
                          onChange={(e) => setWeAssetsDir(e.target.value)}
                          className="min-w-0 flex-1 rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12px] outline-none focus:border-[var(--accent-strong)]"
                        />
                        <button
                          type="button"
                          className="btn !py-1 text-[11.5px] disabled:opacity-50"
                          disabled={localAssetsBusy}
                          onClick={() => void pickWeAssetsDir()}
                        >
                          {tr("选择文件夹")}
                        </button>
                        <button
                          type="button"
                          className="btn !py-1 text-[11.5px] disabled:opacity-50"
                          disabled={localAssetsBusy}
                          onClick={() => void saveWeAssetsDir()}
                        >
                          {tr("保存")}
                        </button>
                      </div>
                    }
                  />
                </>
              )}
              {localAssetsMsg && (
                <div className="px-3 pb-1 text-[12px] text-red-500">{trMsg(localAssetsMsg)}</div>
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
const MEDIA_BRIDGE_URL = "https://github.com/oneincase/media-bridge";

/** 「关于」面板上次的检查结果：切标签会重挂组件，缓存一下避免每次进页面都打更新清单端点 */
let aboutCheckCache: UpdateInfo | null = null;

/**
 * 「关于」面板：应用信息 + 更新检查 / 下载安装 / 重启生效 + 相关链接。
 *
 * 更新走官方 updater 插件：「读 Release 里的 latest-{target}-{arch}.json 清单 →
 * 下载并校验签名（进度 update:progress）→ 平台原地安装 → 重启生效」，
 * Windows 装完由安装器自动重启本体（见 src-tauri/src/update.rs）。
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
  const [busy, setBusy] = useState(false);
  /** 下载完成进入安装阶段（update:phase 事件推进；Windows 上装完进程直接退出） */
  const [installing, setInstalling] = useState(false);
  /** 安装已完成（macOS/Linux 待用户点重启；Windows 到不了这个状态） */
  const [installed, setInstalled] = useState(false);

  const check = useCallback(async () => {
    setPhase("checking");
    setErrMsg("");
    setInstalling(false);
    setInstalled(false);
    setProg(null);
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
    // 有缓存就不再自动打一次端点（切标签回来时）
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

  // 下载结束进入安装阶段时，Rust 侧发 `update:phase`
  useEffect(() => {
    const un = listen<{ phase: string }>("update:phase", (e) => {
      if (e.payload.phase === "installing") setInstalling(true);
    });
    return () => {
      void un.then((f) => f());
    };
  }, []);

  const downloadInstall = useCallback(async () => {
    setBusy(true);
    setErrMsg("");
    setInstalling(false);
    setInstalled(false);
    setProg({ received: 0, total: 0 });
    try {
      await api.appUpdateDownloadInstall();
      setInstalled(true);
    } catch (e) {
      setErrMsg(String(e));
    } finally {
      setBusy(false);
      setInstalling(false);
      setProg(null);
    }
  }, []);

  const restart = useCallback(async () => {
    setErrMsg("");
    try {
      await api.appUpdateRestart();
    } catch (e) {
      setErrMsg(String(e));
    }
  }, []);

  const pct = prog && prog.total > 0 ? Math.min(100, Math.round((prog.received / prog.total) * 100)) : null;
  const osLabel =
    info?.os === "macos" ? "macOS" : info?.os === "windows" ? "Windows" : info?.os === "linux" ? "Linux" : (info?.os ?? "");
  const pubDate = (() => {
    if (!upd?.publishedAt) return "";
    const d = new Date(upd.publishedAt);
    return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString();
  })();

  const linkBtn =
    "rounded-lg border border-[var(--separator)] px-2.5 py-1 text-[12.5px] text-[var(--text-2)] transition-colors hover:bg-white/8 hover:text-[var(--text-1)]";
  const primaryBtn =
    "rounded-lg bg-[var(--accent-fill)] px-3 py-1.5 text-[12.5px] font-medium text-white transition-opacity hover:opacity-90 disabled:opacity-40";

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
                ? tr("macOS 动态壁纸引擎 · 极致优雅的开源壁纸软件，绝非单纯的WE复刻")
                : tr("跨平台动态壁纸引擎 · 极致优雅的开源壁纸软件，绝非单纯的WE复刻")}
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
            disabled={phase === "checking" || busy || installing || installed}
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
          {phase === "available" &&
            (installing
              ? tr("正在安装更新…")
              : installed
                ? tr("更新已安装，重启软件后生效")
                : upd && tr("发现新版本 {v}", { v: `v${upd.latest}` }))}
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
              <div className="rounded-lg border border-[var(--separator)] bg-white/5">
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
                    className="h-full rounded-full bg-[var(--accent-fill)] transition-[width] duration-200"
                    style={{ width: pct === null ? "30%" : `${pct}%` }}
                  />
                </div>
                <div className="text-[11.5px] text-[var(--text-2)]">
                  {installing
                    ? tr("下载完成，正在校验签名并安装…")
                    : `${tr("下载中…")}${
                        prog.total > 0
                          ? ` ${formatBytes(prog.received)} / ${formatBytes(prog.total)}${pct !== null ? ` · ${pct}%` : ""}`
                          : ` ${formatBytes(prog.received)}`
                      }`}
                </div>
              </div>
            )}

            <div className="flex flex-wrap items-center gap-2">
              {!installed && (
                <button
                  className={primaryBtn}
                  disabled={busy || installing}
                  onClick={() => void downloadInstall()}
                >
                  {busy ? (installing ? tr("正在安装更新…") : tr("下载中…")) : tr("下载更新并安装")}
                </button>
              )}
              {installed && (
                <button className={primaryBtn} onClick={() => void restart()}>
                  {tr("重启并完成更新")}
                </button>
              )}
              <button className={linkBtn} onClick={() => void openUrl(upd.htmlUrl || RELEASES_URL)}>
                {tr("前往下载页")}
              </button>
            </div>

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
          <button className={linkBtn} onClick={() => void openUrl(MEDIA_BRIDGE_URL)}>
            {tr("第三方组件")}
          </button>
        </div>
        <div className="mt-3 text-[11.5px] leading-relaxed text-[var(--text-2)] opacity-80">
          {tr("以 MIT 许可开源。系统「正在播放」与系统音频采集基于同作者的开源组件 media-bridge（MIT）。")}
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
