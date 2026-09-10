//! 外部指针注入（macOS）。
//!
//! 为什么需要：「隐藏图标」关闭时壁纸窗口位于桌面图标**下方**，图标层在上面把
//! 鼠标事件全截走了，窗口自己一个 mouseMoved 也收不到 —— 场景视差、网页 hover、
//! 鼠标跟随类壁纸会整个失效。所以由宿主轮询系统光标位置，换算成窗口内归一化
//! 坐标后经 `window.__wp.pushPointer(u, v, buttons)` 推进渲染器，
//! 再由库的 `SceneInstance.pushPointer` 喂给引擎。
//!
//! 「隐藏图标」开启时**必须停止注入**：那时窗口在图标之上，能直接收到真实
//! 鼠标事件，再注入等于同一次移动被处理两遍（视差抖动、hover 反复触发）。
//!
//! 轮询而非事件监听：全局鼠标事件要 CGEventTap，那需要「输入监控」权限
//! （TCC 弹窗 + 系统设置里手动勾选）。而光标位置用 `CGEventSourceGet…` 系
//! 的只读 API 就能拿，不需要任何权限 —— 对壁纸这种「只要位置」的场景够用。
//!
//! 只在桌面活动时注入：前台是别的应用时，光标根本不在桌面层上，注入只会让
//! 壁纸跟着一个「看不见」的光标做视差/跟随，还每 33ms 白过一次 JS 桥。
//! 所以注入的实际生效条件是「非交互态（图标隐藏关闭）且桌面活动（Finder 前台）」，
//! 桌面活动状态由 macos.rs 的前台应用切换观察者同步进来（set_desktop_active）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};

/// 轮询间隔。60fps 没必要：指针类壁纸的视差/跟随都有自己的插值缓动，
/// 30Hz 足够跟手，而每次注入要过一次 evaluate_script（JS 解析开销）。
const POLL_MS: u64 = 33;

/// 注入总开关（派生值，由 refresh_injecting 统一计算）。生效条件 =
/// 非交互态（!INTERACTIVE）且桌面活动（DESKTOP_ACTIVE）。
static INJECTING: AtomicBool = AtomicBool::new(false);

/// 「隐藏图标」开关原值：true = 窗口在图标之上收真实事件 → 不注入。
static INTERACTIVE: AtomicBool = AtomicBool::new(false);

/// 桌面是否活动（Finder 为前台应用）。默认 true：启动时尚未收到前台切换
/// 通知，保持旧的「总是注入」行为，第一次切换后由观察者校正。
static DESKTOP_ACTIVE: AtomicBool = AtomicBool::new(true);

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGEventGetLocation(event: *const std::ffi::c_void) -> CPoint;
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    // 签名与 audio_capture.rs 里的声明保持一致（*mut），否则同一符号被两处
    // 以不同签名重复声明，rustc 会警告 "redeclared with a different signature"
    fn CFRelease(cf: *mut std::ffi::c_void);
}

