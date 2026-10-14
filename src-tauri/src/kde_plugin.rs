//! KDE Plasma 壁纸插件的「一键启用 / 关闭」（Linux/KDE 桌面专用）。
//!
//! WallpaperEM 在 KDE 下能让壁纸动起来的路有两条：
//!
//! 1. 引擎自己开一扇「桌面层」窗口（`wallpaper::linux` 的沉底/透明逻辑）；
//! 2. **本模块**：把随应用分发的 Plasma 壁纸插件包装进用户目录，用 plasmashell
//!    自己的脚本接口把桌面壁纸换成它，再让引擎按插件契约往 tmpfs 写帧。
//!
//! 第 2 条的价值：桌面层始终由 plasmashell 自己管，图标、右键菜单、面板的层级
//! 关系完全不变，引擎崩了也带不走桌面 —— 代价是两进程之间要用文件传帧
//! （契约见 WallpaperEM-Plasma/docs/engine-contract.md）。
//!
//! 三个动作都**幂等且可逆**：`enable` 先记住切换前的壁纸配置（appletsrc 里每个
//! 桌面容器的插件 id 与 General 组键值），`disable` 原样写回。
//!
//! 为什么用 `qdbus … evaluateScript` 而不是直接改 appletsrc：改文件要 plasmashell
//! 自己重读（不保证生效），而脚本接口是 Plasma 官方给外部程序用的
//! （`plasma-apply-wallpaperimage` 内部就是同一套调用）。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

/// 插件包 id，必须与 `packaging/plasma-wallpaper/<id>/metadata.json` 的 `KPlugin.Id` 一致。
pub const PLUGIN_ID: &str = "io.github.oneincase.wallpaperem";
/// 插件在 Plasma 里显示的名字（仅用于文案）。
pub const PLUGIN_LABEL: &str = "WallpaperEM";
/// 帧文件的默认目录，与插件的默认 `FramePath` 同源。
pub const DEFAULT_FRAME_DIR: &str = "/dev/shm/wallpaperem";
/// 默认导出帧率：插件端每帧都要重新解码，20fps 在 1080p 下已足够顺滑。
pub const DEFAULT_FPS: u32 = 20;

const SETTING_ENABLED: &str = "kde.plugin.enabled";
const SETTING_PREVIOUS: &str = "kde.plugin.previous";
const SETTING_FRAME_DIR: &str = "kde.plugin.frame_dir";
const SETTING_FPS: &str = "kde.plugin.fps";

/// 壁纸插件在用户目录里的落点（Plasma 从 `~/.local/share/plasma/wallpapers/` 读）。
const PLUGINS_SUBDIR: &str = ".local/share/plasma/wallpapers";
/// 桌面容器壁纸配置所在的配置文件。
const APPLETSRC: &str = ".config/plasma-org.kde.plasma.desktop-appletsrc";

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 设置页 / MCP 的只读状态快照。**不产生副作用**（README/文档承诺）。
#[derive(Debug, Clone, Serialize)]
pub struct KdePluginStatus {
    /// 当前平台能不能用这套（Linux + 正在跑的 plasmashell）。
    pub supported: bool,
    /// 不能用的原因（supported=true 时为空）。
    pub unsupported_reason: String,
    /// `x11` / `wayland` / `unknown`：帧源在 Wayland 上还没实现，文案要区别对待。
    pub session: String,
    /// plasmashell 进程在不在。
    pub plasmashell: bool,
    /// 能用哪个命令行去调 plasmashell 的脚本接口。
    pub script_tool: Option<String>,
    /// 随应用分发的插件包完不完整（缺了就没法安装）。
    pub package_ready: bool,
    /// 随应用分发的插件包路径。
    pub package_dir: Option<String>,
    /// 插件包是否已装进用户目录。
    pub installed: bool,
    /// 已装插件的版本号（读它自己的 metadata.json）。
    pub installed_version: Option<String>,
    /// 已装插件的目录。
    pub installed_dir: String,
    /// 桌面壁纸当前是不是本插件（读 appletsrc，不依赖 plasmashell）。
    pub active: bool,
    /// 用户是不是点了「启用」（持久化标记）。
    pub enabled: bool,
    /// 引擎此刻是不是在导出帧。
    pub exporting: bool,
    /// 引擎此刻有没有正在播放的壁纸（没有就没东西可导出）。
    pub engine_playing: bool,
    /// 帧文件目录 / 帧率（启用时用的是这两个值）。
    pub frame_dir: String,
    pub fps: u32,
    /// 给用户看的一句话说明（状态页直接展示）。
    pub note: String,
}

