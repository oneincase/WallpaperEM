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
        .title("壁纸设置")
        .inner_size(620.0, 640.0)
        .min_inner_size(480.0, 420.0)
        .center()
        .resizable(true)
        // 透明窗口 + 面板的 rgba(--content) + backdrop-blur = 与主窗口一致的磨砂
        .transparent(true);
    // Overlay 标题栏/隐藏标题是 macOS-only API；Linux 下窗口带原生标题栏
    #[cfg(target_os = "macos")]
    let builder = builder
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true);
    let win = builder.build()?;

    // 背景拖动：WKWebView 的原生手势在非交互区域会吃掉鼠标事件，页面 JS 拖动
    // 与之冲突 = "时灵时不灵"。显式开启系统背景拖动（movableByWindowBackground），
    // 只留这一套机制：标题栏及所有非交互区域都能稳定拖动窗口。
    // （macOS 专属能力；Linux 下为空实现 —— Overlay 标题栏样式不生效，
    //  窗口带原生标题栏，直接拖标题栏即可）
    crate::wallpaper::platform::set_movable_by_background(&win, true);

    // 独立窗口的关闭是真关闭（不像主窗口 close-to-hide）：窗口销毁即可，
    // 下次托盘点击重建。不需要注册 close_to_hide。
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
