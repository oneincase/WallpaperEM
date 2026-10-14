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
//! - **前台应用观察者 / 自动暂停**：X11 下经 [`frontmost_kind`] 读 EWMH
//!   `_NET_ACTIVE_WINDOW` 分类（桌面/面板 → 播放，自己 → 播放，别的应用 → 暂停）；
//!   Wayland 没有「当前活动窗口」的全局概念，恒 `Unknown`（判定表恒 Keep，
//!   不改变播放状态）。自动暂停的另一判据是**遮挡快照**
//!   （[`occlusion_snapshot`]，X11 会话有效），Wayland 会话无法枚举全局窗口清单，
//!   开关同样不生效（详见该函数说明）。
//! - **显示器睡眠**：无统一查询接口（logind/DPMS 各管一段），恒返回 false，
//!   即不做「合盖暂停/释放壁纸」的自动处理。
//! - **窗口抓帧**（[`capture_wallpaper_window_rgba`]）：Linux/WebKitGTK 上页面侧
//!   自抓帧（`drawImage(glCanvas).toDataURL`）返回**全黑**，同一窗口用 X11
//!   `GetImage` 抓到的却是真实像素（实测平均亮度 192），且窗口设
//!   `_NET_WM_WINDOW_OPACITY=0` 之后 X11 侧仍能抓到完整内容 —— 所以帧导出在
//!   X11 会话改走「抓窗口像素」而不是读页面画布。**仅 X11**：Wayland 没有
//!   `GetImage` 等价的全窗口读回，待 KWin `org.kde.KWin.ScreenShot2`
//!   （`CaptureWindow`）接口补上，在此之前 Wayland 会话调用即报错、由调用方
//!   回退页面自抓帧。

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
    // 空列表**不进缓存**：刚启动那一瞬 GTK/tao 还枚举不出显示器（实测 2026-10-14 验证机上
    // 启动后约 1s 内 `available_monitors()` 返回空），把它缓存 1.5s 会让这期间的每个命令都
    // 拿不到屏 —— `wallpaper_apply` 会直接报「未找到目标显示器」。不缓存空结果，下一次调用
    // 立即重查，屏一出现就能用。
    if !out.is_empty() {
        if let Ok(mut cache) = SCREENS_CACHE.lock() {
            *cache = Some((Instant::now(), out.clone()));
        }
    }
    out.into_iter().map(|(s, _)| s).collect()
}

/// 光标所在的显示器 id（「当前屏幕」）：[`cursor_state`] 已是逻辑坐标（GTK 的
/// 物理坐标按命中屏 scale 换算），与 [`hit_screen_id`] 吃的 ScreenInfo 逻辑矩形同系。
pub fn cursor_display_id() -> Option<u32> {
    let (x, y, _) = cursor_state()?;
    hit_screen_id(x, y)
}

/// 全局逻辑坐标点落在哪台显示器（滚轮派发用；Linux 暂无系统滚轮源，备平台接口一致）。
#[allow(dead_code)]
pub fn hit_screen_id(x: f64, y: f64) -> Option<u32> {
    active_screens()
        .into_iter()
        .find(|s| x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h)
        .map(|s| s.id)
}