// ---------------------------------------------------------------------------
// 纯逻辑：appletsrc 解析、脚本生成（可单测，不碰进程/文件系统）
// ---------------------------------------------------------------------------

/// 一个桌面容器切换前的壁纸配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainmentWallpaper {
    /// appletsrc 里的容器编号（`[Containments][N]`）。
    pub index: u32,
    /// 原来的壁纸插件 id（如 `org.kde.image`）。
    pub plugin: String,
    /// 该插件 General 组的键值（如 `Image=file:///...`）。
    pub config: Vec<(String, String)>,
}

/// 切换前的全部桌面壁纸（存进 settings，`disable` 用它还原）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WallpaperSnapshot {
    pub containments: Vec<ContainmentWallpaper>,
}

impl WallpaperSnapshot {
    /// 所有容器都用同一个插件时返回它（多屏同款壁纸的常见情况 → 可以统一还原）。
    pub fn uniform_plugin(&self) -> Option<&str> {
        let first = self.containments.first()?.plugin.as_str();
        if self.containments.iter().all(|c| c.plugin == first) {
            Some(first)
        } else {
            None
        }
    }
}

/// 解析 appletsrc，取出每个桌面容器当前的壁纸插件与其 General 配置。
///
/// 只认 `[Containments][N]`（容器自身，含 `wallpaperplugin=`）与
/// `[Containments][N][Wallpaper][<plugin>][General]`（该容器的壁纸配置）。
/// Plasma 的壁纸配置组里可能还有 `[Containments][N][Wallpaper][org.kde.image][General]` 之外的
/// 兄弟组（如 `Wallpaper=...`），那些我们不关心：还原时按插件 id 重写 General 组即可。
pub fn parse_wallpapers(text: &str) -> WallpaperSnapshot {
    // 第一步：容器自身的 wallpaperplugin
    let mut plugins: Vec<(u32, String)> = Vec::new();
    // 第二步：General 组的键值
    let mut configs: std::collections::HashMap<u32, Vec<(String, String)>> =
        std::collections::HashMap::new();

    let mut cur_container: Option<u32> = None; // 正在 `[Containments][N]` 段
    let mut cur_general: Option<u32> = None; // 正在 `[Containments][N][Wallpaper][p][General]` 段

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            cur_container = None;
            cur_general = None;
            let sec = line.trim_start_matches('[').trim_end_matches(']');
            let parts: Vec<&str> = sec.split("][").collect();
            if parts.len() == 2 && parts[0] == "Containments" {
                cur_container = parts[1].parse::<u32>().ok();
            } else if parts.len() == 5
                && parts[0] == "Containments"
                && parts[2] == "Wallpaper"
                && parts[4] == "General"
            {
                cur_general = parts[1].parse::<u32>().ok();
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if let Some(n) = cur_container {
            if k == "wallpaperplugin" && !v.is_empty() {
                plugins.push((n, v.to_string()));
            }
        } else if let Some(n) = cur_general {
            // 同一个键重复出现时保留最后一条（INI 语义）
            let entry = configs.entry(n).or_default();
            match entry.iter_mut().find(|(ek, _)| ek == k) {
                Some(slot) => slot.1 = v.to_string(),
                None => entry.push((k.to_string(), v.to_string())),
            }
        }
    }

    let mut containments: Vec<ContainmentWallpaper> = plugins
        .into_iter()
        .map(|(index, plugin)| ContainmentWallpaper {
            index,
            plugin,
            config: configs.remove(&index).unwrap_or_default(),
        })
        .collect();
    containments.sort_by_key(|c| c.index);
    WallpaperSnapshot { containments }
}

/// appletsrc 里当前是否有容器正在用本插件。
pub fn snapshot_uses_plugin(snap: &WallpaperSnapshot, plugin_id: &str) -> bool {
    snap.containments.iter().any(|c| c.plugin == plugin_id)
}

