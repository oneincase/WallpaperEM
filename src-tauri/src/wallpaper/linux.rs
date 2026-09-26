//! Linux 桌面层窗口与屏幕枚举（X11 / Wayland 兼容层）。
//!
//! 与 macOS 后端的差异与取舍：
//! - **屏幕枚举**走 Tauri 的 `available_monitors`（GTK/GDK），拿到的是物理像素 +
//!   per-monitor scale factor，这里统一换算成**逻辑坐标**（与窗口 set_position/
//!   set_size 的逻辑单位一致，等价于 macOS 的 points）。
//! - **桌面层级**：X11 下用 `_NET_WM_WINDOW_TYPE_DESKTOP`（经
//!   tauri-plugin-desktop-underlay 的 GTK type hint），壁纸窗口沉到所有普通
//!   窗口之下、跨工作区可见。Wayland 没有等价的公开协议（需要 wlr-layer-shell，
//!   且各合成器行为不一），当前版本在 Wayland 下壁纸窗口表现为「置底 + 全工作区」
//!   的普通无边框窗口 —— 覆盖桌面时正常，但没有严格的桌面层语义。
//! - **桌面图标堆叠**：`_NET_WM_WINDOW_TYPE_DESKTOP` 只保证「低于普通窗口」，
//!   桌面层内部按映射顺序堆叠——GNOME(DING)/Budgie 的图标是独立透明覆盖窗口，
//!   壁纸窗口后映射会把图标盖住，需要显式 lower 到桌面层底部（见
//!   [`lower_below_desktop_icons`]）。KDE/XFCE 的桌面窗口自绘不透明背景+图标，
//!   沉底会让壁纸整个不可见，这些 DE 保持「壁纸在上」的降级行为。
//! - **「隐藏图标」开关**：macOS 上是窗口层级的两档切换；Linux 无法统一压在
//!   图标之上，这里只切换鼠标事件穿透。
//! - **前台应用观察者 / 自动暂停**：Linux 各 DE 没有统一的前台应用通知机制
//!   （X11 可读 _NET_ACTIVE_WINDOW，Wayland 各家私有协议），前台观察为空实现；
//!   但自动暂停的判据已改为**遮挡快照**（[`occlusion_snapshot`]，X11 会话有效），
//!   Wayland 会话无法枚举全局窗口清单，开关不生效（详见该函数说明）。
//! - **显示器睡眠**：无统一查询接口（logind/DPMS 各管一段），恒返回 false，
//!   即不做「合盖暂停/释放壁纸」的自动处理。

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, LogicalPosition, LogicalSize, Runtime, WebviewWindow};
use tauri_plugin_desktop_underlay::DesktopUnderlayExt;

use gtk::prelude::WidgetExt;

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    /// 稳定显示器 id（连接器名的 FNV-1a 哈希，见 [`monitor_id`]）
    pub id: u32,
    /// 显示器名称（GDK 输出名，如 `DP-1`/`HDMI-A-1`）
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    pub is_primary: bool,
}

/// Linux 的显示器 id 用连接器名哈希、跨重启稳定，无旧→新 迁移需求。
pub fn legacy_display_ids() -> Vec<(String, String)> {
    Vec::new()
}

/// 供电类型。Linux 无统一查询（upower/logind 各管一段），恒 None = 不拦
/// （「仅充电时轮播」暂仅 macOS 生效）。
pub fn on_ac_power() -> Option<bool> {
    None
}

/// 显示器名称缓存刷新（macOS 用：NSScreen 主线程限制）。Linux 无需缓存，空实现。
pub fn refresh_display_meta() {}

/// 引擎启动时注入的 AppHandle（屏幕枚举/光标查询都需要它进 Tauri runtime）
static APP: OnceLock<AppHandle> = OnceLock::new();

