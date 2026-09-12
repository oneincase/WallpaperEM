//! 主窗口生命周期：内存压力下回收 + 按需重建
//!
//! 应用主窗口（React UI，独立 WKWebView）隐藏/最小化后**不再按时间回收** ——
//! 只有系统真的报告内存压力（[`crate::mem_pressure`]）时才销毁窗口，让 WebKit
//! 回收其 WebContent 进程；桌面壁纸窗口（label = `wallpaper-*`）不受影响。
//!
//! 为什么放弃「隐藏 X 秒就销毁」：那一步在 macOS 上**回收不了内存**。主窗口用的是
//! 默认（共享）`WKWebsiteDataStore` —— 没有按标识删除的 API，`destroy()` 只是把
//! WKWebView 从窗口上摘下来，WebContent 进程连页面一起留在 WebKit 的进程池里，
//! 下次重建又落回同一个池子（壁纸窗口那边是同一个机制，靠「每窗口独占一份存储 +
//! 销毁时删除」才绕开）。实测（2026-09-11 日志 + 活动监视器）「3s 闲置回收」的
//! 收益接近于零，代价却是每次重开都付一次页面重载，外加隐藏瞬间的主线程停顿 ——
//! 当天日志里 30 次 `UI event loop wedged?` 全部落在窗口被隐藏的那一刻（逐条对照
//! 上下文可知是 WebKit 的百毫秒级抖动，不是卡死）。改成「压力下才回收」：这时把
//! 几百 MB 还回去，才值得付一次重建。
//!
//! 隐藏判定覆盖三种用户路径：
//! - 黄色按钮最小化（miniaturized 时 isVisible 仍为 true，故必须查 is_minimized）
//! - 关闭按钮（CloseRequested -> hide，见 lib.rs setup）
//! - ⌘H 隐藏整个应用（窗口 orderOut，is_visible 变 false）
//!
//! 回收后用户从 托盘菜单 / 托盘左键 / Dock 图标(Reopen) / 二次启动(single-instance)
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

/// 主窗口的 WebContent 进程 pid（不该动或拿不到时 None）。**必须在主线程、且在销毁前调用。**
///
/// 例外：主窗口用的是默认（共享）存储，与 props-* 设置窗**可能是同一个 WebContent
/// 进程**（同存储同进程），那种时候踢进程会把设置窗的页面一起打掉 —— 所以只有除了
/// 壁纸窗口以外没有别的共享窗口时才返回 pid，其余情况返回 None，调用方退回原来的 destroy。
///
/// pid 用 `i32` 而不是 `libc::pid_t`：这个文件三平台都要编，而 `libc::pid_t` 只在
/// Unix 上存在（Windows 上没有，CI 会直接编译失败）。macOS 上 `pid_t` 就是 `i32`。
#[cfg(target_os = "macos")]
fn own_web_content_pid(app: &AppHandle, w: &WebviewWindow) -> Option<i32> {
    app.webview_windows()
        .keys()
        .all(|label| label == "main" || label.starts_with("wallpaper-"))
        .then(|| crate::wallpaper::macos::web_content_pid(w))
        .flatten()
}

#[cfg(not(target_os = "macos"))]
fn own_web_content_pid(_app: &AppHandle, _w: &WebviewWindow) -> Option<i32> {
    None
}

/// 结束被回收窗口的 WebContent 进程，再记一行内存观测。
///
/// 发信号 + 等进程从进程表消失是阻塞的（上限 500ms），而调用点在主线程上，所以整件事
/// 交给后台任务；读数也放在进程真消失之后 —— 那一行读到的才是「回收后」的占用。
/// 非 macOS 没有可结束的进程（[`own_web_content_pid`] 恒为 None），只有读数这一步。
fn release_web_content(pid: Option<i32>, tag: &'static str) {
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
            let pid = own_web_content_pid(app, &w);
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

    // 重建：属性与 tauri.conf.json app.windows[0] 保持一致。
    // 刚释放后 "main" label 可能仍被僵尸占用：destroy 的注册表清理是异步的，
    // 短间隔重试等其真正释放（避免一次失败后主窗口彻底出不来）。
    for attempt in 0..10 {
        let builder = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("WallpaperEM")
            .inner_size(1240.0, 800.0)
            .min_inner_size(940.0, 600.0)
            .center()
            .transparent(true);
        // Overlay 标题栏/隐藏标题是 macOS-only API；Linux 下窗口带原生标题栏
        #[cfg(target_os = "macos")]
        let builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
        let built = builder.build();
        match built {
            Ok(w) => {
                // 装饰与初始窗口一致：关闭=隐藏 + 侧栏 vibrancy
                register_close_to_hide(&w);
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

/// 关闭主窗口 = 隐藏（壁纸继续运行；托盘/Dock 重新显示）。
/// setup（初始窗口）与 ensure_main_window（重建窗口）共用。
pub fn register_close_to_hide(w: &WebviewWindow) {
    let w2 = w.clone();
    w.on_window_event(move |event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = w2.hide();
        }
    });
}
