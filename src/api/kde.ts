// KDE Plasma 壁纸插件 API 封装（Rust `kde_plugin` 模块的命令签名）。
//
// 为什么存在：Linux/KDE 上「一键启用」其实是四件事 —— 装插件包、记住原壁纸、
// 用 plasmashell 的脚本接口切过去、再让引擎开始导出帧；「关闭」是它的逆操作。
// 这些步骤只在 Rust 侧实现（要碰文件系统与进程），前端只需要三个薄封装：
// 读只读状态（无副作用）、启用、关闭。字段名与 Rust 的 serde 输出一一对应
// （snake_case，结构体没有 rename 属性）。
import { invoke } from "@tauri-apps/api/core";

/** 引擎帧导出的实时统计（镜像 Rust `frame_export::ExportStats`） */
export interface KdeExportStats {
  /** 成功抓到的帧数（written + skipped = frames） */
  frames: number;
  /** 其中真正落盘的（内容有变化） */
  written: number;
  /** 与上一帧逐字节相同、跳过落盘的 */
  skipped: number;
  /** 抓帧/解码失败次数 */
  errors: number;
  last_seq: number;
  last_error: string | null;
  width: number;
  height: number;
  /** 最后一次落盘的帧字节数 */
  bytes: number;
}

/**
 * 引擎帧导出状态（镜像 Rust `frame_export::ExportStatus`）。
 * 统计字段**嵌在 `stats` 里**，不是平铺 —— 与 Rust 结构体保持一致。
 */
export interface KdeExportStatus {
  running: boolean;
  dir: string;
  frame_path: string;
  seq_path: string;
  fps: number;
  /** 最近一次真正抓到的窗口标题；一次都没抓到过时为 null */
  label: string | null;
  /** 被设为整窗透明的引擎壁纸窗口数（>0 = 插件模式，引擎自己不出图） */
  hidden_windows: number;
  /** 最近一次成功抓帧的来源：`x11-window` / `page` */
  source: string;
  stats: KdeExportStats;
}

/** 设置页的状态快照（`kde_plugin_status`）。只读命令，不会产生副作用 */
export interface KdePluginStatus {
  /** 当前平台能不能用这套（Linux + 正在跑的 plasmashell） */
  supported: boolean;
  /** supported=false 时的人话原因；可用时为空串 */
  unsupported_reason: string;
  /** `x11` / `wayland` / `unknown`：Wayland 上帧源还没实现，文案要区别对待 */
  session: string;
  plasmashell: boolean;
  /** 调 plasmashell 脚本接口的命令行（qdbus/gdbus）；找不到时为 null */
  script_tool: string | null;
  /** 随应用分发的插件包完不完整 */
  package_ready: boolean;
  package_dir: string | null;
  /** 插件包已装进 ~/.local/share/plasma/wallpapers/ */
  installed: boolean;
  installed_version: string | null;
  /** 已装插件的目录 */
  installed_dir: string;
  /** 桌面壁纸当前就是本插件（读 appletsrc，不依赖 plasmashell） */
  active: boolean;
  /** 用户点过启用（持久化标记） */
  enabled: boolean;
  /** 引擎此刻正在导出帧 */
  exporting: boolean;
  /** 引擎此刻有正在播放的壁纸（没有就没帧可导） */
  engine_playing: boolean;
  /** 帧目录（启用时用的值） */
  frame_dir: string;
  /** 帧率（启用时用的值） */
  fps: number;
  /** 给用户看的一句话状态说明 */
  note: string;
}

/** `kde_plugin_enable` 的结果：装包 + 切壁纸 + 起导出的汇总 */
export interface KdeEnableOutcome {
  /** 这次有没有把包写进用户目录 */
  installed: boolean;
  plugin_dir: string;
  frame_dir: string;
  /** 切过来的桌面容器数量 */
  switched_desktops: number;
  export: KdeExportStatus;
  note: string;
}

/** `kde_plugin_disable` 的结果：停导出 + 还原原壁纸 */
export interface KdeDisableOutcome {
  /** 有没有把切换前的壁纸配置写回去 */
  restored: boolean;
  /** 有没有停掉帧导出 */
  stopped: boolean;
  note: string;
}

/** 只读状态：进设置页/操作完成后刷新界面用，可随时调用 */
export function kdePluginStatus(): Promise<KdePluginStatus> {
  return invoke<KdePluginStatus>("kde_plugin_status");
}

/**
 * 一键启用：装插件包 → 记住原壁纸 → 切到本插件 → 开始导出帧。
 * 失败时 invoke 抛错，reason 是中文人话（可直接展示）。
 */
export function kdePluginEnable(): Promise<KdeEnableOutcome> {
  return invoke<KdeEnableOutcome>("kde_plugin_enable");
}

/** 关闭并还原：停导出 → 把切换前的壁纸写回（插件包留在用户目录，重启用不必重装） */
export function kdePluginDisable(): Promise<KdeDisableOutcome> {
  return invoke<KdeDisableOutcome>("kde_plugin_disable");
}