/// 主窗口中心点（逻辑坐标、左上原点），供 [`hit_screen_id`]（ScreenInfo 逻辑矩形）。
#[allow(dead_code)]
pub fn window_hit_point<R: Runtime>(window: &WebviewWindow<R>) -> Option<(f64, f64)> {
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    let scale = window.scale_factor().ok().unwrap_or(1.0);
    Some((
        (pos.x as f64 + size.width as f64 / 2.0) / scale,
        (pos.y as f64 + size.height as f64 / 2.0) / scale,
    ))
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

/// 主显示器是否睡眠。
///
/// 只在确认是 KDE 会话时才去问会话总线（`org.freedesktop.ScreenSaver`）：其他 DE 上
/// 这个接口不一定有人实现，问了也只是白等一次超时。查询带 2 秒缓存（见
/// [`crate::kwin::idle_seconds`]），取不到就按「没睡」处理——宁可继续播放，也不要
/// 因为一次查询失败把壁纸误停（自动暂停默认还是关的，见 `wallpaper_auto_pause`）。
pub fn display_asleep() -> bool {
    if !crate::kde::env().is_kde {
        return false;
    }
    matches!(crate::kwin::idle_seconds(), Some(u64::MAX))
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
    // 判定统一收口到 kde 模块（能力路由 P0）：它同时回答「是不是 KDE」和
    // 「XDG_CURRENT_DESKTOP 里的覆盖层式图标 DE 白名单」。非 KDE 的白名单
    // 语义与改动前逐字一致（GNOME/Budgie/Pantheon 为真，其余为假）。
    let env = crate::kde::env_snapshot();
    let kde = crate::kde::env();
    let overlay = crate::kde::shell_icons_are_overlay_with(&env, kde);
    if !overlay {
        let de = env.get("XDG_CURRENT_DESKTOP").map(String::as_str).unwrap_or("");
        tracing::debug!(
            "linux: XDG_CURRENT_DESKTOP={de:?} kde={} 非覆盖层式图标 DE，跳过壁纸沉底（避免被桌面背景窗口遮住）",
            kde.is_kde
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

/// 把本进程的桌面层壁纸窗口整窗设为指定不透明度，返回被改动的窗口数。
///
/// 属性是 EWMH 的 `_NET_WM_WINDOW_OPACITY`（32 位 CARDINAL，`0` = 全透明、
/// `0xFFFFFFFF` = 完全不透明），compositor（KWin/Mutter…）合成时按它调 alpha。
///
/// **这是 Plasma 壁纸插件模式的前置条件**（2026-10-14 在真 KWin 上实测）：
/// 插件负责把引擎导出的帧画到桌面上，引擎自己的窗口就必须看不见 —— 但**不能**
/// 用「隐藏 / 取消映射」实现：WebKitGTK 会把不可见窗口的 rAF 停掉，抓到的帧就
/// 冻结在最后一帧（同一场景、同一 1080p 窗口，unmap 后 7 秒 140 帧里 139 帧
/// 逐字节相同）；保持映射 + 整窗透明则照常出帧（同 7 秒 83 帧全部有变化）。
///
/// X11 专用：Wayland 下壁纸窗口是原生 surface，没有这个属性可设，返回 `Ok(0)`
/// 并记一条 debug 日志（插件模式在 Wayland 下仍需别的机制，见
/// `docs/kde-adaptation-plan.md` 的开放问题）。
pub fn set_wallpaper_windows_opacity(opacity: f64) -> Result<usize, String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;

    let force_x11 = std::env::var("GDK_BACKEND").is_ok_and(|b| b == "x11");
    if !force_x11 && std::env::var("WAYLAND_DISPLAY").is_ok() {
        tracing::debug!("linux: Wayland 会话不支持整窗透明（插件模式另需机制）");
        return Ok(0);
    }

    // 窗口筛选（winclist 取法 + pid/type 过滤）收口到 our_desktop_windows，
    // 与窗口抓帧共用一套条件；这里只要 id。
    let windows: Vec<u32> = our_desktop_windows()?
        .into_iter()
        .map(|(w, _, _)| w)
        .collect();

    let (conn, _screen_num) = x11rb::connect(None).map_err(|e| format!("连不上 X11：{e}"))?;
    let intern = |name: &[u8]| -> Option<u32> {
        Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
    };
    let opacity_atom = intern(b"_NET_WM_WINDOW_OPACITY").ok_or("没有 _NET_WM_WINDOW_OPACITY")?;

    let value = opacity_to_cardinal(opacity);
    let mut changed = 0usize;
    for w in &windows {
        if let Err(e) = conn.change_property32(
            PropMode::REPLACE,
            *w,
            opacity_atom,
            AtomEnum::CARDINAL,
            &[value],
        ) {
            return Err(format!("写 _NET_WM_WINDOW_OPACITY 失败：{e}"));
        }
        changed += 1;
    }
    let _ = conn.flush();

    // 读回校验：`change_property32` 返回的是 VoidCookie，X 的错误是**异步**的 ——
    // 窗口已经没了、请求被拒，调用方都只会看到“成功”。插件模式完全押在这个属性上，
    // 静默失败的两头都是坏结果（该透明时没透明 = 引擎窗口盖住插件；该恢复时没恢复 =
    // 用户看到「壁纸不见了」），所以这里逐窗读回，对不上就记 warn。
    let mut mismatched = 0usize;
    for w in &windows {
        let got = conn
            .get_property(false, *w, opacity_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut it| it.next()));
        if got != Some(value) {
            mismatched += 1;
            tracing::warn!(
                "linux: 窗口 {w:#x} 不透明度写回不一致：期望 {value}，实际 {got:?}"
            );
        }
    }
    if changed > 0 {
        tracing::debug!("linux: 已把 {changed} 扇窗口的不透明度写成 {value}（不一致 {mismatched}）");
    }
    Ok(changed)
}

/// 本进程的桌面层窗口清单（X11），逐项给出 `(window_id, width, height)`。
///
/// 全文件唯一的「枚举本进程 DESKTOP 窗口」实现，[`set_wallpaper_windows_opacity`] 与
/// [`find_wallpaper_window`] 共用，避免两处各写一套过滤条件而后悄悄分叉：
/// - 顶层窗口清单取 `_NET_CLIENT_LIST_STACKING`（WM 维护的堆叠序），缺失或为空时
///   退回 `_NET_CLIENT_LIST`（无序，但内容一致）；
/// - 只留 `_NET_WM_PID == std::process::id()` 且 `_NET_WM_WINDOW_TYPE` 含
///   `_NET_WM_WINDOW_TYPE_DESKTOP` 的窗口（本进程的普通窗口/设置窗被排除）。
///
/// 刻意**不过滤映射状态与尺寸**：调用方语义不同 —— 整窗透明对未映射窗口也要设属性
/// （窗口可能还没映射，属性得先写好），而抓帧要求窗口可见且够大。任一属性读不到就
/// 跳过该窗口（当作「不是我们的桌面窗」），不因此整体失败。
fn our_desktop_windows() -> Result<Vec<(u32, u16, u16)>, String> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Window};

    let (conn, screen_num) = x11rb::connect(None).map_err(|e| format!("连不上 X11：{e}"))?;
    let root = conn.setup().roots[screen_num].root;
    let intern = |name: &[u8]| -> Option<u32> {
        Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
    };

    // 顶层窗口清单（没有 EWMH 的 WM 罕见；两代属性都没有才算失败）
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
    let windows: Vec<Window> = match intern(b"_NET_CLIENT_LIST_STACKING")
        .and_then(&prop)
        .filter(|v| !v.is_empty())
    {
        Some(v) => v,
        None => {
            let list = intern(b"_NET_CLIENT_LIST").ok_or("没有 _NET_CLIENT_LIST")?;
            prop(list).ok_or("读不到窗口清单")?
        }
    };

    let pid_atom = intern(b"_NET_WM_PID").ok_or("没有 _NET_WM_PID")?;
    let type_atom = intern(b"_NET_WM_WINDOW_TYPE").ok_or("没有 _NET_WM_WINDOW_TYPE")?;
    let type_desktop =
        intern(b"_NET_WM_WINDOW_TYPE_DESKTOP").ok_or("没有 _NET_WM_WINDOW_TYPE_DESKTOP")?;

    let mut out = Vec::new();
    for w in windows {
        let pid = conn
            .get_property(false, w, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut it| it.next()));
        if pid != Some(std::process::id()) {
            continue;
        }
        let wtype: Vec<u32> = conn
            .get_property(false, w, type_atom, AtomEnum::ATOM, 0, 8)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().map(|it| it.collect()))
            .unwrap_or_default();
        if !wtype.contains(&type_desktop) {
            continue;
        }
        // 几何拿不到就按 0x0 记（抓帧会因「太小」拒绝；整窗透明不需要尺寸）
        let (w_, h_) = conn
            .get_geometry(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|g| (g.width, g.height))
            .unwrap_or((0, 0));
        out.push((w, w_, h_));
    }
    Ok(out)
}

