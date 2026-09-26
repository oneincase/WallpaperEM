//! 主窗口生命周期：关闭 / 最小化即释放 + 按需重建
//!
//! 应用主窗口（React UI，独立 WKWebView）的生命周期：
//! - **用户关闭（红色按钮/⌘W）→ 立即销毁并结束 WebContent 进程**
//!   （[`register_close_to_release`]）：用户点关就是「退出主界面」，桌面上只留
//!   壁纸渲染进程。
//! - **黄色最小化 / ⌘H（已自定义为最小化）→ 同样立即释放**（与关闭同一语义）：
//!   主界面那一整页 React UI 压着几十～几百 MB，最小化即「此刻不看了」，内存
//!   立刻归还，而不是把窗口藏着等系统内存压力。重开走托盘 / Dock(Reopen) 的
//!   重建路径（页面重载一次，换内存即时回收）。
//! - 桌面壁纸窗口（label = `wallpaper-*`）独占数据存储，不受本文件任何动作影响。
//!
//! 为什么「隐藏后按时间回收」被放弃（历史上试过）：那一步在 macOS 上**回收不了
//! 内存**。主窗口用的是默认（共享）`WKWebsiteDataStore` —— 没有按标识删除的 API，
//! `destroy()` 只是把 WKWebView 从窗口上摘下来，WebContent 进程连页面一起留在
//! WebKit 的进程池里，下次重建又落回同一个池子。所以释放必须走「先问 pid、再
//! destroy、再按 pid 结束进程」（[`exclusive_web_content_pid`] +
//! [`release_web_content`]，与壁纸窗口的 [`crate::wallpaper`] 同一套思路）。
//!
//! 隐藏判定覆盖三种用户路径（压力回收那条路用）：
//! - 黄色按钮最小化（miniaturized 时 isVisible 仍为 true，故必须查 is_minimized）
//! - 关闭按钮 → v1.0.2 起立即释放（见上），不再进入隐藏态
//! - ⌘H 自定义为最小化主窗口（lib.rs setup，避免 NSApp hide 连壁纸窗口一起藏）
//!
//! 释放/回收后用户从 托盘菜单 / 托盘左键 / Dock 图标(Reopen) / 二次启动(single-instance)
//! 唤起时，由 [`ensure_main_window`] 按 tauri.conf.json 原配置重建窗口。
//!
//! 已知边界：压力下若主窗口**可见**，我们不动它（销毁用户正在看的窗口更糟）。
//! 此时 WebKit 自己也可能在压力下结束这个进程 → 界面变空白，关掉重开即可（走既有
//! 重建路径）；这种情况会打一条 WARN 留痕。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::download;
use crate::mem_pressure;

/// 隐藏满这么久之后，才允许在内存压力下回收 —— 避免用户在两个应用间来回切时
/// 被反复重建（一次重建 = 一次完整页面加载）。
const MIN_HIDDEN: Duration = Duration::from_secs(5);
/// 看门狗轮询周期（实际触发时延 = MIN_HIDDEN + 最多一次轮询间隔）
const POLL: Duration = Duration::from_secs(1);
/// 内存压力的复查间隔（拍数）：读一次 sysctl / `/proc/meminfo` 很便宜，
/// 但压力是缓变量，没必要每秒读。
const PRESSURE_POLL_TICKS: u32 = 5;

#[derive(Default)]
struct MainWindowState {
    /// 连续隐藏起始时刻；None = 当前可见（或窗口不存在）
    hidden_since: Option<Instant>,
    /// watchdog 已销毁主窗口。Tauri 注册表的条目移除依赖 macOS windowWillClose
    /// 事件链，隐藏（orderOut）窗口的 destroy 不保证送达——此后 get_webview_window
    /// 会拿到僵尸条目并对其 show()（静默失败），主窗口永远无法重建。该标志强制
    /// ensure_main_window 走清场重建路径。
    released: bool,
    /// 本轮内存压力里是否已就「主窗口可见、不回收」提醒过一次（压力持续时不刷屏）
    noted_visible: bool,
}

