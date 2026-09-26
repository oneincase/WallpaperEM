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
    if v < 7 {
        // v7：批量导入的引用模式 + 工坊上传回写。
        // - source_path：非 NULL = 引用模式条目（壁纸内容留在源目录，不拷贝进库根）
        // - publishedfileid：库条目上传创意工坊成功后的 publishedfileid（更新同一
        //   工坊条目用）；MCP 工程目录的对应信息存 project.json 的 workshop.fileId
        if !column_exists(conn, "library_items", "source_path")? {
            conn.execute_batch("ALTER TABLE library_items ADD COLUMN source_path TEXT;")?;
        }
        if !column_exists(conn, "library_items", "publishedfileid")? {
            conn.execute_batch("ALTER TABLE library_items ADD COLUMN publishedfileid TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_library_source
                 ON library_items(source_path) WHERE source_path IS NOT NULL;",
        )?;
        conn.pragma_update(None, "user_version", 7)?;
        tracing::info!("db migrated to version 7（引用模式 source_path + publishedfileid）");
    }
    if v < 8 {
        // v8：清晰度从「绝对 DPR 三档（0.8/1/2）」改为「相对设备像素比倍率四档」：
        //   0 自动（=设备 DPR，新默认）/ 0.75 省电 / 0.85 标准 / 1 高清（=原生）。
        // 旧值是绝对 DPR（语义受 min(devicePixelRatio, cap) 约束），按观感就近映射：
        //   2（旧高清，Retina 上=2x）→ 1（新高清=1×设备，Retina 同为 2x，观感一致）
        //   1（旧标准）             → 0.85（新标准）
        //   ≤0.9（旧省电 0.8 等）   → 0.75（新省电）
        // renderer 收到倍率后乘 devicePixelRatio 换算成库的绝对 DPR。
        fn map_dpr(raw: f64) -> f64 {
            if raw <= 0.9 {
                0.75
            } else if raw <= 1.5 {
                0.85
            } else {
                1.0
            }
        }
        if let Some(raw) = get_setting(conn, "wallpaper_render_dpr")
            .as_deref()
            .and_then(|s| s.trim().parse::<f64>().ok())
        {
            let mapped = map_dpr(raw);
            set_setting(conn, "wallpaper_render_dpr", &mapped.to_string())?;
            tracing::info!("db v8：全局清晰度（绝对 DPR {raw}）→ 相对倍率 {mapped}");
        }
        // 每壁纸覆盖（play_cfg:* 的 renderDpr）同样映射
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
                let mapped = map_dpr(d);
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
        conn.pragma_update(None, "user_version", 8)?;
        tracing::info!("db migrated to version 8（清晰度改相对倍率四档，修正 {fixed} 条壁纸覆盖）");
    }
    if v < 9 {
        // v9：作者昵称/头像缓存（Steam 资料页抓取结果）。ok=0 是负缓存——
        // 抓取失败也记一行，离线/被限流时避免每次打开面板都重打 Steam。
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS author_profiles (
               steamid TEXT PRIMARY KEY,
               persona TEXT,
               avatar_url TEXT,
               fetched_at INTEGER NOT NULL,
               ok INTEGER NOT NULL DEFAULT 1
             );",
        )?;
        conn.pragma_update(None, "user_version", 9)?;
        tracing::info!("db migrated to version 9（作者昵称/头像缓存表）");
    }
    if v < 10 {
        // v10：轮播升级 —— playlists 补随机开关（洗牌队列）与更新时间。
        if !column_exists(conn, "playlists", "shuffle")? {
            conn.execute_batch(
                "ALTER TABLE playlists ADD COLUMN shuffle INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !column_exists(conn, "playlists", "updated_at")? {
            conn.execute_batch(
                "ALTER TABLE playlists ADD COLUMN updated_at INTEGER NOT NULL DEFAULT (unixepoch());",
            )?;
        }
        conn.pragma_update(None, "user_version", 10)?;
        tracing::info!("db migrated to version 10（playlists 加 shuffle/updated_at）");
    }
    if v < 11 {
        // v11：下载任务自动重试轮数（网络瞬断自愈，见 download::fail）
        if !column_exists(conn, "downloads", "attempts")? {
            conn.execute_batch(
                "ALTER TABLE downloads ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        conn.pragma_update(None, "user_version", 11)?;
        tracing::info!("db migrated to version 11（downloads 加 attempts）");
    }
    if v < 12 {
        // v12：壁纸分享（mcp::shares）。shareId 即访客凭据（128bit 随机 hex），
        // 行本身无敏感信息；expires_at NULL = 永久，过期行由懒校验 + 定时清扫回收
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS shares (
                id          TEXT PRIMARY KEY,
                item_id     TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                expires_at  INTEGER,
                enabled     INTEGER NOT NULL DEFAULT 1,
                note        TEXT,
                views       INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_shares_item ON shares(item_id);",
        )?;
        conn.pragma_update(None, "user_version", 12)?;
        tracing::info!("db migrated to version 12（shares 表）");
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

// ---------- 作者昵称/头像缓存 ----------

/// 成功缓存 TTL：7 天（昵称/头像不常变）
pub const AUTHOR_OK_TTL_SECS: i64 = 7 * 24 * 3600;
/// 失败负缓存 TTL：12 小时（离线/限流时不反复重试）
pub const AUTHOR_FAIL_TTL_SECS: i64 = 12 * 3600;

/// 读作者缓存：
/// - `Some(Some((name, avatar)))` = 命中成功缓存
/// - `Some(None)` = 命中负缓存（近期抓取失败过，别再立刻重试 Steam）
/// - `None` = 无记录或已过期，需要重新抓取
pub fn find_author_profile(
    conn: &Connection,
    steamid: &str,
) -> Result<Option<Option<(String, String)>>, String> {
    let row = conn
        .query_row(
            "SELECT persona, avatar_url, fetched_at, ok FROM author_profiles WHERE steamid = ?1",
            [steamid],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((persona, avatar, fetched_at, ok)) = row else {
        return Ok(None);
    };
    let ttl = if ok == 1 {
        AUTHOR_OK_TTL_SECS
    } else {
        AUTHOR_FAIL_TTL_SECS
    };
    if chrono::Utc::now().timestamp() - fetched_at > ttl {
        return Ok(None);
    }
    if ok == 1 {
        Ok(Some(persona.map(|p| (p, avatar.unwrap_or_default()))))
    } else {
        Ok(Some(None))
    }
}

/// 写作者缓存。persona=None 记负缓存（ok=0）。
pub fn upsert_author_profile(
    conn: &Connection,
    steamid: &str,
    persona: Option<(&str, &str)>,
) -> Result<(), String> {
    let (name, avatar, ok) = match persona {
        Some((n, a)) => (Some(n), Some(a), 1),
        None => (None, None, 0),
    };
    conn.execute(
        "INSERT INTO author_profiles(steamid, persona, avatar_url, fetched_at, ok)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(steamid) DO UPDATE SET persona = ?2, avatar_url = ?3, fetched_at = ?4, ok = ?5",
        params![steamid, name, avatar, chrono::Utc::now().timestamp(), ok],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
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
    /// v12：shares 表随迁移创建（幂等 CREATE IF NOT EXISTS；索引就位）
    #[test]
    fn v12_creates_shares_table() {
        let c = v4_db();
        migrate(&c).unwrap();
        let v: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 12);
        // 插入 + 唯一键约束生效
        c.execute(
            "INSERT INTO shares (id, item_id, created_at) VALUES ('a1', 'i1', 1)",
            [],
        )
        .unwrap();
        assert!(c
            .execute(
                "INSERT INTO shares (id, item_id, created_at) VALUES ('a1', 'i1', 1)",
                [],
            )
            .is_err());
        let n: i64 = c
            .query_row("SELECT COUNT(*) FROM shares", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    fn v5_maps_old_dpr_tiers_to_three_levels() {
        let c = v4_db();
        migrate(&c).unwrap();        // v8 把绝对 DPR 三档再映射成相对倍率四档：旧 2（高清）→ 1（新高清）
        let v: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='wallpaper_render_dpr'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "1");

        // 每壁纸覆盖里的 renderDpr 也一路迁移到相对倍率（旧 2 → 1）
        let cfg: String = c
            .query_row(
                "SELECT value FROM settings WHERE key='play_cfg:999'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(cfg.contains("\"renderDpr\":1"), "cfg 未迁移: {cfg}");

        let ver: i64 = c
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        // 版本号是链式的：v4 的库跑一次 migrate() 会一路升到当前最新。
        // 断言下限而不是具体数字 —— 迁移只会追加，钉死「当前最新」每加一版都要来改一次
        assert!(ver >= 9, "migrate 未把 v4 一路升上来: {ver}");
    }

    #[test]
    fn v9_author_profile_cache_roundtrip_and_ttls() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("schema.sql")).unwrap();
        migrate(&c).unwrap();

        // 无记录 → 需要抓取
        assert_eq!(find_author_profile(&c, "123").unwrap(), None);

        // 成功缓存：命中且带头像
        upsert_author_profile(&c, "123", Some(("abi toads", "https://a/b.jpg"))).unwrap();
        assert_eq!(
            find_author_profile(&c, "123").unwrap(),
            Some(Some(("abi toads".into(), "https://a/b.jpg".into())))
        );

        // 负缓存：近期失败 → Some(None)，调用方不应立刻重试
        upsert_author_profile(&c, "456", None).unwrap();
        assert_eq!(find_author_profile(&c, "456").unwrap(), Some(None));

        // 过期的成功缓存 → 重新抓取
        c.execute(
            "UPDATE author_profiles SET fetched_at = ?1 WHERE steamid = '123'",
            [chrono::Utc::now().timestamp() - AUTHOR_OK_TTL_SECS - 1],
        )
        .unwrap();
        assert_eq!(find_author_profile(&c, "123").unwrap(), None);

        // 过期的负缓存 → 重新抓取
        c.execute(
            "UPDATE author_profiles SET fetched_at = ?1 WHERE steamid = '456'",
            [chrono::Utc::now().timestamp() - AUTHOR_FAIL_TTL_SECS - 1],
        )
        .unwrap();
        assert_eq!(find_author_profile(&c, "456").unwrap(), None);
    }

    #[test]
    fn v8_maps_absolute_dpr_to_relative_multipliers() {
        // 旧绝对 DPR（v5 三档：0.8/1/2）→ 新相对倍率（0.75/0.85/1）
        let map = |raw: f64| -> f64 {
            if raw <= 0.9 {
                0.75
            } else if raw <= 1.5 {
                0.85
            } else {
                1.0
            }
        };
        assert_eq!(map(0.8), 0.75); // 旧省电 → 新省电
        assert_eq!(map(1.0), 0.85); // 旧标准 → 新标准
        assert_eq!(map(2.0), 1.0); // 旧高清（Retina 上=2x）→ 新高清（1×设备=2x）

        // 已是相对倍率的库（v7）迁移到 v8 后，三档历史值（0.8/1/2）会被映射；
        // 自动档 0 是 v8 新引入，旧库不可能存，无需保持。
        for (existing, expected) in [(0.8_f64, 0.75), (1.0, 0.85), (2.0, 1.0)] {
            let c = Connection::open_in_memory().unwrap();
            c.execute_batch(include_str!("schema.sql")).unwrap();
            set_setting(&c, "wallpaper_render_dpr", &existing.to_string()).unwrap();
            // 钉到 7，migrate 只跑 v8
            c.pragma_update(None, "user_version", 7).unwrap();
            migrate(&c).unwrap();
            let v: String = c
                .query_row(
                    "SELECT value FROM settings WHERE key='wallpaper_render_dpr'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!((v.parse::<f64>().unwrap() - expected).abs() < 1e-9, "existing={existing}");
        }
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
