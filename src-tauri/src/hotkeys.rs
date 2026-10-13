// 用户可配置快捷键（主窗口「快捷键」页）。
//
// 七个动作各有绑定（默认一组，可录制替换）：
//   主窗口显示/隐藏 · 壁纸设置窗口显示/隐藏 · 手动暂停/播放 · 自动暂停开启/关闭 ·
//   定时切换开启/关闭 · 下一个壁纸 · 上一个壁纸（后两个在存在轮播列表时生效：
//   暂停轮播也照旧能手动切，列表被移除 —— 停止轮播 / 手动设了单张壁纸 —— 才忽略）。
//
// 注册策略（macOS 的关键取舍）：
//   - **⌘+单键**（⌘M/⌘H 这类系统惯例键）注册为**应用菜单快捷键**：全局注册会
//     把其它所有 App 的 ⌘M/⌘H 一起劫走（最小化/隐藏失效），所以只在本应用
//     聚焦时生效 —— 这正是「mac 下默认的 command+m/h」应有的语义。
//   - **其余组合**注册为全局热键（tauri-plugin-global-shortcut），游戏/其它
//     应用在前台时也能用（⌘⇧P 暂停这一档一直是全局的，保持不变）。
//
// 录制的坑（用户明确点过名）：录制时如果菜单和全局热键还在，⌘+任意键会被
// 菜单键位或全局热键**吞掉**，keydown 根本到不了录制框（「不能录制 command+
// 其它键」）。所以录制前后整组挂起/恢复：hotkeys_record_begin 摘菜单 + 注销
// 全部全局热键，record_end 反向恢复。
//
// KDE / Wayland（T-P7H）——本模块的第二个策略维度：
//   底层的 `global-hotkey 0.8.0` 只有 macos / windows / x11 三套实现，**没有
//   Wayland 后端**，`grab_key` 是 X11 专有（`src/platform_impl/x11/mod.rs`）。
//   Wayland 会话里注册要么直接失败，要么经 XWayland「成功」——但那种 grab 只在
//   X11 客户端拿到焦点时才触发，表现就是**静默失效**：状态显示已注册、按键没反应。
//   KDE 的全局快捷键真由 KWin 内嵌的 KGlobalAccelD 管理
//   （`kwin/src/globalshortcuts.cpp:97`），第三方应用的正路是
//   `org.freedesktop.portal.GlobalShortcuts`（KDE 后端自 Plasma 5.27 起，
//   `xdg-desktop-portal-kde/src/globalshortcuts.cpp`）。所以：
//     * KDE/Wayland → [`ShortcutBackend::KdeSystem`]：**不做 X11 抢键**，每个动作
//       标 [`HotkeyStatus::SystemManaged`] 并带出 [`KDE_SYSTEM_MANAGED_NOTE`] 文案；
//     * 非 KDE/Wayland → [`ShortcutBackend::Unsupported`]：同样不抢键，标
//       [`HotkeyStatus::Unavailable`] 并说明原因（本应用尚未接 portal）；
//     * KDE/X11 → 仍走 X11 全局热键（X11 下 XGrabKey 确实全局有效），但默认键位
//       必须避开 KWin 的保留区（[`KDE_BANNED_DEFAULTS`] / [`kde_default_conflict`]）。
//   portal 的接缝已留好（[`PortalRegistration`] / [`portal_bind_plan`] /
//   [`portal_register`]，标注 `TODO(P7-Wayland-portal)`）：实现后状态自动变成
//   [`HotkeyStatus::Bound`]，调用方与前端 JSON 形状都不用改。
use std::collections::HashMap;
use std::sync::Mutex;

use crate::kde::{KdeEnv, SessionKind};

use tauri::{
    menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu},
    AppHandle, Manager, Wry,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

/// 动作 id → 界面文案（中文原文即 i18n 键，同全项目惯例）。
/// 顺序 = 「快捷键」页的展示顺序。
pub const ACTIONS: &[(&str, &str)] = &[
    ("toggle_main_window", "主窗口显示/隐藏"),
    ("toggle_props_window", "壁纸设置窗口显示/隐藏"),
    ("toggle_pause", "手动暂停/播放"),
    ("toggle_auto_pause", "自动暂停开启/关闭"),
    ("toggle_rotation", "定时切换开启/关闭"),
    ("next_wallpaper", "下一个壁纸"),
    ("prev_wallpaper", "上一个壁纸"),
];

/// 宿主 OS 类别。**纯数据**（不进 `#[cfg]`）：这样「macOS / Windows /
/// Linux+KDE / Linux 非 KDE」四条分支的逻辑与单测在任何宿主上都能跑全 ——
/// macOS 开发机上也要能验证 KDE 分支，否则那套策略永远没人看（同文件下方
/// 「两份保留清单都常驻」的理由）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsClass {
    Macos,
    Windows,
    Linux,
}

pub fn current_os_class() -> OsClass {
    if cfg!(target_os = "macos") {
        OsClass::Macos
    } else if cfg!(target_os = "windows") {
        OsClass::Windows
    } else {
        OsClass::Linux
    }
}

/// 全局快捷键在本会话由谁负责（判定是纯函数，理由见文件头 KDE/Wayland 段）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutBackend {
    /// 本应用用 tauri-plugin-global-shortcut 真注册（macOS / Windows / Linux X11）。
    App,
    /// KDE 管理：KWin 内嵌 KGlobalAccelD，第三方不抢键（抢了 = 静默失效）。
    KdeSystem,
    /// 平台没有可用后端：Wayland 且非 KDE（`global-hotkey` 无 Wayland 实现，
    /// 本应用尚未接 portal）。
    Unsupported,
}

impl ShortcutBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            ShortcutBackend::App => "app",
            ShortcutBackend::KdeSystem => "kde_system",
            ShortcutBackend::Unsupported => "unsupported",
        }
    }

    /// 由平台/桌面代管时给前端的一句话说明（`App` 没有）。
    pub fn note(self) -> Option<&'static str> {
        match self {
            ShortcutBackend::App => None,
            ShortcutBackend::KdeSystem => Some(KDE_SYSTEM_MANAGED_NOTE),
            ShortcutBackend::Unsupported => Some(WAYLAND_UNSUPPORTED_REASON),
        }
    }
}

/// 纯函数：当前 OS + 桌面 → 谁负责全局快捷键（文件头「KDE / Wayland」段的判定）。
///
/// * macOS / Windows → [`ShortcutBackend::App`]；
/// * Linux/X11（含 KDE/X11）→ [`ShortcutBackend::App`]：X11 的 XGrabKey 确实全局
///   有效，被 KWin 占用的组合会让注册直接报错，如实落成 [`HotkeyStatus::Failed`]；
/// * Linux/Wayland + KDE → [`ShortcutBackend::KdeSystem`]（不抢键，走 portal 接缝）；
/// * Linux/Wayland 非 KDE → [`ShortcutBackend::Unsupported`]；
/// * 会话未知（既无 `DISPLAY`/`WAYLAND_DISPLAY` 也无 `XDG_SESSION_TYPE`）→ 仍试
///   `App`：注册失败会带出底层错误，比「猜成不支持」信息量更大。
pub fn shortcut_backend_with(os: OsClass, kde: &KdeEnv) -> ShortcutBackend {
    match os {
        OsClass::Macos | OsClass::Windows => ShortcutBackend::App,
        OsClass::Linux => match kde.session {
            SessionKind::Wayland => {
                if kde.is_kde {
                    ShortcutBackend::KdeSystem
                } else {
                    ShortcutBackend::Unsupported
                }
            }
            SessionKind::X11 | SessionKind::Unknown => ShortcutBackend::App,
        },
    }
}

/// [`shortcut_backend_with`] 的真机版本（macOS 分支不摸 D-Bus）。
pub fn shortcut_backend() -> ShortcutBackend {
    match current_os_class() {
        OsClass::Macos => ShortcutBackend::App,
        os => shortcut_backend_with(os, crate::kde::env()),
    }
}

/// KDE/Wayland 下每个动作的状态说明。措辞要点：**别承诺当前不生效的东西**。
/// 本应用尚未接 portal/KGlobalAccel，所以动作现在不会出现在系统设置的快捷键表里；
/// portal 接上之后（KDE 会弹一次确认对话框）才会出现、才能改键。
pub const KDE_SYSTEM_MANAGED_NOTE: &str = "KDE Wayland：全局快捷键由桌面管理（KWin 内嵌 KGlobalAccelD）。\
本应用不在 Wayland 上做 X11 抢键（那会「显示已注册、按键却不生效」）。键位已保存在本地；\
接入 org.freedesktop.portal.GlobalShortcuts 后可在「系统设置 → 快捷键」中查看与修改，\
在此之前请用主窗口或托盘操作。";

/// Wayland 且非 KDE 的说明。
pub const WAYLAND_UNSUPPORTED_REASON: &str = "Wayland 会话下 global-hotkey 只有 X11 后端；\
本应用尚未接入 org.freedesktop.portal.GlobalShortcuts，故不注册（避免静默失效）。\
键位已保存，接入后自动生效。";

/// 动作没有任何绑定时的状态说明（用户清空绑定是合法状态）。
pub const NO_BINDING_NOTE: &str = "该动作当前没有绑定按键";

/// 录制期间的状态说明：全局热键与菜单被临时摘除（见 `hotkeys_record_begin`）。
pub const RECORDING_NOTE: &str = "录制中：全局热键已临时摘除，录制结束自动恢复";

