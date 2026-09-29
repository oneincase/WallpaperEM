// 插件宿主 API 封装（Rust `plugin` 模块的命令签名）。
//
// 命令名来自 Rust 函数名：`dsh_scan` / `dsh_open` / `dsh_stop`。
// 插件命令的错误是**结构化**的（`{ code, detail }`）而不是一句话：code 是稳定
// 标识，界面据此出本地化文案；detail 是进程输出/io 原文，只作诊断展示。
import { invoke } from "@tauri-apps/api/core";
import type { PluginEntry } from "../lib/plugins";

/** 内置插件 DeepSeek Harness 的环境扫描结果 */
export interface DshScan {
  /** 插件使用的 profile 名（固定 wallpallperem） */
  profile: string;
  dshHome: string;
  profileDir: string;
  /** profile 是否已经建过（package.json 在不在） */
  profileInitialized: boolean;
  /** profile 是否还是「干净」的（bundle 清单 = dsh-base + dsh-web-app） */
  profileClean: boolean;
  /** MCP 层是否已经指向**当前**的 MCP 地址（端口/令牌都对得上） */
  mcpInjected: boolean;
  /** 本机是否找到 dsh 命令行 */
  installed: boolean;
  cliPath: string | null;
  /** path = PATH 里就有；global = 从常见全局安装目录扫出来的 */
  cliSource: "path" | "global" | null;
  version: string | null;
  /** 桌面版安装路径（不含命令行，只用于引导文案） */
  desktopApp: string | null;
  nodePath: string | null;
  npmPath: string | null;
  /** pnpm：只有 dsh-profile 型插件需要（`dsh plugin … add` 靠它装包）；可为空 */
  pnpmPath: string | null;
  /** 引导用：npm 全局安装命令 + 官网下载页 */
  installCommand: string;
  downloadUrl: string;
  /** dsh 进程是否在跑、跑在哪个地址 */
  running: boolean;
  url: string | null;
  mcp: {
    serverName: string;
    enabled: boolean;
    running: boolean;
    url: string | null;
  };
}

export interface DshOpenResult {
  url: string;
  port: number;
  profileDir: string;
  mcpUrl: string;
}

/** 已安装的第三方插件（Rust 扫 `<appData>/plugins/<id>/wem-plugin.json` 得到） */
export interface InstalledPlugins {
  items: PluginEntry[];
  dir: string;
  count: number;
  error?: string;
}

/**
 * 市场搜索结果（GitHub `topic:wem-plugin`）。
 *
 * 失败**不抛错**，而是回一个带 `error` 的载荷：市场页离线/限流时应当继续显示官方
 * 清单，而不是把整页打空。
 */
export interface MarketSearchResult {
  items: PluginEntry[];
  totalCount: number;
  /** 失败码：plugin-rate-limited / plugin-network / plugin-http-status / plugin-parse */
  error: string | null;
  errorDetail: string | null;
  topic: string;
  /** 限流时的额度重置时间（Unix 秒） */
  rateLimitReset: number | null;
  cached: boolean;
}

export interface PluginFailure {
  code: string;
  detail: string;
}

export const pluginsApi = {
  dshScan: () => invoke<DshScan>("dsh_scan"),
  dshOpen: () => invoke<DshOpenResult>("dsh_open"),
  dshStop: () => invoke<{ ok: boolean }>("dsh_stop"),
  /**
   * 一键装 pnpm（`npm install -g pnpm`）。会真的改动用户的 Node 全局环境，
   * 调用前必须让用户确认。
   */
  installPnpm: () =>
    invoke<{ installed: boolean; pnpmPath?: string; npmPath?: string; hint?: string }>(
      "dsh_install_pnpm",
    ),
  // ---- 第三方插件（热插拔：装/卸都只动 <appData>/plugins 下的一个目录） ----
  installed: () => invoke<InstalledPlugins>("plugin_installed"),
  /** 从清单地址安装（GitHub raw / 任意 http(s) 的 wem-plugin.json） */
  installUrl: (url: string) => invoke<PluginEntry>("plugin_install_url", { url }),
  uninstall: (id: string) => invoke<{ ok: boolean; id: string }>("plugin_uninstall", { id }),
  /** 在文件管理器里打开插件目录（手放一个清单文件夹进去也算装好） */
  openDir: () => invoke<{ dir: string }>("plugin_open_dir"),
  /**
   * 打开 `dsh` 型插件：把清单里的包装进干净 profile，再打开 dsh 窗口。
   * 慢（pnpm + 联网，最长可等几分钟），期间界面应显示进行中。
   */
  dshOpenPlugin: (id: string) => invoke<{ packages: string[]; installedNow: string[] }>("plugin_dsh_open", { id }),
  /** 清单要求应用内窗口时走这个（同步命令，主线程开窗） */
  openWindow: (id: string, url: string, name: string) =>
    invoke<{ label: string; url: string }>("plugin_open_window", { id, url, name }),
  marketSearch: (query: string, sort: string, topic?: string) =>
    invoke<MarketSearchResult>("plugin_market_search", { query, sort, topic: topic ?? null }),
};

/** invoke 的 reject 值：插件命令给对象，其它命令给字符串 —— 统一成一种形状 */
export function asPluginFailure(e: unknown): PluginFailure {
  if (e && typeof e === "object") {
    const o = e as Record<string, unknown>;
    if (typeof o.code === "string") {
      return { code: o.code, detail: typeof o.detail === "string" ? o.detail : "" };
    }
  }
  return { code: "unknown", detail: String(e ?? "") };
}
