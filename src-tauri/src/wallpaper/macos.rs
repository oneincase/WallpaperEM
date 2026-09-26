//! macOS 桌面层窗口与屏幕枚举（T3：多显示器）
//!
//! - 屏幕枚举/睡眠检测用 CoreGraphics C API（CGGetActiveDisplayList/CGDisplayIsAsleep），
//!   避免 objc2 的 NSScreen 复杂遍历；
//! - 窗口的「桌面 underlay」层级（位于桌面壁纸与图标之间）由
//!   `tauri-plugin-desktop-underlay` 统一管理（macOS 实现 = CGWindowLevelForKey(2)-1）；
//! - 「隐藏图标」开启时 → `set_desktop_underlay(false)`（回到 normal 层级，窗口在桌面图标之上）；
//!   「隐藏图标」关闭（默认）→ `set_desktop_underlay(true)`（壁纸窗口位于桌面图标下方、壁纸上方）。
//! - 关键修复（T0.5 验证）：orderFrontRegardless + show 后重设层级，否则被遮挡的
//!   WKWebView 动态内容不合成到屏幕。

use std::ffi::c_void;
use tauri::{Runtime, WebviewWindow};

// 层级说明（WindowServer 实际值）：
//   WindowServer 桌面画 = -2147483626
//   程序坞 (Dock)       = -2147483624
//   kCGDesktopWindowLevel = -2147483623
//
// 「隐藏图标」开启时（interactive=true）：窗口回到 normal 层级 → 位于桌面图标之上并接收鼠标
// 「隐藏图标」关闭（interactive=false，默认）：由 desktop-underlay 插件置为 CGWindowLevelForKey(2)-1
//   → 位于桌面图标下方、壁纸上方

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
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut u32,
        display_count: *mut u32,
    ) -> i32;
    fn CGDisplayBounds(display: u32) -> CRect;
    fn CGDisplayIsAsleep(display: u32) -> bool;
    fn CGWindowLevelForKey(key: i32) -> i32;
    fn CGDisplayCreateUUIDFromDisplayID(display: u32) -> *mut c_void;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *mut c_void;
    fn CGRectMakeWithDictionaryRepresentation(dict: *mut c_void, rect: *mut CRect) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFUUIDCreateString(alloc: *mut c_void, uuid: *mut c_void) -> *mut c_void;
    fn CFStringGetCString(
        s: *mut c_void,
        buf: *mut std::os::raw::c_char,
        size: usize,
        enc: u32,
    ) -> bool;
    fn CFRelease(cf: *mut c_void);
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPSGetProvidingPowerSourceType() -> *mut c_void;
}

/// 供电类型：Some(true)=交流电源、Some(false)=电池；查询失败返回 None（视为不拦，
/// 「仅充电时轮播」的语义是宁可多播不漏播）。macOS 台式机恒为 AC。
pub fn on_ac_power() -> Option<bool> {
    unsafe {
        // 返回常量 CFString（Get 规则，不释放）："AC Power" | "Battery Power"
        let state = IOPSGetProvidingPowerSourceType();
        if state.is_null() {
            return None;
        }
        let mut buf = [0 as std::os::raw::c_char; 32];
        let ok = CFStringGetCString(state, buf.as_mut_ptr(), buf.len(), CF_STRING_ENCODING_UTF8);
        if !ok {
            return None;
        }
        let s = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
        if s.contains("AC") {
            Some(true)
        } else if s.contains("Battery") {
            Some(false)
        } else {
            None
        }
    }
}

/// kCFStringEncodingUTF8
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

