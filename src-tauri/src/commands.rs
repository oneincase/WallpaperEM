//! 通用命令（T0 骨架）：ping / app_info / db_status / settings / autostart

use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

#[tauri::command]
pub fn ping() -> &'static str {
    "pong"
}

#[tauri::command]
pub fn app_info() -> Value {
    json!({
        "name": "WallpaperEM",
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "description": "macOS 动态壁纸引擎 —— 浏览/下载并应用 Steam 创意工坊壁纸",
    })
}

#[tauri::command]
pub fn db_status(db: State<'_, Arc<Mutex<rusqlite::Connection>>>) -> Result<Value, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|e| e.to_string())?;
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    Ok(json!({ "userVersion": user_version, "tables": tables }))
}

#[tauri::command]
pub fn settings_get(
    db: State<'_, Arc<Mutex<rusqlite::Connection>>>,
    key: String,
) -> Result<Option<String>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn settings_set(
    db: State<'_, Arc<Mutex<rusqlite::Connection>>>,
    key: String,
    value: String,
) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        [&key, &value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn autostart_status(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// 在自启 LaunchAgent plist 中注入 `KeepAlive`（缺失时）。
///
/// `auto-launch` 生成的 plist 只有 `RunAtLoad`，没有 `KeepAlive`：一旦应用在
/// 睡眠/盒盖期间被 macOS 终结（jetsam/内存压力/watchdog），launchd 不会把它拉起来，
/// 用户就会看到"壁纸软件整个退出"。补上 `KeepAlive` 后进程被杀会自动重启。
#[cfg(target_os = "macos")]
fn ensure_keepalive(app: &AppHandle) {
    let name = &app.package_info().name;
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join("Library/LaunchAgents")
        .join(format!("{name}.plist"));
    let Ok(mut xml) = std::fs::read_to_string(&path) else {
        return;
    };
    // 已含 KeepAlive（或该键已启用）则不重复注入
    if xml.contains("<key>KeepAlive</key>") {
        return;
    }
    // 插到根 dict 的 </dict> 之前（auto-launch 生成的 plist 只有一个根 dict）
    if let Some(pos) = xml.rfind("</dict>") {
        xml.insert_str(pos, "  <key>KeepAlive</key>\n  <true/>\n");
        if let Err(e) = std::fs::write(&path, xml) {
            tracing::warn!("inject KeepAlive into {path:?} failed: {e}");
        } else {
            tracing::info!("autostart KeepAlive injected: {path:?}");
        }
    }
}

#[tauri::command]
pub fn autostart_set(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let autolaunch = app.autolaunch();
    if enabled {
        autolaunch.enable().map_err(|e| e.to_string())?;
        // enable() 会重写 plist，故必须在之后注入 KeepAlive
        #[cfg(target_os = "macos")]
        ensure_keepalive(&app);
    } else {
        autolaunch.disable().map_err(|e| e.to_string())?;
    }
    autolaunch.is_enabled().map_err(|e| e.to_string())
}