/// 抓帧的最小窗口边长（像素）：比这更小的不可能是壁纸窗口，直接判失败而不是
/// 抓出一张 1x1 的垃圾帧去覆盖整屏（调用方会回退页面自抓帧）。
const MIN_CAPTURE_SIDE: u16 = 16;

/// 找到本进程的壁纸窗口，返回 `(window_id, width, height)`。
///
/// 桌面窗口理论上每个显示器一扇（`wallpaper-<displayId>`），但可能有还没销毁的旧
/// 窗口或辅助窗口，所以按**面积最大**挑：壁纸窗口必然铺满一块屏，是最大的一扇。
///
/// `Ok(None)` = 一扇本进程的桌面窗口都没有（窗口还没建出来、或系统壁纸窗口不归
/// 本进程）——这是「暂不可用」而非错误，调用方回退即可。连接失败/没有 EWMH 属性
/// 才返回 `Err`。尺寸小于 [`MIN_CAPTURE_SIDE`] 的窗口不参与选择；若全部都不够大，
/// 也返回 `Ok(None)`（同样交给调用方回退）。
pub fn find_wallpaper_window() -> Result<Option<(u32, u16, u16)>, String> {
    Ok(our_desktop_windows()?
        .into_iter()
        .filter(|(_, w, h)| *w >= MIN_CAPTURE_SIDE && *h >= MIN_CAPTURE_SIDE)
        .max_by_key(|(_, w, h)| (*w as u64) * (*h as u64)))
}