/// 本机实测的 CGWindowLevelForKey 取值（macOS 26 / Tahoe）：
///
/// ```text
/// -2147483626  WindowServer  Display 1 Backstop
/// -2147483625  墙纸           Offscreen Wallpaper Window
/// -2147483624  程序坞         Wallpaper-<UUID>      ← 系统壁纸（= desktop-1）
/// -2147483623  kCGDesktopWindowLevel (key 2)
/// -2147483622  程序坞         Fullscreen Backdrop
/// -2147483603  kCGDesktopIconWindowLevel (key 18)  ← 桌面图标
/// ```
///
/// `desktop_underlay` 插件用的是 `desktop-1`，那是**系统壁纸自己的层**：
/// 与系统壁纸同层时先后顺序由 order 决定，而我们在末尾还要
/// `orderFrontRegardless()` 保证 WKWebView 合成，于是必然压在系统壁纸之上
/// —— 这一半是符合预期的（我们就是要替换壁纸）。
///
/// 真正的问题是它**离桌面图标层太远也没关系，但插件的 unset 会跳到
/// normal(0)**，那才是「盖住图标」的来源。所以这里不再用插件的二值语义，
/// 直接按需要的目标层显式 setLevel：
/// - 非交互（隐藏图标关闭）：`desktop-1`，在图标（desktopIcon）之下；
/// - 交互（隐藏图标开启）：`desktopIcon+1`，刚好压过图标一层，
///   而不是跳到 normal(0) —— normal 会连普通应用窗口一起盖住。
fn target_window_level(interactive: bool) -> i32 {
    unsafe {
        if interactive {
            // key 18 = kCGDesktopIconWindowLevelKey
            CGWindowLevelForKey(18) + 1
        } else {
            // key 2 = kCGDesktopWindowLevelKey
            CGWindowLevelForKey(2) - 1
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScreenInfo {
    /// 稳定显示器 id（显示器 UUID 的 FNV-1a 哈希；跨重启不变，会话恢复才对得上）
    pub id: u32,
    /// 显示器名称（NSScreen.localizedName，如 "内建 Liquid Retina XDR 显示器"）
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// backing scale（像素宽 / points 宽）
    pub scale: f64,
    pub is_primary: bool,
}

/// FNV-1a 32bit → 显示器数值 id（与 Windows/Linux 后端的 monitor_id 同一套哈希）。
/// 避开 0（wallpaper-0 与「无 id」语义冲突）。
fn hash_id(key: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in key.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// CGDirectDisplayID → 稳定 id 的缓存。active_screens 被指针轮询以 ~30Hz 调用，
/// CFUUID 查询不能每次都跑；显示器集合变化（重启/热插拔）自然会带来新键。
/// （条目 ≤16，线性扫即可，不必 HashMap）
static STABLE_IDS: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

/// 显示器的跨重启稳定 id：显示器 UUID 的哈希。
///
/// 为什么不用 CGDirectDisplayID 直接当 id：它只在本次开机内稳定，重启后同块屏
/// 会拿到不同数值，`wallpaper_sessions` 的每屏会话就全对不上了（表现为「重启后
/// 壁纸跑到别的屏上」）。UUID 是 EDID 派生的持久标识，重启/热插拔不变。
/// 拿不到 UUID 时退回运行时 id（会话恢复降级为旧行为）。
fn stable_display_id(cg_id: u32) -> u32 {
    if let Ok(cache) = STABLE_IDS.lock() {
        if let Some((_, v)) = cache.iter().find(|(c, _)| *c == cg_id) {
            return *v;
        }
    }
    let id = unsafe {
        let uuid = CGDisplayCreateUUIDFromDisplayID(cg_id);
        if uuid.is_null() {
            return cg_id;
        }
        let cfstr = CFUUIDCreateString(std::ptr::null_mut(), uuid);
        CFRelease(uuid);
        if cfstr.is_null() {
            return cg_id;
        }
        let mut buf = [0 as std::os::raw::c_char; 128];
        let ok = CFStringGetCString(cfstr, buf.as_mut_ptr(), buf.len(), CF_STRING_ENCODING_UTF8);
        CFRelease(cfstr);
        if !ok {
            return cg_id;
        }
        hash_id(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().as_ref())
    };
    if let Ok(mut cache) = STABLE_IDS.lock() {
        cache.push((cg_id, id));
    }
    id
}

/// CGDisplayID → (显示器名称, backing scale)。NSScreen 是 AppKit 对象、只能主线程碰，
/// 故由 [`refresh_display_meta`]（主线程调用：init 与 2s 监控 tick）写入，其余线程只读。
/// scale 取 NSScreen.backingScaleFactor —— CGDisplayPixelsWide/Bounds 之比在
/// 虚拟化/缩放模式的机器上会恒为 1.0，不是 Retina 倍率的可靠来源。
static META: std::sync::Mutex<Vec<(u32, String, f64)>> = std::sync::Mutex::new(Vec::new());

/// 刷新显示器名称缓存（NSScreen 只能在主线程访问，非主线程调用直接返回）。
/// 名称只用于 UI 展示，读不到就降级为「显示器 N」，不影响任何会话逻辑。
pub fn refresh_display_meta() {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSScreen;
    use objc2_foundation::NSString;
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    let screens = NSScreen::screens(mtm);
    let mut out: Vec<(u32, String, f64)> = Vec::new();
    for (i, screen) in screens.iter().enumerate() {
        let name = screen.localizedName().to_string();
        let scale = screen.backingScaleFactor() as f64;
        // NSScreenNumber（= CGDirectDisplayID）在 deviceDescription 字典里；
        // 键常量带 NSGraphics feature 门，这里用裸 msg_send 取，免受 feature 影响
        let cg_id: Option<u32> = unsafe {
            let desc: *mut AnyObject = msg_send![&*screen, deviceDescription];
            if desc.is_null() {
                None
            } else {
                let key = NSString::from_str("NSScreenNumber");
                let num: *mut AnyObject = msg_send![desc, objectForKey: &*key];
                if num.is_null() {
                    None
                } else {
                    Some(msg_send![num, unsignedIntValue])
                }
            }
        };
        match cg_id {
            Some(id) => out.push((id, name, scale)),
            // 匹配不上 CG id 时按顺序兜底命名，不参与 id 映射
            None => tracing::debug!("refresh_display_meta: 第 {} 块屏拿不到 NSScreenNumber", i + 1),
        }
    }
    if let Ok(mut g) = META.lock() {
        *g = out;
    }
}

/// 旧版本把 CGDirectDisplayID 直接当 display_id 持久化（重启后会漂移），稳定 id
/// 上线时按当前已连接显示器做一次性 旧→新 改写。返回 (旧 id 串, 新 id 串) 列表，
/// 仅含两者不同的显示器；未连接的显示器无法反查 UUID，其旧行保留（下次手动应用覆盖）。
pub fn legacy_display_ids() -> Vec<(String, String)> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count);
    }
    (0..(count.min(16) as usize))
        .map(|i| ids[i])
        .filter(|cg_id| stable_display_id(*cg_id) != *cg_id)
        .map(|cg_id| (cg_id.to_string(), stable_display_id(cg_id).to_string()))
        .collect()
}

/// 活动显示器列表（points 坐标）
pub fn active_screens() -> Vec<ScreenInfo> {
    let mut ids = [0u32; 16];
    let mut count: u32 = 0;
    unsafe {
        CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count);
    }
    let primary = unsafe { CGMainDisplayID() };
    let meta = META.lock().map(|g| g.clone()).unwrap_or_default();
    let mut out = Vec::new();
    for i in 0..(count.min(16) as usize) {
        let cg_id = ids[i];
        let b = unsafe { CGDisplayBounds(cg_id) };
        let m = meta.iter().find(|(c, _, _)| *c == cg_id);
        let name = m
            .map(|(_, n, _)| n.clone())
            .unwrap_or_else(|| format!("显示器 {}", out.len() + 1));
        // 名称缓存未就绪（首帧 / 非主线程首查）时退回 像素/points 估算
        let scale = match m.map(|(_, _, s)| *s) {
            Some(s) if s > 0.0 => s,
            _ => {
                let px_w = unsafe { CGDisplayPixelsWide(cg_id) } as f64;
                if b.size.width > 0.0 && px_w > 0.0 {
                    px_w / b.size.width
                } else {
                    1.0
                }
            }
        };
        out.push(ScreenInfo {
            id: stable_display_id(cg_id),
            name,
            x: b.origin.x,
            y: b.origin.y,
            w: b.size.width,
            h: b.size.height,
            scale,
            is_primary: cg_id == primary,
        });
    }
    out
}