/// 把任意字符串转成 JS 单引号字面量（路径里出现 `\` 或 `'` 也不破串）。
fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// 生成「把桌面壁纸换成本插件」的 Plasma 脚本。
///
/// 只写插件真正需要的键：帧路径/序号路径/序号开关/填充模式/状态文字。
/// 其余（`RefreshMs`、`StallMs`…）保持插件包里的默认值，避免把用户手动改过的
/// 配置项在每次启用时悄悄覆盖。
pub fn build_enable_script(plugin_id: &str, frame_dir: &str) -> String {
    let dir = frame_dir.trim_end_matches('/');
    let frame = format!("{dir}/frame.ppm");
    let seq = format!("{dir}/frame.seq");
    let id = js_str(plugin_id);
    let mut js = String::new();
    js.push_str("var ds = desktops();\n");
    js.push_str("for (var i = 0; i < ds.length; i++) {\n");
    js.push_str("  var d = ds[i];\n");
    js.push_str(&format!("  d.wallpaperPlugin = {id};\n"));
    js.push_str(&format!(
        "  d.currentConfigGroup = ['Wallpaper', {id}, 'General'];\n"
    ));
    js.push_str(&format!("  d.writeConfig('FramePath', {});\n", js_str(&frame)));
    js.push_str(&format!("  d.writeConfig('SeqPath', {});\n", js_str(&seq)));
    js.push_str("  d.writeConfig('UseSeqFile', true);\n");
    js.push_str("  d.writeConfig('FillMode', 1);\n");
    js.push_str("  d.writeConfig('ShowStatusText', false);\n");
    js.push_str("}\n");
    js
}

/// 生成「还原切换前壁纸」的 Plasma 脚本。
///
/// 所有容器原本同款壁纸（最常见）就统一写回：`desktops()` 的顺序与 appletsrc 的
/// 容器编号顺序不保证一致，按位置硬对会张冠李戴。只有原本就各屏不同款时才按位置对，
/// 且数量对不上时退化成「用第一条统一还原」（宁可一致也不写错插件）。
pub fn build_restore_script(snap: &WallpaperSnapshot) -> String {
    if snap.containments.is_empty() {
        return String::new();
    }
    let mut js = String::new();
    js.push_str("var ds = desktops();\n");
    js.push_str("for (var i = 0; i < ds.length; i++) {\n");
    js.push_str("  var d = ds[i];\n");
    if let Some(plugin) = snap.uniform_plugin() {
        js.push_str(&config_writes(plugin, &snap.containments[0].config, "  "));
    } else {
        // 每屏不同款：按位置对（数量一致才敢对），对不上就整屏统一用第一条。
        js.push_str("  var c = null;\n");
        js.push_str(&format!(
            "  if (ds.length === {}) {{\n",
            snap.containments.len()
        ));
        for (i, c) in snap.containments.iter().enumerate() {
            js.push_str(&format!(
                "    if (i === {i}) {{ c = {{ plugin: {}, config: {} }}; }}\n",
                js_str(&c.plugin),
                config_json(&c.config)
            ));
        }
        js.push_str("  } else if (ds.length > 0) {\n");
        js.push_str(&format!(
            "    c = {{ plugin: {}, config: {} }};\n",
            js_str(&snap.containments[0].plugin),
            config_json(&snap.containments[0].config)
        ));
        js.push_str("  }\n");
        js.push_str("  if (c) {\n");
        js.push_str("    d.wallpaperPlugin = c.plugin;\n");
        js.push_str("    d.currentConfigGroup = ['Wallpaper', c.plugin, 'General'];\n");
        js.push_str("    for (var k in c.config) { d.writeConfig(k, c.config[k]); }\n");
        js.push_str("  }\n");
    }
    js.push_str("}\n");
    js
}

/// 单条壁纸配置的写回片段。
fn config_writes(plugin: &str, config: &[(String, String)], indent: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("{indent}d.wallpaperPlugin = {};\n", js_str(plugin)));
    s.push_str(&format!(
        "{indent}d.currentConfigGroup = ['Wallpaper', {}, 'General'];\n",
        js_str(plugin)
    ));
    for (k, v) in config {
        s.push_str(&format!(
            "{indent}d.writeConfig({}, {});\n",
            js_str(k),
            js_str(v)
        ));
    }
    s
}

/// 把键值表转成 JS 对象字面量（用于按位置映射的还原分支）。
fn config_json(config: &[(String, String)]) -> String {
    let mut s = String::from("{");
    for (i, (k, v)) in config.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("{}: {}", js_str(k), js_str(v)));
    }
    s.push('}');
    s
}

// ---------------------------------------------------------------------------
// 环境探测
// ---------------------------------------------------------------------------

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 插件包在用户目录的落点。
pub fn installed_dir(home: &Path) -> PathBuf {
    home.join(PLUGINS_SUBDIR).join(PLUGIN_ID)
}

fn appletsrc_path(home: &Path) -> PathBuf {
    home.join(APPLETSRC)
}

/// `/proc` 里有没有这个进程名（不依赖 pgrep）。
fn process_running(name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !fname.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(path.join("comm")) {
            if comm.trim() == name {
                return true;
            }
        }
    }
    false
}

