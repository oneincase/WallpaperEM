//! KDE Plasma 会话探测与能力路由（方案 P0，见 `docs/kde-adaptation-plan.md` §4.2）
//!
//! 本模块是**唯一**允许回答「当前是不是 KDE、是哪个会话、KWin 在不在」的地方。
//! 其它模块（`wallpaper/linux.rs`、`system_wallpaper.rs`、`kwin.rs` …）不得自己
//! 拼 `XDG_CURRENT_DESKTOP` 判断——历史上 `wallpaper/linux.rs` 的
//! `shell_icons_are_overlay()` 就是各自为政的一套，KDE/XFCE 的政策散在注释里。
//!
//! 两条硬规则（方案 §3 第 11 条，依据 KDE 自己的 `kde-systemd-start-condition.cpp`）：
//! 1. `XDG_CURRENT_DESKTOP` 是**冒号分隔列表**，历史值有 `KDE` 也有 `KDE:Plasma`，
//!    匹配必须拆开、去空白、忽略大小写。**禁止**写成 `== "KDE"`。
//! 2. 只看环境变量不够：真正的二次确认是 `org.kde.KWin` 在会话总线上存不存在。
//!
//! 设计约束：探测逻辑写成**纯函数 + 环境快照**（[`detect_with`]），这样单测不需要
//! `std::env::set_var`（那会污染并发测试）；[`detect`] 才去读真实环境与总线。

use std::collections::HashMap;

/// 会话类型。Wayland 下 `_NET_WM_WINDOW_TYPE_DESKTOP` 一类的 X11 语义整体失效，
/// 桌面层落位必须换机制，因此这个字段决定走哪条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    X11,
    Wayland,
    Unknown,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::X11 => "x11",
            SessionKind::Wayland => "wayland",
            SessionKind::Unknown => "unknown",
        }
    }
}

/// 本应用在 KDE 下要接管到哪一层。
///
/// - [`KdeScope::None`]：不是 KDE，或 KDE 但会话类型未知 —— **一律保持既有行为**，
///   不施加任何 KDE 专用改动。这是防回归的关键：拿不准时不动。
/// - [`KdeScope::Desktop`]：KDE/X11，桌面层落位、`lower_below_desktop_icons` 的
///   下沉策略等「桌面窗口」语义可用（X11 的 `_NET_WM_WINDOW_TYPE_DESKTOP` 有效）。
/// - [`KdeScope::Backend`]：KDE/Wayland，桌面窗口语义失效（已实测：5.27 下该 hint
///   被忽略，窗口退化成置底普通窗口），只能靠 KWin 窗口规则 / layer-shell / D-Bus
///   这些「后端」能力补。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KdeScope {
    None,
    Desktop,
    Backend,
}

impl KdeScope {
    pub fn as_str(self) -> &'static str {
        match self {
            KdeScope::None => "none",
            KdeScope::Desktop => "desktop",
            KdeScope::Backend => "backend",
        }
    }
}

/// KDE 会话环境快照（纯数据，便于单测与日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdeEnv {
    pub is_kde: bool,
    /// `KDE_SESSION_VERSION`：5 / 6。缺失时为 `None`（老版本或非 KDE 启动路径）。
    pub plasma_major: Option<u32>,
    pub session: SessionKind,
    /// `org.kde.KWin` 在会话总线上存在（D-Bus 二次确认 KWin 真在跑）。
    pub kwin_on_bus: bool,
    pub scope: KdeScope,
}

impl KdeEnv {
    /// 非 KDE（或 KDE 但会话未知）→ 所有 KDE 分支都必须走原有的通用路径。
    pub fn is_none_scope(&self) -> bool {
        self.scope == KdeScope::None
    }

    /// 一行式日志串，方便真机取证时 grep。
    pub fn summary(&self) -> String {
        format!(
            "is_kde={} plasma={:?} session={} kwin_on_bus={} scope={}",
            self.is_kde,
            self.plasma_major,
            self.session.as_str(),
            self.kwin_on_bus,
            self.scope.as_str()
        )
    }
}