/// 启动回收看门狗（setup 阶段调用一次）
pub fn start(app: &AppHandle) {
    app.manage(Arc::new(Mutex::new(MainWindowState::default())));
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut ticks: u32 = 0;
        // 最近一次内存压力读数；None = 本平台读不到（按「无压力」处理 → 不回收）
        let mut reading: Option<mem_pressure::Reading> = None;
        // 连续读不到的次数；两条日志各自只记一次（见下面的日志说明）
        let mut misses: u32 = 0;
        let (mut ready_logged, mut missing_warned) = (false, false);
        loop {
            tokio::time::sleep(POLL).await;
            if ticks.is_multiple_of(PRESSURE_POLL_TICKS) {
                let next = mem_pressure::read();
                // 这两条日志是「探测有没有生效」的唯一凭据：读不到读数时主窗口会**永远**
                // 常驻，「一直没看到回收」既可能是没压力，也可能是信号一直是 None，必须
                // 能从日志里区分开。首次读到记 INFO，连续 3 次读不到才记 WARN（躲开偶发）。
                match next.as_ref() {
                    Some(r) => {
                        misses = 0;
                        if !ready_logged {
                            ready_logged = true;
                            tracing::info!("内存压力探测就绪：{}", r.detail);
                        }
                    }
                    None => {
                        misses = misses.saturating_add(1);
                        if misses >= 3 && !missing_warned {
                            missing_warned = true;
                            tracing::warn!(
                                "连续 {} 次读不到内存压力读数：主窗口将常驻不回收（只是少了这条回收路径，\
                                 壁纸窗口的销毁/回收不受影响）",
                                misses
                            );
                        }
                    }
                }
                let was = reading.as_ref().is_some_and(|r| r.pressure);
                if next.as_ref().is_some_and(|r| r.pressure) && !was {
                    tracing::warn!(
                        "system memory pressure: {}（隐藏中的主窗口将被回收）",
                        reading_detail(next.as_ref())
                    );
                }
                reading = next;
            }
            ticks = ticks.wrapping_add(1);
            let app3 = app2.clone();
            let reading = reading.clone();
            // AppKit 调用必须在主线程
            let _ = app2.run_on_main_thread(move || on_tick(&app3, reading.as_ref()));
        }
    });
    tracing::info!("main window release armed（仅在系统内存压力下回收）");
}

/// 读数的日志文本（读不到时给个占位）
fn reading_detail(reading: Option<&mem_pressure::Reading>) -> &str {
    reading.map(|r| r.detail.as_str()).unwrap_or("读数不可用")
}

/// 主窗口是否处于"用户不可见"状态（最小化/隐藏/应用隐藏）。
/// 窗口不存在（已释放或尚未创建）返回 false，让看门狗重置计时。
fn is_hidden(w: &WebviewWindow) -> bool {
    w.is_minimized().unwrap_or(false) || !w.is_visible().unwrap_or(true)
}