/// 最近一次枚举到的显示器（逻辑坐标 + scale）与枚举时刻。
///
/// 为什么缓存：GTK 不是线程安全的，tao 的 EventLoopWindowTarget 跨线程访问
/// 包在 unsafe Send/Sync 里「碰巧能用」，但 30Hz 的指针轮询线程每次都过一遍
/// GTK 查询仍是不必要的风险与开销。权威刷新由壁纸监控（2s tick，主线程）
/// 驱动；其余调用方（指针轮询/光标换算）读这份 TTL 缓存。
static SCREENS_CACHE: Mutex<Option<(Instant, Vec<(ScreenInfo, f64)>)>> = Mutex::new(None);

/// 屏幕列表缓存有效期（与壁纸监控 tick 同量级）
const SCREENS_CACHE_TTL: Duration = Duration::from_millis(1500);

/// 由 wallpaper::init 调用一次（仅 Linux 需要 AppHandle 常驻句柄）
pub fn store_app_handle(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// 显示器名称 → 稳定的数值 id（macOS 用 CGDisplayID；Linux 用连接器名的哈希，
/// 如 "eDP-1"/"HDMI-A-1"，热插拔后同一块屏能拿回同一个 id，会话恢复才对得上）
fn monitor_id(name: Option<&str>, index: usize) -> u32 {
    match name {
        Some(n) if !n.is_empty() => {
            // FNV-1a 32bit
            let mut h: u32 = 0x811c_9dc5;
            for b in n.as_bytes() {
                h ^= *b as u32;
                h = h.wrapping_mul(0x0100_0193);
            }
            // 避开 0（wallpaper-0 与「无 id」语义冲突）
            if h == 0 {
                1
            } else {
                h
            }
        }
        _ => (index as u32) + 1,
    }
}

/// 活动显示器列表（逻辑坐标，左上原点）。
/// 命中 TTL 缓存直接返回；过期则经 GTK 重新枚举并回填缓存。
pub fn active_screens() -> Vec<ScreenInfo> {
    if let Ok(cache) = SCREENS_CACHE.lock() {
        if let Some((at, screens)) = &*cache {
            if at.elapsed() < SCREENS_CACHE_TTL {
                return screens.iter().map(|(s, _)| s.clone()).collect();
            }
        }
    }
    let out = query_monitors();
    if let Ok(mut cache) = SCREENS_CACHE.lock() {
        *cache = Some((Instant::now(), out.clone()));
    }
    out.into_iter().map(|(s, _)| s).collect()
}

/// 全局逻辑坐标点落在哪台显示器（滚轮派发用；Linux 暂无系统滚轮源，备平台接口一致）。
#[allow(dead_code)]
pub fn hit_screen_id(x: f64, y: f64) -> Option<u32> {
    active_screens()
        .into_iter()
        .find(|s| x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h)
        .map(|s| s.id)
}

/// 经 Tauri/GTK 枚举显示器，返回（逻辑坐标屏幕信息, scale_factor）
fn query_monitors() -> Vec<(ScreenInfo, f64)> {
    let Some(app) = APP.get() else {
        return Vec::new();
    };
    let monitors = match app.available_monitors() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => match app.primary_monitor() {
            Ok(Some(m)) => vec![m],
            _ => return Vec::new(),
        },
        Err(e) => {
            tracing::warn!("linux: available_monitors failed: {e}");
            return Vec::new();
        }
    };
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let scale = m.scale_factor();
            let scale = if scale > 0.0 { scale } else { 1.0 };
            let pos = m.position();
            let size = m.size();
            (
                ScreenInfo {
                    id: monitor_id(m.name().map(|s| s.as_str()), i),
                    name: m
                        .name()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| format!("显示器 {}", i + 1)),
                    x: pos.x as f64 / scale,
                    y: pos.y as f64 / scale,
                    w: size.width as f64 / scale,
                    h: size.height as f64 / scale,
                    scale,
                    // 虚拟桌面坐标系里主屏恒在原点
                    is_primary: pos.x == 0 && pos.y == 0,
                },
                scale,
            )
        })
        .collect()
}

/// 主显示器是否睡眠。Linux 无统一查询（logind/DPMS 分属不同栈），恒 false。
pub fn display_asleep() -> bool {
    false
}