/// KWin/Plasma 默认占用、KDE 会话下**不允许**当默认值的组合。
///
/// 依据（KWin/Plasma 默认快捷键；调研见 `docs/kde-plasma-constraints.md` §9）：
/// `Meta+方向`（贴边/最大化）、`Meta+L`（锁屏）、`Meta+P`（显示切换）、
/// `Meta+数字`（任务栏/活动）、`Ctrl+Alt+*`（切虚拟桌面、TTY 等）。
/// 本表只用于「默认值不许落在这里」，用户显式录制这些组合仍然允许（前端用
/// [`RESERVED_KDE`] 提示冲突）。
pub const KDE_BANNED_DEFAULTS: &[&str] = &[
    "super+left",
    "super+right",
    "super+up",
    "super+down",
    "super+l",
    "super+p",
    "super+1",
    "super+2",
    "super+3",
    "ctrl+alt+left",
    "ctrl+alt+right",
    "ctrl+alt+up",
    "ctrl+alt+down",
    "ctrl+alt+t",
    "ctrl+alt+l",
    "ctrl+alt+delete",
];

/// 纯函数：该组合在 KDE 下是否属于「默认值禁区」。`None` = 可安全用作默认值。
///
/// 判定与 [`KDE_BANNED_DEFAULTS`] 同源，但按**语义**判（而不是查表），这样
/// `super+9`、`super+shift+left` 这类表里没列的写法也挡得住：
///   1. 用到 `super`（= `cmd`/`meta`/`LOGO`）→ KWin 自己拿 Meta 当快捷键前缀，
///      第三方默认值不该占；
///   2. 同时按 `ctrl` 和 `alt` → KWin/GNOME 的传统保留区（切桌面等）；
///   3. 方向键带修饰 → KWin 的贴边/最大化默认键。
pub fn kde_default_conflict(accel: &str) -> Option<&'static str> {
    // 先查显式表：表里是逐条核过的 KWin/Plasma 默认键（语义规则之外的兜底）
    let lower = accel.trim().to_ascii_lowercase();
    if KDE_BANNED_DEFAULTS.contains(&lower.as_str()) {
        return Some(
            "该组合在 KWin/Plasma 的默认快捷键表里（见 docs/kde-plasma-constraints.md §9）",
        );
    }
    let parts: Vec<String> = accel
        .split('+')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let (key, mods) = parts.split_last()?;
    let has_super = mods
        .iter()
        .any(|m| matches!(m.as_str(), "super" | "cmd" | "command" | "meta" | "logo"));
    let has_ctrl = mods.iter().any(|m| m == "ctrl" || m == "control");
    let has_alt = mods.iter().any(|m| m == "alt" || m == "option");
    let is_arrow = matches!(
        key.as_str(),
        "left" | "right" | "up" | "down" | "arrowleft" | "arrowright" | "arrowup" | "arrowdown"
    );

    if has_super {
        return Some("KWin 用 Meta(Super) 作为自身快捷键修饰键（Meta+方向/数字/L/P），默认值不应占用");
    }
    if has_ctrl && has_alt {
        return Some("Ctrl+Alt+* 是 KWin/GNOME 的保留区（切虚拟桌面、TTY 等）");
    }
    if is_arrow && !mods.is_empty() {
        return Some("方向键带修饰是 KWin 的贴边/最大化默认键");
    }
    None
}

/// macOS 默认：主窗口用系统惯例键 ⌘M/⌘H（菜单快捷键，不劫持其它 App），
/// 另配一枚 ⌘⇧M 作**全局**键 —— ⌘M/⌘H 只在应用内生效，窗口隐藏后人在
/// 其它应用里要能把主窗口唤回来，必须有一枚全局的。
/// 其余沿用 ⌘⇧ 字母（历史上就在这套）。
fn mac_bindings(action: &str) -> Vec<String> {
    match action {
        "toggle_main_window" => vec!["cmd+m".into(), "cmd+h".into(), "cmd+shift+m".into()],
        "toggle_props_window" => vec!["cmd+shift+s".into()],
        "toggle_pause" => vec!["cmd+shift+p".into()],
        "toggle_auto_pause" => vec!["cmd+shift+a".into()],
        "toggle_rotation" => vec!["cmd+shift+r".into()],
        "next_wallpaper" => vec!["cmd+shift+n".into()],
        "prev_wallpaper" => vec!["cmd+shift+b".into()],
        _ => vec![],
    }
}

/// Windows 与非 KDE Linux：`Ctrl+Shift+字母`，全部走全局热键（无菜单键位可依）。
fn linux_generic_bindings(action: &str) -> Vec<String> {
    match action {
        "toggle_main_window" => vec!["ctrl+shift+m".into()],
        "toggle_props_window" => vec!["ctrl+shift+s".into()],
        "toggle_pause" => vec!["ctrl+shift+p".into()],
        "toggle_auto_pause" => vec!["ctrl+shift+a".into()],
        "toggle_rotation" => vec!["ctrl+shift+r".into()],
        "next_wallpaper" => vec!["ctrl+shift+n".into()],
        "prev_wallpaper" => vec!["ctrl+shift+b".into()],
        _ => vec![],
    }
}

/// KDE 默认键位：只占 `Ctrl+Shift+<字母>` 这片 KWin 默认表里空着的空间。
///
/// * 依据：KWin 的默认快捷键集中在 `Meta+*` 与 `Ctrl+Alt+*`（见表
///   [`KDE_BANNED_DEFAULTS`] 的注释），`Ctrl+Shift+字母` 不在其中；
/// * KDE 下第三方的 Meta(Super) 组合可能先被 KWin 自己吞掉，故 KDE 默认
///   **不使用 Meta**（由 [`kde_default_conflict`] 钉住）；
/// * 键位当前与通用 Linux 分支相同 —— 独立成函数是为了让 KDE 的策略能独立演进，
///   并由单测把上面两条约束钉死（改坏了立刻红）。
fn kde_bindings(action: &str) -> Vec<String> {
    match action {
        "toggle_main_window" => vec!["ctrl+shift+m".into()],
        "toggle_props_window" => vec!["ctrl+shift+s".into()],
        "toggle_pause" => vec!["ctrl+shift+p".into()],
        "toggle_auto_pause" => vec!["ctrl+shift+a".into()],
        "toggle_rotation" => vec!["ctrl+shift+r".into()],
        "next_wallpaper" => vec!["ctrl+shift+n".into()],
        "prev_wallpaper" => vec!["ctrl+shift+b".into()],
        _ => vec![],
    }
}

/// 平台/桌面判定是纯函数：任意宿主上都能把四条分支测一遍。
pub fn default_bindings_with(action: &str, os: OsClass, kde: &KdeEnv) -> Vec<String> {
    match os {
        OsClass::Macos => mac_bindings(action),
        OsClass::Windows => linux_generic_bindings(action),
        OsClass::Linux => {
            if kde.is_kde {
                kde_bindings(action)
            } else {
                linux_generic_bindings(action)
            }
        }
    }
}

/// [`default_bindings_with`] 的真机版本。
fn default_bindings(action: &str) -> Vec<String> {
    match current_os_class() {
        // macOS 分支与 KDE 无关，别为它去摸一次 D-Bus
        OsClass::Macos => mac_bindings(action),
        os => default_bindings_with(action, os, crate::kde::env()),
    }
}

/// 系统/菜单已占用、不建议覆盖的组合（macOS）。⌘M/⌘H 不在其中 —— 它们正是
/// 「主窗口显示/隐藏」的默认值，属于我们自己的动作。
///
/// 两份平台清单都常驻（不用 cfg 从编译里删掉），好让单测在任何平台上都能把两份
/// 解析一遍：CI 只构建不跑 cargo test，若各自清单只在各自平台校验，另一份永远
/// 没人看（键名写错不报错，只是静默不命中）。
#[allow(dead_code)]
const RESERVED_MACOS: &[&str] = &[
    "cmd+q", "cmd+w", "cmd+space", "cmd+tab", "cmd+`", "cmd+z", "cmd+shift+z", "cmd+x", "cmd+c",
    "cmd+v", "cmd+a",
];

/// Windows / Linux 的保留组合。命中只影响「冲突确认框」这一层提示（前端拿它比对录制
/// 结果），真能不能注册上仍由系统说了算 —— X11 会放行不少桌面组合，覆盖后表现为把
/// 桌面的快捷键抢过来（GNOME 的 Ctrl+Alt+←/→ 切工作区就是这样）。
/// 不列 `cmd+*`（super）：Windows 的 Win 键在系统层就被截走，webview 收不到，录制框
/// 根本录不出这个修饰键，列了也是永远命中不了的死条目。
#[allow(dead_code)]
const RESERVED_OTHER: &[&str] = &[
    // 窗口/任务切换（Windows 与多数 Linux 桌面同）
    "alt+f4",
    "alt+tab",
    "alt+esc",
    // Windows 专有
    "ctrl+shift+esc",
    "ctrl+shift+delete",
    // Linux 桌面（GNOME/KDE 默认键位）
    "ctrl+alt+t",
    "ctrl+alt+l",
    "ctrl+alt+delete",
    "ctrl+alt+arrowleft",
    "ctrl+alt+arrowright",
    "ctrl+alt+arrowup",
    "ctrl+alt+arrowdown",
    // 文本编辑惯例（与 mac 那份同口径）
    "ctrl+c",
    "ctrl+v",
    "ctrl+x",
    "ctrl+a",
    "ctrl+z",
    "ctrl+shift+z",
];

