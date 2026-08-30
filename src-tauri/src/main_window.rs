//! 主窗口生命周期：闲置释放 + 按需重建
//!
//! 应用主窗口（React UI，独立 WKWebView）在隐藏/最小化超过 [`RELEASE_AFTER`]
//! 后销毁窗口，让 WebKit 回收其 WebContent 进程（通常是最大的"多余"内存占用，
//! 含缩略图/预览等媒体资源）；桌面壁纸窗口（label = `wallpaper-*`）不受影响。
//!
//! 隐藏判定覆盖三种用户路径：
//! - 黄色按钮最小化（miniaturized 时 isVisible 仍为 true，故必须查 is_minimized）
//! - 关闭按钮（CloseRequested -> hide，见 lib.rs setup）
//! - ⌘H 隐藏整个应用（窗口 orderOut，is_visible 变 false）
//!
//! 释放后用户从 托盘菜单 / 托盘左键 / Dock 图标(Reopen) / 二次启动(single-instance)
//! 唤起时，由 [`ensure_main_window`] 按 tauri.conf.json 原配置重建窗口。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::download;

/// 主窗口持续隐藏多久后释放（销毁窗口 -> 终止其 WebContent 进程）
pub const RELEASE_AFTER: Duration = Duration::from_secs(10);
/// 轮询周期（实际触发时延 = RELEASE_AFTER + 最多一次轮询间隔）
const POLL: Duration = Duration::from_secs(1);

#[derive(Default)]
struct MainWindowState {
    /// 连续隐藏起始时刻；None = 当前可见（或窗口不存在）
    hidden_since: Option<Instant>,
    /// watchdog 已销毁主窗口。Tauri 注册表的条目移除依赖 macOS windowWillClose
    /// 事件链，隐藏（orderOut）窗口的 destroy 不保证送达——此后 get_webview_window
    /// 会拿到僵尸条目并对其 show()（静默失败），主窗口永远无法重建。该标志强制
    /// ensure_main_window 走清场重建路径。
    released: bool,
}

/// 启动闲置释放看门狗（setup 阶段调用一次）
pub fn start(app: &AppHandle) {
    app.manage(Arc::new(Mutex::new(MainWindowState::default())));
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(POLL).await;
            let app3 = app2.clone();
            // AppKit 调用必须在主线程
            let _ = app2.run_on_main_thread(move || on_tick(&app3));
        }
    });
    tracing::info!(
        "main window idle release armed ({}s)",
        RELEASE_AFTER.as_secs()
    );
}

/// 主窗口是否处于"用户不可见"状态（最小化/隐藏/应用隐藏）。
/// 窗口不存在（已释放或尚未创建）返回 false，让看门狗重置计时。
fn is_hidden(w: &WebviewWindow) -> bool {
    w.is_minimized().unwrap_or(false) || !w.is_visible().unwrap_or(true)
}

/// 单次检查（在主线程执行，检查与销毁天然无竞态）
fn on_tick(app: &AppHandle) {
    let hidden = app
        .get_webview_window("main")
        .as_ref()
        .map(is_hidden)
        .unwrap_or(false);

    let Some(st) = app.try_state::<Arc<Mutex<MainWindowState>>>() else {
        return;
    };
    let mut st = st.lock().unwrap();
    if !hidden {
        st.hidden_since = None;
        return;
    }
    let since = *st.hidden_since.get_or_insert_with(Instant::now);
    if since.elapsed() < RELEASE_AFTER {
        return;
    }
    // 到时：先重置计时（本次要么释放，要么因下载活动跳过后重新计满时长）
    st.hidden_since = None;

    // 下载/Guard/扫码登录进行中：跳过释放，避免打断 Steam Guard 输入与进度展示
    if download::is_busy(app) {
        tracing::debug!("main window idle release skipped: download active");
        return;
    }

    // 释放前再次确认仍隐藏（用户可能刚重新打开）
    if let Some(w) = app.get_webview_window("main") {
        if is_hidden(&w) {
            match w.destroy() {
                Ok(_) => {
                    // 注意：此处不可再 lock()——外层 guard（第 73 行）仍存活，
                    // std::sync::Mutex 不可重入，同线程二次 lock = 自死锁，
                    // 表现为「主窗口被回收的瞬间整个软件无响应」
                    st.released = true;
                    tracing::info!(
                        "main window released after {}s hidden (WebContent 进程回收)",
                        RELEASE_AFTER.as_secs()
                    );
                }
                Err(e) => tracing::warn!("main window release failed: {e}"),
            }
        }
    }
}

/// 显示主窗口；若已被闲置释放销毁，则按 tauri.conf.json 原配置重建。
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
        let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
            .title("WallpaperEM")
            .inner_size(1240.0, 800.0)
            .min_inner_size(940.0, 600.0)
            .center()
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true)
            .transparent(true)
            .build();
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