/// 用 X11 `GetImage` 抓本进程壁纸窗口的像素，返回 `(RGBA8, width, height)`。
///
/// 为什么走窗口而不是读页面画布：见文件头「窗口抓帧」一段（WebKitGTK 的
/// `toDataURL` 全黑，X11 后备存储里是真内容，且整窗透明后内容仍在）。
///
/// 说清楚的三个边界：
/// - **仅 X11**：`WAYLAND_DISPLAY` 且未强制 `GDK_BACKEND=x11` 时直接报错（Wayland
///   没有 `GetImage` 等价物，待 KWin `ScreenShot2`），调用方回退页面自抓帧；
/// - **depth 只支持 24/32**：都按 32bpp 解（X server 的 ZPixmap 把 24 位像素也放在
///   4 字节里，最高字节是填充/alpha），其他 depth（如 16 位 565）返回 Err 而不是
///   猜字节序把颜色抓错；
/// - **像素序**：`GetImage` 给的是服务器字节序的 BGRX，转换见 [`bgrx_to_rgba`]。
///
/// 抓不到像素的原因（窗口未映射、被合成器接管、BadMatch…）由 GetImage 的
/// `ReplyError` 变成带上下文的中文 Err，而不是 panic。
pub fn capture_wallpaper_window_rgba() -> Result<(Vec<u8>, u16, u16), String> {
    #[cfg(not(target_os = "linux"))]
    {
        // 该模块只在 Linux 编译（见 mod.rs 的 `#[cfg(target_os = "linux")]`），这里
        // 是为「有人误把模块放开到全平台」时仍能优雅失败而留的分包（同任务要求）。
        Err("仅 X11/Linux 支持窗口抓帧".into())
    }
    #[cfg(target_os = "linux")]
    {
        // `get_image`/`get_property` 等都在 `xproto::ConnectionExt` 上；
        // 这里没有 `flush`，所以不需要 `x11rb::connection::Connection`。
        use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

        let force_x11 = std::env::var("GDK_BACKEND").is_ok_and(|b| b == "x11");
        if !force_x11 && std::env::var("WAYLAND_DISPLAY").is_ok() {
            return Err("Wayland 会话不支持窗口抓帧（待 KWin ScreenShot2）".into());
        }

        let (wid, w, h) =
            find_wallpaper_window()?.ok_or("未找到本进程的壁纸窗口（桌面窗口还没建出来？）")?;
        if w < MIN_CAPTURE_SIDE || h < MIN_CAPTURE_SIDE {
            return Err(format!("壁纸窗口太小，拒绝抓帧：{w}x{h}"));
        }

        let (conn, _screen_num) = x11rb::connect(None).map_err(|e| format!("连不上 X11：{e}"))?;

        // X11 0.13 的实参顺序是 (format, drawable, x, y, w, h, plane_mask)；
        // drawable 是 `type Drawable = u32` 的别名，窗口 id 直接传（没有 Drawable::Window）。
        // plane_mask 用 u32::MAX = 全部平面；depth 从 reply 读，用来决定行跨度/合法性。
        let reply = conn
            .get_image(ImageFormat::Z_PIXMAP, wid, 0, 0, w, h, u32::MAX)
            .map_err(|e| format!("GetImage 请求失败：{e}"))?
            .reply()
            .map_err(|e| format!("GetImage 无回复：{e}"))?;

        if reply.depth != 24 && reply.depth != 32 {
            return Err(format!(
                "壁纸窗口色深 {} 不支持（仅 24/32 bpp）",
                reply.depth
            ));
        }

        // depth 24/32 的 ZPixmap 每像素 4 字节；X 的行跨度是 4 字节对齐，这里 (w*4+3)/4*4
        // 恒等于 w*4（w: u16 ⇒ w*4 已是 4 的倍数），但保留通用写法，单测覆盖了非整除步长。
        let bytes_per_line = (w as usize * 4 + 3) / 4 * 4;
        let need = bytes_per_line
            .checked_mul(h as usize)
            .ok_or("窗口尺寸溢出（宽×高过大）")?;
        if reply.data.len() < need {
            return Err(format!(
                "GetImage 数据不完整：{} 字节 < 期望 {} 字节（{w}x{h} depth {}）",
                reply.data.len(),
                need,
                reply.depth
            ));
        }

        Ok((bgrx_to_rgba(&reply.data, w, h, bytes_per_line), w, h))
    }
}

