// 插件页：两栏 —— **已安装**（内置插件 + 装到本机的第三方插件）与 **插件市场**。
//
// 市场的数据源是 GitHub `topic:wem-plugin` 的仓库搜索（Rust `plugin_market_search`，
// 失败只提示不报错）；已安装的插件并入同一份列表。**第三方插件是声明式清单**
// （`<appData>/plugins/<id>/wem-plugin.json`），所以热插拔就是增删一个目录：
// 装、卸、往目录里手放一份、重扫，全都立刻生效，不用重启应用。
//
// 交互约定（需求原文）：**每个插件点开都是新窗口**。内置插件走 Rust 宿主命令（应用
// 内窗口）；第三方插件按清单的 `open` 字段走系统浏览器或应用内窗口。
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ConfirmModal } from "../components/ConfirmModal";
import {
  asPluginFailure,
  pluginsApi,
  type DshScan,
  type MarketSearchResult,
  type PluginFailure,
} from "../api/plugins";
import {
  BUILTIN_PLUGINS,
  PLUGIN_CATEGORIES,
  PLUGIN_SORTS,
  capabilityLabel,
  categoryLabel,
  filterPlugins,
  isPrivileged,
  mergeMarket,
  sortPlugins,
  type PluginCategory,
  type PluginEntry,
  type PluginSort,
  type PluginSource,
} from "../lib/plugins";
import { useMessage } from "../components/Message";
import { tr } from "../lib/i18n";

/**
 * 插件命令的错误码 → 界面文案。
 *
 * 码是 Rust 侧定的稳定标识（src-tauri/src/plugin/*.rs），刻意用 error code 而不是
 * 「后端拼好的中文句子」：翻译留在界面侧这一张表里，后端只讲事实。
 */
function failureText(code: string): string {
  switch (code) {
    case "dsh-missing":
      return tr("未检测到 dsh 命令行，请先按引导安装");
    case "dsh-starting":
      return tr("DeepSeek Harness 正在启动，请稍候");
    case "dsh-port":
      return tr("本机没有可用的空闲端口");
    case "dsh-spawn":
      return tr("启动 dsh 进程失败");
    case "dsh-exited":
      return tr("dsh 启动后立即退出");
    case "dsh-timeout":
      return tr("等待 dsh 就绪超时");
    case "dsh-no-stdout":
      return tr("无法读取 dsh 的输出");
    case "dsh-start":
      return tr("启动 dsh 失败");
    case "mcp-start":
      return tr("本应用的 MCP 服务启动失败");
    case "mcp-unavailable":
      return tr("本应用的 MCP 服务不可用");
    case "profile-write":
      return tr("写入 dsh profile 失败");
    case "plugin-window":
      return tr("打开插件窗口失败");
    case "plugin-manifest":
      return tr("插件清单格式不正确");
    case "plugin-name":
      return tr("插件名称缺失或过长");
    case "plugin-url":
      return tr("插件入口地址无效（只支持 http/https）");
    case "plugin-entry-type":
      return tr("不支持的插件入口类型");
    case "plugin-open":
      return tr("不支持的打开方式");
    case "plugin-id":
      return tr("插件 id 不合法");
    case "plugin-id-reserved":
      return tr("这个 id 已被内置插件占用");
    case "plugin-write":
      return tr("写入插件目录失败");
    case "plugin-remove":
      return tr("删除插件失败");
    case "plugin-not-installed":
      return tr("这个插件没有安装");
    case "plugin-dir":
      return tr("插件目录不可用");
    case "plugin-http":
      return tr("网络客户端初始化失败");
    case "plugin-network":
      return tr("网络请求失败，检查网络或代理设置");
    case "plugin-http-status":
      return tr("远端返回了错误状态");
    case "plugin-fetch":
      return tr("下载插件清单失败");
    case "plugin-too-large":
      return tr("插件清单过大");
    case "plugin-parse":
      return tr("插件清单不是合法的 JSON");
    case "plugin-open-dir":
      return tr("打开插件目录失败");
    case "plugin-rate-limited":
      return tr("GitHub 搜索限流（匿名每分钟 10 次），稍后再试");
    case "plugin-schema":
      return tr("插件清单协议版本比当前应用新，请升级应用");
    case "plugin-capability":
      return tr("插件声明了当前应用不支持的能力");
    case "plugin-app-version":
      return tr("插件要求更高的应用版本");
    case "plugin-id-conflict":
      return tr("这个 id 已被另一个来源的插件占用");
    case "plugin-packages":
      return tr("插件声明的 DeepSeek Harness 包名不合法");
    case "plugin-pnpm":
      return tr("安装到 DeepSeek Harness 失败");
    case "plugin-pnpm-missing":
      return tr("找不到 pnpm（DeepSeek Harness 装插件需要它）");
    case "plugin-npm-missing":
      return tr("本机没有 npm（需要先安装 Node.js）");
    case "plugin-pnpm-install":
      return tr("安装 pnpm 失败");
    default:
      return tr("操作失败");
  }
}

