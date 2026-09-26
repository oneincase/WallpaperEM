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
//!   NSWorkspaceDidActivateApplicationNotification 对齐，只作判定表的即时提示；
//!   暂停/恢复判据见 [`super::auto_pause`]（EnumWindows 遮挡快照，与 macOS
//!   同享「看得见就播」可见性判据）。
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
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetParent, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
    RegisterClassW, SendMessageTimeoutA, SetParent, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
    EVENT_SYSTEM_FOREGROUND, GWL_EXSTYLE, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, HWND_BOTTOM,
    HWND_MESSAGE, MSG, PBT_POWERSETTINGCHANGE, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT, WNDCLASSW,
    WM_POWERBROADCAST, WS_CHILD, WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WS_POPUP, WS_VISIBLE,
};

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    /// 稳定显示器 id（设备名的 FNV-1a 哈希，见 [`monitor_id`]）
    pub id: u32,
    /// 显示器名称（Win32 设备名，如 `\\.\DISPLAY1`；产品友好名需要 EnumDisplayDevicesW
    /// 查 EDID，暂以设备名展示）
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub scale: f64,
    pub is_primary: bool,
}

/// Windows 的显示器 id 用设备名哈希、跨重启稳定，无旧→新 迁移需求。
pub fn legacy_display_ids() -> Vec<(String, String)> {
    Vec::new()
}

/// 供电类型。Windows 侧暂未接（GetSystemPowerStatus），恒 None = 不拦
/// （「仅充电时轮播」暂仅 macOS 生效）。
pub fn on_ac_power() -> Option<bool> {
    None
}

/// 显示器名称缓存刷新（macOS 用：NSScreen 主线程限制）。Windows 无需缓存，空实现。
pub fn refresh_display_meta() {}

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

/// 被我们改成 `WS_CHILD` 的窗口 → 它原来的 `GWL_STYLE`。
/// Win11 的 raised-desktop 需要把壁纸窗口设成 Progman 的子窗口样式；切到交互态时要
/// 脱离父子关系，必须把样式原样还原（带 `WS_CHILD` 又没有父窗口的窗口不会正常显示）。
static ORIG_STYLE: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

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
        // 脱离桌面层（SetParent(None) + 还原样式），再压到 Z 序最底
        unsafe { detach_from_parent(hwnd) };
        unsafe { place_top_level(hwnd, phys) };
    } else {
        match unsafe { parent_to_worker(hwnd) } {
            Ok(parent) => {
                tracing::info!("windows: 已挂到桌面层（parent={}）", parent.0 as isize);
                unsafe { place_in_underlay(hwnd, phys) };
            }
            Err(e) => {
                tracing::warn!(
                    "windows: 挂到桌面层失败（{e}）—— 退回 Z 序最底，避免全屏盖住桌面与应用"
                );
                unsafe { place_top_level(hwnd, phys) };
            }
        }
    }
    // 窗口是 visible(false) 创建的：几何/层级设置完再显示，避免闪现未定位的窗口
    let _ = window.show();
    unsafe { dump_desktop_zorder(if interactive { "interactive" } else { "underlay" }, hwnd) };
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
        let r = unsafe { desktop_attach_target().and_then(|t| attach_inner(hwnd, &t)) };
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

/// 桌面层的挂载目标（两种 Windows 桌面结构各一套）
struct DesktopAttach {
    /// `SetParent` 的目标
    parent: HWND,
    /// 挂好后紧贴到这个窗口的**下方**（z 序）—— Win11 的图标层 SHELLDLL_DefView
    insert_after: Option<HWND>,
    /// 静态壁纸所在的 WorkerW：需压到 Progman 子窗口 z 序最底
    worker_bottom: Option<HWND>,
    /// 是否需把窗口改成 `WS_CHILD`（Win11 raised-desktop 才需要）
    child_style: bool,
}