/// 会话类型：Wayland 上还没有帧源（需要 KWin ScreenShot2/PipeWire），文案要区分。
fn session_kind() -> String {
    let forced_x11 = std::env::var("GDK_BACKEND").map(|v| v == "x11").unwrap_or(false)
        || std::env::var("QT_QPA_PLATFORM").map(|v| v == "xcb").unwrap_or(false);
    let has_wayland = std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if has_wayland && !forced_x11 {
        "wayland".to_string()
    } else if std::env::var("DISPLAY").map(|v| !v.is_empty()).unwrap_or(false) {
        "x11".to_string()
    } else {
        "unknown".to_string()
    }
}

/// 找出可用的 plasmashell 脚本调用工具。
fn script_tool() -> Option<String> {
    for bin in ["qdbus", "qdbus-qt5", "qdbus6", "gdbus"] {
        if which(bin).is_some() {
            return Some(bin.to_string());
        }
    }
    None
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}

/// 随应用分发的插件包目录（含 metadata.json）。
///
/// 查找顺序（都要求目录里有 metadata.json）：
/// 1. `WALLPAPEREM_PLASMA_PACKAGE`（脚本/排障用，直接指到包目录）
/// 2. 打包后的资源目录：`<resource_dir>/plasma-wallpaper/<PLUGIN_ID>`
/// 3. 源码树布局（`cargo run` / `cargo build` 未打包时）：可执行文件上溯、当前目录
pub fn resolve_package_dir(app: Option<&AppHandle>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(env) = std::env::var_os("WALLPAPEREM_PLASMA_PACKAGE") {
        candidates.push(PathBuf::from(env));
    }
    if let Some(app) = app {
        if let Ok(dir) = app.path().resource_dir() {
            candidates.push(dir.join("plasma-wallpaper").join(PLUGIN_ID));
            candidates.push(dir.join(PLUGIN_ID));
        }
    }
    // 源码树：<repo>/src-tauri/target/<profile>/WallpaperEM → <repo>/packaging/plasma-wallpaper
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        for _ in 0..4 {
            let Some(dir) = cur else { break };
            candidates.push(dir.join("packaging/plasma-wallpaper").join(PLUGIN_ID));
            candidates.push(dir.join("plasma-wallpaper").join(PLUGIN_ID));
            cur = dir.parent().map(Path::to_path_buf);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("packaging/plasma-wallpaper").join(PLUGIN_ID));
        candidates.push(cwd.join("../packaging/plasma-wallpaper").join(PLUGIN_ID));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("metadata.json").is_file())
}

/// 读取插件包/已装插件的版本号。
fn read_version(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("metadata.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("KPlugin")?
        .get("Version")?
        .as_str()
        .map(|s| s.to_string())
}

/// 递归拷贝目录（插件包就几个小文件，不引第三方依赖）。
fn copy_dir(src: &Path, dst: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("建目录 {} 失败：{e}", dst.display()))?;
    let mut count = 0usize;
    let entries = std::fs::read_dir(src).map_err(|e| format!("读目录 {} 失败：{e}", src.display()))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let meta = entry
            .metadata()
            .map_err(|e| format!("读 {} 元信息失败：{e}", from.display()))?;
        if meta.is_dir() {
            count += copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("拷贝 {} → {} 失败：{e}", from.display(), to.display()))?;
            count += 1;
        }
    }
    Ok(count)
}

/// 从 settings 表读一个键（DB 未就绪时返回 None 而不是报错：状态查询不该因为
/// DB 没起来就整个失败）。
fn setting_get(app: &AppHandle, key: &str) -> Option<String> {
    let db = app.try_state::<Arc<Mutex<rusqlite::Connection>>>()?;
    let conn = db.lock().ok()?;
    crate::db::get_setting(&conn, key)
}