/** 状态小胶囊 */
function Chip({ tone = "muted", children }: { tone?: "muted" | "ok" | "warn"; children: ReactNode }) {
  const cls =
    tone === "ok"
      ? "border-green-500/30 bg-green-500/10 text-green-400"
      : tone === "warn"
        ? "border-amber-500/30 bg-amber-500/10 text-amber-400"
        : "border-[var(--separator)] text-[var(--text-2)]";
  return (
    <span className={`shrink-0 rounded-md border px-1.5 py-[1px] text-[11px] ${cls}`}>
      {children}
    </span>
  );
}

/** 来源徽标：内置 / 第三方 / 已安装 / GitHub */
function SourceBadge({ source }: { source: PluginSource }) {
  return (
    <span className="shrink-0 rounded border border-[var(--card-border)] px-1.5 py-[1px] text-[10.5px] text-[var(--text-2)]">
      {source === "builtin" ? tr("内置") : tr("第三方")}
    </span>
  );
}

function MarketBadge({ p }: { p: PluginEntry }) {
  const label = p.installed ? tr("已安装") : "GitHub";
  const tone = p.installed
    ? "border-green-500/30 bg-green-500/10 text-green-400"
    : "border-[var(--card-border)] text-[var(--text-2)]";
  return (
    <span className={`shrink-0 rounded border px-1.5 py-[1px] text-[10.5px] ${tone}`}>{label}</span>
  );
}

