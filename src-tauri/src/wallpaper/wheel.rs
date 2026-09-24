//! 外部滚轮 / 触控板手势注入。
//!
//! 与 [super::pointer] 解决同一类问题：非交互态（壁纸窗口在桌面图标下方）时
//! 窗口自己收不到任何滚轮事件，**网页壁纸**里的缩放/滚动/翻页（pano2vr 全景、
//! OrbitControls、幻灯片）全部不可用。由宿主捕获系统级滚轮并经
//! `window.__wp.pushWheel(dx, dy, mode, mods)` 推进渲染器，再由 webwallgl 的
//! `SceneInstance.pushWheel` 合成 WheelEvent 喂给网页壁纸（场景壁纸不消费滚轮，
//! 库内部静默丢弃）。
//!
//! ## 触控板双指手势（主要目标）
//! - **双指滚动**：触控板给的是像素级连续小 delta（macOS `hasPreciseScrollingDeltas`，
//!   mode=0），普通鼠标滚轮是按行跳变（mode=1）。两套 delta 都取，有像素值优先像素。
//! - **双指捏合缩放**：系统会把捏合合成成 **ctrl + 滚轮**事件（浏览器就是这样
//!   识别 pinch 的，OrbitControls / pano2vr 一族靠 `event.ctrlKey` 区分缩放与滚动），
//!   所以只需原样转发 ctrl 修饰位（mods bit0）。
//!
//! ## 方向
//! 与浏览器 WheelEvent 同向：dy>0 向下滚/内容下移，dx>0 向右。macOS 的
//! `NSEvent.scrollingDeltaY`/CG 像素 delta 与浏览器相反（手指方向），Y 轴取反；
//! Windows 的 WM_MOUSEWHEEL 上滚为正、而浏览器 deltaY 下为正，Y 轴同样取反。
//! X 轴两平台符号约定与浏览器一致，不取反。
//!
//! ## 门控
//! 与指针注入同纪律：只在「非交互态 + 桌面活动」（[super::pointer::is_injecting]）
//! 时转发。交互态窗口在图标之上能直接收到真实滚轮事件，再注入会处理两遍。
//! 触控板惯性（momentum）阶段的事件照常转发——浏览器也会把惯性滚动派发给页面。
//!
//! 懒启动：只有在确实播放**网页壁纸**时才创建系统捕获（见 pointer 轮询线程里的
//! ensure_started），场景/视频类壁纸的用户不会被索要「输入监控」权限。

use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use tauri::AppHandle;

static STARTED: AtomicBool = AtomicBool::new(false);

/// 启动系统滚轮捕获（进程内幂等）。由指针轮询线程在出现网页壁纸时调用。
pub fn ensure_started(app: &AppHandle) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    platform_start(app.clone());
}

/// 一次滚轮事件派发给光标所在屏幕的壁纸窗口。
/// 零向量（个别驱动的空事件）忽略；不在任何屏内或门控关闭时不发。
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
fn dispatch(app: &AppHandle, gx: f64, gy: f64, dx: f64, dy: f64, mode: u32, mods: u32) {
    if dx == 0.0 && dy == 0.0 {
        return;
    }
    if !super::pointer::is_injecting() {
        return;
    }
    let Some(id) = super::platform::hit_screen_id(gx, gy) else {
        return;
    };
    let label = format!("wallpaper-{id}");
    for w in super::wallpaper_windows(app, &label) {
        let js = format!("window.__wp&&window.__wp.pushWheel({dx:.4},{dy:.4},{mode},{mods})");
        let _ = w.eval(&js);
    }
}

// ---------- macOS：CGEventTap（需「输入监控」权限） ----------

#[cfg(target_os = "macos")]
fn platform_start(app: AppHandle) {
    std::thread::Builder::new()
        .name("wallpaper-wheel".into())
        .spawn(move || tap_loop(app))
        .ok();
}

/// kCGEventTapProxy 不透明指针
#[cfg(target_os = "macos")]
type EventTapProxy = *mut std::ffi::c_void;
#[cfg(target_os = "macos")]
type EventRef = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: extern "C" fn(EventTapProxy, u32, EventRef, *mut std::ffi::c_void) -> EventRef,
        refcon: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;
    fn CGEventTapEnable(tap: *const std::ffi::c_void, enable: bool);
    fn CGEventGetIntegerValueField(event: EventRef, field: u32) -> i64;
    fn CGEventGetFlags(event: EventRef) -> u64;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
    fn CFRunLoopAddSource(
        rl: *mut std::ffi::c_void,
        src: *mut std::ffi::c_void,
        mode: *const std::ffi::c_void,
    );
    fn CFRunLoopRun();
    fn CFRelease(cf: *mut std::ffi::c_void);
    fn CFMachPortCreateRunLoopSource(
        allocator: *mut std::ffi::c_void,
        port: *mut std::ffi::c_void,
        order: isize,
    ) -> *mut std::ffi::c_void;
    fn CFMachPortInvalidate(mp: *mut std::ffi::c_void);
    static kCFRunLoopCommonModes: *const std::ffi::c_void;
}