/// 当前平台的保留组合（`hotkeys_list` 返回给前端做冲突提示）
#[cfg(target_os = "macos")]
pub const RESERVED: &[&str] = RESERVED_MACOS;
#[cfg(not(target_os = "macos"))]
pub const RESERVED: &[&str] = RESERVED_OTHER;

/// KDE 专有的保留组合（供前端冲突提示；只在 KDE 会话下附加到 [`RESERVED_OTHER`] 后面）。
///
/// 用 `super+`（而不是 `meta+`）是因为解析器（`global-hotkey`）只认
/// `SUPER`/`CMD`/`COMMAND` 作为 Super 修饰键名，写 `meta+` 会解析失败 ——
/// 保留清单的既有单测会立刻抓到这种错字。
#[allow(dead_code)]
pub const RESERVED_KDE: &[&str] = &[
    // KWin 窗口管理（贴边/最大化/最小化/锁屏/显示切换）
    "super+left",
    "super+right",
    "super+up",
    "super+down",
    // KWin / Plasma 全局
    "super+l",
    "super+p",
    // 任务栏与活动
    "super+1",
    "super+2",
    "super+3",
    "super+4",
    "super+5",
    // 虚拟控制台（切 TTY）
    "ctrl+alt+f1",
    "ctrl+alt+f2",
    "ctrl+alt+f3",
    "ctrl+alt+f4",
    "ctrl+alt+f5",
    "ctrl+alt+f6",
];

/// 该平台 + 桌面下给前端做冲突提示的保留组合。
///
/// 非 KDE 的 Linux 会话不该冒出 KWin 的 `super+*`（那是 KDE 专用表）；
/// KDE/Wayland 上这些组合更是被 KWin 直接截走，录制框大概率录不到 ——
/// 但作为「冲突提示」多一条无害，反而能解释「为什么录不出来」。
pub fn reserved_for(os: OsClass, kde: &KdeEnv) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = match os {
        OsClass::Macos => RESERVED_MACOS.to_vec(),
        OsClass::Windows => RESERVED_OTHER.to_vec(),
        OsClass::Linux => {
            let mut v = RESERVED_OTHER.to_vec();
            if kde.is_kde {
                v.extend_from_slice(RESERVED_KDE);
            }
            v
        }
    };
    out.dedup();
    out
}

/// 每个动作当前的注册状态（前端「快捷键」页据此显示，而不是让用户猜）。
///
/// 这是本次改动的主产出：以前注册失败只有一行 `warn!`，用户看到的是「按了没反应」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyStatus {
    /// 已由本应用注册成功（macOS 的菜单加速键也算 —— 它在应用内生效）。
    Bound,
    /// 由桌面代管：本应用不抢键。当前 KDE/Wayland 落到这里
    /// （见 [`KDE_SYSTEM_MANAGED_NOTE`]）。
    SystemManaged { note: String },
    /// 本平台/本动作没有可用绑定：Wayland 无后端、或用户清空了该动作的绑定。
    Unavailable { reason: String },
    /// 尝试注册但失败（被系统或其它应用占用）。
    Failed { reason: String },
}

impl HotkeyStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            HotkeyStatus::Bound => "bound",
            HotkeyStatus::SystemManaged { .. } => "system_managed",
            HotkeyStatus::Unavailable { .. } => "unavailable",
            HotkeyStatus::Failed { .. } => "failed",
        }
    }

    /// 给用户看的补充说明（`Bound` 为 `None`）。
    pub fn detail(&self) -> Option<&str> {
        match self {
            HotkeyStatus::Bound => None,
            HotkeyStatus::SystemManaged { note } => Some(note),
            HotkeyStatus::Unavailable { reason } => Some(reason),
            HotkeyStatus::Failed { reason } => Some(reason),
        }
    }

    /// `hotkeys_list` 里的 JSON 形状：`{"state": "...", "detail": "..."}`。
    pub fn to_json(&self) -> serde_json::Value {
        match self.detail() {
            Some(d) => serde_json::json!({ "state": self.as_str(), "detail": d }),
            None => serde_json::json!({ "state": self.as_str() }),
        }
    }
}

/// 一行式摘要，给日志用（真机取证时 grep `hotkeys:`）。
pub fn status_digest(status: &HashMap<String, HotkeyStatus>) -> String {
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for s in status.values() {
        *counts.entry(s.as_str()).or_insert(0) += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    parts.sort();
    format!("{} 个动作 [{}]", status.len(), parts.join(" "))
}

// ---- org.freedesktop.portal.GlobalShortcuts（KDE Wayland 的正路）----
//
// TODO(P7-Wayland-portal)：D-Bus 调用未实现 —— 这里只交付**接缝**（签名 + 调用
// 顺序 + 纯函数部分）。诚实说明：`portal_register` 目前恒返回
// `PortalRegistration::Unavailable`，KDE/Wayland 的动作因此显示
// `SystemManaged`，不会假装「已注册」。

/// portal `BindShortcuts` 的一条条目（纯数据，便于单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortalShortcut {
    /// 我们的动作 id（portal 侧叫 shortcut id；`Activated` 信号原样带回来）。
    pub id: String,
    /// 给用户看的描述（KDE 的确认对话框里显示）。
    pub description: String,
    /// 首选键位，格式见 [`portal_trigger`]。`None` = 键位无法映射成 portal 写法，
    /// 仍然提交（让用户/后续实现在系统设置里指派）。
    pub preferred_trigger: Option<String>,
}

/// portal 注册结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalRegistration {
    /// 会话建立且 `BindShortcuts` 被接受。
    // 目前只有测试构造它（真实实现见 `TODO(P7-Wayland-portal)`）；这里保留是为了
    // 实现落地时 `to_json()`/调用方与前端 JSON 形状都不用改。
    #[allow(dead_code)]
    Registered {
        session_handle: String,
        portal_version: u32,
    },
    /// 没有 portal（旧版/非 KDE）或本应用尚未实现。
    Unavailable { reason: String },
}

impl PortalRegistration {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            PortalRegistration::Registered {
                session_handle,
                portal_version,
            } => serde_json::json!({
                "implemented": true,
                "state": "registered",
                "session_handle": session_handle,
                "portal_version": portal_version,
            }),
            PortalRegistration::Unavailable { reason } => serde_json::json!({
                "implemented": false,
                "state": "unavailable",
                "detail": reason,
            }),
        }
    }
}

