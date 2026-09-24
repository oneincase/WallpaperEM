//! 独立「壁纸设置」窗口（托盘右键 → 壁纸设置）。
//!
//! 与主窗口完全分离：不加载侧边栏/页面/路由，只打开 props.html 渲染
//! WallpaperPropsPanel。这样托盘唤起时**不会**把主界面（本地库那一大页）
//! 一起带出来 —— 早期实现把配置做成主窗口里的 React 弹窗，为了显示它不得不
//! ensure_main_window，用户就看到整个主窗口跳出来。
//!
//! label 固定为 `props-<itemId>`：同一张壁纸重复点击只聚焦已有窗口，
//! 不同壁纸可同时开多个设置窗。

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 打开（或聚焦）某壁纸的独立设置窗口。
pub fn open(app: &AppHandle, item_id: &str) -> tauri::Result<()> {
    let label = format!("props-{item_id}");

    // 已存在：取消最小化、显示、聚焦，不重复开窗
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    // 只加载 props.html，query 带 itemId（props-main.tsx 读取）
    let url = format!("props.html?item={}", urlencode(item_id));
    let builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
        // 原生标题栏文案（页面里的 document.title 由 props-main.tsx 自己设）
        .title(crate::i18n::tr("壁纸设置"))
        .inner_size(620.0, 640.0)
        .min_inner_size(480.0, 420.0)
        .center()
        .resizable(true)
        // 透明窗口 + 面板 .props-tint 半透明底色 + 原生 vibrancy（下方）= 磨砂
        .transparent(true);
    // Overlay 标题栏/隐藏标题是 macOS-only API；Linux 下窗口带原生标题栏
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let win = builder.build()?;

    // 平台磨砂材质：与主窗口一致（macOS 侧栏 vibrancy / Windows Acrylic），
    // 让 .props-tint 的半透明底色透出真模糊 —— CSS backdrop-filter 在透明
    // WKWebView 里会被 WebKit 丢弃（见 index.css），只能靠原生材质。
    // 材质只能在主线程调用（同 main_window.rs 的重建路径），
    // 统一走 run_on_main_thread，避免托盘/命令入口所在线程不确定。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let app2 = app.clone();
        let label2 = label.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(w) = app2.get_webview_window(&label2) {
                crate::apply_backdrop(&w);
            }
        });
    }

    // 背景拖动：WKWebView 的原生手势在非交互区域会吃掉鼠标事件，页面 JS 拖动
    // 与之冲突 = "时灵时不灵"。显式开启系统背景拖动（movableByWindowBackground），
    // 只留这一套机制：标题栏及所有非交互区域都能稳定拖动窗口。
    // （macOS 专属能力；Linux 下为空实现 —— Overlay 标题栏样式不生效，
    //  窗口带原生标题栏，直接拖标题栏即可）
    crate::wallpaper::platform::set_movable_by_background(&win, true);

    // 关闭 = 立即销毁并尽力结束其 WebContent 进程（与主窗口同语义）：设置窗
    // 关掉后桌面上应当只剩壁纸渲染进程。进程只在「除它以外只剩壁纸窗口」时才
    // 结束 —— 它与主窗口共用默认（共享）存储，可能同进程，踢了会连累主界面。
    {
        let label2 = label.clone();
        let w2 = win.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app = w2.app_handle().clone();
                if let Err(e) = crate::main_window::destroy_and_release(&app, &w2, &label2) {
                    let _ = w2.hide();
                    tracing::warn!("props window {label2} release failed: {e}（已退回隐藏）");
                }
            }
        });
    }
    Ok(())
}

/// 最小 URL 编码：itemId 是工坊 ID（数字或本地导入目录名），只需要防少数
/// 特殊字符；空格/中文/保留字符都编码，其余原样。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