/// 全局点（points，左上原点）落在哪块活动显示器上，返回其 id。
/// 滚轮事件派发用：CGEventGetLocation 与 CGDisplayBounds 同一坐标系。
pub fn hit_screen_id(x: f64, y: f64) -> Option<u32> {
    active_screens()
        .into_iter()
        .find(|s| x >= s.x && x < s.x + s.w && y >= s.y && y < s.y + s.h)
        .map(|s| s.id)
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
                    size: objc2_foundation::NSSize {
                        width: w,
                        height: h,
                    },
                },
                true,
            );
        }
    }
}

/// 应用桌面层属性（必须在主线程调用）。
///
/// `interactive=false`（「隐藏图标」关闭，默认）：层级 `desktop-1`，壁纸在桌面
/// 图标**下方**；此时窗口收不到真实鼠标事件（图标层在上面截走了），交互靠宿主
/// 轮询系统鼠标 + `__wp.pushPointer` 注入。
///
/// `interactive=true`（「隐藏图标」开启）：层级 `desktopIcon+1`，壁纸盖住桌面
/// 图标并直接接收真实鼠标事件；此时**不能**再注入，否则同一次移动会被处理两遍。
pub fn apply_desktop_window<R: Runtime>(
    window: &tauri::WebviewWindow<R>,
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
    let level = target_window_level(interactive);
    unsafe {
        if let Some(win) = retain_window(ptr) {
            win.setLevel(level as isize);
            win.setCollectionBehavior(
                objc2_app_kit::NSWindowCollectionBehavior::CanJoinAllSpaces
                    | objc2_app_kit::NSWindowCollectionBehavior::Stationary
                    | objc2_app_kit::NSWindowCollectionBehavior::IgnoresCycle
                    // 桌面级无边框窗口也允许跨满屏（含顶部菜单栏区域），
                    // 否则顶部会露出一条桌面壁纸（渐变浅灰）。
                    | objc2_app_kit::NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            // 非交互态在图标下方，本来也收不到事件；置 false 是为了不去抢
            // 那些「穿过壁纸落到桌面」的点击（右键菜单、框选图标）。
            win.setIgnoresMouseEvents(!interactive);
            // 接收「鼠标移动」事件（场景视差 / 网页 hover 需要）。
            // 非交互态由 pushPointer 注入，不需要原生事件。
            win.setAcceptsMouseMovedEvents(interactive);
            win.setOpaque(false);
            let clear = objc2_app_kit::NSColor::clearColor();
            win.setBackgroundColor(Some(&clear));
            win.setHasShadow(false);
            win.setHidesOnDeactivate(false);
            // ⌘H / Dock→隐藏 只应影响主窗口：canHide=false 让桌面壁纸窗口
            // 不跟随 App 隐藏（壁纸引擎的"隐藏应用"不该把桌面一起藏起来）
            win.setCanHide(false);
            win.setFrame_display(
                objc2_foundation::NSRect {
                    origin: objc2_foundation::NSPoint {
                        x: frame.0,
                        y: frame.1,
                    },
                    size: objc2_foundation::NSSize {
                        width: frame.2,
                        height: frame.3,
                    },
                },
                true,
            );
            // 关键修复：show 之后重设层级 + 强制前置合成（防 WKWebView 被遮挡暂停渲染）。
            // orderFrontRegardless 只在**本层级内**提到最前，不会跨层跳到图标之上。
            win.setLevel(level as isize);
            win.orderFrontRegardless();
            // 黑屏探针：NSWindow 遮挡状态（NSWindowOcclusionStateVisible = 1<<1）。
            // WebKit 用视图所在窗口的遮挡态决定页面可见性；不可见 → 停帧 → 黑屏。
            let occ: usize = objc2::msg_send![&win, occlusionState];
            tracing::info!(
                "apply_desktop_window occlusionState={occ} (visible_bit={})",
                occ & 2 != 0
            );
        }
        // 私有 KVC 探测（macOS 26 不支持，安全忽略）
        if let Ok(view) = window.ns_view() {
            set_webview_update_while_hidden(view);
            // 原生右键菜单屏蔽（详见函数注释；进程内只安装一次）
            install_context_menu_block(view);
        }
    }
    tracing::info!(
        "apply_desktop_window ok (interactive={interactive}, level={level}, frame={:?})",
        frame
    );
}

/// 定位 wry 的运行时子类（其父类 = WKWebView 的那一层）。
///
/// `window.ns_view()` 拿到的不是 WKWebView 本体：wry 0.55 在窗口里套了一个
/// 容器视图（WryWebViewParent），WKWebView 是它的子视图。所以从传入视图做
/// 深度优先遍历，对每个候选沿父类链向上找 WKWebView，命中时返回它正下方
/// 那一层（即 wry 的子类；KVO 动态子类 NSKVONotifying_* 会被自然跳过）。
fn find_wry_webview_class(view: *mut c_void) -> Option<&'static objc2::runtime::AnyClass> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    if view.is_null() {
        return None;
    }
    unsafe {
        let mut stack: Vec<*mut c_void> = vec![view];
        while let Some(v) = stack.pop() {
            if v.is_null() {
                continue;
            }
            let cls = objc2::ffi::object_getClass(v.cast());
            let mut cur: Option<&AnyClass> = cls.as_ref();
            let mut below: Option<&AnyClass> = None;
            while let Some(c) = cur {
                if c.name() == c"WKWebView" {
                    return below;
                }
                below = Some(c);
                cur = c.superclass();
            }
            // 子视图入栈继续找（NSArray 元素 +0 借用，父视图持有，主线程上安全）
            let subs: *mut AnyObject = msg_send![v.cast::<AnyObject>(), subviews];
            if subs.is_null() {
                continue;
            }
            let count: usize = msg_send![&*subs, count];
            for i in 0..count {
                let s: *mut AnyObject = msg_send![&*subs, objectAtIndex: i];
                stack.push(s.cast::<c_void>());
            }
        }
        None
    }
}