/// BGRX(ZPixmap) → RGBA8。**平台无关纯函数**（不带 cfg，宿主机也能跑单测）。
///
/// X server 的 ZPixmap 每像素 4 字节、按服务器字节序排成 `B,G,R,X`（X 是填充；
/// depth 32 时是 alpha，但本用法下窗口内容恒不透明，输出 alpha 一律 255）。
/// `bytes_per_line` 由调用方按 X 的行跨度给（≥ `w*4`），因此这里必须**按 stride
/// 跳行**而不是假定紧凑排列 —— 少了这一步，任何一行跨度的差异都会把图像斜切。
///
/// 越界一律**跳过**而不是 panic：缓冲区来自 X server，长度不由我们保证，宁可输出
/// 一张尾部有洞的图（上层会发现异常）也不要让帧导出线程崩掉。
fn bgrx_to_rgba(data: &[u8], w: u16, h: u16, bytes_per_line: usize) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut rgba = vec![0u8; w * h * 4];
    for row in 0..h {
        let row_start = row * bytes_per_line;
        for col in 0..w {
            let s = row_start + col * 4;
            // 需要 s..s+4 四个字节；不足则本行剩余像素全部跳过
            let Some(px) = data.get(s..s + 4) else {
                break;
            };
            let d = (row * w + col) * 4;
            rgba[d] = px[2]; // R：BGRX 的第三字节
            rgba[d + 1] = px[1]; // G
            rgba[d + 2] = px[0]; // B
            rgba[d + 3] = 255; // X/alpha → 不透明
        }
    }
    rgba
}

/// 不透明度 → `_NET_WM_WINDOW_OPACITY` 的 32 位值（EWMH：`0xFFFFFFFF` = 完全不透明）。
/// 越界输入截到 `0..=1`，NaN 按 0 处理 —— 宁可透过去，也不要因为一个坏值把壁纸
/// 钉成不透明、把插件模式悄悄变成「引擎窗口盖住插件」。
fn opacity_to_cardinal(opacity: f64) -> u32 {
    if opacity.is_nan() || opacity <= 0.0 {
        return 0;
    }
    if opacity >= 1.0 {
        return u32::MAX;
    }
    (opacity * u32::MAX as f64).round() as u32
}

