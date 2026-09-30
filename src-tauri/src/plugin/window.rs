//! 把 URL 开成应用窗口。
//!
//! 内置插件的宿主界面（dsh Web GUI）与第三方插件的外链都走这里，规则统一：
//! **同一插件只允许一个窗口**（label = 插件 id，重复点击聚焦已有窗口而不是叠窗）。
//!
//! 刻意不做无边框：窗口里装的是外部页面，没有我们自己的拖动区与窗口控制按钮，
//! 去掉系统标题栏就等于这扇窗既拖不动也关不掉。外观交给系统原生装饰。

use std::sync::Arc;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// 打开（或聚焦）一个插件窗口。
///
/// - `label`：插件 id，同一 label 重复调用只聚焦；
/// - `on_closed`：**仅在新窗口真的建起来时**注册一次，用来回收插件背后的子进程
///   （外部页面里没有我们的关闭钩子，窗口销毁就是唯一的收尾时机）。关闭与销毁
///   两条路都会调它，必须是幂等的。
pub fn open_url_window<F>(
    app: &AppHandle,
    label: &str,
    url: &str,
    title: &str,
    on_closed: F,
) -> Result<(), String>
where
    F: Fn() + Send + Sync + 'static,
{
    if let Some(w) = app.get_webview_window(label) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return Ok(());
    }

    // 外链必须先自己解析一次：WebviewUrl::External 要的是 url::Url，解析失败
    // 属于「插件给的地址坏了」，得在开窗之前就说清楚，而不是开出一个空白窗
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid plugin url: {e}"))?;
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::External(parsed))
        .title(title)
        // 尺寸按「一个能用浏览器」给：dsh GUI 是双栏布局，窄了会挤成一栏
        .inner_size(1280.0, 860.0)
        .min_inner_size(720.0, 520.0)
        .resizable(true)
        .center()
        // 同主窗口：关掉 WebKit 开发者附加，右键不弹「Reload / Inspect Element」
        .devtools(false)
        .build()
        .map_err(|e| format!("build plugin window failed: {e}"))?;

    let on_closed = Arc::new(on_closed);

    // 关闭 = 立刻收子进程再销毁窗口并结束它的 WebContent 进程（与主窗口 / 设置窗
    // 同一个语义，见 main_window::destroy_and_release）：dsh 这类插件窗背后挂着
    // 子进程，关掉之后桌面上不该留下没人认领的 node，也不该留一个空转的 WebKit。
    {
        let app = app.clone();
        let win2 = win.clone();
        let label = label.to_string();
        let cb = on_closed.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                // 先收子进程：Destroyed 那条路是兜底，别把希望全押在它身上
                cb();
                if let Err(e) = crate::main_window::destroy_and_release(&app, &win2, &label) {
                    // 退回正常关闭：宁可少杀一个 WebContent，也不能把窗口卡住
                    let _ = win2.close();
                    tracing::warn!("plugin window {label} release failed: {e}（退回关闭）");
                }
            }
        });
    }

    // 兜底：别的销毁路径（系统关闭、上层链式销毁）也要把子进程收掉
    win.on_window_event(move |event| {
        if let tauri::WindowEvent::Destroyed = event {
            on_closed();
        }
    });
    Ok(())
}
