//! 平台无关的桌面层窗口接口：按目标 OS 转发到具体后端。
//!
//! 各后端（macos.rs / linux.rs）对外提供完全一致的函数集：
//! - `ScreenInfo` / `active_screens` / `display_asleep`：屏幕枚举与睡眠检测
//! - `legacy_display_ids` / `refresh_display_meta`：显示器稳定 id 迁移映射与名称缓存刷新
//! - `set_frame` / `apply_desktop_window`：窗口几何与桌面层级
//! - `set_movable_by_background`：背景拖动（仅 macOS 有效，其余平台空实现）
//! - `start_auto_pause_observer`：前台应用切换观察（自动暂停数据源）
//! - `cursor_state`：系统光标位置查询（指针注入数据源）
//!
//! 未支持的平台走 fallback：壁纸窗口是普通置底窗口，引擎主体功能仍可用。

#[cfg(target_os = "macos")]
pub use super::macos::*;

#[cfg(target_os = "linux")]
pub use super::linux::*;

#[cfg(target_os = "windows")]
pub use super::windows::*;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use fallback::*;

/// 其他平台的编译兜底：桌面层级/屏幕枚举未实现，壁纸以全屏普通窗口呈现
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod fallback {
    use tauri::{LogicalPosition, LogicalSize, Runtime, WebviewWindow};

    #[derive(Debug, Clone)]
    pub struct ScreenInfo {
        pub id: u32,
        pub name: String,
        pub x: f64,
        pub y: f64,
        pub w: f64,
        pub h: f64,
        pub scale: f64,
        pub is_primary: bool,
    }

    pub fn legacy_display_ids() -> Vec<(String, String)> {
        Vec::new()
    }

    pub fn on_ac_power() -> Option<bool> {
        None
    }

    pub fn refresh_display_meta() {}

    pub fn active_screens() -> Vec<ScreenInfo> {
        Vec::new()
    }

    pub fn hit_screen_id(_x: f64, _y: f64) -> Option<u32> {
        None
    }

    pub fn display_asleep() -> bool {
        false
    }

    pub fn set_frame<R: Runtime>(window: &WebviewWindow<R>, x: f64, y: f64, w: f64, h: f64) {
        let _ = window.set_position(LogicalPosition::new(x, y));
        let _ = window.set_size(LogicalSize::new(w, h));
    }

    pub fn apply_desktop_window<R: Runtime>(
        window: &tauri::WebviewWindow<R>,
        frame: (f64, f64, f64, f64),
        interactive: bool,
    ) {
        let _ = window.set_ignore_cursor_events(!interactive);
        set_frame(window, frame.0, frame.1, frame.2, frame.3);
    }

    pub fn set_movable_by_background<R: Runtime>(_window: &WebviewWindow<R>, _movable: bool) {}

    pub fn start_auto_pause_observer(_app: &tauri::AppHandle) {}

    pub fn start_desktop_click_monitor(_app: &tauri::AppHandle) {}

    pub fn cursor_state() -> Option<(f64, f64, u32)> {
        None
    }
}