/// 前台窗口分类的纯逻辑：把「已解析出来的 X11 属性」映射成 [`FrontKind`]。
///
/// 判定顺序刻意与 macOS 后端一致（Finder → 桌面；自己 → SelfApp）：
/// 1. 拿不到 `_NET_ACTIVE_WINDOW` → `Unknown`（判定表恒 `Keep`，不改变播放状态）；
/// 2. 前台窗口是桌面（`_NET_WM_WINDOW_TYPE_DESKTOP`）或面板/程序坞
///    （`_NET_WM_WINDOW_TYPE_DOCK`）→ `Desktop`（壁纸露着，应播放）；
/// 3. 前台窗口属于本进程 → `SelfApp`（设置窗/壁纸窗在最前，不应因「被盖住」而暂停）；
/// 4. 其余 → `Other`（真应用在前台，按「看不见就暂停」处理）。
///
/// `active_pid` 取不到时归 `Other` 而不是 `SelfApp`：宁可少播也不要漏暂停。
fn classify_frontmost(
    has_active: bool,
    active_pid: Option<u32>,
    our_pid: u32,
    is_desktop: bool,
    is_dock: bool,
) -> super::auto_pause::FrontKind {
    use super::auto_pause::FrontKind;
    if !has_active {
        return FrontKind::Unknown;
    }
    if is_desktop || is_dock {
        return FrontKind::Desktop;
    }
    match active_pid {
        Some(pid) if pid == our_pid => FrontKind::SelfApp,
        _ => FrontKind::Other,
    }
}

/// 前台应用类型（Linux/X11）：读 EWMH 的 `_NET_ACTIVE_WINDOW`，再看该窗口的
/// `_NET_WM_PID` 与 `_NET_WM_WINDOW_TYPE`。分类逻辑见 [`classify_frontmost`]。
///
/// **Wayland 会话返回 `Unknown`**：协议没有「当前活动窗口」的全局概念
/// （KWin 的 `org.kde.KWin` D-Bus 在 5.27 上也不提供查询，KWin 脚本 API 才有
/// `workspace.activeClient`，但它要求注入脚本、且不是稳定接口）。显式
/// `GDK_BACKEND=x11`（XWayland 会话）不受此限。失败一律回 `Unknown`，
/// 不抛错、不阻塞。
pub fn frontmost_kind(_app: &tauri::AppHandle) -> super::auto_pause::FrontKind {
    use super::auto_pause::FrontKind;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};

    let force_x11 = std::env::var("GDK_BACKEND").is_ok_and(|b| b == "x11");
    if !force_x11 && std::env::var("WAYLAND_DISPLAY").is_ok() {
        return FrontKind::Unknown;
    }
    let Some((conn, screen_num)) = x11rb::connect(None).ok() else {
        return FrontKind::Unknown;
    };
    let root = conn.setup().roots[screen_num].root;
    let intern = |name: &[u8]| -> Option<u32> {
        Some(conn.intern_atom(false, name).ok()?.reply().ok()?.atom)
    };
    let Some(active_atom) = intern(b"_NET_ACTIVE_WINDOW") else {
        return FrontKind::Unknown;
    };
    let prop = |window, atom, kind: AtomEnum, len: u32| -> Option<Vec<u32>> {
        Some(
            conn.get_property(false, window, atom, kind, 0, len)
                .ok()?
                .reply()
                .ok()?
                .value32()?
                .collect(),
        )
    };
    // 活动窗口 id（WM 会把它写成 0 表示「无前台」）
    let active = prop(root, active_atom, AtomEnum::WINDOW, 1)
        .and_then(|v| v.first().copied())
        .filter(|w| *w != 0);
    let Some(active) = active else {
        return FrontKind::Unknown;
    };
    let active_pid = intern(b"_NET_WM_PID")
        .and_then(|atom| prop(active, atom, AtomEnum::CARDINAL, 1))
        .and_then(|v| v.first().copied());
    let is_desktop = intern(b"_NET_WM_WINDOW_TYPE_DESKTOP")
        .zip(intern(b"_NET_WM_WINDOW_TYPE"))
        .and_then(|(desktop, kind)| {
            prop(active, kind, AtomEnum::ATOM, 8).map(|types| types.contains(&desktop))
        })
        .unwrap_or(false);
    let is_dock = intern(b"_NET_WM_WINDOW_TYPE_DOCK")
        .zip(intern(b"_NET_WM_WINDOW_TYPE"))
        .and_then(|(dock, kind)| {
            prop(active, kind, AtomEnum::ATOM, 8).map(|types| types.contains(&dock))
        })
        .unwrap_or(false);
    classify_frontmost(true, active_pid, std::process::id(), is_desktop, is_dock)
}