/// 环境变量快照：变量名 → 值。缺失的键直接不在表里。
pub type EnvSnapshot = HashMap<String, String>;

/// 从进程环境抓快照。只抓本模块关心的键，避免把密钥类环境变量捞进日志。
pub fn env_snapshot() -> EnvSnapshot {
    const KEYS: [&str; 6] = [
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "KDE_FULL_SESSION",
        "KDE_SESSION_VERSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
    ];
    let mut map = EnvSnapshot::new();
    for k in KEYS {
        if let Ok(v) = std::env::var(k) {
            if !v.is_empty() {
                map.insert(k.to_string(), v);
            }
        }
    }
    map
}

/// 纯函数：由环境快照 + `kwin_on_bus` 推出能力结构。无副作用、不发 D-Bus。
///
/// 判定链：
/// 1. `XDG_CURRENT_DESKTOP` 拆分后含 `kde`（忽略大小写/空白）**或** `KDE_FULL_SESSION=true`
///    → `is_kde`。注意 `KDE_FULL_SESSION` 只表达「这是个 KDE 会话」，取值是 `true`/`1`
///    之类，历史上也出现过空值，故只要**存在且非 `false`** 就算。
/// 2. `KDE_SESSION_VERSION` → `plasma_major`（只认 5/6，其它值留 `None` 不猜）。
/// 3. `XDG_SESSION_TYPE` 优先；没有时 `WAYLAND_DISPLAY` 有值 → Wayland，
///    `DISPLAY` 有值 → X11。
/// 4. `scope`：非 KDE 或会话未知 → `None`；KDE/X11 → `Desktop`；KDE/Wayland → `Backend`。
pub fn detect_with(env: &EnvSnapshot, kwin_on_bus: bool) -> KdeEnv {
    let current = env.get("XDG_CURRENT_DESKTOP").map(String::as_str).unwrap_or("");
    let is_kde = current
        .split(':')
        .map(|t| t.trim())
        .any(|t| t.eq_ignore_ascii_case("kde"))
        || env
            .get("KDE_FULL_SESSION")
            .map(|v| {
                let v = v.trim();
                !v.is_empty() && !v.eq_ignore_ascii_case("false") && v != "0"
            })
            .unwrap_or(false);

    let plasma_major = env
        .get("KDE_SESSION_VERSION")
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v == 5 || *v == 6);

    let session = detect_session(env);

    let scope = if !is_kde {
        KdeScope::None
    } else {
        match session {
            SessionKind::X11 => KdeScope::Desktop,
            SessionKind::Wayland => KdeScope::Backend,
            SessionKind::Unknown => KdeScope::None,
        }
    };

    KdeEnv {
        is_kde,
        plasma_major,
        session,
        kwin_on_bus,
        scope,
    }
}

fn detect_session(env: &EnvSnapshot) -> SessionKind {
    if let Some(t) = env.get("XDG_SESSION_TYPE") {
        let t = t.trim();
        if t.eq_ignore_ascii_case("wayland") {
            return SessionKind::Wayland;
        }
        if t.eq_ignore_ascii_case("x11") {
            return SessionKind::X11;
        }
    }
    // XDG_SESSION_TYPE 缺失/取值不认识时按显示服务器线索兜底
    if env.get("WAYLAND_DISPLAY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return SessionKind::Wayland;
    }
    if env.get("DISPLAY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
        return SessionKind::X11;
    }
    SessionKind::Unknown
}

/// 读真实环境 + 一次同步的 D-Bus 存在性检查（超时 ≤300ms，失败即 `false`）。
///
/// D-Bus 检查失败**不是错误**：没有会话总线（容器、SSH tty、GNOME 精简环境）时
/// 只意味着无法二次确认，能力结构照常返回，调用方按 `is_kde`/`session` 决策。
pub fn detect() -> KdeEnv {
    let env = env_snapshot();
    let kwin_on_bus = kwin_service_present();
    detect_with(&env, kwin_on_bus)
}

