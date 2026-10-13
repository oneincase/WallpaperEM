//! Linux 主窗口背景模糊（磨砂质感）
//!
//! Linux 没有跨桌面统一的窗口材质 API（window-vibrancy 在 Linux 是空实现），
//! 模糊只能由合成器完成。这里对接 KDE Plasma (KWin)——主流桌面中唯一原生
//! 支持「应用窗口实时模糊」的合成器（其「模糊/Blur」桌面效果默认开启）：
//!
//! - X11：给主窗口设置 `_KDE_NET_WM_BLUR_BEHIND_REGION` 属性（空区域 = 整窗
//!   模糊，kitty 等应用同款约定），KWin 监听属性变化实时生效；
//! - Wayland：经 GDK 拿到主窗口的 wl_display/wl_surface，**优先**用新协议
//!   `ext_background_effect_manager_v1`（Plasma 6.7+ 才有；旧协议已被 KWin 移除），
//!   bind 失败再回退 `org_kde_kwin_blur`（Plasma ≤6.6 只有它）。协议对接全部在
//!   `protocols/ext_background_effect.rs`，本文件只负责 GDK 侧的指针包装与日志。
//!
//! 其他合成器的降级行为（不报错、不影响功能，界面保持半透明）：
//! - GNOME (Mutter)：无窗口模糊能力（Blur my Shell 扩展只模糊 Shell 自身组件）；
//! - Hyprland / wlroots：走合成器侧 windowrule/layerrule，由用户配置；
//! - 无 KWin 模糊效果（X11 下被用户关闭；Wayland 下 KWin 不声明 blur capability）：
//!   属性/区域无害残留，不生效。
//!
//! 触发点与 macOS vibrancy 共用：setup 一次 + 主窗口回收后重建的那一次
//! （见 lib.rs apply_vibrancy）。

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use gtk::prelude::*;
use tauri::{AppHandle, Manager};

/// Wayland 侧协议对接（ext_background_effect_v1 + org_kde_kwin_blur 回退）。
/// 单独成文件且不依赖 gtk/tauri，便于脱离 GUI 单独做类型检查。
#[path = "protocols/ext_background_effect.rs"]
mod ext_background_effect;

// GDK 的 X11/Wayland 后端符号（libgdk-3 已随 gtk 依赖链接，直接 FFI 取用，
// 免去 gdkx11/gdkwayland crate 的版本对齐）
extern "C" {
    fn gdk_x11_window_get_xid(window: *mut c_void) -> std::ffi::c_ulong;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut c_void;
    fn gdk_wayland_window_get_wl_surface(window: *mut c_void) -> *mut c_void;
}

/// 已经成功请求过模糊的 wl_surface 指针（0 = 尚未请求过任何 surface）。
///
/// 幂等的必要性：`ext_background_effect_manager_v1.get_background_effect` 规定一个
/// surface 只能有一个效果对象，重复调用是 `background_effect_exists` 协议错误，
/// 而协议错误会直接断掉整条 wl_display 连接（不是本模块能 catch 的 Err）。而效果
/// 对象按设计不显式销毁（见 protocols/ext_background_effect.rs），所以「同一
/// surface 指针」就等价于「已请求过」。地址被回收后复用给新 surface 的极端情况下
/// 只会漏掉一次模糊，不会报错。
static APPLIED_SURFACE: AtomicUsize = AtomicUsize::new(0);

/// 为主窗口请求合成器模糊（幂等）。窗口未 realize 时尚无 GdkWindow，
/// 挂 connect_realize 等其就绪后自动应用。
pub(crate) fn apply(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else {
        return;
    };
    let gtk_win = match w.gtk_window() {
        Ok(win) => win,
        Err(e) => {
            tracing::warn!("blur: gtk_window() failed: {e}");
            return;
        }
    };
    if gtk_win.is_realized() {
        apply_to(&gtk_win);
    } else {
        gtk_win.connect_realize(apply_to);
    }
}

fn apply_to(gtk_win: &gtk::ApplicationWindow) {
    match gtk_win.display().backend() {
        gtk::gdk::Backend::Wayland => apply_wayland(gtk_win),
        gtk::gdk::Backend::X11 => apply_x11(gtk_win),
        other => tracing::debug!("blur: unsupported GDK backend {other:?}, skipped"),
    }
}

