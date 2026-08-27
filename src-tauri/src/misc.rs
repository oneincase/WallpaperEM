//! 收藏 + 网络探测 + 诊断包（T4）

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Manager};

// ---------- 收藏 ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FavoriteItem {
    pub item_id: String,
    pub title: String,
    pub preview_url: Option<String>,
    pub r#type: String,
    pub created_at: i64,
}

#[tauri::command]
pub fn favorites_list(app: AppHandle) -> Result<Vec<FavoriteItem>, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT f.item_id, COALESCE(w.title, f.item_id), COALESCE(w.preview_url, ''),
                    COALESCE(w.type, 'unknown'), f.created_at
             FROM favorites f LEFT JOIN workshop_items w ON w.id = f.item_id
             ORDER BY f.created_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            let preview: String = r.get(2)?;
            Ok(FavoriteItem {
                item_id: r.get(0)?,
                title: r.get(1)?,
                preview_url: if preview.is_empty() { None } else { Some(preview) },
                r#type: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn favorite_add(app: AppHandle, item_id: String) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR IGNORE INTO favorites(user_id, item_id, created_at) VALUES (1, ?1, unixepoch())",
        [&item_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn favorite_remove(app: AppHandle, item_id: String) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM favorites WHERE item_id = ?1", [&item_id])
        .map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn favorite_status(app: AppHandle, item_id: String) -> Result<bool, String> {
    let db = app.state::<Arc<Mutex<Connection>>>();
    let conn = db.lock().map_err(|e| e.to_string())?;
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM favorites WHERE item_id = ?1",
            [&item_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

// ---------- 网络探测 ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetcheckItem {
    pub host: String,
    pub label: String,
    pub ok: bool,
    pub ms: i64,
}

#[tauri::command]
pub async fn network_probe(app: AppHandle) -> Result<serde_json::Value, String> {
    let client = match app.try_state::<crate::steam::SteamClient>() {
        Some(c) => c.http().clone(),
        None => {
            return Err("Steam 客户端未就绪".into());
        }
    };
    let hosts = [
        ("steamcommunity.com", "Steam 社区（工坊）"),
        ("api.steampowered.com", "Steam Web API（详情）"),
        ("cdn.steamstatic.com", "Steam CDN（预览图）"),
        ("content2.steampowered.com", "Steam 内容服务器（下载）"),
    ];
    let mut results = Vec::new();
    for (host, label) in hosts {
        let url = format!("https://{host}/");
        let start = std::time::Instant::now();
        let ok = client
            .get(&url)
            .send()
            .await
            .map(|r| r.status().is_success() || r.status().as_u16() < 500)
            .unwrap_or(false);
        let ms = start.elapsed().as_millis() as i64;
        results.push(NetcheckItem {
            host: host.to_string(),
            label: label.to_string(),
            ok,
            ms,
        });
    }
    let all_ok = results.iter().all(|r| r.ok);
    Ok(json!({
        "results": results,
        "allOk": all_ok,
        "hint": if all_ok { "Steam 网络连通正常" } else { "部分主机不通：请配置代理（设置 → 下载 → 代理），或切换代理为全局模式" }
    }))
}

// ---------- 诊断包 ----------

/// 导出诊断包：日志 + 数据库结构 + 环境信息 + 网络探测 → zip
#[tauri::command]
pub async fn diagnostics_export(app: AppHandle) -> Result<String, String> {
    let data_dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let log_dir = app.path().app_log_dir().map_err(|e| e.to_string())?;
    let out_dir = data_dir.join("diagnostics");
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let out_file = out_dir.join(format!("diagnostics-{}.zip", chrono::Utc::now().format("%Y%m%d-%H%M%S")));

    let file = std::fs::File::create(&out_file).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    // 日志
    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for e in entries.flatten() {
            if let Ok(data) = std::fs::read(e.path()) {
                let name = e.file_name().to_string_lossy().to_string();
                let _ = zip.start_file(format!("logs/{name}"), opts);
                let _ = zip.write_all(&data);
            }
        }
    }
    // 数据库结构
    {
        let db = app.state::<Arc<Mutex<Connection>>>();
        let conn = db.lock().map_err(|e| e.to_string())?;
        let mut schema = String::new();
        let mut stmt = conn
            .prepare("SELECT sql FROM sqlite_master WHERE type IN ('table','index') ORDER BY name")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .map_err(|e| e.to_string())?;
        for row in rows.flatten() {
            if let Some(sql) = row {
                schema.push_str(&sql);
                schema.push_str(";\n\n");
            }
        }
        let _ = zip.start_file("db-schema.sql", opts);
        let _ = zip.write_all(schema.as_bytes());
    }
    // 环境信息
    let info = json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "dataDir": data_dir.display().to_string(),
        "time": chrono::Utc::now().to_rfc3339(),
    });
    let _ = zip.start_file("environment.json", opts);
    let _ = zip.write_all(serde_json::to_string_pretty(&info).unwrap_or_default().as_bytes());

    // 网络探测
    let probe = network_probe(app.clone()).await.unwrap_or_else(|e| json!({ "error": e }));
    let _ = zip.start_file("network-probe.json", opts);
    let _ = zip.write_all(serde_json::to_string_pretty(&probe).unwrap_or_default().as_bytes());

    let _ = zip.finish();
    Ok(out_file.display().to_string())
}

use std::io::Write;
