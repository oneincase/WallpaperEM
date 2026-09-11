//! Windows 桌面层窗口与屏幕枚举。
//!
//! 与 Linux/macOS 后端的差异与取舍：
//! - **屏幕枚举**走 Tauri 的 `available_monitors`（Win32 显示器 API），拿到的是
//!   物理像素 + per-monitor DPI scale，这里统一换算成**逻辑坐标**（与窗口
//!   set_position/set_size 的逻辑单位一致，等价于 macOS 的 points）。
//! - **桌面层级**：Win32 没有「桌面层窗口」这个官方概念，通行做法是把窗口
//!   `SetParent` 到资源管理器的 `WorkerW`（即 `Progman` 之下、桌面图标之下的
//!   那层），由 `tauri-plugin-desktop-underlay` 完成（见其 core/windows.rs）。
//!   父子化后窗口坐标变成「相对 WorkerW 客户区」，且 WorkerW 覆盖整个虚拟屏，
//!   所以几何一律用物理像素 + 父窗口客户区原点换算，不再走 tao 的顶层窗口语义。
//! - **「隐藏图标」开关**：对应交互态 —— 非交互时窗口在图标**下方**（收不到
//!   真实鼠标，靠宿主轮询 + 注入）；交互时脱离 WorkerW，压到 Z 序最底但仍在
//!   普通窗口之下（盖住桌面图标并直接接收鼠标）。
//! - **前台应用观察者 / 自动暂停**：用 `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`
//!   实现（Win32 有稳定的全局前台切换通知），语义与 macOS 的
//!   NSWorkspaceDidActivateApplicationNotification 对齐：切到非桌面自动暂停、
//!   回桌面（Progman/WorkerW 前台，或点在自己的壁纸窗口上）自动恢复。
//! - **显示器睡眠**：独占线程注册 `GUID_CONSOLE_DISPLAY_STATE` 电源通知
//!   （`WM_POWERBROADCAST` → `PBT_POWERSETTINGCHANGE`），关屏时暂停、亮屏恢复，
//!   与 macOS 的 `CGDisplayIsAsleep` 轮询等价但事件驱动、零轮询开销。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, Runtime, WebviewWindow};
use windows::core::BOOL;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    RegisterPowerSettingNotification, POWERBROADCAST_SETTING,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, DPI_AWARENESS_CONTEXT_UNAWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, EnumWindows, FindWindowA, FindWindowExA,
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetParent,
    GetWindowThreadProcessId, RegisterClassW, SendMessageTimeoutA, SetParent, SetWindowPos,
    TranslateMessage, EVENT_SYSTEM_FOREGROUND, HWND_BOTTOM, HWND_MESSAGE, MSG,
    PBT_POWERSETTINGCHANGE, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT, WNDCLASSW, WM_POWERBROADCAST,
};

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    pub id: u32,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 一台显示器的完整信息：逻辑坐标（对外）+ 物理像素矩形（Win32 调用用）
#[derive(Clone)]
struct Mon {
    info: ScreenInfo,
    scale: f64,
    /// (x, y, w, h)，物理像素、虚拟桌面坐标系（主屏左上为原点，可为负）
    px: (i32, i32, i32, i32),
}

/// 引擎启动时注入的 AppHandle（屏幕枚举需要它进 Tauri runtime）
static APP: OnceLock<AppHandle> = OnceLock::new();

/// 最近一次枚举到的显示器与枚举时刻（TTL 缓存）。
///
/// 为什么缓存：`available_monitors` 要回到 Tauri 事件循环线程取 tao 的数据，
/// 而指针注入是 30Hz 轮询、每帧都要坐标换算，每次现查既慢又容易在窗口重建
/// 的临界区读到空表。权威刷新由壁纸监控（2s tick）驱动，其余调用方读缓存。
static SCREENS_CACHE: Mutex<Option<(Instant, Vec<Mon>)>> = Mutex::new(None);

