//! macOS 桌面层窗口与屏幕枚举（T3：多显示器）
//!
//! - 屏幕枚举/睡眠检测用 CoreGraphics C API（CGGetActiveDisplayList/CGDisplayIsAsleep），
//!   避免 objc2 的 NSScreen 复杂遍历；
//! - 窗口置为 kCGDesktopWindowLevel（-2147483623），位于桌面壁纸与图标之间；
//! - 关键修复（T0.5 验证）：orderFrontRegardless + show 后重设层级，否则被遮挡的
//!   WKWebView 动态内容不合成到屏幕。

use std::ffi::c_void;
use tauri::WebviewWindow;

// 层级说明（WindowServer 实际值）：
//   WindowServer 桌面画 = -2147483626
//   程序坞 (Dock)       = -2147483624
//   kCGDesktopWindowLevel = -2147483623（在其之上，会盖住 Dock → Dock 自动隐藏后无法显示）
//
// 层级选择权衡：
//   - kCGDesktopWindowLevel(-2147483623) 下 WKWebView 内容正常合成渲染（T0.5/T3 已验证），
//     但会盖住 Dock（其 backdrop 在 -2147483624）。
//   - -2147483625（桌面画之上、Dock 之下）Dock 正常，但实测壁纸窗口内容不合成（透明/黑），
//     系统动态壁纸透过显示 → 用户看到"软件壁纸被覆盖"。
// 故当前回退 kCGDesktopWindowLevel(-2147483623) 保证壁纸内容渲染；
// Dock 显示通过窗口 collectionBehavior/单独处理（见 apply_desktop_window）。
pub const K_CG_DESKTOP_WINDOW_LEVEL: isize = -2147483623;
#[allow(dead_code)]
pub const K_CG_DESKTOP_ICON_WINDOW_LEVEL: isize = -2147483622; // kCGDesktopIconWindowLevel
/// 「交互壁纸」层级：明显高于桌面/图标（-2147483622 一线），但仍在普通窗口(0)之下。
/// 用于覆盖桌面图标并接收鼠标互动；如某系统桌面图标层级更高仍盖不住，可继续调小（更接近 0）。
pub const K_INTERACTIVE_WALLPAPER_LEVEL: isize = -1000;

// CGRect 的 ABI 兼容结构（与 CoreGraphics 的 CGRect 布局一致：origin+size，各 2×f64）
#[repr(C)]
#[derive(Clone, Copy)]
struct CPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CSize {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CRect {
    origin: CPoint,
    size: CSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGGetActiveDisplayList(max_displays: u32, active_displays: *mut u32, display_count: *mut u32) -> i32;
    fn CGDisplayBounds(display: u32) -> CRect;
    fn CGDisplayIsAsleep(display: u32) -> bool;
}

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 活动显示器列表（points 坐标）
pub fn active_screens() -> Vec<ScreenInfo> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count);
    }
    let mut out = Vec::new();
    for i in 0..(count.min(16) as usize) {
        let id = ids[i];
        let b = unsafe { CGDisplayBounds(id) };
        out.push(ScreenInfo {
            id,
            x: b.origin.x,
            y: b.origin.y,
            w: b.size.width,
            h: b.size.height,
        });
    }
    out
}

/// 主显示器是否睡眠
pub fn display_asleep() -> bool {
    unsafe { CGDisplayIsAsleep(CGMainDisplayID()) }
}

/// 设置窗口 frame（points）
pub fn set_frame(window: &WebviewWindow, x: f64, y: f64, w: f64, h: f64) {
    let ptr = match window.ns_window() {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };
    unsafe {
        if let Some(win) = retain_window(ptr) {
            win.setFrame_display(
                objc2_foundation::NSRect {
                    origin: objc2_foundation::NSPoint { x, y },
                    size: objc2_foundation::NSSize { width: w, height: h },
                },
                true,
            );
        }
    }
}

/// 应用桌面层属性（必须在主线程调用）。
/// `interactive=true` 时把窗口提到桌面图标之上并接收鼠标（可互动，会盖住图标）；
/// `interactive=false`（默认）时保持在图标之下并忽略鼠标，桌面图标/点击正常。
pub fn apply_desktop_window(
    window: &WebviewWindow,
    frame: (f64, f64, f64, f64),
    interactive: bool,
) {
    let ptr = match window.ns_window() {
        Ok(p) if !p.is_null() => p,
        Ok(_) => {
            tracing::warn!("apply_desktop_window: ns_window is null");
            return;
        }
        Err(e) => {
            tracing::warn!("apply_desktop_window: ns_window error: {e}");
            return;
        }
    };
    unsafe {
        if let Some(win) = retain_window(ptr) {
            let level = if interactive {
                K_INTERACTIVE_WALLPAPER_LEVEL
            } else {
                K_CG_DESKTOP_WINDOW_LEVEL
            };
            win.setLevel(level);
            win.setCollectionBehavior(
                objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllSpaces
                    | objc2_app_kit::NSWindowCollectionBehavior::Stationary
                    | objc2_app_kit::NSWindowCollectionBehavior::IgnoresCycle
                    // 桌面级无边框窗口也允许跨满屏（含顶部菜单栏区域），
                    // 否则顶部会露出一条桌面壁纸（渐变浅灰）。
                    | objc2_app_kit::NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            // interactive：接收输入（视差 / 网页交互）；否则忽略鼠标，桌面正常。
            win.setIgnoresMouseEvents(!interactive);
            // 接收「鼠标移动」事件（场景视差 / 网页 hover 需要）
            win.setAcceptsMouseMovedEvents(interactive);
            win.setOpaque(false);
            let clear = objc2_app_kit::NSColor::clearColor();
            win.setBackgroundColor(Some(&clear));
            win.setHasShadow(false);
            win.setHidesOnDeactivate(false);
            win.setFrame_display(
                objc2_foundation::NSRect {
                    origin: objc2_foundation::NSPoint { x: frame.0, y: frame.1 },
                    size: objc2_foundation::NSSize { width: frame.2, height: frame.3 },
                },
                true,
            );
            // 关键修复：show 之后重设层级 + 强制前置合成（防 WKWebView 被遮挡暂停渲染）
            win.setLevel(level);
            win.orderFrontRegardless();
        }
        // 私有 KVC 探测（macOS 26 不支持，安全忽略）
        if let Ok(view) = window.ns_view() {
            set_webview_update_while_hidden(view);
        }
    }
    tracing::info!(
        "apply_desktop_window ok (level={K_CG_DESKTOP_WINDOW_LEVEL}, frame={:?})",
        frame
    );
}

unsafe fn retain_window(ptr: *mut c_void) -> Option<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSWindow;
    if MainThreadMarker::new().is_none() {
        tracing::warn!("window op not on main thread");
        return None;
    }
    unsafe { Retained::retain(ptr.cast::<NSWindow>()) }
}

/// 让 WKWebView 在窗口被完全遮挡时仍持续渲染（私有方法，先探测）
unsafe fn set_webview_update_while_hidden(view: *mut c_void) {
    use objc2::msg_send;
    use objc2::runtime::NSObject;

    if view.is_null() {
        return;
    }
    let obj = view.cast::<NSObject>();
    let sel = objc2::sel!(setShouldUpdateWhileHidden:);
    let responds: bool = unsafe { msg_send![&*obj, respondsToSelector: sel] };
    if responds {
        let _: () = unsafe { msg_send![&*obj, setShouldUpdateWhileHidden: true] };
        tracing::info!("wallpaper webview: setShouldUpdateWhileHidden = YES");
    }
}