#[cfg(test)]
mod frontmost_tests {
    use super::classify_frontmost;
    use super::super::auto_pause::FrontKind;

    const OURS: u32 = 4242;

    #[test]
    fn no_active_window_is_unknown() {
        assert_eq!(
            classify_frontmost(false, None, OURS, false, false),
            FrontKind::Unknown
        );
    }

    #[test]
    fn desktop_and_dock_are_desktop() {
        assert_eq!(
            classify_frontmost(true, None, OURS, true, false),
            FrontKind::Desktop
        );
        assert_eq!(
            classify_frontmost(true, Some(1), OURS, false, true),
            FrontKind::Desktop
        );
    }

    #[test]
    fn desktop_type_wins_over_our_pid() {
        // plasmashell 的桌面视图就属于「我们之外」的进程；即便 pid 巧合相同，
        // 桌面窗口的语义优先（它一在前台就说明壁纸露着）。
        assert_eq!(
            classify_frontmost(true, Some(OURS), OURS, true, false),
            FrontKind::Desktop
        );
    }

    #[test]
    fn our_own_active_window_is_self_app() {
        assert_eq!(
            classify_frontmost(true, Some(OURS), OURS, false, false),
            FrontKind::SelfApp
        );
    }

    #[test]
    fn foreign_window_is_other() {
        assert_eq!(
            classify_frontmost(true, Some(1), OURS, false, false),
            FrontKind::Other
        );
        // pid 读不到时宁可判 Other（少播不漏暂停）
        assert_eq!(
            classify_frontmost(true, None, OURS, false, false),
            FrontKind::Other
        );
    }
}

#[cfg(test)]
mod opacity_tests {
    use super::opacity_to_cardinal;

    #[test]
    fn zero_is_fully_transparent() {
        assert_eq!(opacity_to_cardinal(0.0), 0);
    }

    #[test]
    fn one_and_above_are_fully_opaque() {
        // EWMH：0xFFFFFFFF 才是「不透明」，少一位都会让壁纸半透明
        assert_eq!(opacity_to_cardinal(1.0), u32::MAX);
        assert_eq!(opacity_to_cardinal(1.5), u32::MAX);
    }

    #[test]
    fn negative_or_nan_falls_back_to_transparent() {
        // 坏值必须偏「透」：偏不透明 = 引擎窗口把插件盖住，故障看不出来
        assert_eq!(opacity_to_cardinal(-0.5), 0);
        assert_eq!(opacity_to_cardinal(f64::NAN), 0);
    }

    #[test]
    fn middle_values_scale_monotonically() {
        let quarter = opacity_to_cardinal(0.25);
        let half = opacity_to_cardinal(0.5);
        let three_quarter = opacity_to_cardinal(0.75);
        assert!(quarter < half && half < three_quarter);
        assert!(three_quarter < u32::MAX);
        assert_eq!(half, 0x8000_0000);
    }
}