/// 我们的快捷键写法 → portal `preferred_trigger`。
///
/// 格式依据 KDE 后端自己的解析器 `xdg-desktop-portal-kde/src/xdgshortcut.cpp::parse`：
/// * 修饰键名**大写**且只认 `SHIFT` / `CAPS` / `CTRL` / `ALT` / `NUM` / `LOGO`
///   （`LOGO` = Super/Meta）；
/// * 键名走 `xkb_keysym_from_name(..., XKB_KEYSYM_CASE_INSENSITIVE)`，即 XKB keysym
///   名（`a` / `F5` / `Left` / `space` …），大小写不敏感；
/// * 分隔符 `+`，空段非法（`CTRL++a` 直接失败）。
///
/// 返回 `None` = 本次映射不了（未知修饰键、未知键名、或没有修饰键的组合）——
/// 调用方只记 `debug`，不 panic、不静默吞掉（[`PortalShortcut::preferred_trigger`]
/// 会带 `None` 上报）。
pub fn portal_trigger(accel: &str) -> Option<String> {
    let parts: Vec<String> = accel
        .split('+')
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let (key, mods) = parts.split_last()?;
    if mods.is_empty() {
        // portal 的首选键位要求是「修饰键 + 键」的组合；裸 F 键留给用户在系统设置里指派
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for m in mods {
        let name = match m.as_str() {
            "ctrl" | "control" => "CTRL",
            "alt" | "option" => "ALT",
            "shift" => "SHIFT",
            "cmd" | "command" | "super" | "meta" | "logo" => "LOGO",
            _ => return None,
        };
        if !out.iter().any(|x| x == name) {
            out.push(name.to_string());
        }
    }
    let key = match key.as_str() {
        "left" | "arrowleft" => "Left".to_string(),
        "right" | "arrowright" => "Right".to_string(),
        "up" | "arrowup" => "Up".to_string(),
        "down" | "arrowdown" => "Down".to_string(),
        "space" => "space".to_string(),
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k.to_string(),
        k if k.starts_with('f') && k[1..].parse::<u32>().is_ok() => k.to_uppercase(),
        _ => return None,
    };
    out.push(key);
    Some(out.join("+"))
}

/// 把当前绑定整理成 portal 请求清单（纯函数）。
///
/// 每个动作取**第一枚能映射**的绑定作为 `preferred_trigger`（portal 的一个
/// shortcut id 只有一个首选键位；它的最终键位由用户在 KDE 的确认对话框/系统设置里定）。
pub fn portal_bind_plan(bindings: &HashMap<String, Vec<String>>) -> Vec<PortalShortcut> {
    let mut out = Vec::with_capacity(ACTIONS.len());
    for (id, label) in ACTIONS {
        let accels = bindings.get(*id);
        let trigger = accels.and_then(|v| v.iter().find_map(|a| portal_trigger(a)));
        if let Some(v) = accels {
            if trigger.is_none() && !v.is_empty() {
                tracing::debug!(
                    "hotkeys: portal 无法映射 {id} 的键位 {v:?}，将不提交 preferred_trigger"
                );
            }
        }
        out.push(PortalShortcut {
            id: (*id).to_string(),
            description: (*label).to_string(),
            preferred_trigger: trigger,
        });
    }
    out
}

/// 真实 portal 注册（**未实现**）。签名与调用顺序已经定死，实现时只补 D-Bus 部分。
///
/// 调用顺序（`org.freedesktop.portal.GlobalShortcuts`，KDE 后端实现见
/// `xdg-desktop-portal-kde/src/globalshortcuts.cpp`）：
/// 1. `CreateSession(a{sv}) -> o request`，options 带 `handle_token` 与
///    `session_handle_token`；等 `org.freedesktop.portal.Request::Response(u, a{sv})`
///    拿 `session_handle`（`o`，路径形如 `/org/freedesktop/portal/desktop/session/...`）。
/// 2. `BindShortcuts(o session_handle, a(sa{sv}) shortcuts, s parent_window, a{sv}) -> o request`
///    （`shortcuts` 每项 = `(id, {description, preferred_trigger})`）。
///    **KDE 会弹一次确认对话框**（`GlobalShortcutsDialog.qml`）；用户拒绝时 portal 仍会
///    以「无键位」注册，之后可在系统设置 → 快捷键里指派（上游代码注释明确写了这条）。
/// 3. 订阅 `Activated(o session_handle, s shortcut_id, t timestamp, a{sv} options)`
///    → 按 shortcut_id 调 [`dispatch`]（与全局热键 handler 汇合）。
/// 4. 订阅 `org.freedesktop.portal.Session::Closed`；会话失效后要重新 `CreateSession`
///    （portal 的会话随应用/桌面的生命周期结束）。
///
/// 需要 zbus（`Cargo.toml` 的 Linux target 下已有 `zbus = "5.19"`）。**当前恒返回
/// `Unavailable`**：KDE/Wayland 的动作因此显示 `SystemManaged`，不会假装已注册。
pub fn portal_register(_app: &AppHandle, plan: &[PortalShortcut]) -> PortalRegistration {
    tracing::debug!(
        "hotkeys: TODO(P7-Wayland-portal) 未发起 portal 调用（计划提交 {} 条）",
        plan.len()
    );
    PortalRegistration::Unavailable {
        reason: "TODO(P7-Wayland-portal)：尚未实现 CreateSession → BindShortcuts → Activated 流程".into(),
    }
}

const SETTING_KEY: &str = "hotkeys_v1";

pub struct Hotkeys {
    /// 动作 id → 绑定列表（规范小写写法，如 "cmd+shift+p"）
    bindings: Mutex<HashMap<String, Vec<String>>>,
    /// 反查表：Shortcut 的 Display → 动作 id（handler 只拿得到 Shortcut；
    /// 两侧都经同一类型序列化当键，格式不依赖手写约定）
    lookup: Mutex<HashMap<String, String>>,
    /// 录制挂起前的应用菜单（hotkeys_record_end 时原样装回）
    saved_menu: Mutex<Option<Menu<Wry>>>,
    /// 录制中：全局热键与菜单已摘除
    suspended: Mutex<bool>,
    /// 动作 id → 当前注册状态（每次 [`apply`] 后刷新，前端在 `hotkeys_list` 里读）
    status: Mutex<HashMap<String, HotkeyStatus>>,
    /// 最近一次 portal 注册结果（`App` 后端下为 `None`；前端据此说明「尚未接 portal」）
    portal: Mutex<Option<PortalRegistration>>,
}

/// 取锁且**不 panic**：mutex 中毒（某个持锁线程 panic 过）时取回内部值继续用。
/// 快捷键状态不值得让整个应用崩掉 —— 而且托盘/窗口回调里 panic 会很难查。
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Hotkeys {
    fn snapshot(&self) -> HashMap<String, Vec<String>> {
        lock(&self.bindings).clone()
    }

    /// 状态快照（`hotkeys_list` 与其它 Rust 模块读这里）。
    pub fn status_snapshot(&self) -> HashMap<String, HotkeyStatus> {
        lock(&self.status).clone()
    }

    /// 最近一次 portal 注册结果（`App` 后端下恒 `None`）。
    pub fn portal_snapshot(&self) -> Option<PortalRegistration> {
        lock(&self.portal).clone()
    }
}

/// 绑定是否是「⌘+单键」：macOS 走菜单快捷键，其余走全局热键。
fn is_mac_menu_accel(accel: &str) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    let parts: Vec<&str> = accel.split('+').map(str::trim).collect();
    parts.len() == 2 && parts[0] == "cmd" && parts[1].len() == 1
}

/// "cmd+shift+s" → 菜单加速键写法 "Cmd+Shift+S"（muda 的 Accelerator 解析用）。
fn menu_accel(accel: &str) -> String {
    accel
        .split('+')
        .map(|p| match p {
            "cmd" => "Cmd".into(),
            "ctrl" => "Ctrl".into(),
            "alt" => "Alt".into(),
            "shift" => "Shift".into(),
            k if k.len() == 1 => k.to_uppercase(),
            // f1 / space / up 这类命名键：首字母大写即可（F1 / Space / Up）
            k => {
                let mut c = k.chars();
                match c.next() {
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// 菜单项文案：⌘M/⌘H 用 macOS 惯例说法，其余用动作名。
fn menu_label(action: &str, accel: &str) -> &'static str {
    match accel {
        "cmd+m" => "最小化主窗口",
        "cmd+h" => "隐藏主窗口",
        _ => action_label(action),
    }
}

fn action_label(action: &str) -> &'static str {
    ACTIONS
        .iter()
        .find(|(id, _)| *id == action)
        .map(|(_, label)| *label)
        .unwrap_or("快捷键")
}

/// 校验一条绑定：至少一个修饰键（或独立 F 键），能被 Shortcut 解析。
pub fn validate_accel(accel: &str) -> Result<(), String> {
    let parts: Vec<&str> = accel.split('+').map(str::trim).collect();
    let (mods, key): (Vec<&str>, &str) = match parts.split_last() {
        Some((k, m)) => (m.to_vec(), *k),
        None => return Err("空快捷键".into()),
    };
    if key.is_empty() {
        return Err("缺少按键".into());
    }
    let has_mod = ["cmd", "ctrl", "alt", "shift"].iter().any(|m| mods.contains(m));
    let is_fn = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u32>().is_ok();
    if !has_mod && !is_fn {
        return Err("至少需要一个修饰键（⌘/Ctrl/Alt/Shift）或使用 F 键".into());
    }
    accel
        .parse::<Shortcut>()
        .map(|_| ())
        .map_err(|e| format!("无法识别的快捷键 {accel}: {e}"))
}

/// 启动时读库 + 注册。DB 未就绪的键走默认值。
pub fn init(app: &AppHandle) -> Result<(), String> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    let stored = app
        .try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| crate::db::get_setting(&c, SETTING_KEY))
        });
    if let Some(raw) = stored {
        if let Ok(parsed) = serde_json::from_str::<HashMap<String, Vec<String>>>(&raw) {
            for (k, v) in parsed {
                // 空列表是合法状态（用户清空了该动作的绑定），不能因为 is_empty 被丢掉
                let ok = ACTIONS.iter().any(|(id, _)| *id == k)
                    && v.iter().all(|a| validate_accel(a).is_ok());
                if ok {
                    map.insert(k, v);
                }
            }
        }
    }
    for (id, _) in ACTIONS {
        map.entry((*id).to_string())
            .or_insert_with(|| default_bindings(id));
    }
    // 状态先全部标成「尚未注册」：apply 里任何早退（例如 macOS 菜单构建失败）
    // 都会留下诚实的状态，而不是空表让前端以为一切正常
    let mut pending: HashMap<String, HotkeyStatus> = HashMap::new();
    for (id, _) in ACTIONS {
        pending.insert(
            (*id).to_string(),
            HotkeyStatus::Unavailable {
                reason: "尚未注册（初始化未走完）".into(),
            },
        );
    }
    app.manage(Hotkeys {
        bindings: Mutex::new(map),
        lookup: Mutex::new(HashMap::new()),
        saved_menu: Mutex::new(None),
        suspended: Mutex::new(false),
        status: Mutex::new(pending),
        portal: Mutex::new(None),
    });
    // 注册失败不能掀掉整个 setup：绑定已入库，下次启动或改绑时会再试。
    // 但**不许静默**：状态已写进 state（前端读 `hotkeys_list` 就能看到哪条失败），
    // 日志里带上原因与状态摘要，真机取证时 grep `hotkeys`。
    if let Err(e) = apply(app) {
        tracing::warn!("hotkeys apply at init failed (bindings kept, 见 hotkeys_list 状态): {e}");
    }
    Ok(())
}

/// 某动作在 `App` 后端下的状态。空绑定是合法状态，单独说明。
fn app_status(accels: &[String], failure: Option<&String>) -> HotkeyStatus {
    if accels.is_empty() {
        return HotkeyStatus::Unavailable {
            reason: NO_BINDING_NOTE.into(),
        };
    }
    match failure {
        Some(reason) => HotkeyStatus::Failed {
            reason: reason.clone(),
        },
        None => HotkeyStatus::Bound,
    }
}