/// 在视图树里找 WKWebView 本体（wry 在窗口里套了一层容器视图，它才是子视图）。
fn find_webview(view: *mut c_void) -> Option<*mut objc2::runtime::AnyObject> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    if view.is_null() {
        return None;
    }
    unsafe {
        let mut stack: Vec<*mut c_void> = vec![view];
        while let Some(v) = stack.pop() {
            if v.is_null() {
                continue;
            }
            let cls = objc2::ffi::object_getClass(v.cast());
            let mut cur: Option<&AnyClass> = cls.as_ref();
            while let Some(c) = cur {
                if c.name() == c"WKWebView" {
                    return Some(v.cast::<AnyObject>());
                }
                cur = c.superclass();
            }
            let subs: *mut AnyObject = msg_send![v.cast::<AnyObject>(), subviews];
            if subs.is_null() {
                continue;
            }
            let count: usize = msg_send![&*subs, count];
            for i in 0..count {
                let s: *mut AnyObject = msg_send![&*subs, objectAtIndex: i];
                stack.push(s.cast::<c_void>());
            }
        }
        None
    }
}

/// 读这扇窗口的 WebContent 进程 pid（私有 API `_webProcessIdentifier`，WKWebViewPrivate.h）。
///
/// **必须在主线程调用**（取视图树、问 WKWebView 都只能在主线程），且必须在**窗口还活着**
/// 的时候调用 —— 窗口一销毁 WKWebView 就没了，pid 再也问不到。所以调用方的顺序是
/// 「主线程取 pid → `destroy()` → 按 pid 结束进程」。
///
/// 返回 None 的几种情况：选择器不可用（老系统）、视图树里没有 WKWebView、页面还没起
/// 独立进程（`pid <= 0`）。此时调用方退回「导航到 about:blank 再销毁」的老路。
pub fn web_content_pid<R: Runtime>(window: &WebviewWindow<R>) -> Option<i32> {
    use objc2::msg_send;
    use objc2::runtime::NSObject;
    if objc2::MainThreadMarker::new().is_none() {
        tracing::warn!("web_content_pid 必须在主线程调用");
        return None;
    }
    let view = window.ns_view().ok()?;
    let Some(webview) = find_webview(view) else {
        tracing::warn!("web_content_pid: 视图树里没找到 WKWebView");
        return None;
    };
    let obj = webview.cast::<NSObject>();
    let sel = objc2::sel!(_webProcessIdentifier);
    let responds: bool = unsafe { msg_send![&*obj, respondsToSelector: sel] };
    if !responds {
        tracing::warn!("web_content_pid: 这版 WebKit 没有 _webProcessIdentifier");
        return None;
    }
    let pid: i32 = unsafe { msg_send![&*obj, _webProcessIdentifier] };
    (pid > 0).then_some(pid)
}

