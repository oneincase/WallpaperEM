//! Linux 主窗口背景模糊（磨砂质感）
//!
//! Linux 没有跨桌面统一的窗口材质 API（window-vibrancy 在 Linux 是空实现），
//! 模糊只能由合成器完成。这里对接 KDE Plasma (KWin)——主流桌面中唯一原生
//! 支持「应用窗口实时模糊」的合成器（其「模糊/Blur」桌面效果默认开启）：
//!
//! - X11：给主窗口设置 `_KDE_NET_WM_BLUR_BEHIND_REGION` 属性（空区域 = 整窗
//!   模糊，kitty 等应用同款约定），KWin 监听属性变化实时生效；
//! - Wayland：经 GDK 拿到主窗口的 wl_display/wl_surface，通过 KWin 专有协议
//!   `org_kde_kwin_blur_manager` 为该 surface 创建 blur 并 commit。
//!
//! 其他合成器的降级行为（不报错、不影响功能，界面保持半透明）：
//! - GNOME (Mutter)：无窗口模糊能力（Blur my Shell 扩展只模糊 Shell 自身组件）；
//! - Hyprland / wlroots：走合成器侧 windowrule/layerrule，由用户配置；
//! - 无 KWin 模糊效果（X11 下被用户关闭）：属性无害残留，不生效。
//!
//! 触发点与 macOS vibrancy 共用：setup 一次 + 主窗口闲置释放重建后一次
//! （见 lib.rs apply_vibrancy）。

use std::ffi::c_void;

use gtk::prelude::*;
use tauri::{AppHandle, Manager};

// GDK 的 X11/Wayland 后端符号（libgdk-3 已随 gtk 依赖链接，直接 FFI 取用，
// 免去 gdkx11/gdkwayland crate 的版本对齐）
extern "C" {
    fn gdk_x11_window_get_xid(window: *mut c_void) -> std::ffi::c_ulong;
    fn gdk_wayland_display_get_wl_display(display: *mut c_void) -> *mut c_void;
    fn gdk_wayland_window_get_wl_surface(window: *mut c_void) -> *mut c_void;
}

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

/// Wayland + KWin：`org_kde_kwin_blur_manager.create(surface)` + `commit`。
/// 复用 GDK 已建立的 wl_display 连接（use_system_lib 后端），只发送请求不做
/// 事件分发——blur_manager/blur 接口均无事件，proxy 丢弃后合成器侧 blur 仍
/// 随 surface 生命周期保留。
fn apply_wayland(gtk_win: &gtk::ApplicationWindow) {
    // org_kde_kwin_blur 是 KWin 专有协议，只在 KDE 会话尝试，避免在
    // GNOME/wlroots 会话做无谓的协议交互（bind 会走 Err 降级，但跳过更干净）
    let de = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !de.split(':').any(|t| t.trim().eq_ignore_ascii_case("kde")) {
        tracing::debug!("blur: XDG_CURRENT_DESKTOP={de:?} 非 KDE，跳过 wayland blur");
        return;
    }
    use gtk::glib::object::ObjectType;
    use wayland_client::backend::{Backend, ObjectId};
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols_plasma::blur::client::org_kde_kwin_blur::OrgKdeKwinBlur;
    use wayland_protocols_plasma::blur::client::org_kde_kwin_blur_manager::OrgKdeKwinBlurManager;

    struct BlurState;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for BlurState {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    wayland_client::delegate_noop!(BlurState: OrgKdeKwinBlurManager);
    wayland_client::delegate_noop!(BlurState: OrgKdeKwinBlur);

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

    // 模糊是纯装饰：这段外部协议互操作（foreign display/proxy 包装）出现任何
    // 内部 panic（如版本不匹配断言）都只应丢失模糊效果，绝不能带崩主线程
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
        || -> Result<(), Box<dyn std::error::Error>> {
            // SAFETY：两个指针均来自存活中的 GDK 对象，且 display 生命周期由 GDK 持有
            //（foreign 语义，wayland-client 不会在 drop 时关闭它）。
            let backend = unsafe { Backend::from_foreign_display(wl_display.cast()) };
            let conn = Connection::from_backend(backend);
            // SAFETY：wl_surface 是同一 display 上由 GDK 创建的有效 wl_proxy；
            // from_ptr 标记为 foreign，本端 drop 不会销毁 GDK 的 surface。
            let surface_id =
                unsafe { ObjectId::from_ptr(WlSurface::interface(), wl_surface.cast()) }?;
            let surface = WlSurface::from_id(&conn, surface_id)?;

            let (globals, queue) = registry_queue_init::<BlurState>(&conn)?;
            let qh = queue.handle();
            // KWin 专有接口；其他合成器的 registry 里没有它，bind 直接报错走降级。
            // 版本范围必须 ≤ XML 定义的接口版本（v1），请求更高版本会 panic 而非返回 Err
            let manager: OrgKdeKwinBlurManager = globals.bind(&qh, 1..=1, ())?;
            let blur = manager.create(&surface, &qh, ());
            blur.commit();
            conn.flush()?;
            Ok(())
        },
    ));
    match res {
        Ok(Ok(())) => tracing::info!("blur: wayland org_kde_kwin_blur committed"),
        // debug 而非 warn：非 KWin Wayland 合成器必然走到这里，属预期降级
        Ok(Err(e)) => tracing::debug!("blur: wayland blur unavailable (non-KWin?): {e}"),
        Err(_) => tracing::warn!("blur: wayland blur panicked internally, skipped"),
    }
}