/// 某动作在不抢键的 Wayland 后端下的状态（portal 接缝见 [`portal_register`]）。
fn wayland_status(
    backend: ShortcutBackend,
    accels: &[String],
    registration: &PortalRegistration,
) -> HotkeyStatus {
    if accels.is_empty() {
        return HotkeyStatus::Unavailable {
            reason: NO_BINDING_NOTE.into(),
        };
    }
    match (backend, registration) {
        (ShortcutBackend::KdeSystem, PortalRegistration::Registered { .. }) => HotkeyStatus::Bound,
        (ShortcutBackend::KdeSystem, PortalRegistration::Unavailable { .. }) => {
            HotkeyStatus::SystemManaged {
                note: KDE_SYSTEM_MANAGED_NOTE.into(),
            }
        }
        _ => HotkeyStatus::Unavailable {
            reason: WAYLAND_UNSUPPORTED_REASON.into(),
        },
    }
}

/// （重新）注册全部绑定，并刷新每个动作的 [`HotkeyStatus`]。
///
/// * [`ShortcutBackend::App`]：装菜单（macOS）+ 逐条注册全局热键。单条失败**不再**
///   中断整批（继续注册其余），最后把失败原因合成一个 `Err` 上报 —— 「一个键被占用
///   就全部失效」变成「其余仍可用 + 前端看得到哪条失败」。`hotkeys_set` 的非 force
///   路径仍靠这个 `Err` 回滚，语义不变。
/// * [`ShortcutBackend::KdeSystem`] / [`ShortcutBackend::Unsupported`]：**一次 grab
///   都不做**，只写状态（原因见文件头 KDE/Wayland 段）。
pub fn apply(app: &AppHandle) -> Result<(), String> {
    let state = app
        .try_state::<Hotkeys>()
        .ok_or("快捷键未初始化（Hotkeys state 缺失）")?;
    let backend = shortcut_backend();
    let bindings = state.snapshot();
    *lock(&state.suspended) = false;

    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    // 快捷键 → 动作 的反查表（handler 只拿得到 Shortcut，用它的 Display 当键，
    // 两侧都经同一类型序列化，格式不依赖手写约定）
    let mut lookup: HashMap<String, String> = HashMap::new();
    let mut fresh: HashMap<String, HotkeyStatus> = HashMap::new();

    #[cfg(target_os = "macos")]
    {
        match build_app_menu(app, &bindings).and_then(|menu| {
            *lock(&state.saved_menu) = Some(menu.clone());
            app.set_menu(menu)
        }) {
            Ok(_) => {}
            Err(e) => {
                // 菜单挂了就不存在「应用内快捷键」了，如实标 Failed 再上报
                let reason = format!("应用菜单构建失败: {e}");
                for (id, _) in ACTIONS {
                    fresh.insert(
                        (*id).to_string(),
                        HotkeyStatus::Failed {
                            reason: reason.clone(),
                        },
                    );
                }
                *lock(&state.status) = fresh;
                return Err(reason);
            }
        }
    }

    match backend {
        ShortcutBackend::KdeSystem | ShortcutBackend::Unsupported => {
            // 一次 X11/XWayland grab 都不做。KDE 走 portal 接缝（当前恒 Unavailable）。
            let registration = match backend {
                ShortcutBackend::KdeSystem => {
                    let plan = portal_bind_plan(&bindings);
                    portal_register(app, &plan)
                }
                _ => PortalRegistration::Unavailable {
                    reason: WAYLAND_UNSUPPORTED_REASON.into(),
                },
            };
            tracing::info!(
                "hotkeys: backend={} 不抢键；portal={}",
                backend.as_str(),
                registration_state(&registration)
            );
            *lock(&state.portal) = Some(registration.clone());
            for (action, accels) in &bindings {
                fresh.insert(
                    action.clone(),
                    wayland_status(backend, accels, &registration),
                );
            }
        }
        ShortcutBackend::App => {
            let mut failures: HashMap<String, String> = HashMap::new();
            for (action, accels) in &bindings {
                for accel in accels {
                    if is_mac_menu_accel(accel) {
                        continue; // 菜单快捷键已在上面处理
                    }
                    match gs.register(accel.as_str()) {
                        Ok(_) => {
                            if let Ok(sk) = accel.parse::<Shortcut>() {
                                lookup.insert(sk.to_string(), action.clone());
                            }
                        }
                        Err(e) => {
                            // 记录原因继续注册其余组合（旧行为是立刻收摊 + 全组失效）
                            let reason =
                                format!("快捷键 {accel} 注册失败（可能被系统或其它应用占用）: {e}");
                            tracing::warn!("hotkeys: {reason}");
                            failures.entry(action.clone()).or_insert(reason);
                        }
                    }
                }
            }
            for (action, accels) in &bindings {
                fresh.insert(action.clone(), app_status(accels, failures.get(action)));
            }
            *lock(&state.lookup) = lookup;
            *lock(&state.status) = fresh.clone();
            tracing::debug!("hotkeys: backend=app {}", status_digest(&fresh));
            if !failures.is_empty() {
                let mut msgs: Vec<&String> = failures.values().collect();
                msgs.sort();
                return Err(msgs
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("；"));
            }
            return Ok(());
        }
    }

    // 不抢键的分支：反查表留空（没有全局热键 handler 会命中），状态落库
    *lock(&state.lookup) = lookup;
    *lock(&state.status) = fresh.clone();
    tracing::debug!(
        "hotkeys: backend={} {}",
        backend.as_str(),
        status_digest(&fresh)
    );
    Ok(())
}

fn registration_state(registration: &PortalRegistration) -> &'static str {
    match registration {
        PortalRegistration::Registered { .. } => "registered",
        PortalRegistration::Unavailable { .. } => "unavailable",
    }
}

/// 全局热键 handler 入口：Shortcut → 动作 id → 分发。
pub fn dispatch_by_shortcut(app: &AppHandle, shortcut: &Shortcut) {
    let key = shortcut.to_string();
    let action = app
        .try_state::<Hotkeys>()
        .and_then(|s| s.lookup.lock().unwrap().get(&key).cloned());
    if let Some(action) = action {
        dispatch(app, &action);
    } else {
        tracing::warn!("global shortcut without action: {key}");
    }
}

/// 录制开始：摘菜单 + 注销全局热键，让 ⌘+任意键都能到达录制框。
#[tauri::command]
pub fn hotkeys_record_begin(app: AppHandle) -> Result<(), String> {
    let state = app.state::<Hotkeys>();
    *lock(&state.suspended) = true;
    let _ = app.global_shortcut().unregister_all();
    // 录制期间热键真的被摘掉了：状态如实标成「临时摘除」，别让前端继续显示 bound。
    // 录制结束（成功或取消）都走 hotkeys_record_end → apply() 重算状态。
    for s in lock(&state.status).values_mut() {
        *s = HotkeyStatus::Unavailable {
            reason: RECORDING_NOTE.into(),
        };
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(Some(menu)) = app.remove_menu() {
            *lock(&state.saved_menu) = Some(menu);
        }
    }
    Ok(())
}

/// 录制结束（成功或取消都要调）：恢复菜单与全部热键。
#[tauri::command]
pub fn hotkeys_record_end(app: AppHandle) -> Result<(), String> {
    apply(&app)
}