/// 进程内缓存的能力结构（第一次访问时探测，之后只读）。
///
/// 桌面会话类型在进程生命周期内不会变，不需要每次重探；但**不允许**在单测里
/// 走这条路径（单测用 [`detect_with`]）。
static CACHED: std::sync::OnceLock<KdeEnv> = std::sync::OnceLock::new();

/// 取缓存的能力结构（真机路径统一走这里，顺带只打一次日志）。
pub fn env() -> &'static KdeEnv {
    CACHED.get_or_init(|| {
        let e = detect();
        tracing::info!("kde: {}", e.summary());
        e
    })
}

/// 桌面图标是否绘制在独立透明覆盖层上（= 壁纸沉底后图标仍可见）。
///
/// 只在 Linux 有调用点（`wallpaper/linux.rs::lower_below_desktop_icons`）；
/// 纯函数版本 [`shell_icons_are_overlay_with`] 在全部平台可测。
#[cfg(target_os = "linux")]
pub fn shell_icons_are_overlay() -> bool {
    shell_icons_are_overlay_with(&env_snapshot(), env())
}

/// [`shell_icons_are_overlay`] 的纯函数版本（单测入口）。
pub fn shell_icons_are_overlay_with(env: &EnvSnapshot, kde: &KdeEnv) -> bool {
    if kde.is_kde {
        // KDE 的桌面背景与图标同窗口绘制，永远不是「覆盖层式图标」
        return false;
    }
    let de = env.get("XDG_CURRENT_DESKTOP").map(String::as_str).unwrap_or("");
    de.split(':')
        .map(|t| t.trim())
        .any(|t| matches!(t.to_ascii_lowercase().as_str(), "gnome" | "budgie" | "pantheon"))
}

// ---------- 桌面容器（图标承载方式）：决定「壁纸会不会盖住桌面图标」 ----------
//
// 结论来源（真机实测，见 docs/kde-adaptation-plan.md §2.5 / §4.1）：KDE 上桌面背景与
// 桌面图标由 plasmashell **同一个桌面窗口**绘制，而 KWin 不允许任何客户端窗口排到该
// 窗口之下（四种原语全部实测无效）。因此只要桌面是「文件夹视图」，动态壁纸就必然盖住
// 图标；唯一可行的窗口方案是让桌面不再是文件夹视图。本模块负责把这件事**如实判断出来**，
// 交给上层做提示与引导，而不是猜。

/// 桌面 containment 的图标承载方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopIcons {
    /// 桌面是「文件夹视图」（`plugin=org.kde.plasma.folder`）：图标与壁纸同窗口 → 必然被盖住。
    FolderView,
    /// 桌面容器存在但不是文件夹视图（纯桌面，无图标）→ 没有图标可盖。
    Plain,
    /// 读不到或读不懂配置（非 KDE、文件不存在、没有桌面型 containment）→ 不下判断。
    Unknown,
}

impl DesktopIcons {
    pub fn as_str(self) -> &'static str {
        match self {
            DesktopIcons::FolderView => "folder-view",
            DesktopIcons::Plain => "plain",
            DesktopIcons::Unknown => "unknown",
        }
    }
}

/// `plasma-org.kde.plasma.desktop-appletsrc` 里的一个 containment 段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Containment {
    /// 段号，如 `1`（对应 `[Containments][1]`）。
    pub id: String,
    /// `plugin=`：桌面为 `org.kde.plasma.folder`（文件夹视图）或 `org.kde.plasma.desktop`（纯桌面）。
    pub plugin: Option<String>,
    /// `formfactor=`：桌面容器为 `0`，面板为 `2`。
    pub formfactor: Option<String>,
    /// `location=`：桌面为 `0`。
    pub location: Option<String>,
    /// `wallpaperplugin=`。
    pub wallpaper_plugin: Option<String>,
}

impl Containment {
    /// 是否桌面型容器（`formfactor=0` 或 `location=0`）。
    pub fn is_desktop(&self) -> bool {
        self.formfactor.as_deref() == Some("0") || self.location.as_deref() == Some("0")
    }