/// 设置窗口 frame（逻辑坐标）
pub fn set_frame<R: Runtime>(window: &WebviewWindow<R>, x: f64, y: f64, w: f64, h: f64) {
    let _ = window.set_position(LogicalPosition::new(x, y));
    let _ = window.set_size(LogicalSize::new(w, h));
}

/// 应用桌面层属性（主线程调用；插件内部也会自行派发到主线程）。
///
/// `interactive=false`（「隐藏图标」关闭，默认）：桌面层级 + 鼠标穿透，
/// 交互靠指针注入（同 macOS 语义）。
/// `interactive=true`：保持桌面层级但接收鼠标事件（Linux 无法统一压过
/// 桌面图标，图标是否可点取决于文件管理器的桌面实现）。
pub fn apply_desktop_window<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    frame: (f64, f64, f64, f64),
    interactive: bool,
) {
    // 桌面层级（X11: _NET_WM_WINDOW_TYPE_DESKTOP；Wayland 下该 hint 无效，静默忽略）
    if let Err(e) = window.set_desktop_underlay(true) {
        tracing::warn!("linux: set_desktop_underlay failed: {e}");
    }
    // 沉到桌面层底部，让 GNOME(DING) 等覆盖层式桌面图标浮在壁纸之上
    lower_below_desktop_icons(window);
    // 跨工作区可见（对应 macOS 的 CanJoinAllSpaces）
    let _ = window.set_visible_on_all_workspaces(true);
    let _ = window.set_skip_taskbar(true);
    // 非交互态鼠标穿透（对应 macOS 的 ignoresMouseEvents）
    set_ignore_cursor_events_safe(window, !interactive);
    set_frame(window, frame.0, frame.1, frame.2, frame.3);
    tracing::info!("linux: apply_desktop_window ok (interactive={interactive}, frame={frame:?})");
}

/// 把壁纸窗口沉到桌面层底部（X11 `XLowerWindow`；Wayland 下 no-op）。
///
/// `_NET_WM_WINDOW_TYPE_DESKTOP` 只把窗口放进桌面层，层内堆叠按映射顺序：
/// GNOME 的 DING 扩展 / Budgie 桌面视图的图标窗口先于壁纸窗口映射，壁纸不
/// 主动下沉就会盖住图标。仅对「图标是独立透明覆盖层」的 DE 下沉——
/// KDE(plasmashell)/XFCE(xfdesktop) 的桌面窗口自绘不透明背景，沉底会让壁纸
/// 整个不可见，这些 DE 没有纯堆叠解（KDE 需做成 Plasma 壁纸插件），保持
/// 「壁纸在上、图标被盖」的既定降级。窗口未映射时 lower 无效，挂 map 信号补沉。
fn lower_below_desktop_icons<R: Runtime>(window: &WebviewWindow<R>) {
    if !shell_icons_are_overlay() {
        return;
    }
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("linux: gtk_window unavailable: {e}");
            return;
        }
    };
    if gtk_win.is_mapped() {
        if let Some(gdk_win) = gtk_win.window() {
            gdk_win.lower();
            tracing::info!("linux: wallpaper window lowered below desktop icons");
        }
        return;
    }
    gtk_win.connect_map(|w| {
        if let Some(gdk_win) = w.window() {
            gdk_win.lower();
            tracing::info!("linux: wallpaper window lowered below desktop icons (on map)");
        }
    });
}

/// 桌面图标是否绘制在独立的透明覆盖层窗口上（= 壁纸沉底后图标可见、且壁纸
/// 不会被桌面窗口自己的不透明背景遮住）。
/// GNOME(DING)/Budgie/Pantheon 满足；KDE/XFCE/MATE/Cinnamon 不满足（桌面窗口
/// 自绘背景，或图标与背景同窗口绘制）。
fn shell_icons_are_overlay() -> bool {
    let de = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let overlay = de
        .split(':')
        .map(|t| t.trim().to_ascii_lowercase())
        .any(|t| matches!(t.as_str(), "gnome" | "budgie" | "pantheon"));
    if !overlay {
        tracing::debug!(
            "linux: XDG_CURRENT_DESKTOP={de:?} 非覆盖层式图标 DE，跳过壁纸沉底（避免被桌面背景窗口遮住）"
        );
    }
    overlay
}