/// 当前绑定 + 默认值（前端展示/恢复默认用）。
#[tauri::command]
pub fn hotkeys_list(app: AppHandle) -> Result<serde_json::Value, String> {
    let state = app.try_state::<Hotkeys>().ok_or("快捷键未初始化")?;
    let bindings = state.snapshot();
    let status = state.status_snapshot();
    let backend = shortcut_backend();
    let kde = crate::kde::env();
    // KDE 会话才追加 KWin 保留区（非 KDE 不该冒出 super+* 那张表）
    let kde_reserved: &[&str] = if kde.is_kde { RESERVED_KDE } else { &[] };
    let items: Vec<serde_json::Value> = ACTIONS
        .iter()
        .map(|(id, label)| {
            // 状态是本模块的主产出：前端「按了没反应」时要能直接显示原因
            let status_json = status
                .get(*id)
                .map_or_else(|| serde_json::json!({ "state": "unknown" }), HotkeyStatus::to_json);
            serde_json::json!({
                "id": id,
                "label": label,
                "bindings": bindings.get(*id).cloned().unwrap_or_default(),
                "defaults": default_bindings(id),
                "status": status_json,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "items": items,
        // `reserved` 与既有前端契约逐字一致；KDE 追加项走下面两个新字段
        "reserved": RESERVED,
        "reserved_kde": kde_reserved,
        "reserved_with_kde": reserved_for(current_os_class(), kde),
        "backend": {
            "kind": backend.as_str(),
            "note": backend.note(),
            "kde": kde.is_kde,
            "plasma_major": kde.plasma_major,
            "session": kde.session.as_str(),
        },
        "portal": state.portal_snapshot().map_or_else(
            || serde_json::json!({
                "implemented": false,
                "state": "unavailable",
                "detail": "当前后端不需要 portal（或尚未初始化）",
            }),
            |r| r.to_json(),
        ),
    }))
}

/// 设置某动作的绑定（录制结果落地）。整组替换；空列表 = 该动作暂不绑定。
/// `force` = 覆盖模式：注册失败（可能被其它应用占用）也照存不误，
/// 用于「被占用了…可以取消或者覆盖」里的覆盖。
#[tauri::command]
pub fn hotkeys_set(
    app: AppHandle,
    action: String,
    bindings: Vec<String>,
    force: Option<bool>,
) -> Result<(), String> {
    if !ACTIONS.iter().any(|(id, _)| *id == action) {
        return Err(format!("未知的快捷键动作: {action}"));
    }
    for a in &bindings {
        validate_accel(a)?;
    }
    // KDE 下只提醒、不阻止：用户显式录制的组合就是他的选择（KWin 占用时注册会失败，
    // 失败原因由 `apply` 落成 HotkeyStatus::Failed，前端看得到）
    if cfg!(target_os = "linux") && crate::kde::env().is_kde {
        for a in &bindings {
            if let Some(why) = kde_default_conflict(a) {
                tracing::warn!(
                    "hotkeys: {action} 绑定 {a} 落在 KWin 保留区（{why}），可能被 KWin 截走"
                );
            }
        }
    }
    let state = app.state::<Hotkeys>();
    // 互斥：同一组合不能挂两个动作
    {
        let map = state.bindings.lock().unwrap();
        for (other, accels) in map.iter() {
            if *other == action {
                continue;
            }
            if let Some(hit) = accels.iter().find(|a| bindings.contains(a)) {
                return Err(format!("快捷键 {hit} 已被「{}」占用", action_label(other)));
            }
        }
    }
    let old = {
        let mut map = state.bindings.lock().unwrap();
        map.insert(action.clone(), bindings.clone())
    };
    // 注册失败回滚旧绑定，别把用户的原快捷键弄丢；force = 覆盖：照存
    if let Err(e) = apply(&app) {
        if force != Some(true) {
            let mut map = state.bindings.lock().unwrap();
            match old {
                Some(prev) => {
                    map.insert(action.clone(), prev);
                }
                None => {
                    map.remove(&action);
                }
            }
            let _ = apply(&app);
            return Err(e);
        }
        tracing::warn!("hotkeys_set force-keep {action} despite register failure: {e}");
    }
    save(&app);
    Ok(())
}

/// 恢复某动作（或全部）的默认绑定。
#[tauri::command]
pub fn hotkeys_reset(app: AppHandle, action: Option<String>) -> Result<(), String> {
    let state = app.state::<Hotkeys>();
    {
        let mut map = state.bindings.lock().unwrap();
        match action.as_deref() {
            Some(id) => {
                if !ACTIONS.iter().any(|(aid, _)| *aid == id) {
                    return Err(format!("未知的快捷键动作: {id}"));
                }
                map.insert(id.to_string(), default_bindings(id));
            }
            None => {
                for (id, _) in ACTIONS {
                    map.insert((*id).to_string(), default_bindings(id));
                }
            }
        }
    }
    apply(&app)?;
    save(&app);
    Ok(())
}

fn save(app: &AppHandle) {
    let Some(state) = app.try_state::<Hotkeys>() else {
        return;
    };
    let bindings = state.snapshot();
    if let Some(db) = app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>() {
        if let Ok(conn) = db.lock() {
            if let Ok(json) = serde_json::to_string(&bindings) {
                let _ = crate::db::set_setting(&conn, SETTING_KEY, &json);
            }
        }
    }
}

/// 应用菜单（macOS）：app 子菜单 + 快捷键占位 + Edit（同 lib.rs 的裁剪策略）。
/// ⌘+单键绑定都落成菜单项 —— 这是 macOS 上 ⌘M/⌘H 这类组合唯一不劫持
/// 其它 App 的注册方式。
#[cfg(target_os = "macos")]
fn build_app_menu(
    app: &AppHandle,
    bindings: &HashMap<String, Vec<String>>,
) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let items = app_menu.items()?;
        // Menu::default 的 app 子菜单布局固定：
        // [About, sep, Services, sep, Hide, HideOthers, sep, Quit]
        if let Some(hide) = items.get(4) {
            let _ = app_menu.remove(hide);
        }
        let mut idx = 4usize;
        for (action, _) in ACTIONS {
            for accel in bindings.get(*action).into_iter().flatten() {
                if !is_mac_menu_accel(accel) {
                    continue;
                }
                let item = MenuItem::with_id(
                    app,
                    format!("hotkey::{action}::{accel}"),
                    crate::i18n::tr(menu_label(action, accel)),
                    true,
                    Some(menu_accel(accel).as_str()),
                )?;
                app_menu.insert(&item, idx)?;
                idx += 1;
            }
        }
    }
    // 同 lib.rs 的裁剪：只留 app 子菜单 + Edit（WKWebView 输入框要 ⌘C/⌘V/⌘A）
    let kinds = menu.items()?;
    for (i, kind) in kinds.into_iter().enumerate() {
        if i == 0 {
            continue;
        }
        let _ = menu.remove(&kind);
    }
    let undo = PredefinedMenuItem::undo(app, None)?;
    let redo = PredefinedMenuItem::redo(app, None)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let cut = PredefinedMenuItem::cut(app, None)?;
    let copy = PredefinedMenuItem::copy(app, None)?;
    let paste = PredefinedMenuItem::paste(app, None)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let select_all = PredefinedMenuItem::select_all(app, None)?;
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[&undo, &redo, &sep1, &cut, &copy, &paste, &sep2, &select_all],
    )?;
    menu.insert(&edit, 1usize)?;
    Ok(menu)
}

/// 菜单项点击（⌘+单键绑定）→ 分发。返回是否认领了这条事件。
#[cfg(target_os = "macos")]
pub fn handle_menu_event(app: &AppHandle, id: &str) -> bool {
    // id 形如 hotkey::<action>::<accel>
    let Some(rest) = id.strip_prefix("hotkey::") else {
        return false;
    };
    let Some(action) = rest.split("::").next() else {
        return false;
    };
    dispatch(app, action);
    true
}

#[cfg(not(target_os = "macos"))]
pub fn handle_menu_event(_app: &AppHandle, _id: &str) -> bool {
    false // 非 mac 无应用菜单
}

/// 语言切换后重装菜单（菜单文案要跟着变）。非 mac 空实现。
pub fn retranslate(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let Some(state) = app.try_state::<Hotkeys>() else {
            return;
        };
        if *state.suspended.lock().unwrap() {
            return; // 录制中菜单是摘掉的，结束时会重建
        }
        let bindings = state.snapshot();
        if let Ok(menu) = build_app_menu(app, &bindings) {
            *state.saved_menu.lock().unwrap() = Some(menu.clone());
            let _ = app.set_menu(menu);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
    }
}

// ---- 动作分发 ----

