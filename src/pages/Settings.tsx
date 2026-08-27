import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { api } from "../api/steam";

export function SettingsPage() {
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [interactive, setInteractive] = useState(false);
  // 下载账号
  const [cred, setCred] = useState<{ configured: boolean; username?: string } | null>(null);
  const [editingCred, setEditingCred] = useState(false);
  const [tool, setTool] = useState<{ installed: boolean; path?: string; version?: string } | null>(null);
  const [dlUser, setDlUser] = useState("");
  const [dlPass, setDlPass] = useState("");
  const [credMsg, setCredMsg] = useState("");
  // 代理
  const [proxy, setProxy] = useState("");
  const [proxyMsg, setProxyMsg] = useState("");
  const [followSystemProxy, setFollowSystemProxy] = useState(true);

  useEffect(() => {
    invoke<boolean>("autostart_status").then(setAutostart).catch(console.warn);
    invoke<string | null>("settings_get", { key: "wallpaper_interactive" })
      .then((v) => setInteractive(v === "true" || v === "1"))
      .catch(() => {});
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
      .catch(() => {});
    invoke<string | null>("settings_get", { key: "download_proxy" })
      .then((p) => setProxy(p ?? ""))
      .catch(() => {});
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

  const toggleInteractive = async () => {
    const next = !interactive;
    try {
      await api.wallpaperInteractiveSet(next);
      setInteractive(next);
    } catch {
      // 失败则不变
    }
  };

  return (
    <div className="flex flex-col h-full px-7 py-5">
      {/* 头部区域 - 固定在顶部 */}
      <div className="shrink-0 max-w-2xl space-y-5 mb-4">
        <div>
          <h1 className="text-[22px] font-bold tracking-tight">设置</h1>
          <p className="text-[13px] text-[var(--text-2)] mt-1">下载 / 网络 / 通用</p>
        </div>
      </div>

      {/* 内容区域 - 可滚动 */}
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-2xl space-y-5">
          <Group title="下载">
        <Row
          label="下载账号（DepotDownloader）"
          desc={
            cred?.configured
              ? `已配置：${cred.username}（需拥有 Wallpaper Engine）`
              : "下载工坊内容需要拥有 WE 的 Steam 账号；密码本地加密存储（不依赖 macOS 钥匙串授权）"
          }
          control={
            <div className="flex items-center gap-2">
              <span
                className={`text-[12px] font-medium ${cred?.configured ? "text-green-500" : "text-[var(--text-2)]"}`}
              >
                {cred?.configured ? "已配置" : "未配置"}
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
          </div>
        )}
        <Row
          label="下载工具"
          desc={
            tool?.installed
              ? `DepotDownloader ${tool.version ?? ""}（已打包 sidecar）`
              : "DepotDownloader 未就绪"
          }
          control={
            <span className={`text-[12px] font-medium ${tool?.installed ? "text-green-500" : "text-red-500"}`}>
              {tool?.installed ? "就绪" : "缺失"}
            </span>
          }
        />
        <Row
          label="跟随系统代理"
          desc={
            followSystemProxy
              ? "开启：自动使用 macOS「系统设置 → 网络 → 代理」中的配置访问创意工坊（默认开启）"
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

      <Group title="通用">
        <Row
          label="开机自启"
          desc="登录 macOS 时自动启动本应用"
          control={<Switch checked={autostart === true} onChange={toggleAutostart} />}
        />
        <Row
          label="壁纸交互（图标上方）"
          desc="开启后壁纸窗口置于桌面图标之上并可接收鼠标（场景视差/网页互动）；会盖住桌面图标。默认关闭"
          control={<Switch checked={interactive} onChange={toggleInteractive} />}
        />
      </Group>

      <Group title="关于">
        <Row
          label="WallpaperEM"
          desc="macOS 动态壁纸引擎 · 浏览/下载并应用 Steam 创意工坊壁纸（视频 / 场景 / 网页 / GIF / 图片） · Tauri 2"
          control={null}
        />
      </Group>
        </div>
      </div>
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
      className={`relative inline-flex h-[22px] w-[38px] shrink-0 items-center rounded-full transition-colors ${
        checked ? "bg-[var(--accent)]" : "bg-[var(--separator)]"
      }`}
    >
      <span
        className={`inline-block h-[18px] w-[18px] transform rounded-full bg-white shadow transition-transform ${
          checked ? "translate-x-[18px]" : "translate-x-[2px]"
        }`}
      />
    </button>
  );
}