/// 结束一个 WebContent 进程：发 SIGKILL，再等它真的从进程表里消失。
///
/// 为什么是自己发信号，而不是调 WebKit 那两个私有选择器
///（`_killWebContentProcess` / `_killWebContentProcessAndResetState`）：在 macOS 26
/// 的 WebKit 上它们**调用有返回、进程却纹丝不动** —— 实测每换一次壁纸都"成功"结束一次，
/// WebContent 进程数只增不减、内存一 MB 都不还（一路堆到 11 个进程 / 4.3GB），而同一批
/// 进程用 `kill -9` 立刻归还（4.3GB → 1.4GB，以 `about:` 记名的 1.18GB 残留当场消失）。
/// 既然 pid 已经在手上，直接发信号才是真能还内存的那条路。
///
/// 为什么不能用 `destroy()` 代替：它只是把 WKWebView 从窗口上摘下来，WebKit 把进程留在
/// 进程池里且**不保证**还内存。进程真死了之后，按标识删数据存储也不会再撞
/// `Data store is in use`（删不掉的原因正是还有活进程攥着那份存储）。
///
/// 发信号前会确认这个 pid 现在仍是 WebKit 的进程（[`crate::mem_watch::is_webkit_process`]）：
/// 从取 pid 到这里隔着一次窗口销毁，pid 可能已被系统回收给别人，那时再 SIGKILL 就是误杀。
///
/// 会阻塞到确认进程消失为止（正常几毫秒，上限 [`KILL_WAIT`]），**不要在主线程调用**。
pub fn kill_web_content_process(pid: i32) -> bool {
    use std::time::{Duration, Instant};
    /// 等进程消失的上限。SIGKILL 是内核直接处理，正常几毫秒就没了；久等不来
    /// 说明它正卡在不可中断的退出流程里，再等也没意义。
    const KILL_WAIT: Duration = Duration::from_millis(500);
    if pid <= 0 {
        return false;
    }
    // SAFETY: 只对正数 pid 发信号 / 探活，信号量是常量
    if unsafe { libc::kill(pid, 0) } != 0 {
        // 已经没了：destroy() 之后 WebKit 自己把它收掉了。目的已经达到。
        tracing::debug!("wallpaper window: WebContent 进程 {pid} 在发信号前已自行退出");
        return true;
    }
    if !crate::mem_watch::is_webkit_process(pid) {
        tracing::warn!(
            "wallpaper window: pid {pid} 已不是 WebKit 进程，跳过结束（pid 可能已被回收）"
        );
        return false;
    }
    if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
        let e = std::io::Error::last_os_error();
        // 竞态：探活与发信号之间它自己退了
        if e.raw_os_error() == Some(libc::ESRCH) {
            tracing::debug!("wallpaper window: WebContent 进程 {pid} 已自行退出");
            return true;
        }
        tracing::warn!("wallpaper window: 结束 WebContent 进程 {pid} 失败（{e}）");
        return false;
    }
    let started = Instant::now();
    while started.elapsed() < KILL_WAIT {
        if unsafe { libc::kill(pid, 0) } != 0 {
            tracing::info!(
                "wallpaper window: WebContent 进程 {pid} 已结束（SIGKILL，{}ms 后消失）",
                started.elapsed().as_millis()
            );
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // 信号已经发出去了，内存会在这之后还回来；这里只如实报告没等到消失
    tracing::warn!(
        "wallpaper window: WebContent 进程 {pid} 收到 SIGKILL 后 {}ms 仍未从进程表消失",
        KILL_WAIT.as_millis()
    );
    true
}

/// 屏蔽壁纸窗口的原生右键菜单（WKWebView 默认上下文菜单）。
///
/// 背景：「隐藏图标」开启后壁纸窗口位于桌面图标之上、直接接收原生鼠标事件，
/// 右键会弹出 WKWebView 的默认上下文菜单（重新载入 / 存储图像 / 检查元素等）。
/// 渲染器 JS 层虽有 contextmenu preventDefault（renderer/src/main.ts），但
/// WKWebView 的 macOS 原生菜单由 UI 进程组装，页面侧拦截并不可靠
/// （视频 / 链接等元素上尤其如此），所以原生层再补一道。
///
/// 做法：给 wry 运行时注册的 WryWebView 类（WKWebView 子类）挂一个
/// willOpenMenu:withEvent: 覆写 —— WKWebView 弹上下文菜单前会调它，
/// 清空菜单项即不再弹出，而 rightMouseDown / DOM contextmenu 事件照常
/// 到达页面（交互壁纸的右键手势不受影响）。按窗口层级限定作用域：
/// 桌面级窗口（level < 0，即本文件的两种壁纸层级）屏蔽；普通窗口
/// （主设置窗 / 属性窗，level = 0）转发回 WKWebView 原实现，保留输入框
/// 的复制粘贴等系统菜单。
fn install_context_menu_block(view: *mut c_void) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};

        type WillOpenMenuFn =
            unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, *mut AnyObject);

        // IMP 由 Objective-C 运行时装机调用；入参指针按运行期规则非空，
        // 但仍做防御性判空（窗口销毁竞态下 window 可为 null）。
        unsafe extern "C-unwind" fn will_open_menu(
            this: *mut AnyObject,
            _cmd: Sel,
            menu: *mut AnyObject,
            event: *mut AnyObject,
        ) {
            unsafe {
                let window: *mut AnyObject = objc2::msg_send![this, window];
                let level: isize = if window.is_null() {
                    0
                } else {
                    objc2::msg_send![&*window, level]
                };
                if level < 0 {
                    // 桌面层壁纸窗口：清空菜单项，空菜单不会弹出
                    if !menu.is_null() {
                        let _: () = objc2::msg_send![&*menu, removeAllItems];
                    }
                    return;
                }
                // 普通窗口：转发 WKWebView 原实现（等价 super 调用）
                if let Some(supercls) = AnyClass::get(c"WKWebView") {
                    let _: () = objc2::msg_send![super(this, supercls), willOpenMenu: menu, withEvent: event];
                }
            }
        }

        // 不硬编码类名：objc2 0.6 的 define_class! 会把类名自动生成为
        // 「模块路径::类名+版本号」（如 wry::...::WryWebView0.55.1），写死
        // "WryWebView" 会查不到。从活实例沿父类链找到「父类是 WKWebView」
        // 的那一层，即 wry 的运行时子类。
        let Some(cls) = find_wry_webview_class(view) else {
            tracing::warn!("context-menu block: 未找到 wry 的 WKWebView 子类，跳过");
            return;
        };
        let sel = objc2::sel!(willOpenMenu:withEvent:);
        // 具体签名的 fn 指针与抹掉签名的 Imp（extern "C-unwind" fn()）布局相同，
        // 但 Rust 不允许跨签名 as 强转，走 transmute（ABI 由 "v@:@@" 编码保证）
        let imp: Imp = unsafe { std::mem::transmute(will_open_menu as WillOpenMenuFn) };
        let added = unsafe {
            objc2::ffi::class_addMethod(
                cls as *const AnyClass as *mut AnyClass,
                sel,
                imp,
                c"v@:@@".as_ptr(),
            )
        };
        if added.as_bool() {
            tracing::info!("context-menu block: willOpenMenu 覆写已安装（仅桌面级窗口生效）");
        } else {
            tracing::warn!("context-menu block: class_addMethod 失败（wry 已自行覆写？）");
        }
    });
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