pub fn dispatch(app: &AppHandle, action: &str) {
    match action {
        "toggle_main_window" => {
            // 「在眼前」= 可见**且未最小化**：Win32 的 IsWindowVisible 与 GTK 的
            // widget visible 对最小化窗口都返回 true，只看 is_visible 的话在
            // Windows/Linux 上按热键只会反复最小化、永远唤不回来（mac 之所以看着
            // 正常，是最小化被观察者立刻销毁了窗口，下一次查到的是 None）。
            let win = app.get_webview_window("main");
            let shown = win
                .as_ref()
                .map(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false))
                .unwrap_or(false);
            if shown {
                // 与窗口关闭同语义：mac 上最小化随即释放窗口（内存立刻归还）；
                // Windows/Linux 没有这条观察者，最小化只是留在任务栏
                if let Some(w) = win {
                    let _ = w.minimize();
                }
            } else {
                // 最小化/已释放/不存在都走这里：ensure_main_window 内部会
                // unminimize + show + set_focus，必要时按原配置重建
                crate::main_window::ensure_main_window(app);
            }
        }
        "toggle_props_window" => {
            let open: Vec<_> = app
                .webview_windows()
                .into_iter()
                .filter(|(label, _)| label.starts_with("props-"))
                .collect();
            if !open.is_empty() {
                for (_, w) in open {
                    let _ = w.close();
                }
                return;
            }
            // 与托盘「壁纸设置」同一条目标解析：**当前屏幕**（光标所在屏）当前挂的
            // 壁纸 —— 不是 active_items()[0]（多屏时经常是另一块屏的），面板也摆在
            // 那块屏上（见 wallpaper::current_screen / props_window::open_at）。
            match crate::wallpaper::current_screen(app) {
                Some(cs) => match cs.item_id {
                    Some(item) => {
                        tracing::info!(
                            "hotkey open props: 当前屏幕「{}」（{}）→ 壁纸 {item}",
                            cs.name,
                            cs.id
                        );
                        if let Err(e) = crate::props_window::open_at(app, &item, Some(cs.frame)) {
                            tracing::warn!("hotkey open props: {e}");
                        }
                    }
                    None => {
                        tracing::info!(
                            "hotkey open props: 当前屏幕「{}」（{}）没有壁纸，唤起主窗口",
                            cs.name,
                            cs.id
                        );
                        crate::main_window::ensure_main_window(app);
                    }
                },
                None => crate::main_window::ensure_main_window(app),
            }
        }
        "toggle_pause" => {
            let paused = app
                .try_state::<crate::wallpaper::WallpaperEngineState>()
                .map(|s| *s.paused.lock().unwrap())
                .unwrap_or(false);
            let r = if paused {
                crate::wallpaper::resume_all(app.clone())
            } else {
                crate::wallpaper::pause_all(app.clone())
            };
            if let Err(e) = r {
                tracing::warn!("hotkey toggle pause: {e}");
            }
        }
        "toggle_auto_pause" => {
            let cur = crate::tray_read_setting(app, "wallpaper_auto_pause", "false");
            let next = !(cur == "true" || cur == "1");
            if let Some(db) = app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>() {
                if let Ok(conn) = db.lock() {
                    let _ = crate::db::set_setting(
                        &conn,
                        "wallpaper_auto_pause",
                        if next { "true" } else { "false" },
                    );
                }
            }
            crate::notify_setting_changed(
                app,
                "wallpaper_auto_pause",
                if next { "true" } else { "false" },
            );
            // 同托盘：关闭时若正挂在自动暂停上，立即恢复播放
            if !next {
                if let Some(st) = app.try_state::<crate::wallpaper::WallpaperEngineState>() {
                    if !st.auto_paused.lock().unwrap().is_empty() {
                        let _ = crate::wallpaper::resume_all(app.clone());
                    }
                }
            }
        }
        "toggle_rotation" => {
            let paused = app
                .try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
                .and_then(|db| {
                    db.lock()
                        .ok()
                        .and_then(|c| crate::db::get_setting(&c, "playlist_rotation_paused"))
                })
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false);
            if let Err(e) = crate::wallpaper::rotation_set(app.clone(), !paused) {
                tracing::warn!("hotkey toggle rotation: {e}");
            }
        }
        "next_wallpaper" | "prev_wallpaper" => {
            // 有「可切换的轮播上下文」就有效：暂停只停定时自动切换，手动上一张/下一张
            // 照旧要走；上下文被移除后（停止轮播 / 手动设了单张壁纸）才真的没得切
            let st = crate::wallpaper::playlist_status(app.clone())
                .unwrap_or_else(|_| serde_json::json!({ "active": false, "paused": false }));
            let switchable = st
                .get("switchable")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !switchable {
                tracing::debug!("hotkey {action}: 没有可切换的轮播上下文，忽略");
                return;
            }
            let r = if action == "next_wallpaper" {
                crate::wallpaper::next(app.clone(), None)
            } else {
                crate::wallpaper::prev(app.clone(), None)
            };
            if let Err(e) = r {
                tracing::warn!("hotkey {action}: {e}");
            }
        }
        _ => tracing::warn!("unknown hotkey action: {action}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_defaults() {
        for (id, _) in ACTIONS {
            assert!(!default_bindings(id).is_empty(), "{id} 缺默认绑定");
        }
    }

    #[test]
    fn main_window_has_global_binding() {
        // ⌘M/⌘H 是菜单键位（仅应用内）；主窗口隐藏后要能从任意应用唤回，
        // 必须至少有一枚**全局**绑定
        let globals: Vec<String> = default_bindings("toggle_main_window")
            .into_iter()
            .filter(|a| !is_mac_menu_accel(a))
            .collect();
        assert!(!globals.is_empty(), "主窗口显示/隐藏 缺全局绑定");
    }

    // ⌘+单键走菜单级（不劫持其它 App）—— 这条规则只在 macOS 上存在：is_mac_menu_accel
    // 在非 macOS 恒返回 false（Cmd 不是那两个平台的修饰键），所以正向断言必须按平台
    // gate，否则 Linux CI 每次都假红（docs/pr-rules.md §6 脚注 2 说的就是这条）。
    #[cfg(target_os = "macos")]
    #[test]
    fn menu_accel_only_for_bare_cmd_key() {
        assert!(is_mac_menu_accel("cmd+m"));
        assert!(is_mac_menu_accel("cmd+h"));
        assert!(is_mac_menu_accel("cmd+p")); // ⌘+单键一律菜单级（不劫持其它 App）
        assert!(!is_mac_menu_accel("cmd+shift+m")); // 多修饰 → 全局
        assert!(!is_mac_menu_accel("ctrl+m")); // 非 ⌘ 修饰 → 全局
    }

    /// 非 macOS 上恒不是菜单加速键：那里的 `cmd` 绑定走全局快捷键，不会被当菜单拦截。
    /// 这不是"没功能"，是平台契约 —— 钉住它，将来谁把 `is_mac_menu_accel` 里的 cfg! 删了会立刻红。
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn menu_accel_always_false_off_macos() {
        assert!(!is_mac_menu_accel("cmd+m"));
        assert!(!is_mac_menu_accel("ctrl+m"));
    }

    #[test]
    fn reserved_entries_parse_as_shortcuts() {
        // 保留组合只喂给前端做「系统/菜单保留组合」提示比对，条目写错（例如
        // Linux 那组的 arrow 键名）不会报错、只会静默不命中 —— 两个平台的清单
        // 都解析一遍，把错字钉住
        for accel in RESERVED_MACOS.iter().chain(RESERVED_OTHER) {
            assert!(
                accel.parse::<Shortcut>().is_ok(),
                "保留组合无法解析: {accel}"
            );
        }
    }

    #[test]
    fn validate_rejects_bare_key_and_garbage() {
        assert!(validate_accel("cmd+shift+p").is_ok());
        assert!(validate_accel("f5").is_ok()); // F 键可以裸按
        assert!(validate_accel("p").is_err()); // 普通键必须带修饰
        assert!(validate_accel("cmd+").is_err());
        assert!(validate_accel("cmd+shift+notakey").is_err());
    }

    #[test]
    fn menu_accel_formats_for_muda() {
        assert_eq!(menu_accel("cmd+shift+s"), "Cmd+Shift+S");
        assert_eq!(menu_accel("cmd+m"), "Cmd+M");
    }

    // ---------- T-P7H：KDE/Wayland 后端判定 + 安全默认键位 ----------

    /// 用环境快照造 `KdeEnv` —— 复用 `kde.rs` 的判定链，测试不复刻规则。
    fn kde_env(de: &str, session: &str) -> KdeEnv {
        let snap: crate::kde::EnvSnapshot = [
            ("XDG_CURRENT_DESKTOP", de),
            ("XDG_SESSION_TYPE", session),
            ("KDE_SESSION_VERSION", "6"),
        ]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
        crate::kde::detect_with(&snap, true)
    }

    fn accels(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn backend_matrix_by_os_and_session() {
        let kde_x11 = kde_env("KDE", "x11");
        let kde_wl = kde_env("KDE:Plasma", "wayland");
        let gnome_wl = kde_env("GNOME", "wayland");
        let gnome_x11 = kde_env("GNOME", "x11");
        // macOS / Windows 一律自注册（与桌面无关）
        for os in [OsClass::Macos, OsClass::Windows] {
            for kde in [&kde_x11, &kde_wl, &gnome_wl] {
                assert_eq!(shortcut_backend_with(os, kde), ShortcutBackend::App);
            }
        }
        // X11（含 KDE/X11）：XGrabKey 全局有效，真注册、失败如实上报
        assert_eq!(
            shortcut_backend_with(OsClass::Linux, &kde_x11),
            ShortcutBackend::App
        );
        assert_eq!(
            shortcut_backend_with(OsClass::Linux, &gnome_x11),
            ShortcutBackend::App
        );
        // Wayland：KDE 交给桌面（KWin 内嵌 KGlobalAccelD），其它桌面没有可用后端
        assert_eq!(
            shortcut_backend_with(OsClass::Linux, &kde_wl),
            ShortcutBackend::KdeSystem
        );
        assert_eq!(
            shortcut_backend_with(OsClass::Linux, &gnome_wl),
            ShortcutBackend::Unsupported
        );
        // 「不抢键」的两个后端必须带文案；App 没有
        assert!(ShortcutBackend::KdeSystem.note().is_some());
        assert!(ShortcutBackend::Unsupported.note().is_some());
        assert!(ShortcutBackend::App.note().is_none());
    }

    /// KDE 默认键位的硬约束：不许落在 KWin 保留区，且必须是能真正注册的写法。
    #[test]
    fn kde_defaults_stay_out_of_kwin_reserved_area() {
        for de in ["KDE", "KDE:Plasma"] {
            let kde = kde_env(de, "x11");
            for (id, _) in ACTIONS {
                for a in default_bindings_with(id, OsClass::Linux, &kde) {
                    assert!(
                        kde_default_conflict(&a).is_none(),
                        "{id} 的 KDE 默认键位 {a} 落在 KWin 保留区"
                    );
                    assert!(a.parse::<Shortcut>().is_ok(), "默认键位无法解析: {a}");
                }
            }
        }
        // 非 KDE 的 Linux 默认值同表校验：KDE 用户切桌面时也不能踩雷
        let gnome = kde_env("GNOME", "x11");
        for (id, _) in ACTIONS {
            for a in default_bindings_with(id, OsClass::Linux, &gnome) {
                assert!(
                    kde_default_conflict(&a).is_none(),
                    "{id} 的通用 Linux 默认键位 {a} 撞 KWin 保留区"
                );
            }
        }
    }

    #[test]
    fn kde_conflict_rules_are_semantic_not_just_table_lookup() {
        // 表里的逐条命中
        for a in KDE_BANNED_DEFAULTS {
            assert!(kde_default_conflict(a).is_some(), "{a} 应判为冲突");
        }
        // 表里没有、但语义上同样冲突的写法（大小写/写法变体都要挡住）
        for a in [
            "super+9",
            "Meta+L",
            "meta+shift+left",
            "LOGO+x",
            "ctrl+alt+f9",
            "alt+Left",
            "ctrl+alt+arrowright",
        ] {
            assert!(kde_default_conflict(a).is_some(), "{a} 应判为冲突");
        }
        // 可用区：Ctrl+Shift+字母 与不涉保留区的组合
        for a in ["ctrl+shift+m", "ctrl+shift+p", "ctrl+shift+f12", "alt+f4", "f5"] {
            assert!(kde_default_conflict(a).is_none(), "{a} 不该判为冲突");
        }
        // 坏写法不 panic（调用方另有 validate_accel 把关）
        assert!(kde_default_conflict("").is_none());
        assert!(kde_default_conflict("+").is_none());
    }

    #[test]
    fn reserved_lists_parse_and_kde_entries_are_session_scoped() {
        for a in RESERVED_KDE {
            assert!(a.parse::<Shortcut>().is_ok(), "KDE 保留组合无法解析: {a}");
        }
        let kde = kde_env("KDE", "wayland");
        let gnome = kde_env("GNOME", "wayland");
        let kde_list = reserved_for(OsClass::Linux, &kde);
        let gnome_list = reserved_for(OsClass::Linux, &gnome);
        assert!(kde_list.contains(&"super+left"));
        assert!(kde_list.contains(&"ctrl+alt+f1"));
        assert!(
            !gnome_list.contains(&"super+left"),
            "非 KDE 会话不该冒出 KWin 的 super+* 表"
        );
        assert_eq!(reserved_for(OsClass::Macos, &kde), RESERVED_MACOS.to_vec());
        assert_eq!(reserved_for(OsClass::Windows, &kde), RESERVED_OTHER.to_vec());
    }

    // ---------- T-P7H：portal 接缝（TODO(P7-Wayland-portal)） ----------

    #[test]
    fn portal_trigger_uses_kde_backend_syntax() {
        // 格式依据 xdg-desktop-portal-kde 的 xdgshortcut.cpp::parse（LOGO = Super/Meta）
        assert_eq!(
            portal_trigger("ctrl+shift+m").as_deref(),
            Some("CTRL+SHIFT+m")
        );
        assert_eq!(
            portal_trigger("cmd+shift+n").as_deref(),
            Some("LOGO+SHIFT+n")
        );
        assert_eq!(portal_trigger("super+left").as_deref(), Some("LOGO+Left"));
        assert_eq!(portal_trigger("ctrl+alt+f5").as_deref(), Some("CTRL+ALT+F5"));
        // 重复修饰键只出现一次
        assert_eq!(portal_trigger("ctrl+ctrl+a").as_deref(), Some("CTRL+a"));
        // 映射不了的写法：没有修饰键 / 未知键名 / 未知修饰键 / 空串
        assert_eq!(portal_trigger("f5"), None);
        assert_eq!(portal_trigger("ctrl+shift+notakey"), None);
        assert_eq!(portal_trigger("hyper+a"), None);
        assert_eq!(portal_trigger(""), None);
    }

    #[test]
    fn portal_bind_plan_covers_every_action() {
        let mut bindings: HashMap<String, Vec<String>> = HashMap::new();
        for (id, _) in ACTIONS {
            bindings.insert((*id).to_string(), kde_bindings(id));
        }
        let plan = portal_bind_plan(&bindings);
        assert_eq!(plan.len(), ACTIONS.len(), "每个动作都要出现在 portal 清单里");
        for (id, label) in ACTIONS {
            let item = plan.iter().find(|s| s.id == *id).expect("缺动作");
            assert_eq!(item.description, *label);
        }
        assert!(
            plan.iter().all(|s| s.preferred_trigger.is_some()),
            "KDE 默认键位都应能映射成 portal 的首选键位: {plan:?}"
        );
        // 空绑定：条目仍在（保留 id 与描述），但没有首选键位
        let mut empty: HashMap<String, Vec<String>> = HashMap::new();
        empty.insert("toggle_pause".to_string(), Vec::new());
        let p2 = portal_bind_plan(&empty);
        assert_eq!(p2.len(), ACTIONS.len());
        assert!(p2.iter().all(|s| s.preferred_trigger.is_none()));
    }

    #[test]
    fn portal_registration_json_shape_is_stable() {
        let unavailable = PortalRegistration::Unavailable {
            reason: "TODO(P7-Wayland-portal)".into(),
        };
        let j = unavailable.to_json();
        assert_eq!(j["implemented"], serde_json::json!(false));
        assert_eq!(j["state"], "unavailable");
        let registered = PortalRegistration::Registered {
            session_handle: "/org/freedesktop/portal/desktop/session/1_1/wem".into(),
            portal_version: 1,
        };
        let j = registered.to_json();
        assert_eq!(j["implemented"], serde_json::json!(true));
        assert_eq!(j["state"], "registered");
        assert_eq!(j["portal_version"], serde_json::json!(1));
        assert_eq!(registration_state(&unavailable), "unavailable");
        assert_eq!(registration_state(&registered), "registered");
    }

    // ---------- T-P7H：状态可观测（本模块的主产出） ----------

    #[test]
    fn app_status_reflects_binding_and_failure() {
        let bound = accels(&["ctrl+shift+p"]);
        assert_eq!(app_status(&bound, None), HotkeyStatus::Bound);
        let fail = "快捷键 ctrl+shift+p 注册失败（可能被系统或其它应用占用）: boom".to_string();
        assert_eq!(
            app_status(&bound, Some(&fail)),
            HotkeyStatus::Failed {
                reason: fail.clone()
            }
        );
        assert_eq!(
            app_status(&[], None),
            HotkeyStatus::Unavailable {
                reason: NO_BINDING_NOTE.into()
            }
        );
    }

    /// 关键诚实性约束：portal 没接上时，KDE/Wayland **不许**报 `Bound`。
    #[test]
    fn wayland_status_never_claims_bound_without_portal() {
        let bound = accels(&["ctrl+shift+p"]);
        let not_impl = PortalRegistration::Unavailable {
            reason: WAYLAND_UNSUPPORTED_REASON.into(),
        };
        match wayland_status(ShortcutBackend::KdeSystem, &bound, &not_impl) {
            HotkeyStatus::SystemManaged { note } => assert_eq!(note, KDE_SYSTEM_MANAGED_NOTE),
            other => panic!("KDE/Wayland 应为 SystemManaged，实际 {other:?}"),
        }
        match wayland_status(ShortcutBackend::Unsupported, &bound, &not_impl) {
            HotkeyStatus::Unavailable { reason } => assert_eq!(reason, WAYLAND_UNSUPPORTED_REASON),
            other => panic!("非 KDE/Wayland 应为 Unavailable，实际 {other:?}"),
        }
        // portal 真接上后才允许 Bound（今天只有测试构造得出这个状态）
        let ok = PortalRegistration::Registered {
            session_handle: ":1.1".into(),
            portal_version: 1,
        };
        assert_eq!(
            wayland_status(ShortcutBackend::KdeSystem, &bound, &ok),
            HotkeyStatus::Bound
        );
        // 空绑定优先于后端：没键可注册就不该说「已注册」
        assert_eq!(
            wayland_status(ShortcutBackend::KdeSystem, &[], &ok),
            HotkeyStatus::Unavailable {
                reason: NO_BINDING_NOTE.into()
            }
        );
    }

    #[test]
    fn status_json_and_digest_are_frontend_ready() {
        assert_eq!(
            HotkeyStatus::Bound.to_json(),
            serde_json::json!({ "state": "bound" })
        );
        let j = HotkeyStatus::Failed {
            reason: "被占用".into(),
        }
        .to_json();
        assert_eq!(j["state"], "failed");
        assert_eq!(j["detail"], "被占用");
        assert_eq!(
            HotkeyStatus::SystemManaged { note: "n".into() }.as_str(),
            "system_managed"
        );

        let mut m: HashMap<String, HotkeyStatus> = HashMap::new();
        m.insert("a".into(), HotkeyStatus::Bound);
        m.insert("b".into(), HotkeyStatus::Bound);
        m.insert(
            "c".into(),
            HotkeyStatus::Failed {
                reason: "x".into(),
            },
        );
        let d = status_digest(&m);
        assert!(d.contains("3 个动作"), "digest 缺动作数: {d}");
        assert!(d.contains("bound=2"), "digest 缺 bound 计数: {d}");
        assert!(d.contains("failed=1"), "digest 缺 failed 计数: {d}");
    }

    /// KDE/Wayland 全链路（纯函数）冒烟：后端 → portal 接缝 → 每动作状态，
    /// 断言「不做 X11 抢键 + 状态不是 Bound」——防止将来有人为了「看起来能用」
    /// 把 Wayland 分支改回 App。
    #[test]
    fn kde_wayland_chain_smoke() {
        let kde = kde_env("KDE", "wayland");
        let backend = shortcut_backend_with(OsClass::Linux, &kde);
        assert_eq!(backend, ShortcutBackend::KdeSystem);

        let mut bindings: HashMap<String, Vec<String>> = HashMap::new();
        for (id, _) in ACTIONS {
            bindings.insert((*id).to_string(), default_bindings_with(id, OsClass::Linux, &kde));
        }
        let plan = portal_bind_plan(&bindings);
        let registration = portal_register_plan_stub(&plan);
        assert!(
            matches!(registration, PortalRegistration::Unavailable { .. }),
            "TODO(P7-Wayland-portal) 落地前必须如实报告不可用"
        );
        for (id, _) in ACTIONS {
            let a = bindings.get(*id).cloned().unwrap_or_default();
            let st = wayland_status(backend, &a, &registration);
            assert_ne!(st, HotkeyStatus::Bound, "{id} 不该在未接 portal 时报 Bound");
            assert!(st.detail().is_some(), "{id} 必须带原因说明");
        }
    }

    /// 与 `portal_register` 同形但不需要 `AppHandle` 的桩（单测无法构造 Tauri app）。
    fn portal_register_plan_stub(plan: &[PortalShortcut]) -> PortalRegistration {
        assert!(!plan.is_empty());
        PortalRegistration::Unavailable {
            reason: "TODO(P7-Wayland-portal)：尚未实现 CreateSession → BindShortcuts → Activated 流程"
                .into(),
        }
    }
}