/// 探测并返回当前桌面结构下的挂载目标。
///
/// Win11（近版本）把桌面拆成了「raised desktop」：Progman 自身带
/// `WS_EX_NOREDIRECTIONBITMAP`（不画任何 GDI 内容），`SHELLDLL_DefView`（图标，几乎
/// 全透明）与承载静态壁纸的 `WorkerW` 都成了 **Progman 的子窗口**。此时若还按老办法
/// 挂到那个「兄弟 WorkerW」上，壁纸就落在静态壁纸**下面**，表现为「被原生壁纸盖住」
/// —— 必须在 Progman 下建一个子窗口，并让它紧贴 `SHELLDLL_DefView` 之后（图标之下、
/// 静态壁纸 WorkerW 之上），这也是微软给第三方壁纸程序的官方指引。
unsafe fn desktop_attach_target() -> Result<DesktopAttach, String> {
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

        let ex = GetWindowLongPtrW(progman, GWL_EXSTYLE) as u32;
        if ex & (WS_EX_NOREDIRECTIONBITMAP.0 as u32) != 0 {
            let defview =
                FindWindowExA(Some(progman), None, windows::core::s!("SHELLDLL_DefView"), None)
                    .unwrap_or_default();
            let worker = FindWindowExA(Some(progman), None, windows::core::s!("WorkerW"), None)
                .unwrap_or_default();
            if !defview.is_invalid() {
                tracing::info!("windows: 检测到 Win11 raised desktop（Progman 带 WS_EX_NOREDIRECTIONBITMAP）");
                return Ok(DesktopAttach {
                    parent: progman,
                    insert_after: Some(defview),
                    worker_bottom: (!worker.is_invalid()).then_some(worker),
                    child_style: true,
                });
            }
        }

        // 经典结构（Win10 / 旧版 Win11）：找「承载 SHELLDLL_DefView 的窗口」之后的
        // WorkerW 兄弟，它就是图标之下那层
        let mut worker = HWND::default();
        let _ = EnumWindows(Some(enum_worker), LPARAM(&mut worker as *mut HWND as _));
        if worker.is_invalid() {
            worker = FindWindowExA(Some(progman), None, windows::core::s!("WorkerW"), None)
                .unwrap_or_default();
        }
        if worker.is_invalid() {
            return Err("未找到壁纸 WorkerW".into());
        }
        Ok(DesktopAttach {
            parent: worker,
            insert_after: None,
            worker_bottom: None,
            child_style: false,
        })
    }
}