/// 被关闭窗口的 WebContent 进程 pid（不该动或拿不到时 None）。**必须在主线程、且在销毁前调用。**
///
/// `closing` 是本次要销毁的窗口 label：主窗口与 props-* 设置窗共用默认（共享）
/// `WKWebsiteDataStore`，**可能是同一个 WebContent 进程**（同存储同进程），那种
/// 时候踢进程会把其它 UI 窗口的页面一起打掉 —— 所以只有「除它以外只剩壁纸窗口」
/// 时才返回 pid，其余情况返回 None，调用方退回单纯的 destroy。
///
/// pid 用 `i32` 而不是 `libc::pid_t`：这个文件三平台都要编，而 `libc::pid_t` 只在
/// Unix 上存在（Windows 上没有，CI 会直接编译失败）。macOS 上 `pid_t` 就是 `i32`。
#[cfg(target_os = "macos")]
pub(crate) fn exclusive_web_content_pid(
    app: &AppHandle,
    w: &WebviewWindow,
    closing: &str,
) -> Option<i32> {
    app.webview_windows()
        .keys()
        .all(|label| label == closing || label.starts_with("wallpaper-"))
        .then(|| crate::wallpaper::macos::web_content_pid(w))
        .flatten()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn exclusive_web_content_pid(
    _app: &AppHandle,
    _w: &WebviewWindow,
    _closing: &str,
) -> Option<i32> {
    None
}

/// 结束被回收窗口的 WebContent 进程，再记一行内存观测。
///
/// 发信号 + 等进程从进程表消失是阻塞的（上限 500ms），而调用点在主线程上，所以整件事
/// 交给后台任务；读数也放在进程真消失之后 —— 那一行读到的才是「回收后」的占用。
/// 非 macOS 没有可结束的进程（[`exclusive_web_content_pid`] 恒为 None），只有读数这一步。
pub(crate) fn release_web_content(pid: Option<i32>, tag: &'static str) {
    tauri::async_runtime::spawn(async move {
        #[cfg(target_os = "macos")]
        if let Some(pid) = pid {
            crate::wallpaper::macos::kill_web_content_process(pid);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = pid;
        crate::mem_watch::report(tag);
    });
}

/// 单次检查（在主线程执行，检查与销毁天然无竞态）
fn on_tick(app: &AppHandle, pressure: Option<&mem_pressure::Reading>) {
    let window = app.get_webview_window("main");
    let hidden = window.as_ref().map(is_hidden).unwrap_or(false);
    let pressured = pressure.is_some_and(|r| r.pressure);

    let Some(st) = app.try_state::<Arc<Mutex<MainWindowState>>>() else {
        return;
    };
    let mut st = st.lock().unwrap();
    // 没有内存压力就什么都不做：主窗口常驻（理由见文件头），计时也归零
    if !pressured {
        st.hidden_since = None;
        st.noted_visible = false;
        return;
    }
    if !hidden {
        st.hidden_since = None;
        if !st.noted_visible {
            st.noted_visible = true;
            tracing::warn!(
                "memory pressure（{}）但主窗口可见：本次不回收 —— WebKit 也可能自行结束这个进程，\
                 界面若变成空白，关掉重开即可（会走重建路径）",
                reading_detail(pressure)
            );
        }
        return;
    }
    let since = *st.hidden_since.get_or_insert_with(Instant::now);
    let hidden_for = since.elapsed();
    if hidden_for < MIN_HIDDEN {
        return;
    }
    // 到时：先重置计时（本次要么释放，要么因下载活动跳过后重新计满时长）
    st.hidden_since = None;

    // 下载/Guard/扫码登录进行中：跳过释放，避免打断 Steam Guard 输入与进度展示
    if download::is_busy(app) {
        tracing::debug!("main window memory-pressure release skipped: download active");
        return;
    }

    // 释放前再次确认仍隐藏（用户可能刚重新打开）
    if let Some(w) = window {
        if is_hidden(&w) {
            // 和壁纸窗口一样：destroy() 只是把 WKWebView 摘下来，WebKit 会把进程留在
            // 池子里且不保证还内存 —— 而这一步的全部目的就是还内存，所以能踢就踢。
            // pid 得**在销毁之前**问（窗口一没，WKWebView 就没了），销毁后再在后台
            // 发 SIGKILL 并等它真消失。
            let pid = exclusive_web_content_pid(app, &w, "main");
            match w.destroy() {
                Ok(_) => {
                    // 注意：此处不可再 lock()——本函数开头拿到的 st guard 仍存活，
                    // std::sync::Mutex 不可重入，同线程二次 lock = 自死锁，
                    // 表现为「主窗口被回收的瞬间整个软件无响应」
                    st.released = true;
                    tracing::info!(
                        "main window released under memory pressure（{}，已隐藏 {}s；WebContent 进程回收）",
                        reading_detail(pressure),
                        hidden_for.as_secs()
                    );
                    // 结束进程要发信号 + 等它消失，都在后台做（这里正是主线程），
                    // 等它真结束了再量一次 —— 这一行读数才代表「回收后」的占用
                    release_web_content(pid, "主窗口回收后");
                }
                Err(e) => tracing::warn!("main window release failed: {e}"),
            }
        }
    }
}

/// 显示主窗口；若已被内存压力回收销毁，则按 tauri.conf.json 原配置重建。
/// 供托盘菜单/托盘左键/Dock Reopen/单实例聚焦调用；可在任意线程调用
/// （窗口操作经 runtime 派发到主线程，主线程调用则同步执行）。
pub fn ensure_main_window(app: &AppHandle) {
    // ⌘H 隐藏整个应用时，仅 show 窗口不够，需先 unhide 应用（仅 macOS）
    #[cfg(target_os = "macos")]
    let _ = app.show();

    // released 标志 = 窗口已被 watchdog 销毁、注册表条目可能是僵尸；此时即便
    // get_webview_window 返回 Some 也只是僵尸，对其 show() 会静默失败
    let released = app
        .try_state::<Arc<Mutex<MainWindowState>>>()
        .map(|s| s.lock().unwrap().released)
        .unwrap_or(false);

    if !released {
        if let Some(w) = app.get_webview_window("main") {
            // 最小化（miniaturize 到 Dock）的窗口对 show() 无反应（macOS 的
            // orderFront 不会自动 deminiaturize），必须显式还原，否则托盘点击
            // 后主窗口永远停在 Dock 里，表现为「软件无响应」
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            return;
        }
    } else if let Some(zombie) = app.get_webview_window("main") {
        tracing::info!("stale main window entry after release; clearing");
        let _ = zombie.destroy();
    }

    // 重建：属性与 tauri.conf.json app.windows[0] 保持一致（全平台无边框 +
    // 自绘窗口控制；shadow 在 borderless 窗口上仍由系统提供投影）。
    // 刚释放后 "main" label 可能仍被僵尸占用：destroy 的注册表清理是异步的，
    // 短间隔重试等其真正释放（避免一次失败后主窗口彻底出不来）。
    for attempt in 0..10 {
        let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("WallpaperEM")
            .inner_size(1240.0, 800.0)
            .min_inner_size(940.0, 600.0)
            .center()
            .transparent(true)
            .decorations(false)
            .shadow(true)
            // 关掉 WebKit 开发者附加（devtools:false）：不关的话 WKWebView 右键会
            // 弹「Reload / Inspect Element」菜单，正式界面不该有（与 tauri.conf.json
            // 的 devtools:false 保持同步 —— 这里是回收重建的第三份窗口配置）
            .devtools(false);
        let built = builder.build();
        match built {
            Ok(w) => {
                // 装饰与初始窗口一致：关闭=立即释放 + 侧栏 vibrancy
                register_close_to_release(&w);
                // vibrancy 只能在主线程调用；重建可能由非主线程入口触发
                //（single-instance 回调），直接调用会失败并丢失侧栏磨砂效果
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let _ = crate::apply_vibrancy(&app2);
                });
                if let Some(st) = app.try_state::<Arc<Mutex<MainWindowState>>>() {
                    st.lock().unwrap().released = false;
                }
                let _ = w.set_focus();
                // 防御性首帧上屏：transparent WKWebView 在「销毁后重建」序列下偶发
                // 首帧不合成（整窗透明，观感即主界面不渲染）。创建后做一次 1px
                // 尺寸扰动再还原，强制 AppKit/CA 重新合成一帧。
                let w2 = w.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    if let Ok(size) = w2.inner_size() {
                        let (wd, ht) = (size.width, size.height);
                        if wd >= 2 && ht >= 2 {
                            let _ = w2.set_size(tauri::PhysicalSize::new(wd - 1, ht));
                            tokio::time::sleep(Duration::from_millis(60)).await;
                            let _ = w2.set_size(tauri::PhysicalSize::new(wd, ht));
                        }
                    }
                });
                tracing::info!("main window recreated");
                return;
            }
            Err(e) => {
                if attempt == 9 {
                    tracing::error!("main window recreate failed: {e}");
                } else {
                    tracing::debug!("main window build retry ({}): {e}", attempt + 1);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// 关闭 / 最小化主窗口 = 立即释放（销毁窗口 + 尽力结束其 WebContent 进程）。
/// setup（初始窗口）与 ensure_main_window（重建窗口）共用。
///
/// 主界面那一大页 React UI 压着几十～几百 MB：用户点关或点最小化都是「退出
/// 主界面 / 此刻不看了」，窗口销毁、内存立刻归还。重开走托盘 / Dock Reopen 的
/// 重建路径（页面重载一次，换内存即时回收）。
pub fn register_close_to_release(w: &WebviewWindow) {
    let w2 = w.clone();
    w.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            release_main(&w2, "关闭");
        }
    });

    // macOS：最小化（黄色按钮 / Cmd+M）只触发 NSWindow 的 miniaturize（窗口
    // orderOut，逻辑尺寸不变），**tauri 不发 Resized 事件**（2026-09-24 实测）。
    // 直接注册 NSWindowDidMiniaturizeNotification 才能在最小化时释放。
    #[cfg(target_os = "macos")]
    observe_minimize(w);
}