/// 设置鼠标穿透（安全版）。
///
/// tao 的 `CursorIgnoreEvents` 分支是 `window().unwrap()`：GdkWindow 尚未
/// realize（窗口创建后、show/映射之前）时调用必 panic（tao 0.35
/// event_loop.rs:457，且发生在 glib 回调里 = 直接 abort）。
/// 因此未 realize 时挂一次性 realize 信号补设。
fn set_ignore_cursor_events_safe<R: Runtime>(window: &WebviewWindow<R>, ignore: bool) {
    let gtk_win = match window.gtk_window() {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!("linux: gtk_window unavailable: {e}");
            return;
        }
    };
    if gtk_win.window().is_some() {
        let _ = window.set_ignore_cursor_events(ignore);
        return;
    }
    let w = window.clone();
    gtk_win.connect_realize(move |_| {
        let _ = w.set_ignore_cursor_events(ignore);
    });
}

/// macOS 的 movableByWindowBackground 在 Linux 无对应概念：
/// Linux 窗口有原生标题栏（props 窗口的 Overlay 样式在非 macOS 上被忽略），
/// 直接拖标题栏即可。空实现保持平台层签名一致。
pub fn set_movable_by_background<R: Runtime>(_window: &WebviewWindow<R>, _movable: bool) {}

/// 前台应用切换观察者：Linux 各 DE 无统一通知机制，空实现。
/// 自动暂停不依赖它 —— 判据是 [`occlusion_snapshot`] 的逐屏遮挡（X11 有效）；
/// 指针注入保持「桌面恒活动」的旧行为。
pub fn start_auto_pause_observer(_app: &tauri::AppHandle) {
    tracing::info!("linux: 前台应用观察者不可用（自动暂停走遮挡快照，X11 会话生效）");
}

/// Linux 无此机制：恢复信号只走前台观察（见 start_auto_pause_observer 的说明）
pub fn start_desktop_click_monitor(_app: &tauri::AppHandle) {}

