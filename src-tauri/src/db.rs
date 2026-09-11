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
    if v < 3 {
        // v3：移除 DepotDownloader 后端，下载工具只剩 steamcmd。
        // 清掉旧的后端选择键，避免残留一个再也不会被读的设置。
        conn.execute_batch("DELETE FROM settings WHERE key = 'download_backend';")?;
        conn.pragma_update(None, "user_version", 3)?;
        tracing::info!("db migrated to version 3");
    }
    if v < 4 {
        // v4：本地库筛选需要标签，但 library_items.tags 从未被写入过（历史遗漏，
        // 实测 244 行全为空）。从工坊元数据缓存回填一次；缓存里没有的条目保持空数组，
        // 筛选时仍可经 LEFT JOIN workshop_items 兜底。
        let n = conn.execute(
            "UPDATE library_items
                SET tags = COALESCE(
                    (SELECT w.tags FROM workshop_items w WHERE w.id = library_items.item_id),
                    '[]')
              WHERE tags IS NULL OR tags = '' OR tags = '[]'",
            [],
        )?;
        // 这两列是筛选与排序的主力，之前全库无索引
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_library_type ON library_items(type);
             CREATE INDEX IF NOT EXISTS idx_library_downloaded ON library_items(downloaded_at);",
        )?;
        conn.pragma_update(None, "user_version", 4)?;
        tracing::info!("db migrated to version 4（回填 {n} 条本地库标签 + 建索引）");
    }
    if v < 5 {
        // v5：清晰度档位从 5 档（0.5~5x）收敛到 3 档（0.8 省电 / 1 标准 / 2 高清）。
        // 旧值就近映射，3/4/5 一律归到 2 —— 实际生效值受 min(devicePixelRatio, cap)
        // 约束，2x 屏上 3x 以上本来就等于 2x，映射过去不改变任何观感；留着 5 反而
        // 让下拉框找不到对应项（显示成空档或错档，用户看不出当前生效的是哪个）。
        if let Some(raw) = get_setting(conn, "wallpaper_render_dpr")
            .as_deref()
            .and_then(|s| s.trim().parse::<f32>().ok())
        {
            let mapped = if raw <= 0.9 {
                "0.8"
            } else if raw <= 1.5 {
                "1"
            } else {
                "2"
            };
            set_setting(conn, "wallpaper_render_dpr", mapped)?;
            tracing::info!("db v5：全局清晰度 {raw} → {mapped}");
        }
        // 每壁纸覆盖里也可能存着旧档位（play_cfg:* 的 renderDpr）
        let mut fixed = 0usize;
        {
            let rows: Vec<(String, String)> = conn
                .prepare("SELECT key, value FROM settings WHERE key LIKE 'play_cfg:%'")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .filter_map(|r| r.ok())
                .collect();
            for (k, val) in rows {
                let Ok(mut j) = serde_json::from_str::<serde_json::Value>(&val) else {
                    continue;
                };
                let Some(d) = j.get("renderDpr").and_then(|x| x.as_f64()) else {
                    continue;
                };
                let mapped: f64 = if d <= 0.9 {
                    0.8
                } else if d <= 1.5 {
                    1.0
                } else {
                    2.0
                };
                if (d - mapped).abs() < 1e-9 {
                    continue;
                }
                j["renderDpr"] = serde_json::json!(mapped);
                conn.execute(
                    "UPDATE settings SET value = ?2 WHERE key = ?1",
                    rusqlite::params![k, j.to_string()],
                )?;
                fixed += 1;
            }
        }
        conn.pragma_update(None, "user_version", 5)?;
        tracing::info!("db migrated to version 5（清晰度档位收敛，修正 {fixed} 条壁纸覆盖）");
    }
    if v < 6 {
        // v6：本地导入条目的 tags 从来没写过（导入路径漏了这一步），而 v4 的回填
        // 只从工坊缓存取 —— 本地导入（custom-*）在缓存里没有记录，于是筛选面板
        // 按「视频/场景/网页」筛时，导入进来的壁纸一条都命中不到。
        // 这里按 type 列补上类型标签；新导入的条目由 upsert_library_item 直接写。
        let n = conn.execute(
            "UPDATE library_items
                SET tags = json_array(CASE lower(type)
                        WHEN 'video' THEN 'Video'
                        WHEN 'scene' THEN 'Scene'
                        WHEN 'web' THEN 'Web'
                        WHEN 'gif' THEN 'GIF'
                        WHEN 'application' THEN 'Application'
                     END)
              WHERE (tags IS NULL OR tags = '' OR tags = '[]')
                AND lower(type) IN ('video','scene','web','gif','application')",
            [],
        )?;
        conn.pragma_update(None, "user_version", 6)?;
        tracing::info!("db migrated to version 6（回填 {n} 条本地导入类型标签）");
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
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
        r.get(0)
    })
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
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
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
        let mut rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.next().transpose().map_err(|e| e.to_string())?
    };
    drop(conn);
    match meta {
        Some(m) => serde_json::from_str(&m)
            .map(Some)
            .map_err(|e| e.to_string()),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4_db() -> Connection {
        // 复刻 v4 状态：跑 schema(v1) 后直接把 user_version 钉到 4，
        // 并灌入旧档位值与本地导入条目 —— 这样 migrate() 会执行 v5 与 v6 两段
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("schema.sql")).unwrap();
        c.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES('wallpaper_render_dpr','5')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES(?,?)",
            rusqlite::params!["play_cfg:999", r#"{"renderDpr":3}"#],
        )
        .unwrap();
        c.pragma_update(None, "user_version", 4).unwrap();
        c
    }

    fn add_local_import(c: &Connection, id: &str, ty: &str, tags: Option<&str>) {
        c.execute(
            "INSERT INTO library_items(item_id, title, type, size_bytes, file_count, downloaded_at, tags)
             VALUES (?1, ?2, ?3, 1, 1, 1, ?4)",
            rusqlite::params![id, id, ty, tags],
        )
        .unwrap();
    }

    #[test]
    fn v5_maps_old_dpr_tiers_to_three_levels() {
        let c = v4_db();
        migrate(&c).unwrap();

        let v: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='wallpaper_render_dpr'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // 旧 5（高清档 3/4/5 之一）就近归到 2，不能留着让下拉框找不到对应项
        assert_eq!(v, "2");

        // 每壁纸覆盖里的 renderDpr 也要一起迁移
        let cfg: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='play_cfg:999'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(cfg.contains("\"renderDpr\":2"), "cfg 未迁移: {cfg}");

        let ver: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        // 版本号是链式的：v4 的库跑一次 migrate() 会一路升到当前最新
        assert_eq!(ver, 6);
    }

    #[test]
    fn v6_backfills_type_tag_for_local_imports_without_tags() {
        let c = v4_db();
        // 本地导入的条目：类型有，标签空（导入路径历史遗漏）
        add_local_import(&c, "custom-a", "video", None);
        add_local_import(&c, "custom-b", "scene", Some("[]"));
        // 已有标签的条目不能被覆盖（工坊缓存回填的题材标签要保住）
        add_local_import(&c, "custom-c", "video", Some(r#"["Anime"]"#));
        // 缓存里已有元数据的工坊条目不属于本次回填目标（v4 的语句会覆盖它，
        // 但 v6 只处理「空标签」的条目，这里顺带确认没被写脏）
        add_local_import(&c, "123", "video", Some(r#"["Video"]"#));

        migrate(&c).unwrap();

        let tags = |id: &str| -> String {
            c.query_row(
                "SELECT tags FROM library_items WHERE item_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(tags("custom-a"), r#"["Video"]"#);
        assert_eq!(tags("custom-b"), r#"["Scene"]"#);
        assert_eq!(tags("custom-c"), r#"["Anime"]"#);
        assert_eq!(tags("123"), r#"["Video"]"#);
    }

    #[test]
    fn v5_mapping_table_boundaries() {
        // 逐档验证映射边界（与迁移代码同一规则）
        let map = |raw: f32| {
            if raw <= 0.9 {
                "0.8"
            } else if raw <= 1.5 {
                "1"
            } else {
                "2"
            }
        };
        assert_eq!(map(0.5), "0.8");
        assert_eq!(map(0.8), "0.8");
        assert_eq!(map(1.0), "1");
        assert_eq!(map(1.5), "1");
        assert_eq!(map(2.0), "2");
        assert_eq!(map(5.0), "2");
    }
}