/// macOS：注册 NSWindowDidMiniaturizeNotification，窗口最小化时立即释放。
#[cfg(target_os = "macos")]
fn observe_minimize(w: &WebviewWindow) {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSNotificationCenter;
    use std::ptr::NonNull;

    let win_ptr = match w.ns_window() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("observe_minimize: 拿不到 NSWindow: {e}");
            return;
        }
    };

    // block 回调里持有窗口克隆（释放用）。NSNotificationCenter 要求 Sendable
    // block：用 RcBlock 的 Send 变体 StackBlock→copy；这里用 block2::RcBlock
    // 并以 DynBlock 形式传（签名 NonNull<NSNotification>）。
    let weak_w = w.clone();
    let block = block2::RcBlock::new(move |note: NonNull<objc2_foundation::NSNotification>| {
        let _ = note; // 只需要「最小化了」这个信号
        tracing::info!("main window did miniaturize → releasing");
        release_main(&weak_w, "最小化");
    });

    let center = NSNotificationCenter::defaultCenter();
    let name = unsafe { objc2_app_kit::NSWindowDidMiniaturizeNotification };
    // 只观察这一个窗口（object = 该 NSWindow）
    let obj: Retained<AnyObject> = unsafe { Retained::retain(win_ptr.cast()) }
        .expect("NSWindow 必然存活");
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(Some(name), Some(&obj), None, &block)
    };

    // observer 与 block 必须存活整个窗口生命周期，否则通知静默失效 —— 交给
    // 进程级缓存持有（主窗口重建时会重新注册新的观察者）
    MINIMIZE_TOKENS.with(|tokens| {
        if let Ok(mut v) = tokens.lock() {
            v.push(MinimizeToken {
                _block: block,
                _observer: observer,
            });
        }
    });
}