/// 允许点击窗口背景（非交互区域）直接拖动窗口（NSWindow.movableByWindowBackground）。
///
/// WKWebView 的原生行为：按在它判定为"非交互"（箭头光标）的区域时，AppKit 会
/// 侵入式地启动自己的拖动检测循环并吃掉鼠标事件，页面 JS 收不到
/// mousedown/mousemove——而按在文字/控件附近时事件才到达 JS。两套机制按按下
/// 位置随机命中，这就是设置窗口拖动"时灵时不灵"的根源。显式开启背景拖动后
/// 只留系统这一套：所有非交互区域（含标题栏留白）都能稳定拖动。
pub fn set_movable_by_background<R: Runtime>(window: &WebviewWindow<R>, movable: bool) {
    let ptr = match window.ns_window() {
        Ok(p) if !p.is_null() => p,
        _ => return,
    };
    unsafe {
        if let Some(win) = retain_window(ptr) {
            let _: () = objc2::msg_send![&win, setMovableByWindowBackground: movable];
        }
    }
}

// ---------- 自动暂停（前台应用切换监听 + 桌面可见性快照） ----------

/// 前台应用切换观察者，驱动两件事：
/// 1. 自动暂停的**即时提示**（设置 → 通用 / 托盘菜单，默认关）：判定表在
///    [`super::auto_pause`]（判据是桌面可见性，前台只是加速信号），这里把
///    通知自带的前台类型按 Hint 传过去 —— 切应用立刻暂停/恢复，不等轮询。
/// 2. 指针注入门控（无开关，纯优化）：只有桌面活动（Finder 前台）时才注入
///    光标，前台是别的应用（含本应用自己）时停注入 —— 光标不在桌面层上，
///    注入的坐标对壁纸无意义，还每 33ms 白过一次 JS 桥。
///
/// 用 NSWorkspaceDidActivateApplicationNotification 观察者（即时响应）；
/// 最小化/关窗这类**不换前台**的回桌面路径由 auto_pause 的 250ms 对账兜底。
pub fn start_auto_pause_observer(app: &tauri::AppHandle) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    let app2 = app.clone();
    let own_bundle = app.config().identifier.clone();
    let block = block2::RcBlock::new(move |notif: *mut AnyObject| {
        unsafe {
            if notif.is_null() {
                return;
            }
            // userInfo[NSWorkspaceApplicationKey] = 刚激活的 NSRunningApplication（+0）
            let info: *mut AnyObject = msg_send![notif, userInfo];
            if info.is_null() {
                return;
            }
            let key = NSString::from_str("NSWorkspaceApplicationKey");
            let running: *mut AnyObject = msg_send![info, objectForKey: &*key];
            if running.is_null() {
                return;
            }
            let bundle: *mut NSString = msg_send![running, bundleIdentifier];
            if bundle.is_null() {
                return;
            }
            on_frontmost_changed(&app2, &(*bundle).to_string(), &own_bundle);
        }
    });

    unsafe {
        let Some(ws_cls) = AnyClass::get(c"NSWorkspace") else {
            tracing::warn!("auto-pause: NSWorkspace 类不可用，观察者未注册");
            return;
        };
        let ws: *mut AnyObject = msg_send![ws_cls, sharedWorkspace];
        let center: *mut AnyObject = msg_send![ws, notificationCenter];
        let name = NSString::from_str("NSWorkspaceDidActivateApplicationNotification");
        // queue=NULL：在投递线程（主线程）回调；返回的观察者是 +0，retain+forget
        // 让它常驻（进程生命周期内都需要，无需移除）
        let observer: *mut AnyObject = msg_send![center,
            addObserverForName: &*name,
            object: std::ptr::null_mut::<AnyObject>(),
            queue: std::ptr::null_mut::<AnyObject>(),
            usingBlock: &*block
        ];
        if observer.is_null() {
            tracing::warn!("auto-pause: 观察者注册失败");
            return;
        }
        std::mem::forget(objc2::rc::Retained::retain(observer));
    }
    // 启动时的初始前台状态：通知只在「变化」时发，主动查一次校正注入门控
    // （自动暂停不在这里补跑：启动时的播放状态由会话恢复/用户设置决定，
    //  不由前台应用代劳）
    if let Some(bundle) = frontmost_bundle_id() {
        super::pointer::set_desktop_active(bundle == "com.apple.finder");
    }
    tracing::info!("auto-pause observer registered (wallpaper_auto_pause，默认关)");
}