/** 筛选胶囊（选中/未选两态，与筛选抽屉同一套观感） */
function FilterChip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded-lg border px-2 py-[3px] text-[11.5px] transition-colors ${
        active
          ? "border-[var(--accent-strong)] bg-[var(--accent)] text-[var(--accent-fg)]"
          : "border-[var(--separator)] text-[var(--text-2)] hover:bg-[var(--glass-hover)]"
      }`}
    >
      {children}
    </button>
  );
}

/** 页内分栏：已安装 / 插件市场 */
function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      data-active={active}
      className={`-mb-[1px] border-b-2 px-1 pb-2 text-[14px] font-semibold transition-colors ${
        active
          ? "border-[var(--accent-strong)] text-[var(--text-1)]"
          : "border-transparent text-[var(--text-2)] hover:text-[var(--text-1)]"
      }`}
    >
      {children}
    </button>
  );
}

/** 未装 dsh 时的引导面板：命令 + 下载页 + 重新检测 */
function DshGuide({
  scan,
  onRefresh,
  onClose,
}: {
  scan: DshScan | null;
  onRefresh: () => void;
  onClose: () => void;
}) {
  const msg = useMessage();
  const cmd = scan?.installCommand || "npm install -g @deepseek-ai/dsh";
  const downloadUrl = scan?.downloadUrl || "https://harness.deepseek.com";
  return (
    <div className="card mt-3 flex flex-col gap-2.5 p-4">
      <div className="flex items-center justify-between gap-2">
        <h3 className="text-[13.5px] font-semibold">{tr("安装 dsh 环境")}</h3>
        <button
          className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
          onClick={onClose}
        >
          {tr("收起")}
        </button>
      </div>
      <ol className="flex flex-col gap-2.5 text-[12.5px] text-[var(--text-2)]">
        <li>
          <div className="text-[var(--text-1)]">{tr("1. 装 Node.js（已装可跳过）")}</div>
          <div className="mt-0.5">
            {scan?.nodePath
              ? tr("本机 node：{path}", { path: scan.nodePath })
              : tr("本机未检测到 node —— dsh 是 Node 应用，先装 Node.js 18+")}
          </div>
        </li>
        <li>
          <div className="text-[var(--text-1)]">{tr("2. 全局安装 dsh 命令行")}</div>
          <div className="mt-1 flex flex-wrap items-center gap-2">
            <code className="rounded-md border border-[var(--separator)] bg-[var(--card)] px-2 py-1 text-[12px] text-[var(--text-1)]">
              {cmd}
            </code>
            <button
              className="btn !py-1 text-[11.5px]"
              onClick={() => {
                void navigator.clipboard
                  .writeText(cmd)
                  .then(() => msg.success(tr("已复制安装命令")))
                  .catch(() => msg.error(tr("复制失败，请手动选择命令")));
              }}
            >
              {tr("复制")}
            </button>
          </div>
        </li>
        <li>
          <div className="text-[var(--text-1)]">{tr("3. 或者装桌面版")}</div>
          <div className="mt-1 flex flex-wrap items-center gap-2">
            <button
              className="btn !py-1 text-[11.5px]"
              onClick={() => void openUrl(downloadUrl).catch(() => msg.error(tr("无法打开浏览器")))}
            >
              {tr("打开下载页")}
            </button>
            {scan?.desktopApp ? (
              <span className="text-[11.5px] text-amber-400">
                {tr("已检测到桌面版；本插件要的是命令行版 dsh，桌面版不能切换 profile")}
              </span>
            ) : null}
          </div>
        </li>
      </ol>
      <div className="flex items-center gap-2">
        <button className="btn btn-primary !py-1 text-[12px]" onClick={onRefresh}>
          {tr("重新检测")}
        </button>
        <span className="text-[11.5px] text-[var(--text-2)]">
          {tr("装好后不用重启应用，点这里重新检测即可")}
        </span>
      </div>
    </div>
  );
}

/** 内置插件卡片：DeepSeek Harness（状态 + 打开 / 安装引导） */
function DshCard({
  scan,
  scanning,
  scanFailed,
  busy,
  failure,
  guideOpen,
  onOpen,
  onStop,
  onRefresh,
  onToggleGuide,
  onInstallPnpm,
  pnpmBusy,
}: {
  scan: DshScan | null;
  scanning: boolean;
  scanFailed: string;
  busy: boolean;
  failure: PluginFailure | null;
  guideOpen: boolean;
  onOpen: () => void;
  onStop: () => void;
  onRefresh: () => void;
  onToggleGuide: () => void;
  onInstallPnpm: () => void;
  pnpmBusy: boolean;
}) {
  const plugin = BUILTIN_PLUGINS[0];
  const installed = scan?.installed ?? false;
  return (
    <div className="card flex flex-col gap-3 p-4">
      <div className="flex items-start gap-3">
        <div className="grid h-11 w-11 shrink-0 place-items-center rounded-xl bg-[var(--accent)] text-[20px]">
          {plugin.icon}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-[15px] font-semibold">{plugin.name}</h3>
            <SourceBadge source="builtin" />
          </div>
          <p className="mt-0.5 text-[12.5px] leading-[1.5] text-[var(--text-2)]">
            {tr(plugin.summary)}
          </p>
        </div>
      </div>

      {/* 环境状态：一眼看出「能不能点」，以及点下去会发生什么 */}
      <div className="flex flex-wrap items-center gap-1.5">
        {scanning && !scan ? (
          <Chip>{tr("检测中…")}</Chip>
        ) : scanFailed ? (
          <Chip tone="warn">{tr("环境检测失败")}</Chip>
        ) : installed ? (
          <>
            <Chip tone="ok">{scan?.version ? `dsh ${scan.version}` : tr("dsh 已就绪")}</Chip>
            {scan?.running ? <Chip tone="ok">{tr("运行中")}</Chip> : null}
            <Chip tone={scan?.mcpInjected ? "ok" : "muted"}>
              {scan?.mcpInjected ? tr("已注入 MCP 工具") : tr("打开时注入 MCP 工具")}
            </Chip>
            {scan?.profileInitialized && !scan.profileClean ? (
              <Chip tone="warn">{tr("profile 已有自定义配置（保留不动）")}</Chip>
            ) : null}
          </>
        ) : (
          <Chip tone="warn">{tr("未检测到 dsh 环境")}</Chip>
        )}
      </div>

      <details className="text-[11.5px] text-[var(--text-2)]">
        <summary className="cursor-pointer list-none marker:hidden">{tr("详情")}</summary>
        <div className="mt-1.5 flex flex-col gap-0.5 break-all">
          <div>{tr("profile：{name}", { name: scan?.profile ?? "wallpallperem" })}</div>
          <div>{tr("目录：{path}", { path: scan?.profileDir ?? "—" })}</div>
          <div>{tr("命令行：{path}", { path: scan?.cliPath ?? tr("未检测到") })}</div>
          <div>
            {tr("pnpm（装插件用）：{state}", {
              state: scan?.pnpmPath ?? tr("未检测到"),
            })}
          </div>
          <div>
            {tr("MCP 服务：{state}", {
              state: scan?.mcp?.running
                ? tr("运行中")
                : scan?.mcp?.enabled
                  ? tr("已开启但未监听")
                  : tr("关闭（打开插件时自动开启）"),
            })}
          </div>
        </div>
      </details>

      {failure ? (
        <div className="rounded-md border border-red-500/30 bg-red-500/10 px-2.5 py-2 text-[11.5px] text-red-400">
          <div>{failureText(failure.code)}</div>
          {failure.detail ? (
            <pre className="mt-1 max-h-24 overflow-auto whitespace-pre-wrap break-all text-[10.5px] opacity-80">
              {failure.detail}
            </pre>
          ) : null}
        </div>
      ) : null}

      <div className="mt-auto flex items-center gap-2">
        {installed ? (
          <button
            className="btn btn-primary !py-1 text-[12px] disabled:opacity-60"
            disabled={busy}
            onClick={onOpen}
          >
            {busy ? tr("启动中…") : scan?.running ? tr("打开窗口") : tr("打开")}
          </button>
        ) : (
          <button className="btn btn-primary !py-1 text-[12px]" onClick={onToggleGuide}>
            {guideOpen ? tr("收起引导") : tr("安装引导")}
          </button>
        )}
        {/* 关掉插件窗口会自动收掉后台进程；这里只是给「窗口已经不在、进程还在」的
            异常情况留一个手动的出口 */}
        {scan?.running ? (
          <button className="btn !py-1 text-[12px]" onClick={onStop}>
            {tr("停止后台进程")}
          </button>
        ) : null}
        {/* pnpm 只有 dsh-profile 型插件需要；缺了就在这里一键补上（有 npm 才装得了） */}
        {!scan?.pnpmPath && scan?.npmPath ? (
          <button
            className="btn !py-1 text-[12px] disabled:opacity-60"
            disabled={pnpmBusy}
            onClick={onInstallPnpm}
          >
            {pnpmBusy ? tr("安装中…") : tr("安装 pnpm")}
          </button>
        ) : null}
        <button className="btn !py-1 text-[12px]" onClick={onRefresh} disabled={scanning}>
          {scanning ? tr("检测中…") : tr("重新检测")}
        </button>
      </div>
    </div>
  );
}

/** 第三方插件卡片：市场里给「安装」，已安装里给「打开 / 卸载」 */
function ThirdPartyCard({
  p,
  busy,
  inMarket,
  onOpen,
  onInstall,
  onUninstall,
}: {
  p: PluginEntry;
  busy: boolean;
  inMarket: boolean;
  onOpen: (p: PluginEntry) => void;
  onInstall: (p: PluginEntry) => void;
  onUninstall: (p: PluginEntry) => void;
}) {
  return (
    <div className="card flex flex-col gap-3 p-4">
      <div className="flex items-start gap-3">
        <div className="grid h-11 w-11 shrink-0 place-items-center rounded-xl bg-[var(--accent)] text-[20px]">
          {p.icon}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-[15px] font-semibold">{tr(p.name)}</h3>
            {inMarket ? <MarketBadge p={p} /> : <SourceBadge source="third" />}
            {p.archived ? <Chip tone="warn">{tr("已归档")}</Chip> : null}
          </div>
          <p className="mt-0.5 text-[12.5px] leading-[1.5] text-[var(--text-2)]">
            {tr(p.summary) || tr("这个插件没有写说明")}
          </p>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-1.5">
        {p.author ? <Chip>{p.author}</Chip> : null}
        <Chip>{categoryLabel(p.category)}</Chip>
        {p.version ? <Chip>v{p.version}</Chip> : null}
        {typeof p.stars === "number" ? <Chip>★ {p.stars}</Chip> : null}
        {/* 清单声明的能力（协议见 docs/plugin-protocol.md 的能力矩阵） */}
        {(p.capabilities ?? []).map((c) => (
          <Chip key={c}>{capabilityLabel(c)}</Chip>
        ))}
        {(p.packages ?? []).map((pkg) => (
          <Chip key={pkg}>{pkg}</Chip>
        ))}
        {isPrivileged(p) ? (
          <Chip tone="warn" >{tr("特权：会运行第三方代码")}</Chip>
        ) : null}
        {p.compatible === false ? (
          <Chip tone="warn">
            {tr("需要 App ≥ {v}", { v: p.minAppVersion || "—" })}
          </Chip>
        ) : null}
      </div>

      {p.description && p.description !== p.summary ? (
        <details className="text-[11.5px] text-[var(--text-2)]">
          <summary className="cursor-pointer list-none marker:hidden">{tr("说明")}</summary>
          <p className="mt-1.5 whitespace-pre-wrap break-words leading-[1.6]">{tr(p.description)}</p>
        </details>
      ) : null}

      <div className="mt-auto flex flex-wrap items-center gap-2">
        {p.installed ? (
          <>
            <button
              className="btn btn-primary !py-1 text-[12px] disabled:opacity-60"
              disabled={busy || p.compatible === false}
              title={p.compatible === false ? tr("需要 App ≥ {v}", { v: p.minAppVersion || "—" }) : undefined}
              onClick={() => onOpen(p)}
            >
              {busy && p.kind === "dsh" ? tr("正在安装到 Harness…") : tr("打开")}
            </button>
            <button
              className="btn btn-danger !py-1 text-[12px] disabled:opacity-60"
              disabled={busy}
              onClick={() => onUninstall(p)}
            >
              {tr("卸载")}
            </button>
          </>
        ) : (
          <button
            className="btn btn-primary !py-1 text-[12px] disabled:opacity-60"
            disabled={busy}
            onClick={() => onInstall(p)}
          >
            {busy ? tr("安装中…") : tr("安装")}
          </button>
        )}
        {p.homepage ? (
          <button
            className="btn !py-1 text-[12px]"
            onClick={() => void openUrl(p.homepage!).catch(() => {})}
          >
            {p.installed ? tr("主页") : tr("打开仓库")}
          </button>
        ) : null}
      </div>
    </div>
  );
}

export function PluginsPage() {
  const [tab, setTab] = useState<"installed" | "market">("installed");
  const [query, setQuery] = useState("");
  const [source, setSource] = useState<"all" | PluginSource>("all");
  const [category, setCategory] = useState<PluginCategory | "all">("all");
  const [sort, setSort] = useState<PluginSort>("relevance");
  const [busyId, setBusyId] = useState<string | null>(null);
  const [manifestUrl, setManifestUrl] = useState("");
  /** 待确认的特权插件（弹框询问后才真的去装包） */
  const [confirmPlugin, setConfirmPlugin] = useState<PluginEntry | null>(null);
  const confirmedRef = useRef<Set<string>>(new Set());
  /** 待确认的 pnpm 安装（`retry` = 装完自动重试刚失败的那次打开） */
  const [pnpmPrompt, setPnpmPrompt] = useState<{ retry?: () => void } | null>(null);
  const [pnpmBusy, setPnpmBusy] = useState(false);
  const msg = useMessage();

  // ---- 内置插件（dsh）：环境扫描 ----
  const [scan, setScan] = useState<DshScan | null>(null);
  const [scanning, setScanning] = useState(true);
  const [scanFailed, setScanFailed] = useState("");
  const [dshBusy, setDshBusy] = useState(false);
  const [guideOpen, setGuideOpen] = useState(false);
  const [failure, setFailure] = useState<PluginFailure | null>(null);
  const guideTouched = useRef(false);

  const refreshDsh = useCallback(async () => {
    setScanning(true);
    try {
      const s = await pluginsApi.dshScan();
      setScan(s);
      setScanFailed("");
      // 没装 dsh：唯一的下一步就是安装 —— 首屏直接把引导摊开，别让用户再点一次
      if (!s.installed && !guideTouched.current) setGuideOpen(true);
    } catch (e) {
      setScanFailed(asPluginFailure(e).detail || String(e));
    } finally {
      setScanning(false);
    }
  }, []);

  // ---- 第三方插件：本地已安装清单（热插拔：这就是磁盘当前的样子） ----
  const [installed, setInstalled] = useState<PluginEntry[]>([]);
  const [installedDir, setInstalledDir] = useState("");
  const [installedLoading, setInstalledLoading] = useState(true);

  const refreshInstalled = useCallback(async () => {
    setInstalledLoading(true);
    try {
      const r = await pluginsApi.installed();
      setInstalled(r.items ?? []);
      setInstalledDir(r.dir ?? "");
      if (r.error) msg.error(failureText(r.error));
    } catch (e) {
      msg.error(failureText(asPluginFailure(e).code));
    } finally {
      setInstalledLoading(false);
    }
  }, [msg]);

  useEffect(() => {
    void refreshDsh();
    void refreshInstalled();
  }, [refreshDsh, refreshInstalled]);

  // ---- 市场：GitHub 话题搜索（防抖 + Rust 侧 90s 缓存，匿名限流 10 次/分钟） ----
  const [remote, setRemote] = useState<MarketSearchResult | null>(null);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    if (tab !== "market") return;
    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(() => {
      pluginsApi
        .marketSearch(query, sort)
        .then((r) => {
          if (!cancelled) setRemote(r);
        })
        .catch((e) => {
          if (cancelled) return;
          const f = asPluginFailure(e);
          setRemote({
            items: [],
            totalCount: 0,
            error: f.code === "unknown" ? "plugin-network" : f.code,
            errorDetail: f.detail,
            topic: "wem-plugin",
            rateLimitReset: null,
            cached: false,
          });
        })
        .finally(() => {
          if (!cancelled) setSearching(false);
        });
    }, 500);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [tab, query, sort]);

  // ---- 动作 ----
  const openDsh = useCallback(async () => {
    if (dshBusy) return;
    setDshBusy(true);
    setFailure(null);
    try {
      const r = await pluginsApi.dshOpen();
      msg.success(tr("已在新窗口打开 DeepSeek Harness"));
      setScan((s) => (s ? { ...s, running: true, url: r.url } : s));
    } catch (e) {
      const f = asPluginFailure(e);
      setFailure(f);
      msg.error(failureText(f.code));
      if (f.code === "dsh-missing") setGuideOpen(true);
    } finally {
      setDshBusy(false);
      void refreshDsh();
    }
  }, [dshBusy, msg, refreshDsh]);

  /** 装 pnpm（`npm install -g pnpm`）：改的是用户的全局环境，必须先确认 */
  const installPnpm = useCallback(
    async (then?: () => void) => {
      setPnpmBusy(true);
      msg.info(tr("正在安装 pnpm…"));
      try {
        const r = await pluginsApi.installPnpm();
        if (r?.installed) {
          msg.success(tr("pnpm 已安装：{path}", { path: r.pnpmPath ?? "" }));
          then?.();
        } else {
          // npm 报成功但没在预期位置找到：多半是 PATH 类问题，重开应用再看
          msg.info(tr("pnpm 已安装，但没在预期位置找到；重开应用后再试一次"));
        }
      } catch (e) {
        const f = asPluginFailure(e);
        msg.error(failureText(f.code));
        if (f.code === "plugin-npm-missing") {
          void openUrl("https://nodejs.org/zh-cn/download").catch(() => {});
        }
      } finally {
        setPnpmBusy(false);
        void refreshDsh();
      }
    },
    [msg, refreshDsh],
  );

  const stopDsh = useCallback(async () => {
    try {
      await pluginsApi.dshStop();
      msg.info(tr("已停止后台 dsh 进程"));
    } catch (e) {
      msg.error(asPluginFailure(e).detail || tr("停止后台进程失败"));
    } finally {
      void refreshDsh();
    }
  }, [msg, refreshDsh]);

  /** 打开一个已安装的第三方插件：按清单的 open 字段决定落点 */
  /** 真正执行打开（特权确认之后走这里） */
  const doOpenPlugin = useCallback(
    async (p: PluginEntry) => {
      setBusyId(p.id);
      try {
        if (p.kind === "dsh") {
          // 装包 + 拉起 dsh：可能要几分钟（pnpm + 联网）
          const r = await pluginsApi.dshOpenPlugin(p.id);
          msg.success(tr("已在 DeepSeek Harness 中打开「{name}」", { name: p.name }));
          if (r?.installedNow?.length) {
            msg.info(tr("本次装进 profile：{list}", { list: r.installedNow.join("、") }));
          }
        } else if (p.url && p.open === "window") {
          await pluginsApi.openWindow(p.id, p.url, p.name);
        } else if (p.url) {
          await openUrl(p.url);
        }
      } catch (e) {
        const f = asPluginFailure(e);
        msg.error(failureText(f.code));
        // 缺 pnpm 是唯一「应用能替用户解决」的前置条件：问一句，装完自动重试这次打开
        if (f.code === "plugin-pnpm-missing") {
          setPnpmPrompt({ retry: () => void doOpenPlugin(p) });
        }
      } finally {
        setBusyId(null);
      }
    },
    [msg],
  );

  const openPlugin = useCallback(
    (p: PluginEntry) => {
      // 清单要求的最低应用版本没满足：可装不可开（协议规则 R5）
      if (p.compatible === false) {
        msg.error(tr("插件要求更高的应用版本"));
        return;
      }
      // 特权能力（往 dsh profile 装第三方包）**每次会话第一次打开**都要确认：
      // 那是会被 dsh 执行的代码，不能跟普通外链一个待遇（协议规则 R12）
      if (isPrivileged(p) && !confirmedRef.current.has(p.id)) {
        setConfirmPlugin(p);
        return;
      }
      void doOpenPlugin(p);
    },
    [doOpenPlugin, msg],
  );

  const installPlugin = useCallback(
    async (p: PluginEntry) => {
      setBusyId(p.id);
      try {
        // 市场条目都来自 GitHub：拉仓库根目录的 wem-plugin.json 落地
        if (!p.manifestUrl) {
          msg.error(tr("这个插件没有可安装的清单地址"));
          return;
        }
        await pluginsApi.installUrl(p.manifestUrl);
        msg.success(tr("已安装「{name}」", { name: p.name }));
        await refreshInstalled();
      } catch (e) {
        const f = asPluginFailure(e);
        msg.error(`${failureText(f.code)}${f.detail ? `（${f.detail}）` : ""}`);
      } finally {
        setBusyId(null);
      }
    },
    [msg, refreshInstalled],
  );

  const uninstallPlugin = useCallback(
    async (p: PluginEntry) => {
      setBusyId(p.id);
      try {
        await pluginsApi.uninstall(p.id);
        msg.info(tr("已卸载「{name}」", { name: p.name }));
        await refreshInstalled();
      } catch (e) {
        msg.error(failureText(asPluginFailure(e).code));
      } finally {
        setBusyId(null);
      }
    },
    [msg, refreshInstalled],
  );

  /** 从清单地址安装（热插拔的手动口子：贴一个 wem-plugin.json 链接） */
  const installFromUrl = useCallback(async () => {
    const url = manifestUrl.trim();
    if (!url) return;
    setBusyId("__url__");
    try {
      const entry = await pluginsApi.installUrl(url);
      msg.success(tr("已安装「{name}」", { name: entry?.name ?? url }));
      setManifestUrl("");
      await refreshInstalled();
    } catch (e) {
      const f = asPluginFailure(e);
      msg.error(`${failureText(f.code)}${f.detail ? `（${f.detail}）` : ""}`);
    } finally {
      setBusyId(null);
    }
  }, [manifestUrl, msg, refreshInstalled]);

  const openPluginDir = useCallback(async () => {
    try {
      await pluginsApi.openDir();
    } catch (e) {
      msg.error(failureText(asPluginFailure(e).code));
    }
  }, [msg]);

  // ---- 列表计算 ----
  const builtin = filterPlugins(BUILTIN_PLUGINS, query, category);
  const installedFiltered = useMemo(
    () => filterPlugins(installed, query, category),
    [installed, query, category],
  );
  const installedThird = useMemo(
    () => sortPlugins(installedFiltered, sort === "relevance" ? "name" : sort),
    [installedFiltered, sort],
  );
  const market = useMemo(
    () =>
      sortPlugins(
        mergeMarket(installedFiltered, filterPlugins(remote?.items ?? [], query, category)),
        sort,
      ),
    [installedFiltered, remote, query, category, sort],
  );

  const showBuiltin = source !== "third" && builtin.length > 0;
  const showInstalledThird = source !== "builtin" && installedThird.length > 0;
  const total =
    tab === "installed"
      ? (source !== "third" ? builtin.length : 0) + (source !== "builtin" ? installedThird.length : 0)
      : market.length;

  return (
    <div className="flex h-full flex-col px-7 py-5">
      <div className="mb-2 shrink-0">
        <h1 className="text-[22px] font-bold tracking-tight">{tr("插件")}</h1>
      </div>

      {/* 页内分栏 */}
      <div className="mb-3 flex shrink-0 items-center gap-4 border-b border-[var(--separator)]">
        <TabButton active={tab === "installed"} onClick={() => setTab("installed")}>
          {tr("已安装")}
        </TabButton>
        <TabButton active={tab === "market"} onClick={() => setTab("market")}>
          {tr("插件市场")}
        </TabButton>
        <span className="ml-auto pb-2 text-[11.5px] text-[var(--text-2)]">
          {tab === "installed"
            ? tr("第三方插件是声明式清单，装/卸都立刻生效，不需要重启应用")
            : tr("市场来自 GitHub 话题 {topic}，支持搜索与排序", {
                topic: remote?.topic ?? "wem-plugin",
              })}
        </span>
      </div>

      {/* 工具栏：搜索 + 分类（+ 已安装页的来源筛选、市场页的排序） */}
      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2.5">
        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder={tab === "market" ? tr("搜索插件市场…") : tr("搜索插件…")}
          className="w-64 rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[13px] outline-none focus:border-[var(--accent-strong)]"
        />
        {tab === "installed" ? (
          <div className="flex items-center gap-1.5">
            {(
              [
                { value: "all", label: "全部" },
                { value: "builtin", label: "内置" },
                { value: "third", label: "第三方" },
              ] as const
            ).map((s) => (
              <FilterChip key={s.value} active={source === s.value} onClick={() => setSource(s.value)}>
                {tr(s.label)}
              </FilterChip>
            ))}
          </div>
        ) : null}
        <div className="flex items-center gap-1.5">
          <FilterChip active={category === "all"} onClick={() => setCategory("all")}>
            {tr("全部分类")}
          </FilterChip>
          {PLUGIN_CATEGORIES.map((c) => (
            <FilterChip key={c} active={category === c} onClick={() => setCategory(c)}>
              {categoryLabel(c)}
            </FilterChip>
          ))}
        </div>
        {tab === "market" ? (
          <select
            value={sort}
            onChange={(e) => setSort(e.target.value as PluginSort)}
            title={tr("排序")}
            className="rounded-lg border border-[var(--separator)] bg-[var(--card)] px-2.5 py-1.5 text-[13px] outline-none"
          >
            {PLUGIN_SORTS.map((s) => (
              <option key={s.value} value={s.value}>
                {tr(s.label)}
              </option>
            ))}
          </select>
        ) : null}
        <span className="text-[12px] text-[var(--text-2)]">
          {tr("共 {n} 个插件", { n: total })}
        </span>
      </div>

      <div className="flex-1 overflow-y-auto">
        {tab === "market" ? (
          <>
            {/* GitHub 搜索状态：失败只提示，已安装的插件照常可用 */}
            <div className="mb-3 flex flex-wrap items-center gap-2 text-[11.5px] text-[var(--text-2)]">
              {searching ? (
                <span>{tr("正在搜索 GitHub…")}</span>
              ) : remote?.error === "plugin-rate-limited" ? (
                <Chip tone="warn">{tr("GitHub 搜索限流（匿名每分钟 10 次），稍后再试")}</Chip>
              ) : remote?.error ? (
                <Chip tone="warn">
                  {tr("GitHub 搜索不可用（离线或受限）")}
                  {remote.errorDetail ? ` · ${remote.errorDetail}` : ""}
                </Chip>
              ) : remote ? (
                <Chip tone="ok">
                  {tr("GitHub 话题 {topic}：{n} 个结果", {
                    topic: remote.topic,
                    n: remote.totalCount,
                  })}
                </Chip>
              ) : null}
              {remote?.cached ? <Chip>{tr("结果来自缓存")}</Chip> : null}
            </div>

            {/* 手贴一个清单地址也能装：热插拔的手动口子 */}
            <div className="mb-4 flex flex-wrap items-center gap-2">
              <input
                value={manifestUrl}
                onChange={(e) => setManifestUrl(e.target.value)}
                placeholder={tr("从 wem-plugin.json 地址安装（粘贴 GitHub raw 链接…）")}
                className="w-[420px] rounded-lg border border-[var(--separator)] bg-[var(--card)] px-3 py-1.5 text-[12.5px] outline-none focus:border-[var(--accent-strong)]"
              />
              <button
                className="btn !py-1 text-[12px] disabled:opacity-60"
                disabled={!manifestUrl.trim() || busyId === "__url__"}
                onClick={() => void installFromUrl()}
              >
                {busyId === "__url__" ? tr("安装中…") : tr("从地址安装")}
              </button>
            </div>

            {market.length === 0 && !searching ? (
              <div className="card px-4 py-8 text-center text-[13px] text-[var(--text-2)]">
                {remote?.error
                  ? tr("市场暂时不可用：{reason}", { reason: failureText(remote.error) })
                  : query.trim() || category !== "all"
                    ? tr("没有匹配的插件，换个关键词或分类试试")
                    : tr("这个话题下还没有插件仓库；作者给仓库打上 {topic} 话题后就会出现在这里", {
                        topic: remote?.topic ?? "wem-plugin",
                      })}
              </div>
            ) : (
              <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                {market.map((p) => (
                  <ThirdPartyCard
                    key={p.id}
                    p={p}
                    busy={busyId === p.id}
                    inMarket
                    onOpen={openPlugin}
                    onInstall={(x) => void installPlugin(x)}
                    onUninstall={(x) => void uninstallPlugin(x)}
                  />
                ))}
              </div>
            )}
          </>
        ) : (
          <>
            {showBuiltin ? (
              <section className="mb-6">
                <h2 className="mb-2.5 text-[13px] font-semibold text-[var(--text-2)]">
                  {tr("内置插件")}
                </h2>
                <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                  <DshCard
                    scan={scan}
                    scanning={scanning}
                    scanFailed={scanFailed}
                    busy={dshBusy}
                    failure={failure}
                    guideOpen={guideOpen}
                    onOpen={() => void openDsh()}
                    onStop={() => void stopDsh()}
                    onRefresh={() => void refreshDsh()}
                    onToggleGuide={() => {
                      guideTouched.current = true;
                      setGuideOpen((v) => !v);
                    }}
                    onInstallPnpm={() => setPnpmPrompt({})}
                    pnpmBusy={pnpmBusy}
                  />
                </div>
                {guideOpen ? (
                  <DshGuide
                    scan={scan}
                    onRefresh={() => void refreshDsh()}
                    onClose={() => {
                      guideTouched.current = true;
                      setGuideOpen(false);
                    }}
                  />
                ) : null}
              </section>
            ) : null}

            {showInstalledThird ? (
              <section className="mb-4">
                <div className="mb-2.5 flex items-center gap-3">
                  <h2 className="text-[13px] font-semibold text-[var(--text-2)]">
                    {tr("第三方插件")}
                  </h2>
                  <button
                    className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
                    onClick={() => void refreshInstalled()}
                    disabled={installedLoading}
                  >
                    {installedLoading ? tr("扫描中…") : tr("重新扫描")}
                  </button>
                  <button
                    className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
                    onClick={() => void openPluginDir()}
                  >
                    {tr("打开插件目录")}
                  </button>
                </div>
                <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3">
                  {installedThird.map((p) => (
                    <ThirdPartyCard
                      key={p.id}
                      p={p}
                      busy={busyId === p.id}
                      inMarket={false}
                      onOpen={openPlugin}
                      onInstall={(x) => void installPlugin(x)}
                      onUninstall={(x) => void uninstallPlugin(x)}
                    />
                  ))}
                </div>
              </section>
            ) : null}

            {!showBuiltin && !showInstalledThird ? (
              <div className="card flex flex-col items-center gap-3 px-4 py-10 text-center">
                <div className="text-[13px] text-[var(--text-2)]">
                  {query.trim() || category !== "all"
                    ? tr("没有匹配的插件，换个关键词或分类试试")
                    : tr("还没有安装第三方插件")}
                </div>
                {!query.trim() && category === "all" ? (
                  <>
                    <button
                      className="btn btn-primary !py-1 text-[12px]"
                      onClick={() => setTab("market")}
                    >
                      {tr("去插件市场看看")}
                    </button>
                    <div className="text-[11px] text-[var(--text-2)]">
                      {tr("插件目录：{path}", { path: installedDir || "—" })}
                    </div>
                    <div className="text-[11px] text-[var(--text-2)]">
                      {tr("把插件文件夹放进这个目录，点「重新扫描」即可（不用重启应用）")}
                    </div>
                    <div className="flex items-center gap-2">
                      <button className="btn !py-1 text-[12px]" onClick={() => void openPluginDir()}>
                        {tr("打开插件目录")}
                      </button>
                      <button
                        className="btn !py-1 text-[12px]"
                        onClick={() => void refreshInstalled()}
                      >
                        {tr("重新扫描")}
                      </button>
                    </div>
                  </>
                ) : null}
              </div>
            ) : null}

            {!showInstalledThird && showBuiltin ? (
              <div className="mb-3 flex items-center gap-3">
                <button
                  className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
                  onClick={() => void refreshInstalled()}
                  disabled={installedLoading}
                >
                  {installedLoading ? tr("扫描中…") : tr("重新扫描第三方插件")}
                </button>
                <button
                  className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
                  onClick={() => void openPluginDir()}
                >
                  {tr("打开插件目录")}
                </button>
                <button
                  className="text-[11.5px] text-[var(--text-2)] hover:text-[var(--text-1)]"
                  onClick={() => setTab("market")}
                >
                  {tr("去插件市场看看")}
                </button>
              </div>
            ) : null}
          </>
        )}
      </div>

      {/* 装 pnpm 会改动用户的 Node 全局环境：先确认，装完自动接着干刚才没干成的事 */}
      {pnpmPrompt ? (
        <ConfirmModal
          title={tr("安装 pnpm？")}
          message={tr(
            "DeepSeek Harness 装插件需要 pnpm，本机没有。要用 npm 全局安装一个吗？（执行 npm install -g pnpm，会写入你的 Node 全局 bin 目录）",
          )}
          confirmText={tr("安装并继续")}
          onCancel={() => setPnpmPrompt(null)}
          onConfirm={() => {
            const retry = pnpmPrompt.retry;
            setPnpmPrompt(null);
            void installPnpm(retry);
          }}
        />
      ) : null}

      {/* 特权插件的确认（协议规则 R12）：装进 DeepSeek Harness 的包会被 dsh 执行 */}
      {confirmPlugin ? (
        <ConfirmModal
          danger
          title={tr("安装特权插件？")}
          message={tr(
            "「{name}」会把下面这些包装进 DeepSeek Harness 的 profile：{list}。它们会在 harness 里运行（等同于第三方代码），安装时可能需要联网下载。",
            {
              name: confirmPlugin.name,
              list: (confirmPlugin.packages ?? []).join("、") || "—",
            },
          )}
          confirmText={tr("继续")}
          onCancel={() => setConfirmPlugin(null)}
          onConfirm={() => {
            const p = confirmPlugin;
            setConfirmPlugin(null);
            if (!p) return;
            confirmedRef.current.add(p.id);
            void doOpenPlugin(p);
          }}
        />
      ) : null}
    </div>
  );
}