/// X11 抓帧的像素转换单测：`bgrx_to_rgba` 刻意不带 cfg 门槛，就是为了能在任何
/// 开发机（含 macOS/Windows）上跑。**注意**：本模块 `linux` 整体挂在
/// `#[cfg(target_os = "linux")]` 下（见 mod.rs），所以在非 Linux 宿主上这些测试会被
/// 编译掉、`cargo test --lib` 显示 0 tests（不是失败）；要在这台 macOS 上真跑，得把
/// `bgrx_to_rgba` + 本模块挪进一个不带平台门槛的私有子模块（需改 mod.rs）。
#[cfg(test)]
mod window_capture_tests {
    use super::bgrx_to_rgba;

    /// 每像素 B,G,R,X 的紧缩缓冲（stride = w*4）
    fn px(b: u8, g: u8, r: u8) -> [u8; 4] {
        [b, g, r, 0xFF]
    }

    #[test]
    fn converts_bgrx_rows_to_rgba() {
        // 2x2、无行填充：第 1 行红绿，第 2 行蓝白（X 通道应被丢弃、alpha 恒 255）
        let mut data = Vec::new();
        data.extend_from_slice(&px(0x00, 0x00, 0xFF)); // 红
        data.extend_from_slice(&px(0x00, 0xFF, 0x00)); // 绿
        data.extend_from_slice(&px(0xFF, 0x00, 0x00)); // 蓝
        data.extend_from_slice(&px(0xFF, 0xFF, 0xFF)); // 白

        let rgba = bgrx_to_rgba(&data, 2, 2, 8);
        assert_eq!(
            rgba,
            vec![
                255, 0, 0, 255, // 红
                0, 255, 0, 255, // 绿
                0, 0, 255, 255, // 蓝
                255, 255, 255, 255, // 白
            ]
        );
    }

    #[test]
    fn large_bytes_per_line_skips_row_padding() {
        // 2x1 窗口但 stride = 20（> 8）：第 2 行起点在 20，中间 12 字节填充必须整段跳过
        let mut data = vec![0u8; 40];
        data[0..4].copy_from_slice(&px(0xAA, 0xBB, 0xCC)); // 行 0 像素 0
        data[4..8].copy_from_slice(&px(0x11, 0x22, 0x33)); // 行 0 像素 1
        data[8..20].fill(0xEE); // 行 0 尾部填充（不该出现在输出里）
        data[20..24].copy_from_slice(&px(0x01, 0x02, 0x03)); // 行 1 像素 0
        data[24..28].copy_from_slice(&px(0x04, 0x05, 0x06)); // 行 1 像素 1
        data[28..40].fill(0xEE); // 行 1 尾部填充

        let rgba = bgrx_to_rgba(&data, 2, 2, 20);
        assert_eq!(
            rgba,
            vec![
                0xCC, 0xBB, 0xAA, 255, 0x33, 0x22, 0x11, 255, // 行 0
                0x03, 0x02, 0x01, 255, 0x06, 0x05, 0x04,
                255, // 行 1（填充 0xEE 未混入）
            ]
        );
    }

    #[test]
    fn truncated_data_does_not_panic_and_zero_fills_tail() {
        // 声明 2x2 却只给 1.5 行：不得 panic，缺的像素保持 0
        let mut data = Vec::new();
        data.extend_from_slice(&px(0x00, 0x00, 0xFF)); // 行 0 像素 0 → 红
        data.extend_from_slice(&px(0x00, 0xFF, 0x00)); // 行 0 像素 1 → 绿
        data.extend_from_slice(&[0x10, 0x20, 0x30, 0xFF]); // 行 1 像素 0（截断处）

        let rgba = bgrx_to_rgba(&data, 2, 2, 8);
        assert_eq!(rgba.len(), 16);
        assert_eq!(&rgba[0..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
        assert_eq!(&rgba[8..12], &[0x30, 0x20, 0x10, 255]);
        assert_eq!(&rgba[12..16], &[0, 0, 0, 0]); // 越界像素跳过 → 全 0
    }

    #[test]
    fn empty_buffer_yields_all_transparent_black() {
        // 最极端：X server 一个字节都没给，也必须返回正确长度的全 0 图而不是 panic
        let rgba = bgrx_to_rgba(&[], 3, 2, 12);
        assert_eq!(rgba, vec![0u8; 24]);
    }
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