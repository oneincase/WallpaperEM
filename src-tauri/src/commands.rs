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
pub fn app_info(app: AppHandle) -> Value {
    // 平台相关的一句自我描述（设置页「关于」直接展示）
    let description = if cfg!(target_os = "macos") {
        "macOS 动态壁纸引擎 —— 浏览/下载并应用 Steam 创意工坊壁纸"
    } else {
        "跨平台动态壁纸引擎 —— 浏览/下载并应用 Steam 创意工坊壁纸"
    };
    // 版本以 Tauri 包信息为准（来自 tauri.conf.json，与发版 tag / 安装包一致）
    json!({
        "name": "WallpaperEM",
        "version": app.package_info().version.to_string(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "description": description,
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
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |r| {
        r.get(0)
    })
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

/// 条件化 KeepAlive：只在「崩溃 / 被信号杀死（jetsam、内存压力）」时让 launchd
/// 把进程拉回来。**不能**写成无条件的 `<true/>` —— 那会让用户主动退出也被立刻
/// 复活（退出 → 马上重启 → 再退出 → 再重启，软件永远退不干净）：
/// - `Crashed = true`：异常信号终止 → 拉起（自愈目标场景）
/// - `SuccessfulExit = false`：退出码非 0 → 拉起；托盘退出（exit 0）不复活
const KEEPALIVE_DICT: &str = concat!(
    "  <key>KeepAlive</key>\n",
    "  <dict>\n",
    "    <key>Crashed</key>\n",
    "    <true/>\n",
    "    <key>SuccessfulExit</key>\n",
    "    <false/>\n",
    "  </dict>\n",
);

/// 在自启 LaunchAgent plist 中注入条件化 `KeepAlive`（缺失时注入；老版本写入的
/// 无条件 `<true/>` 就地升级为条件字典）。
///
/// `auto-launch` 生成的 plist 只有 `RunAtLoad`，没有 `KeepAlive`：一旦应用在
/// 睡眠/盒盖期间被 macOS 终结（jetsam/内存压力/watchdog），launchd 不会把它拉起来，
/// 用户就会看到"壁纸软件整个退出"。
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
    let plain = regex::Regex::new(r"(?s)<key>KeepAlive</key>\s*<true/>").expect("静态正则");
    if plain.is_match(&xml) {
        // 老版本注入的无条件 KeepAlive：升级为条件字典（否则用户退出也会被复活）
        let upgraded = plain.replace(&xml, KEEPALIVE_DICT.trim_end());
        if let Err(e) = std::fs::write(&path, upgraded.as_ref()) {
            tracing::warn!("upgrade KeepAlive in {path:?} failed: {e}");
        } else {
            tracing::info!("autostart KeepAlive upgraded (crash-only): {path:?}");
        }
        return;
    }
    if xml.contains("<key>KeepAlive</key>") {
        // 已是条件字典，幂等返回
        return;
    }
    // 插到根 dict 的 </dict> 之前（auto-launch 生成的 plist 只有一个根 dict）
    if let Some(pos) = xml.rfind("</dict>") {
        xml.insert_str(pos, KEEPALIVE_DICT);
        if let Err(e) = std::fs::write(&path, xml) {
            tracing::warn!("inject KeepAlive into {path:?} failed: {e}");
        } else {
            tracing::info!("autostart KeepAlive injected (crash-only): {path:?}");
        }
    }
}

/// 启动时自愈：已安装的老版本可能带着无条件 KeepAlive 的 plist（用户退出被
/// launchd 立即复活，表现为"退出软件后一直自己重新启动"）。每次启动顺手升级。
#[cfg(target_os = "macos")]
pub fn ensure_keepalive_startup(app: &AppHandle) {
    ensure_keepalive(app);
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