/// 存活中的最小化观察者令牌（observer 对象 + block），防止提前失效。
#[cfg(target_os = "macos")]
struct MinimizeToken {
    _block: block2::RcBlock<dyn Fn(std::ptr::NonNull<objc2_foundation::NSNotification>)>,
    _observer: objc2::rc::Retained<
        objc2::runtime::ProtocolObject<dyn objc2_foundation::NSObjectProtocol>,
    >,
}

#[cfg(target_os = "macos")]
thread_local! {
    static MINIMIZE_TOKENS: std::sync::Mutex<Vec<MinimizeToken>> =
        const { std::sync::Mutex::new(Vec::new()) };
}

/// 销毁主窗口并结束其 WebContent 进程；失败退回隐藏，不阻塞用户操作。
fn release_main(w: &WebviewWindow, why: &str) {
    let app = w.app_handle().clone();
    match destroy_and_release(&app, w, "main") {
        Ok(()) => {
            // released 标志确保托盘/Dock 重开时走清场重建（僵尸条目
            // 对 show() 静默失败，主窗口会永远出不来）
            if let Some(st) = app.try_state::<Arc<Mutex<MainWindowState>>>() {
                let mut g = st.lock().unwrap();
                g.released = true;
                g.hidden_since = None;
            }
            tracing::info!("main window released（{why}）");
        }
        Err(e) => {
            // 销毁失败兜底：退回隐藏，至少不挡着用户操作
            let _ = w.hide();
            tracing::warn!("main window release failed（{why}）: {e}（已退回隐藏）");
        }
    }
}

/// 销毁一个 UI 窗口并尽力结束它的 WebContent 进程（立即释放内存）。
///
/// pid 必须在销毁**之前**问（窗口一没，WKWebView 就没了）；且只在「除它以外
/// 只剩壁纸窗口」时才结束进程 —— 共享存储的其它 UI 窗口（主窗口/props-*）可能
/// 挂在同一个 WebContent 进程上，踢了会连累。结束后台执行（发信号 + 等进程
/// 消失是阻塞的），并量一次内存观测。
/// props-* 设置窗关闭时复用本函数（[`crate::props_window`]）。
pub(crate) fn destroy_and_release(
    app: &AppHandle,
    w: &WebviewWindow,
    label: &str,
) -> Result<(), tauri::Error> {
    let pid = exclusive_web_content_pid(app, w, label);
    w.destroy()?;
    let process = if pid.is_some() { "，WebContent 进程已结束" } else { "（进程与其它 UI 窗口共享，保留）" };
    tracing::info!("window {label} closed: 窗口已销毁{process}");
    release_web_content(pid, "关闭 UI 窗口后");
    Ok(())
}