/// 读系统光标位置（全局屏幕坐标，左上原点、y 向下）与左键状态。
///
/// `CGEventCreate(NULL)` 造一个「当前状态」事件再取位置，是不需要任何权限的
/// 只读查询；不要换成 NSEvent.mouseLocation —— 那个只能在主线程调用，
/// 而这里跑在轮询线程上。
#[cfg(target_os = "macos")]
fn cursor_state() -> Option<(f64, f64, u32)> {
    unsafe {
        let ev = CGEventCreate(std::ptr::null_mut());
        if ev.is_null() {
            return None;
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        // state_id 1 = kCGEventSourceStateCombinedSessionState（含硬件与合成事件）
        // button 0 = kCGMouseButtonLeft。引擎只消费按键位掩码的 bit0，右键不读。
        let left = CGEventSourceButtonState(1, 0);
        Some((p.x, p.y, if left { 1 } else { 0 }))
    }
}

/// 非 macOS 平台的光标查询由各平台后端提供（Linux 见 linux.rs 的说明与限制）
#[cfg(not(target_os = "macos"))]
fn cursor_state() -> Option<(f64, f64, u32)> {
    super::platform::cursor_state()
}

/// 设置「隐藏图标」开关：
/// 开启（true）→ 窗口在图标之上收真实事件 → 不注入；
/// 关闭（false，默认）→ 窗口在图标之下收不到事件 → 注入（还需桌面活动）。
pub fn set_interactive(interactive: bool) {
    INTERACTIVE.store(interactive, Ordering::SeqCst);
    refresh_injecting();
}

/// 设置桌面活动状态（由 macos.rs 的前台应用切换观察者调用；Linux 暂无观察者）。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// 非桌面活动时停注入：光标在别的应用窗口上，注入的坐标对壁纸无意义，
/// 还白耗 JS 桥；轮询线程发现 INJECTING 关闭时会给最后命中的窗口补发
/// pointerLeave，让视差/跟随回正，而不是停在切走前那一点。
pub fn set_desktop_active(active: bool) {
    let was = DESKTOP_ACTIVE.swap(active, Ordering::SeqCst);
    if was != active {
        tracing::debug!("pointer inject: desktop active = {active}");
    }
    refresh_injecting();
}

/// 汇总两个门控条件，更新实际注入开关。
fn refresh_injecting() {
    let inject = !INTERACTIVE.load(Ordering::SeqCst) && DESKTOP_ACTIVE.load(Ordering::SeqCst);
    let was = INJECTING.swap(inject, Ordering::SeqCst);
    if was != inject {
        tracing::info!(
            "pointer inject: {} (interactive={}, desktop_active={})",
            if inject { "on" } else { "off" },
            INTERACTIVE.load(Ordering::SeqCst),
            DESKTOP_ACTIVE.load(Ordering::SeqCst),
        );
    }
}

/// 启动轮询线程。重复调用无副作用（进程内只跑一条）。
pub fn start(app: &AppHandle, interactive: bool) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    set_interactive(interactive);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("wallpaper-pointer".into())
        .spawn(move || poll_loop(app))
        .ok();
}

fn poll_loop(app: AppHandle) {
    // 每个窗口上一次推送的归一化坐标 + 按键，用来跳过「没动」的帧。
    // 光标静止时（用户在别的应用里打字）没必要每 33ms 过一次 JS 桥。
    let last: Arc<Mutex<std::collections::HashMap<String, (f64, f64, u32)>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    // 上一帧光标所在的窗口 label，用于在跨屏时给旧窗口补一次 pointerLeave
    let mut prev_inside: Option<String> = None;

    loop {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        if !INJECTING.load(Ordering::Relaxed) {
            // 关注入的瞬间要把「指针已离开」这个终态送到，否则壁纸会停在
            // 最后一次注入的位置上（视差歪着不回正）
            if let Some(label) = prev_inside.take() {
                emit_leave(&app, &label);
            }
            continue;
        }
        let Some((gx, gy, buttons)) = cursor_state() else {
            continue;
        };

        // 命中哪块屏：CGDisplayBounds 与 CGEventGetLocation 同为「左上原点、
        // y 向下」的全局坐标，可以直接比较，不需要翻转。
        let screens = super::platform::active_screens();
        let hit = screens
            .iter()
            .find(|s| gx >= s.x && gx < s.x + s.w && gy >= s.y && gy < s.y + s.h);
        let Some(s) = hit else {
            if let Some(label) = prev_inside.take() {
                emit_leave(&app, &label);
            }
            continue;
        };

        let label = format!("wallpaper-{}", s.id);
        // 跨屏了：先让旧窗口知道指针走了，否则它会一直停在边缘那一点
        if prev_inside.as_deref() != Some(label.as_str()) {
            if let Some(old) = prev_inside.replace(label.clone()) {
                emit_leave(&app, &old);
            }
        }

        let u = ((gx - s.x) / s.w).clamp(0.0, 1.0);
        let v = ((gy - s.y) / s.h).clamp(0.0, 1.0);

        // 亚像素级抖动不值得过 JS 桥：阈值取窗口宽度的千分之一量级
        if let Ok(mut g) = last.lock() {
            if let Some(&(pu, pv, pb)) = g.get(&label) {
                if pb == buttons && (pu - u).abs() < 0.001 && (pv - v).abs() < 0.001 {
                    continue;
                }
            }
            g.insert(label.clone(), (u, v, buttons));
        }

        if let Some(w) = app.get_webview_window(&label) {
            let js = format!("window.__wp&&window.__wp.pushPointer({u:.5},{v:.5},{buttons})");
            let _ = w.eval(&js);
        }
    }
}

fn emit_leave(app: &AppHandle, label: &str) {
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.eval("window.__wp&&window.__wp.pointerLeave()");
    }
}
