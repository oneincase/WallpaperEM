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
                Ok(_) => tracing::info!(
                    "main window released after {}s hidden (WebContent 进程回收)",
                    RELEASE_AFTER.as_secs()
                ),
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

    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }

    // 重建：属性与 tauri.conf.json app.windows[0] 保持一致
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
            let _ = crate::apply_vibrancy(app);
            let _ = w.set_focus();
            tracing::info!("main window recreated");
        }
        Err(e) => tracing::error!("main window recreate failed: {e}"),
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