fn setting_set(app: &AppHandle, key: &str, value: Option<&str>) -> Result<(), String> {
    let db = app
        .try_state::<Arc<Mutex<rusqlite::Connection>>>()
        .ok_or("DB 未就绪")?;
    let conn = db.lock().map_err(|e| e.to_string())?;
    match value {
        Some(v) => crate::db::set_setting(&conn, key, v),
        None => conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

/// 用 plasmashell 的脚本接口执行一段 Plasma JS。
///
/// `evaluateScript` 成功时返回空串；脚本内部抛异常时 D-Bus 调用仍可能成功，
/// 所以这里把非空 stdout 记进日志（plasmashell 会把 JS 错误写在里面）。
fn run_plasma_script(js: &str) -> Result<String, String> {
    if js.trim().is_empty() {
        return Ok(String::new());
    }
    let mut errors: Vec<String> = Vec::new();
    let mut cmds: Vec<(String, Vec<String>)> = Vec::new();
    for bin in ["qdbus", "qdbus-qt5", "qdbus6"] {
        if which(bin).is_some() {
            cmds.push((
                bin.to_string(),
                vec![
                    "org.kde.plasmashell".into(),
                    "/PlasmaShell".into(),
                    "org.kde.PlasmaShell.evaluateScript".into(),
                    js.to_string(),
                ],
            ));
        }
    }
    if which("gdbus").is_some() {
        cmds.push((
            "gdbus".into(),
            vec![
                "call".into(),
                "--session".into(),
                "--dest".into(),
                "org.kde.plasmashell".into(),
                "--object-path".into(),
                "/PlasmaShell".into(),
                "--method".into(),
                "org.kde.PlasmaShell.evaluateScript".into(),
                js.to_string(),
            ],
        ));
    }
    if cmds.is_empty() {
        return Err("没找到 qdbus/gdbus，无法调用 plasmashell 的脚本接口".into());
    }
    for (bin, args) in cmds {
        match std::process::Command::new(&bin).args(&args).output() {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                // 空串（qdbus 打印 ""）是正常返回；有内容就记下来便于排障
                if !stdout.is_empty() && stdout != "\"\"" {
                    tracing::warn!("KDE 脚本返回：{stdout}");
                }
                return Ok(stdout);
            }
            Ok(out) => errors.push(format!(
                "{bin}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => errors.push(format!("{bin}: {e}")),
        }
    }
    Err(format!("调用 plasmashell 脚本接口失败（{}）", errors.join("；")))
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 收集状态快照（只读）。
pub fn status(app: &AppHandle) -> KdePluginStatus {
    let home = home_dir();
    let plasmashell = process_running("plasmashell");
    let tool = script_tool();
    let package = resolve_package_dir(Some(app));
    let installed = home.as_deref().map(installed_dir);
    let installed_ok = installed.as_deref().map(|d| d.join("metadata.json").is_file()).unwrap_or(false);
    let snapshot = home
        .as_deref()
        .and_then(|h| std::fs::read_to_string(appletsrc_path(h)).ok())
        .map(|t| parse_wallpapers(&t))
        .unwrap_or_default();
    let enabled = setting_get(app, SETTING_ENABLED).as_deref() == Some("1");
    let exporting = crate::wallpaper::frame_export::status_global().running;
    let engine_playing = app
        .try_state::<crate::wallpaper::WallpaperEngineState>()
        .map(|s| !s.windows.lock().map(|w| w.is_empty()).unwrap_or(true))
        .unwrap_or(false);
    let frame_dir = setting_get(app, SETTING_FRAME_DIR).unwrap_or_else(|| DEFAULT_FRAME_DIR.to_string());
    let fps = setting_get(app, SETTING_FPS)
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(DEFAULT_FPS);

    let mut reason = String::new();
    if !cfg!(target_os = "linux") {
        reason = "只有 Linux 上的 KDE Plasma 才用得上这套".into();
    } else if !plasmashell {
        reason = "没检测到正在运行的 plasmashell".into();
    } else if tool.is_none() {
        reason = "没找到 qdbus/gdbus".into();
    }
    let supported = reason.is_empty();

    let note = if !supported {
        reason.clone()
    } else if !installed_ok {
        "插件包还没装到用户目录（点「启用」会先装）".into()
    } else if snapshot_uses_plugin(&snapshot, PLUGIN_ID) {
        if exporting {
            "已启用：桌面壁纸是本插件，引擎正在导出帧".into()
        } else {
            "桌面壁纸是本插件，但引擎没有在导出帧（壁纸会停在最后一帧）".into()
        }
    } else if engine_playing {
        "插件已装好，随时可以切过去".into()
    } else {
        "插件已装好；先在库里应用一张动态壁纸再启用".into()
    };

    KdePluginStatus {
        supported,
        unsupported_reason: reason,
        session: session_kind(),
        plasmashell,
        script_tool: tool,
        package_ready: package.is_some(),
        package_dir: package.map(|p| p.display().to_string()),
        installed: installed_ok,
        installed_version: installed.as_deref().and_then(read_version),
        installed_dir: installed
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        active: snapshot_uses_plugin(&snapshot, PLUGIN_ID),
        enabled,
        exporting,
        engine_playing,
        frame_dir,
        fps,
        note,
    }
}

// ---------------------------------------------------------------------------
// 启用 / 关闭
// ---------------------------------------------------------------------------

/// 启用时的可调参数（设置页用默认值，MCP/CLI 可以改）。
#[derive(Debug, Clone)]
pub struct EnableOptions {
    pub frame_dir: String,
    pub fps: u32,
    pub max_width: Option<u32>,
}

impl Default for EnableOptions {
    fn default() -> Self {
        Self {
            frame_dir: DEFAULT_FRAME_DIR.to_string(),
            fps: DEFAULT_FPS,
            max_width: Some(1920),
        }
    }
}

impl EnableOptions {
    pub fn from_settings(app: &AppHandle) -> Self {
        let mut o = Self::default();
        if let Some(dir) = setting_get(app, SETTING_FRAME_DIR) {
            if !dir.trim().is_empty() {
                o.frame_dir = dir;
            }
        }
        if let Some(fps) = setting_get(app, SETTING_FPS).and_then(|v| v.parse::<u32>().ok()) {
            o.fps = fps.clamp(1, 60);
        }
        o
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnableOutcome {
    /// 这次有没有把包写进用户目录。
    pub installed: bool,
    pub plugin_dir: String,
    pub frame_dir: String,
    /// 切过来的桌面容器数量。
    pub switched_desktops: usize,
    /// 引擎的帧导出状态。
    pub export: crate::wallpaper::frame_export::ExportStatus,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisableOutcome {
    /// 有没有把切换前的壁纸配置写回去。
    pub restored: bool,
    /// 有没有停掉帧导出。
    pub stopped: bool,
    pub note: String,
}

/// 一键启用：装包 → 记住原壁纸 → 切到本插件 → 开始导出帧。
///
/// 每一步都能单独失败并给出人话原因；失败在「切壁纸」之前都不会留下半成品状态
/// （包已装是无害的，切壁纸之后导出失败也只是画面停在最后一帧）。
pub async fn enable(app: &AppHandle, opts: EnableOptions) -> Result<EnableOutcome, String> {
    if !cfg!(target_os = "linux") {
        return Err("KDE 插件只能在 Linux 上启用".into());
    }
    if !process_running("plasmashell") {
        return Err("没检测到正在运行的 plasmashell：这套只对 KDE Plasma 桌面生效".into());
    }
    // 没有正在播放的壁纸就没有帧可导 —— 提前挡住，否则用户会看到一个兜底色的桌面
    let playing = app
        .try_state::<crate::wallpaper::WallpaperEngineState>()
        .map(|s| !s.windows.lock().map(|w| w.is_empty()).unwrap_or(true))
        .unwrap_or(false);
    if !playing {
        return Err("还没有正在播放的壁纸：先在库里应用一张动态壁纸，再回来一键启用".into());
    }
    let home = home_dir().ok_or("读不到 HOME，无法定位 Plasma 的用户目录")?;
    let package = resolve_package_dir(Some(app))
        .ok_or("应用里没有带上 Plasma 插件包（packaging/plasma-wallpaper）")?;

    // 1) 装包（每次都覆盖，保证用的是随应用分发的版本）
    let target = installed_dir(&home);
    let files = copy_dir(&package, &target)?;
    tracing::info!(
        "KDE 插件已写入 {}（{} 个文件，来自 {}）",
        target.display(),
        files,
        package.display()
    );

    // 2) 记住原壁纸（已经是本插件时不覆盖，避免把「原壁纸」记成自己）
    let src = appletsrc_path(&home);
    let before = std::fs::read_to_string(&src).unwrap_or_default();
    let snapshot = parse_wallpapers(&before);
    let already = snapshot_uses_plugin(&snapshot, PLUGIN_ID);
    if !already {
        let json = serde_json::to_string(&snapshot).map_err(|e| e.to_string())?;
        setting_set(app, SETTING_PREVIOUS, Some(&json))?;
    }
    setting_set(app, SETTING_FRAME_DIR, Some(&opts.frame_dir))?;
    setting_set(app, SETTING_FPS, Some(&opts.fps.to_string()))?;

    // 3) 切壁纸（plasmashell 自己的脚本接口）
    let js = build_enable_script(PLUGIN_ID, &opts.frame_dir);
    run_plasma_script(&js)?;

    // 4) 开始导出帧（插件模式：整窗透明而不是隐藏；Linux 上抓 X11 窗口而不是页面读回）
    let cfg = crate::wallpaper::frame_export::ExportConfig::new()
        .with_dir(&opts.frame_dir)
        .with_fps(opts.fps)
        .with_max_width(opts.max_width)
        .with_hide_window(true)
        .with_window_capture(true);
    let export = crate::wallpaper::frame_export::start_global_async(app.clone(), cfg).await?;

    setting_set(app, SETTING_ENABLED, Some("1"))?;

    let after = std::fs::read_to_string(&src).unwrap_or_default();
    let switched = parse_wallpapers(&after).containments.len();
    Ok(EnableOutcome {
        installed: true,
        plugin_dir: target.display().to_string(),
        frame_dir: opts.frame_dir.clone(),
        switched_desktops: switched,
        export,
        note: format!(
            "已把桌面壁纸换成 {PLUGIN_LABEL} 并开始导出帧（{} → {}）",
            opts.frame_dir,
            format!("{}/frame.ppm", opts.frame_dir.trim_end_matches('/'))
        ),
    })
}

/// 关闭：停导出 → 把原壁纸写回去（包留在用户目录，重启用不必重装）。
pub async fn disable(app: &AppHandle) -> Result<DisableOutcome, String> {
    if !cfg!(target_os = "linux") {
        return Err("KDE 插件只能在 Linux 上关闭".into());
    }
    let stopped = {
        let before = crate::wallpaper::frame_export::status_global().running;
        if before {
            crate::wallpaper::frame_export::stop_global_async().await;
        }
        before
    };

    let mut restored = false;
    if let Some(json) = setting_get(app, SETTING_PREVIOUS) {
        if let Ok(snapshot) = serde_json::from_str::<WallpaperSnapshot>(&json) {
            let js = build_restore_script(&snapshot);
            if !js.trim().is_empty() {
                run_plasma_script(&js)?;
                restored = true;
            }
        }
    }
    setting_set(app, SETTING_ENABLED, None)?;
    if restored {
        setting_set(app, SETTING_PREVIOUS, None)?;
    }
    Ok(DisableOutcome {
        restored,
        stopped,
        note: if restored {
            "已还原切换前的壁纸并停掉帧导出".into()
        } else {
            "已停掉帧导出（没有记录到切换前的壁纸，桌面壁纸需要在 Plasma 设置里改回）".into()
        },
    })
}

/// 应用启动时自愈：上次是启用状态 → 如果桌面壁纸还是本插件，就把帧导出接回去。
///
/// 只在「壁纸确实还是本插件」时动手：用户若在 Plasma 设置里自己换回了别的壁纸，
/// 这里只清掉标记，不去抢桌面。
pub async fn resume_on_startup(app: AppHandle) {
    if !cfg!(target_os = "linux") {
        return;
    }
    if setting_get(&app, SETTING_ENABLED).as_deref() != Some("1") {
        return;
    }
    let Some(home) = home_dir() else { return };
    let snapshot = std::fs::read_to_string(appletsrc_path(&home))
        .map(|t| parse_wallpapers(&t))
        .unwrap_or_default();
    if !snapshot_uses_plugin(&snapshot, PLUGIN_ID) {
        tracing::info!("KDE 插件：壁纸已被改回其它插件，清掉启用标记");
        let _ = setting_set(&app, SETTING_ENABLED, None);
        return;
    }
    if crate::wallpaper::frame_export::status_global().running {
        return;
    }
    tracing::info!("KDE 插件：恢复上次的帧导出");
    match enable(&app, EnableOptions::from_settings(&app)).await {
        Ok(out) => tracing::info!("KDE 插件：{}", out.note),
        Err(e) => tracing::warn!("KDE 插件：启动恢复失败：{e}"),
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令（设置页「通用 → KDE 桌面集成」用）
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn kde_plugin_status(app: AppHandle) -> KdePluginStatus {
    status(&app)
}

#[tauri::command]
pub async fn kde_plugin_enable(app: AppHandle) -> Result<EnableOutcome, String> {
    enable(&app, EnableOptions::from_settings(&app)).await
}

#[tauri::command]
pub async fn kde_plugin_disable(app: AppHandle) -> Result<DisableOutcome, String> {
    disable(&app).await
}

// ---------------------------------------------------------------------------
// 单测
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[ActionPlugins][0]
RightButton;NoModifier=org.kde.contextmenu

[Containments][1]
ItemGeometries-1024x768=
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///usr/share/wallpapers/Altai/contents/images/1080x1920.png
FillMode=2

[Containments][2]
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][2][Wallpaper][org.kde.image][General]
Image=file:///tmp/other.png
";

    #[test]
    fn parses_each_containment_wallpaper() {
        let snap = parse_wallpapers(SAMPLE);
        assert_eq!(snap.containments.len(), 2);
        assert_eq!(snap.containments[0].index, 1);
        assert_eq!(snap.containments[0].plugin, "org.kde.image");
        assert_eq!(
            snap.containments[0].config,
            vec![
                (
                    "Image".to_string(),
                    "file:///usr/share/wallpapers/Altai/contents/images/1080x1920.png".to_string()
                ),
                ("FillMode".to_string(), "2".to_string()),
            ]
        );
        assert_eq!(snap.containments[1].index, 2);
        assert_eq!(snap.containments[1].config.len(), 1);
    }

    #[test]
    fn uniform_plugin_detected_and_mixed_not() {
        assert_eq!(parse_wallpapers(SAMPLE).uniform_plugin(), Some("org.kde.image"));
        let mixed = "\
[Containments][1]
wallpaperplugin=org.kde.image
[Containments][2]
wallpaperplugin=io.github.oneincase.wallpaperem
";
        assert_eq!(parse_wallpapers(mixed).uniform_plugin(), None);
        assert!(snapshot_uses_plugin(
            &parse_wallpapers(mixed),
            "io.github.oneincase.wallpaperem"
        ));
    }

    #[test]
    fn empty_or_unrelated_config_is_empty_snapshot() {
        assert!(parse_wallpapers("").containments.is_empty());
        assert!(parse_wallpapers("[Containments][1]\nplugin=org.kde.plasma.folder\n")
            .containments
            .is_empty());
        // wallpaperplugin 后面带空值：不算一个有效配置
        assert!(parse_wallpapers("[Containments][1]\nwallpaperplugin=\n")
            .containments
            .is_empty());
    }

    #[test]
    fn enable_script_sets_plugin_and_frame_paths() {
        let js = build_enable_script(PLUGIN_ID, "/dev/shm/wallpaperem/");
        assert!(js.contains("var ds = desktops();"));
        assert!(js.contains("d.wallpaperPlugin = 'io.github.oneincase.wallpaperem';"));
        assert!(js.contains(
            "d.currentConfigGroup = ['Wallpaper', 'io.github.oneincase.wallpaperem', 'General'];"
        ));
        // 目录结尾多余的斜杠要吃掉，否则会写出 /dev/shm/wallpaperem//frame.ppm
        assert!(js.contains("d.writeConfig('FramePath', '/dev/shm/wallpaperem/frame.ppm');"));
        assert!(js.contains("d.writeConfig('SeqPath', '/dev/shm/wallpaperem/frame.seq');"));
        assert!(js.contains("d.writeConfig('UseSeqFile', true);"));
        assert!(js.contains("d.writeConfig('ShowStatusText', false);"));
    }

    #[test]
    fn restore_script_uniform_writes_every_desktop() {
        let snap = parse_wallpapers(SAMPLE);
        let js = build_restore_script(&snap);
        assert!(js.contains("d.wallpaperPlugin = 'org.kde.image';"));
        assert!(js.contains(
            "d.writeConfig('Image', 'file:///usr/share/wallpapers/Altai/contents/images/1080x1920.png');"
        ));
        // 统一还原是「对每个桌面同一套写法」，不该出现按位置映射的 var c
        assert!(!js.contains("var c = null;"));
    }

    #[test]
    fn restore_script_mixed_maps_by_position() {
        let mixed = "\
[Containments][1]
wallpaperplugin=org.kde.image
[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///a.png
[Containments][2]
wallpaperplugin=com.example.other
[Containments][2][Wallpaper][com.example.other][General]
Path=/tmp/b
";
        let js = build_restore_script(&parse_wallpapers(mixed));
        assert!(js.contains("if (ds.length === 2) {"));
        assert!(js.contains("plugin: 'org.kde.image'"));
        assert!(js.contains("plugin: 'com.example.other'"));
        // 键也带引号（配置键来自用户的 appletsrc，不能假设是合法标识符）
        assert!(js.contains("'Image': 'file:///a.png'"));
        assert!(js.contains("'Path': '/tmp/b'"));
    }

    #[test]
    fn empty_snapshot_produces_no_script() {
        assert!(build_restore_script(&WallpaperSnapshot::default()).is_empty());
    }

    #[test]
    fn js_str_escapes_quotes_and_backslashes() {
        assert_eq!(js_str("plain"), "'plain'");
        assert_eq!(js_str("a'b"), "'a\\'b'");
        assert_eq!(js_str("c:\\tmp\\x"), "'c:\\\\tmp\\\\x'");
        // 路径里的引号不会把脚本文本撑破
        let js = build_enable_script("p", "/tmp/it's here");
        assert!(js.contains("'/tmp/it\\'s here/frame.ppm'"));
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let snap = parse_wallpapers(SAMPLE);
        let json = serde_json::to_string(&snap).unwrap();
        let back: WallpaperSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }
}