/// 按目标描述完成挂载 + z 序摆放，并校验父窗口真的是目标。
unsafe fn attach_inner(hwnd: HWND, t: &DesktopAttach) -> Result<HWND, String> {
    unsafe {
        if t.child_style {
            // 记录原始样式，交互态脱离时原样还原（带 WS_CHILD 却无父的窗口不会正常显示）
            if let Ok(mut g) = ORIG_STYLE.lock() {
                let key = hwnd.0 as isize;
                if !g.iter().any(|(h, _)| *h == key) {
                    g.push((key, GetWindowLongPtrW(hwnd, GWL_STYLE)));
                }
            }
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE);
            let _ = SetWindowLongPtrW(
                hwnd,
                GWL_STYLE,
                (style | WS_CHILD.0 as isize) & !(WS_POPUP.0 as isize),
            );
        }
        // ⚠️ SetParent 返回的是「旧父窗口」：顶层窗口原本无父 ⇒ 返回 NULL，
        // 而 windows-rs 会把 NULL 判成 Err（哪怕调用其实成功了）。故忽略返回值，
        // 改用 GetParent 读回真实父窗口来校验。
        let _ = SetParent(hwnd, Some(t.parent));
        if GetParent(hwnd).ok().filter(|p| !p.is_invalid()) != Some(t.parent) {
            return Err(format!(
                "SetParent 后父窗口不是目标（target={}）",
                t.parent.0 as isize
            ));
        }
        if let Some(anchor) = t.insert_after {
            // 紧贴锚点窗口的**下方**：SWP_NOMOVE|SWP_NOSIZE 只动 z 序
            let _ = SetWindowPos(
                hwnd,
                Some(anchor),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        if let Some(worker) = t.worker_bottom {
            // 静态壁纸那层压到最底，别让它盖住我们
            let _ = SetWindowPos(
                worker,
                Some(HWND_BOTTOM),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        Ok(t.parent)
    }
}

/// 解除父子关系（回到顶层窗口），并还原被我们改成 `WS_CHILD` 的样式
unsafe fn detach_from_parent(hwnd: HWND) {
    unsafe {
        // 同 attach_inner：返回值是旧父窗口，NULL 会被误判成 Err，忽略之
        let _ = SetParent(hwnd, None);
        if let Ok(mut g) = ORIG_STYLE.lock() {
            let key = hwnd.0 as isize;
            if let Some(i) = g.iter().position(|(h, _)| *h == key) {
                let (_, style) = g.remove(i);
                let _ = SetWindowLongPtrW(hwnd, GWL_STYLE, style);
            }
        }
    }
}

/// 诊断：按 z 序（前 = 更靠上）打印 Progman 的子窗口，并标出我们的窗口。
///
/// 专治「新建时挂错层、手动开关一次就正常」这类问题：同一段挂载代码在两种时机
/// 得到不同结果，只有把真实的子窗口顺序打出来才能看清窗口被插到了谁的前后。
unsafe fn dump_desktop_zorder(tag: &str, me: HWND) {
    unsafe {
        let Ok(progman) = FindWindowA(windows::core::s!("Progman"), None) else {
            return;
        };
        tracing::info!(
            "windows[z/{tag}]: progman={} ex=0x{:X}",
            progman.0 as isize,
            GetWindowLongPtrW(progman, GWL_EXSTYLE) as u32
        );
        let mut child = GetWindow(progman, GW_CHILD).unwrap_or_default();
        let mut n = 0;
        while !child.is_invalid() && n < 10 {
            let mut buf = [0u16; 96];
            let len = GetClassNameW(child, &mut buf).max(0) as usize;
            let class = String::from_utf16_lossy(&buf[..len.min(buf.len())]);
            let style = GetWindowLongPtrW(child, GWL_STYLE) as u32;
            let ex = GetWindowLongPtrW(child, GWL_EXSTYLE) as u32;
            tracing::info!(
                "windows[z/{tag}]: #{n}{} hwnd={} class={class:?} vis={} child={} layered={} noredir={}",
                if child == me { " <== OUR WINDOW" } else { "" },
                child.0 as isize,
                style & (WS_VISIBLE.0 as u32) != 0,
                style & (WS_CHILD.0 as u32) != 0,
                ex & (WS_EX_LAYERED.0 as u32) != 0,
                ex & (WS_EX_NOREDIRECTIONBITMAP.0 as u32) != 0,
            );
            child = GetWindow(child, GW_HWNDNEXT).unwrap_or_default();
            n += 1;
        }
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

/// 前台窗口变化：指针注入门控 + 把前台类型当即时提示（Hint）交给共享判定表
/// （[`super::auto_pause`]）。判定的**判据**是逐屏可见性（[`occlusion_snapshot`]），
/// 前台类型只作 Hint 加速与快照缺失时的兜底语义。
fn on_foreground_changed(app: &tauri::AppHandle, hwnd: HWND) {
    use super::auto_pause::{recheck_with, settings_window_visible, FrontKind, Mode};
    let kind = kind_of_hwnd(hwnd);
    // 指针注入门控：桌面活动（Progman/WorkerW 前台 / 壁纸窗口被点）才注入。
    // 「本应用前台且设置窗不可见」= 交互态点了壁纸窗口，按桌面语义
    let is_desktop = match kind {
        FrontKind::Desktop => true,
        FrontKind::SelfApp => !settings_window_visible(app),
        _ => false,
    };
    super::pointer::set_desktop_active(is_desktop);
    recheck_with(app, Mode::Hint, kind);
}

/// 窗口 → 前台类型：本进程窗口 = SelfApp（判定表再分「点壁纸」/「设置窗」），
/// 桌面类名（Progman/WorkerW/图标视图）= Desktop，其余 = Other。
fn kind_of_hwnd(hwnd: HWND) -> super::auto_pause::FrontKind {
    use super::auto_pause::FrontKind;
    let own_process = std::process::id();
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == own_process {
        FrontKind::SelfApp
    } else if class_name_is_desktop(hwnd) {
        FrontKind::Desktop
    } else {
        FrontKind::Other
    }
}

/// 当前前台窗口类型（判定表轮询路径用）。查询失败返回 Unknown。
pub fn frontmost_kind(_app: &tauri::AppHandle) -> super::auto_pause::FrontKind {
    use super::auto_pause::FrontKind;
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return FrontKind::Unknown;
    }
    kind_of_hwnd(hwnd)
}

/// 遮挡快照（Windows）：逐屏遮挡比例（物理虚拟桌面坐标），自动暂停
/// 「看得见就播」的可见性判据（与 macOS 的 CGWindowList 实现同构）。
///
/// 数据源 `EnumWindows` 顶层窗口 + Tauri `available_monitors`（物理坐标）。
/// 过滤规则：
/// - 本进程窗口（设置窗/交互态壁纸窗）不算遮挡 —— 我们是壁纸系统自身；
///   非交互态壁纸窗是 WorkerW 的**子**窗口，EnumWindows 本来就不枚举；
/// - 桌面类名（Progman/WorkerW/图标视图）铺满整屏，必须排除；
/// - 不可见/最小化的跳过；**DWM cloak 的跳过** —— 别的虚拟桌面上的窗口与
///   挂起的 UWP 窗口 `IsWindowVisible` 仍为真但并不显示，不过滤会让
///   「切到空虚拟桌面」永远判成有遮挡（这正是要修的 bug 形态）；
/// - 其余可见顶层窗口都算（含任务栏：它确实盖着壁纸一条，单靠它过不了阈值）。
///
/// 坐标：枚举前后把线程钉在 `DPI_AWARENESS_CONTEXT_SYSTEM_AWARE` 并还原 ——
/// GetWindowRect 与显示器物理坐标同处一个坐标系（遮挡比例与单位无关，
/// 只要两边一致）。
pub fn occlusion_snapshot() -> Option<super::auto_pause::OcclusionSnapshot> {
    use super::auto_pause::{covered_ratio, OcclusionSnapshot};
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};

    struct EnumCtx {
        own_pid: u32,
        rects: Vec<(f64, f64, f64, f64)>,
    }
    unsafe extern "system" fn collect(window: HWND, out: LPARAM) -> BOOL {
        unsafe {
            let ctx = &mut *(out.0 as *mut EnumCtx);
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if pid == ctx.own_pid {
                return true.into();
            }
            if !IsWindowVisible(window).as_bool() || IsIconic(window).as_bool() {
                return true.into();
            }
            let mut cloaked: u32 = 0;
            let _ = DwmGetWindowAttribute(
                window,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut _,
                std::mem::size_of::<u32>() as u32,
            );
            if cloaked != 0 {
                return true.into();
            }
            if class_name_is_desktop(window) {
                return true.into();
            }
            let mut r = RECT::default();
            if GetWindowRect(window, &mut r).is_ok() && r.right > r.left && r.bottom > r.top {
                ctx.rects
                    .push((r.left as f64, r.top as f64, r.right as f64, r.bottom as f64));
            }
            true.into()
        }
    }

    let app = APP.get()?;
    // 显示器（物理虚拟桌面坐标 + 稳定 id，与 active_screens 同一套 id 推导）
    let monitors: Vec<(u32, (f64, f64, f64, f64))> = app
        .available_monitors()
        .ok()?
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let p = m.position();
            let s = m.size();
            let x = p.x as f64;
            let y = p.y as f64;
            (
                monitor_id(m.name().map(|n| n.as_str()), i),
                (x, y, x + s.width as f64, y + s.height as f64),
            )
        })
        .collect();
    if monitors.is_empty() {
        return None;
    }

    let prev = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE) };
    let mut ctx = EnumCtx {
        own_pid: std::process::id(),
        rects: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut ctx as *mut EnumCtx as _));
    }
    unsafe {
        SetThreadDpiAwarenessContext(prev);
    }

    let coverages = monitors
        .iter()
        .map(|(id, rect)| (*id, covered_ratio(&ctx.rects, *rect)))
        .collect();
    Some(OcclusionSnapshot { coverages })
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

/// 全局物理像素点（GetCursorPos / 低级钩子坐标）落在哪台显示器，返回其 id。
/// 滚轮派发用；不在任何屏内返回 None（这次事件丢弃）。
pub fn hit_screen_id(x: f64, y: f64) -> Option<u32> {
    let (xi, yi) = (x as i32, y as i32);
    // 不能在这里调 active_screens()（先 fill 再取会重复加锁同一把非重入锁）；
    // 缓存由指针轮询以 30Hz 保鲜，启动瞬间 miss 只是丢一次滚轮事件
    let cache = SCREENS_CACHE.lock().ok()?;
    let (_, mons) = cache.as_ref()?;
    mons.iter()
        .find(|m| xi >= m.px.0 && xi < m.px.0 + m.px.2 && yi >= m.px.1 && yi < m.px.1 + m.px.3)
        .map(|m| m.info.id)
}

/// 未被 Windows 后端使用的占位：`GetForegroundWindow` 供排障日志用
#[allow(dead_code)]
pub fn foreground_window_is_desktop() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    !hwnd.is_invalid() && class_name_is_desktop(hwnd)
}