// CGEventTapCreate 参数 / 事件常量（CoreGraphics，SDK 头里是枚举，这里直接定值）
#[cfg(target_os = "macos")]
const K_CG_SESSION_EVENT_TAP: u32 = 1;
#[cfg(target_os = "macos")]
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
#[cfg(target_os = "macos")]
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
#[cfg(target_os = "macos")]
const K_CG_EVENT_SCROLL_WHEEL: u32 = 22;
#[cfg(target_os = "macos")]
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
#[cfg(target_os = "macos")]
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
// CGScrollEventEventField：行 delta 11/12，触控板像素 delta 96/97
#[cfg(target_os = "macos")]
const F_AXIS1_LINE: u32 = 11;
#[cfg(target_os = "macos")]
const F_AXIS2_LINE: u32 = 12;
#[cfg(target_os = "macos")]
const F_AXIS1_POINT: u32 = 96;
#[cfg(target_os = "macos")]
const F_AXIS2_POINT: u32 = 97;
// NX 修饰位（与 CGEventFlags 同值）：shift 1<<17 / control 1<<18 /
// alternate 1<<19 / command 1<<20
#[cfg(target_os = "macos")]
const M_SHIFT: u64 = 1 << 17;
#[cfg(target_os = "macos")]
const M_CTRL: u64 = 1 << 18;
#[cfg(target_os = "macos")]
const M_ALT: u64 = 1 << 19;
#[cfg(target_os = "macos")]
const M_META: u64 = 1 << 20;

/// 已创建的 tap 指针：tap 被系统超时时回调要拿它重新 enable（回调只给 proxy）。
#[cfg(target_os = "macos")]
static ACTIVE_TAP: std::sync::atomic::AtomicPtr<std::ffi::c_void> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

#[cfg(target_os = "macos")]
extern "C" fn tap_callback(
    _proxy: EventTapProxy,
    event_type: u32,
    event: EventRef,
    refcon: *mut std::ffi::c_void,
) -> EventRef {
    // 系统在响应超时（或用户输入禁用）时会停掉 tap，这里重新拉起；event 为空
    if event_type == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT
        || event_type == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT
    {
        let tap = ACTIVE_TAP.load(Ordering::SeqCst);
        if !tap.is_null() {
            unsafe { CGEventTapEnable(tap, true) };
        }
        return event;
    }
    if event_type == K_CG_EVENT_SCROLL_WHEEL && !event.is_null() && !refcon.is_null() {
        unsafe {
            let dx_line = CGEventGetIntegerValueField(event, F_AXIS2_LINE) as f64;
            let dy_line = CGEventGetIntegerValueField(event, F_AXIS1_LINE) as f64;
            let dx_pt = CGEventGetIntegerValueField(event, F_AXIS2_POINT) as f64;
            let dy_pt = CGEventGetIntegerValueField(event, F_AXIS1_POINT) as f64;
            // 触控板（含捏合）给像素 delta；普通鼠标滚轮只填行 delta
            let (dx, dy, mode) = if dx_pt != 0.0 || dy_pt != 0.0 {
                (dx_pt, -dy_pt, 0)
            } else {
                (dx_line, -dy_line, 1)
            };
            let flags = CGEventGetFlags(event);
            let mods = ((flags & M_CTRL != 0) as u32)
                | (((flags & M_SHIFT != 0) as u32) << 1)
                | (((flags & M_ALT != 0) as u32) << 2)
                | (((flags & M_META != 0) as u32) << 3);
            let (gx, gy) = super::pointer::cg_event_location(event as *const _);
            let app = &*(refcon as *const AppHandle);
            dispatch(app, gx, gy, dx, dy, mode, mods);
        }
    }
    // listen-only 也必须原样返回事件；返回 NULL 会吞掉这次输入
    event
}