    /// 是否文件夹视图（= 会画桌面图标）。
    pub fn is_folder_view(&self) -> bool {
        self.is_desktop() && self.plugin.as_deref() == Some("org.kde.plasma.folder")
    }
}

/// 桌面图标承载方式的判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopContainment {
    pub icons: DesktopIcons,
    /// 桌面型 containment 的数量（多屏 / 多活动会 >1）。
    pub desktop_count: usize,
    /// 其中文件夹视图的数量。
    pub folder_view_count: usize,
    /// 第一个桌面容器的壁纸插件（如 `org.kde.image`）。
    pub wallpaper_plugin: Option<String>,
}

impl DesktopContainment {
    /// Plasma 桌面容器的配置文件（相对 `~/.config`）。
    pub const CONFIG_FILE: &'static str = "plasma-org.kde.plasma.desktop-appletsrc";

    /// 桌面图标会被动态壁纸盖住（需要在首次启动时提示用户 / 引导关掉图标）。
    pub fn icons_will_be_covered(&self) -> bool {
        self.icons == DesktopIcons::FolderView
    }

    pub fn summary(&self) -> String {
        format!(
            "icons={} desktop_containments={} folder_views={} wallpaper_plugin={:?}",
            self.icons.as_str(),
            self.desktop_count,
            self.folder_view_count,
            self.wallpaper_plugin
        )
    }
}

/// 解析 Plasma 桌面容器配置的纯函数（单测入口）。
///
/// 只认 `[Containments][<id>]` 这一层；`[Containments][1][Wallpaper][org.kde.image][General]`
/// 这类子段不产生新 containment，其键也不回填到父段。
pub fn parse_containments(text: &str) -> Vec<Containment> {
    let mut out: Vec<Containment> = Vec::new();
    let mut cur: Option<Containment> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(c) = cur.take() {
                out.push(c);
            }
            // 仅 `[Containments][N]`：恰好两组方括号，第二组是纯数字。
            let inner = &line[1..line.len() - 1];
            let groups: Vec<&str> = inner.split("][").collect();
            if groups.len() == 2
                && groups[0] == "Containments"
                && !groups[1].is_empty()
                && groups[1].chars().all(|c| c.is_ascii_digit())
            {
                cur = Some(Containment { id: groups[1].to_string(), ..Default::default() });
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let Some(c) = cur.as_mut() else { continue };
        let value = value.trim().to_string();
        match key.trim() {
            "plugin" => c.plugin = Some(value),
            "formfactor" => c.formfactor = Some(value),
            "location" => c.location = Some(value),
            "wallpaperplugin" => c.wallpaper_plugin = Some(value),
            _ => {}
        }
    }
    if let Some(c) = cur.take() {
        out.push(c);
    }
    out
}

/// 由配置文本判定桌面图标承载方式（纯函数）。
pub fn desktop_containment_with(text: &str) -> DesktopContainment {
    let all = parse_containments(text);
    let desktops: Vec<&Containment> = all.iter().filter(|c| c.is_desktop()).collect();
    let folder_views = desktops.iter().filter(|c| c.is_folder_view()).count();
    let icons = if desktops.is_empty() {
        DesktopIcons::Unknown
    } else if folder_views > 0 {
        DesktopIcons::FolderView
    } else {
        DesktopIcons::Plain
    };
    DesktopContainment {
        icons,
        desktop_count: desktops.len(),
        folder_view_count: folder_views,
        wallpaper_plugin: desktops.first().and_then(|c| c.wallpaper_plugin.clone()),
    }
}

/// 读取并判定本机桌面图标承载方式（非 Linux 恒 `Unknown`），带 `XDG_CONFIG_HOME` 回落。
pub fn desktop_containment() -> DesktopContainment {
    let text = read_desktop_appletsrc();
    match text {
        Some(t) => desktop_containment_with(&t),
        None => DesktopContainment {
            icons: DesktopIcons::Unknown,
            desktop_count: 0,
            folder_view_count: 0,
            wallpaper_plugin: None,
        },
    }
}

fn read_desktop_appletsrc() -> Option<String> {
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    std::fs::read_to_string(base.join(DesktopContainment::CONFIG_FILE)).ok()
}