/// 遮挡快照（Linux/X11）：逐屏遮挡比例，自动暂停「看得见就播」的可见性判据
/// （与 macOS 的 CGWindowList / Windows 的 EnumWindows 实现同构）。
///
/// 数据源 `_NET_CLIENT_LIST_STACKING`（窗口管理器维护的顶层窗口清单，缺失时
/// 退回无序的 `_NET_CLIENT_LIST`）+ `get_geometry`（相对根窗口 = 虚拟桌面坐标）。
/// 过滤规则与另两平台一致：本应用窗口（`_NET_WM_PID`）不算、
/// `_NET_WM_WINDOW_TYPE_DESKTOP/DOCK`（桌面/面板，铺屏或常驻）不算、
/// 不可见的（map_state != VIEWABLE，含最小化）不算。
///
/// **Wayland 会话直接返回 None**：Wayland 协议没有全局窗口清单，Xwayland 只能看到
/// X11 客户端、看不到原生 Wayland 窗口 —— 拿残缺清单算遮挡会把「原生应用全屏」
/// 误判成桌面干净（壁纸在全屏后面白烧），宁可保持不生效（判定表恒 Keep）。
/// 显式 `GDK_BACKEND=x11` 的会话不受此限（X11 就是真实视图）。
pub fn occlusion_snapshot() -> Option<super::auto_pause::OcclusionSnapshot> {
    use super::auto_pause::{covered_ratio, OcclusionSnapshot};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState, Window};

    let force_x11 = std::env::var("GDK_BACKEND").is_ok_and(|b| b == "x11");
    if !force_x11 && std::env::var("WAYLAND_DISPLAY").is_ok() {
        return None;
    }

    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let intern = |name: &[u8]| -> Option<u32> {
        Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
    };

    // 顶层窗口清单（没有 EWMH 的 WM 罕见；两代属性都没有就不判定）
    let list_atom = intern(b"_NET_CLIENT_LIST_STACKING")?;
    let windows: Vec<Window> = {
        let prop = |atom| -> Option<Vec<u32>> {
            Some(
                conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 4096)
                    .ok()?
                    .reply()
                    .ok()?
                    .value32()?
                    .collect(),
            )
        };
        match prop(list_atom).filter(|v| !v.is_empty()) {
            Some(v) => v,
            None => prop(intern(b"_NET_CLIENT_LIST")?)?,
        }
    };

    let pid_atom = intern(b"_NET_WM_PID")?;
    let type_atom = intern(b"_NET_WM_WINDOW_TYPE")?;
    let type_desktop = intern(b"_NET_WM_WINDOW_TYPE_DESKTOP")?;
    let type_dock = intern(b"_NET_WM_WINDOW_TYPE_DOCK")?;
    let mut rects: Vec<(f64, f64, f64, f64)> = Vec::new();
    for w in windows {
        // 本应用窗口（设置窗/壁纸窗）不算遮挡
        let pid = conn
            .get_property(false, w, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut it| it.next()));
        if pid == Some(std::process::id()) {
            continue;
        }
        // 桌面/面板不算（桌面元素铺满整屏，会让「桌面干净」恒不成立）
        let wtype: Vec<u32> = conn
            .get_property(false, w, type_atom, AtomEnum::ATOM, 0, 8)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|it| it.collect()))
            .unwrap_or_default();
        if wtype.contains(&type_desktop) || wtype.contains(&type_dock) {
            continue;
        }
        // 只算映射可见的（最小化/收起 = UNMAPPED/UNVIEWABLE）
        let viewable = conn
            .get_window_attributes(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|a| a.map_state == MapState::VIEWABLE)
            .unwrap_or(false);
        if !viewable {
            continue;
        }
        // 顶层窗口的 x/y 相对根窗口 = 虚拟桌面坐标（与显示器同一空间）
        if let Some(g) = conn.get_geometry(w).ok().and_then(|c| c.reply().ok()) {
            if g.width > 0 && g.height > 0 {
                rects.push((
                    g.x as f64,
                    g.y as f64,
                    g.x as f64 + g.width as f64,
                    g.y as f64 + g.height as f64,
                ));
            }
        }
    }

    // 显示器（active_screens 是逻辑坐标，× scale 还原 X11 像素空间；id 同一套推导）
    let coverages = active_screens()
        .into_iter()
        .map(|s| {
            let rect = (
                s.x * s.scale,
                s.y * s.scale,
                (s.x + s.w) * s.scale,
                (s.y + s.h) * s.scale,
            );
            (s.id, covered_ratio(&rects, rect))
        })
        .collect();
    Some(OcclusionSnapshot { coverages })
}

/// 前台应用类型（Linux）：无统一实现，恒 Unknown。
pub fn frontmost_kind(_app: &tauri::AppHandle) -> super::auto_pause::FrontKind {
    super::auto_pause::FrontKind::Unknown
}

/// 读系统光标位置（逻辑坐标，左上原点）与左键状态。
///
/// 经 Tauri `cursor_position`（GTK 实现）查询：X11 下任意时刻有效；
/// Wayland 下协议不允许读取全局光标，仅当光标悬停在本应用窗口上时才有值，
/// 其余情况返回 None（指针注入自动静默失效，属 Wayland 的既定限制）。
/// 按键状态 GTK 同样拿不到全局值，恒报 0（视差/hover 正常，按压跟随类
/// 效果在 Linux 上暂不支持）。
pub fn cursor_state() -> Option<(f64, f64, u32)> {
    let app = APP.get()?;
    let pos = app.cursor_position().ok()?;
    // 物理像素 → 逻辑坐标：按命中屏幕的 scale 换算；不在任何屏内时用主屏 scale
    let cache = SCREENS_CACHE.lock().ok()?;
    let (_, screens) = cache.as_ref()?;
    let hit = screens.iter().find(|(s, scale)| {
        let (px, py) = (pos.x / scale, pos.y / scale);
        px >= s.x && px < s.x + s.w && py >= s.y && py < s.y + s.h
    });
    let scale = match hit {
        Some((_, scale)) => *scale,
        None => screens.first()?.1,
    };
    Some((pos.x / scale, pos.y / scale, 0))
}