#[cfg(target_os = "macos")]
fn tap_loop(app: AppHandle) {
    // refcon 借回调读 AppHandle：tap 随进程存活，leak 一次即可
    let refcon = Box::into_raw(Box::new(app)) as *mut std::ffi::c_void;
    loop {
        let tap = unsafe {
            CGEventTapCreate(
                K_CG_SESSION_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                1u64 << K_CG_EVENT_SCROLL_WHEEL,
                tap_callback,
                refcon,
            )
        };
        if tap.is_null() {
            // 没授「输入监控」权限（或有程序独占了 tap）：不刷日志，每 5s 试一次，
            // 用户在系统设置里勾选后不必重启应用即自愈
            tracing::info!("wheel tap: 创建失败（通常是尚未授予「输入监控」权限），5s 后重试");
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }
        ACTIVE_TAP.store(tap as *mut _, Ordering::SeqCst);
        tracing::info!("wheel tap: 已启动（双指滚动 / 捏合缩放注入网页壁纸）");
        unsafe {
            // CreateRunLoopSource 返回我们持有的 source；AddSource 会再 retain 一次，
            // 加入后即可释放本引用（不能用已非公有的 CFMachPortGetRunLoopSource）
            let src = CFMachPortCreateRunLoopSource(std::ptr::null_mut(), tap, 0);
            let rl = CFRunLoopGetCurrent();
            CFRunLoopAddSource(rl, src, kCFRunLoopCommonModes);
            if !src.is_null() {
                CFRelease(src);
            }
            // 阻塞到 CFRunLoopStop（本应用不主动停）；tap 失效由回调自行 re-enable
            CFRunLoopRun();
            CFMachPortInvalidate(tap);
            CFRelease(tap);
        }
        ACTIVE_TAP.store(std::ptr::null_mut(), Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

// ---------- Windows：WH_MOUSE_LL 低级钩子（无需权限） ----------

#[cfg(target_os = "windows")]
fn platform_start(app: AppHandle) {
    std::thread::Builder::new()
        .name("wallpaper-wheel".into())
        .spawn(move || hook_loop(app))
        .ok();
}

#[cfg(target_os = "windows")]
mod win32 {
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, MSG,
        MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_MOUSEHWHEEL, WM_MOUSEWHEEL,
    };

    use super::*;

    /// 每档滚轮的标准 delta（WHEEL_DELTA）
    const WHEEL_DELTA: f64 = 120.0;

    /// GetKeyState 最高位置 1 = 该键当前按下（钩子是同步派发的，取的就是事件
    /// 发生瞬间的状态）。触控板捏合合成 ctrl+滚轮，靠这个位识别缩放。
    fn key_down(vk: i32) -> bool {
        unsafe { GetKeyState(vk) as u16 & 0x8000 != 0 }
    }

    static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

    unsafe extern "system" fn low_level_mouse(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // HC_ACTION == 0：只有真实事件才处理
        if code >= 0 {
            let msg = wparam.0 as u32;
            if msg == WM_MOUSEWHEEL || msg == WM_MOUSEHWHEEL {
                let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
                // 低级钩子里滚轮量在 mouseData 高字（带符号，WHEEL_DELTA=120 为一档），
                // 不在 wParam；wParam 这里只是消息 ID
                let delta = ((info.mouseData >> 16) & 0xFFFF) as i16 as f64;
                let notches = delta / WHEEL_DELTA;
                // 垂直：上滚为正、浏览器下为正 → 取反。水平：右为正，与浏览器一致
                let (dx, dy) = if msg == WM_MOUSEWHEEL {
                    (0.0, -notches)
                } else {
                    (notches, 0.0)
                };
                // 精准触控板捏合在 Windows 上同样合成 ctrl+滚轮；按键态取自 GetKeyState
                let mods = (key_down(VK_CONTROL.0 as i32) as u32)
                    | ((key_down(VK_SHIFT.0 as i32) as u32) << 1);
                if let Some(app) = APP.get() {
                    dispatch(app, info.pt.x as f64, info.pt.y as f64, dx, dy, 1, mods);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub(super) fn run(app: AppHandle) {
        let _ = APP.set(app);
        loop {
            // 当前可执行模块句柄：低级钩子的回调在本进程内派发，按 MSDN 规范
            // 传当前模块句柄最稳（与 windows.rs 的观察者窗口同一写法）
            let hmod = unsafe { GetModuleHandleW(None) }
                .ok()
                .map(|h| HINSTANCE(h.0))
                .unwrap_or_default();
            let hook: Result<HHOOK, _> = unsafe {
                SetWindowsHookExW(WH_MOUSE_LL, Some(low_level_mouse), hmod, 0)
            };
            let Ok(hook) = hook else {
                tracing::warn!("wheel hook: SetWindowsHookExW 失败，5s 后重试");
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            };
            tracing::info!("wheel hook: 已启动（滚轮 / 触控板手势注入网页壁纸）");
            // 低级钩子要求线程跑消息循环，超时不响应还会被系统静默摘除
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                // 钩子回调由系统在本线程直接派发，无需 TranslateMessage
            }
            let _ = unsafe { UnhookWindowsHookEx(hook) };
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}

#[cfg(target_os = "windows")]
fn hook_loop(app: AppHandle) {
    win32::run(app);
}

// ---------- Linux / 其它平台：暂无系统级滚轮源 ----------

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_start(_app: AppHandle) {
    // X11 可经 XInput2 / Wayland 无全局手势协议，后续单独接入；
    // 网页壁纸在交互态仍能收到窗口自身的真实滚轮事件。
    tracing::debug!("wheel inject: 当前平台无系统级滚轮捕获，未启动");
}
