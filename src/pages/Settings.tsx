import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  type DownloadToolStatus,
  type SteamcmdInstallProgress,
} from "../api/steam";
import { getSidebarAlpha, setSidebarAlpha } from "../lib/sidebar";
import { ConfirmModal } from "../components/ConfirmModal";
import { useMessage } from "../components/Message";

// 设置页标签：账号 / 通用 / 网络 / 关于
// （id 沿用 "download"：该页内容是下载工具安装 + 下载账号登录，改 id 无收益）
type SettingsTab = "download" | "general" | "network" | "about";
const SETTINGS_TABS: { id: SettingsTab; label: string }[] = [
  { id: "download", label: "账号" },
  { id: "general", label: "通用" },
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
  // 全局清晰度（有效 dpr 封顶，越低越省内存）。三档 0.8 / 1 / 2，
  // 默认 1 标准（与 Rust DEFAULT_RENDER_DPR 一致）
  const [renderDpr, setRenderDpr] = useState<number>(1);
  const [renderDprMsg, setRenderDprMsg] = useState("");
  // 全局场景帧率上限（30/60/120，越低 GPU 占用越低），默认 60
  const [sceneFps, setSceneFps] = useState<number>(60);
  const [sceneFpsMsg, setSceneFpsMsg] = useState("");
  // 壁纸语言（只影响壁纸的 language 属性，不是软件本体语言）。默认简体中文
  const [language, setLanguage] = useState<string>("simplifiedchinese");
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
  // 运行平台（macos/linux/...）：平台相关文案与不可用功能的提示
  const [os, setOs] = useState<string>("macos");
  // 系统音频捕获平台支持性（Linux 暂未支持，开关给出明确提示而不是报权限错误）
  const [audioSupported, setAudioSupported] = useState(true);

  useEffect(() => {
    invoke<boolean>("autostart_status").then(setAutostart).catch(console.warn);
    invoke<{ os: string }>("app_info")
      .then((i) => setOs(i.os))
      .catch(console.warn);
    api
      .wallpaperAudioProcessingStatus()
      .then((s) => setAudioSupported(s.supported))
      .catch(console.warn);
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
      .then((v) => setRenderDpr(Number(v) || 1))
      .catch(() => { });
    invoke<string | null>("settings_get", { key: "wallpaper_scene_fps" })
      .then((v) => setSceneFps(Number(v) || 60))
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

  const saveCredentials = async () => {
    setCredMsg("");
    if (!dlUser || !dlPass) {
      setCredMsg("请填写用户名与密码");
      return;
    }
    try {
      await api.downloadCredentialsSet(dlUser, dlPass);
      setCred({ configured: true, username: dlUser });
      setDlPass("");
      setEditingCred(false);
      setCredMsg("✅ 已保存（密码本地加密存储）");
    } catch (e) {
      setCredMsg(String(e));
    }
  };

  const logout = async () => {
    try {
      await api.downloadCredentialsClear();
      setCred({ configured: false });
      setDlUser("");
      setDlPass("");
      setCredMsg("");
      msg.success("已登出，账号与登录态已清除");
    } catch (e) {
      msg.error(String(e));
    }
  };

  // 安装 / 修复 steamcmd
  const installSteamcmd = async (force = false) => {
    setInstalling(true);
    setInstallMsg("准备安装…");
    try {
      await api.steamcmdInstall(force);
      setTool(await api.downloadToolStatus());
      setInstallMsg("✅ steamcmd 已就绪");
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
        phase === "download" ? "下载中" : phase === "extract" ? "解压中" : "初始化中";
      setInstallMsg(`${label}：${message}`);
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
      setProxyMsg("✅ 已保存（工坊访问重启应用后生效）");
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
      setDiagMsg(`✅ 诊断包已导出：${p}`);
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

  // 音频可视化（系统音频处理）：开启时触发屏幕录制授权，授权后系统音乐驱动可视化
  const toggleAudioProcessing = async () => {
    const next = !audioProcessing;
    setAudioMsg("");
    if (next && !audioSupported) {
      setAudioMsg("当前平台暂不支持系统音频捕获（macOS 已支持；Linux 待后续版本接入 PipeWire）");
      return;
    }
    try {
      const s = await api.wallpaperAudioProcessingSet(next);
      setAudioProcessing(s.enabled);
      if (s.enabled && !s.granted) {
        setAudioMsg(
          "⚠️ 权限尚未生效：① 系统设置 → 隐私与安全性 → 屏幕录制 → 允许 WallpaperEM；" +
            "② 完全退出应用（⌘Q）再重新打开（运行中的进程不会自动获得新授权）；" +
            "③ 若列表里已开启但重启后仍无效，先在列表中选中 WallpaperEM 按「−」移除，再重新添加并允许"
        );
      } else if (s.enabled && s.running) {
        setAudioMsg("✅ 系统音频分析已开启");
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

  // 语言在挂载时才注入（壁纸 project.json 没有自带 language 属性时生效），
  // 改完需要重新应用壁纸；不像清晰度/帧率能实时热切
  const changeLanguage = async (next: string) => {
    const prev = language;
    setLanguage(next); // 乐观更新
    try {
      await api.wallpaperSetLanguage(next);
    } catch (e) {
      setLanguage(prev);
      msg.error(`语言设置失败：${String(e)}`);
    }
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部 + 标签栏：与内容列同宽、整体居中 */}
      <div className="shrink-0 w-full">
        <div className="flex items-center justify-between gap-4 flex-wrap">
          <div>
            <h1 className="text-[22px] font-bold tracking-tight">设置</h1>
            <p className="text-[13px] text-[var(--text-2)] mt-0.5">按标签切换设置分类</p>
          </div>
        </div>
      </div>
      <div className="w-full mt-5 inline-flex gap-0.5 p-0.5 items-center justify-center">
        {SETTINGS_TABS.map((t) => (
          <button
            key={t.id}
            onClick={() => setTab(t.id)}
            className={`rounded-[7px] px-4 py-[5px] text-[13px] font-medium transition-colors ${tab === t.id
              ? "bg-[var(--accent)] text-white shadow-sm"
              : "text-[var(--text-2)] hover:bg-black/5 dark:hover:bg-white/8"
              }`}
          >
            {t.label}
          </button>
        ))}
      </div>
      {/* 内容区域：整列水平居中（不贴最左边）；内容不足一屏时垂直居中，超出则滚动 */}
      <div className="min-h-0 overflow-y-auto flex flex-col">
        <div className="w-full p-5  mx-auto my-auto space-y-5">
          {tab === "download" && (
            <Group title="下载账号">
              <Row
                label="下载工具"
                desc={
                  tool?.rosettaMissing
                    ? "缺少 Rosetta 2：steamcmd 的官方引导程序是 x86_64，首次启动需要它。请在终端执行 softwareupdate --install-rosetta --agree-to-license 后重试（首次自更新后 steamcmd 即以原生 arm64 运行）"
                    : tool?.installed
                      ? `steamcmd 已就绪${tool.version ? ` · 版本 ${tool.version}` : ""}${tool.path ? ` · ${tool.path}` : ""}`
                      : tool?.downloaded
                        ? "已下载但未完成初始化，请点击「修复」重试"
                        : "Valve 官方 steamcmd。尚未安装，点击「安装」从官方源下载（约 2.5 MB 引导包，初始化后约 85 MB）"
                }
                control={
                  <div className="flex items-center gap-2">
                    <span
                      className={`text-[12px] font-medium ${tool?.installed ? "text-green-500" : "text-red-500"}`}
                    >
                      {tool?.installed ? "就绪" : "未就绪"}
                    </span>
                    <button
                      className="btn !py-1 text-[11.5px]"
                      disabled={installing || tool?.rosettaMissing}
                      onClick={() => installSteamcmd(!!tool?.installed)}
                    >
                      {installing ? "安装中…" : tool?.installed ? "修复" : "安装"}
                    </button>
                  </div>
                }
              />
              {installMsg && (
                <div className="mt-1.5 break-all text-[12px] text-[var(--text-2)]">{installMsg}</div>
              )}

              <Row
                label="下载账号"
                desc={
                  cred?.configured
                    ? `已登录：${cred.username}（需拥有 Wallpaper Engine，下载不再重复验证）`
                    : "下载工坊内容需拥有 WE 的 Steam 账号。首次下载会要求输入 Steam Guard 验证码，之后记住登录态。注意：steamcmd 登录会挤掉你正在运行的 Steam 客户端（同账号同时只能登录一处）"
                }
                control={
                  <div className="flex items-center gap-2 flex-wrap">
                    <span
                      className={`text-[12px] font-medium ${cred?.configured ? "text-green-500" : "text-[var(--text-2)]"}`}
                    >
                      {cred?.configured ? "已登录" : "未登录"}
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
                        重新配置
                      </button>
                    )}
                    {cred?.configured && (
                      <button
                        className="btn btn-danger !py-1 text-[11.5px]"
                        onClick={() => setConfirmLogout(true)}
                      >
                        登出
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
                    placeholder="Steam 账号（登录名或邮箱）"
                    className="w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent)]"
                  />
                  <input
                    type="password"
                    value={dlPass}
                    onChange={(e) => setDlPass(e.target.value)}
                    placeholder="密码"
                    className="w-full rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent)]"
                  />
                  <div className="flex items-center gap-2">
                    <button className="btn btn-primary" onClick={saveCredentials}>
                      保存凭据
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
                        取消
                      </button>
                    )}
                    {credMsg && <span className="text-[12px] text-[var(--text-2)]">{credMsg}</span>}
                  </div>
                  <p className="text-[11.5px] text-[var(--text-2)]">
                    密码本地加密存储。首次下载时若需要 Steam Guard 验证码，会在下载页弹窗提示输入。
                  </p>
                </div>
              )}
            </Group>
          )}

          {tab === "network" && (
            <>
              <Group title="代理">
                <Row
                  label="跟随系统代理"
                  desc={
                    followSystemProxy
                      ? os === "macos"
                        ? "开启：自动使用 macOS「系统设置 → 网络 → 代理」中的配置访问创意工坊（默认开启）"
                        : "开启：自动使用系统代理环境变量中的配置访问创意工坊（默认开启）"
                      : "关闭：绕过系统代理，直连网络访问创意工坊"
                  }
                  control={<Switch checked={followSystemProxy} onChange={toggleFollowSystemProxy} />}
                />
                <Row
                  label="手动代理"
                  desc="可选，优先级高于系统代理；如 http://127.0.0.1:7890（大陆网络访问 Steam 建议配置）；工坊访问重启应用后生效"
                  control={null}
                />
                <div className="mt-1 flex items-center gap-2">
                  <input
                    value={proxy}
                    onChange={(e) => setProxy(e.target.value)}
                    placeholder="http://127.0.0.1:7890"
                    className="flex-1 rounded-lg border border-[var(--separator)] bg-[var(--content)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent)]"
                  />
                  <button className="btn" onClick={saveProxy}>
                    保存
                  </button>
                  {proxyMsg && <span className="text-[12px] text-[var(--text-2)]">{proxyMsg}</span>}
                </div>
              </Group>

              <Group title="网络与诊断">
                <Row
                  label="Steam 网络连通性"
                  desc="逐主机 TLS 探测（社区 / API / CDN / 内容服务器）"
                  control={
                    <button className="btn" onClick={runProbe} disabled={probing}>
                      {probing ? "探测中…" : "开始探测"}
                    </button>
                  }
                />
                {probe && (
                  <div className="mt-2 space-y-1">
                    {probe.results.map((r) => (
                      <div key={r.host} className="flex items-center gap-2 text-[12.5px]">
                        <span className={`h-2 w-2 rounded-full ${r.ok ? "bg-green-500" : "bg-red-500"}`} />
                        <span className="w-64 truncate">{r.label}</span>
                        <span className="text-[var(--text-2)]">{r.ok ? `${r.ms}ms` : "不通"}</span>
                      </div>
                    ))}
                    <div className="text-[12px] text-[var(--text-2)] pt-1">{probe.hint}</div>
                  </div>
                )}
                <Row
                  label="导出诊断包"
                  desc="日志 + 数据库结构 + 环境信息 + 网络探测 → zip（排障用）"
                  control={
                    <button className="btn" onClick={doDiagnostics}>
                      导出
                    </button>
                  }
                />
                {diagMsg && <div className="mt-1 text-[12px] text-[var(--text-2)] break-all">{diagMsg}</div>}
              </Group>
            </>
          )}

          {tab === "general" && (
            <Group title="通用">
              <Row
                label="开机自启"
                desc={os === "macos" ? "登录 macOS 时自动启动本应用" : "登录系统时自动启动本应用"}
                control={<Switch checked={autostart === true} onChange={toggleAutostart} />}
              />
              <Row
                label="显示模式"
                desc={
                  fit === "cover"
                    ? "等比铺满并居中裁切溢出（不变形、无黑边，默认）"
                    : fit === "contain"
                      ? "等比缩放完整显示，边缘留暗边（不变形）"
                      : "忽略宽高比铺满（会拉伸变形，旧行为）"
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={fit}
                      onChange={(e) => changeFit(e.target.value as "cover" | "contain" | "stretch")}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent)]"
                    >
                      <option value="cover">裁剪</option>
                      <option value="contain">缩放</option>
                      <option value="stretch">拉伸</option>
                    </select>
                    {fitMsg && <span className="text-[12px] text-red-500">{fitMsg}</span>}
                  </div>
                }
              />
              <Row
                label="清晰度"
                desc="越高越清晰，显存占用也越高。实际生效值不超过屏幕像素比。默认标准"
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={renderDpr}
                      onChange={(e) => changeRenderDpr(Number(e.target.value))}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent)]"
                    >
                      <option value={0.8}>省电</option>
                      <option value={1}>标准</option>
                      <option value={2}>高清</option>
                    </select>
                    {renderDprMsg && <span className="text-[12px] text-red-500">{renderDprMsg}</span>}
                  </div>
                }
              />
              <Row
                label="帧率限制"
                desc={
                  sceneFps <= 30
                    ? "30 FPS：GPU 占用最低，场景动画/视差略卡"
                    : sceneFps >= 120
                      ? "120 FPS：最流畅，GPU 占用最高（需高刷屏才看得出）"
                      : "60 FPS：默认，画质与 GPU 占用均衡"
                }
                control={
                  <div className="flex items-center gap-2">
                    <select
                      value={sceneFps}
                      onChange={(e) => changeSceneFps(Number(e.target.value))}
                      className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent)]"
                    >
                      <option value={30}>30 FPS</option>
                      <option value={60}>60 FPS</option>
                      <option value={120}>120 FPS</option>
                    </select>
                    {sceneFpsMsg && <span className="text-[12px] text-red-500">{sceneFpsMsg}</span>}
                  </div>
                }
              />
              <Row
                label="语言"
                desc="壁纸的语言偏好。壁纸自带语言设置时以壁纸为准；仅对没有该设置的壁纸生效。改动需重新应用壁纸，暂不影响软件界面"
                control={
                  <select
                    value={language}
                    onChange={(e) => void changeLanguage(e.target.value)}
                    className="rounded-lg border border-[var(--separator)] bg-[var(--content)] px-2 py-1 text-[12.5px] outline-none focus:border-[var(--accent)]"
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
                label="音频可视化（系统声音）"
                desc={
                  audioSupported
                    ? "开启后壁纸可响应整个系统的声音（如音乐软件），与 WE 桌面端一致；需授予屏幕录制权限。壁纸自带的音乐无需此开关也会可视化"
                    : "当前平台暂不支持系统音频捕获（Linux 待接入 PipeWire）；壁纸自带的音乐无需此开关也会可视化"
                }
                control={<Switch checked={audioProcessing} onChange={toggleAudioProcessing} />}
              />
              {audioMsg && (
                <div className="text-[12px] text-[var(--text-2)]">{audioMsg}</div>
              )}
              <Row
                label="自动设置系统壁纸"
                desc={os === "macos" ? "应用动态壁纸后，自动抽首帧（scene/web 用工坊预览图）设为 macOS 静态壁纸：锁屏、登录窗口与壁纸引擎未运行时保持视觉一致" : "应用动态壁纸后，自动抽首帧设为系统静态壁纸（依赖 ffmpeg 与桌面环境的壁纸接口）：壁纸引擎未运行时保持视觉一致"}
                control={<Switch checked={autoSystemStatic} onChange={toggleAutoSystemStatic} />}
              />
              <Row
                label="自动暂停"
                desc="切到非桌面应用时自动暂停壁纸，切回桌面时自动播放（手动暂停不受影响）"
                control={<Switch checked={autoPause} onChange={toggleAutoPause} />}
              />
              <Row
                label="隐藏图标"
                desc={
                  interactive
                    ? "开启：壁纸窗口位于桌面图标之上（会盖住桌面图标，用于场景视差/网页互动）"
                    : "关闭：壁纸窗口位于桌面图标下方、壁纸上方，桌面图标正常显示（默认）"
                }
                control={<Switch checked={interactive} onChange={toggleInteractive} />}
              />
              {/* 侧边栏透明度：入口暂时隐藏（默认固定 55%，见 lib/sidebar.ts）。
                  代码保留，需要恢复时去掉这层注释即可（changeSidebarAlpha 也在）。
              <Row
                label="侧边栏透明度"
                desc={`调节左侧菜单栏的半透明/磨砂质感（${Math.round(sidebarAlpha * 100)}%）。默认 55%`}
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

          {tab === "about" && (
            <Group title="关于">
              <Row
                label="WallpaperEM"
                desc={os === "macos" ? "macOS 动态壁纸引擎 · 浏览/下载并应用 Steam 创意工坊壁纸（视频 / 场景 / 网页 / 图片）" : "跨平台动态壁纸引擎 · 浏览/下载并应用 Steam 创意工坊壁纸（视频 / 场景 / 网页 / 图片）"}
                control={null}
              />
            </Group>
          )}
        </div>
      </div>

      {confirmLogout && (
        <ConfirmModal
          title="登出下载账号"
          message="将清除本地保存的账号密码与 steamcmd 登录态，下次下载需要重新登录并再过一次 Steam Guard 验证。"
          confirmText="登出"
          danger
          onCancel={() => setConfirmLogout(false)}
          onConfirm={() => {
            setConfirmLogout(false);
            logout();
          }}
        />
      )}
    </div>
  );
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
      className={`relative inline-flex h-[22px] w-[38px] shrink-0 items-center rounded-full transition-colors ${checked ? "bg-[var(--accent)]" : "bg-[var(--separator)]"
        }`}
    >
      <span
        className={`inline-block h-[18px] w-[18px] transform rounded-full bg-white shadow transition-transform ${checked ? "translate-x-[18px]" : "translate-x-[2px]"
          }`}
      />
    </button>
  );
}