/// 当前前台应用的 bundle id（NSWorkspace.frontmostApplication）。
/// 用于启动时初始化「桌面是否活动」；查询失败返回 None，调用方保持默认。
fn frontmost_bundle_id() -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;
    unsafe {
        let ws_cls = AnyClass::get(c"NSWorkspace")?;
        let ws: *mut AnyObject = msg_send![ws_cls, sharedWorkspace];
        if ws.is_null() {
            return None;
        }
        let front: *mut AnyObject = msg_send![ws, frontmostApplication];
        if front.is_null() {
            return None;
        }
        let bundle: *mut NSString = msg_send![front, bundleIdentifier];
        if bundle.is_null() {
            return None;
        }
        Some((*bundle).to_string())
    }
}

/// 前台应用类型（自动暂停判定表的前台一元）。查询失败返回 Unknown。
pub fn frontmost_kind(app: &tauri::AppHandle) -> super::auto_pause::FrontKind {
    use super::auto_pause::FrontKind;
    let own = app.config().identifier.clone();
    match frontmost_bundle_id() {
        Some(b) if b == "com.apple.finder" => FrontKind::Desktop,
        Some(b) if b == own => FrontKind::SelfApp,
        Some(_) => FrontKind::Other,
        None => FrontKind::Unknown,
    }
}

/// 「盖住壁纸」的窗口快照：自动暂停的可见性判据（消费方见 [`super::auto_pause`]）。
///
/// 数据源 `CGWindowListCopyWindowInfo(OnScreenOnly | ExcludeDesktopElements)`：
/// 只含当前 Space 上的可见窗口，桌面元素（壁纸/图标窗）已排除。再过滤：
/// - 层 0（普通应用窗口）与屏保层（kCGScreenSaverWindowLevelKey≈1000 附近）
///   才算「盖壁纸」；其余非 0 层是程序坞/菜单栏/光标层等系统 UI —— 不计入，
///   否则「桌面干净」永远不成立（光标层 28x28 常驻窗实测会把判据判死）；
/// - 本应用自己的窗口（设置窗等）不算：我们是壁纸系统自身；
/// - alpha≈0、零面积、不与任何屏幕相交的忽略（F11 把窗口移出屏幕边缘后
///   窗口还在、但不算遮挡，判据要回落到「桌面干净」）。
///
/// `coverages` 用网格采样估算每块屏被盖住的比例（[`super::auto_pause::covered_ratio`]）。
/// 坐标系：CGWindowList 的 bounds 与 CGDisplayBounds 同为 CG 全局（左上原点），可直接比较。
pub fn occlusion_snapshot() -> Option<super::auto_pause::OcclusionSnapshot> {
    use super::auto_pause::{covered_ratio, OcclusionSnapshot};
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    // kCGWindowListOptionOnScreenOnly(1) | kCGWindowListExcludeDesktopElements(16)
    const LIST_FLAGS: u32 = 1 | 16;
    let own_pid = std::process::id() as i32;
    let mut rects: Vec<(f64, f64, f64, f64)> = Vec::new();
    unsafe {
        let info = CGWindowListCopyWindowInfo(LIST_FLAGS, 0);
        if info.is_null() {
            return None;
        }
        let arr = info.cast::<AnyObject>();
        let key_pid = NSString::from_str("kCGWindowOwnerPID");
        let key_layer = NSString::from_str("kCGWindowLayer");
        let key_alpha = NSString::from_str("kCGWindowAlpha");
        let key_bounds = NSString::from_str("kCGWindowBounds");
        let n: usize = msg_send![arr, count];
        for i in 0..n {
            let w: *mut AnyObject = msg_send![arr, objectAtIndex: i];
            if w.is_null() {
                continue;
            }
            // objc 消息发给 nil 返回 0：缺字段按「层 0 / 不透明」处理前先显式取值
            let pid_v: *mut AnyObject = msg_send![w, objectForKey: &*key_pid];
            let pid: i32 = msg_send![pid_v, intValue];
            if pid == own_pid {
                continue;
            }
            let layer_v: *mut AnyObject = msg_send![w, objectForKey: &*key_layer];
            let layer: i32 = msg_send![layer_v, intValue];
            // 只认两种层：0（普通应用窗口）与屏保层（kCGScreenSaverWindowLevelKey=13
            // 附近，全屏盖住壁纸时该停）。其余非 0 层都是系统 UI —— 程序坞(20)/
            // 菜单栏(24)/光标层(2147483630)等一律不算：实测光标层的 28x28 常驻窗
            // 会把「桌面干净」判死（恢复永不触发），是本过滤的重点。
            let saver_level: i32 = CGWindowLevelForKey(13);
            let covering_layer = layer == 0 || (saver_level..=saver_level + 32).contains(&layer);
            if !covering_layer {
                continue;
            }
            let alpha_v: *mut AnyObject = msg_send![w, objectForKey: &*key_alpha];
            let alpha: f64 = msg_send![alpha_v, doubleValue];
            if alpha <= 0.05 {
                continue;
            }
            let bounds_v: *mut AnyObject = msg_send![w, objectForKey: &*key_bounds];
            if bounds_v.is_null() {
                continue;
            }
            let mut r = CRect {
                origin: CPoint { x: 0.0, y: 0.0 },
                size: CSize {
                    width: 0.0,
                    height: 0.0,
                },
            };
            let ok: bool =
                CGRectMakeWithDictionaryRepresentation(bounds_v.cast::<c_void>(), &mut r);
            if !ok || r.size.width <= 0.0 || r.size.height <= 0.0 {
                continue;
            }
            rects.push((
                r.origin.x,
                r.origin.y,
                r.origin.x + r.size.width,
                r.origin.y + r.size.height,
            ));
        }
        CFRelease(info);
    }
    let screens = active_screens();
    rects.retain(|(x1, y1, x2, y2)| {
        screens
            .iter()
            .any(|s| *x1 < s.x + s.w && *x2 > s.x && *y1 < s.y + s.h && *y2 > s.y)
    });
    let coverages = screens
        .iter()
        .map(|s| (s.id, covered_ratio(&rects, (s.x, s.y, s.w, s.h))))
        .collect();
    Some(OcclusionSnapshot { coverages })
}

