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
use tauri::{Manager, Runtime, WebviewWindow};

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
}

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

// ---------- 自动暂停（前台应用切换监听） ----------

/// 前台应用切换观察者，驱动两件事：
/// 1. 自动暂停（设置 → 通用 / 托盘菜单，默认关）：切到非桌面应用自动暂停
///    壁纸，切回桌面（Finder 成为前台）自动恢复。只恢复本功能自己挂的暂停
///    （auto_paused 标志），用户手动暂停不受前台切换影响；
///    本应用自身前台化算中性（设置窗口/托盘操作常见，不动播放状态）。
/// 2. 指针注入门控（无开关，纯优化）：只有桌面活动（Finder 前台）时才注入
///    光标，前台是别的应用（含本应用自己）时停注入 —— 光标不在桌面层上，
///    注入的坐标对壁纸无意义，还每 33ms 白过一次 JS 桥。
///
/// 用 NSWorkspaceDidActivateApplicationNotification 观察者（即时响应），
/// 不走 2s 轮询 —— 切应用的停顿感对「自动暂停」是可感知的。
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

/// 主设置窗/壁纸属性窗是否可见（任一可见即认为用户正在操作本应用设置，
/// 此时自身前台化保持中性；都不可见说明自身前台化是「点击桌面壁纸」所致）
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

/// 「隐藏图标」开启后的桌面点击监视（自动暂停的第二条恢复信号）。
///
/// 为什么需要：自动暂停的恢复挂在「前台应用切换」上（Finder / 本应用被点活），
/// 但交互态下点桌面点的是本应用的壁纸窗口 —— 无边框窗不能成为 key window，
/// 点击不一定触发应用激活，前台可能一直停在别的应用上，恢复信号就永远不来
/// （实测：暂停后点桌面不恢复）。所以补一条不依赖前台切换的直接信号：
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
                // 桌面级（壁纸）窗口上的点击 = 用户在操作桌面：恢复自动暂停
                if let Some(st) = app2.try_state::<super::WallpaperEngineState>() {
                    let was_auto = {
                        let mut g = st.auto_paused.lock().unwrap();
                        std::mem::replace(&mut *g, false)
                    };
                    if was_auto {
                        let _ = super::resume_all(app2.clone());
                        tracing::info!("auto-pause: 桌面壁纸被点击，壁纸已恢复播放");
                    }
                }
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

/// 前台应用变化：非桌面 → 自动暂停；桌面（Finder）→ 恢复本功能挂的暂停
fn on_frontmost_changed(app: &tauri::AppHandle, bundle_id: &str, own_bundle: &str) {
    tracing::debug!("frontmost changed: {bundle_id}");
    // 「隐藏图标」开启后，点桌面实际点在本应用的壁纸窗口上（它在图标之上且
    // 接收鼠标）→ 前台应用变成我们自己，而不是 Finder。这并非「离开桌面」，
    // 恰恰是「在桌面上点壁纸」：主设置窗/属性窗不可见时按桌面语义处理
    // （恢复自动暂停 + 桌面活动）；任一设置窗可见才是真的在操作本应用
    // （保持旧的中性语义，不动播放状态）。
    let self_click_desktop = bundle_id == own_bundle && !settings_window_visible(app);
    let is_desktop = bundle_id == "com.apple.finder" || self_click_desktop;
    // 指针注入门控：桌面活动（Finder 前台 / 壁纸窗口被点）才注入。这句要放在
    // own_bundle 早退之前 —— 与自动暂停的「自身前台化中性」语义不同。
    super::pointer::set_desktop_active(is_desktop);
    // 自己前台化且在操作设置窗：中性，不动播放状态
    if bundle_id == own_bundle && !self_click_desktop {
        return;
    }
    let Some(st) = app.try_state::<super::WallpaperEngineState>() else {
        return;
    };
    if is_desktop {
        // 回到桌面：只恢复「自动暂停」挂的，用户手动暂停不动
        let was_auto = {
            let mut g = st.auto_paused.lock().unwrap();
            std::mem::replace(&mut *g, false)
        };
        if was_auto {
            let _ = super::resume_all(app.clone());
            tracing::info!("auto-pause: 回到桌面（前台={bundle_id}），壁纸已恢复播放");
        }
        return;
    }
    // 切到非桌面：开关开且当前未暂停才自动暂停（已暂停的不覆盖标志，
    // 避免把用户的手动暂停误标成自动的、回桌面时被代劳恢复）
    if !auto_pause_enabled(app) || *st.paused.lock().unwrap() {
        return;
    }
    if super::pause_all(app.clone()).is_ok() {
        *st.auto_paused.lock().unwrap() = true;
        tracing::info!("auto-pause: 切到 {bundle_id}，壁纸已自动暂停");
    }
}

/// 设置开关：wallpaper_auto_pause，默认关。每次前台切换时直读 DB（切换是
/// 低频事件，SQLite 读取亚毫秒；直读免去与设置页/托盘两侧的缓存同步）
fn auto_pause_enabled(app: &tauri::AppHandle) -> bool {
    app.try_state::<std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>>()
        .and_then(|db| {
            db.lock()
                .ok()
                .and_then(|c| crate::db::get_setting(&c, "wallpaper_auto_pause"))
        })
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}