/// 屏幕列表缓存有效期（与壁纸监控 tick 同量级）
const SCREENS_CACHE_TTL: Duration = Duration::from_millis(1500);

/// 显示器是否处于关屏/睡眠（电源通知回调写入，`display_asleep` 读取）
static DISPLAY_ASLEEP: AtomicBool = AtomicBool::new(false);

/// 「显示器枚举为空」告警的限流时刻（指针线程会以 ~30Hz 调 active_screens，
/// 过渡期可能连续为空，不能每次都打日志）
static LAST_EMPTY_WARN: Mutex<Option<Instant>> = Mutex::new(None);

fn warn_empty_screens() {
    if let Ok(mut g) = LAST_EMPTY_WARN.lock() {
        let now = Instant::now();
        if g.is_some_and(|t| now.duration_since(t) < Duration::from_secs(10)) {
            return;
        }
        *g = Some(now);
    }
    tracing::warn!("windows: 显示器枚举为空（睡眠/热插拔过渡？），本次不写缓存");
}

/// 由 wallpaper::init 调用一次（屏幕枚举需要常驻 AppHandle）
pub fn store_app_handle(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

/// 显示器名称 → 稳定的数值 id（Windows 用设备名如 `\\.\DISPLAY1` 的哈希，
/// 热插拔/重启后同一块屏能拿回同一个 id，会话恢复才对得上；与 Linux 后端同一套
/// FNV-1a，避免换端口/换线导致 `wallpaper-<id>` 标签漂移）
fn monitor_id(name: Option<&str>, index: usize) -> u32 {
    match name {
        Some(n) if !n.is_empty() => {
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

/// 活动显示器列表（逻辑坐标，左上原点）。命中 TTL 缓存直接返回。
pub fn active_screens() -> Vec<ScreenInfo> {
    if let Ok(cache) = SCREENS_CACHE.lock() {
        if let Some((at, screens)) = &*cache {
            if at.elapsed() < SCREENS_CACHE_TTL {
                return screens.iter().map(|m| m.info.clone()).collect();
            }
        }
    }
    let out = query_monitors();
    if out.is_empty() {
        // ⚠️ 空列表**绝不写缓存**：显示器枚举会短暂返回空（睡眠/热插拔/会话切换过渡），
        // 而指针线程以 ~30Hz 调本函数、监控 tick 每 2s 调一次 —— 一旦把空结果缓存住，
        // 监控侧 `ensure_windows_inner` 会因 `screens.is_empty()` 持续提前返回，
        // 表现就是「重启后一直不恢复桌面壁纸」。保持旧缓存/下次重查即可。
        warn_empty_screens();
        return Vec::new();
    }
    if let Ok(mut cache) = SCREENS_CACHE.lock() {
        *cache = Some((Instant::now(), out.clone()));
    }
    out.into_iter().map(|m| m.info).collect()
}

/// 经 Tauri 枚举显示器（物理像素 + scale → 逻辑坐标）
fn query_monitors() -> Vec<Mon> {
    let Some(app) = APP.get() else {
        return Vec::new();
    };
    let monitors = match app.available_monitors() {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return Vec::new(),
        Err(e) => {
            tracing::warn!("windows: available_monitors failed: {e}");
            return Vec::new();
        }
    };
    monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let pos = m.position();
            let size = m.size();
            let scale = if m.scale_factor() > 0.0 {
                m.scale_factor()
            } else {
                1.0
            };
            Mon {
                info: ScreenInfo {
                    id: monitor_id(m.name().map(|s| s.as_str()), i),
                    x: pos.x as f64 / scale,
                    y: pos.y as f64 / scale,
                    w: size.width as f64 / scale,
                    h: size.height as f64 / scale,
                },
                scale,
                px: (pos.x, pos.y, size.width as i32, size.height as i32),
            }
        })
        .collect()
}

/// 主显示器是否睡眠（电源通知驱动）
pub fn display_asleep() -> bool {
    DISPLAY_ASLEEP.load(Ordering::Relaxed)
}

/// 逻辑 frame → 物理像素矩形。命中缓存的按该屏 scale 换算；未命中（布局刚变、
/// 枚举还没跟上）时退回按主屏 scale 估算 —— 宁可差一帧几何，也不要错位不动。
fn physical_for_frame(frame: (f64, f64, f64, f64)) -> (i32, i32, i32, i32) {
    if let Ok(cache) = SCREENS_CACHE.lock() {
        if let Some((_, mons)) = &*cache {
            // 先按逻辑原点精确匹配（同一块屏），再退回「包含该点」的屏
            let exact = mons.iter().find(|m| {
                (m.info.x - frame.0).abs() < 0.5 && (m.info.y - frame.1).abs() < 0.5
            });
            let hit = exact.or_else(|| {
                let (cx, cy) = (frame.0 + frame.2 / 2.0, frame.1 + frame.3 / 2.0);
                mons.iter().find(|m| {
                    cx >= m.info.x - 0.5
                        && cx < m.info.x + m.info.w + 0.5
                        && cy >= m.info.y - 0.5
                        && cy < m.info.y + m.info.h + 0.5
                })
            });
            if let Some(m) = hit {
                // 以命中屏的物理原点为基准，避免混合 DPI 下逐边取整产生 1px 缝
                let dx = ((frame.0 - m.info.x) * m.scale).round() as i32;
                let dy = ((frame.1 - m.info.y) * m.scale).round() as i32;
                return (
                    m.px.0 + dx,
                    m.px.1 + dy,
                    (frame.2 * m.scale).round() as i32,
                    (frame.3 * m.scale).round() as i32,
                );
            }
        }
    }
    (
        frame.0.round() as i32,
        frame.1.round() as i32,
        frame.2.round() as i32,
        frame.3.round() as i32,
    )
}

/// 设置窗口 frame（逻辑坐标）
pub fn set_frame<R: Runtime>(window: &WebviewWindow<R>, x: f64, y: f64, w: f64, h: f64) {
    let phys = physical_for_frame((x, y, w, h));
    match window.hwnd() {
        Ok(hwnd) if !hwnd.is_invalid() => unsafe {
            // 以**真实父窗口**判断当前在哪一层（父子化后坐标是相对父客户区）。
            // 不查任何缓存：缓存一旦与实际不一致，就会把窗口摆到错误的坐标系。
            if has_parent(hwnd) {
                place_in_underlay(hwnd, phys);
            } else {
                place_top_level(hwnd, phys);
            }
        },
        _ => {
            // 窗口尚未创建原生句柄：退回 tao 的顶层语义（建窗后 apply_desktop_window 会矫正）
            let _ = window.set_position(LogicalPosition::new(x, y));
            let _ = window.set_size(LogicalSize::new(w, h));
        }
    }
}

/// 桌面层级 + 鼠标穿透。
///
/// `interactive=false`（「隐藏图标」关闭，默认）：父子化到 WorkerW，窗口位于
/// 桌面图标**下方**；此时收不到真实鼠标事件，交互靠宿主轮询系统鼠标 + 注入。
///
/// `interactive=true`（「隐藏图标」开启）：脱离 WorkerW 并压到 Z 序最底，盖住
/// 桌面图标但仍在普通应用窗口之下，直接接收真实鼠标事件。
///
/// ⚠️ 这里**不用** tauri-plugin-desktop-underlay 的 `set/is_desktop_underlay`：
/// 它只用窗口 label 记录「是否已下沉」，窗口销毁时不会清理；切壁纸走的是
/// 「销毁 + 同名重建」，重建后的窗口会被误判为「已是 underlay」而跳过 SetParent，
/// 结果以普通顶层窗口 show 出来、全屏盖住一切（Windows 实测）。改为自己父子化，
/// 并用 `GetParent` 读回真实父窗口校验；任何一步失败都退回 Z 序最底，宁可
/// 「图标被壁纸盖住」也绝不出现「壁纸盖住整个桌面」。
pub fn apply_desktop_window<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
    frame: (f64, f64, f64, f64),
    interactive: bool,
) {
    let _ = window.set_ignore_cursor_events(!interactive);
    let phys = physical_for_frame(frame);
    let hwnd = match window.hwnd() {
        Ok(h) if !h.is_invalid() => h,
        Ok(_) => {
            tracing::warn!("windows: apply_desktop_window hwnd is null");
            return;
        }
        Err(e) => {
            tracing::warn!("windows: apply_desktop_window hwnd error: {e}");
            return;
        }
    };

    if interactive {
        // 脱离桌面层（SetParent(None)），再压到 Z 序最底
        unsafe { detach_from_parent(hwnd) };
        unsafe { place_top_level(hwnd, phys) };
    } else {
        match unsafe { parent_to_worker(hwnd) } {
            Ok(worker) => {
                tracing::info!("windows: 已父子化到壁纸 WorkerW {worker:?}");
                unsafe { place_in_underlay(hwnd, phys) };
            }
            Err(e) => {
                tracing::warn!(
                    "windows: WorkerW 父子化失败（{e}）—— 退回 Z 序最底，避免全屏盖住桌面与应用"
                );
                unsafe { place_top_level(hwnd, phys) };
            }
        }
    }
    // 窗口是 visible(false) 创建的：几何/层级设置完再显示，避免闪现未定位的窗口
    let _ = window.show();
    tracing::info!(
        "windows: apply_desktop_window ok (interactive={interactive}, phys={phys:?})"
    );
}

/// 窗口是否已有父窗口（= 已下沉到 WorkerW）
unsafe fn has_parent(hwnd: HWND) -> bool {
    unsafe { matches!(GetParent(hwnd), Ok(p) if !p.is_invalid()) }
}

/// EnumWindows 回调：找到「承载桌面图标」的窗口后，取它**之后**的 WorkerW 兄弟 ——
/// 那才是 Explorer 用来放壁纸的层（位于图标之下）。找到即写入 out。
unsafe extern "system" fn enum_worker(window: HWND, out: LPARAM) -> BOOL {
    unsafe {
        let def_view =
            FindWindowExA(Some(window), None, windows::core::s!("SHELLDLL_DefView"), None)
                .unwrap_or_default();
        if def_view.is_invalid() {
            return true.into();
        }
        let worker = FindWindowExA(None, Some(window), windows::core::s!("WorkerW"), None)
            .unwrap_or_default();
        if worker.is_invalid() {
            return true.into();
        }
        *(out.0 as *mut HWND) = worker;
        true.into()
    }
}

/// 把窗口父子化到「壁纸 WorkerW」，并用 `GetParent` 校验确实挂上了。
///
/// 跨进程 `SetParent` 在两个进程的 DPI 感知级别不一致时会失败（Windows 官方文档
/// 明确记载的行为），而 Explorer 的 Progman/WorkerW 通常不是 Per-Monitor V2，本进程
/// 却是 —— 所以这里依次换几个线程 DPI 上下文重试，直到校验通过。
unsafe fn parent_to_worker(hwnd: HWND) -> Result<HWND, String> {
    let mut last = String::from("未尝试任何 DPI 上下文");
    for ctx in [
        None,
        Some(DPI_AWARENESS_CONTEXT_UNAWARE),
        Some(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE),
    ] {
        let prev = ctx.map(|c| unsafe { SetThreadDpiAwarenessContext(c) });
        let r = unsafe { try_parent_to_worker(hwnd) };
        if let Some(p) = prev {
            unsafe { SetThreadDpiAwarenessContext(p) };
        }
        match r {
            Ok(w) => return Ok(w),
            Err(e) => last = e,
        }
    }
    Err(last)
}

unsafe fn try_parent_to_worker(hwnd: HWND) -> Result<HWND, String> {
    unsafe {
        let progman = FindWindowA(windows::core::s!("Progman"), None)
            .map_err(|e| format!("找不到 Progman: {e}"))?;
        // 让 Progman 生成承载壁纸的 WorkerW（0x052C 是通行做法）
        let _ = SendMessageTimeoutA(
            progman,
            0x052C,
            WPARAM(0x0000000D),
            LPARAM(0x00000001),
            SMTO_NORMAL,
            1000,
            None,
        );
        let mut worker = HWND::default();
        let _ = EnumWindows(Some(enum_worker), LPARAM(&mut worker as *mut HWND as _));
        if worker.is_invalid() {
            // 退回 Progman 直属的 WorkerW
            worker = FindWindowExA(Some(progman), None, windows::core::s!("WorkerW"), None)
                .unwrap_or_default();
        }
        if worker.is_invalid() {
            return Err("未找到壁纸 WorkerW".into());
        }
        // ⚠️ SetParent 返回的是「旧父窗口」：顶层窗口原本无父 ⇒ 返回 NULL，
        // 而 windows-rs 会把 NULL 判成 Err（哪怕调用其实成功了）。故忽略返回值，
        // 改用 GetParent 读回真实父窗口来校验。
        let _ = SetParent(hwnd, Some(worker));
        match GetParent(hwnd) {
            Ok(p) if !p.is_invalid() && p == worker => Ok(worker),
            Ok(p) => Err(format!(
                "SetParent 后父窗口不是预期的 WorkerW（实际 parent={}, worker={}）",
                p.0 as isize, worker.0 as isize
            )),
            Err(e) => Err(format!("SetParent 后读不到父窗口（{e}）")),
        }
    }
}

/// 解除与 WorkerW 的父子关系（回到顶层窗口）
unsafe fn detach_from_parent(hwnd: HWND) {
    unsafe {
        // 同 parent_to_worker：返回值是旧父窗口，NULL 会被误判成 Err，忽略之
        let _ = SetParent(hwnd, None);
    }
}

/// 在 WorkerW 客户区内定位：WorkerW 覆盖整个虚拟屏，其客户区原点可能不在
/// (0,0)（虚拟屏原点为主屏左上，主屏不在最左上时整体为负），必须换算。
unsafe fn place_in_underlay(hwnd: HWND, phys: (i32, i32, i32, i32)) {
    unsafe {
        let (x, y, w, h) = phys;
        let mut origin = POINT { x: 0, y: 0 };
        if let Ok(parent) = GetParent(hwnd) {
            if !parent.is_invalid() {
                let _ = ClientToScreen(parent, &mut origin);
            }
        }
        let _ = SetWindowPos(
            hwnd,
            None,
            x - origin.x,
            y - origin.y,
            w.max(1),
            h.max(1),
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// 顶层窗口定位（交互态）：物理像素 + 虚拟桌面坐标，压到 Z 序最底。
unsafe fn place_top_level(hwnd: HWND, phys: (i32, i32, i32, i32)) {
    unsafe {
        let (x, y, w, h) = phys;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_BOTTOM),
            x,
            y,
            w.max(1),
            h.max(1),
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

/// Windows 无此概念：窗口有原生标题栏，拖标题栏即可。空实现保持平台层签名一致。
pub fn set_movable_by_background<R: Runtime>(_window: &WebviewWindow<R>, _movable: bool) {}

/// 前台应用切换观察者 + 显示器电源通知，跑在一条独占线程上。
///
/// 线程内建一个**消息专用窗口**（HWND_MESSAGE，不出现在屏幕上）承接两类消息：
/// - `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` 的回调（WINEVENT_OUTOFCONTEXT
///   要求调用线程有消息循环，回调也投递到该线程）；
/// - `RegisterPowerSettingNotification` 的 `WM_POWERBROADCAST`。
///
/// 失败只记日志：自动暂停/关屏暂停都是体验增强，缺失不影响壁纸本身。
pub fn start_auto_pause_observer(app: &tauri::AppHandle) {
    let app2 = app.clone();
    let spawned = std::thread::Builder::new()
        .name("wpem-win-observer".into())
        .spawn(move || unsafe {
            let hinstance = GetModuleHandleW(None).ok();
            let hinst = hinstance.map(|h| HINSTANCE(h.0));
            let class_name = windows::core::w!("WallpaperEMObserverWnd");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(observer_wndproc),
                hInstance: hinst.unwrap_or_default(),
                lpszClassName: class_name,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 {
                tracing::warn!("windows: RegisterClassW failed, 自动暂停/关屏暂停不生效");
                return;
            }
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class_name,
                windows::core::w!("WallpaperEMObserver"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                hinst,
                None,
            ) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("windows: observer window create failed: {e}");
                    return;
                }
            };

            // 关屏/亮屏：GUID_CONSOLE_DISPLAY_STATE，数据 0=关 1=开 2=暗
            match RegisterPowerSettingNotification(
                windows::Win32::Foundation::HANDLE(hwnd.0),
                &GUID_CONSOLE_DISPLAY_STATE,
                windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE,
            ) {
                Ok(_) => tracing::info!("windows: console display power notification registered"),
                Err(e) => tracing::warn!("windows: power notification failed: {e}"),
            }

            let app_for_hook = app2.clone();
            let hook: HWINEVENTHOOK = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(foreground_hook_proc),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            if hook.is_invalid() {
                tracing::warn!("windows: SetWinEventHook failed, 自动暂停不生效");
            } else {
                // 回调不能捕获闭包（是裸 fn 指针），AppHandle 经线程局部静态传递
                let _ = HOOK_APP.set(app_for_hook);
                tracing::info!("windows: foreground hook registered（自动暂停可用）");
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
    if spawned.is_err() {
        tracing::warn!("windows: observer thread spawn failed");
    }
}

/// 观察者线程专用：SetWinEventHook 的回调是裸函数指针，AppHandle 只能这样带进去
static HOOK_APP: OnceLock<AppHandle> = OnceLock::new();

unsafe extern "system" fn observer_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if msg == WM_POWERBROADCAST && wparam.0 as u32 == PBT_POWERSETTINGCHANGE {
            let pbs = lparam.0 as *const POWERBROADCAST_SETTING;
            if !pbs.is_null() && (*pbs).PowerSetting == GUID_CONSOLE_DISPLAY_STATE {
                let state = (*pbs).Data[0];
                let asleep = state == 0;
                let was = DISPLAY_ASLEEP.swap(asleep, Ordering::Relaxed);
                if was != asleep {
                    tracing::info!("windows: console display state={state} (asleep={asleep})");
                }
            }
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

unsafe extern "system" fn foreground_hook_proc(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // 只要窗口级的 OBJID_WINDOW(0)，子对象（菜单/光标等）事件忽略
    if id_object != 0 || hwnd.is_invalid() {
        return;
    }
    let Some(app) = HOOK_APP.get() else { return };
    on_foreground_changed(app, hwnd);
}

/// 前台窗口变化：非桌面 → 自动暂停；桌面（Progman/WorkerW）或点在自己的壁纸
/// 窗口上 → 恢复本功能挂的暂停。语义与 macOS 的 on_frontmost_changed 一一对应。
fn on_foreground_changed(app: &tauri::AppHandle, hwnd: HWND) {
    let own_process = std::process::id();
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };

    // 点桌面在交互态下点的是自己的壁纸窗口（不是 Explorer）——「在桌面上点壁纸」
    // 按桌面语义处理；任一设置窗可见才是真的在操作本应用（保持中性语义）
    let self_click_desktop = pid == own_process && !settings_window_visible(app);
    let is_desktop = self_click_desktop || class_name_is_desktop(hwnd);

    super::pointer::set_desktop_active(is_desktop);
    if pid == own_process && !self_click_desktop {
        return;
    }
    let Some(st) = app.try_state::<super::WallpaperEngineState>() else {
        return;
    };
    if is_desktop {
        let was_auto = {
            let mut g = st.auto_paused.lock().unwrap();
            std::mem::replace(&mut *g, false)
        };
        if was_auto {
            let _ = super::resume_all(app.clone());
            tracing::info!("auto-pause: 回到桌面，壁纸已恢复播放");
        }
        return;
    }

    if !auto_pause_enabled(app) || *st.paused.lock().unwrap() {
        return;
    }
    if super::pause_all(app.clone()).is_ok() {
        *st.auto_paused.lock().unwrap() = true;
        tracing::info!("auto-pause: 前台切换，壁纸已自动暂停");
    }
}

/// 前台窗口是否为「桌面」（Explorer 的 Progman / WorkerW / 图标视图）
fn class_name_is_desktop(hwnd: HWND) -> bool {
    let mut buf = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return false;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    matches!(
        name.as_str(),
        "Progman" | "WorkerW" | "SHELLDLL_DefView" | "SysListView32"
    )
}

/// 主设置窗/壁纸属性窗是否可见（任一可见即认为用户正在操作本应用设置）
fn settings_window_visible(app: &tauri::AppHandle) -> bool {
    let visible = |label: &str| {
        app.get_webview_window(label)
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false)
    };
    if visible("main") {
        return true;
    }
    app.webview_windows()
        .keys()
        .any(|label| label.starts_with("props-") && visible(label))
}

/// 设置开关：wallpaper_auto_pause，默认关（与 macOS 同一份持久化）
fn auto_pause_enabled(app: &tauri::AppHandle) -> bool {
    app.try_state::<std::sync::Arc<Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| crate::db::get_setting(&c, "wallpaper_auto_pause"))
        })
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

/// 交互态下点桌面走的是壁纸窗口自己的鼠标事件（已直收），无需 macOS 那条
/// local event monitor 补丁；保留函数以对齐平台层签名。
pub fn start_desktop_click_monitor(_app: &tauri::AppHandle) {}

/// 读系统光标位置（逻辑坐标，左上原点）与左键状态。
///
/// 用 `GetCursorPos`（免权限、任意时刻有效，等价 macOS 的 CGEventSource 只读光标）
/// 与 `GetAsyncKeyState(VK_LBUTTON)`（按压态，donate 视差/hover/按压跟随一类效果）。
pub fn cursor_state() -> Option<(f64, f64, u32)> {
    let mut p = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut p).ok()?;
    }
    let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
    let scale = monitor_scale_at(p.x, p.y).unwrap_or(1.0);
    Some((p.x as f64 / scale, p.y as f64 / scale, u32::from(down)))
}

/// 命中某物理点的显示器 scale（不在任何屏内时用主屏）
fn monitor_scale_at(px: i32, py: i32) -> Option<f64> {
    // 保证缓存是新鲜的：指针轮询线程可能先于监控 tick 跑起来
    let _ = active_screens();
    let cache = SCREENS_CACHE.lock().ok()?;
    let (_, mons) = cache.as_ref()?;
    mons.iter()
        .find(|m| {
            px >= m.px.0 && px < m.px.0 + m.px.2 && py >= m.px.1 && py < m.px.1 + m.px.3
        })
        .or_else(|| mons.first())
        .map(|m| m.scale)
}

/// 未被 Windows 后端使用的占位：`GetForegroundWindow` 供排障日志用
#[allow(dead_code)]
pub fn foreground_window_is_desktop() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    !hwnd.is_invalid() && class_name_is_desktop(hwnd)
}