/// 「隐藏图标」开启后的桌面点击监视（自动暂停的即时恢复信号之一）。
///
/// 交互态下点桌面点的是本应用的壁纸窗口 —— 无边框窗不能成为 key window，
/// 点击不一定触发应用激活，前台可能一直停在别的应用上（通知不发、恢复不走
/// Hint 路径）。虽然可见性对账 250ms 内也会兜住，但「用户刚点了桌面」是最
/// 强的恢复意图，这里直接恢复不等对账：
/// 本应用收到的、落在桌面级窗口（level < 0，即壁纸窗口）上的鼠标按下 =
/// 用户在操作桌面 → 恢复「自动暂停」挂起的播放（用户手动暂停不动）。
///
/// 用 local event monitor 而非 CGEventTap：只收投递给本应用窗口的事件，
/// 正好覆盖「点在壁纸窗口上」这一场景，且不需要输入监控权限。
pub fn start_desktop_click_monitor(app: &tauri::AppHandle) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let app2 = app.clone();
    let block = block2::RcBlock::new(move |event: *mut AnyObject| -> *mut AnyObject {
        unsafe {
            if event.is_null() {
                return event;
            }
            let win: *mut AnyObject = msg_send![event, window];
            if win.is_null() {
                return event;
            }
            let level: isize = msg_send![&*win, level];
            if level < 0 {
                // 桌面级（壁纸）窗口上的点击 = 用户在操作桌面：恢复自动暂停。
                // 只恢复「当前看得见」的屏 —— 被全屏盖住的屏保持暂停（按屏独立）。
                // 用户手动暂停不动（resume_visible 只动 auto_paused 集合内的）
                super::auto_pause::resume_visible(&app2);
            }
            event
        }
    });

    unsafe {
        let Some(cls) = AnyClass::get(c"NSEvent") else {
            tracing::warn!("desktop click monitor: NSEvent 类不可用，未注册");
            return;
        };
        // NSEventMask: leftMouseDown(1<<1) | rightMouseDown(1<<3) | otherMouseDown(1<<25)
        let mask: u64 = (1 << 1) | (1 << 3) | (1 << 25);
        let monitor: *mut AnyObject = msg_send![cls,
            addLocalMonitorForEventsMatchingMask: mask,
            handler: &*block
        ];
        if monitor.is_null() {
            tracing::warn!("desktop click monitor 注册失败");
            return;
        }
        std::mem::forget(objc2::rc::Retained::retain(monitor));
    }
    tracing::info!("desktop click monitor registered（交互态点桌面恢复自动暂停）");
}

/// 前台应用变化：只做两件事 —— 指针注入门控 + 把前台类型当即时提示（Hint）
/// 交给共享判定表（[`super::auto_pause`]）。
///
/// 暂停/恢复的**判据**是桌面可见性（窗口清单），前台只是加速信号：
/// 最小化/关闭窗口后前台仍停在原应用上，恢复由判定表的轮询兜底接管
/// （旧实现只认前台切换，回桌面必须点一下才恢复 —— 已废除该耦合）。
fn on_frontmost_changed(app: &tauri::AppHandle, bundle_id: &str, own_bundle: &str) {
    use super::auto_pause::{recheck_with, settings_window_visible, FrontKind, Mode};
    tracing::debug!("frontmost changed: {bundle_id}");
    let kind = if bundle_id == own_bundle {
        FrontKind::SelfApp
    } else if bundle_id == "com.apple.finder" {
        FrontKind::Desktop
    } else {
        FrontKind::Other
    };
    // 指针注入门控：桌面活动（Finder 前台 / 壁纸窗口被点）才注入 —— 光标不在
    // 桌面层上时注入的坐标对壁纸无意义。「本应用前台且设置窗不可见」= 交互态
    // 点了壁纸窗口，按桌面语义；设置窗在场 = 在操作本应用，不注入。
    let is_desktop = match kind {
        FrontKind::Desktop => true,
        FrontKind::SelfApp => !settings_window_visible(app),
        _ => false,
    };
    super::pointer::set_desktop_active(is_desktop);
    recheck_with(app, Mode::Hint, kind);
}
