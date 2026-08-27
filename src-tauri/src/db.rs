//! SQLite（rusqlite）初始化、迁移与业务辅助函数。schema 见 schema.sql（设计方案 §5）。

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tauri::Manager;

use crate::steam::types::WorkshopItem;

pub fn init(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let db_path = dir.join("app.db");
    let conn = Connection::open(&db_path)?;
    migrate(&conn)?;
    app.manage(Arc::new(Mutex::new(conn)));
    tracing::info!("db ready at {}", db_path.display());
    Ok(())
}

fn migrate(conn: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if v < 1 {
        conn.execute_batch(include_str!("schema.sql"))?;
        conn.pragma_update(None, "user_version", 1)?;
        tracing::info!("db migrated to version 1");
    }
    if v < 2 {
        // v2：downloads 表补 waiting_guard 列（幂等：列已存在则跳过）。
        // 注意 schema.sql(v1) 里也可能已含该列，直接 ALTER 会报 duplicate column。
        if !column_exists(conn, "downloads", "waiting_guard")? {
            conn.execute_batch(
                "ALTER TABLE downloads ADD COLUMN waiting_guard INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        conn.pragma_update(None, "user_version", 2)?;
        tracing::info!("db migrated to version 2");
    }
    Ok(())
}

/// 判断表是否存在指定列（用于幂等迁移）
fn column_exists(conn: &Connection, table: &str, col: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?; // cid,name,type,... 第 2 列是列名
    for r in rows {
        if r? == col {
            return Ok(true);
        }
    }
    Ok(false)
}

// ---------- 设置 ----------

pub fn get_setting(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

#[allow(dead_code)]
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = ?2",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- 工坊条目缓存 ----------

/// upsert 工坊条目（metadata_json 存完整 camelCase JSON，fetched_at 新鲜度）
pub fn upsert_workshop_item(conn: &Connection, item: &WorkshopItem) -> Result<(), String> {
    let meta = serde_json::to_string(item).map_err(|e| e.to_string())?;
    let tags = serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".into());
    conn.execute(
        "INSERT INTO workshop_items(id, title, description, preview_url, file_url, type, tags,
                                    size_x, size_y, subscriptions, favorited, metadata_json, fetched_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET
           title = ?2, description = ?3, preview_url = ?4, file_url = ?5, type = ?6, tags = ?7,
           subscriptions = ?8, favorited = ?9, metadata_json = ?10, fetched_at = ?11, updated_at = ?11",
        params![
            item.id,
            item.title,
            item.description,
            item.preview_url,
            item.file_url,
            item.r#type,
            tags,
            item.subscriptions,
            item.favorited,
            meta,
            chrono::Utc::now().timestamp(),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// 查询一批条目（仅返回 metadata_json 有效且 fetched_at 新鲜度内的）
pub fn find_workshop_items(
    conn: &Connection,
    ids: &[String],
    fresh_ms: i64,
) -> Result<HashMap<String, WorkshopItem>, String> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, metadata_json, fetched_at FROM workshop_items WHERE id IN ({placeholders})"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(ids.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })
        .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp_millis();
    for row in rows {
        let (id, meta, fetched_at) = row.map_err(|e| e.to_string())?;
        if now - fetched_at * 1000 > fresh_ms {
            continue;
        }
        if let Ok(item) = serde_json::from_str::<WorkshopItem>(&meta) {
            out.insert(id, item);
        }
    }
    Ok(out)
}

/// 查询单条目完整元数据（不受新鲜度限制），用于下载时回退类型等
pub fn find_workshop_item(
    db: &Arc<Mutex<Connection>>,
    id: &str,
) -> Result<Option<WorkshopItem>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let meta = {
        let mut stmt = conn
            .prepare("SELECT metadata_json FROM workshop_items WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut rows = stmt.query_map([id], |r| r.get::<_, String>(0)).map_err(|e| e.to_string())?;
        rows.next().transpose().map_err(|e| e.to_string())?
    };
    drop(conn);
    match meta {
        Some(m) => serde_json::from_str(&m).map(Some).map_err(|e| e.to_string()),
        None => Ok(None),
    }
}