/// 启动时把「桌面图标会被动态壁纸盖住」这一已知降级如实写进日志（只读，不改用户配置）。
///
/// 真机实测（Plasma 5.27/X11）证明：图标不会消失，但会**被盖住且仍可点中**——
/// 比「点不到」更糟。所以这里必须提示，而不是静默降级。
pub fn log_desktop_icons_status() {
    let e = env();
    if !e.is_kde {
        return;
    }
    let c = desktop_containment();
    tracing::info!("kde desktop containment: {}", c.summary());
    if c.icons_will_be_covered() {
        tracing::warn!(
            "kde: 桌面为「文件夹视图」，动态壁纸会盖住桌面图标（图标仍可点中却看不见）；\
             如需图标可见请把桌面布局改为「桌面」（无需文件夹视图），或改用壁纸插件方案。\
             详见 docs/kde-adaptation-plan.md §4.1"
        );
    }
}

/// `org.kde.KWin` 是否在会话总线上（同步、带超时）。
#[cfg(target_os = "linux")]
fn kwin_service_present() -> bool {
    kwin_service_present_impl()
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)] // 非 Linux 平台恒 false；detect_with 的单测不走这条路径
fn kwin_service_present() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn kwin_service_present_impl() -> bool {
    use std::time::Duration;
    // 独立线程里跑异步，避免把 tokio 运行时假设带进同步调用点（探测发生在
    // setup 早期，可能还没有运行时句柄）。
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let ok = match rt {
            Ok(rt) => rt.block_on(async {
                match tokio::time::timeout(
                    Duration::from_millis(300),
                    zbus::Connection::session(),
                )
                .await
                {
                    Ok(Ok(conn)) => {
                        let has = conn
                            .call_method(
                                Some("org.freedesktop.DBus"),
                                "/org/freedesktop/DBus",
                                Some("org.freedesktop.DBus"),
                                "NameHasOwner",
                                &("org.kde.KWin",),
                            )
                            .await
                            .ok()
                            .and_then(|reply| reply.body().deserialize::<bool>().ok())
                            .unwrap_or(false);
                        // 退路：NameHasOwner 不可用时按接口 introspection 存在性判断
                        if has {
                            true
                        } else {
                            matches!(
                                tokio::time::timeout(
                                    Duration::from_millis(300),
                                    conn.call_method(
                                        Some("org.kde.KWin"),
                                        "/KWin",
                                        Some("org.freedesktop.DBus.Introspectable"),
                                        "Introspect",
                                        &(),
                                    ),
                                )
                                .await,
                                Ok(Ok(_))
                            )
                        }
                    }
                    _ => false,
                }
            }),
            Err(_) => false,
        };
        let _ = tx.send(ok);
    });
    rx.recv_timeout(Duration::from_millis(400)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> EnvSnapshot {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // ---------- is_kde 判定 ----------

    #[test]
    fn kde_variants_are_recognized() {
        for de in ["KDE", "kde", "KDE:Plasma", "kde:plasma", "  KDE : Plasma  "] {
            let e = detect_with(&env(&[("XDG_CURRENT_DESKTOP", de)]), false);
            assert!(e.is_kde, "XDG_CURRENT_DESKTOP={de:?} 应判为 KDE");
        }
    }

    #[test]
    fn non_kde_variants_are_not_recognized() {
        for de in ["GNOME", "XFCE", "MATE", "Cinnamon", "ubuntu:GNOME", ""] {
            let e = detect_with(&env(&[("XDG_CURRENT_DESKTOP", de)]), false);
            assert!(!e.is_kde, "XDG_CURRENT_DESKTOP={de:?} 不应判为 KDE");
        }
    }

    #[test]
    fn kde_full_session_alone_is_enough() {
        let e = detect_with(&env(&[("KDE_FULL_SESSION", "true")]), true);
        assert!(e.is_kde);
        let e = detect_with(&env(&[("KDE_FULL_SESSION", "1")]), true);
        assert!(e.is_kde);
        // false/0 不算
        for v in ["false", "FALSE", "0", ""] {
            let e = detect_with(&env(&[("KDE_FULL_SESSION", v)]), false);
            assert!(!e.is_kde, "KDE_FULL_SESSION={v:?} 不应判为 KDE");
        }
    }

    /// 回归护栏：`XDG_CURRENT_DESKTOP` 含 `kde` 子串但不是独立段时不算 KDE
    /// （例如某个自定义 DE 叫 `kdestro`，不该被当 Plasma 接管）。
    #[test]
    fn substring_kde_is_not_kde() {
        let e = detect_with(&env(&[("XDG_CURRENT_DESKTOP", "kdestro")]), false);
        assert!(!e.is_kde);
        let e = detect_with(&env(&[("XDG_CURRENT_DESKTOP", "GNOME:kdeish")]), false);
        assert!(!e.is_kde);
    }

    // ---------- plasma 版本 ----------

    #[test]
    fn plasma_major_only_accepts_5_and_6() {
        for (v, want) in [("5", Some(5u32)), ("6", Some(6u32))] {
            assert_eq!(
                detect_with(&env(&[("KDE_FULL_SESSION", "true"), ("KDE_SESSION_VERSION", v)]), true)
                    .plasma_major,
                want
            );
        }
        // 缺失/不认识的值一律 None，不猜
        for v in ["4", "7", "6.0", "", "x"] {
            assert_eq!(
                detect_with(&env(&[("KDE_FULL_SESSION", "true"), ("KDE_SESSION_VERSION", v)]), true)
                    .plasma_major,
                None,
                "KDE_SESSION_VERSION={v:?} 应留 None"
            );
        }
        assert_eq!(detect_with(&env(&[("KDE_FULL_SESSION", "true")]), true).plasma_major, None);
    }

    // ---------- 会话类型 ----------

    #[test]
    fn session_type_from_xdg_session_type() {
        let e = detect_with(
            &env(&[("KDE_FULL_SESSION", "true"), ("XDG_SESSION_TYPE", "wayland")]),
            true,
        );
        assert_eq!(e.session, SessionKind::Wayland);
        let e = detect_with(
            &env(&[("KDE_FULL_SESSION", "true"), ("XDG_SESSION_TYPE", "x11")]),
            true,
        );
        assert_eq!(e.session, SessionKind::X11);
        // 大小写不敏感
        let e = detect_with(
            &env(&[("KDE_FULL_SESSION", "true"), ("XDG_SESSION_TYPE", "Wayland")]),
            true,
        );
        assert_eq!(e.session, SessionKind::Wayland);
    }

    #[test]
    fn session_type_falls_back_to_display_vars() {
        let e = detect_with(&env(&[("KDE_FULL_SESSION", "true"), ("WAYLAND_DISPLAY", "wayland-0")]), true);
        assert_eq!(e.session, SessionKind::Wayland);
        let e = detect_with(&env(&[("KDE_FULL_SESSION", "true"), ("DISPLAY", ":0")]), true);
        assert_eq!(e.session, SessionKind::X11);
        // 两者都有时 WAYLAND_DISPLAY 优先（XWayland 会话的 X11 应用也会设 DISPLAY）
        let e = detect_with(
            &env(&[("KDE_FULL_SESSION", "true"), ("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")]),
            true,
        );
        assert_eq!(e.session, SessionKind::Wayland);
        // 都没有
        let e = detect_with(&env(&[("KDE_FULL_SESSION", "true")]), true);
        assert_eq!(e.session, SessionKind::Unknown);
    }

    // ---------- scope 路由（P0 的核心产出） ----------

    #[test]
    fn scope_routing_matrix() {
        let cases: &[(&str, Option<u32>, SessionKind, KdeScope, &str)] = &[
            // 非 KDE 一律 None（防回归：GNOME/XFCE 行为必须零变化）
            ("GNOME", None, SessionKind::X11, KdeScope::None, "gnome-x11"),
            ("XFCE", None, SessionKind::Wayland, KdeScope::None, "xfce-wayland"),
            // KDE/X11 → 桌面层语义可用
            ("KDE", Some(5), SessionKind::X11, KdeScope::Desktop, "plasma5-x11"),
            ("KDE", Some(6), SessionKind::X11, KdeScope::Desktop, "plasma6-x11"),
            // KDE/Wayland → 只能走后端能力（桌面类型 hint 已实测无效）
            ("KDE:Plasma", Some(6), SessionKind::Wayland, KdeScope::Backend, "plasma6-wayland"),
            // KDE 但会话未知 → 不接管（拿不准时保持既有行为）
            ("KDE", Some(6), SessionKind::Unknown, KdeScope::None, "plasma6-unknown"),
        ];
        for (de, major, session, want, label) in cases {
            let mut pairs = vec![("XDG_CURRENT_DESKTOP", *de)];
            if let Some(m) = major {
                pairs.push(("KDE_SESSION_VERSION", if *m == 5 { "5" } else { "6" }));
            }
            pairs.push((
                "XDG_SESSION_TYPE",
                match session {
                    SessionKind::X11 => "x11",
                    SessionKind::Wayland => "wayland",
                    SessionKind::Unknown => "tty",
                },
            ));
            let got = detect_with(&env(&pairs), true);
            assert_eq!(got.scope, *want, "{label}: scope 应为 {:?}", want);
        }
    }

    /// 会话类型未知且环境里也没有 DISPLAY/WAYLAND_DISPLAY 时，
    /// 不能被 `XDG_SESSION_TYPE=tty` 误判成 X11。
    #[test]
    fn tty_session_does_not_become_x11() {
        let e = detect_with(
            &env(&[("KDE_FULL_SESSION", "true"), ("XDG_SESSION_TYPE", "tty")]),
            false,
        );
        assert_eq!(e.session, SessionKind::Unknown);
        assert_eq!(e.scope, KdeScope::None);
    }

    // ---------- 覆盖层式图标判定（既有行为护栏） ----------

    #[test]
    fn overlay_icons_only_for_gnome_like() {
        // 白名单判定本身与 KDE 无关：这里用「非 KDE 会话」的能力结构
        let non_kde = detect_with(&env(&[("XDG_CURRENT_DESKTOP", "GNOME")]), false);
        assert!(!non_kde.is_kde);
        for de in ["GNOME", "gnome", "ubuntu:GNOME", "Budgie", "Pantheon", "GNOME:GNOME-Classic"] {
            assert!(
                shell_icons_are_overlay_with(&env(&[("XDG_CURRENT_DESKTOP", de)]), &non_kde),
                "XDG_CURRENT_DESKTOP={de:?} 应判为覆盖层式图标"
            );
        }
        // 非覆盖层 DE 保持 false，且与既有实现逐字一致
        for de in ["KDE", "XFCE", "MATE", "Cinnamon", "kdestro", ""] {
            assert!(
                !shell_icons_are_overlay_with(&env(&[("XDG_CURRENT_DESKTOP", de)]), &non_kde),
                "XDG_CURRENT_DESKTOP={de:?} 不应判为覆盖层式图标"
            );
        }
        // KDE 会话下，即使 XDG_CURRENT_DESKTOP 写成 GNOME（异常组合），也不下沉
        let kde = detect_with(&env(&[("KDE_FULL_SESSION", "true")]), true);
        assert!(kde.is_kde);
        assert!(
            !shell_icons_are_overlay_with(&env(&[("XDG_CURRENT_DESKTOP", "GNOME")]), &kde),
            "KDE 会话不应判为覆盖层式图标（plasmashell 背景与图标同窗口绘制）"
        );
    }

    #[test]
    fn summary_is_greppable() {
        let e = detect_with(
            &env(&[
                ("XDG_CURRENT_DESKTOP", "KDE"),
                ("KDE_SESSION_VERSION", "6"),
                ("XDG_SESSION_TYPE", "wayland"),
            ]),
            true,
        );
        assert_eq!(
            e.summary(),
            "is_kde=true plasma=Some(6) session=wayland kwin_on_bus=true scope=backend"
        );
    }

    // ---------- 桌面容器 / 图标承载方式（P4 的判定依据） ----------

    /// Plasma 5.27 真机 `~/.config/plasma-org.kde.plasma.desktop-appletsrc` 的形状：
    /// 桌面容器 `[Containments][1]`（文件夹视图）+ 面板 `[Containments][2]` +
    /// 壁纸子段（子段不得产生新 containment，也不得回填父段）。
    const APPLETSRC_FOLDER_VIEW: &str = "\
[Containments][1]
activityId=1dfc8044-0000-0000-0000-000000000000
formfactor=0
lastScreen=0
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Color=0,0,0
Image=file:///usr/share/wallpapers/Next/contents/images/1280x800.png

[Containments][2]
formfactor=2
location=4
plugin=org.kde.panel
wallpaperplugin=org.kde.image
";

    #[test]
    fn folder_view_desktop_is_detected() {
        let c = desktop_containment_with(APPLETSRC_FOLDER_VIEW);
        assert_eq!(c.icons, DesktopIcons::FolderView);
        assert_eq!(c.desktop_count, 1, "面板 formfactor=2/location=4 不算桌面");
        assert_eq!(c.folder_view_count, 1);
        assert_eq!(c.wallpaper_plugin.as_deref(), Some("org.kde.image"));
        assert!(c.icons_will_be_covered());
        assert!(c.summary().contains("icons=folder-view"));
    }

    #[test]
    fn plain_desktop_is_not_folder_view() {
        let text = "[Containments][1]\nformfactor=0\nlocation=0\nplugin=org.kde.plasma.desktop\n";
        let c = desktop_containment_with(text);
        assert_eq!(c.icons, DesktopIcons::Plain);
        assert_eq!(c.desktop_count, 1);
        assert_eq!(c.folder_view_count, 0);
        assert!(!c.icons_will_be_covered());
    }

    #[test]
    fn multiple_desktops_count_and_any_folder_view_wins() {
        let text = "\
[Containments][1]
formfactor=0
location=0
plugin=org.kde.plasma.desktop

[Containments][3]
formfactor=0
location=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image
";
        let c = desktop_containment_with(text);
        assert_eq!(c.desktop_count, 2);
        assert_eq!(c.folder_view_count, 1);
        assert_eq!(c.icons, DesktopIcons::FolderView, "任一桌面是文件夹视图即会画图标");
    }

    #[test]
    fn panel_only_or_missing_config_is_unknown() {
        let panel_only = "[Containments][2]\nformfactor=2\nlocation=4\nplugin=org.kde.panel\n";
        let c = desktop_containment_with(panel_only);
        assert_eq!(c.icons, DesktopIcons::Unknown, "没有桌面型 containment 时不下判断");
        assert_eq!(c.desktop_count, 0);
        assert_eq!(desktop_containment_with("").icons, DesktopIcons::Unknown);
    }

    #[test]
    fn garbage_appletsrc_does_not_panic_and_stays_unknown() {
        for text in [
            "not an ini at all\n===\n[Containments]\nplugin=org.kde.plasma.folder\n",
            "[Containments][x]\nformfactor=0\nplugin=org.kde.plasma.folder\n",
            "[]\n[]\nplugin=org.kde.plasma.folder\n",
            "[Containments][1]\nformfactor=0\n",
        ] {
            let c = desktop_containment_with(text);
            assert!(
                matches!(c.icons, DesktopIcons::Unknown | DesktopIcons::Plain),
                "垃圾输入不得判成文件夹视图（拿不准时不下判断）: {text:?}"
            );
        }
    }

    #[test]
    fn parse_ignores_wallpaper_subsections() {
        let all = parse_containments(APPLETSRC_FOLDER_VIEW);
        assert_eq!(all.len(), 2, "子段 [Containments][1][Wallpaper] 不得产生第三个 containment");
        assert_eq!(all[0].id, "1");
        assert_eq!(all[0].plugin.as_deref(), Some("org.kde.plasma.folder"));
        assert_eq!(all[1].id, "2");
        assert!(!all[1].is_desktop());
    }
}