/// X11 + KWin：设置 `_KDE_NET_WM_BLUR_BEHIND_REGION`（空区域 = 整窗模糊）。
/// KWin 的 Blur 桌面效果监听该属性，窗口已 map 后设置同样即时生效。
fn apply_x11(gtk_win: &gtk::ApplicationWindow) {
    use gtk::glib::object::ObjectType;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;

    let Some(gdk_win) = gtk_win.window() else {
        return;
    };
    let xid = unsafe { gdk_x11_window_get_xid(gdk_win.as_ptr() as *mut c_void) } as u32;
    if xid == 0 {
        tracing::debug!("blur: X11 xid is 0, skipped");
        return;
    }
    let res = (|| -> Result<(), Box<dyn std::error::Error>> {
        // 另起一条到同一 X server 的连接即可设置属性（X 协议天然跨客户端）。
        // 窗口生命周期内设置一次足够；本进程退出时连接断开，属性随窗口销毁清理。
        let (conn, _screen) = x11rb::connect(None)?;
        let atom = conn
            .intern_atom(false, b"_KDE_NET_WM_BLUR_BEHIND_REGION")?
            .reply()?
            .atom;
        conn.change_property32(PropMode::REPLACE, xid, atom, AtomEnum::CARDINAL, &[])?;
        conn.flush()?;
        Ok(())
    })();
    match res {
        Ok(()) => tracing::info!("blur: X11 _KDE_NET_WM_BLUR_BEHIND_REGION set (xid={xid})"),
        Err(e) => tracing::warn!("blur: X11 blur request failed: {e}"),
    }
}

/// Wayland + KWin 的背景模糊请求。
///
/// 三种结果各有可区分的日志（验收要求）：
/// - info `blur: wayland 已启用（新协议 ext_background_effect_manager_v1…）`
/// - info `blur: wayland 已启用（旧协议 org_kde_kwin_blur 回退…）`（Plasma ≤6.6）
/// - debug `blur: wayland blur unavailable, degraded silently (non-KWin?): …`（非 KWin / 两者都没有）
///
/// 整个协议交互都在 catch_unwind 里：foreign display/proxy 包装出现内部 panic
///（如版本不匹配断言）也只丢模糊效果，绝不带崩主线程。
fn apply_wayland(gtk_win: &gtk::ApplicationWindow) {
    use gtk::glib::object::ObjectType;
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::protocol::wl_surface::WlSurface;
    use wayland_client::{Connection, Proxy};

    // 两个协议都是 KWin 专有/扩展协议，只在 KDE 会话尝试；KDE 判定统一走
    // kde.rs（它已缓存 XDG_CURRENT_DESKTOP 探测结果并带 D-Bus 二次确认）。
    // 非 KDE 会话连 bind 都不做，避免无谓的协议交互（bind 失败也只是静默降级）
    let kde = crate::kde::env();
    if !kde.is_kde {
        tracing::debug!("blur: wayland 跳过（非 KDE 会话，{}）", kde.summary());
        return;
    }

    let Some(gdk_win) = gtk_win.window() else {
        return;
    };
    let display = gtk_win.display();
    let wl_display =
        unsafe { gdk_wayland_display_get_wl_display(display.as_ptr() as *mut c_void) };
    let wl_surface = unsafe { gdk_wayland_window_get_wl_surface(gdk_win.as_ptr() as *mut c_void) };
    if wl_display.is_null() || wl_surface.is_null() {
        tracing::debug!("blur: wayland display/surface is null, skipped");
        return;
    }
    if APPLIED_SURFACE.load(Ordering::Acquire) == wl_surface as usize {
        tracing::debug!("blur: wayland surface 已请求过模糊，幂等跳过");
        return;
    }

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<ext_background_effect::Outcome, Box<dyn std::error::Error>> {
            // SAFETY：两个指针均来自存活中的 GDK 对象，且 display 生命周期由 GDK 持有
            //（foreign 语义，wayland-client 不会在 drop 时关闭它）。
            let backend = unsafe { Backend::from_foreign_display(wl_display.cast()) };
            let conn = Connection::from_backend(backend);
            // SAFETY：wl_surface 是同一 display 上由 GDK 创建的有效 wl_proxy；
            // from_ptr 标记为 foreign，本端 drop 不会销毁 GDK 的 surface。
            let surface_id =
                unsafe { ObjectId::from_ptr(WlSurface::interface(), wl_surface.cast()) }?;
            let surface = WlSurface::from_id(&conn, surface_id)?;
            ext_background_effect::request(&conn, &surface)
        },
    ));

    match res {
        Ok(Ok(outcome)) => {
            APPLIED_SURFACE.store(wl_surface as usize, Ordering::Release);
            match outcome.path {
                ext_background_effect::BlurPath::ExtBackgroundEffect => tracing::info!(
                    "blur: wayland 已启用（新协议 ext_background_effect_manager_v1，{}，blur_capability={:?}）",
                    kde.summary(),
                    outcome.blur_capable
                ),
                ext_background_effect::BlurPath::PlasmaLegacy => tracing::info!(
                    "blur: wayland 已启用（旧协议 org_kde_kwin_blur 回退，{}，新协议不可用原因: {}）",
                    kde.summary(),
                    outcome.fallback_reason.as_deref().unwrap_or("unknown")
                ),
            }
        }
        // debug 而非 warn：非 KWin Wayland 合成器（或两端协议都没有）必然走到这里，
        // 属预期降级，不是错误
        Ok(Err(e)) => {
            tracing::debug!("blur: wayland blur unavailable, degraded silently (non-KWin?): {e}")
        }
        Err(_) => tracing::warn!("blur: wayland blur panicked internally, skipped"),
    }
}
